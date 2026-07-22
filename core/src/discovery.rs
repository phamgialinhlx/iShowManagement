//! Server discovery from `~/.ssh/config`. We enumerate concrete `Host` aliases
//! (skipping wildcard/negated patterns and `Host *`), following `Include`
//! directives, then resolve each with `ssh -G <alias>` so ssh's own parser
//! handles `Match`, tokens, and nested includes. Mirrors the Model-A design in
//! `plans/rust-port.md` (ADR 007 + 010).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct ResolvedHost {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    #[serde(rename = "proxyJump")]
    pub proxy_jump: Option<String>,
}

/// The ssh config file we enumerate and resolve against. `ISM_SSH_CONFIG`
/// overrides (used by tests); otherwise `~/.ssh/config`.
///
/// Important: OpenSSH locates the default `~/.ssh/config` via the password-db
/// home (getpwuid), NOT `$HOME`. We therefore pass this path to `ssh -G` with
/// `-F` so enumeration and resolution always read the *same* file.
pub fn config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ISM_SSH_CONFIG") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".ssh").join("config"))
}

/// Concrete Host aliases from the ssh config tree, in first-seen order.
pub fn list_aliases() -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    if let Some(cfg) = config_path() {
        collect_hosts(&cfg, &mut ordered, &mut seen, 0);
    }
    ordered
}

fn collect_hosts(path: &Path, out: &mut Vec<String>, seen: &mut BTreeSet<String>, depth: u8) {
    if depth > 8 {
        return; // guard against include cycles
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `Key value` or `Key=value`; keyword match is case-insensitive.
        let (keyword, rest) = split_kv(line);
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                for tok in rest.split_whitespace() {
                    if is_concrete_alias(tok) && seen.insert(tok.to_string()) {
                        out.push(tok.to_string());
                    }
                }
            }
            "include" => {
                for pat in rest.split_whitespace() {
                    for inc in expand_include(pat, path) {
                        collect_hosts(&inc, out, seen, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

fn split_kv(line: &str) -> (&str, &str) {
    match line.find(|c: char| c.is_whitespace() || c == '=') {
        Some(i) => (line[..i].trim(), line[i + 1..].trim_start_matches(['=', ' ', '\t'])),
        None => (line, ""),
    }
}

/// A usable alias: no wildcard/negation, not the catch-all `*`.
fn is_concrete_alias(tok: &str) -> bool {
    !tok.is_empty() && !tok.contains(['*', '?', '!'])
}

/// Resolve an `Include` pattern (supporting `~`, absolute, and relative-to the
/// including file) into concrete file paths via glob.
fn expand_include(pattern: &str, including: &Path) -> Vec<PathBuf> {
    let expanded: PathBuf = if let Some(rest) = pattern.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(h) => h.join(rest),
            None => return Vec::new(),
        }
    } else if pattern.starts_with('/') {
        PathBuf::from(pattern)
    } else {
        // Relative includes resolve against the including file's directory
        // (ssh treats them as relative to ~/.ssh, which is that directory).
        including
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(pattern)
    };
    match glob::glob(&expanded.to_string_lossy()) {
        Ok(paths) => paths.filter_map(Result::ok).collect(),
        Err(_) => Vec::new(),
    }
}

/// Resolve one alias's effective config via `ssh -G`.
pub async fn resolve(alias: &str) -> ResolvedHost {
    let mut cmd = tokio::process::Command::new("ssh");
    // Force the same config file we enumerated (see `config_path` docs).
    if let Some(cfg) = config_path().filter(|p| p.exists()) {
        cmd.arg("-F").arg(cfg);
    }
    let out = cmd.arg("-G").arg(alias).output().await;

    let mut host = ResolvedHost {
        alias: alias.to_string(),
        hostname: alias.to_string(),
        user: String::new(),
        port: 22,
        proxy_jump: None,
    };
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            let (k, v) = match line.split_once(' ') {
                Some(kv) => kv,
                None => continue,
            };
            match k.to_ascii_lowercase().as_str() {
                "hostname" => host.hostname = v.to_string(),
                "user" => host.user = v.to_string(),
                "port" => host.port = v.trim().parse().unwrap_or(22),
                "proxyjump" if v != "none" => host.proxy_jump = Some(v.to_string()),
                _ => {}
            }
        }
    }
    host
}

/// Enumerate + resolve every alias concurrently.
pub async fn discover() -> Vec<ResolvedHost> {
    let aliases = list_aliases();
    let futures = aliases.iter().map(|a| resolve(a));
    futures::future::join_all(futures).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concrete_alias_filtering() {
        assert!(is_concrete_alias("web"));
        assert!(is_concrete_alias("db-1.internal"));
        assert!(!is_concrete_alias("*"));
        assert!(!is_concrete_alias("*.example.com"));
        assert!(!is_concrete_alias("prod-?"));
        assert!(!is_concrete_alias("!bastion"));
    }

    #[test]
    fn split_kv_handles_space_and_equals() {
        assert_eq!(split_kv("Host web db"), ("Host", "web db"));
        assert_eq!(split_kv("Port=2222"), ("Port", "2222"));
        assert_eq!(split_kv("Include   ~/.ssh/conf.d/*"), ("Include", "~/.ssh/conf.d/*"));
    }

    #[test]
    fn collects_hosts_skipping_wildcards_and_following_includes() {
        let dir = std::env::temp_dir().join(format!("ism-ssh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let extra = dir.join("extra.conf");
        std::fs::write(&extra, "Host included-host\n  HostName 10.0.0.9\n").unwrap();
        let main = dir.join("config");
        std::fs::write(
            &main,
            format!(
                "Host web db\n  User admin\nHost *\n  ForwardAgent yes\nInclude {}\n",
                extra.display()
            ),
        )
        .unwrap();

        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        collect_hosts(&main, &mut out, &mut seen, 0);

        assert_eq!(out, vec!["web", "db", "included-host"]);
        assert!(!out.contains(&"*".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
