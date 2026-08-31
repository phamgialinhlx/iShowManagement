//! The keystrokes that drive Claude Code.
//!
//! This is the only module that knows how Claude's TUI is operated, kept apart so
//! the mapping is testable and so a change to its interface has exactly one place
//! to land.
//!
//! Two rules govern everything here, both learned expensively:
//!
//! 1. **Answer with the digit, not the arrow keys.** Selecting by pressing Down N
//!    times depends on where the highlight already is — and it moves on its own as
//!    Claude redraws. Pressing `2` selects option 2 no matter what is highlighted.
//! 2. **Send text and Enter as separate writes.** Appending `\r` to a paste makes
//!    the TUI treat the whole thing as one bracketed chunk and swallow the
//!    newline; the message then sits in the composer, typed but never sent.

/// Bytes to press one of Claude's numbered options.
///
/// The digit alone: Claude's dialogs commit immediately on digit-select, and
/// adding a newline would submit an extra empty line to whatever comes next.
pub fn choose(key: &str) -> Vec<u8> {
    key.as_bytes().to_vec()
}

/// Bytes to interrupt whatever Claude is doing.
pub const fn interrupt() -> &'static [u8] {
    // Escape. Claude prints "esc to interrupt" precisely because this is it.
    b"\x1b"
}

/// A message split into the writes needed to send it.
///
/// Returned as separate chunks — see rule 2 above. The caller must write them in
/// order, and should let the terminal settle between them.
pub fn send_message(text: &str) -> Vec<Vec<u8>> {
    if text.is_empty() {
        return Vec::new();
    }

    // Newlines inside the message would submit it early, so they become the
    // shift+enter sequence Claude uses for a literal line break in the composer.
    let body = text.replace('\n', "\\\r");

    vec![body.into_bytes(), b"\r".to_vec()]
}

/// A message split into the writes needed to send it to **pi**.
///
/// Single-line is byte-identical to what already works over the bridge: the text,
/// then Enter. Only multi-line differs, and it must — Claude's `\\\r` line-break
/// escape (see `send_message`) is Claude's composer alone; pi renders the
/// backslash literally and submits at the first `\r`, so a Claude-encoded
/// multi-line message reaches pi mangled and cut off at line one.
///
/// The multi-line body is wrapped in a **bracketed paste** (`ESC[200~` … `ESC[201~`)
/// with literal newlines, so pi's composer ingests every line verbatim as pasted
/// content rather than acting on the embedded Enter. The trailing `\r` submits, as
/// a separate write — the same two-write discipline (and `SUBMIT_SETTLE` gap) the
/// caller already applies, so the paste is fully ingested before the return is read
/// as "submit" instead of as more pasted content.
///
/// This does not depend on knowing pi's soft-newline chord, which is why it is the
/// safe default — but it *does* assume pi honours bracketed paste, which nothing in
/// this repo pins. It **must be live-verified** against a real `pi --tui-mode
/// regular` session: if pi does not accept bracketed paste, the multi-line branch
/// is where the real mechanism (a literal `\n`, or a pi-specific chord) belongs,
/// and only observing real pi can decide it.
pub fn send_message_pi(text: &str) -> Vec<Vec<u8>> {
    if text.is_empty() {
        return Vec::new();
    }

    // Single-line: exactly what pi already accepts today. Kept byte-identical so
    // the common case cannot regress.
    if !text.contains('\n') {
        return vec![text.as_bytes().to_vec(), b"\r".to_vec()];
    }

    let mut paste = Vec::new();
    paste.extend_from_slice(b"\x1b[200~");
    paste.extend_from_slice(text.as_bytes()); // literal newlines, unescaped
    paste.extend_from_slice(b"\x1b[201~");
    vec![paste, b"\r".to_vec()]
}

/// How long to wait between the text and the Enter that submits it.
///
/// The composer needs a moment to finish processing a paste before it will treat
/// a carriage return as "submit" rather than as part of the pasted content. This
/// delay is applied where the keys are *written* — never as a round trip from
/// somewhere else, which is what made the old design drop keystrokes on a slow
/// link.
pub const SUBMIT_SETTLE: std::time::Duration = std::time::Duration::from_millis(60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_option_is_chosen_by_its_digit() {
        // Not by arrow keys: the highlight moves as Claude redraws, so a relative
        // selection lands on whatever happens to be highlighted at that instant.
        assert_eq!(choose("2"), b"2");
        assert_eq!(choose("10"), b"10");
    }

    #[test]
    fn choosing_does_not_append_a_newline() {
        // Claude commits on digit-select; an extra newline would be delivered to
        // whatever the dialog hands control to next.
        assert!(!choose("1").contains(&b'\r'));
        assert!(!choose("1").contains(&b'\n'));
    }

    #[test]
    fn a_message_is_sent_as_text_then_enter() {
        let writes = send_message("run the tests");

        assert_eq!(writes.len(), 2, "text and submit must be separate writes");
        assert_eq!(writes[0], b"run the tests");
        assert_eq!(writes[1], b"\r");
    }

    #[test]
    fn the_submit_is_never_folded_into_the_text() {
        // This is the "typed but not sent" bug: with the newline appended, the
        // TUI treats the whole thing as pasted content and the message sits in
        // the composer unsent.
        let writes = send_message("hello");
        assert!(!writes[0].ends_with(b"\r"), "the text chunk must not carry the submit");
    }

    #[test]
    fn embedded_newlines_do_not_submit_early() {
        // Otherwise a two-line message sends the first line and leaves the rest.
        let writes = send_message("first line\nsecond line");
        let text = String::from_utf8(writes[0].clone()).unwrap();

        assert!(!text.contains('\n'), "a bare newline would submit mid-message");
        assert!(text.contains("first line"));
        assert!(text.contains("second line"));
    }

    #[test]
    fn an_empty_message_sends_nothing() {
        // Submitting an empty composer would advance whatever Claude is showing.
        assert!(send_message("").is_empty());
    }

    #[test]
    fn interrupt_is_escape() {
        assert_eq!(interrupt(), b"\x1b");
    }

    #[test]
    fn a_single_line_pi_message_is_identical_to_the_claude_encoding() {
        // The common case must not regress: single-line already works over the
        // bridge, so pi's encoder has to produce exactly what it produces today.
        let pi = send_message_pi("run the tests");
        assert_eq!(pi, send_message("run the tests"));
        assert_eq!(pi, vec![b"run the tests".to_vec(), b"\r".to_vec()]);
    }

    #[test]
    fn an_empty_pi_message_sends_nothing() {
        assert!(send_message_pi("").is_empty());
    }

    #[test]
    fn a_multiline_pi_message_is_a_bracketed_paste_then_enter() {
        let writes = send_message_pi("first line\nsecond line");

        assert_eq!(writes.len(), 2, "the paste and the submit must be separate writes");
        // The body is one bracketed-paste chunk with the newline preserved
        // literally — never Claude's backslash-CR escape, which pi renders raw.
        assert!(writes[0].starts_with(b"\x1b[200~"), "must open bracketed paste");
        assert!(writes[0].ends_with(b"\x1b[201~"), "must close bracketed paste");
        assert!(writes[0].contains(&b'\n'), "the newline must survive verbatim");
        assert!(
            !writes[0].windows(2).any(|w| w == b"\\\r"),
            "pi must NOT get Claude's backslash-CR line break",
        );
        // The submit is its own write, so the paste is ingested first.
        assert_eq!(writes[1], b"\r");
        assert!(!writes[0].ends_with(b"\r"), "the paste chunk must not carry the submit");
    }
}
