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
use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Context, FocusHandle, Focusable, KeyDownEvent, Keystroke, Pixels, Size, Window, canvas,
    div, font, prelude::*, px, rgb,
};
use rmux_term::{TermSize, Terminal, TerminalEvent};
use rmux_transport::{CommandSpec, LocalTarget, Target};

/// Initial PTY grid, before the pane has been measured (see `reflow`).
const COLS: u16 = 100;
const ROWS: u16 = 30;
const FONT_FAMILY: &str = "Lilex";
const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 17.;

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

    fn resize(&mut self, cols: u16, rows: u16) {
        let size = Dims { columns: cols.max(1) as usize, screen_lines: rows.max(1) as usize };
        self.term.resize(size);
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
    /// The grid size the emulator and PTY are currently at.
    size: TermSize,
    /// Pane pixel bounds, written each frame by the measuring `canvas` and read
    /// by `reflow` on the next frame to resize the grid to fit.
    measured: Rc<Cell<Size<Pixels>>>,
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
            loop {
                let Ok(event) = rx.recv().await else { break };
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
        Self {
            focus,
            emu: Emu::new(COLS, ROWS),
            pty,
            size: TermSize { cols: COLS, rows: ROWS },
            measured: Rc::new(Cell::new(Size::default())),
        }
    }

    /// Resize the emulator grid and PTY to fit the last measured pane bounds.
    /// A no-op until the pane has been measured, or when the cell count is
    /// unchanged.
    fn reflow(&mut self, cx: &mut Context<Self>) {
        let px_size = self.measured.get();
        if px_size.width <= px(0.) || px_size.height <= px(0.) {
            return;
        }
        let font_id = cx.text_system().resolve_font(&font(FONT_FAMILY));
        let cell_width = cx
            .text_system()
            .em_advance(font_id, px(FONT_SIZE))
            .unwrap_or(px(FONT_SIZE * 0.6));
        let cols = (px_size.width / cell_width).floor().max(1.) as u16;
        let rows = (px_size.height / px(LINE_HEIGHT)).floor().max(1.) as u16;
        if cols == self.size.cols && rows == self.size.rows {
            return;
        }
        self.size = TermSize { cols, rows };
        self.emu.resize(cols, rows);
        let _ = self.pty.resize(self.size);
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, _: &mut Context<Self>) {
        let bytes = encode_key(&event.keystroke);
        if !bytes.is_empty() {
            let _ = self.pty.write(&bytes);
        }
    }

    /// Whether this terminal currently holds window focus (used by the workspace
    /// to track the active pane).
    pub fn has_focus(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reflow(cx);
        let measured = self.measured.clone();
        div()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(rgb(0x14110f))
            .text_color(rgb(0xe8e6e1))
            .font_family(FONT_FAMILY)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .flex()
            .flex_col()
            // An invisible overlay that reports the pane's pixel bounds so the
            // next frame can reflow the grid to fit. Absolutely positioned so it
            // stays out of the text flex flow.
            .child(
                canvas(
                    move |bounds, window, _cx| {
                        if measured.get() != bounds.size {
                            measured.set(bounds.size);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .children(self.emu.rows())
    }
}

/// Minimal keystroke → PTY byte encoding. Enough to use a shell; the full xterm
/// encoder (mouse, all the modified arrows) comes with the real painter.
fn encode_key(ks: &Keystroke) -> Vec<u8> {
    let key = ks.key.as_str();

    // Cmd combos are workspace shortcuts (split/close), not terminal input.
    if ks.modifiers.platform {
        return Vec::new();
    }

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
