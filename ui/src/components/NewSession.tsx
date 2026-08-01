import { useEffect, useMemo, useRef, useState } from "react";
import { motion, AnimatePresence } from "motion/react";

import { api, type ConfigHost, type TargetRef } from "../lib/api";
import { ClaudeSessionPicker } from "./ClaudeSessionPicker";
import { FolderBrowser } from "./FolderBrowser";
import { basename, useSessions } from "../lib/sessions";

/**
 * Create a coding session in two steps: choose a machine, then choose a folder
 * **on** it.
 *
 * The order is the point. A single form asking for a host and a path at once
 * requires you to already know the path — which you generally do not, on a server
 * you have not opened yet. So: pick the host, connect (which is where a bad host
 * or a credential prompt surfaces), and only then browse the directories that
 * actually exist there and pick one.
 *
 * Host names come from `~/.ssh/config`. rmux reads it only to *list* names; the
 * alias goes to `ssh` verbatim and `ssh` resolves the rest.
 */

type Step =
  | { kind: "host" }
  | { kind: "connecting"; target: TargetRef; label: string }
  | { kind: "folder"; target: TargetRef; label: string; home: string }
  | { kind: "claude"; target: TargetRef; label: string; folder: string };

export function NewSession({ onClose }: { onClose: () => void }) {
  const sessions = useSessions((s) => s.sessions);
  const addSession = useSessions((s) => s.addSession);

  const [step, setStep] = useState<Step>({ kind: "host" });
  const [configHosts, setConfigHosts] = useState<ConfigHost[]>([]);
  const [filter, setFilter] = useState("");
  const [typed, setTyped] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const filterRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    filterRef.current?.focus();
    // A missing config is not an error worth showing — it just means there is
    // nothing to suggest, and typing an alias still works.
    api
      .sshConfigHosts()
      // Anything other than an array would throw in the `useMemo` below and take
      // the whole dialog down — which looks like the modal vanishing on click.
      .then((hosts) => setConfigHosts(Array.isArray(hosts) ? hosts : []))
      .catch(() => setConfigHosts([]));
  }, []);

  const inUse = useMemo(
    () => new Set(sessions.map((s) => s.target.host).filter(Boolean) as string[]),
    [sessions],
  );

  const matches = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const ranked = [...configHosts].sort((a, b) => {
      // Hosts you are already working on float to the top.
      const used = Number(inUse.has(b.alias)) - Number(inUse.has(a.alias));
      return used !== 0 ? used : a.alias.localeCompare(b.alias);
    });
    if (!needle) return ranked;
    return ranked.filter(
      (h) =>
        h.alias.toLowerCase().includes(needle) ||
        h.hostname?.toLowerCase().includes(needle) ||
        h.user?.toLowerCase().includes(needle),
    );
  }, [configHosts, filter, inUse]);

  /** Folders already opened on a target — offered as shortcuts when browsing. */
  const recentsFor = (target: TargetRef) =>
    sessions
      .filter((s) => (s.target.host ?? "") === (target.host ?? ""))
      .map((s) => s.folder);

  /**
   * Connect, then move to the folder step.
   *
   * Resolving the home directory is what actually establishes the connection, so
   * this is where an unreachable host, a wrong alias or a credential prompt
   * surfaces — before a session exists, rather than as an empty tree afterwards.
   */
  const connect = async (target: TargetRef, label: string) => {
    setError(null);
    setStep({ kind: "connecting", target, label });
    try {
      const home = await api.fsHome(target);
      setStep({ kind: "folder", target, label, home });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setStep({ kind: "host" });
    }
  };

  /** Folder chosen — now offer the conversations recorded there. */
  const chooseFolder = (target: TargetRef, label: string, folder: string) => {
    setError(null);
    setStep({ kind: "claude", target, label, folder });
  };

  const create = async (target: TargetRef, folder: string, resume?: string, title?: string) => {
    setCreating(true);
    setError(null);
    try {
      // Resuming adopts the conversation's own name; a new one starts as the
      // folder and picks up Claude's title once the work has a shape.
      await addSession(target, folder, title?.trim() || basename(folder), resume);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setCreating(false);
    }
  };

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.12 }}
      className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
      style={{ background: "rgba(6,6,6,0.62)" }}
      onClick={onClose}
    >
      <motion.div
        initial={{ opacity: 0, y: -8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ type: "spring", stiffness: 300, damping: 28 }}
        onClick={(e) => e.stopPropagation()}
        className="menu corner flex w-full max-w-[560px] flex-col gap-4 p-5"
        style={{ height: 520 }}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
      >
        <header className="flex items-baseline gap-3">
          <span className="kicker">New session</span>
          {/* Where you are in the flow, and how to get back. */}
          <span className="micro">
            {step.kind === "host" || step.kind === "connecting"
              ? "1 · choose a machine"
              : step.kind === "folder"
                ? "2 · choose a folder"
                : "3 · resume or start"}
          </span>
          {(step.kind === "folder" || step.kind === "claude") && (
            <button
              type="button"
              className="micro ml-auto"
              onClick={() => {
                setError(null);
                // Back one step, not all the way out.
                if (step.kind === "claude") {
                  setStep({ kind: "folder", target: step.target, label: step.label, home: step.folder });
                } else {
                  setStep({ kind: "host" });
                }
              }}
            >
              {step.kind === "claude" ? "← change folder" : "← change machine"}
            </button>
          )}
        </header>

        {step.kind === "host" && (
          <>
            <input
              ref={filterRef}
              className="field"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="filter hosts…"
              spellCheck={false}
              autoComplete="off"
            />

            <div
              className="inset min-h-0 flex-1 overflow-y-auto"
              style={{ border: "1px solid var(--border)" }}
            >
              <button
                type="button"
                onClick={() => void connect({}, "this machine")}
                className="flex w-full flex-col px-2 py-[6px] text-left"
                onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
                onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
              >
                <span className="data text-[12px]">This machine</span>
                <span className="micro">local — no connection needed</span>
              </button>

              {matches.map((h) => (
                <button
                  key={h.alias}
                  type="button"
                  onClick={() => void connect({ host: h.alias }, h.alias)}
                  className="flex w-full flex-col px-2 py-[6px] text-left"
                  title={`ssh ${h.alias}`}
                  onMouseEnter={(e) => (e.currentTarget.style.background = "var(--hover)")}
                  onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
                >
                  <span className="data truncate text-[12px]">
                    {h.alias}
                    {inUse.has(h.alias) && (
                      <span className="micro" style={{ marginLeft: 6 }}>
                        in use
                      </span>
                    )}
                  </span>
                  {/* Display only — never used to connect. */}
                  <span className="micro truncate">
                    {[h.user, h.hostname].filter(Boolean).join("@") || "—"}
                  </span>
                </button>
              ))}
            </div>

            {/* An alias that is not in the config is still perfectly valid. */}
            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (typed.trim()) void connect({ host: typed.trim() }, typed.trim());
              }}
            >
              <input
                className="field flex-1"
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                placeholder="or type an alias / user@host"
                spellCheck={false}
                autoComplete="off"
              />
              <button className="btn" type="submit" disabled={!typed.trim()}>
                Connect
              </button>
            </form>
          </>
        )}

        {step.kind === "connecting" && (
          <div className="grid min-h-0 flex-1 place-items-center">
            <div className="flex flex-col items-center gap-3">
              {/* Data movement rather than a spinner — rule 2. */}
              <div className="flex h-[16px] items-end gap-[3px]">
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
                <div className="eq-bar" />
              </div>
              <span className="micro">connecting to {step.label}</span>
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                a password or 2FA prompt will appear if the host asks for one
              </span>
            </div>
          </div>
        )}

        {step.kind === "folder" && (
          <>
            <span className="micro">
              on <span style={{ color: "var(--text)" }}>{step.label}</span>
            </span>
            <FolderBrowser
              target={step.target}
              initialPath={step.home}
              recents={recentsFor(step.target)}
              busy={creating}
              onChoose={(folder) => chooseFolder(step.target, step.label, folder)}
            />
          </>
        )}

        {step.kind === "claude" && (
          <>
            <span className="micro">
              on <span style={{ color: "var(--text)" }}>{step.label}</span>
            </span>
            <ClaudeSessionPicker
              target={step.target}
              folder={step.folder}
              busy={creating}
              onChoose={(resume, title) => void create(step.target, step.folder, resume, title)}
            />
          </>
        )}

        {/* Inline and persistent until the next attempt. */}
        {error && (
          <p
            role="alert"
            className="data text-[11px] leading-relaxed"
            style={{ color: "rgb(var(--primary))" }}
          >
            {error}
          </p>
        )}
      </motion.div>
    </motion.div>
  );
}

export function NewSessionLayer({ open, onClose }: { open: boolean; onClose: () => void }) {
  return <AnimatePresence>{open && <NewSession onClose={onClose} />}</AnimatePresence>;
}
