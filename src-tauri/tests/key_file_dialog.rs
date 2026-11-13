// See `window_drag.rs` for why the duplicate-symbol linker warning is allowed.
#![allow(linker_messages)]

//! Guards the KEY FILE picker in Connect a server.
//!
//! `plugin:dialog|open` is a **plugin** command, so it is ACL-checked, and a
//! missing grant makes the picker's promise reject with nothing on screen — the
//! button simply does nothing. This asserts the grant is there, and (as a
//! control) that an ungranted dialog command is refused, so the first assertion
//! cannot pass merely because the harness permits everything.

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
            // No `options`: the command fails to deserialise *after* the ACL has
            // admitted it, so no native panel is ever opened by this test.
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
fn the_key_file_picker_is_permitted_and_nothing_else_is() {
    let app = mock_builder()
        .plugin(tauri_plugin_dialog::init())
        .build(tauri::generate_context!())
        .expect("failed to build test app");
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    let open = call(&webview, "plugin:dialog|open");
    assert!(
        !denied(&open),
        "plugin:dialog|open is blocked — add `dialog:allow-open` to capabilities/default.json. Got: {open:?}"
    );
    assert!(open.is_err(), "the empty body must be refused before any panel opens: {open:?}");

    let save = call(&webview, "plugin:dialog|save");
    assert!(denied(&save), "an ungranted dialog command must be denied, or the check above proves nothing: {save:?}");
}
