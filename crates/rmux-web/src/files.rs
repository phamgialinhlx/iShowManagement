//! `GET /api/file?path=` — files the UI loads by URL.
//!
//! The desktop equivalent is Tauri's asset protocol, scoped to the app's
//! backgrounds directory; this is scoped **exactly** the same way. An unscoped
//! file endpoint is a read of the whole host (`docs/rmux-web.md` › "Files").
//!
//! The check is on the *canonical* path, so `..`, symlinks and doubled slashes
//! all resolve before the prefix comparison — a lexical check on the string as
//! sent would be defeated by any of them.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::{AppState, error};

#[derive(Deserialize)]
pub(crate) struct FileQuery {
    path: Option<String>,
}

pub(crate) async fn serve(State(state): State<Arc<AppState>>, Query(q): Query<FileQuery>) -> Response {
    let Some(path) = q.path.filter(|p| !p.is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "missing ?path=");
    };
    // Nothing has been put there yet: nothing can be served from it.
    let Ok(root) = tokio::fs::canonicalize(&state.backgrounds).await else {
        return error(StatusCode::NOT_FOUND, "no such file");
    };
    let Ok(real) = tokio::fs::canonicalize(&path).await else {
        return error(StatusCode::NOT_FOUND, "no such file");
    };
    if !real.starts_with(&root) || real == root {
        // The path is the client's own input; not echoed into the log.
        tracing::warn!("refused a file request outside the backgrounds directory");
        return error(StatusCode::FORBIDDEN, "only background pictures are served");
    }
    match tokio::fs::metadata(&real).await {
        Ok(m) if m.is_file() => {}
        _ => return error(StatusCode::NOT_FOUND, "no such file"),
    }
    let bytes = match tokio::fs::read(&real).await {
        Ok(b) => b,
        Err(_) => return error(StatusCode::NOT_FOUND, "no such file"),
    };

    let mut res = Body::from(bytes).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&real)));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // An SVG opened directly would otherwise run script *on this origin*, where
    // the token lives in sessionStorage. `sandbox` gives it an opaque origin.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; sandbox"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
    res
}

fn content_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("bmp") => "image/bmp",
        Some("heic") => "image/heic",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
}
