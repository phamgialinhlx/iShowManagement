import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { record } from "../../lib/activity";
import { linesForBlocks, lineOfOffset, offsetOfLine } from "../../lib/note-lines";
import { MARKDOWN_COMPONENTS } from "../../lib/markdown-code";
import { continueList, noteTasks, taskProgress, toggleTask } from "../../lib/note-tasks";

/**
 * A scratch note, per session.
 *
 * The thing you would otherwise keep in a text file you never open again: the
 * staging URL, the test account, what you were in the middle of when you
 * switched machines. Per session because that is what makes it worth having —
 * a single global pad is a second notes app, whereas a note attached to *this
 * host, this folder* is context you get back for free when you return.
 *
 * Kept in `localStorage` rather than on the target. It is about the work, not
 * part of it: writing a file into someone's repository because they typed a
 * reminder would be a surprising side effect, and it would want committing.
 *
 * ## Two states, and the whole note is in one of them
 *
 * Click it and the **entire note** becomes raw markdown you can edit. Click
 * away and the entire note renders. That is the whole model.
 *
 * It replaced a per-block editor that swapped only the paragraph under the
 * caret, Notion-style. That version was better on paper and worse in the hand:
 * blocks split and merged as you typed, so the thing being edited changed shape
 * underneath you, and three separate faults came out of it — a remount mid-word
 * that ate text, checkbox indices that drifted, and a caret the browser took
 * away as the editor mounted. One textarea over one string has none of those
 * failure modes, because nothing is being reconciled.
 *
 * The cost is that the note is not *both* readable and editable at once. That
 * is the right trade here: a note is read far more often than written, and
 * reading is the state it sits in.
 */

const key = (sessionId: string) => `rmux.note.${sessionId}`;
const heightKey = (sessionId: string) => `rmux.note.height.${sessionId}`;

/**
 * Where each session's note was scrolled to.
 *
 * The widget is rebuilt whenever the rail switches session, so the scroller
 * came back at the top every time — on a note longer than the box, that means
 * the part you were working on is somewhere above and you have to find it
 * again on every switch, which is exactly the "nothing moves under the
 * operator's hands" rule failing in slow motion.
 *
 * **In memory, not `localStorage`.** A scroll offset is a position in a session
 * you are in the middle of, not a preference: it is meaningless a day later
 * against a note that has since been edited, and the storage quota it would
 * share is the one that also holds the session list. Restoring at the top after
 * a restart is correct; doing it on every glance is not.
 */
const scrollOf = new Map<string, number>();
/** Long enough that a burst of typing is one write, short enough to survive a crash. */
const SAVE_AFTER = 400;
const MIN_HEIGHT = 64;

export function Note({ sessionId }: { sessionId: string }) {
  const [text, setText] = useState("");
  const [editing, setEditing] = useState(false);
  const [height, setHeight] = useState(140);
  const timer = useRef<number | undefined>(undefined);
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  /**
   * The line the reader was on, carried across the switch between the two faces
   * of this note.
   *
   * Rendered markdown and raw source are **different scrollers with different
   * content**, so a scroll offset does not survive the switch: the textarea is
   * `h-full` and scrolls inside itself, while the rendered note makes the box
   * around it scroll. Neither knows the other's position, which is why clicking
   * to edit opened at the top and clicking away went back to the top. A *line*
   * survives where a pixel offset cannot.
   */
  const enterAt = useRef<number | null>(null);
  const leaveAt = useRef<number | null>(null);

  /**
   * Put the scroller back where this session left it.
   *
   * A **layout** effect, so the offset is applied in the same frame the content
   * is laid out — with a plain effect the box paints at the top first and the
   * restore reads as a jump. It runs on `text` as well as `sessionId` because
   * the note's content arrives in a second render: the first pass has an empty
   * string, nothing to scroll, and any offset set then is silently clamped to
   * zero.
   */
  useLayoutEffect(() => {
    const el = scrollRef.current;
    const at = scrollOf.get(sessionId);
    if (!el || !at) return;
    // Clamped by the browser anyway, but doing it here means a note that has
    // since been shortened lands at its end rather than reporting a position
    // that no longer exists.
    el.scrollTop = Math.min(at, Math.max(0, el.scrollHeight - el.clientHeight));
  }, [sessionId, text, editing, height]);

  /**
   * Label each rendered block with the source line it came from, and put the
   * reader back on the line they were editing.
   *
   * The labelling is done here rather than in the markdown components because
   * `react-markdown` **strips `node.position`** — measured in this file already,
   * where the task checkboxes had to fall back to document order for the same
   * reason. Document order is parse order, so the blocks are matched against the
   * source in order, forward only (`linesForBlocks`).
   *
   * A layout effect, so the scroll lands in the frame the content is laid out
   * in: with a plain effect the note paints at the top first and the restore
   * reads as a jump — the same reason the offset restore below is one.
   */
  useLayoutEffect(() => {
    if (editing) return;
    const root = scrollRef.current?.querySelector<HTMLElement>(".note-rendered");
    if (!root) return;

    // Leaf blocks only. A loose list renders `li > p`, and counting both would
    // consume two source lines for one block and drag every later match out of
    // step.
    const all = [...root.querySelectorAll<HTMLElement>("p, li, h1, h2, h3, h4, h5, h6, pre, blockquote")];
    const blocks = all.filter((el) => !all.some((other) => other !== el && el.contains(other)));

    const lines = linesForBlocks(text, blocks.map((el) => el.textContent ?? ""));
    blocks.forEach((el, i) => {
      const line = lines[i]!;
      if (line >= 0) el.dataset.line = String(line);
      else delete el.dataset.line;
    });

    const want = leaveAt.current;
    leaveAt.current = null;
    if (want === null) return;

    // The nearest labelled block at or above the line being edited — a blank
    // line, or one inside a fenced block, has no element of its own.
    let best: HTMLElement | null = null;
    for (const el of blocks) {
      const line = Number(el.dataset.line);
      if (Number.isFinite(line) && line <= want) best = el;
    }
    const scroller = scrollRef.current;
    if (!best || !scroller) return;

    const top =
      best.getBoundingClientRect().top -
      scroller.getBoundingClientRect().top +
      scroller.scrollTop;
    scroller.scrollTop = Math.max(0, top - 8);
    scrollOf.set(sessionId, scroller.scrollTop);
  }, [editing, text, sessionId, height]);

  // Reloaded per session, and *not* merged: switching sessions must show the
  // other note, never this one's text under the other one's name.
  useEffect(() => {
    setText(localStorage.getItem(key(sessionId)) ?? "");
    setHeight(Number(localStorage.getItem(heightKey(sessionId))) || 140);
    setEditing(false);
    return () => {
      // Flush on the way out, or switching sessions inside the debounce window
      // silently discards whatever was just typed.
      window.clearTimeout(timer.current);
    };
  }, [sessionId]);

  const save = useCallback(
    (value: string) => {
      window.clearTimeout(timer.current);
      const id = sessionId;
      timer.current = window.setTimeout(() => {
        try {
          if (value) localStorage.setItem(key(id), value);
          else localStorage.removeItem(key(id));
          // `localStorage` does not fire `storage` in the tab that wrote it, so
          // the dashboard counting these tasks in another window would never
          // hear about an edit without this.
          window.dispatchEvent(new CustomEvent("rmux:notes-changed"));
        } catch {
          // A full localStorage must not break typing.
        }
      }, SAVE_AFTER);
    },
    [sessionId],
  );

  /**
   * Remember the line the caret is on, before the editor goes away.
   *
   * Called from **every** exit — blur, DONE, Escape — because a note that keeps
   * your place from one of the three and loses it from the others is harder to
   * trust than one that never keeps it. Read at the moment of leaving, since the
   * textarea is unmounted immediately afterwards.
   */
  const rememberCaret = () => {
    const el = areaRef.current;
    if (el) leaveAt.current = lineOfOffset(el.value, el.selectionStart);
  };

  const change = (value: string) => {
    setText(value);
    save(value);
  };

  const progress = useMemo(() => taskProgress(text), [text]);

  /**
   * Take focus once the editor exists, and put the caret at the end.
   *
   * `autoFocus` is not enough, and the reason is worth keeping. Editing starts
   * on `mousedown`; the browser then finishes that gesture by moving focus
   * itself — by which time React has replaced the rendered note with a
   * textarea, so focus lands on nothing and no caret appears. The rendered note
   * calls `preventDefault` to stop that, and focus is taken here, where the
   * element is known to exist.
   *
   * For anyone testing this: a *dispatched* `mousedown` does not run the
   * browser's default focus handling, so a synthetic click cannot see this bug.
   * It reported a working editor while the app had none.
   */
  useEffect(() => {
    if (!editing) return;
    const el = areaRef.current;
    if (!el) return;

    const line = enterAt.current;
    enterAt.current = null;
    const at = line === null ? el.value.length : offsetOfLine(el.value, line);

    // **Selection first, focus second.** A textarea scrolls to its caret when it
    // *gains* focus; setting the range afterwards moves the caret without moving
    // the view, which is how the editor opened at the top with the caret
    // somewhere off-screen. This order also handles wrapped lines correctly,
    // which arithmetic on a line height does not.
    el.setSelectionRange(at, at);
    el.focus();
  }, [editing]);

  /**
   * Ticking a box rewrites the markdown line it came from.
   *
   * **The index is read from the DOM at the moment of the click.** Two earlier
   * attempts were both wrong and both silent: a ref counting the boxes as React
   * built them drifted (children are re-invoked without the parent, and
   * StrictMode double-invokes), and `node.position` from the parser is `null` —
   * measured — because `react-markdown` strips positions, so every box resolved
   * to line 0 and ticking any of them ticked the first. The rendered boxes sit
   * in document order, which is parse order; asking where this one sits among
   * its siblings needs no bookkeeping and cannot drift.
   */
  const toggleFromDom = (input: HTMLInputElement) => {
    const root = input.closest(".note-rendered");
    if (!root) return;
    const nth = Array.from(root.querySelectorAll<HTMLInputElement>("input.note-check")).indexOf(input);
    const task = noteTasks(text)[nth];
    if (!task) return;
    change(toggleTask(text, task.line));
    // A note records that a task *is* done, never when — so the day it was
    // finished has to be written down as it happens or it is unknowable.
    record(sessionId, "tasksDone", task.done ? -1 : 1);
  };

  const components = useMemo(
    () => ({
      ...MARKDOWN_COMPONENTS,
      input(props: { type?: string; checked?: boolean }) {
        if (props.type !== "checkbox") return null;
        return (
          <input
            type="checkbox"
            checked={!!props.checked}
            // `react-markdown` renders task boxes disabled; a note is the one
            // place they must be live.
            onChange={(e) => toggleFromDom(e.currentTarget)}
            // **`mousedown`, not just `click`.** The note enters edit mode from
            // its own `onMouseDown`, which fires *before* `click` — so stopping
            // the click alone opened the editor underneath the tick, and the
            // box appeared to do nothing.
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
            className="note-check"
          />
        );
      },
    }),
    // The handler closes over `text`; a stale map would tick against an old note.
    [text], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Escape renders. A note in a narrow rail can be fiddly to click out of, and
    // there should always be a keyboard way back to reading it.
    if (e.key === "Escape") {
      rememberCaret();
      setEditing(false);
      return;
    }
    if (e.key !== "Enter" || e.shiftKey) return;

    // Continue a list rather than making people retype the punctuation. Without
    // it every task costs six keystrokes of syntax and people stop writing them.
    const area = e.currentTarget;
    const caret = area.selectionStart;
    const upto = area.value.slice(0, caret);
    const currentLine = upto.slice(upto.lastIndexOf("\n") + 1);
    const marker = continueList(currentLine);

    if (marker === null) {
      // An empty marker means "get me out of this list" — drop the bare bullet
      // rather than leaving punctuation nobody typed on the line.
      if (/^\s*([-*+]\s+(\[[ xX/]\]\s*)?|\d+[.)]\s+)$/.test(currentLine) && currentLine.trim()) {
        e.preventDefault();
        const lineStart = caret - currentLine.length;
        const next = `${area.value.slice(0, lineStart)}\n${area.value.slice(caret)}`;
        change(next);
        requestAnimationFrame(() => areaRef.current?.setSelectionRange(lineStart + 1, lineStart + 1));
      }
      return;
    }

    e.preventDefault();
    const next = `${area.value.slice(0, caret)}\n${marker}${area.value.slice(caret)}`;
    change(next);
    requestAnimationFrame(() => {
      const at = caret + 1 + marker.length;
      areaRef.current?.setSelectionRange(at, at);
    });
  };

  /** Drag the bottom edge. Persisted per session, like the note itself. */
  const startResize = (e: React.PointerEvent) => {
    e.preventDefault();
    const startY = e.clientY;
    const startH = height;
    const move = (ev: PointerEvent) =>
      // Measured against the *pointer*, not accumulated deltas: accumulation
      // drifts under the interface `zoom` this rail sits inside.
      setHeight(Math.max(MIN_HEIGHT, startH + (ev.clientY - startY)));
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      try {
        localStorage.setItem(
          heightKey(sessionId),
          String(Math.round(Math.max(MIN_HEIGHT, startH + (ev.clientY - startY)))),
        );
      } catch {
        /* a full localStorage must not break resizing */
      }
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        {progress.total > 0 ? (
          <TaskProgress done={progress.done} total={progress.total} />
        ) : (
          <span className="flex-1" />
        )}
        {/*
          An explicit way in and out, beside the automatic one. Clicking the note
          already edits it, but a control that *names* the state is what makes
          the two modes discoverable — without it, someone who does not think to
          click the text has no way of knowing the note is editable at all.
        */}
        <button
          type="button"
          className="micro shrink-0"
          style={{ color: editing ? "var(--text)" : "var(--text-faint)" }}
          // `mousedown` is where the editor's blur fires, so a plain `onClick`
          // on DONE would land after `editing` had already gone false and turn
          // it straight back on.
          onMouseDown={(e) => {
            e.preventDefault();
            if (editing) rememberCaret();
            setEditing((on) => !on);
          }}
          title={editing ? "Render the markdown" : "Edit the raw markdown"}
        >
          {editing ? "DONE" : "EDIT"}
        </button>
      </div>

      <div
        ref={scrollRef}
        onScroll={(e) => {
          scrollOf.set(sessionId, e.currentTarget.scrollTop);
        }}
        className="inset overflow-y-auto px-2 py-[6px]"
        style={{ border: "1px solid var(--border)", height, minHeight: MIN_HEIGHT }}
      >
        {editing ? (
          <textarea
            ref={areaRef}
            value={text}
            spellCheck={false}
            placeholder="staging URL, test account, - [ ] a task…"
            onChange={(e) => change(e.target.value)}
            onKeyDown={onKeyDown}
            // Clicking away renders it — the behaviour asked for, and the one
            // people already expect from an inline editor.
            onBlur={() => {
              rememberCaret();
              setEditing(false);
            }}
            className="data h-full w-full resize-none bg-transparent text-[11px] leading-relaxed outline-none"
            style={{ color: "var(--text)" }}
          />
        ) : (
          <div
            className="note-rendered markdown note-block data h-full text-[11px] leading-relaxed"
            // `preventDefault` stops the browser moving focus as part of this
            // gesture; the effect above takes it deliberately instead.
            onMouseDown={(e) => {
              e.preventDefault();
              // The line under the pointer, so the editor opens where the eye
              // already is rather than at the top or the end.
              const block = (e.target as HTMLElement).closest<HTMLElement>("[data-line]");
              const line = block ? Number(block.dataset.line) : NaN;
              enterAt.current = Number.isFinite(line) ? line : null;
              setEditing(true);
            }}
          >
            {text ? (
              <Markdown remarkPlugins={[remarkGfm]} components={components}>
                {text}
              </Markdown>
            ) : (
              <span style={{ color: "var(--text-faint)" }}>
                staging URL, test account, - [ ] a task…
              </span>
            )}
          </div>
        )}
      </div>

      {/* The grip is a real control rather than a CSS `resize` corner: `resize`
          cannot be read back, so the height could not be persisted, and a note
          that forgets its size on every session switch is worse than one that
          cannot be resized at all. */}
      <div
        onPointerDown={startResize}
        title="Drag to resize"
        className="h-[6px] w-full cursor-ns-resize"
        style={{ background: "var(--border)" }}
      />
    </div>
  );
}

/**
 * How much of this note is done.
 *
 * Shown only when the note *has* checkboxes — a bar reading 0/0 on a note of
 * prose is a control that means nothing, and a meter must report a measurement
 * rather than decorate.
 */
function TaskProgress({ done, total }: { done: number; total: number }) {
  const pct = total ? Math.round((done / total) * 100) : 0;
  const complete = done === total;
  return (
    <>
      <div className="h-[3px] flex-1" style={{ background: "var(--border)" }}>
        <div
          className="h-full"
          style={{
            width: `${pct}%`,
            // Amber for in-progress, plain text colour when finished. Red is
            // reserved for "the operator must act"; a half-done list is not an
            // alarm.
            background: complete ? "var(--text-soft)" : "rgb(var(--busy))",
            transition: "width var(--dur) var(--ease)",
          }}
        />
      </div>
      <span className="data text-[10px] tabular-nums" style={{ color: "var(--text-faint)" }}>
        {done}/{total}
      </span>
    </>
  );
}
