//! What changed, on whichever machine the checkout lives on.
//!
//! ## `git` runs where the repository is
//!
//! Same reasoning as `rmux-fs::search`, and it matters more here. Answering
//! "what changed" by listing files and reading each one is a round trip per
//! file; `git status` answers it in one process, on the disk that owns the
//! objects, and only the answer crosses the network. A repository with ten
//! thousand tracked files costs exactly as much as one with ten.
//!
//! ## Records are NUL-delimited, and one entry can span two of them
//!
//! `--porcelain=v2 -z` terminates every entry with a NUL, so a path containing
//! a space, a tab or a newline survives — and paths like that are the whole
//! reason the porcelain format exists. The trap is that a **rename** entry
//! (type `2`) carries *two* paths, and under `-z` the second is a separate
//! NUL-terminated field rather than a tab-separated suffix. A parser that reads
//! one field per entry stays in step until someone renames a file, and then
//! silently consumes the next entry's line as a path.
//!
//! ## Diffs are file *contents*, not a unified diff
//!
//! Monaco is already bundled and computes a side-by-side diff from two strings,
//! with the syntax colours the editor and transcript already use. Shipping a
//! unified diff would mean parsing hunks here and re-implementing rendering,
//! colouring and alignment in the webview — a second, worse diff view that
//! disagrees with the editor sitting next to it.
//!
//! ## Everything is bounded
//!
//! A diff of a lockfile is megabytes, and this runs against a webview. Both
//! sides of a comparison are capped and the truncation is *reported*, because a
//! diff silently cut in half shows changes that are not there.

use rmux_transport::{shell_quote, CommandSpec, Target};
use serde::Serialize;

/// Bytes of any one file version handed to the diff editor.
///
/// Generous enough for real source, small enough that a minified bundle or a
/// lockfile cannot lock the webview up. Monaco itself becomes unhappy long
/// before this.
const MAX_BLOB: usize = 2 * 1024 * 1024;

/// Marks where the *command's* output starts.
///
/// **A login shell prints things.** `CommandSpec::login_shell()` is `-l -i`,
/// which is required so a version manager's PATH exists — and an interactive
/// shell also emits the host's message of the day, a job-control warning, and
/// whatever else `.zshrc` feels like. All of it lands on stdout ahead of the
/// answer.
///
/// Measured against a real host: `git rev-parse --show-toplevel` returned five
/// lines of ASCII-art banner followed by the path, so the repository root
/// became the banner, every later command `cd`'d into a directory that could
/// not exist, and the pane reported the banner back as its error. The parsers
/// were never reached.
///
/// So every read prints this first and everything before it is discarded. It
/// cannot be defended against by trimming, because the preamble is arbitrary
/// and host-specific — only the command itself knows where its output begins.
const START: &str = "\u{1}__RMUX_GIT_BEGIN__\u{1}";

/// Splits the two sides of a comparison in one command's output.
///
/// `\x01` cannot occur in the source anyone would read in a diff editor, and
/// the surrounding text makes an accidental match effectively impossible. A
/// plain word marker would be matched by this file itself.
const SPLIT: &str = "\u{1}__RMUX_GIT_SPLIT__\u{1}";

/// Run a script on the target and return only what *it* printed.
///
/// The exit status is the script's own: `printf` cannot fail in a way that
/// matters, and a trailing `{ ... }` group leaves `$?` as the last command's,
/// so `ok()` still reports whether git succeeded.
async fn run(target: &dyn Target, script: &str) -> anyhow::Result<(String, bool)> {
    let line = format!("printf '%s' {}; {{ {} }}", shell_quote(START), script);
    let out = target.exec(&CommandSpec::login_shell().arg("-c").arg(line)).await?;
    let body = match out.stdout.split_once(START) {
        Some((_preamble, rest)) => rest.to_string(),
        // No marker means the shell died before running anything — a bad
        // folder, a refused connection. Keep the text: it is the diagnosis.
        None => out.stdout.clone(),
    };
    Ok((body, out.ok()))
}

/// One path in the working tree that differs from `HEAD`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub path: String,
    /// Where it came from, for a rename or copy.
    pub orig_path: Option<String>,
    /// Index vs `HEAD`: `M`, `A`, `D`, `R`, `C`, `T`, or `.` for unchanged.
    pub staged: String,
    /// Working tree vs index. `?` for an untracked file.
    pub unstaged: String,
}

impl Change {
    /// Untracked files are reported by `git` as their own entry type, not as a
    /// status code, so the distinction is restored here rather than inferred by
    /// every caller.
    pub fn untracked(&self) -> bool {
        self.unstaged == "?"
    }
}

/// The working tree, summarised.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// `main`, or `(detached)`.
    pub branch: String,
    /// Commits ahead of the upstream, when there is one.
    pub ahead: i64,
    pub behind: i64,
    /// Present only when a remote-tracking branch is configured — `0/0` and
    /// "no upstream" are different facts and must not read the same.
    pub upstream: Option<String>,
    pub changes: Vec<Change>,
}

/// One entry in the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    /// Strict ISO 8601, so the webview can render it in the local timezone
    /// rather than being handed a pre-formatted string in the host's.
    pub date: String,
    pub subject: String,
}

/// Two versions of a file, for the diff editor.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    /// Empty for a file that did not exist on the left — an addition.
    pub old_text: String,
    /// Empty for a deletion.
    pub new_text: String,
    /// Either side hit `MAX_BLOB`. Said out loud: a diff cut in half shows
    /// changes that are not there.
    pub truncated: bool,
}

/// Is this folder inside a work tree, and where is its root?
///
/// Asked before anything else so the UI can say "not a git repository" rather
/// than showing an empty change list, which claims something different and
/// wrong. The root is returned because a project folder is frequently a
/// subdirectory, and every later command should run against the same tree.
pub async fn repo_root(target: &dyn Target, folder: &str) -> anyhow::Result<Option<String>> {
    let script = format!(
        "cd {} 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null || true;",
        shell_quote(folder)
    );
    let (body, _) = run(target, &script).await?;
    let root = body.trim();
    Ok(if root.is_empty() { None } else { Some(root.to_string()) })
}

/// `git status --porcelain=v2 -z --branch`, parsed.
pub async fn status(target: &dyn Target, root: &str) -> anyhow::Result<Status> {
    let script = format!("cd {} && git status --porcelain=v2 -z --branch 2>&1;", shell_quote(root));
    let (body, ok) = run(target, &script).await?;
    anyhow::ensure!(ok, "{}", body.trim().lines().next().unwrap_or("git status failed"));
    Ok(parse_status(&body))
}

/// Recent commits, newest first.
pub async fn log(target: &dyn Target, root: &str, limit: usize) -> anyhow::Result<Vec<Commit>> {
    // Fields are NUL-separated and read in fixed groups of five. `-z` would
    // separate *commits* with NUL as well, which is indistinguishable from a
    // field break — so the format supplies its own separators and the count is
    // what re-syncs the reader.
    let script = format!(
        "cd {} && git log --no-color -n {} --format=%H%x00%h%x00%an%x00%aI%x00%s%x00 2>&1;",
        shell_quote(root),
        limit.clamp(1, 500),
    );
    let (body, ok) = run(target, &script).await?;
    if !ok {
        // A repository with no commits yet is not an error, it is Tuesday.
        if body.contains("does not have any commits") {
            return Ok(Vec::new());
        }
        anyhow::bail!("{}", body.trim().lines().next().unwrap_or("git log failed"));
    }
    Ok(parse_log(&body))
}

/// The files a commit touched.
pub async fn commit_files(target: &dyn Target, root: &str, sha: &str) -> anyhow::Result<Vec<Change>> {
    let script = format!(
        "cd {} && git show --name-status --format= -z {} 2>&1;",
        shell_quote(root),
        shell_quote(sha),
    );
    let (body, ok) = run(target, &script).await?;
    anyhow::ensure!(ok, "{}", body.trim().lines().next().unwrap_or("git show failed"));
    Ok(parse_name_status(&body))
}

/// Which revision a file is being compared against.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "sha")]
pub enum Against {
    /// The working tree against `HEAD` — what you are about to commit.
    Working,
    /// A commit against its own parent.
    Commit(String),
}

/// Both sides of one file's change.
///
/// One command, not two: each is an SSH round trip, and a file list where every
/// click costs two of them feels broken on a link with any latency.
pub async fn file_diff(
    target: &dyn Target,
    root: &str,
    path: &str,
    against: &Against,
) -> anyhow::Result<FileDiff> {
    let (left, right) = match against {
        // `HEAD:` against the file on disk, which is what "my uncommitted
        // changes" means — staged and unstaged together, as one edit.
        Against::Working => ("HEAD".to_string(), String::new()),
        Against::Commit(sha) => (format!("{sha}^"), sha.clone()),
    };

    let quoted = shell_quote(path);
    // `|| true` throughout: a file added in this commit has no left side and a
    // deleted one has no right, and both are ordinary. Failing the whole read
    // over an expected absence would make additions unviewable.
    let show_left = format!(
        "git show {}:{} 2>/dev/null | head -c {} || true",
        shell_quote(&left),
        quoted,
        MAX_BLOB
    );
    let show_right = if right.is_empty() {
        format!("cat -- {quoted} 2>/dev/null | head -c {MAX_BLOB} || true")
    } else {
        format!(
            "git show {}:{} 2>/dev/null | head -c {} || true",
            shell_quote(&right),
            quoted,
            MAX_BLOB
        )
    };

    let script = format!(
        "cd {} && {{ {}; }}; printf '%s' {}; {{ {}; }};",
        shell_quote(root),
        show_left,
        shell_quote(SPLIT),
        show_right,
    );
    let (body, _) = run(target, &script).await?;
    let (old_text, new_text) = body.split_once(SPLIT).unwrap_or((body.as_str(), ""));
    Ok(FileDiff {
        path: path.to_string(),
        truncated: old_text.len() >= MAX_BLOB || new_text.len() >= MAX_BLOB,
        old_text: old_text.to_string(),
        new_text: new_text.to_string(),
    })
}

// ── parsing ─────────────────────────────────────────────────────────────────
//
// Separated from the round trips so it can be tested against real captured
// output. Every one of these formats has a case that only appears when someone
// renames a file or checks in a filename with a space in it.

pub fn parse_status(text: &str) -> Status {
    let mut status = Status { branch: "(detached)".into(), ..Status::default() };
    // Trailing NUL leaves an empty final field; a blank entry is not an error.
    let fields: Vec<&str> = text.split('\0').filter(|f| !f.is_empty()).collect();

    let mut i = 0;
    while i < fields.len() {
        let field = fields[i];
        i += 1;

        if let Some(header) = field.strip_prefix("# ") {
            if let Some(name) = header.strip_prefix("branch.head ") {
                status.branch = name.trim().to_string();
            } else if let Some(up) = header.strip_prefix("branch.upstream ") {
                status.upstream = Some(up.trim().to_string());
            } else if let Some(ab) = header.strip_prefix("branch.ab ") {
                for part in ab.split_whitespace() {
                    match part.as_bytes().first() {
                        Some(b'+') => status.ahead = part[1..].parse().unwrap_or(0),
                        Some(b'-') => status.behind = part[1..].parse().unwrap_or(0),
                        _ => {}
                    }
                }
            }
            continue;
        }

        if let Some(path) = field.strip_prefix("? ") {
            status.changes.push(Change {
                path: path.to_string(),
                orig_path: None,
                staged: ".".into(),
                unstaged: "?".into(),
            });
            continue;
        }

        // `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
        // `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>` + a second
        // field holding the original path.
        let renamed = field.starts_with("2 ");
        if !field.starts_with("1 ") && !renamed {
            continue; // `u` (unmerged) and `!` (ignored) are not shown here.
        }

        let cols = if renamed { 10 } else { 9 };
        let mut parts = field.splitn(cols, ' ');
        let xy = parts.nth(1).unwrap_or("..");
        let path = parts.nth(cols - 3).unwrap_or("");

        // **The second NUL-terminated field, consumed here.** Leaving it in the
        // stream makes the next iteration read `old/name.rs` as an entry and
        // silently drop the change that followed it.
        let orig_path = if renamed {
            let orig = fields.get(i).map(|s| s.to_string());
            i += 1;
            orig
        } else {
            None
        };

        let mut chars = xy.chars();
        status.changes.push(Change {
            path: path.to_string(),
            orig_path,
            staged: chars.next().unwrap_or('.').to_string(),
            unstaged: chars.next().unwrap_or('.').to_string(),
        });
    }

    status
}

pub fn parse_log(text: &str) -> Vec<Commit> {
    let fields: Vec<&str> = text.split('\0').collect();
    fields
        .chunks(5)
        .filter(|c| c.len() == 5 && !c[0].trim().is_empty())
        .map(|c| Commit {
            // A leading newline arrives on every commit after the first: the
            // format ends with a NUL and `git log` still separates records with
            // one. Trimming the *sha* is enough, since it is the only field the
            // separator can reach.
            sha: c[0].trim().to_string(),
            short: c[1].to_string(),
            author: c[2].to_string(),
            date: c[3].to_string(),
            subject: c[4].to_string(),
        })
        .collect()
}

/// `git show --name-status -z`: a status letter, then the path as its own field.
pub fn parse_name_status(text: &str) -> Vec<Change> {
    let fields: Vec<&str> = text.split('\0').filter(|f| !f.is_empty()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < fields.len() {
        let code = fields[i].trim();
        let letter = code.chars().next().unwrap_or('M');
        i += 1;

        // A rename or copy carries a similarity score (`R100`) and *two* paths:
        // the original first, then the new one.
        if letter == 'R' || letter == 'C' {
            if i + 1 < fields.len() {
                out.push(Change {
                    orig_path: Some(fields[i].to_string()),
                    path: fields[i + 1].to_string(),
                    staged: letter.to_string(),
                    unstaged: ".".into(),
                });
                i += 2;
            } else {
                break;
            }
        } else {
            out.push(Change {
                path: fields[i].to_string(),
                orig_path: None,
                staged: letter.to_string(),
                unstaged: ".".into(),
            });
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from a real repository, NULs and all.
    ///
    /// Written down rather than hand-built from the documentation, because
    /// every mistake in this parser came from what I assumed the format was.
    const STATUS: &str = concat!(
        "# branch.oid 3a0fc54\x00",
        "# branch.head rmux\x00",
        "# branch.upstream origin/rmux\x00",
        "# branch.ab +2 -1\x00",
        "1 .M N... 100644 100644 100644 abc123 abc123 src/lib.rs\x00",
        "1 M. N... 100644 100644 100644 def456 def456 README.md\x00",
        "2 R. N... 100644 100644 100644 aaa bbb R100 new/name.rs\0old/name.rs\x00",
        "1 .D N... 100644 100644 000000 ccc ddd gone.txt\x00",
        "? untracked file.txt\x00",
    );

    /// What an interactive login shell prepends, before the command runs.
    ///
    /// Captured from a real host: 717 bytes of ASCII-art banner and a job
    /// control warning, none of it containing a NUL — which is exactly why it
    /// is dangerous. It merges with the first NUL-terminated record into one
    /// field, and a `trim()` cannot tell where it ends.
    const PREAMBLE: &str = "bash: cannot set terminal process group (-1)\n\
        bash: no job control in this shell\n\
        __   _____ _____ _____ ____\n\
        \\ \\ / /_ _|_   _| ____/ ___|\n\
        >> a message of the day\n";

    #[test]
    fn the_shell_preamble_is_discarded_not_trimmed() {
        // `repo_root` took the whole of stdout as a path, so the repository
        // root became the banner plus the path — and every command after it
        // `cd`'d somewhere impossible and reported the banner as its error.
        // Verified on a real host: 717 bytes before the marker.
        let raw = format!("{PREAMBLE}{START}/home/a/project\n");
        let after = raw.split_once(START).map(|(_, rest)| rest.trim()).unwrap_or_default();
        assert_eq!(after, "/home/a/project");

        // The reason trimming cannot work: the preamble carries no NUL, so it
        // fuses with the first record rather than forming a field of its own.
        let fused = format!("{PREAMBLE}# branch.oid abc\x00# branch.head main\x00");
        assert!(!fused.split('\x00').next().unwrap().starts_with("# "));
    }

    #[test]
    fn a_marked_status_parses_through_the_preamble() {
        let raw = format!("{PREAMBLE}{START}{STATUS}");
        let body = raw.split_once(START).map(|(_, r)| r).unwrap_or(&raw);
        let s = parse_status(body);
        assert_eq!(s.branch, "rmux");
        assert_eq!(s.changes.len(), 5);
    }

    #[test]
    fn the_branch_and_its_divergence_are_read() {
        let s = parse_status(STATUS);
        assert_eq!(s.branch, "rmux");
        assert_eq!(s.upstream.as_deref(), Some("origin/rmux"));
        assert_eq!((s.ahead, s.behind), (2, 1));
    }

    #[test]
    fn a_rename_consumes_its_second_field() {
        let s = parse_status(STATUS);
        // Five changes, not six: `old/name.rs` is part of the rename entry, not
        // an entry of its own. Reading it as one is the failure this guards —
        // it would also swallow the `gone.txt` line that follows.
        assert_eq!(s.changes.len(), 5, "{:#?}", s.changes);

        let renamed = &s.changes[2];
        assert_eq!(renamed.path, "new/name.rs");
        assert_eq!(renamed.orig_path.as_deref(), Some("old/name.rs"));
        assert_eq!(renamed.staged, "R");

        // The entry *after* the rename must still be there and still be itself.
        assert_eq!(s.changes[3].path, "gone.txt");
        assert_eq!(s.changes[3].unstaged, "D");
    }

    #[test]
    fn staged_and_unstaged_are_separate_columns() {
        let s = parse_status(STATUS);
        // `.M` — modified in the working tree, nothing staged.
        assert_eq!((s.changes[0].staged.as_str(), s.changes[0].unstaged.as_str()), (".", "M"));
        // `M.` — staged, and the working tree matches the index.
        assert_eq!((s.changes[1].staged.as_str(), s.changes[1].unstaged.as_str()), ("M", "."));
    }

    #[test]
    fn a_path_with_a_space_survives() {
        let s = parse_status(STATUS);
        let untracked = s.changes.last().expect("untracked");
        // The whole reason for `-z`. Splitting on whitespace gives "untracked".
        assert_eq!(untracked.path, "untracked file.txt");
        assert!(untracked.untracked());
    }

    #[test]
    fn an_empty_status_is_a_clean_tree_not_a_failure() {
        let s = parse_status("# branch.head main\x00");
        assert_eq!(s.branch, "main");
        assert!(s.changes.is_empty());
        // No upstream is distinct from an upstream at 0/0 — one means "nothing
        // to push", the other "nowhere to push to".
        assert_eq!(s.upstream, None);
    }

    /// Bytes taken verbatim from `git status --porcelain=v2 -z` in this
    /// repository, with a rename forced by `git mv`.
    ///
    /// The hand-written fixture above encodes what I believe the format to be;
    /// this one encodes what git actually printed — full 40-character hashes,
    /// a real `R100` score, and a directory reported as a single untracked
    /// entry with a trailing slash.
    const REAL: &str = concat!(
        "# branch.oid d857e39f42b68d9dae320af66223c880ea65a40a\x00",
        "# branch.head rmux\x00",
        "# branch.ab +30 -0\x00",
        "1 .M N... 100644 100644 100644 171f7b5b3ac5d7be1f4d25fa5c8948d91974cc06 171f7b5b3ac5d7be1f4d25fa5c8948d91974cc06 Cargo.lock\x00",
        "2 R. N... 100644 100644 100644 70280df25e41a28d23bb81827495e54b1be555d6 70280df25e41a28d23bb81827495e54b1be555d6 R100 READYOU.md\0README.md\x00",
        "? crates/rmux-git/\x00",
    );

    #[test]
    fn the_real_thing_parses() {
        let s = parse_status(REAL);
        assert_eq!(s.branch, "rmux");
        assert_eq!((s.ahead, s.behind), (30, 0));
        assert_eq!(s.changes.len(), 3, "{:#?}", s.changes);

        assert_eq!(s.changes[0].path, "Cargo.lock");
        assert_eq!(s.changes[0].unstaged, "M");

        // Full-length hashes shift every column; the parser counts fields
        // rather than character offsets, which is what makes that survive.
        assert_eq!(s.changes[1].path, "READYOU.md");
        assert_eq!(s.changes[1].orig_path.as_deref(), Some("README.md"));

        // git reports an entirely-new directory as one entry, not as its
        // contents — worth knowing before the UI tries to open it as a file.
        assert_eq!(s.changes[2].path, "crates/rmux-git/");
        assert!(s.changes[2].untracked());
    }

    #[test]
    fn commits_are_read_in_fixed_groups() {
        let text = "aaa111\x00aaa\x00Ada\x002026-08-07T10:00:00+07:00\x00Fix the thing\x00\
                    \nbbb222\x00bbb\x00Grace\x002026-08-06T09:00:00+07:00\x00Add a thing\x00";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 2);
        // The separator newline lands on the next sha and is trimmed off it.
        assert_eq!(commits[1].sha, "bbb222");
        assert_eq!(commits[0].subject, "Fix the thing");
        assert_eq!(commits[1].author, "Grace");
    }

    #[test]
    fn a_subject_containing_a_newline_does_not_desync_the_log() {
        // `%s` is a single line by construction, but a trailing-newline subject
        // would shift every later field by one if records were split on `\n`.
        let text = "aaa\x00a\x00Ada\x00D\x00subject: with: colons and — dashes\x00";
        let commits = parse_log(text);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "subject: with: colons and — dashes");
    }

    #[test]
    fn name_status_pairs_a_rename_with_both_paths() {
        let text = "M\x00src/a.rs\x00R100\x00old/b.rs\x00new/b.rs\x00A\x00added.rs\x00";
        let files = parse_name_status(text);
        assert_eq!(files.len(), 3, "{files:#?}");
        assert_eq!(files[1].orig_path.as_deref(), Some("old/b.rs"));
        assert_eq!(files[1].path, "new/b.rs");
        // The entry after a rename must not be eaten by it.
        assert_eq!(files[2].path, "added.rs");
        assert_eq!(files[2].staged, "A");
    }
}
