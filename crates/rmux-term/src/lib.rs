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
//! Output fans out over bounded per-subscriber queues, and a bounded scrollback
//! buffer lets a reattaching view replay what it missed — so switching tabs or
//! reloading the window does not lose the session.
//!
//! ## Flow control, not drops
//!
//! The queues are bounded and the reader thread **blocks** when one is full,
//! which is the whole design: a consumer that cannot keep up stops the PTY from
//! being read, the PTY buffer fills, and the producer — a local program, or
//! `sshd` relaying a remote one through TCP's window — blocks on write exactly
//! as it would in any real terminal. Output is never dropped; the firehose is
//! made to wait instead. The previous broadcast-based fan-out dropped chunks
//! when a view fell behind and told the operator the screen was now unreliable,
//! which was honest but still a corrupted screen.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::Arc;

use bytes::Bytes;
use camino::Utf8Path;
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rmux_transport::ResolvedCommand;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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

/// How many chunks may sit unconsumed in one subscriber's queue before the
/// reader thread stops reading the PTY. At `READ_CHUNK` each this is ~512 KiB
/// of slack — deep enough that an interactive session never touches the limit,
/// shallow enough that a `cat` of a gigabyte is throttled to what the view can
/// actually render rather than buffered without bound.
const SUBSCRIBER_QUEUE: usize = 64;

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
///
/// Stored as the chunks the reader produced rather than one contiguous buffer:
/// a full contiguous `Vec` had to memmove ~248 KiB to the front for every
/// 8 KiB appended — ~31× write amplification on the hottest path in the app,
/// paid again on the daemon side of an agent session. Evicting whole chunks is
/// O(1), and the one place that needs contiguity — replay on attach — pays the
/// concatenation exactly once per reattach instead.
#[derive(Debug, Default)]
struct Scrollback {
    chunks: VecDeque<Bytes>,
    bytes: usize,
}

impl Scrollback {
    fn push(&mut self, chunk: Bytes) {
        self.bytes += chunk.len();
        self.chunks.push_back(chunk);

        // Evict from the front, whole chunks at a time. This can cut an escape
        // sequence at the boundary, which would leave a replaying view briefly
        // mis-styled — acceptable, and far cheaper than parsing the stream for
        // a safe split. The newest chunk is never evicted, so a single oversized
        // push cannot empty its own scrollback.
        while self.bytes > SCROLLBACK_BYTES && self.chunks.len() > 1 {
            let front = self.chunks.pop_front().expect("len > 1");
            self.bytes -= front.len();
        }
    }

    fn snapshot(&self) -> Bytes {
        let mut out = Vec::with_capacity(self.bytes);
        for chunk in &self.chunks {
            out.extend_from_slice(chunk);
        }
        out.into()
    }
}

/// Everything `attach` must see atomically, behind one lock.
///
/// The reader thread appends to the scrollback and delivers to every
/// subscriber while holding this lock; `attach` snapshots and subscribes under
/// the same lock. That single invariant is what makes the replay-then-stream
/// handover exact — see the note on [`Terminal::attach`].
#[derive(Default)]
struct Shared {
    scrollback: Scrollback,
    subscribers: Vec<mpsc::Sender<TerminalEvent>>,
    /// Set once the child has exited and every subscriber has been told. A
    /// subscription after that point gets the replay and a receiver that ends
    /// immediately, rather than one that waits forever on a dead terminal.
    finished: bool,
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
    shared: Arc<Mutex<Shared>>,
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

        let shared = Arc::new(Mutex::new(Shared::default()));
        let child = Arc::new(Mutex::new(child));

        let terminal = Self {
            id: uuid::Uuid::new_v4().to_string(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Arc::clone(&child),
            shared: Arc::clone(&shared),
            size: Mutex::new(size),
        };

        spawn_reader(reader, shared, child, terminal.id.clone());

        Ok(terminal)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn size(&self) -> TermSize {
        *self.size.lock()
    }

    /// Subscribe to output without catching up. Prefer [`Terminal::attach`].
    ///
    /// The receiver ends (`recv()` returns `None`) once the child has exited
    /// and its `Exited` event has been consumed.
    pub fn subscribe(&self) -> mpsc::Receiver<TerminalEvent> {
        self.attach().1
    }

    /// Recent output, for a view that is attaching or reattaching.
    pub fn replay(&self) -> Bytes {
        self.shared.lock().scrollback.snapshot()
    }

    /// Catch up and subscribe **atomically**.
    ///
    /// Doing these separately is subtly wrong in both orders: snapshot-then-
    /// subscribe drops anything emitted in between, and subscribe-then-snapshot
    /// replays it twice. Either way a reattaching terminal shows corrupted
    /// output, and because the window is microseconds wide it reproduces roughly
    /// never in development and constantly under load.
    ///
    /// Taking the shared lock is what makes this atomic: the reader thread
    /// holds that same lock across *both* appending to the scrollback and
    /// delivering to subscribers, so it can never be halfway between the two
    /// here. The flip side is that this call can wait — during a firehose with
    /// a stalled consumer the reader parks *holding the lock*, which is the
    /// flow control working as designed, not a hang.
    pub fn attach(&self) -> (Bytes, mpsc::Receiver<TerminalEvent>) {
        let mut shared = self.shared.lock();
        let (tx, rx) = mpsc::channel(SUBSCRIBER_QUEUE);
        if !shared.finished {
            shared.subscribers.push(tx);
        }
        // When `finished`, the sender is dropped here and `rx` ends immediately
        // after the replay — a view attaching to a dead terminal sees the last
        // screen and a stream that is over, not one that never speaks.
        (shared.scrollback.snapshot(), rx)
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

/// Pump the PTY into the subscriber queues.
///
/// The read is blocking, so this is a dedicated OS thread rather than a tokio
/// task — parking a runtime worker on a blocking read would starve unrelated
/// futures. Being an OS thread is also what lets `blocking_send` below park
/// legally when a subscriber's queue is full.
fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    shared: Arc<Mutex<Shared>>,
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
                        // One allocation per chunk, shared by the scrollback and
                        // every subscriber — `Bytes` clones are reference counts.
                        let chunk = Bytes::copy_from_slice(&buf[..n]);

                        // The shared lock is held across BOTH the append and the
                        // delivery. That is what lets `Terminal::attach` catch up
                        // and subscribe atomically — released early, a
                        // reattaching view could see this chunk twice or not at
                        // all. See the note on `attach`.
                        //
                        // `blocking_send` parks this thread when a queue is full.
                        // That stall *is* the flow control: the PTY stops being
                        // read, its buffer fills, and the producer blocks — for a
                        // remote program, through sshd and TCP's window — until
                        // the slow consumer catches up. Nothing is dropped. A
                        // subscriber whose receiver is gone is removed instead.
                        let mut shared = shared.lock();
                        shared.scrollback.push(chunk.clone());
                        shared.subscribers.retain(|tx| {
                            tx.blocking_send(TerminalEvent::Output(chunk.clone())).is_ok()
                        });
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

            // Tell everyone, then drop the senders: a drained receiver ends
            // rather than waiting forever on a terminal that will never speak.
            let mut shared = shared.lock();
            shared.finished = true;
            for tx in shared.subscribers.drain(..) {
                let _ = tx.blocking_send(TerminalEvent::Exited { code });
            }
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
        while let Ok(Some(TerminalEvent::Output(chunk))) =
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
            sb.push(Bytes::from(vec![b'x'; 16 * 1024]));
        }
        // A terminal streaming logs for hours must not grow without limit.
        assert!(sb.snapshot().len() <= SCROLLBACK_BYTES, "scrollback grew past its cap");
    }

    #[test]
    fn a_slow_subscriber_stalls_the_reader_instead_of_losing_output() {
        // Emit far more than one subscriber queue (64 chunks) can hold, while
        // the subscriber deliberately reads nothing. Under the old broadcast
        // fan-out this dropped chunks; now the reader must park and every byte
        // must still arrive once the subscriber starts draining.
        const LINES: usize = 2000;

        let term = Terminal::spawn(
            &local("sh", &["-c", &format!("i=1; while [ $i -le {LINES} ]; do echo LINE-$i; i=$((i+1)); done")]),
            None,
            TermSize::default(),
        )
        .unwrap();

        let (replayed, mut rx) = term.attach();

        // Let the child race ahead into a full queue before draining anything.
        std::thread::sleep(std::time::Duration::from_millis(300));

        let mut seen = String::from_utf8_lossy(&replayed).into_owned();
        while let Some(event) = rx.blocking_recv() {
            match event {
                TerminalEvent::Output(chunk) => seen.push_str(&String::from_utf8_lossy(&chunk)),
                TerminalEvent::Exited { .. } => break,
            }
        }

        for i in [1, LINES / 2, LINES] {
            let needle = format!("LINE-{i}\r\n");
            assert!(seen.contains(&needle), "missing {needle:?} — output was dropped");
        }
    }

    #[test]
    fn attaching_after_exit_ends_the_stream_rather_than_hanging() {
        let term =
            Terminal::spawn(&local("sh", &["-c", "echo done"]), None, TermSize::default()).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while term.exit_code().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Give the reader thread a beat to observe EOF and finish the registry.
        std::thread::sleep(std::time::Duration::from_millis(200));

        let (replayed, mut rx) = term.attach();
        assert!(String::from_utf8_lossy(&replayed).contains("done"));
        // The receiver must end, not park a caller forever on a dead terminal.
        assert!(rx.blocking_recv().is_none());
    }

    #[test]
    fn zero_sized_terminals_are_clamped() {
        // A pane can measure 0x0 for a frame during layout; forwarding that makes
        // programs divide by zero while computing their own layout.
        let pty = TermSize { cols: 0, rows: 0 }.to_pty();
        assert_eq!((pty.cols, pty.rows), (1, 1));
    }
}
