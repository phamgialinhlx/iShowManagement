import { useEffect, useRef, useState } from "react";

import { api, type SearchHit, type TargetRef } from "../lib/api";

/**
 * Find text across the project — ⌘⇧F.
 *
 * ## It reports every state it can be in
 *
 * Searching a real checkout over SSH takes a moment, and a pane that shows
 * nothing while it works is indistinguishable from one that is broken. So there
 * are four distinct states and each says which it is: idle (with the shortcut
 * that got you here), searching (with what is being searched), a result count,
 * or "nothing matched" — never a blank.
 *
 * ## Truncation is stated, never implied
 *
 * The backend stops at 500 hits. A list that silently ends at a round number
 * reads as the whole answer, and the operator concludes there are no more
 * matches when there may be thousands. When the cap is reached it says so and
 * says what to do about it.
 *
 * ## The query is not run on every keystroke
 *
 * Each search spawns a process on the far machine. Debounced, and superseded
 * results are dropped — otherwise a fast typist queues eight greps and watches
 * the answers arrive out of order.
 */
export function FileSearch({
  target,
  root,
  onOpen,
  onClose,
}: {
  target: TargetRef;
  root: string;
  onOpen: (path: string, line: number) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState("");
  const [caseSensitive, setCaseSensitive] = useState(false);
  const [regex, setRegex] = useState(false);
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Focus on open. Opening a search box you then have to click into is the
  // shortcut failing at the only thing it was for.
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  useEffect(() => {
    const query = text.trim();
    if (!query) {
      setHits(null);
      setError(null);
      return;
    }

    let cancelled = false;
    setBusy(true);
    const timer = setTimeout(() => {
      api
        .fsSearch(target, root, { text: query, caseSensitive, regex })
        .then((found) => {
          // A superseded search must not overwrite a newer one.
          if (cancelled) return;
          setHits(found);
          setError(null);
        })
        .catch((e) => !cancelled && setError(e instanceof Error ? e.message : String(e)))
        .finally(() => !cancelled && setBusy(false));
    }, 220);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [text, caseSensitive, regex, target, root]);

  const capped = hits !== null && hits.length >= 500;

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="flex items-center gap-2">
        <input
          ref={inputRef}
          value={text}
          spellCheck={false}
          placeholder="find in files"
          className="data inset min-w-0 flex-1 px-2 py-[5px] text-[12px] outline-none"
          style={{ border: "1px solid var(--border-strong)", color: "var(--text)", background: "transparent" }}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
          }}
        />
        <button type="button" className="micro" onClick={onClose} title="Escape">
          CLOSE
        </button>
      </div>

      <div className="flex items-center gap-3">
        <Toggle on={caseSensitive} onClick={() => setCaseSensitive((v) => !v)} label="Aa" title="Match case" />
        <Toggle on={regex} onClick={() => setRegex((v) => !v)} label=".*" title="Regular expression" />
        <span className="micro ml-auto truncate" title={root}>
          {root}
        </span>
      </div>

      {error && (
        <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      {/* Four states, each named. Never a blank pane. */}
      {!text.trim() ? (
        <span className="micro py-4">TYPE TO SEARCH EVERY FILE UNDER THIS FOLDER</span>
      ) : busy && hits === null ? (
        <div className="flex items-center gap-2 py-4">
          <div className="flex h-[14px] items-end gap-[3px]" aria-hidden="true">
            <div className="eq-bar" />
            <div className="eq-bar" />
            <div className="eq-bar" />
          </div>
          <span className="micro">searching {target.host ?? "this machine"}…</span>
        </div>
      ) : hits && hits.length === 0 ? (
        <span className="micro py-4">NOTHING MATCHED</span>
      ) : (
        <>
          <span className="micro">
            {hits?.length} {hits?.length === 1 ? "MATCH" : "MATCHES"}
            {busy ? " · REFRESHING" : ""}
          </span>

          <div className="min-h-0 flex-1 overflow-y-auto" style={{ border: "1px solid var(--border)" }}>
            {hits?.map((hit, i) => (
              <button
                key={`${hit.path}:${hit.line}:${i}`}
                type="button"
                onClick={() => onOpen(hit.path, hit.line)}
                className="flex w-full flex-col gap-[1px] border-b px-2 py-[5px] text-left"
                style={{ borderColor: "var(--border)" }}
              >
                <span className="flex items-baseline gap-2">
                  <span className="micro shrink-0" style={{ color: "var(--text-soft)" }}>
                    {hit.line}
                  </span>
                  {/* The line, monospaced and clipped rather than wrapped — a
                      result list you can scan beats one you can read in full. */}
                  <span className="data flex-1 truncate text-[11.5px]" style={{ color: "var(--text)" }}>
                    {hit.text}
                  </span>
                </span>
                <span className="micro truncate" style={{ letterSpacing: "0.08em" }}>
                  {relative(hit.path, root)}
                </span>
              </button>
            ))}
          </div>

          {/* Stated, not implied. A list that stops at a round number reads as
              the complete answer. */}
          {capped && (
            <span className="micro" style={{ color: "rgb(var(--busy))" }}>
              FIRST 500 SHOWN — NARROW THE SEARCH TO SEE THE REST
            </span>
          )}
        </>
      )}
    </div>
  );
}

function Toggle({
  on,
  onClick,
  label,
  title,
}: {
  on: boolean;
  onClick: () => void;
  label: string;
  title: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-pressed={on}
      className="data px-[6px] py-[2px] text-[11px]"
      style={{
        color: on ? "var(--text)" : "var(--text-faint)",
        border: `1px solid ${on ? "var(--border-strong)" : "transparent"}`,
        background: on ? "var(--app-elev)" : "transparent",
      }}
    >
      {label}
    </button>
  );
}

/** Show the path relative to the project, since the prefix is the same for all. */
function relative(path: string, root: string): string {
  const trimmed = root.endsWith("/") ? root : `${root}/`;
  return path.startsWith(trimmed) ? path.slice(trimmed.length) : path;
}
