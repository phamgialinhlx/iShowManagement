//! Listing the hosts in `~/.ssh/config`, for the connection picker.
//!
//! **This does not resolve SSH configuration, and must never start to.** zmux's
//! standing rule is that host aliases go to the `ssh` binary verbatim, because
//! OpenSSH's grammar — `Match`, `Include`, `CanonicalizeHostname`, `%h`/`%p`/`%r`
//! tokens, per-host `ProxyJump` — is far larger than it looks, and every Rust
//! reimplementation covers only part of it. Getting that subtly wrong means
//! connecting to the wrong machine.
//!
//! What happens here is strictly *enumeration*: reading which names exist so the
//! picker can offer them. Nothing read here decides how a connection is made. The
//! alias is handed to `ssh` exactly as written, and `ssh` resolves it.
//!
//! The one thing this module deliberately does interpret is `Include`, because a
//! config that farms its hosts out to `~/.ssh/config.d/*` would otherwise look
//! empty — and an empty picker is indistinguishable from a broken one.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A host the user can pick.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigHost {
    /// The `Host` alias, passed to `ssh` unchanged.
    pub alias: String,
    /// `HostName` from the same block, shown to help tell similar aliases apart.
    ///
    /// **Display only.** It is never used to connect — that would be resolving
    /// config ourselves, which is exactly what this module refuses to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// `User` from the same block. Display only, as above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Guards against an `Include` cycle, which would otherwise recurse forever.
const MAX_INCLUDE_DEPTH: usize = 8;

/// Hosts from the user's SSH config, in the order they appear.
pub fn list_hosts() -> Vec<ConfigHost> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".ssh").join("config");

    let mut hosts = Vec::new();
    let mut seen = HashSet::new();
    read_into(&path, &home, 0, &mut hosts, &mut seen);
    hosts
}

fn read_into(
    path: &Path,
    home: &Path,
    depth: usize,
    hosts: &mut Vec<ConfigHost>,
    seen: &mut HashSet<String>,
) {
    if depth > MAX_INCLUDE_DEPTH {
        tracing::warn!(?path, "ssh config Include nested too deeply; stopping");
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };

    for entry in parse(&text) {
        match entry {
            Entry::Host(host) => {
                // A later block for the same alias does not add a new choice.
                if seen.insert(host.alias.clone()) {
                    hosts.push(host);
                }
            }
            Entry::Include(pattern) => {
                for included in expand_include(&pattern, home) {
                    read_into(&included, home, depth + 1, hosts, seen);
                }
            }
        }
    }
}

/// Resolve an `Include` value into concrete files.
///
/// Relative paths are relative to `~/.ssh`, per `ssh_config(5)`. Globs are
/// expanded manually rather than with a glob crate — only the `*` and `?` forms
/// that appear in real configs are supported, which keeps a dependency out of the
/// tree for a listing feature.
fn expand_include(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let expanded = if let Some(rest) = pattern.strip_prefix("~/") {
        home.join(rest)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        home.join(".ssh").join(pattern)
    };

    let text = expanded.to_string_lossy().into_owned();
    if !text.contains('*') && !text.contains('?') {
        return vec![expanded];
    }

    // Only the final component may contain a wildcard in practice.
    let Some(parent) = expanded.parent() else {
        return Vec::new();
    };
    let Some(file_pattern) = expanded.file_name().map(|f| f.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };

    let mut matched: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter(|e| matches_glob(&file_pattern, &e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    // Directory order is arbitrary; sort so the picker is stable between runs.
    matched.sort();
    matched
}

/// Split a `Host` value into patterns, honouring double quotes.
///
/// Whitespace separates patterns — `Host a b` really is two aliases, and `ssh a`
/// and `ssh b` both work — but `ssh_config` also allows quoting, so
/// `Host "my server"` is one alias containing a space. Splitting that naively
/// would offer two hosts that do not exist.
fn split_patterns(value: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut current = String::new();
    let mut quoted = false;

    for ch in value.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    patterns.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        patterns.push(current);
    }
    patterns
}

/// Match a name against a `*`/`?` glob.
fn matches_glob(pattern: &str, name: &str) -> bool {
    fn walk(pattern: &[u8], name: &[u8]) -> bool {
        match pattern.first() {
            None => name.is_empty(),
            Some(b'*') => {
                // Try every split point: `*` may consume nothing or everything.
                (0..=name.len()).any(|i| walk(&pattern[1..], &name[i..]))
            }
            Some(b'?') => !name.is_empty() && walk(&pattern[1..], &name[1..]),
            Some(c) => name.first() == Some(c) && walk(&pattern[1..], &name[1..]),
        }
    }
    walk(pattern.as_bytes(), name.as_bytes())
}

enum Entry {
    Host(ConfigHost),
    Include(String),
}

/// Extract `Host` blocks and `Include` directives from config text.
///
/// A pure function over the text, so the awkward cases — multiple patterns on one
/// line, wildcards, `Match` blocks, odd spacing — are testable without a
/// filesystem.
fn parse(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    // Index into `entries` of the block currently being filled in, so `HostName`
    // and `User` land on the right hosts.
    let mut current: Vec<usize> = Vec::new();

    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        // Keywords may be separated by whitespace or '='.
        let (keyword, value) = match line.split_once(['=', ' ', '\t']) {
            Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim_start_matches(['=', ' ', '\t']).trim()),
            None => (line.to_ascii_lowercase(), ""),
        };

        match keyword.as_str() {
            "host" => {
                current.clear();
                for pattern in split_patterns(value) {
                    let pattern = pattern.as_str();
                    // Wildcards and negations describe *rules*, not machines —
                    // `Host *` sets defaults for everything and is not somewhere
                    // you can connect. Offering it would be offering a mistake.
                    if pattern.contains(['*', '?', '!']) {
                        continue;
                    }
                    current.push(entries.len());
                    entries.push(Entry::Host(ConfigHost {
                        alias: pattern.to_owned(),
                        hostname: None,
                        user: None,
                    }));
                }
            }
            // A `Match` block ends the current `Host` block, and its own settings
            // belong to no single alias.
            "match" => current.clear(),
            "hostname" | "user" => {
                for &index in &current {
                    if let Some(Entry::Host(host)) = entries.get_mut(index) {
                        if keyword == "hostname" {
                            host.hostname = Some(value.to_owned());
                        } else {
                            host.user = Some(value.to_owned());
                        }
                    }
                }
            }
            "include" => {
                for pattern in value.split_whitespace() {
                    entries.push(Entry::Include(pattern.to_owned()));
                }
            }
            _ => {}
        }
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(text: &str) -> Vec<ConfigHost> {
        parse(text)
            .into_iter()
            .filter_map(|e| match e {
                Entry::Host(h) => Some(h),
                Entry::Include(_) => None,
            })
            .collect()
    }

    #[test]
    fn aliases_are_listed_with_their_display_details() {
        let found = hosts(
            "Host devbox\n    HostName 10.0.0.5\n    User deploy\n\nHost prod\n    HostName prod.example.com\n",
        );

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].alias, "devbox");
        assert_eq!(found[0].hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(found[0].user.as_deref(), Some("deploy"));
        assert_eq!(found[1].alias, "prod");
        assert_eq!(found[1].user, None);
    }

    #[test]
    fn one_line_can_declare_several_aliases() {
        // `Host a b c` is three connectable names sharing settings.
        let found = hosts("Host web1 web2 web3\n    User ubuntu\n");

        assert_eq!(found.len(), 3);
        assert_eq!(found.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(), ["web1", "web2", "web3"]);
        // The shared settings apply to every alias on the line.
        assert!(found.iter().all(|h| h.user.as_deref() == Some("ubuntu")));
    }

    #[test]
    fn a_quoted_pattern_with_a_space_is_one_alias() {
        // Unquoted whitespace separates aliases, but a quoted name is a single
        // host — splitting it would offer two machines that do not exist.
        let found = hosts("Host \"my server\"\n    User me\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].alias, "my server");
    }

    #[test]
    fn unquoted_whitespace_still_separates_aliases() {
        // Faithful to ssh: `Host a b` means `ssh a` and `ssh b` both work.
        let found = hosts("Host Build Server\n    HostName 192.168.100.123\n");
        assert_eq!(found.iter().map(|h| h.alias.as_str()).collect::<Vec<_>>(), ["Build", "Server"]);
    }

    #[test]
    fn wildcard_and_negated_patterns_are_not_offered() {
        // `Host *` sets defaults for every host; it is not a machine, and putting
        // it in the picker would offer the user something that cannot work.
        let found = hosts(
            "Host *\n    ServerAliveInterval 60\n\nHost !secret prod\n    User root\n\nHost bastion-?\n",
        );

        let aliases: Vec<&str> = found.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, ["prod"], "only real, connectable names belong in the list");
    }

    #[test]
    fn comments_and_odd_spacing_are_handled() {
        let found = hosts(
            "# a comment\nHost   devbox   # trailing comment\n\tHostName=10.0.0.5\n  User\tdeploy\n",
        );

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].alias, "devbox");
        assert_eq!(found[0].hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(found[0].user.as_deref(), Some("deploy"));
    }

    #[test]
    fn keywords_are_case_insensitive() {
        // ssh_config keywords are case-insensitive, and real configs are mixed.
        let found = hosts("HOST devbox\n    hostname 10.0.0.5\n    USER deploy\n");
        assert_eq!(found[0].alias, "devbox");
        assert_eq!(found[0].hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(found[0].user.as_deref(), Some("deploy"));
    }

    #[test]
    fn settings_after_a_match_block_are_not_attributed_to_a_host() {
        // A `Match` block's settings belong to no single alias; letting them leak
        // onto the previous Host would show a misleading hostname.
        let found = hosts("Host devbox\n    HostName 10.0.0.5\n\nMatch host *.internal\n    User root\n");

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].user, None, "the Match block's User must not attach to devbox");
    }

    #[test]
    fn include_directives_are_reported() {
        let includes: Vec<String> = parse("Include ~/.ssh/config.d/*\nHost devbox\n")
            .into_iter()
            .filter_map(|e| match e {
                Entry::Include(p) => Some(p),
                Entry::Host(_) => None,
            })
            .collect();

        // A config that farms its hosts out to an include would otherwise look
        // empty, which is indistinguishable from the feature being broken.
        assert_eq!(includes, ["~/.ssh/config.d/*"]);
    }

    #[test]
    fn an_empty_or_missing_config_yields_nothing_rather_than_failing() {
        assert!(hosts("").is_empty());
        assert!(hosts("# only comments\n\n").is_empty());
    }

    #[test]
    fn globs_match_the_way_ssh_config_expects() {
        assert!(matches_glob("*.conf", "work.conf"));
        assert!(matches_glob("*", "anything"));
        assert!(matches_glob("host-?", "host-1"));
        assert!(!matches_glob("host-?", "host-12"));
        assert!(!matches_glob("*.conf", "notes.txt"));
        // A `*` may match nothing at all.
        assert!(matches_glob("a*b", "ab"));
    }

    #[test]
    fn a_hostname_is_never_used_as_the_connection_target() {
        // The alias is what goes to ssh. This test exists to pin the invariant:
        // if someone later "helpfully" substitutes hostname for alias, resolution
        // silently stops honouring ProxyJump, IdentityFile and friends.
        let found = hosts("Host devbox\n    HostName 10.0.0.5\n");
        assert_eq!(found[0].alias, "devbox", "the alias is the connection target");
        assert_ne!(found[0].alias, found[0].hostname.clone().unwrap_or_default());
    }
}
