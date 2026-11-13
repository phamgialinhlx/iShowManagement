//! The detach key for a command-line attach: tmux's `C-b d`.
//!
//! **Opt-in per attach, never a default of `attach`.** The desktop app drives
//! `rmux-agent attach --session …` too, and it uses `Ctrl-B` as *its own*
//! prefix — a double `Ctrl-B` there sends a literal `\x02` to the shell. If the
//! attach client always watched for `C-b d`, a `\x02` the app forwarded followed
//! by a `d` the operator typed would silently drop the app's terminal. So the
//! filter exists only when the CLI asks for it with `--detach-key`, and the app
//! never does.
//!
//! The state machine is pure bytes-in/bytes-out so it is tested without a pty:
//!
//! - prefix, then the detach key → detach (neither byte reaches the shell)
//! - prefix, then the prefix again → one literal prefix byte reaches the shell
//! - prefix, then anything else → both bytes reach the shell, unchanged
//!
//! A prefix that ends a read is held until the next read decides it, because a
//! key pair can arrive split across two `read`s.

/// The default for a command-line attach, exactly as tmux spells it.
pub const DEFAULT: &str = "C-b d";

/// `RMUX_DETACH_KEY` overrides [`DEFAULT`] for the CLI; `none` turns it off.
pub const ENV: &str = "RMUX_DETACH_KEY";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetachKey {
    /// The prefix byte (`C-b` is `0x02`).
    pub prefix: u8,
    /// The byte that, after the prefix, detaches.
    pub key: u8,
}

impl DetachKey {
    /// Parse `"<prefix> <key>"`: `C-b d`, `^B d`, `ctrl+b d`, `ctrl-a x`.
    ///
    /// `Ok(None)` for `none`/`off`, which a user picks when the prefix collides
    /// with something they need (`C-b` is emacs's backward-char).
    pub fn parse(spec: &str) -> Result<Option<Self>, String> {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("none") || spec.eq_ignore_ascii_case("off") || spec.is_empty() {
            return Ok(None);
        }
        let mut words = spec.split_whitespace();
        let (Some(prefix), Some(key), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!(
                "a detach key is a prefix and a key, like \"C-b d\" (or \"none\"), not {spec:?}"
            ));
        };
        let prefix = parse_byte(prefix)
            .filter(|b| *b < 0x20)
            .ok_or_else(|| format!("the detach prefix must be a control key like C-b, not {prefix:?}"))?;
        let key = parse_byte(key).ok_or_else(|| format!("unknown detach key {key:?}"))?;
        Ok(Some(Self { prefix, key }))
    }

    /// The spelling `--detach-key` takes, so the CLI can hand a parsed key on to
    /// a remote agent without re-deriving the text.
    pub fn spec(&self) -> String {
        format!("{} {}", spell(self.prefix), spell(self.key))
    }
}

/// One key: `C-x`/`^x`/`ctrl+x`/`ctrl-x` is a control byte, a single printable
/// character is itself.
fn parse_byte(word: &str) -> Option<u8> {
    let lower = word.to_ascii_lowercase();
    let ctrl = lower
        .strip_prefix("c-")
        .or_else(|| lower.strip_prefix("ctrl+"))
        .or_else(|| lower.strip_prefix("ctrl-"))
        .or_else(|| lower.strip_prefix('^'));
    if let Some(rest) = ctrl {
        let mut chars = rest.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { return None };
        return match c {
            'a'..='z' => Some(c as u8 - b'a' + 1),
            '@' => Some(0),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            _ => None,
        };
    }
    let mut chars = word.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Some(c as u8),
        _ => None,
    }
}

fn spell(b: u8) -> String {
    match b {
        1..=26 => format!("C-{}", (b'a' + b - 1) as char),
        0 => "C-@".into(),
        0x1b => "C-[".into(),
        0x1c => "C-\\".into(),
        0x1d => "C-]".into(),
        _ => (b as char).to_string(),
    }
}

/// The filter itself: carries one bit of state between reads.
#[derive(Debug)]
pub struct Filter {
    key: DetachKey,
    pending: bool,
}

/// What one chunk of input turned into.
#[derive(Debug, PartialEq, Eq)]
pub struct Filtered {
    /// The bytes to send on to the session.
    pub forward: Vec<u8>,
    /// The detach key was pressed. Anything typed after it in the same chunk is
    /// dropped — it was typed at a terminal that is about to stop being attached.
    pub detach: bool,
}

impl Filter {
    pub fn new(key: DetachKey) -> Self {
        Self { key, pending: false }
    }

    pub fn feed(&mut self, input: &[u8]) -> Filtered {
        let mut forward = Vec::with_capacity(input.len() + 1);
        for &b in input {
            if self.pending {
                self.pending = false;
                if b == self.key.key {
                    return Filtered { forward, detach: true };
                }
                // `C-b C-b` is how a literal prefix gets through, as in tmux.
                forward.push(self.key.prefix);
                if b != self.key.prefix {
                    forward.push(b);
                }
            } else if b == self.key.prefix {
                self.pending = true;
            } else {
                forward.push(b);
            }
        }
        Filtered { forward, detach: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default() -> DetachKey {
        DetachKey::parse(DEFAULT).unwrap().unwrap()
    }

    #[test]
    fn parses_tmux_spellings() {
        assert_eq!(default(), DetachKey { prefix: 0x02, key: b'd' });
        for spec in ["^B d", "ctrl+b d", "Ctrl-B d", "c-b d"] {
            assert_eq!(DetachKey::parse(spec).unwrap(), Some(default()), "{spec}");
        }
        assert_eq!(DetachKey::parse("C-a x").unwrap(), Some(DetachKey { prefix: 1, key: b'x' }));
        assert_eq!(DetachKey::parse("C-a C-d").unwrap(), Some(DetachKey { prefix: 1, key: 4 }));
        assert_eq!(DetachKey::parse("none").unwrap(), None);
        // A printable prefix would eat that letter from every word typed.
        assert!(DetachKey::parse("b d").is_err());
        assert!(DetachKey::parse("C-b").is_err());
        assert!(DetachKey::parse("C-b d e").is_err());
        assert_eq!(default().spec(), "C-b d");
        assert_eq!(DetachKey::parse(&default().spec()).unwrap(), Some(default()));
    }

    #[test]
    fn prefix_then_key_detaches_and_sends_neither() {
        let mut f = Filter::new(default());
        let out = f.feed(b"echo hi\r\x02d");
        assert_eq!(out, Filtered { forward: b"echo hi\r".to_vec(), detach: true });
    }

    #[test]
    fn double_prefix_sends_one_literal_prefix() {
        let mut f = Filter::new(default());
        let out = f.feed(b"\x02\x02d");
        // The first two bytes are one literal C-b; the `d` after it is just a d.
        assert_eq!(out, Filtered { forward: b"\x02d".to_vec(), detach: false });
    }

    #[test]
    fn prefix_then_other_key_passes_both_through() {
        let mut f = Filter::new(default());
        assert_eq!(f.feed(b"\x02c").forward, b"\x02c".to_vec());
        assert_eq!(f.feed(b"plain").forward, b"plain".to_vec());
    }

    #[test]
    fn a_pair_split_across_reads_still_detaches() {
        let mut f = Filter::new(default());
        let first = f.feed(b"ls\x02");
        assert_eq!(first, Filtered { forward: b"ls".to_vec(), detach: false });
        let second = f.feed(b"d");
        assert!(second.detach);
        assert!(second.forward.is_empty());

        // And a split literal prefix arrives once, late but intact.
        let mut f = Filter::new(default());
        assert!(f.feed(b"\x02").forward.is_empty());
        assert_eq!(f.feed(b"\x02").forward, b"\x02".to_vec());
    }
}
