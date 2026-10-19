//! **Every existing feature must keep going over ssh.**
//!
//! mosh is opt-in and interactive-only, and the rule that keeps it that way is
//! `Tty::None` — one line in `SshTarget::build_command`. That protection is
//! real but it was *implicit*: nothing said out loud that a file save, an image
//! paste, a search or a metrics sample depends on it, so a later change that
//! flipped one of those specs to `Tty::Allocate` would have broken it silently
//! and for a reason nobody would connect to mosh.
//!
//! mosh is a **terminal emulator**. It delivers a rendered screen, not a
//! command's bytes, and it has no binary-safe stdin channel. A megabyte of PNG
//! or a NUL-delimited `grep` stream sent through it is not a failure, it is
//! *corruption* — which is the worst shape a regression can take here, because
//! the file listing still looks like a file listing.
//!
//! ## Why this is one table and not one test per feature
//!
//! A per-feature list is a list someone forgets to extend. So the features are
//! **driven**, through their own public entry points, against a recording
//! target — and every spec they produce, whatever it is, is then resolved by a
//! real `SshTarget` whose host is marked as mosh-capable. Anything a feature
//! adds later is caught by the same table without anyone remembering to add it.
//!
//! What is *not* reachable from here — `src-tauri`'s image paste, `rmux-agent
//! kill`/`list`/`setenv` — is covered by the floor asserted at the bottom:
//! `exec` and `exec_with_input` overwrite the TTY request themselves, so a
//! one-shot cannot reach mosh even if its author asked for one.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rmux_ssh::{SshTarget, mosh};
use rmux_transport::{
    CommandSpec, Output, Platform, ResolvedCommand, SshHostId, Target, TargetId, Tty,
};

/// Everything a feature asked the target to do.
#[derive(Default)]
struct Log {
    /// `(what it was, the spec, how many bytes rode stdin)`.
    calls: Mutex<Vec<(&'static str, CommandSpec, Option<usize>)>>,
}

/// A target that runs nothing and remembers everything.
#[derive(Clone)]
struct Recorder {
    id: TargetId,
    log: Arc<Log>,
}

impl Recorder {
    fn new() -> Self {
        Self { id: TargetId::Local, log: Arc::new(Log::default()) }
    }

    fn record(&self, what: &'static str, spec: &CommandSpec, input: Option<usize>) {
        self.log.calls.lock().unwrap().push((what, spec.clone(), input));
    }
}

#[async_trait]
impl Target for Recorder {
    fn id(&self) -> &TargetId {
        &self.id
    }

    fn build_command(&self, spec: &CommandSpec) -> anyhow::Result<ResolvedCommand> {
        self.record("build_command", spec, None);
        Ok(ResolvedCommand {
            program: "sh".into(),
            args: Vec::new(),
            env: Default::default(),
        })
    }

    async fn exec(&self, spec: &CommandSpec) -> anyhow::Result<Output> {
        self.record("exec", spec, None);
        Ok(Output { status: 0, stdout: String::new(), stderr: String::new() })
    }

    async fn exec_with_input(&self, spec: &CommandSpec, input: &[u8]) -> anyhow::Result<Output> {
        self.record("exec_with_input", spec, Some(input.len()));
        Ok(Output { status: 0, stdout: String::new(), stderr: String::new() })
    }

    fn platform(&self) -> Option<Platform> {
        Some(Platform::Linux)
    }
}

/// Says yes to everything, so the test runs against the *worst* case: a host
/// where mosh is genuinely available and would be used if the rule slipped.
struct AlwaysAvailable;

#[async_trait]
impl mosh::Probe for AlwaysAvailable {
    fn local_client(&self) -> bool {
        true
    }
    async fn remote_server(&self) -> mosh::ServerProbe {
        // Current, so nothing here starts a background upgrade attempt.
        mosh::ServerProbe::Present(Some(mosh::MIN_SERVER_VERSION))
    }
}

/// An ssh target for a host already known to support mosh.
async fn mosh_capable_host(alias: &str) -> SshTarget {
    let host = SshHostId::new(alias);
    mosh::availability_for(&host, &AlwaysAvailable).await;
    assert_eq!(mosh::cached(&host), Some(mosh::Availability::Available));
    SshTarget::new(host)
}

/// Drive every feature reachable from this crate's dev-dependencies.
///
/// The bodies are ignored — several of these parse output this target does not
/// produce. What is being collected is the *specs*, which is the thing under
/// test.
async fn every_feature_spec() -> Vec<(&'static str, CommandSpec, Option<usize>)> {
    let rec = Recorder::new();

    // --- the filesystem: listings, reads, saves, uploads, search -----------
    {
        use rmux_fs::FileSystem;
        let fs = rmux_fs::TargetFs::new(rec.clone());

        let _ = fs.list_dir("~/project").await;
        let _ = fs.home_dir().await;
        let _ = fs.read_file("~/project/notes.md").await;
        let _ = fs.read_preview("~/project/logo.png").await;
        // Saving copies over the original; it never `mv`s onto it.
        let _ = fs.write_file("~/project/notes.md", "hello").await;
        let _ = fs.create_file("~/project/new.txt").await;
        let _ = fs.create_dir("~/project/sub").await;
        let _ = fs.rename("~/project/a", "~/project/b").await;
        let _ = fs.delete("~/project/b").await;
        // Upload: bytes over **stdin**, refusing to clobber with `set -C`.
        let _ = fs.upload("~/project/shot.png", &[0u8, 159, 146, 150, 0, 10]).await;
        let _ = fs
            .search("~/project", &rmux_fs::SearchQuery {
                text: "needle".to_owned(),
                case_sensitive: false,
                regex: false,
            })
            .await;
    }

    // --- metrics: procfs sampling, the process list, and a kill ------------
    {
        let mut collector = rmux_metrics::Collector::new();
        let _ = collector.sample(&rec).await;
        let _ = collector.processes(&rec, rmux_metrics::SortBy::Cpu, 5).await;
        let _ = collector.kill(&rec, 4242, false).await;
    }

    let mut calls = rec.log.calls.lock().unwrap().clone_specs();

    // --- the agent's terminal attach: the one thing that may use mosh ------
    //
    // Included in the same table so its shape is pinned beside the others
    // rather than in a test of its own.
    let installed = rmux_agent::provision::Installed { program: "/root/.rmux/bin/rmux-agent".into() };
    calls.push(("agent attach", installed.attach_spec("tab-7", Some("~/project"), 120, 40), None));

    // The two agent-hosted launch shapes exactly as `src-tauri`'s `claude_start`
    // and `pi_start` build them: the same attach spec with a `--login-command`.
    // **The opt-in must not become an opt-out.** `low_latency` is set at those
    // call sites and only there, guarded by a per-session preference that is
    // absent for every session saved before it existed — so the spec a launch
    // produces with nobody asking has to carry `false`, and this is where that
    // is pinned rather than trusted.
    for (what, line) in [
        ("claude launch", "claude --resume abc"),
        ("pi launch", "pi --tui-mode regular"),
    ] {
        calls.push((
            what,
            installed
                .attach_spec("claude-7", Some("~/project"), 120, 40)
                .arg("--login-command")
                .arg(line),
            None,
        ));
    }

    calls
}

/// Small helper so the lock guard is not held across an await.
trait CloneSpecs {
    fn clone_specs(&self) -> Vec<(&'static str, CommandSpec, Option<usize>)>;
}

impl CloneSpecs for Vec<(&'static str, CommandSpec, Option<usize>)> {
    fn clone_specs(&self) -> Vec<(&'static str, CommandSpec, Option<usize>)> {
        self.iter().map(|(w, s, i)| (*w, s.clone(), *i)).collect()
    }
}

/// **The table.** Every spec every reachable feature produces, resolved by a
/// real ssh target against a host where mosh *is* available.
#[tokio::test]
async fn every_existing_feature_still_resolves_to_ssh() {
    let target = mosh_capable_host("pinning-host").await;
    let specs = every_feature_spec().await;

    assert!(specs.len() >= 15, "the features stopped producing commands: {}", specs.len());

    let mut interactive = 0;
    for (what, spec, _) in &specs {
        // Nothing in this table asks for low latency. That is a property of the
        // call sites, and it is asserted rather than assumed: a feature that
        // starts asking for it has to come past this test.
        assert!(!spec.low_latency, "{what} asked for low latency: {spec:?}");

        match spec.tty {
            Tty::None => {
                let resolved = target.build_command(spec).unwrap();
                assert_eq!(
                    resolved.program, "ssh",
                    "{what} left the ssh path; mosh renders a screen, not this command's bytes"
                );
                // Even if someone later sets the flag on it, the TTY rule holds.
                let resolved =
                    target.build_command(&spec.clone().low_latency(true)).unwrap();
                assert_eq!(resolved.program, "ssh", "{what} reached mosh with a one-shot");
            }
            Tty::Allocate => interactive += 1,
        }
    }

    // The interactive specs in the table are the agent attach and the two agent
    // launches built on it. Everything else a feature does is a one-shot whose
    // stdout is parsed.
    assert_eq!(interactive, 3, "an unexpected feature became interactive: {specs:?}");

    // Said again, in the other direction and by name: the attach spec is the
    // one thing that *may* go through mosh, so it is the one thing whose
    // default has to be checked rather than inferred. Resolved against a host
    // where mosh is genuinely available, so a default that had flipped would
    // resolve to `mosh` here and fail.
    let attach = installed_attach();
    assert!(!attach.low_latency, "the attach spec started asking for low latency by default");
    assert_eq!(
        target.build_command(&attach).unwrap().program,
        "ssh",
        "a session nobody configured left the ssh path, and mosh has no scrollback"
    );
}

/// The attach spec as every spawn path starts from it.
fn installed_attach() -> CommandSpec {
    let installed =
        rmux_agent::provision::Installed { program: "/root/.rmux/bin/rmux-agent".into() };
    installed.attach_spec("tab-7", Some("~/project"), 120, 40)
}

/// The payload-carrying paths, called out by name because corruption there is
/// silent. An upload and an image paste are the same shape: base64 over
/// **stdin**, because `ARG_MAX` caps one argument at 128 KiB and a screenshot
/// is routinely a megabyte.
#[tokio::test]
async fn anything_carrying_bytes_rides_stdin_and_stays_on_ssh() {
    let target = mosh_capable_host("pinning-stdin").await;
    let specs = every_feature_spec().await;

    let piped: Vec<_> = specs.iter().filter(|(_, _, bytes)| bytes.is_some()).collect();
    assert!(!piped.is_empty(), "no feature piped anything; the upload path changed shape");

    for (what, spec, bytes) in piped {
        assert!(bytes.unwrap() > 0, "{what} piped nothing");
        assert_eq!(spec.tty, Tty::None, "{what} must stay a one-shot");
        assert_eq!(target.build_command(spec).unwrap().program, "ssh", "{what}");
    }
}

/// **The floor.** Whatever a caller asks for, `exec` and `exec_with_input`
/// rebuild the spec with `Tty::None` before resolving it — which is what keeps
/// the image paste, `rmux-agent kill`/`list`/`setenv` and every other one-shot
/// that this crate cannot import on the ssh path. Asserted over the source
/// because it is a property of two functions and no test that runs a command
/// can see it.
#[test]
fn a_one_shot_cannot_opt_into_a_terminal_emulator() {
    let source = include_str!("../src/lib.rs");

    for f in ["async fn exec(", "async fn exec_with_input(", "async fn exec_with_input_file("] {
        let body = source.split(f).nth(1).unwrap_or_else(|| panic!("{f} is gone"));
        let head: String = body.chars().take(400).collect();
        assert!(
            head.contains("spec.clone().tty(Tty::None)"),
            "{f} stopped forcing Tty::None, so a one-shot could reach mosh:\n{head}"
        );
    }
}

/// **Port forwarding never goes through a `Target` at all**, and must not start
/// to: mosh has no port forwarding of any kind, so a `-L` tunnel or a `-D`
/// SOCKS proxy routed through the new branch would simply stop existing.
/// `forward.rs` spawns `ssh` directly, on its own unmultiplexed connection —
/// which is exactly why it is safe, and why this asserts it stays that way.
#[test]
fn forwards_and_the_socks_proxy_spawn_ssh_directly() {
    let source = include_str!("../src/forward.rs");

    assert!(!source.contains("build_command"), "a forward started going through a Target");
    assert!(!source.contains("low_latency"), "a forward started asking for a transport");
    assert!(!source.contains("mosh"), "a forward must never reach mosh");
    assert!(source.contains("Command::new(\"ssh\")"), "a forward must be a plain ssh");
    // Unchanged and still load-bearing: a bare `-D <port>` binds per the host's
    // GatewayPorts, and a SOCKS proxy into the operator's infrastructure that
    // the LAN can reach is a hole.
    assert!(source.contains("127.0.0.1:{port}"), "the SOCKS proxy stopped binding to loopback");
}

/// **The image paste is the one that cannot be driven from here**, because it
/// lives in `src-tauri`. It is also the one where a regression would be worst:
/// a screenshot is routinely a megabyte, `ARG_MAX` caps a single argument at
/// 128 KiB, and mosh has no binary-safe stdin — so the bytes have to keep
/// going through `exec_with_input` on a `Tty::None` spec, which is the ssh
/// path by the rule above. Asserted over its source, because that is the only
/// place this crate can reach it.
#[test]
fn the_image_paste_still_pipes_its_bytes_through_a_one_shot() {
    let source = include_str!("../../../src-tauri/src/paste.rs");

    assert!(source.contains("exec_with_input"), "the paste stopped piping its bytes");
    assert!(source.contains("Tty::None"), "the paste stopped being a one-shot");
    assert!(!source.contains("low_latency"), "the paste asked for a transport");
    // The path is typed into the composer, never submitted — unchanged, and
    // restated here so a rewrite of this file has to come past it.
    assert!(source.contains("claude_paste_image"), "the entry point was renamed");
}

/// The terminal attach is the **only** thing that may go through mosh, and what
/// it runs must not change when it does: the same `rmux-agent attach --session`
/// line, delivered by a different wrapper. A session that reattached to a
/// different name would start a second shell beside the one being adopted and
/// leave the original running with nothing pointing at it.
#[tokio::test]
async fn the_attach_line_is_identical_on_both_transports() {
    let target = mosh_capable_host("pinning-attach").await;
    let installed =
        rmux_agent::provision::Installed { program: "/root/.rmux/bin/rmux-agent".into() };
    let spec = installed.attach_spec("tab-7", Some("~/project"), 120, 40);

    let over_ssh = target.build_command(&spec).unwrap();
    let over_mosh = target.build_command(&spec.clone().low_latency(true)).unwrap();

    assert_eq!(over_ssh.program, "ssh");
    assert_eq!(over_mosh.program, "mosh");

    let last = |c: &ResolvedCommand| c.args.last().unwrap().to_string_lossy().into_owned();
    let ssh_line = last(&over_ssh);
    let mosh_line = last(&over_mosh);

    assert!(ssh_line.contains("attach --session tab-7"), "{ssh_line}");
    // Same command, same session name, same geometry — the mosh line adds only
    // the UTF-8 locale mosh itself requires.
    for fragment in ["attach --session tab-7", "--cols 120", "--rows 40", "--cwd"] {
        assert!(mosh_line.contains(fragment), "{fragment:?} was lost: {mosh_line}");
    }
    assert_eq!(
        mosh_line.replace("env LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 ", ""),
        ssh_line,
        "the attach line differs by more than the locale"
    );
}
