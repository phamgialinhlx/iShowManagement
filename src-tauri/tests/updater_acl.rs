// See `window_drag.rs` for why the duplicate-symbol linker warning is allowed.
#![allow(linker_messages)]

//! The self-updater is driven from Rust, and the webview must not be able to
//! drive it directly.
//!
//! `src/updates.rs` checks, downloads and installs through the plugin's *Rust*
//! API; the page only ever names an action (`update_restart`). Granting
//! `updater:*` would let any script in the webview — a rendered `.docx`, a
//! transcript — trigger `download_and_install` itself, so **no updater (or
//! process) permission is granted at all**. This pins that: every plugin command
//! is refused. As a control, a granted plugin command is admitted, so the
//! denials cannot pass merely because the harness refuses everything.

use base64::Engine as _;
use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, mock_builder};
use tauri::webview::InvokeRequest;

fn call(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
) -> Result<InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(json!({})),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}

fn denied(result: &Result<InvokeResponseBody, serde_json::Value>) -> bool {
    matches!(result, Err(e) if e.to_string().contains("not allowed"))
}

#[test]
fn the_webview_cannot_drive_the_updater() {
    let app = mock_builder()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .build(tauri::generate_context!())
        .expect("failed to build test app");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    for cmd in [
        "plugin:updater|check",
        "plugin:updater|download",
        "plugin:updater|install",
        "plugin:updater|download_and_install",
        "plugin:process|restart",
        "plugin:process|exit",
    ] {
        let result = call(&webview, cmd);
        assert!(denied(&result), "{cmd} must not be callable from the webview: {result:?}");
    }

    // Control: a plugin command that *is* granted gets past the ACL.
    let open = call(&webview, "plugin:dialog|open");
    assert!(!denied(&open), "control failed — dialog:allow-open should be admitted: {open:?}");
}

#[test]
fn the_capabilities_grant_nothing_from_the_updater_or_process_plugins() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities/default.json");
    let text = std::fs::read_to_string(path).expect("read capabilities");
    assert!(!text.contains("updater:"), "updater permissions must stay ungranted: {text}");
    assert!(!text.contains("process:"), "process permissions must stay ungranted: {text}");
}

/// The public key shipped in `tauri.conf.json` verifies a file signed with the
/// release key (`~/.tauri/rmux-updater.key`, never in CI). If the key is ever
/// regenerated without updating the config — or the other way round — every
/// installed rmux would refuse every update; this fails first. Re-sign the
/// fixture with `pnpm tauri signer sign` (key from the file) when rotating it.
#[test]
fn the_shipped_public_key_verifies_the_release_key() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let conf: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("tauri.conf.json")).unwrap()).unwrap();
    let pubkey_b64 = conf["plugins"]["updater"]["pubkey"].as_str().expect("plugins.updater.pubkey");
    assert_eq!(
        conf["plugins"]["updater"]["endpoints"][0].as_str(),
        Some("https://github.com/phamgialinhlx/iShowManagement/releases/latest/download/latest.json")
    );

    let b64 = base64::engine::general_purpose::STANDARD;
    let pubkey = String::from_utf8(b64.decode(pubkey_b64).unwrap()).unwrap();
    let pubkey = minisign_verify::PublicKey::decode(&pubkey).expect("decode pubkey");

    let data = std::fs::read(dir.join("tests/fixtures/updater-sample.txt")).unwrap();
    let sig_b64 = std::fs::read_to_string(dir.join("tests/fixtures/updater-sample.txt.sig")).unwrap();
    let sig = String::from_utf8(b64.decode(sig_b64.trim()).unwrap()).unwrap();
    let sig = minisign_verify::Signature::decode(&sig).expect("decode signature");
    pubkey.verify(&data, &sig, false).expect("the shipped pubkey must verify the release key's signature");

    // And a tampered file must not verify, or the check above proves nothing.
    let mut tampered = data.clone();
    tampered[0] ^= 1;
    assert!(pubkey.verify(&tampered, &sig, false).is_err());
}
