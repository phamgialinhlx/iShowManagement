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
use serde::{Deserialize, Serialize};

use crate::registry::{Registry, Web};
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
pub async fn ssh_key_status(target: TargetRef) -> Result<KeyOffer, String> {
    let TargetId::Ssh(host) = target.id() else {
        // The local machine has nothing to authenticate to.
        return Ok(KeyOffer { have_key: true, path: String::new() });
    };
    let path = keys::key_path(&home()?, &host.label());
    Ok(KeyOffer {
        // Same trap as in `ensure_local_key`: a host with dots — every IP
        // address — made `with_extension` name a file nothing writes, so this
        // reported "no key" for a host that had one and offered again on every
        // connection.
        have_key: path.exists() && keys::public_path(&path).exists(),
        path: path.to_string_lossy().into_owned(),
    })
}

/// Generate a key if needed and append its public half to the host.
///
/// Runs over the connection the operator has *already* authenticated, which is
/// what makes this work at all: the password they just typed is what authorises
/// writing `authorized_keys`, and it is never asked for again afterwards.
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

/// What the connect dialog shows beside a picked key file. Never its contents.
pub async fn ssh_identity_check(path: String) -> Result<keys::IdentityCheck, String> {
    let p = std::path::PathBuf::from(path.trim());
    if !p.is_absolute() {
        return Err("choose the key with the file picker — the path must be absolute".into());
    }
    Ok(keys::check_identity(&p))
}

/// TEST CONNECTION: one `uname -s` over a fresh connection with exactly these
/// settings — host verbatim, user, port, the chosen key and rmux's own.
///
/// **Builds the host id by hand rather than through `TargetRef::id`**, which
/// registers the chosen key for every later connection. A test the operator
/// then cancels must leave nothing behind.
pub async fn ssh_connection_test(target: TargetRef) -> Result<String, String> {
    let Some(alias) = target.host.clone().filter(|h| !h.trim().is_empty()) else {
        return Ok("this machine needs no connection".into());
    };
    let host = rmux_transport::SshHostId { alias, user: target.user.clone(), port: target.port };
    if let Some(raw) = target.identity.as_deref().filter(|r| !r.trim().is_empty()) {
        if target.identity_path().is_none() {
            return Err("the key path must be absolute".into());
        }
        if !std::path::Path::new(raw.trim()).is_file() {
            return Err(format!("{raw} does not exist"));
        }
    }
    rmux_ssh::probe::test_connection(&host, target.identity_path().as_deref()).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetArgs {
    pub target: TargetRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathArgs {
    pub path: String,
}

pub fn register(r: &mut Registry) {
    // On the web, "this machine" is the dev host rmux-web runs on — which is the
    // ssh client there, so a key it generates is the key its connections offer.
    r.add("ssh_key_status", Web::Yes, |_, a: TargetArgs| ssh_key_status(a.target));
    r.add("ssh_key_install", Web::Yes, |_, a: TargetArgs| ssh_key_install(a.target));
    // Desktop only: a key *file* is chosen with the native picker, and on the web
    // these would let a browser probe paths on the machine rmux-web runs on.
    r.add("ssh_identity_check", Web::No, |_, a: PathArgs| ssh_identity_check(a.path));
    r.add("ssh_connection_test", Web::No, |_, a: TargetArgs| ssh_connection_test(a.target));
}
