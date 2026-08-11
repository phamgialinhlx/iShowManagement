//! A native terminal tab: `alacritty_terminal` emulation over a `rmux-term` PTY,
//! painted by gpui.
//!
//! The PTY is spawned from a `Target::build_command` argv, so local and remote
//! are one code path (this scaffold uses `LocalTarget`; a remote target is the
//! same call). Terminal bytes never cross an RPC — they go PTY → emulator →
//! screen, exactly as the invariants require.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use gpui::{
    App, Context, FocusHandle, Focusable, KeyDownEvent, Keystroke, Window, div, prelude::*, px, rgb,
};
use rmux_term::{TermSize, Terminal, TerminalEvent};
use rmux_transport::{CommandSpec, LocalTarget, Target};

const COLS: u16 = 100;
const ROWS: u16 = 30;

/// Grid dimensions for the emulator (`alacritty_terminal::grid::Dimensions`).
#[derive(Clone, Copy)]
struct Dims {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// The alacritty emulation of the PTY byte stream.
struct Emu {
    term: Term<VoidListener>,
    parser: Processor,
}

impl Emu {
    fn new(cols: u16, rows: u16) -> Self {
        let size = Dims { columns: cols.max(1) as usize, screen_lines: rows.max(1) as usize };
        Self { term: Term::new(Config::default(), &size, VoidListener), parser: Processor::new() }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// The visible screen as one string per row (spaces preserved for alignment).
    fn rows(&self) -> Vec<String> {
        let grid = self.term.grid();
        let cols = grid.columns();
        (0..grid.screen_lines())
            .map(|row| {
                let mut s = String::with_capacity(cols);
                for col in 0..cols {
                    s.push(grid[Line(row as i32)][Column(col)].c);
                }
                s
            })
            .collect()
    }
}

/// One terminal, as a gpui view.
pub struct TerminalView {
    focus: FocusHandle,
    emu: Emu,
    pty: Terminal,
}

impl TerminalView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cmd = LocalTarget::new()
            .build_command(&CommandSpec::login_shell())
            .expect("build local login shell");
        let pty = Terminal::spawn(&cmd, None, TermSize { cols: COLS, rows: ROWS })
            .expect("spawn local pty");

        let mut rx = pty.subscribe();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let keep_going = this.update(cx, |view, cx| match event {
                    TerminalEvent::Output(bytes) => {
                        view.emu.feed(&bytes);
                        cx.notify();
                        true
                    }
                    TerminalEvent::Exited { .. } => false,
                });
                if !matches!(keep_going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self { focus, emu: Emu::new(COLS, ROWS), pty }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, _: &mut Context<Self>) {
        let bytes = encode_key(&event.keystroke);
        if !bytes.is_empty() {
            let _ = self.pty.write(&bytes);
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(rgb(0x14110f))
            .text_color(rgb(0xe8e6e1))
            .font_family("Menlo")
            .text_size(px(13.))
            .line_height(px(18.))
            .flex()
            .flex_col()
            .children(self.emu.rows())
    }
}

/// Minimal keystroke → PTY byte encoding. Enough to use a shell; the full xterm
/// encoder (mouse, all the modified arrows) comes with the real painter.
fn encode_key(ks: &Keystroke) -> Vec<u8> {
    let key = ks.key.as_str();

    // Ctrl+letter → control code.
    if ks.modifiers.control && key.len() == 1 {
        if let Some(c) = key.chars().next() {
            if c.is_ascii_alphabetic() {
                return vec![(c.to_ascii_lowercase() as u8) & 0x1f];
            }
        }
    }

    let named: &[u8] = match key {
        "enter" => b"\r",
        "backspace" => b"\x7f",
        "tab" => b"\t",
        "escape" => b"\x1b",
        "space" => b" ",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        _ => b"",
    };
    if !named.is_empty() {
        return named.to_vec();
    }

    // A typed character (respects the layout / shift).
    if let Some(text) = &ks.key_char {
        return text.as_bytes().to_vec();
    }
    if key.chars().count() == 1 {
        return key.as_bytes().to_vec();
    }
    Vec::new()
}
