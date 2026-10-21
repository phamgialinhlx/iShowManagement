//! The registry, driven the way a transport drives it: a name, a JSON argument
//! object, and a `Call` built from a fake host.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use rmux_app::{Call, Ctx, Host, NoArgs, Registry, SecretStore, Sink, SinkResolver, Web};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Default)]
struct FakeHost {
    events: Mutex<Vec<(String, Value)>>,
}

impl Host for FakeHost {
    fn resource_dir(&self) -> Option<PathBuf> {
        None
    }
    fn data_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(std::env::temp_dir())
    }
    fn cache_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(std::env::temp_dir())
    }
    fn emit(&self, event: &str, payload: Value) {
        self.events.lock().push((event.to_owned(), payload));
    }
    fn secrets(&self) -> Option<&dyn SecretStore> {
        None
    }
}

/// A sink that records what it was sent, for one channel id.
#[derive(Default)]
struct Recorder {
    bytes: Mutex<Vec<Vec<u8>>>,
    json: Mutex<Vec<Value>>,
}
impl Sink for Recorder {
    fn send_bytes(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.bytes.lock().push(bytes);
        Ok(())
    }
    fn send_json(&self, value: Value) -> anyhow::Result<()> {
        self.json.lock().push(value);
        Ok(())
    }
}

struct OneChannel(Arc<Recorder>);
impl SinkResolver for OneChannel {
    fn resolve(&self, id: &str) -> anyhow::Result<Arc<dyn Sink>> {
        anyhow::ensure!(id == "__CHANNEL__:7", "unknown channel {id}");
        Ok(self.0.clone())
    }
}

fn call() -> (Call, Arc<FakeHost>, Arc<Recorder>) {
    let host = Arc::new(FakeHost::default());
    let recorder = Arc::new(Recorder::default());
    let ctx = Arc::new(Ctx::new(host.clone()));
    (Call::new(ctx, Arc::new(OneChannel(recorder.clone()))), host, recorder)
}

#[tokio::test]
async fn a_moved_command_dispatches_from_json() {
    let (call, _, _) = call();
    let out = rmux_app::registry().invoke("local_target", call, json!({})).await.unwrap();
    // Byte-for-byte what the function itself serialises to — the registry adds
    // no wrapping of its own.
    let direct = serde_json::to_value(rmux_app::commands::local_target().await).unwrap();
    assert_eq!(out, direct);
    assert_eq!(out["id"], json!("Local"), "{out}");
}

#[tokio::test]
async fn git_reads_a_real_repository_through_the_registry() {
    // This crate lives in a git checkout, so `git_repo` on its own directory has
    // an answer. Keys are exactly what `ui/src/lib/api.ts` sends.
    let (call, _, _) = call();
    let here = env!("CARGO_MANIFEST_DIR");
    let out = rmux_app::registry()
        .invoke("git_repo", call, json!({ "target": {}, "folder": here }))
        .await
        .unwrap();
    let root = out["root"].as_str().unwrap_or_else(|| panic!("no root in {out}"));
    assert!(here.starts_with(root), "{root} is not above {here}");
}

#[tokio::test]
async fn errors_keep_their_text() {
    // The same string the function produces when called directly, so the UI
    // shows what it always showed.
    let (call, _, _) = call();
    let program = "rmux-no-such-program-anywhere";
    let err = rmux_app::registry()
        .invoke("run_on_target", call, json!({ "program": program, "args": [] }))
        .await
        .unwrap_err();
    let direct = rmux_app::commands::run_on_target(program.into(), vec![]).await.unwrap_err();
    assert_eq!(err, direct);
    assert!(!err.is_empty());
}

#[tokio::test]
async fn an_unknown_command_is_refused_in_tauris_words() {
    let (call, _, _) = call();
    let err = rmux_app::registry().invoke("no_such_command", call, json!({})).await.unwrap_err();
    assert_eq!(err, "Command no_such_command not found");
}

#[tokio::test]
async fn missing_args_name_the_command_and_the_field() {
    let (call, _, _) = call();
    let err =
        rmux_app::registry().invoke("git_status", call, json!({ "target": {} })).await.unwrap_err();
    assert!(err.contains("git_status") && err.contains("root"), "{err}");
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snake {
    session_id: String,
    max_bytes: Option<u64>,
}

#[tokio::test]
async fn args_are_camel_case_like_tauri() {
    let mut r = Registry::new();
    r.add("echo", Web::Yes, |_, a: Snake| async move {
        Ok::<_, String>(json!({ "id": a.session_id, "max": a.max_bytes }))
    });

    let (c, _, _) = call();
    let out = r.invoke("echo", c, json!({ "sessionId": "s1", "maxBytes": 9 })).await.unwrap();
    assert_eq!(out, json!({ "id": "s1", "max": 9 }));

    // An absent `Option` is `None`, as a Tauri parameter was.
    let (c, _, _) = call();
    let out = r.invoke("echo", c, json!({ "sessionId": "s1" })).await.unwrap();
    assert_eq!(out, json!({ "id": "s1", "max": null }));

    // The snake_case spelling is *not* accepted: the UI never sent it, and
    // accepting both would hide a call site using the wrong one.
    let (c, _, _) = call();
    assert!(r.invoke("echo", c, json!({ "session_id": "s1" })).await.is_err());
}

#[tokio::test]
async fn no_args_accepts_empty_and_null() {
    let mut r = Registry::new();
    r.add("ping", Web::Yes, |_, _: NoArgs| async { Ok::<_, String>("pong") });
    for args in [json!({}), Value::Null] {
        let (c, _, _) = call();
        assert_eq!(r.invoke("ping", c, args).await.unwrap(), json!("pong"));
    }
}

#[tokio::test]
async fn a_handler_reaches_its_sink_host_and_stores() {
    #[derive(Default)]
    struct Hits(Mutex<u32>);

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Args {
        on_output: String,
    }

    let mut r = Registry::new();
    r.add("stream", Web::Yes, |call: Call, a: Args| async move {
        let sink = call.sink(&a.on_output).map_err(|e| e.to_string())?;
        sink.send_bytes(vec![0, 159, 255]).map_err(|e| e.to_string())?;
        sink.send_json(json!({ "type": "exited", "code": 0 })).map_err(|e| e.to_string())?;
        call.ctx().host().emit("rmux:test", json!(1));
        *call.ctx().store::<Hits>().0.lock() += 1;
        Ok::<_, String>(())
    });

    let (c, host, rec) = call();
    let ctx = c.ctx_arc();
    r.invoke("stream", c, json!({ "onOutput": "__CHANNEL__:7" })).await.unwrap();
    assert_eq!(*rec.bytes.lock(), vec![vec![0u8, 159, 255]]);
    assert_eq!(*rec.json.lock(), vec![json!({ "type": "exited", "code": 0 })]);
    assert_eq!(*host.events.lock(), vec![("rmux:test".to_owned(), json!(1))]);
    assert_eq!(*ctx.store::<Hits>().0.lock(), 1);

    // A channel the transport does not know is an error from the command.
    let (c, _, _) = call();
    let err = r.invoke("stream", c, json!({ "onOutput": "__CHANNEL__:8" })).await.unwrap_err();
    assert!(err.contains("unknown channel"), "{err}");
}

#[test]
#[should_panic(expected = "registered twice")]
fn a_duplicate_name_panics() {
    let mut r = rmux_app::registry();
    r.add("git_status", Web::Yes, |_, _: NoArgs| async { Ok::<_, String>(()) });
}

#[test]
fn every_command_is_classified_and_the_table_builds() {
    let r = rmux_app::registry();
    let names: Vec<_> = r.commands().map(|c| (c.name(), c.web())).collect();
    for expected in ["local_target", "git_repo", "git_status", "git_log", "git_commit_files", "git_file_diff"] {
        assert!(r.contains(expected), "{expected} missing from {names:?}");
    }
    // The exec-shaped one stays off the web.
    assert_eq!(r.get("run_on_target").map(|c| c.web()), Some(Web::No));
}
