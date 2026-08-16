//! Getting the agent onto a target, so terminals can outlive the connection.
//!
//! zmux runs `zmuxd attach` *on the target* rather than a login shell. That
//! is what makes a terminal survive: the shell belongs to a daemon the
//! connection cannot reach, so quitting zmux, losing the network or closing the
//! laptop leaves it running. Reattaching is by **name**, so the same terminal tab
//! comes back to the same shell across restarts.
//!
//! For a local target the binary already exists next to zmux. For an SSH target
//! it has to be put there once, which is what this module does.

use std::path::{Path, PathBuf};

use zmux_transport::{CommandSpec, Target, Tty, shell_quote};

/// The agent version this build speaks.
///
/// Part of the installed filename, so a client never attaches to a daemon whose
/// wire format it does not share — the two simply use different paths and
/// different sockets.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the agent lives on a target, and how to run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Path to invoke. Absolute on the remote, or the local binary's path.
    pub program: String,
}

impl Installed {
    /// The command that attaches to `session`, creating it if it does not exist.
    ///
    /// `cwd` is applied by the *daemon* when the session is created, not by the
    /// shell line — a reattach must not `cd` anywhere, or every reconnect would
    /// yank a shell back to the project root from wherever the operator had
    /// navigated to.
    pub fn attach_spec(&self, session: &str, cwd: Option<&str>, cols: u16, rows: u16) -> CommandSpec {
        let mut spec = CommandSpec::new(&self.program)
            .arg("attach")
            .arg("--session")
            .arg(session)
            .arg("--cols")
            .arg(cols.to_string())
            .arg("--rows")
            .arg(rows.to_string())
            .tty(Tty::Allocate);

        if let Some(cwd) = cwd {
            spec = spec.arg("--cwd").arg(cwd);
        }
        spec
    }
}

/// What the remote reported about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePlatform {
    pub os: String,
    pub arch: String,
}

/// `uname -s` / `uname -m` → the Rust target triple whose binary will run there.
///
/// The Linux builds are musl: a statically linked binary has no libc version to
/// disagree with, so one build runs on every Linux from an ancient CentOS to a
/// current Debian. Matching glibc versions across a fleet is exactly the class
/// of problem this avoids.
///
/// **Windows answers `MINGW64_NT-…` or `CYGWIN_NT-…`**, because zmux reaches it
/// through Git for Windows' bash — but the agent that gets installed is a
/// *native* `windows-gnu` binary, not an MSYS one. That distinction matters: the
/// daemon has to outlive the SSH connection, and a process tied to the MSYS
/// runtime that spawned it is a worse bet than a plain Win32 one. `gnu` rather
/// than `msvc` so it cross-compiles from a Mac with no Visual Studio.
pub fn triple_for(platform: &RemotePlatform) -> anyhow::Result<&'static str> {
    let os = platform.os.to_ascii_lowercase();
    let arch = platform.arch.to_ascii_lowercase();

    if os.starts_with("mingw") || os.starts_with("cygwin") || os.starts_with("msys") {
        return Ok(match arch.as_str() {
            "x86_64" | "amd64" => "x86_64-pc-windows-gnu",
            other => anyhow::bail!("no prebuilt zmuxd for Windows {other}"),
        });
    }

    anyhow::ensure!(
        os == "linux",
        "no prebuilt zmuxd for {} — persistent sessions need one",
        platform.os
    );

    Ok(match arch.as_str() {
        "x86_64" | "amd64" => "x86_64-unknown-linux-musl",
        "aarch64" | "arm64" => "aarch64-unknown-linux-musl",
        other => anyhow::bail!("no prebuilt zmuxd for Linux {other}"),
    })
}

/// Whether a triple names a Windows target, and so needs a `.exe`.
pub fn is_windows(triple: &str) -> bool {
    triple.contains("windows")
}

/// Parse the two-line reply from the probe script.
pub fn parse_uname(output: &str) -> anyhow::Result<RemotePlatform> {
    let mut lines = output.lines().map(str::trim).filter(|l| !l.is_empty());
    let os = lines.next().unwrap_or_default().to_owned();
    let arch = lines.next().unwrap_or_default().to_owned();

    anyhow::ensure!(!os.is_empty() && !arch.is_empty(), "could not identify the target: {output:?}");
    Ok(RemotePlatform { os, arch })
}

/// Where the agent is installed on a remote target.
///
/// Takes an **absolute** home rather than writing `$HOME`. Every use of this path
/// is interpolated into a shell line and therefore quoted, and quoting is exactly
/// what stops `$HOME` from expanding — so a `$HOME` here becomes a literal
/// directory named `$HOME`, or a "Directory nonexistent" error. Resolving the
/// home once and passing it down keeps the path safe to quote everywhere.
///
/// Version-stamped, so upgrading zmux installs alongside rather than over — a
/// running daemon from the previous version keeps serving the sessions it owns
/// instead of being replaced underneath them.
pub fn remote_path(home: &str, version: &str, fingerprint: &str) -> String {
    remote_path_for(home, version, fingerprint, "")
}

/// The install path, with the extension the target needs.
///
/// **Windows will not execute a file without `.exe`**, whatever its contents —
/// so an agent installed under the bare fingerprinted name uploads perfectly,
/// reports the right size, and then fails to run with an error that says nothing
/// about extensions. `ipc::socket_stem` already strips `.exe` before deriving
/// the pipe name, so the two builds still agree on where to talk.
pub fn remote_path_for(home: &str, version: &str, fingerprint: &str, triple: &str) -> String {
    let suffix = if is_windows(triple) { ".exe" } else { "" };
    format!(
        "{}/.zmux/bin/zmuxd-{version}-{fingerprint}{suffix}",
        home.trim_end_matches('/')
    )
}

/// A short content hash of the agent binary.
///
/// The version alone is not enough to decide whether an installed agent is the
/// one this build wants. Two agents can share a version and differ — every
/// development build does, and so does any release where the crate version was
/// not bumped alongside a protocol change. The symptom is baffling: the host
/// keeps running the *old* binary, and a newly added flag comes back as
/// "unknown option" from a version number that looks correct.
///
/// Hashing the bytes removes the judgement call. A different binary gets a
/// different path, so it installs; an identical one is found and nothing is
/// uploaded.
///
/// FNV-1a rather than a cryptographic hash: this detects change, it does not
/// defend against a forged binary — an attacker who can write into the install
/// directory has already won.
pub fn fingerprint(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Script that prints the target's home directory.
pub fn home_script() -> String {
    "printf %s \"$HOME\"".to_owned()
}

/// Script that reports the target's OS and architecture.
pub fn probe_script() -> String {
    "uname -s; uname -m".to_owned()
}

/// Script that prints the installed agent's version, or nothing.
///
/// `2>/dev/null` and the `|| true`: a missing binary is the normal first-run
/// case, not a failure, and a non-zero exit here would be reported to the user
/// as an error when the correct response is simply to install it.
pub fn installed_version_script(home: &str, version: &str, fingerprint: &str) -> String {
    installed_version_script_for(home, version, fingerprint, "")
}

pub fn installed_version_script_for(
    home: &str,
    version: &str,
    fingerprint: &str,
    triple: &str,
) -> String {
    let path = shell_quote(&remote_path_for(home, version, fingerprint, triple));
    format!("{path} version 2>/dev/null || true")
}

/// Script that removes agent binaries that are not `keep`.
///
/// Every changed agent installs under a new fingerprinted name, so without this
/// the install directory grows by a megabyte on every upgrade and never shrinks.
///
/// Only binaries with **no running daemon** are removed. On Unix the inode
/// survives an unlink, so deleting a live daemon's binary would not kill it —
/// but it would leave sessions belonging to an agent that can no longer be
/// launched, which is worse than a stale file. Old daemons are left alone to
/// finish serving whatever is still attached to them.
pub fn prune_script(home: &str, keep: &str) -> String {
    let bin = shell_quote(&format!("{}/.zmux/bin", home.trim_end_matches('/')));
    let keep = shell_quote(keep);
    format!(
        r#"for f in {bin}/zmuxd-*; do
  [ -f "$f" ] || continue
  [ "$f" = {keep} ] && continue
  # `[p]` so the pattern does not match this very command line.
  if pgrep -f "$(printf %s "$f" | sed 's/^\(.\)/[\1]/') daemon" > /dev/null 2>&1; then
    continue
  fi
  rm -f "$f"
done"#
    )
}

/// Script that receives the binary on stdin and installs it atomically.
///
/// Written to a temporary name and then `mv`d into place. This is the one spot
/// where `mv` is correct rather than forbidden: the rule against it protects a
/// *user's* file, whose inode carries permissions, ownership and hard links. Here
/// the target is a binary zmux owns entirely, and the risk being defended against
/// is the opposite one — a half-written executable being run by a second window
/// that connects while the upload is still in flight.
pub fn install_script(home: &str, version: &str, fingerprint: &str) -> String {
    install_script_for(home, version, fingerprint, "")
}

pub fn install_script_for(
    home: &str,
    version: &str,
    fingerprint: &str,
    triple: &str,
) -> String {
    let home = home.trim_end_matches('/');
    let path = remote_path_for(home, version, fingerprint, triple);
    // `$$` is left unquoted deliberately — it must expand to the shell's pid so
    // two concurrent installs cannot collide on one temporary file.
    let tmp = format!("{}.partial", shell_quote(&path));
    format!(
        "mkdir -p {bin} && chmod 700 {dir} && \
         cat > {tmp}.$$ && chmod 755 {tmp}.$$ && mv -f {tmp}.$$ {path}",
        bin = shell_quote(&format!("{home}/.zmux/bin")),
        dir = shell_quote(&format!("{home}/.zmux")),
        tmp = tmp,
        path = shell_quote(&path),
    )
}

/// Ensure the agent is present on `target`, and return how to run it.
pub async fn ensure<T: Target + ?Sized>(target: &T, binaries: &dyn BinarySource) -> anyhow::Result<Installed> {
    // Local: the agent ships beside zmux, so there is nothing to install.
    if matches!(target.id(), zmux_transport::TargetId::Local)
        && let Some(local) = binaries.local_agent()
    {
        return Ok(Installed { program: local.to_string_lossy().into_owned() });
    }

    // Resolved once, then used for every path — see `remote_path`.
    let home = run(target, &home_script()).await?;
    let home = home.trim();
    anyhow::ensure!(
        home.starts_with('/'),
        "the target reported a home directory of {home:?}"
    );

    // The binary has to be identified before the path can be, because the path
    // contains its fingerprint — that is what makes "is it already there?"
    // answerable without trusting a version number.
    let platform = parse_uname(&run(target, &probe_script()).await?)?;
    let triple = triple_for(&platform)?;
    let bytes = binaries.agent_for(triple)?;
    let fingerprint = fingerprint(&bytes);
    let path = remote_path_for(home, VERSION, &fingerprint, triple);

    // Already installed? The common case, and one round trip.
    let probe =
        run(target, &installed_version_script_for(home, VERSION, &fingerprint, triple)).await?;
    if probe.trim() == VERSION {
        return Ok(Installed { program: path });
    }

    let spec = CommandSpec::new("sh")
        .arg("-c")
        .arg(install_script_for(home, VERSION, &fingerprint, triple))
        .tty(Tty::None);
    let out = target.exec_with_input(&spec, &bytes).await?;
    anyhow::ensure!(
        out.status == 0,
        "could not install the zmuxd: {}",
        out.stderr.trim()
    );

    // Verify rather than assume: a truncated upload produces a file that exists,
    // is executable, and fails in a way that would otherwise surface much later
    // as an unexplained terminal that will not open.
    let after = run(target, &installed_version_script(home, VERSION, &fingerprint)).await?;
    anyhow::ensure!(
        after.trim() == VERSION,
        "the uploaded agent reported {:?}, expected {VERSION}",
        after.trim()
    );

    // Housekeeping, after the new one is known good.
    let _ = run(target, &prune_script(home, &path)).await;

    Ok(Installed { program: path })
}

async fn run<T: Target + ?Sized>(target: &T, script: &str) -> anyhow::Result<String> {
    let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None);
    let out = target.exec(&spec).await?;
    Ok(out.stdout)
}

/// Where the prebuilt agent binaries come from.
///
/// A trait so the provisioning logic can be tested without a filesystem layout
/// or a real host, and so the app can resolve them from its bundle while a
/// development build reads them out of `target/`.
pub trait BinarySource: Send + Sync {
    /// The agent for a target triple.
    fn agent_for(&self, triple: &str) -> anyhow::Result<Vec<u8>>;
    /// The agent for *this* machine, if it is available.
    fn local_agent(&self) -> Option<PathBuf>;
}

/// Reads binaries from a directory of `zmuxd-<triple>` files.
pub struct DirectorySource {
    pub dir: PathBuf,
    pub local: Option<PathBuf>,
}

impl BinarySource for DirectorySource {
    fn agent_for(&self, triple: &str) -> anyhow::Result<Vec<u8>> {
        let path = self.dir.join(format!("zmuxd-{triple}"));
        std::fs::read(&path).map_err(|e| {
            anyhow::anyhow!(
                "no agent binary for {triple} at {} ({e}). Run scripts/build-agents.sh",
                path.display()
            )
        })
    }

    fn local_agent(&self) -> Option<PathBuf> {
        self.local.as_ref().filter(|p| p.exists()).cloned()
    }
}

/// The directory holding prebuilt agents, and the local agent, for this build.
///
/// In a bundled app both sit next to the executable; in a development build they
/// are wherever cargo put them. Checked in order so a developer running
/// `cargo run` gets the freshly built agent rather than a stale bundled one.
pub fn default_source(resource_dir: Option<&Path>, exe_dir: Option<&Path>) -> DirectorySource {
    let local_name = if cfg!(windows) { "zmuxd.exe" } else { "zmuxd" };

    let candidates: Vec<PathBuf> = [exe_dir, resource_dir]
        .into_iter()
        .flatten()
        .map(|d| d.to_path_buf())
        .collect();

    // **Both `<dir>/zmuxd` and `<dir>/agents/zmuxd`**, and the second
    // one is not a nicety — without it a session on *this Mac* could not be
    // persistent at all.
    //
    // `scripts/build-agents.sh` writes the host build to `src-tauri/agents/`
    // alongside the cross-compiled ones, and `tauri.conf.json` ships that whole
    // directory as `resources`, so in a bundle it lands at
    // `Contents/Resources/agents/zmuxd`. Only the bare `<dir>/zmuxd`
    // was checked, which is where a `cargo run` build puts it and nowhere a
    // bundle ever does. So `local` was `None` in every shipped app, `ensure`
    // fell through to the *remote install* path, and `triple_for` — which only
    // knows Linux and Windows, because those are the platforms an agent is
    // cross-compiled for — refused with "no prebuilt zmuxd for Darwin".
    //
    // The message sent the reader after a missing cross-compilation target,
    // when the binary was sitting in the bundle the whole time and the local
    // case needs no upload at all.
    let local = candidates
        .iter()
        .flat_map(|d| [d.join(local_name), d.join("agents").join(local_name)])
        .find(|p| p.exists());
    let dir = candidates
        .iter()
        .map(|d| d.join("agents"))
        .find(|p| p.exists())
        .or_else(|| candidates.first().cloned())
        .unwrap_or_else(|| PathBuf::from("agents"));

    DirectorySource { dir, local }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_NAME: &str = if cfg!(windows) { "zmuxd.exe" } else { "zmuxd" };

    /// A scratch directory of our own. The pid keeps two concurrent test
    /// binaries from sharing one, which is the same reasoning `zmux-fs`'s tests
    /// use.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zmux-provision-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The shape a real bundle has: the agents are a resource *directory*, so
    /// the host build is one level deeper than the naive lookup expected.
    ///
    /// This is the regression that made a local Mac session refuse to start
    /// with a message about a missing cross-compilation target — while the
    /// binary it wanted was inside the app.
    #[test]
    fn the_bundled_local_agent_is_found_inside_the_resources_directory() {
        let root = scratch("bundle-local");
        let resources = root.join("Resources");
        let exe = root.join("MacOS");
        std::fs::create_dir_all(resources.join("agents")).unwrap();
        std::fs::create_dir_all(&exe).unwrap();

        let bundled = resources.join("agents").join(LOCAL_NAME);
        std::fs::write(&bundled, b"host build").unwrap();

        let source = default_source(Some(&resources), Some(&exe));
        assert_eq!(
            source.local.as_deref(),
            Some(bundled.as_path()),
            "a bundle keeps the host agent in Resources/agents; missing it sends `ensure` \
             down the remote-install path, which has no Darwin target"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A `cargo run` build puts the agent straight beside the executable, and
    /// that copy must keep winning — otherwise a developer silently tests
    /// against whatever stale binary the last bundle left behind.
    #[test]
    fn a_binary_beside_the_executable_beats_the_bundled_one() {
        let root = scratch("bundle-fresh");
        let resources = root.join("Resources");
        let exe = root.join("MacOS");
        std::fs::create_dir_all(resources.join("agents")).unwrap();
        std::fs::create_dir_all(&exe).unwrap();

        std::fs::write(resources.join("agents").join(LOCAL_NAME), b"bundled").unwrap();
        let beside = exe.join(LOCAL_NAME);
        std::fs::write(&beside, b"fresh").unwrap();

        let source = default_source(Some(&resources), Some(&exe));
        assert_eq!(source.local.as_deref(), Some(beside.as_path()));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn linux_architectures_map_to_static_musl_builds() {
        for (arch, want) in [
            ("x86_64", "x86_64-unknown-linux-musl"),
            ("amd64", "x86_64-unknown-linux-musl"),
            ("aarch64", "aarch64-unknown-linux-musl"),
            ("arm64", "aarch64-unknown-linux-musl"),
        ] {
            let platform = RemotePlatform { os: "Linux".into(), arch: arch.into() };
            assert_eq!(triple_for(&platform).unwrap(), want, "for {arch}");
        }
    }

    #[test]
    fn an_unsupported_target_is_named_rather_than_guessed() {
        // Silently shipping an x86 binary to something else produces "cannot
        // execute binary file" from a shell the user never asked to run.
        let platform = RemotePlatform { os: "Linux".into(), arch: "riscv64".into() };
        let error = triple_for(&platform).unwrap_err().to_string();
        assert!(error.contains("riscv64"), "got {error}");

        let platform = RemotePlatform { os: "FreeBSD".into(), arch: "x86_64".into() };
        assert!(triple_for(&platform).unwrap_err().to_string().contains("FreeBSD"));
    }

    #[test]
    fn uname_output_is_parsed_in_order() {
        let platform = parse_uname("Linux\nx86_64\n").unwrap();
        assert_eq!(platform, RemotePlatform { os: "Linux".into(), arch: "x86_64".into() });
    }

    #[test]
    fn a_truncated_probe_is_an_error_not_an_empty_platform() {
        // An empty arch would fall through to "no prebuilt agent for Linux ",
        // which reads like a missing build rather than a failed probe.
        assert!(parse_uname("Linux\n").is_err());
        assert!(parse_uname("").is_err());
    }

    #[test]
    fn the_install_path_is_version_stamped() {
        // Upgrading zmux must not overwrite the binary a running daemon came
        // from — that daemon still owns live sessions.
        assert!(remote_path("/home/x", "9.9.9", "abc").contains("9.9.9"));
        assert!(install_script("/home/x", "9.9.9", "abc").contains("9.9.9"));
    }

    #[test]
    fn the_install_never_writes_the_final_path_directly() {
        // A partially uploaded binary at the real path would be executed by the
        // next window to connect.
        let script = install_script("/home/x", VERSION, "abc123");
        let final_path = remote_path("/home/x", VERSION, "abc123");
        assert!(script.contains("mv -f"), "nothing is moved into place: {script}");

        // Compare the redirect *target*, not a substring: the temporary name has
        // the final path as its prefix, so `contains` would pass for both the
        // safe and the unsafe form and prove nothing.
        let target = script
            .split("cat > ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .expect("the script should redirect into a file");

        assert_ne!(target, final_path, "writes straight to the live path: {script}");
        assert!(
            target.starts_with(&final_path) && target.contains("partial"),
            "the temporary file is not beside the final one: {target}"
        );
        // Same directory, so the `mv` is a rename rather than a copy across
        // filesystems — which would not be atomic.
        assert_eq!(
            std::path::Path::new(target).parent(),
            std::path::Path::new(&final_path).parent(),
            "the temporary file is on a different filesystem: {target}"
        );
    }

    #[test]
    fn pruning_keeps_the_binary_just_installed() {
        // Deleting the one about to be run would be a spectacular own goal.
        let keep = remote_path("/home/x", VERSION, "keepme");
        let script = prune_script("/home/x", &keep);
        assert!(script.contains(&keep), "the kept path is not named: {script}");
        assert!(script.contains("continue"), "nothing is skipped: {script}");
    }

    #[test]
    fn pruning_leaves_binaries_with_a_running_daemon() {
        // Removing the binary of a live daemon strands the sessions it owns:
        // they keep running but can never be launched again.
        let script = prune_script("/home/x", "/home/x/.zmux/bin/zmuxd-1-a");
        assert!(script.contains("pgrep"), "no liveness check: {script}");
    }

    #[test]
    fn pruning_only_touches_agent_binaries() {
        // The directory is ours, but a glob mistake here deletes a user's files.
        let script = prune_script("/home/x", "keep");
        assert!(script.contains("zmuxd-*"), "{script}");
        assert!(!script.contains("rm -rf"), "recursive delete in a prune: {script}");
    }

    #[test]
    fn a_changed_binary_installs_to_a_different_path() {
        // The bug this prevents: a rebuilt agent that kept its version number is
        // never uploaded, so the host keeps running the old one and a newly
        // added flag comes back as "unknown option".
        let old = fingerprint(b"agent v1 bytes");
        let new = fingerprint(b"agent v2 bytes");
        assert_ne!(old, new);
        assert_ne!(
            remote_path("/home/x", VERSION, &old),
            remote_path("/home/x", VERSION, &new),
            "two different binaries share an install path"
        );
    }

    #[test]
    fn an_identical_binary_keeps_its_path() {
        // The other half: provisioning must not re-upload on every launch.
        assert_eq!(fingerprint(b"same bytes"), fingerprint(b"same bytes"));
    }

    #[test]
    fn attach_passes_the_session_name_and_size() {
        let installed = Installed { program: "/opt/zmuxd".into() };
        let spec = installed.attach_spec("term-7", Some("/srv/app"), 120, 40);

        assert_eq!(spec.program, "/opt/zmuxd");
        assert!(spec.args.iter().any(|a| a == "term-7"), "{:?}", spec.args);
        assert!(spec.args.iter().any(|a| a == "/srv/app"), "{:?}", spec.args);
        assert!(spec.args.iter().any(|a| a == "120"), "{:?}", spec.args);
        // A terminal without a TTY gives a shell with no job control, no line
        // editing and no prompt — the classic "it works but feels broken".
        assert!(matches!(spec.tty, Tty::Allocate));
    }

    #[test]
    fn reattaching_without_a_cwd_does_not_move_the_shell() {
        // The daemon applies cwd only when creating. Passing it on every attach
        // would still be wrong here: it would announce an intent to move.
        let installed = Installed { program: "zmuxd".into() };
        let spec = installed.attach_spec("term-7", None, 80, 24);
        assert!(!spec.args.iter().any(|a| a == "--cwd"), "{:?}", spec.args);
    }

    #[test]
    fn the_version_probe_never_fails_on_a_missing_binary() {
        // First run has no binary. If this script exited non-zero, provisioning
        // would report an error instead of installing.
        let script = installed_version_script("/home/x", "1.0.0", "abc");
        assert!(script.contains("|| true"), "{script}");
        assert!(script.contains("2>/dev/null"), "{script}");
    }
}

#[cfg(test)]
mod windows_tests {
    use super::*;

    #[test]
    fn a_windows_host_gets_a_native_windows_agent() {
        // zmux reaches Windows through Git Bash, so `uname -s` answers
        // `MINGW64_NT-…` — but the agent installed there is a native Win32
        // binary, not an MSYS one. The daemon has to outlive the connection that
        // started it, and a process tied to the MSYS runtime is a worse bet.
        for os in ["MINGW64_NT-10.0-26200", "CYGWIN_NT-10.0", "MSYS_NT-10.0"] {
            let platform = RemotePlatform { os: os.into(), arch: "x86_64".into() };
            assert_eq!(triple_for(&platform).unwrap(), "x86_64-pc-windows-gnu", "{os}");
        }
    }

    #[test]
    fn a_windows_agent_is_installed_with_an_exe_extension() {
        // Windows will not execute a file without it, whatever the contents —
        // so the upload succeeds, the size is right, and running it fails with
        // an error that never mentions extensions.
        let win = remote_path_for("/c/Users/dev", "0.1.0", "abc123", "x86_64-pc-windows-gnu");
        assert!(win.ends_with(".exe"), "{win}");

        // And Unix must not gain one.
        let unix = remote_path_for("/home/x", "0.1.0", "abc123", "x86_64-unknown-linux-musl");
        assert!(!unix.ends_with(".exe"), "{unix}");
        assert_eq!(unix, remote_path("/home/x", "0.1.0", "abc123"));
    }

    #[test]
    fn the_exe_reaches_the_install_and_probe_scripts_too() {
        // A path that gains `.exe` in one place and not another installs to one
        // name and is then looked for under a different one — which reads as
        // "the agent is never already installed" and re-uploads on every launch.
        let triple = "x86_64-pc-windows-gnu";
        assert!(install_script_for("/c/Users/Y", "0.1.0", "fp", triple).contains(".exe"));
        assert!(installed_version_script_for("/c/Users/Y", "0.1.0", "fp", triple).contains(".exe"));
    }

    #[test]
    fn the_socket_name_still_matches_across_the_extension() {
        // `ipc::socket_stem` strips `.exe`, so the client and the daemon derive
        // the same pipe name from the same install. If they ever disagreed, the
        // client would start a second daemon and every session would double —
        // the bug that already cost a day.
        let path = remote_path_for("/c/Users/Y", "0.1.0", "abc123", "x86_64-pc-windows-gnu");
        let name = path.rsplit('/').next().unwrap();
        assert_eq!(name, "zmuxd-0.1.0-abc123.exe");
        assert_eq!(name.strip_suffix(".exe").unwrap(), "zmuxd-0.1.0-abc123");
    }

    #[test]
    fn an_unknown_windows_architecture_is_refused_rather_than_guessed() {
        let platform = RemotePlatform { os: "MINGW64_NT-10.0".into(), arch: "arm64".into() };
        assert!(triple_for(&platform).is_err(), "arm64 Windows has no prebuilt agent yet");
    }
}
