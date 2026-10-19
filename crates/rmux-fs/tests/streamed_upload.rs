//! Uploads of any size stream from disk through the same noclobber script.
//!
//! Driven through `TargetFs<LocalTarget>`, so the real `sh` runs the real
//! upload script and the bytes really cross a pipe — the path an SSH upload
//! takes, minus the network. 70 MB is past the old 64 MB cap on purpose.

use std::sync::atomic::{AtomicU64, Ordering};

use rmux_fs::{FileSystem, LocalFs, TargetFs};
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

    let sent = AtomicU64::new(0);
    fs.upload_file(dest, &local, &sent).await.unwrap();
    assert!(std::fs::read(dest).unwrap() == bytes, "the uploaded file differs from the source");
    assert_eq!(sent.load(Ordering::Relaxed), bytes.len() as u64, "progress did not count every byte");

    // A second upload onto the same name is refused, and leaves the first intact.
    let err = fs.upload_file(dest, &local, &AtomicU64::new(0)).await.unwrap_err();
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
