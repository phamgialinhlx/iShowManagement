import { useEffect, useRef, useState } from 'react'
import type { Annotation } from '@shared/contract'
import { summarise, type PromptOptions } from '@shared/prompt'
import { Empty, OutcomeLabel } from './Bits'
import { IconCamera, IconCopy, IconEdit, IconImage, IconRuler, IconTrash } from './Icons'
import { hostOf } from '../lib/url'
import { useOutcome } from '../lib/hooks'

/**
 * NOTES — the panel the app exists for.
 *
 * The gesture this has to make fast is: **click an element → type what is wrong
 * → Enter → click the next element.** Everything here follows from that. A pick
 * appends a row already in edit state with its box focused, Enter commits and
 * Escape reverts, and there is no modal anywhere in the path — a dialog would
 * take the keystrokes and break the loop at exactly the point it has to flow.
 */
export function NotesPanel({
  annotations, selected, draftId, stale, options,
  onNote, onDelete, onDeleteMany, onReshoot, onRemeasure, onHover, onToggle,
  onSelectAll, onSelectNone, onOptions, onCopyPrompt, onCopyOne, onDraftHandled,
}: {
  annotations: Annotation[]
  selected: ReadonlySet<string>
  /** The row a pick just created: it opens in edit state with focus. */
  draftId: string | null
  /** Ids whose element no longer resolves. Kept as a *mark*, never a deletion —
   *  the note is still worth having after the page it described has changed. */
  stale: ReadonlySet<string>
  options: PromptOptions
  onNote: (id: string, note: string) => void
  onDelete: (id: string) => void
  /** Wipe several at once — the header's clear buttons. */
  onDeleteMany: (ids: string[]) => void
  onReshoot: (id: string) => Promise<void>
  onRemeasure: (id: string) => Promise<boolean>
  onHover: (id: string | null) => void
  onToggle: (id: string) => void
  onSelectAll: () => void
  onSelectNone: () => void
  onOptions: (next: PromptOptions) => void
  onCopyPrompt: () => Promise<string>
  onCopyOne: (a: Annotation) => Promise<string>
  onDraftHandled: () => void
}) {
  const [editing, setEditing] = useState<string | null>(null)
  const { outcome, run, busy } = useOutcome()

  // A draft opens itself. `draftId` is consumed rather than watched, so an edit
  // the operator abandons does not re-open every time this component renders.
  useEffect(() => {
    if (!draftId) return
    setEditing(draftId)
    onDraftHandled()
  }, [draftId, onDraftHandled])

  return (
    <div className="inspector-body">
      <div className="panel-bar">
        <button type="button" className="chip" onClick={onSelectAll}>all</button>
        <button type="button" className="chip" onClick={onSelectNone}>none</button>
        <span className="micro">{selected.size} / {annotations.length}</span>
        <span className="spacer" />
        {/* Wipe old comments. "delete selected" acts on the ticked rows;
            "clear all" empties the list. Both absent when there is nothing to
            act on, per the rule — a dead control invites "how do I use this". */}
        {selected.size > 0 && (
          <button
            type="button"
            className="chip chip-danger"
            title="delete the selected notes"
            onClick={() => onDeleteMany([...selected])}
          >
            <IconTrash size={10} /> delete{selected.size > 1 ? ` · ${selected.size}` : ''}
          </button>
        )}
        {annotations.length > 0 && (
          <button
            type="button"
            className="chip chip-danger"
            title="delete every note in the list"
            onClick={() => onDeleteMany(annotations.map(a => a.id))}
          >
            clear all
          </button>
        )}
      </div>

      <div className="scroll-list" onMouseLeave={() => onHover(null)}>
        {annotations.length === 0 && (
          <Empty
            phase="no notes yet"
            detail="press pick, then click something in the page"
          />
        )}
        {annotations.map((a, i) => (
          <Row
            key={a.id}
            index={i + 1}
            annotation={a}
            selected={selected.has(a.id)}
            stale={stale.has(a.id)}
            editing={editing === a.id}
            onEdit={() => setEditing(a.id)}
            onDoneEditing={() => setEditing(null)}
            onNote={note => onNote(a.id, note)}
            onDelete={() => onDelete(a.id)}
            onReshoot={() => onReshoot(a.id)}
            onRemeasure={() => onRemeasure(a.id)}
            onCopyOne={() => onCopyOne(a)}
            onHover={hovering => onHover(hovering ? a.id : null)}
            onToggle={() => onToggle(a.id)}
          />
        ))}
      </div>

      {/* The prompt controls sit below the list, where the eye ends up after
          reading it, and the options sit *beside* the button rather than behind
          a settings sheet — they change what the button produces. */}
      <div className="panel-bar" style={{ borderBottom: 0, borderTop: '1px solid var(--border)' }}>
        <label className="check">
          <input
            type="checkbox"
            checked={options.includeConsole}
            onChange={e => onOptions({ ...options, includeConsole: e.target.checked })}
          />
          console
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={options.includeNetwork}
            onChange={e => onOptions({ ...options, includeNetwork: e.target.checked })}
          />
          network
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={options.includeScreenshots}
            onChange={e => onOptions({ ...options, includeScreenshots: e.target.checked })}
          />
          screenshots
        </label>
      </div>
      <div className="panel-bar" style={{ borderBottom: 0 }}>
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy || selected.size === 0}
          onClick={() => void run('building', onCopyPrompt)}
        >
          copy prompt{selected.size > 1 ? ` · ${selected.size}` : ''}
        </button>
        <OutcomeLabel outcome={outcome} />
        {/*
          * Said once, beside the control, rather than enforced.
          *
          * Batching more than a handful is a real risk — prompt-level accuracy
          * falls off steeply as independent instructions multiply, and the
          * documented failure is silent omission rather than a wrong fix. But
          * the operator may have four notes about one component, which is one
          * edit; refusing to copy them would be the app overruling someone who
          * can see their own notes. Amber, because it is a caution and not an
          * error — red is reserved for "you must act".
          */}
        {selected.size > 4 && (
          <span className="micro warn-text">
            {selected.size} at once — agents commonly fix the first few and drop the rest. The list is
            numbered and the task asks for a per-item report, but copying them one at a time lands better.
          </span>
        )}
      </div>
    </div>
  )
}

function Row({
  index, annotation, selected, stale, editing,
  onEdit, onDoneEditing, onNote, onDelete, onReshoot, onRemeasure, onCopyOne, onHover, onToggle,
}: {
  index: number
  annotation: Annotation
  selected: boolean
  stale: boolean
  editing: boolean
  onEdit: () => void
  onDoneEditing: () => void
  onNote: (note: string) => void
  onDelete: () => void
  onReshoot: () => Promise<void>
  onRemeasure: () => Promise<boolean>
  onCopyOne: () => Promise<string>
  onHover: (hovering: boolean) => void
  onToggle: () => void
}) {
  const [draft, setDraft] = useState(annotation.note)
  const box = useRef<HTMLTextAreaElement | null>(null)
  const { outcome, run, busy } = useOutcome()

  useEffect(() => {
    if (editing) {
      setDraft(annotation.note)
      box.current?.focus()
      // Caret at the end, not over the text: re-opening an existing note to add
      // a sentence is the common case, and a selected value loses it to the
      // first keystroke.
      const len = annotation.note.length
      box.current?.setSelectionRange(len, len)
    }
  }, [editing, annotation.note])

  const commit = () => { onNote(draft.trim()); onDoneEditing() }

  return (
    <div
      className="note-row"
      onMouseEnter={() => onHover(true)}
      onMouseLeave={() => onHover(false)}
    >
      <div className="note-index col">
        <input type="checkbox" checked={selected} onChange={onToggle} />
        <span className="micro data">{index}</span>
      </div>

      <div className="note-main">
        <span className="micro data truncate" title={annotation.element?.selector}>
          {summarise(annotation)}
        </span>
        <span className="micro faint truncate">
          {hostOf(annotation.url)}
          {annotation.device ? ` · ${annotation.device}` : ''}
        </span>

        {stale && (
          /* Amber, not red: this is information about the page having moved on,
             not something the operator must act on. The note still stands. */
          <span className="micro note-stale">element no longer on the page</span>
        )}

        {editing ? (
          <textarea
            ref={box}
            className="field"
            placeholder="what is wrong with it?"
            value={draft}
            onChange={e => setDraft(e.target.value)}
            onBlur={commit}
            onKeyDown={e => {
              // Enter commits so the next pick can start immediately; Shift
              // gives a second line for the rare note that needs one.
              if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); commit() }
              if (e.key === 'Escape') { setDraft(annotation.note); onDoneEditing() }
            }}
          />
        ) : (
          <span
            className="note-text selectable"
            onDoubleClick={onEdit}
          >
            {annotation.note || <span className="micro faint">empty — double-click to write it</span>}
          </span>
        )}

        {annotation.edits && annotation.edits.length > 0 && (
          /* The live changes made in the browser's edit mode. Shown as a compact
             before→after list so the operator can see the prompt will carry them
             — this is the "make it look like this" half of the note. */
          <div className="note-edits">
            {annotation.edits.map((e, i) => (
              <span key={i} className="micro note-edit" title={`${e.from} → ${e.to}`}>
                <span className="faint">{e.prop === 'text' ? 'text' : e.prop}</span>
                {' '}
                <span className="truncate">{e.to}</span>
              </span>
            ))}
          </div>
        )}

        {annotation.screenshot && (
          <img
            className="note-shot"
            alt=""
            src={`data:image/png;base64,${annotation.screenshot}`}
          />
        )}

        <div className="note-actions">
          {/*
            * Copy just this one. First in the row because it is the action most
            * likely to be wanted: an agent given several items drops the later
            * ones, so one note at a time is the shape that lands.
            */}
          <button
            type="button"
            className="chip"
            title="copy a prompt for only this note"
            disabled={busy}
            onClick={() => void run('building', onCopyOne)}
          >
            <IconCopy size={10} /> copy
          </button>
          <button type="button" className="chip" title="edit note" onClick={onEdit}>
            <IconEdit size={10} /> edit
          </button>
          <button
            type="button"
            className="chip"
            title="re-shoot"
            disabled={busy}
            onClick={() => void run('shooting', async () => { await onReshoot(); return 'shot' })}
          >
            <IconCamera size={10} />
          </button>
          <button
            type="button"
            className="chip"
            title="re-measure — does this element still resolve?"
            disabled={busy}
            onClick={() => void run('measuring', async () => {
              const found = await onRemeasure()
              return found ? 'found' : 'gone'
            })}
          >
            <IconRuler size={10} />
          </button>
          {/*
            * Copying the *image* is its own button, and it only exists on a note
            * that has one — absent, never disabled, per rule.
            *
            * It is separate from COPY PROMPT because a clipboard holds one thing
            * at a time: text or an image, not both. The earlier build papered
            * over that by writing "[screenshot attached]" into the prompt and
            * attaching nothing, which is a claim the app could not honour. The
            * prompt now carries the PNG's *path* — which is what an agent in a
            * terminal can actually open — and this button is for pasting into a
            * chat UI that takes images instead.
            */}
          {annotation.screenshot && (
            <button
              type="button"
              className="chip"
              title="copy the image itself to the clipboard"
              disabled={busy}
              onClick={() => void run('copying', async () => {
                await window.rbrowse.clipboard.image(annotation.screenshot ?? '')
                return 'image copied'
              })}
            >
              <IconImage size={10} />
            </button>
          )}
          <button type="button" className="chip chip-danger" title="delete this note" onClick={onDelete}>
            <IconTrash size={10} /> delete
          </button>
          <OutcomeLabel outcome={outcome} />
        </div>
      </div>
    </div>
  )
}
