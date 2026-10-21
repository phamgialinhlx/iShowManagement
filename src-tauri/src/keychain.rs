//! The OS keychain, as `rmux_app`'s [`SecretStore`](rmux_app::SecretStore).
//!
//! The legacy-service-name migration that used to live here moved to
//! `rmux_app::secrets::read_migrating`, beside the Claude account and model
//! profiles that are its only callers; it is expressed over `SecretStore`, so it
//! still runs against this keychain on the desktop exactly as before.

/// The OS keychain as `rmux_app`'s [`SecretStore`](rmux_app::SecretStore).
///
/// A plain `(service, account)` mapping onto `keyring::Entry`.
pub struct KeyringSecrets;

impl rmux_app::SecretStore for KeyringSecrets {
    fn get(&self, service: &str, account: &str) -> anyhow::Result<Option<String>> {
        match keyring::Entry::new(service, account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> anyhow::Result<()> {
        Ok(keyring::Entry::new(service, account)?.set_password(value)?)
    }

    fn delete(&self, service: &str, account: &str) -> anyhow::Result<()> {
        match keyring::Entry::new(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
