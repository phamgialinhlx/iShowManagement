//! Fallback chains — moving one Claude session to the next account when the
//! current one hits a usage or rate limit.
//!
//! Everything here is pure: the limit **matcher** (what a limit looks like in a
//! transcript record or on the screen), the **chain state machine** (which
//! member is next, which are exhausted, when one has recovered), the **resume
//! line** a switched session is relaunched with, and the shape of the switch log.
//! The agent daemon does the I/O — reading the transcript tail, respawning the
//! process, writing `~/.rmux/fallback.json` — and the desktop app reads the log.
//! One definition, so the two cannot disagree about what a switch was.
//!
//! ## Detection is data
//!
//! Claude Code renames and rewords its limit messages between releases, so the
//! phrases are a table ([`PATTERNS`]) rather than code. Updating detection for a
//! new wording is adding a row — and a test row beside it.
//!
//! ## What is never a trigger
//!
//! - **Overloaded / 529** is the provider being busy, not the account being
//!   spent. Switching accounts would not help and would burn the next member's
//!   window for nothing, so it is classed [`LimitClass::Overloaded`] and only
//!   acted on when a chain opts in.
//! - **Someone typing the words.** A user prompt that says "rate limit" or even
//!   "usage limit reached" is the operator talking. Transcript matching reads
//!   only records Claude marks as API errors; screen matching skips composer and
//!   echoed-prompt lines and requires the exact error phrasing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What kind of limit a message describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitClass {
    /// The account's usage window is spent (5-hour, weekly, Opus…). Switching
    /// to another account is exactly the remedy.
    Usage,
    /// A 429 / `rate_limit_error`. Also a switch trigger: the account is being
    /// throttled and another account is not.
    Rate,
    /// 529 / `overloaded_error`. The provider is busy for everyone; retry, do
    /// not switch — unless the chain explicitly opts in.
    Overloaded,
}

impl LimitClass {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitClass::Usage => "usage limit",
            LimitClass::Rate => "rate limit",
            LimitClass::Overloaded => "overloaded",
        }
    }
}

/// One detection rule: every substring in `all` must appear in one line
/// (lower-cased, curly apostrophes folded to `'`).
#[derive(Clone, Copy, Debug)]
pub struct Pattern {
    /// Stable name, recorded in the switch log so a false positive can be traced
    /// to the row that produced it.
    pub id: &'static str,
    pub all: &'static [&'static str],
    pub class: LimitClass,
}

/// The detection table. Order matters only in that the first match wins, so the
/// specific usage-limit rows come before the generic ones.
///
/// Sources: Claude Code's own error rendering —
/// `Claude AI usage limit reached|<unix reset>` (the transcript text of 1.x),
/// `Claude usage limit reached. Your limit will reset at 3pm (Europe/Paris).`,
/// `5-hour limit reached ∙ resets 3pm`, `You've hit your limit · resets 3pm`,
/// `Weekly limit reached`, and the API passthroughs
/// `API Error: 429 {"type":"error","error":{"type":"rate_limit_error",…}}` and
/// `API Error: 529 {…"overloaded_error"…}`.
pub const PATTERNS: &[Pattern] = &[
    Pattern { id: "usage-limit-reached", all: &["usage limit reached"], class: LimitClass::Usage },
    Pattern { id: "hit-your-limit", all: &["hit your limit"], class: LimitClass::Usage },
    Pattern { id: "reached-usage-limit", all: &["reached your usage limit"], class: LimitClass::Usage },
    Pattern { id: "five-hour-limit", all: &["5-hour limit reached"], class: LimitClass::Usage },
    Pattern { id: "weekly-limit", all: &["weekly limit reached"], class: LimitClass::Usage },
    Pattern { id: "limit-will-reset", all: &["limit will reset at"], class: LimitClass::Usage },
    Pattern { id: "limit-reached-resets", all: &["limit reached", "resets"], class: LimitClass::Usage },
    Pattern { id: "rate-limit-error", all: &["rate_limit_error"], class: LimitClass::Rate },
    Pattern { id: "api-error-429", all: &["api error", "429"], class: LimitClass::Rate },
    Pattern { id: "overloaded-error", all: &["overloaded_error"], class: LimitClass::Overloaded },
    Pattern { id: "api-error-529", all: &["api error", "529"], class: LimitClass::Overloaded },
    Pattern { id: "api-error-overloaded", all: &["api error", "overloaded"], class: LimitClass::Overloaded },
];

/// A limit, found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitHit {
    pub class: LimitClass,
    /// [`Pattern::id`] of the row that matched.
    pub pattern: &'static str,
    /// When the limit lifts (unix seconds), if the message said.
    pub resets_at: Option<u64>,
    /// The line that matched, trimmed, for the log. Never a secret: it is what
    /// Claude printed about the account, not the account.
    pub line: String,
}

fn normalise(line: &str) -> String {
    line.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'")
}

/// Match one line against the table. Pure; no context about where it came from.
fn match_line(line: &str, now: u64, utc_offset: i64) -> Option<LimitHit> {
    let low = normalise(line);
    let pattern = PATTERNS.iter().find(|p| p.all.iter().all(|s| low.contains(s)))?;
    Some(LimitHit {
        class: pattern.class,
        pattern: pattern.id,
        resets_at: parse_reset(&low, now, utc_offset),
        line: line.trim().chars().take(240).collect(),
    })
}

/// Whether a rendered line is the operator's own text rather than Claude's.
///
/// The composer (`> …`, `❯ …`, boxed as `│ > … │`) and echoed prompts are what
/// someone typed; matching them would switch accounts because a person *wrote*
/// "usage limit reached".
fn is_user_text(line: &str) -> bool {
    let t = line.trim_start_matches(|c: char| c.is_whitespace() || c == '│' || c == '|');
    t.starts_with('>') || t.starts_with('❯') || t.starts_with("Human:")
}

/// Look for a limit in rendered screen text (escape sequences already stripped).
///
/// Only the **bottom** of the screen is read — `last_lines` of it. A limit that
/// stops a session sits at the end, beside the composer; the same words higher
/// up are history or prose (Claude explaining a rate limiter it is writing), and
/// switching accounts over prose is the false positive that matters.
pub fn match_screen(text: &str, last_lines: usize, now: u64, utc_offset: i64) -> Option<LimitHit> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(last_lines);
    lines[start..]
        .iter()
        .rev()
        .filter(|l| !is_user_text(l))
        .find_map(|l| match_line(l, now, utc_offset))
}

/// A limit found in one transcript record, with the record's own timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptHit {
    pub hit: LimitHit,
    /// The record's `timestamp`, unix seconds. What makes one limit message
    /// trigger one switch: only records newer than the member's binding count.
    pub at: Option<u64>,
}

/// Look for a limit in one line of Claude's `.jsonl` transcript.
///
/// **Only records Claude marks as errors count** — `isApiErrorMessage: true`, an
/// `error` field, or `level: "error"`. An ordinary assistant or user record that
/// merely *contains* the words is conversation, never a trigger. Fields are
/// picked one at a time: binding to Claude's schema turns every release into
/// "nothing ever matches".
pub fn match_transcript_line(line: &str, now: u64, utc_offset: i64) -> Option<TranscriptHit> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    if !matches!(kind, "assistant" | "system") {
        return None;
    }
    let error_field = v.get("error").filter(|e| !e.is_null());
    let flagged = v.get("isApiErrorMessage").and_then(|f| f.as_bool()).unwrap_or(false)
        || error_field.is_some()
        || v.get("level").and_then(|l| l.as_str()) == Some("error");
    if !flagged {
        return None;
    }

    let mut texts: Vec<String> = Vec::new();
    if let Some(content) = v.pointer("/message/content") {
        collect_text(content, &mut texts);
    }
    if let Some(content) = v.get("content") {
        collect_text(content, &mut texts);
    }
    if let Some(e) = error_field {
        match e.as_str() {
            Some(s) => texts.push(s.to_owned()),
            None => texts.push(e.to_string()),
        }
    }

    let at = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_rfc3339);
    let hit = texts
        .iter()
        .flat_map(|t| t.lines())
        .find_map(|l| match_line(l, now, utc_offset))
        .or_else(|| {
            // A bare `"error":"rate_limit"` with text we do not recognise.
            let e = error_field?.as_str()?;
            e.contains("rate_limit").then(|| LimitHit {
                class: LimitClass::Rate,
                pattern: "error-field-rate-limit",
                resets_at: None,
                line: e.to_owned(),
            })
        })?;
    Some(TranscriptHit { hit, at })
}

fn collect_text(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    out.push(t.to_owned());
                } else if let Some(s) = item.as_str() {
                    out.push(s.to_owned());
                }
            }
        }
        _ => {}
    }
}

/// The newest limit in a transcript tail that is newer than `since`.
///
/// A record without a timestamp is ignored: without one there is no telling a
/// fresh limit from the one that caused the previous switch, and acting on that
/// is the double switch the debounce exists to prevent.
pub fn newest_transcript_limit(tail: &str, since: u64, now: u64, utc_offset: i64) -> Option<TranscriptHit> {
    tail.lines()
        .rev()
        .filter_map(|l| match_transcript_line(l, now, utc_offset))
        .find(|h| h.at.is_some_and(|at| at > since))
}

// ── Reset times ──────────────────────────────────────────────────────────────

/// When a limit message says it resets, as unix seconds.
///
/// Understands `…|1749924000` (Claude's machine form), `resets 3pm`,
/// `resets at 3:30 pm`, `reset at 15:00`. A clock time is the next occurrence
/// after `now` in the host's local time (`utc_offset` seconds east of UTC); a
/// named zone in parentheses is not resolved — this is a display hint for
/// "switch back", never something acted on automatically.
pub fn parse_reset(low: &str, now: u64, utc_offset: i64) -> Option<u64> {
    if let Some((_, tail)) = low.split_once('|') {
        let digits: String = tail.trim().chars().take_while(char::is_ascii_digit).collect();
        if digits.len() >= 9
            && let Ok(ts) = digits.parse::<u64>()
        {
            return Some(ts);
        }
    }
    let at = ["resets at ", "reset at ", "resets "]
        .iter()
        .find_map(|k| low.find(k).map(|i| &low[i + k.len()..]))?;
    let (hour, minute) = parse_clock(at)?;
    let local_now = now as i64 + utc_offset;
    let day = local_now.div_euclid(86_400) * 86_400;
    let mut candidate = day + i64::from(hour) * 3600 + i64::from(minute) * 60;
    if candidate <= local_now {
        candidate += 86_400;
    }
    u64::try_from(candidate - utc_offset).ok()
}

/// `3pm`, `3:30 pm`, `15:00`, `3 am` → (hour, minute) in 24h.
fn parse_clock(s: &str) -> Option<(u32, u32)> {
    let s = s.trim_start();
    let digits_end = s.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(s.len());
    let (clock, rest) = s.split_at(digits_end);
    if clock.is_empty() {
        return None;
    }
    let (h, m) = match clock.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (clock.parse::<u32>().ok()?, 0),
    };
    let rest = rest.trim_start();
    let hour = if rest.starts_with("pm") {
        if h == 12 { 12 } else { h + 12 }
    } else if rest.starts_with("am") {
        if h == 12 { 0 } else { h }
    } else if clock.contains(':') {
        h
    } else {
        // A bare number with no am/pm and no colon is not a clock.
        return None;
    };
    (hour < 24 && m < 60).then_some((hour, m))
}

/// `2026-11-12T14:02:03.123Z` (or with a `+hh:mm` offset) → unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
        Some(i) => time.split_at(i),
        None => (time, ""),
    };
    let mut c = clock.split(':');
    let hh: i64 = c.next()?.parse().ok()?;
    let mm: i64 = c.next()?.parse().ok()?;
    let ss: i64 = c.next().unwrap_or("0").split('.').next()?.parse().ok()?;
    let off: i64 = match offset.chars().next() {
        Some('+') | Some('-') => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let (oh, om) = offset[1..].split_once(':').unwrap_or((&offset[1..], "0"));
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
        _ => 0,
    };
    let days = days_from_civil(year, month, day);
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss - off).ok()
}

/// Howard Hinnant's days-from-civil: days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ── The chain ────────────────────────────────────────────────────────────────

/// How long a member counts as exhausted when its limit message gave no reset
/// time: Claude's shortest window.
pub const DEFAULT_WINDOW_SECS: u64 = 5 * 3600;

/// After a switch, how long a new detection is ignored. The relaunched process
/// re-renders the conversation — including the limit message that caused the
/// switch — and one limit must cause one switch.
pub const SWITCH_COOLDOWN_SECS: u64 = 90;

/// Why a member is out of rotation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exhaustion {
    /// When the limit was seen (unix seconds).
    pub at: u64,
    /// When it lifts, if the message said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

impl Exhaustion {
    /// When this member may be used again.
    pub fn until(&self) -> u64 {
        self.resets_at.unwrap_or(self.at + DEFAULT_WINDOW_SECS)
    }
}

/// A session's place in its fallback chain. Names only — every member is a
/// credential set the daemon already holds, keyed by profile id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainState {
    /// The chain's id in the app's store.
    pub id: String,
    /// Member set names (profile ids), in order.
    pub members: Vec<String>,
    /// The member the session runs on now.
    pub active: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exhausted: BTreeMap<String, Exhaustion>,
    /// Treat 529 / overloaded as a switch trigger. Off by default — see the
    /// module docs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub switch_on_overloaded: bool,
}

/// What to do about a limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Move to this member.
    SwitchTo(String),
    /// Every later member is exhausted (or there are none): stay, and say so.
    EndOfChain,
    /// This class of limit is not a trigger for this chain.
    Ignore,
}

impl ChainState {
    pub fn is_exhausted(&self, member: &str, now: u64) -> bool {
        self.exhausted.get(member).is_some_and(|e| now < e.until())
    }

    /// Whether `class` should move this chain at all.
    pub fn triggers(&self, class: LimitClass) -> bool {
        match class {
            LimitClass::Usage | LimitClass::Rate => true,
            LimitClass::Overloaded => self.switch_on_overloaded,
        }
    }

    /// The active member hit a limit: mark it, and pick the next one.
    ///
    /// **Forward only, never round.** The search starts after the active member
    /// and stops at the end of the chain. Members before it are where the chain
    /// has already been; going back to one is the operator's choice
    /// ([`ChainState::recovered`] offers it), never the daemon's — a member
    /// whose window is "probably" over is exactly the guess that cycles a
    /// session between two spent accounts.
    pub fn on_limit(&mut self, class: LimitClass, now: u64, resets_at: Option<u64>) -> Decision {
        if !self.triggers(class) {
            return Decision::Ignore;
        }
        self.exhausted.insert(self.active.clone(), Exhaustion { at: now, resets_at });
        let start = self.members.iter().position(|m| *m == self.active).map_or(0, |i| i + 1);
        match self.members[start.min(self.members.len())..]
            .iter()
            .find(|m| !self.is_exhausted(m, now))
            .cloned()
        {
            Some(next) => {
                self.active = next.clone();
                Decision::SwitchTo(next)
            }
            None => Decision::EndOfChain,
        }
    }

    /// Earlier members whose window has passed — what "switch back" may offer.
    pub fn recovered(&self, now: u64) -> Vec<String> {
        let upto = self.members.iter().position(|m| *m == self.active).unwrap_or(0);
        self.members[..upto]
            .iter()
            .filter(|m| self.exhausted.get(*m).is_some_and(|e| now >= e.until()))
            .cloned()
            .collect()
    }

    /// The operator chose to move back to `member` (or anywhere in the chain).
    pub fn switch_back(&mut self, member: &str) -> bool {
        if !self.members.iter().any(|m| m == member) {
            return false;
        }
        self.exhausted.remove(member);
        self.active = member.to_owned();
        true
    }
}

/// Whether a detection should act, given the debounce.
///
/// `seen_at` is when the limit was recorded (a transcript timestamp, or now for
/// the screen); `bound_since` is when the active member was bound; a record from
/// before that belongs to the previous member's switch.
pub fn should_act(seen_at: u64, bound_since: u64, last_switch: Option<u64>, now: u64) -> bool {
    seen_at > bound_since && last_switch.is_none_or(|t| now >= t + SWITCH_COOLDOWN_SECS)
}

// ── Relaunch ─────────────────────────────────────────────────────────────────

/// Launch flags that are a property of the *session* and survive a switch.
/// Everything else after `claude` — a positional first prompt above all — is
/// dropped: resuming with the original prompt would send it a second time.
const KEPT_SWITCHES: &[&str] = &["--dangerously-skip-permissions", "--verbose"];
const KEPT_WITH_VALUE: &[&str] = &["--model", "--permission-mode", "--add-dir", "--settings"];

/// The line a switched session is relaunched with: the same rendering
/// assignments and session flags, plus `--resume <conversation>` (or
/// `--continue` when the conversation is unknown, which resumes the newest one
/// in the session's folder).
///
/// `None` when the line is not a recognisable Claude launch — the switch is then
/// refused rather than guessed at.
pub fn resume_line(line: &str, conversation: Option<&str>) -> Option<String> {
    let words = shell_words(line)?;
    let at = words.iter().position(|w| w == "claude" || w.ends_with("/claude"))?;
    if words[..at].iter().any(|w| !is_assignment(w)) {
        return None;
    }
    let mut out: Vec<String> = words[..at].iter().map(|w| quote_assignment(w)).collect();
    out.push(rmux_transport::shell_quote(&words[at]));
    match conversation {
        Some(id) => {
            out.push("--resume".to_owned());
            out.push(rmux_transport::shell_quote(id));
        }
        None => out.push("--continue".to_owned()),
    }
    let mut rest = words[at + 1..].iter();
    while let Some(w) = rest.next() {
        if KEPT_SWITCHES.contains(&w.as_str()) {
            out.push(w.clone());
        } else if let Some(flag) = KEPT_WITH_VALUE.iter().find(|f| w == *f) {
            if let Some(v) = rest.next() {
                out.push((*flag).to_owned());
                out.push(rmux_transport::shell_quote(v));
            }
        } else if matches!(w.as_str(), "--resume" | "-r" | "--session-id") {
            // The old conversation id: replaced above.
            rest.next();
        }
    }
    Some(out.join(" "))
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(k, _)| {
        let mut c = k.chars();
        c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

fn quote_assignment(word: &str) -> String {
    let (k, v) = word.split_once('=').unwrap_or((word, ""));
    format!("{k}={}", if v.is_empty() { String::new() } else { rmux_transport::shell_quote(v) })
}

/// A small POSIX word splitter: single quotes, double quotes (with `\"` and
/// `\\`), backslash escapes. `None` for anything it cannot be sure of — an
/// unterminated quote, an expansion, an operator.
fn shell_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            ch @ ('"' | '\\' | '$' | '`') => cur.push(ch),
                            ch => {
                                cur.push('\\');
                                cur.push(ch);
                            }
                        },
                        '$' | '`' => return None,
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                cur.push(chars.next()?);
            }
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '$' | '`' | '\n' => return None,
            ch => {
                in_word = true;
                cur.push(ch);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// What is typed into the relaunched session so the work carries on.
pub const CONTINUE_MESSAGE: &str =
    "Continue where you left off — this session was moved to another account after a usage limit.";

// ── The switch log ───────────────────────────────────────────────────────────

/// One entry in `~/.rmux/fallback.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackRecord {
    /// The daemon session name.
    pub session: String,
    pub chain: String,
    /// Set name (profile id) the session was on.
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_label: Option<String>,
    /// Where it went; `None` when the chain had nowhere left to go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_label: Option<String>,
    /// `usage limit`, `rate limit`, `overloaded`.
    pub reason: String,
    /// The pattern row that matched, and the line itself.
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub line: String,
    /// `transcript` or `screen`.
    #[serde(default)]
    pub source: String,
    /// `switched`, `end_of_chain`, or `failed: …`.
    pub outcome: String,
    /// Unix seconds.
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<u64>,
}

/// The whole file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackLog {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub switches: Vec<FallbackRecord>,
}

/// The file's schema version.
pub const LOG_VERSION: u32 = 1;
/// Entries kept, newest last.
pub const LOG_MAX: usize = 200;
/// Entries older than this are dropped on every write.
pub const LOG_MAX_AGE_SECS: u64 = 14 * 86_400;

/// Append and prune — bounded in count **and** age, so the file is bounded in
/// both directions (the `activity.ts` lesson).
pub fn log_append(mut log: FallbackLog, record: FallbackRecord, now: u64) -> FallbackLog {
    log.version = LOG_VERSION;
    log.switches.push(record);
    log.switches.retain(|r| r.at + LOG_MAX_AGE_SECS >= now);
    let excess = log.switches.len().saturating_sub(LOG_MAX);
    log.switches.drain(..excess);
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_794_000_000; // 2026-11-06T…Z

    fn hit(line: &str) -> Option<LimitHit> {
        match_screen(line, 20, NOW, 0)
    }

    // Captured shapes of what Claude Code prints and records. Each is the form
    // the corresponding `PATTERNS` row was written against.
    const SCREEN_SAMPLES: &[(&str, LimitClass)] = &[
        ("  ⎿  Claude AI usage limit reached|1794003600", LimitClass::Usage),
        ("Claude usage limit reached. Your limit will reset at 3pm (Europe/Paris).", LimitClass::Usage),
        ("5-hour limit reached ∙ resets 3pm", LimitClass::Usage),
        ("You\u{2019}ve hit your limit · resets 11pm (America/New_York)", LimitClass::Usage),
        ("Weekly limit reached ∙ resets Mon 9am", LimitClass::Usage),
        ("Opus limit reached ∙ resets 7pm", LimitClass::Usage),
        (
            "  ⎿  API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"This request would exceed your account's rate limit.\"}}",
            LimitClass::Rate,
        ),
        (
            "  ⎿  API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}",
            LimitClass::Overloaded,
        ),
        ("  ⎿  API Error: Repeated 529 Overloaded errors", LimitClass::Overloaded),
    ];

    #[test]
    fn every_captured_limit_message_matches_its_class() {
        for (line, class) in SCREEN_SAMPLES {
            let h = hit(line).unwrap_or_else(|| panic!("no match: {line}"));
            assert_eq!(h.class, *class, "{line}");
        }
    }

    #[test]
    fn the_operator_typing_the_words_is_not_a_limit() {
        for line in [
            "> why do we hit a rate limit here?",
            "> usage limit reached — how should the UI say it?",
            "│ > fix the 429 handling in the API error path       │",
            "❯ the weekly limit reached banner is ugly",
            // Prose about rate limiting, the case this feature itself produces.
            "⏺ I added a rate limit to the login endpoint.",
            "The server now returns 429 when the limit is hit.",
            "Usage: rmux limit [--reset]",
        ] {
            assert!(hit(line).is_none(), "false positive: {line}");
        }
    }

    #[test]
    fn only_the_bottom_of_the_screen_is_read() {
        let mut screen = String::from("5-hour limit reached ∙ resets 3pm\n");
        for i in 0..30 {
            screen.push_str(&format!("line {i}\n"));
        }
        assert!(match_screen(&screen, 20, NOW, 0).is_none(), "history above the fold triggered");
        screen.push_str("5-hour limit reached ∙ resets 3pm\n");
        assert!(match_screen(&screen, 20, NOW, 0).is_some());
    }

    #[test]
    fn the_machine_reset_time_is_read_exactly() {
        let h = hit("Claude AI usage limit reached|1794003600").unwrap();
        assert_eq!(h.resets_at, Some(1_794_003_600));
    }

    #[test]
    fn a_clock_reset_is_the_next_occurrence_in_local_time() {
        // NOW = 1794000000 = 2026-11-06T21:20:00Z.
        let utc = parse_reset("resets 11pm", NOW, 0).unwrap();
        assert_eq!(utc, days_from_civil(2026, 11, 6) as u64 * 86_400 + 23 * 3600);
        // 3pm has passed today in UTC, so it is tomorrow.
        let tomorrow = parse_reset("resets 3pm", NOW, 0).unwrap();
        assert_eq!(tomorrow, days_from_civil(2026, 11, 7) as u64 * 86_400 + 15 * 3600);
        // In UTC+9 it is already 06:20 on the 7th, so 3pm is today there.
        let tokyo = parse_reset("resets at 3:30 pm", NOW, 9 * 3600).unwrap();
        assert_eq!(tokyo, days_from_civil(2026, 11, 7) as u64 * 86_400 + 15 * 3600 + 1800 - 9 * 3600);
        assert_eq!(parse_reset("limit will reset at 15:00", NOW, 0), Some(tomorrow));
        // Not a clock: no time at all, or a bare number.
        assert_eq!(parse_reset("resets mon", NOW, 0), None);
        assert_eq!(parse_reset("resets 3", NOW, 0), None);
    }

    #[test]
    fn rfc3339_timestamps_parse() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-11-06T21:20:00.123Z"), Some(NOW));
        assert_eq!(parse_rfc3339("2026-11-06T22:20:00+01:00"), Some(NOW));
        assert_eq!(parse_rfc3339("garbage"), None);
    }

    // Transcript records, in the shapes Claude Code writes.
    fn rec(text: &str, ts: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "isApiErrorMessage": true,
            "timestamp": ts,
            "sessionId": "conv-1",
            "message": {"role": "assistant", "model": "<synthetic>", "content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    #[test]
    fn a_transcript_api_error_record_is_a_limit() {
        let line = rec("Claude AI usage limit reached|1794003600", "2026-11-06T21:20:00.000Z");
        let h = match_transcript_line(&line, NOW, 0).unwrap();
        assert_eq!(h.hit.class, LimitClass::Usage);
        assert_eq!(h.at, Some(NOW));
        assert_eq!(h.hit.resets_at, Some(1_794_003_600));

        let rate = rec(
            "API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"x\"}}",
            "2026-11-06T21:20:00Z",
        );
        assert_eq!(match_transcript_line(&rate, NOW, 0).unwrap().hit.class, LimitClass::Rate);

        // Newer shape: an `error` field beside unrecognised text.
        let field = serde_json::json!({
            "type": "assistant", "error": "rate_limit", "timestamp": "2026-11-06T21:20:00Z",
            "message": {"content": [{"type": "text", "text": "Something new happened"}]}
        })
        .to_string();
        assert_eq!(match_transcript_line(&field, NOW, 0).unwrap().hit.class, LimitClass::Rate);
    }

    #[test]
    fn ordinary_records_that_mention_limits_are_conversation() {
        let user = serde_json::json!({
            "type": "user", "timestamp": "2026-11-06T21:20:00Z",
            "message": {"role": "user", "content": "usage limit reached — what does this mean? API Error: 429"}
        })
        .to_string();
        assert!(match_transcript_line(&user, NOW, 0).is_none());
        let prose = serde_json::json!({
            "type": "assistant", "timestamp": "2026-11-06T21:20:00Z",
            "message": {"content": [{"type": "text", "text": "The handler maps rate_limit_error to a 429."}]}
        })
        .to_string();
        assert!(match_transcript_line(&prose, NOW, 0).is_none());
        assert!(match_transcript_line("not json", NOW, 0).is_none());
    }

    #[test]
    fn only_records_newer_than_the_binding_count() {
        let tail = [
            rec("Claude AI usage limit reached|1794003600", "2026-11-06T21:00:00Z"),
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}".to_owned(),
            rec("API Error: 429 rate_limit_error", "2026-11-06T21:10:00Z"),
        ]
        .join("\n");
        let newest = newest_transcript_limit(&tail, NOW - 3600, NOW, 0).unwrap();
        assert_eq!(newest.hit.class, LimitClass::Rate);
        // Bound after both: neither counts — this is the debounce.
        assert!(newest_transcript_limit(&tail, NOW - 60, NOW, 0).is_none());
    }

    fn chain(members: &[&str]) -> ChainState {
        ChainState {
            id: "c1".into(),
            members: members.iter().map(|m| (*m).to_owned()).collect(),
            active: members[0].to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn a_limit_moves_to_the_next_member_and_marks_the_old_one() {
        let mut c = chain(&["a", "b", "c"]);
        assert_eq!(c.on_limit(LimitClass::Usage, NOW, Some(NOW + 600)), Decision::SwitchTo("b".into()));
        assert_eq!(c.active, "b");
        assert!(c.is_exhausted("a", NOW));
        assert_eq!(c.exhausted["a"].resets_at, Some(NOW + 600));
    }

    #[test]
    fn exhausted_members_are_skipped_and_the_chain_never_wraps() {
        let mut c = chain(&["a", "b", "c"]);
        c.exhausted.insert("b".into(), Exhaustion { at: NOW, resets_at: None });
        assert_eq!(c.on_limit(LimitClass::Rate, NOW, None), Decision::SwitchTo("c".into()));
        // At the end: "a" recovered long ago would still not be cycled back to.
        let later = NOW + 10 * 3600;
        assert_eq!(c.on_limit(LimitClass::Usage, later, None), Decision::EndOfChain);
        assert_eq!(c.active, "c", "end of chain leaves the session where it is");
        assert_eq!(c.recovered(later), ["a", "b"], "recovered members are offered, not used");
    }

    #[test]
    fn overloaded_is_retried_not_switched_unless_the_chain_opts_in() {
        let mut c = chain(&["a", "b"]);
        assert_eq!(c.on_limit(LimitClass::Overloaded, NOW, None), Decision::Ignore);
        assert_eq!(c.active, "a");
        assert!(c.exhausted.is_empty());
        c.switch_on_overloaded = true;
        assert_eq!(c.on_limit(LimitClass::Overloaded, NOW, None), Decision::SwitchTo("b".into()));
    }

    #[test]
    fn switching_back_clears_the_member_s_exhaustion() {
        let mut c = chain(&["a", "b"]);
        c.on_limit(LimitClass::Usage, NOW, Some(NOW + 60));
        assert!(c.recovered(NOW).is_empty(), "not yet reset");
        assert_eq!(c.recovered(NOW + 61), ["a"]);
        assert!(c.switch_back("a"));
        assert_eq!(c.active, "a");
        assert!(!c.is_exhausted("a", NOW));
        assert!(!c.switch_back("zzz"));
    }

    #[test]
    fn one_limit_triggers_one_switch() {
        // The record that caused the switch predates the new binding.
        assert!(!should_act(NOW - 5, NOW, Some(NOW), NOW + 1));
        // A fresh record inside the cooldown still waits.
        assert!(!should_act(NOW + 10, NOW, Some(NOW), NOW + 10));
        // Fresh, and past the cooldown.
        assert!(should_act(NOW + 100, NOW, Some(NOW), NOW + SWITCH_COOLDOWN_SECS + 1));
        // First switch ever.
        assert!(should_act(NOW + 1, NOW, None, NOW + 1));
    }

    #[test]
    fn the_resume_line_keeps_rendering_and_flags_and_drops_the_prompt() {
        let line = crate::launch_line(
            None,
            &["--dangerously-skip-permissions".into(), "fix the 'bug' now".into()],
            crate::Rendering::Fullscreen,
        );
        assert_eq!(
            resume_line(&line, Some("0b1c2d3e-aaaa")).unwrap(),
            "CLAUDE_CODE_NO_FLICKER=1 claude --resume 0b1c2d3e-aaaa --dangerously-skip-permissions"
        );
        // An existing resume is replaced, not doubled.
        let resumed = crate::launch_line(Some("old-id"), &[], crate::Rendering::Inline);
        assert_eq!(
            resume_line(&resumed, Some("new-id")).unwrap(),
            "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 CLAUDE_CODE_DISABLE_MOUSE=1 claude --resume new-id"
        );
        // Unknown conversation: continue the newest in the folder.
        assert_eq!(resume_line("claude --model opus", None).unwrap(), "claude --continue --model opus");
        // A hostile id is quoted.
        let evil = resume_line("claude", Some("x; rm -rf ~")).unwrap();
        assert_eq!(evil, "claude --resume 'x; rm -rf ~'");
        // Not a Claude launch, or not a simple one: refused.
        assert!(resume_line("pi --tui-mode regular", None).is_none());
        assert!(resume_line("cd /x && claude", None).is_none());
        assert!(resume_line("echo hi claude", None).is_none());
    }

    #[test]
    fn the_log_is_bounded_in_count_and_age() {
        let mut log = FallbackLog::default();
        for i in 0..(LOG_MAX as u64 + 20) {
            log = log_append(log, FallbackRecord { at: NOW + i, ..Default::default() }, NOW + i);
        }
        assert_eq!(log.switches.len(), LOG_MAX);
        assert_eq!(log.switches.last().unwrap().at, NOW + LOG_MAX as u64 + 19);
        let log = log_append(log, FallbackRecord { at: NOW + LOG_MAX_AGE_SECS + 500, ..Default::default() }, NOW + LOG_MAX_AGE_SECS + 500);
        assert!(log.switches.iter().all(|r| r.at + LOG_MAX_AGE_SECS >= NOW + LOG_MAX_AGE_SECS + 500));
        assert_eq!(log.version, LOG_VERSION);
    }
}
