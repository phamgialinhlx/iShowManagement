//! Putting `mosh-server` on a host that has not got one.
//!
//! ## Why this exists, and why it is the shape it is
//!
//! [`mosh`](crate::mosh) refuses a host with [`Refusal::RemoteServerMissing`],
//! and that refusal is *actionable* in a way the other two are not: the fix is
//! one package away on the machine rmux is already connected to. The operator
//! asked for it to be done for them rather than offered as a button.
//!
//! Installing a package on someone else's machine is, however, the most
//! consequential thing rmux does without being asked at the moment it happens.
//! So the whole module is written around the two ways it could hurt:
//!
//! - **It must never ask for a password.** `sudo -n` only, and a host that
//!   answers "I need a password" is recorded and *never asked again*. Routing
//!   this through the askpass bridge was considered and is wrong: CLAUDE.md
//!   already records what that costs — "a password dialog every second", and
//!   "roughly one failed login every two seconds… enough to get the machine
//!   banned by `fail2ban`". A ban reads afterwards as "the server is down",
//!   which is a far worse failure than not having mosh. One of the operator's
//!   own hosts is exactly this case.
//! - **It must never make the operator wait.** The attempt is a background task
//!   fired from the probe, not a step inside it. `connect`, a terminal opening
//!   and the TRANSPORT panel all return at their usual speed; a host that takes
//!   ninety seconds to install a package still gives an instant ssh shell.
//!
//! ## At most one attempt, and a refusal that survives a restart
//!
//! Retrying a refused privileged operation on every reconnect is the
//! fail2ban-shaped mistake this codebase has already made once, so the decision
//! is persisted in [`ledger`]-managed `~/.rmux/mosh-install.json` (`0600` in a
//! `0700` directory, mode set **before** the content is written, atomic
//! rename — the same rules as `redstone.json` and `aliases.json`). A host that
//! failed is left alone for [`RETRY_AFTER`]; a host that needs a password is
//! left alone until the operator clears the file. In-memory, [`claim`] makes
//! the attempt once per run even when several targets probe the same host at
//! once.
//!
//! ## The same machinery upgrades a host that is merely *old*
//!
//! A host can have a `mosh-server` that is present and too old to be usable for
//! what rmux does with it — see [`mosh::MIN_SERVER_VERSION`] for the measured
//! reason. That is the same consequential act against the same machine, so it is
//! the same mechanism rather than a second one: one [`Job`], one [`detect_line`],
//! one [`Privilege`] decision, one [`Ledger`], one [`claim`]. Every guard above
//! protects the upgrade identically — `sudo -n` only, never blocking, one attempt
//! per host remembered across runs, and the off switch covers both.
//!
//! The upgrade is filed under its own ledger key (`<host>#upgrade`), because a
//! host can legitimately have settled answers to both questions and one record
//! would overwrite the other.
//!
//! **A repository with nothing newer is its own outcome.** `apt-get install
//! --only-upgrade` against a Debian whose archive holds only 1.3.2 succeeds and
//! changes nothing; reporting that as an upgrade would be a claim the re-probe
//! flatly contradicts, so [`Outcome::NothingNewer`] exists and is the one
//! outcome here that asks the *operator* to decide something — a backport, a
//! PPA, or accepting the limitation.
//!
//! ## A running session survives it
//!
//! An upgrade replaces the `mosh-server` binary on a host that may have live
//! mosh sessions. Each such session is an **already-running** `mosh-server`
//! process, which holds its executable open; `dpkg`, `rpm`, `apk` and `pacman`
//! all publish a new binary by writing a temporary file and renaming over the
//! old path, so the running process keeps the inode it started with and is not
//! disturbed. The `mosh` package ships no service unit, so nothing restarts
//! anything either. New sessions get the new binary; nothing attaches an old
//! client to a new server mid-stream, because a mosh session's server is the
//! process it started with.
//!
//! ## What is *observed* rather than assumed
//!
//! A package manager exiting 0 is not proof that `mosh-server` is on `PATH` —
//! a package can install a binary somewhere the login shell does not look, and
//! `apt-get` is perfectly capable of succeeding at doing nothing. So the
//! outcome is the *re-probe*, not the exit status, and on a verified success
//! the cached [`Availability`](crate::mosh::Availability) is updated so the
//! TRANSPORT panel and the next session see the new answer with no restart.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use rmux_transport::{Output, SshHostId, shell_quote};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::mosh;

/// The package, in one place. Interpolated through [`shell_quote`] like
/// everything else that reaches a remote shell line — it is a constant today,
/// and a constant that is quoted stays safe when it stops being one.
const PACKAGE: &str = "mosh";

/// How long a *failed* host is left alone.
///
/// Seven days. Long enough that a reconnect loop cannot turn one failure into a
/// pattern of privileged commands against a machine that already said no, short
/// enough that a host whose repositories were merely broken gets another go
/// without anyone having to know this file exists.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// The ceiling on one attempt.
///
/// `apt-get` on a cold host genuinely takes a minute or two; five is past any
/// honest install and short of leaving the TRANSPORT panel waiting forever on a
/// command that will never answer.
const ATTEMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Whether rmux may install `mosh-server` on a host at all.
///
/// **Default on**, because that is what was asked for — but this mutates
/// machines that are not all the operator's, so there is a switch, and when it
/// is off nothing is attempted and nothing is written. Mirrored from the
/// webview's stored preference at startup and on every change, the same way the
/// benchmark-logging gate is.
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// What an install or upgrade attempt came to.
///
/// Separate outcomes rather than a bool, because they ask **different things of
/// the operator**: a missing package manager is nothing they can do from here, a
/// password host is one command they can run themselves, a failure is a reason
/// to go and look, and a repository with nothing newer is a decision only they
/// can make. A single "it did not work" would flatten all of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Outcome {
    /// `mosh-server` was installed **and then seen on `PATH`**.
    Installed { manager: String },
    /// A too-old `mosh-server` was replaced, and the **re-probe** then read the
    /// new version back. Both versions are carried: "upgraded" with no numbers
    /// is not something anyone can check.
    Upgraded { manager: String, from: String, to: String },
    /// The upgrade ran, succeeded, and the version did not move far enough —
    /// this manager's repositories simply have nothing newer. The version now
    /// on the host is carried, because the operator's next step (a backport, a
    /// PPA, or accepting it) turns on which one it is.
    NothingNewer { manager: String, version: String },
    /// The host's `mosh-server` is already new enough. Nothing was run and
    /// nothing was written; it exists so the panel can say so rather than
    /// showing an empty space where a state should be.
    AlreadyCurrent { version: String },
    /// `sudo -n` was refused. Never retried; the command to run by hand is
    /// carried so the operator is not left to reconstruct it.
    NeedsPassword { command: String },
    /// The install ran and did not work, in the host's own words.
    Failed { reason: String },
    /// Nothing here can install it — no known package manager, or a Windows
    /// host, where a POSIX install line has no meaning at all.
    Unsupported { reason: String },
}

/// The name a [`Outcome`] is filed under, so the ledger is readable by a person
/// who opens it and stays stable if the enum's shape changes.
fn tag(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Installed { .. } => "installed",
        Outcome::Upgraded { .. } => "upgraded",
        Outcome::NothingNewer { .. } => "nothing-newer",
        Outcome::AlreadyCurrent { .. } => "already-current",
        Outcome::NeedsPassword { .. } => "needs-password",
        Outcome::Failed { .. } => "failed",
        Outcome::Unsupported { .. } => "unsupported",
    }
}

/// Which of the two things is being asked of the host.
///
/// One enum threaded through [`attempt`] rather than two parallel functions:
/// the detection, the privilege decision, the ledger, the claim, the timeout and
/// the re-probe are identical, and a second copy of them is a second place for
/// the `sudo -n` rule to be got wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// The host has no `mosh-server`.
    Install,
    /// The host has one, this old, and it is below [`mosh::MIN_SERVER_VERSION`].
    Upgrade(mosh::Version),
}

impl Job {
    fn is_upgrade(self) -> bool {
        matches!(self, Job::Upgrade(_))
    }

    /// How this job is filed, so a host can hold a settled answer to both
    /// questions without one overwriting the other.
    ///
    /// `Install` keeps the bare host key: ledgers written before upgrades
    /// existed are still read, rather than every host silently getting one more
    /// privileged attempt on the release that added this.
    fn suffix(self) -> &'static str {
        match self {
            Job::Install => "",
            Job::Upgrade(_) => "#upgrade",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Job::Install => "install",
            Job::Upgrade(_) => "upgrade",
        }
    }
}

/// The package managers rmux knows how to drive without a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    AptGet,
    Dnf,
    Yum,
    Apk,
    Pacman,
    Zypper,
    Brew,
}

impl Manager {
    /// In probe order — the first one present on the host wins. `apt-get`
    /// before `dnf` before `yum` is not arbitrary: a Fedora host has `yum` as a
    /// shim over `dnf`, and driving the shim gets a deprecation notice on
    /// stderr that would then be reported as the reason for a failure.
    pub const ALL: &'static [Manager] = &[
        Manager::AptGet,
        Manager::Dnf,
        Manager::Yum,
        Manager::Apk,
        Manager::Pacman,
        Manager::Zypper,
        Manager::Brew,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Manager::AptGet => "apt-get",
            Manager::Dnf => "dnf",
            Manager::Yum => "yum",
            Manager::Apk => "apk",
            Manager::Pacman => "pacman",
            Manager::Zypper => "zypper",
            Manager::Brew => "brew",
        }
    }

    pub fn from_name(name: &str) -> Option<Manager> {
        Manager::ALL.iter().copied().find(|m| m.name() == name)
    }

    /// Does this manager have to be run as root?
    ///
    /// **Homebrew must not.** It refuses outright when run as root — so
    /// prefixing `sudo` there would not be a precaution, it would be a
    /// guaranteed failure on every macOS target.
    pub fn needs_root(self) -> bool {
        !matches!(self, Manager::Brew)
    }

    /// The non-interactive install line for this manager.
    ///
    /// Every one of these flags exists to stop the command opening a prompt on
    /// a machine with no terminal attached: a package manager that stops to ask
    /// "Do you want to continue [Y/n]?" hangs until the timeout and then
    /// reports a failure whose reason is a question.
    pub fn install_line(self, privileged: bool) -> String {
        let sudo = if privileged { "sudo -n " } else { "" };
        let pkg = shell_quote(PACKAGE);
        match self {
            // `update` first, because `install` on a host whose package lists
            // were never fetched fails with "Unable to locate package" — which
            // is a true statement about an empty index, not about mosh. It is
            // best-effort: a refresh that fails must not veto an install that
            // would have worked from a cached index.
            Manager::AptGet => format!(
                "{sudo}env DEBIAN_FRONTEND=noninteractive apt-get update -qq >/dev/null 2>&1 || true; \
                 {sudo}env DEBIAN_FRONTEND=noninteractive apt-get install -y {pkg}"
            ),
            Manager::Dnf => format!("{sudo}dnf -y install {pkg}"),
            Manager::Yum => format!("{sudo}yum -y install {pkg}"),
            Manager::Apk => format!("{sudo}apk add --no-cache {pkg}"),
            Manager::Pacman => format!("{sudo}pacman -S --noconfirm {pkg}"),
            Manager::Zypper => format!("{sudo}zypper -n install {pkg}"),
            // No `sudo`, ever. See `needs_root`.
            Manager::Brew => format!("brew install {pkg}"),
        }
    }

    /// The non-interactive line that replaces an installed `mosh` with a newer
    /// one, if the repositories have one.
    ///
    /// Deliberately *upgrade* verbs rather than a plain `install`: on a host
    /// that already has the package, `apt-get install mosh` is a no-op that
    /// exits 0, so the attempt would report success having done nothing at all
    /// and the re-probe would be the only thing that noticed. Saying what is
    /// meant is also what makes the "nothing newer" outcome honest — the command
    /// tried to move the version and the archive had nowhere to move it to.
    ///
    /// Every one of these is non-interactive for the same reason
    /// [`install_line`](Self::install_line) is: there is no terminal on the far
    /// side to answer a question.
    pub fn upgrade_line(self, privileged: bool) -> String {
        let sudo = if privileged { "sudo -n " } else { "" };
        let pkg = shell_quote(PACKAGE);
        match self {
            // `--only-upgrade` rather than a bare install, so this can never
            // install mosh on a host that has deliberately not got it — this
            // path is reached only for a host that *has* one, and a flag that
            // enforces that is better than a comment saying so. The `update` is
            // best-effort for the same reason as the install line: without it
            // the archive index may predate the version being reached for.
            Manager::AptGet => format!(
                "{sudo}env DEBIAN_FRONTEND=noninteractive apt-get update -qq >/dev/null 2>&1 || true; \
                 {sudo}env DEBIAN_FRONTEND=noninteractive apt-get install -y --only-upgrade {pkg}"
            ),
            Manager::Dnf => format!("{sudo}dnf -y upgrade {pkg}"),
            Manager::Yum => format!("{sudo}yum -y update {pkg}"),
            Manager::Apk => format!("{sudo}apk upgrade --no-cache {pkg}"),
            // Arch has no separate upgrade verb for one package; `-S` on an
            // installed package *is* the upgrade.
            Manager::Pacman => format!("{sudo}pacman -S --noconfirm {pkg}"),
            Manager::Zypper => format!("{sudo}zypper -n update {pkg}"),
            // No `sudo`, ever. See `needs_root`.
            Manager::Brew => format!("brew upgrade {pkg}"),
        }
    }

    /// The line the *operator* can run themselves on a password host.
    ///
    /// Shown verbatim in the TRANSPORT panel. The alias is quoted because it
    /// reaches a shell — theirs, this time, but the rule does not get an
    /// exception for that.
    pub fn manual_command(self, alias: &str) -> String {
        self.manual_command_for(alias, Job::Install)
    }

    /// The same, for whichever job was being attempted.
    ///
    /// The upgrade wording matters: handing someone `sudo apt-get install -y
    /// mosh` for a host that already has mosh is a command that will exit 0 and
    /// change nothing, and they would reasonably read that as rmux being wrong
    /// about the version rather than as the wrong command.
    pub fn manual_command_for(self, alias: &str, job: Job) -> String {
        // Written out rather than derived from `install_line`, because the two
        // are answering different questions. That line is for a machine with no
        // terminal: `sudo -n`, `DEBIAN_FRONTEND`, a best-effort `update`. This
        // one is read by a person who *has* a terminal, so `sudo` may prompt
        // (which is the entire point) and every flag that exists only to
        // suppress a prompt is noise in an instruction.
        let inner = match (self, job.is_upgrade()) {
            (Manager::AptGet, false) => "sudo apt-get install -y mosh",
            (Manager::Dnf, false) => "sudo dnf -y install mosh",
            (Manager::Yum, false) => "sudo yum -y install mosh",
            (Manager::Apk, false) => "sudo apk add mosh",
            (Manager::Pacman, false) => "sudo pacman -S mosh",
            (Manager::Zypper, false) => "sudo zypper install mosh",
            (Manager::Brew, false) => "brew install mosh",
            (Manager::AptGet, true) => "sudo apt-get install -y --only-upgrade mosh",
            (Manager::Dnf, true) => "sudo dnf -y upgrade mosh",
            (Manager::Yum, true) => "sudo yum -y update mosh",
            (Manager::Apk, true) => "sudo apk upgrade mosh",
            (Manager::Pacman, true) => "sudo pacman -S mosh",
            (Manager::Zypper, true) => "sudo zypper update mosh",
            (Manager::Brew, true) => "brew upgrade mosh",
        };
        format!("ssh {} {}", shell_quote(alias), shell_quote(inner))
    }
}

/// How the install may be run on this host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    /// Already root — no `sudo` needed or wanted.
    Root,
    /// `sudo -n true` succeeded: a passwordless sudoers rule exists.
    Sudo,
    /// `sudo` would prompt. **Stop here.**
    NeedsPassword,
    /// Homebrew, which must run unprivileged.
    Unprivileged,
}

/// One POSIX line that answers both questions at once: which package manager is
/// here, and may we use it without a password.
///
/// One line rather than two round trips because on the link this whole feature
/// exists for a round trip is ~270ms, and because the two answers are only
/// meaningful together — `brew` decides that the sudo question is not asked at
/// all, and asking it anyway would put a `sudo` attempt in the logs of a
/// machine that never needed one.
///
/// It prints exactly one line, `<manager> <privilege>`, and nothing else; every
/// probe is redirected because "not found" is the ordinary answer for most of
/// the list.
pub fn detect_line() -> String {
    let candidates =
        Manager::ALL.iter().map(|m| m.name()).collect::<Vec<_>>().join(" ");
    format!(
        "m=; for c in {candidates}; do if command -v \"$c\" >/dev/null 2>&1; then m=$c; break; fi; done; \
         if [ -z \"$m\" ]; then p=none; elif [ \"$m\" = brew ]; then p=unprivileged; \
         elif [ \"$(id -u)\" = 0 ]; then p=root; \
         elif sudo -n true >/dev/null 2>&1; then p=sudo; else p=needs-password; fi; \
         printf '%s %s\\n' \"${{m:-unknown}}\" \"$p\""
    )
}

/// Read [`detect_line`]'s single line of output.
pub fn parse_detection(stdout: &str) -> (Option<Manager>, Option<Privilege>) {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut fields = line.split_whitespace();
    let manager = fields.next().and_then(Manager::from_name);
    let privilege = match fields.next() {
        Some("root") => Some(Privilege::Root),
        Some("sudo") => Some(Privilege::Sudo),
        Some("needs-password") => Some(Privilege::NeedsPassword),
        Some("unprivileged") => Some(Privilege::Unprivileged),
        _ => None,
    };
    (manager, privilege)
}

/// The host, as much of it as an install needs.
///
/// A trait so every test in this module runs against an injected fake: nothing
/// here may touch a real machine, and nothing here may depend on whether the
/// machine running the tests happens to have `sudo`.
#[async_trait]
pub trait InstallHost: Send + Sync {
    /// Run a POSIX shell line with no TTY and give back what happened.
    async fn run(&self, line: &str) -> anyhow::Result<Output>;

    /// Does this host's login shell parse POSIX?
    ///
    /// **False is a full stop, not a degraded path.** Windows is reached
    /// through a `bash` that `winshell` located; emitting a POSIX install line
    /// at a host whose package management is nothing like this would at best do
    /// nothing, and mosh has no viable server there in any case.
    fn is_posix(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// The ledger
// ---------------------------------------------------------------------------

/// One host's settled answer, as it is stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    /// `installed` | `upgraded` | `nothing-newer` | `needs-password` |
    /// `failed` | `unsupported`.
    outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manager: Option<String>,
    /// The reason, or the by-hand command — whichever the outcome carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// The version this host was on before an upgrade.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    /// The version it was observed on afterwards — for `upgraded`, the new one;
    /// for `nothing-newer`, the one the repositories left it on. Stored because
    /// [`Ledger::standing`] compares it against the *current*
    /// [`mosh::MIN_SERVER_VERSION`]: without that, raising the minimum in a
    /// later release would be silently vetoed for every host that already has an
    /// `upgraded` record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    /// Unix seconds. Wall clock rather than a monotonic instant on purpose:
    /// this has to survive a restart, and nothing monotonic does.
    at: u64,
}

impl Record {
    fn to_outcome(&self) -> Option<Outcome> {
        let detail = self.detail.clone().unwrap_or_default();
        let manager = self.manager.clone().unwrap_or_default();
        Some(match self.outcome.as_str() {
            "installed" => Outcome::Installed { manager },
            "upgraded" => Outcome::Upgraded {
                manager,
                from: self.from.clone().unwrap_or_default(),
                to: self.to.clone().unwrap_or_default(),
            },
            "nothing-newer" => Outcome::NothingNewer {
                manager,
                version: self.to.clone().unwrap_or_default(),
            },
            "needs-password" => Outcome::NeedsPassword { command: detail },
            "failed" => Outcome::Failed { reason: detail },
            "unsupported" => Outcome::Unsupported { reason: detail },
            // `already-current` is never written — see `Ledger::record`.
            _ => return None,
        })
    }
}

/// Where attempts are remembered.
///
/// A path rather than a global so every test writes into its own temporary
/// directory: a unit test that touched `~/.rmux` would be editing the state of
/// the operator's running app.
#[derive(Clone, Debug)]
pub struct Ledger {
    path: PathBuf,
}

impl Ledger {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `~/.rmux/mosh-install.json`.
    pub fn default_path() -> anyhow::Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        Ok(home.join(".rmux").join("mosh-install.json"))
    }

    pub fn standard() -> anyhow::Result<Self> {
        Ok(Self::at(Self::default_path()?))
    }

    fn load(&self) -> HashMap<String, Record> {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return HashMap::new() };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Replace the file atomically, owner-only.
    ///
    /// The mode is set **before** any content is written — `create_new` with an
    /// explicit `0600` rather than write-then-chmod, which leaves a window in
    /// which the file exists at the umask's mode. Same rule as `redstone.json`,
    /// for a weaker reason: this file names the operator's hosts.
    fn save(&self, records: &HashMap<String, Record>) -> anyhow::Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no parent directory"))?;
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }

        let text = serde_json::to_string_pretty(records)?;
        let temp = dir.join(format!("mosh-install.json.{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&temp);

        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }

        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }

    /// The recorded answer for `host`, if one still stands.
    ///
    /// `installed`, `upgraded` and `needs-password` never expire — the first two
    /// because there is nothing left to do, the third because asking again is the
    /// thing this module exists not to do. `failed`, `unsupported` and
    /// `nothing-newer` expire after [`RETRY_AFTER`]: broken repositories get
    /// fixed, and an archive with nothing newer today may have something in a
    /// week.
    ///
    /// One record does stop standing early: an `upgraded` whose observed version
    /// is below the minimum rmux asks for *now*. Raising
    /// [`mosh::MIN_SERVER_VERSION`] in a later release would otherwise be vetoed
    /// for every host that had already been upgraded once, which is precisely
    /// the "stale means silently wrong" failure — the panel would report a host
    /// as upgraded while rmux went on considering it too old.
    pub fn standing(&self, host: &SshHostId, job: Job, now: u64) -> Option<Outcome> {
        let record = self.load().get(&key(host, job)).cloned()?;
        let outcome = record.to_outcome()?;

        if let Outcome::Upgraded { to, .. } = &outcome {
            match mosh::Version::parse(to) {
                Some(v) if v < mosh::MIN_SERVER_VERSION => return None,
                // An unreadable stored version is left standing rather than
                // re-attempted: the conservative direction is the same one
                // `parse_server_version` takes, and for the same reason.
                _ => {}
            }
        }

        let expires = matches!(
            outcome,
            Outcome::Failed { .. } | Outcome::Unsupported { .. } | Outcome::NothingNewer { .. }
        );
        if expires && now.saturating_sub(record.at) >= RETRY_AFTER.as_secs() {
            return None;
        }
        Some(outcome)
    }

    /// File an outcome against `host`.
    ///
    /// A write that fails is logged and swallowed: an unwritable `~/.rmux` must
    /// not turn "mosh is now installed" into an error the operator sees, and
    /// the in-memory claim still prevents a second attempt this run.
    pub fn record(&self, host: &SshHostId, job: Job, outcome: &Outcome, now: u64) {
        // **Never written.** "Already current" is not a decision to remember: it
        // is a reading of the host taken a moment ago, and storing it would veto
        // the upgrade for ever on the day the minimum version is raised.
        if matches!(outcome, Outcome::AlreadyCurrent { .. }) {
            return;
        }
        let mut from = None;
        let mut to = None;
        let (manager, detail) = match outcome {
            Outcome::Installed { manager } => (Some(manager.clone()), None),
            Outcome::Upgraded { manager, from: f, to: t } => {
                from = Some(f.clone());
                to = Some(t.clone());
                (Some(manager.clone()), None)
            }
            Outcome::NothingNewer { manager, version } => {
                to = Some(version.clone());
                (Some(manager.clone()), None)
            }
            Outcome::AlreadyCurrent { .. } => unreachable!("returned above"),
            Outcome::NeedsPassword { command } => (None, Some(command.clone())),
            Outcome::Failed { reason } => (None, Some(reason.clone())),
            Outcome::Unsupported { reason } => (None, Some(reason.clone())),
        };
        let mut records = self.load();
        records.insert(
            key(host, job),
            Record { outcome: tag(outcome).to_owned(), manager, detail, from, to, at: now },
        );
        if let Err(e) = self.save(&records) {
            tracing::warn!(error = %e, "could not record the mosh-server install attempt");
        }
    }
}

/// How a host is filed. The whole destination, like the availability cache and
/// the ControlMaster socket: a different user or port is a different machine as
/// far as `sudo` is concerned.
fn key(host: &SshHostId, job: Job) -> String {
    let destination = match host.port {
        Some(port) => format!("{}:{port}", host.label()),
        None => host.label(),
    };
    format!("{destination}{}", job.suffix())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// One attempt per host per run
// ---------------------------------------------------------------------------

/// Every host whose attempt has been claimed this run, and how to wait for it.
static ATTEMPTS: LazyLock<Mutex<HashMap<String, watch::Receiver<Option<Outcome>>>>> =
    LazyLock::new(Default::default);

/// The right to attempt an install, held by exactly one task.
///
/// Several targets exist for one host and any of them may probe, so without
/// this the same `sudo` would run three or four times concurrently against one
/// machine — which is the shape of the failure this whole module is written to
/// avoid, arriving from the other direction.
pub struct Claim {
    tx: watch::Sender<Option<Outcome>>,
}

impl Claim {
    pub fn settle(&self, outcome: Outcome) {
        let _ = self.tx.send(Some(outcome));
    }
}

impl Drop for Claim {
    /// A claim dropped without an answer would leave [`status`] awaiting
    /// forever — a TRANSPORT panel stuck on "installing" for the life of the
    /// app, which is precisely the "is this broken?" impression the design
    /// rules forbid. A panicked or cancelled task settles as a failure that
    /// says so.
    fn drop(&mut self) {
        if self.tx.borrow().is_none() {
            let _ = self.tx.send(Some(Outcome::Failed {
                reason: "the install task ended without reporting an outcome".to_owned(),
            }));
        }
    }
}

/// Claim the attempt for `host`, or `None` if it is already claimed this run.
///
/// Per host *and per job*: a host may legitimately be installed on and, once
/// that install turns out to have produced an old server, upgraded — and a
/// single claim would make the second one look like a duplicate of the first.
pub fn claim(host: &SshHostId, job: Job) -> Option<Claim> {
    let mut attempts = ATTEMPTS.lock();
    let k = key(host, job);
    if attempts.contains_key(&k) {
        return None;
    }
    let (tx, rx) = watch::channel(None);
    attempts.insert(k, rx);
    Some(Claim { tx })
}

/// What the attempt for `host` came to, **awaiting one that is still running**.
///
/// This is deliberately not a poller: there is one call, and it resolves when
/// there is something to say. `None` means no attempt was made this run — the
/// host has mosh already, the switch is off, or nothing has probed it yet — and
/// the caller should say nothing rather than invent a state.
pub async fn status(host: &SshHostId) -> Option<Outcome> {
    // The upgrade first. A host reaches the upgrade path only *after* it has a
    // `mosh-server`, so when both exist the upgrade is the later fact and the
    // one the operator is waiting on; reporting the install would be answering
    // last week's question.
    let claimed = {
        let attempts = ATTEMPTS.lock();
        // The version does not enter the key — `Job::suffix` ignores it — so
        // any `Upgrade` stands in for the one that was actually claimed.
        let upgrade = key(host, Job::Upgrade(mosh::MIN_SERVER_VERSION));
        attempts
            .get(&upgrade)
            .or_else(|| attempts.get(&key(host, Job::Install)))
            .cloned()
    };
    let mut rx = claimed?;
    loop {
        if let Some(outcome) = rx.borrow().clone() {
            return Some(outcome);
        }
        if rx.changed().await.is_err() {
            // The sender is gone without a value, which `Claim::drop` makes
            // impossible — but returning is still better than hanging.
            return rx.borrow().clone();
        }
    }
}

/// Is there a claimed attempt for `host` that has not settled yet?
///
/// **Synchronous, and that is the point.** The panel has to decide whether to
/// draw an in-flight state *before* it awaits one — awaiting the answer to
/// "is anything happening?" would be the thing being asked. Without this it had
/// to assume an attempt was running and then take it back, which flashes
/// "installing…" at every host where nothing was attempted at all.
pub fn pending(host: &SshHostId) -> bool {
    let attempts = ATTEMPTS.lock();
    [Job::Upgrade(mosh::MIN_SERVER_VERSION), Job::Install]
        .iter()
        .filter_map(|job| attempts.get(&key(host, *job)))
        .any(|rx| rx.borrow().is_none())
}

/// Forget every claim. Tests only: the map is process-wide by design.
#[doc(hidden)]
pub fn reset_claims_for_test() {
    ATTEMPTS.lock().clear();
}

// ---------------------------------------------------------------------------
// The attempt itself
// ---------------------------------------------------------------------------

/// Try to install `mosh-server` on `host`, and say what was **observed**.
///
/// Nothing here spawns anything and nothing here decides where it runs, which
/// is what lets every branch below be driven from a test with no host and no
/// `sudo`. It does write two things: the [`Ledger`] record, which is the
/// decision, and — when a re-probe actually read a version off the host — the
/// version cache, because that reading is only ever seen here.
pub async fn attempt(
    host: &SshHostId,
    h: &impl InstallHost,
    ledger: &Ledger,
    job: Job,
) -> Outcome {
    let now = now_secs();

    // A host that is already new enough is answered before anything is run, and
    // before the ledger is consulted — it is a reading of the host taken a
    // moment ago rather than a decision, so there is nothing to remember and
    // nothing to expire.
    if let Job::Upgrade(from) = job
        && from >= mosh::MIN_SERVER_VERSION
    {
        return Outcome::AlreadyCurrent { version: from.to_string() };
    }

    // A standing answer is returned rather than re-derived, so the panel shows
    // "this host needs a password" on the fiftieth launch exactly as it did on
    // the first — and no command is sent.
    if let Some(standing) = ledger.standing(host, job, now) {
        return standing;
    }

    if !h.is_posix() {
        let outcome = Outcome::Unsupported {
            reason: format!(
                "this is a Windows host, where rmux cannot {} mosh-server",
                job.word()
            ),
        };
        ledger.record(host, job, &outcome, now);
        return outcome;
    }

    let detected = match h.run(&detect_line()).await {
        Ok(out) if out.ok() => out.stdout,
        Ok(out) => {
            let outcome = Outcome::Failed { reason: first_reason(&out) };
            ledger.record(host, job, &outcome, now);
            return outcome;
        }
        Err(e) => {
            let outcome = Outcome::Failed { reason: e.to_string() };
            ledger.record(host, job, &outcome, now);
            return outcome;
        }
    };

    let (manager, privilege) = parse_detection(&detected);
    let Some(manager) = manager else {
        let known = Manager::ALL.iter().map(|m| m.name()).collect::<Vec<_>>().join(", ");
        let outcome = Outcome::Unsupported {
            reason: format!("no package manager rmux knows how to drive ({known})"),
        };
        ledger.record(host, job, &outcome, now);
        return outcome;
    };

    let privileged = match privilege {
        // **The stop.** Not a prompt, not a retry, not the askpass bridge.
        Some(Privilege::NeedsPassword) => {
            let outcome = Outcome::NeedsPassword {
                command: manager.manual_command_for(&host.alias, job),
            };
            ledger.record(host, job, &outcome, now);
            return outcome;
        }
        Some(Privilege::Sudo) => true,
        Some(Privilege::Root) | Some(Privilege::Unprivileged) => false,
        None => {
            let outcome = Outcome::Failed {
                reason: format!(
                    "the host did not say whether it can {} without a password",
                    job.word()
                ),
            };
            ledger.record(host, job, &outcome, now);
            return outcome;
        }
    };

    let root = privileged && manager.needs_root();
    let line = match job {
        Job::Install => manager.install_line(root),
        Job::Upgrade(_) => manager.upgrade_line(root),
    };
    let ran = h.run(&line).await;

    // **The exit status is not the answer.** It is only a reason to print when
    // the re-probe below also says no. `brew upgrade` on an up-to-date formula
    // is the clearest case: it exits non-zero to say "already installed", which
    // is not a failure of anything.
    let complaint = match ran {
        Ok(out) if out.ok() => None,
        Ok(out) => Some(first_reason(&out)),
        Err(e) => Some(e.to_string()),
    };

    // One re-probe, answering both halves: is it there, and which one is it.
    let reprobe = h.run(mosh::server_probe_line()).await;
    let present = matches!(&reprobe, Ok(out) if out.ok());
    let seen = match &reprobe {
        Ok(out) if out.ok() => {
            mosh::parse_server_version(&out.stdout).or_else(|| mosh::parse_server_version(&out.stderr))
        }
        _ => None,
    };

    // The observed version, recorded whatever the job was. A fresh install can
    // perfectly well land an old `mosh-server` — Debian oldstable's archive has
    // 1.3.2 — and without this reading the upgrade question would never be asked
    // about a host rmux had just installed on.
    if present {
        mosh::remember_server_version(host, seen);
    }

    let outcome = match job {
        Job::Install if present => Outcome::Installed { manager: manager.name().to_owned() },
        Job::Install => Outcome::Failed {
            reason: complaint.unwrap_or_else(|| {
                format!(
                    "{} reported success but mosh-server is still not on PATH",
                    manager.name()
                )
            }),
        },

        // An upgrade that ended with no `mosh-server` on `PATH` is the one
        // genuinely alarming result here: the host had one before.
        Job::Upgrade(_) if !present => Outcome::Failed {
            reason: complaint.unwrap_or_else(|| {
                format!(
                    "after {} upgraded it, mosh-server is no longer on PATH",
                    manager.name()
                )
            }),
        },

        Job::Upgrade(from) => match seen {
            // **What was observed, not what the package manager claimed.**
            Some(to) if to >= mosh::MIN_SERVER_VERSION => Outcome::Upgraded {
                manager: manager.name().to_owned(),
                from: from.to_string(),
                to: to.to_string(),
            },
            // It ran, it worked, and the archive had nowhere newer to go — or
            // somewhere newer that is still short of what rmux needs. Either way
            // the operator now owns the decision, and the version they are on is
            // what they need to make it.
            Some(to) => Outcome::NothingNewer {
                manager: manager.name().to_owned(),
                version: to.to_string(),
            },
            // The version read fine before the upgrade and does not now, which
            // is not something to report as an upgrade.
            None => Outcome::Failed {
                reason: complaint.unwrap_or_else(|| {
                    "mosh-server stopped reporting a version it could read".to_owned()
                }),
            },
        },
    };

    ledger.record(host, job, &outcome, now);
    outcome
}

/// Run [`attempt`] under the [`ATTEMPT_TIMEOUT`], settle the claim, and — on a
/// verified success — update the cached availability so the panel and the next
/// session see the new answer without an app restart.
pub async fn run_claimed(
    host: &SshHostId,
    h: &impl InstallHost,
    ledger: &Ledger,
    job: Job,
    claim: Claim,
) {
    let outcome = match tokio::time::timeout(ATTEMPT_TIMEOUT, attempt(host, h, ledger, job)).await {
        Ok(outcome) => outcome,
        Err(_) => Outcome::Failed {
            reason: format!(
                "the {} did not finish within {} seconds",
                job.word(),
                ATTEMPT_TIMEOUT.as_secs()
            ),
        },
    };

    match &outcome {
        Outcome::Installed { .. } => {
            mosh::remember(host, mosh::Availability::Available);
            tracing::info!(host = %host.label(), "mosh-server installed; low latency is now available");
        }
        // The cached version is updated for the same reason the cached
        // availability is after an install: without it the panel would go on
        // offering to upgrade a host that had just been upgraded, and the next
        // probe of this run would start the whole attempt again.
        Outcome::Upgraded { to, .. } => {
            mosh::remember(host, mosh::Availability::Available);
            mosh::remember_server_version(host, mosh::Version::parse(to));
            tracing::info!(host = %host.label(), version = %to, "mosh-server upgraded");
        }
        Outcome::NothingNewer { manager, version } => {
            // Recorded as what it is: the host is still on an old mosh-server and
            // nobody but the operator can change that.
            mosh::remember_server_version(host, mosh::Version::parse(version));
            tracing::info!(
                host = %host.label(), %manager, %version,
                "no newer mosh-server is available from this host's repositories"
            );
        }
        other => {
            tracing::info!(host = %host.label(), job = job.word(), outcome = tag(other), "mosh-server was not changed");
        }
    }

    claim.settle(outcome);
}

/// The host's own complaint, shortened to something that fits beside a control.
///
/// stderr first, because that is where a package manager explains itself; the
/// last non-empty line, because the useful sentence is at the end of a wall of
/// progress output.
fn first_reason(out: &Output) -> String {
    let pick = |text: &str| {
        text.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned)
    };
    pick(&out.stderr)
        .or_else(|| pick(&out.stdout))
        .unwrap_or_else(|| format!("the install exited {}", out.status))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that records every line it was asked to run and answers from a
    /// script. **Nothing in this module's tests touches a real machine, a real
    /// `sudo` or the operator's `~/.rmux`.**
    struct Fake {
        posix: bool,
        /// `(substring the line must contain, status, stdout)` — matched in
        /// order, first match wins, and an unmatched line is a failure so a
        /// test cannot pass by accident on a command it never described.
        script: Vec<(&'static str, i32, &'static str)>,
        ran: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(script: Vec<(&'static str, i32, &'static str)>) -> Self {
            Self { posix: true, script, ran: Default::default() }
        }
        fn windows(script: Vec<(&'static str, i32, &'static str)>) -> Self {
            Self { posix: false, script, ran: Default::default() }
        }
        fn lines(&self) -> Vec<String> {
            self.ran.lock().clone()
        }
    }

    #[async_trait]
    impl InstallHost for Fake {
        async fn run(&self, line: &str) -> anyhow::Result<Output> {
            self.ran.lock().push(line.to_owned());
            for (needle, status, stdout) in &self.script {
                if line.contains(needle) {
                    return Ok(Output {
                        status: *status,
                        stdout: (*stdout).to_owned(),
                        stderr: if *status == 0 { String::new() } else { (*stdout).to_owned() },
                    });
                }
            }
            Ok(Output { status: 127, stdout: String::new(), stderr: "no such command".to_owned() })
        }

        fn is_posix(&self) -> bool {
            self.posix
        }
    }

    /// Each test gets its own ledger file, in its own directory.
    fn ledger(name: &str) -> (Ledger, PathBuf) {
        let dir = std::env::temp_dir().join(format!("rmux-mosh-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Ledger::at(dir.join("mosh-install.json")), dir)
    }

    fn host(alias: &str) -> SshHostId {
        SshHostId::new(alias)
    }

    fn v(major: u32, minor: u32, patch: u32) -> mosh::Version {
        mosh::Version { major, minor, patch }
    }

    /// What a host's `mosh-server --version` prints, as the probe reads it.
    fn says(version: &str) -> String {
        format!("mosh-server (mosh {version})")
    }

    /// The happy path, and the two things it must have done besides succeed.
    #[tokio::test]
    async fn a_verified_install_updates_the_cached_availability() {
        let (led, _dir) = ledger("installed");
        let h = host("apt-host-installed");
        let fake = Fake::new(vec![
            ("for c in apt-get", 0, "apt-get sudo"),
            ("apt-get install -y", 0, ""),
            ("command -v mosh-server", 0, ""),
        ]);

        let claim = claim(&h, Job::Install).expect("first claim");
        run_claimed(&h, &fake, &led, Job::Install, claim).await;

        assert_eq!(status(&h).await, Some(Outcome::Installed { manager: "apt-get".to_owned() }));
        // The reason this matters: without it the cache holds
        // `RemoteServerMissing` for the rest of the run, so the operator
        // installs mosh and is then told the host has none until they restart.
        assert_eq!(mosh::cached(&h), Some(mosh::Availability::Available));
        assert!(mosh::availability_for(&h, &NeverProbe).await.is_available());
    }

    struct NeverProbe;
    #[async_trait]
    impl mosh::Probe for NeverProbe {
        fn local_client(&self) -> bool {
            panic!("a settled host must not be re-probed");
        }
        async fn remote_server(&self) -> mosh::ServerProbe {
            panic!("a settled host must not be re-probed");
        }
    }

    /// The fail2ban rule, which is the whole point of the module.
    #[tokio::test]
    async fn a_password_host_is_told_once_and_never_asked_again() {
        let (led, _dir) = ledger("needs-password");
        let h = host("password-host");
        let fake = Fake::new(vec![("for c in apt-get", 0, "apt-get needs-password")]);

        let first = attempt(&h, &fake, &led, Job::Install).await;
        match &first {
            Outcome::NeedsPassword { command } => {
                assert!(command.contains("sudo apt-get install -y mosh"), "{command}");
                assert!(command.starts_with("ssh password-host "), "{command}");
                // `sudo -n` is rmux's rule, not the operator's: by hand they
                // have a terminal, and `-n` would only make it fail.
                assert!(!command.contains("sudo -n"), "{command}");
            }
            other => panic!("expected NeedsPassword, got {other:?}"),
        }

        // Exactly one command was sent: the detection. No install was tried,
        // and above all no second `sudo`.
        assert_eq!(fake.lines().len(), 1, "{:?}", fake.lines());
        assert!(!fake.lines()[0].contains("apt-get install"), "{:?}", fake.lines());

        // And it survives a restart: a fresh ledger read of the same file
        // answers from the record without sending anything at all.
        let again = Fake::new(vec![]);
        assert_eq!(attempt(&h, &again, &led, Job::Install).await, first);
        assert!(again.lines().is_empty(), "a recorded password host was asked again");
    }

    /// A failure is retried, but only after the cooling-off period.
    #[tokio::test]
    async fn a_failure_waits_seven_days() {
        let (led, _dir) = ledger("retry-window");
        let h = host("broken-repos");
        let outcome = Outcome::Failed { reason: "E: Unable to locate package mosh".to_owned() };
        let at = 1_000_000_000u64;
        led.record(&h, Job::Install, &outcome, at);

        assert_eq!(led.standing(&h, Job::Install, at + 1), Some(outcome.clone()));
        assert_eq!(led.standing(&h, Job::Install, at + RETRY_AFTER.as_secs() - 1), Some(outcome));
        assert_eq!(
            led.standing(&h, Job::Install, at + RETRY_AFTER.as_secs()),
            None,
            "a week later the host deserves another go"
        );

        // A password host does not expire at all — that is the difference.
        let p = host("password-forever");
        led.record(&p, Job::Install, &Outcome::NeedsPassword { command: "ssh p ...".to_owned() }, at);
        assert!(led.standing(&p, Job::Install, at + 10 * RETRY_AFTER.as_secs()).is_some());
        // Nor does a success.
        let i = host("already-done");
        led.record(&i, Job::Install, &Outcome::Installed { manager: "apk".to_owned() }, at);
        assert!(led.standing(&i, Job::Install, at + 10 * RETRY_AFTER.as_secs()).is_some());
    }

    /// A POSIX install line must never be emitted at a host whose login shell
    /// is `cmd.exe` behind a located `bash`.
    #[tokio::test]
    async fn a_windows_target_is_never_sent_an_install_line() {
        let (led, _dir) = ledger("windows");
        let h = host("win11");
        let fake = Fake::windows(vec![("", 0, "apt-get sudo")]);

        match attempt(&h, &fake, &led, Job::Install).await {
            Outcome::Unsupported { reason } => assert!(reason.contains("Windows"), "{reason}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert!(fake.lines().is_empty(), "a Windows host was sent {:?}", fake.lines());
    }

    #[tokio::test]
    async fn an_unknown_package_manager_stops_rather_than_guessing() {
        let (led, _dir) = ledger("unsupported");
        let h = host("exotic");
        let fake = Fake::new(vec![("for c in apt-get", 0, "unknown none")]);

        match attempt(&h, &fake, &led, Job::Install).await {
            Outcome::Unsupported { reason } => assert!(reason.contains("apt-get"), "{reason}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert_eq!(fake.lines().len(), 1, "something was run after the detection failed");
    }

    /// A manager exiting 0 is not proof the binary is on `PATH`.
    #[tokio::test]
    async fn success_is_the_re_probe_and_not_the_exit_status() {
        let (led, _dir) = ledger("unverified");
        let h = host("lying-host");
        let fake = Fake::new(vec![
            ("for c in apt-get", 0, "dnf root"),
            ("dnf -y install", 0, ""),
            ("command -v mosh-server", 1, ""),
        ]);

        match attempt(&h, &fake, &led, Job::Install).await {
            Outcome::Failed { reason } => assert!(reason.contains("still not on PATH"), "{reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(mosh::cached(&h), None, "an unverified install must not claim availability");
    }

    #[test]
    fn every_manager_has_a_documented_non_interactive_line() {
        for (manager, must_contain) in [
            (Manager::AptGet, "DEBIAN_FRONTEND=noninteractive"),
            (Manager::Dnf, "dnf -y install"),
            (Manager::Yum, "yum -y install"),
            (Manager::Apk, "apk add --no-cache"),
            (Manager::Pacman, "pacman -S --noconfirm"),
            (Manager::Zypper, "zypper -n install"),
            (Manager::Brew, "brew install"),
        ] {
            let line = manager.install_line(manager.needs_root());
            assert!(line.contains(must_contain), "{manager:?}: {line}");
            assert!(line.contains("mosh"), "{manager:?}: {line}");
        }
        assert!(Manager::AptGet.install_line(true).contains("apt-get install -y"));
    }

    /// Homebrew refuses to run as root, so a `sudo` here is not a precaution —
    /// it is a guaranteed failure on every macOS target.
    #[test]
    fn brew_never_carries_sudo() {
        assert!(!Manager::Brew.needs_root());
        assert!(!Manager::Brew.install_line(true).contains("sudo"));
        assert!(!Manager::Brew.install_line(false).contains("sudo"));
        assert!(!Manager::Brew.manual_command("mac-host").contains("sudo"));
        for other in Manager::ALL.iter().filter(|m| **m != Manager::Brew) {
            assert!(other.install_line(true).contains("sudo -n "), "{other:?}");
        }
    }

    /// The install line carries no host-controlled text at all, and the by-hand
    /// command quotes the one thing it does carry.
    #[test]
    fn a_hostile_alias_cannot_escape_into_a_command() {
        let hostile = "evil'; touch /tmp/pwned; echo '";
        let command = Manager::AptGet.manual_command(hostile);
        assert!(command.contains("pwned"), "the alias should still be shown: {command}");
        // Quoted, so the `;` is text rather than a statement separator. The
        // only unquoted words in the line are `ssh` itself.
        assert!(command.starts_with("ssh 'evil'"), "{command}");
        assert_eq!(
            command.matches("touch /tmp/pwned").count(),
            1,
            "the payload appears more than once, so something un-quoted it: {command}"
        );

        // And the line actually sent to the host interpolates nothing.
        for manager in Manager::ALL {
            let line = manager.install_line(manager.needs_root());
            assert!(!line.contains("pwned"), "{manager:?}: {line}");
            assert!(!line.contains('\''), "no quoting is needed and none appears: {line}");
        }
        assert!(!detect_line().contains("pwned"));
    }

    #[test]
    fn the_detection_asks_both_questions_in_one_line() {
        let line = detect_line();
        for manager in Manager::ALL {
            assert!(line.contains(manager.name()), "{} missing: {line}", manager.name());
        }
        assert!(line.contains("sudo -n true"), "{line}");
        // `sudo` must not be attempted for brew, which never needs it — an
        // attempt would put a failed sudo in the logs of a machine that never
        // required one.
        assert!(line.contains("[ \"$m\" = brew ]"), "{line}");
        assert_eq!(line.lines().count(), 1, "the detection must be one line: {line}");
    }

    #[test]
    fn the_detection_is_read_back_exactly() {
        assert_eq!(
            parse_detection("apt-get sudo\n"),
            (Some(Manager::AptGet), Some(Privilege::Sudo))
        );
        assert_eq!(parse_detection("brew unprivileged"), (Some(Manager::Brew), Some(Privilege::Unprivileged)));
        assert_eq!(parse_detection("apk root"), (Some(Manager::Apk), Some(Privilege::Root)));
        assert_eq!(
            parse_detection("zypper needs-password"),
            (Some(Manager::Zypper), Some(Privilege::NeedsPassword))
        );
        assert_eq!(parse_detection("unknown none"), (None, None));
        assert_eq!(parse_detection(""), (None, None));
    }

    /// Off means off: nothing attempted, nothing written, no claim to await.
    #[tokio::test]
    async fn the_off_switch_suppresses_the_attempt_entirely() {
        let h = host("opted-out");
        set_enabled(false);
        assert!(!enabled());

        // This is the gate `SshTarget::spawn_mosh_install` consults before it
        // claims anything, expressed here because the spawn itself needs a
        // host. Nothing was claimed, so there is nothing to report.
        assert_eq!(status(&h).await, None);

        set_enabled(true);
        assert!(enabled());
    }

    /// Several targets exist for one host and any of them may probe.
    #[tokio::test]
    async fn one_host_is_claimed_once() {
        let h = host("contended");
        let first = claim(&h, Job::Install).expect("the first claim");
        assert!(claim(&h, Job::Install).is_none(), "a second task claimed the same host");
        first.settle(Outcome::Installed { manager: "apk".to_owned() });
        assert_eq!(status(&h).await, Some(Outcome::Installed { manager: "apk".to_owned() }));
    }

    /// A claim dropped without an answer would leave the panel on "installing"
    /// for the life of the app.
    #[tokio::test]
    async fn a_dropped_claim_still_answers() {
        let h = host("abandoned");
        drop(claim(&h, Job::Install).expect("claim"));
        match status(&h).await {
            Some(Outcome::Failed { reason }) => assert!(reason.contains("without reporting"), "{reason}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn the_ledger_is_owner_only_in_an_owner_only_directory() {
        let (led, dir) = ledger("modes");
        led.record(&host("m"), Job::Install, &Outcome::Installed { manager: "apk".to_owned() }, 1);

        assert!(led.path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = std::fs::metadata(&led.path).unwrap().permissions().mode() & 0o777;
            let folder = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(file, 0o600, "the ledger is {file:o}");
            assert_eq!(folder, 0o700, "its directory is {folder:o}");
        }
        // No temp file left behind — the rename is what publishes it.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The default location, without writing to it.
    #[test]
    fn the_ledger_lives_beside_the_other_rmux_state() {
        let path = Ledger::default_path().expect("a home directory");
        assert!(path.ends_with(".rmux/mosh-install.json"), "{}", path.display());
    }

    // -----------------------------------------------------------------
    // Upgrading a host whose mosh-server is too old
    // -----------------------------------------------------------------

    /// The trigger itself: 1.3.2 is upgraded, and the new version is the one
    /// the **re-probe** reported rather than the one the manager claimed.
    #[tokio::test]
    async fn an_old_server_is_upgraded_and_the_new_version_is_observed() {
        let (led, _dir) = ledger("upgrade");
        let h = host("old-mosh-host");
        let fake = Fake::new(vec![
            ("for c in apt-get", 0, "apt-get sudo"),
            ("--only-upgrade", 0, ""),
            ("command -v mosh-server", 0, "mosh-server (mosh 1.4.0)"),
        ]);

        let claim = claim(&h, Job::Upgrade(v(1, 3, 2))).expect("first claim");
        run_claimed(&h, &fake, &led, Job::Upgrade(v(1, 3, 2)), claim).await;

        assert_eq!(
            status(&h).await,
            Some(Outcome::Upgraded {
                manager: "apt-get".to_owned(),
                from: "1.3.2".to_owned(),
                to: "1.4.0".to_owned(),
            })
        );
        // Without this the panel would go on offering to upgrade a host that
        // had just been upgraded, for the rest of the run.
        assert_eq!(mosh::cached_server_version(&h), Some(v(1, 4, 0)));
        assert!(!mosh::is_outdated(mosh::cached_server_version(&h)));
    }

    /// Debian oldstable's archive holds 1.3.2, so a *successful install* can
    /// perfectly well land a server that is already too old. The version read
    /// during verification is kept, which is what lets the upgrade question be
    /// asked about a host rmux had just installed on — without it that host
    /// would be left on 1.3.2 for ever with nothing saying why.
    #[tokio::test]
    async fn an_install_that_lands_an_old_server_still_records_its_version() {
        let (led, _dir) = ledger("install-old");
        let h = host("oldstable");
        let fake = Fake::new(vec![
            ("for c in apt-get", 0, "apt-get root"),
            ("apt-get install -y", 0, ""),
            ("command -v mosh-server", 0, "mosh-server (mosh 1.3.2)"),
        ]);

        assert_eq!(
            attempt(&h, &fake, &led, Job::Install).await,
            Outcome::Installed { manager: "apt-get".to_owned() }
        );
        assert_eq!(mosh::cached_server_version(&h), Some(v(1, 3, 2)));
        assert!(mosh::is_outdated(mosh::cached_server_version(&h)));
    }

    /// A host that is already new enough is answered without sending anything.
    #[tokio::test]
    async fn a_current_server_is_never_touched() {
        let (led, _dir) = ledger("current");
        for version in [v(1, 4, 0), v(1, 5, 1)] {
            let h = host(&format!("current-{version}"));
            let fake = Fake::new(vec![]);
            let outcome = attempt(&h, &fake, &led, Job::Upgrade(version)).await;
            assert_eq!(outcome, Outcome::AlreadyCurrent { version: version.to_string() });
            assert!(fake.lines().is_empty(), "{version} was sent {:?}", fake.lines());
        }
        // And nothing was written: "already current" is a reading of the host,
        // not a decision, so it must not be able to veto a later minimum.
        assert!(!led.path.exists() || led.load().is_empty(), "already-current was recorded");
    }

    /// The conservative rule, at the level that matters: a version rmux cannot
    /// read never reaches this module at all, because `is_outdated(None)` is
    /// false — and if it somehow did, the detection would still not run.
    #[tokio::test]
    async fn an_unreadable_version_is_not_an_upgrade_trigger() {
        assert!(!mosh::is_outdated(None));
        assert!(!mosh::is_outdated(mosh::parse_server_version("mosh-server: unknown option")));
        assert!(mosh::is_outdated(mosh::parse_server_version(&says("1.3.2"))));
        assert!(!mosh::is_outdated(mosh::parse_server_version(&says("1.4.0"))));
    }

    /// The case the operator has to act on themselves. The upgrade *succeeded*
    /// and the version did not move, so reporting it as an upgrade would be a
    /// claim the re-probe flatly contradicts.
    #[tokio::test]
    async fn a_repository_with_nothing_newer_says_so_rather_than_claiming_success() {
        let (led, _dir) = ledger("nothing-newer");
        let h = host("debian-oldstable");
        let fake = Fake::new(vec![
            ("for c in apt-get", 0, "apt-get root"),
            ("--only-upgrade", 0, "mosh is already the newest version (1.3.2-2.1)."),
            ("command -v mosh-server", 0, "mosh-server (mosh 1.3.2)"),
        ]);

        let outcome = attempt(&h, &fake, &led, Job::Upgrade(v(1, 3, 2))).await;
        assert_eq!(
            outcome,
            Outcome::NothingNewer { manager: "apt-get".to_owned(), version: "1.3.2".to_owned() },
            "an unchanged version is not an upgrade"
        );

        // And it is not retried on the next reconnect, only after the window.
        let again = Fake::new(vec![]);
        assert_eq!(attempt(&h, &again, &led, Job::Upgrade(v(1, 3, 2))).await, outcome);
        assert!(again.lines().is_empty(), "the host was asked again: {:?}", again.lines());
        assert_eq!(led.standing(&h, Job::Upgrade(v(1, 3, 2)), now_secs() + RETRY_AFTER.as_secs()), None);
    }

    /// `sudo -n` failing is the whole point of the module, and it has to hold on
    /// this path identically: told once, no second attempt, and the command the
    /// operator can run is the *upgrade* one.
    #[tokio::test]
    async fn a_password_host_is_not_upgraded_and_is_not_asked_twice() {
        let (led, _dir) = ledger("upgrade-password");
        let h = host("upgrade-password-host");
        let fake = Fake::new(vec![("for c in apt-get", 0, "apt-get needs-password")]);

        let first = attempt(&h, &fake, &led, Job::Upgrade(v(1, 3, 2))).await;
        match &first {
            Outcome::NeedsPassword { command } => {
                // The *upgrade* command. Handing them a plain install for a host
                // that already has mosh is a command that exits 0 and changes
                // nothing, which reads as rmux being wrong about the version.
                assert!(command.contains("--only-upgrade"), "{command}");
                assert!(!command.contains("sudo -n"), "{command}");
            }
            other => panic!("expected NeedsPassword, got {other:?}"),
        }

        // One command was sent — the detection. No `sudo` upgrade, and above all
        // no second password attempt: that is the fail2ban rule.
        assert_eq!(fake.lines().len(), 1, "{:?}", fake.lines());
        assert!(!fake.lines()[0].contains("--only-upgrade"), "{:?}", fake.lines());

        let again = Fake::new(vec![]);
        assert_eq!(attempt(&h, &again, &led, Job::Upgrade(v(1, 3, 2))).await, first);
        assert!(again.lines().is_empty(), "a recorded password host was asked again");
    }

    /// A failed upgrade is left alone for the same week an install is.
    #[tokio::test]
    async fn a_failed_upgrade_is_not_retried_inside_the_window() {
        let (led, _dir) = ledger("upgrade-retry");
        let h = host("upgrade-breaks");
        let job = Job::Upgrade(v(1, 3, 2));
        let fake = Fake::new(vec![
            ("for c in apt-get", 0, "dnf root"),
            ("dnf -y upgrade", 1, "Error: Transaction test error"),
            // The server is still there, still old — so the *reason* is the
            // host's own, not a generic one.
            ("command -v mosh-server", 0, "mosh-server (mosh 1.3.2)"),
        ]);

        // A run whose upgrade left the version where it was reads as "nothing
        // newer", which is the honest reading; a run where the manager failed
        // *and* the server went away is the failure below.
        let outcome = attempt(&h, &fake, &led, job).await;
        assert!(matches!(outcome, Outcome::NothingNewer { .. }), "{outcome:?}");

        let gone = host("upgrade-destroys");
        let broken = Fake::new(vec![
            ("for c in apt-get", 0, "dnf root"),
            ("dnf -y upgrade", 1, "Error: Transaction test error"),
            ("command -v mosh-server", 1, ""),
        ]);
        match attempt(&gone, &broken, &led, job).await {
            Outcome::Failed { reason } => assert!(reason.contains("Transaction test error"), "{reason}"),
            other => panic!("expected Failed, got {other:?}"),
        }

        // And it is not retried until the window closes.
        let again = Fake::new(vec![]);
        assert!(matches!(attempt(&gone, &again, &led, job).await, Outcome::Failed { .. }));
        assert!(again.lines().is_empty(), "a failed upgrade was retried: {:?}", again.lines());
        assert_eq!(led.standing(&gone, job, now_secs() + RETRY_AFTER.as_secs()), None);
    }

    /// The two questions about one host are two records. Folding them together
    /// would have one answer silently overwrite the other.
    #[tokio::test]
    async fn an_install_record_does_not_answer_the_upgrade_question() {
        let (led, _dir) = ledger("two-records");
        let h = host("both");
        let at = 1_000_000_000u64;
        led.record(&h, Job::Install, &Outcome::Installed { manager: "apt-get".to_owned() }, at);

        assert!(led.standing(&h, Job::Install, at + 1).is_some());
        assert_eq!(
            led.standing(&h, Job::Upgrade(v(1, 3, 2)), at + 1),
            None,
            "the install record answered the upgrade question"
        );
    }

    /// Raising the minimum in a later release must not be vetoed by a record
    /// written when the minimum was lower — that is "stale means silently
    /// wrong": the panel would report a host as upgraded while rmux went on
    /// treating it as too old.
    #[tokio::test]
    async fn an_upgrade_record_below_the_current_minimum_stops_standing() {
        let (led, _dir) = ledger("raised-minimum");
        let h = host("upgraded-once");
        let job = Job::Upgrade(v(1, 2, 0));
        let at = 1_000_000_000u64;

        led.record(
            &h,
            job,
            &Outcome::Upgraded {
                manager: "apk".to_owned(),
                from: "1.2.0".to_owned(),
                // As if a past release had been content with this.
                to: "1.3.2".to_owned(),
            },
            at,
        );
        assert_eq!(led.standing(&h, job, at + 1), None, "1.3.2 is below the minimum rmux asks for");

        // An upgrade that reached the current minimum stands for ever.
        led.record(
            &h,
            job,
            &Outcome::Upgraded {
                manager: "apk".to_owned(),
                from: "1.3.2".to_owned(),
                to: mosh::MIN_SERVER_VERSION.to_string(),
            },
            at,
        );
        assert!(led.standing(&h, job, at + 10 * RETRY_AFTER.as_secs()).is_some());
    }

    /// Off means off on this path too. The gate is consulted by
    /// `SshTarget::spawn_mosh_job` before anything is claimed, so there is
    /// nothing to await and no record.
    #[tokio::test]
    async fn the_off_switch_suppresses_the_upgrade_too() {
        let h = host("opted-out-of-upgrades");
        set_enabled(false);
        assert!(!enabled());
        assert_eq!(status(&h).await, None, "an attempt was claimed with the switch off");
        set_enabled(true);
    }

    #[test]
    fn every_manager_has_an_upgrade_line_that_is_an_upgrade() {
        for (manager, must_contain) in [
            (Manager::AptGet, "apt-get install -y --only-upgrade"),
            (Manager::Dnf, "dnf -y upgrade"),
            (Manager::Yum, "yum -y update"),
            (Manager::Apk, "apk upgrade --no-cache"),
            // Arch has no per-package upgrade verb; `-S` on an installed
            // package *is* the upgrade.
            (Manager::Pacman, "pacman -S --noconfirm"),
            (Manager::Zypper, "zypper -n update"),
            (Manager::Brew, "brew upgrade"),
        ] {
            let line = manager.upgrade_line(manager.needs_root());
            assert!(line.contains(must_contain), "{manager:?}: {line}");
            assert!(line.contains("mosh"), "{manager:?}: {line}");
            // Nothing may stop and ask a question on a host with no terminal.
            assert!(!line.contains("-i "), "{manager:?} looks interactive: {line}");
        }
        assert!(
            Manager::AptGet.upgrade_line(true).contains("DEBIAN_FRONTEND=noninteractive"),
            "{}",
            Manager::AptGet.upgrade_line(true)
        );
    }

    /// The `sudo -n` rule and the brew exemption, on the upgrade line.
    #[test]
    fn the_upgrade_line_carries_sudo_n_and_brew_carries_none() {
        assert!(!Manager::Brew.upgrade_line(true).contains("sudo"));
        assert!(!Manager::Brew.upgrade_line(false).contains("sudo"));
        assert!(
            !Manager::Brew.manual_command_for("mac-host", Job::Upgrade(v(1, 3, 2))).contains("sudo")
        );
        for other in Manager::ALL.iter().filter(|m| **m != Manager::Brew) {
            let line = other.upgrade_line(true);
            assert!(line.contains("sudo -n "), "{other:?}: {line}");
            // Never a bare `sudo`, which would prompt.
            assert!(!line.replace("sudo -n ", "").contains("sudo"), "{other:?}: {line}");
        }
    }

    /// Nothing host-controlled reaches an upgrade line either, and the by-hand
    /// command quotes the one thing it carries.
    #[test]
    fn a_hostile_alias_cannot_escape_into_an_upgrade_command() {
        let hostile = "evil'; touch /tmp/pwned; echo '";
        let command = Manager::Dnf.manual_command_for(hostile, Job::Upgrade(v(1, 3, 2)));
        assert!(command.starts_with("ssh 'evil'"), "{command}");
        assert_eq!(command.matches("touch /tmp/pwned").count(), 1, "{command}");
        for manager in Manager::ALL {
            let line = manager.upgrade_line(manager.needs_root());
            assert!(!line.contains("pwned"), "{manager:?}: {line}");
            assert!(!line.contains('\''), "no quoting is needed and none appears: {line}");
        }
    }

    #[test]
    fn a_destination_is_the_whole_host_id() {
        assert_eq!(key(&SshHostId::new("a"), Job::Install), "a");
        assert_eq!(
            key(
                &SshHostId { alias: "a".into(), user: Some("root".into()), port: Some(2222) },
                Job::Install
            ),
            "root@a:2222"
        );
        // The upgrade is a second question about the same machine, and one
        // record cannot hold two answers.
        assert_eq!(
            key(&SshHostId::new("a"), Job::Upgrade(v(1, 3, 2))),
            "a#upgrade",
            "the version must not enter the key, or every version would re-attempt"
        );
        assert_eq!(key(&SshHostId::new("a"), Job::Upgrade(v(1, 0, 0))), "a#upgrade");
    }
}
