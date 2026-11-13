//! OpenAI Codex CLI as an rmux session.
//!
//! The Codex twin of `claude_start`/`pi_start`, and it shares their machinery
//! rather than copying it: the session lives in the same [`ClaudeStore`], so the
//! id-keyed commands — `claude_attach`, `claude_write`, `claude_resize`,
//! `claude_stop`, `claude_end_session` — drive a Codex pane unchanged. What is
//! Codex's own is only what Codex genuinely does differently:
//!
//! - **The launch line** ([`rmux_claude::codex::launch_line`]): `codex [resume
//!   <id>]`, the sandbox/bypass choice, and the OpenAI profile's model and base
//!   URL as flags. One definition, shared with `rmux spawn --agent codex`.
//! - **Credentials** reach the host as the `openai` kind
//!   (`rmux-agent setenv --kind openai`, stdin, never argv), so a Codex session
//!   gets its OpenAI key and nothing of Claude's or DeepSeek's.
//! - **Status** is read off Codex's rendered screen ([`codex_state`]), because
//!   Codex publishes no status of its own that rmux could use — see the module
//!   note on [`rmux_claude::codex`] for why its hooks are not that.
//!
//! Like pi, Codex runs **only under the agent**: there is no non-agent launch
//! path, so a host without one (Windows) refuses with a reason.

use std::sync::Arc;

use rmux_claude::codex::{self, Launch, Sandbox};
use rmux_claude::profile::ProfileKind;
use rmux_claude::{ClaudeSession, ClaudeState, Prompt};
use rmux_term::TermSize;
use serde::{Deserialize, Serialize};

use crate::claude::{ClaudeStore, StartedSession};
use crate::ctx::Ctx;
use crate::host::Sink;
use crate::registry::{Call, Registry, Web};
use crate::terminal::TargetRef;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Launch Codex under the agent and stream its screen to the UI.
#[allow(clippy::too_many_arguments)]
pub async fn codex_start(
    ctx: &Ctx,
    store: &ClaudeStore,
    target: TargetRef,
    cwd: Option<String>,
    // A recorded conversation to continue (`codex resume <id>`).
    resume: Option<String>,
    // The daemon session name, `codex-<id>`. The prefix is what scopes the
    // `openai` credentials to this session on the host.
    session_name: Option<String>,
    // `--sandbox read-only|workspace-write`. Absent is Codex's own default.
    sandbox: Option<String>,
    // `--dangerously-bypass-approvals-and-sandbox`: a per-launch judgement kept
    // with the session, never a stored default — the skip-permissions rule.
    bypass_approvals: Option<bool>,
    // The OpenAI profile to run under. `None` is the host's own Codex login —
    // and is still *applied*, to clear whatever a previous profile left.
    model_profile: Option<String>,
    low_latency: Option<bool>,
    cols: u16,
    rows: u16,
    output: Arc<dyn Sink>,
) -> Result<StartedSession, String> {
    let resolved = crate::claude::resolve(store, &target).await?;
    let size = TermSize { cols, rows };

    let agent_available = resolved.platform() != Some(rmux_transport::Platform::Windows);
    let Some(name) = session_name.as_ref().filter(|_| agent_available) else {
        return Err(
            "Codex runs under the rmux agent, which is not available on this host yet — start \
             Codex on a Linux or macOS host, or open a plain terminal there instead"
                .to_owned(),
        );
    };
    // The prefix is load-bearing on the host: it is what selects the `openai`
    // credential set. A name without it would start Codex with no profile.
    if !name.starts_with("codex-") {
        return Err(format!("a Codex session name must start with `codex-`, not {name:?}"));
    }

    let sandbox = match sandbox.as_deref().filter(|s| !s.is_empty()) {
        None => None,
        Some(raw) => Some(
            Sandbox::parse(raw).ok_or_else(|| format!("unknown Codex sandbox mode {raw:?}"))?,
        ),
    };

    // Credentials first, scoped to `openai`, and before the attach: the daemon
    // applies the set when it *creates* the session.
    let applied = crate::model_profile::apply_to_target(
        ctx,
        resolved.as_ref(),
        ProfileKind::Openai,
        model_profile.as_deref(),
    )
    .await?;
    let profile = applied.profile.clone();

    let installed = crate::agent::ensure_agent(ctx, resolved.as_ref()).await?;

    let launch = Launch {
        resume,
        sandbox,
        bypass: bypass_approvals.unwrap_or(false),
        model: profile.as_ref().and_then(|p| p.value("OPENAI_MODEL")).map(str::to_owned),
        base_url: profile.as_ref().and_then(|p| p.value("OPENAI_BASE_URL")).map(str::to_owned),
    };
    let line = codex::launch_line(&launch, None);

    // The line stands alone as `--login-command`; the agent spawns it through
    // `CommandSpec::login_shell()` (`-l -i`), because `codex` is installed by
    // npm under a version manager whose PATH exists only in a login shell.
    let mut spec = installed.attach_spec(name, cwd.as_deref(), cols, rows);
    spec = spec.arg("--login-command").arg(line).args(applied.attach_args.clone());
    spec = crate::claude::ask_for_low_latency(resolved.as_ref(), spec, low_latency).await;

    let session = Arc::new(
        ClaudeSession::start_with_spec(resolved.as_ref(), spec, cwd.as_deref(), size).map_err(err)?,
    );
    let id = crate::claude::insert_session(store, Arc::clone(&session));
    crate::claude::stream_claude(session, output);
    Ok(StartedSession { id })
}

/// Codex's state, in the shape the Claude pane already polls.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolledCodexState {
    #[serde(flatten)]
    pub state: ClaudeState,
    /// The raw reading, for anything that wants more than working/waiting.
    pub codex: codex::State,
    /// Set once the attach process has died — see `claude::PolledState::exited`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exited: Option<i32>,
}

/// What Codex is showing — read with Codex's rules, reported in Claude's shape.
///
/// `prompt` is set when Codex is blocked on the operator (an approval, the
/// folder-trust question), so the rail's "needs you" mark works unchanged; it
/// carries no choices, because rmux draws no controls over another program's
/// dialog (the decision card is gone for exactly that reason).
pub fn codex_state(store: &ClaudeStore, id: &str) -> Result<PolledCodexState, String> {
    let session = crate::claude::session_by_id(store, id)?;
    let reading = codex::detect(&session.screen_lines());
    Ok(PolledCodexState {
        state: state_of(reading),
        codex: reading,
        exited: session.terminal().exit_code(),
    })
}

fn state_of(reading: codex::State) -> ClaudeState {
    match reading {
        codex::State::Blocked => ClaudeState {
            prompt: Some(Prompt {
                question: "Codex is waiting for an answer".to_owned(),
                choices: Vec::new(),
                fingerprint: "codex-blocked".to_owned(),
            }),
            working: false,
        },
        codex::State::Working => ClaudeState { prompt: None, working: true },
        codex::State::Idle | codex::State::Unknown => ClaudeState::default(),
    }
}

/// Every recorded Codex conversation on a host, newest first — the resume list.
pub async fn codex_list_all_sessions(
    store: &ClaudeStore,
    target: TargetRef,
) -> Result<Vec<codex::Conversation>, String> {
    let resolved = crate::claude::resolve(store, &target).await?;
    ClaudeSession::codex_list_all(resolved.as_ref()).await.map_err(err)
}

/// Read a Codex conversation back — the tail only, parsed tolerantly.
pub async fn codex_transcript(
    store: &ClaudeStore,
    target: TargetRef,
    cwd: String,
    session: Option<String>,
    tail_bytes: Option<u64>,
) -> Result<rmux_claude::transcript::Transcript, String> {
    let resolved = crate::claude::resolve(store, &target).await?;
    let tail = tail_bytes.unwrap_or(rmux_claude::transcript::DEFAULT_TAIL_BYTES);
    let spec = rmux_transport::CommandSpec::new("sh")
        .arg("-c")
        .arg(codex::transcript_script(&cwd, session.as_deref(), tail))
        .tty(rmux_transport::Tty::None);
    let out = resolved.exec(&spec).await.map_err(err)?;
    Ok(codex::parse(out.stdout.as_bytes()))
}

// ── Registration ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexStartArgs {
    pub target: TargetRef,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub bypass_approvals: Option<bool>,
    #[serde(default)]
    pub model_profile: Option<String>,
    #[serde(default)]
    pub low_latency: Option<bool>,
    pub cols: u16,
    pub rows: u16,
    pub output: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdArgs {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetArgs {
    pub target: TargetRef,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptArgs {
    pub target: TargetRef,
    pub cwd: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub tail_bytes: Option<u64>,
}

fn store_of(call: &Call) -> Arc<ClaudeStore> {
    call.ctx().store::<ClaudeStore>()
}

pub fn register(r: &mut Registry) {
    r.add("codex_start", Web::Yes, |call: Call, a: CodexStartArgs| async move {
        let output = call.sink(&a.output).map_err(err)?;
        codex_start(
            call.ctx(),
            &store_of(&call),
            a.target,
            a.cwd,
            a.resume,
            a.session_name,
            a.sandbox,
            a.bypass_approvals,
            a.model_profile,
            a.low_latency,
            a.cols,
            a.rows,
            output,
        )
        .await
    });
    r.add("codex_state", Web::Yes, |call: Call, a: IdArgs| async move {
        codex_state(&store_of(&call), &a.id)
    });
    r.add("codex_list_all_sessions", Web::Yes, |call: Call, a: TargetArgs| async move {
        codex_list_all_sessions(&store_of(&call), a.target).await
    });
    r.add("codex_transcript", Web::Yes, |call: Call, a: TranscriptArgs| async move {
        codex_transcript(&store_of(&call), a.target, a.cwd, a.session, a.tail_bytes).await
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_is_reported_as_needing_the_operator_without_inventing_choices() {
        let s = state_of(codex::State::Blocked);
        assert!(s.prompt.as_ref().is_some_and(|p| p.choices.is_empty()));
        assert!(!s.working);
        assert!(state_of(codex::State::Working).working);
        // Unknown never reads as working or waiting.
        assert_eq!(state_of(codex::State::Unknown), ClaudeState::default());
    }
}
