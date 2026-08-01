// `generate_context!` embeds Info.plist; linking it alongside rmux_lib (which
// already embeds one) produces a duplicate-symbol warning that is harmless here —
// the test binary is never bundled as a .app.
#![allow(linker_messages)]

//! Guards the window's ability to be dragged.
//!
//! The window is transparent with an overlay title bar, so macOS provides no
//! native strip to grab and the UI declares its own `data-tauri-drag-region`.
//! Tauri's injected script turns a mousedown there into
//! `plugin:window|start_dragging`.
//!
//! That is a **plugin** command, and unlike rmux's own commands plugin commands
//! are always ACL-checked. `core:default` grants `allow-internal-toggle-maximize`
//! but **not** `allow-start-dragging`, so with only the default set the call is
//! rejected — and because nothing awaits that promise, the window simply refuses
//! to move with no error anywhere. It shipped that way once already.
//!
//! These tests assert the capability file still grants what the chrome needs.

use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, mock_builder};
use tauri::webview::InvokeRequest;

fn test_app() -> tauri::App<tauri::test::MockRuntime> {
    mock_builder().build(tauri::generate_context!()).expect("failed to build test app")
}

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
            // Must be the local origin the real webview uses; a foreign origin
            // changes how the ACL is applied.
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(json!({})),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}

/// The ACL rejects with a message containing "not allowed"; anything else means
/// the command was permitted and reached the plugin.
fn was_denied_by_acl(result: &Result<InvokeResponseBody, serde_json::Value>) -> bool {
    match result {
        Ok(_) => false,
        Err(e) => e.to_string().contains("not allowed"),
    }
}

#[test]
fn the_window_can_be_dragged() {
    let app = test_app();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    let result = call(&webview, "plugin:window|start_dragging");

    assert!(
        !was_denied_by_acl(&result),
        "start_dragging is blocked by the ACL — the window cannot be moved. \
         Add `core:window:allow-start-dragging` to capabilities/default.json; \
         `core:default` does not include it. Got: {result:?}"
    );
}

#[test]
fn the_window_chrome_controls_are_permitted() {
    let app = test_app();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    // Double-clicking the drag region maximizes; the title bar will grow explicit
    // controls. Each is a plugin command and needs its own grant.
    for cmd in [
        "plugin:window|internal_toggle_maximize",
        "plugin:window|minimize",
        "plugin:window|close",
    ] {
        let result = call(&webview, cmd);
        assert!(!was_denied_by_acl(&result), "{cmd} is blocked by the ACL. Got: {result:?}");
    }
}

/// Confirms the assertions above can actually fail, rather than passing because
/// every command looks permitted in this harness.
#[test]
fn a_command_with_no_grant_is_detected_as_denied() {
    let app = test_app();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    // Deliberately not granted in capabilities/default.json.
    let result = call(&webview, "plugin:window|set_always_on_top");

    assert!(
        was_denied_by_acl(&result),
        "expected an ungranted command to be denied — if this fails, the checks \
         above prove nothing. Got: {result:?}"
    );
}
