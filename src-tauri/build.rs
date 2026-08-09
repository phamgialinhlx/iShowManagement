use std::path::Path;

/// Refuse to build a bundle whose embedded agents are from another version.
///
/// ## The failure this exists to make impossible
///
/// The agent is cross-compiled by `scripts/build-agents.sh` into
/// `src-tauri/agents/`, embedded as a Tauri **resource**, uploaded to a host on
/// first use, and then checked: the app runs `--version` on what it uploaded and
/// refuses a mismatch. So bumping the workspace version without rerunning that
/// script ships an app that **cannot open a terminal on any host at all**, with
/// the only symptom being
///
/// ```text
/// the uploaded agent reported "0.2.13", expected 0.2.14
/// ```
///
/// which names the versions and not the cause. That happened on a signed,
/// notarised build that was installed before anyone saw it, and it is the second
/// time this class of mistake has cost real time — `CLAUDE.md` already carried
/// the rule in prose ("a fix that lives in the agent is not deployed until a new
/// daemon runs it"), and prose was not enough, because the person who forgets to
/// rebuild the agents is exactly the person not re-reading the paragraph about
/// rebuilding the agents.
///
/// A rule that is only written down is a rule that holds until someone is in a
/// hurry. This one now fails the build in under a second, before the three
/// minutes of release compile and the two notarisation round trips.
///
/// ## Why absent agents are only a warning
///
/// `src-tauri/agents/` is git-ignored, so a fresh clone legitimately has none —
/// and rmux handles that at runtime by falling back to a direct login shell with
/// a stated reason. Absent means "no persistence"; **stale means silently
/// wrong**, and only the second deserves to stop a build.
fn main() {
    check_agents();
    tauri_build::build();
}

fn check_agents() {
    let dir = Path::new("agents");
    println!("cargo:rerun-if-changed=agents");
    println!("cargo:rerun-if-changed=agents/VERSION");

    let binaries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("rmux-agent"))
        .collect();

    if binaries.is_empty() {
        println!(
            "cargo:warning=no agents in src-tauri/agents — remote terminals will not be \
             persistent. Run scripts/build-agents.sh to include them."
        );
        return;
    }

    let want = env!("CARGO_PKG_VERSION");
    let stamp = std::fs::read_to_string(dir.join("VERSION"));
    let got = stamp.as_deref().map(str::trim).unwrap_or("");

    if got == want {
        return;
    }

    // `panic!` rather than a warning: a warning scrolls past in a build that
    // otherwise looks perfect, and the artefact it produces is broken on every
    // host. The whole point is that this cannot be walked past.
    panic!(
        "\n\n\
         The embedded agents are stale.\n\n\
         \x20 this app:    {want}\n\
         \x20 agents/:     {}\n\n\
         The agent is uploaded to every host and its version is checked there, so\n\
         shipping this would fail with 'the uploaded agent reported ...' and no\n\
         host would get a terminal.\n\n\
         Fix it with:\n\n\
         \x20 ./scripts/build-agents.sh\n\n",
        if got.is_empty() { "unknown (no VERSION stamp — rebuild them)" } else { got },
    );
}
