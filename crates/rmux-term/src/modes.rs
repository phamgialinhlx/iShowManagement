//! DEC private-mode mirroring for the replay buffer.
//!
//! The scrollback is a ring that drops from the front. A fullscreen TUI sets its
//! modes once, at startup — alternate screen, mouse tracking, SGR encoding — and
//! then redraws for hours, so after a few minutes the bytes that turned those
//! modes on have been drained. A view that attaches late then replays a stream
//! that never enables them: measured as Claude's pane in the alternate buffer
//! with no mouse mode, where xterm turns every wheel step into ↑/↓ and the
//! wheel recalls old prompts instead of scrolling.
//!
//! [`ModeTracker`] is fed the bytes **as they leave the ring**, so it always
//! holds the mode state at the ring's first byte. [`ModeTracker::prefix`]
//! re-establishes exactly that state, and replaying the ring after it then
//! reproduces every change the program made since — the prefix plus the ring
//! has the same effect on these modes as the whole stream had.
//!
//! It parses `ESC [ ? Pm ; Pm … h|l` and nothing else, never allocates, and
//! survives a sequence split across any number of calls.

/// The modes mirrored. Anything else a program sets is left to the replay —
/// resetting a mode we do not understand is how a display gets wiped.
const ALT: [u16; 3] = [1049, 1047, 47];
const OTHERS: [u16; 10] = [
    1000, 1002, 1003, // mouse tracking
    1005, 1006, 1015, // mouse encoding
    1004, // focus reporting
    2004, // bracketed paste
    1,    // application cursor keys (DECCKM)
    25,   // cursor visibility
];

/// Parameters kept per sequence. Real programs set a handful at once; extras
/// are ignored rather than grown into.
const MAX_PARAMS: usize = 16;

/// Upper bound on [`ModeTracker::prefix`]'s length: every tracked mode emitted
/// separately, `ESC [ ? nnnn h` at most 8 bytes each.
pub(crate) const MAX_PREFIX: usize = (ALT.len() + OTHERS.len()) * 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    Csi,
    Private,
    /// Inside a private sequence we do not track (`ESC [ ? 1 $ p`, too many
    /// parameters): wait for its final byte and apply nothing.
    Ignore,
}

/// The last value a program gave one mode, and when — the order matters,
/// because terminals treat the mouse modes (and the encodings) as one setting,
/// so `1000h 1002h 1000l` means something different from `1000l 1002h`.
#[derive(Clone, Copy, Debug, Default)]
struct Slot {
    on: bool,
    /// 0 means the program never touched this mode.
    seq: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ModeTracker {
    state: State,
    params: [u16; MAX_PARAMS],
    count: usize,
    current: u32,
    has_digit: bool,
    alt: [Slot; ALT.len()],
    others: [Slot; OTHERS.len()],
    clock: u32,
}

impl ModeTracker {
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    fn step(&mut self, b: u8) {
        // ESC restarts from anywhere; CAN and SUB abort a sequence — both as a
        // terminal's own parser does, so we never disagree with it about where
        // a sequence ended.
        match b {
            0x1b => {
                self.state = State::Esc;
                return;
            }
            0x18 | 0x1a => {
                self.state = State::Ground;
                return;
            }
            _ => {}
        }
        match self.state {
            State::Ground => {}
            State::Esc => self.state = if b == b'[' { State::Csi } else { State::Ground },
            State::Csi => {
                if b == b'?' {
                    self.state = State::Private;
                    self.count = 0;
                    self.current = 0;
                    self.has_digit = false;
                } else {
                    // Any other CSI is not ours to parse.
                    self.state = State::Ground;
                }
            }
            State::Private => match b {
                b'0'..=b'9' => {
                    self.current = (self.current * 10 + u32::from(b - b'0')).min(100_000);
                    self.has_digit = true;
                }
                b';' => self.push_param(),
                b'h' | b'l' => {
                    self.push_param();
                    self.apply(b == b'h');
                    self.state = State::Ground;
                }
                // Intermediates (`$`, ` ` …) make it a different request.
                0x20..=0x2f => self.state = State::Ignore,
                // Some other final byte: a private CSI we do not track.
                0x40..=0x7e => self.state = State::Ground,
                // C0 controls inside a CSI are executed and do not end it.
                _ => {}
            },
            State::Ignore => {
                if (0x40..=0x7e).contains(&b) {
                    self.state = State::Ground;
                }
            }
        }
    }

    fn push_param(&mut self) {
        if self.count >= MAX_PARAMS {
            self.state = State::Ignore;
            return;
        }
        // An empty parameter is 0, which is no mode we track.
        self.params[self.count] = if self.has_digit { self.current.min(u32::from(u16::MAX)) as u16 } else { 0 };
        self.count += 1;
        self.current = 0;
        self.has_digit = false;
    }

    fn apply(&mut self, on: bool) {
        if self.state == State::Ignore {
            return;
        }
        for i in 0..self.count {
            let mode = self.params[i];
            let slot = if let Some(j) = ALT.iter().position(|&m| m == mode) {
                &mut self.alt[j]
            } else if let Some(j) = OTHERS.iter().position(|&m| m == mode) {
                &mut self.others[j]
            } else {
                continue;
            };
            self.clock = self.clock.saturating_add(1);
            *slot = Slot { on, seq: self.clock };
        }
    }

    /// Bytes that put a fresh terminal into this state: the alternate screen
    /// first (switching buffers must precede anything drawn), then every other
    /// touched mode in the order the program last changed it. A mode the
    /// program never touched is not mentioned. Each sequence is complete, so
    /// whatever follows the prefix starts from a clean parser.
    pub(crate) fn prefix(&self, out: &mut Vec<u8>) {
        fn emit(out: &mut Vec<u8>, slots: &[Slot], modes: &[u16]) {
            let mut order: [usize; OTHERS.len()] = [0; OTHERS.len()];
            let mut n = 0;
            for (i, s) in slots.iter().enumerate() {
                if s.seq != 0 {
                    order[n] = i;
                    n += 1;
                }
            }
            order[..n].sort_unstable_by_key(|&i| slots[i].seq);
            for &i in &order[..n] {
                out.extend_from_slice(b"\x1b[?");
                push_number(out, modes[i]);
                out.push(if slots[i].on { b'h' } else { b'l' });
            }
        }
        emit(out, &self.alt, &ALT);
        emit(out, &self.others, &OTHERS);
    }

    /// Whether `mode` is on, `None` if the program never touched it.
    #[cfg(test)]
    pub(crate) fn mode(&self, mode: u16) -> Option<bool> {
        let slot = ALT
            .iter()
            .position(|&m| m == mode)
            .map(|j| self.alt[j])
            .or_else(|| OTHERS.iter().position(|&m| m == mode).map(|j| self.others[j]))?;
        (slot.seq != 0).then_some(slot.on)
    }
}

fn push_number(out: &mut Vec<u8>, n: u16) {
    let mut digits = [0u8; 5];
    let mut len = 0;
    let mut v = n;
    loop {
        digits[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    out.extend(digits[..len].iter().rev());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(bytes: &[u8]) -> ModeTracker {
        let mut t = ModeTracker::default();
        t.feed(bytes);
        t
    }

    #[test]
    fn several_parameters_in_one_sequence() {
        let t = tracked(b"\x1b[?1000;1006h");
        assert_eq!(t.mode(1000), Some(true));
        assert_eq!(t.mode(1006), Some(true));
        assert_eq!(t.mode(1002), None);
    }

    #[test]
    fn a_sequence_split_byte_by_byte_is_tracked() {
        let mut t = ModeTracker::default();
        for b in b"\x1b[?1002h" {
            t.feed(&[*b]);
        }
        assert_eq!(t.mode(1002), Some(true));
    }

    #[test]
    fn other_csi_and_requests_are_ignored() {
        // SGR, a cursor move, DECRQM (`$p`) and a non-private `h` (insert mode).
        let t = tracked(b"\x1b[31m\x1b[1000h\x1b[?1000$p\x1b[4h\x1b[?1000r");
        assert_eq!(t.mode(1000), None);
        let mut out = Vec::new();
        t.prefix(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn escape_restarts_a_sequence() {
        let t = tracked(b"\x1b[?10\x1b[?1006h");
        assert_eq!(t.mode(1006), Some(true));
        assert_eq!(t.mode(10), None);
    }

    #[test]
    fn prefix_puts_the_alternate_screen_first_and_keeps_order() {
        let t = tracked(b"\x1b[?1000h\x1b[?1002h\x1b[?1000l\x1b[?1049h");
        let mut out = Vec::new();
        t.prefix(&mut out);
        assert_eq!(out, b"\x1b[?1049h\x1b[?1002h\x1b[?1000l");
        assert!(out.len() <= MAX_PREFIX);
    }
}
