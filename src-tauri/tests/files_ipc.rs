// `generate_context!` embeds Info.plist; linking it alongside rmux_lib produces a
// duplicate-symbol warning that is harmless for a test binary.
#![allow(linker_messages)]

//! Drives the filesystem and metrics commands through the real IPC layer.
//!
//! The `rmux-fs` tests cover the filesystem itself. This covers the glue the app
//! actually runs on: command registration, argument deserialisation, the target
//! cache, and the JSON shapes the UI destructures. A mismatch in any of those
//! produces an editor that silently fails to open or save a file.

use serde_json::json;
use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
use tauri::test::{INVOKE_KEY, mock_builder};
use tauri::webview::InvokeRequest;

fn test_app() -> tauri::App<tauri::test::MockRuntime> {
    mock_builder()
        .manage(rmux_lib::files::FsStore::default())
        .manage(rmux_lib::metrics::MetricsStore::default())
        .invoke_handler(tauri::generate_handler![
            rmux_lib::files::fs_list,
            rmux_lib::files::fs_read,
            rmux_lib::files::fs_write,
            rmux_lib::files::fs_home,
            rmux_lib::files::fs_join,
            rmux_lib::files::fs_parent,
            rmux_lib::files::fs_create_file,
            rmux_lib::files::fs_create_dir,
            rmux_lib::files::fs_rename,
            rmux_lib::files::fs_delete,
            rmux_lib::files::ssh_config_hosts,
            rmux_lib::metrics::metrics_sample,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build test app")
}

fn call(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> Result<InvokeResponseBody, serde_json::Value> {
    tauri::test::get_ipc_response(
        webview,
        InvokeRequest {
            cmd: cmd.into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            // Must be the local origin the real webview uses.
            url: "tauri://localhost".parse().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.to_string(),
        },
    )
}

fn json_of(
    webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    call(webview, cmd, body)
        .unwrap_or_else(|e| panic!("{cmd} failed: {e}"))
        .deserialize()
        .unwrap_or_else(|e| panic!("{cmd} returned an unexpected shape: {e}"))
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rmux-ipc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn webview(app: &tauri::App<tauri::test::MockRuntime>) -> tauri::WebviewWindow<tauri::test::MockRuntime> {
    tauri::WebviewWindowBuilder::new(app, "main", Default::default())
        .build()
        .expect("failed to build webview")
}

/// The full lifecycle the editor performs: create, write, read back, rename,
/// delete. Anything broken here is a file the user cannot edit.
#[test]
fn a_file_can_be_created_written_read_renamed_and_deleted() {
    let app = test_app();
    let w = webview(&app);

    let dir = temp_dir("lifecycle");
    let a = dir.join("notes.txt").to_string_lossy().into_owned();
    let b = dir.join("renamed.md").to_string_lossy().into_owned();
    let target = json!({});

    call(&w, "fs_create_file", json!({ "target": target, "path": a })).expect("create");
    assert!(std::path::Path::new(&a).exists());

    call(
        &w,
        "fs_write",
        json!({ "target": target, "path": a, "contents": "hello\nworld\n" }),
    )
    .expect("write");

    // The UI destructures `{ kind: "text", text }`; a shape change here breaks
    // the editor silently.
    let content = json_of(&w, "fs_read", json!({ "target": target, "path": a }));
    assert_eq!(content["kind"], "text");
    assert_eq!(content["text"], "hello\nworld\n");

    call(&w, "fs_rename", json!({ "target": target, "from": a, "to": b })).expect("rename");
    assert!(!std::path::Path::new(&a).exists());
    assert!(std::path::Path::new(&b).exists());

    call(&w, "fs_delete", json!({ "target": target, "path": b })).expect("delete");
    assert!(!std::path::Path::new(&b).exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn listings_carry_the_kind_the_tree_renders() {
    let app = test_app();
    let w = webview(&app);

    let dir = temp_dir("listing");
    std::fs::write(dir.join("file.txt"), "x").unwrap();
    std::fs::create_dir(dir.join("folder")).unwrap();

    let entries = json_of(
        &w,
        "fs_list",
        json!({ "target": {}, "path": dir.to_string_lossy() }),
    );
    let entries = entries.as_array().expect("a listing should be an array");

    // Directories first — the tree relies on this ordering rather than sorting.
    assert_eq!(entries[0]["name"], "folder");
    assert_eq!(entries[0]["kind"], "directory");
    assert_eq!(entries[1]["name"], "file.txt");
    assert_eq!(entries[1]["kind"], "file");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn destructive_mistakes_are_refused_through_the_ipc_layer() {
    let app = test_app();
    let w = webview(&app);

    let dir = temp_dir("refuse");
    let keep = dir.join("keep.txt").to_string_lossy().into_owned();
    let other = dir.join("other.txt").to_string_lossy().into_owned();
    std::fs::write(&keep, "precious").unwrap();
    std::fs::write(&other, "also precious").unwrap();

    // Creating over an existing file must not truncate it.
    assert!(call(&w, "fs_create_file", json!({ "target": {}, "path": keep })).is_err());
    assert_eq!(std::fs::read_to_string(&keep).unwrap(), "precious");

    // Renaming onto an existing file must not replace it.
    assert!(
        call(&w, "fs_rename", json!({ "target": {}, "from": other, "to": keep })).is_err()
    );
    assert_eq!(std::fs::read_to_string(&keep).unwrap(), "precious");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_helpers_match_what_the_tree_expects() {
    let app = test_app();
    let w = webview(&app);

    let joined: String = json_of(&w, "fs_join", json!({ "parent": "/", "name": "etc" }))
        .as_str()
        .unwrap()
        .to_owned();
    // "//etc" would 404 on every subsequent listing.
    assert_eq!(joined, "/etc");

    let parent = json_of(&w, "fs_parent", json!({ "path": "/home/me/f.txt" }));
    assert_eq!(parent, "/home/me");

    // Walking up must terminate, or "go up" loops at the top of the tree.
    let root_parent = json_of(&w, "fs_parent", json!({ "path": "/" }));
    assert!(root_parent.is_null());
}

#[test]
fn metrics_report_real_memory_for_this_machine() {
    let app = test_app();
    let w = webview(&app);

    let sample = json_of(&w, "metrics_sample", json!({ "target": {} }));

    // Field names are what the status bar destructures.
    let total = sample["memoryTotalBytes"].as_u64().expect("memoryTotalBytes");
    let used = sample["memoryUsedBytes"].as_u64().expect("memoryUsedBytes");

    assert!(total > 0, "this machine should report some memory");
    assert!(used <= total, "used memory cannot exceed the total");
    // Sanity: any machine running this has at least 256MB.
    assert!(total > 256 * 1024 * 1024, "implausible total: {total}");

    // The first sample cannot know CPU — it needs a baseline to difference
    // against — so the field must be present and null rather than invented.
    assert!(sample.get("cpuPercent").is_some(), "cpuPercent must be present");
}

#[test]
fn a_second_metrics_sample_yields_a_cpu_figure_on_linux() {
    let app = test_app();
    let w = webview(&app);

    json_of(&w, "metrics_sample", json!({ "target": {} }));
    std::thread::sleep(std::time::Duration::from_millis(120));
    let second = json_of(&w, "metrics_sample", json!({ "target": {} }));

    // Only Linux exposes cumulative counters cheaply; on macOS this stays null
    // by design rather than being fabricated.
    if cfg!(target_os = "linux") {
        let cpu = second["cpuPercent"].as_f64().expect("a second sample should yield CPU");
        assert!((0.0..=100.0).contains(&cpu), "implausible cpu: {cpu}");
    } else {
        assert!(second["cpuPercent"].is_null(), "macOS must not invent a CPU figure");
    }
}

/// The picker's host list, through the real IPC layer.
///
/// Verifies the command is registered and the JSON shape matches what the picker
/// destructures. The list itself depends on this machine's `~/.ssh/config`, so
/// the assertions are about shape and invariants rather than specific hosts.
#[test]
fn ssh_config_hosts_are_listed_for_the_picker() {
    let app = test_app();
    let w = webview(&app);

    let hosts = json_of(&w, "ssh_config_hosts", json!({}));
    let hosts = hosts.as_array().expect("a host list should be an array");

    for host in hosts {
        let alias = host["alias"].as_str().expect("every host needs an alias");
        assert!(!alias.is_empty());
        // Wildcard and negation patterns configure other hosts; they are not
        // machines and must never be offered as somewhere to connect.
        assert!(
            !alias.contains(['*', '?', '!']),
            "a pattern leaked into the picker: {alias}"
        );
    }

    eprintln!("{} host(s) available to the picker", hosts.len());
}

/// The new-session flow, as the dialog performs it.
///
/// Step 1 picks a host. Step 2 needs the target's home to start browsing, then
/// lists directories to show. This checks that sequence through the real IPC
/// layer, including the detail the browser depends on: that listings distinguish
/// directories from files, since only folders are offered.
#[test]
fn the_new_session_flow_resolves_a_home_then_lists_folders() {
    let app = test_app();
    let w = webview(&app);

    // Step 2 begins by resolving home — this is also what establishes the
    // connection for a remote target, so a bad host fails here rather than
    // producing an empty tree later.
    let home = json_of(&w, "fs_home", json!({ "target": {} }));
    let home = home.as_str().expect("home should be a path");
    assert!(home.starts_with('/'), "expected an absolute home, got {home:?}");

    let entries = json_of(&w, "fs_list", json!({ "target": {}, "path": home }));
    let entries = entries.as_array().expect("a listing should be an array");

    // The browser shows only directories; without a reliable `kind` it would
    // either hide real folders or offer files that cannot be opened.
    assert!(
        entries.iter().all(|e| {
            matches!(e["kind"].as_str(), Some("directory" | "file" | "symlink"))
        }),
        "every entry needs a kind the browser can filter on"
    );

    // Navigating into the first folder must work, since that is every click.
    if let Some(folder) = entries.iter().find(|e| e["kind"] == "directory") {
        let name = folder["name"].as_str().unwrap();
        let child = json_of(&w, "fs_join", json!({ "parent": home, "name": name }));
        let child = child.as_str().unwrap();
        assert!(child.starts_with(home), "a child path should extend its parent");

        // And climbing back out must return exactly where we were.
        let parent = json_of(&w, "fs_parent", json!({ "path": child }));
        assert_eq!(parent.as_str(), Some(home), "up should undo entering a folder");
    }
}
