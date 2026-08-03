//! Windows, driven exactly as the app drives it.
//!
//! rmux reaches a Windows host through Git for Windows' bash, because OpenSSH
//! for Windows hands the remote command to `cmd.exe` and every rmux operation is
//! a POSIX script. The unit tests pin the *shape* of that wrapper; only a real
//! host can prove `cmd` actually delivers it, that stdin survives, and that the
//! login shell is bash rather than the `cmd.exe` Windows puts in `$SHELL`.
//!
//! ```text
//! RMUX_LIVE_WINDOWS=ytai-win cargo test -p rmux-fs --test live_windows -- --ignored --nocapture
//! ```

use rmux_fs::{FileContent, FileSystem, SearchQuery, TargetFs};
use rmux_ssh::SshTarget;
use rmux_transport::{CommandSpec, Platform, SshHostId, Target, Tty};

fn live_host() -> Option<String> {
    std::env::var("RMUX_LIVE_WINDOWS").ok().filter(|h| !h.is_empty())
}

#[tokio::test]
#[ignore = "needs a real Windows host; set RMUX_LIVE_WINDOWS"]
async fn a_windows_host_behaves_like_any_other() {
    let Some(host) = live_host() else {
        eprintln!("skipping: set RMUX_LIVE_WINDOWS to a Windows host alias");
        return;
    };

    let target = SshTarget::new(SshHostId::new(&host));
    let platform = target.connect().await.expect("connect");
    assert_eq!(platform, Platform::Windows, "expected a Windows host");
    eprintln!("connected to {host}: {platform:?}");

    // **The login shell must be bash, not cmd.** Windows sets
    // `SHELL=/c/windows/system32/cmd.exe` and MSYS does not override it, so
    // without the wrapper's correction every terminal and every Claude launch
    // would start cmd through a POSIX pipeline.
    let out = target
        // Through `sh -c`, because arguments are shell-quoted: passing `$SHELL`
        // directly would assert against the literal string.
        .exec(&CommandSpec::new("sh").arg("-c").arg("printf %s \"$SHELL\"").tty(Tty::None))
        .await
        .expect("shell probe");
    let shell = out.stdout.trim().to_owned();
    assert!(
        shell.ends_with("bash") || shell.ends_with("bash.exe"),
        "the login shell is {shell:?}, which would launch cmd"
    );
    eprintln!("login shell: {shell}");

    let fs = TargetFs::new(SshTarget::new(SshHostId::new(&host)));
    let home = fs.home_dir().await.expect("home_dir");
    assert!(home.starts_with('/'), "expected a POSIX home, got {home:?}");

    let root = format!("{home}/rmux-live-windows");
    let _ = fs.delete(&root).await;
    let outcome = checks(&fs, &root).await;
    let _ = fs.delete(&root).await;
    outcome.expect("windows checks failed");
    eprintln!("live verification passed against {host}");
}

async fn checks(fs: &dyn FileSystem, root: &str) -> anyhow::Result<()> {
    fs.create_dir(root).await?;

    // A filename `cmd` would mangle and a payload that is not text — both cross
    // the wrapper, which is the part that could quietly corrupt them.
    let awkward = format!("{root}/two words & %PATH%.txt");
    fs.write_file(&awkward, "kept\n").await?;
    match fs.read_file(&awkward).await? {
        FileContent::Text { text } => anyhow::ensure!(text == "kept\n", "round trip changed it"),
        other => anyhow::bail!("expected text, got {other:?}"),
    }

    // Upload streams its payload over **stdin**, which the obvious
    // `echo|base64 -d|bash` wrapper would have consumed itself.
    let binary = format!("{root}/blob.bin");
    let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x00, 0xff, 0xfe, b'\n', 0x1b];
    fs.upload(&binary, &bytes).await?;
    match fs.read_preview(&binary).await? {
        rmux_fs::PreviewContent::Base64 { bytes: n, .. } => {
            anyhow::ensure!(n == bytes.len() as u64, "upload changed the length: {n}")
        }
        other => anyhow::bail!("expected base64, got {other:?}"),
    }

    // Search runs `grep` on the host, and its records are NUL-delimited.
    fs.write_file(&format!("{root}/a.txt"), "alpha needle here\n").await?;
    let hits = fs.search(root, &SearchQuery { text: "needle".into(), ..Default::default() }).await?;
    anyhow::ensure!(!hits.is_empty(), "search found nothing on a file it wrote");
    anyhow::ensure!(hits[0].text.contains("needle"), "search returned {:?}", hits[0]);

    // The clobber guard holds here too — the `set -C` is inside the payload, so
    // it has to survive the wrapper.
    anyhow::ensure!(
        fs.upload(&binary, b"replacement").await.is_err(),
        "upload overwrote an existing file"
    );

    Ok(())
}
