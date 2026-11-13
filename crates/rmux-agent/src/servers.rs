//! The operator's servers, as the `rmux` CLI sees them, and how to reach one.
//!
//! The desktop app owns the server list (it lives in its webview's storage), so
//! it mirrors it to `~/.rmux/servers.json` whenever it changes — the
//! `servers_sync` command — and the CLI reads that. No secrets cross: an entry
//! is an ssh destination exactly as the app stores it.
//!
//! ## Reaching a server
//!
//! **The host is handed to `ssh` verbatim** (CLAUDE.md: never resolve
//! `~/.ssh/config` ourselves). `ProxyJump`, `Match`, certificates and 2FA all
//! come free because `ssh` does them. What is added:
//!
//! - rmux's own key for that host, with `-i`, only when it exists — the same
//!   file the app generates and offers (`rmux-ssh::keys::key_path`, pinned by a
//!   test so the two cannot drift). Additive, never `IdentitiesOnly`.
//! - a ControlMaster of the CLI's own under `~/.rmux/cm/%C`, so a second
//!   `rmux ls` within a minute costs no handshake. `%C` is a fixed-length hash,
//!   which keeps the socket path well inside `sun_path`.
//!
//! The remote command is one POSIX line run through `sh -c`, with every
//! argument passed through `shell_quote`. It finds the agent on the far side
//! itself — `~/.rmux/bin/rmux` (the shim) or the newest `rmux-agent-*` — using a
//! `"$HOME"` the *remote* shell expands. `$HOME` is never inside a quoted path:
//! quoting is exactly what stops it expanding.

use std::path::{Path, PathBuf};

use rmux_transport::shell_quote;
use serde::{Deserialize, Serialize};

/// One server the operator added in the app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    /// The app's server id. Stable; matched by `-t`.
    pub id: String,
    /// What `rmux ls` prints before the session name.
    pub label: String,
    /// The ssh destination — an alias or a hostname, passed to `ssh` verbatim.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Absolute local path of the private key the operator chose for this
    /// server in the app. **The path only** — never the key's contents — so the
    /// file stays free of secrets; the key itself stays where it was, under the
    /// operator's own permissions. Offered as one more `-i`, additively, and
    /// only when the file still exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServersFile {
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
}

pub fn path(home: &Path) -> PathBuf {
    home.join(".rmux").join("servers.json")
}

/// The file under `home`. Missing or unreadable is an empty list — the CLI
/// still works on this machine with no app ever run.
pub fn load(home: &Path) -> ServersFile {
    std::fs::read_to_string(path(home))
        .ok()
        .and_then(|t| parse(&t).ok())
        .unwrap_or(ServersFile { version: 1, servers: Vec::new() })
}

pub fn parse(text: &str) -> anyhow::Result<ServersFile> {
    let file: ServersFile = serde_json::from_str(text)?;
    anyhow::ensure!(file.version == 1, "servers.json version {} is newer than this rmux", file.version);
    Ok(file)
}

/// Write the file: `0600`, created with that mode *before* anything is
/// written, then renamed over the old one so a reader never sees half a list.
pub fn write(home: &Path, servers: &[ServerEntry]) -> anyhow::Result<()> {
    let target = path(home);
    let dir = target.parent().ok_or_else(|| anyhow::anyhow!("no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string_pretty(&ServersFile { version: 1, servers: servers.to_vec() })?;
    let temp = dir.join(format!("servers.json.{}.tmp", std::process::id()));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = options.open(&temp)?;
        // A temp file left by a crashed writer kept whatever mode it had.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, &target)?;
    Ok(())
}

impl ServerEntry {
    /// A destination typed on the command line rather than found in the file:
    /// `host`, `user@host`, `host:port`, `user@host:port`. The host part stays
    /// verbatim — it may well be an ssh alias.
    pub fn from_destination(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.contains(char::is_whitespace) {
            return None;
        }
        let (user, rest) = match raw.split_once('@') {
            Some((u, r)) if !u.is_empty() => (Some(u.to_owned()), r),
            Some(_) => return None,
            None => (None, raw),
        };
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && !h.contains(':') => (h, Some(p.parse().ok()?)),
            Some(_) => return None,
            None => (rest, None),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self { id: raw.to_owned(), label: raw.to_owned(), host: host.to_owned(), user, port, identity: None })
    }

    /// `user@host` or `host` — the name rmux's key for this host is filed under
    /// (`rmux_transport::SshHost::label`).
    pub fn key_label(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// Does `query` name this server? Its id, label, host, `user@host`, or any
    /// of those with `:port`.
    pub fn matches(&self, query: &str) -> bool {
        let base = self.key_label();
        let mut names = vec![self.id.clone(), self.label.clone(), self.host.clone(), base.clone()];
        if let Some(port) = self.port {
            names.push(format!("{}:{port}", self.host));
            names.push(format!("{base}:{port}"));
        }
        names.iter().any(|n| n == query)
    }
}

impl ServersFile {
    pub fn find(&self, query: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|s| s.matches(query))
    }
}

/// rmux's own key for a host — **the same name `rmux-ssh::keys::key_path`
/// gives**, so a key the app installed is the key the CLI offers. Duplicated
/// rather than depended on because the agent is a static binary uploaded to
/// every host and `rmux-ssh` is not small; a test compares the two.
pub fn key_path(home: &Path, label: &str) -> PathBuf {
    let safe: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    home.join(".ssh").join(format!("rmux_{safe}_ed25519"))
}

/// The directory the CLI's ControlMaster sockets live in.
pub fn control_dir(home: &Path) -> PathBuf {
    home.join(".rmux").join("cm")
}

/// How an ssh invocation should behave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SshMode {
    /// `-tt` (an interactive attach) or `-T`.
    pub tty: bool,
    /// `BatchMode=yes`: never prompt. For the `ls` fan-out, where a password
    /// prompt from one of six parallel connections would be unanswerable.
    pub batch: bool,
}

/// The full `ssh` argv (without the program) that runs `remote_line` on
/// `server` through `sh -c`.
///
/// `key_exists` is injected so the command line is testable without a key on
/// disk; callers pass `Path::exists`.
pub fn ssh_args(
    server: &ServerEntry,
    home: &Path,
    mode: SshMode,
    remote_line: &str,
    key_exists: impl Fn(&Path) -> bool,
) -> Vec<String> {
    let mut args = vec![
        "-o".to_owned(),
        "ControlMaster=auto".to_owned(),
        "-o".to_owned(),
        "ControlPersist=60".to_owned(),
        "-o".to_owned(),
        format!("ControlPath={}/%C", control_dir(home).display()),
        "-o".to_owned(),
        "ConnectTimeout=6".to_owned(),
    ];
    if mode.batch {
        args.push("-o".to_owned());
        args.push("BatchMode=yes".to_owned());
    }
    // The key the operator chose first, then rmux's own — the same order and
    // the same rules as the app (`rmux_ssh::keys::identity_args_with`): each
    // only if present, de-duplicated, never exclusive.
    let chosen = server
        .identity
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && key_exists(p));
    if let Some(chosen) = &chosen {
        args.push("-i".to_owned());
        args.push(chosen.to_string_lossy().into_owned());
    }
    let key = key_path(home, &server.key_label());
    if key_exists(&key) && chosen.as_deref() != Some(key.as_path()) {
        args.push("-i".to_owned());
        args.push(key.to_string_lossy().into_owned());
    }
    if let Some(user) = &server.user {
        args.push("-l".to_owned());
        args.push(user.clone());
    }
    if let Some(port) = server.port {
        args.push("-p".to_owned());
        args.push(port.to_string());
    }
    args.push(if mode.tty { "-tt" } else { "-T" }.to_owned());
    args.push(server.host.clone());
    args.push("--".to_owned());
    // ssh joins its trailing words with spaces and the remote login shell
    // re-parses them, so the line travels as one quoted word for `sh -c`.
    args.push(format!("sh -c {}", shell_quote(remote_line)));
    args
}

/// The preamble that finds an agent on the far side and leaves it in `$a`.
///
/// The shim first (it prefers `$RMUX_BIN`, unset over ssh, then the newest
/// build), then the newest `rmux-agent-*` directly, skipping half-uploaded
/// `*.partial*` files exactly as the shim does.
const FIND_AGENT: &str = r#"d="$HOME/.rmux/bin"; a=
if [ -x "$d/rmux" ]; then a="$d/rmux"; else
  for f in $(ls -t "$d" 2>/dev/null); do
    case "$f" in *.partial*|*.tmp) ;; rmux-agent-*) if [ -x "$d/$f" ]; then a="$d/$f"; break; fi ;; esac
  done
fi
if [ -z "$a" ]; then echo "rmux: no rmux agent on this host yet; open it once in the rmux app" >&2; exit 127; fi
"#;

/// What an older agent says to a new-CLI-only command, and the exit it uses.
const TOO_OLD: &str = r#"echo "rmux: the agent on this host predates the rmux command line; open the server once in the rmux app to upgrade it" >&2; exit 3"#;

fn quoted(args: &[&str]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

/// Run `rmux-agent <args>` on the far side — any build.
pub fn remote_line(args: &[&str]) -> String {
    format!("{FIND_AGENT}exec \"$a\" {}", quoted(args))
}

/// The same, for a command only a build with the CLI has (`-V` is how one is
/// told apart: an older build exits non-zero on it).
pub fn remote_line_new_cli(args: &[&str]) -> String {
    format!("{FIND_AGENT}\"$a\" -V >/dev/null 2>&1 || {{ {TOO_OLD}; }}\nexec \"$a\" {}", quoted(args))
}

/// Attach to an existing session on the far side, with the detach key when the
/// build there understands it. An older build attaches without one; the session
/// still survives the connection, so ssh's own `~.` detaches it, and the line
/// says so.
pub fn remote_attach_line(session: &str, detach_key: Option<&str>) -> String {
    let plain = quoted(&["attach", "--session", session]);
    match detach_key {
        None => format!("{FIND_AGENT}exec \"$a\" {plain}"),
        Some(key) => format!(
            "{FIND_AGENT}if \"$a\" -V >/dev/null 2>&1; then exec \"$a\" {}; fi\n\
             echo \"rmux: this host's agent predates the detach key; press Enter then ~. to detach (the session keeps running)\" >&2\n\
             exec \"$a\" {plain}",
            quoted(&["attach", "--session", session, "--detach-key", key]),
        ),
    }
}

/// cmd.exe's answer to `sh -c …`: the signal that the host is Windows, where the
/// agent does not run and nothing here can work.
pub fn looks_like_windows(output: &str) -> bool {
    output.contains("is not recognized as an internal or external command")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(host: &str, user: Option<&str>, port: Option<u16>) -> ServerEntry {
        ServerEntry {
            id: format!("id-{host}"),
            label: host.to_owned(),
            host: host.to_owned(),
            user: user.map(str::to_owned),
            port,
            identity: None,
        }
    }

    #[test]
    fn the_file_parses_and_round_trips_at_0600() {
        let home = std::env::temp_dir().join(format!("rmux-servers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let servers = vec![entry("devbox", None, None), entry("10.0.0.107", Some("dev"), Some(2222))];
        write(&home, &servers).unwrap();

        let text = std::fs::read_to_string(path(&home)).unwrap();
        assert!(!text.contains("password") && !text.contains("token"));
        let back = parse(&text).unwrap();
        assert_eq!(back.servers, servers);
        assert_eq!(load(&home).servers, servers);
        // Optional fields are absent, not null, for a plain alias.
        assert!(!text.contains("null"), "{text}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(&home)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(home.join(".rmux"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        std::fs::remove_dir_all(&home).ok();

        // The documented shape, from the app.
        let file = parse(r#"{"version":1,"servers":[{"id":"s1","label":"dev","host":"dev","user":"me","port":22}]}"#)
            .unwrap();
        assert_eq!(file.servers[0].port, Some(22));
        assert!(parse(r#"{"version":2,"servers":[]}"#).is_err());
    }

    #[test]
    fn a_server_is_found_by_any_name_the_operator_would_type() {
        let file = ServersFile {
            version: 1,
            servers: vec![entry("10.0.0.107", Some("dev"), Some(2222))],
        };
        for q in ["id-10.0.0.107", "10.0.0.107", "dev@10.0.0.107", "dev@10.0.0.107:2222"] {
            assert!(file.find(q).is_some(), "{q}");
        }
        assert!(file.find("10.0.0.10").is_none());
    }

    #[test]
    fn a_typed_destination_keeps_the_host_verbatim() {
        let s = ServerEntry::from_destination("me@devbox:2200").unwrap();
        assert_eq!((s.user.as_deref(), s.host.as_str(), s.port), (Some("me"), "devbox", Some(2200)));
        let s = ServerEntry::from_destination("devbox").unwrap();
        assert_eq!((s.user, s.host.as_str(), s.port), (None, "devbox", None));
        assert!(ServerEntry::from_destination("host:notaport").is_none());
        assert!(ServerEntry::from_destination("@host").is_none());
        assert!(ServerEntry::from_destination("a b").is_none());
    }

    #[test]
    fn the_key_name_matches_the_app_s() {
        let home = Path::new("/home/x");
        for label in ["dev@10.0.0.107", "devbox", "a/b c"] {
            assert_eq!(key_path(home, label), rmux_ssh::keys::key_path(home, label), "{label}");
        }
    }

    #[test]
    fn ssh_args_pass_the_host_verbatim_and_offer_the_key_only_when_present() {
        let home = Path::new("/Users/me");
        let s = entry("10.0.0.107", Some("dev"), Some(2222));
        let args = ssh_args(&s, home, SshMode { tty: false, batch: true }, "true", |_| true);
        let key = "/Users/me/.ssh/rmux_dev_10.0.0.107_ed25519";
        assert!(args.windows(2).any(|w| w == ["-i", key]), "{args:?}");
        assert!(args.windows(2).any(|w| w == ["-l", "dev"]));
        assert!(args.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(args.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
        assert!(args.contains(&"ControlPath=/Users/me/.rmux/cm/%C".to_owned()));
        // Never exclusive: the operator's own keys and config still apply.
        assert!(!args.iter().any(|a| a.contains("IdentitiesOnly")));
        let host_at = args.iter().position(|a| a == "10.0.0.107").unwrap();
        assert_eq!(args[host_at - 1], "-T");
        assert_eq!(args[host_at + 1], "--");
        assert_eq!(args[host_at + 2], "sh -c true");

        let args = ssh_args(&entry("devbox", None, None), home, SshMode { tty: true, batch: false }, "x", |_| false);
        assert!(!args.iter().any(|a| a == "-i" || a == "-l" || a == "-p" || a == "BatchMode=yes"));
        assert!(args.contains(&"-tt".to_owned()));
    }

    /// The CLI offers the key chosen in the app, beside rmux's own, only while
    /// the file exists — and servers.json carries its *path*, nothing more.
    #[test]
    fn a_chosen_identity_is_offered_and_stored_as_a_path() {
        let home = Path::new("/Users/me");
        let mut s = entry("10.0.0.107", Some("dev"), Some(22));
        s.identity = Some("/Users/me/.ssh/work_ed25519".to_owned());
        let own = "/Users/me/.ssh/rmux_dev_10.0.0.107_ed25519";

        let args = ssh_args(&s, home, SshMode { tty: false, batch: true }, "true", |_| true);
        let ids: Vec<&str> =
            args.windows(2).filter(|w| w[0] == "-i").map(|w| w[1].as_str()).collect();
        assert_eq!(ids, ["/Users/me/.ssh/work_ed25519", own], "chosen first, rmux key kept: {args:?}");
        assert!(!args.iter().any(|a| a.contains("IdentitiesOnly")));

        // Gone from disk: not named, and rmux's key still offered.
        let args = ssh_args(&s, home, SshMode { tty: false, batch: true }, "true", |p| p == Path::new(own));
        let ids: Vec<&str> =
            args.windows(2).filter(|w| w[0] == "-i").map(|w| w[1].as_str()).collect();
        assert_eq!(ids, [own]);

        // Choosing rmux's own file is one offer, not two.
        s.identity = Some(own.to_owned());
        let args = ssh_args(&s, home, SshMode { tty: false, batch: true }, "true", |_| true);
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 1);

        // On disk: just the path.
        let text = serde_json::to_string(&ServersFile { version: 1, servers: vec![s.clone()] }).unwrap();
        assert!(text.contains(&format!("\"identity\":\"{own}\"")), "{text}");
        assert_eq!(parse(&text).unwrap().servers[0].identity.as_deref(), Some(own));
        // And a file written before the field existed still parses.
        let old = parse(r#"{"version":1,"servers":[{"id":"a","label":"a","host":"a"}]}"#).unwrap();
        assert_eq!(old.servers[0].identity, None);
    }

    #[test]
    fn remote_lines_quote_every_argument_and_keep_home_unquoted() {
        let hostile = "x'; rm -rf ~; echo '";
        let line = remote_line(&["kill", hostile]);
        assert!(line.ends_with(&format!("exec \"$a\" kill {}", shell_quote(hostile))), "{line}");
        // `$HOME` is only ever expanded by the far shell in double quotes —
        // never frozen inside a single-quoted path.
        assert!(line.contains("d=\"$HOME/.rmux/bin\""));
        for word in [hostile, "kill"] {
            assert!(!shell_quote(word).contains("$HOME"));
        }
        assert!(!line.contains("'$HOME"));

        let attach = remote_attach_line("term-1", Some("C-b d"));
        assert!(attach.contains("attach --session term-1 --detach-key 'C-b d'"), "{attach}");
        assert!(attach.contains("-V"), "an old agent must be told apart before --detach-key reaches it");
        assert!(remote_attach_line("term-1", None).ends_with("exec \"$a\" attach --session term-1"));

        let new = remote_line_new_cli(&["new", "-d", "-s", "demo"]);
        assert!(new.contains("predates the rmux command line"));

        // The whole line survives being wrapped for `sh -c` by the ssh argv.
        let args = ssh_args(&entry("h", None, None), Path::new("/h"), SshMode { tty: false, batch: false }, &line, |_| false);
        assert_eq!(args.last().unwrap(), &format!("sh -c {}", shell_quote(&line)));
    }

    #[test]
    fn cmd_exe_is_recognised() {
        assert!(looks_like_windows("'sh' is not recognized as an internal or external command,\r\noperable program or batch file."));
        assert!(!looks_like_windows("sh: 1: rmux: not found"));
    }
}
