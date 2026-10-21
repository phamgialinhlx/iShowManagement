//! IPC commands.
//!
//! Errors cross the IPC boundary as strings because `anyhow::Error` is not
//! serialisable. Each handler converts at the edge so the crates underneath can
//! keep using `anyhow` internally.
//!
//! Moved from `src-tauri/src/commands.rs` unchanged.

use rmux_transport::{CommandSpec, LocalTarget, Target, Tty};
use serde::Deserialize;

use crate::registry::{NoArgs, Registry, Web};
use crate::target::{TargetInfo, describe};

/// The one spec builder command handlers share.
pub fn spec(program: &str, args: &[String]) -> CommandSpec {
    CommandSpec::new(program).args(args.to_vec())
}

/// Describe the local machine.
pub async fn local_target() -> TargetInfo {
    describe(&LocalTarget::new())
}

/// Run a one-shot command on a target and return its stdout.
///
/// Interactive work does not come through here — terminals get a PTY, which is a
/// separate channel. This is for the short probes the UI needs (`git branch`,
/// `uname`, and so on).
pub async fn run_on_target(program: String, args: Vec<String>) -> Result<String, String> {
    let target = LocalTarget::new();
    let output = target
        .exec(&spec(&program, &args).tty(Tty::None))
        .await
        .map_err(|e| e.to_string())?;

    output.stdout_or_err().map(str::to_owned).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOnTargetArgs {
    pub program: String,
    pub args: Vec<String>,
}

pub fn register(r: &mut Registry) {
    r.add("local_target", Web::Yes, |_, _: NoArgs| async { Ok::<_, String>(local_target().await) });
    // **Not on the web.** It runs an arbitrary program on the machine rmux runs
    // on — which, for rmux-web, is the dev host itself. Over HTTP that is an
    // `exec` endpoint, the one shape `docs/rmux-web.md` and the Redstone bridge
    // both exist to refuse. Desktop keeps it as it was.
    r.add("run_on_target", Web::No, |_, a: RunOnTargetArgs| run_on_target(a.program, a.args));
}
