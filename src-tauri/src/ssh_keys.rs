//! Offer to stop typing the password.
//!
//! A host that authenticates by password asks for one on *every* connection,
//! and rmux opens many per session — a terminal, a Claude run, a metrics
//! sample, a file read. Each is an askpass dialog. So the operator who is
//! reaching for a password is exactly the operator who should be asked whether
//! they would like a key instead, at the moment it is obvious why.
//!
//! The private half never leaves this machine; only the `.pub` is sent. See
//! `rmux_ssh::keys` for the rest of the reasoning and the tests that pin it.

use rmux_ssh::keys::{self, Installed};
use rmux_ssh::SshTarget;
use rmux_transport::{Target, TargetId};
use serde::Serialize;

use crate::terminal::TargetRef;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyOffer {
    /// Whether a key rmux generated for this host already exists locally.
    pub have_key: bool,
    /// Where it is, so the operator can see and remove it themselves.
    pub path: String,
}

fn home() -> Result<std::path::PathBuf, String> {
    // `$HOME` on this machine, not the target's — this is where the private key
    // lives and it must never be resolved from anything remote.
    dirs::home_dir().ok_or_else(|| "no home directory".to_string())
}

/// Is there already an rmux key for this host?
#[tauri::command]
pub async fn ssh_key_status(target: TargetRef) -> Result<KeyOffer, String> {
    let TargetId::Ssh(host) = target.id() else {
        // The local machine has nothing to authenticate to.
        return Ok(KeyOffer { have_key: true, path: String::new() });
    };
    let path = keys::key_path(&home()?, &host.label());
    Ok(KeyOffer {
        have_key: path.exists() && path.with_extension("pub").exists(),
        path: path.to_string_lossy().into_owned(),
    })
}

/// Generate a key if needed and append its public half to the host.
///
/// Runs over the connection the operator has *already* authenticated, which is
/// what makes this work at all: the password they just typed is what authorises
/// writing `authorized_keys`, and it is never asked for again afterwards.
#[tauri::command]
pub async fn ssh_key_install(target: TargetRef) -> Result<String, String> {
    let TargetId::Ssh(host) = target.id() else {
        return Err("this machine needs no key".into());
    };

    let path = keys::key_path(&home()?, &host.label());
    // The comment is what identifies this key in `authorized_keys` months
    // later, when someone is deciding which lines are safe to delete. `rmux@`
    // plus the local account is enough to place it and costs no dependency.
    let comment = format!("rmux@{}", std::env::var("USER").unwrap_or_else(|_| "local".into()));
    let public = keys::ensure_local_key(&path, &comment).map_err(|e| e.to_string())?;

    let ssh = SshTarget::new(host.clone());
    ssh.connect().await.map_err(|e| e.to_string())?;
    let installed = keys::install_key(&ssh as &dyn Target, &public).await.map_err(|e| e.to_string())?;

    Ok(match installed {
        // Reported distinctly. A second attempt that says "added" would leave
        // the operator unsure whether the first one worked.
        Installed::Added => format!("key added to {}", host.label()),
        Installed::AlreadyPresent => format!("{} already had this key", host.label()),
    })
}
