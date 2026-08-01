//! Pasting an image into a Claude session, including a remote one.
//!
//! ## Why this is not just "paste"
//!
//! Claude Code reads images off the clipboard itself. That works when it is
//! running on your machine and is impossible when it is not: the `claude`
//! process is on a server, and the server has no clipboard, no pasteboard, and
//! no way to reach yours. Nothing about typing harder into the terminal fixes
//! that — the bytes have to physically travel.
//!
//! So they do. The image is written to a file **on the target**, and the path
//! is what goes into Claude's prompt. Claude then reads it the same way it
//! reads any file you mention, which is a capability it already has and which
//! works identically local or remote. That is the whole trick, and it is why
//! the same code path serves both: there is no `if is_local` here, because
//! `Target` already handles that seam.
//!
//! ## Getting the bytes there
//!
//! Through **stdin**, never the command line. A screenshot is routinely a
//! megabyte, base64 inflates it by a third, and `ARG_MAX` on Linux caps a
//! single argument at 128 KiB — so an argv-shaped version of this would work
//! for a small icon and fail on anything real, which is the worst kind of bug.
//! `exec_with_input` pipes to a remote `cat`, exactly as the file writer does.
//!
//! ## Where it lands
//!
//! `~/.rmux/pastes`, not the project. A screenshot dropped into a conversation
//! is about the work, not part of it — writing it into someone's repository
//! would show up in `git status` and eventually get committed by accident.

use rmux_transport::{CommandSpec, Tty};
use serde::Serialize;
use tauri::State;

use crate::claude::ClaudeStore;
use crate::terminal::TargetRef;

/// Where the image ended up, and what to say about it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pasted {
    /// Absolute path **on the target**. This is what goes into the prompt.
    pub path: String,
    pub bytes: usize,
}

/// The largest image accepted.
///
/// Not arbitrary: this crosses the IPC bridge as base64, gets decoded here, and
/// is then held in memory while it streams to the far side. A phone screenshot
/// is ~3 MB and a display-resolution capture ~8 MB, so 24 MiB covers the real
/// cases with room to spare while refusing the "pasted a video frame by
/// accident" ones before they reach the network.
const MAX_BYTES: usize = 24 * 1024 * 1024;

/// Write a pasted image to the target and return its path.
#[tauri::command]
pub async fn claude_paste_image(
    store: State<'_, ClaudeStore>,
    target: TargetRef,
    // Base64, no data-URI prefix. The webview cannot hand Rust raw bytes.
    data: String,
    // `png`, `jpeg`, … Used only for the extension.
    kind: String,
) -> Result<Pasted, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|e| format!("that did not decode as an image: {e}"))?;

    if bytes.is_empty() {
        return Err("the clipboard image was empty".into());
    }
    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "that image is {} MB; the limit is {} MB",
            bytes.len() / 1_048_576,
            MAX_BYTES / 1_048_576
        ));
    }

    // Extension only, and whitelisted. It is interpolated into a shell line, so
    // anything the webview could put here has to be something a filename can
    // safely contain — and a media type from a paste event is not a value to
    // trust just because it usually looks like "png".
    let extension = match kind.rsplit('/').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "png",
        "jpeg" | "jpg" => "jpg",
        "gif" => "gif",
        "webp" => "webp",
        _ => "png",
    };

    let resolved = crate::claude::resolve(&store, &target).await?;

    // One round trip: make the directory, mint a name, swallow stdin into it,
    // then print where it went.
    //
    // `$HOME` is expanded by the *remote* shell here, which is correct
    // precisely because this whole script is one quoted argument to `sh -c`.
    // Quoting the path instead would make it a literal directory called
    // `$HOME` — the trap `provision::home_script` exists to avoid.
    //
    // `0700` on the directory: a screenshot pasted into a conversation is
    // frequently of something private, and the default umask on a shared dev
    // box routinely leaves new directories world-readable.
    let script = format!(
        r#"set -e
d="$HOME/.rmux/pastes"
mkdir -p "$d"
chmod 700 "$d"
f="$d/paste-$(date +%Y%m%d-%H%M%S)-$$.{extension}"
cat > "$f"
chmod 600 "$f"
printf %s "$f""#
    );

    let out = resolved
        .exec_with_input(
            &CommandSpec::new("sh").arg("-c").arg(&script).tty(Tty::None),
            &bytes,
        )
        .await
        .map_err(|e| e.to_string())?;

    let path = out.stdout_or_err().map_err(|e| e.to_string())?.trim().to_owned();
    if path.is_empty() {
        return Err("the target accepted the image but did not say where it went".into());
    }

    Ok(Pasted { path, bytes: bytes.len() })
}
