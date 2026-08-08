import { useCallback, useEffect, useRef, useState } from "react";
import * as monaco from "monaco-editor";

import { api, isTauri, type GitChange, type GitCommit, type GitStatus } from "../lib/api";
import { initMonaco, languageForPath, THEME_NAME } from "../lib/monaco";
import { readMonoStack } from "../lib/fonts";
import { useWorkspace } from "../lib/workspace";

/**
 * What changed, for the project this pane belongs to.
 *
 * ## The diff is Monaco's, not ours
 *
 * `rmux-git` returns the two *versions* of a file rather than a unified diff,
 * and `createDiffEditor` does the rest: alignment, intra-line highlighting, and
 * the same syntax colours the editor and the transcript already use. Parsing
 * hunks here would have meant a second diff renderer that disagrees with the
 * editor sitting beside it — the mistake the transcript avoided by sharing the
 * tokenizer.
 *
 * ## Nothing polls
 *
 * `git status` on a large repository is real work on someone else's machine,
 * and the answer only changes when the operator does something. It reads on
 * open and on REFRESH. A four-second timer here would be a background process
 * scanning a checkout forever because a tab is open somewhere.
 */

/**
 * A commit graph drawing itself, while the real one is read.
 *
 * The pane showed nothing at all during the first read — and that read is a
 * `git status` and a `git log` over SSH, which on a large repository takes long
 * enough to look broken. The rule is that a state which can outlast a frame
 * needs a visible state of its own, and it must say *what* it is doing.
 *
 * A skeleton rather than a spinner: the rows sit where the change list will,
 * so nothing jumps when the answer lands.
 */
function Reading({ where }: { where: string }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 px-3 py-3">
      <div className="flex items-center gap-3">
        {/* The trunk grows and the nodes land on it in sequence — the shape of
            a history arriving, rather than an opacity flicker (rule 2). */}
        <svg width="16" height="56" viewBox="0 0 16 56" aria-hidden="true" className="shrink-0">
          <line
            className="git-trunk"
            x1="8" y1="2" x2="8" y2="54"
            stroke="rgb(var(--busy) / 0.45)" strokeWidth="1.5"
          />
          {[8, 22, 36, 50].map((cy) => (
            <circle key={cy} className="git-node" cx="8" cy={cy} r="3" fill="rgb(var(--busy))"
              style={{ transformOrigin: `8px ${cy}px` }} />
          ))}
        </svg>
        <div className="flex min-w-0 flex-col gap-[3px]">
          <span className="micro" style={{ color: "var(--text-soft)" }}>
            READING THE REPOSITORY
          </span>
          {/* Where from, not just that it is loading — over SSH the host is the
              reason it is slow, so naming it is the useful half. */}
          <span className="micro truncate" style={{ color: "var(--text-faint)" }} title={where}>
            {where}
          </span>
        </div>
      </div>

      <ul className="flex flex-col gap-[6px]">
        {[68, 84, 55, 76, 62].map((w, i) => (
          <li key={i} className="git-row flex items-center gap-2">
            <span className="shrink-0" style={{ width: 6, height: 6, background: "var(--text-faint)" }} />
            <span style={{ height: 7, width: `${w}%`, background: "var(--text-faint)" }} />
          </li>
        ))}
      </ul>
    </div>
  );
}

/** A letter and its meaning, for the row marker. */
function mark(change: GitChange): { letter: string; color: string; title: string } {
  const code = change.unstaged === "?" ? "?" : change.staged !== "." ? change.staged : change.unstaged;
  switch (code) {
    case "A":
      return { letter: "A", color: "rgb(var(--busy))", title: "added" };
    case "D":
      // Rule 0 holds: a deletion is not an alarm, it is a change like any
      // other. Red here would compete with the one thing red is for.
      return { letter: "D", color: "var(--text-soft)", title: "deleted" };
    case "R":
      return { letter: "R", color: "var(--text-soft)", title: "renamed" };
    case "?":
      return { letter: "?", color: "var(--text-faint)", title: "untracked" };
    default:
      return { letter: "M", color: "rgb(var(--busy))", title: "modified" };
  }
}

/** Which side of the app a diff came from, so the header can say so. */
type Viewing =
  | { kind: "working"; change: GitChange }
  | { kind: "commit"; sha: string; short: string; change: GitChange };

export function GitPane({ projectId }: { projectId: string }) {
  const project = useWorkspace((s) => s.projects.find((p) => p.id === projectId));
  const target = useWorkspace((s) => s.targetOfProject(projectId));
  const folder = project?.folder ?? "";

  const [root, setRoot] = useState<string | null | undefined>(undefined);
  const [status, setStatus] = useState<GitStatus | null>(null);
  const [commits, setCommits] = useState<GitCommit[]>([]);
  const [openSha, setOpenSha] = useState<string | null>(null);
  const [commitFiles, setCommitFiles] = useState<GitChange[]>([]);
  const [viewing, setViewing] = useState<Viewing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!isTauri() || !folder) return;
    setBusy(true);
    setError(null);
    try {
      const repo = await api.gitRepo(target, folder);
      setRoot(repo.root);
      if (!repo.root) return;
      const [s, l] = await Promise.all([
        api.gitStatus(target, repo.root),
        api.gitLog(target, repo.root, 50),
      ]);
      setStatus(s);
      setCommits(l);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [target, folder]);

  useEffect(() => {
    void load();
  }, [load]);

  const openCommit = async (c: GitCommit) => {
    if (openSha === c.sha) {
      setOpenSha(null);
      return;
    }
    setOpenSha(c.sha);
    setCommitFiles([]);
    if (!root) return;
    try {
      setCommitFiles(await api.gitCommitFiles(target, root, c.sha));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  // The *first* read only. A REFRESH leaves the existing list on screen:
  // replacing a good answer with a skeleton makes the pane flicker under
  // someone who just asked it to check, which is the rule about nothing moving
  // under the operator's hands.
  if (root === undefined || (busy && !status)) {
    return <Reading where={folder} />;
  }

  // Said outright rather than shown as an empty change list, which would claim
  // "nothing has changed" — a different and wrong fact.
  if (root === null) {
    return (
      <div className="grid h-full place-items-center p-6">
        <p className="data max-w-[420px] text-center text-[12px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          <span style={{ color: "var(--text)" }}>{folder}</span> is not inside a git repository.
        </p>
      </div>
    );
  }

  return (
    <div className="flex h-full min-h-0">
      {/* ── left: what changed, and what happened before ─────────────── */}
      <div
        className="flex w-[300px] shrink-0 flex-col border-r"
        style={{ borderColor: "var(--border)" }}
      >
        <div
          className="flex shrink-0 items-center gap-2 border-b px-3 py-[6px]"
          style={{ borderColor: "var(--border)" }}
        >
          <span className="data truncate text-[12px]" style={{ color: "var(--text)" }}>
            {status?.branch ?? "…"}
          </span>
          {status?.upstream && (status.ahead > 0 || status.behind > 0) && (
            <span className="micro shrink-0" style={{ color: "var(--text-faint)" }}>
              {status.ahead > 0 ? `↑${status.ahead}` : ""}
              {status.behind > 0 ? ` ↓${status.behind}` : ""}
            </span>
          )}
          <button
            type="button"
            className="chip ml-auto shrink-0"
            disabled={busy}
            onClick={() => void load()}
          >
            {busy ? "…" : "REFRESH"}
          </button>
        </div>

        {error && (
          <p className="data px-3 py-2 text-[11px] leading-relaxed" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}

        {/* Both lists have a ceiling and scroll inside it. A checkout mid-rebase
            has hundreds of changed files and a repository has thousands of
            commits; either would otherwise push the other off the pane. */}
        <div className="flex min-h-0 flex-1 flex-col">
          <div className="flex min-h-0 flex-[3] flex-col">
            <div className="flex shrink-0 items-baseline justify-between px-3 py-[4px]">
              <span className="micro">CHANGES</span>
              <span className="micro" style={{ color: "var(--text-faint)" }}>
                {status?.changes.length ?? 0}
              </span>
            </div>
            <ul className="min-h-0 flex-1 overflow-y-auto">
              {status?.changes.length === 0 && (
                <li className="micro px-3 py-2" style={{ color: "var(--text-faint)" }}>
                  nothing changed
                </li>
              )}
              {status?.changes.map((change) => {
                const m = mark(change);
                const on = viewing?.kind === "working" && viewing.change.path === change.path;
                return (
                  <li key={change.path}>
                    <button
                      type="button"
                      className="flex w-full items-baseline gap-2 px-3 py-[3px] text-left"
                      style={{ background: on ? "var(--hover)" : "transparent" }}
                      onClick={() => setViewing({ kind: "working", change })}
                    >
                      <span className="data shrink-0 text-[10px]" style={{ color: m.color }} title={m.title}>
                        {m.letter}
                      </span>
                      {/* The tail is what distinguishes two files with the same
                          name, so the truncation has to bite at the front. */}
                      <span
                        className="data min-w-0 flex-1 truncate text-[11px]"
                        style={{ color: "var(--text)", direction: "rtl", textAlign: "left" }}
                        title={change.origPath ? `${change.origPath} → ${change.path}` : change.path}
                      >
                        {change.path}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>

          <div className="flex min-h-0 flex-[4] flex-col border-t" style={{ borderColor: "var(--border)" }}>
            <span className="micro shrink-0 px-3 py-[4px]">HISTORY</span>
            <ul className="min-h-0 flex-1 overflow-y-auto">
              {commits.map((c) => (
                <li key={c.sha}>
                  <button
                    type="button"
                    className="flex w-full flex-col items-start gap-[1px] px-3 py-[4px] text-left"
                    style={{ background: openSha === c.sha ? "var(--hover)" : "transparent" }}
                    onClick={() => void openCommit(c)}
                  >
                    <span className="data w-full truncate text-[11px]" style={{ color: "var(--text)" }}>
                      {c.subject}
                    </span>
                    <span className="micro w-full truncate" style={{ color: "var(--text-faint)" }}>
                      {c.short} · {c.author} · {when(c.date)}
                    </span>
                  </button>

                  {openSha === c.sha && (
                    <ul className="pb-1">
                      {commitFiles.length === 0 && (
                        <li className="micro px-3 py-1 pl-6" style={{ color: "var(--text-faint)" }}>
                          reading…
                        </li>
                      )}
                      {commitFiles.map((f) => {
                        const m = mark(f);
                        const on = viewing?.kind === "commit" && viewing.sha === c.sha && viewing.change.path === f.path;
                        return (
                          <li key={f.path}>
                            <button
                              type="button"
                              className="flex w-full items-baseline gap-2 py-[2px] pl-6 pr-3 text-left"
                              style={{ background: on ? "var(--hover)" : "transparent" }}
                              onClick={() =>
                                setViewing({ kind: "commit", sha: c.sha, short: c.short, change: f })
                              }
                            >
                              <span className="data shrink-0 text-[10px]" style={{ color: m.color }}>
                                {m.letter}
                              </span>
                              <span
                                className="data min-w-0 flex-1 truncate text-[10.5px]"
                                style={{ color: "var(--text-soft)", direction: "rtl", textAlign: "left" }}
                                title={f.path}
                              >
                                {f.path}
                              </span>
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </li>
              ))}
            </ul>
          </div>
        </div>
      </div>

      {/* ── right: the diff itself ───────────────────────────────────── */}
      <div className="min-h-0 min-w-0 flex-1">
        {viewing && root ? (
          <DiffView
            key={`${viewing.kind}:${viewing.kind === "commit" ? viewing.sha : ""}:${viewing.change.path}`}
            target={target}
            root={root}
            viewing={viewing}
          />
        ) : (
          <div className="grid h-full place-items-center">
            <span className="micro" style={{ color: "var(--text-faint)" }}>
              SELECT A CHANGE
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * One file, before and after.
 *
 * The editor is created once and disposed on unmount; the models are created
 * with it. Re-mounting per selection (via `key`) rather than swapping models is
 * deliberate — a diff editor holds two models and disposing them in the wrong
 * order leaks one, and the cost of a fresh editor is unmeasurable next to the
 * SSH round trip that fetched the content.
 */
function DiffView({
  target,
  root,
  viewing,
}: {
  target: Parameters<typeof api.gitFileDiff>[0];
  root: string;
  viewing: Viewing;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<{ truncated: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let editor: monaco.editor.IStandaloneDiffEditor | null = null;
    let models: monaco.editor.ITextModel[] = [];

    void (async () => {
      try {
        const against =
          viewing.kind === "working"
            ? ({ kind: "working" } as const)
            : ({ kind: "commit", sha: viewing.sha } as const);
        const diff = await api.gitFileDiff(target, root, viewing.change.path, against);
        if (disposed) return;

        const host = hostRef.current;
        if (!host) return;
        initMonaco();

        const language = languageForPath(viewing.change.path);
        const original = monaco.editor.createModel(diff.oldText, language);
        const modified = monaco.editor.createModel(diff.newText, language);
        models = [original, modified];

        editor = monaco.editor.createDiffEditor(host, {
          theme: THEME_NAME,
          automaticLayout: true,
          fontFamily: readMonoStack(),
          fontSize: 12.5,
          lineHeight: 1.6,
          minimap: { enabled: false },
          scrollBeyondLastLine: false,
          // Read-only on both sides. This is a *preview*: editing here would
          // write to a file the pane does not own, and an edit that silently
          // does not save is worse than one that cannot be made.
          readOnly: true,
          originalEditable: false,
          renderSideBySide: true,
          renderOverviewRuler: false,
        });
        editor.setModel({ original, modified });
        setState({ truncated: diff.truncated });
      } catch (e) {
        if (!disposed) setError(e instanceof Error ? e.message : String(e));
      }
    })();

    return () => {
      disposed = true;
      editor?.dispose();
      // After the editor, or it is left holding disposed models.
      for (const m of models) m.dispose();
    };
  }, [target, root, viewing]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className="flex shrink-0 items-baseline gap-2 border-b px-3 py-[5px]"
        style={{ borderColor: "var(--border)" }}
      >
        <span className="data min-w-0 flex-1 truncate text-[11px]" style={{ color: "var(--text)" }}>
          {viewing.change.origPath
            ? `${viewing.change.origPath} → ${viewing.change.path}`
            : viewing.change.path}
        </span>
        <span className="micro shrink-0" style={{ color: "var(--text-faint)" }}>
          {viewing.kind === "working" ? "WORKING TREE" : `COMMIT ${viewing.short}`}
        </span>
      </div>

      {state?.truncated && (
        <p className="micro shrink-0 px-3 py-1" style={{ color: "rgb(var(--primary))" }}>
          TRUNCATED — THIS FILE IS TOO LARGE TO DIFF IN FULL
        </p>
      )}
      {error && (
        <p className="data px-3 py-2 text-[11px]" style={{ color: "rgb(var(--primary))" }}>
          {error}
        </p>
      )}

      <div ref={hostRef} className="min-h-0 flex-1" />
    </div>
  );
}

/** `3h`, `2d`, or a date once it stops being recent. */
function when(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const seconds = Math.max(0, (Date.now() - at.getTime()) / 1000);
  if (seconds < 3600) return `${Math.max(1, Math.round(seconds / 60))}m`;
  if (seconds < 86_400) return `${Math.round(seconds / 3600)}h`;
  if (seconds < 86_400 * 14) return `${Math.round(seconds / 86_400)}d`;
  return at.toLocaleDateString(undefined, { day: "numeric", month: "short" });
}
