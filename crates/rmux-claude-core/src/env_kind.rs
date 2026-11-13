//! Which credential environment a daemon session receives.
//!
//! The agent daemon spawns every session on a host, and it used to hold **one**
//! environment that every session inherited. That was fine while the only
//! credentials were Claude's. With DeepSeek Harness and Codex beside it, one
//! shared environment means a DeepSeek key in a Claude session and an Anthropic
//! base URL in Codex — and the failure CLAUDE.md calls out by name, a session
//! quietly talking to a provider nobody selected for it.
//!
//! So the daemon keeps one environment **per kind** and applies exactly the one
//! that matches the session it is spawning. Here is the single definition of
//! both halves: the kind names (shared with `rmux_claude::profile::ProfileKind`,
//! and a test there pins that they agree) and the rule that maps a daemon
//! session name to its kind.
//!
//! The rule is the session-name prefix — the same rule the bridge, the CLI and
//! the app already use to tell a Claude from a shell — so every spawn path
//! (desktop app, `rmux spawn`, the Redstone bridge) gets the right environment
//! with nothing to remember to pass.

/// The kinds a daemon keeps a separate environment for.
pub const KINDS: &[&str] = &["claude", "deepseek", "openai"];

/// The credential environment a session named `session` receives, if any.
///
/// `claude-` sessions get the Claude set, `codex-` the OpenAI set, `dsh-` the
/// DeepSeek set. Anything else — a shell, pi — gets none of them.
pub fn for_session(session: &str) -> Option<&'static str> {
    if session.starts_with("claude-") {
        Some("claude")
    } else if session.starts_with("codex-") {
        Some("openai")
    } else if session.starts_with("dsh-") {
        Some("deepseek")
    } else {
        None
    }
}

/// The reserved set name meaning **no profile**: the session gets no
/// profile variables at all — only what every session shares (the stored
/// account) and the host's own login. Always present in a daemon; it cannot be
/// delivered, because a set called "none" that carried something would be a
/// contradiction nobody could see.
pub const NO_PROFILE: &str = "none";

/// The reserved set name for a kind's **default profile** — what a child of a
/// *different* kind binds to when it cannot inherit its parent's set (a Claude
/// session spawning Codex). Delivered by the app like any profile's set.
pub const DEFAULT_SET: &str = "default";

/// Whether `name` may be used as a set name: a profile id, `default`, or
/// `none`. Printable, short, no whitespace — it is a key, and it travels in
/// argv (`--env-set`), where only names belong.
pub fn valid_set_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether `kind` is one a daemon will hold.
pub fn is_known(kind: &str) -> bool {
    KINDS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_agent_prefix_maps_to_exactly_its_own_kind() {
        assert_eq!(for_session("claude-session-1"), Some("claude"));
        assert_eq!(for_session("codex-codexsession-1"), Some("openai"));
        assert_eq!(for_session("dsh-dshsession-1"), Some("deepseek"));
        // A shell and pi carry no provider credential of ours.
        assert_eq!(for_session("term-1"), None);
        assert_eq!(for_session("pi-pisession-1"), None);
        // A prefix is a prefix, not a substring.
        assert_eq!(for_session("my-claude-notes"), None);
        for k in KINDS {
            assert!(is_known(k));
        }
        assert!(!is_known("anthropic"));
    }
}
