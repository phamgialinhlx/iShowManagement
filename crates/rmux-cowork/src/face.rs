//! Face unlock: a device secret plus a live descriptor.
//!
//! Sign-in here is **two things at once**, and the split matters. The server
//! holds 128-float face embeddings; the client holds a `rcwd_…` secret minted
//! when the machine was first trusted. `POST /auth/face/login` needs both — the
//! secret says *which account to compare against*, the descriptor says *is this
//! them*. Matching is never a search across the organisation, only against the
//! one account the device resolves to.
//!
//! **Camera frames never leave the machine.** The webview computes the
//! descriptor and only that vector is sent, which is why the model weights have
//! to be present locally at all.
//!
//! ## The device secret does not go to the webview
//!
//! It is a bearer credential: anyone holding it plus any descriptor that matches
//! can mint a session. So it lives in the OS keychain and is only ever read here,
//! in Rust. The UI hands *up* a descriptor and gets back a session — it never
//! learns the secret, exactly as the previous desktop app was careful to arrange.
//!
//! ## What face unlock is, honestly
//!
//! There is no liveness check anywhere in this design — not here and not on the
//! server, which accepts any well-formed 128-float array. A photograph held to
//! the camera produces a valid descriptor; the org's own admin tooling enrols
//! faces from still JPEGs, which proves it. A captured descriptor can also be
//! replayed indefinitely.
//!
//! So face is offered as a **convenience over the PIN, never as a replacement
//! for it**. The PIN is what actually seals the stored session (see
//! [`crate::lock`]); face is a faster way to say "it's me" when the server is
//! reachable. Anywhere the two disagree, the PIN is the floor.

use serde::{Deserialize, Serialize};

use crate::{Account, CoworkError, Session};

/// The number of floats in a face-api descriptor. Fixed by the model, and
/// checked here so a malformed vector fails locally with a clear message rather
/// than as a 400 from a Zod schema.
pub const DESCRIPTOR_LEN: usize = 128;

/// A machine that has been trusted for face unlock.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceTrust {
    pub server_url: String,
    /// `rcwd_` + 48 hex. Returned by the server exactly once, in plaintext, and
    /// stored there only as a SHA-256 — so losing this means re-trusting the
    /// machine, not recovering it.
    pub secret: String,
}

#[derive(Deserialize)]
struct TrustResponse {
    #[serde(default, rename = "deviceSecret")]
    device_secret: String,
}

#[derive(Serialize)]
struct FaceLoginRequest<'a> {
    #[serde(rename = "deviceSecret")]
    device_secret: &'a str,
    descriptor: &'a [f64],
}

#[derive(Deserialize)]
struct FaceLoginResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    account: Option<Account>,
}

/// Reject a descriptor that cannot have come from the model.
pub fn check_descriptor(descriptor: &[f64]) -> Result<(), CoworkError> {
    if descriptor.len() != DESCRIPTOR_LEN {
        return Err(CoworkError::Transport(format!(
            "a face descriptor is {DESCRIPTOR_LEN} numbers, got {}",
            descriptor.len()
        )));
    }
    // NaN would compare unequal to everything and make the server's distance
    // calculation meaningless rather than failing it.
    if descriptor.iter().any(|f| !f.is_finite()) {
        return Err(CoworkError::Transport("that face capture is not usable".to_owned()));
    }
    Ok(())
}

impl Session {
    /// Trust this machine for face unlock, without enrolling a new face.
    ///
    /// The right call when the account already has descriptors on file — which is
    /// the common case for anyone who used the previous desktop app. Enrolling
    /// again would *append* another sample rather than replace one, quietly
    /// growing the set that every future login is compared against.
    pub async fn trust_device(&self, label: &str) -> Result<DeviceTrust, CoworkError> {
        let secret: TrustResponse = self
            .post_json("/accounts/me/device/trust", &serde_json::json!({ "deviceLabel": label }))
            .await?;

        self.trust_from(secret.device_secret)
    }

    /// Enrol a face *and* trust this machine, in one call.
    ///
    /// For an account with nothing on file yet. The server appends the descriptor
    /// to the account's samples and hands back a device secret.
    pub async fn enroll_face(
        &self,
        descriptor: &[f64],
        label: &str,
    ) -> Result<DeviceTrust, CoworkError> {
        check_descriptor(descriptor)?;

        let secret: TrustResponse = self
            .post_json(
                "/accounts/me/face/enroll",
                &serde_json::json!({ "descriptor": descriptor, "deviceLabel": label }),
            )
            .await?;

        self.trust_from(secret.device_secret)
    }

    /// Forget every enrolled face for this account.
    pub async fn forget_faces(&self) -> Result<(), CoworkError> {
        self.delete("/accounts/me/face").await
    }

    fn trust_from(&self, secret: String) -> Result<DeviceTrust, CoworkError> {
        if !secret.starts_with("rcwd_") {
            return Err(CoworkError::Transport(
                "the server did not return a device secret".to_owned(),
            ));
        }
        Ok(DeviceTrust { server_url: self.base_url().to_owned(), secret })
    }
}

/// Sign in with a face.
///
/// Public on the server — it carries no bearer, because the device secret *is*
/// the possession factor. A successful call mints a **new** session, which is why
/// this path does not need the sealed vault opened first.
pub async fn face_login(
    trust: &DeviceTrust,
    descriptor: &[f64],
) -> Result<(Session, Account), CoworkError> {
    check_descriptor(descriptor)?;

    let base_url = crate::normalize_base_url(trust.server_url.clone());
    let http = crate::build_http_client().map_err(|e| CoworkError::Transport(e.to_string()))?;

    let res = http
        .post(format!("{base_url}/auth/face/login"))
        .json(&FaceLoginRequest { device_secret: &trust.secret, descriptor })
        .send()
        .await
        .map_err(crate::transport)?;

    let status = res.status();
    let body = res.text().await.map_err(crate::transport)?;

    if !status.is_success() {
        return Err(face_error(status.as_u16(), &body));
    }

    let parsed: FaceLoginResponse =
        serde_json::from_str(&body).map_err(|e| CoworkError::Transport(e.to_string()))?;

    let (Some(token), Some(account)) = (parsed.token, parsed.account) else {
        return Err(CoworkError::Transport("the server accepted the face but returned no session".to_owned()));
    };

    Ok((Session::from_account_token(http, base_url, token, &account), account))
}

/// Turn the server's error codes into something an operator can act on.
///
/// The three cases need genuinely different responses — try again, re-trust this
/// machine, enrol a face — so collapsing them into "face login failed" would
/// leave someone retrying a thing that cannot start working.
fn face_error(status: u16, body: &str) -> CoworkError {
    let code = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_default();

    let message = match code.as_str() {
        "face_no-match" => "that is not a face this account knows",
        "face_no-device" => "this machine is no longer trusted — sign in to trust it again",
        "face_not-enrolled" => "no face is enrolled for this account",
        _ => return CoworkError::Server { status, message: body.chars().take(200).collect() },
    };

    CoworkError::Unauthorized(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> Vec<f64> {
        vec![0.1; DESCRIPTOR_LEN]
    }

    #[test]
    fn a_descriptor_must_be_the_length_the_model_produces() {
        assert!(check_descriptor(&descriptor()).is_ok());
        assert!(check_descriptor(&[]).is_err());
        assert!(check_descriptor(&vec![0.1; 127]).is_err());
        assert!(check_descriptor(&vec![0.1; 129]).is_err());
    }

    #[test]
    fn a_descriptor_full_of_nan_is_refused_locally() {
        // NaN compares unequal to everything, so the server's euclidean distance
        // would come out NaN and the `> threshold` test would *pass* it through
        // as a match. Catching it here is the difference between a rejected
        // capture and a rejected security property.
        let mut d = descriptor();
        d[7] = f64::NAN;
        assert!(check_descriptor(&d).is_err());

        d[7] = f64::INFINITY;
        assert!(check_descriptor(&d).is_err());
    }

    #[test]
    fn each_face_failure_says_what_to_do_about_it() {
        let no_match = face_error(401, r#"{"error":"face_no-match"}"#);
        assert!(no_match.to_string().contains("not a face this account knows"));

        // "Re-trust the machine" and "try again" are different actions, so these
        // must not read the same.
        let no_device = face_error(401, r#"{"error":"face_no-device"}"#);
        assert!(no_device.to_string().contains("trusted"));
        assert_ne!(no_match.to_string(), no_device.to_string());

        let not_enrolled = face_error(401, r#"{"error":"face_not-enrolled"}"#);
        assert!(not_enrolled.to_string().contains("no face is enrolled"));
    }

    #[test]
    fn every_face_failure_asks_for_a_sign_in_rather_than_a_retry() {
        // A retry loop against a face the account does not know is exactly the
        // behaviour the lock screen must not fall into.
        for body in [
            r#"{"error":"face_no-match"}"#,
            r#"{"error":"face_no-device"}"#,
            r#"{"error":"face_not-enrolled"}"#,
        ] {
            assert!(face_error(401, body).requires_signin(), "{body}");
        }
    }

    #[test]
    fn an_unrecognised_failure_is_reported_rather_than_guessed_at() {
        let err = face_error(500, "upstream exploded");
        assert!(matches!(err, CoworkError::Server { status: 500, .. }), "{err}");
        // Not treated as "sign in again": a 500 is the server's problem, and
        // signing the user out over one would lose a working session.
        assert!(!err.requires_signin());
    }

    #[test]
    fn a_long_error_body_does_not_become_the_whole_message() {
        // This reaches a lock screen with very little room on it.
        let err = face_error(500, &"x".repeat(5000));
        assert!(err.to_string().len() < 260, "{}", err.to_string().len());
    }
}
