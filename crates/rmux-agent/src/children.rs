//! Who started which session: `~/.rmux/children.json`.
//!
//! When an agent inside one session spawns another (`rmux spawn`), the new
//! session is recorded here with its parent. Two readers depend on it:
//!
//! - **`rmux close`**, which refuses to end a session unless it descends from
//!   the caller — "never close what you did not create" enforced rather than
//!   merely written down in a skill.
//! - **The desktop app**, which places a spawned session beside its parent in
//!   the operator's window. That is a contract with another codebase, so the
//!   shape below is fixed:
//!
//! ```json
//! {"version":1,"children":[{"session":"claude-session-…","parent":"claude-session-…"|null,
//!   "kind":"claude"|"pi"|"terminal","cwd":"/abs/path","name":"label","createdAt":1731300000000}]}
//! ```
//!
//! ## The same discipline as `aliases.json`
//!
//! `0600` in the `0700` `~/.rmux`, written temp-then-rename, and **pruned of
//! sessions that no longer exist** on every write and every `children` read.
//! A file instead of daemon memory for the same two reasons the alias file
//! gives: the daemon's memory dies with its sessions, and more than one daemon
//! runs on a host.
//!
//! One addition: writes are serialised with `flock` on a sibling lock file.
//! Two agents spawning at once is the *ordinary* case here (that is the point
//! of the feature), and an unlocked read-modify-write loses one of them.
//!
//! ## Orphans are re-parented, not dropped
//!
//! When a child is pruned, its own children inherit its parent. Otherwise
//! closing a middle session would cut the chain, and the agent that started the
//! whole tree could no longer close the grandchildren it is responsible for.

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The file's schema version. Bumped only for an incompatible change.
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Child {
    /// The daemon session name — what the app reattaches by.
    pub session: String,
    /// The daemon session name of whoever spawned it; `null` from outside rmux.
    pub parent: Option<String>,
    /// `claude`, `pi` or `terminal`.
    pub kind: String,
    pub cwd: String,
    /// The label asked for with `--name`, else the session name.
    pub name: String,
    /// Unix milliseconds.
    pub created_at: u64,
    /// The credential set (profile id) it was bound to at spawn — inherited
    /// from the parent unless `--profile` said otherwise. Absent for an unbound
    /// child and in every record written before bindings existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The fallback chain it is on, when it inherited one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Children {
    pub version: u32,
    pub children: Vec<Child>,
}

fn path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    Ok(home.join(".rmux").join("children.json"))
}

/// Read the file. Missing or unreadable is empty, not an error: this is
/// bookkeeping, and failing a `list` over it would trade working sessions for
/// a nicety. An unknown future version is also read as empty rather than
/// guessed at.
pub fn load() -> Children {
    let empty = Children { version: VERSION, children: Vec::new() };
    let Ok(path) = path() else { return empty };
    let Ok(text) = std::fs::read_to_string(&path) else { return empty };
    match serde_json::from_str::<Children>(&text) {
        Ok(c) if c.version == VERSION => c,
        _ => empty,
    }
}

/// Drop entries whose session is not live, re-parenting their children.
///
/// Pure, so the rule is testable without a daemon.
pub fn prune(mut children: Children, live: &[String]) -> Children {
    let live: HashSet<&str> = live.iter().map(String::as_str).collect();
    while let Some(dead) =
        children.children.iter().position(|c| !live.contains(c.session.as_str()))
    {
        let gone = children.children.remove(dead);
        for c in &mut children.children {
            if c.parent.as_deref() == Some(gone.session.as_str()) {
                c.parent = gone.parent.clone();
            }
        }
    }
    children.version = VERSION;
    children
}

/// Is `session` a descendant of `ancestor` (not equal to it)?
pub fn descends_from(children: &Children, session: &str, ancestor: &str) -> bool {
    let mut current = session.to_owned();
    // Bounded by the number of entries: a cycle (hand-edited file) cannot hang.
    for _ in 0..=children.children.len() {
        let Some(entry) = children.children.iter().find(|c| c.session == current) else {
            return false;
        };
        match &entry.parent {
            Some(p) if p == ancestor => return true,
            Some(p) => current = p.clone(),
            None => return false,
        }
    }
    false
}

/// Write the file atomically, `0600`, with `~/.rmux` kept `0700`.
fn save(children: &Children) -> anyhow::Result<()> {
    let path = path()?;
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(children)?;
    let temp = dir.join(format!("children.json.{}.tmp", std::process::id()));

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        // Mode set at creation, not after: there is no window in which the
        // temp file is readable by anyone else.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    std::fs::write(&temp, text.as_bytes())?;

    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Held while reading-modifying-writing the file. Released on drop.
struct Lock(#[allow(dead_code)] Option<std::fs::File>);

fn lock() -> Lock {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;
        let Ok(path) = path() else { return Lock(None) };
        let lock_path = path.with_file_name("children.lock");
        if let Some(dir) = lock_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
        else {
            return Lock(None);
        };
        // SAFETY: a valid open descriptor; `flock` blocks until granted and is
        // released when the file is closed.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        Lock(Some(file))
    }
    #[cfg(not(unix))]
    Lock(None)
}

/// Record a new child, pruning against `live` in the same write.
pub fn record(child: Child, live: &[String]) -> anyhow::Result<()> {
    let _lock = lock();
    let mut all = prune(load(), live);
    all.children.retain(|c| c.session != child.session);
    all.children.push(child);
    save(&all)
}

/// The pruned list, written back when pruning changed anything.
pub fn read_pruned(live: &[String]) -> anyhow::Result<Children> {
    let _lock = lock();
    let before = load();
    let after = prune(before.clone(), live);
    if after != before && path()?.exists() {
        save(&after)?;
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(session: &str, parent: Option<&str>) -> Child {
        Child {
            session: session.into(),
            parent: parent.map(str::to_owned),
            kind: "terminal".into(),
            cwd: "/tmp".into(),
            name: session.into(),
            created_at: 1,
            profile: None,
            chain: None,
        }
    }

    fn live(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_wire_shape_is_the_contract() {
        let c = Children { version: 1, children: vec![child("term-a", None)] };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"version":1,"children":[{"session":"term-a","parent":null,
                "kind":"terminal","cwd":"/tmp","name":"term-a","createdAt":1}]})
        );
    }

    #[test]
    fn pruning_reparents_orphans_so_ownership_survives() {
        // root → mid → leaf; mid is closed. The agent at root must still own leaf.
        let all = Children {
            version: 1,
            children: vec![child("mid", Some("root")), child("leaf", Some("mid"))],
        };
        let kept = prune(all, &live(&["root", "leaf"]));
        assert_eq!(kept.children, vec![child("leaf", Some("root"))]);
        assert!(descends_from(&kept, "leaf", "root"));
    }

    #[test]
    fn descent_is_strict_and_cycle_safe() {
        let all = Children {
            version: 1,
            children: vec![child("a", Some("b")), child("b", Some("a"))],
        };
        assert!(!descends_from(&all, "a", "c"), "a cycle must terminate");
        assert!(descends_from(&all, "a", "b"));
        let one = Children { version: 1, children: vec![child("x", Some("p"))] };
        assert!(!descends_from(&one, "p", "p"), "a session is not its own descendant");
        assert!(!descends_from(&one, "unknown", "p"));
    }
}
