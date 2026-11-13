//! Uploads of any size stream from disk through the same noclobber script.
//!
//! Driven through `TargetFs<LocalTarget>`, so the real `sh` runs the real
//! upload script and the bytes really cross a pipe — the path an SSH upload
//! takes, minus the network. 70 MB is past the old 64 MB cap on purpose.

use rmux_fs::{FileSystem, LocalFs, TargetFs, Transfer};
use rmux_transport::LocalTarget;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rmux-upload-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Arbitrary bytes, including NUL and invalid UTF-8, without a rand dependency.
fn payload(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

async fn round_trip(fs: &dyn FileSystem, dir: &std::path::Path) {
    let bytes = payload(70 * 1024 * 1024 + 123);
    let local = dir.join("source.bin");
    std::fs::write(&local, &bytes).unwrap();
    let dest = dir.join("dest.bin");
    let dest = dest.to_str().unwrap();

    let sent = Transfer::default();
    fs.upload_file(dest, &local, &sent).await.unwrap();
    assert!(std::fs::read(dest).unwrap() == bytes, "the uploaded file differs from the source");
    assert_eq!(sent.done(), bytes.len() as u64, "progress did not count every byte");
    assert_eq!(sent.total(), Some(bytes.len() as u64));

    // A second upload onto the same name is refused, and leaves the first intact.
    let err = fs.upload_file(dest, &local, &Transfer::default()).await.unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(std::fs::metadata(dest).unwrap().len(), bytes.len() as u64);
}

#[tokio::test]
async fn a_large_file_streams_through_the_shell_upload_script() {
    let dir = scratch("target");
    round_trip(&TargetFs::new(LocalTarget), &dir).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_large_file_streams_into_a_local_folder() {
    let dir = scratch("local");
    round_trip(&LocalFs, &dir).await;
    let _ = std::fs::remove_dir_all(dir);
}

/// A cancelled upload is removed, never left behind looking complete — on the
/// shell path by the far script's length check after our end of the pipe closes.
async fn cancelled(fs: &dyn FileSystem, dir: &std::path::Path) {
    let local = dir.join("big.bin");
    std::fs::write(&local, payload(40 * 1024 * 1024)).unwrap();
    let dest = dir.join("cancelled.bin");
    let dest_s = dest.to_str().unwrap().to_owned();

    let progress = Transfer::default();
    progress.cancel();
    let err = fs.upload_file(&dest_s, &local, &progress).await.unwrap_err();
    assert!(err.to_string().contains(rmux_fs::CANCELLED), "{err}");
    // The far script removes the short file once it sees end-of-input; give it
    // a moment, since our side returns as soon as it stops writing.
    for _ in 0..50 {
        if !dest.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(!dest.exists(), "a cancelled upload left a partial file behind");
}

#[tokio::test]
async fn a_cancelled_shell_upload_leaves_nothing() {
    let dir = scratch("cancel-target");
    cancelled(&TargetFs::new(LocalTarget), &dir).await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_cancelled_local_upload_leaves_nothing() {
    let dir = scratch("cancel-local");
    cancelled(&LocalFs, &dir).await;
    let _ = std::fs::remove_dir_all(dir);
}

/// The upload script refuses a short stream: it measures what arrived and
/// removes it. Driven through the raw single-shot entry with a lie about size
/// would need the private spec, so this streams a file that changes length
/// underneath — the same thing a dropped connection looks like to `cat`.
#[tokio::test]
async fn a_short_stream_is_removed_by_the_far_side() {
    let dir = scratch("short");
    let dest = dir.join("short.bin");
    let script = rmux_fs::protocol::upload_script(dest.to_str().unwrap(), 10);
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write as _;
            child.stdin.take().unwrap().write_all(b"abc")?;
            child.wait_with_output()
        })
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "S");
    assert!(!dest.exists(), "a short upload was left on disk");
    let _ = std::fs::remove_dir_all(dir);
}
