//! Auth IPC — sign in, resume, sign out.
//!
//! The session lives in Rust and the token never crosses into the webview. The UI
//! gets an account and a signed-in flag; it has no way to read or leak the bearer.

use rmux_cowork::{Account, AuthConfig, CoworkError, Session, StoredCredentials, Vault, VaultKey, credentials};
use serde::Serialize;
use tauri::State;
use tokio::sync::RwLock;

/// Holds the active session, if any.
#[derive(Default)]
pub struct AuthStore {
    pub(crate) session: RwLock<Option<Session>>,
    server_url: RwLock<Option<String>>,
    /// Present exactly when the app was unlocked with a PIN this run.
    ///
    /// Without it, persisting a rotated token would mean either re-prompting for
    /// the PIN mid-session or writing the token back unsealed — and the second
    /// would silently undo the lock, which is the worst of the three because
    /// nothing about it is visible.
    pub(crate) vault_key: RwLock<Option<VaultKey>>,
}

impl AuthStore {
    pub(crate) async fn server_url(&self) -> Option<String> {
        self.server_url.read().await.clone()
    }

    /// Install a session as the active one.
    pub(crate) async fn adopt(&self, session: Session, server_url: &str, key: Option<VaultKey>) {
        *self.session.write().await = Some(session);
        *self.server_url.write().await = Some(server_url.to_owned());
        *self.vault_key.write().await = key;
    }

    /// Persist credentials the way they are currently held — sealed if the app is
    /// locked, plain if it is not.
    ///
    /// Every path that saves a token goes through here. A `credentials::save`
    /// called directly from one of them would write plaintext over a sealed
    /// vault and disable the lock without saying so.
    pub(crate) async fn persist(
        &self,
        server_url: &str,
        creds: &StoredCredentials,
    ) -> Result<(), AuthError> {
        match self.vault_key.read().await.as_ref() {
            Some(key) => {
                // Carry the face flag over rather than defaulting it: dropping it
                // here would turn face unlock off on the next token rotation,
                // which would look like it broke at random.
                let face = matches!(
                    credentials::load_vault(server_url)?,
                    Some(Vault::Sealed(v)) if v.face
                );
                let sealed = key.seal(creds, face).map_err(|e| AuthError::message(e.to_string()))?;
                credentials::save_vault(server_url, &Vault::Sealed(sealed))?;
            }
            None => credentials::save(server_url, creds)?,
        }
        Ok(())
    }
}

/// What the UI needs to render the signed-in state.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedIn {
    pub account: Account,
    pub server_url: String,
}

/// Errors reach the UI as a message plus a flag saying whether to show the login
/// screen. That distinction matters: a 401 means sign in again, but an
/// unreachable server means show an error and keep the session — signing the user
/// out over a transient network blip loses a still-valid session.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthError {
    pub message: String,
    pub requires_signin: bool,
}

impl From<CoworkError> for AuthError {
    fn from(e: CoworkError) -> Self {
        Self { requires_signin: e.requires_signin(), message: e.to_string() }
    }
}

impl AuthError {
    /// A plain message, with no implication that the session is gone.
    pub(crate) fn message(text: impl Into<String>) -> Self {
        Self { message: text.into(), requires_signin: false }
    }
}

impl From<anyhow::Error> for AuthError {
    fn from(e: anyhow::Error) -> Self {
        Self { message: e.to_string(), requires_signin: false }
    }
}

/// Ask a server which sign-in methods it offers, before any credentials exist.
#[tauri::command]
pub async fn auth_config(server_url: String) -> Result<AuthConfig, AuthError> {
    Ok(Session::auth_config(&server_url).await?)
}

/// Sign in with an employee account.
#[tauri::command]
pub async fn sign_in(
    store: State<'_, AuthStore>,
    server_url: String,
    username: String,
    password: String,
) -> Result<SignedIn, AuthError> {
    let device = device_label();
    let (session, account) =
        Session::login_account(&server_url, &username, &password, &device).await?;

    // Persist only after the server accepted the credentials, so a failed attempt
    // never overwrites a working stored session.
    let creds = session.credentials().await;
    store.adopt(session, &server_url, None).await;
    store.persist(&server_url, &creds).await?;

    Ok(SignedIn { account, server_url })
}

/// Open a URL in the operator's real browser.
///
/// An app command rather than a plugin, so no ACL entry is needed — and more
/// importantly it is **restricted to `https`**. A general "open anything" bridge
/// reachable from the webview would let injected content launch a local file or a
/// custom scheme handler.
#[tauri::command]
pub async fn open_external(url: String) -> Result<(), AuthError> {
    if !url.starts_with("https://") {
        return Err(AuthError::message("only https links can be opened"));
    }

    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };

    tokio::process::Command::new(program)
        .arg(&url)
        .spawn()
        .map_err(|e| AuthError::message(format!("could not open a browser: {e}")))?;

    Ok(())
}

/// Begin a Jira sign-in and hand the UI the URL to open.
///
/// Which Jira, and whether it is offered at all, is the **server's** configuration
/// — rmux only asks. That is why the flow starts with a server URL: everything
/// else follows from what that server says it supports.
#[tauri::command]
pub async fn jira_start(server_url: String) -> Result<rmux_cowork::JiraStart, AuthError> {
    Ok(Session::jira_start(&server_url).await?)
}

/// Check whether a Jira sign-in has completed.
///
/// `Ok(None)` means the operator has not finished in the browser yet, which is
/// the answer for as long as they are still typing a password. The server
/// **drains** a completed outcome, so a success is stored here immediately rather
/// than returned and fetched again.
#[tauri::command]
pub async fn jira_poll(
    store: State<'_, AuthStore>,
    server_url: String,
    state: String,
) -> Result<Option<SignedIn>, AuthError> {
    let Some((session, account)) = Session::jira_poll(&server_url, &state).await? else {
        return Ok(None);
    };

    let creds = session.credentials().await;
    store.adopt(session, &server_url, None).await;
    store.persist(&server_url, &creds).await?;

    Ok(Some(SignedIn { account, server_url }))
}

/// Restore a session from the OS keychain at startup.
///
/// Returns `Ok(None)` when there is nothing stored — the ordinary first-run case,
/// not an error. A **sealed** vault also answers `None`: there is a session, but
/// it is not readable until a PIN opens it, and the UI learns that from
/// `lock_status` rather than from here.
#[tauri::command]
pub async fn resume_session(
    store: State<'_, AuthStore>,
    server_url: String,
) -> Result<Option<SignedIn>, AuthError> {
    let Some(creds) = credentials::load(&server_url)? else {
        return Ok(None);
    };

    let session = Session::resume(&server_url, creds)?;

    // Verify against the server rather than trusting the stored token: it may
    // have lapsed or been revoked while the app was closed.
    let account = match session.me().await {
        Ok(account) => account,
        Err(e) if e.requires_signin() => {
            // Dead token — clear it so the next start goes straight to login.
            credentials::clear(&server_url)?;
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };

    // The token may have been rotated by the `me()` call above.
    let creds = session.credentials().await;
    store.adopt(session, &server_url, None).await;
    store.persist(&server_url, &creds).await?;

    Ok(Some(SignedIn { account, server_url }))
}

/// Sign out and forget the stored credentials.
///
/// `server_url` is accepted because sign-out is also the way past a forgotten
/// PIN, and in that state **nothing has been adopted** — the app never unlocked,
/// so the store holds no URL to clear. Relying on the store alone would make
/// "forget this session" appear to work while leaving the sealed vault in the
/// keychain, and the next start would present the same lock screen.
#[tauri::command]
pub async fn sign_out(
    store: State<'_, AuthStore>,
    server_url: Option<String>,
) -> Result<(), AuthError> {
    if let Some(url) = store.server_url.write().await.take().or(server_url) {
        credentials::clear(&url)?;
        // The device secret goes too. Leaving it behind would keep the machine
        // able to mint a session by face for an account that just signed out —
        // a sign-out that does not sign you out.
        credentials::clear_device(&url)?;
    }
    *store.session.write().await = None;
    *store.vault_key.write().await = None;
    Ok(())
}

/// A human-readable label for the login audit, so a user can recognise their own
/// devices in the trail.
pub(crate) fn device_label() -> String {
    let host = hostname().unwrap_or_else(|| "unknown".to_owned());
    format!("rmux on {host} ({})", std::env::consts::OS)
}

fn hostname() -> Option<String> {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_failures_do_not_sign_the_user_out() {
        // Losing a valid session because the server briefly went away is worse
        // than showing an error and retrying.
        let err: AuthError = CoworkError::Transport("connection refused".into()).into();
        assert!(!err.requires_signin);

        let err: AuthError = CoworkError::SessionExpired.into();
        assert!(err.requires_signin);
    }

    #[test]
    fn device_label_identifies_the_app_and_machine() {
        let label = device_label();
        assert!(label.starts_with("rmux on "));
        assert!(label.contains(std::env::consts::OS));
    }
}

/// Jira connections this server has configured.
#[tauri::command]
pub async fn jira_profiles(
    store: State<'_, AuthStore>,
) -> Result<Vec<rmux_cowork::JiraProfile>, AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_profiles().await?)
}

/// Projects inside one of them.
#[tauri::command]
pub async fn jira_projects(
    store: State<'_, AuthStore>,
    profile: String,
) -> Result<Vec<rmux_cowork::JiraProject>, AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_projects(&profile).await?)
}

/// The signed-in account's assigned Jira issues.
///
/// `/agency/missions` — a real, deployed, session-independent route. An earlier
/// version of this file called invented profile-level endpoints and reported
/// their 404 as "your server does not expose issues", which was wrong: the
/// server exposes them under a name I had not looked for.
#[tauri::command]
pub async fn jira_missions(
    store: State<'_, AuthStore>,
) -> Result<Vec<rmux_cowork::JiraIssue>, AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_missions().await?)
}

/// One issue in full — description and comments included.
#[tauri::command]
pub async fn jira_mission(
    store: State<'_, AuthStore>,
    key: String,
) -> Result<rmux_cowork::JiraIssueDetail, AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_mission(&key).await?)
}

/// The moves this issue's workflow currently permits.
#[tauri::command]
pub async fn jira_transitions(
    store: State<'_, AuthStore>,
    key: String,
) -> Result<Vec<rmux_cowork::JiraTransition>, AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_transitions(&key).await?)
}

#[tauri::command]
pub async fn jira_transition(
    store: State<'_, AuthStore>,
    key: String,
    transition: String,
) -> Result<(), AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_transition(&key, &transition).await?)
}

#[tauri::command]
pub async fn jira_comment(
    store: State<'_, AuthStore>,
    key: String,
    body: String,
) -> Result<(), AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.jira_comment(&key, &body).await?)
}
