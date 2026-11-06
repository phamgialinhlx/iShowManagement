//! `GET /api/seed` — a starting workspace for a browser that has none.
//!
//! A browser's session list lives in its own `localStorage`, so a new phone or
//! a cleared iPad opens to an empty rail. `<data dir>/seed.json` is a set of
//! `localStorage` entries the page writes on first boot — only keys it does not
//! already have, and only when it has no workspace at all (`ui/src/main.tsx`).
//!
//! Format: `{"keys": {"<localStorage key>": "<string value>", ...}}`.
//!
//! Read per request, so replacing the file needs no restart. Validated before
//! it is served: a malformed file is a 500 that names the problem, rather than
//! junk written into someone's storage. Authenticated like every `/api` route —
//! a workspace names hosts and folders.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::{AppState, error};

pub const FILE_NAME: &str = "seed.json";
/// Larger than any real workspace; `localStorage` itself is ~5 MB per origin.
pub const MAX_SEED_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Seed {
    keys: BTreeMap<String, String>,
}

pub(crate) async fn serve(State(state): State<Arc<AppState>>) -> Response {
    let path = &state.seed;
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => m,
        _ => return error(StatusCode::NOT_FOUND, format!("no {FILE_NAME} in the server's data directory")),
    };
    if meta.len() > MAX_SEED_BYTES {
        return error(StatusCode::INTERNAL_SERVER_ERROR, format!("{FILE_NAME} is larger than {MAX_SEED_BYTES} bytes"));
    }
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("reading {FILE_NAME}: {e}")),
    };
    let seed: Seed = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{FILE_NAME} is not {{\"keys\": {{\"<key>\": \"<string>\"}}}}: {e}"),
            );
        }
    };
    let mut res = axum::Json(seed).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}
