// `generate_context!` embeds Info.plist; linking it alongside rmux_lib (which
// already embeds one) produces a duplicate-symbol warning that is harmless
// here — the test binary is never bundled as a .app.
#![allow(linker_messages)]

//! The desktop bridge to `rmux-app`'s registry, driven through Tauri's real IPC
//! path on the mock runtime: ACL resolution, `Invoke` dispatch, the resolver,
//! and channel ids resolved on the invoking webview.
//!
//! What this cannot reach is the webview's JavaScript: on the mock runtime a
//! channel message is observed through Tauri's `channel_interceptor` — the hook
//! `JavaScriptChannelId::channel_on` consults before it would `eval` into the
//! page — so the *body and callback id* handed to the page are verified, but the
//! page actually receiving them is not.

use std::sync::{Arc, Mutex};

use rmux_app::{Call, NoArgs, Registry, Web};
use serde::Deserialize;
use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, MockRuntime, mock_builder};
use tauri::webview::InvokeRequest;

type Captured = Arc<Mutex<Vec<(u32, usize, InvokeResponseBody)>>>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamArgs {
    on_output: String,
}

/// The real registry plus one test command that streams through a sink.
fn registry() -> Registry {
    let mut r = rmux_app::registry();
    r.add("test_stream", Web::No, |call: Call, a: StreamArgs| async move {
        let sink = call.sink(&a.on_output).map_err(|e| e.to_string())?;
        sink.send_bytes(vec![0, 1, 254, 255]).map_err(|e| e.to_string())?;
        sink.send_json(json!({ "type": "exited", "code": 3 })).map_err(|e| e.to_string())?;
        Ok::<_, String>("streamed")
    });
    r.add("test_store", Web::No, |call: Call, _: NoArgs| async move {
        #[derive(Default)]
        struct Count(Mutex<u32>);
        let store = call.ctx().store::<Count>();
        let mut n = store.0.lock().unwrap();
        *n += 1;
        Ok::<_, String>(*n)
    });
    r
}

fn test_app(captured: Captured) -> tauri::App<MockRuntime> {
    mock_builder()
        .invoke_handler(rmux_lib::bridge::invoke_handler(
            Arc::new(registry()),
            // The fallback: a desktop-only command, answered by the macro.
            tauri::generate_handler![rmux_lib::logs::log_status],
        ))
        .channel_interceptor(move |_webview, callback, index, body| {
            captured.lock().unwrap().push((callback.0, index, body.clone()));
            true
        })
        // The real context, not `mock_context`, whose empty ACL rejects every
        // command — and this way the shipped capability configuration is the
        // one exercised: a registry command must need no grant, as before.
        .build(tauri::generate_context!())
        .expect("failed to build test app")
}

fn call(
    webview: &tauri::WebviewWindow<MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    let request = InvokeRequest {
        cmd: cmd.into(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        // A LOCAL origin, as the real webview uses; app commands are only
        // ACL-checked for remote content.
        url: "tauri://localhost".parse().unwrap(),
        body: InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: INVOKE_KEY.to_string(),
    };
    tauri::test::get_ipc_response(webview, request).map(|r| r.deserialize().expect("JSON body"))
}

fn app_and_webview() -> (tauri::App<MockRuntime>, tauri::WebviewWindow<MockRuntime>, Captured) {
    let captured: Captured = Default::default();
    let app = test_app(captured.clone());
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .expect("failed to build webview");
    (app, webview, captured)
}

#[test]
fn a_registry_command_answers_through_tauri_ipc() {
    let (_app, webview, _) = app_and_webview();

    let out = call(&webview, "local_target", json!({})).expect("local_target failed");
    assert_eq!(out["id"], json!("Local"), "{out}");

    // Same keys `ui/src/lib/api.ts` sends; this crate is inside a checkout.
    let here = env!("CARGO_MANIFEST_DIR");
    let out = call(&webview, "git_repo", json!({ "target": {}, "folder": here }))
        .expect("git_repo failed");
    let root = out["root"].as_str().unwrap_or_else(|| panic!("no root in {out}"));
    assert!(here.starts_with(root), "{root} is not above {here}");
}

#[test]
fn an_error_rejects_with_the_commands_own_string() {
    let (_app, webview, _) = app_and_webview();
    let err = call(&webview, "git_status", json!({ "target": {} })).expect_err("missing root");
    let text = err.as_str().unwrap_or_else(|| panic!("error is not a bare string: {err}"));
    assert!(text.contains("git_status") && text.contains("root"), "{text}");
}

#[test]
fn an_unregistered_command_falls_back_to_generate_handler() {
    let (_app, webview, _) = app_and_webview();
    // Answered by the macro list, not the registry.
    call(&webview, "log_status", json!({})).expect("fallback command failed");
    // Neither knows it: Tauri's own refusal.
    let err = call(&webview, "nope_not_a_command", json!({})).expect_err("unknown");
    assert_eq!(err, json!("Command nope_not_a_command not found"));
}

#[test]
fn a_sink_resolves_to_a_channel_on_the_invoking_webview() {
    let (_app, webview, captured) = app_and_webview();

    let out = call(&webview, "test_stream", json!({ "onOutput": "__CHANNEL__:42" }))
        .expect("test_stream failed");
    assert_eq!(out, json!("streamed"));

    let got = captured.lock().unwrap().clone();
    assert_eq!(got.len(), 2, "{got:?}");
    // Callback 42 is the id the page allocated; messages are numbered in order.
    assert_eq!((got[0].0, got[0].1), (42, 0));
    assert_eq!((got[1].0, got[1].1), (42, 1));
    // Bytes stay raw — an ArrayBuffer in JS, never a JSON array of numbers.
    match &got[0].2 {
        InvokeResponseBody::Raw(b) => assert_eq!(b, &vec![0u8, 1, 254, 255]),
        other => panic!("bytes were not sent raw: {other:?}"),
    }
    match &got[1].2 {
        InvokeResponseBody::Json(s) => {
            assert_eq!(serde_json::from_str::<serde_json::Value>(s).unwrap(), json!({ "type": "exited", "code": 3 }))
        }
        other => panic!("json was not sent as json: {other:?}"),
    }
}

#[test]
fn a_malformed_channel_id_is_an_error_not_a_panic() {
    let (_app, webview, captured) = app_and_webview();
    let err = call(&webview, "test_stream", json!({ "onOutput": "not-a-channel" }))
        .expect_err("bad channel accepted");
    assert!(err.as_str().is_some_and(|s| s.contains("invalid channel value")), "{err}");
    assert!(captured.lock().unwrap().is_empty());
}

#[test]
fn every_invoke_shares_one_ctx() {
    let (_app, webview, _) = app_and_webview();
    // No `setup` ran here — the Ctx is created by the first invoke and reused.
    assert_eq!(call(&webview, "test_store", json!({})).unwrap(), json!(1));
    assert_eq!(call(&webview, "test_store", json!(null)).unwrap(), json!(2));
}

/// The names inside `generate_handler![…]` in `src/lib.rs`.
fn fallback_names() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let source = std::fs::read_to_string(&path).expect("read src/lib.rs");
    let marker = "tauri::generate_handler![";
    let lists: Vec<&str> = source.match_indices(marker).map(|(i, _)| &source[i + marker.len()..]).collect();
    assert_eq!(lists.len(), 1, "expected exactly one generate_handler! in src/lib.rs");
    let body = &lists[0][..lists[0].find(']').expect("unterminated generate_handler!")];
    body.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with("//"))
        .map(|path| path.rsplit("::").next().unwrap().to_owned())
        .collect()
}

#[test]
fn no_command_is_both_registered_and_in_the_fallback() {
    let fallback = fallback_names();
    // If the parse silently found nothing, an empty intersection proves nothing.
    assert!(fallback.len() > 50, "only {} fallback names parsed — the scan is wrong", fallback.len());
    // A name known to be desktop-only, so a scan that parsed the wrong list
    // fails here. (`terminal_open` served this role until it moved to rmux-app.)
    assert!(fallback.iter().any(|n| n == "server_disconnect"), "{fallback:?}");

    let registry = rmux_app::registry();
    let both: Vec<_> = fallback.iter().filter(|n| registry.contains(n)).collect();
    assert!(
        both.is_empty(),
        "registered in rmux-app AND still in generate_handler! — the fallback copy is dead: {both:?}"
    );

    // And the fallback has no duplicates of its own.
    let mut sorted = fallback.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), fallback.len(), "a name appears twice in generate_handler!");
}
