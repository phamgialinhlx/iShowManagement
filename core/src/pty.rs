//! PTY plumbing (Phase 1 local shell + Phase 2 ssh with password injection).
//!
//! `portable-pty` is blocking, so each session gets two OS threads: one reading
//! PTY output into a `tokio::mpsc` the async WS task drains, and one writing
//! queued input into the PTY. The master handle stays on the struct for resize.
//! When a password is supplied, the reader thread also watches for a
//! password/passphrase prompt and injects it **once**, then stops scanning
//! (MOTD-safe). Mirrors `references/tsmanager/server/ssh.js` attachPasswordInjection.

use std::io::{Read, Write};
use std::sync::mpsc::Sender as StdSender;
use std::sync::OnceLock;

use anyhow::Result;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use regex::Regex;
use tokio::sync::mpsc;

/// Matches an ssh password/passphrase prompt at the end of the current output.
fn prompt_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(password|passphrase).*:\s*$").expect("valid regex"))
}

/// A running PTY with a shell or ssh attached.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    input: StdSender<Vec<u8>>,
    child: Box<dyn Child + Send + Sync>,
}

impl Pty {
    /// Spawn the user's login shell (`$SHELL`, fallback `/bin/bash`) in a PTY.
    pub fn spawn_shell(cols: u16, rows: u16) -> Result<(Self, mpsc::Receiver<Vec<u8>>)> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        if let Ok(home) = std::env::var("HOME") {
            cmd.cwd(home);
        }
        Self::spawn(cmd, cols, rows, None)
    }

    /// Spawn an arbitrary command in a PTY. If `password` is set, inject it once
    /// when a password/passphrase prompt is seen.
    pub fn spawn(
        cmd: CommandBuilder,
        cols: u16,
        rows: u16,
        password: Option<String>,
    ) -> Result<(Self, mpsc::Receiver<Vec<u8>>)> {
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system().openpty(size)?;
        let child = pair.slave.spawn_command(cmd)?;
        // Drop the slave so the PTY reaches EOF when the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;

        // Input: queued bytes → blocking writer.
        let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(data) = in_rx.recv() {
                if writer.write_all(&data).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        // Output: blocking reads → tokio channel the WS task drains. Optionally
        // scans for a password prompt and injects once via the input channel.
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(256);
        let inject_tx = in_tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut injected = password.is_none();
            let mut scan = String::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // EOF or error → session gone
                    Ok(n) => {
                        let chunk = &buf[..n];
                        if !injected {
                            scan.push_str(&String::from_utf8_lossy(chunk));
                            if prompt_re().is_match(&scan) {
                                if let Some(pw) = &password {
                                    let mut line = pw.clone().into_bytes();
                                    line.push(b'\n');
                                    let _ = inject_tx.send(line);
                                }
                                injected = true;
                                scan.clear();
                            } else if scan.len() > 4096 {
                                // Keep the scan window bounded to recent output.
                                scan.drain(..scan.len() - 1024);
                            }
                        }
                        if out_tx.blocking_send(chunk.to_vec()).is_err() {
                            break; // receiver dropped (WS closed)
                        }
                    }
                }
            }
        });

        Ok((
            Self {
                master: pair.master,
                input: in_tx,
                child,
            },
            out_rx,
        ))
    }

    /// Queue bytes to write to the PTY (keystrokes). Non-blocking.
    pub fn write(&self, data: Vec<u8>) {
        let _ = self.input.send(data);
    }

    /// Resize the PTY window.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // WS closed → tear the child down instead of orphaning a shell/ssh.
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_regex_matches_ssh_prompts() {
        let re = prompt_re();
        assert!(re.is_match("alice@host's password: "));
        assert!(re.is_match("Password:"));
        assert!(re.is_match("Enter passphrase for key '/home/a/.ssh/id_rsa': "));
        // Not a prompt: the word appears mid-MOTD, not at a trailing colon.
        assert!(!re.is_match("Your password was changed yesterday.\n$ "));
    }
}
