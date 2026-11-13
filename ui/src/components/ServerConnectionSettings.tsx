import { useState } from "react";
import { motion } from "motion/react";

import { api } from "../lib/api";
import { targetLabel, type KeyChoice } from "../lib/connect-form";
import { useWorkspace } from "../lib/workspace";
import type { Server } from "../lib/workspace-model";
import { KeyChoiceField, TestConnection } from "./ConnectionFields";

/**
 * A server's connection settings — the key, edited in place.
 *
 * ## Only the key is editable here, and the dialog says why
 *
 * Host, user and port are *which server this is*: the server's id is derived
 * from them, and every project and session on it is filed under that id.
 * Changing one would silently re-key all of them, or leave them pointing at a
 * server that no longer exists. So those three are shown, read-only, with the
 * honest answer — a different host/user/port is a new server, added from
 * Connect a server. The key is how rmux authenticates, not which machine it
 * is, so it changes in place.
 *
 * ## Saved settings apply on the next connection
 *
 * An open connection has already authenticated, and keeps working. RECONNECT
 * NOW is the existing disconnect (sessions keep running on the host) — the
 * next use dials again with the new key.
 */
export function ServerConnectionSettings({
  server,
  onClose,
  onReconnect,
}: {
  server: Server;
  onClose: () => void;
  /** The rail's own disconnect, so there is one definition of it. */
  onReconnect: () => Promise<string>;
}) {
  const setServerIdentity = useWorkspace((s) => s.setServerIdentity);
  const saved = server.target.identity ?? "";
  const [choice, setChoice] = useState<KeyChoice>(saved ? "file" : "default");
  const [path, setPath] = useState(saved);
  const [blocked, setBlocked] = useState(false);
  const [note, setNote] = useState<{ text: string; ok: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const [savedOnce, setSavedOnce] = useState(false);

  const identity = choice === "file" ? path.trim() || undefined : undefined;
  const draft = { ...server.target, identity };
  if (!identity) delete draft.identity;
  const changed = (identity ?? "") !== saved;

  const save = async () => {
    setNote(null);
    if (choice === "generate") {
      // Generating needs the host: it runs over the connection, which may ask
      // for the password one last time.
      setBusy(true);
      try {
        const text = await api.sshKeyInstall({ ...server.target, identity: undefined });
        setServerIdentity(server.id, undefined);
        setNote({ ok: true, text: `${text} · offered automatically from now on` });
        setSavedOnce(true);
        setChoice("default");
      } catch (e) {
        setNote({ ok: false, text: e instanceof Error ? e.message : String(e) });
      } finally {
        setBusy(false);
      }
      return;
    }
    setServerIdentity(server.id, identity);
    setSavedOnce(true);
    setNote({ ok: true, text: "saved · applies on the next connection" });
  };

  const reconnect = async () => {
    setBusy(true);
    setNote({ ok: true, text: "disconnecting…" });
    try {
      setNote({ ok: true, text: await onReconnect() });
    } catch (e) {
      setNote({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  const fieldRow = (label: string, value: string | undefined) => (
    <div className="flex items-baseline gap-3">
      <span className="micro" style={{ minWidth: 40 }}>
        {label}
      </span>
      <span className="data truncate text-[12px]" style={{ color: value ? "var(--text)" : "var(--text-faint)" }}>
        {value || "ssh's default"}
      </span>
    </div>
  );

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.12 }}
      className="fixed inset-0 z-[95] grid place-items-start justify-center pt-[9vh]"
      style={{ background: "color-mix(in srgb, var(--app-bg) 62%, transparent)" }}
      onClick={onClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") onClose();
        }}
        className="menu corner flex w-full max-w-[520px] flex-col gap-4 overflow-y-auto p-5"
        style={{ maxHeight: "calc(91vh - 16px)" }}
      >
        <header className="flex items-baseline gap-3">
          <span className="kicker">Connection settings</span>
          <span className="micro">{targetLabel(server.target)}</span>
        </header>

        <div className="flex flex-col gap-1">
          {fieldRow("HOST", server.target.host)}
          {fieldRow("USER", server.target.user)}
          {fieldRow("PORT", server.target.port ? String(server.target.port) : undefined)}
          <p className="micro mt-1" style={{ color: "var(--text-faint)" }}>
            Host, user and port are which server this is — its projects and sessions are filed under them. A
            different one is a new server: add it from Connect a server.
          </p>
        </div>

        <KeyChoiceField
          choice={choice}
          path={path}
          onChoice={(c) => (setChoice(c), setNote(null))}
          onPath={(p) => (setPath(p), setNote(null))}
          onBlockingChange={setBlocked}
        />

        <div className="flex flex-wrap items-center gap-2">
          <TestConnection target={choice === "generate" ? undefined : draft} disabled={blocked} />
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {note && (
            <span
              role={note.ok ? "status" : "alert"}
              className="data min-w-0 flex-1 text-[11px]"
              style={{ color: note.ok ? "var(--text-soft)" : "rgb(var(--primary))" }}
            >
              {note.text}
            </span>
          )}
          <span className="ml-auto flex gap-2">
            {savedOnce && (
              <button type="button" className="btn" disabled={busy} onClick={() => void reconnect()}>
                Reconnect now
              </button>
            )}
            <button type="button" className="btn" onClick={onClose}>
              {savedOnce ? "Close" : "Cancel"}
            </button>
            <button
              type="button"
              className="btn-primary btn"
              disabled={busy || blocked || (choice !== "generate" && !changed)}
              onClick={() => void save()}
            >
              {busy && choice === "generate" ? "installing…" : choice === "generate" ? "Generate & install" : "Save"}
            </button>
          </span>
        </div>
      </div>
    </motion.div>
  );
}
