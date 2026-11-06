//! Cloudflare Access: verifying the JWT Access puts on every proxied request.
//!
//! rmux-web is reached through a cloudflared tunnel behind Cloudflare Access.
//! Access in front is **not enough on its own** — anything on the host or the
//! LAN that reaches the port bypasses the tunnel — so the server checks the
//! assertion itself, on every request and every WebSocket upgrade
//! (`docs/rmux-web.md` › "Auth"). This module is the verifier; the *decision*
//! stays in [`crate::auth::authorize`].
//!
//! What is checked, all of it, every time:
//!
//! - **`alg` is `RS256`, and nothing else.** `none` and the `HS*` family are
//!   refused before any key is looked at: an HMAC "verified" against an RSA
//!   public key is the classic forgery, and the public key is public.
//! - the signature, against the team's JWKS (`https://<team>/cdn-cgi/access/certs`),
//!   chosen by `kid`;
//! - `iss == https://<team>`, `aud` contains the pinned AUD tag;
//! - `exp` and `nbf`, with [`LEEWAY_SECS`] of clock skew;
//! - an `email` claim, and the allowlist when one was given.
//!
//! **Keys.** Fetched once at startup — a failure there stops the server, since
//! a verifier with no keys refuses everyone and nothing would say why — then
//! refreshed hourly, and on an unknown `kid` at most once per
//! [`UNKNOWN_KID_REFRESH`] (Cloudflare rotates its signing keys; an attacker
//! sending random `kid`s must not turn us into a fetch loop against it).
//! A failed refresh keeps the keys already held.
//!
//! Nothing here logs a JWT, a claim beyond the email, or a signature.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use parking_lot::RwLock;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;

/// Clock skew tolerated on `exp` and `nbf`.
pub const LEEWAY_SECS: u64 = 60;
/// Minimum gap between refreshes triggered by an unknown `kid`.
pub const UNKNOWN_KID_REFRESH: Duration = Duration::from_secs(60);
/// How often the keys are refreshed regardless.
pub const PERIODIC_REFRESH: Duration = Duration::from_secs(60 * 60);
/// Bound on one JWKS fetch.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// A JWT is a header; anything larger is not one of Cloudflare's.
const MAX_JWT_LEN: usize = 16 * 1024;

/// Who a verified assertion names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub email: String,
    pub sub: String,
}

/// A verified-signature key set, by `kid`.
type KeySet = HashMap<String, RsaPublicKeyComponents<Vec<u8>>>;

/// The verifier: configuration plus the cached keys.
pub struct Access {
    team: String,
    issuer: String,
    aud: String,
    /// Lower-cased. Empty means any authenticated Access identity.
    emails: Vec<String>,
    keys: RwLock<Arc<KeySet>>,
    /// `None` in tests: keys are injected and never refreshed.
    fetcher: Option<Fetcher>,
    /// When a refresh was last attempted; also single-flights refreshes.
    last_fetch: tokio::sync::Mutex<Option<Instant>>,
}

struct Fetcher {
    client: reqwest::Client,
    url: String,
}

impl std::fmt::Debug for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Access")
            .field("team", &self.team)
            .field("aud", &self.aud)
            .field("emails", &self.emails)
            .field("keys", &self.keys.read().len())
            .finish()
    }
}

/// Validate a team domain: a bare hostname (`myteam.cloudflareaccess.com`). A
/// leading `https://` and a trailing `/` are forgiven; a path, a port or
/// anything else that is not a hostname is refused, because the issuer and the
/// certs URL are both built from it.
pub fn normalise_team(team: &str) -> Result<String, String> {
    let t = team.trim();
    let t = t.strip_prefix("https://").unwrap_or(t).trim_end_matches('/');
    let ok = !t.is_empty()
        && t.contains('.')
        && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !t.starts_with(['.', '-'])
        && !t.ends_with(['.', '-']);
    if ok {
        Ok(t.to_ascii_lowercase())
    } else {
        Err(format!("--access-team `{team}` is not a hostname like `myteam.cloudflareaccess.com`"))
    }
}

impl Access {
    fn new(team: &str, aud: &str, emails: &[String], fetcher: Option<Fetcher>) -> Result<Self, String> {
        let team = normalise_team(team)?;
        let aud = aud.trim();
        if aud.is_empty() || !aud.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("--access-aud must be the application's AUD tag".into());
        }
        Ok(Self {
            issuer: format!("https://{team}"),
            team,
            aud: aud.to_owned(),
            emails: emails.iter().map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).collect(),
            keys: RwLock::new(Arc::new(KeySet::new())),
            fetcher,
            last_fetch: tokio::sync::Mutex::new(None),
        })
    }

    /// Build the verifier and fetch the team's keys. **Fails** if the first
    /// fetch fails or yields no RSA key: starting anyway would refuse every
    /// Access request with nothing on screen saying why.
    pub async fn connect(team: &str, aud: &str, emails: &[String]) -> anyhow::Result<Arc<Self>> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .map_err(|e| anyhow::anyhow!("building the HTTP client for Access keys: {e}"))?;
        let team_norm = normalise_team(team).map_err(anyhow::Error::msg)?;
        let url = format!("https://{team_norm}/cdn-cgi/access/certs");
        let access = Self::new(team, aud, emails, Some(Fetcher { client, url: url.clone() }))
            .map_err(anyhow::Error::msg)?;
        let keys = access.fetch().await.map_err(|e| anyhow::anyhow!("fetching Cloudflare Access keys from {url}: {e}"))?;
        anyhow::ensure!(!keys.is_empty(), "{url} returned no RSA signing keys");
        *access.keys.write() = Arc::new(keys);
        *access.last_fetch.lock().await = Some(Instant::now());
        Ok(Arc::new(access))
    }

    /// A verifier over a fixed JWKS document, never refreshed. For tests.
    pub fn from_jwks(team: &str, aud: &str, emails: &[String], jwks: &str) -> Result<Arc<Self>, String> {
        let access = Self::new(team, aud, emails, None)?;
        *access.keys.write() = Arc::new(parse_jwks(jwks)?);
        Ok(Arc::new(access))
    }

    pub fn team(&self) -> &str {
        &self.team
    }

    pub fn key_count(&self) -> usize {
        self.keys.read().len()
    }

    pub fn emails(&self) -> &[String] {
        &self.emails
    }

    /// Refresh the keys every [`PERIODIC_REFRESH`] for as long as the verifier
    /// lives. Holds only a `Weak`, so it does not keep the verifier alive.
    pub fn spawn_periodic_refresh(self: &Arc<Self>) {
        let weak: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(PERIODIC_REFRESH);
            tick.tick().await; // the first tick is immediate; startup already fetched
            loop {
                tick.tick().await;
                let Some(access) = weak.upgrade() else { return };
                access.refresh(None).await;
            }
        });
    }

    async fn fetch(&self) -> Result<KeySet, String> {
        let Some(f) = &self.fetcher else { return Err("no key source".into()) };
        let res = f.client.get(&f.url).send().await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }
        let body = res.text().await.map_err(|e| e.to_string())?;
        parse_jwks(&body)
    }

    /// Refetch the keys, unless `min_gap` says one was tried too recently.
    /// Concurrent callers wait for one fetch rather than each making one.
    async fn refresh(&self, min_gap: Option<Duration>) {
        if self.fetcher.is_none() {
            return;
        }
        let mut last = self.last_fetch.lock().await;
        if let (Some(gap), Some(at)) = (min_gap, *last)
            && at.elapsed() < gap
        {
            return;
        }
        *last = Some(Instant::now());
        match self.fetch().await {
            Ok(keys) if !keys.is_empty() => {
                tracing::debug!(keys = keys.len(), "refreshed Cloudflare Access keys");
                *self.keys.write() = Arc::new(keys);
            }
            Ok(_) => tracing::warn!("Cloudflare Access returned no RSA keys; keeping the previous set"),
            Err(e) => tracing::warn!("refreshing Cloudflare Access keys failed ({e}); keeping the previous set"),
        }
    }

    /// Verify one assertion. `Err` carries a short, fixed reason for the log —
    /// never anything taken from the token.
    pub async fn verify(&self, jwt: &str) -> Result<Identity, &'static str> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let parts = split(jwt)?;
        let key = match self.key(&parts.kid) {
            Some(k) => k,
            None => {
                self.refresh(Some(UNKNOWN_KID_REFRESH)).await;
                self.key(&parts.kid).ok_or("jwt signed by an unknown key")?
            }
        };
        verify_parts(&parts, &key, now, &self.issuer, &self.aud, &self.emails)
    }

    fn key(&self, kid: &str) -> Option<RsaPublicKeyComponents<Vec<u8>>> {
        self.keys.read().get(kid).cloned()
    }
}

/// A JWT split and its header checked — before any key is chosen.
struct Parts<'a> {
    kid: String,
    signed: &'a str,
    signature: Vec<u8>,
    claims: Vec<u8>,
}

#[derive(Deserialize)]
struct Header {
    alg: Option<String>,
    kid: Option<String>,
}

fn split(jwt: &str) -> Result<Parts<'_>, &'static str> {
    let jwt = jwt.trim();
    if jwt.is_empty() || jwt.len() > MAX_JWT_LEN {
        return Err("jwt empty or oversized");
    }
    let mut it = jwt.split('.');
    let (Some(h), Some(c), Some(s), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return Err("jwt is not three segments");
    };
    let header: Header = B64URL
        .decode(h)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or("jwt header unreadable")?;
    // Before anything else: `none` and HS* never reach a key.
    if header.alg.as_deref() != Some("RS256") {
        return Err("jwt alg is not RS256");
    }
    let kid = header.kid.filter(|k| !k.is_empty()).ok_or("jwt has no kid")?;
    let signature = B64URL.decode(s).map_err(|_| "jwt signature unreadable")?;
    let claims = B64URL.decode(c).map_err(|_| "jwt claims unreadable")?;
    Ok(Parts { kid, signed: &jwt[..h.len() + 1 + c.len()], signature, claims })
}

/// `aud` is a string or an array of strings; anything else matches nothing.
#[derive(Deserialize)]
#[serde(untagged)]
enum Aud {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Aud>,
    exp: Option<u64>,
    nbf: Option<u64>,
    email: Option<String>,
    sub: Option<String>,
}

fn verify_parts(
    parts: &Parts<'_>,
    key: &RsaPublicKeyComponents<Vec<u8>>,
    now: u64,
    issuer: &str,
    aud: &str,
    emails: &[String],
) -> Result<Identity, &'static str> {
    key.verify(&RSA_PKCS1_2048_8192_SHA256, parts.signed.as_bytes(), &parts.signature)
        .map_err(|_| "jwt signature does not verify")?;
    // Only now, with the signature good, are the claims worth reading.
    let claims: Claims = serde_json::from_slice(&parts.claims).map_err(|_| "jwt claims unreadable")?;
    if claims.iss.as_deref() != Some(issuer) {
        return Err("jwt issuer is not this team");
    }
    let aud_ok = match &claims.aud {
        Some(Aud::One(a)) => a == aud,
        Some(Aud::Many(v)) => v.iter().any(|a| a == aud),
        None => false,
    };
    if !aud_ok {
        return Err("jwt audience is not this application");
    }
    let exp = claims.exp.ok_or("jwt has no exp")?;
    if now > exp.saturating_add(LEEWAY_SECS) {
        return Err("jwt expired");
    }
    if let Some(nbf) = claims.nbf
        && nbf > now.saturating_add(LEEWAY_SECS)
    {
        return Err("jwt not yet valid");
    }
    // A service token carries no email; this server admits people.
    let email = claims.email.filter(|e| !e.is_empty()).ok_or("jwt has no email")?;
    if !emails.is_empty() && !emails.iter().any(|e| e.eq_ignore_ascii_case(&email)) {
        return Err("jwt email is not on the allowlist");
    }
    Ok(Identity { email, sub: claims.sub.unwrap_or_default() })
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: Option<String>,
    kid: Option<String>,
    alg: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

/// The RSA keys in a JWKS document. Non-RSA keys, keys declaring another
/// `alg`, and keys without a `kid` are skipped — none could verify an RS256
/// assertion we would accept.
pub(crate) fn parse_jwks(body: &str) -> Result<KeySet, String> {
    let jwks: Jwks = serde_json::from_str(body).map_err(|e| format!("not a JWKS document: {e}"))?;
    let mut keys = KeySet::new();
    for k in jwks.keys {
        if k.kty.as_deref() != Some("RSA") || k.alg.as_deref().is_some_and(|a| a != "RS256") {
            continue;
        }
        let (Some(kid), Some(n), Some(e)) = (k.kid, k.n, k.e) else { continue };
        let (Ok(n), Ok(e)) = (B64URL.decode(n.trim_end_matches('=')), B64URL.decode(e.trim_end_matches('='))) else {
            continue;
        };
        let strip = |v: Vec<u8>| -> Vec<u8> {
            let first = v.iter().position(|&b| b != 0).unwrap_or(v.len());
            v[first..].to_vec()
        };
        keys.insert(kid, RsaPublicKeyComponents { n: strip(n), e: strip(e) });
    }
    Ok(keys)
}

/// Test-only signing helpers, shared with `auth`'s tests.
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};

    pub const TEAM: &str = "test-team.cloudflareaccess.com";
    pub const AUD: &str = "aud-0123456789abcdef";

    /// Two throwaway RSA keys generated for these tests (openssl genpkey).
    /// They sign nothing anywhere else.
    pub const KEY_A: &[u8] = include_bytes!("testdata/access_test_key_a.pk8");
    pub const KEY_B: &[u8] = include_bytes!("testdata/access_test_key_b.pk8");
    const MOD_A: &str = include_str!("testdata/access_test_key_a.modulus.hex");
    const MOD_B: &str = include_str!("testdata/access_test_key_b.modulus.hex");

    fn hex(s: &str) -> Vec<u8> {
        (0..s.trim().len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A JWKS holding key A (`kid-a`) only.
    pub fn jwks_a() -> String {
        serde_json::json!({ "keys": [
            { "kty": "RSA", "alg": "RS256", "use": "sig", "kid": "kid-a", "e": "AQAB", "n": B64URL.encode(hex(MOD_A)) },
            // Present in the set but not used to sign the "wrong key" test: a
            // forger needs the private half, not a slot in the JWKS.
            { "kty": "EC", "kid": "kid-ec", "crv": "P-256", "x": "AA", "y": "AA" },
        ]})
        .to_string()
    }

    pub fn modulus_b() -> Vec<u8> {
        hex(MOD_B)
    }

    pub fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    /// Standard valid claims for `email`.
    pub fn claims(email: &str) -> serde_json::Value {
        let now = now();
        serde_json::json!({
            "iss": format!("https://{TEAM}"),
            "aud": [AUD],
            "exp": now + 600,
            "nbf": now - 5,
            "iat": now - 5,
            "email": email,
            "sub": "user-sub-1",
            "type": "app",
        })
    }

    pub fn encode(header: &serde_json::Value, claims: &serde_json::Value) -> String {
        format!("{}.{}", B64URL.encode(header.to_string()), B64URL.encode(claims.to_string()))
    }

    /// RS256-sign `claims` with `key` under `kid`.
    pub fn sign(key: &[u8], kid: &str, claims: &serde_json::Value) -> String {
        let pair = RsaKeyPair::from_pkcs8(key).unwrap();
        let signed = encode(&serde_json::json!({ "alg": "RS256", "kid": kid, "typ": "JWT" }), claims);
        let mut sig = vec![0u8; pair.public().modulus_len()];
        pair.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signed.as_bytes(), &mut sig).unwrap();
        format!("{signed}.{}", B64URL.encode(sig))
    }

    pub fn verifier(emails: &[&str]) -> Arc<Access> {
        let emails: Vec<String> = emails.iter().map(|s| s.to_string()).collect();
        Access::from_jwks(TEAM, AUD, &emails, &jwks_a()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    #[test]
    fn team_domains_are_normalised_or_refused() {
        assert_eq!(normalise_team("https://MYTEAM.cloudflareaccess.com/").unwrap(), "myteam.cloudflareaccess.com");
        for bad in ["", "localhost", "a.com/x", "a.com:443", "evil.com#.cloudflareaccess.com", "-a.com"] {
            assert!(normalise_team(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn jwks_keeps_only_rsa_keys_with_a_kid() {
        let keys = parse_jwks(&jwks_a()).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(keys.contains_key("kid-a"));
        assert!(parse_jwks("not json").is_err());
    }

    #[tokio::test]
    async fn a_valid_assertion_verifies() {
        let v = verifier(&[]);
        let jwt = sign(KEY_A, "kid-a", &claims("a@b.c"));
        assert_eq!(v.verify(&jwt).await, Ok(Identity { email: "a@b.c".into(), sub: "user-sub-1".into() }));
    }

    #[tokio::test]
    async fn aud_as_a_string_is_accepted() {
        let v = verifier(&[]);
        let mut c = claims("a@b.c");
        c["aud"] = serde_json::json!(AUD);
        assert!(v.verify(&sign(KEY_A, "kid-a", &c)).await.is_ok());
    }

    #[tokio::test]
    async fn an_unknown_kid_is_refused_without_a_source() {
        let v = verifier(&[]);
        assert!(v.verify(&sign(KEY_A, "kid-zzz", &claims("a@b.c"))).await.is_err());
    }

    #[tokio::test]
    async fn a_tampered_payload_is_refused() {
        let v = verifier(&[]);
        let jwt = sign(KEY_A, "kid-a", &claims("a@b.c"));
        let mut parts: Vec<&str> = jwt.split('.').collect();
        let forged = B64URL.encode(claims("admin@b.c").to_string());
        parts[1] = &forged;
        assert_eq!(v.verify(&parts.join(".")).await, Err("jwt signature does not verify"));
    }

    #[test]
    fn modulus_b_differs_from_a() {
        // Guards the fixture: the "wrong key" test means nothing if the two match.
        let a = parse_jwks(&jwks_a()).unwrap().remove("kid-a").unwrap().n;
        assert_ne!(a, modulus_b());
        let _ = KEY_B;
    }
}
