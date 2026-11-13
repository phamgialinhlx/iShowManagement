//! The worktree mirror and `AgentFs`, end to end on this machine: the same
//! commands the UI calls, the in-process `fs-serve`, a real watcher on a real
//! temp directory, and the events the UI would hear.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rmux_app::{Ctx, Host, SecretStore, TargetRef};
use serde_json::Value;

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

fn local() -> TargetRef {
    TargetRef::default()
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rmux-worktree-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical, so the paths the UI builds match what is on disk.
    dir.canonicalize().unwrap()
}

async fn wait_for(host: &FakeHost, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some((_, v)) = host.events.lock().iter().find(|(e, v)| e == "fs-worktree" && pred(v)) {
            return v.clone();
        }
        assert!(Instant::now() < deadline, "never saw: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_local_worktree_snapshots_and_streams_changes_to_the_ui() {
    let dir = temp("stream");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}").unwrap();
    std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir_all(dir.join("build/out")).unwrap();

    let host = Arc::new(FakeHost::default());
    let ctx = Arc::new(Ctx::new(host.clone()));
    let snap = rmux_app::worktree::open(&ctx, local(), dir.to_string_lossy().into_owned()).await.unwrap();
    let paths: Vec<&str> = snap.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"src/lib.rs"), "{paths:?}");
    assert!(snap.entries.iter().any(|e| e.path == "build" && e.lazy && e.ignored));
    assert!(!paths.contains(&"build/out"), "ignored directories are never scanned");

    // A second open of the same root joins the same mirror.
    let again = rmux_app::worktree::open(&ctx, local(), dir.to_string_lossy().into_owned()).await.unwrap();
    assert_eq!(again.id, snap.id);

    std::fs::write(dir.join("src/new.rs"), "x").unwrap();
    let patch = wait_for(&host, "the created file as a patch", |v| {
        v["id"] == snap.id
            && v["kind"] == "patch"
            && v["upserts"].as_array().is_some_and(|u| u.iter().any(|e| e["path"] == "src/new.rs"))
    })
    .await;
    assert!(patch["seq"].as_u64().unwrap() > snap.seq, "patches are sequenced after the snapshot");

    std::fs::remove_file(dir.join("src/new.rs")).unwrap();
    wait_for(&host, "the removal", |v| {
        v["kind"] == "patch" && v["removes"].as_array().is_some_and(|r| r.iter().any(|p| p == "src/new.rs"))
    })
    .await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_quotes_its_version_and_a_stale_one_is_a_conflict() {
    let dir = temp("versioned");
    let file = dir.join("notes.md");
    std::fs::write(&file, "v1").unwrap();
    let path = file.to_string_lossy().into_owned();

    let host = Arc::new(FakeHost::default());
    let ctx = Arc::new(Ctx::new(host.clone()));
    // Open the mirror first so the fs-serve connection is live.
    rmux_app::worktree::open(&ctx, local(), dir.to_string_lossy().into_owned()).await.unwrap();

    let read = rmux_app::files::fs_read_versioned(&ctx, local(), path.clone()).await.unwrap();
    let v1 = read.version.clone().expect("the live path vouches for a version");

    let v2 = rmux_app::files::fs_write_versioned(&ctx, local(), path.clone(), "v2".into(), Some(v1.clone()))
        .await
        .unwrap()
        .expect("a new version");
    assert_ne!(v1, v2);

    // An agent rewrites it behind the editor's back.
    tokio::time::sleep(Duration::from_millis(20)).await;
    std::fs::write(&file, "agent").unwrap();
    let err = rmux_app::files::fs_write_versioned(&ctx, local(), path.clone(), "mine".into(), Some(v2))
        .await
        .unwrap_err();
    assert!(err.starts_with(rmux_app::worktree::CONFLICT_PREFIX), "{err}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "agent", "neither side is silently overwritten");

    // And a read after the watcher caught up returns the agent's bytes, not a
    // stale cached copy.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let fresh = rmux_app::files::fs_read_versioned(&ctx, local(), path.clone()).await.unwrap();
    match fresh.content {
        rmux_fs::FileContent::Text { text } => assert_eq!(text, "agent"),
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ordinary_fs_commands_go_through_fs_serve_and_keep_their_rules() {
    let dir = temp("ordinary");
    let host = Arc::new(FakeHost::default());
    let ctx = Arc::new(Ctx::new(host.clone()));
    rmux_app::worktree::open(&ctx, local(), dir.to_string_lossy().into_owned()).await.unwrap();
    let p = |name: &str| dir.join(name).to_string_lossy().into_owned();

    rmux_app::files::fs_create_file(&ctx, local(), p("a.txt")).await.unwrap();
    let err = rmux_app::files::fs_create_file(&ctx, local(), p("a.txt")).await.unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    rmux_app::files::fs_create_file(&ctx, local(), p("b.txt")).await.unwrap();
    let err = rmux_app::files::fs_rename(&ctx, local(), p("a.txt"), p("b.txt")).await.unwrap_err();
    assert!(err.contains("already exists"), "{err}");
    rmux_app::files::fs_create_dir(&ctx, local(), p("sub")).await.unwrap();
    let listed = rmux_app::files::fs_list(&ctx, local(), dir.to_string_lossy().into_owned()).await.unwrap();
    let names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["sub", "a.txt", "b.txt"], "directories first, as before");
    rmux_app::files::fs_delete(&ctx, local(), p("sub")).await.unwrap();
    assert!(!dir.join("sub").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
