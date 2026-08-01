//! Locking rmux behind a PIN.
//!
//! The lock is **opt-in**. With it off, the app opens straight into the workbench
//! — which is the right default, because terminals, files and Claude are a direct
//! SSH connection that never involves an account at all. Turning it on says "ask
//! who is at the keyboard before restoring my session".
//!
//! ## The PIN actually holds the key
//!
//! The obvious way to build this is a boolean in a config file and a screen that
//! refuses to go away. That is theatre: the session token still sits in the
//! keychain in the clear, so anything that can read the keychain has the account,
//! and the lock only inconveniences the person who knows the PIN.
//!
//! So the PIN *is* the key. The stored credentials are sealed with a key derived
//! from it, and unlocking means decrypting them. There is no correct-PIN check to
//! bypass — with the wrong PIN the plaintext does not exist. That also makes the
//! wrong-PIN answer trustworthy: it comes from an AEAD tag, not from a comparison
//! this code could get wrong.
//!
//! ## Why argon2id, and why it is deliberately slow
//!
//! A PIN is a tiny secret — four digits is ten thousand possibilities, which a
//! plain hash would exhaust instantly. The only defence is making each guess
//! expensive, so the derivation is memory-hard and takes an appreciable fraction
//! of a second. That is a cost paid once per unlock and ten thousand times by
//! someone guessing.
//!
//! It is honest to be clear about the limit: this protects the *session token*.
//! It does not encrypt the user's disk, and it cannot protect the SSH keys that
//! the workbench uses without any account. The threat it answers is a laptop left
//! open or lent out, not a forensic image.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

use crate::credentials::StoredCredentials;

/// Memory cost in KiB. 64 MiB is enough to make massively parallel guessing
/// expensive while staying comfortable on a laptop.
const MEMORY_KIB: u32 = 64 * 1024;
/// Passes over that memory.
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;

const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

/// PIN length bounds, matching what the Cowork server accepts so a PIN set here
/// is the same PIN everywhere.
pub const MIN_PIN_LEN: usize = 4;
pub const MAX_PIN_LEN: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// The PIN did not open the vault. Deliberately the *only* failure a caller
    /// can distinguish, so nothing here becomes an oracle for a partially
    /// correct PIN.
    #[error("that PIN is not correct")]
    WrongPin,
    #[error("a PIN must be {MIN_PIN_LEN} to {MAX_PIN_LEN} digits")]
    BadPin,
    #[error("the stored session is unreadable")]
    Corrupt,
    #[error("could not derive a key: {0}")]
    Kdf(String),
}

/// What the keychain holds for a server.
///
/// Untagged so a vault written before the lock existed still loads: the plain
/// form is exactly the old JSON. Order matters — `Sealed` is tried first because
/// it is the more specific shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Vault {
    Sealed(SealedVault),
    /// No lock: the credentials as they have always been stored.
    Plain(StoredCredentials),
}

/// Credentials encrypted under a PIN-derived key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SealedVault {
    /// Format version. Present so a future change to the KDF can be recognised
    /// rather than mis-parsed into a wrong key and an unopenable vault.
    pub sealed: u8,
    pub salt: String,
    pub nonce: String,
    pub blob: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    /// Whether face unlock is offered alongside the PIN. Readable without the
    /// PIN on purpose — the lock screen has to know which buttons to draw before
    /// anyone has proved anything, and this discloses nothing.
    #[serde(default)]
    pub face: bool,
    /// Who the sealed session belongs to, for the "signing in as …" line on the
    /// lock screen. A display name is already visible to anyone holding the
    /// machine; making them unlock blind buys nothing.
    #[serde(default)]
    pub username: String,
}

impl Vault {
    pub fn is_sealed(&self) -> bool {
        matches!(self, Vault::Sealed(_))
    }
}

/// Reject anything that is not a PIN before it reaches the KDF.
///
/// Digits only, because that is what the server stores and what the on-screen
/// keypad can enter — accepting a letter here would create a PIN that works in
/// rmux and nowhere else.
pub fn check_pin(pin: &str) -> Result<(), LockError> {
    let ok = (MIN_PIN_LEN..=MAX_PIN_LEN).contains(&pin.len())
        && pin.chars().all(|c| c.is_ascii_digit());
    ok.then_some(()).ok_or(LockError::BadPin)
}

fn random(len: usize) -> Vec<u8> {
    use rand::RngCore as _;
    let mut buf = vec![0u8; len];
    rand::rng().fill_bytes(&mut buf);
    buf
}

/// A key derived from a PIN, kept for as long as the app stays unlocked.
///
/// It exists because the session token is **rewritten while the app runs** — an
/// SSO refresh rotates it, and the server can hand back a new one on any call.
/// Without the key in hand the only ways to persist a rotated token would be to
/// ask for the PIN again mid-session, or to write it back unsealed, which would
/// quietly undo the lock. Keeping the derived key avoids both, and re-running
/// argon2 for every token rotation would in any case be a noticeable stall.
///
/// The salt and cost parameters travel with it so a re-seal reproduces the same
/// key rather than deriving a new one.
pub struct VaultKey {
    key: [u8; KEY_LEN],
    salt: Vec<u8>,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        // Volatile so the compiler cannot elide a write to memory that is about
        // to be freed — which is exactly the write that matters here.
        for byte in &mut self.key {
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl std::fmt::Debug for VaultKey {
    /// Never print the key. A struct that derives `Debug` eventually ends up in
    /// a log line or an error message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(…)")
    }
}

impl VaultKey {
    fn derive(
        pin: &str,
        salt: Vec<u8>,
        memory_kib: u32,
        iterations: u32,
        parallelism: u32,
    ) -> Result<Self, LockError> {
        // Cheap check first: the KDF takes an appreciable fraction of a second,
        // so running it on input that cannot be a PIN is free work for an
        // attacker to ask for.
        check_pin(pin)?;

        let params = Params::new(memory_kib, iterations, parallelism, Some(KEY_LEN))
            .map_err(|e| LockError::Kdf(e.to_string()))?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        let mut key = [0u8; KEY_LEN];
        argon
            .hash_password_into(pin.as_bytes(), &salt, &mut key)
            .map_err(|e| LockError::Kdf(e.to_string()))?;

        Ok(Self { key, salt, memory_kib, iterations, parallelism })
    }

    /// Derive a key for a brand-new vault, at the current cost.
    ///
    /// A fresh salt every time: re-using one across servers would let a single
    /// derivation be tried against several vaults at once.
    pub fn new(pin: &str) -> Result<Self, LockError> {
        Self::derive(pin, random(SALT_LEN), MEMORY_KIB, ITERATIONS, PARALLELISM)
    }

    /// Derive the key that opens an existing vault.
    ///
    /// The cost parameters come from the vault rather than from the constants
    /// above, so raising the defaults later does not strand anyone whose vault
    /// was sealed under the old ones.
    pub fn for_vault(vault: &SealedVault, pin: &str) -> Result<Self, LockError> {
        let salt = B64.decode(&vault.salt).map_err(|_| LockError::Corrupt)?;
        Self::derive(pin, salt, vault.memory_kib, vault.iterations, vault.parallelism)
    }

    /// Seal credentials under this key, with a nonce that has never been used.
    pub fn seal(&self, creds: &StoredCredentials, face: bool) -> Result<SealedVault, LockError> {
        let nonce = random(NONCE_LEN);
        let plaintext = serde_json::to_vec(creds).map_err(|_| LockError::Corrupt)?;

        let cipher = XChaCha20Poly1305::new((&self.key).into());
        let blob = cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext.as_ref())
            .map_err(|_| LockError::Corrupt)?;

        Ok(SealedVault {
            sealed: 1,
            salt: B64.encode(&self.salt),
            nonce: B64.encode(nonce),
            blob: B64.encode(blob),
            memory_kib: self.memory_kib,
            iterations: self.iterations,
            parallelism: self.parallelism,
            face,
            username: creds.username.clone(),
        })
    }

    /// Open a vault, or fail because the PIN behind this key is wrong.
    pub fn open(&self, vault: &SealedVault) -> Result<StoredCredentials, LockError> {
        let nonce = B64.decode(&vault.nonce).map_err(|_| LockError::Corrupt)?;
        let blob = B64.decode(&vault.blob).map_err(|_| LockError::Corrupt)?;

        if nonce.len() != NONCE_LEN {
            return Err(LockError::Corrupt);
        }

        let cipher = XChaCha20Poly1305::new((&self.key).into());
        // The authentication tag is what decides this, so a wrong PIN cannot
        // yield plausible-looking garbage that fails somewhere less careful.
        let plaintext = cipher
            .decrypt(XNonce::from_slice(&nonce), blob.as_ref())
            .map_err(|_| LockError::WrongPin)?;

        serde_json::from_slice(&plaintext).map_err(|_| LockError::Corrupt)
    }
}

/// Seal credentials under a PIN.
pub fn seal(creds: &StoredCredentials, pin: &str, face: bool) -> Result<SealedVault, LockError> {
    VaultKey::new(pin)?.seal(creds, face)
}

/// Open a sealed vault, or fail because the PIN is wrong.
pub fn open(vault: &SealedVault, pin: &str) -> Result<StoredCredentials, LockError> {
    VaultKey::for_vault(vault, pin)?.open(vault)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> StoredCredentials {
        StoredCredentials {
            token: "rcwa_a_real_looking_token".into(),
            refresh_token: None,
            username: "dev.user".into(),
        }
    }

    #[test]
    fn the_right_pin_returns_the_credentials_unchanged() {
        let vault = seal(&creds(), "482913", false).unwrap();
        assert_eq!(open(&vault, "482913").unwrap(), creds());
    }

    #[test]
    fn a_wrong_pin_yields_nothing_usable() {
        let vault = seal(&creds(), "482913", false).unwrap();
        assert!(matches!(open(&vault, "482914"), Err(LockError::WrongPin)));
        // Every other wrong PIN of a legal length answers the same way. If one
        // of these came back Corrupt instead, the error itself would leak which
        // guesses were closer.
        for guess in ["0000", "48291", "4829130", "99999999"] {
            assert!(matches!(open(&vault, guess), Err(LockError::WrongPin)), "{guess}");
        }
    }

    #[test]
    fn the_token_is_not_present_in_the_sealed_form() {
        // The whole point. If this passed while the blob still contained the
        // token, the lock would be decoration.
        let vault = seal(&creds(), "482913", false).unwrap();
        let json = serde_json::to_string(&vault).unwrap();
        assert!(!json.contains("rcwa_a_real_looking_token"), "{json}");

        // …and not merely base64'd out of sight, either.
        let blob = B64.decode(&vault.blob).unwrap();
        let window = String::from_utf8_lossy(&blob);
        assert!(!window.contains("rcwa_"), "{window}");
    }

    #[test]
    fn sealing_twice_produces_different_ciphertext() {
        // Same PIN, same credentials. Equal output would mean a fixed salt or
        // nonce, and a reused nonce with a stream cipher is a break, not a
        // blemish.
        let a = seal(&creds(), "482913", false).unwrap();
        let b = seal(&creds(), "482913", false).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.blob, b.blob);
    }

    #[test]
    fn a_tampered_blob_is_refused_rather_than_decrypted() {
        let mut vault = seal(&creds(), "482913", false).unwrap();
        let mut blob = B64.decode(&vault.blob).unwrap();
        blob[0] ^= 0x01;
        vault.blob = B64.encode(blob);

        // Refused by the authentication tag, not by the JSON parser noticing the
        // result was gibberish. `is_err()` alone would pass even with no
        // encryption at all, which makes it no test of anything.
        assert!(matches!(open(&vault, "482913"), Err(LockError::WrongPin)));
    }

    #[test]
    fn every_byte_of_the_vault_is_covered_by_the_tag() {
        // Flipping the salt or the nonce must fail too. If only the blob were
        // authenticated, either field could be swapped between vaults.
        let vault = seal(&creds(), "482913", false).unwrap();

        let mut nonce_tampered = vault.clone();
        let mut nonce = B64.decode(&vault.nonce).unwrap();
        nonce[0] ^= 0x01;
        nonce_tampered.nonce = B64.encode(nonce);
        assert!(matches!(open(&nonce_tampered, "482913"), Err(LockError::WrongPin)));

        let mut salt_tampered = vault.clone();
        let mut salt = B64.decode(&vault.salt).unwrap();
        salt[0] ^= 0x01;
        salt_tampered.salt = B64.encode(salt);
        assert!(matches!(open(&salt_tampered, "482913"), Err(LockError::WrongPin)));
    }

    #[test]
    fn pins_must_be_digits_of_a_sensible_length() {
        assert!(check_pin("1234").is_ok());
        assert!(check_pin("12345678").is_ok());
        assert!(check_pin("123").is_err());
        assert!(check_pin("123456789").is_err());
        assert!(check_pin("12a4").is_err());
        assert!(check_pin("").is_err());
        // Not a digit, but easy to type by accident.
        assert!(check_pin("1 34").is_err());
    }

    #[test]
    fn a_pin_that_is_not_a_pin_never_reaches_the_kdf() {
        // Cheap check first: the KDF takes a large fraction of a second, so
        // running it on obviously invalid input is a free denial of service.
        assert!(matches!(seal(&creds(), "abc", false), Err(LockError::BadPin)));
        let vault = seal(&creds(), "1234", false).unwrap();
        assert!(matches!(open(&vault, "abc"), Err(LockError::BadPin)));
    }

    #[test]
    fn an_unlocked_vault_still_loads_from_the_old_format() {
        // Everyone signed in before the lock existed has exactly this JSON in
        // their keychain. Failing to parse it would sign them all out.
        let old = r#"{"token":"rcwa_x","username":"nolan"}"#;
        let vault: Vault = serde_json::from_str(old).unwrap();
        match vault {
            Vault::Plain(c) => assert_eq!(c.token, "rcwa_x"),
            Vault::Sealed(_) => panic!("plain credentials read as sealed"),
        }
        assert!(!serde_json::from_str::<Vault>(old).unwrap().is_sealed());
    }

    #[test]
    fn a_sealed_vault_is_not_mistaken_for_plain_credentials() {
        let sealed = seal(&creds(), "482913", true).unwrap();
        let json = serde_json::to_string(&sealed).unwrap();

        match serde_json::from_str::<Vault>(&json).unwrap() {
            Vault::Sealed(v) => {
                assert_eq!(v, sealed);
                assert!(v.face);
                // Readable without the PIN, by design.
                assert_eq!(v.username, "dev.user");
            }
            Vault::Plain(_) => panic!("sealed vault read as plain credentials"),
        }
    }

    #[test]
    fn cost_parameters_travel_with_the_vault() {
        // Raising the defaults must not strand vaults sealed under the old ones,
        // so a vault sealed at a cost that is not the current one still opens.
        let cheap = VaultKey::derive("482913", random(SALT_LEN), 8, 1, 1).unwrap();
        let vault = cheap.seal(&creds(), false).unwrap();

        assert_eq!(vault.memory_kib, 8);
        assert_ne!(vault.memory_kib, MEMORY_KIB);
        assert_eq!(open(&vault, "482913").unwrap(), creds());
        assert!(matches!(open(&vault, "000000"), Err(LockError::WrongPin)));
    }

    #[test]
    fn a_held_key_reseals_a_rotated_token_without_the_pin() {
        // The session token is rewritten while the app runs — an SSO refresh
        // rotates it. If that could not be re-sealed, the choice would be to
        // re-prompt mid-session or to write the token back in the clear.
        let key = VaultKey::new("482913").unwrap();
        let vault = key.seal(&creds(), false).unwrap();

        let rotated = StoredCredentials { token: "rcwa_rotated".into(), ..creds() };
        let resealed = key.seal(&rotated, false).unwrap();

        // Still the same PIN, because the salt was carried over rather than
        // regenerated — a new salt here would silently lock the user out.
        assert_eq!(resealed.salt, vault.salt);
        assert_eq!(open(&resealed, "482913").unwrap().token, "rcwa_rotated");
        // …and a fresh nonce, because reusing one under the same key is a break.
        assert_ne!(resealed.nonce, vault.nonce);
    }

    #[test]
    fn the_key_is_not_printable() {
        // Anything that derives Debug ends up in a log line eventually.
        let key = VaultKey::new("482913").unwrap();
        assert_eq!(format!("{key:?}"), "VaultKey(…)");
    }

    #[test]
    fn refreshable_sessions_survive_the_round_trip() {
        // SSO sessions carry a refresh token; losing it on unlock would turn a
        // renewable session into one that expires for no reason.
        let sso = StoredCredentials { refresh_token: Some("r_1".into()), ..creds() };
        let vault = seal(&sso, "482913", false).unwrap();
        let opened = open(&vault, "482913").unwrap();
        assert_eq!(opened.refresh_token.as_deref(), Some("r_1"));
        assert!(opened.is_refreshable());
    }
}
