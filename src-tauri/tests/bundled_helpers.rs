//! Guards the two binaries the app cannot do its job without.
//!
//! Both of these have already shipped missing, and neither failure looks like a
//! packaging failure from the outside:
//!
//! - **`rmux-askpass`.** `askpass::helper_path` looks for it beside the main
//!   executable — true under `cargo run`, and true of no bundle until this was
//!   added. Without it `env_for_gui_prompts` tells `ssh` not to wait for a
//!   terminal, so a password or 2FA host answers `Permission denied
//!   (publickey,password)` **with no prompt at all**. That reads as wrong
//!   credentials, and it was diagnosed as wrong credentials, twice.
//! - **The host `rmux-agent`.** Shipped inside `agents/`, which is also how a
//!   local session finds it (`provision::default_source`). Drop it and sessions
//!   on this very machine lose persistence.
//!
//! These assert the *configuration*, which is the part that has actually gone
//! wrong. Whether the file exists on disk is deliberately not asserted: it is a
//! build output, absent on a clean checkout, and a test that fails before you
//! have built anything is a test people learn to ignore.

use serde_json::Value;

/// Every staged agent must be built from the *current* version.
///
/// The agents are produced by `scripts/build-agents.sh`, which is a separate
/// step nothing runs for you — so a version bump leaves last week's binaries
/// sitting in `agents/` looking perfectly fine. `provision::ensure` compares the
/// uploaded agent's own `version` against the client's `CARGO_PKG_VERSION` and
/// refuses on a mismatch, so the result is that **every remote session fails**
/// with "the uploaded agent reported 0.1.6, expected 0.2.7" — a message that
/// reads like a corrupted upload rather than a build step nobody ran.
///
/// That shipped. The reasoning that produced it was "the merge changed no agent
/// code, so the agents do not need rebuilding", which is true of the code and
/// false of the *version* — it is compiled in through `env!`.
///
/// The check is byte-presence rather than executing them: three of the four are
/// cross-compiled for Linux and cannot run on the machine doing the building,
/// and a guard that only covers the one native binary would miss exactly the
/// ones that go to hosts. A binary built at 0.1.6 does not contain "0.2.7".
///
/// Absence is not a failure, per this file's convention: these are build
/// outputs, and a test that fails on a clean checkout is one people learn to
/// ignore. It fails only when a *stale* binary is present, which is the case
/// that reaches users.
#[test]
fn every_staged_agent_carries_the_current_version() {
    let version = env!("CARGO_PKG_VERSION");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agents");

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return; // never built here
    };

    let mut checked = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("rmux-agent") {
            continue; // PLACEHOLDER.txt and anything else
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };

        let needle = version.as_bytes();
        let present = bytes.windows(needle.len()).any(|w| w == needle);
        assert!(
            present,
            "{name} does not contain {version:?}, so it was built from an older \
             version and every host will refuse it. Re-run scripts/build-agents.sh \
             (it needs zig + cargo-zigbuild) and rebuild the bundle."
        );
        checked += 1;
    }

    // A directory holding only `PLACEHOLDER.txt` would otherwise pass silently
    // while shipping no agent at all.
    if dir.join("rmux-agent.exe").exists() || dir.join("rmux-agent").exists() {
        assert!(checked > 0, "agents/ has a host agent but nothing was checked");
    }
}

fn conf() -> Value {
    let raw = include_str!("../tauri.conf.json");
    serde_json::from_str(raw).expect("tauri.conf.json must be valid JSON")
}

#[test]
fn the_askpass_helper_is_shipped_as_a_sidecar() {
    let conf = conf();
    let external = conf["bundle"]["externalBin"]
        .as_array()
        .expect("bundle.externalBin must exist — without it no helper reaches the bundle");

    assert!(
        external.iter().any(|v| v.as_str() == Some("binaries/rmux-askpass")),
        "rmux-askpass must be an externalBin. A `resources` entry is not enough: \
         resources land in Contents/Resources, and helper_path looks beside the \
         executable in Contents/MacOS. Got {external:?}"
    );
}

#[test]
fn the_agents_directory_is_shipped_as_a_resource() {
    let conf = conf();
    let resources = conf["bundle"]["resources"].as_array().expect("bundle.resources must exist");

    assert!(
        resources.iter().any(|v| v.as_str() == Some("agents/*")),
        "the whole agents directory ships: the cross-compiled ones are uploaded to \
         hosts, and the bare `rmux-agent` beside them is what a session on *this* \
         machine runs. Got {resources:?}"
    );
}

/// The staging step must be wired into the build, not left to memory.
///
/// This project's one recurring failure is a fix that is in the source and not
/// in the artefact, and it is always silent. `beforeBuildCommand` runs `pnpm
/// build`, so the helper cannot be omitted from a bundle by forgetting a step.
#[test]
fn building_the_ui_also_stages_the_helper() {
    let package: Value =
        serde_json::from_str(include_str!("../../package.json")).expect("package.json is JSON");
    let build = package["scripts"]["build"].as_str().unwrap_or_default();

    assert!(
        build.contains("build-askpass"),
        "`pnpm build` must stage the askpass sidecar — it is what beforeBuildCommand \
         runs, and it is the only thing standing between a release and a silently \
         missing helper. Got {build:?}"
    );

    let conf = conf();
    assert_eq!(
        conf["build"]["beforeBuildCommand"].as_str(),
        Some("pnpm build"),
        "beforeBuildCommand must remain the script that stages the helper"
    );
}
