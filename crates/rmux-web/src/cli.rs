//! Command line, listen-address policy and the token.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

/// Default address: loopback, a fixed port so an SSH tunnel can name it.
pub const DEFAULT_LISTEN: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 47110);

pub const USAGE: &str = "\
rmux-web — rmux's UI and commands, served over HTTP

USAGE:
    rmux-web [OPTIONS]

OPTIONS:
    --ui <DIR>               The built UI (vite's dist/). Without it only /api is served.
    --resources <DIR>        Bundled agent binaries, as the desktop app ships them.
    --listen <IP:PORT>       Address to bind [default: 127.0.0.1:47110].
    --allow-non-loopback     Permit --listen on an address other than loopback.
    --token-file <FILE>      Read the access token from FILE (must not be group- or
                             world-readable). Without it a random token is made per run.
    -h, --help               Print this help.

The access URL, token included, is printed to stderr once at startup.
There is deliberately no --token: a secret in argv is visible to every account
on the host through `ps`.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub listen: SocketAddr,
    pub allow_non_loopback: bool,
    pub token_file: Option<PathBuf>,
    pub ui: Option<PathBuf>,
    pub resources: Option<PathBuf>,
    pub help: bool,
}

/// Parse arguments (without the program name). `--flag value` and
/// `--flag=value` are both accepted.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut cli = Cli {
        listen: DEFAULT_LISTEN,
        allow_non_loopback: false,
        token_file: None,
        ui: None,
        resources: None,
        help: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline.clone().or_else(|| args.next()).ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => cli.help = true,
            "--allow-non-loopback" => cli.allow_non_loopback = true,
            "--listen" => {
                let v = value("--listen")?;
                cli.listen = v.parse().map_err(|_| format!("--listen: `{v}` is not an IP:PORT address"))?;
            }
            "--token-file" => cli.token_file = Some(value("--token-file")?.into()),
            "--ui" => cli.ui = Some(value("--ui")?.into()),
            "--resources" => cli.resources = Some(value("--resources")?.into()),
            "--token" => {
                return Err("--token is not accepted: a secret in argv is visible to every account on the \
                            host through `ps`. Put it in a 0600 file and pass --token-file."
                    .into());
            }
            other => return Err(format!("unknown argument `{other}` (see --help)")),
        }
    }
    Ok(cli)
}

/// Refuse to serve a shell on a non-loopback address unless asked explicitly.
///
/// Bound to loopback, the only ways in are the host itself, an SSH tunnel and
/// the cloudflared tunnel — all of which already required being someone. Bound
/// to `0.0.0.0`, the LAN is one URL away from a terminal.
pub fn check_listen(addr: SocketAddr, allow_non_loopback: bool) -> Result<(), String> {
    if addr.ip().is_loopback() || allow_non_loopback {
        Ok(())
    } else {
        Err(format!(
            "refusing to listen on {addr}: it is not a loopback address, and rmux-web serves a shell. \
             Reach it through an SSH or cloudflared tunnel, or pass --allow-non-loopback if you mean it."
        ))
    }
}

/// A fresh per-run token: 32 bytes from the OS RNG, hex.
pub fn generate_token() -> anyhow::Result<String> {
    use rand::TryRngCore as _;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes).map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Read a token from a file. Surrounding whitespace is ignored; the rest must
/// be at least 16 visible ASCII characters, so it survives a URL fragment and
/// an `Authorization` header unchanged.
///
/// A file other accounts can read is **refused**, not used: a token anyone on
/// the host can `cat` is not a credential.
pub fn read_token_file(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path)
            .map_err(|e| anyhow::anyhow!("--token-file {}: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "--token-file {} is readable by other accounts (mode {:o}); chmod 600 it",
                path.display(),
                mode & 0o777
            );
        }
    }
    let raw = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("--token-file {}: {e}", path.display()))?;
    let token = raw.trim();
    if token.len() < 16 || !token.bytes().all(|b| b.is_ascii_graphic()) {
        anyhow::bail!("--token-file {}: the token must be at least 16 visible ASCII characters", path.display());
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_to_loopback() {
        let cli = parse(args(&[])).unwrap();
        assert_eq!(cli.listen, DEFAULT_LISTEN);
        assert!(cli.listen.ip().is_loopback());
        assert!(!cli.allow_non_loopback);
    }

    #[test]
    fn both_value_forms_parse() {
        let cli = parse(args(&["--listen=127.0.0.1:9", "--ui", "dist", "--resources=r"])).unwrap();
        assert_eq!(cli.listen, "127.0.0.1:9".parse().unwrap());
        assert_eq!(cli.ui, Some("dist".into()));
        assert_eq!(cli.resources, Some("r".into()));
    }

    #[test]
    fn a_token_in_argv_is_refused() {
        assert!(parse(args(&["--token", "abc"])).unwrap_err().contains("ps"));
        assert!(parse(args(&["--token=abc"])).unwrap_err().contains("ps"));
    }

    #[test]
    fn non_loopback_needs_the_flag() {
        for addr in ["0.0.0.0:1", "[::]:1", "192.168.1.5:1"] {
            let addr: SocketAddr = addr.parse().unwrap();
            assert!(check_listen(addr, false).is_err(), "{addr}");
            assert!(check_listen(addr, true).is_ok(), "{addr}");
        }
        assert!(check_listen("127.0.0.1:1".parse().unwrap(), false).is_ok());
        assert!(check_listen("[::1]:1".parse().unwrap(), false).is_ok());
    }

    #[test]
    fn generated_tokens_are_long_and_distinct() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[cfg(unix)]
    #[test]
    fn a_readable_token_file_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "0123456789abcdef0123\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token_file(&path).unwrap_err().to_string().contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_token_file(&path).unwrap(), "0123456789abcdef0123");
        std::fs::write(&path, "short").unwrap();
        assert!(read_token_file(&path).is_err());
    }
}
