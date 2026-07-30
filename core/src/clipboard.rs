//! OSC 52 clipboard extraction (see `plans/2026-07-23-bidirectional-clipboard.md`).
//!
//! Remote programs copy by emitting `ESC ] 52 ; Pc ; Pd (BEL | ESC \)` with a
//! base64 payload. The [`Scanner`] watches the PTY output stream for these
//! sequences — split at any byte boundary across read chunks — and yields the
//! decoded text; `ws.rs` writes it to the macOS clipboard. The stream itself is
//! forwarded unchanged (pass-through tee): xterm.js ignores OSC 52 without a
//! clipboard addon.
//!
//! The read direction (`Pd = ?`, remote queries our clipboard) is deliberately
//! ignored and never answered — same posture as Alacritty's `OnlyCopy`.

use base64::Engine;

/// Max decoded copy size; larger payloads are dropped.
const MAX_DECODED: usize = 1024 * 1024;
/// Abandon an unterminated sequence once this much payload has accumulated.
const MAX_SEQ: usize = 2 * 1024 * 1024;

const PREFIX: &[u8] = b"\x1b]52;";

enum State {
    /// Not inside a sequence; `usize` = how many PREFIX bytes matched so far.
    Ground(usize),
    /// Inside `ESC ] 52 ; …`, accumulating `Pc ; Pd` until BEL or ST.
    Body { buf: Vec<u8>, esc: bool },
}

/// Per-session OSC 52 detector. Feed each PTY output chunk; get decoded copies.
pub struct Scanner {
    state: State,
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            state: State::Ground(0),
        }
    }

    /// Scan one output chunk. Returns the decoded text of every OSC 52 copy
    /// sequence that completed within (or across) chunks.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut copies = Vec::new();
        for &b in chunk {
            match &mut self.state {
                State::Ground(matched) => {
                    if b == PREFIX[*matched] {
                        *matched += 1;
                        if *matched == PREFIX.len() {
                            self.state = State::Body {
                                buf: Vec::new(),
                                esc: false,
                            };
                        }
                    } else {
                        // Restart the match; the current byte may itself be ESC.
                        *matched = if b == PREFIX[0] { 1 } else { 0 };
                    }
                }
                State::Body { buf, esc } => {
                    if *esc {
                        // `ESC \` (ST) ends the sequence; any other escaped
                        // byte means this wasn't a clean OSC — drop it.
                        if b == b'\\' {
                            if let Some(text) = decode(buf) {
                                copies.push(text);
                            }
                        }
                        self.state = State::Ground(0);
                    } else if b == 0x07 {
                        if let Some(text) = decode(buf) {
                            copies.push(text);
                        }
                        self.state = State::Ground(0);
                    } else if b == 0x1b {
                        *esc = true;
                    } else if buf.len() >= MAX_SEQ {
                        self.state = State::Ground(0);
                    } else {
                        buf.push(b);
                    }
                }
            }
        }
        copies
    }
}

/// Decode the `Pc ; Pd` body of an OSC 52 sequence to the copied text.
/// Queries (`?`), bad base64, non-UTF-8, and oversized payloads yield None.
fn decode(body: &[u8]) -> Option<String> {
    let sep = body.iter().position(|&b| b == b';')?;
    let payload = &body[sep + 1..];
    if payload == b"?" {
        return None; // clipboard query — never answered (OnlyCopy)
    }
    if payload.len() > MAX_DECODED / 3 * 4 + 4 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_DECODED {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Write `text` to the macOS clipboard via `pbcopy`. Blocking — call from
/// `spawn_blocking`. Failures are logged, never fatal to the session.
#[cfg(target_os = "macos")]
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let run = || -> std::io::Result<()> {
        let mut child = Command::new("pbcopy")
            // Without a UTF-8 LC_CTYPE, pbcopy mangles non-ASCII input.
            .env("LC_CTYPE", "UTF-8")
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(text.as_bytes())?;
        child.wait()?;
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("clipboard: pbcopy failed: {e}");
    }
}

/// Write `text` to the Windows clipboard via `clip.exe`. Blocking — call from
/// `spawn_blocking`. Failures are logged, never fatal to the session.
///
/// `clip.exe` decodes stdin as the console codepage unless given a BOM, so the
/// text is prefixed with a UTF-16LE BOM and encoded to UTF-16LE; otherwise
/// non-ASCII (accents, CJK, box-drawing) arrives mangled.
#[cfg(windows)]
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let run = || -> std::io::Result<()> {
        let mut bytes = vec![0xff, 0xfe]; // UTF-16LE BOM
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let mut child = Command::new("clip.exe").stdin(Stdio::piped()).spawn()?;
        child.stdin.take().expect("piped stdin").write_all(&bytes)?;
        child.wait()?;
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("clipboard: clip.exe failed: {e}");
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn copy_to_clipboard(_text: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc(pc: &str, payload: &str, term: &str) -> Vec<u8> {
        format!("\x1b]52;{pc};{payload}{term}").into_bytes()
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn copy_with_bel_terminator() {
        let mut s = Scanner::new();
        assert_eq!(s.feed(&osc("c", &b64("hello"), "\x07")), vec!["hello"]);
    }

    #[test]
    fn copy_with_st_terminator() {
        let mut s = Scanner::new();
        assert_eq!(s.feed(&osc("c", &b64("hi ☂"), "\x1b\\")), vec!["hi ☂"]);
    }

    #[test]
    fn sequence_split_across_chunks() {
        let mut s = Scanner::new();
        let seq = osc("c", &b64("split"), "\x07");
        for cut in 1..seq.len() {
            let mut sc = Scanner::new();
            let mut got = sc.feed(&seq[..cut]);
            got.extend(sc.feed(&seq[cut..]));
            assert_eq!(got, vec!["split"], "cut at {cut}");
        }
        // And in three pieces through the prefix.
        let mut got = s.feed(b"\x1b]");
        got.extend(s.feed(b"52;c;"));
        got.extend(s.feed(format!("{}\x07", b64("abc")).as_bytes()));
        assert_eq!(got, vec!["abc"]);
    }

    #[test]
    fn interleaved_output_and_multiple_copies() {
        let mut s = Scanner::new();
        let mut data = b"ls -la\r\n".to_vec();
        data.extend(osc("c", &b64("one"), "\x07"));
        data.extend(b"more output \x1b[31mred\x1b[0m ");
        data.extend(osc("p", &b64("two"), "\x1b\\"));
        assert_eq!(s.feed(&data), vec!["one", "two"]);
    }

    #[test]
    fn query_is_ignored() {
        let mut s = Scanner::new();
        assert!(s.feed(&osc("c", "?", "\x07")).is_empty());
    }

    #[test]
    fn bad_base64_and_empty_are_ignored() {
        let mut s = Scanner::new();
        assert!(s.feed(&osc("c", "!!notb64!!", "\x07")).is_empty());
        assert!(s.feed(&osc("c", "", "\x07")).is_empty());
        // Scanner recovers: a valid copy afterwards still works.
        assert_eq!(s.feed(&osc("c", &b64("ok"), "\x07")), vec!["ok"]);
    }

    #[test]
    fn missing_semicolon_body_is_ignored() {
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]52;nosemicolon\x07").is_empty());
    }

    #[test]
    fn oversized_payload_is_dropped() {
        let mut s = Scanner::new();
        let big = "a".repeat(MAX_DECODED + 1);
        assert!(s.feed(&osc("c", &b64(&big), "\x07")).is_empty());
    }

    #[test]
    fn unterminated_sequence_is_abandoned() {
        let mut s = Scanner::new();
        let mut data = b"\x1b]52;c;".to_vec();
        data.extend(std::iter::repeat(b'A').take(MAX_SEQ + 10));
        assert!(s.feed(&data).is_empty());
        // Ground state restored — later copies still detected.
        assert_eq!(s.feed(&osc("c", &b64("after"), "\x07")), vec!["after"]);
    }

    #[test]
    fn other_osc_sequences_pass_unharmed() {
        let mut s = Scanner::new();
        // OSC 0 (title) then a real copy.
        let mut data = b"\x1b]0;my title\x07".to_vec();
        data.extend(osc("c", &b64("x"), "\x07"));
        assert_eq!(s.feed(&data), vec!["x"]);
    }
}
