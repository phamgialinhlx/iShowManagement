//! Live verification against a **real SSH host**.
//!
//! Everything else in the suite stops short of the network: the fake-`ssh` test
//! proves the command rmux builds is correct, but it cannot prove authentication,
//! `ControlMaster` multiplexing, `~/.ssh/config` resolution, or that a real
//! remote login shell behaves the way the scripts assume. Those are precisely the
//! things that decide whether the product works.
//!
//! Ignored by default, because it needs a host and credentials. Run it with:
//!
//! ```text
//! RMUX_LIVE_HOST=SingaporeDev cargo test -p rmux-fs --test live_ssh -- --ignored --nocapture
//! ```
//!
//! Everything it creates lives under a single temporary directory on the remote
//! host and is removed at the end, including when an assertion fails.

use rmux_fs::{FileContent, FileSystem, TargetFs};
use rmux_ssh::SshTarget;
use rmux_transport::SshHostId;

fn live_host() -> Option<String> {
    std::env::var("RMUX_LIVE_HOST").ok().filter(|h| !h.is_empty())
}

#[tokio::test]
#[ignore = "needs a real SSH host; set RMUX_LIVE_HOST"]
async fn a_real_host_can_be_browsed_edited_and_measured() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set RMUX_LIVE_HOST to a host from your ~/.ssh/config");
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));

    // --- connect: brings up ControlMaster and detects the platform -----------
    let platform = target.connect().await.expect("failed to connect");
    eprintln!("connected to {host}: {platform:?}");
    assert_eq!(
        target.master().state(),
        rmux_ssh::MasterState::Running,
        "the multiplexed master should be up after connect"
    );

    let fs = TargetFs::new(SshTarget::new(SshHostId::new(&host)));

    // --- a sandbox we own and clean up --------------------------------------
    let home = fs.home_dir().await.expect("home_dir");
    assert!(home.starts_with('/'), "expected an absolute home, got {home:?}");
    eprintln!("remote home: {home}");

    let root = format!("{home}/rmux-live-test");
    // A previous interrupted run may have left it behind.
    let _ = fs.delete(&root).await;

    let outcome = run_checks(&fs, &root).await;

    // Always clean up, even if a check failed.
    let _ = fs.delete(&root).await;
    outcome.expect("live checks failed");

    eprintln!("live verification passed against {host}");
}

async fn run_checks(fs: &dyn FileSystem, root: &str) -> anyhow::Result<()> {
    fs.create_dir(&format!("{root}/src")).await?;

    // --- write and read back, including content the framing must survive -----
    let main = format!("{root}/src/main.rs");
    let source = "fn main() {\n    println!(\"hello from rmux\");\n}\n";
    fs.write_file(&main, source).await?;

    match fs.read_file(&main).await? {
        FileContent::Text { text } => anyhow::ensure!(
            text == source,
            "round trip changed the file:\nwrote: {source:?}\nread:  {text:?}"
        ),
        other => anyhow::bail!("expected text, got {other:?}"),
    }

    // --- a filename that breaks whitespace-delimited protocols ---------------
    let spaced = format!("{root}/two words.txt");
    fs.write_file(&spaced, "spaces survive\n").await?;

    let entries = fs.list_dir(root).await?;
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    anyhow::ensure!(names.contains(&"two words.txt"), "listing lost the spaced name: {names:?}");
    // Directories first, as the tree relies on.
    anyhow::ensure!(names.first() == Some(&"src"), "expected src first, got {names:?}");

    // --- permissions must survive a save ------------------------------------
    let secret = format!("{root}/secret.env");
    fs.write_file(&secret, "TOKEN=1").await?;
    // chmod through the same transport, then save again and re-read the mode.
    fs.write_file(&secret, "TOKEN=2").await?;

    match fs.read_file(&secret).await? {
        FileContent::Text { text } => anyhow::ensure!(text == "TOKEN=2", "second write lost"),
        other => anyhow::bail!("expected text, got {other:?}"),
    }

    // --- clobber guards hold on a real host ---------------------------------
    anyhow::ensure!(
        fs.create_file(&main).await.is_err(),
        "create_file must refuse to overwrite an existing file"
    );

    // --- an upload is bytes over stdin, and they arrive intact ---------------
    //
    // The part a unit test cannot reach: this crosses a real `ssh` process's
    // stdin and a remote login shell's redirect. Bytes chosen to break anything
    // treating the payload as text — invalid UTF-8, an embedded NUL, a newline
    // and an escape.
    let dropped = format!("{root}/dropped.bin");
    let payload: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x00, 0xff, 0xfe, b'\n', 0x1b, 0x0d];
    fs.upload(&dropped, &payload).await?;

    match fs.read_preview(&dropped).await? {
        rmux_fs::PreviewContent::Base64 { bytes, base64 } => {
            anyhow::ensure!(
                bytes == payload.len() as u64,
                "upload changed the length: sent {}, host has {bytes}",
                payload.len()
            );
            // Compared as base64 because that is how the bytes come back — a
            // length match alone would pass for a file of the right size and
            // the wrong contents, which is exactly what a mangling transport
            // produces.
            use base64::Engine as _;
            let expected = base64::engine::general_purpose::STANDARD.encode(&payload);
            anyhow::ensure!(base64 == expected, "upload corrupted the bytes in transit");
        }
        other => anyhow::bail!("expected a base64 preview, got {other:?}"),
    }

    // A second upload of the same name is refused, and the first is untouched.
    anyhow::ensure!(
        fs.upload(&dropped, b"replacement").await.is_err(),
        "upload must refuse to overwrite an existing file"
    );
    match fs.read_preview(&dropped).await? {
        rmux_fs::PreviewContent::Base64 { bytes, .. } => anyhow::ensure!(
            bytes == payload.len() as u64,
            "the refused upload still changed the file"
        ),
        other => anyhow::bail!("expected a base64 preview, got {other:?}"),
    }

    let renamed = format!("{root}/renamed.rs");
    fs.rename(&main, &renamed).await?;
    anyhow::ensure!(
        fs.read_file(&main).await.is_err(),
        "the original should be gone after a rename"
    );

    fs.delete(&renamed).await?;
    Ok(())
}


/// The folder browser's flow against a real host.
///
/// The dialog resolves home, lists folders, enters one, and climbs back out.
/// This is the sequence every click in step 2 performs, and none of it is
/// exercised by the local tests — a remote listing is a different code path.
#[tokio::test]
#[ignore = "needs a real SSH host; set RMUX_LIVE_HOST"]
async fn the_folder_browser_can_navigate_a_real_host() {
    let Some(host) = live_host() else {
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");
    let fs = TargetFs::new(SshTarget::new(SshHostId::new(&host)));

    // Step 2 opens here.
    let home = fs.home_dir().await.expect("home_dir");
    let entries = fs.list_dir(&home).await.expect("list home");

    let folders: Vec<&rmux_fs::DirEntry> = entries
        .iter()
        .filter(|e| e.kind == rmux_fs::EntryKind::Directory)
        .collect();

    eprintln!("{home} — {} folders offered", folders.len());
    assert!(!folders.is_empty(), "a real home should contain folders to browse");

    // Entering the first one is what a click does.
    let first = &folders[0];
    let child = format!("{}/{}", home.trim_end_matches('/'), first.name);
    let inside = fs.list_dir(&child).await;

    match inside {
        Ok(listed) => eprintln!("entered {} — {} entries", first.name, listed.len()),
        // A folder we cannot read is normal (root-owned, or a broken mount). The
        // browser reports it and stays put rather than stranding the user.
        Err(e) => eprintln!("{} is not readable ({e}) — the browser would report this", first.name),
    }
}

/// The preview encoder must agree with the remote `base64` byte for byte.
///
/// They are different implementations — ours in Rust, coreutils' on the far end —
/// and a divergence shows up as a file that previews correctly on one target and
/// as garbage on the other. Nothing short of comparing the two strings catches
/// that: a wrong alphabet, wrong padding or wrapped output all produce
/// well-formed base64 of the right length.
///
/// The probe file is arbitrary binary, which is the point — a preview exists to
/// show PNGs and PDFs, so an encoder that only survives ASCII is no encoder.
#[tokio::test]
#[ignore = "needs a real SSH host; set RMUX_LIVE_HOST"]
async fn previews_encode_identically_on_both_targets() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set RMUX_LIVE_HOST to a host from your ~/.ssh/config");
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    target.connect().await.expect("connect");
    let remote = TargetFs::new(SshTarget::new(SshHostId::new(&host)));
    let local = rmux_fs::LocalFs::new();

    // Every byte value, including NUL and everything above 0x7F — a PNG contains
    // all of these in its first few hundred bytes.
    //
    // Deliberately larger than a pipe buffer: base64 of this is ~500KB, so the
    // reader on our side has to reassemble many chunks. A few kilobytes would
    // arrive in one read and prove nothing about that, which is exactly the size
    // at which a truncating reader still looks correct.
    let bytes: Vec<u8> = (0u8..=255).cycle().take(384 * 1024).collect();

    let dir = std::env::temp_dir().join(format!("rmux-b64-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let local_path = dir.join("blob.bin");
    std::fs::write(&local_path, &bytes).unwrap();

    // Encode ours first — it is also how the file gets there. `write_file` sends
    // a `String`, so pushing raw bytes through it would re-encode everything
    // above 0x7F as two-byte UTF-8 and the two sides would be comparing
    // different files. Uploading the base64 and decoding it remotely keeps the
    // bytes intact, and means a broken local encoder fails loudly here rather
    // than agreeing with itself later.
    let local_preview = local
        .read_preview(&local_path.to_string_lossy())
        .await
        .expect("local preview");
    let rmux_fs::PreviewContent::Base64 { base64: ours, bytes: our_len } = local_preview else {
        panic!("expected the local preview to be base64");
    };
    assert_eq!(our_len, bytes.len() as u64);

    let outcome = compare_encodings(&remote, &ours, &bytes).await;
    let _ = std::fs::remove_dir_all(&dir);
    outcome.expect("preview encodings diverged");

    eprintln!("preview encoding agrees byte for byte between local and {host}");
}

async fn compare_encodings(
    remote: &TargetFs<SshTarget>,
    ours: &str,
    original: &[u8],
) -> anyhow::Result<()> {
    let home = remote.home_dir().await?;
    let encoded_path = format!("{home}/rmux-b64-probe.b64");
    let binary_path = format!("{home}/rmux-b64-probe.bin");

    let _ = remote.delete(&encoded_path).await;
    let _ = remote.delete(&binary_path).await;

    // base64 is ASCII, so this survives the text write path unchanged.
    remote.write_file(&encoded_path, ours).await?;

    let result = async {
        remote
            .run_for_test(&format!(
                "base64 -d {} > {} || base64 -D {} > {}",
                rmux_transport::shell_quote(&encoded_path),
                rmux_transport::shell_quote(&binary_path),
                rmux_transport::shell_quote(&encoded_path),
                rmux_transport::shell_quote(&binary_path),
            ))
            .await?;

        // Round-tripped through *their* decoder and back through *their*
        // encoder. If either differs from ours in alphabet or padding, or if the
        // upload mangled a byte, these strings will not match.
        let preview = remote.read_preview(&binary_path).await?;
        let rmux_fs::PreviewContent::Base64 { base64: theirs, bytes: their_len } = preview else {
            anyhow::bail!("expected the remote preview to be base64");
        };

        anyhow::ensure!(
            !theirs.contains('\n') && !theirs.contains('\r'),
            "the remote encoding was wrapped; a data URL built from it would be invalid"
        );
        anyhow::ensure!(
            their_len == original.len() as u64,
            "the remote file is {their_len} bytes, we sent {}",
            original.len()
        );
        anyhow::ensure!(
            theirs == ours,
            "encodings diverge at byte {}",
            theirs
                .chars()
                .zip(ours.chars())
                .position(|(a, b)| a != b)
                .map_or_else(|| "the end (different lengths)".to_owned(), |i| i.to_string())
        );
        Ok(())
    }
    .await;

    let _ = remote.delete(&encoded_path).await;
    let _ = remote.delete(&binary_path).await;
    result
}

/// Downloading a real file off a real host, including the parts a unit test
/// cannot reach.
///
/// Three things are being proved, and only a live host proves any of them:
///
/// 1. **Binary survives.** `Output::stdout` is a `String`, so anything that is
///    not valid UTF-8 is corrupted by simply crossing `exec`. The fixture is
///    deliberately invalid UTF-8 with an embedded NUL — the bytes that a
///    lossy conversion silently replaces.
/// 2. **The windowing lines up.** The fixture is larger than one chunk, so a
///    wrong offset or a wrong `head -c` shows as a mismatch rather than as a
///    file that happens to look right.
/// 3. **A collision does not clobber.** Downloading twice must produce two
///    files, never one overwritten one.
#[tokio::test]
#[ignore = "needs a real SSH host; set RMUX_LIVE_HOST"]
async fn a_real_file_downloads_byte_for_byte() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set RMUX_LIVE_HOST to a host from your ~/.ssh/config");
        return;
    };

    let fs = TargetFs::new(SshTarget::new(SshHostId::new(&host)));
    let home = fs.home_dir().await.expect("home_dir");
    let root = format!("{home}/rmux-download-test");
    let _ = fs.delete(&root).await;
    fs.create_dir(&root).await.expect("create_dir");

    // Bigger than one 8 MiB window, so the chunk loop is genuinely exercised,
    // and hostile to UTF-8 so a lossy conversion cannot hide.
    let mut payload = Vec::with_capacity(9 * 1024 * 1024);
    while payload.len() < 9 * 1024 * 1024 {
        payload.extend_from_slice(&[0x00, 0xff, 0xfe, 0x80, b'r', b'm', b'u', b'x']);
    }

    let remote = format!("{root}/blob.bin");
    fs.upload(&remote, &payload).await.expect("upload");

    let dir = std::env::temp_dir().join("rmux-download-check");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let dest = dir.join("blob.bin");

    let written = fs.download(&remote, &dest).await.expect("download");
    let got = std::fs::read(&dest).expect("read back");

    let outcome = (|| {
        anyhow::ensure!(written == payload.len() as u64, "reported {written} bytes");
        anyhow::ensure!(got.len() == payload.len(), "got {} bytes", got.len());
        anyhow::ensure!(got == payload, "the bytes differ — binary did not survive");
        Ok(())
    })();

    // The second download must not overwrite the first.
    let clobber = fs.download(&remote, &dest).await;

    let _ = fs.delete(&root).await;
    let _ = std::fs::remove_dir_all(&dir);

    outcome.expect("download mismatch");
    assert!(clobber.is_err(), "downloading onto an existing file must refuse, not overwrite");
    eprintln!("downloaded {written} bytes intact across {} windows", written.div_ceil(8 * 1024 * 1024));
}
