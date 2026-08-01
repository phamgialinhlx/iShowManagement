//! The face recognition models, fetched only if someone turns face unlock on.
//!
//! Three neural networks — a detector, a landmark model and the recogniser that
//! actually produces the 128-float descriptor — totalling **6.7 MB**, of which
//! the recogniser alone is 6.1 MB. Bundling them would put that in every copy of
//! the app, including the overwhelming majority that never enable face unlock, so
//! they are downloaded on first use and kept in the app's data directory
//! thereafter.
//!
//! ## Why the hashes are pinned
//!
//! These files are fetched over the network and then fed to a model runtime in
//! the webview. Trusting whatever a CDN returns would mean the integrity of face
//! unlock rests on a third party's continued good behaviour, so each file is
//! checked against a SHA-256 recorded here and a mismatch is refused outright
//! rather than cached. The digests were taken from the copies the previous
//! desktop app shipped and verified to match the published package byte for byte.
//!
//! ## Why the download happens in Rust
//!
//! Not for convenience — the webview *cannot* do it. The CSP restricts
//! `connect-src` to the app's own origin and the IPC channel, so a `fetch` to a
//! CDN from the UI is blocked. Relaxing that to let the renderer reach the
//! internet directly would be a much larger hole than this feature is worth.

use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

use crate::auth::AuthError;

/// Where the weights come from.
///
/// The published package for the same library the descriptors must be compatible
/// with — a descriptor is only comparable to the enrolled ones if it came from
/// the same weights, so this cannot be substituted for a different model.
const BASE_URL: &str = "https://cdn.jsdelivr.net/npm/@vladmandic/face-api@1.7.15/model";

/// File name, SHA-256, expected size.
///
/// The size is not redundant with the digest: it lets a download that is going to
/// fail be abandoned early rather than after streaming an unbounded body, which
/// matters when the "server" is whatever answered the request.
const MODELS: [(&str, &str, u64); 6] = [
    (
        "tiny_face_detector_model-weights_manifest.json",
        "5d1af4849ac48d5b985f4a9b16010c512353ddd6fcc63d50fd0bc9e9e64296e5",
        3_219,
    ),
    (
        "tiny_face_detector_model.bin",
        "b7503ce7df31039b1c43316a9b865cab6a70dd748cc602d3fa28b551503c3871",
        193_321,
    ),
    (
        "face_landmark_68_model-weights_manifest.json",
        "ca4886639f86e99b39fed0c155f81b63317225773bd9616716e887b0153389c9",
        8_485,
    ),
    (
        "face_landmark_68_model.bin",
        "4611ef65c87d836d03d684b30eec4d195d8b219fa1dd58fc58945831c6b9299b",
        356_840,
    ),
    (
        "face_recognition_model-weights_manifest.json",
        "cbaffa501b0b9275a12b63357a6843e7e30c054e1c9151e1a5f879b26e32986b",
        19_615,
    ),
    (
        "face_recognition_model.bin",
        "b413e420d6840b2775fba32008db6f3cddb07d485967fb42cfcf379c16a8c589",
        6_444_032,
    ),
];

/// Total bytes, so the UI can say what it is about to download rather than
/// showing an unexplained wait.
pub fn total_bytes() -> u64 {
    MODELS.iter().map(|(_, _, size)| size).sum()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStatus {
    pub installed: bool,
    /// What a fresh install would fetch.
    pub bytes: u64,
    /// A URL the webview can load the models from, once they exist.
    pub dir: String,
}

fn models_dir(app: &AppHandle) -> Result<PathBuf, AuthError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AuthError::message(format!("no app data directory: {e}")))?
        .join("face-models");
    Ok(dir)
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A file counts as present only if its contents still hash correctly.
///
/// A truncated file from an interrupted download is the case that matters: it
/// exists, it has the right name, and it would fail deep inside the model loader
/// with an error about tensor shapes.
fn verified(path: &PathBuf, expected: &str) -> bool {
    std::fs::read(path).is_ok_and(|bytes| digest(&bytes) == expected)
}

#[tauri::command]
pub async fn face_models_status(app: AppHandle) -> Result<ModelsStatus, AuthError> {
    let dir = models_dir(&app)?;

    let installed = MODELS
        .iter()
        .all(|(name, sha, _)| verified(&dir.join(name), sha));

    Ok(ModelsStatus {
        installed,
        bytes: total_bytes(),
        dir: dir.to_string_lossy().into_owned(),
    })
}

/// Download every model that is missing or damaged.
#[tauri::command]
pub async fn face_models_install(app: AppHandle) -> Result<ModelsStatus, AuthError> {
    install_into(&models_dir(&app)?).await?;
    face_models_status(app).await
}

/// The download itself, against a directory rather than an app handle.
///
/// Split out so it can be exercised for real — the interesting failures here are
/// a wrong URL, a changed digest and a partial write, none of which a mocked HTTP
/// client would catch, and all of which would surface to a user as face unlock
/// simply not working.
pub(crate) async fn install_into(dir: &std::path::Path) -> Result<(), AuthError> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AuthError::message(format!("could not create {}: {e}", dir.display())))?;

    let http = reqwest::Client::builder()
        .user_agent(concat!("rmux/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        // Generous: the recogniser is 6.1 MB and this runs on hotel wifi.
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| AuthError::message(e.to_string()))?;

    for (name, sha, size) in MODELS {
        let path = dir.join(name);
        if verified(&path, sha) {
            continue;
        }

        let response = http
            .get(format!("{BASE_URL}/{name}"))
            .send()
            .await
            .map_err(|e| AuthError::message(format!("could not fetch {name}: {e}")))?;

        if !response.status().is_success() {
            return Err(AuthError::message(format!(
                "could not fetch {name}: the server answered {}",
                response.status()
            )));
        }

        // Refuse a wrong-sized body before reading it. Without this, a captive
        // portal answering every request with a login page would be streamed in
        // full before the digest rejected it.
        if let Some(len) = response.content_length()
            && len != size
        {
            return Err(AuthError::message(format!(
                "{name} is {len} bytes, expected {size} — refusing it"
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| AuthError::message(format!("could not read {name}: {e}")))?;

        let actual = digest(&bytes);
        if actual != sha {
            return Err(AuthError::message(format!(
                "{name} does not match its expected checksum — refusing it"
            )));
        }

        // Write beside, then rename. A crash mid-write must not leave a
        // half-written file that the next start reads as installed — the digest
        // check would catch it, but only after someone waited for a failure.
        let partial = path.with_extension("partial");
        std::fs::write(&partial, &bytes)
            .map_err(|e| AuthError::message(format!("could not write {name}: {e}")))?;
        std::fs::rename(&partial, &path)
            .map_err(|e| AuthError::message(format!("could not install {name}: {e}")))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_advertised_size_is_the_sum_of_the_files() {
        // This number is shown to the operator before they agree to a download.
        assert_eq!(total_bytes(), 7_025_512);
        // …and is the 6.7 MB the UI says it is.
        assert_eq!(total_bytes() / 1024 / 1024, 6);
    }

    #[test]
    fn every_model_has_a_full_length_digest() {
        for (name, sha, size) in MODELS {
            assert_eq!(sha.len(), 64, "{name}");
            assert!(sha.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
            assert!(size > 0, "{name}");
        }
    }

    #[test]
    fn the_three_nets_the_descriptor_needs_are_all_present() {
        // A missing net does not fail until the model loader runs in the webview,
        // where the error is about a fetch rather than about a missing model.
        for net in ["tiny_face_detector", "face_landmark_68", "face_recognition"] {
            assert!(
                MODELS.iter().any(|(n, _, _)| n.starts_with(net) && n.ends_with(".bin")),
                "no weights for {net}"
            );
            assert!(
                MODELS.iter().any(|(n, _, _)| n.starts_with(net) && n.ends_with(".json")),
                "no manifest for {net}"
            );
        }
    }

    #[test]
    fn digests_are_lowercase_hex_of_the_content() {
        // Pinned against a known vector, because a digest function that returned
        // a differently formatted string would reject every genuine download and
        // the failure would look like a network problem.
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(digest(b"").len(), 64);
    }

    #[test]
    fn a_file_with_the_wrong_contents_is_not_treated_as_installed() {
        let dir = std::env::temp_dir().join("rmux-face-models-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("truncated.bin");
        std::fs::write(&path, b"not the model").unwrap();

        assert!(!verified(&path, MODELS[0].1));
        assert!(verified(&path, &digest(b"not the model")));
        // A file that is not there at all is also not installed, rather than a
        // read error surfacing somewhere less convenient.
        assert!(!verified(&dir.join("absent.bin"), MODELS[0].1));

        std::fs::remove_file(&path).ok();
    }

    /// The real download, against the real CDN.
    ///
    /// `#[ignore]`d because it fetches 6.7 MB. Run with
    /// `cargo test -p rmux -- --ignored --nocapture face_models`.
    ///
    /// This is the only check that can catch the failure that actually matters:
    /// the published files changing out from under the pinned digests. Everything
    /// else here tests the table, not the world.
    #[tokio::test]
    #[ignore = "downloads 6.7 MB from a CDN"]
    async fn every_pinned_model_downloads_and_matches_its_digest() {
        let dir = std::env::temp_dir().join("rmux-face-models-live");
        std::fs::remove_dir_all(&dir).ok();

        install_into(&dir).await.expect("install");

        for (name, sha, size) in MODELS {
            let path = dir.join(name);
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(bytes.len() as u64, size, "{name} is the wrong size");
            assert_eq!(digest(&bytes), sha, "{name} does not match its pinned digest");
            println!("ok {name} ({} bytes)", bytes.len());
        }

        // No `.partial` left behind — a leftover would be read as installed by
        // nothing, but it would waste the space silently and forever.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".partial"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");

        // Running again is a no-op rather than a re-download.
        let before = std::fs::metadata(dir.join(MODELS[0].0)).unwrap().modified().unwrap();
        install_into(&dir).await.expect("second install");
        let after = std::fs::metadata(dir.join(MODELS[0].0)).unwrap().modified().unwrap();
        assert_eq!(before, after, "an already-installed model was fetched again");

        // A damaged file is repaired rather than trusted.
        std::fs::write(dir.join(MODELS[0].0), b"corrupt").unwrap();
        install_into(&dir).await.expect("repair");
        assert_eq!(digest(&std::fs::read(dir.join(MODELS[0].0)).unwrap()), MODELS[0].1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_source_is_the_library_the_descriptors_must_match() {
        // Descriptors are only comparable to the enrolled ones if they came from
        // the same weights, so the version here is load-bearing, not cosmetic.
        assert!(BASE_URL.contains("@vladmandic/face-api@1.7.15"), "{BASE_URL}");
        assert!(BASE_URL.starts_with("https://"), "{BASE_URL}");
    }
}
