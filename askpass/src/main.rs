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

    let request = serde_json::json!({ "token": token, "prompt": prompt });

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
        Some(answer) => {
            // OpenSSH takes the first line of stdout as the secret.
            println!("{answer}");
            ExitCode::SUCCESS
        }
        // The user dismissed the dialog. Non-zero tells ssh to give up rather
        // than retrying with an empty password.
        None => ExitCode::FAILURE,
    }
}

/// Windows has no Unix sockets, and rmux uses an in-process SSH client there
/// rather than driving the `ssh` binary, so this helper is never invoked.
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!("rmux-askpass: not supported on this platform");
    ExitCode::FAILURE
}
