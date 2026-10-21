//! rmux-web end to end: a real server on an ephemeral loopback port, driven
//! over real HTTP and a real WebSocket.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use rmux_app::{Registry, Web};
use rmux_web::{Options, conns::DEFAULT_QUEUE_CAPACITY};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{self, Message, client::IntoClientRequest as _};

const TOKEN: &str = "test-token-0123456789abcdef";

struct Server {
    base: String,
    addr: std::net::SocketAddr,
    data: tempfile::TempDir,
    _cache: tempfile::TempDir,
    _ui: tempfile::TempDir,
    state: Arc<rmux_web::AppState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamArgs {
    on_channel: String,
}

/// The real table plus test-only commands that exercise the seams.
fn test_registry() -> Registry {
    let mut r = rmux_app::registry();
    r.add("test_stream", Web::Yes, |call, a: StreamArgs| async move {
        let sink = call.sink(&a.on_channel).map_err(|e| e.to_string())?;
        sink.send_bytes(vec![0x00, b'h', b'i', 0xff]).map_err(|e| e.to_string())?;
        sink.send_json(json!({ "n": 1 })).map_err(|e| e.to_string())?;
        Ok::<_, String>("ok")
    });
    r.add("test_emit", Web::Yes, |call, _: rmux_app::NoArgs| async move {
        call.ctx().host().emit("test-event", json!({ "x": [1, 2] }));
        Ok::<_, String>(())
    });
    r.add("test_desktop_only", Web::No, |_, _: rmux_app::NoArgs| async move {
        panic!("a Web::No command ran over HTTP");
        #[allow(unreachable_code)]
        Ok::<_, String>(())
    });
    r
}

async fn start() -> Server {
    let data = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let ui = tempfile::tempdir().unwrap();
    std::fs::write(ui.path().join("index.html"), "<!doctype html><html><head><title>rmux</title></head><body></body></html>")
        .unwrap();
    std::fs::create_dir(ui.path().join("assets")).unwrap();
    std::fs::write(ui.path().join("assets/app.js"), "console.log(1)").unwrap();

    let (router, state) = rmux_web::build(Options {
        registry: test_registry(),
        ui_dir: Some(ui.path().to_path_buf()),
        resource_dir: None,
        data_dir: data.path().join("data"),
        cache_dir: cache.path().join("cache"),
        token: TOKEN.into(),
        queue_capacity: DEFAULT_QUEUE_CAPACITY,
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(rmux_web::serve(listener, router, std::future::pending()));
    Server { base: format!("http://{addr}"), addr, data, _cache: cache, _ui: ui, state }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn invoke(s: &Server, cmd: &str, args: Value, conn: Option<&str>) -> (u16, Value) {
    let mut req = client().post(format!("{}/api/invoke/{cmd}", s.base)).bearer_auth(TOKEN).json(&args);
    if let Some(c) = conn {
        req = req.header("X-Rmux-Conn", c);
    }
    let res = req.send().await.unwrap();
    let status = res.status().as_u16();
    let text = res.text().await.unwrap();
    (status, if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap() })
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(s: &Server, origin: Option<&str>, token: Option<&str>) -> Result<Ws, tungstenite::Error> {
    let mut url = format!("ws://{}/api/ws", s.addr);
    if let Some(t) = token {
        url.push_str(&format!("?token={t}"));
    }
    let mut req = url.into_client_request().unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("Origin", o.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
}

async fn next_frame(ws: &mut Ws) -> Message {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("no frame within 5s").unwrap().unwrap();
        if !matches!(m, Message::Ping(_) | Message::Pong(_)) {
            return m;
        }
    }
}

async fn hello(ws: &mut Ws) -> String {
    match next_frame(ws).await {
        Message::Text(t) => {
            let v: Value = serde_json::from_str(&t).unwrap();
            assert_eq!(v.as_object().unwrap().len(), 1, "hello carries only hello: {v}");
            v["hello"]["conn"].as_str().unwrap().to_owned()
        }
        other => panic!("first frame was not hello: {other:?}"),
    }
}

fn origin(s: &Server) -> String {
    format!("http://{}", s.addr)
}

#[tokio::test]
async fn api_without_a_token_is_401_and_runs_nothing() {
    let s = start().await;
    for (method, path) in [("POST", "/api/invoke/local_target"), ("GET", "/api/file?path=/etc/passwd"), ("GET", "/api/nope")] {
        let res = client().request(method.parse().unwrap(), format!("{}{path}", s.base)).send().await.unwrap();
        assert_eq!(res.status(), 401, "{method} {path}");
        let body: Value = res.json().await.unwrap();
        assert!(body["message"].is_string(), "{body}");
    }
    let wrong = client()
        .post(format!("{}/api/invoke/local_target", s.base))
        .bearer_auth("test-token-0123456789abcdeX")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
}

#[tokio::test]
async fn a_query_token_is_not_accepted_on_invoke() {
    let s = start().await;
    let res = client().post(format!("{}/api/invoke/local_target?token={TOKEN}", s.base)).send().await.unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn unknown_and_desktop_only_commands_are_404() {
    let s = start().await;
    for cmd in ["no_such_command", "run_on_target", "test_desktop_only"] {
        let (status, body) = invoke(&s, cmd, json!({ "program": "touch", "args": ["/tmp/rmux-web-pwned"] }), None).await;
        assert_eq!(status, 404, "{cmd}: {body}");
        assert_eq!(body["message"], format!("Command {cmd} not found"));
    }
    // The registry really does classify run_on_target as desktop-only, so the
    // 404 above is the classification at work, not a missing name.
    assert!(rmux_app::registry().get("run_on_target").is_some_and(|c| c.web() == Web::No));
}

#[tokio::test]
async fn a_web_command_round_trips() {
    let s = start().await;
    let (status, body) = invoke(&s, "local_target", json!({}), None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.is_object(), "{body}");
    // An empty body is "no arguments", as Tauri's invoke("cmd") is.
    let res = client().post(format!("{}/api/invoke/local_target", s.base)).bearer_auth(TOKEN).send().await.unwrap();
    assert_eq!(res.status(), 200);
    // `()` serialises as null.
    let (status, body) = invoke(&s, "test_emit", json!({}), None).await;
    assert_eq!((status, body), (200, Value::Null));
}

#[tokio::test]
async fn bad_arguments_are_a_4xx_with_a_message() {
    let s = start().await;
    let (status, body) = invoke(&s, "git_status", json!({ "nope": 1 }), None).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"].as_str().unwrap().contains("invalid args"), "{body}");

    let res = client()
        .post(format!("{}/api/invoke/local_target", s.base))
        .bearer_auth(TOKEN)
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn websocket_says_hello_first() {
    let s = start().await;
    let mut ws = ws_connect(&s, Some(&origin(&s)), Some(TOKEN)).await.unwrap();
    let conn = hello(&mut ws).await;
    assert_eq!(conn.len(), 32);
    assert_eq!(s.state.conns().len(), 1);
}

#[tokio::test]
async fn websocket_needs_the_token() {
    let s = start().await;
    match ws_connect(&s, Some(&origin(&s)), None).await {
        Err(tungstenite::Error::Http(res)) => assert_eq!(res.status(), 401),
        other => panic!("expected 401, got {:?}", other.map(|_| ())),
    }
}

#[tokio::test]
async fn cross_origin_or_originless_upgrades_are_refused() {
    let s = start().await;
    for o in [Some("http://evil.example"), Some("http://127.0.0.1:1"), Some("null"), None] {
        match ws_connect(&s, o, Some(TOKEN)).await {
            Err(tungstenite::Error::Http(res)) => assert_eq!(res.status(), 403, "{o:?}"),
            other => panic!("{o:?}: expected 403, got {:?}", other.map(|_| ())),
        }
    }
    assert_eq!(s.state.conns().len(), 0);
}

#[tokio::test]
async fn a_channel_streams_exact_frames_to_its_socket() {
    let s = start().await;
    let mut ws = ws_connect(&s, Some(&origin(&s)), Some(TOKEN)).await.unwrap();
    let conn = hello(&mut ws).await;

    let (status, body) = invoke(&s, "test_stream", json!({ "onChannel": "__CHANNEL__:7" }), Some(&conn)).await;
    assert_eq!((status, body), (200, json!("ok")));

    match next_frame(&mut ws).await {
        Message::Binary(b) => assert_eq!(b.as_ref(), &[0, 0, 0, 7, 0x00, b'h', b'i', 0xff]),
        other => panic!("{other:?}"),
    }
    match next_frame(&mut ws).await {
        Message::Text(t) => assert_eq!(t.as_str(), r#"{"channel":7,"data":{"n":1}}"#),
        other => panic!("{other:?}"),
    }
    // The command finished and dropped its sink: the page may forget the channel.
    match next_frame(&mut ws).await {
        Message::Text(t) => assert_eq!(t.as_str(), r#"{"channel":7,"end":true}"#),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_channel_needs_a_live_conn() {
    let s = start().await;
    let (status, body) = invoke(&s, "test_stream", json!({ "onChannel": "__CHANNEL__:1" }), None).await;
    assert_eq!(status, 500);
    assert!(body["message"].as_str().unwrap().contains("X-Rmux-Conn"), "{body}");
    let (status, body) = invoke(&s, "test_stream", json!({ "onChannel": "__CHANNEL__:1" }), Some("deadbeef")).await;
    assert_eq!(status, 500);
    assert!(body["message"].as_str().unwrap().contains("gone"), "{body}");
}

#[tokio::test]
async fn events_reach_every_socket() {
    let s = start().await;
    let mut a = ws_connect(&s, Some(&origin(&s)), Some(TOKEN)).await.unwrap();
    let mut b = ws_connect(&s, Some(&origin(&s)), Some(TOKEN)).await.unwrap();
    hello(&mut a).await;
    hello(&mut b).await;
    let (status, _) = invoke(&s, "test_emit", json!({}), None).await;
    assert_eq!(status, 200);
    for ws in [&mut a, &mut b] {
        match next_frame(ws).await {
            Message::Text(t) => assert_eq!(t.as_str(), r#"{"event":"test-event","payload":{"x":[1,2]}}"#),
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn a_closed_socket_unbinds_its_conn() {
    let s = start().await;
    let mut ws = ws_connect(&s, Some(&origin(&s)), Some(TOKEN)).await.unwrap();
    let conn = hello(&mut ws).await;
    ws.close(None).await.unwrap();
    for _ in 0..50 {
        if s.state.conns().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(s.state.conns().is_empty());
    let (status, _) = invoke(&s, "test_stream", json!({ "onChannel": "__CHANNEL__:1" }), Some(&conn)).await;
    assert_eq!(status, 500);
}

#[tokio::test]
async fn index_is_served_with_the_shell_meta_for_any_ui_path() {
    let s = start().await;
    for path in ["/", "/index.html", "/some/deep/route"] {
        let res = client().get(format!("{}{path}", s.base)).send().await.unwrap();
        assert_eq!(res.status(), 200, "{path}");
        assert_eq!(res.headers()["cache-control"], "no-store");
        let html = res.text().await.unwrap();
        assert!(html.contains(r#"<meta name="rmux-shell" content="web">"#), "{path}: {html}");
    }
    let js = client().get(format!("{}/assets/app.js", s.base)).send().await.unwrap();
    assert_eq!(js.status(), 200);
    assert_eq!(js.text().await.unwrap(), "console.log(1)");
    // Traversal out of the UI dir is not a file read.
    let res = client().get(format!("{}/assets/..%2f..%2f..%2fetc%2fpasswd", s.base)).send().await.unwrap();
    assert!(!res.text().await.unwrap().contains("root:"));
}

#[tokio::test]
async fn files_are_served_only_from_backgrounds() {
    let s = start().await;
    let data = s.data.path().join("data");
    std::fs::create_dir_all(data.join("backgrounds")).unwrap();
    std::fs::write(data.join("backgrounds/wall.png"), b"\x89PNG fake").unwrap();
    std::fs::write(data.join("secret.txt"), b"secret").unwrap();

    let get = |path: String, auth: bool| {
        let mut q = vec![("path", path)];
        if auth {
            q.push(("token", TOKEN.to_owned()));
        }
        let url = format!("{}/api/file", s.base);
        async move { client().get(url).query(&q).send().await.unwrap() }
    };

    let ok = get(data.join("backgrounds/wall.png").display().to_string(), true).await;
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.headers()["content-type"], "image/png");
    assert_eq!(ok.bytes().await.unwrap().as_ref(), b"\x89PNG fake");

    for outside in [
        data.join("secret.txt").display().to_string(),
        data.join("backgrounds/../secret.txt").display().to_string(),
        "/etc/passwd".to_owned(),
        data.join("backgrounds").display().to_string(),
    ] {
        let res = get(outside.clone(), true).await;
        assert_eq!(res.status(), 403, "{outside}");
    }
    assert_eq!(get(data.join("backgrounds/wall.png").display().to_string(), false).await.status(), 401);
}

#[test]
fn the_binary_refuses_a_non_loopback_address() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rmux-web"))
        .args(["--listen", "0.0.0.0:0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a loopback address"), "{err}");
}

#[test]
fn the_binary_refuses_a_token_in_argv() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rmux-web")).args(["--token", "x"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--token-file"));
}
