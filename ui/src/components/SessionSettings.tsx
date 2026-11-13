import { useEffect, useState } from "react";

import {
  api,
  isTauri,
  profileKindFor,
  type JiraProfile,
  type JiraProject,
  type LowLatencyOutcome,
  type MoshInstallOutcome,
  type MoshServerStatus,
} from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { ModelChoice } from "./ModelChoice";
import { ChainStatus } from "./ChainStatus";
import { PermissionChoice } from "./PermissionChoice";
import { PanelLoader } from "./PanelLoader";
import { effectiveLowLatency, useDefaultTransport } from "../lib/transport-default";

/**
 * Settings for one session.
 *
 * Per-session rather than global, because both of these genuinely differ
 * between pieces of work: a personal Claude subscription on your own projects
 * and the org's Console key on the team's, a different Jira project for each
 * piece of work. A single app-wide value would quietly bill the wrong account
 * and show the wrong board.
 *
 * Assigning a Jira project is what makes the Jira tab appear for this session.
 * A tab that is always present but empty for most sessions is a tab everyone
 * learns to ignore.
 *
 * **A tab, not a dialog.** These are settings you come back to and read — which
 * account is this billing, which board is it on — and a modal is the wrong shape
 * for something you consult: it covers the session you are configuring, and it
 * has to be dismissed before you can look at anything else. As a tab it sits
 * beside Claude and the terminal, where the rest of the session's surfaces are.
 */
export function SessionSettings({ sessionId }: { sessionId: string }) {
  const session = useWorkspace((s) => s.sessions.find((x) => x.id === sessionId));
  const host = useWorkspace((s) => s.serverOf(sessionId)?.target.host);
  const configure = useWorkspace((s) => s.configureSession);
  const setModelProfile = useWorkspace((s) => s.setModelProfile);
  const setCodexLaunch = useWorkspace((s) => s.setCodexLaunch);
  const addSession = useWorkspace((s) => s.addSession);
  const projectOfSession = useWorkspace((s) => s.projectOf(sessionId));
  const setFullscreen = useWorkspace((s) => s.setFullscreen);
  const setLowLatency = useWorkspace((s) => s.setLowLatency);
  const clearLowLatency = useWorkspace((s) => s.clearLowLatency);
  const transportDefault = useDefaultTransport();
  const lowLatencyOn = effectiveLowLatency(session, transportDefault);
  const followsDefault = session?.lowLatency === undefined;
  const target = useWorkspace((s) => s.targetOf(sessionId));

  /**
   * **What renders is decided by the session's kind, and a setting that cannot
   * work is absent rather than greyed.**
   *
   * This panel used to be reachable only from the Claude pane, so every section
   * in it was a Claude section by construction. It is now the settings for
   * *any* session — a pi conversation or a plain shell — and the rule that a
   * control which cannot work must be missing rather than disabled decides each
   * one individually:
   *
   * - **TRANSPORT** applies to every kind. It is a property of the *connection*,
   *   not of what runs over it, and `terminal_open`, `claude_start` and
   *   `pi_start` all carry `low_latency`. A plain shell is in fact where it is
   *   felt most, since every keystroke is a round trip.
   * - **MODEL, CLAUDE RENDERING, CLAUDE ACCOUNT** are Claude's. `pi_start`
   *   takes no model profile, no Claude account and no rendering mode (pi picks
   *   its TUI mode on its own launch line), and a shell has no agent at all.
   * - **CONTEXT WINDOW** is the denominator of the rail's context meter, which
   *   is filled from `claude_transcript`'s usage. Nothing else produces one, so
   *   for pi and for a shell the field would configure a meter that never draws.
   * - **JIRA PROJECT** is offered to both agent kinds. It is not only the Claude
   *   pane's Jira tab: the rail's JIRA widget renders for whichever session is
   *   active and is keyed by its id, so a pi conversation pointed at a ticket
   *   gets a working board and its transitions. The tab is Claude-only, so the
   *   copy below says which of the two a given session gets rather than
   *   promising a tab that will not appear. A shell is left out: it is a
   *   terminal rather than a piece of tracked work, and nothing in a shell pane
   *   ever reads it.
   */
  const kind = session?.kind;
  const isClaude = kind === "claude";
  const isCodex = kind === "codex";
  /** Which profiles this session can run under — claude, deepseek (dsh) or
   *  openai (Codex). `undefined` for pi and a shell, which take none. */
  const profileKind = kind ? profileKindFor(kind) : undefined;
  const showJira = kind === "claude" || kind === "pi";

  const [profiles, setProfiles] = useState<JiraProfile[] | null>(null);
  const [profile, setProfile] = useState("");
  const [projects, setProjects] = useState<JiraProject[]>([]);
  const [project, setProject] = useState(session?.jiraProject ?? "");
  const [account, setAccount] = useState(session?.claudeAccount ?? "");
  const [window_, setWindow] = useState(String(session?.contextWindow ?? ""));
  const [error, setError] = useState<string | null>(null);
  /**
   * What this connection would make of a low-latency request.
   *
   * `null` is *not probed yet*, which is neither a yes nor a no — so the
   * section claims neither and shows that it is asking. `undefined` never
   * occurs; the probe either answers or fails.
   */
  const [transport, setTransport] = useState<LowLatencyOutcome | null>(null);
  /** The probe itself failed — a different thing from the host declining. */
  const [transportError, setTransportError] = useState<string | null>(null);
  /**
   * What rmux made of a host that has no `mosh-server`.
   *
   * Three values and they are three different things. `undefined` is *the
   * answer has not arrived*, which is the in-flight install — the probe is what
   * starts one, and the call below stays pending for exactly as long as it
   * runs, so this needs no poller and has none. `null` is *nothing was
   * attempted*: the switch is off, or this refusal was about something an
   * install cannot fix. Anything else is an outcome to report.
   */
  const [install, setInstall] = useState<MoshInstallOutcome | null | undefined>(null);
  /**
   * Which `mosh-server` this host has, and whether rmux thinks it too old.
   *
   * Read from Rust rather than derived here: the minimum is defined once, and a
   * second version comparison in the webview is a second chance to get `1.10`
   * against `1.9` backwards. `null` is *not asked yet*.
   */
  const [server, setServer] = useState<MoshServerStatus | null>(null);
  /** Shown after a verified install, then allowed to fade — a success is not a
   *  state to live with, unlike the two failures beside it. */
  const [installed, setInstalled] = useState<string | null>(null);
  /** Why the connection list could not be read, as the server put it. */
  const [profileError, setProfileError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // Which Jira connections exist is the server's configuration, not ours — so
  // it is asked for rather than assumed, exactly as the sign-in flow does.
  //
  // **The failure is reported, not flattened.** This used to `catch` into an
  // empty list, so "you are not signed in", "the server said no" and "there
  // genuinely are no connections" all rendered as one sentence covering all
  // three — which tells the operator nothing about which one to go and fix.
  //
  // Gated on the section being shown, not only on the section being rendered:
  // asking the Cowork server which Jira connections exist, for a shell that
  // will never display the answer, is exactly the "a widget switched off must
  // not run" failure in miniature.
  useEffect(() => {
    if (!isTauri() || !showJira) return;
    let cancelled = false;
    api
      .jiraProfiles()
      .then((list) => {
        if (cancelled) return;
        setProfiles(list);
        setProfileError(null);
        if (list.length === 1) setProfile(list[0]!.name);
      })
      .catch((e) => {
        if (cancelled) return;
        setProfiles([]);
        setProfileError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [showJira]);

  /**
   * Ask, once, when this tab is opened.
   *
   * **On demand and never a poller.** This component is mounted only while the
   * SETTINGS view is on screen, so a session nobody configured performs no
   * work — the rule that a widget which is switched off must not run. The
   * answer is cached per host in Rust for the whole run, so reopening the tab,
   * or opening it for a second session on the same machine, costs nothing.
   *
   * It is also why the probe was taken out of `connect`: asking every host at
   * startup is a round trip per host on exactly the slow link this feature
   * exists to fix.
   */
  useEffect(() => {
    if (!isTauri()) return;
    let cancelled = false;
    setTransport(null);
    setTransportError(null);
    setInstall(null);
    setServer(null);
    setInstalled(null);
    api
      .lowLatencyProbe(target)
      .then(async (outcome) => {
        if (cancelled) return;
        setTransport(outcome);

        /*
          **Asked, not polled, and asked of the state that already exists.**

          The probe above is what starts an install *or an upgrade*, so by the
          time this runs there is either an attempt to await or there is not.
          `moshInstallPending` says which, immediately — and it is asked first
          because `moshInstallStatus`'s promise stays pending for precisely as
          long as the work takes, which is what makes the in-flight state honest
          without a timer anywhere. Assuming an attempt and taking it back would
          flash "installing…" at every host where nothing was attempted.

          Asked on both outcomes, not only on a refusal: a host that *has* a
          mosh-server can have one too old to be usable, and that is the same
          mechanism reporting through the same call.

          Nothing here reads the refusal's *wording* to decide what applies.
          That sentence is prose written for the operator and is free to change;
          the fact lives in Rust's own record of what it attempted and of what
          version it read, which is the only thing that can be right.
        */
        const report = async (): Promise<MoshInstallOutcome | null> => {
          const [pending, version] = await Promise.all([
            api.moshInstallPending(target).catch(() => false),
            api.moshServerStatus(target).catch(() => null),
          ]);
          if (cancelled) return null;
          setServer(version);
          if (pending) setInstall(undefined);

          const settled = await api.moshInstallStatus(target).catch(() => null);
          if (cancelled) return null;
          setInstall(settled);
          return settled;
        };

        const settled = await report();
        if (cancelled || !settled) return;

        if (settled.kind === "upgraded") {
          // The version on screen is now false, and Rust has already replaced
          // its own cached reading.
          const again = await api.moshServerStatus(target).catch(() => null);
          if (!cancelled) setServer(again);
          return;
        }

        if (settled.kind === "installed") {
          // The refusal on screen is now false. Rust has already updated its
          // cached answer for this host, so asking again replaces "no
          // mosh-server here" with the control itself rather than leaving the
          // operator reading a sentence that stopped being true.
          setInstalled(settled.manager);
          const again = await api.lowLatencyProbe(target).catch(() => null);
          if (cancelled || !again) return;
          setTransport(again);
          // **And the second question, which that re-probe is what asks.** An
          // install can perfectly well land a mosh-server that is already too
          // old — Debian oldstable's archive holds 1.3.2 — and the re-probe
          // above is what starts the upgrade. Without this the operator would
          // be shown a version with nothing saying rmux was already acting on
          // it, and would find out only by reopening the tab.
          await report();
        }
      })
      .catch((e) => {
        if (cancelled) return;
        setTransportError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [JSON.stringify(target)]);

  // A success is allowed to fade; the two failures beside it are not, because
  // they are still true and still want something from the operator.
  useEffect(() => {
    if (!installed) return;
    const timer = setTimeout(() => setInstalled(null), 2500);
    return () => clearTimeout(timer);
  }, [installed]);

  useEffect(() => {
    if (!profile) {
      setProjects([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    api
      .jiraProjects(profile)
      .then((list) => !cancelled && setProjects(list))
      .catch((e) => !cancelled && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [profile]);

  const [saved, setSaved] = useState(false);

  /**
   * Only the fields this kind actually showed are written.
   *
   * `configure` reads an absent key as "leave it alone" and a present-but-empty
   * one as "clear it", so sending every key unconditionally would let a pi
   * session's Save quietly erase a Claude account or context window it never
   * displayed — a setting changed by a control that was not on screen.
   */
  const save = () => {
    configure(sessionId, {
      ...(isClaude
        ? { claudeAccount: account.trim(), contextWindow: Number(window_) || 0 }
        : {}),
      ...(showJira ? { jiraProject: project } : {}),
    });
    // Confirmed in place, next to the control that did it, and allowed to fade —
    // there is no dialog to close, so without this nothing marks that the click
    // did anything.
    setSaved(true);
    setTimeout(() => setSaved(false), 2500);
  };

  const field = {
    border: "1px solid var(--border-strong)",
    color: "var(--text)",
    background: "transparent",
  } as const;

  return (
    <div className="h-full overflow-auto p-5">
      <div className="flex w-full max-w-[520px] flex-col gap-4">
        <header className="flex items-baseline justify-between">
          <span className="kicker">SESSION · {(session?.name ?? "").toUpperCase()}</span>
          <span className="micro">{host ?? "this machine"}</span>
        </header>

        {/* Applied on the next start rather than staged with the rest of this
            form: the provider is read from the environment Claude was spawned
            with, so a running conversation cannot be moved without restarting
            it. The note says so, because a control that appears to do nothing
            is the same "is this broken?" impression as one that does. */}
        {profileKind && (
          <div className="flex flex-col gap-1">
            <ModelChoice
              kind={profileKind}
              value={session?.modelProfile ?? null}
              onChange={(id) => setModelProfile(sessionId, id ?? undefined)}
            />
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              Takes effect the next time this session starts — restart it to switch provider. Only{" "}
              {profileKind === "claude" ? "Claude" : profileKind === "deepseek" ? "DeepSeek" : "OpenAI"}{" "}
              profiles are offered: the server hands a profile's variables only to sessions of its
              own agent. Profiles are managed under Settings › Models.
            </span>
            {profileKind === "claude" && <ChainStatus sessionId={sessionId} target={target} />}
          </div>
        )}

        {/* Codex's launch choices. Applied on the next start, like the provider:
            they are flags on the line Codex was launched with. The bypass follows
            the skip-permissions rule exactly — off unless chosen, here, for this
            session. */}
        {isCodex && session && (
          <div
            className="flex flex-col gap-3"
            style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}
          >
            <div className="flex flex-col gap-[6px]">
              <span className="micro">CODEX SANDBOX</span>
              <div className="seg seg-wrap">
                {(
                  [
                    [null, "CODEX DEFAULT"],
                    ["read-only", "READ-ONLY"],
                    ["workspace-write", "WORKSPACE-WRITE"],
                  ] as const
                ).map(([mode, label]) => (
                  <button
                    key={label}
                    type="button"
                    aria-pressed={(session.codexSandbox ?? null) === mode}
                    disabled={!!session.bypassApprovals}
                    onClick={() => setCodexLaunch(session.id, { codexSandbox: mode })}
                    style={{ padding: "6px 12px" }}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>
            <PermissionChoice
              agent="codex"
              value={!!session.bypassApprovals}
              onChange={(bypass) => setCodexLaunch(session.id, { bypassApprovals: bypass })}
            />
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              Both take effect the next time this session's Codex starts. The bypass replaces the
              sandbox choice — Codex refuses the two together.
            </span>

            {/* Sign-in is Codex's own, in a real terminal: `codex login
                --device-auth` prints a link and a code for any browser, which
                works the same on this machine and on a server. rmux scripts
                none of it. */}
            {projectOfSession && (
              <div className="flex flex-col gap-1">
                <span className="micro">SIGN IN</span>
                <button
                  type="button"
                  className="chip self-start"
                  onClick={() =>
                    addSession(projectOfSession.id, "terminal", {
                      name: "codex login",
                      run: "codex-login",
                    })
                  }
                >
                  SIGN IN TO CODEX ON {(host ?? "this machine").toUpperCase()}
                </button>
                <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
                  Opens a terminal running <code>codex login --device-auth</code>: open the link it
                  prints in your browser and enter the code. Not needed when an OpenAI profile
                  carries an API key.
                </span>
              </div>
            )}
          </div>
        )}

        {/*
          **Moved here out of the Claude tab's header.**

          It restarts the conversation to take effect, and in that header it sat
          a stray click from INTERRUPT — a control with a real cost, at the
          busiest point in the app, reached mid-run. It is also decided once per
          session rather than adjusted while working, which is what this tab is
          for.

          Fullscreen is the default for desktop-launched sessions: Claude's own
          TUI, with its pinned composer, is what most people expect. Inline is
          the deliberate alternative — it keeps xterm selection, Cmd-C and native
          mouse-scroll working, at the cost of the in-TUI scrolling and /focus.
          Both trade-offs are honest and named in the copy below. The explicit
          per-session choice persists and always wins over the default.
        */}
        {isClaude && (
        <div className="flex flex-col gap-1" style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}>
          <span className="micro">CLAUDE RENDERING</span>
          <div className="flex gap-2">
            {([false, true] as const).map((mode) => (
              <button
                key={String(mode)}
                type="button"
                className="chip"
                onClick={() => session && setFullscreen(session.id, mode)}
                style={{
                  flex: "1 1 0",
                  color: !!session?.fullscreen === mode ? "var(--text)" : "var(--text-faint)",
                  background: !!session?.fullscreen === mode ? "var(--hover)" : "transparent",
                  textDecoration: !!session?.fullscreen === mode ? "underline" : "none",
                }}
              >
                {mode ? "FULLSCREEN TUI" : "INLINE"}
              </button>
            ))}
          </div>
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            Fullscreen TUI is the default: Claude's native full-screen interface with its pinned
            composer. It captures the mouse, so selecting text and Cmd-C need Select mode and
            mouse-scroll goes to the TUI rather than xterm's scrollback. Inline gives selection,
            Cmd-C and native scrolling back, trading away the in-TUI scrolling and /focus. Takes
            effect the next time this session's Claude starts.
          </span>
        </div>
        )}

        {/*
          **TRANSPORT — and the absent switch is the design.**

          Where the connection cannot answer a low-latency request, the *reason*
          is shown and there is no control at all. A greyed switch invites "how
          do I enable this"; a missing one asks nothing, and the sentence beside
          it already says what would have to change. The reason is Rust's,
          printed verbatim — re-wording it here would be a second copy to keep
          true.

          Nothing is claimed before the answer arrives. Not probed is neither a
          yes nor a no, so the section says it is asking, and names the machine
          it is asking — over ssh that machine is the reason it is slow.

          Shown only under Tauri: in a browser preview there is no connection to
          measure, and a control with nothing behind it is worse than none.
        */}
        {/* Not for dsh: its console is a log and its UI is a webview, so a
            low-latency transport would change nothing — a dead switch. */}
        {isTauri() && kind !== "dsh" && (
          <div
            className="flex flex-col gap-1"
            style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}
          >
            <span className="micro">TRANSPORT</span>
            {transportError ? (
              <span
                role="alert"
                className="data text-[10.5px]"
                style={{ color: "rgb(var(--primary))" }}
              >
                {transportError}
              </span>
            ) : transport === null ? (
              <PanelLoader
                variant="inline"
                phase={`asking ${host ?? "this machine"} about a low-latency transport…`}
              />
            ) : transport.kind === "declined" ? (
              <div className="flex flex-col gap-1">
                <span
                  className="data text-[10.5px] leading-relaxed"
                  style={{ color: "var(--text-soft)" }}
                >
                  {transport.reason}
                </span>

                {/*
                  **The attempt reports itself, beside the sentence it answers.**

                  A host with no `mosh-server` is the one refusal rmux can act
                  on, so it does — in the background, with this as the only
                  place the operator is told. Every branch below is a *different
                  ask*: a password host is one command they can run, a failure
                  is somewhere to go and look, and an unsupported host is
                  neither. "It did not work" would have said none of that.

                  Red is only on the failure. Needing a password is the host
                  behaving correctly and rmux declining to argue with it, which
                  is not an alarm.
                */}
                {install === undefined ? (
                  <PanelLoader
                    variant="inline"
                    phase={`installing mosh-server on ${host ?? "this machine"}…`}
                  />
                ) : install?.kind === "needsPassword" ? (
                  <div className="flex flex-col gap-1">
                    <span
                      className="data text-[10px] leading-relaxed"
                      style={{ color: "var(--text-soft)" }}
                    >
                      Installing it needs a password on this host, and rmux will never ask one for
                      it — repeated password attempts are how a machine gets locked out. It will
                      not try this host again. Run it yourself:
                    </span>
                    <code
                      className="data inset px-2 py-[5px] text-[10px] leading-relaxed"
                      style={{ color: "var(--text)", userSelect: "text", wordBreak: "break-all" }}
                    >
                      {install.command}
                    </code>
                  </div>
                ) : install?.kind === "failed" ? (
                  <span
                    role="alert"
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "rgb(var(--primary))" }}
                  >
                    rmux tried to install mosh-server here and it did not work: {install.reason}
                  </span>
                ) : install?.kind === "unsupported" ? (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-faint)" }}
                  >
                    rmux cannot install it here — {install.reason}.
                  </span>
                ) : null}
                {/* The stored choice is left alone rather than cleared: this
                    host says no, another one this session is moved to may not,
                    and silently unsetting a preference the operator made is the
                    kind of change nothing on screen would account for. */}
                {lowLatencyOn && (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-faint)" }}
                  >
                    This session still asks for it, and gets ssh here.
                  </span>
                )}
              </div>
            ) : (
              <>
                {/*
                  **The upgrade reports itself here, where the control is.**

                  A host with a `mosh-server` too old for what rmux does with it
                  is not a refusal — low latency works — so it does not belong
                  beside the refusals above. It belongs next to the switch it
                  changes the behaviour of.

                  Every branch is a different ask. `nothingNewer` is the one the
                  *operator* has to act on: nothing rmux can run will move this
                  host, so it says which version they are on and what that costs
                  them. Needing a password is the host behaving correctly and
                  rmux declining to argue with it, which is not an alarm; red is
                  only on the two that are.
                */}
                {install === undefined ? (
                  <PanelLoader
                    variant="inline"
                    phase={`upgrading mosh-server on ${host ?? "this machine"}${
                      server?.version ? ` from ${server.version}` : ""
                    }…`}
                  />
                ) : install?.kind === "upgraded" ? (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-soft)" }}
                  >
                    mosh-server on {host ?? "this machine"} upgraded from {install.from} to{" "}
                    {install.to} with {install.manager}. Takes effect the next time this session
                    starts.
                  </span>
                ) : install?.kind === "nothingNewer" ? (
                  <div className="flex flex-col gap-1">
                    <span
                      role="alert"
                      className="data text-[10px] leading-relaxed"
                      style={{ color: "rgb(var(--primary))" }}
                    >
                      This host has mosh-server {install.version} and {install.manager} has nothing
                      newer to offer. rmux cannot fix this one.
                    </span>
                    <span
                      className="data text-[10px] leading-relaxed"
                      style={{ color: "var(--text-soft)" }}
                    >
                      Older than {server?.minimum ?? "1.4.0"}, so coming back to a fullscreen agent
                      pane over mosh can lose the ability to scroll it, and drag-to-copy there may
                      not reach the clipboard. A backport or a newer repository would fix it; using
                      SSH for this session avoids it.
                    </span>
                  </div>
                ) : install?.kind === "needsPassword" ? (
                  <div className="flex flex-col gap-1">
                    <span
                      className="data text-[10px] leading-relaxed"
                      style={{ color: "var(--text-soft)" }}
                    >
                      This host has mosh-server {server?.version ?? "older than we would like"}, and
                      upgrading it needs a password — which rmux will never ask for, because
                      repeated password attempts are how a machine gets locked out. It will not try
                      this host again. Run it yourself:
                    </span>
                    <code
                      className="data inset px-2 py-[5px] text-[10px] leading-relaxed"
                      style={{ color: "var(--text)", userSelect: "text", wordBreak: "break-all" }}
                    >
                      {install.command}
                    </code>
                  </div>
                ) : install?.kind === "failed" ? (
                  <span
                    role="alert"
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "rgb(var(--primary))" }}
                  >
                    rmux tried to upgrade mosh-server here and it did not work: {install.reason}
                  </span>
                ) : install?.kind === "unsupported" ? (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-faint)" }}
                  >
                    rmux cannot upgrade it here — {install.reason}.
                  </span>
                ) : install?.kind === "alreadyCurrent" ? (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-faint)" }}
                  >
                    mosh-server {install.version} — already current.
                  </span>
                ) : server?.outdated ? (
                  // Reached when rmux is *not* allowed to change this host: the
                  // reading still stands and is still worth saying, because the
                  // limitation is real whether or not anything was attempted.
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-soft)" }}
                  >
                    This host has mosh-server {server.version}, older than {server.minimum}.
                    Automatic upgrades are switched off, so it stays that way — coming back to a
                    fullscreen agent pane over mosh may lose the ability to scroll it.
                  </span>
                ) : server?.version ? (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-faint)" }}
                  >
                    mosh-server {server.version} — already current.
                  </span>
                ) : null}

                {/* Said once, beside the control it just enabled, and then
                    allowed to go — a success is not a state to live with. The
                    next-start caveat is the part worth saying: the pane on
                    screen is still the ssh one it was launched as. */}
                {installed && (
                  <span
                    className="data text-[10px] leading-relaxed"
                    style={{ color: "var(--text-soft)" }}
                  >
                    mosh-server installed on {host ?? "this machine"} with {installed}. It takes
                    effect the next time this session starts.
                  </span>
                )}
                <div className="flex gap-2">
                  {([false, true] as const).map((mode) => (
                    <button
                      key={String(mode)}
                      type="button"
                      className="chip"
                      onClick={() => session && setLowLatency(session.id, mode)}
                      style={{
                        flex: "1 1 0",
                        color: lowLatencyOn === mode ? "var(--text)" : "var(--text-faint)",
                        background:
                          lowLatencyOn === mode ? "var(--hover)" : "transparent",
                        textDecoration: lowLatencyOn === mode ? "underline" : "none",
                      }}
                    >
                      {mode ? "LOW LATENCY" : "SSH"}
                    </button>
                  ))}
                </div>
                <span
                  className="data text-[10px] leading-relaxed"
                  style={{ color: "var(--text-faint)" }}
                >
                  {followsDefault ? (
                    <>
                      Following the app default ({transportDefault === "mosh" ? "low latency" : "ssh"})
                      — change it in Settings › Connections.
                    </>
                  ) : (
                    <>
                      Set for this session.{" "}
                      <button
                        type="button"
                        className="underline"
                        onClick={() => session && clearLowLatency(session.id)}
                      >
                        use the app default
                      </button>
                    </>
                  )}
                </span>
                <span
                  className="data text-[10px] leading-relaxed"
                  style={{ color: "var(--text-soft)" }}
                >
                  Predicts each keystroke locally instead of waiting for the host to echo it —
                  the delay a slow link is actually felt as. The cost is scrollback: this pane
                  would keep a screen, not a history.{" "}
                  {server?.outdated
                    ? "On this host's older mosh-server, drag-to-copy in a fullscreen agent pane " +
                      "may also stop reaching the clipboard, since it travels by OSC 52; Select " +
                      "mode still works. "
                    : ""}
                  Takes effect next time this session starts.
                </span>
              </>
            )}
          </div>
        )}

        {isClaude && (
        <label className="flex flex-col gap-1" style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}>
          <span className="micro">CLAUDE ACCOUNT</span>
          <input
            value={account}
            spellCheck={false}
            placeholder="leave empty to use the default"
            className="data inset px-2 py-[5px] text-[12px] outline-none"
            style={field}
            onChange={(e) => setAccount(e.target.value)}
          />
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            A label for the credential this session runs as. Manage the credentials themselves
            under Settings › Claude.
          </span>
        </label>
        )}

        {isClaude && (
        <label className="flex flex-col gap-1" style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}>
          <span className="micro">CONTEXT WINDOW</span>
          <select
            value={window_}
            className="data inset px-2 py-[5px] text-[12px] outline-none"
            style={field}
            onChange={(e) => setWindow(e.target.value)}
          >
            <option value="">unknown — show tokens only</option>
            <option value="200000">200k</option>
            <option value="1000000">1M</option>
          </select>
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            Usually filled in for you: rmux reads this from Claude's own banner, which prints the
            window beside the model. Set it by hand only if that never appeared — the transcript
            says <span style={{ color: "var(--text)" }}>claude-opus-5</span> whether the window is
            200k or 1M, so without one of the two there is no honest percentage to show.
          </span>
        </label>
        )}

        {showJira && (
        <div className="flex flex-col gap-1" style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}>
          <span className="micro">JIRA PROJECT</span>
          {profiles === null ? (
            <span className="micro">looking for Jira connections…</span>
          ) : profileError ? (
            <div className="flex flex-col gap-1">
              <span role="alert" className="data text-[10.5px]" style={{ color: "rgb(var(--primary))" }}>
                {profileError}
              </span>
              <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
                {/* The two causes worth naming, because they need different
                    actions and the message above is the server's, not ours. */}
                {/sign in/i.test(profileError)
                  ? "Sign in from the footer — the Jira credential lives on the Cowork server and never comes here."
                  : "That came from your Cowork server. rmux only asks it which connections exist."}
              </span>
            </div>
          ) : profiles.length === 0 ? (
            <span className="data text-[10.5px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              You are signed in, and your Cowork server has no Jira connection configured. An
              admin adds one there with <span style={{ color: "var(--text)" }}>PUT /jira/profiles/:name</span>;
              rmux cannot create one, because the credential is the server's to hold.
            </span>
          ) : (
            <>
              {profiles.length > 1 && (
                <select
                  value={profile}
                  className="data inset px-2 py-[5px] text-[12px] outline-none"
                  style={field}
                  onChange={(e) => setProfile(e.target.value)}
                >
                  <option value="">choose a connection…</option>
                  {profiles.map((p) => (
                    <option key={p.name} value={p.name}>
                      {p.name}
                    </option>
                  ))}
                </select>
              )}
              <select
                value={project}
                disabled={!profile || loading}
                className="data inset px-2 py-[5px] text-[12px] outline-none"
                style={field}
                onChange={(e) => setProject(e.target.value)}
              >
                <option value="">{loading ? "loading projects…" : "no project — hide the tab"}</option>
                {projects.map((p) => (
                  <option key={p.key} value={p.key}>
                    {p.key} — {p.name}
                  </option>
                ))}
              </select>
              <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
                {/* Named per kind rather than promised in general: a pi pane has
                    no Jira tab to add, and telling someone a tab will appear
                    that then does not is the same "is this broken?" impression
                    as showing a dead control. */}
                {isClaude
                  ? "Choosing a project adds a Jira tab to this session, and a JIRA widget to the rail while it is the active session."
                  : "Choosing a project adds a JIRA widget to the rail while this is the active session. pi panes have no Jira tab of their own."}
              </span>
            </>
          )}
        </div>
        )}

        {/* Saved fields only. TRANSPORT and the rest write straight to the
            store as they are clicked, so a shell — which has nothing else —
            gets no button that would save nothing. */}
        {(isClaude || showJira) && (
          <div className="flex items-center gap-3">
            <button type="button" className="btn btn-primary" onClick={save}>
              Save
            </button>
            {saved && <span className="micro">SAVED</span>}
          </div>
        )}

        {/* Every section this kind has was gated away — which happens to a shell
            outside the desktop shell, where there is no connection to ask about
            a transport either. An empty box reads as a pane that failed to
            load, so it says which it is. */}
        {!isClaude && !showJira && !isTauri() && (
          <span
            className="data text-[10.5px] leading-relaxed"
            style={{ borderTop: "1px solid var(--border)", paddingTop: 12, color: "var(--text-soft)" }}
          >
            A shell session's only setting is its transport, and that needs the rmux desktop
            shell — there is no connection to measure in a browser preview.
          </span>
        )}

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </div>
    </div>
  );
}
