//! Display names for sessions, kept on the host.
//!
//! A session's key is how the daemon holds it and how a client reattaches, so
//! renaming *that* would orphan running work. An alias is additive: the key
//! stays `term-…`, and the alias is a second name that resolves to it.
//!
//! ## Why a file rather than a frame
//!
//! The obvious design — and the one PR #9 built — puts the map in the daemon and
//! adds a wire frame to set it. Two things go wrong with that, and both are
//! properties of how this agent already works:
//!
//! 1. **The daemon owns the sessions**, so its map dies exactly when they do.
//!    Nothing is gained by keeping it in memory, and a rename is lost to any
//!    restart.
//! 2. **There is more than one daemon.** A rebuilt agent runs beside the older
//!    build until the old one's sessions end — that is deliberate, so upgrading
//!    never kills work — and `list` unions all of them. A per-daemon map means a
//!    session renamed under one build shows its raw key under another, which
//!    reads as the rename silently failing.
//!
//! A file answers both, and it costs no protocol change at all: `attach`
//! resolves the alias to a key *before* it sends `Hello`, so the daemon never
//! learns aliases exist and an older daemon works unmodified.
//!
//! ## What is stored
//!
//! `~/.rmux/aliases.json`, `0600`, a flat `{ "<key>": "<alias>" }`. Keyed by the
//! session key rather than by alias so a session can be renamed repeatedly
//! without leaving its earlier names behind.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Characters an alias may not contain.
///
/// `list` prints tab-separated columns and one line per session, so a tab or a
/// newline in a name does not merely look wrong — it produces a row the client
/// parses into the wrong fields, or two rows for one session. NUL is refused
/// because it terminates the strings this eventually crosses.
const FORBIDDEN: [char; 3] = ['\0', '\n', '\t'];

fn path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".rmux").join("aliases.json"))
}

/// Every alias on this host, keyed by session key.
///
/// A missing or unreadable file is an **empty map, not an error**: aliases are
/// cosmetic, and failing `list` or `attach` because a display name could not be
/// read would trade a working session for a nicety.
pub fn load() -> BTreeMap<String, String> {
    let Ok(path) = path() else { return BTreeMap::new() };
    let Ok(text) = std::fs::read_to_string(&path) else { return BTreeMap::new() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// The key an alias points at, or the name itself when it is already a key.
///
/// Called before `Hello`, which is what keeps this out of the wire protocol.
pub fn resolve(name: &str) -> String {
    let aliases = load();
    aliases
        .iter()
        .find_map(|(key, alias)| (alias == name).then(|| key.clone()))
        .unwrap_or_else(|| name.to_owned())
}

/// Write the map, replacing the file atomically.
///
/// Temp-file-then-rename because several daemons may be running: a half-written
/// file would be read as no aliases at all by whichever process looked next, and
/// `rename` within a directory is atomic, so a reader sees either the old map or
/// the new one.
fn save(aliases: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let path = path()?;
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent directory"))?;
    std::fs::create_dir_all(dir)?;

    let text = serde_json::to_string_pretty(aliases)?;
    let temp = dir.join(format!("aliases.json.{}.tmp", std::process::id()));
    std::fs::write(&temp, text.as_bytes())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A session name can say what someone is working on. Same reasoning as
        // the socket beside it.
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Point `alias` at `key`, given the session keys currently live on the host.
///
/// `live` is passed in rather than looked up so this is a pure decision that can
/// be tested without a daemon — and so the caller, which has just listed the
/// sessions anyway, does not list them twice.
///
/// Pruning happens here rather than on a timer: every write already knows what
/// is running, and an entry for a session that ended is a name nothing can
/// resolve. Without it the file grows for the life of the host.
pub fn set(key: &str, alias: &str, live: &[String]) -> anyhow::Result<()> {
    let alias = alias.trim();
    anyhow::ensure!(!alias.is_empty(), "an alias cannot be empty");
    anyhow::ensure!(
        !alias.contains(FORBIDDEN),
        "an alias cannot contain a tab, a newline or a NUL"
    );
    anyhow::ensure!(live.iter().any(|s| s == key), "no running session named {key}");

    // Renaming something to its own key is what "undo the rename" looks like.
    if alias == key {
        let mut aliases = prune(load(), live);
        aliases.remove(key);
        return save(&aliases);
    }

    anyhow::ensure!(
        !live.iter().any(|s| s == alias),
        "a session named {alias} is already running"
    );

    let mut aliases = prune(load(), live);
    // Two sessions answering to one name would make `attach` a coin toss.
    if let Some((owner, _)) = aliases.iter().find(|(k, a)| *a == alias && k.as_str() != key) {
        anyhow::bail!("{owner} is already called {alias}");
    }

    aliases.insert(key.to_owned(), alias.to_owned());
    save(&aliases)
}

/// Drop aliases for sessions that are no longer running.
fn prune(aliases: BTreeMap<String, String>, live: &[String]) -> BTreeMap<String, String> {
    aliases.into_iter().filter(|(key, _)| live.iter().any(|s| s == key)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn an_alias_may_not_break_the_list_format() {
        let sessions = live(&["term-1"]);
        for bad in ["with\ttab", "with\nnewline", "with\0nul"] {
            let err = set("term-1", bad, &sessions).expect_err("must be refused");
            assert!(err.to_string().contains("cannot contain"), "got: {err}");
        }
        let err = set("term-1", "   ", &sessions).expect_err("blank must be refused");
        assert!(err.to_string().contains("cannot be empty"), "got: {err}");
    }

    #[test]
    fn an_alias_needs_a_live_session() {
        let err = set("term-gone", "webapp", &live(&["term-1"])).expect_err("must be refused");
        assert!(err.to_string().contains("no running session"), "got: {err}");
    }

    #[test]
    fn an_alias_may_not_shadow_a_live_session() {
        // Otherwise attaching by that name is ambiguous: it is both a real key
        // and someone else's display name.
        let err = set("term-1", "term-2", &live(&["term-1", "term-2"]))
            .expect_err("shadowing a live key must be refused");
        assert!(err.to_string().contains("already running"), "got: {err}");
    }

    #[test]
    fn pruning_drops_names_for_sessions_that_ended() {
        let mut aliases = BTreeMap::new();
        aliases.insert("term-1".to_owned(), "webapp".to_owned());
        aliases.insert("term-old".to_owned(), "gone".to_owned());

        let kept = prune(aliases, &live(&["term-1"]));
        assert_eq!(kept.len(), 1, "only the live session keeps its name");
        assert_eq!(kept.get("term-1").map(String::as_str), Some("webapp"));
    }
}
