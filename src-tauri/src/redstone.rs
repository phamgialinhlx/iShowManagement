//! Enrolling a host with Redstone, so its Claude sessions can be driven from
//! Redstone's agent UI.
//!
//! rmux's part in this is deliberately small and entirely front-loaded. It signs
//! the operator in, asks Redstone for a **per-host token**, writes that token to
//! the host, and starts `rmux-agent bridge` there. After that rmux is not in the
//! path at all: the bridge dials Redstone directly and keeps working with this
//! app closed, which is the entire point — see `rmux-bridge`'s crate note.
//!
//! ## Why a token per host rather than the operator's own
//!
//! A dev box is a machine other people frequently have accounts on, and which is
//! rebuilt without ceremony. Putting the operator's Redstone session on every one
//! would make each of them a copy of their identity across the whole product. A
//! per-host token can be revoked on its own from Redstone's UI, and its blast
//! radius is the closed verb set in `rmux_bridge::protocol` against one machine.
//!
//! ## Enrolment is per host and explicit
//!
//! There is no "enrol everything". Which machines an outside service may drive is
//! exactly the decision an operator should make one at a time, and a checkbox
//! that silently enrolled every host in `~/.ssh/config` on sign-in would be the
//! kind of default nobody remembers agreeing to.
//!
//! ## Signing in: an address, not a token
//!
//! The operator types their Redstone address and signs in on **Redstone's own
//! login page**, opened in a window. rmux mints host tokens itself from then on,
//! so no credential is ever typed into rmux and none is carried by hand between
//! two apps that can talk to each other perfectly well.
//!
//! That is also what Redstone's desktop specification prescribes — *"do not
//! build a native login screen; do not handle passwords in the shell"* — and the
//! reason it works against **every** deployment: it needs nothing the server has
//! not already got, unlike the device grant below.
//!
//! Two alternatives were rejected. The `password` grant is gated on a
//! `client_secret`, and a secret compiled into a desktop app is a secret
//! published to everyone who downloads it. The device grant (RFC 8628) is the
//! tidier long-term answer and [`redstone_sign_in_start`] implements the client
//! half, but no deployment ships it yet — so it stays available and is not
//! waited on.
//!
//! **Never work around a missing flow by embedding a secret.**

use std::collections::HashMap;

use rmux_transport::{shell_quote, CommandSpec, Target, Tty};
use tauri::Manager;
use serde::{Deserialize, Serialize};

/// Keychain service and slot. The session is a credential for someone's whole
/// Redstone account, so it is held where the Claude account tokens are and never
/// in `localStorage` — which is a shared quota that also holds the session list.
const SERVICE: &str = "ai.betterscale.rmux.redstone";
const SLOT: &str = "session";

/// Where the enrolment lives on a host. Must match `rmux_bridge::enrolment_path`.
const ENROLMENT_PATH: &str = ".rmux/redstone.json";

/// What rmux knows about a Redstone deployment and this operator's session.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// `https://redstone.example`. Stored, never compiled in: Redstone is
    /// self-hosted, and a deployment's hostname baked into a shipped binary is
    /// that deployment's address handed to everyone who downloads rmux.
    pub base_url: String,
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// The session, minus anything secret. What the webview is allowed to see.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// What one host's enrolment looks like to the UI.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    /// Whether `~/.rmux/redstone.json` is present over there.
    pub enrolled: bool,
    /// Whether a `rmux-agent bridge` process is actually running.
    ///
    /// Separate from `enrolled` on purpose: a host whose file is present but
    /// whose bridge died is the failure that otherwise looks like success, and
    /// it is the one an operator needs to be able to see.
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

// ---------------------------------------------------------------------------
// The host-side scripts
//
// Pure functions returning POSIX, so the quoting and the mode bits are testable
// without a host. Every one of these runs through `Target`, so local and remote
// are one code path.
// ---------------------------------------------------------------------------

/// Write the enrolment file on the host.
///
/// The body arrives on **stdin**, never in the command line: it carries a bearer
/// token, and `ps` shows one user's argv to every account on the machine. Same
/// rule, and the same reason, as `rmux-agent setenv` and the Claude account
/// token.
///
/// The mode is set **before** the token is written. A file created world-readable
/// and tightened a moment later is world-readable for that moment, and that is
/// all a loop on the machine needs.
pub fn write_enrolment_script(home: &str) -> String {
    let dir = shell_quote(&format!("{home}/.rmux"));
    let path = shell_quote(&format!("{home}/{ENROLMENT_PATH}"));
    format!(
        "set -e; mkdir -p {dir}; chmod 700 {dir}; \
         umask 077; : > {path}; chmod 600 {path}; cat > {path}"
    )
}

/// Read the enrolment back, or print nothing when the host is not enrolled.
pub fn read_enrolment_script(home: &str) -> String {
    let path = shell_quote(&format!("{home}/{ENROLMENT_PATH}"));
    format!("cat {path} 2>/dev/null || true")
}

/// Remove the enrolment and stop the bridge.
///
/// **The file is deleted, not merely ignored.** A host that has been revoked in
/// Redstone but still holds its token on disk is one restart away from being
/// enrolled again, and nothing in either UI would say so.
pub fn unenrol_script(home: &str, agent: &str) -> String {
    let path = shell_quote(&format!("{home}/{ENROLMENT_PATH}"));
    let agent = shell_quote(agent);
    format!(
        "pkill -f {agent}' bridge' 2>/dev/null || true; \
         rm -f {path}; echo unenrolled"
    )
}

/// Start the bridge if it is not already running, and report which happened.
///
/// **Guarded by a check rather than started unconditionally.** `ensure_agent`
/// runs on every connection to a host, so an unguarded start would leave one
/// bridge process per reconnect — each with its own WebSocket, each answering
/// the same requests, and Redstone seeing one host appear a dozen times.
///
/// `pgrep -f` against the *installed binary's own path*, which carries a content
/// fingerprint, so a rebuilt agent starts its own bridge rather than assuming the
/// previous build's will do — the same reasoning as the daemon socket carrying
/// the build rather than the version.
///
/// `nohup` and a full detach, because this outlives the ssh command that starts
/// it. Without the redirections the connection will not close: ssh waits for the
/// pipes, and rmux would hang on every host it enrolled.
pub fn start_bridge_script(home: &str, agent: &str) -> String {
    let agent_q = shell_quote(agent);
    let log = shell_quote(&format!("{home}/.rmux/bridge.log"));
    format!(
        "if pgrep -f {agent_q}' bridge' >/dev/null 2>&1; then echo running; else \
         nohup {agent_q} bridge >> {log} 2>&1 < /dev/null & \
         echo started; fi"
    )
}

/// Is a bridge running for this build?
pub fn bridge_status_script(agent: &str) -> String {
    let agent = shell_quote(agent);
    format!("pgrep -f {agent}' bridge' >/dev/null 2>&1 && echo running || echo stopped")
}

// ---------------------------------------------------------------------------
// Talking to Redstone
// ---------------------------------------------------------------------------

/// What a deployment offers, asked before rmux shows any control for it.
///
/// The same shape as the Cowork server's `/auth/config`, and for the same
/// reason: which flows a server supports is that server's configuration, not
/// ours, and **a control that cannot work must be absent rather than disabled**.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Whether `/api/v1/rmux/*` exists at all.
    #[serde(default)]
    pub bridge: bool,
    /// Whether the device-authorization flow is available. See the module note:
    /// without it a desktop app cannot sign in without shipping a secret.
    #[serde(default)]
    pub device_flow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    /// The protocol versions the server speaks, so a mismatch is a sentence
    /// rather than a field that silently defaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<u32>,
}

fn store_session(session: &Session) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE, SLOT).map_err(|e| e.to_string())?;
    let raw = serde_json::to_string(session).map_err(|e| e.to_string())?;
    entry.set_password(&raw).map_err(|e| e.to_string())
}

fn load_session() -> Option<Session> {
    let entry = keyring::Entry::new(SERVICE, SLOT).ok()?;
    serde_json::from_str(&entry.get_password().ok()?).ok()
}

fn clear_session() {
    if let Ok(entry) = keyring::Entry::new(SERVICE, SLOT) {
        let _ = entry.delete_credential();
    }
}

#[tauri::command]
pub async fn redstone_session() -> Result<Option<SessionView>, String> {
    Ok(load_session().map(|s| SessionView { base_url: s.base_url, user: s.user }))
}

#[tauri::command]
pub async fn redstone_sign_out() -> Result<(), String> {
    clear_session();
    Ok(())
}

/// What a deployment supports. Asked before any Redstone control is shown.
#[tauri::command]
pub async fn redstone_capabilities(base_url: String) -> Result<Capabilities, String> {
    let base = base_url.trim_end_matches('/');
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/rmux/config"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    // A 404 is the honest answer for a Redstone that predates the bridge, and it
    // is not an error: it means every rmux control for this server stays hidden.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Capabilities::default());
    }
    response.json().await.map_err(|e| e.to_string())
}

/// Enrol `target` with Redstone.
///
/// Four steps, in this order, and the order matters: mint the token, write it to
/// the host, start the bridge, report. Starting the bridge before the file exists
/// would give a process that immediately exits with "not enrolled", and rmux
/// would have to guess how long to wait before believing it.
#[tauri::command]
pub async fn redstone_enrol(
    app: tauri::AppHandle,
    target: crate::terminal::TargetRef,
) -> Result<HostStatus, String> {
    let session = load_session().ok_or(
        "sign in to Redstone first, or enrol with a token pasted from its web UI",
    )?;
    let base = session.base_url.trim_end_matches('/');

    // Ask Redstone for a token belonging to this machine. The hostname is a
    // *label* — every fresh cloud image is `localhost` — so Redstone keys on its
    // own id and rmux keeps whatever it is given.
    let minted: Minted = reqwest::Client::new()
        .post(format!("{base}/api/v1/rmux/hosts"))
        .bearer_auth(&session.access_token)
        .json(&serde_json::json!({
            "label": target.host.clone().unwrap_or_else(|| "this machine".into()),
            "agentVersion": rmux_agent::provision::VERSION,
            "protocol": rmux_bridge::VERSION,
        }))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    install(
        &app,
        &target,
        rmux_bridge::Enrolment {
            endpoint: minted.endpoint.unwrap_or_else(|| default_endpoint(base)),
            token: minted.token,
            host_id: Some(minted.host_id),
            enrolled_by: session.user.clone(),
            enrolled_at: now(),
        },
    )
    .await
}

/// Enrol with a token somebody already minted, pasted by the operator.
///
/// **The answer to "must rmux sign in first?" is no.** Minting is a convenience,
/// not the mechanism: the host only ever needs an endpoint and a token, and
/// whether Redstone handed those to rmux over HTTP or to a person through a web
/// page makes no difference to anything downstream. Keeping this path means the
/// device grant is an upgrade rather than a gate — and it is the path that works
/// on a deployment which has not built §2.3 yet.
///
/// It is also the honest shape for a self-hosted product: an operator who can
/// read a token out of their own admin UI should not be blocked because their
/// deployment has not enabled an OAuth flow.
#[tauri::command]
pub async fn redstone_enrol_with_token(
    app: tauri::AppHandle,
    target: crate::terminal::TargetRef,
    endpoint: String,
    token: String,
    host_id: Option<String>,
) -> Result<HostStatus, String> {
    let endpoint = endpoint.trim().to_owned();
    let token = token.trim().to_owned();

    // Checked here rather than left for the bridge to discover on the host,
    // where the only symptom is a log file nobody is reading. A pasted value is
    // routinely a whole curl command, a JSON blob, or the wrong half of one.
    if token.is_empty() {
        return Err("paste the host token from Redstone".into());
    }
    if !(endpoint.starts_with("wss://") || endpoint.starts_with("ws://")) {
        return Err(format!(
            "the bridge endpoint must be a websocket URL starting ws:// or wss:// — got {endpoint:?}"
        ));
    }
    // A token with whitespace in it is a copy-paste that took the surrounding
    // line. Left alone it becomes an `Authorization` header the server rejects,
    // and the operator is shown a policy close rather than the typo.
    if token.split_whitespace().count() != 1 {
        return Err("that looks like more than just the token — paste only the token itself".into());
    }

    install(
        &app,
        &target,
        rmux_bridge::Enrolment {
            endpoint,
            token,
            host_id,
            enrolled_by: load_session().and_then(|s| s.user),
            enrolled_at: now(),
        },
    )
    .await
}

fn now() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Write an enrolment to a host and start its bridge.
///
/// The half both enrolment paths share, so a minted token and a pasted one
/// cannot drift in how they are delivered — which matters, because this is the
/// part with the mode bits and the stdin rule on it.
async fn install(
    app: &tauri::AppHandle,
    target: &crate::terminal::TargetRef,
    enrolment: rmux_bridge::Enrolment,
) -> Result<HostStatus, String> {
    let claude_store = app.state::<crate::claude::ClaudeStore>();
    let resolved = crate::claude::resolve(&claude_store, target).await?;
    let installed = crate::agent::ensure_agent(app, resolved.as_ref()).await?;
    let home = home_of(resolved.as_ref()).await?;

    // Over stdin. See `write_enrolment_script`.
    let body = serde_json::to_vec(&enrolment).map_err(|e| e.to_string())?;
    let spec = CommandSpec::new("sh")
        .arg("-c")
        .arg(write_enrolment_script(&home))
        .tty(Tty::None);
    resolved.exec_with_input(&spec, &body).await.map_err(|e| e.to_string())?;

    let started = run(resolved.as_ref(), &start_bridge_script(&home, &installed.program)).await?;

    Ok(HostStatus {
        enrolled: true,
        running: started.contains("running") || started.contains("started"),
        host_id: enrolment.host_id,
        endpoint: Some(enrolment.endpoint),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Minted {
    host_id: String,
    token: String,
    /// `wss://…`. Given by the server rather than derived from `base_url`,
    /// because a deployment may terminate WebSockets somewhere else entirely and
    /// guessing at the scheme and path is how an integration breaks on the one
    /// installation that does.
    ///
    /// Optional only so an older deployment that omits it still enrols against
    /// the conventional path, rather than failing on a missing field. A value
    /// the server *did* send always wins.
    #[serde(default)]
    endpoint: Option<String>,
}

/// Whether this host is enrolled, and whether its bridge is actually up.
#[tauri::command]
pub async fn redstone_host_status(
    app: tauri::AppHandle,
    target: crate::terminal::TargetRef,
) -> Result<HostStatus, String> {
    let claude_store = app.state::<crate::claude::ClaudeStore>();
    let resolved = crate::claude::resolve(&claude_store, &target).await?;
    let home = home_of(resolved.as_ref()).await?;

    let raw = run(resolved.as_ref(), &read_enrolment_script(&home)).await?;
    let enrolment: Option<rmux_bridge::Enrolment> = serde_json::from_str(raw.trim()).ok();

    let Some(enrolment) = enrolment else { return Ok(HostStatus::default()) };

    // Only asked once we know the host is enrolled — provisioning the agent to
    // discover that an unenrolled machine has no bridge would upload a megabyte
    // to answer "no".
    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;
    let running = run(resolved.as_ref(), &bridge_status_script(&installed.program))
        .await
        .map(|out| out.contains("running"))
        .unwrap_or(false);

    Ok(HostStatus {
        enrolled: true,
        running,
        host_id: enrolment.host_id,
        endpoint: Some(enrolment.endpoint),
    })
}

/// Stop the bridge and remove the token.
#[tauri::command]
pub async fn redstone_unenrol(
    app: tauri::AppHandle,
    target: crate::terminal::TargetRef,
) -> Result<HostStatus, String> {
    let claude_store = app.state::<crate::claude::ClaudeStore>();
    let resolved = crate::claude::resolve(&claude_store, &target).await?;
    let home = home_of(resolved.as_ref()).await?;
    let installed = crate::agent::ensure_agent(&app, resolved.as_ref()).await?;

    run(resolved.as_ref(), &unenrol_script(&home, &installed.program)).await?;

    // Told to Redstone as well, best-effort. A token that is gone from the host
    // but still live in Redstone's table is a credential nobody can see and
    // nobody can use — litter, and the confusing kind.
    if let Some(session) = load_session() {
        let raw = run(resolved.as_ref(), &read_enrolment_script(&home)).await.unwrap_or_default();
        if let Ok(previous) = serde_json::from_str::<rmux_bridge::Enrolment>(raw.trim())
            && let Some(host_id) = previous.host_id
        {
            let _ = reqwest::Client::new()
                .delete(format!(
                    "{}/api/v1/rmux/hosts/{host_id}",
                    session.base_url.trim_end_matches('/')
                ))
                .bearer_auth(&session.access_token)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await;
        }
    }

    Ok(HostStatus::default())
}

/// Sign in by letting the operator use Redstone's own login page.
///
/// **This is what makes the panel a domain field rather than a token field.**
/// The operator types `redstone.example`, signs in the way they already do, and
/// rmux mints host tokens itself from then on. No credential is ever typed into
/// rmux, and no token is carried by hand.
///
/// It is also exactly what Redstone's own desktop specification prescribes:
/// *"The user logs in through the normal web login form. Do not build a native
/// login screen; do not handle passwords in the shell."* The web app sets an
/// `rs_token` cookie on its origin, and the shell reads it back.
///
/// ## Why this beats the device grant it replaces
///
/// The device flow (§2.3) needs the server to implement a grant type it has not
/// shipped. This needs **nothing from Redstone at all** — it works against any
/// deployment whose web app can be logged into, which is all of them. The device
/// grant stays the tidier long-term answer, and this removes the wait.
///
/// ## Why the login window cannot touch rmux
///
/// It loads a **remote origin**, so it gets no Tauri IPC: a remote domain has to
/// be listed in `dangerousRemoteDomainIpcAccess` to reach any command, and rmux
/// lists none. The window can render Redstone and nothing else. rmux reads one
/// cookie out of it and never injects a line of script.
#[tauri::command]
pub async fn redstone_sign_in(app: tauri::AppHandle, base_url: String) -> Result<SessionView, String> {
    let base = normalise_base(&base_url)?;

    let url: tauri::Url = base.parse().map_err(|e| format!("{base} is not a URL: {e}"))?;

    // Closed first if one is already open, so a second attempt does not fail on
    // the duplicate label — the same reason `open_settings` focuses rather than
    // rebuilds, except here a stale half-finished login is worth discarding.
    if let Some(existing) = app.get_webview_window(LOGIN_LABEL) {
        let _ = existing.close();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }

    let window = tauri::WebviewWindowBuilder::new(
        &app,
        LOGIN_LABEL,
        tauri::WebviewUrl::External(url.clone()),
    )
    .title("Sign in to Redstone")
    .inner_size(920.0, 760.0)
    .min_inner_size(480.0, 520.0)
    .build()
    .map_err(|e| format!("could not open the sign-in window: {e}"))?;

    // Poll for the cookie the web app sets. Polling rather than a navigation
    // hook because the sign-in may take any number of redirects, SSO hops and
    // second factors, and none of those are ours to model — the only thing that
    // reliably marks success is the credential existing.
    let deadline = std::time::Instant::now() + SIGN_IN_TIMEOUT;
    loop {
        if std::time::Instant::now() > deadline {
            let _ = window.close();
            return Err("sign-in timed out".into());
        }
        // The operator closing the window is a cancellation, not a failure.
        if app.get_webview_window(LOGIN_LABEL).is_none() {
            return Err("sign-in cancelled".into());
        }

        if let Some(token) = window
            .cookies_for_url(url.clone())
            .ok()
            .and_then(|cookies| {
                cookies
                    .into_iter()
                    .find(|c| c.name() == TOKEN_COOKIE)
                    .map(|c| c.value().to_owned())
            })
            .filter(|t| !t.is_empty())
        {
            // **Proved before it is believed.** A cookie by that name may be a
            // stale or half-written value, and storing one that does not work
            // means the failure surfaces later, at enrolment, pointing at the
            // wrong thing. `/rmux/hosts` needs authentication and is known to
            // exist, so a 200 is proof without guessing at an identity route.
            let ok = reqwest::Client::new()
                .get(format!("{base}/api/v1/rmux/hosts"))
                .bearer_auth(&token)
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);

            if ok {
                let session = Session {
                    base_url: base.clone(),
                    user: subject_of(&token),
                    access_token: token,
                    refresh_token: None,
                };
                store_session(&session)?;
                let _ = window.close();
                return Ok(SessionView { base_url: session.base_url, user: session.user });
            }
        }

        tokio::time::sleep(SIGN_IN_POLL).await;
    }
}

/// The window that holds Redstone's own login page.
const LOGIN_LABEL: &str = "redstone-signin";
/// The cookie Redstone's web app stores its session in.
const TOKEN_COOKIE: &str = "rs_token";
/// Long enough for an SSO hop and a second factor, short enough that a window
/// left open overnight does not keep a task alive for ever.
const SIGN_IN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const SIGN_IN_POLL: std::time::Duration = std::time::Duration::from_millis(600);

/// Turn what an operator typed into a base URL.
///
/// They will type `redstone.example`, because that is what the field asks for.
/// Requiring a scheme would be a validation error for the most likely input.
fn normalise_base(input: &str) -> Result<String, String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("enter your Redstone address".into());
    }
    // A pasted bridge endpoint is a realistic input here; take the origin from it
    // rather than refusing.
    let trimmed = trimmed
        .strip_suffix("/api/v1/rmux/bridge")
        .unwrap_or(trimmed)
        .trim_end_matches('/');

    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_owned()
    } else if let Some(rest) = trimmed.strip_prefix("wss://").or_else(|| trimmed.strip_prefix("ws://")) {
        // Someone pasting the websocket URL means the right host; https is the
        // only sane reading of `wss`.
        format!("https://{rest}")
    } else {
        // Default to https rather than http. A plain hostname over http would
        // send a session cookie in the clear.
        format!("https://{trimmed}")
    };
    Ok(with_scheme.trim_end_matches('/').to_owned())
}

/// Where the bridge lives on a deployment, when Redstone did not say.
///
/// Only used as a fallback: `POST /rmux/hosts` returns an explicit `endpoint`,
/// and that always wins — a deployment may terminate websockets elsewhere.
fn default_endpoint(base: &str) -> String {
    format!("{}/api/v1/rmux/bridge", base.replacen("http", "ws", 1).trim_end_matches('/'))
}

/// Begin signing in.
///
/// See the module note: this needs the device-authorization flow, which Redstone
/// does not expose yet. It reports that rather than offering the password grant,
/// which would require shipping a `client_secret` inside rmux.
#[tauri::command]
pub async fn redstone_sign_in_start(base_url: String) -> Result<SignIn, String> {
    let caps = redstone_capabilities(base_url.clone()).await?;
    if !caps.device_flow {
        return Err(
            "this Redstone deployment does not offer the device sign-in flow yet \
             (see docs/redstone-bridge.md §2)"
                .to_owned(),
        );
    }

    let base = base_url.trim_end_matches('/');
    reqwest::Client::new()
        .post(format!("{base}/api/v1/oauth2/device/authorize"))
        .form(&[("client_id", "rmux"), ("scope", "openid profile email rmux.hosts")])
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// RFC 8628 §3.2.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SignIn {
    pub device_code: String,
    /// Shown to the operator to type in. Short, because they type it by hand.
    pub user_code: String,
    /// Opened in the **real browser**, never in a webview: this is where they
    /// enter a password, and a password form inside our own window is both a
    /// phishing lesson and a place their password manager will not fill.
    pub verification_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// Poll for the operator having approved the sign-in.
#[tauri::command]
pub async fn redstone_sign_in_poll(
    base_url: String,
    device_code: String,
) -> Result<Option<SessionView>, String> {
    let base = base_url.trim_end_matches('/');
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/oauth2/token"))
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &device_code),
            ("client_id", "rmux"),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;

    // `authorization_pending` is the *expected* answer for as long as the
    // operator is still in the browser, so it is `Ok(None)` rather than an error
    // the UI would have to special-case out of a message string.
    if let Some(error) = body.get("error").and_then(|e| e.as_str()) {
        return match error {
            "authorization_pending" | "slow_down" => Ok(None),
            other => Err(other.to_owned()),
        };
    }

    let session = Session {
        base_url: base.to_owned(),
        access_token: body
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or("no access_token in the response")?
            .to_owned(),
        refresh_token: body.get("refresh_token").and_then(|t| t.as_str()).map(str::to_owned),
        user: body
            .get("id_token")
            .and_then(|t| t.as_str())
            .and_then(subject_of)
            .or_else(|| body.get("username").and_then(|u| u.as_str()).map(str::to_owned)),
    };
    store_session(&session)?;

    Ok(Some(SessionView { base_url: session.base_url, user: session.user }))
}

/// The `preferred_username` out of an unverified `id_token`, for display only.
///
/// **Not verification.** rmux is a public client: it holds no `client_secret`, so
/// it cannot check the HS256 signature that Redstone's `id_token` carries, and
/// pretending otherwise would be worse than not trying. The token is authority
/// for nothing here — every call that matters is made with the access token and
/// authorised by Redstone. This is a label under an avatar.
fn subject_of(id_token: &str) -> Option<String> {
    use base64::Engine;
    let payload = id_token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("preferred_username")
        .or_else(|| claims.get("name"))
        .or_else(|| claims.get("email"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

async fn run(target: &dyn Target, script: &str) -> Result<String, String> {
    let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None);
    let out = target.exec(&spec).await.map_err(|e| e.to_string())?;
    Ok(out.stdout_or_err().unwrap_or_default().to_owned())
}

/// The host's home directory, resolved once.
///
/// **Never interpolate `$HOME` into a script that gets `shell_quote`d** — quoting
/// is what stops it expanding, so it becomes a literal directory called `$HOME`.
/// Resolved here and passed down as an absolute path.
async fn home_of(target: &dyn Target) -> Result<String, String> {
    let out = run(target, &rmux_agent::provision::home_script()).await?;
    let home = out.trim().to_owned();
    if home.is_empty() {
        return Err("could not resolve the home directory on that host".to_owned());
    }
    Ok(home)
}

/// Unused today; kept so the shape is one edit away when refresh lands.
#[allow(dead_code)]
type Refresh = HashMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_enrolment_is_written_before_it_can_be_read_by_anyone_else() {
        // `umask 077` and an explicit `chmod 600` *before* the token is written.
        // A file created world-readable and tightened afterwards is readable for
        // that window, and on a shared dev box that is all a loop needs.
        let script = write_enrolment_script("/home/dev.user");

        assert!(script.contains("chmod 700"), "{script}");
        assert!(script.contains("umask 077"), "{script}");
        let chmod = script.find("chmod 600").expect("no chmod 600");
        let cat = script.find("cat >").expect("no write");
        assert!(chmod < cat, "the mode must be set before the token is written: {script}");
    }

    #[test]
    fn the_token_never_appears_in_the_command_line() {
        // `ps` shows one user's argv to every account on the host. The body goes
        // over stdin; this asserts the script has no placeholder for it at all,
        // so nobody can later "simplify" it into an echo.
        let script = write_enrolment_script("/home/dev.user");
        assert!(!script.contains("echo"), "{script}");
        // Ends with a bare redirect and nothing after it. `shell_quote` leaves a
        // path with no special characters alone, which is correct and is why this
        // does not assert on the quotes — `a_home_with_a_space_is_quoted_everywhere`
        // covers the case that needs them.
        assert!(
            script.trim_end().ends_with("cat > /home/dev.user/.rmux/redstone.json"),
            "{script}"
        );
    }

    #[test]
    fn a_home_with_a_space_is_quoted_everywhere() {
        // Real on macOS, and on any host with a Windows-style account name. An
        // unquoted path here is not a cosmetic bug: the remote login shell
        // re-parses the line, so it is an injection.
        let home = "/home/dev user";
        for script in [
            write_enrolment_script(home),
            read_enrolment_script(home),
            unenrol_script(home, "/home/dev user/.rmux/bin/rmux-agent-0.2.19-ab"),
            start_bridge_script(home, "/home/dev user/.rmux/bin/rmux-agent-0.2.19-ab"),
        ] {
            assert!(
                !script.contains("/home/dev user/.rmux/redstone.json ")
                    && script.contains('\''),
                "unquoted path in: {script}"
            );
        }
    }

    #[test]
    fn starting_the_bridge_twice_starts_one_bridge() {
        // `ensure_agent` runs on *every* connection to a host, so an unguarded
        // start would leave one bridge per reconnect — each with its own socket,
        // each answering the same requests, and Redstone showing one machine a
        // dozen times.
        let script = start_bridge_script("/home/dev.user", "/home/dev.user/.rmux/bin/agent");
        assert!(script.starts_with("if pgrep -f"), "{script}");
        assert!(script.contains("echo running"), "{script}");
        assert!(script.contains("echo started"), "{script}");
    }

    #[test]
    fn the_bridge_is_detached_from_the_ssh_command_that_starts_it() {
        // Without the redirections ssh waits on the pipes and never returns, so
        // rmux would hang on every host it enrolled — and the bridge would die
        // with the connection anyway, which is the one thing it must not do.
        let script = start_bridge_script("/home/dev.user", "/home/dev.user/.rmux/bin/agent");
        assert!(script.contains("nohup"), "{script}");
        assert!(script.contains("< /dev/null"), "{script}");
        assert!(script.contains("2>&1"), "{script}");
        assert!(script.contains(" &"), "not backgrounded: {script}");
    }

    #[test]
    fn the_pgrep_pattern_is_this_builds_binary() {
        // The installed path carries a content fingerprint, so a rebuilt agent
        // must start *its own* bridge rather than assuming the previous build's
        // will serve — the same reasoning as the daemon socket carrying the build
        // rather than the version.
        let a = start_bridge_script("/h", "/h/.rmux/bin/rmux-agent-0.2.19-aaaa");
        let b = start_bridge_script("/h", "/h/.rmux/bin/rmux-agent-0.2.19-bbbb");
        assert_ne!(a, b, "two builds must not share a bridge check");
    }

    #[test]
    fn unenrolling_deletes_the_token_rather_than_ignoring_it() {
        // A revoked host still holding its credential is one restart away from
        // being enrolled again, with nothing in either UI saying so.
        let script = unenrol_script("/home/dev.user", "/home/dev.user/.rmux/bin/agent");
        assert!(script.contains("rm -f"), "{script}");
        assert!(script.contains("pkill"), "{script}");
    }

    #[test]
    fn reading_an_unenrolled_host_is_empty_rather_than_an_error() {
        // Most hosts are not enrolled. A `cat` that fails would surface as an
        // error dialog on every host the operator opens.
        let script = read_enrolment_script("/home/dev.user");
        assert!(script.contains("|| true"), "{script}");
        assert!(script.contains("2>/dev/null"), "{script}");
    }

    /// The two enrolment paths deliver the credential identically.
    ///
    /// A pasted token and a minted one both go through `install`, which is where
    /// the `0600`-before-write rule and the stdin rule live. If the paste path
    /// grew its own delivery, those two rules would have to be got right twice —
    /// and the second copy is the one that ships without them.
    #[test]
    fn both_enrolment_paths_share_one_delivery() {
        // Only the half above `mod tests`, or this counts the literal it is
        // searching for and reports its own text as a second call site.
        let src = include_str!("redstone.rs");
        let code = &src[..src.find("mod tests {").unwrap_or(src.len())];

        // `write_enrolment_script` is called exactly once in the module: from
        // `install`. A second call site means the paths have diverged.
        let calls = code.matches("write_enrolment_script(&home)").count();
        assert_eq!(calls, 1, "the enrolment write is duplicated across paths");
    }

    #[test]
    fn an_address_is_taken_the_way_a_person_types_it() {
        // The field asks for an address, so `redstone.example` is the *likely*
        // input. Refusing it for want of a scheme would be a validation error on
        // the most ordinary thing anyone can enter.
        assert_eq!(normalise_base("redstone.example").unwrap(), "https://redstone.example");
        assert_eq!(normalise_base("  redstone.example/  ").unwrap(), "https://redstone.example");
        assert_eq!(
            normalise_base("https://redstone.example").unwrap(),
            "https://redstone.example"
        );
    }

    #[test]
    fn a_pasted_bridge_url_is_reduced_to_its_origin() {
        // Realistic: the operator has the wss endpoint on their clipboard from
        // the previous flow and pastes it into the address field. Taking the
        // origin beats refusing it — and `wss` can only sensibly mean `https`,
        // never `http`, or the session cookie would travel in the clear.
        for input in [
            "wss://redstone.example/api/v1/rmux/bridge",
            "ws://redstone.example/api/v1/rmux/bridge",
            "https://redstone.example/api/v1/rmux/bridge",
        ] {
            assert_eq!(normalise_base(input).unwrap(), "https://redstone.example", "{input}");
        }
    }

    #[test]
    fn a_bare_host_never_defaults_to_plaintext() {
        // A session cookie over http is a session cookie on the wire.
        assert!(normalise_base("redstone.example").unwrap().starts_with("https://"));
        assert!(normalise_base("10.0.0.5:8080").unwrap().starts_with("https://"));
        // …but an explicit http:// is honoured, because a local dev deployment
        // is a real thing and refusing it would be us overruling the operator.
        assert_eq!(normalise_base("http://localhost:8000").unwrap(), "http://localhost:8000");
    }

    #[test]
    fn an_empty_address_says_what_to_do() {
        assert!(normalise_base("   ").unwrap_err().contains("Redstone address"));
    }

    #[test]
    fn the_bridge_endpoint_is_only_derived_as_a_fallback() {
        // Redstone's own `endpoint` always wins — a deployment may terminate
        // websockets elsewhere. This is what an older one that omits it gets.
        assert_eq!(
            default_endpoint("https://redstone.example"),
            "wss://redstone.example/api/v1/rmux/bridge",
        );
        assert_eq!(
            default_endpoint("http://localhost:8000"),
            "ws://localhost:8000/api/v1/rmux/bridge",
        );
    }

    #[test]
    fn a_pasted_endpoint_must_be_a_websocket_url() {
        // Redstone's own docs show an `https://` base URL right next to the
        // `wss://` bridge endpoint, so pasting the wrong one is the likely
        // accident — and left alone it fails on the host, in a log nobody reads.
        for bad in [
            "https://redstone.example/api/v1/rmux/bridge",
            "redstone.example/api/v1/rmux/bridge",
            "",
        ] {
            assert!(
                !(bad.starts_with("wss://") || bad.starts_with("ws://")),
                "{bad:?} should be rejected",
            );
        }
        assert!("wss://redstone.example/api/v1/rmux/bridge".starts_with("wss://"));
        // `ws://` is allowed on purpose: it is what makes a local stand-in
        // server usable as a first step, exactly as §5.1 of the doc describes.
        assert!("ws://127.0.0.1:8787/bridge".starts_with("ws://"));
    }

    #[test]
    fn a_token_pasted_with_its_surroundings_is_refused() {
        // The realistic paste is a whole curl line or a JSON fragment. Left
        // alone it becomes an `Authorization` header the server rejects, and the
        // operator is shown a policy close instead of their own typo.
        for bad in ["rbt_abc def", "token: rbt_abc", "  rbt_abc  extra\n"] {
            assert_ne!(bad.split_whitespace().count(), 1, "{bad:?} should be refused");
        }
        assert_eq!("  rbt_abc  ".split_whitespace().count(), 1);
    }

    #[test]
    fn a_deployment_without_the_bridge_reports_no_capabilities() {
        // A 404 from an older Redstone means every rmux control for it stays
        // hidden — absent, not disabled.
        let caps = Capabilities::default();
        assert!(!caps.bridge);
        assert!(!caps.device_flow);
    }

    #[test]
    fn a_display_name_is_read_without_claiming_to_verify_it() {
        // rmux is a public client and cannot check the signature. This is a label
        // under an avatar, and nothing is authorised by it.
        use base64::Engine;
        let claims = serde_json::json!({ "preferred_username": "dev.user", "sub": "u-1" });
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims).unwrap());
        assert_eq!(subject_of(&format!("h.{payload}.s")).as_deref(), Some("dev.user"));

        // Garbage must be `None` rather than a panic — it is attacker-shaped
        // input the moment anyone points rmux at a server they do not control.
        assert_eq!(subject_of("not-a-token"), None);
        assert_eq!(subject_of("a.!!!!.c"), None);
        assert_eq!(subject_of(""), None);
    }
}
