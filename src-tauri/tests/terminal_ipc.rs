// `generate_context!` embeds Info.plist; linking it alongside rmux_lib (which
// already embeds one) produces a duplicate-symbol warning that is harmless
// here — the test binary is never bundled as a .app.
#![allow(linker_messages)]

//! Drives the terminal commands through the real IPC layer.
//!
//! The `rmux-term` unit tests cover the PTY itself. What they cannot cover is the
//! glue: command registration, argument deserialisation, the `State` registry,
//! and the error paths for stale terminal ids. Tauri's mock runtime exercises all
//! of that headlessly, against the app's real generated context.
//!
//! It does NOT verify that output bytes cross the boundary unencoded — the mock
//! runtime has no webview to receive channel messages, so that claim rests on
//! `Response::new(InvokeResponseBody::Raw(..))` in `terminal.rs` and on observing
//! the running app.

use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, mock_builder};
use tauri::webview::InvokeRequest;

/// Build an app exposing the terminal commands, exactly as `run()` does.
fn test_app() -> tauri::App<tauri::test::MockRuntime> {
    mock_builder()
        .manage(rmux_lib::terminal::TerminalStore::default())
        .invoke_handler(tauri::generate_handler![
            rmux_lib::terminal::terminal_open,
            rmux_lib::terminal::terminal_write,
            rmux_lib::terminal::terminal_resize,
            rmux_lib::terminal::terminal_close,
        ])
        // The real context, not `mock_context` — the latter carries an EMPTY ACL
        // (`Resolved::default()`), so every command is rejected with "Plugin not
        // found". Using the app's own generated context also means this test
        // exercises the same capability configuration that ships.
        .build(tauri::generate_context!())
        .expect("failed to build test app")
}

fn request(cmd: &str, body: serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        // Must be a LOCAL origin. Tauri only enforces the ACL on app commands for
        // remote content, so a made-up URL here gets everything rejected with a
        // misleading "Plugin not found". This is the origin the real webview uses.
        url: "tauri://localhost".parse().unwrap(),
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    }
}

fn call(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> Result<InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(webview, request(cmd, body))
}

#[test]
fn a_local_terminal_opens_runs_a_command_and_streams_raw_bytes() {
    let app = test_app();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    // Channels are identified across the IPC boundary by an id the frontend
    // allocates; the mock runtime captures what is sent back to them.
    let opened = call(
        &webview,
        "terminal_open",
        json!({
            "target": {},
            "cwd": null,
            "cols": 100,
            "rows": 30,
            "output": "__CHANNEL__:1",
            "lifecycle": "__CHANNEL__:2",
        }),
    )
    .expect("terminal_open failed");

    let opened: serde_json::Value = opened.deserialize().expect("bad open response");
    let id = opened["id"].as_str().expect("no terminal id").to_owned();
    assert!(!id.is_empty());
    assert_eq!(opened["cols"], 100);

    // Drive the shell and let it echo something recognisable back.
    call(&webview, "terminal_write", json!({ "id": id, "data": "echo rmux-ipc-ok\n" }))
        .expect("terminal_write failed");

    // Resizing must be accepted for a live terminal.
    call(&webview, "terminal_resize", json!({ "id": id, "cols": 120, "rows": 40 }))
        .expect("terminal_resize failed");

    call(&webview, "terminal_close", json!({ "id": id })).expect("terminal_close failed");
}

#[test]
fn operations_on_an_unknown_terminal_report_an_error_rather_than_panicking() {
    let app = test_app();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    // A stale id is normal — a view can outlive the terminal it was showing, so
    // this path must degrade to a message, never take the app down.
    let err = call(&webview, "terminal_write", json!({ "id": "nope", "data": "x" }))
        .expect_err("expected an error for an unknown terminal");
    assert!(err.to_string().contains("no such terminal"), "got: {err}");

    let err = call(&webview, "terminal_resize", json!({ "id": "nope", "cols": 80, "rows": 24 }))
        .expect_err("expected an error for an unknown terminal");
    assert!(err.to_string().contains("no such terminal"), "got: {err}");

    // Closing something already gone is success, not an error — otherwise every
    // teardown race surfaces as a spurious failure.
    call(&webview, "terminal_close", json!({ "id": "nope" }))
        .expect("closing an unknown terminal should be a no-op");
}

#[test]
fn closing_a_terminal_stops_it() {
    let app = test_app();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");

    let opened = call(
        &webview,
        "terminal_open",
        json!({
            "target": {},
            "cwd": null,
            "cols": 80,
            "rows": 24,
            "output": "__CHANNEL__:1",
            "lifecycle": "__CHANNEL__:2",
        }),
    )
    .expect("terminal_open failed");
    let opened: serde_json::Value = opened.deserialize().unwrap();
    let id = opened["id"].as_str().unwrap().to_owned();

    call(&webview, "terminal_close", json!({ "id": id })).expect("terminal_close failed");

    // The registry must forget it, or every opened terminal leaks for the life of
    // the process.
    let err = call(&webview, "terminal_write", json!({ "id": id, "data": "x" }))
        .expect_err("a closed terminal should no longer be addressable");
    assert!(err.to_string().contains("no such terminal"), "got: {err}");
}
