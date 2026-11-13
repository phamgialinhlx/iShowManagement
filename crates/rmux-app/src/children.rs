//! Sub-sessions an agent spawned on its host — herdr-style orchestration.
//!
//! An agent running under `rmux-agent` can start further sessions with the
//! host-local `rmux spawn` CLI. The host records each one in
//! `~/.rmux/children.json` (`0600`, pruned of dead sessions by the host):
//!
//! ```json
//! {"version":1,"children":[{"session":"claude-x","parent":"claude-y",
//!   "kind":"claude","cwd":"/abs","name":"label","createdAt":1731300000000}]}
//! ```
//!
//! The desktop reads it so a child can appear beside its parent pane without
//! anyone asking. This module only *reads*: adoption decisions belong to the UI
//! (`ui/src/lib/children.ts`), which knows what this window already shows.
//!
//! ## Tolerant, because the writer is a different build
//!
//! The file is written by whichever agent build spawned the child, and a host
//! routinely runs several builds side by side. So the parse picks fields out
//! rather than binding to a schema: unknown fields are ignored, a row missing a
//! required field is skipped, and a file that is not JSON at all is *no
//! children* with a logged warning — never an error the operator sees, since a
//! sidebar that turns red because a helper file is half-written is a sidebar
//! reporting a problem that does not exist. A missing file is the ordinary case
//! (an older agent, or nothing spawned yet) and is simply empty.

use rmux_transport::{CommandSpec, Target, Tty};
use serde::Serialize;
use serde_json::Value;

use crate::terminal::TargetRef;

/// One spawned child, as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Child {
    /// The daemon session name — attached to verbatim (`hostName`).
    pub session: String,
    /// The daemon session name of whoever spawned it; `None` for a child
    /// started from a shell with no rmux parent.
    pub parent: Option<String>,
    /// `claude`, `pi` or `terminal`. Anything else is skipped: the UI has no
    /// pane that could show it.
    pub kind: String,
    pub cwd: String,
    pub name: String,
    /// Unix ms; 0 when absent.
    pub created_at: i64,
    /// The credential set (profile id, `default` or `none`) the host bound it
    /// to at spawn — inherited from its parent. The app copies the parent's
    /// profile into the adopted session so a restart from the UI agrees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The fallback chain it inherited, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
}

/// Resolve home and read the file in one round trip.
///
/// `cd` with no argument goes home (even with `$HOME` unset) and the path that
/// follows is relative — so `$HOME` never sits inside anything quoted, which is
/// the "never put `$HOME` in a quoted path" rule. `|| true`: a missing file is
/// the normal answer, not a failure.
pub fn read_script() -> &'static str {
    "cd 2>/dev/null && cat .rmux/children.json 2>/dev/null || true"
}

/// Parse the file's contents. Never fails; see the module docs.
pub fn parse_children(raw: &str) -> Vec<Child> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let doc: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("children.json is not valid JSON, treating as empty: {e}");
            return Vec::new();
        }
    };
    let Some(rows) = doc.get("children").and_then(Value::as_array) else {
        tracing::warn!("children.json has no `children` array, treating as empty");
        return Vec::new();
    };

    let text = |row: &Value, key: &str| {
        row.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };

    let mut out: Vec<Child> = Vec::new();
    for row in rows {
        let Some(session) = text(row, "session") else { continue };
        let Some(kind) = text(row, "kind") else { continue };
        if !matches!(kind.as_str(), "claude" | "pi" | "terminal") {
            continue;
        }
        // One entry per session: a duplicate would adopt one shell twice.
        if out.iter().any(|c| c.session == session) {
            continue;
        }
        let parent = text(row, "parent").filter(|p| *p != session);
        out.push(Child {
            name: text(row, "name").unwrap_or_else(|| session.clone()),
            cwd: text(row, "cwd").unwrap_or_default(),
            created_at: row.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
            profile: text(row, "profile"),
            chain: text(row, "chain"),
            session,
            parent,
            kind,
        });
    }
    out
}

/// Read and parse one host's children.
pub async fn read_children(target: &dyn Target) -> anyhow::Result<Vec<Child>> {
    let spec = CommandSpec::new("sh").arg("-c").arg(read_script()).tty(Tty::None);
    let out = target.exec(&spec).await?;
    Ok(parse_children(&out.stdout))
}

/// `host_children` — the IPC entry point. A target that cannot be reached is an
/// error (the UI's poll treats it as "no answer this tick"); a file that cannot
/// be parsed is not.
pub async fn host_children(target: TargetRef) -> Result<Vec<Child>, String> {
    let t = crate::services::resolved(&target).await?;
    read_children(t.as_ref()).await.map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetArgs {
    pub target: TargetRef,
}

pub fn register(r: &mut crate::registry::Registry) {
    use crate::registry::Web;
    // Read-only, like `host_sessions`: it names sessions the operator could
    // already list from HOST ▸ SESSIONS.
    r.add("host_children", Web::Yes, |_, a: TargetArgs| host_children(a.target));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_contract_shape() {
        let raw = r#"{"version":1,"children":[
            {"session":"claude-a","parent":"claude-p","kind":"claude","cwd":"/w","name":"lint","createdAt":17},
            {"session":"term-b","parent":null,"kind":"terminal","cwd":"/w/x","name":"build","createdAt":18}
        ]}"#;
        let c = parse_children(raw);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].session, "claude-a");
        assert_eq!(c[0].parent.as_deref(), Some("claude-p"));
        assert_eq!(c[0].kind, "claude");
        assert_eq!(c[0].cwd, "/w");
        assert_eq!(c[0].name, "lint");
        assert_eq!(c[0].created_at, 17);
        assert_eq!(c[1].parent, None);
        assert_eq!(c[1].kind, "terminal");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let raw = r#"{"version":9,"extra":true,"children":[
            {"session":"pi-a","kind":"pi","cwd":"/w","name":"n","createdAt":1,"model":"x","nested":{"a":1}}
        ]}"#;
        let c = parse_children(raw);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].session, "pi-a");
    }

    #[test]
    fn malformed_is_empty_not_an_error() {
        assert!(parse_children("").is_empty(), "missing file");
        assert!(parse_children("   \n").is_empty());
        assert!(parse_children("{not json").is_empty());
        assert!(parse_children(r#"{"version":1,"chi"#).is_empty(), "half-written");
        assert!(parse_children("[]").is_empty());
        assert!(parse_children(r#"{"children":"nope"}"#).is_empty());
    }

    #[test]
    fn bad_rows_are_skipped_good_rows_kept() {
        let raw = r#"{"children":[
            {"kind":"claude"},
            {"session":"","kind":"claude"},
            {"session":"x-1","kind":"browser"},
            {"session":"claude-ok","kind":"claude"},
            {"session":"claude-ok","kind":"claude","name":"dup"},
            7, null, "s"
        ]}"#;
        let c = parse_children(raw);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].session, "claude-ok");
        // Absent name falls back to the session; absent fields default.
        assert_eq!(c[0].name, "claude-ok");
        assert_eq!(c[0].cwd, "");
        assert_eq!(c[0].created_at, 0);
        assert_eq!(c[0].parent, None);
    }

    #[test]
    fn a_session_is_not_its_own_parent() {
        let c = parse_children(r#"{"children":[{"session":"a","parent":"a","kind":"pi"}]}"#);
        assert_eq!(c[0].parent, None);
    }

    #[test]
    fn serialises_camel_case() {
        let c = parse_children(r#"{"children":[{"session":"a","kind":"pi","createdAt":5}]}"#);
        let v = serde_json::to_value(&c[0]).unwrap();
        assert_eq!(v["createdAt"], 5);
        assert!(v.get("created_at").is_none());
    }

    #[test]
    fn the_script_never_quotes_home() {
        assert!(!read_script().contains("$HOME"));
        assert!(read_script().contains("|| true"));
    }
}
