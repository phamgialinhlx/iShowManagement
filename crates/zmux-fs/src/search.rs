//! Finding text across a project, on whichever machine it lives on.
//!
//! ## Why `grep -rIZ` and not a walk
//!
//! The obvious implementation — list every directory, read every file, scan it —
//! is one round trip per file. Over SSH that is unusable on any real checkout:
//! a few thousand files becomes a few thousand connections' worth of latency
//! even with ControlMaster holding the socket open. `grep` already does this
//! well, in one process, on the machine that owns the disk, and returns only the
//! matches. So the search runs *there* and only the answer crosses the network.
//!
//! ## Records are NUL-delimited, for the same reason listings are
//!
//! A Unix filename may contain spaces, tabs and newlines — everything except
//! `/` and NUL. `grep -Z` terminates the *filename* with a NUL, so the parser
//! reads to the NUL for the path and only then to the newline for the rest.
//! Splitting the whole stream on newlines would corrupt any file whose name
//! contains one, which is rare enough that nobody notices until it happens to
//! someone.
//!
//! ## Binary files are skipped, not mangled
//!
//! `-I` drops binaries. Without it a match inside a `.png` returns a line of
//! raw bytes that the webview renders as replacement characters and that no one
//! can act on.

use zmux_transport::shell_quote;
use serde::{Deserialize, Serialize};

/// What to look for.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub text: String,
    /// Match case. Off by default — the common case is looking for a symbol
    /// whose exact casing you half-remember.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Treat `text` as a regular expression rather than a literal.
    #[serde(default)]
    pub regex: bool,
}

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub line: u32,
    /// The matching line, trimmed of trailing whitespace.
    pub text: String,
}

/// How many matches to bring back.
///
/// A bounded answer that arrives beats a complete one that hangs the pane. The
/// UI says when it truncated, so the number is never mistaken for the total.
pub const LIMIT: usize = 500;

/// Directories never worth searching.
///
/// Not a nicety: a single `node_modules` outweighs the entire project it sits
/// in, so without this the first page of results for any word is dependency
/// source and the search reads as broken.
const SKIP: &[&str] = &[".git", "node_modules", "target", "dist", "build", ".next", "vendor"];

/// The shell line that performs the search.
///
/// Everything interpolated goes through `shell_quote` — this reaches a remote
/// login shell, so an unquoted query is an injection rather than a cosmetic bug.
/// A literal search also passes `-F`, which means a query full of `.` and `*`
/// finds what it says rather than matching everything.
pub fn script(root: &str, query: &SearchQuery) -> String {
    let mut flags = String::from("-rInZ");
    if !query.case_sensitive {
        flags.push('i');
    }
    if !query.regex {
        flags.push('F');
    }

    let excludes: String =
        SKIP.iter().map(|d| format!(" --exclude-dir={}", shell_quote(d))).collect();

    // `|| true` because grep exits 1 when nothing matched, which is an answer,
    // not a failure — without it "no results" surfaces as an error dialog.
    format!(
        "grep {flags}{excludes} -e {} -- {} 2>/dev/null | head -n {LIMIT} || true",
        shell_quote(&query.text),
        shell_quote(root),
    )
}

/// Parse `path\0line:text\n` records.
///
/// Deliberately forgiving: a record it cannot read is skipped rather than
/// failing the whole search. `grep` implementations differ at the edges, and one
/// odd line must not cost the other four hundred.
pub fn parse(bytes: &[u8]) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = bytes;

    while let Some(nul) = rest.iter().position(|b| *b == 0) {
        let path = String::from_utf8_lossy(&rest[..nul]).into_owned();
        rest = &rest[nul + 1..];

        // The rest of the record runs to the newline: `<line>:<text>`.
        let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
        let record = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = if end < rest.len() { &rest[end + 1..] } else { &[] };

        let Some((number, text)) = record.split_once(':') else { continue };
        let Ok(line) = number.trim().parse::<u32>() else { continue };

        hits.push(SearchHit { path, line, text: text.trim_end().to_owned() });
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(text: &str) -> SearchQuery {
        SearchQuery { text: text.to_owned(), ..Default::default() }
    }

    #[test]
    fn a_filename_containing_a_newline_survives() {
        // The reason records are NUL-delimited. Splitting the stream on newlines
        // would read this as two records and report a file that does not exist.
        let hits = parse(b"src/od\nd name.rs\x0012:hello\n");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/od\nd name.rs");
        assert_eq!(hits[0].line, 12);
        assert_eq!(hits[0].text, "hello");
    }

    #[test]
    fn a_match_containing_a_colon_keeps_all_of_it() {
        // `split_once`, not `split` — a line of code is full of colons and
        // splitting on every one truncates the result to its first fragment.
        let hits = parse(b"a.rs\x007:  let url = \"http://x\";\n");
        // Leading indentation is kept — it is how you recognise the line when
        // the result is shown next to forty others. Only trailing space goes.
        assert_eq!(hits[0].text, "  let url = \"http://x\";");
    }

    #[test]
    fn the_query_is_quoted_and_literal_by_default() {
        // It reaches a remote login shell. Without quoting this is an injection,
        // and without `-F` a query of `.*` matches every line in the project.
        let hostile = "'; rm -rf /; echo '";
        let line = script("/srv/app", &query(hostile));
        assert!(line.contains("-rInZiF"), "{line}");
        // Checking that the substring is *absent* would be wrong: the correctly
        // quoted form still contains it, which is what a safe line looks like.
        // What matters is that it appears only in its quoted form.
        assert!(line.contains(&zmux_transport::shell_quote(hostile)), "unquoted query: {line}");
        assert!(!line.contains("-e '; rm"), "escaped out of its quotes: {line}");
        assert!(line.contains("--exclude-dir=node_modules"), "{line}");
    }

    #[test]
    fn a_regex_search_drops_the_literal_flag() {
        let line = script("/srv", &SearchQuery { text: "fn .*".into(), regex: true, ..Default::default() });
        assert!(!line.contains("F "), "{line}");
        assert!(line.contains("-rInZi"), "{line}");
    }

    #[test]
    fn case_sensitivity_is_opt_in() {
        let sensitive =
            script("/srv", &SearchQuery { text: "Foo".into(), case_sensitive: true, ..Default::default() });
        assert!(!sensitive.contains("-rInZi"), "{sensitive}");
    }

    #[test]
    fn nothing_found_is_not_an_error() {
        // grep exits 1 when there are no matches. Without the `|| true` the
        // caller reports a failed command and the pane shows an error for the
        // most ordinary outcome there is.
        assert!(script("/srv", &query("x")).ends_with("|| true"));
        assert!(parse(b"").is_empty());
    }
}
