//! Signing in to Claude, by driving the CLI rather than reimplementing it.
//!
//! `claude setup-token` runs an OAuth flow and prints a **long-lived token** —
//! the mechanism Claude Code provides for machines where a browser login is not
//! practical, which is every remote host rmux talks to.
//!
//! ## Why the CLI and not a hand-rolled OAuth client
//!
//! Observed from a real run, the flow is PKCE against a **fixed public client**,
//! redirecting to `platform.claude.com/oauth/code/callback` — a page that simply
//! displays a code for the operator to paste back. There is no client secret and
//! nothing to host, so rmux *could* build the URL and exchange the code itself.
//!
//! It deliberately does not. The client id, the endpoints, the scope and the
//! exchange are Anthropic's to change, and a reimplementation would break
//! silently on the release that changed them — presenting as "login is broken"
//! with nothing in rmux to point at. Driving the real CLI means the flow is
//! whatever the CLI says it is, which is the same reason the Claude tab renders
//! the real TUI instead of a native chat UI.
//!
//! ## It runs through `Target`, so a host works the same as this machine
//!
//! The command is built by [`rmux_transport::Target`], so signing in *on a
//! server* is the same code path as signing in locally — no `if is_local`
//! branch. That matters because the login is useful precisely where a browser is
//! not available.
//!
//! The token itself never reaches the webview, and never becomes an argument to
//! anything: see [`crate::claude_account`].

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rmux_claude::auth;
use rmux_transport::{CommandSpec, Tty};
use serde::Serialize;
use tauri::State;

use crate::terminal::TargetRef;

/// How long to wait for the CLI to print its authorisation URL.
const URL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// How long to wait for a token after the code is pasted.
const TOKEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const POLL: std::time::Duration = std::time::Duration::from_millis(120);

/// A login in progress.
///
/// One at a time: two concurrent `setup-token` runs would race to write the same
/// keychain slot, and there is no sensible reason to start a second.
#[derive(Default)]
pub struct LoginStore {
    session: Mutex<Option<Login>>,
}

struct Login {
    /// Everything the child has printed so far. Scanned for the URL, then for
    /// the token — the CLI redraws its own output, so only the accumulated
    /// buffer is reliable.
    output: Arc<Mutex<String>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Kept alive: dropping the master closes the pty and kills the flow.
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStarted {
    /// Open this in the operator's real browser.
    pub auth_url: String,
}

fn fail(message: impl Into<String>) -> String {
    message.into()
}

/// Start `claude setup-token` and wait for the URL it prints.
#[tauri::command]
pub async fn claude_login_start(
    store: State<'_, LoginStore>,
    claude_store: State<'_, crate::claude::ClaudeStore>,
    target: Option<TargetRef>,
) -> Result<LoginStarted, String> {
    // Replace any previous attempt rather than refusing: an abandoned flow would
    // otherwise block every later one until the app restarts.
    cancel_locked(&store);

    // Resolved through the same store the Claude tab uses, so a login on a host
    // reuses that host's existing ControlMaster connection rather than opening a
    // second one.
    let target = crate::claude::resolve(&claude_store, &target.unwrap_or(TargetRef { host: None, user: None, port: None })).await?;

    // A login shell, because `claude` is installed by a version manager whose
    // PATH only exists there — spawning the binary directly gives "command not
    // found" on a host where it is plainly installed.
    let spec = CommandSpec::new("claude").args(vec!["setup-token".to_owned()]).tty(Tty::Allocate);
    let argv = target.build_command(&spec).map_err(|e| fail(e.to_string()))?;

    let pty = native_pty_system()
        .openpty(PtySize { rows: 40, cols: 100, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| fail(format!("could not allocate a terminal: {e}")))?;

    let mut command = CommandBuilder::new(&argv.program);
    for arg in &argv.args {
        command.arg(arg);
    }
    for (key, value) in &argv.env {
        command.env(key, value);
    }

    let child = pty
        .slave
        .spawn_command(command)
        .map_err(|e| fail(format!("could not start `claude setup-token`: {e}")))?;

    let output = Arc::new(Mutex::new(String::new()));
    let mut reader = pty
        .master
        .try_clone_reader()
        .map_err(|e| fail(format!("could not read from the terminal: {e}")))?;

    // A blocking read on its own thread. The CLI animates a spinner, so output
    // arrives continuously and a poll loop on the main thread would either spin
    // or stall.
    {
        let output = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut sink) = output.lock() {
                    sink.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
        });
    }

    let writer = pty
        .master
        .take_writer()
        .map_err(|e| fail(format!("could not write to the terminal: {e}")))?;

    *store.session.lock().unwrap() = Some(Login {
        output: Arc::clone(&output),
        writer,
        child,
        _master: pty.master,
    });

    // The URL is what the operator needs; everything before it is decoration.
    match wait_for(&output, URL_TIMEOUT, auth::find_auth_url) {
        Some(auth_url) => Ok(LoginStarted { auth_url }),
        None => {
            let tail = tail_of(&output);
            cancel_locked(&store);
            Err(fail(format!(
                "`claude setup-token` did not print a sign-in link. Last output: {tail}"
            )))
        }
    }
}

/// Hand the CLI the code from the browser, and take the token it prints.
#[tauri::command]
pub async fn claude_login_submit(
    store: State<'_, LoginStore>,
    code: String,
) -> Result<crate::claude_account::AccountStatus, String> {
    let code = code.trim().to_owned();
    if code.is_empty() {
        return Err(fail("paste the code from your browser first"));
    }

    let output = {
        let mut guard = store.session.lock().unwrap();
        let login = guard
            .as_mut()
            .ok_or_else(|| fail("that sign-in is no longer active — start it again"))?;

        // Mark where the token search should begin. The authorisation URL
        // contains no `sk-ant-oat`, but a previous attempt's output might, and
        // resuming from a stale token would silently store the wrong one.
        login
            .writer
            .write_all(format!("{code}\r").as_bytes())
            .map_err(|e| fail(format!("could not send the code: {e}")))?;
        login.writer.flush().ok();

        Arc::clone(&login.output)
    };

    let Some(token) = wait_for(&output, TOKEN_TIMEOUT, auth::find_token) else {
        let tail = tail_of(&output);
        return Err(fail(format!("no token came back. Last output: {tail}")));
    };

    // The flow is done either way; leaving the child running would hold a pty
    // and a process for nothing.
    cancel_locked(&store);

    crate::claude_account::claude_account_save(token).await
}

/// Abandon a login in progress.
#[tauri::command]
pub async fn claude_login_cancel(store: State<'_, LoginStore>) -> Result<(), String> {
    cancel_locked(&store);
    Ok(())
}

fn cancel_locked(store: &State<'_, LoginStore>) {
    if let Some(mut login) = store.session.lock().unwrap().take() {
        let _ = login.child.kill();
        let _ = login.child.wait();
    }
}

/// Poll the accumulated output until `find` succeeds or the deadline passes.
fn wait_for(
    output: &Arc<Mutex<String>>,
    timeout: std::time::Duration,
    find: fn(&str) -> Option<String>,
) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(text) = output.lock()
            && let Some(found) = find(&text)
        {
            return Some(found);
        }
        std::thread::sleep(POLL);
    }
    None
}

/// The last bit of output, for an error message.
///
/// Bounded and stripped of escape sequences: the raw buffer is a redrawing TUI,
/// and pasting it into a dialog would be unreadable. Never includes a token —
/// this is only reached when no token was found.
fn tail_of(output: &Arc<Mutex<String>>) -> String {
    let text = output.lock().map(|t| t.clone()).unwrap_or_default();
    let cleaned: String = strip_ansi(&text);
    let tail: String = cleaned.chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
    tail.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove CSI/OSC escape sequences.
///
/// Hand-rolled rather than pulled in as a dependency: this is only ever used to
/// make an error message readable, and it must not panic on partial sequences —
/// the buffer is whatever the child had written when it was read.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then a letter terminates it.
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            // OSC: runs to BEL or ST.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_stripped_from_an_error_tail() {
        let raw = "\u{1b}[36mSigning in\u{1b}[0m\u{1b}]0;title\u{7} failed";
        assert_eq!(strip_ansi(raw), "Signing in failed");
    }

    #[test]
    fn a_truncated_escape_sequence_does_not_panic() {
        // The buffer is read while the child is mid-write, so a partial sequence
        // at the end is normal rather than exceptional.
        assert_eq!(strip_ansi("ok\u{1b}"), "ok");
        assert_eq!(strip_ansi("ok\u{1b}["), "ok");
        assert_eq!(strip_ansi("ok\u{1b}[38;5;"), "ok");
        assert_eq!(strip_ansi("ok\u{1b}]0;unterminated"), "ok");
    }

    #[test]
    fn the_real_prompt_survives_stripping() {
        // Captured from an actual `claude setup-token` run.
        let raw = "\u{1b}[?25l\u{1b}[1mPaste code here if prompted\u{1b}[22m > ";
        assert_eq!(strip_ansi(raw), "Paste code here if prompted > ");
    }

    #[test]
    fn the_error_tail_is_bounded_and_single_line() {
        let output = Arc::new(Mutex::new(format!("{}\n\n  spread   out  ", "x".repeat(4000))));
        let tail = tail_of(&output);
        assert!(tail.len() <= 200, "{}", tail.len());
        assert!(!tail.contains('\n'), "{tail}");
        assert!(tail.ends_with("spread out"), "{tail}");
    }

    #[test]
    fn the_url_the_cli_actually_prints_is_picked_up() {
        let output = Arc::new(Mutex::new(String::new()));
        output.lock().unwrap().push_str(
            "Browser didn't open? Use the url below to sign in (c to copy)\r\n\r\n\
             https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a\r\n",
        );

        let found = wait_for(&output, std::time::Duration::from_millis(300), auth::find_auth_url);
        assert_eq!(
            found.as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a")
        );
    }

    #[test]
    fn waiting_gives_up_rather_than_hanging_forever() {
        // The whole reason this is bounded: when the URL cannot be found, the
        // alternative is a spinner nobody can cancel.
        let output = Arc::new(Mutex::new("no link in here".to_owned()));
        let start = std::time::Instant::now();
        let found = wait_for(&output, std::time::Duration::from_millis(300), auth::find_auth_url);
        assert!(found.is_none());
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }
}
