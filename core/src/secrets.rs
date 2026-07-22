//! Encrypted-at-rest secret storage. Primary backend is the OS keychain
//! (`keyring` → macOS Keychain); when that's unavailable (headless/CI) we fall
//! back to an AES-256-GCM file with a 0600 key file. Holds SSH passwords under
//! `pw:<alias>`. Mirrors `references/tsmanager/server/secrets.js`.
//!
//! `ISM_SECRET_BACKEND=file|keyring` forces a backend (tests use `file` to avoid
//! touching the real Keychain).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{anyhow, Result};

const SERVICE: &str = "iShowManagement";

pub fn pw_key(alias: &str) -> String {
    format!("pw:{alias}")
}

pub enum SecretStore {
    Keyring,
    File(Mutex<FileVault>),
}

impl SecretStore {
    /// Choose a backend: honor `ISM_SECRET_BACKEND`, else prefer keyring and
    /// fall back to the AES file if the keychain isn't usable.
    pub fn open(data_dir: &Path) -> Self {
        match std::env::var("ISM_SECRET_BACKEND").ok().as_deref() {
            Some("file") => return Self::file(data_dir),
            Some("keyring") => return Self::Keyring,
            _ => {}
        }
        if Self::keyring_usable() {
            Self::Keyring
        } else {
            tracing::warn!("keychain unavailable — using encrypted file fallback");
            Self::file(data_dir)
        }
    }

    fn file(data_dir: &Path) -> Self {
        Self::File(Mutex::new(FileVault::open(data_dir)))
    }

    fn keyring_usable() -> bool {
        let Ok(entry) = keyring::Entry::new(SERVICE, "__probe__") else {
            return false;
        };
        if entry.set_password("1").is_err() {
            return false;
        }
        let ok = entry.get_password().map(|v| v == "1").unwrap_or(false);
        let _ = entry.delete_credential();
        ok
    }

    pub fn get(&self, key: &str) -> Option<String> {
        match self {
            Self::Keyring => keyring::Entry::new(SERVICE, key)
                .ok()
                .and_then(|e| e.get_password().ok()),
            Self::File(v) => v.lock().unwrap().map.get(key).cloned(),
        }
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        match self {
            Self::Keyring => keyring::Entry::new(SERVICE, key)?
                .set_password(value)
                .map_err(Into::into),
            Self::File(v) => {
                let mut vault = v.lock().unwrap();
                vault.map.insert(key.to_string(), value.to_string());
                vault.save()
            }
        }
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        match self {
            Self::Keyring => match keyring::Entry::new(SERVICE, key)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.into()),
            },
            Self::File(v) => {
                let mut vault = v.lock().unwrap();
                vault.map.remove(key);
                vault.save()
            }
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// AES-256-GCM encrypted key/value file. On-disk layout of `secrets.enc`:
/// `nonce(12) || ciphertext`. The 32-byte key lives beside it in `secrets.key`
/// (mode 0600).
pub struct FileVault {
    enc_path: PathBuf,
    key: [u8; 32],
    map: BTreeMap<String, String>,
}

impl FileVault {
    fn open(data_dir: &Path) -> Self {
        let _ = fs::create_dir_all(data_dir);
        let key = load_or_create_key(&data_dir.join("secrets.key"))
            .expect("secret key file read/create");
        let enc_path = data_dir.join("secrets.enc");
        let map = load_map(&enc_path, &key);
        Self { enc_path, key, map }
    }

    // `Nonce::from_slice` / `Key::from_slice` are deprecated in aes-gcm 0.11 in
    // favor of TryFrom; keeping them (functionally identical) until we bump.
    #[allow(deprecated)]
    fn save(&self) -> Result<()> {
        let plaintext = serde_json::to_vec(&self.map)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| anyhow!("rng: {e}"))?;
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_slice())
            .map_err(|_| anyhow!("AES encrypt failed"))?;
        let mut blob = Vec::with_capacity(12 + ct.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        write_private(&self.enc_path, &blob)
    }
}

fn load_or_create_key(path: &Path) -> Result<[u8; 32]> {
    if let Ok(bytes) = fs::read(path) {
        if bytes.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            return Ok(k);
        }
    }
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).map_err(|e| anyhow!("rng: {e}"))?;
    write_private(path, &k)?;
    Ok(k)
}

#[allow(deprecated)]
fn load_map(enc_path: &Path, key: &[u8; 32]) -> BTreeMap<String, String> {
    let Ok(data) = fs::read(enc_path) else {
        return BTreeMap::new();
    };
    if data.len() < 12 {
        return BTreeMap::new();
    }
    let (nonce, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .ok()
        .and_then(|pt| serde_json::from_slice(&pt).ok())
        .unwrap_or_default()
}

/// Write `bytes` to `path` with mode 0600, via a temp file + rename.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_vault_roundtrips_and_persists_encrypted() {
        let dir = std::env::temp_dir().join(format!("ism-secrets-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        {
            let store = SecretStore::File(Mutex::new(FileVault::open(&dir)));
            assert!(!store.has("pw:web"));
            store.set("pw:web", "s3cr3t").unwrap();
            assert_eq!(store.get("pw:web").as_deref(), Some("s3cr3t"));
            assert!(store.has("pw:web"));
        }

        // Reopen: value survives (was persisted, decrypts correctly).
        {
            let store = SecretStore::File(Mutex::new(FileVault::open(&dir)));
            assert_eq!(store.get("pw:web").as_deref(), Some("s3cr3t"));
            store.delete("pw:web").unwrap();
            assert!(!store.has("pw:web"));
        }

        // The ciphertext file must not contain the plaintext secret.
        let store = SecretStore::File(Mutex::new(FileVault::open(&dir)));
        store.set("pw:db", "PLAINTEXT_MARKER").unwrap();
        let raw = fs::read(dir.join("secrets.enc")).unwrap();
        assert!(!raw
            .windows(b"PLAINTEXT_MARKER".len())
            .any(|w| w == b"PLAINTEXT_MARKER"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pw_key_is_namespaced() {
        assert_eq!(pw_key("web"), "pw:web");
    }
}
