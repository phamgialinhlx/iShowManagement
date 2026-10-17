//! mosh as an **opt-in** transport for interactive terminals.
//!
//! ## Why
//!
//! Measured on the operator's own path to their hosts: a median RTT of 271ms,
//! p75 424ms, and **zero packet loss** — a bufferbloated shared uplink rather
//! than a lossy one. Over ssh every keystroke in a full-screen TUI waits for the
//! far side to echo it back, so a quarter of a second sits between pressing a key
//! and seeing it. mosh predicts the echo locally over UDP and removes that wait.
//!
//! ## What this module is, and is not
//!
//! It is the *decision*: can this host be reached with mosh, and if not, why not.
//! The argv is built here too ([`argv`]) so it can be tested without a host, but
//! nothing here spawns anything and nothing here knows about [`crate::SshTarget`].
//!
//! It is emphatically **not** a second transport abstraction. Callers ask for
//! [`rmux_transport::CommandSpec::low_latency`] — a request phrased in what the
//! operator feels, not in a protocol — and the branch lives inside the `Target`
//! impl, exactly like the Windows shell branch in [`crate::winshell`]. Feature
//! code never learns mosh exists, for the same reason it never learns ssh does.
//!
//! ## The invariants this has to respect
//!
//! - **`mosh` is a local binary spawned into a local PTY.** `build_command` still
//!   resolves to a locally spawnable argv; nothing about the terminal layer
//!   changes, and no terminal bytes travel through rmux's RPC.
//! - **The bootstrap is an ssh**, built from the *same* `client_options()`,
//!   identity arguments and port override the ssh path uses — so ControlMaster,
//!   an installed key and a `~/.ssh/config` alias all keep working. mosh's own
//!   `--port` is the **UDP** port and has nothing to do with the ssh port; they
//!   are never interchanged.
//! - **The host alias goes through verbatim.** mosh bootstraps through `ssh`, so
//!   `Match`, `Include`, `ProxyJump`, certificates and FIDO keys come free — but
//!   only as long as the alias, not a resolved `HostName`, is what `ssh` is
//!   handed.
//! - **The remote command is delivered as one argument.** `mosh host -- <argv>`
//!   `exec`s the command rather than running it through a shell, so a POSIX shell
//!   line must arrive as `sh -c '<line>'` with the line intact. Read the [`argv`]
//!   docs before touching that: the string is re-quoted by mosh's wrapper and
//!   then re-parsed by the remote shell, and getting it wrong is an injection,
//!   not a cosmetic bug.
//!
//! ## Only ever for an interactive session
//!
//! mosh is a terminal emulator. It emits a *rendered screen*, not a command's
//! bytes. Routing a one-shot through it would silently corrupt every file
//! listing, search result and metrics sample — so the caller's `Tty::Allocate`
//! is load-bearing and pinned by a test.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use async_trait::async_trait;
use parking_lot::Mutex;
use rmux_transport::{SshHostId, shell_quote};

/// A UTF-8 locale for the remote session.
///
/// mosh requires one on both ends — without it `mosh-server` refuses to start,
/// and a session that does start with a non-UTF-8 locale garbles every
/// non-ASCII character a TUI draws. `en_US.UTF-8` is the value mosh's own
/// documentation reaches for and is present on essentially every Linux and
/// macOS host; it is set as a *remote environment* variable on the shell line,
/// not as an argv flag, because that is what it is.
pub const UTF8_LOCALE: &str = "en_US.UTF-8";

/// The oldest `mosh-server` rmux is willing to leave a host on.
///
/// **1.4.0, and the reason is measured rather than a preference for newness.**
/// `fishe-server` runs 1.3.2, and the operator reported that in a fullscreen
/// Claude pane, coming back to a session *loses the ability to scroll* —
/// "this won't happen in SSH mode but happens in Mosh mode".
///
/// The mechanism: a fullscreen agent pane scrolls through mouse reporting
/// (`?1002h`/`?1006h`). Over ssh a reattach replays the raw scrollback, which
/// carries those sequences with it; mosh has no byte replay at all — it hands
/// back a *rendered screen* — and 1.3.x synchronises less terminal state across
/// a reconnect than 1.4 does. The alternate screen has no scrollback of its
/// own, so the wheel stops doing anything whatsoever.
///
/// 1.4 also carries the OSC 52 support that drag-to-copy in a fullscreen pane
/// needs, which is the same class of loss from the same gap.
pub const MIN_SERVER_VERSION: Version = Version { major: 1, minor: 4, patch: 0 };

/// A `mosh-server` version, ordered so "too old" is a comparison and not a
/// string test.
///
/// Derived `Ord` over the three fields in declaration order is exactly the
/// semantics wanted — `1.3.2 < 1.4.0 < 1.5.1` — so there is no hand-rolled
/// comparison to get backwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Version {
    /// Parse a bare `1.4.0` or `1.4`, and nothing else.
    ///
    /// A missing patch is `0` — `mosh 1.4` and `mosh 1.4.0` are the same
    /// release, and refusing the shorter spelling would read as an unparseable
    /// version and leave a host alone that could have been upgraded.
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = match parts.next() {
            Some(p) => p.parse().ok()?,
            None => 0,
        };
        // A fourth component is a spelling this has never seen; refusing it is
        // the conservative half of the rule below.
        if parts.next().is_some() {
            return None;
        }
        Some(Version { major, minor, patch })
    }
}

/// What `mosh-server --version` said, read defensively.
///
/// **An unreadable version is not an upgrade trigger.** That is the governing
/// rule here and it points one way on purpose: upgrading packages on someone
/// else's server because a version string changed format is a far worse
/// outcome than leaving a host on an older mosh. So this refuses anything it is
/// not sure of — the text has to mention mosh *and* carry a plain dotted
/// number — and `None` is read by every caller as "good enough, leave it".
///
/// The output is read from stdout and stderr together, because which of the two
/// `mosh-server --version` prints to varies by build.
pub fn parse_server_version(text: &str) -> Option<Version> {
    // `mosh-server (mosh 1.3.2)` is the shape; requiring the word means a
    // stray `3.10` out of a shell diagnostic cannot become a version.
    if !text.to_ascii_lowercase().contains("mosh") {
        return None;
    }
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .filter(|t| !t.is_empty() && t.contains('.'))
        .find_map(Version::parse)
}

/// Whether a host's `mosh-server` is old enough to be worth replacing.
///
/// `None` — nothing readable to compare — is **false**, not true.
pub fn is_outdated(version: Option<Version>) -> bool {
    matches!(version, Some(v) if v < MIN_SERVER_VERSION)
}

/// What one look at a host's `mosh-server` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerProbe {
    /// No `mosh-server` on the host at all.
    Missing,
    /// There is one. `None` is *present, version unreadable* — a real state
    /// with its own answer, and the reason this is not `Option<Version>`.
    Present(Option<Version>),
}

impl ServerProbe {
    pub fn present(self) -> bool {
        matches!(self, ServerProbe::Present(_))
    }

    pub fn version(self) -> Option<Version> {
        match self {
            ServerProbe::Present(v) => v,
            ServerProbe::Missing => None,
        }
    }
}

/// Why mosh cannot be used for a host.
///
/// **The reason is not decoration.** A later UI slice has to be able to say why
/// a session the operator asked to be low-latency is not — an outcome reported
/// as a bare "no" is the same "is this broken?" impression the rest of this app
/// exists to avoid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// No `mosh` on this machine's `PATH`.
    LocalClientMissing,
    /// The host has no `mosh-server`.
    RemoteServerMissing,
    /// mosh is not viable on this client platform at all (Windows).
    Unsupported,
}

impl Refusal {
    /// One sentence, written for the operator rather than for us.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::LocalClientMissing => {
                "mosh is not installed on this machine, so this session uses ssh"
            }
            Refusal::RemoteServerMissing => {
                "this host has no mosh-server installed, so this session uses ssh"
            }
            Refusal::Unsupported => {
                "mosh is not supported on this platform, so this session uses ssh"
            }
        }
    }
}

/// Whether mosh can be used for a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    Refused(Refusal),
}

impl Availability {
    pub fn is_available(self) -> bool {
        matches!(self, Availability::Available)
    }

    /// The refusal, when there is one — so a caller can report *why*.
    pub fn refusal(self) -> Option<Refusal> {
        match self {
            Availability::Available => None,
            Availability::Refused(why) => Some(why),
        }
    }
}

/// What has to be discovered about a host before mosh can be used for it.
///
/// A trait rather than two closures so tests can inject both halves: the probe
/// must never touch a real machine from a unit test, and the local half must not
/// depend on whether the machine running the tests happens to have mosh.
#[async_trait]
pub trait Probe: Send + Sync {
    /// Is there a `mosh` on this machine's `PATH`?
    fn local_client(&self) -> bool {
        client_on_path()
    }

    /// Is there a `mosh-server` on the host, and which one?
    ///
    /// Implemented by running [`server_probe_line`] through the ordinary ssh
    /// path with no TTY, so it is multiplexed over the ControlMaster that is
    /// already open and costs no handshake.
    ///
    /// **Presence and version come back from one round trip.** Asking twice
    /// would double the cost of the probe on exactly the slow link this whole
    /// feature exists for, and the second answer is only ever wanted when the
    /// first was yes.
    async fn remote_server(&self) -> ServerProbe;
}

/// Every host whose mosh availability has been settled.
///
/// **Per host, not per [`crate::SshTarget`].** Several targets exist for one
/// host — terminals, files, metrics, the agent, Claude — and only one of them
/// calls `connect`. `winshell` learned this the expensive way: held per
/// instance, the other targets silently assumed the default and every command
/// they built was wrong. Keyed by the whole [`SshHostId`], which is how the
/// ControlMaster registry keys its own entries: a different user or port is a
/// different destination and deserves its own answer.
static AVAILABILITY: LazyLock<Mutex<HashMap<SshHostId, Availability>>> =
    LazyLock::new(Default::default);

/// Hosts whose fallback has already been logged, so it is said once per host
/// rather than once per command — a line per terminal launch is noise, and
/// noise is not a report.
static LOGGED: LazyLock<Mutex<HashSet<SshHostId>>> = LazyLock::new(Default::default);

/// What each host's `mosh-server` said its version was, when it had one.
///
/// A second map rather than a field on [`Availability`], which is `Copy` and
/// is compared for equality all over this crate — widening it would make every
/// one of those comparisons depend on a version nobody was asking about.
/// `Some(None)` is *present with an unreadable version*, which is the answer
/// that must not trigger an upgrade.
static SERVER_VERSION: LazyLock<Mutex<HashMap<SshHostId, Option<Version>>>> =
    LazyLock::new(Default::default);

/// The version last seen for `host`, without probing.
///
/// `None` covers both "never probed" and "present but unreadable", and both are
/// answered the same way by [`is_outdated`]: leave the host alone.
pub fn cached_server_version(host: &SshHostId) -> Option<Version> {
    SERVER_VERSION.lock().get(host).copied().flatten()
}

/// Record the version seen for `host`, replacing whatever was there.
///
/// Called by the probe, and again by `mosh_install` after a verified upgrade —
/// without the second call the cache would hold 1.3.2 for the rest of the run,
/// so the panel would keep offering to upgrade a host that had just been
/// upgraded.
pub fn remember_server_version(host: &SshHostId, version: Option<Version>) {
    SERVER_VERSION.lock().insert(host.clone(), version);
}

/// What is already known about `host`, without probing anything.
///
/// `build_command` is synchronous and documented never to perform I/O, so this
/// is the only form it may consult; [`availability_for`] is what fills it in,
/// during `connect`.
pub fn cached(host: &SshHostId) -> Option<Availability> {
    AVAILABILITY.lock().get(host).copied()
}

/// Settle whether mosh can be used for `host`, once per run.
///
/// The probe runs only on the first call for a destination; every later caller
/// gets the remembered answer, which is what makes this affordable to call from
/// every `connect`.
pub async fn availability_for(host: &SshHostId, probe: &impl Probe) -> Availability {
    if let Some(known) = cached(host) {
        return known;
    }

    let (settled, version) = settle(probe).await;
    AVAILABILITY.lock().insert(host.clone(), settled);
    // Recorded even when it is `None`, so "present with an unreadable version"
    // is a remembered fact rather than something a later caller mistakes for
    // "never probed" and goes and asks about again.
    if settled.is_available() {
        remember_server_version(host, version);
    }
    settled
}

/// Record an answer for `host`, replacing whatever was settled before.
///
/// The one legitimate reason to overwrite a settled answer: `mosh_install` has
/// just put a `mosh-server` on a host that was refused for not having one, and
/// **verified it on `PATH`**. Without this the cache would hold
/// `RemoteServerMissing` for the rest of the run, so the operator would install
/// mosh and then be told the host has none until they restarted the app.
pub fn remember(host: &SshHostId, availability: Availability) {
    AVAILABILITY.lock().insert(host.clone(), availability);
    // A host that can now do it has nothing left to report, so the
    // once-per-host refusal log is armed again rather than left spent.
    if availability.is_available() {
        LOGGED.lock().remove(host);
    }
}

/// The decision itself, with the platform split isolated.
///
/// On Windows there is nothing to ask: mosh has no viable client there, and
/// probing anyway would cost a round trip to reach a foregone conclusion.
#[cfg(windows)]
async fn settle(_probe: &impl Probe) -> (Availability, Option<Version>) {
    (Availability::Refused(Refusal::Unsupported), None)
}

#[cfg(not(windows))]
async fn settle(probe: &impl Probe) -> (Availability, Option<Version>) {
    // The local check first, and not merely for tidiness: without a client
    // there is no session to have, so asking the host would be a round trip
    // spent on an answer that cannot change the outcome.
    if !probe.local_client() {
        return (Availability::Refused(Refusal::LocalClientMissing), None);
    }
    match probe.remote_server().await {
        ServerProbe::Missing => (Availability::Refused(Refusal::RemoteServerMissing), None),
        ServerProbe::Present(version) => (Availability::Available, version),
    }
}

/// The POSIX line that answers "is there a mosh-server here?".
///
/// `command -v` rather than `which`: it is a shell builtin, so it needs nothing
/// installed and answers correctly for a shell function or alias too. The
/// status is what is read, not the output.
pub fn server_probe_line() -> &'static str {
    // Three things, in one round trip, and the order matters.
    //
    // `command -v` is the presence test and the *status* of the whole line:
    // `|| exit 1` keeps "no mosh-server here" as a non-zero exit, which is what
    // every existing caller reads.
    //
    // `--version` then prints something like `mosh-server (mosh 1.3.2)`, and
    // `2>&1` is not tidiness — which of the two streams it lands on varies by
    // build, so reading only stdout would silently see no version on some hosts
    // and read as "unparseable", leaving them alone forever.
    //
    // `|| true` because a `mosh-server` that does not understand `--version` is
    // still a `mosh-server`. Letting it fail would turn an unreadable *version*
    // into an absent *server*, and rmux would then try to install one over the
    // top of the one that is already there.
    "command -v mosh-server >/dev/null 2>&1 || exit 1; mosh-server --version 2>&1 || true"
}

/// Is there a `mosh` on this machine's `PATH`?
///
/// Walked by hand rather than shelling out to `which`: this runs during
/// `connect` and spawning a process to answer a filesystem question is a poor
/// trade. A Finder-launched `.app` has a stripped `PATH`, which is why
/// `userpath::adopt_login_path` runs at startup — by the time anything here is
/// called, `PATH` is the login shell's.
#[cfg(not(windows))]
pub fn client_on_path() -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join("mosh").is_file())
}

/// Never, on Windows. There is no mosh client worth reaching for there, and the
/// caller is told so by name rather than left with a silent failure.
#[cfg(windows)]
pub fn client_on_path() -> bool {
    false
}

/// Report a refusal — once per host, at info level.
///
/// The operator asked for a low-latency session and did not get one. That is
/// exactly the "stale means silently wrong" case: the session works, so nothing
/// looks broken, and the thing they asked for is simply absent.
pub fn log_refusal_once(host: &SshHostId, why: Refusal) {
    if LOGGED.lock().insert(host.clone()) {
        tracing::info!(host = %host.label(), reason = why.reason(), "low latency was requested but mosh was refused");
    }
}

/// The ssh bootstrap and the remote command, ready to be rendered.
pub struct Bootstrap<'a> {
    /// Everything that would have gone on the `ssh` command line before the
    /// destination — multiplexing options, identity arguments, `-l`, `-p`.
    /// **Built by the caller from the same sources the ssh path uses**, so the
    /// two cannot drift.
    pub ssh_options: &'a [String],
    /// The `~/.ssh/config` alias, passed through **verbatim**.
    pub alias: &'a str,
    /// The POSIX shell line to run on the far side, already rendered and
    /// already carrying its quoting.
    pub remote_line: &'a str,
}

/// Render the local `mosh` argv.
///
/// ## The two re-parses
///
/// `--ssh=` is split by mosh with `shellwords`, so every option inside it is
/// quoted for that split — a `ControlPath` under a home directory with a space
/// in it would otherwise become two options and the multiplexing would silently
/// stop being used.
///
/// The remote command is the dangerous half. mosh `exec`s it directly rather
/// than through a shell, so a POSIX line has to be handed over as an explicit
/// `sh -c <line>` with the line as **one** argument. mosh then `shell_quote`s
/// each of those arguments itself before putting them on the `mosh-server`
/// command line, so the line survives intact — provided it is one argv element
/// here and not a string we joined. Everything *inside* the line was already
/// quoted by `spec_to_shell_line`/`shell_quote`, which is the layer that stops
/// a path with a `;` in it from becoming a second command.
///
/// ## `--port` is not the ssh port
///
/// mosh's `-p`/`--port` requests the **UDP** port range `mosh-server` binds.
/// The ssh port belongs inside `--ssh=`, with `ssh`'s own `-p`. Confusing the
/// two produces a bootstrap that connects to the wrong TCP port and a UDP
/// session that binds somewhere nobody opened, so a test pins it.
pub fn argv(b: &Bootstrap) -> Vec<String> {
    let ssh_command = std::iter::once("ssh".to_owned())
        .chain(b.ssh_options.iter().cloned())
        .map(|opt| shell_quote(&opt))
        .collect::<Vec<_>>()
        .join(" ");

    vec![
        format!("--ssh={ssh_command}"),
        // Ends mosh's own option parsing. Everything after this is the
        // destination and then the command.
        "--".to_owned(),
        b.alias.to_owned(),
        // `sh -c <line>`, three arguments. mosh appends them after its own `--`
        // and quotes each one, so the line arrives at the remote shell whole.
        "sh".to_owned(),
        "-c".to_owned(),
        b.remote_line.to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        local: bool,
        remote: ServerProbe,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Fake {
        fn new(local: bool, remote: bool) -> Self {
            Self {
                local,
                remote: if remote {
                    ServerProbe::Present(Some(Version { major: 1, minor: 4, patch: 0 }))
                } else {
                    ServerProbe::Missing
                },
                calls: Default::default(),
            }
        }
        fn with_version(version: Option<Version>) -> Self {
            Self { local: true, remote: ServerProbe::Present(version), calls: Default::default() }
        }
        fn remote_calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Probe for Fake {
        fn local_client(&self) -> bool {
            self.local
        }
        async fn remote_server(&self) -> ServerProbe {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.remote
        }
    }

    fn v(major: u32, minor: u32, patch: u32) -> Version {
        Version { major, minor, patch }
    }

    /// The version is read off the same round trip that settles availability,
    /// and a host that is merely old is still *available* — the upgrade is a
    /// separate decision, not a refusal.
    #[tokio::test]
    async fn an_old_server_is_available_and_its_version_is_remembered() {
        let h = host("old-mosh");
        let probe = Fake::with_version(Some(v(1, 3, 2)));
        assert_eq!(availability_for(&h, &probe).await, Availability::Available);
        assert_eq!(cached_server_version(&h), Some(v(1, 3, 2)));
        assert!(is_outdated(cached_server_version(&h)), "1.3.2 is below the minimum");
        assert_eq!(probe.remote_calls(), 1, "presence and version are one round trip");
    }

    #[tokio::test]
    async fn a_current_server_is_never_an_upgrade_trigger() {
        for version in [v(1, 4, 0), v(1, 5, 1), v(2, 0, 0)] {
            let h = host(&format!("current-{version}"));
            assert_eq!(availability_for(&h, &Fake::with_version(Some(version))).await, Availability::Available);
            assert!(!is_outdated(cached_server_version(&h)), "{version} should be current");
        }
    }

    /// The conservative half of the rule: a version rmux cannot read leaves the
    /// host alone. Guessing here means upgrading packages on someone's server
    /// because a string changed format.
    #[tokio::test]
    async fn an_unreadable_version_triggers_nothing() {
        let h = host("mystery-mosh");
        assert_eq!(availability_for(&h, &Fake::with_version(None)).await, Availability::Available);
        assert_eq!(cached_server_version(&h), None);
        assert!(!is_outdated(cached_server_version(&h)));
        assert!(!is_outdated(None));
    }

    #[test]
    fn the_version_is_read_out_of_what_mosh_server_actually_prints() {
        // The real shapes, both streams, and both spellings of the release.
        assert_eq!(parse_server_version("mosh-server (mosh 1.3.2)\n"), Some(v(1, 3, 2)));
        assert_eq!(
            parse_server_version("mosh-server (mosh 1.4.0) [build mosh 1.4.0]\nCopyright 2012"),
            Some(v(1, 4, 0))
        );
        assert_eq!(parse_server_version("mosh-server (mosh 1.4)"), Some(v(1, 4, 0)));
        assert_eq!(parse_server_version("mosh 1.5.1-1ubuntu2"), Some(v(1, 5, 1)));

        // And everything it must refuse rather than guess at.
        for refused in [
            "",
            "mosh-server: unknown option --version",
            "sh: 1: mosh-server: not found",
            // No mention of mosh at all: a number out of some other
            // diagnostic must not become a server version.
            "bash 3.10 something",
            "mosh-server (mosh)",
        ] {
            assert_eq!(parse_server_version(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn versions_order_the_way_a_release_number_does() {
        assert!(v(1, 3, 2) < v(1, 4, 0));
        assert!(v(1, 4, 0) < v(1, 5, 1));
        assert!(v(1, 10, 0) > v(1, 9, 9), "the components are numbers, not text");
        assert_eq!(MIN_SERVER_VERSION, v(1, 4, 0));
        assert_eq!(v(1, 3, 2).to_string(), "1.3.2");
        // A four-component string is a spelling this has never seen.
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("1"), None);
    }

    /// The line has to answer presence *as its exit status* — every existing
    /// caller reads that — while still printing a version to read.
    #[test]
    fn the_probe_line_answers_presence_by_status_and_version_by_output() {
        let line = server_probe_line();
        assert!(line.contains("command -v mosh-server"), "{line}");
        assert!(line.contains("|| exit 1"), "absence must stay a non-zero exit: {line}");
        assert!(line.contains("mosh-server --version"), "{line}");
        // Both streams, because which one it prints to varies by build.
        assert!(line.contains("2>&1"), "{line}");
        // And a server too old to understand `--version` is still a server.
        assert!(line.contains("|| true"), "{line}");
    }

    fn host(alias: &str) -> SshHostId {
        SshHostId::new(alias)
    }

    fn bootstrap() -> Vec<String> {
        argv(&Bootstrap {
            ssh_options: &[
                "-o".to_owned(),
                "ControlPath=/tmp/rmux/sock".to_owned(),
                "-p".to_owned(),
                "2222".to_owned(),
            ],
            alias: "devbox",
            remote_line: r#""$SHELL" -l -i"#,
        })
    }

    #[tokio::test]
    async fn a_missing_local_client_is_never_blamed_on_the_host() {
        // And the host is not asked at all: the answer cannot change the
        // outcome, so the round trip would be spent for nothing.
        let probe = Fake::new(false, true);
        let got = availability_for(&host("no-client"), &probe).await;

        assert_eq!(got, Availability::Refused(Refusal::LocalClientMissing));
        assert_eq!(probe.remote_calls(), 0, "the host must not be probed with no local client");
    }

    #[tokio::test]
    async fn a_host_without_mosh_server_says_so() {
        let probe = Fake::new(true, false);
        let got = availability_for(&host("bare-host"), &probe).await;
        assert_eq!(got, Availability::Refused(Refusal::RemoteServerMissing));
        assert!(got.refusal().is_some(), "a refusal must carry a reason a UI can print");
    }

    /// The `winshell` bug, which cost a live run: held per target rather than
    /// per host, every target but the one that probed assumed the default.
    #[tokio::test]
    async fn the_answer_is_remembered_per_host_and_the_probe_runs_once() {
        let h = host("cached-host");
        let probe = Fake::new(true, true);

        assert_eq!(availability_for(&h, &probe).await, Availability::Available);
        assert_eq!(availability_for(&h, &probe).await, Availability::Available);
        // A *different* target for the same destination, with a probe that
        // would answer differently, still gets the settled answer.
        let other = Fake::new(true, false);
        assert_eq!(availability_for(&h, &other).await, Availability::Available);

        assert_eq!(probe.remote_calls(), 1, "the host was probed more than once");
        assert_eq!(other.remote_calls(), 0, "a second target re-probed the same host");
        assert_eq!(cached(&h), Some(Availability::Available));
    }

    #[tokio::test]
    async fn a_different_user_or_port_is_a_different_destination() {
        // Same rule as the ControlMaster socket: two destinations that would
        // not share a connection must not share an answer either.
        let base = SshHostId { alias: "multi".to_owned(), user: None, port: None };
        let other = SshHostId { alias: "multi".to_owned(), user: None, port: Some(2222) };

        assert_eq!(availability_for(&base, &Fake::new(true, true)).await, Availability::Available);
        assert_eq!(
            availability_for(&other, &Fake::new(true, false)).await,
            Availability::Refused(Refusal::RemoteServerMissing)
        );
    }

    #[test]
    fn the_ssh_port_rides_inside_the_ssh_bootstrap_and_never_moshs_own_port() {
        let args = bootstrap();
        let ssh = args.iter().find(|a| a.starts_with("--ssh=")).expect("no --ssh=");

        assert!(ssh.contains("-p 2222"), "the ssh port must be in the bootstrap: {ssh}");
        // `--port`/`-p` on *mosh* is the UDP range mosh-server binds. Putting
        // an ssh port there points the bootstrap at the wrong TCP port and the
        // UDP session at a port nobody opened.
        assert!(
            !args.iter().any(|a| a == "-p" || a == "--port" || a.starts_with("--port=")),
            "mosh's own port must not be set: {args:?}"
        );
    }

    #[test]
    fn the_alias_is_passed_verbatim() {
        // Resolving `~/.ssh/config` ourselves is what this whole crate exists
        // not to do; mosh bootstraps through ssh, so the alias keeps `Match`,
        // `ProxyJump` and certificates working — but only unresolved.
        let args = bootstrap();
        let sep = args.iter().position(|a| a == "--").expect("no -- separator");
        assert_eq!(args[sep + 1], "devbox");
        assert!(!args.iter().any(|a| a.contains("192.168")), "a resolved address leaked: {args:?}");
    }

    #[test]
    fn the_remote_command_is_one_argument_after_sh_minus_c() {
        let args = bootstrap();
        let sh = args.iter().position(|a| a == "sh").expect("no sh");
        assert_eq!(args[sh + 1], "-c");
        assert_eq!(args[sh + 2], r#""$SHELL" -l -i"#);
        assert_eq!(sh + 3, args.len(), "the line must be the final argument");
    }

    /// The injection test. mosh re-quotes what it hands to `ssh`, and the
    /// remote login shell re-parses the result, so a fragment that escapes its
    /// quoting here is a command someone else's machine runs.
    #[test]
    fn a_hostile_line_stays_one_inert_argument() {
        let hostile = "cd '/tmp/'\\''; touch /tmp/pwned; '\\''' && \
                       echo `id` $(whoami) && printf 'a\nb' && rm -rf /";
        let args = argv(&Bootstrap {
            ssh_options: &[],
            alias: "devbox",
            remote_line: hostile,
        });

        // Whatever it contains, it is exactly one argv element and it is the
        // last one — there is no position after it for a second command to
        // occupy.
        assert_eq!(args.last().map(String::as_str), Some(hostile));
        assert_eq!(args.iter().filter(|a| a.contains("pwned")).count(), 1);

        // And no fragment of it leaked into the bootstrap, which is the part
        // mosh splits with shellwords.
        let ssh = args.iter().find(|a| a.starts_with("--ssh=")).expect("no --ssh=");
        for fragment in ["pwned", "whoami", "rm -rf", "id"] {
            assert!(!ssh.contains(fragment), "{fragment:?} reached the bootstrap: {ssh}");
        }
    }

    #[test]
    fn a_bootstrap_option_with_a_space_survives_moshs_shellwords_split() {
        // mosh splits `--ssh=` with shellwords. A ControlPath under a home
        // directory with a space in it would otherwise become two options, and
        // multiplexing would stop being used with nothing saying so.
        let args = argv(&Bootstrap {
            ssh_options: &["-o".to_owned(), "ControlPath=/Users/a b/.rmux/s".to_owned()],
            alias: "devbox",
            remote_line: "true",
        });
        let ssh = args.iter().find(|a| a.starts_with("--ssh=")).unwrap();
        assert!(ssh.contains("'ControlPath=/Users/a b/.rmux/s'"), "{ssh}");
    }

    #[test]
    fn the_probe_reads_a_status_rather_than_output() {
        let line = server_probe_line();
        assert!(line.starts_with("command -v mosh-server"), "{line}");
        assert!(line.contains(">/dev/null"), "the output is noise; the status is the answer: {line}");
    }

    #[test]
    fn every_refusal_can_be_explained_to_the_operator() {
        for why in [Refusal::LocalClientMissing, Refusal::RemoteServerMissing, Refusal::Unsupported]
        {
            assert!(why.reason().contains("ssh"), "the fallback must be named: {why:?}");
            assert!(why.reason().len() > 20, "{why:?}");
        }
    }
}

/// The environment the **local** `mosh` wrapper needs, layered over whatever the
/// caller already set.
///
/// `mosh` is a Perl script that inspects its own environment before it ever
/// reaches `ssh`: it runs `tput colors`, which needs `TERM`, and it refuses to
/// run without a UTF-8 native locale. rmux is a GUI process with no controlling
/// terminal, so a Finder-launched `.app` has neither — and `ssh` never noticed,
/// because the pty belongs to rmux and the *remote* environment rides the shell
/// line. Measured, verbatim, from a real session:
///
/// ```text
/// TERM environment variable not set.
/// Died at /opt/homebrew/bin/mosh line 320.
/// ```
///
/// `xterm-256color` because the far end is xterm.js, which is what it is; the
/// value is only used locally to count colours.
///
/// The caller's own values win — an operator who launched rmux from a terminal
/// has a real `TERM`, and overriding it would be rmux inventing an answer it
/// was handed.
pub fn local_env(
    base: std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut env = base;
    for (key, value) in [
        ("TERM", LOCAL_TERM),
        ("LANG", UTF8_LOCALE),
        ("LC_ALL", UTF8_LOCALE),
    ] {
        if !env.contains_key(key) {
            if let Ok(inherited) = std::env::var(key)
                && !inherited.is_empty()
            {
                env.insert(key.to_owned(), inherited);
                continue;
            }
            env.insert(key.to_owned(), value.to_owned());
        }
    }
    env
}

/// What rmux tells the local `mosh` wrapper it is drawing into. The far end is
/// xterm.js; this value never leaves this machine.
pub const LOCAL_TERM: &str = "xterm-256color";

#[cfg(test)]
mod local_env_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The regression this exists for, reported from a live pane:
    /// `TERM environment variable not set. Died at .../mosh line 320.`
    #[test]
    fn the_local_wrapper_is_given_a_term_and_a_utf8_locale() {
        let env = local_env(BTreeMap::new());
        assert!(!env["TERM"].is_empty(), "mosh runs `tput colors` and dies without TERM");
        assert!(env["LANG"].contains("UTF-8"));
        assert!(env["LC_ALL"].contains("UTF-8"));
    }

    /// Never override what the caller already decided — the askpass bridge's
    /// variables must survive, and a real `TERM` beats one rmux invented.
    #[test]
    fn the_callers_own_values_win() {
        let mut base = BTreeMap::new();
        base.insert("TERM".to_owned(), "screen-256color".to_owned());
        base.insert("SSH_ASKPASS".to_owned(), "/x/askpass".to_owned());
        let env = local_env(base);
        assert_eq!(env["TERM"], "screen-256color");
        assert_eq!(env["SSH_ASKPASS"], "/x/askpass");
    }
}
