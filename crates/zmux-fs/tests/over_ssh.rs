//! Verifies the remote filesystem through the **real SSH command path**.
//!
//! The unit tests drive `TargetFs` over a `LocalTarget`, which exercises the
//! shell scripts but skips everything `SshTarget` does: building the `ssh` argv,
//! ordering the multiplexing options, terminating them with `--`, folding the
//! request into a single shell line the far side re-parses, and piping stdin
//! through for writes. Those are exactly the places a remote-only bug hides.
//!
//! There is no sshd available here, so the test substitutes a **fake `ssh`
//! binary** on `PATH` that takes the final argument — the shell line zmux built —
//! and runs it locally. Everything up to and including the remote shell's
//! re-parsing is therefore genuinely exercised.
//!
//! What this does NOT cover, and must not be read as covering: authentication,
//! `ControlMaster` multiplexing, `~/.ssh/config` resolution, or network
//! behaviour. It proves the command zmux constructs is correct and that the far
//! side does the right thing with it.

use zmux_fs::{FileSystem, TargetFs};
use zmux_ssh::SshTarget;
use zmux_transport::SshHostId;

/// Install a fake `ssh` on `PATH` and return the directory holding it.
///
/// The stand-in mirrors what OpenSSH does with a remote command: everything
/// before the destination is options, and the trailing argument is handed to the
/// login shell as one string.
fn install_fake_ssh(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zmux-fakessh-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let script = r#"#!/bin/sh
# Stand-in for the ssh binary. Options are ignored; the last argument is the
# shell line zmux built, which a real remote login shell would re-parse the same
# way. stdin passes straight through, so piped writes behave as they would.
for a in "$@"; do last="$a"; done
exec sh -c "$last"
"#;

    let path = dir.join("ssh");
    std::fs::write(&path, script).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    dir
}

/// Point `PATH` at the fake ssh for the duration of a test.
///
/// `PATH` is process-wide, and cargo runs `#[test]` functions on parallel
/// threads — so two of these guards overlapping means one restores `PATH` while
/// another test is still spawning, and that test silently gets the *real* `ssh`.
/// That is not hypothetical: it is exactly how this file failed the first time,
/// with `Could not resolve hostname fake-host`. Everything therefore lives in a
/// single `#[test]` below.
struct FakeSshGuard {
    dir: std::path::PathBuf,
    original_path: Option<std::ffi::OsString>,
}

impl FakeSshGuard {
    fn install(name: &str) -> Self {
        let dir = install_fake_ssh(name);
        let original_path = std::env::var_os("PATH");

        let mut new_path = std::ffi::OsString::from(&dir);
        if let Some(existing) = &original_path {
            new_path.push(":");
            new_path.push(existing);
        }
        // SAFETY: single-threaded within this test binary; see the note above.
        unsafe { std::env::set_var("PATH", &new_path) };

        Self { dir, original_path }
    }
}

impl Drop for FakeSshGuard {
    fn drop(&mut self) {
        match &self.original_path {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn workspace(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("zmux-ssh-fs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Everything zmux does to a remote machine, over the ssh command path.
///
/// One test, not several, because the `PATH` override is process-wide — see the
/// note on `FakeSshGuard`.
#[test]
fn a_remote_machine_can_be_browsed_edited_and_measured() {
    let _guard = FakeSshGuard::install("full");
    let work = workspace("full");

    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

    runtime.block_on(async {
        let fs = TargetFs::new(SshTarget::new(SshHostId::new("fake-host")));

        // --- listing, including names that break naive protocols -------------
        std::fs::write(work.join("plain.txt"), "one\n").unwrap();
        std::fs::write(work.join("two words.txt"), "two\n").unwrap();
        std::fs::create_dir(work.join("src")).unwrap();

        let entries = fs.list_dir(&work.to_string_lossy()).await.expect("list over ssh");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        assert_eq!(names, vec!["src", "plain.txt", "two words.txt"]);

        // --- read -------------------------------------------------------------
        let file = work.join("plain.txt").to_string_lossy().into_owned();
        match fs.read_file(&file).await.expect("read over ssh") {
            zmux_fs::FileContent::Text { text } => assert_eq!(text, "one\n"),
            other => panic!("expected text, got {other:?}"),
        }

        // --- write: the path that pipes stdin through ssh ---------------------
        fs.write_file(&file, "rewritten\nfrom zmux\n").await.expect("write over ssh");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "rewritten\nfrom zmux\n");

        // --- a path containing a space, end to end ----------------------------
        let spaced = work.join("two words.txt").to_string_lossy().into_owned();
        fs.write_file(&spaced, "still fine\n").await.expect("write to a spaced path");
        assert_eq!(std::fs::read_to_string(&spaced).unwrap(), "still fine\n");

        // --- create / rename / delete -----------------------------------------
        let made = work.join("made.txt").to_string_lossy().into_owned();
        let moved = work.join("moved.txt").to_string_lossy().into_owned();

        fs.create_file(&made).await.expect("create over ssh");
        assert!(std::path::Path::new(&made).exists());

        // The clobber guards must hold over ssh too.
        assert!(fs.create_file(&made).await.is_err(), "create must refuse to overwrite");

        fs.rename(&made, &moved).await.expect("rename over ssh");
        assert!(!std::path::Path::new(&made).exists());
        assert!(std::path::Path::new(&moved).exists());

        fs.delete(&moved).await.expect("delete over ssh");
        assert!(!std::path::Path::new(&moved).exists());

        // --- home directory: what the tree opens on ---------------------------
        let home = fs.home_dir().await.expect("home over ssh");
        assert!(home.starts_with('/'), "expected an absolute home, got {home:?}");

        // --- saving must not replace the inode --------------------------------
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let secret = work.join("secret.env").to_string_lossy().into_owned();
            std::fs::write(&secret, "TOKEN=1").unwrap();
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();

            fs.write_file(&secret, "TOKEN=2").await.expect("write over ssh");

            let mode = std::fs::metadata(&secret).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "a remote save changed the file mode: {mode:o}");
            assert_eq!(std::fs::read_to_string(&secret).unwrap(), "TOKEN=2");
        }
    });

    let _ = std::fs::remove_dir_all(&work);
}
