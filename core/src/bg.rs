//! A long-lived background `ssh` process (port-forward `-L` or SOCKS `-D`).
//! It runs in a PTY (so password injection works), drains its output into a
//! small rolling buffer for error reporting, and tracks whether it has exited.
//! Dropping it kills the child (the PTY's `Drop`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use portable_pty::CommandBuilder;

use crate::pty::Pty;

pub struct BgSsh {
    _pty: Pty,
    exited: Arc<AtomicBool>,
    recent: Arc<Mutex<String>>,
}

impl BgSsh {
    pub fn spawn(cmd: CommandBuilder, password: Option<String>) -> Result<Self> {
        let (pty, mut rx) = Pty::spawn(cmd, 80, 24, password)?;
        let exited = Arc::new(AtomicBool::new(false));
        let recent = Arc::new(Mutex::new(String::new()));

        let exited_t = exited.clone();
        let recent_t = recent.clone();
        tokio::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                let mut buf = recent_t.lock().unwrap();
                buf.push_str(&String::from_utf8_lossy(&chunk));
                let overflow = buf.len().saturating_sub(4096);
                if overflow > 0 {
                    buf.drain(..overflow);
                }
            }
            // Channel closed = reader thread saw EOF = ssh exited.
            exited_t.store(true, Ordering::SeqCst);
        });

        Ok(Self { _pty: pty, exited, recent })
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    pub fn recent(&self) -> String {
        self.recent.lock().unwrap().trim().chars().take(300).collect()
    }
}
