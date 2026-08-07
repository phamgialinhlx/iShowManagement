//! The Claude account rmux signs sessions in with.
//!
//! One login, used on every host. `claude setup-token` produces a long-lived
//! OAuth token; rmux keeps it in the OS keychain and hands it to each host's
//! agent, so a new server is usable immediately instead of needing its own
//! browser login.
//!
//! **The token is never an argument and never written to a remote disk.** `ps`
//! shows one user's command line to every account on a machine, and a credential
//! file outlives the session that needed it. It goes over the agent's `0600`
//! socket into the daemon's memory, via `rmux-agent setenv`, which reads it from
//! **stdin**.

use rmux_claude::auth;
use rmux_transport::{CommandSpec, Target, Tty};
use serde::Serialize;
use tauri::State;

use crate::claude::ClaudeStore;
use crate::terminal::TargetRef;

/// Keychain slot. Distinct from the Cowork session entry — different credential,
/// different lifetime, and revoking one must not disturb the other.
const SERVICE: &str = "group.yitec.rmux.claude";
/// The credential sessions run with — an OAuth token or a Console API key.
const ACCOUNT: &str = "oauth-token";
/// The admin key that reads the organisation's usage report.
///
/// A separate slot on purpose: it is far more powerful than the run credential
/// and must never be sent to a host, so it is never even loaded on that path.
const ADMIN_ACCOUNT: &str = "admin-key";

/// What the UI is allowed to know about the stored account.
///
/// Never the token itself: the webview renders untrusted content, and a
/// credential that reaches it is one XSS away from leaving the machine.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub connected: bool,
    /// Last four characters, enough to tell two accounts apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// `oauthToken` | `apiKey`, so the UI can say which kind is in use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Whether an admin key is stored, which is what makes usage readable.
    pub usage_available: bool,
}

fn entry(slot: &str) -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, slot)?)
}

fn stored(slot: &str) -> Option<String> {
    // Falls back to the pre-rename service name, so an upgrade does not lose
    // the account the operator already signed in with. See `keychain`.
    crate::keychain::read_migrating(SERVICE, crate::keychain::LEGACY_CLAUDE_SERVICE, slot)
        .filter(|t| !t.is_empty())
}

fn stored_token() -> Option<String> {
    stored(ACCOUNT)
}

fn status_now() -> AccountStatus {
    let usage_available = stored(ADMIN_ACCOUNT).is_some();
    match stored_token() {
        Some(token) => AccountStatus {
            connected: true,
            hint: Some(auth::redact(&token)),
            kind: auth::CredentialKind::detect(&token).map(|k| {
                match k {
                    auth::CredentialKind::OauthToken => "oauthToken",
                    auth::CredentialKind::ApiKey => "apiKey",
                    auth::CredentialKind::AdminKey => "adminKey",
                }
                .to_owned()
            }),
            usage_available,
        },
        None => AccountStatus { connected: false, hint: None, kind: None, usage_available },
    }
}

/// Is an account stored, and which one.
#[tauri::command]
pub async fn claude_account_status() -> Result<AccountStatus, String> {
    Ok(status_now())
}

/// Store a token captured from the login flow.
#[tauri::command]
pub async fn claude_account_save(token: String) -> Result<AccountStatus, String> {
    let token = token.trim().to_owned();
    if token.is_empty() {
        return Err("no credential was provided".to_owned());
    }

    let kind = auth::CredentialKind::detect(&token).ok_or_else(|| {
        "unrecognised credential — expected sk-ant-oat…, sk-ant-api… or sk-ant-admin…".to_owned()
    })?;

    // An admin key is filed separately and never runs a session. Classifying by
    // prefix rather than asking means the dangerous one cannot be put in the
    // slot that gets shipped to hosts.
    let slot = if kind == auth::CredentialKind::AdminKey { ADMIN_ACCOUNT } else { ACCOUNT };

    entry(slot)
        .and_then(|e| Ok(e.set_password(&token)?))
        .map_err(|e| format!("could not store the credential: {e}"))?;

    Ok(status_now())
}

/// Forget the stored account.
#[tauri::command]
pub async fn claude_account_forget() -> Result<AccountStatus, String> {
    for slot in [ACCOUNT, ADMIN_ACCOUNT] {
        if let Ok(entry) = entry(slot) {
            // Already absent is the outcome being asked for, not an error.
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(format!("could not remove the credential: {e}")),
            }
        }
    }
    Ok(status_now())
}

/// The organisation's real usage, from the Console Admin API.
///
/// On demand, never polled: it is a billing API, and this is a number that
/// changes on the scale of hours.
#[tauri::command]
pub async fn claude_usage_report(days: Option<u32>) -> Result<rmux_claude::usage::UsageReport, String> {
    let admin_key = stored(ADMIN_ACCOUNT)
        .ok_or_else(|| "add an admin key (sk-ant-admin…) to read organisation usage".to_owned())?;

    let days = days.unwrap_or(7).clamp(1, 31);
    let starting_at = (std::time::SystemTime::now()
        - std::time::Duration::from_secs(u64::from(days) * 24 * 60 * 60))
    .duration_since(std::time::UNIX_EPOCH)
    .map_err(|e| e.to_string())?
    .as_secs();

    // RFC 3339, which is what the API takes. Formatted by hand rather than
    // pulling in a date crate for one timestamp.
    let starting_at = rfc3339(starting_at);

    rmux_claude::usage::fetch(&admin_key, days, &starting_at).await.map_err(|e| e.to_string())
}

/// Unix seconds → `YYYY-MM-DDT00:00:00Z`.
///
/// Snapped to the start of the day, which is the bucket the API uses anyway, so
/// two calls within a day ask exactly the same question and stay cacheable.
fn rfc3339(unix_seconds: u64) -> String {
    let days_since_epoch = unix_seconds / 86_400;

    // Civil-from-days. Fixed arithmetic, no crate, no drift.
    let z = days_since_epoch as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}

/// Push the stored account to a target's agent, if there is one.
///
/// Called before a Claude session starts. Silent when no account is stored —
/// that is the ordinary case for someone already logged in on the host, and
/// failing there would break a setup that works.
pub async fn apply_to_target<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    target: &dyn Target,
) -> Result<(), String> {
    let Some(token) = stored_token() else {
        return Ok(());
    };

    // Classified by prefix, so the variable matches the credential. An admin key
    // has no variable at all and is refused outright — it administers the
    // organisation and has no business on a host that merely runs models.
    let Some(kind) = auth::CredentialKind::detect(&token) else {
        return Ok(());
    };
    let Some(var) = kind.env_var() else {
        return Err("an admin key cannot be used to run sessions".to_owned());
    };

    let installed = crate::agent::ensure_agent(app, target).await?;
    let spec = CommandSpec::new(&installed.program).arg("setenv").tty(Tty::None);

    // Over **stdin**. As an argument it would be in this host's `ps` output for
    // every user on it.
    let line = format!("{var}={token}\n");
    target
        .exec_with_input(&spec, line.as_bytes())
        .await
        .map_err(|e| format!("could not hand the Claude account to the agent: {e}"))?;

    Ok(())
}

/// Run `claude setup-token` on a target and return its output for parsing.
///
/// The flow is interactive — it prints a URL, waits for the browser round trip,
/// then prints the token — so this is driven from the UI as a terminal rather
/// than captured in one shot. This command exists to *start* it; the UI watches
/// the stream for the URL and the token using `rmux_claude::auth`.
#[tauri::command]
pub async fn claude_login_command(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
) -> Result<String, String> {
    // Resolving proves the target is reachable before the UI opens a login view
    // that could only fail.
    let _ = crate::claude::resolve(store.inner(), &target).await?;
    Ok(auth::setup_token_command())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_formatted_without_a_calendar_crate() {
        // Hand-rolled civil-from-days arithmetic, so it is pinned against known
        // values — including both leap-year rules, which is where this kind of
        // code goes wrong.
        for (unix, want) in [
            (0u64, "1970-01-01T00:00:00Z"),
            (1_767_225_600, "2026-01-01T00:00:00Z"),
            (1_785_542_400, "2026-08-01T00:00:00Z"),
            // 2000 is a leap year (divisible by 400); 1900 is not.
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_709_164_800, "2024-02-29T00:00:00Z"),
            (1_614_556_800, "2021-03-01T00:00:00Z"),
        ] {
            assert_eq!(rfc3339(unix), want, "for {unix}");
        }
    }

    #[test]
    fn a_timestamp_mid_day_snaps_to_the_start_of_that_day() {
        // The API buckets by day, so two calls hours apart must ask the same
        // question — otherwise nothing downstream can cache.
        assert_eq!(rfc3339(1_785_542_400 + 3600 * 13), "2026-08-01T00:00:00Z");
    }
}
