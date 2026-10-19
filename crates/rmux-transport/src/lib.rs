//! The seam between "where work happens" and everything that does work.
//!
//! Terminals, file I/O, metrics and Claude session control are written against
//! [`Target`] and nothing else. There are two implementations — the local machine
//! and an SSH host — and callers cannot tell them apart. That is deliberate: the
//! previous generation of this app grew a separate code path for local operation
//! and the two drifted until neither was trustworthy.
//!
//! The most important method is [`Target::build_command`]. It does *not* run
//! anything remotely; it resolves a logical command into an argv that the **local**
//! machine spawns. For an SSH target that means wrapping the request in
//! `ssh -t <host> -- <program>`. Terminal code therefore always spawns a local PTY
//! and never learns that SSH exists.

use std::collections::BTreeMap;
use std::ffi::OsString;

use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

pub mod console;
pub mod local;

pub use console::NoConsoleWindow;
pub use local::LocalTarget;

/// Identifies a place work can happen. Cheap to clone; used as a map key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TargetId {
    Local,
    /// An SSH destination. `alias` is passed to `ssh` verbatim.
    Ssh(SshHostId),
}

impl TargetId {
    pub fn is_local(&self) -> bool {
        matches!(self, TargetId::Local)
    }

    /// Short label for window titles and tabs.
    pub fn label(&self) -> String {
        match self {
            TargetId::Local => "local".to_owned(),
            TargetId::Ssh(host) => host.label(),
        }
    }
}

/// An SSH destination.
///
/// `alias` is handed to the `ssh` binary **verbatim and unparsed**. It is very
/// often a `Host` alias from `~/.ssh/config`, in which case the real hostname,
/// user, port, identity file and any `ProxyJump` live in that file — resolving it
/// ourselves would mean reimplementing OpenSSH's config grammar (`Match`,
/// `Include`, `%h`/`%p`/`%r` tokens, canonicalisation), which no Rust crate does
/// completely. `user` and `port` are overrides for hosts typed in by hand; when
/// they are `None` the config supplies them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SshHostId {
    pub alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl SshHostId {
    pub fn new(alias: impl Into<String>) -> Self {
        Self { alias: alias.into(), user: None, port: None }
    }

    pub fn label(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.alias),
            None => self.alias.clone(),
        }
    }
}

/// Which OS a target runs, discovered once at connect time.
///
/// Metrics collection branches on this — Linux has `/proc`, macOS does not — so
/// it is resolved eagerly rather than probed per sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Platform::Linux,
            "macos" => Platform::MacOs,
            "windows" => Platform::Windows,
            _ => Platform::Other,
        }
    }

    /// Whether `/proc/stat` and `/proc/meminfo` can be read for metrics.
    /// Whether `/proc/stat` and `/proc/meminfo` exist, in Linux format.
    ///
    /// **Windows counts.** Not the OS itself — but rmux reaches a Windows host
    /// through Git for Windows' shell, and MSYS2 emulates procfs faithfully:
    /// measured on a real machine, `/proc/meminfo` reports `MemTotal:` in kB and
    /// `/proc/stat` opens with a `cpu` line, both exactly as the Linux collector
    /// already parses them. Excluding Windows here bought nothing and cost the
    /// host panel on every Windows target.
    pub fn has_procfs(self) -> bool {
        matches!(self, Platform::Linux | Platform::Windows)
    }
}

/// Where a POSIX shell lives on a Windows machine, in order of likelihood.
///
/// One list, used twice and for different reasons — which is why it lives here
/// rather than in either caller. `rmux-ssh` needs it to *reach* a Windows host
/// (as a path `cmd.exe` can execute), and the agent needs it once it is
/// *running* on one, to spawn the shell a session lives in. Two lists would
/// drift, and the failure would be a host rmux could connect to but not open a
/// terminal on.
///
/// **Short (8.3) paths.** The real location is `C:\Program Files\Git\bin\bash.exe`,
/// and that space would have to be quoted inside a command line `cmd` is already
/// re-parsing.
pub const WINDOWS_BASH_CANDIDATES: &[&str] = &[
    r"C:\PROGRA~1\Git\bin\bash.exe",
    r"C:\PROGRA~2\Git\bin\bash.exe",
    r"C:\msys64\usr\bin\bash.exe",
    r"C:\cygwin64\bin\bash.exe",
];

/// Whether the command needs a TTY allocated.
///
/// This maps to `ssh -t` and matters more than it looks: without it a remote
/// shell gets no job control and full-screen programs (including `claude`)
/// misbehave in ways that are tedious to diagnose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tty {
    /// Allocate a TTY — terminals, and anything interactive.
    #[default]
    Allocate,
    /// No TTY — one-shot commands whose output we parse.
    None,
}

/// A command described in terms of the *target*, before it is resolved into
/// something locally spawnable.
#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<Utf8PathBuf>,
    pub tty: Tty,
    /// **A request, not a transport.**
    ///
    /// The caller is saying one thing: *this is an interactive session where
    /// echo latency is what the operator feels*. It is not saying which
    /// protocol to use, and it must never be read that way — naming a protocol
    /// here would put transport knowledge in the layer whose entire job is to
    /// not have any, which is the same mistake as an `if is_local` branch in
    /// feature code.
    ///
    /// **Each [`Target`] decides what to do about it.** A local PTY has no
    /// round trip at all, so [`LocalTarget`] ignores the flag entirely and its
    /// argv is unchanged. A target that reaches across a network may answer it
    /// by choosing a predictive transport, or may decline — in which case the
    /// decline has to be *visible*, because a session the operator asked to be
    /// low-latency and quietly is not is the silently-wrong kind of wrong.
    ///
    /// Defaults to `false`.
    pub low_latency: bool,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            tty: Tty::default(),
            low_latency: false,
        }
    }

    /// The target's default interactive login shell.
    ///
    /// Resolved on the far side rather than here — for SSH we deliberately let
    /// the remote `sshd` pick, which respects the remote user's `chsh`.
    ///
    /// **`-i` as well as `-l`, and that is load-bearing.** `claude` is installed
    /// by a version manager, and which startup file that manager writes its PATH
    /// into depends on the shell:
    ///
    /// | shell | `-l` reads | `-i` reads |
    /// |---|---|---|
    /// | bash | `.bash_profile`, `.profile` | `.bashrc` |
    /// | zsh  | `.zprofile`, `.zlogin` | `.zshrc` |
    ///
    /// nvm, fnm, bun and volta overwhelmingly write to `.bashrc`/`.zshrc` — the
    /// *interactive* files — so a login-only shell finds nothing and reports
    /// `command not found: claude` on a host where it is plainly installed and
    /// works when the operator types it. Measured: a zsh host failed under
    /// `-l` alone while a bash host whose PATH came from `.profile` did not,
    /// which is exactly why this went unnoticed until a zsh machine appeared.
    ///
    /// `-c` still runs the command and exits, so nothing about this makes the
    /// shell wait for input.
    pub fn login_shell() -> Self {
        Self::new("$SHELL").arg("-l").arg("-i")
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<Utf8PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn tty(mut self, tty: Tty) -> Self {
        self.tty = tty;
        self
    }

    /// Ask the target to minimise echo latency if it can — see
    /// [`CommandSpec::low_latency`] for what that does and does not promise.
    pub fn low_latency(mut self, yes: bool) -> Self {
        self.low_latency = yes;
        self
    }
}

/// A command that can be spawned on the local machine right now — in a PTY, or
/// with `tokio::process::Command`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Environment for the **local** process. For SSH targets this carries
    /// `SSH_ASKPASS` and friends, not the user's requested env — that is encoded
    /// into the remote command line instead.
    pub env: BTreeMap<String, String>,
}

/// A reserved slot on whatever connection a channel rides.
///
/// **Opaque on purpose.** Only the [`Target`] that issued it knows what it is
/// accounting — for SSH it is one of at most
/// `MAX_CHANNELS_PER_MASTER` slots on one `ControlMaster`, for a local target
/// there is nothing to account at all — and the caller's whole job is to *hold*
/// it for exactly as long as the channel lives, then drop it.
///
/// It exists because the thing being bounded is not knowable where it is spent.
/// sshd's `MaxSessions` caps the channels on **one connection** (default 10),
/// and rmux puts terminals, Claude panes, file listings, metrics samples and
/// transcript tails on the same one. Measured: 39 client commands queued onto a
/// connection that permits 10, and three panes stuck on `CONNECTING` for 163
/// seconds because everything past the tenth waits for a slot that the sessions
/// already holding one are themselves blocked from releasing.
///
/// A terminal holds its channel for its entire lifetime, so the count cannot be
/// derived from "what is in flight" — somebody has to say when it ended. That
/// somebody is whoever owns the spawned process, which is why this travels out
/// of [`Target::open_channel`] rather than being bookkeeping the transport can
/// do alone. Park it beside the process (see `Terminal::spawn_holding`, whose
/// reader thread owns it for exactly as long as the pty is open) and the
/// accounting is exact; drop it early and the transport undercounts a channel
/// that is still there.
#[derive(Default)]
pub struct ChannelLease {
    /// Dropping this releases the slot. `dyn Send + Sync` so a lease can be
    /// parked in shared state and dropped from any thread.
    held: Option<Box<dyn Send + Sync>>,
}

impl ChannelLease {
    /// A lease over nothing — for a target with no per-connection limit to run
    /// into. Not an error and not a refusal: it is the honest answer for a
    /// local target, and for Windows, where every command is its own
    /// connection and there is no shared counter to fill.
    pub const fn none() -> Self {
        Self { held: None }
    }

    /// Wrap the issuer's own guard, whose `Drop` releases the slot.
    pub fn held_by(guard: Box<dyn Send + Sync>) -> Self {
        Self { held: Some(guard) }
    }

    /// Whether this lease is actually accounting anything.
    pub fn is_accounted(&self) -> bool {
        self.held.is_some()
    }
}

impl std::fmt::Debug for ChannelLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelLease").field("accounted", &self.is_accounted()).finish()
    }
}

/// What a [`CommandSpec::low_latency`] request actually got.
///
/// Phrased without naming a protocol, for the same reason the request is: the
/// caller asked for a property of the session, and what it needs back is
/// whether it got it and — when it did not — a sentence it can show. A
/// [`Target`] that declines must say so, because a session that quietly is not
/// what was asked for is the silently-wrong kind of wrong: everything works, so
/// nothing looks broken, and the thing requested is simply absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LowLatencyOutcome {
    /// The target answered the request.
    Granted,
    /// It could not, and this is what to tell the operator.
    #[serde(rename_all = "camelCase")]
    Declined { reason: String },
}

/// Result of a non-interactive command.
#[derive(Clone, Debug)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What a one-shot reads on stdin.
pub enum Stdin<'a> {
    Bytes(&'a [u8]),
    /// Streamed from disk, never loaded whole, counting into `sent`.
    File { path: &'a std::path::Path, sent: &'a std::sync::atomic::AtomicU64 },
}

/// Spawn a resolved command, feed it `input`, and collect its output.
///
/// The one implementation behind both targets' `exec_with_input*`, so a fix to
/// how stdin is fed cannot land in one and miss the other. Stdin is closed
/// before the output is awaited: the far side's `cat` reads to EOF, so holding
/// it open would deadlock.
pub async fn run_feeding(
    resolved: &ResolvedCommand,
    cwd: Option<&Utf8PathBuf>,
    input: Stdin<'_>,
) -> anyhow::Result<Output> {
    use tokio::io::AsyncWriteExt;

    let mut cmd = tokio::process::Command::new(&resolved.program);
    cmd.no_console_window();
    cmd.args(&resolved.args);
    for (k, v) in &resolved.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    match input {
        Stdin::Bytes(bytes) => stdin.write_all(bytes).await?,
        Stdin::File { path, sent } => {
            use tokio::io::AsyncReadExt;

            // A loop rather than `io::copy` only so `sent` moves as it goes.
            let mut file = tokio::fs::File::open(path).await?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                stdin.write_all(&buf[..n]).await?;
                sent.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    stdin.flush().await?;
    drop(stdin);

    let out = child.wait_with_output().await?;
    Ok(Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    /// Trimmed stdout, or an error carrying stderr — the common case for the
    /// one-shot probes used during connect.
    pub fn stdout_or_err(&self) -> anyhow::Result<&str> {
        if self.ok() {
            Ok(self.stdout.trim())
        } else {
            anyhow::bail!("command failed (status {}): {}", self.status, self.stderr.trim())
        }
    }
}

/// A place where work happens.
#[async_trait]
pub trait Target: Send + Sync + 'static {
    fn id(&self) -> &TargetId;

    /// Resolve `spec` into an argv spawnable on **this** machine.
    ///
    /// Never performs I/O, so terminal creation stays synchronous and cannot
    /// block the UI thread.
    fn build_command(&self, spec: &CommandSpec) -> anyhow::Result<ResolvedCommand>;

    /// Resolve `spec` **and reserve the channel it will ride**, returning both.
    ///
    /// The async counterpart of [`Target::build_command`], and it exists for
    /// one reason: a connection can be full. `build_command` is synchronous and
    /// documented never to perform I/O, so it can neither wait for a slot nor
    /// open a second connection — it can only hand back an argv and hope, which
    /// is exactly how 39 channels came to be queued onto a connection that
    /// permits 10.
    ///
    /// **The caller must hold the returned [`ChannelLease`] for as long as the
    /// spawned process lives.** Dropping it releases the slot, so a lease
    /// dropped at the end of the function that spawned a terminal undercounts
    /// that terminal for the rest of its life.
    ///
    /// A trait method for the same reason [`Target::disconnect`] is one:
    /// whether a target has a per-connection limit is a property of the target,
    /// so the branch belongs in the impl. The default resolves the command and
    /// leases nothing, which is correct for a target with no shared connection
    /// — there is no counter to fill and nothing to wait for.
    async fn open_channel(
        &self,
        spec: &CommandSpec,
    ) -> anyhow::Result<(ResolvedCommand, ChannelLease)> {
        Ok((self.build_command(spec)?, ChannelLease::none()))
    }

    /// Run a command to completion and capture its output.
    async fn exec(&self, spec: &CommandSpec) -> anyhow::Result<Output>;

    /// Run a command, feeding `input` to its stdin.
    ///
    /// Needed for writing files: piping the bytes to a remote `cat` keeps the
    /// content out of the command line, which has both a length limit and a
    /// quoting hazard.
    async fn exec_with_input(&self, spec: &CommandSpec, input: &[u8])
    -> anyhow::Result<Output>;

    /// Run a command, streaming the file at `input` to its stdin.
    ///
    /// For uploads of any size: the file is copied into the pipe a buffer at a
    /// time, so memory stays flat whether it is a screenshot or a 6 GB archive.
    /// [`Target::exec_with_input`] needs the whole payload in memory, which is
    /// exactly what capped uploads at 64 MB.
    ///
    /// The default reads the file whole and defers to `exec_with_input` — right
    /// for a test double, and wrong for a real target, which must override it
    /// (both of ours do, through [`run_feeding`]).
    ///
    /// `sent` counts the bytes handed to the command as they go, so a caller can
    /// report a long transfer's progress honestly rather than inventing one.
    async fn exec_with_input_file(
        &self,
        spec: &CommandSpec,
        input: &std::path::Path,
        sent: &std::sync::atomic::AtomicU64,
    ) -> anyhow::Result<Output> {
        let bytes = tokio::fs::read(input).await?;
        let out = self.exec_with_input(spec, &bytes).await?;
        sent.store(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(out)
    }

    /// The target's OS, if known yet.
    fn platform(&self) -> Option<Platform>;

    /// Re-assert any persistent transport this target keeps warm (for SSH, the
    /// shared control master).
    ///
    /// Cheap and idempotent; the default is a no-op, correct for a target that
    /// holds nothing open. Polled callers invoke this each tick so a master
    /// that died is revived here rather than every poll paying a fresh
    /// handshake. Best-effort: producing a result must not depend on it.
    async fn ensure_ready(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Tear down any persistent transport this target keeps open.
    ///
    /// The counterpart of [`Target::ensure_ready`], and the reason it exists as
    /// a trait method rather than a downcast: whether a target *has* a
    /// connection to close is a property of the target, so the branch belongs in
    /// the impl. A local target holds nothing, hence the default.
    ///
    /// **Not merely dropping the last `Arc`.** The SSH master does die when its
    /// owner is dropped, but relying on that means a disconnect silently fails
    /// whenever a single clone survives anywhere — in a cache nobody remembered,
    /// in an in-flight task — and a connection that is still up looks exactly
    /// like a feature that does not work. Asking the master to exit says so
    /// regardless of who else is holding it.
    ///
    /// Best-effort and idempotent: disconnecting something already disconnected
    /// is not an error, and a failure here must not stop the caller forgetting
    /// the target.
    async fn disconnect(&self) {}

    /// What the last [`CommandSpec::low_latency`] request on this target
    /// resolved to, or `None` when none was made.
    ///
    /// A trait method for the same reason [`Target::disconnect`] is one:
    /// whether a target can answer that request at all is a property of the
    /// target, so the branch belongs in the impl — and callers hold
    /// `Arc<dyn Target>`, so a fallback they cannot read is a fallback nobody
    /// reports. Never performs I/O and never re-probes anything: this is a
    /// reading of what already happened.
    ///
    /// The default is `None`, which is correct for a target with no round trip
    /// to remove — there was nothing to grant and nothing to decline.
    fn low_latency_outcome(&self) -> Option<LowLatencyOutcome> {
        None
    }

    /// Settle — *before a command is built* — whether this target can answer a
    /// [`CommandSpec::low_latency`] request, and report what it found.
    ///
    /// This is the async half that [`Target::low_latency_outcome`] deliberately
    /// is not. `build_command` is synchronous and documented never to perform
    /// I/O, so a target that has to *discover* whether it can grant the request
    /// can only read what it already knows. **A caller that intends to set
    /// `low_latency` must await this first**, or the discovery has not happened
    /// yet when the command is built and the request falls back silently and
    /// for ever.
    ///
    /// It is also what the UI asks, so that a control the operator cannot use
    /// can be *absent* rather than present-and-dead. It must therefore be safe
    /// to call repeatedly: an implementation caches its answer and a second
    /// call costs nothing.
    ///
    /// A trait method for the same reason [`Target::disconnect`] is one — the
    /// branch belongs in the impl, and callers hold `Arc<dyn Target>`.
    async fn probe_low_latency(&self) -> LowLatencyOutcome {
        LowLatencyOutcome::Declined {
            reason: "this connection has no low-latency transport, so this session uses its \
                     ordinary one"
                .to_owned(),
        }
    }
}

/// Quote a string for a POSIX shell using single quotes.
///
/// Needed because an SSH command line is re-parsed by the remote login shell:
/// everything after the host is joined and handed to `sh -c`, so a path with a
/// space or a `$` is not merely cosmetic breakage, it is an injection.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'+' | b','))
    {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            // Close the quote, emit an escaped quote, reopen. There is no way to
            // escape a single quote inside single quotes.
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Quote a **path** for a POSIX shell, preserving a leading `~`.
///
/// [`shell_quote`] is correct for arbitrary strings but wrong for paths the user
/// typed, because it quotes the tilde — and a quoted `~` is a literal directory
/// named "~", not the home directory. The remote shell then reports
/// `cd: ~/project: No such file or directory` for a path that plainly exists,
/// which is exactly how this was found: on a real host, against a real project.
///
/// The tilde is emitted as `"$HOME"` so the far side expands it, with the
/// remainder quoted as usual. `~user` is deliberately **not** expanded — there is
/// no safe way to do it without interpolating a username into the command line,
/// and it is vanishingly rare next to `~/`.
pub fn shell_quote_path(path: &str) -> String {
    if path == "~" {
        return "\"$HOME\"".to_owned();
    }
    match path.strip_prefix("~/") {
        // `"$HOME"` then the quoted remainder; the shell concatenates adjacent
        // words, so `"$HOME"'/my dir'` is one argument.
        Some(rest) => format!("\"$HOME\"{}", shell_quote(&format!("/{rest}"))),
        None => shell_quote(path),
    }
}

/// Render a [`CommandSpec`] as a single POSIX shell command string.
///
/// Used to build the remote half of an `ssh` invocation. `$SHELL` is passed
/// through unquoted on purpose so the remote shell expands it.
pub fn spec_to_shell_line(spec: &CommandSpec) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(cwd) = &spec.cwd {
        // A path, not an arbitrary string — a leading `~` must still expand.
        parts.push(format!("cd {} &&", shell_quote_path(cwd.as_str())));
    }
    if !spec.env.is_empty() {
        parts.push("env".to_owned());
        for (k, v) in &spec.env {
            parts.push(format!("{k}={}", shell_quote(v)));
        }
    }

    // `$SHELL` must reach the remote shell unquoted to be expanded there; any
    // other program name is quoted normally.
    if spec.program == "$SHELL" {
        parts.push("\"$SHELL\"".to_owned());
    } else {
        parts.push(shell_quote(&spec.program));
    }
    parts.extend(spec.args.iter().map(|a| shell_quote(a)));

    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_are_not_quoted() {
        assert_eq!(shell_quote("hello"), "hello");
        assert_eq!(shell_quote("/usr/bin/env"), "/usr/bin/env");
        assert_eq!(shell_quote("user@host"), "user@host");
    }

    #[test]
    fn spaces_and_metacharacters_are_quoted() {
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("$HOME"), "'$HOME'");
        assert_eq!(shell_quote("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn single_quotes_are_escaped_by_closing_and_reopening() {
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn a_leading_tilde_still_expands_on_the_far_side() {
        // Quoting the tilde makes the remote shell look for a directory literally
        // named "~", and report a path that exists as missing.
        assert_eq!(shell_quote_path("~/project"), r#""$HOME"/project"#);
        assert_eq!(shell_quote_path("~"), r#""$HOME""#);
    }

    #[test]
    fn a_tilde_path_with_spaces_is_still_quoted() {
        // The home part expands; everything after it stays one argument.
        assert_eq!(shell_quote_path("~/my project"), r#""$HOME"'/my project'"#);
    }

    #[test]
    fn a_tilde_path_cannot_smuggle_a_command() {
        let quoted = shell_quote_path("~/'; touch /tmp/pwned; '");
        // Only the expansion we added is unquoted; the payload stays inert.
        assert!(quoted.starts_with(r#""$HOME"'/"#), "got: {quoted}");
        assert!(quoted.contains(r"'\''"), "the quote was not escaped: {quoted}");
    }

    #[test]
    fn a_tilde_that_is_not_leading_is_never_expanded() {
        // Only a leading `~/` means home. Elsewhere it is an ordinary character,
        // and quoting it (which shell_quote does) is exactly what keeps it literal.
        for path in ["/tmp/~backup", "./~/x", "/var/~"] {
            let quoted = shell_quote_path(path);
            assert!(!quoted.contains("$HOME"), "{path} should not expand: {quoted}");
            assert!(quoted.contains('~'), "the tilde should survive: {quoted}");
        }
    }

    #[test]
    fn absolute_paths_are_unaffected() {
        assert_eq!(shell_quote_path("/etc/hosts"), "/etc/hosts");
        assert_eq!(shell_quote_path("/my dir/x"), "'/my dir/x'");
    }

    #[test]
    fn shell_line_includes_cwd_and_env() {
        let spec = CommandSpec::new("ls").arg("-la").cwd("/tmp/my dir").env("FOO", "bar baz");
        assert_eq!(spec_to_shell_line(&spec), "cd '/tmp/my dir' && env FOO='bar baz' ls -la");
    }

    #[test]
    fn login_shell_stays_expandable_on_the_far_side() {
        // Quoting this would run a program literally named "$SHELL".
        assert_eq!(spec_to_shell_line(&CommandSpec::login_shell()), r#""$SHELL" -l -i"#);
    }

    #[test]
    fn the_login_shell_is_interactive_so_rc_files_are_read() {
        // `-i` is what makes `.bashrc` / `.zshrc` load, and those are where nvm,
        // fnm, bun and volta put their PATH — so without it `claude` is "command
        // not found" on a host where it is installed and works when typed. A
        // zsh host is where this shows up, because `zsh -l` reads neither
        // `.zshrc` nor anything else the version managers touch.
        let line = spec_to_shell_line(&CommandSpec::login_shell());
        assert!(line.contains(" -i"), "the login shell must be interactive: {line}");
        assert!(line.contains(" -l"), "and still a login shell: {line}");
    }
}
