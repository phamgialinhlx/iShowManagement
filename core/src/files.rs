//! File browser: list a directory, stat a path, preview text/images, and
//! download files/folders. Everything runs as small NUL-delimited shell
//! commands over `ssh::exec` (so it works local and remote); downloads use
//! rsync→scp for remote and tar for folders. Mirrors
//! `references/tsmanager/server/file-routes.js`.

use std::path::Path as FsPath;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::io::ReaderStream;

use crate::api::{AppState, LOCAL_ID};
use crate::security::{safe_name, shell_quote as q};
use crate::ssh::{self, Target};

const TEXT_LIMIT: u64 = 512 * 1024;
const IMAGE_LIMIT: u64 = 4 * 1024 * 1024;

const TEXT_MIME: &[&str] = &[
    "application/json",
    "application/javascript",
    "application/x-javascript",
    "application/xml",
    "application/x-sh",
    "application/x-shellscript",
    "application/x-yaml",
    "application/yaml",
];
const TEXT_EXT: &[&str] = &[
    "conf", "config", "css", "csv", "env", "gitignore", "html", "ini", "js", "json", "jsx", "log",
    "md", "mjs", "py", "rb", "rs", "sh", "sql", "toml", "ts", "tsx", "txt", "xml", "yaml", "yml",
];

#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    path: String,
}

fn target(id: &str) -> Option<Target<'_>> {
    if id == LOCAL_ID {
        Some(Target::Local)
    } else if safe_name(id) {
        Some(Target::Remote(id))
    } else {
        None
    }
}

fn is_local(id: &str) -> bool {
    id == LOCAL_ID
}

// ------------------------------------------------------------ list ----

#[derive(Serialize)]
pub struct Entry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    size: u64,
    mtime: f64,
}

#[derive(Serialize)]
pub struct Listing {
    path: String,
    parent: Option<String>,
    entries: Vec<Entry>,
}

fn path_expr(value: &str) -> String {
    if value.is_empty() {
        r#""${HOME:-.}""#.to_string()
    } else {
        q(value)
    }
}

fn list_command(target_path: &str) -> String {
    [
        format!("p={}", path_expr(target_path)),
        r#"if [ ! -e "$p" ]; then echo "not found" >&2; exit 2; fi"#.into(),
        r#"if [ ! -d "$p" ]; then echo "not a directory" >&2; exit 3; fi"#.into(),
        r#"dir=$(cd "$p" 2>/dev/null && pwd -P) || exit 4"#.into(),
        r#"printf "DIR\0%s\0" "$dir""#.into(),
        r#"find "$dir" -mindepth 1 -maxdepth 1 -printf "%f\0%p\0%y\0%s\0%T@\0" 2>/dev/null"#.into(),
    ]
    .join("; ")
}

fn parse_listing(stdout: &str) -> Result<Listing, String> {
    let parts: Vec<&str> = stdout.split('\0').collect();
    if parts.first() != Some(&"DIR") {
        return Err("unexpected file list response".into());
    }
    let dir = parts.get(1).copied().unwrap_or("").to_string();

    let mut entries = Vec::new();
    let mut i = 2;
    while i + 4 < parts.len() {
        let name = parts[i];
        if !name.is_empty() {
            entries.push(Entry {
                name: name.to_string(),
                path: parts[i + 1].to_string(),
                kind: match parts[i + 2] {
                    "d" => "dir",
                    "l" => "link",
                    _ => "file",
                }
                .to_string(),
                size: parts[i + 3].parse().unwrap_or(0),
                mtime: parts[i + 4].parse().unwrap_or(0.0),
            });
        }
        i += 5;
    }
    entries.sort_by(|a, b| {
        let ad = a.kind == "dir";
        let bd = b.kind == "dir";
        bd.cmp(&ad).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let parent = if !dir.is_empty() && dir != "/" {
        Some(
            FsPath::new(&dir)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into()),
        )
    } else {
        None
    };
    Ok(Listing { path: dir, parent, entries })
}

pub async fn list(
    State(_): State<AppState>,
    Path(id): Path<String>,
    Query(pq): Query<PathQuery>,
) -> Result<Json<Listing>, (StatusCode, Json<Value>)> {
    let tgt = target(&id).ok_or((StatusCode::BAD_REQUEST, Json(json!({"error":"bad server id"}))))?;
    let r = ssh::exec(tgt, &list_command(&pq.path), Duration::from_secs(15)).await;
    if !r.ok {
        let msg = trim300(if r.stderr.trim().is_empty() { "list failed" } else { &r.stderr });
        return Err((StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))));
    }
    parse_listing(&r.stdout).map(Json).map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))))
}

// ------------------------------------------------------------ stat ----

struct Stat {
    kind: String,
    size: u64,
    mime: String,
    name: String,
    path: String,
}

fn stat_command(target_path: &str) -> String {
    [
        format!("p={}", q(target_path)),
        r#"if [ ! -e "$p" ]; then echo "not found" >&2; exit 2; fi"#.into(),
        "type=file".into(),
        r#"[ -d "$p" ] && type=dir"#.into(),
        r#"size=$(wc -c < "$p" 2>/dev/null | tr -d " " || printf 0)"#.into(),
        r#"mime=$(file -b --mime-type "$p" 2>/dev/null || true)"#.into(),
        r#"[ -n "$mime" ] || mime=application/octet-stream"#.into(),
        r#"base=$(basename "$p")"#.into(),
        r#"real=$(cd "$(dirname "$p")" 2>/dev/null && printf "%s/%s" "$(pwd -P)" "$base")"#.into(),
        r#"printf "STAT\0%s\0%s\0%s\0%s\0%s\0" "$type" "$size" "$mime" "$base" "$real""#.into(),
    ]
    .join("; ")
}

fn parse_stat(stdout: &str) -> Result<Stat, String> {
    let p: Vec<&str> = stdout.split('\0').collect();
    if p.first() != Some(&"STAT") {
        return Err("unexpected file stat response".into());
    }
    Ok(Stat {
        kind: p.get(1).copied().unwrap_or("file").to_string(),
        size: p.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
        mime: p.get(3).filter(|s| !s.is_empty()).copied().unwrap_or("application/octet-stream").to_string(),
        name: p.get(4).filter(|s| !s.is_empty()).copied().unwrap_or("download").to_string(),
        path: p.get(5).copied().unwrap_or("").to_string(),
    })
}

async fn stat_path(tgt: Target<'_>, target_path: &str) -> Result<Stat, String> {
    let r = ssh::exec(tgt, &stat_command(target_path), Duration::from_secs(10)).await;
    if !r.ok {
        return Err(trim300(if r.stderr.trim().is_empty() { "stat failed" } else { &r.stderr }));
    }
    parse_stat(&r.stdout)
}

// --------------------------------------------------------- preview ----

pub fn is_text_preview(name: &str, mime: &str) -> bool {
    if mime.starts_with("text/") || TEXT_MIME.contains(&mime) {
        return true;
    }
    let ext = FsPath::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        // `.gitignore` etc. have no "extension" — fall back to the trimmed name.
        .unwrap_or_else(|| name.trim_start_matches('.').to_lowercase());
    TEXT_EXT.contains(&ext.as_str())
}

pub fn is_image_preview(mime: &str) -> bool {
    matches!(
        mime.to_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/svg+xml"
    )
}

fn preview_limit(s: &Stat) -> u64 {
    if is_image_preview(&s.mime) {
        IMAGE_LIMIT
    } else if is_text_preview(&s.name, &s.mime) {
        TEXT_LIMIT
    } else {
        0
    }
}

fn base64_command(target_path: &str, limit: u64) -> String {
    [
        format!("p={}", q(target_path)),
        r#"size=$(wc -c < "$p" 2>/dev/null | tr -d " " || printf 0)"#.into(),
        format!(r#"if [ "$size" -gt {limit} ]; then echo "too large" >&2; exit 7; fi"#),
        // Read via stdin so it works on both GNU and BSD base64 (macOS `base64
        // "$p"` rejects a filename argument).
        r#"base64 < "$p""#.into(),
    ]
    .join("; ")
}

pub async fn view(
    State(_): State<AppState>,
    Path(id): Path<String>,
    Query(pq): Query<PathQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let tgt = target(&id).ok_or(err(StatusCode::BAD_REQUEST, "bad server id"))?;
    if pq.path.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "path is required"));
    }
    let stat = stat_path(tgt, &pq.path).await.map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    if stat.kind != "file" {
        return Err(err(StatusCode::BAD_REQUEST, "only regular files can be previewed"));
    }

    let limit = preview_limit(&stat);
    let base = json!({ "size": stat.size, "mime": stat.mime, "name": stat.name, "path": stat.path });
    if limit == 0 {
        return Ok(Json(with_type(base, "unsupported", None)));
    }
    if stat.size > limit {
        let mut v = with_type(base, "too_large", None);
        v["limit"] = json!(limit);
        return Ok(Json(v));
    }

    let r = ssh::exec(tgt, &base64_command(&pq.path, limit), Duration::from_secs(20)).await;
    if !r.ok {
        return Err(err(StatusCode::BAD_REQUEST, &trim300(if r.stderr.trim().is_empty() { "read failed" } else { &r.stderr })));
    }
    let cleaned: String = r.stdout.split_whitespace().collect();
    if is_image_preview(&stat.mime) {
        let data_url = format!("data:{};base64,{}", stat.mime, cleaned);
        let mut v = with_type(base, "image", None);
        v["dataUrl"] = json!(data_url);
        return Ok(Json(v));
    }
    // Text: decode base64 to bytes → utf8 (lossy).
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(cleaned.as_bytes()).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Json(with_type(base, "text", Some(text))))
}

fn with_type(mut base: Value, ty: &str, text: Option<String>) -> Value {
    base["type"] = json!(ty);
    if let Some(t) = text {
        base["text"] = json!(t);
    }
    base
}

// -------------------------------------------------------- download ----

fn safe_download_name(name: &str, fallback: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == '\0' { '_' } else { c })
        .collect();
    let clean = clean.trim();
    if clean.is_empty() {
        fallback.to_string()
    } else {
        clean.to_string()
    }
}

fn unique_tmp(suffix: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ism-dl-{}-{}-{}", std::process::id(), n, suffix))
}

async fn command_exists(cmd: &str) -> bool {
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", q(cmd)))
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Stream a response body from an in-memory buffer with download headers.
fn download_bytes(bytes: Vec<u8>, filename: &str, mime: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", safe_download_name(filename, "download")),
            ),
        ],
        bytes,
    )
        .into_response()
}

pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(pq): Query<PathQuery>,
) -> Response {
    let Some(tgt) = target(&id) else {
        return err(StatusCode::BAD_REQUEST, "bad server id").into_response();
    };
    if pq.path.is_empty() {
        return err(StatusCode::BAD_REQUEST, "path is required").into_response();
    }
    let stat = match stat_path(tgt, &pq.path).await {
        Ok(s) => s,
        Err(e) => return err(StatusCode::BAD_REQUEST, &e).into_response(),
    };
    let src = if stat.path.is_empty() { pq.path.clone() } else { stat.path.clone() };
    let name = safe_download_name(&stat.name, if stat.kind == "dir" { "folder" } else { "file" });

    match download_impl(&state, &id, tgt, &src, &stat.kind, &name).await {
        Ok(resp) => resp,
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &e).into_response(),
    }
}

async fn download_impl(
    _state: &AppState,
    id: &str,
    _tgt: Target<'_>,
    src: &str,
    kind: &str,
    name: &str,
) -> Result<Response, String> {
    if is_local(id) {
        if kind == "dir" {
            let archive = tar_gz_dir(src, name).await?;
            let bytes = read_and_remove(&archive).await?;
            return Ok(download_bytes(bytes, &format!("{name}.tar.gz"), "application/gzip"));
        }
        // Stream the local file straight from disk.
        let file = tokio::fs::File::open(src).await.map_err(|e| e.to_string())?;
        let stream = ReaderStream::new(file);
        return Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
            ],
            Body::from_stream(stream),
        )
            .into_response());
    }

    // Remote: stage into a temp dir via rsync (preferred) or scp, then serve.
    let alias = match _tgt {
        Target::Remote(a) => a,
        Target::Local => unreachable!(),
    };
    let stage = unique_tmp("stage");
    tokio::fs::create_dir_all(&stage).await.map_err(|e| e.to_string())?;
    let result = stage_remote(alias, src, &stage).await;
    let served = match result {
        Ok(()) => serve_staged(&stage, name).await,
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_dir_all(&stage).await;
    served
}

async fn stage_remote(alias: &str, src: &str, stage: &std::path::Path) -> Result<(), String> {
    let opts = ssh::transfer_opts(alias);
    let remote = format!("{alias}:{}", src);

    if command_exists("rsync").await {
        let ssh_e = format!("ssh {}", opts.iter().map(|o| q(o)).collect::<Vec<_>>().join(" "));
        let out = tokio::process::Command::new("rsync")
            .args(["-az", "--protect-args", "-e", &ssh_e, &remote])
            .arg(format!("{}/", stage.display()))
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(());
        }
        // fall through to scp
    }

    let mut cmd = tokio::process::Command::new("scp");
    cmd.arg("-r");
    for o in &opts {
        cmd.arg(o);
    }
    cmd.arg(&remote).arg(stage);
    let out = cmd.output().await.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(trim300(&String::from_utf8_lossy(&out.stderr)))
    }
}

async fn serve_staged(stage: &std::path::Path, name: &str) -> Result<Response, String> {
    let mut entries = tokio::fs::read_dir(stage).await.map_err(|e| e.to_string())?;
    let first = entries.next_entry().await.map_err(|e| e.to_string())?.ok_or("download produced no files")?;
    let path = first.path();
    let meta = tokio::fs::metadata(&path).await.map_err(|e| e.to_string())?;
    let entry_name = first.file_name().to_string_lossy().into_owned();
    if meta.is_dir() {
        let archive = tar_gz_dir(&path.to_string_lossy(), &entry_name).await?;
        let bytes = read_and_remove(&archive).await?;
        Ok(download_bytes(bytes, &format!("{}.tar.gz", safe_download_name(&entry_name, "folder")), "application/gzip"))
    } else {
        let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
        Ok(download_bytes(bytes, &safe_download_name(&entry_name, name), "application/octet-stream"))
    }
}

async fn tar_gz_dir(dir: &str, name: &str) -> Result<std::path::PathBuf, String> {
    let archive = unique_tmp(&format!("{}.tar.gz", safe_download_name(name, "folder")));
    let parent = FsPath::new(dir).parent().unwrap_or(FsPath::new(".")).to_path_buf();
    let base = FsPath::new(dir).file_name().map(|b| b.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into());
    let out = tokio::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&parent)
        .arg(&base)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(archive)
    } else {
        Err(trim300(&String::from_utf8_lossy(&out.stderr)))
    }
}

async fn read_and_remove(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(path).await;
    Ok(bytes)
}

// ------------------------------------------------------------ util ----

fn trim300(s: &str) -> String {
    s.trim().chars().take(300).collect()
}

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}

/// Decode bytes as text for display: the lossy string plus whether the input
/// was *strictly* valid UTF-8. The strict flag drives editability — saving
/// lossy text would corrupt the original bytes, so invalid-UTF-8 files are
/// shown read-only even when they look like text.
fn decode_text(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), true),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), false),
    }
}

/// A file is editable when it's a regular text file under the size limit with
/// valid UTF-8 (see the spec's editable rule).
fn is_editable(stat: &Stat, valid_utf8: bool) -> bool {
    stat.kind == "file"
        && stat.size <= TEXT_LIMIT
        && is_text_preview(&stat.name, &stat.mime)
        && valid_utf8
}

/// The remote save command: stream stdin into a temp in the same directory
/// (so the `mv` is an atomic rename), preserve the original mode, then move
/// it over the target. `chmod --reference` is GNU — consistent with `list`'s
/// `find -printf` assumption.
fn save_command(target_path: &str) -> String {
    [
        format!("p={}", q(target_path)),
        r#"dir=$(dirname "$p")"#.into(),
        r#"tmp=$(mktemp "$dir/.ism-XXXXXX")"#.into(),
        r#"cat > "$tmp""#.into(),
        r#"chmod --reference="$p" "$tmp" 2>/dev/null"#.into(),
        r#"mv -f "$tmp" "$p""#.into(),
    ]
    .join("; ")
}

/// Write `bytes` to `path` atomically on the local machine: a temp file in the
/// same directory, the original mode copied onto it, then an atomic rename.
/// Assumes `path` already exists (the caller stats it first).
async fn atomic_write_local(path: &str, bytes: &[u8]) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    let base = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".ism-{}-{}-{}", std::process::id(), n, base));
    tokio::fs::write(&tmp, bytes).await.map_err(|e| e.to_string())?;
    // Preserve the original mode across the inode swap.
    let perms = tokio::fs::metadata(path).await.map_err(|e| e.to_string())?.permissions();
    tokio::fs::set_permissions(&tmp, perms).await.map_err(|e| e.to_string())?;
    tokio::fs::rename(&tmp, path).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_listing_sorts_dirs_first() {
        // DIR \0 /home/u \0 then 5-tuples per entry.
        let out = "DIR\0/home/u\0zeta.txt\0/home/u/zeta.txt\0f\010\01700000000\0apps\0/home/u/apps\0d\04096\01700000001\0Beta\0/home/u/Beta\0f\05\01700000002\0";
        let l = parse_listing(out).unwrap();
        assert_eq!(l.path, "/home/u");
        assert_eq!(l.parent.as_deref(), Some("/home"));
        // dir first, then case-insensitive name order
        assert_eq!(l.entries[0].name, "apps");
        assert_eq!(l.entries[0].kind, "dir");
        assert_eq!(l.entries[1].name, "Beta");
        assert_eq!(l.entries[2].name, "zeta.txt");
        assert_eq!(l.entries[2].size, 10);
    }

    #[test]
    fn parse_stat_fields() {
        let out = "STAT\0file\01234\0text/plain\0notes.txt\0/home/u/notes.txt\0";
        let s = parse_stat(out).unwrap();
        assert_eq!(s.kind, "file");
        assert_eq!(s.size, 1234);
        assert_eq!(s.mime, "text/plain");
        assert_eq!(s.name, "notes.txt");
        assert_eq!(s.path, "/home/u/notes.txt");
    }

    #[test]
    fn preview_type_detection() {
        assert!(is_text_preview("a.rs", "application/octet-stream"));
        assert!(is_text_preview("x", "text/x-python"));
        assert!(is_text_preview(".gitignore", "application/octet-stream"));
        assert!(is_text_preview("data.json", "application/json"));
        assert!(!is_text_preview("a.bin", "application/octet-stream"));
        assert!(is_image_preview("image/png"));
        assert!(is_image_preview("image/svg+xml"));
        assert!(!is_image_preview("application/pdf"));
    }

    #[test]
    fn safe_download_name_strips_separators() {
        assert_eq!(safe_download_name("a/b\\c", "x"), "a_b_c");
        assert_eq!(safe_download_name("  ", "fallback"), "fallback");
    }

    #[tokio::test]
    async fn atomic_write_local_replaces_content_and_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ism-awt-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("target.txt");
        tokio::fs::write(&path, b"old").await.unwrap();
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
        atomic_write_local(&path.to_string_lossy(), b"new content")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "new content");
        let mode = tokio::fs::metadata(&path).await.unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode should be preserved across rename");
        // No leftover temp file in the directory.
        let mut rd = tokio::fs::read_dir(&dir).await.unwrap();
        let mut left = Vec::new();
        while let Some(e) = rd.next_entry().await.unwrap() {
            left.push(e);
        }
        assert_eq!(left.len(), 1, "only the target should remain");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn decode_text_valid_and_invalid_utf8() {
        let (text, ok) = decode_text(b"hello");
        assert_eq!(text, "hello");
        assert!(ok);
        let (text, ok) = decode_text(&[0xff, 0xfe, 0xfd]);
        assert!(!ok);
        assert!(text.contains('\u{fffd}'), "lossy text has replacement char");
    }

    #[test]
    fn is_editable_enforces_all_conditions() {
        let mk = |kind: &str, size: u64, name: &str, mime: &str| Stat {
            kind: kind.into(),
            size,
            mime: mime.into(),
            name: name.into(),
            path: name.into(),
        };
        assert!(is_editable(&mk("file", 100, "a.txt", "text/plain"), true));
        assert!(!is_editable(&mk("dir", 100, "a", "text/plain"), true), "dirs not editable");
        assert!(
            !is_editable(&mk("file", TEXT_LIMIT + 1, "a.txt", "text/plain"), true),
            "over limit not editable"
        );
        assert!(
            !is_editable(&mk("file", 100, "a.bin", "application/octet-stream"), true),
            "binary not editable"
        );
        assert!(
            !is_editable(&mk("file", 100, "a.txt", "text/plain"), false),
            "invalid utf8 not editable"
        );
    }

    #[test]
    fn save_command_shape() {
        let cmd = save_command("/home/u/notes.txt");
        assert!(cmd.contains("/home/u/notes.txt"), "path present: {cmd}");
        assert!(cmd.contains("mktemp"), "writes to a temp: {cmd}");
        assert!(cmd.contains("cat > "), "reads stdin into temp: {cmd}");
        assert!(cmd.contains("chmod --reference="), "preserves mode: {cmd}");
        assert!(cmd.contains("mv -f "), "atomic rename: {cmd}");
    }
}
