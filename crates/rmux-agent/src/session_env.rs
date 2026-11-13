//! What every session the daemon spawns knows about rmux.
//!
//! An agent running inside a session drives rmux through the `rmux` CLI (see
//! [`crate::orchestrate`]), so it needs three facts and one binary:
//!
//! - `RMUX_ENV=1` — "you are inside rmux". The skill refuses to act without it.
//! - `RMUX_SESSION` — this session's daemon name, which is what makes `close`
//!   ownership and `children.json` parentage work without the caller naming
//!   itself (and so without being able to name somebody else by mistake).
//! - `RMUX_BIN` — the **agent build that owns this session**. Several builds
//!   coexist on a host (fingerprinted installs, `hand_off`), and a session lives
//!   in its creator's daemon; a CLI call from inside it must reach that daemon.
//! - `rmux` on `PATH` — a tiny shim in `~/.rmux/bin` that execs `$RMUX_BIN`.
//!
//! ## Why the env is set *here*
//!
//! The daemon is the one process every session passes through — sessions the
//! desktop app opens, the bridge's, and the CLI's own children. Env attached to
//! the client's attach command would never arrive (the daemon spawns from its
//! own environment); `Hello.env` would need every client to remember it.
//!
//! ## Why `PATH` also goes on the shell line
//!
//! A login shell re-reads the system profile, and on Debian/Ubuntu
//! `/etc/profile` *assigns* `PATH` outright — so an env-only prepend is gone
//! before the first prompt. For a session launched with a command line (every
//! Claude and pi), the daemon prefixes `PATH=<bin>:"$PATH"` as an assignment on
//! that line, which runs *after* the profiles. An assignment prefix is what the
//! line already starts with (`CLAUDE_CODE_…=1 claude`), so the shell still execs
//! the single command and the session keeps its pid — the pid is how status is
//! correlated, so a wrapper (`export …; claude`) that forked would silently cut
//! every status dot. A plain interactive shell gets the env prepend only; on a
//! profile that resets `PATH`, `"$RMUX_BIN"` still works and the skill says so.

use std::path::{Path, PathBuf};

/// The environment every spawned session carries, in application order.
///
/// `PATH` is the daemon's own with the shim directory prepended; it is an
/// envp entry, never rendered into an argument list.
pub fn session_env(session: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("RMUX_ENV".to_owned(), "1".to_owned()),
        ("RMUX_SESSION".to_owned(), session.to_owned()),
    ];
    if let Some(bin) = std::env::current_exe().ok().and_then(|p| p.to_str().map(str::to_owned)) {
        env.push(("RMUX_BIN".to_owned(), bin));
    }
    // POSIX only: the shim is a shell script and `:` is the separator. The agent
    // does not host sessions on Windows, and a `:`-joined Path there would be one
    // no Windows process can parse (CLAUDE.md, `adopt_login_path`).
    if cfg!(unix)
        && let Some(dir) = bin_dir().and_then(|d| d.to_str().map(str::to_owned))
    {
        let path = std::env::var("PATH").unwrap_or_default();
        let joined = if path.is_empty() { dir } else { format!("{dir}:{path}") };
        env.push(("PATH".to_owned(), joined));
    }
    env
}

/// `~/.rmux/bin`, where installed agent builds and the `rmux` shim live.
pub fn bin_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".rmux").join("bin"))
}

/// Shell reserved words and openers an assignment prefix cannot precede.
///
/// `PATH=x if …` and `PATH=x (cmd)` are syntax errors; a line starting with one
/// of these is left untouched rather than broken.
const NOT_A_SIMPLE_COMMAND: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "time", "!", "{", "}", "[[", "]]", "coproc",
];

/// The first command word of a launch line, skipping leading `NAME=value`
/// assignments. `None` when an assignment is quoted (too clever to classify)
/// or the line is empty.
///
/// Used to decide what a session *is* (`claude`, `pi`) and whether the `PATH`
/// prefix is safe. Deliberately conservative: anything unusual reads as "don't
/// know", and "don't know" means the line is used verbatim.
pub fn first_command_word(line: &str) -> Option<&str> {
    for word in line.split_whitespace() {
        if is_plain_assignment(word) {
            continue;
        }
        if word.contains(['\'', '"', '\\', '`', '$']) && word.contains('=') {
            return None;
        }
        return Some(word);
    }
    None
}

fn is_plain_assignment(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else { return false };
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !value.contains(['\'', '"', '\\', '`', '$', '(', ')', ';', '&', '|', '<', '>'])
}

/// `line` with `PATH=<bin>:"$PATH"` prefixed, when that is certain to be safe.
///
/// `bin` is shell-quoted (it is an absolute path resolved here, never a
/// `$HOME` that quoting would freeze into a literal directory name). A line
/// whose first word is a reserved word or opener is returned unchanged.
pub fn with_rmux_path(line: &str, bin: &Path) -> String {
    let Some(first) = first_command_word(line) else { return line.to_owned() };
    if NOT_A_SIMPLE_COMMAND.contains(&first) || first.starts_with(['(', '{', '!']) {
        return line.to_owned();
    }
    let Some(bin) = bin.to_str() else { return line.to_owned() };
    format!("PATH={}:\"$PATH\" {line}", rmux_transport::shell_quote(bin))
}

/// `line` with `unset K…; ` in front and `exec` before its command, so the
/// session runs **without** `keys` even when the login shell's own profile
/// exports them.
///
/// A named credential set removes keys (a subscription-token profile removes
/// `ANTHROPIC_API_KEY`), and removing them from the spawn environment is not
/// enough: `-l -i` reads `.zshrc`/`.bashrc` *after* the daemon built the
/// environment, and an export there would come straight back. `exec` keeps the
/// pid — status is correlated on it. Key names only, never values. A line that
/// is not a simple command is returned unchanged.
pub fn with_unset(line: &str, keys: &[String]) -> String {
    let keys: Vec<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|k| {
            let mut c = k.chars();
            c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
        .collect();
    if keys.is_empty() {
        return line.to_owned();
    }
    let Some(first) = first_command_word(line) else { return line.to_owned() };
    if NOT_A_SIMPLE_COMMAND.contains(&first) || first.starts_with(['(', '{', '!']) || first == "exec" {
        return line.to_owned();
    }
    let at = first.as_ptr() as usize - line.as_ptr() as usize;
    format!("unset {}; {}exec {}", keys.join(" "), &line[..at], &line[at..])
}

/// The `rmux` shim: run the build that owns this session, or the newest one.
///
/// A **script**, not a symlink to one build: builds are pruned once their
/// daemon has gone, and a symlink to a pruned build is `rmux: not found` with
/// nothing to say why. `$RMUX_BIN` is preferred because a session lives in the
/// daemon of the build that created it; the fallback is the newest installed
/// build, skipping half-uploaded `*.partial*` files (provision `chmod`s them
/// executable before the rename).
pub const SHIM: &str = r#"#!/bin/sh
# rmux CLI shim. Written by rmux-agent; rewritten when it changes. Do not edit.
if [ -n "${RMUX_BIN:-}" ] && [ -x "$RMUX_BIN" ]; then
  exec "$RMUX_BIN" "$@"
fi
dir="$HOME/.rmux/bin"
for f in $(ls -t "$dir" 2>/dev/null); do
  case "$f" in
    *.partial*|*.tmp) continue ;;
    rmux-agent-*) [ -x "$dir/$f" ] && exec "$dir/$f" "$@" ;;
  esac
done
echo '{"error":{"code":"no_agent","message":"no rmux-agent build in ~/.rmux/bin and RMUX_BIN is unset"}}' >&2
exit 1
"#;

/// Make sure `~/.rmux/bin/rmux` is the current shim. Idempotent: an identical
/// file is not rewritten. Atomic: temp-then-rename, so a session starting
/// mid-write never runs half a script.
#[cfg(unix)]
pub fn ensure_shim() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = bin_dir().ok_or_else(|| std::io::Error::other("no home directory"))?;
    let path = dir.join("rmux");
    if std::fs::read_to_string(&path).is_ok_and(|s| s == SHIM) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    if let Some(rmux) = dir.parent() {
        let _ = std::fs::set_permissions(rmux, std::fs::Permissions::from_mode(0o700));
    }
    let temp = dir.join(format!(".rmux.{}.tmp", std::process::id()));
    std::fs::write(&temp, SHIM)?;
    std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&temp, &path)
}

/// No shim on Windows: the agent does not host sessions there (see CLAUDE.md,
/// "The agent does not run on Windows") and the shim is a POSIX script.
#[cfg(not(unix))]
pub fn ensure_shim() -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_word_skips_rendering_assignments() {
        let line = rmux_claude_core::launch_line(None, &[], rmux_claude_core::Rendering::Inline);
        assert_eq!(first_command_word(&line), Some("claude"));
        assert_eq!(first_command_word("RMUX_AGENT_BIN=/x/y pi --tui-mode regular"), Some("pi"));
        assert_eq!(first_command_word("A='x y' claude"), None);
        assert_eq!(first_command_word(""), None);
    }

    #[test]
    fn the_path_prefix_is_an_assignment_on_a_simple_command() {
        let bin = Path::new("/home/a.user/.rmux/bin");
        let line = with_rmux_path("CLAUDE_CODE_DISABLE_MOUSE=1 claude 'hi; there'", bin);
        assert_eq!(
            line,
            "PATH=/home/a.user/.rmux/bin:\"$PATH\" CLAUDE_CODE_DISABLE_MOUSE=1 claude 'hi; there'"
        );
        // A path with a space is quoted, and `$PATH` still expands.
        let spaced = with_rmux_path("claude", Path::new("/Users/x y/.rmux/bin"));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{} sh -c 'printf %s \"$PATH\"'", spaced.trim_end_matches("claude")))
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/Users/x y/.rmux/bin:/usr/bin:/bin");
    }

    #[test]
    fn a_compound_or_reserved_line_is_left_alone() {
        let bin = Path::new("/h/.rmux/bin");
        for line in ["if true; then claude; fi", "(claude)", "{ claude; }", "! claude"] {
            assert_eq!(with_rmux_path(line, bin), line, "{line}");
        }
    }

    #[test]
    fn the_prefix_keeps_the_pid_of_the_single_command() {
        // Status is correlated on the session's pid, so the prefixed line must
        // still be exec'd by the shell rather than forked. The inner command
        // reports its own pid; it must be the pid we spawned.
        let line = with_rmux_path("sh -c 'echo $$'", Path::new("/nonexistent"));
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&line)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            pid.to_string(),
            "the PATH prefix made the shell fork instead of exec: {line}"
        );
    }

    #[test]
    fn unset_removes_a_profile_export_and_keeps_the_pid() {
        // What a `.zshrc` export looks like by the time the line runs.
        let line = with_unset("A=1 sh -c 'echo $$:${ANTHROPIC_API_KEY:-gone}:$A'", &["ANTHROPIC_API_KEY".into()]);
        assert_eq!(line, "unset ANTHROPIC_API_KEY; A=1 exec sh -c 'echo $$:${ANTHROPIC_API_KEY:-gone}:$A'");
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&line)
            .env("ANTHROPIC_API_KEY", "leaked")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let out = child.wait_with_output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("{pid}:gone:1"));
        // Nothing to remove, an unsafe key, or a compound line: unchanged.
        assert_eq!(with_unset("claude", &[]), "claude");
        assert_eq!(with_unset("claude", &["A;rm".into()]), "claude");
        assert_eq!(with_unset("if x; then claude; fi", &["K".into()]), "if x; then claude; fi");
    }

    #[test]
    fn the_shim_prefers_rmux_bin_and_skips_partial_uploads() {
        let home = std::path::PathBuf::from(format!("/tmp/rmux-shim-{}", std::process::id()));
        let bin = home.join(".rmux/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join("rmux");
        std::fs::write(&shim, SHIM).unwrap();
        let fake = |name: &str, says: &str| {
            let p = bin.join(name);
            std::fs::write(&p, format!("#!/bin/sh\necho {says} \"$@\"\n")).unwrap();
            std::process::Command::new("chmod").arg("755").arg(&p).status().unwrap();
            p
        };
        let owner = fake("rmux-agent-0.1.0-aaaa", "owner");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fake("rmux-agent-0.1.0-bbbb.partial.123", "partial");

        let run = |rmux_bin: Option<&std::path::Path>| {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg(&shim).arg("list").env("HOME", &home).env_remove("RMUX_BIN");
            if let Some(b) = rmux_bin {
                cmd.env("RMUX_BIN", b);
            }
            String::from_utf8_lossy(&cmd.output().unwrap().stdout).trim().to_owned()
        };
        assert_eq!(run(Some(&owner)), "owner list");
        // RMUX_BIN pruned: newest real build, never the newer partial upload.
        assert_eq!(run(Some(std::path::Path::new("/gone/rmux-agent-x"))), "owner list");
        assert_eq!(run(None), "owner list");
        let _ = std::fs::remove_dir_all(&home);
    }
}
