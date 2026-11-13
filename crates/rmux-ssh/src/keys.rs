//! Stop typing a password into every session.
//!
//! A host that authenticates by password asks for one on **every** connection —
//! and rmux opens many: a terminal, a Claude session, a metrics sample, a file
//! read. The askpass bridge makes each of those a dialog, so a password host is
//! not merely less secure than a key host, it is materially unpleasant to use.
//! The fix is the one every operator eventually does by hand: put a public key
//! in `~/.ssh/authorized_keys` and never type the password again.
//!
//! ## What must never happen
//!
//! - **The private key never leaves this machine.** Only the `.pub` half is sent.
//!   That is the entire point of asymmetric keys, and a "helpful" copy of the
//!   private half to a server is a compromise of every host that trusts it.
//! - **No passphrase is set on the generated key.** This reads wrong and is
//!   right: the alternative is prompting for that passphrase on every connection,
//!   which is the problem being solved wearing a different hat. The key is
//!   protected by the file permissions of a `0700` directory on the operator's
//!   own machine, and it is scoped — one key per host, so a lost laptop is
//!   revoked per host rather than everywhere at once.
//! - **`authorized_keys` is appended to, never rewritten.** Other keys in it
//!   belong to other machines the operator uses, and truncating that file locks
//!   them out of their own server with no warning and no way back in if this was
//!   the only session.
//! - **Everything interpolated into the remote line is quoted.** The remote
//!   login shell re-parses it; a public key comment contains spaces, and a
//!   `user@host` comment can contain almost anything.

use std::path::{Path, PathBuf};

use rmux_transport::{shell_quote, CommandSpec, NoConsoleWindow, Target};

/// Where rmux keeps the keys it generates.
///
/// Under `~/.ssh` rather than `~/.rmux`, because that is where `ssh` looks and
/// where an operator looks — a key hidden in an app's own directory is one
/// nobody will find when they later want to remove it. Named per host so a
/// single compromised machine is revoked per host.
pub fn key_path(home: &Path, host: &str) -> PathBuf {
    // Only characters that are safe in a filename and recognisable afterwards.
    // A host alias can contain `/` (it is matched against patterns), and a key
    // called `~/.ssh/rmux_a/b` would silently fail to be created.
    let safe: String = host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    home.join(".ssh").join(format!("rmux_{safe}_ed25519"))
}

/// The `-i` arguments for a host, if rmux has generated a key for it.
///
/// **Without this the whole feature does nothing.** rmux writes its key to
/// `~/.ssh/rmux_<host>_ed25519`, which is not one of the names OpenSSH tries on
/// its own — so the public half was installed on the host, the offer reported
/// "key added", and the very next connection asked for a password again. The
/// operator is then told the problem is solved while nothing has changed, which
/// is worse than the feature not existing.
///
/// **Additive, never exclusive.** `-i` appends to the identities ssh will try;
/// it does not suppress the defaults or anything from `~/.ssh/config`. Adding
/// `IdentitiesOnly=yes` would make this exclusive and break every host that
/// already authenticates with the operator's own key — a fix for one host that
/// breaks the rest.
///
/// Empty when there is no such key, because naming a file that does not exist
/// makes ssh warn about it on every single connection.
pub fn identity_args(home: &Path, host: &str) -> Vec<String> {
    identity_args_with(home, host, None)
}

/// Both identities rmux may add for a host: the one the operator **chose**
/// (Connect a server › KEY FILE), then rmux's own generated key.
///
/// The same three rules as the rmux key, for the same reasons:
///
/// - **Additive.** Each is one more `-i`; neither suppresses the other, the
///   agent, nor anything `~/.ssh/config` names. Never `IdentitiesOnly=yes`.
/// - **Only when the file exists.** A chosen key that has since been moved or
///   deleted is skipped silently rather than making ssh warn on every
///   connection — the connection then fails or succeeds on its other keys,
///   which is the honest outcome.
/// - **Chosen first.** ssh tries identities in order, and on a server counting
///   failures (`MaxAuthTries`, fail2ban) the key the operator picked on purpose
///   is the one that should not be crowded out.
///
/// The two are de-duplicated: choosing rmux's own key file explicitly must not
/// offer it twice, which is one wasted authentication attempt.
pub fn identity_args_with(home: &Path, host: &str, chosen: Option<&Path>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(chosen) = chosen.filter(|p| p.is_absolute() && p.is_file()) {
        args.push("-i".to_owned());
        args.push(chosen.to_string_lossy().into_owned());
    }
    let own = key_path(home, host);
    if own.exists() && chosen != Some(own.as_path()) {
        args.push("-i".to_owned());
        args.push(own.to_string_lossy().into_owned());
    }
    args
}

/// The same, resolving the home directory and the chosen identity itself.
///
/// A convenience for every call site that builds an `ssh` command line — the
/// master, client commands, mosh's bootstrap and the forwards — none of which
/// has any reason to know where keys live. Silent when there is no home
/// directory: a machine without one cannot have an rmux key either, so there is
/// nothing to report.
pub fn identity_args_for(host: &str) -> Vec<String> {
    let chosen = chosen_identity(host);
    match dirs::home_dir() {
        Some(home) => identity_args_with(&home, host, chosen.as_deref()),
        None => chosen
            .filter(|p| p.is_file())
            .map(|p| vec!["-i".to_owned(), p.to_string_lossy().into_owned()])
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// The identity the operator chose for a host

/// Which private key the operator picked for each host, by connection label.
///
/// **A side table, not part of the host's identity.** `SshHostId` is what
/// targets are cached by, what the ControlMaster socket is named after and what
/// the UI's `serverId` is derived from; folding a key path into it would give
/// every existing project and session a new id the moment someone picked a key,
/// and two caches for one machine. The key is *how* rmux authenticates to a
/// host, not *which* host it is — so it lives beside the id, keyed by the same
/// `user@alias` label rmux's own key is filed under.
///
/// Filled from two places: every `TargetRef` that crosses IPC carrying an
/// `identity` registers it (so the very first connection of a new server already
/// offers it), and the app's server-list sync **replaces** the whole table, which
/// is how switching a server back to DEFAULT takes effect.
static CHOSEN: std::sync::LazyLock<parking_lot::RwLock<std::collections::HashMap<String, PathBuf>>> =
    std::sync::LazyLock::new(Default::default);

/// Record `path` as the identity to offer `label`. Ignored unless absolute: a
/// relative path would resolve against whatever directory rmux was launched
/// from, which is not something the operator chose.
pub fn choose_identity(label: &str, path: &Path) {
    if path.is_absolute() {
        CHOSEN.write().insert(label.to_owned(), path.to_path_buf());
    }
}

/// Replace every chosen identity at once — the server list is the authority,
/// so a host whose key choice was cleared stops being offered it.
pub fn replace_chosen_identities(entries: impl IntoIterator<Item = (String, PathBuf)>) {
    let next = entries.into_iter().filter(|(_, p)| p.is_absolute()).collect();
    *CHOSEN.write() = next;
}

/// The identity chosen for `label`, if any.
pub fn chosen_identity(label: &str) -> Option<PathBuf> {
    CHOSEN.read().get(label).cloned()
}

/// Held by every test that touches the process-wide table, because
/// `replace_chosen_identities` in one would wipe another's entry mid-run — they
/// would pass alone and fail together.
#[cfg(test)]
pub(crate) static CHOSEN_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// What the connect dialog needs to know about a key file before offering it.
///
/// Every field is something ssh would otherwise discover *later*, on the
/// connection, as an error that names the wrong problem: a `.pub` passed to
/// `-i` loads as no usable key, an unreadable file is skipped, and a private key
/// readable by others is refused outright with "UNPROTECTED PRIVATE KEY FILE".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCheck {
    pub path: String,
    pub exists: bool,
    pub readable: bool,
    /// The file is the public half (`.pub`, or its contents start `ssh-…`).
    pub is_public: bool,
    /// The matching private key, when this is a `.pub` and it sits beside it.
    pub private_sibling: Option<String>,
    /// Unix permission bits, when known.
    pub mode: Option<u32>,
    /// Readable or writable by group or others — ssh will refuse it.
    pub too_open: bool,
}

/// Inspect a candidate key file. Reads at most the first line, to tell a public
/// key from a private one; never returns any of its contents.
pub fn check_identity(path: &Path) -> IdentityCheck {
    let text = path.to_string_lossy().into_owned();
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file());
    let exists = meta.is_some();
    let first = if exists {
        std::fs::File::open(path).ok().and_then(|f| {
            use std::io::{BufRead, BufReader, Read};
            let mut line = String::new();
            BufReader::new(f.take(512)).read_line(&mut line).ok().map(|_| line)
        })
    } else {
        None
    };
    let readable = first.is_some();
    let looks_public = first
        .as_deref()
        .map(|l| {
            let l = l.trim_start();
            l.starts_with("ssh-") || l.starts_with("ecdsa-") || l.starts_with("sk-")
        })
        .unwrap_or(false);
    let is_public = text.ends_with(".pub") || looks_public;
    let private_sibling = text
        .strip_suffix(".pub")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned());

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.as_ref().map(|m| m.permissions().mode() & 0o777)
    };
    #[cfg(not(unix))]
    let mode: Option<u32> = None;

    IdentityCheck {
        path: text,
        exists,
        readable,
        is_public,
        private_sibling,
        mode,
        // Only meaningful for a private key: a `.pub` is meant to be readable.
        too_open: !is_public && mode.is_some_and(|m| m & 0o077 != 0),
    }
}

/// Where `ssh-keygen -f <path>` puts the public half.
///
/// **Appended, never `with_extension`.** `ssh-keygen` adds `.pub` to the whole
/// filename; `Path::with_extension` *replaces* everything after the last dot.
/// For a host with dots in it — which is to say every IP address —
/// `rmux_dev_10.0.1.22_ed25519` became `rmux_dev_10.0.1.pub`,
/// a file nothing ever writes. The key was generated correctly and reading it
/// back failed with a bare "No such file or directory (os error 2)", so the
/// offer looked broken on exactly the hosts most likely to need it, while
/// working perfectly against an ssh_config alias like `devbox`.
///
/// This is the same trap as the agent socket's `file_stem`, recorded in
/// CLAUDE.md, arrived at from the other direction: a name with dots is not a
/// name with an extension.
pub fn public_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

/// Generate a keypair if one is not already there, and return the public half.
///
/// **ed25519, not RSA.** Small, fast, and supported by every OpenSSH since 6.5
/// (2014); an RSA key large enough to be worth having is slower to generate and
/// far longer to paste. `-N ""` sets no passphrase — see the note above.
///
/// Generating is skipped when the file exists, so this is safe to call again:
/// re-generating would orphan the key already installed on other hosts and
/// leave the operator locked out of every one of them.
pub fn ensure_local_key(path: &Path, comment: &str) -> anyhow::Result<String> {
    let public = public_path(path);
    if path.exists() && public.exists() {
        return Ok(std::fs::read_to_string(&public)?.trim().to_string());
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        // `ssh` refuses to use a key in a world-readable directory, and says so
        // in a way that reads as "permission denied" rather than "fix this".
        restrict_dir(dir)?;
    }

    // A half-generated pair — private written, public not — would be picked up
    // by the `exists` check above and never repaired. Clear both first.
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(&public);

    let mut keygen = std::process::Command::new("ssh-keygen");
    // Without this the generation flashes a console window on Windows — the
    // guard in `tests/no_console_window.rs` is what caught it, which is the
    // point of having a test rather than a convention.
    keygen.no_console_window();
    let out = keygen
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(comment)
        .arg("-f")
        .arg(path)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(std::fs::read_to_string(&public)?.trim().to_string())
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// The script that installs a public key on the far side.
///
/// Separated from the running of it so the exact text can be asserted: this is
/// a line that edits the file controlling who may log in, and "it seemed to
/// work against my server" is not evidence that it is correct.
///
/// `grep -qxF` is the idempotence: `-x` matches whole lines and `-F` treats the
/// key as literal text, so a key already present is not added twice and a `+`
/// or `/` in the base64 cannot be read as a regex.
pub fn install_script(public_key: &str) -> String {
    let key = shell_quote(public_key);
    // `umask 077` before the redirect rather than `chmod` after it: between
    // creating the file and changing its mode there is a window where
    // `authorized_keys` is world-readable, and on a shared host that window is
    // the whole attack.
    format!(
        "set -e; umask 077; mkdir -p \"$HOME/.ssh\"; chmod 700 \"$HOME/.ssh\"; \
         touch \"$HOME/.ssh/authorized_keys\"; chmod 600 \"$HOME/.ssh/authorized_keys\"; \
         if grep -qxF {key} \"$HOME/.ssh/authorized_keys\"; then echo already; \
         else printf '%s\\n' {key} >> \"$HOME/.ssh/authorized_keys\"; echo added; fi"
    )
}

/// What installing a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Installed {
    /// The key was appended.
    Added,
    /// It was already in `authorized_keys` — reported rather than hidden, so a
    /// second attempt does not look like a first success.
    AlreadyPresent,
}

/// Append the public key to the target's `authorized_keys`.
pub async fn install_key(target: &dyn Target, public_key: &str) -> anyhow::Result<Installed> {
    let key = public_key.trim();
    // A private key pasted here by mistake must not be shipped. Cheap to check,
    // and the consequence of not checking is the worst outcome this file has.
    anyhow::ensure!(
        !key.contains("PRIVATE KEY"),
        "that is a private key — only the .pub half may be installed"
    );
    anyhow::ensure!(
        key.starts_with("ssh-") || key.starts_with("ecdsa-"),
        "not an OpenSSH public key"
    );
    anyhow::ensure!(!key.contains('\n'), "a public key is a single line");

    let spec = CommandSpec::login_shell().arg("-c").arg(install_script(key));
    let out = target.exec(&spec).await?;
    anyhow::ensure!(
        out.ok(),
        "could not write authorized_keys: {}",
        out.stderr.trim()
    );

    Ok(if out.stdout.contains("already") {
        Installed::AlreadyPresent
    } else {
        Installed::Added
    })
}

#[cfg(test)]
mod public_path_tests {
    use super::*;

    /// **A host with dots is not a filename with an extension.**
    ///
    /// `ssh-keygen -f <path>` writes `<path>.pub`, appending to the whole name.
    /// `Path::with_extension("pub")` *replaces* everything after the last dot,
    /// so `rmux_dev_10.0.1.22_ed25519` became
    /// `rmux_dev_10.0.1.pub` — a file nothing writes. Reading it back
    /// failed with a bare "No such file or directory (os error 2)", so the key
    /// offer was broken for **every IP address** while working perfectly
    /// against an ssh_config alias.
    ///
    /// The same class of mistake as the agent socket's `file_stem`, which
    /// CLAUDE.md already records. This is the version that pins it.
    #[test]
    fn the_public_half_is_appended_not_substituted() {
        let path = Path::new("/home/x/.ssh/rmux_dev_10.0.1.22_ed25519");
        assert_eq!(
            public_path(path).to_string_lossy(),
            "/home/x/.ssh/rmux_dev_10.0.1.22_ed25519.pub",
            "ssh-keygen appends `.pub` to the whole name"
        );

        // What the bug did, stated so nobody reintroduces it thinking it is
        // equivalent.
        assert_ne!(
            public_path(path),
            path.with_extension("pub"),
            "with_extension eats the last dotted segment — that was the bug"
        );
    }

    #[test]
    fn an_undotted_host_is_unaffected() {
        let path = Path::new("/home/x/.ssh/rmux_devbox_ed25519");
        assert_eq!(
            public_path(path).to_string_lossy(),
            "/home/x/.ssh/rmux_devbox_ed25519.pub",
            "the case that always worked must keep working"
        );
    }

    /// End to end against a real `ssh-keygen`: generate with a dotted host name
    /// and read the public half back. This is the assertion the operator's
    /// failure would have tripped — a unit test on paths alone could still be
    /// wrong about what ssh-keygen actually writes.
    #[test]
    fn a_dotted_host_generates_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("rmux-keys-{}", std::process::id()));
        let path = key_path(&dir, "dev@10.0.1.22");

        let public = ensure_local_key(&path, "rmux@test").expect("generate");
        assert!(public.starts_with("ssh-ed25519 "), "got: {public}");

        // Calling again must find the pair rather than regenerating it —
        // regenerating would orphan the key already installed on every host.
        let again = ensure_local_key(&path, "rmux@test").expect("second call");
        assert_eq!(public, again, "a second call must return the same key");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    /// **The key must be offered, or installing it changes nothing.**
    ///
    /// rmux writes its key to `~/.ssh/rmux_<host>_ed25519`, which OpenSSH does
    /// not try on its own. Without `-i` the public half sat in the host's
    /// `authorized_keys` being ignored, the offer reported "key added", and the
    /// next connection asked for a password again — the operator told the
    /// problem was solved while nothing had changed.
    #[test]
    fn an_existing_key_is_offered_to_ssh() {
        let home = std::env::temp_dir().join(format!("rmux-id-{}", std::process::id()));
        let host = "10.0.1.22";
        let path = key_path(&home, host);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not a real key").unwrap();

        let args = identity_args(&home, host);
        assert_eq!(args.len(), 2, "expected `-i <path>`, got {args:?}");
        assert_eq!(args[0], "-i");
        assert!(args[1].ends_with("rmux_10.0.1.22_ed25519"), "got {args:?}");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// **Nothing is offered when there is no key**, because naming a file that
    /// does not exist makes ssh complain on every connection — turning a host
    /// that works into one that prints a warning for ever.
    #[test]
    fn no_key_means_no_argument() {
        let home = std::env::temp_dir().join(format!("rmux-id-none-{}", std::process::id()));
        assert!(identity_args(&home, "never-used").is_empty());
    }

    /// A key belongs to one host. Offering another host's key would be a
    /// pointless extra authentication attempt, and on a server counting failures
    /// it is a step towards being locked out.
    #[test]
    fn a_key_is_not_offered_to_a_different_host() {
        let home = std::env::temp_dir().join(format!("rmux-id-other-{}", std::process::id()));
        let path = key_path(&home, "host-a");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not a real key").unwrap();

        assert!(identity_args(&home, "host-b").is_empty(), "host-b must not get host-a's key");
        assert!(!identity_args(&home, "host-a").is_empty(), "host-a must get its own");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// **A chosen key is offered too — additively, first, and only if present.**
    #[test]
    fn a_chosen_identity_is_offered_beside_the_rmux_key() {
        let home = std::env::temp_dir().join(format!("rmux-id-chosen-{}", std::process::id()));
        let label = "dev@10.9.8.7";
        let own = key_path(&home, label);
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, "not a real key").unwrap();
        let chosen = home.join(".ssh").join("work.key");
        std::fs::write(&chosen, "not a real key").unwrap();

        let args = identity_args_with(&home, label, Some(&chosen));
        assert_eq!(
            args,
            vec!["-i".to_owned(), chosen.to_string_lossy().into_owned(), "-i".to_owned(), own.to_string_lossy().into_owned()],
            "chosen first, rmux key still offered"
        );
        assert!(!args.iter().any(|a| a.contains("IdentitiesOnly")));

        // Choosing rmux's own file is not two offers of one key.
        assert_eq!(identity_args_with(&home, label, Some(&own)).len(), 2);

        // A chosen file that has gone is silently skipped.
        let gone = home.join(".ssh").join("deleted.key");
        assert_eq!(identity_args_with(&home, label, Some(&gone)), identity_args(&home, label));
        // So is a relative one.
        assert_eq!(identity_args_with(&home, label, Some(Path::new("work.key"))), identity_args(&home, label));

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The registry is keyed by label and replaced wholesale by the server sync,
    /// which is how a server switched back to DEFAULT stops offering its key.
    #[test]
    fn the_chosen_table_is_replaced_not_merged() {
        let dir = std::env::temp_dir().join(format!("rmux-id-table-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("k");
        std::fs::write(&key, "x").unwrap();
        let label = "registry-test@chosen-table";
        let _serial = CHOSEN_TEST_LOCK.lock();

        choose_identity(label, &key);
        assert_eq!(chosen_identity(label).as_deref(), Some(key.as_path()));
        assert!(identity_args_for(label).windows(2).any(|w| w[0] == "-i" && w[1] == key.to_string_lossy()));

        choose_identity("registry-test@relative", Path::new("relative/k"));
        assert_eq!(chosen_identity("registry-test@relative"), None, "relative paths are refused");

        replace_chosen_identities(Vec::new());
        assert_eq!(chosen_identity(label), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_file_is_inspected_without_reading_it_out() {
        let dir = std::env::temp_dir().join(format!("rmux-id-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let private = dir.join("id_test");
        std::fs::write(&private, "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n").unwrap();
        let public = public_path(&private);
        std::fs::write(&public, "ssh-ed25519 AAAA test\n").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o644)).unwrap();
            let c = check_identity(&private);
            assert!(c.exists && c.readable && !c.is_public);
            assert!(c.too_open, "0644 private key must be flagged: {c:?}");
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(!check_identity(&private).too_open);
        }

        let c = check_identity(&public);
        assert!(c.is_public && !c.too_open, "{c:?}");
        assert_eq!(c.private_sibling.as_deref(), Some(private.to_string_lossy().as_ref()));

        let c = check_identity(&dir.join("missing"));
        assert!(!c.exists && !c.readable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `user@host` is the label a key is filed under, so the two must agree —
    /// otherwise a key installed for `dev@10.0.0.1` is never found again.
    #[test]
    fn a_user_qualified_host_finds_its_own_key() {
        let home = std::env::temp_dir().join(format!("rmux-id-user-{}", std::process::id()));
        let label = "dev@10.0.1.22";
        let path = key_path(&home, label);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not a real key").unwrap();

        let args = identity_args(&home, label);
        assert_eq!(args.len(), 2, "a user-qualified host must find its key: {args:?}");

        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_quoted_into_the_script() {
        // A public key ends in a comment, which contains spaces and often an
        // `@`. Unquoted, the shell splits it and `grep` is handed three
        // arguments — the file is then searched for the wrong thing and the key
        // appended on every single call.
        let script = install_script("ssh-ed25519 AAAAC3Nz rmux@my machine");
        assert!(script.contains("'ssh-ed25519 AAAAC3Nz rmux@my machine'"), "{script}");
    }

    /// The script with every single-quoted span removed.
    ///
    /// Asserting that hostile text is *absent* is the wrong test and it failed
    /// honestly: `shell_quote` leaves the text in the line, quoted, which is
    /// exactly right — the shell reads it as data. What must be true is that
    /// none of it survives *as shell source*, so the quoted spans are stripped
    /// and only what the shell would execute is examined.
    fn unquoted(script: &str) -> String {
        let mut out = String::new();
        let mut inside = false;
        let mut chars = script.chars();
        while let Some(c) = chars.next() {
            match c {
                '\'' => inside = !inside,
                // Outside quotes, a backslash escapes the next character, which
                // is therefore data too. `shell_quote` spells an embedded quote
                // `'\''` — close, escaped quote, reopen — and a walker that
                // does not honour the escape reads the reopening quote as a
                // *closing* one and reports the rest of the payload as live
                // shell source. That is a false alarm, and this test existed to
                // catch a real one.
                '\\' if !inside => {
                    chars.next();
                }
                _ if !inside => out.push(c),
                _ => {}
            }
        }
        assert!(!inside, "unbalanced quotes: {script}");
        out
    }

    #[test]
    fn a_hostile_comment_cannot_escape() {
        let script = install_script("ssh-ed25519 AAAA '; rm -rf ~; echo '");
        let code = unquoted(&script);
        // `shell_quote` closes the quote, escapes the operator's own, and
        // reopens — so the payload is an argument and never a command.
        assert!(!code.contains("rm -rf"), "{code}");
        assert!(!code.contains("rm "), "{code}");
    }

    #[test]
    fn a_key_carrying_a_command_substitution_stays_data() {
        // Single quotes do not interpolate, but a naive fix using double quotes
        // would, and this is the case that would prove it.
        let script = install_script("ssh-ed25519 AAAA $(id) `whoami` ${HOME}");
        let code = unquoted(&script);
        assert!(!code.contains("$(id)"), "{code}");
        assert!(!code.contains("whoami"), "{code}");
        assert!(!code.contains("${HOME}"), "{code}");
    }

    #[test]
    fn the_file_is_appended_not_written() {
        let script = install_script("ssh-ed25519 AAAA rmux");
        assert!(script.contains(">> \"$HOME/.ssh/authorized_keys\""));
        // A single `>` would truncate the file, locking every other machine the
        // operator uses out of this host.
        assert!(!script.contains("' > \"$HOME/.ssh/authorized_keys\""));
    }

    #[test]
    fn permissions_are_set_before_the_write() {
        let script = install_script("ssh-ed25519 AAAA rmux");
        let umask = script.find("umask 077").expect("umask");
        let append = script.find(">>").expect("append");
        assert!(umask < append, "umask must precede the write");
        assert!(script.contains("chmod 700 \"$HOME/.ssh\""));
    }

    #[test]
    fn duplicates_are_refused_by_the_script_itself() {
        let script = install_script("ssh-ed25519 AAAA rmux");
        // `-x` whole line, `-F` literal: a key containing `+` or `/` is not a
        // regex, and a key that is a prefix of another is not a match.
        assert!(script.contains("grep -qxF"));
    }

    #[test]
    fn a_host_alias_cannot_escape_its_filename() {
        let home = Path::new("/home/a");
        let path = key_path(home, "../../etc/evil");
        assert_eq!(path, home.join(".ssh").join("rmux_.._.._etc_evil_ed25519"));
        assert!(!path.to_string_lossy().contains("/etc/"));
    }

    #[test]
    fn ordinary_aliases_stay_readable() {
        let path = key_path(Path::new("/home/a"), "build-box.example.com");
        assert!(path.ends_with("rmux_build-box.example.com_ed25519"));
    }
}
