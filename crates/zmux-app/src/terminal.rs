//! A native terminal tab: `alacritty_terminal` emulation over a `zmux-term` PTY,
//! painted by gpui.
//!
//! The PTY is spawned from a `Target::build_command` argv, so local and remote
//! are one code path (this scaffold uses `LocalTarget`; a remote target is the
//! same call). Terminal bytes never cross an RPC — they go PTY → emulator →
//! screen, exactly as the invariants require.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor};
use parking_lot::Mutex;
use gpui::{
    App, Bounds, Context, Element, FocusHandle, Focusable, Font, FontWeight, GlobalElementId, Hsla,
    InspectorElementId, KeyDownEvent, Keystroke, LayoutId, MouseButton, MouseDownEvent, Pixels,
    Rgba, Size, Style, TextAlign, TextRun, UnderlineStyle, Window, div, font, point, prelude::*, px,
    relative, size,
};
use zmux_term::{TermSize, Terminal, TerminalEvent};
use zmux_transport::{CommandSpec, LocalTarget, ResolvedCommand, Target};
use theme::ActiveTheme;

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

/// Captures the terminal title set via OSC (the default `VoidListener` drops it).
#[derive(Clone)]
struct TitleSink(Arc<Mutex<String>>);

impl EventListener for TitleSink {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => *self.0.lock() = title,
            Event::ResetTitle => self.0.lock().clear(),
            _ => {}
        }
    }
}

/// The alacritty emulation of the PTY byte stream.
struct Emu {
    term: Term<TitleSink>,
    parser: Processor,
    title: Arc<Mutex<String>>,
}

impl Emu {
    fn new(cols: u16, rows: u16) -> Self {
        let size = Dims { columns: cols.max(1) as usize, screen_lines: rows.max(1) as usize };
        let title = Arc::new(Mutex::new(String::new()));
        Self {
            term: Term::new(Config::default(), &size, TitleSink(title.clone())),
            parser: Processor::new(),
            title,
        }
    }

    fn title(&self) -> String {
        self.title.lock().clone()
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let size = Dims { columns: cols.max(1) as usize, screen_lines: rows.max(1) as usize };
        self.term.resize(size);
    }

    /// Snapshot the visible screen as per-cell styled data, with the cursor cell
    /// inverted so it paints as a block. Colors are resolved to `Hsla` here so
    /// the paint path is pure geometry.
    fn snapshot(&self, palette: &TerminalPalette) -> Frame {
        let (cols, lines, mut rows) = {
            let grid = self.term.grid();
            let cols = grid.columns();
            let lines = grid.screen_lines();
            let rows: Vec<Vec<CellSnap>> = (0..lines)
                .map(|row| {
                    (0..cols)
                        .map(|col| cell_snap(&grid[Line(row as i32)][Column(col)], palette))
                        .collect()
                })
                .collect();
            (cols, lines, rows)
        };

        let cursor = self.term.renderable_content().cursor;
        if cursor.shape != CursorShape::Hidden {
            let cursor_row = cursor.point.line.0;
            let cursor_col = cursor.point.column.0;
            if cursor_row >= 0 && (cursor_row as usize) < lines && cursor_col < cols {
                let cell = &mut rows[cursor_row as usize][cursor_col as usize];
                cell.fg = palette.bg;
                cell.bg = Some(palette.cursor);
            }
        }

        Frame { rows }
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
    /// A fixed tab label for sessions that carry one (an adopted agent session's
    /// alias/name). When `None`, the tab falls back to the OSC shell title.
    label: Option<String>,
}

impl TerminalView {
    /// Spawn a terminal running `cmd` (a locally-resolvable argv — local shell
    /// or `ssh … -- zmuxd attach …`). `label`, when set, overrides the
    /// OSC-derived tab title. Spawning is synchronous: the PTY is a std thread,
    /// and `Target::build_command` never does I/O, so the gpui thread never
    /// blocks. Remote *connection* (ControlMaster + provisioning) is done
    /// before this call, by the backend service.
    ///
    /// Windowless on purpose: the caller focuses the view after construction,
    /// so a terminal can be created from an async task that has resolved an
    /// attach argv but only later gains window access (`update_in`).
    pub fn new(cx: &mut Context<Self>, cmd: ResolvedCommand, label: Option<String>) -> Self {
        let pty = Terminal::spawn(&cmd, None, TermSize { cols: COLS, rows: ROWS })
            .expect("spawn pty");

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

        Self {
            focus: cx.focus_handle(),
            emu: Emu::new(COLS, ROWS),
            pty,
            size: TermSize { cols: COLS, rows: ROWS },
            measured: Rc::new(Cell::new(Size::default())),
            label,
        }
    }

    /// A local login shell, as the workspace's default new tab.
    pub fn new_local(cx: &mut Context<Self>) -> Self {
        let cmd = LocalTarget::new()
            .build_command(&CommandSpec::login_shell())
            .expect("build local login shell");
        Self::new(cx, cmd, None)
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

    /// The tab label: a session's fixed label when set, else the OSC shell
    /// title, else a fallback. The old UI honored a host-side alias verbatim
    /// for adopted sessions; a local shell has no label and shows its shell.
    pub fn title(&self) -> String {
        if let Some(label) = &self.label {
            if !label.is_empty() {
                return label.clone();
            }
        }
        let title = self.emu.title();
        if title.is_empty() { "zsh".to_string() } else { title }
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
        let mut bold_font = font(FONT_FAMILY);
        bold_font.weight = FontWeight::BOLD;
        let colors = cx.theme().colors();
        let palette = TerminalPalette {
            fg: colors.terminal_foreground,
            bg: colors.terminal_background,
            cursor: colors.terminal_foreground,
            ansi: [
                colors.terminal_ansi_black,
                colors.terminal_ansi_red,
                colors.terminal_ansi_green,
                colors.terminal_ansi_yellow,
                colors.terminal_ansi_blue,
                colors.terminal_ansi_magenta,
                colors.terminal_ansi_cyan,
                colors.terminal_ansi_white,
                colors.terminal_ansi_bright_black,
                colors.terminal_ansi_bright_red,
                colors.terminal_ansi_bright_green,
                colors.terminal_ansi_bright_yellow,
                colors.terminal_ansi_bright_blue,
                colors.terminal_ansi_bright_magenta,
                colors.terminal_ansi_bright_cyan,
                colors.terminal_ansi_bright_white,
            ],
        };
        let term_bg = colors.terminal_background;
        let active_border = colors.pane_focused_border;
        let element = TerminalElement {
            frame: self.emu.snapshot(&palette),
            measured: self.measured.clone(),
            font_size: px(FONT_SIZE),
            line_height: px(LINE_HEIGHT),
            base_font: font(FONT_FAMILY),
            bold_font,
        };
        div()
            .track_focus(&self.focus)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                }),
            )
            .size_full()
            .bg(term_bg)
            // A 1px border, tinted only while focused, marks the active pane.
            .border_1()
            .border_color(term_bg)
            .focus(|style| style.border_color(active_border))
            .child(element)
    }
}

/// One cell's rendered attributes (colors already resolved to `Hsla`).
#[derive(Clone, PartialEq)]
struct CellSnap {
    c: char,
    fg: Hsla,
    bg: Option<Hsla>,
    bold: bool,
    underline: bool,
}

/// Resolved terminal colors, threaded from the theme through `snapshot()` to
/// the per-cell color resolution functions. All `Hsla` values are `Copy`.
struct TerminalPalette {
    fg: Hsla,
    bg: Hsla,
    cursor: Hsla,
    ansi: [Hsla; 16],
}

/// A snapshot of the visible screen, one row of cells at a time.
struct Frame {
    rows: Vec<Vec<CellSnap>>,
}

/// Paints a `Frame` cell-by-cell and reports its bounds back for `reflow`.
/// Rows are shaped into colored runs; gpui paints the glyphs, per-run
/// backgrounds, and underlines. The block cursor is just an inverted cell.
struct TerminalElement {
    frame: Frame,
    measured: Rc<Cell<Size<Pixels>>>,
    font_size: Pixels,
    line_height: Pixels,
    base_font: Font,
    bold_font: Font,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(relative(1.).into(), relative(1.).into()),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        // Report the pane's pixel bounds so the next frame can reflow the grid.
        if self.measured.get() != bounds.size {
            self.measured.set(bounds.size);
            window.refresh();
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let left = bounds.origin.x;
        let top = bounds.origin.y;
        for (row_ix, row) in self.frame.rows.iter().enumerate() {
            if row.is_empty() {
                continue;
            }
            let (text, runs) = build_runs(row, &self.base_font, &self.bold_font);
            let shaped = window
                .text_system()
                .shape_line(text.into(), self.font_size, &runs, None);
            let origin = point(left, top + self.line_height * row_ix as f32);
            let _ = shaped.paint_background(origin, self.line_height, TextAlign::Left, None, window, cx);
            let _ = shaped.paint(origin, self.line_height, TextAlign::Left, None, window, cx);
        }
    }
}

/// Group a row's cells into runs of identical style and build the row string
/// plus its `TextRun`s for `shape_line`.
fn build_runs(row: &[CellSnap], base_font: &Font, bold_font: &Font) -> (String, Vec<TextRun>) {
    let mut text = String::with_capacity(row.len());
    let mut runs: Vec<TextRun> = Vec::new();
    let mut i = 0;
    while i < row.len() {
        let head = &row[i];
        let mut run_text = String::new();
        run_text.push(head.c);
        let mut j = i + 1;
        while j < row.len() && same_style(&row[j], head) {
            run_text.push(row[j].c);
            j += 1;
        }
        runs.push(TextRun {
            len: run_text.len(),
            font: if head.bold { bold_font.clone() } else { base_font.clone() },
            color: head.fg,
            background_color: head.bg,
            underline: head.underline.then(|| UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            }),
            strikethrough: None,
        });
        text.push_str(&run_text);
        i = j;
    }
    (text, runs)
}

fn same_style(a: &CellSnap, b: &CellSnap) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.bold == b.bold && a.underline == b.underline
}

fn rgb8(r: u8, g: u8, b: u8) -> Hsla {
    Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: 1. }.into()
}

fn resolve(color: Color, palette: &TerminalPalette) -> Hsla {
    match color {
        Color::Named(named) => resolve_named(named, palette),
        Color::Spec(spec) => rgb8(spec.r, spec.g, spec.b),
        Color::Indexed(i) => resolve_indexed(i, palette),
    }
}

fn resolve_named(named: NamedColor, palette: &TerminalPalette) -> Hsla {
    match named {
        NamedColor::Background => palette.bg,
        NamedColor::Cursor => palette.cursor,
        NamedColor::Foreground | NamedColor::BrightForeground => palette.fg,
        other => {
            let idx = other as usize;
            if idx < 16 { palette.ansi[idx] } else { palette.fg }
        }
    }
}

fn resolve_indexed(i: u8, palette: &TerminalPalette) -> Hsla {
    match i {
        0..=15 => palette.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            rgb8(step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232);
            rgb8(v, v, v)
        }
    }
}

/// Default background cells stay `None` so the pane background shows through.
fn resolve_bg(color: Color, palette: &TerminalPalette) -> Option<Hsla> {
    match color {
        Color::Named(NamedColor::Background) => None,
        other => Some(resolve(other, palette)),
    }
}

fn cell_snap(cell: &alacritty_terminal::term::cell::Cell, palette: &TerminalPalette) -> CellSnap {
    let flags = cell.flags;
    let mut fg = resolve(cell.fg, palette);
    let mut bg = resolve_bg(cell.bg, palette);
    if flags.contains(Flags::INVERSE) {
        let prev_fg = fg;
        fg = bg.unwrap_or(palette.bg);
        bg = Some(prev_fg);
    }
    let c = if flags.contains(Flags::HIDDEN) || cell.c == '\0' { ' ' } else { cell.c };
    CellSnap {
        c,
        fg,
        bg,
        bold: flags.contains(Flags::BOLD),
        underline: flags.contains(Flags::UNDERLINE),
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
