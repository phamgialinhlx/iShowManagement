//! Turning Claude Code's terminal output into structured state.
//!
//! Claude Code is a full-screen TUI: it draws boxes, moves the cursor, redraws in
//! place. The only honest way to know what it is *showing* is to run its bytes
//! through a real terminal emulator and read the resulting screen — which is what
//! [`Screen`] does with `alacritty_terminal`.
//!
//! Everything below that point is a pure function over the rendered text, so the
//! parsing is testable against captured screens without a PTY, a network, or a
//! running Claude.
//!
//! **Why this exists at all.** The previous generation relayed prompts through a
//! server and pressed keys from a poller. That produced ghost cards (a prompt
//! shown after it had been answered) and eaten answers (a keystroke delivered to
//! a screen that had moved on). Reading the screen we are actually looking at,
//! and answering the *same* screen synchronously, removes both by construction.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use serde::{Deserialize, Serialize};

/// A live emulation of what Claude is displaying.
pub struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
}

/// Terminal dimensions for the emulator.
#[derive(Clone, Copy, Debug)]
struct Size {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for Size {
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

impl Screen {
    pub fn new(columns: u16, rows: u16) -> Self {
        let size = Size { columns: columns.max(1) as usize, screen_lines: rows.max(1) as usize };
        Self { term: Term::new(Config::default(), &size, VoidListener), parser: Processor::new() }
    }

    /// Feed bytes straight from the PTY.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, columns: u16, rows: u16) {
        let size = Size { columns: columns.max(1) as usize, screen_lines: rows.max(1) as usize };
        self.term.resize(size);
    }

    /// The visible screen as lines of text, trailing blanks trimmed.
    pub fn lines(&self) -> Vec<String> {
        let grid = self.term.grid();
        let columns = grid.columns();

        (0..grid.screen_lines())
            .map(|row| {
                let mut line = String::with_capacity(columns);
                for col in 0..columns {
                    line.push(grid[alacritty_terminal::index::Line(row as i32)]
                        [alacritty_terminal::index::Column(col)]
                    .c);
                }
                line.trim_end().to_owned()
            })
            .collect()
    }

    /// What Claude is currently asking, if anything.
    pub fn state(&self) -> ClaudeState {
        parse_state(&self.lines())
    }
}

/// A choice Claude is offering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    /// The digit to press. Claude labels its options `1.`, `2.`, `3.`
    pub key: String,
    pub label: String,
    /// Whether the TUI is currently highlighting this option.
    pub selected: bool,
}

/// What the session is doing right now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeState {
    /// Claude is asking something and is blocked until answered.
    pub prompt: Option<Prompt>,
    /// Claude is working — the operator need not do anything.
    pub working: bool,
}

/// A question awaiting an answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    /// The question, as Claude worded it.
    pub question: String,
    pub choices: Vec<Choice>,
    /// A stable identity for this prompt, so the UI can tell a redraw of the
    /// same question from a genuinely new one. Without it, every repaint would
    /// look like a fresh prompt and the card would flicker or duplicate — one of
    /// the exact failures this design exists to avoid.
    pub fingerprint: String,
}

/// Lines Claude draws while it is thinking. Matched case-insensitively.
const WORKING_MARKERS: &[&str] = &["esc to interrupt", "esc to stop"];

/// Does this line look like Claude's "still going" spinner?
///
/// **The hint is not always on screen.** Detection used to rest entirely on
/// `esc to interrupt`, and inline rendering frequently does not print it — so a
/// session that was plainly working (`Adding due dates and priority… (2m 40s ·
/// ↓ 4.0k tokens)`) reported idle, and the rail said nothing was happening
/// anywhere. That defeats the rail's whole purpose.
///
/// What *is* always there is the elapsed-time parenthetical: an ellipsis, then
/// a bracket, then a duration. Matching that shape rather than a phrase also
/// survives Claude renaming the verb, which it does constantly — every spinner
/// word is different.
///
/// Deliberately narrow: the digits must be followed by a time unit, so ordinary
/// prose ending in "… (see below)" does not read as work in progress.
fn looks_like_spinner(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find('…') {
        rest = &rest[at + '…'.len_utf8()..];
        let after = rest.trim_start();
        let Some(after) = after.strip_prefix('(') else { continue };

        let digits = after.trim_start_matches(|c: char| c.is_ascii_digit());
        // There must have been at least one digit, and a unit must follow it.
        if digits.len() < after.len() && digits.starts_with(['m', 's', 'h']) {
            return true;
        }
    }
    false
}

/// Recognise a numbered option line: `❯ 1. Yes` / `  2. No`.
///
/// Returns `(key, label, selected)`.
fn parse_choice(line: &str) -> Option<(String, String, bool)> {
    // Box drawing and the selection caret are chrome, not content.
    let cleaned = line.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '|' | '╭' | '╮' | '╰' | '╯' | '─' | '━')
    });
    let selected = cleaned.starts_with('❯') || cleaned.starts_with('>');
    let cleaned = cleaned.trim_start_matches(['❯', '>']).trim();

    let (digits, rest) = cleaned.split_at(cleaned.find(|c: char| !c.is_ascii_digit())?);
    if digits.is_empty() {
        return None;
    }

    // The separator must be "." or ")" — otherwise "2024 was a year" parses as
    // an option.
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let label = cut_at_neighbour(rest.trim());
    if label.is_empty() {
        return None;
    }

    Some((digits.to_owned(), label.to_owned(), selected))
}

/// Cut a label where a *different* panel starts on the same row.
///
/// Claude runs inline (see `Rendering`), so a redraw paints only the columns it
/// writes — anything an earlier message left further right stays on the row.
/// Options and a leftover diagram then share rows, and reading a label to the
/// end of the line swallows the neighbour: `Cap the answer shape │ turn starts`.
///
/// **The damage is not cosmetic.** The prompt's fingerprint is the question plus
/// every label, so a neighbour that changes as Claude streams changes the labels,
/// changes the fingerprint, and makes `Session::answer` refuse every click as
/// "no longer on screen". The operator experiences that as a card that cannot be
/// clicked and a question that keeps coming back — which is exactly how it was
/// reported, with nothing on screen connecting it to a stray diagram.
///
/// Box drawing is the signal because an option label never contains any: it is
/// chrome by construction, drawn by a TUI rather than typed by whoever wrote the
/// choice. The ASCII pipe is deliberately **not** included — `Use A | B` is a
/// perfectly ordinary label, and truncating it would invent a different bug in
/// the name of fixing this one. The frame's own edges are already gone by here,
/// trimmed from both ends, so what is left is a genuine interior neighbour.
fn cut_at_neighbour(label: &str) -> &str {
    match label.find(|c: char| ('\u{2500}'..='\u{257f}').contains(&c)) {
        Some(at) => label[..at].trim_end(),
        None => label,
    }
}

/// Is this option line drawn inside Claude's box frame?
///
/// Only the vertical rules count. A `─` run is the *trust* prompt's separator
/// rather than a frame, and matching it would re-admit every numbered list that
/// happens to sit under a horizontal rule.
fn is_framed(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with(['│', '|']) && trimmed.ends_with(['│', '|'])
}

/// Strip box-drawing characters and surrounding whitespace from a line.
fn strip_chrome(line: &str) -> String {
    line.trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '|' | '╭' | '╮' | '╰' | '╯' | '─' | '━' | '╌')
    })
    .to_owned()
}

/// How far above the options to look for the question.
///
/// Claude's dialogs put explanatory prose, links and blank lines between the
/// question and the choices, so the search has to reach past them — but not so
/// far that it picks up unrelated conversation further up the screen.
const QUESTION_SEARCH_LINES: usize = 16;

/// Pull the question out of the lines above a dialog's options.
///
/// The obvious rule — "the nearest non-empty line" — is wrong, and a real dialog
/// proves it. Claude's trust prompt ends with a "Security guide" link directly
/// above the choices, so the nearest line is a link label and the card would ask
/// the operator to approve something described as "Security guide".
///
/// Instead the lines are grouped into blocks (contiguous non-empty text), and the
/// nearest block **containing a question mark** wins. The block is then truncated
/// at that question mark, which drops the wrapped explanatory tail that follows
/// it. Only if no block asks anything does this fall back to the nearest text.
fn extract_question(above: &[String]) -> String {
    let start = above.len().saturating_sub(QUESTION_SEARCH_LINES);
    let window = &above[start..];

    // Group into blocks, nearest last.
    let mut blocks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in window {
        let cleaned = strip_chrome(line);
        if cleaned.is_empty() || parse_choice(&cleaned).is_some() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(cleaned);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    let joined = |block: &Vec<String>| block.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");

    // Nearest block that actually asks something.
    if let Some(block) = blocks.iter().rev().find(|b| joined(b).contains('?')) {
        let text = joined(block);
        if let Some(end) = text.find('?') {
            return text[..=end].to_owned();
        }
        return text;
    }

    // Nothing asks a question — fall back to the nearest text so the card is not
    // blank.
    blocks.last().map(joined).unwrap_or_default()
}

/// Derive structured state from a rendered screen.
pub fn parse_state(lines: &[String]) -> ClaudeState {
    let working = lines.iter().any(|l| {
        let lower = l.to_lowercase();
        WORKING_MARKERS.iter().any(|m| lower.contains(m)) || looks_like_spinner(l)
    });

    let mut choices: Vec<Choice> = Vec::new();
    let mut first_choice_row = None;

    for (row, line) in lines.iter().enumerate() {
        if let Some((key, label, selected)) = parse_choice(line) {
            if first_choice_row.is_none() {
                first_choice_row = Some(row);
            }
            choices.push(Choice { key, label, selected });
        }
    }

    // A lone numbered line is ordinary output — a list in a code block, a
    // changelog. Two or more consecutive options is what makes it a dialog.
    if choices.len() < 2 {
        return ClaudeState { prompt: None, working };
    }

    // **A dialog has a cursor; prose does not.** Counting numbered lines alone
    // turned any "1. … 2. … 3. …" list into a decision card — measured on a real
    // session, a six-point security summary became a six-option card covering the
    // terminal. It went unnoticed for as long as it did only because a separate
    // CSS fault (see `.corner` in `signal-room.css`) clipped that card to a 12px
    // sliver, which was reported as a black bar; fixing the position exposed this
    // underneath it.
    //
    // Requiring the box-drawing frame was the mitigation this code suggested, and
    // it is wrong: Claude's *trust* prompt is not framed — it uses a horizontal
    // rule — so a frame check would silently stop detecting the one dialog that
    // blocks a session from ever starting.
    //
    // What both real captures have, and what a numbered list in prose never has,
    // is the selection caret marking the highlighted option. That is the TUI's
    // cursor: an interactive select always renders it, because it is how the
    // operator knows what Enter will choose. The frame is still accepted as an
    // alternative so a repaint that lands mid-frame — options drawn, caret not yet
    // — does not make the card flicker out.
    let has_cursor = choices.iter().any(|c| c.selected);
    let framed = first_choice_row.is_some_and(|row| is_framed(&lines[row]));
    if !has_cursor && !framed {
        return ClaudeState { prompt: None, working };
    }

    let question = first_choice_row.map(|row| extract_question(&lines[..row])).unwrap_or_default();

    // Identity is the question plus the option labels: a repaint produces the
    // same fingerprint, a different question produces a different one.
    let fingerprint = {
        let mut material = question.clone();
        for choice in &choices {
            material.push('\u{1f}');
            material.push_str(&choice.key);
            material.push(':');
            material.push_str(&choice.label);
        }
        fnv1a(&material)
    };

    ClaudeState {
        prompt: Some(Prompt { question, choices, fingerprint }),
        // A blocked prompt is not "working", whatever else is on screen.
        working: false,
    }
}

fn fnv1a(s: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    /// A permission dialog as Claude Code actually draws it.
    const PERMISSION_DIALOG: &str = r#"
● I'll update the config file.

╭──────────────────────────────────────────────────╮
│ Do you want to make this edit to settings.json?  │
│                                                  │
│ ❯ 1. Yes                                         │
│   2. Yes, and don't ask again this session       │
│   3. No, and tell Claude what to do differently  │
╰──────────────────────────────────────────────────╯
"#;

    /// Options with **another panel drawn beside them**, reproduced from a real
    /// session where the card became unusable.
    ///
    /// Claude runs inline, so a redraw only paints the columns it writes and
    /// whatever an earlier message left further right stays on the row. The
    /// options and that leftover diagram then share rows, and reading a label to
    /// the end of the line swallows it.
    ///
    /// The operator's report was not "the labels look wrong" — it was **"I can't
    /// click the first answer and it keeps showing the question again"**, which
    /// is the second-order effect: the panel's text changes as Claude streams,
    /// so the labels change, so the fingerprint changes, so every answer is
    /// refused as stale.
    const SIDE_BY_SIDE: &str = r#"
Which should I build next?

 ❯ 1. Quick-ack agent                    ┌──────────────────────────┐
   2. Cap the answer shape               │ turn starts              │
   3. Fold read-channel into             │   ├─ ack agent (4k ctx,  │
   4. Move per-user blocks last          │  nhắn của anh…"          │
"#;

    /// The same screen one repaint later: only the *neighbour* changed.
    const SIDE_BY_SIDE_LATER: &str = r#"
Which should I build next?

 ❯ 1. Quick-ack agent                    ┌──────────────────────────┐
   2. Cap the answer shape               │ turn ends                │
   3. Fold read-channel into             │   └─ main agent          │
   4. Move per-user blocks last          │  real answer at ~64s     │
"#;

    #[test]
    fn a_neighbouring_panel_is_not_part_of_the_label() {
        let prompt = parse_state(&lines(SIDE_BY_SIDE)).prompt.expect("a real dialog");

        assert_eq!(
            prompt.choices.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
            [
                "Quick-ack agent",
                "Cap the answer shape",
                "Fold read-channel into",
                "Move per-user blocks last",
            ],
            "labels must stop where the adjacent panel starts",
        );
    }

    #[test]
    fn a_neighbours_repaint_does_not_invalidate_the_answer() {
        // The bug that made the card unusable. These are the same question with
        // the same options; only the unrelated panel beside them moved. If the
        // fingerprints differ, `answer` refuses every click as stale and the
        // operator sees the question come back untouched.
        let first = parse_state(&lines(SIDE_BY_SIDE)).prompt.expect("a dialog");
        let second = parse_state(&lines(SIDE_BY_SIDE_LATER)).prompt.expect("a dialog");

        assert_eq!(
            first.fingerprint, second.fingerprint,
            "a neighbour repainting is not a new question",
        );
    }

    /// The real trust prompt, captured from Claude Code 2.1.220 on a live host.
    const REAL_TRUST_PROMPT: &str = r#"
────────────────────────────────────────────────────────────
 Accessing workspace:

 /home/dev/zmux-dialog-probe-534

 Quick safety check: Is this a project you created or one you trust? (Like your own code, a
 well-known open source project, or work from your team). If not, take a moment to review what's in
 this folder first.

 Claude Code'll be able to read, edit, and execute files here.

 Security guide

 ❯ 1. Yes, I trust this folder
   2. No, exit

 Enter to confirm · Esc to cancel
"#;

    #[test]
    fn the_real_trust_prompt_yields_the_actual_question() {
        // Captured from a live server. The nearest line above the options is the
        // "Security guide" link, so a naive reading asks the operator to approve
        // something labelled "Security guide" — which tells them nothing about
        // what they are agreeing to.
        let state = parse_state(&lines(REAL_TRUST_PROMPT));
        let prompt = state.prompt.expect("the real dialog should parse");

        assert_eq!(
            prompt.question,
            "Quick safety check: Is this a project you created or one you trust?"
        );
        assert_eq!(prompt.choices.len(), 2);
        assert_eq!(prompt.choices[0].label, "Yes, I trust this folder");
        assert!(prompt.choices[0].selected, "the caret marks the first option");
        assert_eq!(prompt.choices[1].label, "No, exit");
    }

    #[test]
    fn a_question_is_preferred_over_nearer_prose() {
        let screen = lines(
            "Should I delete the old migrations?\n\nSee the docs\n\n ❯ 1. Yes\n   2. No\n",
        );
        let prompt = parse_state(&screen).prompt.unwrap();
        // "See the docs" is nearer, but it asks nothing.
        assert_eq!(prompt.question, "Should I delete the old migrations?");
    }

    #[test]
    fn a_dialog_with_no_question_mark_still_gets_a_label() {
        // Better a nearby line than an empty card.
        let prompt = parse_state(&lines("Choose a branch\n\n ❯ 1. main\n   2. develop\n"))
            .prompt
            .unwrap();
        assert_eq!(prompt.question, "Choose a branch");
    }

    #[test]
    fn a_permission_dialog_is_recognised_with_its_options() {
        let state = parse_state(&lines(PERMISSION_DIALOG));
        let prompt = state.prompt.expect("should have detected a prompt");

        assert_eq!(prompt.question, "Do you want to make this edit to settings.json?");
        assert_eq!(prompt.choices.len(), 3);
        assert_eq!(prompt.choices[0].key, "1");
        assert_eq!(prompt.choices[0].label, "Yes");
        assert!(prompt.choices[0].selected, "the caret marks the highlighted option");
        assert!(!prompt.choices[1].selected);
        assert_eq!(prompt.choices[2].label, "No, and tell Claude what to do differently");
    }

    #[test]
    fn a_blocked_prompt_is_not_reported_as_working() {
        // The operator must act; showing it as busy would hide that.
        let state = parse_state(&lines(PERMISSION_DIALOG));
        assert!(!state.working);
    }

    #[test]
    fn a_repaint_of_the_same_question_keeps_its_identity() {
        // Claude redraws constantly. If each repaint looked like a new prompt the
        // UI would flicker or stack duplicate cards — the "ghost card" failure.
        let first = parse_state(&lines(PERMISSION_DIALOG)).prompt.unwrap();

        // Same dialog, different highlight and re-wrapped box.
        let moved = PERMISSION_DIALOG.replace("❯ 1. Yes", "  1. Yes").replace("  2. Yes,", "❯ 2. Yes,");
        let second = parse_state(&lines(&moved)).prompt.unwrap();

        assert_eq!(first.fingerprint, second.fingerprint);
        // ...but the highlight moved, and that is visible.
        assert!(!second.choices[0].selected);
        assert!(second.choices[1].selected);
    }

    #[test]
    fn a_different_question_gets_a_different_identity() {
        let first = parse_state(&lines(PERMISSION_DIALOG)).prompt.unwrap();
        let other = PERMISSION_DIALOG.replace("settings.json", "main.rs");
        let second = parse_state(&lines(&other)).prompt.unwrap();

        assert_ne!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn working_is_detected_from_the_interrupt_hint() {
        let state = parse_state(&lines("● Thinking…\n\n  ⏵⏵ esc to interrupt\n"));
        assert!(state.working);
        assert!(state.prompt.is_none());
    }

    /// A numbered list in prose is not a dialog.
    ///
    /// This asserted the opposite for months — "documenting current behaviour,
    /// not endorsing it" — and the behaviour it documented was a real fault:
    /// on a live session a six-point security summary became a six-option
    /// decision card drawn over the terminal. Nobody reported it because a CSS
    /// fault clipped that card to a 12px sliver, which was reported instead as
    /// a black bar along the bottom of the pane.
    #[test]
    fn a_numbered_list_in_prose_is_not_a_dialog() {
        let state = parse_state(&lines(
            "Here is the plan:\n\n1. Read the config\n2. Update the port\n3. Restart",
        ));
        assert!(state.prompt.is_none(), "no cursor and no frame — this is prose");
    }

    #[test]
    fn a_single_numbered_line_is_never_a_dialog() {
        let state = parse_state(&lines("Step 1. Install the dependencies\n"));
        assert!(state.prompt.is_none());
    }

    #[test]
    fn a_year_is_not_an_option() {
        // "2024 was a good year" starts with digits but has no ". " separator.
        let state = parse_state(&lines("2024 was a good year\n2025 will be better\n"));
        assert!(state.prompt.is_none());
    }

    #[test]
    fn an_empty_screen_is_idle() {
        let state = parse_state(&[]);
        assert!(state.prompt.is_none());
        assert!(!state.working);
    }

    #[test]
    fn the_emulator_renders_what_was_written() {
        let mut screen = Screen::new(40, 6);
        screen.feed(b"hello world");

        assert_eq!(screen.lines()[0], "hello world");
    }

    #[test]
    fn the_emulator_applies_cursor_movement_rather_than_appending() {
        // The whole reason a real VTE is used: Claude redraws in place, so naive
        // byte concatenation would show every intermediate frame at once.
        let mut screen = Screen::new(40, 6);
        screen.feed(b"first draft");
        // Carriage return to the start of the line, then overwrite.
        screen.feed(b"\rsecond     ");

        assert_eq!(screen.lines()[0], "second");
    }

    #[test]
    fn a_dialog_is_detected_through_the_emulator() {
        let mut screen = Screen::new(60, 12);
        for line in PERMISSION_DIALOG.lines() {
            screen.feed(line.as_bytes());
            screen.feed(b"\r\n");
        }

        let prompt = screen.state().prompt.expect("dialog should survive emulation");
        assert_eq!(prompt.choices.len(), 3);
    }
}

#[cfg(test)]
mod spinner_tests {
    use super::*;

    #[test]
    fn the_elapsed_timer_is_enough_to_mean_working() {
        // Taken from a real screen. There is no "esc to interrupt" anywhere on
        // it, which is why the rail showed every session idle while Claude was
        // plainly working on several of them.
        for line in [
            "* Adding due dates and priority… (2m 40s · ↓ 4.0k tokens)",
            "✳ Inferring… (4m 4s · ↓ 4.7k tokens)",
            "· Thinking… (12s)",
            "  Running… (51s)",
        ] {
            assert!(parse_state(&[line.to_owned()]).working, "missed: {line}");
        }
    }

    #[test]
    fn the_old_hint_still_counts() {
        assert!(parse_state(&["  (esc to interrupt)".to_owned()]).working);
    }

    #[test]
    fn prose_that_merely_trails_off_is_not_work() {
        // The reason the digits must be followed by a unit: without that check
        // any parenthetical after an ellipsis would pin the rail to "working"
        // forever, which is worse than the bug being fixed — a signal that is
        // always on carries nothing.
        for line in [
            "and so on… (see below)",
            "wait for it… (the 3 options)",
            "done.",
            "",
        ] {
            assert!(!parse_state(&[line.to_owned()]).working, "false positive: {line}");
        }
    }

    #[test]
    fn a_blocked_prompt_still_outranks_the_spinner() {
        // A question on screen means the operator must act, whatever else is
        // being rendered — otherwise a session needing an answer would show as
        // busy and never be visited.
        let screen = [
            "Do you want to proceed?".to_owned(),
            "❯ 1. Yes".to_owned(),
            "  2. No".to_owned(),
            "* Working… (3s)".to_owned(),
        ];
        let state = parse_state(&screen);
        assert!(state.prompt.is_some());
        assert!(!state.working);
    }
}
