import { useEffect, useState } from "react";

import { api, type ModelProfile, type ParsedProfile } from "../lib/api";

/**
 * Model profiles — running Claude Code against something other than Anthropic.
 *
 * ## Paste, do not retype
 *
 * A provider hands you a block of `KEY=value \` lines. Eight text fields would
 * mean transcribing a base URL and a token by hand, and a typo in the base URL
 * points a credential at the wrong host without ever looking wrong. So the
 * whole block goes in one textarea and rmux parses it.
 *
 * ## It shows what it understood before saving
 *
 * The paste is parsed as it is typed and the result is shown: which variables
 * were taken, which were ignored and why, and what the endpoint will be. A
 * parser that silently drops half a paste and reports success is how someone
 * spends an afternoon debugging the wrong provider.
 *
 * ## The endpoint is stated, never implied
 *
 * Choosing a profile decides where a bearer token is sent. The host is
 * therefore printed on every row — a profile's *name* is chosen by the operator
 * and proves nothing about where it points.
 */
export function ModelProfilesPanel() {
  const [profiles, setProfiles] = useState<ModelProfile[] | null>(null);
  const [editing, setEditing] = useState<null | { id: string | null; name: string }>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .modelProfiles()
      .then(setProfiles)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  return (
    <div className="flex flex-col gap-5" style={{ maxWidth: 640 }}>
      <header className="flex flex-col gap-1">
        <h2 className="kicker">MODEL PROFILES</h2>
        <p className="data text-[11px] leading-[1.5]" style={{ color: "var(--text-soft)" }}>
          Run a session against Kimi, GLM, an internal gateway or a reseller instead of
          Anthropic. A profile is the set of environment variables that provider gave you;
          pick one when you start a session.
        </p>
      </header>

      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      {profiles === null ? (
        <span className="micro">reading the keychain…</span>
      ) : profiles.length === 0 && !editing ? (
        // An empty state that says what this is for and offers the one action,
        // rather than an empty box that reads as a feature that failed to load.
        <div className="inset flex flex-col items-start gap-3 p-4">
          <span className="data text-[11px]" style={{ color: "var(--text-soft)" }}>
            No profiles yet. Sessions use your Claude account and Anthropic's API.
          </span>
          <button type="button" className="btn btn-primary" onClick={() => setEditing({ id: null, name: "" })}>
            Add a profile
          </button>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {profiles.map((p) => (
            <Row
              key={p.id}
              profile={p}
              onEdit={() => setEditing({ id: p.id, name: p.name })}
              onDeleted={setProfiles}
            />
          ))}

          {!editing && (
            <button
              type="button"
              className="btn self-start"
              onClick={() => setEditing({ id: null, name: "" })}
            >
              Add a profile
            </button>
          )}
        </div>
      )}

      {editing && (
        <Editor
          initial={editing}
          onSaved={(next) => {
            setProfiles(next);
            setEditing(null);
          }}
          onCancel={() => setEditing(null)}
        />
      )}
    </div>
  );
}

function Row({
  profile,
  onEdit,
  onDeleted,
}: {
  profile: ModelProfile;
  onEdit: () => void;
  onDeleted: (profiles: ModelProfile[]) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      onDeleted(await api.modelProfileDelete(profile.id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <div className="inset flex flex-col gap-2 p-3">
      <div className="flex items-baseline gap-3">
        <span className="data text-[12px]" style={{ color: "var(--text)" }}>
          {profile.name}
        </span>
        {/* The destination, not the name, is the fact that matters. */}
        <span className="micro truncate" title={profile.endpoint ?? "Anthropic"}>
          {profile.endpoint ?? "api.anthropic.com"}
        </span>
        <span className="micro ml-auto shrink-0">
          {profile.hasCredential ? "OWN TOKEN" : "HOST LOGIN"}
        </span>
      </div>

      <div className="flex flex-wrap gap-x-4 gap-y-[2px]">
        {profile.vars.map((v) => (
          <span key={v.key} className="micro" style={{ letterSpacing: "0.06em" }}>
            {v.key}=
            <span style={{ color: v.secret ? "var(--text-faint)" : "var(--text-soft)" }}>
              {v.value}
            </span>
          </span>
        ))}
      </div>

      {confirming ? (
        <div className="flex items-center gap-2">
          <span className="data text-[11px]">
            Delete <span style={{ color: "rgb(var(--primary))" }}>{profile.name}</span>? Sessions
            using it will refuse to start until you pick another.
          </span>
          <button type="button" className="btn btn-primary ml-auto" disabled={busy} onClick={remove}>
            {busy ? "Deleting…" : "Delete"}
          </button>
          <button type="button" className="btn" onClick={() => setConfirming(false)}>
            Cancel
          </button>
        </div>
      ) : (
        <div className="flex gap-2">
          <button type="button" className="btn" onClick={onEdit}>
            Edit
          </button>
          <button type="button" className="btn" onClick={() => setConfirming(true)}>
            Delete
          </button>
        </div>
      )}

      {error && (
        <p role="alert" className="data text-[10px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}
    </div>
  );
}

function Editor({
  initial,
  onSaved,
  onCancel,
}: {
  initial: { id: string | null; name: string };
  onSaved: (profiles: ModelProfile[]) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState(initial.name);
  const [text, setText] = useState("");
  const [parsed, setParsed] = useState<ParsedProfile | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Parsed as it is typed, so what rmux understood is visible *before* it is
  // committed. Debounced because each keystroke would otherwise cross IPC.
  useEffect(() => {
    if (!text.trim()) {
      setParsed(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .modelProfileParse(text)
        .then((p) => !cancelled && setParsed(p))
        .catch(() => {});
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [text]);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      onSaved(await api.modelProfileSave(initial.id, name, text));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  const endpoint = parsed?.vars["ANTHROPIC_BASE_URL"];
  const keys = Object.keys(parsed?.vars ?? {});

  return (
    <div className="inset flex flex-col gap-3 p-4">
      <span className="kicker">{initial.id ? "EDIT PROFILE" : "NEW PROFILE"}</span>

      <label className="flex flex-col gap-1">
        <span className="micro">NAME</span>
        <input
          className="field"
          value={name}
          spellCheck={false}
          placeholder="Synthetic, GLM, Kimi…"
          onChange={(e) => setName(e.target.value)}
        />
      </label>

      <label className="flex flex-col gap-1">
        <span className="micro">CONFIGURATION</span>
        <span className="data text-[10.5px]" style={{ color: "var(--text-faint)" }}>
          Paste the block your provider gave you. Shell continuations, <code>export</code> and
          quotes are all fine.
        </span>
        <textarea
          className="field data"
          rows={8}
          spellCheck={false}
          value={text}
          onChange={(e) => setText(e.target.value)}
          style={{ fontSize: 11, lineHeight: 1.5, resize: "vertical" }}
          placeholder={"ANTHROPIC_BASE_URL=https://…\nANTHROPIC_AUTH_TOKEN=…\nANTHROPIC_DEFAULT_OPUS_MODEL=…"}
        />
        {/* Editing shows an empty box on purpose: the stored token never comes
            back to the page, so there is nothing to edit in place. Said out
            loud, because a blank field on "Edit" otherwise reads as data loss. */}
        {initial.id && (
          <span className="data text-[10.5px]" style={{ color: "var(--text-faint)" }}>
            Paste the whole block again — the saved token is never sent back to this window, so
            there is nothing here to edit.
          </span>
        )}
      </label>

      {/* What rmux understood. Named, not counted. */}
      {parsed && (
        <div className="flex flex-col gap-1">
          <span className="micro">
            {keys.length} {keys.length === 1 ? "VARIABLE" : "VARIABLES"} · SENDS TO{" "}
            {endpoint || "api.anthropic.com"}
          </span>
          {parsed.ignored.length > 0 && (
            <span className="data text-[10.5px]" style={{ color: "rgb(var(--busy))" }}>
              Ignored: {parsed.ignored.join(", ")} — only ANTHROPIC_ and CLAUDE_CODE_ variables
              are carried.
            </span>
          )}
          {parsed.warnings.map((w) => (
            <span key={w} className="data text-[10.5px]" style={{ color: "rgb(var(--busy))" }}>
              {w}
            </span>
          ))}
        </div>
      )}

      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      <div className="flex gap-2">
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || !name.trim() || !keys.length}
          onClick={save}
        >
          {busy ? "Saving…" : "Save to keychain"}
        </button>
        <button type="button" className="btn" onClick={onCancel}>
          Cancel
        </button>
        <span className="micro ml-auto self-center">STORED IN THE OS KEYCHAIN, NOT ON DISK</span>
      </div>
    </div>
  );
}
