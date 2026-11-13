import { useEffect, useState } from "react";

import { api, type IdentityCheck, type TargetRef } from "../lib/api";
import { keyFileNotes, type KeyChoice } from "../lib/connect-form";

/**
 * The parts of "how rmux reaches a server" shared by Connect a server and a
 * server's Connection settings: the key choice and TEST CONNECTION.
 *
 * ## The key choice is segmented, not a `<select>`
 *
 * Three options, and the current one has to be readable without a click
 * (CLAUDE.md, "State must be readable without clicking"). DEFAULT is first and
 * selected by default — it is today's behaviour, ssh's own config and agent,
 * with rmux's own key still offered when one exists.
 *
 * ## A chosen key is *added*, never exclusive
 *
 * The file is offered as one more `-i`. ssh still tries the operator's agent,
 * `~/.ssh/config`'s `IdentityFile` and rmux's generated key — choosing a key for
 * one server must not be able to break authentication anywhere else.
 */

const KEY_LABELS: Record<KeyChoice, string> = {
  default: "DEFAULT",
  file: "KEY FILE…",
  generate: "GENERATE AN RMUX KEY",
};

const KEY_HINTS: Record<KeyChoice, string> = {
  default: "ssh's own config and agent; rmux's key for this host is offered too if it has one",
  file: "a private key on this machine, offered in addition to the defaults",
  generate: "after the first connection (password if asked), rmux makes a key for this host and installs it",
};

let sshDir: Promise<string | undefined> | null = null;
/** `~/.ssh` on *this* machine — where the picker opens. */
function localSshDir(): Promise<string | undefined> {
  sshDir ??= api
    .fsHome({})
    .then((home) => `${home.replace(/\/$/, "")}/.ssh`)
    .catch(() => undefined);
  return sshDir;
}

export function KeyChoiceField({
  choice,
  path,
  allowGenerate = true,
  onChoice,
  onPath,
  onBlockingChange,
}: {
  choice: KeyChoice;
  path: string;
  allowGenerate?: boolean;
  onChoice: (c: KeyChoice) => void;
  onPath: (p: string) => void;
  /** Whether the chosen file is unusable (missing, unreadable, a `.pub`). */
  onBlockingChange: (blocking: boolean) => void;
}) {
  const [check, setCheck] = useState<IdentityCheck | null>(null);
  const [pickError, setPickError] = useState<string | null>(null);
  const [picking, setPicking] = useState(false);

  // Inspect whatever path is chosen — picked, or restored from the server.
  useEffect(() => {
    let cancelled = false;
    setCheck(null);
    if (choice !== "file" || !path) {
      onBlockingChange(choice === "file");
      return;
    }
    onBlockingChange(true);
    api
      .sshIdentityCheck(path)
      .then((c) => {
        if (cancelled) return;
        setCheck(c);
        onBlockingChange(keyFileNotes(c).some((n) => n.blocking));
      })
      .catch((e) => {
        if (cancelled) return;
        setPickError(e instanceof Error ? e.message : String(e));
        onBlockingChange(true);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [choice, path]);

  const pick = async () => {
    setPickError(null);
    setPicking(true);
    try {
      const chosen = await api.pickKeyFile(path ? path.replace(/\/[^/]*$/, "") : await localSshDir());
      if (typeof chosen === "string" && chosen) onPath(chosen);
    } catch (e) {
      setPickError(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(false);
    }
  };

  const notes = keyFileNotes(check);
  const options = (allowGenerate ? ["default", "file", "generate"] : ["default", "file"]) as KeyChoice[];

  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-1">
        <span className="micro mr-2" style={{ minWidth: 40 }}>
          KEY
        </span>
        {options.map((c) => (
          <button key={c} type="button" className="chip" aria-pressed={choice === c} onClick={() => onChoice(c)}>
            {KEY_LABELS[c]}
          </button>
        ))}
      </div>
      <span className="micro" style={{ color: "var(--text-faint)" }}>
        {KEY_HINTS[choice]}
      </span>

      {choice === "file" && (
        <div className="flex flex-col gap-1">
          <div className="flex items-center gap-2">
            <button type="button" className="btn" onClick={() => void pick()} disabled={picking}>
              {picking ? "choosing…" : path ? "Change…" : "Choose…"}
            </button>
            <span
              className="data min-w-0 flex-1 truncate text-[11px]"
              style={{ color: path ? "var(--text)" : "var(--text-faint)" }}
              title={path || undefined}
            >
              {path || "no key chosen — opens in ~/.ssh (⌘⇧. shows hidden files)"}
            </span>
          </div>
          {notes.map((n) => (
            <p
              key={n.text}
              role={n.blocking ? "alert" : undefined}
              className="data flex flex-wrap items-center gap-2 text-[11px]"
              style={{ color: n.blocking ? "rgb(var(--primary))" : "var(--text-soft)" }}
            >
              {n.text}
              {n.fix && (
                <button type="button" className="chip" onClick={() => onPath(n.fix!.path)}>
                  {n.fix.label}
                </button>
              )}
            </p>
          ))}
          {check && !notes.length && (
            <p className="micro" style={{ color: "var(--text-soft)" }}>
              private key · readable{check.mode != null ? ` · mode ${check.mode.toString(8).padStart(4, "0")}` : ""}
            </p>
          )}
          {pickError && (
            <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
              {pickError}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * TEST CONNECTION — one `uname -s` over a fresh connection with exactly these
 * settings, reported beside the button. It never registers the server or keeps
 * a connection open, so testing and then cancelling changes nothing.
 */
export function TestConnection({ target, disabled }: { target: TargetRef | undefined; disabled?: boolean }) {
  const [state, setState] = useState<{ kind: "idle" } | { kind: "running" } | { kind: "ok" | "err"; text: string }>({
    kind: "idle",
  });
  const key = JSON.stringify(target ?? null);
  // A result describes the settings it was run with; change them and it is stale.
  useEffect(() => setState({ kind: "idle" }), [key]);

  const run = async () => {
    if (!target) return;
    setState({ kind: "running" });
    try {
      setState({ kind: "ok", text: await api.sshConnectionTest(target) });
    } catch (e) {
      setState({ kind: "err", text: e instanceof Error ? e.message : String(e) });
    }
  };

  return (
    <div className="flex min-w-0 flex-1 items-center gap-2">
      <button
        type="button"
        className="btn"
        disabled={disabled || !target || state.kind === "running"}
        onClick={() => void run()}
      >
        {state.kind === "running" ? "testing…" : "Test connection"}
      </button>
      {state.kind === "running" && (
        <span className="micro" style={{ color: "var(--text-faint)" }}>
          a password or 2FA prompt will appear if the host asks
        </span>
      )}
      {(state.kind === "ok" || state.kind === "err") && (
        <span
          role={state.kind === "err" ? "alert" : "status"}
          className="data min-w-0 flex-1 break-words text-[11px]"
          style={{ color: state.kind === "err" ? "rgb(var(--primary))" : "var(--text-soft)" }}
        >
          {state.kind === "ok" ? `✓ ${state.text}` : state.text}
        </span>
      )}
    </div>
  );
}
