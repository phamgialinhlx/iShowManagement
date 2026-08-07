//! Where the session token lives.
//!
//! The old desktop app kept tokens in a JSON file under `userData`, encrypted with
//! Electron's `safeStorage` and falling back to **plain base64** when that was
//! unavailable — which is obfuscation, not storage. rmux uses the OS credential
//! store (Keychain on macOS, Credential Manager on Windows, Secret Service on
//! Linux) via the `keyring` crate, and when that is unavailable it fails rather
//! than silently downgrading. A credential store that quietly stops protecting
//! anything is worse than one that refuses.

use serde::{Deserialize, Serialize};

use crate::lock::Vault;

const SERVICE: &str = "group.yitec.rmux";

/// The name this service had before the rename.
///
/// **A keychain entry is addressed by service name, so renaming it orphans
/// every credential already stored.** Left unhandled, an upgrade signs the
/// operator out of Cowork and loses the session they had — with nothing on
/// screen explaining why, because from the app's side the keychain is simply
/// empty. So reads fall back to the old name and migrate what they find.
///
/// It is removed only after the copy under the new name succeeds. Deleting
/// first would turn a keychain write failure into the permanent loss of a
/// token that cannot be recovered without signing in again.
const LEGACY_SERVICE: &str = "ai.betterscale.rmux";

/// Everything needed to resume a session, minus anything derivable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredentials {
    /// The bearer token. `rcwa_`-prefixed for account login, or a Redstone access
    /// token for org SSO.
    pub token: String,
    /// Present only for org SSO. Account tokens have no refresh — they use a
    /// sliding idle window instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub username: String,
}

impl StoredCredentials {
    /// Whether this session can be renewed without asking the user again.
    pub fn is_refreshable(&self) -> bool {
        self.refresh_token.is_some()
    }
}

/// Keyed by server URL so one machine can hold sessions for several installations
/// (a production Cowork server and a local dev one) without them overwriting each
/// other — the old single-slot config file could not.
fn entry(server_url: &str) -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, server_url)?)
}

pub fn save(server_url: &str, creds: &StoredCredentials) -> anyhow::Result<()> {
    save_vault(server_url, &Vault::Plain(creds.clone()))
}

/// Write a vault — sealed or plain — over whatever is stored.
pub fn save_vault(server_url: &str, vault: &Vault) -> anyhow::Result<()> {
    let json = serde_json::to_string(vault)?;
    entry(server_url)?.set_password(&json)?;
    Ok(())
}

/// Load whatever is stored, sealed or not.
///
/// A corrupt entry is treated as absent rather than as an error: the recovery for
/// both is identical (sign in again), and surfacing a deserialisation failure at
/// the login screen helps nobody.
pub fn load_vault(server_url: &str) -> anyhow::Result<Option<Vault>> {
    let json = match entry(server_url)?.get_password() {
        Ok(json) => json,
        Err(keyring::Error::NoEntry) => match migrate_legacy(server_url) {
            Some(json) => json,
            None => return Ok(None),
        },
        Err(e) => return Err(e.into()),
    };

    match serde_json::from_str(&json) {
        Ok(vault) => Ok(Some(vault)),
        Err(e) => {
            tracing::warn!(error = %e, "discarding unreadable stored credentials");
            Ok(None)
        }
    }
}

/// Move a credential stored under the old service name, and return it.
///
/// Best effort throughout: a machine with nothing under the old name is the
/// ordinary case, and a failure to *delete* the old entry after a successful
/// copy is harmless — the new one is authoritative, and the stale copy is a
/// duplicate rather than a leak of anything the keychain was not already
/// holding.
fn migrate_legacy(server_url: &str) -> Option<String> {
    let legacy = keyring::Entry::new(LEGACY_SERVICE, server_url).ok()?;
    let json = legacy.get_password().ok()?;

    match keyring::Entry::new(SERVICE, server_url).and_then(|e| e.set_password(&json)) {
        Ok(()) => {
            tracing::info!(server = %server_url, "migrated stored credentials to the new keychain service");
            let _ = legacy.delete_credential();
        }
        // Returned anyway: being unable to write the new entry is no reason to
        // sign the operator out of a session that is sitting right there.
        Err(e) => tracing::warn!(error = %e, "could not migrate stored credentials; using the old entry"),
    }
    Some(json)
}

/// Load credentials that are usable **right now**, without a PIN.
///
/// A sealed vault answers `None` — not because nothing is stored, but because
/// nothing is readable yet. Callers that need to tell those two apart use
/// [`load_vault`]; conflating them is how a locked session would end up looking
/// like a signed-out one and quietly re-prompting for a full sign-in.
pub fn load(server_url: &str) -> anyhow::Result<Option<StoredCredentials>> {
    Ok(match load_vault(server_url)? {
        Some(Vault::Plain(creds)) => Some(creds),
        Some(Vault::Sealed(_)) | None => None,
    })
}

/// Forget the session. Signing out of an account that was never stored is not an
/// error.
pub fn clear(server_url: &str) -> anyhow::Result<()> {
    match entry(server_url)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// --- device trust -----------------------------------------------------------
//
// Kept under a **separate** key from the session, deliberately. Locking the app
// clears the usable session but must leave the machine paired, because face
// unlock works by minting a fresh session from the device secret plus a live
// match — if the two were stored together, locking would un-pair the device and
// face unlock could only ever work once.

fn device_key(server_url: &str) -> String {
    format!("{server_url}#device")
}

pub fn save_device(trust: &crate::DeviceTrust) -> anyhow::Result<()> {
    let json = serde_json::to_string(trust)?;
    entry(&device_key(&trust.server_url))?.set_password(&json)?;
    Ok(())
}

pub fn load_device(server_url: &str) -> anyhow::Result<Option<crate::DeviceTrust>> {
    match entry(&device_key(server_url))?.get_password() {
        Ok(json) => Ok(serde_json::from_str(&json).ok()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn clear_device(server_url: &str) -> anyhow::Result<()> {
    match entry(&device_key(server_url))?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_sessions_are_not_refreshable_sso_sessions_are() {
        let account = StoredCredentials {
            token: "rcwa_abc".into(),
            refresh_token: None,
            username: "nolan".into(),
        };
        assert!(!account.is_refreshable());

        let sso = StoredCredentials { refresh_token: Some("r_1".into()), ..account };
        assert!(sso.is_refreshable());
    }

    #[test]
    fn credentials_round_trip_through_json() {
        let creds = StoredCredentials {
            token: "rcwa_abc".into(),
            refresh_token: Some("r_1".into()),
            username: "nolan".into(),
        };
        let json = serde_json::to_string(&creds).unwrap();
        assert_eq!(serde_json::from_str::<StoredCredentials>(&json).unwrap(), creds);
    }

    #[test]
    fn a_missing_refresh_token_deserialises_rather_than_failing() {
        // Account-mode entries are written without the field at all.
        let creds: StoredCredentials =
            serde_json::from_str(r#"{"token":"rcwa_x","username":"nolan"}"#).unwrap();
        assert_eq!(creds.refresh_token, None);
    }
}
