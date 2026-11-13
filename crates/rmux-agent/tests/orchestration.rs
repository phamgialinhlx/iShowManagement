//! The orchestration CLI end to end, against a **real daemon** on this machine.
//!
//! Drives the compiled binary exactly as an agent inside a session would — as a
//! subprocess, with `RMUX_SESSION` naming the caller — and checks the things
//! that only exist once a daemon, a pty and a shell are real: the environment a
//! session is born with, the `rmux` shim on its `PATH`, `children.json` on disk
//! with its mode, typing into a shell and reading it back, and that `close`
//! refuses a stranger and *kills the process* for its owner.
//!
//! Shells only. Nothing here starts Claude or pi — that would spend the
//! operator's account on a test.

use std::process::{Command, Output};
use std::time::Duration;

/// Daemons are torn down with `pkill`, which is process-wide; see
/// `persistence.rs` for why that forces serial tests.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const BIN: &str = env!("CARGO_BIN_EXE_rmux-agent");

/// A private, **short** HOME under `/tmp`: the daemon socket lives in
/// `~/.rmux`, and macOS's `/var/folders/…` temp dir pushes it past `sun_path`.
struct Sandbox {
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let home = std::path::PathBuf::from(format!("/tmp/rmo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // A zsh with no startup files at all runs the interactive new-user
        // wizard, which would eat the keystrokes this test types.
        std::fs::write(home.join(".zshrc"), "PS1='%% '\n").unwrap();
        Self { home }
    }

    fn rmux(&self, caller: Option<&str>, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/zsh")
            .env_remove("RMUX_SESSION")
            .env_remove("RMUX_BIN")
            .current_dir(&self.home)
            .env("PWD", &self.home);
        if let Some(c) = caller {
            cmd.env("RMUX_SESSION", c);
        }
        cmd.output().expect("run rmux-agent")
    }

    /// Run and parse stdout as JSON, asserting success.
    fn ok(&self, caller: Option<&str>, args: &[&str]) -> serde_json::Value {
        let out = self.rmux(caller, args);
        assert!(
            out.status.success(),
            "{args:?} failed ({:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!("{args:?} printed non-JSON ({e}): {}", String::from_utf8_lossy(&out.stdout))
        })
    }

    /// Run, assert failure with `exit`, and return the parsed error object.
    fn err(&self, caller: Option<&str>, args: &[&str], exit: i32) -> serde_json::Value {
        let out = self.rmux(caller, args);
        assert_eq!(
            out.status.code(),
            Some(exit),
            "{args:?}: stdout {} stderr {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let v: serde_json::Value = serde_json::from_slice(&out.stderr)
            .unwrap_or_else(|e| panic!("stderr is not JSON ({e}): {}", String::from_utf8_lossy(&out.stderr)));
        v["error"].clone()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = Command::new("pkill").args(["-f", &format!("{BIN} daemon")]).status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[cfg(unix)]
fn pid_alive(pid: u64) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(unix)]
#[test]
fn an_agent_spawns_drives_and_closes_a_shell_it_owns() {
    use std::os::unix::fs::PermissionsExt;

    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new();
    let home = sb.home.to_str().unwrap().to_owned();

    // --- usage errors are exit 2, JSON on stderr ------------------------------
    let e = sb.err(None, &["spawn"], 2);
    assert_eq!(e["code"], "usage");
    let e = sb.err(None, &["spawn", "--agent", "shell", "--cwd", "/definitely/not/here"], 2);
    assert_eq!(e["code"], "usage");

    // --- a top-level shell: no caller, so no parent ---------------------------
    let parent = sb.ok(None, &["spawn", "--agent", "shell"]);
    let parent_name = parent["session"].as_str().unwrap().to_owned();
    assert!(parent_name.starts_with("term-"), "{parent}");
    assert_eq!(parent["parent"], serde_json::Value::Null);
    assert_eq!(parent["kind"], "terminal");
    // Default cwd is the caller's.
    assert_eq!(parent["cwd"], home.as_str());

    // --- a child, spawned "from inside" the parent ----------------------------
    let child = sb.ok(Some(&parent_name), &["spawn", "--agent", "shell", "--name", "kid", "--cwd", &home]);
    let child_name = child["session"].as_str().unwrap().to_owned();
    assert!(child_name.starts_with("term-") && child_name != parent_name, "{child}");
    assert_eq!(child["parent"], parent_name.as_str());

    // --- children.json: mode and contents -------------------------------------
    let file = sb.home.join(".rmux/children.json");
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "children.json must be 0600, is {mode:o}");
    let dir_mode = std::fs::metadata(sb.home.join(".rmux")).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700, "~/.rmux must be 0700, is {dir_mode:o}");
    let on_disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 1);
    let entries = on_disk["children"].as_array().unwrap();
    let kid = entries.iter().find(|c| c["session"] == child_name.as_str()).expect("child recorded");
    assert_eq!(kid["parent"], parent_name.as_str());
    assert_eq!(kid["kind"], "terminal");
    assert_eq!(kid["cwd"], home.as_str());
    assert_eq!(kid["name"], "kid");
    assert!(kid["createdAt"].as_u64().unwrap() > 1_700_000_000_000);
    let top = entries.iter().find(|c| c["session"] == parent_name.as_str()).expect("parent recorded");
    assert_eq!(top["parent"], serde_json::Value::Null);

    // `--name` also named it for the rail, so the label resolves.
    let listed = sb.ok(None, &["list", "--json"]);
    let row = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session"] == child_name.as_str())
        .expect("child listed")
        .clone();
    assert_eq!(row["alias"], "kid");
    assert_eq!(row["parent"], parent_name.as_str());
    assert_eq!(row["kind"], "terminal");
    assert_eq!(row["status"], "unknown");
    let child_pid = row["pid"].as_u64().expect("a pid");

    // --- the session's own environment: who it is, and `rmux` on PATH ---------
    sb.ok(None, &["prompt", &child_name, "echo ENV-$RMUX_ENV-$((6*7)) $RMUX_SESSION"]);
    let hit = sb.ok(None, &["wait-output", &child_name, "--regex", r"ENV-1-42 term-\S+", "--timeout", "10"]);
    assert_eq!(
        hit["matched"].as_str().unwrap(),
        format!("ENV-1-42 {child_name}"),
        "RMUX_ENV/RMUX_SESSION not injected: {hit}"
    );
    sb.ok(None, &["prompt", &child_name, "rmux whoami"]);
    let who = sb.ok(
        None,
        &["wait-output", &child_name, "--match", &format!("\"session\":\"{child_name}\""), "--timeout", "10"],
    );
    assert!(who["line"].as_str().unwrap().contains("\"host\""), "the shim did not reach the agent: {who}");

    // --- keys, then read the screen back --------------------------------------
    sb.ok(None, &["keys", &child_name, "e", "c", "h", "o", "space", "K", "E", "Y", "S", "enter"]);
    sb.ok(None, &["wait-output", &child_name, "--regex", r"(?m)^KEYS$", "--timeout", "10"]);
    let bad = sb.err(None, &["keys", &child_name, "enter", "entr"], 2);
    assert!(bad["message"].as_str().unwrap().contains("entr"), "{bad}");
    let screen = sb.rmux(None, &["read", &child_name, "--lines", "40"]);
    let screen = String::from_utf8_lossy(&screen.stdout);
    assert!(screen.contains("ENV-1-42"), "read --source screen missed the output:\n{screen}");
    assert!(screen.contains("KEYS"), "{screen}");
    assert!(!screen.contains('\u{1b}'), "escape sequences leaked into read:\n{screen}");

    // A terminal has no agent status to wait on.
    let e = sb.err(None, &["wait", &child_name, "--until", "idle"], 1);
    assert_eq!(e["code"], "unsupported");

    // --- close: strangers are refused, the owner kills it ---------------------
    let e = sb.err(None, &["close", &child_name], 1);
    assert_eq!(e["code"], "not_owner");
    let e = sb.err(Some("term-somebody-else"), &["close", &child_name], 1);
    assert_eq!(e["code"], "not_owner");
    // A child cannot close its parent either.
    let e = sb.err(Some(&child_name), &["close", &parent_name], 1);
    assert_eq!(e["code"], "not_owner");
    assert!(pid_alive(child_pid), "a refused close must not have touched the process");

    let closed = sb.ok(Some(&parent_name), &["close", &child_name]);
    assert_eq!(closed["closed"], true);
    assert!(!pid_alive(child_pid), "close reported success but pid {child_pid} is alive");

    // --- pruned -----------------------------------------------------------------
    let tree = sb.ok(None, &["children"]);
    let sessions: Vec<&str> =
        tree["children"].as_array().unwrap().iter().map(|c| c["session"].as_str().unwrap()).collect();
    assert_eq!(sessions, vec![parent_name.as_str()], "{tree}");
    let on_disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(on_disk, tree, "the pruned list is written back");
    let e = sb.err(Some(&parent_name), &["prompt", &child_name, "x"], 1);
    assert_eq!(e["code"], "not_found");

    // --- latitude is never escalated (refused before anything is spawned) ------
    let e = sb.err(Some(&parent_name), &["spawn", "--agent", "claude", "--skip-permissions"], 1);
    assert_eq!(e["code"], "permission_denied");
    let e = sb.err(Some(&parent_name), &["spawn", "--agent", "claude", "--profile", "glm"], 1);
    assert_eq!(e["code"], "unsupported");
    let after = sb.ok(None, &["list", "--json"]);
    assert_eq!(after["sessions"].as_array().unwrap().len(), 1, "a refused spawn started something: {after}");

    // --- --force is the operator's escape hatch -------------------------------
    let e = sb.err(None, &["close", &parent_name], 1);
    assert_eq!(e["code"], "not_owner");
    sb.ok(None, &["close", &parent_name, "--force"]);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(sb.ok(None, &["children"])["children"].as_array().unwrap().len(), 0);
}
