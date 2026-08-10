//! The `SSH_ASKPASS` helper.
//!
//! OpenSSH runs this program whenever it needs a secret — a password, a key
//! passphrase, a 2FA code, or a host-key confirmation. The prompt arrives as
//! `argv[1]`; whatever we print on stdout becomes the answer, and exiting
//! non-zero aborts the authentication.
//!
//! This binary deliberately does nothing clever: it forwards the prompt to the
//! running rmux instance over a Unix socket and relays the reply. It is spawned
//! once per prompt, so it stays tiny and dependency-light.
//!
//! It is not a general-purpose askpass. It answers only to the rmux process that
//! set `RMUX_ASKPASS_SOCKET` and `RMUX_ASKPASS_TOKEN`; without a matching token
//! the server hangs up. Otherwise any local process could ask rmux to pop a
//! credential dialog and read back what the user typed.

use std::process::ExitCode;

/// Hand the secret to `ssh`, and **survive a pipe that has already closed**.
///
/// This was `println!`, and that is a crash. `println!` panics when stdout is a
/// broken pipe, and the workspace sets `panic = "abort"` (`Cargo.toml`) — so an
/// EPIPE became **SIGABRT** rather than an error return.
///
/// The window is not exotic: `ssh` goes away while the dialog is still open
/// whenever the app quits mid-prompt, a ControlMaster is torn down, or a
/// credential is refused. The helper then died without delivering anything, the
/// connection failed, and rmux asked again — which the operator experiences as a
/// **password prompt that will not stop coming back**, with nothing anywhere
/// naming a broken pipe.
///
/// Writing to the raw descriptor rather than through `Stdout` matters too: the
/// buffered writer defers the failure to a flush that happens after this
/// function has already reported success, so the error would arrive with nobody
/// left to return it to.
fn deliver(answer: &str) -> ExitCode {
    // OpenSSH reads the first line of stdout as the secret.
    let mut bytes = answer.as_bytes().to_vec();
    bytes.push(b'\n');

    match write_stdout(&bytes) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rmux-askpass: could not deliver the answer: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn write_stdout(bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::io::FromRawFd;

    // Borrowed, never owned: `File` closes on drop, and closing fd 1 out from
    // under the process would break any later write to stdout.
    let mut out = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
    out.write_all(bytes)
}

#[cfg(not(unix))]
fn write_stdout(bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // A console handle returns ERROR_BROKEN_PIPE as an `Err` rather than raising
    // SIGPIPE, so the ordinary handle is enough here.
    let mut out = std::io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}

#[cfg(unix)]
fn main() -> ExitCode {
    // Imported here rather than at the top: on Windows the whole body below is
    // compiled out, and top-level imports it no longer uses are warnings.
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    // OpenSSH passes the prompt as a single argument; joining is harmless and
    // tolerates builds that split it.
    let prompt: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");

    let (Ok(socket), Ok(token)) =
        (std::env::var("RMUX_ASKPASS_SOCKET"), std::env::var("RMUX_ASKPASS_TOKEN"))
    else {
        // Not launched by rmux. Refuse rather than prompting on a terminal that
        // may not exist.
        eprintln!("rmux-askpass: not invoked by rmux");
        return ExitCode::FAILURE;
    };

    let Ok(stream) = UnixStream::connect(&socket) else {
        eprintln!("rmux-askpass: rmux is not listening on {socket}");
        return ExitCode::FAILURE;
    };

    // Generous: someone may need a moment to reach for a hardware token. Still
    // bounded, so a wedged rmux cannot hang an ssh process forever.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));

    // `attempt` names the `ssh` process we were spawned by, so rmux can tell a
    // credential it just refused apart from another connection asking at the
    // same moment. Absent is fine — it only costs that distinction.
    let request = serde_json::json!({
        "token": token,
        "prompt": prompt,
        "attempt": std::env::var("RMUX_ASKPASS_ATTEMPT").ok(),
    });

    let mut writer = &stream;
    if writeln!(writer, "{request}").and_then(|()| writer.flush()).is_err() {
        eprintln!("rmux-askpass: failed to send the prompt");
        return ExitCode::FAILURE;
    }

    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() || line.trim().is_empty() {
        eprintln!("rmux-askpass: no answer from rmux");
        return ExitCode::FAILURE;
    }

    let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
        eprintln!("rmux-askpass: malformed answer from rmux");
        return ExitCode::FAILURE;
    };

    match response.get("answer").and_then(|a| a.as_str()) {
        Some(answer) => deliver(answer),
        // The user dismissed the dialog. Non-zero tells ssh to give up rather
        // than retrying with an empty password.
        None => ExitCode::FAILURE,
    }
}

/// The same helper, over a named pipe.
///
/// Windows has no Unix sockets, but it does drive the `ssh` binary exactly like
/// every other platform — the comment here used to claim otherwise, and that
/// mistaken belief is why this returned failure for so long. A password host was
/// therefore unusable on Windows: `ssh` has no terminal to ask on and no helper
/// to ask through, so it spends its retries and reports `Permission denied`.
///
/// A pipe client is an ordinary file handle, so the protocol below is the Unix
/// one line for line. The differences are both about pipes: an instance may be
/// momentarily busy, and a handle carries no read timeout.
#[cfg(windows)]
fn main() -> ExitCode {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};
    use std::time::Duration;

    let prompt: String = std::env::args().skip(1).collect::<Vec<_>>().join(" ");

    let (Ok(socket), Ok(token)) =
        (std::env::var("RMUX_ASKPASS_SOCKET"), std::env::var("RMUX_ASKPASS_TOKEN"))
    else {
        eprintln!("rmux-askpass: not invoked by rmux");
        return ExitCode::FAILURE;
    };

    // A `File` has no read timeout, so the bound lives here instead: generous
    // enough to reach for a hardware token, but bounded, so a wedged rmux cannot
    // hang an `ssh` process — and through it a terminal — forever. Matches the
    // 300s the Unix path sets on the socket.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(300));
        eprintln!("rmux-askpass: rmux did not answer");
        std::process::exit(1);
    });

    /// `ERROR_PIPE_BUSY` — every instance is occupied this instant.
    const PIPE_BUSY: i32 = 231;

    // The server opens a fresh instance as soon as one is taken, so busy is a
    // momentary race rather than a queue. Retrying briefly is the documented way
    // to open a pipe; failing here would surface as a refused password.
    let mut pipe = None;
    for _ in 0..50 {
        match OpenOptions::new().read(true).write(true).open(&socket) {
            Ok(handle) => {
                pipe = Some(handle);
                break;
            }
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                eprintln!("rmux-askpass: rmux is not listening on {socket}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(mut pipe) = pipe else {
        eprintln!("rmux-askpass: rmux never freed a pipe instance");
        return ExitCode::FAILURE;
    };

    // `attempt` names the `ssh` process we were spawned by, so rmux can tell a
    // credential it just refused apart from another connection asking at the
    // same moment. Absent is fine — it only costs that distinction.
    let request = serde_json::json!({
        "token": token,
        "prompt": prompt,
        "attempt": std::env::var("RMUX_ASKPASS_ATTEMPT").ok(),
    });
    if writeln!(pipe, "{request}").and_then(|()| pipe.flush()).is_err() {
        eprintln!("rmux-askpass: failed to send the prompt");
        return ExitCode::FAILURE;
    }

    let mut line = String::new();
    if BufReader::new(&mut pipe).read_line(&mut line).is_err() || line.trim().is_empty() {
        eprintln!("rmux-askpass: no answer from rmux");
        return ExitCode::FAILURE;
    }

    let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
        eprintln!("rmux-askpass: malformed answer from rmux");
        return ExitCode::FAILURE;
    };

    match response.get("answer").and_then(|a| a.as_str()) {
        Some(answer) => deliver(answer),
        None => ExitCode::FAILURE,
    }
}

/// Anything that is neither Unix nor Windows has no transport here.
#[cfg(not(any(unix, windows)))]
fn main() -> ExitCode {
    eprintln!("rmux-askpass: not supported on this platform");
    ExitCode::FAILURE
}
