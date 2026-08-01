//! The app lock — PIN, and optionally a face.
//!
//! Off by default, and that default is deliberate: the workbench works with no
//! account at all, so demanding a PIN before terminals and files would be asking
//! people to authenticate to use something the account plays no part in. Turning
//! the lock on is a choice to protect the *session*.
//!
//! ## What each half actually does
//!
//! **The PIN seals the stored session** (see [`rmux_cowork::lock`]). Unlocking is
//! decryption, not a comparison — with the wrong PIN the token does not exist to
//! be stolen. It works with no network, which matters: a lock that needed the
//! server would strand the operator on a plane, in an app that does not need the
//! server for anything they were about to do.
//!
//! Notably the server's own `POST /accounts/me/pin/verify` is **not** used for
//! this. It answers `{ ok: false }` rather than 401, applies no rate limiting and
//! writes no audit entry — it trusts the client to enforce the outcome. That is
//! fine for the advisory check it was built for and useless as a lock, so the PIN
//! is checked here where getting it wrong costs an argon2 derivation.
//!
//! **The face mints a new session instead of opening the old one.** Biometrics
//! are fuzzy and cannot derive a key, so there is no honest way to make a face
//! decrypt the vault. `POST /auth/face/login` sidesteps that entirely: a match
//! plus this machine's device secret returns a *fresh* token. It needs the
//! network, so the PIN remains the offline path — the same arrangement a phone
//! uses, and for the same reason.
//!
//! Face is therefore never the security floor. There is no liveness check in this
//! stack, and the server accepts any well-formed descriptor, so a photograph
//! passes. It is a convenience over typing six digits.

use rmux_cowork::{
    Account, DeviceTrust, Session, Vault, VaultKey, credentials, face, lock as vault,
};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::auth::{AuthError, AuthStore, SignedIn};

/// What the UI needs to decide which screen to show, before anything is proved.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockStatus {
    /// A sealed session is stored, so the app must ask before restoring it.
    pub locked: bool,
    /// Face unlock was enabled and this machine is trusted.
    pub face: bool,
    /// Who the sealed session belongs to. Visible without unlocking on purpose —
    /// anyone holding the machine can already read the username off the lock
    /// screen of any OS, and unlocking blind helps nobody.
    pub username: String,
    pub server_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnableLock {
    pub pin: String,
    /// Also trust this machine for face unlock.
    pub face: bool,
}

/// Is there a sealed session waiting, and what may open it?
#[tauri::command]
pub async fn lock_status(server_url: String) -> Result<LockStatus, AuthError> {
    let Some(Vault::Sealed(sealed)) = credentials::load_vault(&server_url)? else {
        return Ok(LockStatus::default());
    };

    // `face` records the *intent*; the device secret is what makes it work. If
    // the secret is gone — keychain reset, machine re-imaged — offering a face
    // button would be offering a button that cannot succeed.
    let trusted = credentials::load_device(&server_url)?.is_some();

    Ok(LockStatus {
        locked: true,
        face: sealed.face && trusted,
        username: sealed.username,
        server_url,
    })
}

/// Turn the lock on for the session that is already signed in.
#[tauri::command]
pub async fn lock_enable(
    store: State<'_, AuthStore>,
    request: EnableLock,
) -> Result<LockStatus, AuthError> {
    vault::check_pin(&request.pin).map_err(|e| AuthError::message(e.to_string()))?;

    let server_url = store
        .server_url()
        .await
        .ok_or_else(|| AuthError::message("sign in before locking the app"))?;

    // Trust the machine *before* sealing anything. If this fails, the operator is
    // left exactly as they were rather than locked in with a face button that
    // does not work.
    let face = if request.face {
        let guard = store.session.read().await;
        let session = guard
            .as_ref()
            .ok_or_else(|| AuthError::message("sign in before enabling face unlock"))?;

        // Nothing to match against means a lock screen with a camera that can
        // never succeed — the worst possible state, because it looks like it is
        // working right up until it never lets anyone in.
        if !session.me().await?.has_face {
            return Err(AuthError::message(
                "enrol a face before enabling face unlock",
            ));
        }

        // Reuse an existing pairing. Every `trust_device` writes a `device_trust`
        // row the server has no way to revoke, so minting a second one for a
        // machine that is already trusted leaks a credential for nothing.
        if credentials::load_device(&server_url)?.is_none() {
            let trust = session.trust_device(&crate::auth::device_label()).await?;
            credentials::save_device(&trust)?;
        }
        true
    } else {
        false
    };

    let creds = {
        let guard = store.session.read().await;
        let session =
            guard.as_ref().ok_or_else(|| AuthError::message("sign in before locking the app"))?;
        session.credentials().await
    };

    let key = VaultKey::new(&request.pin).map_err(|e| AuthError::message(e.to_string()))?;
    let sealed = key.seal(&creds, face).map_err(|e| AuthError::message(e.to_string()))?;

    credentials::save_vault(&server_url, &Vault::Sealed(sealed))?;
    // Held so a token rotated later in this run can be re-sealed rather than
    // written back in the clear — which would silently undo the lock.
    *store.vault_key.write().await = Some(key);

    Ok(LockStatus { locked: true, face, username: creds.username, server_url })
}

/// Turn the lock off, leaving the session signed in.
#[tauri::command]
pub async fn lock_disable(store: State<'_, AuthStore>, pin: String) -> Result<(), AuthError> {
    let server_url = store
        .server_url()
        .await
        .ok_or_else(|| AuthError::message("nothing is locked"))?;

    let Some(Vault::Sealed(sealed)) = credentials::load_vault(&server_url)? else {
        return Ok(());
    };

    // The PIN is required even though the app is already unlocked. Otherwise
    // anyone who walked up to an unlocked machine could remove the lock without
    // ever knowing it, and the next reopen would let them straight in.
    let key = VaultKey::for_vault(&sealed, &pin).map_err(|e| AuthError::message(e.to_string()))?;
    let creds = key.open(&sealed).map_err(|e| AuthError::message(e.to_string()))?;

    credentials::save(&server_url, &creds)?;
    credentials::clear_device(&server_url)?;
    *store.vault_key.write().await = None;

    Ok(())
}

/// Open the vault with a PIN and restore the session.
#[tauri::command]
pub async fn lock_unlock(
    store: State<'_, AuthStore>,
    server_url: String,
    pin: String,
) -> Result<SignedIn, AuthError> {
    let Some(Vault::Sealed(sealed)) = credentials::load_vault(&server_url)? else {
        return Err(AuthError::message("there is no locked session on this machine"));
    };

    let key = VaultKey::for_vault(&sealed, &pin).map_err(|e| AuthError::message(e.to_string()))?;
    let creds = key.open(&sealed).map_err(|e| AuthError::message(e.to_string()))?;

    let session = Session::resume(&server_url, creds)?;
    let account = session.me().await?;

    // Re-seal: `me()` may have rotated the token, and the whole point of holding
    // the key is that this does not need the PIN again.
    let refreshed = session.credentials().await;
    let resealed = key
        .seal(&refreshed, sealed.face)
        .map_err(|e| AuthError::message(e.to_string()))?;
    credentials::save_vault(&server_url, &Vault::Sealed(resealed))?;

    store.adopt(session, &server_url, Some(key)).await;

    Ok(SignedIn { account, server_url })
}

/// Open with a face: match, mint a new session, re-seal it under the old PIN.
///
/// Takes a **descriptor**, never an image. The camera frame is turned into 128
/// floats in the webview and the frame itself never leaves it.
#[tauri::command]
pub async fn lock_unlock_face(
    store: State<'_, AuthStore>,
    server_url: String,
    descriptor: Vec<f64>,
) -> Result<SignedIn, AuthError> {
    let trust = credentials::load_device(&server_url)?
        .ok_or_else(|| AuthError::message("this machine is not trusted for face unlock"))?;

    let (session, account) = face::face_login(&trust, &descriptor).await?;

    // The vault stays sealed under a PIN nobody has typed, so it cannot be
    // rewritten with the new token. That is correct rather than unfortunate: the
    // sealed token is still valid, and re-sealing would need the PIN this path
    // exists to avoid. The fresh session simply supersedes it for this run.
    store.adopt(session, &server_url, None).await;

    Ok(SignedIn { account, server_url })
}

/// Trust this machine for face unlock without turning the whole lock on.
///
/// Needed because enrolment and locking are separate decisions: an account with
/// no face on file has to enrol one first, and that requires a signed-in session
/// rather than a locked vault.
#[tauri::command]
pub async fn face_enroll(
    store: State<'_, AuthStore>,
    descriptor: Option<Vec<f64>>,
) -> Result<(), AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;

    let label = crate::auth::device_label();
    let trust: DeviceTrust = match descriptor {
        // Appending a sample to an account that already has some quietly grows
        // the set every future login is matched against, so a new descriptor is
        // sent only when one was asked for.
        Some(descriptor) => session.enroll_face(&descriptor, &label).await?,
        None => session.trust_device(&label).await?,
    };

    // Both calls mint a device secret, and both are stored here — so a caller
    // that enrols and then enables the lock does not end up with two.
    credentials::save_device(&trust)?;
    Ok(())
}

/// Whether the signed-in account has a face on file, so the UI knows whether to
/// offer "use my enrolled face" or "enrol one now".
#[tauri::command]
pub async fn face_status(store: State<'_, AuthStore>) -> Result<Account, AuthError> {
    let guard = store.session.read().await;
    let session = guard.as_ref().ok_or_else(|| AuthError::message("sign in first"))?;
    Ok(session.me().await?)
}
