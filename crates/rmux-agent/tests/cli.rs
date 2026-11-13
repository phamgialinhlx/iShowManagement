//! The tmux-style CLI end to end, against a real daemon in a private HOME.
//!
//! Shells only, local only — plus one deliberately unreachable server to prove
//! `ls` reports it as a line rather than failing.

use std::process::{Command, Output};

/// Daemons are torn down with `pkill` on this test binary's own path, which is
/// process-wide; see `persistence.rs` for why that forces serial tests.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

const BIN: &str = env!("CARGO_BIN_EXE_rmux-agent");

/// A short HOME under `/tmp` — `sun_path` (see `orchestration.rs`).
struct Sandbox {
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let home = std::path::PathBuf::from(format!("/tmp/rmc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".zshrc"), "PS1='%% '\n").unwrap();
        Self { home }
    }

    fn rmux(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/zsh")
            .env_remove("RMUX_SESSION")
            .env_remove("RMUX_BIN")
            .env_remove("RMUX_DETACH_KEY")
            .current_dir(&self.home)
            .env("PWD", &self.home)
            .output()
            .expect("run rmux-agent")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.rmux(args);
        assert!(
            out.status.success(),
            "{args:?} failed ({:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = Command::new("pkill").args(["-f", &format!("{BIN} daemon")]).status();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[cfg(unix)]
#[test]
fn new_ls_rename_has_and_kill_work_like_tmux() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let sb = Sandbox::new();

    // `-V` is what a remote line uses to tell this CLI apart from older builds.
    assert!(sb.ok(&["-V"]).starts_with("rmux "));
    assert!(sb.ok(&["--help"]).contains("kill-session"));

    // --- new -d -s: a shell in the app's naming scheme, with the alias ------
    assert_eq!(sb.ok(&["new", "-d", "-s", "demo"]).trim(), "demo");
    let aliases: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.home.join(".rmux/aliases.json")).unwrap()).unwrap();
    let (key, alias) = aliases.as_object().unwrap().iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap();
    assert!(key.starts_with("term-"), "{key}");
    assert_eq!(alias, "demo");

    // A second session under the same name is refused before anything starts.
    let dup = sb.rmux(&["new", "-d", "-s", "demo"]);
    assert_eq!(dup.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&dup.stderr).contains("duplicate session"));

    // --- ls: tmux-like line, plus JSON --------------------------------------
    let ls = sb.ok(&["ls"]);
    assert!(ls.starts_with("local:demo"), "{ls}");
    assert!(ls.contains("terminal") && ls.contains("(detached)"), "{ls}");
    let json: serde_json::Value = serde_json::from_str(&sb.ok(&["ls", "--json"])).unwrap();
    assert_eq!(json["sessions"][0]["session"], key.as_str());
    assert_eq!(json["sessions"][0]["name"], "demo");

    // --- has-session: alias, key, prefix; exit code only --------------------
    for t in ["demo", key.as_str(), "local:demo", &key[..8]] {
        assert!(sb.rmux(&["has-session", "-t", t]).status.success(), "{t}");
    }
    assert_eq!(sb.rmux(&["has-session", "-t", "nope"]).status.code(), Some(1));

    // --- rename-session moves the alias -------------------------------------
    sb.ok(&["rename-session", "-t", "demo", "webapp"]);
    assert!(sb.rmux(&["has-session", "-t", "webapp"]).status.success());
    assert!(!sb.rmux(&["has-session", "-t", "demo"]).status.success());

    // --- ls with an unreachable server: a line, not a failure ---------------
    std::fs::write(
        sb.home.join(".rmux/servers.json"),
        r#"{"version":1,"servers":[{"id":"x","label":"nowhere","host":"rmux-cli-test.invalid"}]}"#,
    )
    .unwrap();
    let out = sb.rmux(&["ls"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("local:webapp"));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("nowhere: "), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_file(sb.home.join(".rmux/servers.json")).unwrap();

    // --- kill-session ends the process, not just the entry ------------------
    let raw: serde_json::Value = serde_json::from_str(&sb.ok(&["list", "--json"])).unwrap();
    let pid = raw["sessions"][0]["pid"].as_u64();
    assert!(pid.is_some(), "{raw}");
    sb.ok(&["kill-session", "-t", "webapp"]);
    assert!(!sb.rmux(&["has-session", "-t", "webapp"]).status.success());
    if let Some(pid) = pid {
        let alive = Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success());
        assert!(!alive, "the shell {pid} survived kill-session");
    }

    // A bare `attach` with nothing running says so instead of creating.
    let out = sb.rmux(&["attach"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no sessions"));
}
