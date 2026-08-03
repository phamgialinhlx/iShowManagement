//! Reading and writing files on a [`Target`].
//!
//! One trait, two implementations — the local filesystem and a remote one driven
//! over the multiplexed SSH connection. Feature code never learns which it has,
//! which is what lets rmux be an ordinary local IDE and a remote one with the
//! same editor.
//!
//! The remote implementation talks to a plain POSIX shell, so it works on any
//! host reachable by `ssh` with nothing installed. That matters more than it
//! sounds: it means a host you can SSH into is immediately editable, with no
//! agent upload, no version matching, and nothing left behind. A future agent
//! will make watching and searching cheaper, but it is an optimisation over this
//! path, not a prerequisite for it.

use async_trait::async_trait;
use rmux_transport::{CommandSpec, Target, Tty};
use serde::{Deserialize, Serialize};

pub mod protocol;
pub mod search;

pub use search::{SearchHit, SearchQuery};

pub use protocol::{MAX_READ_BYTES, ReadOutcome};

/// What a directory entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

/// One entry in a directory listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
}

/// Largest file previewed as base64.
///
/// Base64 inflates by a third and the whole thing crosses the network before
/// anything is shown, so this is a comfort limit rather than a capability one.
pub const MAX_PREVIEW_BYTES: u64 = 24 * 1024 * 1024;

/// Largest file the tree will upload.
///
/// The limit is the IPC bridge, not the disk: the webview has to hand the bytes
/// over as one base64 string, which inflates them by a third and is built,
/// copied and parsed in memory on both sides. A cap that says so beats a
/// several-hundred-megabyte drag that appears to hang the app.
pub const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// A non-text file, encoded for the webview.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PreviewContent {
    Base64 { bytes: u64, base64: String },
    TooLarge { bytes: u64 },
}

/// A file's contents as the editor should see them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FileContent {
    Text { text: String },
    /// Binary files are reported, never silently mangled — opening one in a text
    /// editor and saving would corrupt it.
    Binary { bytes: u64 },
    TooLarge { bytes: u64 },
}

/// Reading and writing files, wherever they live.
#[async_trait]
pub trait FileSystem: Send + Sync {
    async fn list_dir(&self, path: &str) -> anyhow::Result<Vec<DirEntry>>;
    async fn read_file(&self, path: &str) -> anyhow::Result<FileContent>;
    async fn write_file(&self, path: &str, contents: &str) -> anyhow::Result<()>;
    async fn create_dir(&self, path: &str) -> anyhow::Result<()>;
    /// Create an empty file. Fails if something is already there, so a mistyped
    /// name can never truncate an existing file.
    async fn create_file(&self, path: &str) -> anyhow::Result<()>;
    /// Write `bytes` to a new file. Fails if anything is already there.
    ///
    /// Separate from `write_file` because that one takes a `String` — an upload
    /// is arbitrary bytes, and routing a `.png` through UTF-8 would corrupt it.
    /// Refusing to clobber matters more here than anywhere else: a drop lands on
    /// whatever folder was under the pointer, so the name is chosen by the file
    /// rather than typed, and a silent overwrite is the likely accident.
    async fn upload(&self, path: &str, bytes: &[u8]) -> anyhow::Result<()>;

    /// Rename or move. Fails if the destination exists.
    async fn rename(&self, from: &str, to: &str) -> anyhow::Result<()>;
    /// Delete a file or a directory tree.
    async fn delete(&self, path: &str) -> anyhow::Result<()>;
    /// Read a file as base64, for previewing formats the editor cannot show.
    async fn read_preview(&self, path: &str) -> anyhow::Result<PreviewContent>;
    /// Where the file browser should open.
    async fn home_dir(&self) -> anyhow::Result<String>;
    /// Find text under `root`. See `search` for why this runs `grep` on the
    /// machine that owns the disk rather than walking the tree from here.
    async fn search(&self, root: &str, query: &SearchQuery) -> anyhow::Result<Vec<SearchHit>>;
}

/// A filesystem reached by running shell commands on a [`Target`].
///
/// Works for local targets too, but [`LocalFs`] is used there instead — it avoids
/// spawning a shell for every listing.
pub struct TargetFs<T: Target> {
    target: T,
}

impl<T: Target> TargetFs<T> {
    pub fn new(target: T) -> Self {
        Self { target }
    }

    /// Run a shell snippet and return its raw stdout.
    ///
    /// Raw bytes, not a `String`: file contents and filenames are arbitrary bytes
    /// and lossy conversion here would corrupt them before parsing.
    async fn run(&self, script: &str) -> anyhow::Result<Vec<u8>> {
        let spec = CommandSpec::new("sh").arg("-c").arg(script).tty(Tty::None);
        let out = self.target.exec(&spec).await?;
        anyhow::ensure!(
            out.status == 0,
            "remote command failed (status {}): {}",
            out.status,
            out.stderr.trim()
        );
        Ok(out.stdout.into_bytes())
    }

    /// Run an arbitrary snippet on the target. **Tests only.**
    ///
    /// Integration tests live in a separate crate, so they cannot reach `run`.
    /// The live tests need it to set up fixtures with tools the `FileSystem`
    /// trait deliberately does not expose — building those fixtures out of trait
    /// methods would mean testing the code under test with itself.
    #[doc(hidden)]
    pub async fn run_for_test(&self, script: &str) -> anyhow::Result<Vec<u8>> {
        self.run(script).await
    }
}

#[async_trait]
impl<T: Target> FileSystem for TargetFs<T> {
    async fn list_dir(&self, path: &str) -> anyhow::Result<Vec<DirEntry>> {
        let output = self.run(&protocol::list_dir_script(path)).await?;
        protocol::parse_listing(&output)
    }

    async fn read_file(&self, path: &str) -> anyhow::Result<FileContent> {
        let output = self.run(&protocol::read_file_script(path, MAX_READ_BYTES)).await?;

        match protocol::parse_read(&output)? {
            ReadOutcome::Content(bytes) => Ok(classify_content(bytes)),
            ReadOutcome::TooLarge(bytes) => Ok(FileContent::TooLarge { bytes }),
            ReadOutcome::IsDirectory => anyhow::bail!("{path} is a directory"),
            ReadOutcome::Missing => anyhow::bail!("{path} does not exist"),
            ReadOutcome::PermissionDenied => anyhow::bail!("permission denied: {path}"),
        }
    }

    async fn write_file(&self, path: &str, contents: &str) -> anyhow::Result<()> {
        let spec = CommandSpec::new("sh")
            .arg("-c")
            .arg(protocol::write_file_script(path))
            .tty(Tty::None);

        let out = self.target.exec_with_input(&spec, contents.as_bytes()).await?;
        anyhow::ensure!(
            out.status == 0 && out.stdout.trim_end() == "O",
            "failed to write {path}: {}",
            out.stderr.trim()
        );
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> anyhow::Result<()> {
        let output = self.run(&protocol::mkdir_script(path)).await?;
        anyhow::ensure!(output == b"O", "failed to create {path}");
        Ok(())
    }

    async fn create_file(&self, path: &str) -> anyhow::Result<()> {
        let output = self.run(&protocol::create_file_script(path)).await?;
        anyhow::ensure!(output != b"X", "{path} already exists");
        anyhow::ensure!(output == b"O", "failed to create {path}");
        Ok(())
    }

    async fn upload(&self, path: &str, bytes: &[u8]) -> anyhow::Result<()> {
        // `Tty::None` is load-bearing: a PTY would translate the byte stream on
        // its way through and quietly corrupt anything that is not text.
        let spec =
            CommandSpec::new("sh").arg("-c").arg(protocol::upload_script(path)).tty(Tty::None);

        let out = self.target.exec_with_input(&spec, bytes).await?;
        anyhow::ensure!(out.stdout.trim_end() != "X", "{path} already exists");
        anyhow::ensure!(
            out.status == 0 && out.stdout.trim_end() == "O",
            "failed to upload {path}: {}",
            out.stderr.trim()
        );
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> anyhow::Result<()> {
        let output = self.run(&protocol::rename_script(from, to)).await?;
        anyhow::ensure!(output != b"X", "{to} already exists");
        anyhow::ensure!(output == b"O", "failed to rename {from}");
        Ok(())
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        let output = self.run(&protocol::delete_script(path)).await?;
        anyhow::ensure!(output == b"O", "failed to delete {path}");
        Ok(())
    }

    async fn read_preview(&self, path: &str) -> anyhow::Result<PreviewContent> {
        let output = self.run(&protocol::read_base64_script(path, MAX_PREVIEW_BYTES)).await?;

        match protocol::parse_base64(&output)? {
            protocol::Base64Outcome::Content { bytes, base64 } => {
                Ok(PreviewContent::Base64 { bytes, base64 })
            }
            protocol::Base64Outcome::TooLarge(bytes) => Ok(PreviewContent::TooLarge { bytes }),
            protocol::Base64Outcome::Missing => anyhow::bail!("{path} does not exist"),
            protocol::Base64Outcome::PermissionDenied => {
                anyhow::bail!("permission denied: {path}")
            }
        }
    }

    async fn home_dir(&self) -> anyhow::Result<String> {
        let output = self.run(&protocol::home_script()).await?;
        Ok(String::from_utf8_lossy(&output).trim().to_owned())
    }

    async fn search(&self, root: &str, query: &SearchQuery) -> anyhow::Result<Vec<SearchHit>> {
        Ok(search::parse(&self.run(&search::script(root, query)).await?))
    }
}

/// The local filesystem, using `std::fs` directly.
///
/// A separate implementation rather than [`TargetFs`] over a local target: going
/// through a shell for every listing would spawn a process per keystroke in the
/// file browser, which is exactly the sluggishness a local IDE must not have.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl LocalFs {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FileSystem for LocalFs {
    async fn list_dir(&self, path: &str) -> anyhow::Result<Vec<DirEntry>> {
        let mut reader = tokio::fs::read_dir(path).await?;
        let mut entries = Vec::new();

        while let Some(entry) = reader.next_entry().await? {
            // `file_type` does not follow symlinks, so a link to a directory is
            // reported as a link — matching the remote implementation.
            let kind = match entry.file_type().await {
                Ok(t) if t.is_symlink() => EntryKind::Symlink,
                Ok(t) if t.is_dir() => EntryKind::Directory,
                Ok(_) => EntryKind::File,
                // A file that vanished mid-listing; skip rather than fail the
                // whole directory.
                Err(_) => continue,
            };
            entries.push(DirEntry { name: entry.file_name().to_string_lossy().into_owned(), kind });
        }

        protocol::sort_entries(&mut entries);
        Ok(entries)
    }

    async fn read_file(&self, path: &str) -> anyhow::Result<FileContent> {
        let metadata = tokio::fs::metadata(path).await?;
        anyhow::ensure!(!metadata.is_dir(), "{path} is a directory");

        // Checked before reading, so a huge file is never loaded into memory.
        if metadata.len() > MAX_READ_BYTES {
            return Ok(FileContent::TooLarge { bytes: metadata.len() });
        }

        Ok(classify_content(tokio::fs::read(path).await?))
    }

    async fn write_file(&self, path: &str, contents: &str) -> anyhow::Result<()> {
        // Written in place rather than via a temp file and rename: renaming would
        // replace the inode and silently drop the file's permissions and any hard
        // links, which for an editor save is a destructive surprise.
        tokio::fs::write(path, contents).await?;
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(path).await?;
        Ok(())
    }

    async fn create_file(&self, path: &str) -> anyhow::Result<()> {
        // `create_new` is the atomic form: checking then creating would race, and
        // the losing side would silently truncate someone else's file.
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => anyhow::anyhow!("{path} already exists"),
                _ => e.into(),
            })?;
        Ok(())
    }

    async fn upload(&self, path: &str, bytes: &[u8]) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt as _;

        // `create_new` is the atomic refusal, same as `create_file`. Checking
        // and then opening would race, and the loser truncates a file nobody
        // named.
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => anyhow::anyhow!("{path} already exists"),
                _ => e.into(),
            })?;
        file.write_all(bytes).await?;
        file.flush().await?;
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> anyhow::Result<()> {
        // `fs::rename` silently replaces the destination, so the guard is ours.
        anyhow::ensure!(tokio::fs::metadata(to).await.is_err(), "{to} already exists");
        tokio::fs::rename(from, to).await?;
        Ok(())
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        let metadata = tokio::fs::symlink_metadata(path).await?;
        // A symlink to a directory must be unlinked, not walked into — removing
        // the tree behind it would delete files outside what the user selected.
        if metadata.is_dir() && !metadata.is_symlink() {
            tokio::fs::remove_dir_all(path).await?;
        } else {
            tokio::fs::remove_file(path).await?;
        }
        Ok(())
    }

    async fn read_preview(&self, path: &str) -> anyhow::Result<PreviewContent> {
        let metadata = tokio::fs::metadata(path).await?;
        anyhow::ensure!(!metadata.is_dir(), "{path} is a directory");

        // Checked before reading, so an enormous file is never loaded at all.
        if metadata.len() > MAX_PREVIEW_BYTES {
            return Ok(PreviewContent::TooLarge { bytes: metadata.len() });
        }

        let bytes = tokio::fs::read(path).await?;
        Ok(PreviewContent::Base64 {
            bytes: bytes.len() as u64,
            base64: base64_encode(&bytes),
        })
    }

    async fn home_dir(&self) -> anyhow::Result<String> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        Ok(home.to_string_lossy().into_owned())
    }

    /// The same `grep`, run locally.
    ///
    /// Shelling out rather than walking the tree in Rust keeps one definition of
    /// what a search *is* — the flags, the skipped directories and the record
    /// format are shared, so a result found on a server and the same result
    /// found here cannot disagree. This is the one place `LocalFs` spawns a
    /// process, and it is worth it for exactly that reason.
    async fn search(&self, root: &str, query: &SearchQuery) -> anyhow::Result<Vec<SearchHit>> {
        let out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(search::script(root, query))
            .output()
            .await?;
        Ok(search::parse(&out.stdout))
    }
}

/// Standard base64, no wrapping.
///
/// Hand-rolled to keep a dependency out of the tree for forty lines of table
/// lookup, and to guarantee the output matches what the remote `base64` produces.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;

        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        // Padding depends on how many input bytes the chunk actually had.
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Decide whether bytes are editable text.
fn classify_content(bytes: Vec<u8>) -> FileContent {
    if protocol::looks_binary(&bytes) {
        return FileContent::Binary { bytes: bytes.len() as u64 };
    }

    match String::from_utf8(bytes) {
        Ok(text) => FileContent::Text { text },
        // Valid non-UTF-8 text (latin-1 logs, say) would be corrupted by a lossy
        // conversion the moment it was saved back, so it is treated as binary.
        Err(e) => FileContent::Binary { bytes: e.into_bytes().len() as u64 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rmux-fs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn local_round_trips_a_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("note.txt").to_string_lossy().into_owned();
        let fs = LocalFs::new();

        fs.write_file(&path, "hello\nworld\n").await.unwrap();

        match fs.read_file(&path).await.unwrap() {
            FileContent::Text { text } => assert_eq!(text, "hello\nworld\n"),
            other => panic!("expected text, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn saving_keeps_the_files_permissions() {
        // An editor save must not quietly turn a 0600 secret into 0644.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = temp_dir("perms");
            let path = dir.join("secret.env").to_string_lossy().into_owned();
            let fs = LocalFs::new();

            fs.write_file(&path, "TOKEN=1").await.unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

            fs.write_file(&path, "TOKEN=2").await.unwrap();

            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "permissions changed on save: {:o}", mode);

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn local_listings_are_sorted_like_remote_ones() {
        let dir = temp_dir("listing");
        std::fs::write(dir.join("zebra.txt"), "").unwrap();
        std::fs::write(dir.join("Apple.txt"), "").unwrap();
        std::fs::create_dir(dir.join("src")).unwrap();

        let entries = LocalFs::new().list_dir(&dir.to_string_lossy()).await.unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();

        // Directories first, then case-insensitive — identical to the remote path.
        assert_eq!(names, vec!["src", "Apple.txt", "zebra.txt"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_matches_the_reference_encoding() {
        // Must agree with the remote `base64` byte for byte, or a file previews
        // correctly on one target and as garbage on the other.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // Bytes above 0x7f must not be mangled by any char conversion.
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[tokio::test]
    async fn a_preview_round_trips_binary_content() {
        let dir = temp_dir("preview");
        let path = dir.join("logo.png").to_string_lossy().into_owned();
        let bytes: Vec<u8> = (0u8..=255).collect();
        std::fs::write(&path, &bytes).unwrap();

        match LocalFs::new().read_preview(&path).await.unwrap() {
            PreviewContent::Base64 { bytes: n, base64 } => {
                assert_eq!(n, 256);
                assert_eq!(base64, base64_encode(&bytes));
            }
            other => panic!("expected base64, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn binary_files_are_flagged_not_mangled() {
        let dir = temp_dir("binary");
        let path = dir.join("a.bin").to_string_lossy().into_owned();
        std::fs::write(&path, [0x7f, b'E', b'L', b'F', 0, 0, 0, 1]).unwrap();

        // Opening this as text and saving would corrupt it.
        match LocalFs::new().read_file(&path).await.unwrap() {
            FileContent::Binary { bytes } => assert_eq!(bytes, 8),
            other => panic!("expected binary, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn oversized_files_are_refused_without_being_read() {
        let dir = temp_dir("large");
        let path = dir.join("big.log").to_string_lossy().into_owned();
        std::fs::write(&path, vec![b'x'; (MAX_READ_BYTES + 1) as usize]).unwrap();

        match LocalFs::new().read_file(&path).await.unwrap() {
            FileContent::TooLarge { bytes } => assert!(bytes > MAX_READ_BYTES),
            other => panic!("expected TooLarge, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reading_a_directory_is_an_error_not_empty_content() {
        let dir = temp_dir("isdir");
        let err = LocalFs::new().read_file(&dir.to_string_lossy()).await.unwrap_err();
        assert!(err.to_string().contains("directory"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_file_reports_an_error() {
        let err = LocalFs::new().read_file("/nonexistent/rmux/nope.txt").await.unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    /// The local and remote implementations must agree, or the editor behaves
    /// differently depending on where the file lives — the exact divergence the
    /// `FileSystem` trait exists to prevent. This drives the *shell* path against
    /// the local machine, which is the same code a remote host runs.
    #[tokio::test]
    async fn the_shell_path_agrees_with_the_native_one() {
        use rmux_transport::LocalTarget;

        let dir = temp_dir("parity");
        std::fs::write(dir.join("b.txt"), "content\n").unwrap();
        std::fs::write(dir.join("A.txt"), "").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        // Names the naive formats would break on.
        std::fs::write(dir.join("two words.txt"), "").unwrap();

        let path = dir.to_string_lossy().into_owned();
        let native = LocalFs::new().list_dir(&path).await.unwrap();
        let shell = TargetFs::new(LocalTarget::new()).list_dir(&path).await.unwrap();

        assert_eq!(native, shell, "native and shell listings diverged");

        let file = dir.join("b.txt").to_string_lossy().into_owned();
        let shell_read = TargetFs::new(LocalTarget::new()).read_file(&file).await.unwrap();
        match shell_read {
            FileContent::Text { text } => assert_eq!(text, "content\n"),
            other => panic!("expected text, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_shell_path_round_trips_a_write() {
        use rmux_transport::LocalTarget;

        let dir = temp_dir("shellwrite");
        let path = dir.join("via-shell.txt").to_string_lossy().into_owned();
        let fs = TargetFs::new(LocalTarget::new());

        fs.write_file(&path, "written over ssh\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "written over ssh\n");

        // Overwriting must replace, not append.
        fs.write_file(&path, "second\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_shell_path_preserves_permissions_on_save() {
        #[cfg(unix)]
        {
            use rmux_transport::LocalTarget;
            use std::os::unix::fs::PermissionsExt;

            let dir = temp_dir("shellperms");
            let path = dir.join("secret.env").to_string_lossy().into_owned();
            std::fs::write(&path, "TOKEN=1").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

            TargetFs::new(LocalTarget::new()).write_file(&path, "TOKEN=2").await.unwrap();

            // If the script used `mv`, this would now be the temp file's mode.
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "save replaced the inode: {:o}", mode);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "TOKEN=2");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn creating_a_file_never_truncates_an_existing_one() {
        let dir = temp_dir("create");
        let path = dir.join("keep.txt").to_string_lossy().into_owned();
        std::fs::write(&path, "important").unwrap();

        let err = LocalFs::new().create_file(&path).await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
        // The point of the guard: the original content is untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "important");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_upload_never_clobbers_and_keeps_every_byte() {
        let dir = temp_dir("upload");
        let path = dir.join("shot.png").to_string_lossy().into_owned();

        // Bytes that are not valid UTF-8 and contain a NUL — the two things a
        // text-shaped path would corrupt on the way through.
        let bytes: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x00, 0xff, 0xfe, b'\n', 0x1b];
        LocalFs::new().upload(&path, &bytes).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        // A second drop of the same name is refused, not merged into the first.
        let err = LocalFs::new().upload(&path, b"different").await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn renaming_never_clobbers_the_destination() {
        let dir = temp_dir("rename");
        let from = dir.join("a.txt").to_string_lossy().into_owned();
        let to = dir.join("b.txt").to_string_lossy().into_owned();
        std::fs::write(&from, "source").unwrap();
        std::fs::write(&to, "destination").unwrap();

        let err = LocalFs::new().rename(&from, &to).await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "destination");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn deleting_a_symlink_does_not_touch_its_target() {
        #[cfg(unix)]
        {
            let dir = temp_dir("symlink-delete");
            let real = dir.join("real");
            std::fs::create_dir(&real).unwrap();
            std::fs::write(real.join("precious.txt"), "keep me").unwrap();

            let link = dir.join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();

            LocalFs::new().delete(&link.to_string_lossy()).await.unwrap();

            // Following the link would have deleted the real directory.
            assert!(!link.exists());
            assert!(real.join("precious.txt").exists(), "deleting a link removed its target");

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[tokio::test]
    async fn the_shell_path_creates_renames_and_deletes() {
        use rmux_transport::LocalTarget;

        let dir = temp_dir("shellops");
        let fs = TargetFs::new(LocalTarget::new());
        let a = dir.join("a.txt").to_string_lossy().into_owned();
        let b = dir.join("b.txt").to_string_lossy().into_owned();

        fs.create_file(&a).await.unwrap();
        assert!(std::path::Path::new(&a).exists());

        // Same refusals as the native path.
        assert!(fs.create_file(&a).await.is_err());

        fs.rename(&a, &b).await.unwrap();
        assert!(!std::path::Path::new(&a).exists());
        assert!(std::path::Path::new(&b).exists());

        fs.delete(&b).await.unwrap();
        assert!(!std::path::Path::new(&b).exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_filename_that_would_break_a_naive_format_survives_the_shell_path() {
        use rmux_transport::LocalTarget;

        let dir = temp_dir("hostile");
        // Legal on Unix, and fatal to whitespace- or newline-delimited protocols.
        std::fs::write(dir.join("we ird\tname.txt"), "x").unwrap();

        let entries =
            TargetFs::new(LocalTarget::new()).list_dir(&dir.to_string_lossy()).await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "we ird\tname.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
