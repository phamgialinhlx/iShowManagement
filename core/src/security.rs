//! Shell-safety helpers. Any user-influenced name that reaches a shell must pass
//! `safe_name`; anything interpolated into a remote command string must go
//! through `shell_quote`. Mirrors `references/tsmanager/server/shell.js`.

use std::sync::OnceLock;

use regex::Regex;

/// `^[A-Za-z0-9][A-Za-z0-9_.-]*$` — validates container ids, session names, etc.
pub fn safe_name(s: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.-]*$").expect("valid regex"))
        .is_match(s)
}

/// True if a browser `Origin` header points at a loopback host (any port), or
/// is absent/opaque. Non-loopback origins (a remote page hitting our local
/// server) are rejected. Mirrors `references/tsmanager/server/security.js`.
pub fn is_allowed_origin(origin: &str) -> bool {
    // Non-browser clients (curl) send no Origin; `null` is an opaque origin.
    if origin.is_empty() || origin == "null" {
        return true;
    }
    let Some(host) = origin_host(origin) else {
        return false;
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

/// Extract the host from an `scheme://host[:port]` origin, unwrapping `[::1]`.
fn origin_host(origin: &str) -> Option<String> {
    let authority = origin.split("://").nth(1)?;
    let authority = authority.split('/').next().unwrap_or(authority);
    if let Some(rest) = authority.strip_prefix('[') {
        // IPv6 literal: host is inside the brackets.
        return rest.split(']').next().map(|h| h.to_string());
    }
    Some(authority.split(':').next().unwrap_or(authority).to_string())
}

/// POSIX single-quote `s` for safe interpolation into a remote command line.
/// (Used from Phase 4 when manager commands interpolate names.)
#[allow(dead_code)]
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''"); // close, escaped quote, reopen
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_name_accepts_plain_identifiers() {
        for ok in ["web", "db-1", "my_app.v2", "A0"] {
            assert!(safe_name(ok), "{ok} should be allowed");
        }
    }

    #[test]
    fn safe_name_rejects_shell_metacharacters() {
        for bad in ["", "-flag", ".hidden", "a b", "a;b", "a$(x)", "a|b", "a/b", "a`b`"] {
            assert!(!safe_name(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn origin_guard_allows_loopback_and_absent_only() {
        for ok in [
            "",
            "null",
            "http://localhost",
            "http://localhost:5173",
            "http://127.0.0.1:7070",
            "https://[::1]:7070",
        ] {
            assert!(is_allowed_origin(ok), "{ok} should be allowed");
        }
        for bad in [
            "http://evil.com",
            "https://example.org:7070",
            "http://127.0.0.1.evil.com",
            "http://10.0.0.5:7070",
        ] {
            assert!(!is_allowed_origin(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("abc"), "'abc'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        // A quoted value cannot break out of the quotes.
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }
}
