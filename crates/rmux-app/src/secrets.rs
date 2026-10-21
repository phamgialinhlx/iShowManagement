//! Reading a credential that may still be under the old service name.
//!
//! Moved from `src-tauri/src/keychain.rs` and expressed over [`SecretStore`] so
//! the Claude account and model profiles can live in this crate.
//!
//! **A keychain entry is addressed by its service name, so renaming the app's
//! identifier orphans every credential already stored.** Unhandled, an upgrade
//! silently loses the operator's Claude account and every model profile — and
//! from the app's side nothing is wrong, the keychain is simply empty, so there
//! is nothing to show them either. The first symptom is a session refusing to
//! start because it names a profile that no longer exists.
//!
//! One helper rather than the same fallback written twice: the two callers hold
//! different things (a token, a set of profiles) but have exactly the same
//! problem, and a migration that works in one place and not the other is worse
//! than none, because the failure is partial and looks like corruption.
//!
//! On a host with no legacy entry (every headless one) the fallback simply
//! finds nothing, so this costs nothing there.

use crate::host::SecretStore;

/// The service prefix these entries used before the rename.
pub const LEGACY_CLAUDE_SERVICE: &str = "ai.betterscale.rmux.claude";

/// Read `slot`, falling back to the legacy service and moving what it finds.
///
/// The old entry is deleted only after the copy succeeds. Deleting first would
/// turn a failed write into the permanent loss of a credential that cannot be
/// recovered — a Console API key is not re-derivable, it is re-issued.
pub fn read_migrating(
    secrets: &dyn SecretStore,
    service: &str,
    legacy: &str,
    slot: &str,
) -> Option<String> {
    if let Ok(Some(value)) = secrets.get(service, slot) {
        return Some(value);
    }

    let value = secrets.get(legacy, slot).ok().flatten()?;

    match secrets.set(service, slot, &value) {
        Ok(()) => {
            tracing::info!(slot, "migrated a keychain entry to the new service name");
            let _ = secrets.delete(legacy, slot);
        }
        // Handed back regardless: being unable to write the new entry is no
        // reason to behave as though the credential were gone.
        Err(e) => tracing::warn!(error = %e, slot, "could not migrate; using the old entry"),
    }
    Some(value)
}

/// What a command that must *store* a secret says on a host with no store.
pub const NO_STORE: &str = "this host has no credential store";
