//! Who may use this server.
//!
//! rmux-web serves a shell. **Every `/api/*` request goes through [`authorize`]
//! and nothing else decides** — there is exactly one function to read and to
//! test. A route that skipped it would be a terminal for
//! whoever finds the URL.
//!
//! Two credentials, decided here and nowhere else (`docs/rmux-web.md` › "Auth"):
//!
//! 1. **A Cloudflare Access assertion**, when the server was started with
//!    `--access-team`/`--access-aud`: `Cf-Access-Jwt-Assertion` (which
//!    Cloudflare injects on every proxied request, WebSocket upgrades
//!    included), or failing the header the `CF_Authorization` cookie. Verified
//!    by [`crate::access`]. **A present-but-invalid assertion is refused and
//!    never falls through to the token** — otherwise a forged header costs an
//!    attacker nothing, and a revoked identity keeps working for whoever also
//!    holds the token.
//! 2. **The per-run token**, when no assertion is present — still a valid
//!    credential on loopback and through an SSH tunnel:
//!    - `Authorization: Bearer <token>` on every route;
//!    - `?token=<token>` **only** on `/api/ws` and `/api/file`, because a
//!      browser cannot set a header on a WebSocket upgrade or on an `<img>`
//!      load. The query form can land in proxy logs, which is why it is
//!      limited to the two routes that cannot do otherwise, and why the token
//!      dies with the process.
//!
//! The token comparison is constant-time: a byte-by-byte `==` that returns at
//! the first mismatch leaks, through timing, how much of a guess was right.

use std::sync::Arc;

use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE};
use subtle::ConstantTimeEq;

use crate::access::Access;

/// The header Cloudflare Access puts the assertion in.
pub const ACCESS_HEADER: &str = "cf-access-jwt-assertion";
/// The cookie the browser carries the same assertion in.
pub const ACCESS_COOKIE: &str = "CF_Authorization";

/// The server's credentials. `Debug` is redacted on purpose: a stray `{:?}` in
/// a log line must not print the token.
pub struct Auth {
    token: Box<[u8]>,
    /// Cloudflare Access, when configured. Consulted first in [`authorize`].
    access: Option<Arc<Access>>,
}

impl Auth {
    /// Token auth. `token` must be non-empty; [`crate::cli`] guarantees it.
    pub fn token(token: &str) -> Self {
        assert!(!token.is_empty(), "an empty token would admit everyone");
        Self { token: token.as_bytes().into(), access: None }
    }

    /// Also accept (and check) Cloudflare Access assertions.
    pub fn with_access(mut self, access: Option<Arc<Access>>) -> Self {
        self.access = access;
        self
    }

    fn token_matches(&self, presented: &str) -> bool {
        // `ct_eq` on slices of different lengths is `false` without examining the
        // contents; the length of a per-run random token is not a secret.
        bool::from(self.token.as_ref().ct_eq(presented.as_bytes()))
    }
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth").field("token", &"<redacted>").field("access", &self.access).finish()
    }
}

/// Who a request was admitted as. Inserted into the request's extensions, so a
/// handler that one day needs to know (an audit line, a per-user workspace) can
/// read it without re-authenticating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    /// Presented the per-run token.
    Token,
    /// Presented a verified Cloudflare Access assertion.
    Access { email: String, sub: String },
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

/// A refusal: the kind, and a fixed reason for the log line. The reason is a
/// constant string — never anything taken from the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Denial {
    pub kind: Denied,
    pub reason: &'static str,
}

fn deny(kind: Denied, reason: &'static str) -> Denial {
    Denial { kind, reason }
}

/// The Access assertion a request carries: the header, failing that the cookie.
fn access_assertion(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(ACCESS_HEADER) {
        // Present but not text is still *present*: refused, not skipped.
        return Some(v.to_str().unwrap_or("").to_owned());
    }
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == ACCESS_COOKIE).then(|| value.trim().to_owned())
        })
}

/// The single decision: may this request reach a handler?
///
/// - `headers` — the request's headers (`Cf-Access-Jwt-Assertion`, `Cookie`,
///   `Authorization`).
/// - `query_token` — the `token` query parameter, if any.
/// - `query_allowed` — whether this route may authenticate by query
///   (`/api/ws`, `/api/file` only). Elsewhere a query token is ignored, so it
///   counts as *missing*, not as a valid credential.
///
/// Async only because an assertion signed by a key not yet seen triggers a
/// (rate-limited) refetch of the team's keys.
pub async fn authorize(
    auth: &Auth,
    headers: &HeaderMap,
    query_token: Option<&str>,
    query_allowed: bool,
) -> Result<Principal, Denial> {
    // Access first. Without Access configured an assertion header means
    // nothing to us and is ignored — the token decides, as before.
    if let Some(access) = &auth.access
        && let Some(jwt) = access_assertion(headers)
    {
        // Present: it decides, either way. Never falls through to the token.
        return match access.verify(&jwt).await {
            Ok(id) => {
                tracing::debug!(email = %id.email, "admitted through Cloudflare Access");
                Ok(Principal::Access { email: id.email, sub: id.sub })
            }
            Err(reason) => Err(deny(Denied::Invalid, reason)),
        };
    }

    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);

    let presented = match (bearer, query_allowed.then_some(query_token).flatten()) {
        (Some(t), _) => t,
        (None, Some(t)) => t,
        (None, None) => return Err(deny(Denied::Missing, "no credential")),
    };
    if presented.is_empty() {
        return Err(deny(Denied::Missing, "empty token"));
    }
    if auth.token_matches(presented) {
        Ok(Principal::Token)
    } else {
        Err(deny(Denied::Invalid, "wrong token"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::testkit::*;
    use axum::http::HeaderValue;

    fn bearer(t: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {t}")).unwrap());
        h
    }

    /// `authorize`, blocking, with only the refusal's kind kept.
    fn check(auth: &Auth, h: &HeaderMap, q: Option<&str>, allowed: bool) -> Result<Principal, Denied> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(authorize(auth, h, q, allowed)).map_err(|d| d.kind)
    }

    #[test]
    fn the_right_bearer_is_admitted() {
        let auth = Auth::token("s3cret-token");
        assert_eq!(check(&auth, &bearer("s3cret-token"), None, false), Ok(Principal::Token));
    }

    #[test]
    fn a_wrong_or_missing_bearer_is_refused() {
        let auth = Auth::token("s3cret-token");
        assert_eq!(check(&auth, &bearer("s3cret-tokem"), None, false), Err(Denied::Invalid));
        assert_eq!(check(&auth, &bearer("s3cret"), None, false), Err(Denied::Invalid));
        assert_eq!(check(&auth, &bearer(""), None, false), Err(Denied::Missing));
        assert_eq!(check(&auth, &HeaderMap::new(), None, false), Err(Denied::Missing));
    }

    #[test]
    fn a_query_token_counts_only_where_it_is_allowed() {
        let auth = Auth::token("s3cret-token");
        let none = HeaderMap::new();
        assert_eq!(check(&auth, &none, Some("s3cret-token"), true), Ok(Principal::Token));
        assert_eq!(check(&auth, &none, Some("s3cret-token"), false), Err(Denied::Missing));
        assert_eq!(check(&auth, &none, Some("nope"), true), Err(Denied::Invalid));
    }

    #[test]
    fn debug_never_prints_the_token() {
        let shown = format!("{:?}", Auth::token("s3cret-token").with_access(Some(verifier(&[]))));
        assert!(!shown.contains("s3cret"), "{shown}");
    }

    // ---------------------------------------------------------- Cloudflare Access

    const TOKEN: &str = "s3cret-token-0123";

    fn access_auth(emails: &[&str]) -> Auth {
        Auth::token(TOKEN).with_access(Some(verifier(emails)))
    }

    fn assertion(jwt: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(ACCESS_HEADER, HeaderValue::from_str(jwt).unwrap());
        h
    }

    fn admitted(email: &str) -> Result<Principal, Denied> {
        Ok(Principal::Access { email: email.into(), sub: "user-sub-1".into() })
    }

    #[test]
    fn a_valid_assertion_is_an_access_principal() {
        let auth = access_auth(&[]);
        let jwt = sign(KEY_A, "kid-a", &claims("alice@example.com"));
        assert_eq!(check(&auth, &assertion(&jwt), None, false), admitted("alice@example.com"));
    }

    #[test]
    fn the_cookie_is_read_when_the_header_is_absent() {
        let auth = access_auth(&[]);
        let jwt = sign(KEY_A, "kid-a", &claims("alice@example.com"));
        let mut h = HeaderMap::new();
        h.insert(COOKIE, HeaderValue::from_str(&format!("other=1; {ACCESS_COOKIE}={jwt}; x=y")).unwrap());
        assert_eq!(check(&auth, &h, None, true), admitted("alice@example.com"));
    }

    #[test]
    fn every_bad_assertion_is_invalid() {
        let auth = access_auth(&[]);
        let mut cases: Vec<(&str, String)> = Vec::new();

        let mut c = claims("a@b.c");
        c["aud"] = serde_json::json!(["some-other-app"]);
        cases.push(("jwt audience is not this application", sign(KEY_A, "kid-a", &c)));

        let mut c = claims("a@b.c");
        c["iss"] = serde_json::json!("https://other-team.cloudflareaccess.com");
        cases.push(("jwt issuer is not this team", sign(KEY_A, "kid-a", &c)));

        let mut c = claims("a@b.c");
        c["exp"] = serde_json::json!(now() - 3600);
        cases.push(("jwt expired", sign(KEY_A, "kid-a", &c)));

        let mut c = claims("a@b.c");
        c["nbf"] = serde_json::json!(now() + 3600);
        cases.push(("jwt not yet valid", sign(KEY_A, "kid-a", &c)));

        let mut c = claims("a@b.c");
        c.as_object_mut().unwrap().remove("email");
        cases.push(("jwt has no email", sign(KEY_A, "kid-a", &c)));

        // Key B's private half, claiming key A's kid: the classic wrong key.
        cases.push(("jwt signature does not verify", sign(KEY_B, "kid-a", &claims("a@b.c"))));

        cases.push((
            "jwt alg is not RS256",
            format!("{}.", encode(&serde_json::json!({"alg": "none", "kid": "kid-a"}), &claims("a@b.c"))),
        ));
        {
            use base64::Engine as _;
            let signed = encode(&serde_json::json!({"alg": "HS256", "kid": "kid-a"}), &claims("a@b.c"));
            // HMAC keyed with the (public) modulus — what an alg-confusion forger would do.
            let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &modulus_b());
            let tag = ring::hmac::sign(&key, signed.as_bytes());
            let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref());
            cases.push(("jwt alg is not RS256", format!("{signed}.{sig}")));
        }
        cases.push(("jwt header unreadable", "not.a.jwt".into()));
        cases.push(("jwt empty or oversized", String::new()));

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for (reason, jwt) in cases {
            let h = if jwt.is_empty() {
                let mut h = HeaderMap::new();
                h.insert(ACCESS_HEADER, HeaderValue::from_static(""));
                h
            } else {
                assertion(&jwt)
            };
            // Each refused for its own reason — not, say, all of them for a
            // broken fixture key.
            assert_eq!(
                rt.block_on(authorize(&auth, &h, None, false)),
                Err(Denial { kind: Denied::Invalid, reason }),
                "{reason}"
            );
        }
    }

    #[test]
    fn the_email_allowlist_is_enforced_case_insensitively() {
        let auth = access_auth(&["Alice@Example.COM"]);
        let ok = sign(KEY_A, "kid-a", &claims("alice@example.com"));
        assert_eq!(check(&auth, &assertion(&ok), None, false), admitted("alice@example.com"));
        let other = sign(KEY_A, "kid-a", &claims("mallory@example.com"));
        assert_eq!(check(&auth, &assertion(&other), None, false), Err(Denied::Invalid));
    }

    #[test]
    fn an_invalid_assertion_never_falls_through_to_a_valid_token() {
        let auth = access_auth(&[]);
        let mut h = assertion(&sign(KEY_B, "kid-a", &claims("a@b.c")));
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap());
        assert_eq!(check(&auth, &h, None, false), Err(Denied::Invalid));
        // Nor through the query form.
        let h = assertion("garbage");
        assert_eq!(check(&auth, &h, Some(TOKEN), true), Err(Denied::Invalid));
    }

    #[test]
    fn with_no_assertion_the_token_still_works() {
        let auth = access_auth(&["alice@example.com"]);
        assert_eq!(check(&auth, &bearer(TOKEN), None, false), Ok(Principal::Token));
        assert_eq!(check(&auth, &HeaderMap::new(), Some(TOKEN), true), Ok(Principal::Token));
        assert_eq!(check(&auth, &HeaderMap::new(), None, false), Err(Denied::Missing));
    }

    #[test]
    fn without_access_configured_an_assertion_is_ignored() {
        let auth = Auth::token(TOKEN);
        let mut h = assertion("garbage");
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap());
        assert_eq!(check(&auth, &h, None, false), Ok(Principal::Token));
    }
}
