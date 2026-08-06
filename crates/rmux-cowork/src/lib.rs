//! Client for the Cowork server.
//!
//! rmux talks to this server for exactly six things: authentication, the shared
//! server registry, messaging, token-usage reporting, the leaderboard and team
//! targets. **Nothing on the remote-coding path goes through it** — terminals,
//! files, metrics and Claude sessions are direct SSH. That separation is the
//! entire point of the rewrite.
//!
//! Two properties of this API are easy to get wrong and were wrong before:
//!
//! 1. **Account tokens (`rcwa_`) cannot be refreshed.** They use a sliding idle
//!    window — every authenticated request extends it, default seven days. A 401
//!    means the window lapsed and the user must sign in again; there is nothing
//!    to retry.
//! 2. **SSO refresh tokens rotate on use.** The old refresh token is dead the
//!    instant a new pair is issued, so two concurrent refreshes race and the
//!    loser's token is permanently invalid — auth wedges until stored state is
//!    cleared by hand. [`Session::authorization`] therefore single-flights
//!    refresh through a mutex.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

pub mod credentials;
pub mod face;
pub mod lock;

pub use credentials::StoredCredentials;
pub use face::DeviceTrust;
pub use lock::{SealedVault, Vault, VaultKey};

/// What the login screen should offer, from `GET /auth/config`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthConfig {
    /// Org SSO is configured.
    #[serde(default)]
    pub redstone: bool,
    #[serde(default)]
    pub issuer: Option<String>,
    /// At least one employee account exists, so password login is meaningful.
    #[serde(default)]
    pub accounts: bool,
    #[serde(default)]
    pub jira: bool,
    #[serde(default, rename = "orgName")]
    pub org_name: Option<String>,
}

/// An account, as the server describes it. Only the fields rmux renders.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub photo: Option<String>,
    #[serde(default)]
    pub division: String,
    /// Whether a quick-unlock PIN is set on the server. Only `/accounts/me`
    /// reports these three; they are absent from every other account payload,
    /// hence the defaults.
    #[serde(default)]
    pub has_pin: bool,
    #[serde(default)]
    pub has_face: bool,
    #[serde(default)]
    pub face_count: u32,
}

impl Account {
    /// What to show in the UI. Falls back to the username when no display name is set.
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() { &self.username } else { &self.display_name }
    }
}

/// `POST /auth/account/login` response.
/// What `POST /auth/jira/start` returns.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraStart {
    #[serde(default)]
    pub ok: bool,
    /// Open this in the operator's browser.
    #[serde(default)]
    pub auth_url: String,
    /// Poll on this.
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Deserialize)]
struct JiraPoll {
    status: String,
    #[serde(default)]
    session: Option<AccountLoginResponse>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct AccountLoginResponse {
    token: String,
    account: Account,
}

/// `POST /auth/redstone/{login,refresh}` response.
#[derive(Debug, Deserialize)]
struct RedstoneTokens {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
}

/// Why a request failed, in the terms the UI needs to react.
#[derive(Debug, thiserror::Error)]
pub enum CoworkError {
    /// Credentials were rejected. Not retryable — the user must sign in.
    #[error("{0}")]
    Unauthorized(String),
    /// The session lapsed and cannot be renewed automatically.
    #[error("session expired — sign in again")]
    SessionExpired,
    #[error("cowork server unreachable: {0}")]
    Transport(String),
    #[error("cowork server error ({status}): {message}")]
    Server { status: u16, message: String },
}

impl CoworkError {
    /// Whether the UI should drop to the login screen rather than showing an error.
    pub fn requires_signin(&self) -> bool {
        matches!(self, CoworkError::Unauthorized(_) | CoworkError::SessionExpired)
    }
}

/// The server's RFC 6749-shaped error body.
#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// An authenticated session against one Cowork server.
#[derive(Debug)]
pub struct Session {
    http: reqwest::Client,
    base_url: String,
    auth: Mutex<AuthState>,
}

#[derive(Debug, Clone)]
struct AuthState {
    token: String,
    refresh_token: Option<String>,
    username: String,
    /// When the access token stops being valid, for SSO sessions. `None` for
    /// account tokens, whose lifetime is a server-side sliding window we cannot
    /// observe.
    expires_at: Option<Instant>,
}

/// Refresh this far ahead of expiry so an in-flight request never straddles it.
const REFRESH_SKEW: Duration = Duration::from_secs(60);

impl Session {
    /// Build a client for `base_url` from stored credentials.
    pub fn resume(base_url: impl Into<String>, creds: StoredCredentials) -> anyhow::Result<Self> {
        Ok(Self {
            http: build_http_client()?,
            base_url: normalize_base_url(base_url.into()),
            auth: Mutex::new(AuthState {
                token: creds.token,
                refresh_token: creds.refresh_token,
                username: creds.username,
                // Unknown until the first refresh; see `needs_refresh`.
                expires_at: None,
            }),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Snapshot the current credentials for persisting.
    pub async fn credentials(&self) -> StoredCredentials {
        let auth = self.auth.lock().await;
        StoredCredentials {
            token: auth.token.clone(),
            refresh_token: auth.refresh_token.clone(),
            username: auth.username.clone(),
        }
    }

    /// Probe which sign-in methods a server offers. Unauthenticated.
    pub async fn auth_config(base_url: &str) -> Result<AuthConfig, CoworkError> {
        let http = build_http_client().map_err(|e| CoworkError::Transport(e.to_string()))?;
        let url = format!("{}/auth/config", normalize_base_url(base_url.to_owned()));
        let res = http.get(&url).send().await.map_err(transport)?;
        parse_json(res).await
    }

    /// Sign in with an employee account.
    pub async fn login_account(
        base_url: impl Into<String>,
        username: &str,
        password: &str,
        device: &str,
    ) -> Result<(Self, Account), CoworkError> {
        let base_url = normalize_base_url(base_url.into());
        let http = build_http_client().map_err(|e| CoworkError::Transport(e.to_string()))?;

        let res = http
            .post(format!("{base_url}/auth/account/login"))
            .json(&serde_json::json!({
                "username": username,
                "password": password,
                "device": device,
            }))
            .send()
            .await
            .map_err(transport)?;

        let body: AccountLoginResponse = parse_json(res).await?;

        let session = Self {
            http,
            base_url,
            auth: Mutex::new(AuthState {
                token: body.token,
                // Account tokens have no refresh; the server slides an idle window.
                refresh_token: None,
                username: body.account.username.clone(),
                expires_at: None,
            }),
        };
        Ok((session, body.account))
    }

    /// Begin a Jira sign-in.
    ///
    /// The desktop half of the server's OAuth flow: it hands back a URL to open
    /// in the operator's **own browser** and a `state` to poll on. rmux never
    /// sees the Jira password, and the browser is the real one — so an existing
    /// Jira session, a password manager and SSO all work, which an embedded
    /// webview would break.
    pub async fn jira_start(base_url: &str) -> Result<JiraStart, CoworkError> {
        let base_url = normalize_base_url(base_url.to_owned());
        let http = build_http_client().map_err(|e| CoworkError::Transport(e.to_string()))?;

        let res = http
            .post(format!("{base_url}/auth/jira/start"))
            // An empty body, deliberately: sending `redirectTo` would put the
            // server into its *web* flow, where the callback redirects instead of
            // leaving the outcome for this client to drain.
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(transport)?;

        let start: JiraStart = parse_json(res).await?;
        if !start.ok {
            return Err(CoworkError::Transport(
                "the server has no Jira sign-in configured".to_owned(),
            ));
        }
        Ok(start)
    }

    /// Drain the outcome of a Jira sign-in, if it has completed.
    ///
    /// `Ok(None)` means "still waiting", which is the ordinary answer until the
    /// operator finishes in the browser. The outcome is **drained** — the server
    /// deletes it on read — so a successful poll must be acted on, never retried.
    pub async fn jira_poll(base_url: &str, state: &str) -> Result<Option<(Self, Account)>, CoworkError> {
        let base_url = normalize_base_url(base_url.to_owned());
        let http = build_http_client().map_err(|e| CoworkError::Transport(e.to_string()))?;

        let res = http
            .get(format!("{base_url}/auth/jira/poll"))
            .query(&[("state", state)])
            .send()
            .await
            .map_err(transport)?;

        let outcome: JiraPoll = parse_json(res).await?;

        match outcome.status.as_str() {
            "pending" => Ok(None),
            "ok" => {
                let session = outcome.session.ok_or_else(|| {
                    CoworkError::Transport("the server reported success with no session".to_owned())
                })?;
                let account = session.account.clone();
                Ok(Some((
                    Self {
                        http,
                        base_url,
                        auth: Mutex::new(AuthState {
                            token: session.token,
                            // Like an account login: no refresh, a sliding window.
                            refresh_token: None,
                            username: account.username.clone(),
                            expires_at: None,
                        }),
                    },
                    account,
                )))
            }
            _ => Err(CoworkError::Transport(
                outcome.error.unwrap_or_else(|| "Jira sign-in failed".to_owned()),
            )),
        }
    }

    /// Sign in through org SSO.
    pub async fn login_sso(
        base_url: impl Into<String>,
        username: &str,
        password: &str,
    ) -> Result<Self, CoworkError> {
        let base_url = normalize_base_url(base_url.into());
        let http = build_http_client().map_err(|e| CoworkError::Transport(e.to_string()))?;

        let res = http
            .post(format!("{base_url}/auth/redstone/login"))
            .json(&serde_json::json!({ "username": username, "password": password }))
            .send()
            .await
            .map_err(transport)?;

        let tokens: RedstoneTokens = parse_json(res).await?;

        Ok(Self {
            http,
            base_url,
            auth: Mutex::new(AuthState {
                token: tokens.access_token,
                refresh_token: tokens.refresh_token,
                username: username.to_owned(),
                expires_at: expiry_from(tokens.expires_in),
            }),
        })
    }

    /// The `Authorization` header value to use right now, refreshing first if the
    /// token is close to expiring.
    ///
    /// **Single-flighted.** The lock is held across the whole refresh, so a second
    /// caller arriving mid-refresh blocks and then observes the new token rather
    /// than starting a second refresh with the same rotating token. Losing that
    /// race invalidates the token permanently.
    pub async fn authorization(&self) -> Result<String, CoworkError> {
        // The lock is held across the network call, not just across the check.
        // Releasing it to do the request is the classic form of this bug: every
        // concurrent caller then spends the same rotating token, and all but one
        // are rejected. `tests/single_flight.rs` fails with 32 refreshes if this
        // is rewritten that way.
        let mut auth = self.auth.lock().await;

        if auth.needs_refresh() {
            // A caller queued behind an in-flight refresh reaches this check only
            // after it completes, finds the state fresh, and skips straight through.
            self.refresh_locked(&mut auth).await?;
        }

        Ok(format!("Bearer {}", auth.token))
    }

    /// Force a refresh regardless of expiry — the recovery path after a 401.
    pub async fn refresh(&self) -> Result<(), CoworkError> {
        let mut auth = self.auth.lock().await;
        self.refresh_locked(&mut auth).await
    }

    /// Exchange the rotating refresh token for a new pair. Caller holds the lock.
    async fn refresh_locked(&self, auth: &mut AuthState) -> Result<(), CoworkError> {
        let Some(refresh_token) = auth.refresh_token.clone() else {
            // Account tokens: nothing to exchange, so a lapsed window is terminal.
            return Err(CoworkError::SessionExpired);
        };

        let res = self
            .http
            .post(format!("{}/auth/redstone/refresh", self.base_url))
            .json(&serde_json::json!({ "refresh_token": refresh_token }))
            .send()
            .await
            .map_err(transport)?;

        let tokens: RedstoneTokens = match parse_json(res).await {
            Ok(t) => t,
            // A rejected refresh token cannot be retried — it is spent or revoked.
            Err(CoworkError::Unauthorized(_)) => return Err(CoworkError::SessionExpired),
            Err(e) => return Err(e),
        };

        auth.token = tokens.access_token;
        // Keep the previous token if the server omitted a new one; dropping it
        // would strand the session with no way to renew.
        if tokens.refresh_token.is_some() {
            auth.refresh_token = tokens.refresh_token;
        }
        auth.expires_at = expiry_from(tokens.expires_in);
        Ok(())
    }

    /// GET a path and deserialise the response.
    pub async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, CoworkError> {
        let auth = self.authorization().await?;
        let res = self
            .http
            .get(format!("{}{path}", self.base_url))
            .header(reqwest::header::AUTHORIZATION, auth)
            .send()
            .await
            .map_err(transport)?;
        parse_json(res).await
    }

    /// POST a JSON body and deserialise the response.
    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, CoworkError> {
        let auth = self.authorization().await?;
        let res = self
            .http
            .post(format!("{}{path}", self.base_url))
            .header(reqwest::header::AUTHORIZATION, auth)
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        parse_json(res).await
    }

    /// DELETE a path, discarding the body.
    pub async fn delete(&self, path: &str) -> Result<(), CoworkError> {
        let auth = self.authorization().await?;
        let res = self
            .http
            .delete(format!("{}{path}", self.base_url))
            .header(reqwest::header::AUTHORIZATION, auth)
            .send()
            .await
            .map_err(transport)?;
        let _: serde_json::Value = parse_json(res).await?;
        Ok(())
    }

    /// The signed-in account.
    pub async fn me(&self) -> Result<Account, CoworkError> {
        self.get("/accounts/me").await
    }

    /// Build a session around a freshly minted account token.
    ///
    /// Account tokens have no refresh and no stated expiry — they run on a
    /// sliding idle window that every request extends. Giving one an
    /// `expires_at` here would make [`AuthState::needs_refresh`] try to renew
    /// something that cannot be renewed.
    fn from_account_token(
        http: reqwest::Client,
        base_url: String,
        token: String,
        account: &Account,
    ) -> Self {
        Self {
            http,
            base_url,
            auth: Mutex::new(AuthState {
                token,
                refresh_token: None,
                username: account.username.clone(),
                expires_at: None,
            }),
        }
    }
}

impl AuthState {
    fn needs_refresh(&self) -> bool {
        match (self.refresh_token.as_ref(), self.expires_at) {
            // Nothing to refresh with — an account token.
            (None, _) => false,
            // Refreshable but expiry unknown (a resumed session): leave it until a
            // 401 proves it stale, rather than burning a rotating token on a guess.
            (Some(_), None) => false,
            (Some(_), Some(at)) => Instant::now() + REFRESH_SKEW >= at,
        }
    }
}

fn expiry_from(expires_in: u64) -> Option<Instant> {
    (expires_in > 0).then(|| Instant::now() + Duration::from_secs(expires_in))
}

fn build_http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("rmux/", env!("CARGO_PKG_VERSION")))
        // Bounded so a hung server surfaces as an error instead of a spinner that
        // never resolves.
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?)
}

fn transport(e: reqwest::Error) -> CoworkError {
    CoworkError::Transport(e.to_string())
}

/// Trailing slashes are stripped so `{base}/auth/config` never becomes a
/// double-slashed path — some proxies in front of this server 404 on those.
fn normalize_base_url(mut url: String) -> String {
    while url.ends_with('/') {
        url.pop();
    }
    url
}

async fn parse_json<T: serde::de::DeserializeOwned>(
    res: reqwest::Response,
) -> Result<T, CoworkError> {
    let status = res.status();
    let body = res.text().await.map_err(transport)?;

    if status.is_success() {
        return serde_json::from_str(&body).map_err(|e| CoworkError::Server {
            status: status.as_u16(),
            message: format!("unexpected response shape: {e}"),
        });
    }

    // The server answers with `{error, error_description}`; fall back to the raw
    // body when something upstream (a proxy, a tunnel) answers instead.
    let message = serde_json::from_str::<ApiError>(&body)
        .map(|e| if e.error_description.is_empty() { e.error } else { e.error_description })
        .unwrap_or_else(|_| body.chars().take(200).collect());

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CoworkError::Unauthorized(message));
    }
    Err(CoworkError::Server { status: status.as_u16(), message })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(refresh: Option<&str>, expires_in: u64) -> AuthState {
        AuthState {
            token: "t".into(),
            refresh_token: refresh.map(str::to_owned),
            username: "nolan".into(),
            expires_at: expiry_from(expires_in),
        }
    }

    #[test]
    fn account_tokens_are_never_refreshed() {
        // There is no refresh endpoint for `rcwa_` tokens; attempting one would
        // fail forever instead of prompting a sign-in.
        assert!(!state(None, 0).needs_refresh());
        assert!(!state(None, 3600).needs_refresh());
    }

    #[test]
    fn a_resumed_sso_session_waits_for_a_401_rather_than_guessing() {
        // Expiry is unknown until the first refresh. Refreshing speculatively
        // would spend a rotating token we may not need to spend.
        assert!(!state(Some("r"), 0).needs_refresh());
    }

    #[test]
    fn refresh_happens_before_expiry_not_after() {
        // Inside the skew window: refresh now, so no request straddles expiry.
        assert!(state(Some("r"), 30).needs_refresh());
        // Comfortably valid.
        assert!(!state(Some("r"), 3600).needs_refresh());
    }

    #[test]
    fn base_urls_are_normalised_so_paths_never_double_slash() {
        assert_eq!(
            normalize_base_url("https://cowork.example.com/".into()),
            "https://cowork.example.com"
        );
        assert_eq!(
            normalize_base_url("https://cowork.example.com///".into()),
            "https://cowork.example.com"
        );
        assert_eq!(
            normalize_base_url("https://cowork.example.com".into()),
            "https://cowork.example.com"
        );
    }

    #[test]
    fn errors_tell_the_ui_whether_to_show_the_login_screen() {
        assert!(CoworkError::Unauthorized("bad".into()).requires_signin());
        assert!(CoworkError::SessionExpired.requires_signin());
        // A server that is down is not a credentials problem — signing out here
        // would lose a still-valid session over a transient blip.
        assert!(!CoworkError::Transport("refused".into()).requires_signin());
        assert!(!CoworkError::Server { status: 500, message: "boom".into() }.requires_signin());
    }

    #[test]
    fn account_display_falls_back_to_username() {
        let mut a = Account {
            id: "1".into(),
            username: "nolan".into(),
            display_name: String::new(),
            role: "member".into(),
            photo: None,
            division: String::new(),
            has_pin: false,
            has_face: false,
            face_count: 0,
        };
        assert_eq!(a.label(), "nolan");
        a.display_name = "Nolan Lewis".into();
        assert_eq!(a.label(), "Nolan Lewis");
    }

    #[test]
    fn auth_config_tolerates_a_minimal_server_response() {
        // Older/personal-mode installations omit most of these fields.
        let cfg: AuthConfig = serde_json::from_str(r#"{"redstone":false}"#).unwrap();
        assert!(!cfg.redstone);
        assert!(!cfg.accounts);
        assert_eq!(cfg.org_name, None);
    }
}

/// A configured Jira connection on the server.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraProfile {
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// A project inside one of those.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraProject {
    pub key: String,
    #[serde(default)]
    pub name: String,
}

impl Session {
    /// Jira connections this server has configured.
    ///
    /// **Profile-level, deliberately.** The server's issue endpoints hang off
    /// `/sessions/:id/...` and 404 without a row in its own `sessions` table —
    /// the table this architecture removed. These two are session-independent,
    /// so they work as-is.
    pub async fn jira_profiles(&self) -> Result<Vec<JiraProfile>, CoworkError> {
        self.get("/jira/profiles").await
    }

    pub async fn jira_projects(&self, profile: &str) -> Result<Vec<JiraProject>, CoworkError> {
        // The name is a path segment and may contain spaces.
        let encoded = profile.replace(' ', "%20");
        self.get(&format!("/jira/profiles/{encoded}/projects")).await
    }

    /// The signed-in account's assigned Jira issues.
    ///
    /// `/agency/missions`, which is where the deployed server actually keeps a
    /// session-independent view of Jira. An earlier version of this client
    /// invented profile-level issue routes and reported the resulting 404 as
    /// "your server has no endpoint for this" — it does, under a name I had not
    /// looked for. Everything here is a route that exists today.
    ///
    /// Scoped to the operator rather than to a project, because that is what
    /// the server offers and it is also the more useful default: what a session
    /// wants on screen is *my* work, not the whole board.
    pub async fn jira_missions(&self) -> Result<Vec<JiraIssue>, CoworkError> {
        self.get("/agency/missions").await
    }

    /// Create a task: assigned to the signed-in operator, in the active sprint.
    ///
    /// The two things that make it a *quick* add are decided server-side, and
    /// deliberately not passed from here: the assignee is the calling account's
    /// own Jira user (so the task appears in the missions list it was created
    /// from), and the sprint is discovered from the project's board. Sending
    /// either from the client would mean the app deciding who a ticket belongs
    /// to, which is the server's fact, not ours.
    pub async fn jira_create(&self, project: &str, summary: &str) -> Result<JiraIssue, CoworkError> {
        self.post_json(
            "/agency/missions",
            &serde_json::json!({ "projectKey": project, "summary": summary }),
        )
        .await
    }

    /// One issue in full, including its description and comments.
    pub async fn jira_mission(&self, key: &str) -> Result<JiraIssueDetail, CoworkError> {
        self.get(&format!("/agency/missions/{}", encode_segment(key))).await
    }

    /// The moves this issue's workflow currently permits.
    ///
    /// Asked per issue, never assumed. A Jira workflow decides which moves are
    /// legal *from the current status*, and that differs by project and issue
    /// type — a fixed list of statuses would offer moves the server rejects.
    pub async fn jira_transitions(&self, key: &str) -> Result<Vec<JiraTransition>, CoworkError> {
        self.get(&format!("/agency/missions/{}/transitions", encode_segment(key))).await
    }

    pub async fn jira_transition(&self, key: &str, transition: &str) -> Result<(), CoworkError> {
        let _: serde_json::Value = self
            .post_json(
                &format!("/agency/missions/{}/transitions", encode_segment(key)),
                &serde_json::json!({ "transitionId": transition }),
            )
            .await?;
        Ok(())
    }

    /// Add a comment.
    ///
    /// **Comment rather than edit**, and that is the server's shape rather than
    /// a preference: editing an issue's description is only exposed under
    /// `PUT /sessions/:id/jira/issues/:key`, which needs a row in the server's
    /// own sessions table. Commenting says the same thing without rewriting
    /// what someone else wrote, and it is what the deployed API allows.
    pub async fn jira_comment(&self, key: &str, body: &str) -> Result<(), CoworkError> {
        let _: serde_json::Value = self
            .post_json(
                &format!("/agency/missions/{}/comment", encode_segment(key)),
                &serde_json::json!({ "body": body }),
            )
            .await?;
        Ok(())
    }
}

/// A move the workflow currently permits.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraTransition {
    pub id: String,
    pub name: String,
    /// The status this lands the issue in.
    #[serde(default)]
    pub to: Option<String>,
}

/// An issue with its long-form fields.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraIssueDetail {
    #[serde(flatten)]
    pub issue: JiraIssue,
    /// Jira renders its own markup server-side; this is HTML.
    #[serde(default)]
    pub description_html: String,
    /// The raw wiki-markup source, for anyone who would rather read that.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub issue_type: String,
    #[serde(default)]
    pub comments: Vec<JiraComment>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraComment {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub body_html: String,
}

/// Percent-encode one path segment.
///
/// Issue keys are `PROJ-123` in practice, but this is user data reaching a URL
/// and a key that ever contained a slash would otherwise change which route was
/// called.
fn encode_segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}


/// One issue, as much of it as anything here needs.
///
/// Deliberately shallow. Jira's issue document is enormous and versioned by
/// someone else; binding to more of it than is displayed would turn a field
/// rename into "the board is empty".
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraIssue {
    pub key: String,
    #[serde(default)]
    pub summary: String,
    /// The status name as Jira reports it — free text, because a Jira admin can
    /// rename or add statuses at will.
    #[serde(default)]
    pub status: String,
    /// Jira's own three-bucket categorisation: `new`, `indeterminate`, `done`.
    /// The only status field that is safe to reason about, since the *names*
    /// are per-project and the categories are not.
    #[serde(default)]
    pub status_category: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
}
