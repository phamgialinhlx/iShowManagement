//! `rmux-agent fs-serve` as a real process: the binary the host runs.

use std::process::Stdio;
use std::time::Duration;

use rmux_fs::wire::{self, Envelope, Frame, ReadReply, Request, ServerMsg, WirePath};

#[tokio::test]
async fn fs_serve_answers_over_stdio_and_exits_when_stdin_closes() {
    let dir = std::env::temp_dir().join(format!("rmux-fs-serve-bin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("hello.txt");
    std::fs::write(&file, b"hi\0there").unwrap();

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmux-agent"))
        .arg("fs-serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    let hello = wire::read_frame(&mut stdout).await.unwrap().expect("hello");
    assert!(matches!(serde_json::from_value(hello.header).unwrap(), ServerMsg::Hello { protocol: wire::PROTOCOL, .. }));

    let request = Envelope { id: 9, request: Request::Read { path: WirePath::from_path(&file), max: None } };
    wire::write_frame(&mut stdin, &Frame::new(serde_json::to_value(request).unwrap())).await.unwrap();
    let reply = wire::read_frame(&mut stdout).await.unwrap().expect("reply");
    match serde_json::from_value(reply.header).unwrap() {
        ServerMsg::Ok { id, result } => {
            assert_eq!(id, 9);
            let meta: ReadReply = serde_json::from_value(result).unwrap();
            assert_eq!(meta.size, 8);
            assert!(meta.binary);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(reply.payload, b"hi\0there", "the bytes are the payload, raw");

    // An unknown request is answered, not ignored — a client must never hang.
    let unknown = serde_json::json!({"id": 10, "op": "teleport"});
    wire::write_frame(&mut stdin, &Frame::new(unknown)).await.unwrap();
    let reply = wire::read_frame(&mut stdout).await.unwrap().expect("reply to unknown");
    assert!(matches!(serde_json::from_value(reply.header).unwrap(), ServerMsg::Err { id: 10, .. }));

    // Open a worktree so a watcher thread is running, then hang up: the process
    // must still exit — a watcher must not keep an unreachable server alive.
    let open = Envelope { id: 11, request: Request::OpenWorktree { root: WirePath::from_path(&dir), limit: None } };
    wire::write_frame(&mut stdin, &Frame::new(serde_json::to_value(open).unwrap())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("fs-serve must exit when stdin closes")
        .unwrap();
    assert!(status.success(), "{status:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Live, local: the real binary against **this repository**, through the
/// client the app uses. Prints the snapshot size and time and a watched edit's
/// round trip. Ignored by default — it measures, it does not gate.
///
///     cargo test -p rmux-agent --test fs_serve -- --ignored --nocapture
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn live_local_snapshot_and_watch_round_trip_on_this_repo() {
    use rmux_fs::client::Conn;
    use rmux_fs::wire::{Change, Opened};

    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rmux-agent"))
        .arg("fs-serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<(std::time::Instant, ServerMsg)>();
    let tx = parking_lot::Mutex::new(tx);
    let conn = Conn::start(child.stdout.take().unwrap(), child.stdin.take().unwrap(), Duration::from_secs(5), move |m| {
        let _ = tx.lock().send((std::time::Instant::now(), m));
    })
    .await
    .unwrap();

    let started = std::time::Instant::now();
    let opened: Opened = conn.call_as(Request::OpenWorktree { root: WirePath::from_path(&repo), limit: None }).await.unwrap();
    let mut entries = 0usize;
    let mut first_chunk = None;
    loop {
        match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
            (at, ServerMsg::Entries { wt, entries: chunk }) if wt == opened.wt => {
                first_chunk.get_or_insert(at - started);
                entries += chunk.len();
            }
            (at, ServerMsg::Scanned { count, truncated, .. }) => {
                eprintln!(
                    "LOCAL snapshot of {}: {entries} entries ({count} counted, truncated={truncated}) in {:?}; first chunk after {:?}",
                    repo.display(),
                    at - started,
                    first_chunk
                );
                break;
            }
            _ => {}
        }
    }

    let probe = repo.join("crates/rmux-fs").join(format!(".fs-serve-probe-{}", std::process::id()));
    let wrote = std::time::Instant::now();
    std::fs::write(&probe, "probe").unwrap();
    let rel = format!("crates/rmux-fs/.fs-serve-probe-{}", std::process::id());
    loop {
        let (at, msg) = rx.recv_timeout(Duration::from_secs(10)).expect("the edit was never reported");
        if let ServerMsg::Changes { changes, .. } = msg
            && changes.iter().any(|c| matches!(c, Change::Upsert { entry } if entry.path.as_bytes() == rel.as_bytes()))
        {
            eprintln!("LOCAL watched edit round trip: {:?}", at - wrote);
            break;
        }
    }
    std::fs::remove_file(&probe).unwrap();
}
