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
//! On a host with no credential store ([`Host::secrets`] is `None`) there are
//! no profiles, and saving one says why — a profile is never written somewhere
//! less safe instead. Sessions there run on whatever login the host already has.
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

use rmux_claude::profile::{self, ModelProfile, ProfileChain, ProfileKind, ProfileStore};
use rmux_transport::{CommandSpec, Target, Tty};
use serde::{Deserialize, Serialize};

use crate::ctx::Ctx;
use crate::host::{Host, SecretStore};
use crate::registry::{Call, NoArgs, Registry, Web};
use crate::secrets::{LEGACY_CLAUDE_SERVICE, NO_STORE, read_migrating};

/// Keychain slot. Separate from the account token — different lifetime, and
/// deleting every profile must not sign the operator out of Claude.
const SERVICE: &str = "group.yitec.rmux.claude";
const ACCOUNT: &str = "model-profiles";

fn store_of(host: &dyn Host) -> Result<&dyn SecretStore, String> {
    host.secrets().ok_or_else(|| NO_STORE.to_owned())
}

fn load_store(host: &dyn Host) -> ProfileStore {
    // No store, no profiles: the same answer as an empty keychain.
    let Some(secrets) = host.secrets() else {
        return ProfileStore::default();
    };
    // The legacy fallback matters most here: a session names its profile, and
    // one naming a profile that has vanished *refuses to start* rather than
    // quietly running against Anthropic. Losing this set breaks work.
    let Some(raw) = read_migrating(secrets, SERVICE, LEGACY_CLAUDE_SERVICE, ACCOUNT) else {
        return ProfileStore::default();
    };
    // A document that cannot be parsed is treated as absent rather than fatal:
    // refusing to start the app over a corrupt preference would be worse than
    // losing the preference. The old bare-array shape is still read.
    ProfileStore::parse(&raw)
}

fn load(host: &dyn Host) -> Vec<ModelProfile> {
    load_store(host).profiles
}

fn save_store(host: &dyn Host, store: &ProfileStore) -> Result<(), String> {
    let secrets = store_of(host)?;
    if store.is_empty() {
        // Leaving an empty document behind would keep a keychain item nothing
        // can reach from the UI — the same litter a stale background file is.
        let _ = secrets.delete(SERVICE, ACCOUNT);
        return Ok(());
    }
    let raw = serde_json::to_string(store).map_err(|e| e.to_string())?;
    secrets.set(SERVICE, ACCOUNT, &raw).map_err(|e| e.to_string())
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
    /// Which agent this profile is for — `claude`, `deepseek` or `openai`.
    pub kind: ProfileKind,
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
        kind: p.kind,
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

pub fn model_profiles(host: &dyn Host) -> Vec<ProfileView> {
    load(host).iter().map(view).collect()
}

/// Turn a pasted block into variables, without saving anything.
///
/// Separate from saving so the operator sees what rmux understood — which keys
/// it took, which it ignored, and what it wants to warn about — *before*
/// committing. A parser that silently drops half a paste and reports success is
/// how someone spends an afternoon debugging the wrong provider.
pub fn model_profile_parse(kind: Option<ProfileKind>, text: String) -> profile::Parsed {
    profile::parse_for(kind.unwrap_or_default(), &text)
}

/// Create or replace a profile from a pasted block.
///
/// `kind` defaults to Claude, which is what every caller from before kinds
/// existed meant. An existing profile's kind **cannot be changed**: sessions
/// already point at it, and a Claude profile that became an OpenAI one would
/// leave those sessions refusing to start for a reason nobody could see coming.
pub fn model_profile_save(
    host: &dyn Host,
    id: Option<String>,
    name: String,
    kind: Option<ProfileKind>,
    text: String,
) -> Result<Vec<ProfileView>, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("a profile needs a name — it is how you will pick it later".to_owned());
    }

    let mut store = load_store(host);
    let existing_kind = id.as_ref().and_then(|id| store.profile(id)).map(|p| p.kind);
    let kind = match (existing_kind, kind) {
        (Some(was), Some(now)) if was != now => {
            return Err(format!(
                "this is a {} profile and sessions already use it as one — create a new {} \
                 profile instead of changing its kind",
                was.as_str(),
                now.as_str()
            ));
        }
        (Some(was), _) => was,
        (None, kind) => kind.unwrap_or_default(),
    };

    let parsed = profile::parse_for(kind, &text);
    if parsed.vars.is_empty() {
        return Err(format!(
            "no {} variables found in that text",
            kind.namespaces().join(" or ")
        ));
    }

    upsert(&mut store, id, name, kind, parsed.vars);
    save_store(host, &store)?;
    Ok(store.profiles.iter().map(view).collect())
}

fn upsert(store: &mut ProfileStore, id: Option<String>, name: String, kind: ProfileKind, vars: std::collections::BTreeMap<String, String>) -> String {
    match id.and_then(|id| store.profiles.iter_mut().find(|p| p.id == id)) {
        Some(existing) => {
            existing.name = name;
            existing.vars = vars;
            existing.id.clone()
        }
        None => {
            // Ids are content-free and never reused: sessions store one, and a
            // recycled id would silently repoint a session at a different
            // provider.
            let id = next_id(&store.profiles);
            store.profiles.push(ModelProfile { id: id.clone(), name, kind, vars });
            id
        }
    }
}

/// Create or replace a **Claude account** profile from a subscription token
/// (`claude setup-token`). The token is checked by prefix: an admin key and an
/// API key are refused with the reason. It is stored in the keychain like every
/// other profile secret and reaches a host only over `setenv` stdin.
pub fn model_profile_save_token(
    host: &dyn Host,
    id: Option<String>,
    name: String,
    token: String,
) -> Result<Vec<ProfileView>, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("an account profile needs a name — it is how you will pick it later".to_owned());
    }
    let vars = profile::token_vars(&token)?;
    let mut store = load_store(host);
    if let Some(existing) = id.as_ref().and_then(|id| store.profile(id))
        && existing.kind != ProfileKind::Claude
    {
        return Err("only a Claude profile can hold a Claude subscription token".to_owned());
    }
    upsert(&mut store, id, name, ProfileKind::Claude, vars);
    save_store(host, &store)?;
    Ok(store.profiles.iter().map(view).collect())
}

pub fn model_profile_delete(host: &dyn Host, id: String) -> Result<Vec<ProfileView>, String> {
    let mut store = load_store(host);
    store.remove_profile(&id);
    save_store(host, &store)?;
    Ok(store.profiles.iter().map(view).collect())
}

/// Chains and per-kind defaults, for Settings › Models.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainsView {
    pub chains: Vec<ProfileChain>,
    pub defaults: std::collections::BTreeMap<ProfileKind, String>,
}

fn chains_view(store: &ProfileStore) -> ChainsView {
    ChainsView { chains: store.chains.clone(), defaults: store.defaults.clone() }
}

pub fn model_chains(host: &dyn Host) -> ChainsView {
    chains_view(&load_store(host))
}

/// Create or replace a fallback chain. Every member must be an existing
/// **Claude** profile, listed once — the daemon switches between members on a
/// Claude usage limit, and a DeepSeek key in a Claude chain would be exactly
/// the cross-provider routing kinds exist to prevent.
pub fn model_chain_save(
    host: &dyn Host,
    id: Option<String>,
    name: String,
    members: Vec<String>,
    switch_on_overloaded: bool,
) -> Result<ChainsView, String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("a chain needs a name".to_owned());
    }
    if members.len() < 2 {
        return Err("a chain needs at least two profiles — one to fall back from, one to fall back to".to_owned());
    }
    let mut store = load_store(host);
    let mut seen = std::collections::BTreeSet::new();
    for m in &members {
        let p = store.profile(m).ok_or_else(|| format!("no profile with id {m}"))?;
        if p.kind != ProfileKind::Claude {
            return Err(format!("“{}” is a {} profile; a chain holds Claude profiles only", p.name, p.kind.as_str()));
        }
        if !seen.insert(m.clone()) {
            return Err(format!("“{}” is in the chain twice", p.name));
        }
    }
    match id.and_then(|id| store.chains.iter_mut().find(|c| c.id == id)) {
        Some(c) => {
            c.name = name;
            c.members = members;
            c.switch_on_overloaded = switch_on_overloaded;
        }
        None => {
            let highest = store
                .chains
                .iter()
                .filter_map(|c| c.id.strip_prefix(profile::CHAIN_PREFIX)?.parse::<u64>().ok())
                .max()
                .unwrap_or(0);
            store.chains.push(ProfileChain {
                id: format!("{}{}", profile::CHAIN_PREFIX, highest + 1),
                name,
                kind: ProfileKind::Claude,
                members,
                switch_on_overloaded,
            });
        }
    }
    save_store(host, &store)?;
    Ok(chains_view(&store))
}

pub fn model_chain_delete(host: &dyn Host, id: String) -> Result<ChainsView, String> {
    let mut store = load_store(host);
    store.chains.retain(|c| c.id != id);
    save_store(host, &store)?;
    Ok(chains_view(&store))
}

/// Set or clear a kind's default profile — what a child of a *different* kind
/// binds to (a Claude session spawning Codex). Synced to a host as the
/// reserved `default` set whenever a session starts there.
pub fn model_profile_default_set(
    host: &dyn Host,
    kind: ProfileKind,
    id: Option<String>,
) -> Result<ChainsView, String> {
    let mut store = load_store(host);
    match id {
        Some(id) => {
            let p = store.profile(&id).ok_or_else(|| format!("no profile with id {id}"))?;
            if p.kind != kind {
                return Err(format!("“{}” is a {} profile, not a {} one", p.name, p.kind.as_str(), kind.as_str()));
            }
            store.defaults.insert(kind, id);
        }
        None => {
            store.defaults.remove(&kind);
        }
    }
    save_store(host, &store)?;
    Ok(chains_view(&store))
}

/// A fresh id, larger than every existing one.
fn next_id(profiles: &[ModelProfile]) -> String {
    let highest =
        profiles.iter().filter_map(|p| p.id.parse::<u64>().ok()).max().unwrap_or(0);
    (highest + 1).to_string()
}

/// What starting a session with a profile resolved to.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    /// The profile the session starts on (for a chain, the active member).
    /// Launchers read non-secret launch settings from it — Codex's model and
    /// base URL travel as flags, not environment.
    pub profile: Option<ModelProfile>,
    /// `attach` arguments that bind the new session to its set (`--env-set …`,
    /// and for a chain `--env-chain … --env-members …`). Names only — never a
    /// value from a set.
    pub attach_args: Vec<String>,
}

/// Deliver a session's credentials to a target's agent and say how to bind
/// the session to them.
///
/// **Named per-session sets.** The profile's variables are delivered as the
/// set named by its id (`setenv --kind K --set ID`, stdin), and the attach is
/// told `--env-set ID`, so the daemon applies *exactly* that set to *this*
/// session — two Claude sessions on one host can run on two accounts, and a
/// child can inherit its parent's. No profile binds to the reserved `none`.
/// A chain delivers every member and binds to the active one (`active`, or the
/// first member), carrying the member list so the daemon can switch on a
/// usage limit with the app closed.
///
/// Also delivered every time: each kind's **default** profile as the reserved
/// `default` set (what a child of another kind binds to), and the per-kind
/// clear-set, so an unbound session (the bridge, an older client) never
/// inherits whatever profile an older rmux left in the daemon.
///
/// A profile or chain that no longer exists, or of the wrong kind, is refused
/// by name: falling back to the host's default provider would bill the wrong
/// account and say nothing.
pub async fn apply_to_target(
    ctx: &Ctx,
    target: &dyn Target,
    kind: ProfileKind,
    profile_id: Option<&str>,
) -> Result<Applied, String> {
    apply_bound(ctx, target, kind, profile_id, None).await
}

/// [`apply_to_target`], starting a chain on `active` rather than its first
/// member — what a pane restarting after a switch passes.
pub async fn apply_bound(
    ctx: &Ctx,
    target: &dyn Target,
    kind: ProfileKind,
    profile_id: Option<&str>,
    active: Option<&str>,
) -> Result<Applied, String> {
    let store = load_store(ctx.host());
    let plan = plan_delivery(&store, kind, profile_id, active)?;

    let installed = crate::agent::ensure_agent(ctx, target).await?;
    // The per-kind set, cleared: nothing should be inheriting it any more.
    let known: Vec<&str> = store
        .profiles
        .iter()
        .filter(|p| p.kind == kind)
        .flat_map(|p| p.vars.keys().map(String::as_str))
        .collect();
    deliver(target, &installed.program, &["--kind", kind.as_str()], &profile::clearing_set_for(kind, known.iter().copied()))
        .await?;
    for (set, label, vars) in &plan.sets {
        deliver(target, &installed.program, &["--kind", set.0.as_str(), "--set", &set.1, "--label", label], vars).await?;
    }
    Ok(Applied { profile: plan.profile, attach_args: plan.attach_args })
}

/// One named set to deliver: ((kind, set name), label, variables).
type SetDelivery = ((ProfileKind, String), String, std::collections::BTreeMap<String, String>);

/// What [`apply_bound`] will send, decided without touching a host.
#[derive(Debug, Default)]
struct Delivery {
    sets: Vec<SetDelivery>,
    profile: Option<ModelProfile>,
    attach_args: Vec<String>,
}

fn plan_delivery(
    store: &ProfileStore,
    kind: ProfileKind,
    profile_id: Option<&str>,
    active: Option<&str>,
) -> Result<Delivery, String> {
    let mut out = Delivery::default();
    let wrong_kind = |p: &ModelProfile| {
        format!(
            "the profile “{}” is a {} profile, and this is a {} session — choose a {} profile in \
             Session Settings",
            p.name,
            p.kind.as_str(),
            kind.as_str(),
            kind.as_str()
        )
    };
    match profile_id {
        None => {
            out.attach_args = vec!["--env-set".into(), rmux_claude::env_kind::NO_PROFILE.into()];
        }
        Some(id) if profile::is_chain_id(id) => {
            let chain = store
                .chain(id)
                .ok_or_else(|| format!("the fallback chain this session uses no longer exists (id {id})"))?;
            if chain.kind != kind {
                return Err(format!("“{}” is a {} chain, and this is a {} session", chain.name, chain.kind.as_str(), kind.as_str()));
            }
            for m in &chain.members {
                let p = store.profile(m).ok_or_else(|| {
                    format!("the chain “{}” names a profile that no longer exists (id {m})", chain.name)
                })?;
                if p.kind != kind {
                    return Err(wrong_kind(p));
                }
                out.sets.push(((kind, p.id.clone()), p.name.clone(), p.session_set()));
            }
            let start = active.filter(|a| chain.members.iter().any(|m| m == a)).unwrap_or(&chain.members[0]);
            out.profile = store.profile(start).cloned();
            out.attach_args = vec![
                "--env-set".into(),
                start.to_owned(),
                "--env-chain".into(),
                chain.id.clone(),
                "--env-members".into(),
                chain.members.join(","),
            ];
            if chain.switch_on_overloaded {
                out.attach_args.push("--env-overloaded".into());
            }
        }
        Some(id) => {
            // Named rather than silently ignored: falling back to the default
            // provider because a profile was deleted would bill the wrong
            // account and say nothing.
            let p = store
                .profile(id)
                .ok_or_else(|| format!("the model profile this session uses no longer exists (id {id})"))?;
            if p.kind != kind {
                return Err(wrong_kind(p));
            }
            out.sets.push(((kind, p.id.clone()), p.name.clone(), p.session_set()));
            out.profile = Some(p.clone());
            out.attach_args = vec!["--env-set".into(), p.id.clone()];
        }
    }
    for (dkind, did) in &store.defaults {
        if let Some(p) = store.profile(did).filter(|p| p.kind == *dkind) {
            out.sets.push((
                (*dkind, rmux_claude::env_kind::DEFAULT_SET.to_owned()),
                p.name.clone(),
                p.session_set(),
            ));
        }
    }
    Ok(out)
}

/// One `rmux-agent setenv …` with the variables on **stdin**. As arguments
/// they would be in the host's `ps` output for every account on it; the flags
/// are words from closed sets, ids and display names — never a value.
async fn deliver(
    target: &dyn Target,
    program: &str,
    flags: &[&str],
    pairs: &std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    let spec = CommandSpec::new(program).arg("setenv").args(flags.iter().copied()).tty(Tty::None);
    let input = stdin_lines(pairs)?;
    let out = target
        .exec_with_input(&spec, input.as_bytes())
        .await
        .map_err(|e| format!("could not hand the model profile to the agent: {e}"))?;
    // `setenv --kind`/`--set` wait for the daemon's acknowledgement and exit
    // non-zero without one — an agent too old to keep per-session credentials
    // says so here, and the session does not start on a provider nobody chose.
    if out.status != 0 {
        let why = out.stderr.trim();
        return Err(if why.is_empty() {
            format!("the agent did not accept the profile (exit {})", out.status)
        } else {
            why.trim_start_matches("rmux-agent: ").to_owned()
        });
    }
    Ok(())
}

/// The switch log and current bindings on a host — `rmux-agent fallback-log --json`.
pub async fn fallback_log(ctx: &Ctx, target: &dyn Target) -> Result<serde_json::Value, String> {
    let installed = crate::agent::ensure_agent(ctx, target).await?;
    let spec = CommandSpec::new(&installed.program).arg("fallback-log").arg("--json").tty(Tty::None);
    let out = target.exec(&spec).await.map_err(|e| e.to_string())?;
    if out.status != 0 {
        return Err(out.stderr.trim().trim_start_matches("rmux-agent: ").to_owned());
    }
    serde_json::from_str(out.stdout.trim()).map_err(|e| format!("the agent's fallback log was unreadable: {e}"))
}

/// Move a chained session back to `member` (the operator's choice, never the
/// daemon's) and relaunch it on the same conversation.
pub async fn fallback_switch(ctx: &Ctx, target: &dyn Target, session: &str, member: &str) -> Result<(), String> {
    let store = load_store(ctx.host());
    let p = store.profile(member).ok_or_else(|| format!("no profile with id {member}"))?;
    // Re-deliver first: the daemon may have restarted since the session began.
    let installed = crate::agent::ensure_agent(ctx, target).await?;
    deliver(target, &installed.program, &["--kind", p.kind.as_str(), "--set", &p.id, "--label", &p.name], &p.session_set()).await?;
    let spec = CommandSpec::new(&installed.program)
        .arg("fallback-switch")
        .arg("--session")
        .arg(session)
        .arg("--to")
        .arg(member)
        .tty(Tty::None);
    let out = target.exec(&spec).await.map_err(|e| e.to_string())?;
    if out.status != 0 {
        return Err(out.stderr.trim().trim_start_matches("rmux-agent: ").to_owned());
    }
    Ok(())
}

/// `KEY=VALUE\n` lines for `setenv`'s stdin.
fn stdin_lines(pairs: &std::collections::BTreeMap<String, String>) -> Result<String, String> {
    let mut input = String::new();
    for (key, value) in pairs {
        // A newline inside a value would forge a second assignment on the far
        // side. Refused rather than stripped: silently altering a credential
        // produces a profile that fails in a way nobody can explain.
        if key.contains('\n') || value.contains('\n') {
            return Err(format!("{key} contains a newline, which cannot be sent safely"));
        }
        input.push_str(&format!("{key}={value}\n"));
    }
    Ok(input)
}

// ── Registration ─────────────────────────────────────────────────────────────
//
// Argument objects, keyed exactly as `ui/src/lib/api.ts` sends them.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParseArgs {
    #[serde(default)]
    pub kind: Option<ProfileKind>,
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveArgs {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub kind: Option<ProfileKind>,
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteArgs {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTokenArgs {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainSaveArgs {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub members: Vec<String>,
    #[serde(default)]
    pub switch_on_overloaded: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultArgs {
    pub kind: ProfileKind,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackLogArgs {
    pub target: crate::terminal::TargetRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackSwitchArgs {
    pub target: crate::terminal::TargetRef,
    pub session: String,
    pub member: String,
}

pub fn register(r: &mut Registry) {
    r.add("model_profile_save_token", Web::Yes, |call: Call, a: SaveTokenArgs| async move {
        model_profile_save_token(call.ctx().host(), a.id, a.name, a.token)
    });
    r.add("model_chains", Web::Yes, |call: Call, _: NoArgs| async move {
        Ok::<_, String>(model_chains(call.ctx().host()))
    });
    r.add("model_chain_save", Web::Yes, |call: Call, a: ChainSaveArgs| async move {
        model_chain_save(call.ctx().host(), a.id, a.name, a.members, a.switch_on_overloaded)
    });
    r.add("model_chain_delete", Web::Yes, |call: Call, a: DeleteArgs| async move {
        model_chain_delete(call.ctx().host(), a.id)
    });
    r.add("model_profile_default_set", Web::Yes, |call: Call, a: DefaultArgs| async move {
        model_profile_default_set(call.ctx().host(), a.kind, a.id)
    });
    r.add("claude_fallback_log", Web::Yes, |call: Call, a: FallbackLogArgs| async move {
        let t = crate::services::resolved(&a.target).await?;
        fallback_log(call.ctx(), t.as_ref()).await
    });
    r.add("claude_fallback_switch", Web::Yes, |call: Call, a: FallbackSwitchArgs| async move {
        let t = crate::services::resolved(&a.target).await?;
        fallback_switch(call.ctx(), t.as_ref(), &a.session, &a.member).await
    });
    r.add("model_profiles", Web::Yes, |call: Call, _: NoArgs| async move {
        Ok::<_, String>(model_profiles(call.ctx().host()))
    });
    r.add("model_profile_parse", Web::Yes, |_, a: ParseArgs| async move {
        Ok::<_, String>(model_profile_parse(a.kind, a.text))
    });
    r.add("model_profile_save", Web::Yes, |call: Call, a: SaveArgs| async move {
        model_profile_save(call.ctx().host(), a.id, a.name, a.kind, a.text)
    });
    r.add("model_profile_delete", Web::Yes, |call: Call, a: DeleteArgs| async move {
        model_profile_delete(call.ctx().host(), a.id)
    });
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
    fn a_deepseek_or_openai_key_never_reaches_the_view_either() {
        for (kind, text, secret) in [
            (ProfileKind::Deepseek, "DEEPSEEK_API_KEY=ds_secret_value\nDEEPSEEK_BASE_URL=https://ds.test", "ds_secret_value"),
            (ProfileKind::Openai, "OPENAI_API_KEY=oa_secret_value\nOPENAI_MODEL=gpt-5", "oa_secret_value"),
        ] {
            let p = ModelProfile {
                id: "1".into(),
                name: "x".into(),
                kind,
                vars: rmux_claude::profile::parse_for(kind, text).vars,
            };
            let rendered = serde_json::to_string(&view(&p)).unwrap();
            assert!(!rendered.contains(secret), "{rendered}");
            assert!(rendered.contains(&format!("\"kind\":\"{}\"", kind.as_str())), "{rendered}");
        }
    }

    #[test]
    fn a_value_with_a_newline_is_refused_not_forged() {
        let pairs = [("OPENAI_API_KEY".to_owned(), "k\nPATH=/evil".to_owned())].into_iter().collect();
        assert!(stdin_lines(&pairs).is_err());
    }

    #[test]
    fn the_view_never_carries_a_token() {
        // The webview renders untrusted content; a credential that reaches it is
        // one XSS away from leaving the machine.
        let profile = ModelProfile {
            kind: ProfileKind::Claude,
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

    fn store() -> ProfileStore {
        let p = |id: &str, kind, text: &str| ModelProfile {
            id: id.into(),
            name: format!("P{id}"),
            kind,
            vars: rmux_claude::profile::parse_for(kind, text).vars,
        };
        let mut s = ProfileStore {
            profiles: vec![
                p("1", ProfileKind::Claude, "CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-one"),
                p("2", ProfileKind::Claude, "CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-two"),
                p("3", ProfileKind::Openai, "OPENAI_API_KEY=oa"),
            ],
            chains: vec![ProfileChain {
                id: "chain-1".into(),
                name: "Max pair".into(),
                kind: ProfileKind::Claude,
                members: vec!["1".into(), "2".into()],
                switch_on_overloaded: false,
            }],
            ..Default::default()
        };
        s.defaults.insert(ProfileKind::Openai, "3".into());
        s
    }

    #[test]
    fn a_profile_is_delivered_under_its_id_and_the_attach_binds_to_it() {
        let d = plan_delivery(&store(), ProfileKind::Claude, Some("2"), None).unwrap();
        assert_eq!(d.attach_args, ["--env-set", "2"]);
        assert_eq!(d.sets[0].0, (ProfileKind::Claude, "2".to_owned()));
        // The token profile's set clears the API key route.
        assert_eq!(d.sets[0].2.get("ANTHROPIC_API_KEY").map(String::as_str), Some(""));
        // Each kind's default rides along as the reserved `default` set.
        assert!(d.sets.iter().any(|((k, n), _, _)| *k == ProfileKind::Openai && n == "default"));
        // No profile binds to the reserved "none".
        let none = plan_delivery(&store(), ProfileKind::Claude, None, None).unwrap();
        assert_eq!(none.attach_args, ["--env-set", "none"]);
    }

    #[test]
    fn a_chain_delivers_every_member_and_starts_on_the_active_one() {
        let d = plan_delivery(&store(), ProfileKind::Claude, Some("chain-1"), Some("2")).unwrap();
        assert_eq!(d.attach_args, ["--env-set", "2", "--env-chain", "chain-1", "--env-members", "1,2"]);
        let names: Vec<&str> = d.sets.iter().filter(|s| s.0.0 == ProfileKind::Claude).map(|s| s.0.1.as_str()).collect();
        assert_eq!(names, ["1", "2"]);
        assert_eq!(d.profile.unwrap().id, "2");
        // An active member that is not in the chain is ignored, not trusted.
        let d = plan_delivery(&store(), ProfileKind::Claude, Some("chain-1"), Some("3")).unwrap();
        assert_eq!(d.attach_args[1], "1");
    }

    #[test]
    fn a_missing_or_wrong_kind_profile_refuses_rather_than_falling_back() {
        assert!(plan_delivery(&store(), ProfileKind::Claude, Some("9"), None).unwrap_err().contains("no longer exists"));
        assert!(plan_delivery(&store(), ProfileKind::Claude, Some("3"), None).unwrap_err().contains("openai profile"));
        assert!(plan_delivery(&store(), ProfileKind::Claude, Some("chain-7"), None).unwrap_err().contains("chain"));
        assert!(plan_delivery(&store(), ProfileKind::Openai, Some("chain-1"), None).is_err());
    }
}
