//! The static UI: vite's `dist/`, with `index.html` for every other path.
//!
//! `index.html` is served with `<meta name="rmux-shell" content="web">`
//! injected, which is how `bridge.ts` knows it is in rmux-web even without a
//! token (`isWeb`). It is read per request, so rebuilding the UI needs no
//! restart, and is never cached, so a rebuilt bundle's new asset names reach
//! the page.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use tower::ServiceExt as _;
use tower_http::services::ServeDir;

use crate::{AppState, error};

pub const SHELL_META: &str = r#"<meta name="rmux-shell" content="web">"#;

pub(crate) async fn serve(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if req.uri().path() == "/api" || req.uri().path().starts_with("/api/") {
        // Defence in depth: /api is routed (and authenticated) before this.
        return error(StatusCode::NOT_FOUND, "no such endpoint");
    }
    let Some(dir) = state.ui_dir.clone() else {
        return (StatusCode::NOT_FOUND, "rmux-web was started without --ui; only /api is served").into_response();
    };
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = req.uri().path().to_owned();
    if path != "/" && path != "/index.html" {
        // ServeDir refuses `..` and serves nothing outside `dir`.
        let served = ServeDir::new(&dir).append_index_html_on_directories(false).oneshot(req).await;
        if let Ok(res) = served
            && res.status() != StatusCode::NOT_FOUND
        {
            let mut res = res.map(Body::new);
            res.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            return res;
        }
    }
    index(&dir).await
}

async fn index(dir: &std::path::Path) -> Response {
    let Ok(html) = tokio::fs::read_to_string(dir.join("index.html")).await else {
        return (StatusCode::NOT_FOUND, "index.html is missing from --ui").into_response();
    };
    let mut res = Html(inject_shell_meta(&html)).into_response();
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // The page's URL carries the token in its fragment on first load; a
    // fragment is never sent in a Referer, but say so explicitly anyway.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

/// Put [`SHELL_META`] right after `<head>`, once.
pub fn inject_shell_meta(html: &str) -> String {
    if html.contains(SHELL_META) {
        return html.to_owned();
    }
    let lower = html.to_ascii_lowercase();
    if let Some(start) = lower.find("<head")
        && let Some(end) = lower[start..].find('>')
    {
        let at = start + end + 1;
        return format!("{}\n    {SHELL_META}{}", &html[..at], &html[at..]);
    }
    format!("{SHELL_META}\n{html}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_goes_inside_head_once() {
        let html = "<!doctype html><html><head lang=\"en\"><title>x</title></head></html>";
        let out = inject_shell_meta(html);
        assert!(out.contains("<head lang=\"en\">\n    <meta name=\"rmux-shell\" content=\"web\"><title>"), "{out}");
        assert_eq!(inject_shell_meta(&out), out);
    }
}
