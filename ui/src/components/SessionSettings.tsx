import { useEffect, useState } from "react";

import { api, isTauri, type JiraProfile, type JiraProject } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { ModelChoice } from "./ModelChoice";

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
  const setFullscreen = useWorkspace((s) => s.setFullscreen);

  const [profiles, setProfiles] = useState<JiraProfile[] | null>(null);
  const [profile, setProfile] = useState("");
  const [projects, setProjects] = useState<JiraProject[]>([]);
  const [project, setProject] = useState(session?.jiraProject ?? "");
  const [account, setAccount] = useState(session?.claudeAccount ?? "");
  const [window_, setWindow] = useState(String(session?.contextWindow ?? ""));
  const [error, setError] = useState<string | null>(null);
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
  useEffect(() => {
    if (!isTauri()) return;
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
  }, []);

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

  const save = () => {
    configure(sessionId, {
      claudeAccount: account.trim(),
      jiraProject: project,
      contextWindow: Number(window_) || 0,
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
        <div className="flex flex-col gap-1">
          <ModelChoice
            value={session?.modelProfile ?? null}
            onChange={(id) => setModelProfile(sessionId, id ?? undefined)}
          />
          <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
            Takes effect the next time this session's Claude starts — restart the Claude tab to
            switch provider. Profiles are managed under Settings › Models.
          </span>
        </div>

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
                Choosing a project adds a Jira tab to this session.
              </span>
            </>
          )}
        </div>

        <div className="flex items-center gap-3">
          <button type="button" className="btn btn-primary" onClick={save}>
            Save
          </button>
          {saved && <span className="micro">SAVED</span>}
        </div>

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </div>
    </div>
  );
}
