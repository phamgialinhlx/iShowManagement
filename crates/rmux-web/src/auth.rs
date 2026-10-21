//! Who may use this server.
//!
//! rmux-web serves a shell. **Every `/api/*` request goes through [`authorize`]
//! and nothing else decides** — there is exactly one function to read, to test
//! and, in phase 3, to extend. A route that skipped it would be a terminal for
//! whoever finds the URL.
//!
//! Today the credential is a per-run token (`docs/rmux-web.md` › "Auth"):
//!
//! - `Authorization: Bearer <token>` on every route;
//! - `?token=<token>` **only** on `/api/ws` and `/api/file`, because a browser
//!   cannot set a header on a WebSocket upgrade or on an `<img>` load. The query
//!   form can land in proxy logs, which is why it is limited to the two routes
//!   that cannot do otherwise, and why the token dies with the process.
//!
//! The comparison is constant-time: a byte-by-byte `==` that returns at the
//! first mismatch leaks, through timing, how much of a guess was right.

use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use subtle::ConstantTimeEq;

/// The server's credentials. `Debug` is redacted on purpose: a stray `{:?}` in
/// a log line must not print the token.
pub struct Auth {
    token: Box<[u8]>,
    // PHASE 3 — Cloudflare Access. Add the verifier's configuration here:
    //   access: Option<AccessConfig>,   // team domain, pinned audience (AUD tag),
    //                                   // cached JWKS from <team>/cdn-cgi/access/certs
    // and consult it first in `authorize`. See the marked block there.
}

impl Auth {
    /// Token auth. `token` must be non-empty; [`crate::cli`] guarantees it.
    pub fn token(token: &str) -> Self {
        assert!(!token.is_empty(), "an empty token would admit everyone");
        Self { token: token.as_bytes().into() }
    }

    fn token_matches(&self, presented: &str) -> bool {
        // `ct_eq` on slices of different lengths is `false` without examining the
        // contents; the length of a per-run random token is not a secret.
        bool::from(self.token.as_ref().ct_eq(presented.as_bytes()))
    }
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth").field("token", &"<redacted>").finish()
    }
}

/// Who a request was admitted as. Inserted into the request's extensions, so a
/// handler that one day needs to know (an audit line, a per-user workspace) can
/// read it without re-authenticating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// Presented the per-run token.
    Token,
    // PHASE 3: `Access { email: String, sub: String }` — from a verified JWT.
}

/// Why a request was refused. Both answer `401`; the distinction is for tests
/// and for the one log line, never shown in more detail to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// No credential in any accepted place.
    Missing,
    /// A credential was presented and was wrong.
    Invalid,
}

/// The single decision: may this request reach a handler?
///
/// - `headers` — the request's headers (`Authorization`, and in phase 3
///   `Cf-Access-Jwt-Assertion`).
/// - `query_token` — the `token` query parameter, if any.
/// - `query_allowed` — whether this route may authenticate by query
///   (`/api/ws`, `/api/file` only). Elsewhere a query token is ignored, so it
///   counts as *missing*, not as a valid credential.
pub fn authorize(
    auth: &Auth,
    headers: &HeaderMap,
    query_token: Option<&str>,
    query_allowed: bool,
) -> Result<Principal, Denied> {
    // ---------------------------------------------------------------------
    // PHASE 3 — Cloudflare Access JWT verification slots in HERE.
    //
    // When `auth.access` is configured:
    //   1. read `Cf-Access-Jwt-Assertion` from `headers` (and, for the WS
    //      upgrade, the `CF_Authorization` cookie the browser sends with it);
    //   2. verify the RS256 signature against the team's certs, `aud` against
    //      the pinned audience, `iss` against the team domain, and `exp`/`nbf`;
    //   3. on success return `Ok(Principal::Access { .. })`;
    //      on a present-but-invalid JWT return `Err(Denied::Invalid)` — never
    //      fall through to the token, or a forged header is free to retry.
    // Access in front is not enough on its own (`docs/rmux-web.md`): anything
    // on the host that reaches the port bypasses the tunnel, so the check must
    // live here, on every request and every upgrade.
    // ---------------------------------------------------------------------

    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);

    let presented = match (bearer, query_allowed.then_some(query_token).flatten()) {
        (Some(t), _) => t,
        (None, Some(t)) => t,
        (None, None) => return Err(Denied::Missing),
    };
    if presented.is_empty() {
        return Err(Denied::Missing);
    }
    if auth.token_matches(presented) { Ok(Principal::Token) } else { Err(Denied::Invalid) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn bearer(t: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {t}")).unwrap());
        h
    }

    #[test]
    fn the_right_bearer_is_admitted() {
        let auth = Auth::token("s3cret-token");
        assert_eq!(authorize(&auth, &bearer("s3cret-token"), None, false), Ok(Principal::Token));
    }

    #[test]
    fn a_wrong_or_missing_bearer_is_refused() {
        let auth = Auth::token("s3cret-token");
        assert_eq!(authorize(&auth, &bearer("s3cret-tokem"), None, false), Err(Denied::Invalid));
        assert_eq!(authorize(&auth, &bearer("s3cret"), None, false), Err(Denied::Invalid));
        assert_eq!(authorize(&auth, &bearer(""), None, false), Err(Denied::Missing));
        assert_eq!(authorize(&auth, &HeaderMap::new(), None, false), Err(Denied::Missing));
    }

    #[test]
    fn a_query_token_counts_only_where_it_is_allowed() {
        let auth = Auth::token("s3cret-token");
        let none = HeaderMap::new();
        assert_eq!(authorize(&auth, &none, Some("s3cret-token"), true), Ok(Principal::Token));
        assert_eq!(authorize(&auth, &none, Some("s3cret-token"), false), Err(Denied::Missing));
        assert_eq!(authorize(&auth, &none, Some("nope"), true), Err(Denied::Invalid));
    }

    #[test]
    fn debug_never_prints_the_token() {
        let shown = format!("{:?}", Auth::token("s3cret-token"));
        assert!(!shown.contains("s3cret"), "{shown}");
    }
}
