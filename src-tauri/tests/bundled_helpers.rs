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
