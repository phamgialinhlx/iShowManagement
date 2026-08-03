//! Saved model configurations — Kimi, GLM, a reseller, an internal gateway.
//!
//! Claude Code picks its provider out of the environment, so "use GLM for this
//! session" is a set of variables. A profile is that set, named, so it is chosen
//! once and selected thereafter.
//!
//! ## Where a profile is kept, and why it is not `localStorage`
//!
//! A profile routinely contains `ANTHROPIC_AUTH_TOKEN`, which is a credential
//! for a paid account. `localStorage` is plain text on disk readable by anything
//! that can read the user's files, and it is also where the session list lives —
//! so a profile there would be both exposed and competing for a quota that costs
//! someone their sessions when it overflows. The whole set goes in the OS
//! keychain as one JSON document instead: one slot, atomic to write, and
//! encrypted at rest by the system.
//!
//! ## The webview is never given a token
//!
//! Same rule as the Claude account: the UI receives a redacted view, so an XSS
//! in a rendered transcript cannot read a credential out of the page. Editing
//! works by sending a *new* value down, never by round-tripping the old one back
//! up.
//!
//! ## And it reaches a host the same way the account does
//!
//! Over `rmux-agent setenv` — stdin, then the daemon's `0600` socket, then the
//! daemon's memory. Never argv: `spec_to_shell_line` renders `CommandSpec::env`
//! into a command line, and `ps` shows one user's command line to every account
//! on the machine.

use rmux_claude::profile::{self, ModelProfile};
use rmux_transport::{CommandSpec, Target, Tty};
use serde::Serialize;

/// Keychain slot. Separate from the account token — different lifetime, and
/// deleting every profile must not sign the operator out of Claude.
const SERVICE: &str = "ai.betterscale.rmux.claude";
const ACCOUNT: &str = "model-profiles";

fn entry() -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, ACCOUNT)?)
}

fn load() -> Vec<ModelProfile> {
    let Ok(entry) = entry() else { return Vec::new() };
    let Ok(raw) = entry.get_password() else { return Vec::new() };
    // A profile set that cannot be parsed is treated as absent rather than
    // fatal: refusing to start the app over a corrupt preference would be worse
    // than losing the preference.
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save(profiles: &[ModelProfile]) -> Result<(), String> {
    let entry = entry().map_err(|e| e.to_string())?;
    if profiles.is_empty() {
        // Leaving an empty array behind would keep a keychain item nothing can
        // reach from the UI — the same litter a stale background file is.
        let _ = entry.delete_credential();
        return Ok(());
    }
    let raw = serde_json::to_string(profiles).map_err(|e| e.to_string())?;
    entry.set_password(&raw).map_err(|e| e.to_string())
}

/// What the UI is allowed to see.
///
/// Values are shown, except the secret ones — the operator needs to check a
/// model name or a base URL at a glance, and hiding those would make a profile
/// unverifiable.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    pub id: String,
    pub name: String,
    /// The provider this profile sends requests, and the token, to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub has_credential: bool,
    /// Every variable, with credentials replaced by their last four characters.
    pub vars: Vec<VarView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VarView {
    pub key: String,
    pub value: String,
    pub secret: bool,
}

fn view(p: &ModelProfile) -> ProfileView {
    ProfileView {
        id: p.id.clone(),
        name: p.name.clone(),
        endpoint: p.endpoint().map(str::to_owned),
        has_credential: p.has_credential(),
        vars: p
            .vars
            .iter()
            .map(|(key, value)| VarView {
                key: key.clone(),
                value: if profile::is_secret(key) { profile::redact(value) } else { value.clone() },
                secret: profile::is_secret(key),
            })
            .collect(),
    }
}

#[tauri::command]
pub fn model_profiles() -> Vec<ProfileView> {
    load().iter().map(view).collect()
}

/// Turn a pasted block into variables, without saving anything.
///
/// Separate from saving so the operator sees what rmux understood — which keys
/// it took, which it ignored, and what it wants to warn about — *before*
/// committing. A parser that silently drops half a paste and reports success is
/// how someone spends an afternoon debugging the wrong provider.
#[tauri::command]
pub fn model_profile_parse(text: String) -> profile::Parsed {
    profile::parse(&text)
}

/// Create or replace a profile from a pasted block.
#[tauri::command]
pub fn model_profile_save(
    id: Option<String>,
    name: String,
    text: String,
) -> Result<Vec<ProfileView>, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("a profile needs a name — it is how you will pick it later".to_owned());
    }

    let parsed = profile::parse(&text);
    if parsed.vars.is_empty() {
        return Err("no ANTHROPIC_ or CLAUDE_CODE_ variables found in that text".to_owned());
    }

    let mut profiles = load();
    match id.and_then(|id| profiles.iter_mut().find(|p| p.id == id)) {
        Some(existing) => {
            existing.name = name;
            existing.vars = parsed.vars;
        }
        None => {
            // Ids are content-free and never reused: sessions store one, and a
            // recycled id would silently repoint a session at a different
            // provider.
            let id = next_id(&profiles);
            profiles.push(ModelProfile { id, name, vars: parsed.vars });
        }
    }

    save(&profiles)?;
    Ok(profiles.iter().map(view).collect())
}

#[tauri::command]
pub fn model_profile_delete(id: String) -> Result<Vec<ProfileView>, String> {
    let mut profiles = load();
    profiles.retain(|p| p.id != id);
    save(&profiles)?;
    Ok(profiles.iter().map(view).collect())
}

/// A fresh id, larger than every existing one.
fn next_id(profiles: &[ModelProfile]) -> String {
    let highest =
        profiles.iter().filter_map(|p| p.id.parse::<u64>().ok()).max().unwrap_or(0);
    (highest + 1).to_string()
}

/// Push a profile — or the absence of one — to a target's agent.
///
/// Called before every Claude session starts, *including* when no profile is
/// selected. That is not a no-op: the daemon outlives sessions, so a previous
/// profile's base URL is still in its environment, and starting without clearing
/// it would run the new session against the old provider while the UI showed
/// Anthropic. Selecting nothing is an instruction to undo, not an absence.
///
/// The clear-set spans every variable *any* stored profile uses, so switching
/// also removes something a different profile introduced.
pub async fn apply_to_target<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: &dyn Target,
    profile_id: Option<&str>,
) -> Result<(), String> {
    let profiles = load();

    // Nothing has ever been configured and nothing is selected: leave the host
    // alone entirely rather than installing an agent to clear an empty set.
    if profiles.is_empty() && profile_id.is_none() {
        return Ok(());
    }

    let known: Vec<&str> =
        profiles.iter().flat_map(|p| p.vars.keys().map(String::as_str)).collect();

    let pairs = match profile_id {
        Some(id) => {
            let found = profiles.iter().find(|p| p.id == id).ok_or_else(|| {
                // Named rather than silently ignored: falling back to Anthropic
                // because a profile was deleted would bill the wrong account and
                // say nothing.
                format!("the model profile this session uses no longer exists (id {id})")
            })?;
            found.apply_set(known.iter().copied())
        }
        None => profile::clearing_set(known.iter().copied()),
    };

    let installed = crate::agent::ensure_agent(app, target).await?;
    let spec = CommandSpec::new(&installed.program).arg("setenv").tty(Tty::None);

    // Over **stdin**, one `KEY=VALUE` line each. As arguments these would be in
    // the host's `ps` output for every account on it, and one of them is a token.
    let mut input = String::new();
    for (key, value) in &pairs {
        // A newline inside a value would forge a second assignment on the far
        // side. Refused rather than stripped: silently altering a credential
        // produces a profile that fails in a way nobody can explain.
        if key.contains('\n') || value.contains('\n') {
            return Err(format!("{key} contains a newline, which cannot be sent safely"));
        }
        input.push_str(&format!("{key}={value}\n"));
    }

    target
        .exec_with_input(&spec, input.as_bytes())
        .await
        .map_err(|e| format!("could not hand the model profile to the agent: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_never_reused() {
        // A session stores this id. Recycling one after a delete would silently
        // repoint an old session at whatever profile inherited the number.
        let profiles = vec![
            ModelProfile { id: "1".into(), name: "a".into(), ..Default::default() },
            ModelProfile { id: "7".into(), name: "b".into(), ..Default::default() },
        ];
        assert_eq!(next_id(&profiles), "8");
        assert_eq!(next_id(&[]), "1");
    }

    #[test]
    fn the_view_never_carries_a_token() {
        // The webview renders untrusted content; a credential that reaches it is
        // one XSS away from leaving the machine.
        let profile = ModelProfile {
            id: "1".into(),
            name: "Vendor".into(),
            vars: rmux_claude::profile::parse(
                "ANTHROPIC_BASE_URL=https://vendor.test\nANTHROPIC_AUTH_TOKEN=tok_secret_value",
            )
            .vars,
        };

        let rendered = serde_json::to_string(&view(&profile)).unwrap();
        assert!(!rendered.contains("tok_secret_value"), "the token reached the UI: {rendered}");
        // But the base URL is shown in full — it is the fact the operator has to
        // check, and hiding it would make a profile unverifiable.
        assert!(rendered.contains("https://vendor.test"), "{rendered}");
    }
}
