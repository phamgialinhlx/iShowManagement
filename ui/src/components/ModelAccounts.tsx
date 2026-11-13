import { useEffect, useState } from "react";

import { api, type ChainsView, type ModelProfile, type ProfileChain, type ProfileKind } from "../lib/api";
import { invalidateProfileNames } from "../lib/fallback-watch";

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/**
 * Add a **Claude account** profile from a subscription token — pasted, or got
 * by running `claude setup-token` right here.
 *
 * The token is checked by prefix in Rust: an admin key is refused (it must
 * never reach a server), and so is an API key pasted into the token field (it
 * belongs in an ordinary profile). "Get a token" never shows the token: the CLI
 * prints it into Rust, which stores it in the keychain and hands back only the
 * redacted profile.
 */
export function TokenAccountEditor({
  onSaved,
  onCancel,
}: {
  onSaved: (profiles: ModelProfile[]) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [mode, setMode] = useState<"paste" | "login">("paste");
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // A sign-in left running when the editor closes would hold a pty for nothing.
  useEffect(() => () => void api.claudeLoginCancel().catch(() => {}), []);

  const saved = (profiles: ModelProfile[]) => {
    invalidateProfileNames();
    onSaved(profiles);
  };

  const savePaste = async () => {
    setBusy("saving…");
    setError(null);
    try {
      saved(await api.modelProfileSaveToken(null, name, token));
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  };

  const startLogin = async () => {
    setBusy("starting claude setup-token…");
    setError(null);
    try {
      const started = await api.claudeLoginStart();
      setAuthUrl(started.authUrl);
      await api.openExternal(started.authUrl).catch(() => {});
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  };

  const submitCode = async () => {
    setBusy("waiting for the token…");
    setError(null);
    try {
      saved(await api.claudeLoginSubmitProfile(code, name));
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(null);
    }
  };

  const named = name.trim().length > 0;
  return (
    <div className="inset flex flex-col gap-3 p-4">
      <span className="micro">ADD A CLAUDE ACCOUNT (SUBSCRIPTION TOKEN)</span>
      <p className="data text-[11px] leading-[1.5]" style={{ color: "var(--text-soft)" }}>
        A long-lived token from <code>claude setup-token</code> runs sessions on that Pro/Max subscription. The
        session gets <code>CLAUDE_CODE_OAUTH_TOKEN</code> and nothing else — any API key, auth token or base
        URL the server would otherwise give it is removed, so it really uses this subscription.
      </p>
      <label className="flex flex-col gap-1">
        <span className="micro">NAME</span>
        <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="Work Max" />
      </label>

      <div className="seg">
        <button type="button" aria-pressed={mode === "paste"} onClick={() => setMode("paste")}>
          PASTE A TOKEN
        </button>
        <button type="button" aria-pressed={mode === "login"} onClick={() => setMode("login")}>
          GET A TOKEN
        </button>
      </div>

      {mode === "paste" ? (
        <>
          <input
            className="field"
            type="password"
            autoComplete="off"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder="sk-ant-oat01-…"
          />
          <div className="flex gap-2">
            <button type="button" className="btn btn-primary" disabled={!named || !token.trim() || busy !== null} onClick={savePaste}>
              {busy ?? "Save account"}
            </button>
            <button type="button" className="btn" onClick={onCancel}>
              Cancel
            </button>
          </div>
        </>
      ) : authUrl === null ? (
        <div className="flex gap-2">
          <button type="button" className="btn btn-primary" disabled={!named || busy !== null} onClick={startLogin}>
            {busy ?? "Sign in in the browser"}
          </button>
          <button type="button" className="btn" onClick={onCancel}>
            Cancel
          </button>
        </div>
      ) : (
        <>
          <span className="data text-[11px]" style={{ color: "var(--text-soft)" }}>
            Sign in in the browser, then paste the code it shows.{" "}
            <button type="button" className="chip" onClick={() => void api.openExternal(authUrl)}>
              open the link again
            </button>
          </span>
          <input className="field" value={code} onChange={(e) => setCode(e.target.value)} placeholder="code from the browser" />
          <div className="flex gap-2">
            <button type="button" className="btn btn-primary" disabled={!code.trim() || busy !== null} onClick={submitCode}>
              {busy ?? "Save account"}
            </button>
            <button type="button" className="btn" onClick={onCancel}>
              Cancel
            </button>
          </div>
        </>
      )}
      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}
    </div>
  );
}

/**
 * Fallback chains and each kind's default profile.
 *
 * A chain orders existing **Claude** profiles; a session on it starts on the
 * first and the host moves it forward on a usage limit. A default is what a
 * child session of a *different* agent binds to when a profiled session spawns
 * it (a Claude session starting Codex) — without one that spawn is refused,
 * because quietly using the server's own login is a provider nobody chose.
 */
export function ChainsAndDefaults({ profiles }: { profiles: ModelProfile[] }) {
  const [view, setView] = useState<ChainsView | null>(null);
  const [editing, setEditing] = useState<ProfileChain | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    api.modelChains().then(setView).catch((e) => setError(errText(e)));
  }, [profiles]);

  const run = async (label: string, op: () => Promise<ChainsView>) => {
    setBusy(label);
    setError(null);
    try {
      setView(await op());
      invalidateProfileNames();
      return true;
    } catch (e) {
      setError(errText(e));
      return false;
    } finally {
      setBusy(null);
    }
  };

  const claude = profiles.filter((p) => (p.kind ?? "claude") === "claude");
  const nameOf = (id: string) => profiles.find((p) => p.id === id)?.name ?? `#${id}`;
  if (view === null) {
    return error ? (
      <p className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>{error}</p>
    ) : (
      <span className="micro">reading chains…</span>
    );
  }

  return (
    <div className="flex flex-col gap-5">
      <section className="flex flex-col gap-2">
        <h3 className="kicker">FALLBACK CHAINS</h3>
        <p className="data text-[11px] leading-[1.5]" style={{ color: "var(--text-soft)" }}>
          When the account a session runs on hits a usage limit, the server moves it to the next account in
          its chain, resumes the conversation and tells you in the pane. It never cycles back on its own.
        </p>
        {view.chains.map((c) => (
          <div key={c.id} className="inset flex items-baseline gap-3 p-3">
            <span className="data text-[12px]" style={{ color: "var(--text)" }}>{c.name}</span>
            <span className="data min-w-0 flex-1 truncate text-[11px]" style={{ color: "var(--text-soft)" }}>
              {c.members.map(nameOf).join(" → ")}
              {c.switchOnOverloaded ? " · also on overload" : ""}
            </span>
            <button type="button" className="chip" onClick={() => setEditing(c)}>edit</button>
            <button
              type="button"
              className="chip"
              disabled={busy !== null}
              onClick={() => void run("deleting…", () => api.modelChainDelete(c.id))}
            >
              delete
            </button>
          </div>
        ))}
        {editing ? (
          <ChainEditor
            initial={editing}
            claude={claude}
            busy={busy}
            onSave={async (c) => {
              if (await run("saving…", () => api.modelChainSave(c.id || null, c.name, c.members, c.switchOnOverloaded))) {
                setEditing(null);
              }
            }}
            onCancel={() => setEditing(null)}
          />
        ) : claude.length >= 2 ? (
          <button
            type="button"
            className="btn self-start"
            onClick={() => setEditing({ id: "", name: "", kind: "claude", members: [], switchOnOverloaded: false })}
          >
            New chain
          </button>
        ) : (
          <span className="micro">A CHAIN NEEDS AT LEAST TWO CLAUDE PROFILES</span>
        )}
      </section>

      <section className="flex flex-col gap-2">
        <h3 className="kicker">DEFAULT PER AGENT</h3>
        <p className="data text-[11px] leading-[1.5]" style={{ color: "var(--text-soft)" }}>
          What a session started by another agent runs on — a Claude session spawning Codex, say. A child of
          the same agent always inherits its parent's profile instead.
        </p>
        {(["claude", "deepseek", "openai"] as ProfileKind[]).map((kind) => {
          const mine = profiles.filter((p) => (p.kind ?? "claude") === kind);
          if (mine.length === 0) return null;
          const current = view.defaults[kind] ?? null;
          return (
            <div key={kind} className="flex flex-col gap-1">
              <span className="micro">{kind.toUpperCase()}</span>
              <div className="seg seg-wrap">
                <button
                  type="button"
                  aria-pressed={current === null}
                  disabled={busy !== null}
                  onClick={() => void run("saving…", () => api.modelProfileDefaultSet(kind, null))}
                >
                  NONE — REFUSE
                </button>
                {mine.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    aria-pressed={current === p.id}
                    disabled={busy !== null}
                    onClick={() => void run("saving…", () => api.modelProfileDefaultSet(kind, p.id))}
                  >
                    {p.name.toUpperCase()}
                  </button>
                ))}
              </div>
            </div>
          );
        })}
      </section>
      {busy && <span className="micro">{busy}</span>}
      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}
    </div>
  );
}

function ChainEditor({
  initial,
  claude,
  busy,
  onSave,
  onCancel,
}: {
  initial: ProfileChain;
  claude: ModelProfile[];
  busy: string | null;
  onSave: (c: ProfileChain) => void;
  onCancel: () => void;
}) {
  const [chain, setChain] = useState(initial);
  const members = chain.members;
  const add = (id: string) => setChain({ ...chain, members: [...members, id] });
  const remove = (i: number) => setChain({ ...chain, members: members.filter((_, j) => j !== i) });
  const up = (i: number) => {
    if (i === 0) return;
    const next = [...members];
    [next[i - 1], next[i]] = [next[i]!, next[i - 1]!];
    setChain({ ...chain, members: next });
  };
  const nameOf = (id: string) => claude.find((p) => p.id === id)?.name ?? `#${id}`;
  const available = claude.filter((p) => !members.includes(p.id));

  return (
    <div className="inset flex flex-col gap-3 p-4">
      <label className="flex flex-col gap-1">
        <span className="micro">NAME</span>
        <input className="field" value={chain.name} onChange={(e) => setChain({ ...chain, name: e.target.value })} placeholder="Max accounts" />
      </label>
      <span className="micro">ORDER — FIRST IS WHERE A SESSION STARTS</span>
      {/* A list of unknown length gets its own ceiling (CLAUDE.md). */}
      <ol className="flex max-h-[220px] flex-col gap-1 overflow-y-auto">
        {members.map((id, i) => (
          <li key={id} className="data flex items-center gap-2 text-[11px]">
            <span style={{ color: "var(--text-faint)" }}>{i + 1}.</span>
            <span className="flex-1" style={{ color: "var(--text)" }}>{nameOf(id)}</span>
            <button type="button" className="chip" disabled={i === 0} onClick={() => up(i)}>up</button>
            <button type="button" className="chip" onClick={() => remove(i)}>remove</button>
          </li>
        ))}
      </ol>
      {available.length > 0 && (
        <div className="seg seg-wrap">
          {available.map((p) => (
            <button key={p.id} type="button" onClick={() => add(p.id)}>
              + {p.name.toUpperCase()}
            </button>
          ))}
        </div>
      )}
      <label className="data flex items-center gap-2 text-[11px]">
        <input
          type="checkbox"
          checked={chain.switchOnOverloaded}
          onChange={(e) => setChain({ ...chain, switchOnOverloaded: e.target.checked })}
        />
        Also switch when the provider is overloaded (529). Off by default: overload is not the account.
      </label>
      <div className="flex gap-2">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy !== null || !chain.name.trim() || members.length < 2}
          onClick={() => onSave(chain)}
        >
          {busy ?? "Save chain"}
        </button>
        <button type="button" className="btn" onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}
