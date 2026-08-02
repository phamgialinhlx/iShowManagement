//! Terminals.
//!
//! A terminal is a PTY on **this** machine running a [`ResolvedCommand`]. For a
//! local target that command is the user's shell; for an SSH target it is
//! `ssh -tt host -- "$SHELL" -l`. Either way the PTY, the emulator and the
//! rendering are local, and SSH is only the pipe the bytes travel through.
//!
//! That is deliberately not the obvious design. The tempting alternative is to
//! run the PTY on the remote host and tunnel keystrokes and screen updates
//! through an RPC, which is effectively what the previous generation did through
//! the relay server. It buys nothing — a terminal is already a byte stream, and
//! SSH is already a reliable ordered byte-stream transport — while adding a hop
//! to the most latency-sensitive path in the product and requiring the remote
//! side to stay in lockstep with the client. Running `ssh` in a local PTY deletes
//! that whole class of problem: keystroke latency becomes exactly `ssh`'s
//! latency, and local and remote terminals share one code path.
//!
//! Output fans out over a broadcast channel, and a bounded scrollback buffer lets
//! a reattaching view replay what it missed — so switching tabs or reloading the
//! window does not lose the session.

use std::io::{Read, Write};
use std::sync::Arc;

use bytes::Bytes;
use camino::Utf8Path;
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rmux_transport::ResolvedCommand;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// How much output to retain for replay on reattach.
///
/// A reattaching view needs enough context to look continuous, not the entire
/// history — xterm.js keeps its own scrollback once attached. 256 KiB is roughly
/// a few thousand lines of ordinary output.
const SCROLLBACK_BYTES: usize = 256 * 1024;

/// Read size. Large enough that a `cat` of a big file does not thrash, small
/// enough that an interactive keystroke echo is never delayed waiting for a full
/// buffer.
const READ_CHUNK: usize = 8 * 1024;

/// How many output chunks may queue for a slow subscriber before it is dropped.
const BROADCAST_CAPACITY: usize = 512;

pub type TerminalId = String;

/// Terminal dimensions in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

impl TermSize {
    fn to_pty(self) -> PtySize {
        PtySize {
            // A zero dimension makes some programs divide by zero when computing
            // layout, so clamp to at least one cell.
            rows: self.rows.max(1),
            cols: self.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Something that happened on a terminal.
#[derive(Clone, Debug)]
pub enum TerminalEvent {
    Output(Bytes),
    /// The child exited. No further output will arrive.
    Exited { code: i32 },
}

/// A bounded ring of recent output.
#[derive(Debug, Default)]
struct Scrollback {
    buf: Vec<u8>,
}

impl Scrollback {
    fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);

        if self.buf.len() > SCROLLBACK_BYTES {
            // Drop from the front. This can cut an escape sequence in half, which
            // would leave a replaying view briefly mis-styled — acceptable, and
            // far cheaper than parsing the stream to find a safe boundary. The
            // alternative (retaining everything) is an unbounded leak on a
            // terminal that has been streaming logs for hours.
            let excess = self.buf.len() - SCROLLBACK_BYTES;
            self.buf.drain(..excess);
        }
    }

    fn snapshot(&self) -> Bytes {
        Bytes::copy_from_slice(&self.buf)
    }
}

/// A running terminal.
pub struct Terminal {
    id: TerminalId,
    // Behind a mutex so `Terminal` is `Sync`. `MasterPty` is only `Send`, and the
    // app stores terminals in shared state reachable from many IPC handlers at
    // once — without this the whole registry would be single-threaded.
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    scrollback: Arc<Mutex<Scrollback>>,
    events: broadcast::Sender<TerminalEvent>,
    size: Mutex<TermSize>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal").field("id", &self.id).field("size", &*self.size.lock()).finish()
    }
}

impl Terminal {
    /// Spawn `command` in a new PTY.
    pub fn spawn(
        command: &ResolvedCommand,
        cwd: Option<&Utf8Path>,
        size: TermSize,
    ) -> anyhow::Result<Self> {
        let pair = native_pty_system().openpty(size.to_pty())?;

        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        for (key, value) in &command.env {
            builder.env(key, value);
        }
        if let Some(cwd) = cwd {
            builder.cwd(cwd.as_std_path());
        }

        let child = pair.slave.spawn_command(builder)?;
        // Close our handle to the slave side immediately. Holding it open means
        // the master never sees EOF when the child exits, so the reader thread
        // would block forever and the terminal would look alive after its shell
        // had gone.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        let scrollback = Arc::new(Mutex::new(Scrollback::default()));
        let child = Arc::new(Mutex::new(child));

        let terminal = Self {
            id: uuid::Uuid::new_v4().to_string(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Arc::clone(&child),
            scrollback: Arc::clone(&scrollback),
            events: events.clone(),
            size: Mutex::new(size),
        };

        spawn_reader(reader, events, scrollback, child, terminal.id.clone());

        Ok(terminal)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn size(&self) -> TermSize {
        *self.size.lock()
    }

    /// Subscribe to output without catching up. Prefer [`Terminal::attach`].
    pub fn subscribe(&self) -> broadcast::Receiver<TerminalEvent> {
        self.events.subscribe()
    }

    /// Recent output, for a view that is attaching or reattaching.
    pub fn replay(&self) -> Bytes {
        self.scrollback.lock().snapshot()
    }

    /// Catch up and subscribe **atomically**.
    ///
    /// Doing these separately is subtly wrong in both orders: snapshot-then-
    /// subscribe drops anything emitted in between, and subscribe-then-snapshot
    /// replays it twice. Either way a reattaching terminal shows corrupted
    /// output, and because the window is microseconds wide it reproduces roughly
    /// never in development and constantly under load.
    ///
    /// Taking the scrollback lock is what makes this atomic: the reader thread
    /// holds that same lock across *both* appending to the scrollback and
    /// broadcasting, so it can never be halfway between the two here.
    pub fn attach(&self) -> (Bytes, broadcast::Receiver<TerminalEvent>) {
        let scrollback = self.scrollback.lock();
        let receiver = self.events.subscribe();
        (scrollback.snapshot(), receiver)
    }

    /// Send input to the child.
    pub fn write(&self, data: &[u8]) -> anyhow::Result<()> {
        let mut writer = self.writer.lock();
        writer.write_all(data)?;
        // Flushed on every write: buffering here would add latency to exactly the
        // keystroke path this design exists to keep fast.
        writer.flush()?;
        Ok(())
    }

    /// Tell the child its window changed. This is what makes full-screen programs
    /// redraw correctly after the pane is resized.
    pub fn resize(&self, size: TermSize) -> anyhow::Result<()> {
        self.master.lock().resize(size.to_pty())?;
        *self.size.lock() = size;
        Ok(())
    }

    /// The child's process id on this machine.
    ///
    /// Exposed so a session can be correlated with `ps`. That matters because
    /// these processes deliberately outlive the app: when one is left behind,
    /// the pid is the only thing that ties what the daemon reports to what the
    /// operator sees on the host.
    ///
    /// `None` once the child has been reaped — `portable_pty` drops the id with
    /// the process, and reporting a stale pid would point at whatever the OS
    /// has since reused it for.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().process_id()
    }

    /// Terminate the child.
    pub fn kill(&self) -> anyhow::Result<()> {
        self.child.lock().kill()?;
        Ok(())
    }

    /// The child's exit code, if it has exited.
    pub fn exit_code(&self) -> Option<i32> {
        self.child
            .lock()
            .try_wait()
            .ok()
            .flatten()
            .map(|status| if status.success() { 0 } else { 1 })
    }
}

/// Pump the PTY into the broadcast channel.
///
/// The read is blocking, so this is a dedicated OS thread rather than a tokio
/// task — parking a runtime worker on a blocking read would starve unrelated
/// futures.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    events: broadcast::Sender<TerminalEvent>,
    scrollback: Arc<Mutex<Scrollback>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    id: TerminalId,
) {
    std::thread::Builder::new()
        .name(format!("rmux-pty-{id}"))
        .spawn(move || {
            let mut buf = vec![0u8; READ_CHUNK];

            loop {
                match reader.read(&mut buf) {
                    // EOF: the child closed the PTY.
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];

                        // The scrollback lock is held across BOTH the append and
                        // the broadcast. That is what lets `Terminal::attach`
                        // catch up and subscribe atomically — released early, a
                        // reattaching view could see this chunk twice or not at
                        // all. See the note on `attach`.
                        let mut scrollback = scrollback.lock();
                        scrollback.push(chunk);
                        // A send error only means nobody is currently watching.
                        // The terminal keeps running — that is the entire point
                        // of a session surviving a closed view.
                        let _ = events.send(TerminalEvent::Output(Bytes::copy_from_slice(chunk)));
                        drop(scrollback);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::debug!(terminal = %id, error = %e, "pty read ended");
                        break;
                    }
                }
            }

            // The PTY is closed, so the child is gone or going; `wait` will not
            // block meaningfully here.
            let code = child
                .lock()
                .wait()
                .map(|status| if status.success() { 0 } else { 1 })
                .unwrap_or(-1);

            let _ = events.send(TerminalEvent::Exited { code });
        })
        .expect("failed to spawn pty reader thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_transport::{CommandSpec, LocalTarget, Target};

    fn local(program: &str, args: &[&str]) -> ResolvedCommand {
        let spec = CommandSpec::new(program).args(args.to_vec());
        LocalTarget::new().build_command(&spec).unwrap()
    }

    /// Poll the scrollback until `needle` appears or time runs out.
    fn read_until(term: &Terminal, needle: &str, timeout: std::time::Duration) -> String {
        let deadline = std::time::Instant::now() + timeout;
        let mut seen = String::new();

        while std::time::Instant::now() < deadline {
            seen = String::from_utf8_lossy(&term.replay()).into_owned();
            if seen.contains(needle) {
                return seen;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        seen
    }

    #[test]
    fn a_command_runs_and_its_output_is_captured() {
        let term =
            Terminal::spawn(&local("sh", &["-c", "echo hello-from-pty"]), None, TermSize::default())
                .unwrap();

        let out = read_until(&term, "hello-from-pty", std::time::Duration::from_secs(5));
        assert!(out.contains("hello-from-pty"), "got: {out:?}");
    }

    #[test]
    fn input_reaches_the_child() {
        let term = Terminal::spawn(
            &local("sh", &["-c", "read line; echo GOT:$line"]),
            None,
            TermSize::default(),
        )
        .unwrap();

        term.write(b"ping\n").unwrap();

        let out = read_until(&term, "GOT:ping", std::time::Duration::from_secs(5));
        assert!(out.contains("GOT:ping"), "got: {out:?}");
    }

    #[test]
    fn the_child_sees_the_size_we_asked_for() {
        // Proves the PTY is a real terminal, not a pipe — `tput` reads the window
        // size via ioctl, which only works on a TTY.
        let term = Terminal::spawn(
            &local("sh", &["-c", "tput cols; sleep 0.2"]),
            None,
            TermSize { cols: 123, rows: 40 },
        )
        .unwrap();

        let out = read_until(&term, "123", std::time::Duration::from_secs(5));
        assert!(out.contains("123"), "expected the child to see 123 columns, got: {out:?}");
    }

    #[test]
    fn resize_is_reported_to_the_child() {
        let term = Terminal::spawn(
            // Print the new width whenever the window changes.
            &local("sh", &["-c", "trap 'tput cols' WINCH; sleep 2 & wait"]),
            None,
            TermSize { cols: 80, rows: 24 },
        )
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(300));
        term.resize(TermSize { cols: 111, rows: 30 }).unwrap();

        let out = read_until(&term, "111", std::time::Duration::from_secs(5));
        assert!(out.contains("111"), "expected a SIGWINCH redraw at 111 columns, got: {out:?}");
        assert_eq!(term.size(), TermSize { cols: 111, rows: 30 });
    }

    #[test]
    fn a_reattaching_view_replays_what_it_missed() {
        let term =
            Terminal::spawn(&local("sh", &["-c", "echo earlier-output"]), None, TermSize::default())
                .unwrap();

        read_until(&term, "earlier-output", std::time::Duration::from_secs(5));

        // Subscribing now — as a reopened tab would — must still be able to show
        // what already happened.
        let _late = term.subscribe();
        let replayed = String::from_utf8_lossy(&term.replay()).into_owned();
        assert!(replayed.contains("earlier-output"), "got: {replayed:?}");
    }

    /// Attaching mid-stream must lose nothing and duplicate nothing.
    ///
    /// The child emits numbered lines steadily; a view attaches partway through.
    /// Concatenating what it caught up on with what it then streamed must
    /// reproduce the sequence exactly — every line present, exactly once.
    ///
    /// **What this does and does not prove.** It confirms the handover is
    /// coherent, and the assertions genuinely bite: widening the snapshot-to-
    /// subscribe window to 50ms makes it fail with a dropped line. But the real
    /// non-atomic window is nanoseconds against output arriving every 10ms, so
    /// this test would almost never catch that narrow race by chance. The
    /// atomicity in `attach` is justified by construction — holding the lock
    /// across both the append and the broadcast — not by this test alone. Treat a
    /// pass as necessary, not sufficient.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn attaching_mid_stream_neither_drops_nor_duplicates_output() {
        const LINES: usize = 60;

        // Emitted slowly and steadily, so output is genuinely still arriving when
        // the view attaches. A tight loop finishes before the attach and the test
        // degrades into the trivial "everything was already in the scrollback"
        // case, which passes even against a broken `attach` — the assertions
        // below fail loudly if that ever starts happening again.
        let term = Terminal::spawn(
            &local(
                "sh",
                &["-c", &format!("i=1; while [ $i -le {LINES} ]; do echo L$i; sleep 0.01; i=$((i+1)); done")],
            ),
            None,
            TermSize::default(),
        )
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let (replayed, mut rx) = term.attach();

        let caught_up = String::from_utf8_lossy(&replayed).into_owned();
        let mut streamed = String::new();
        while let Ok(Ok(TerminalEvent::Output(chunk))) =
            tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv()).await
        {
            streamed.push_str(&String::from_utf8_lossy(&chunk));
        }

        // Both halves must have contributed, or the handover was never tested.
        assert!(!caught_up.is_empty(), "attached too late — nothing to catch up on");
        assert!(!streamed.is_empty(), "attached too late — no live output followed");

        let seen = format!("{caught_up}{streamed}");
        for i in 1..=LINES {
            let needle = format!("L{i}\r\n");
            let count = seen.matches(&needle).count();
            assert_eq!(
                count, 1,
                "line L{i} appeared {count} times (expected exactly once) — \
                 0 means the handover dropped it, 2 means it was replayed and streamed"
            );
        }
    }

    #[test]
    fn exit_is_reported_and_does_not_hang() {
        // Regression guard for holding the slave PTY open: if we did, the reader
        // would never see EOF and the exit would never be observed.
        let term =
            Terminal::spawn(&local("sh", &["-c", "exit 3"]), None, TermSize::default()).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while term.exit_code().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(term.exit_code().is_some(), "child exit was never observed");
    }

    #[test]
    fn scrollback_is_bounded() {
        let mut sb = Scrollback::default();
        for _ in 0..64 {
            sb.push(&vec![b'x'; 16 * 1024]);
        }
        // A terminal streaming logs for hours must not grow without limit.
        assert!(sb.snapshot().len() <= SCROLLBACK_BYTES, "scrollback grew past its cap");
    }

    #[test]
    fn zero_sized_terminals_are_clamped() {
        // A pane can measure 0x0 for a frame during layout; forwarding that makes
        // programs divide by zero while computing their own layout.
        let pty = TermSize { cols: 0, rows: 0 }.to_pty();
        assert_eq!((pty.cols, pty.rows), (1, 1));
    }
}
