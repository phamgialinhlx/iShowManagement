import { useCallback, useEffect, useState } from 'react'
import type { CredentialMeta } from '@shared/contract'
import { Empty, OutcomeLabel } from './Bits'
import { IconCopy, IconTrash } from './Icons'
import { useOutcome } from '../lib/hooks'

/**
 * The saved-password manager — one inspector tab.
 *
 * It lists what is stored (origin and username, never the secret), and reveals
 * or copies one password only on an explicit click. That asymmetry is the point:
 * the renderer never holds the whole set of plaintext logins, so a compromised
 * page-side bug cannot read them out of it — each reveal is a fresh request to
 * main, for one entry, that the operator asked for.
 *
 * Reveal is per-row and toggles back to dots; deletion is immediate (there is
 * no undo, and a confirm dialog over a native page view is exactly the overlay
 * that cannot be drawn here), so its button is the quiet-until-hovered danger
 * chip rather than a primary.
 */
export function PasswordsPanel() {
  const [items, setItems] = useState<CredentialMeta[] | null>(null)
  const [shown, setShown] = useState<Record<string, string>>({})
  const { outcome, run } = useOutcome()

  const refresh = useCallback(async () => {
    setItems(await window.rbrowse.passwords.list())
  }, [])

  useEffect(() => { void refresh() }, [refresh])

  const keyOf = (m: CredentialMeta) => `${m.origin}\u0000${m.username}`

  const reveal = async (m: CredentialMeta) => {
    const k = keyOf(m)
    if (shown[k] !== undefined) {
      setShown(prev => { const n = { ...prev }; delete n[k]; return n })
      return
    }
    const pw = await window.rbrowse.passwords.reveal(m.origin, m.username)
    setShown(prev => ({ ...prev, [k]: pw ?? '(could not decrypt)' }))
  }

  if (!items) return <Empty phase="reading saved passwords" />
  if (items.length === 0) {
    return <Empty phase="no saved passwords" detail="sign in to a site and choose save" />
  }

  return (
    <div className="inspector-body">
      <div className="panel-bar">
        <span className="micro">{items.length} saved</span>
        <span className="spacer" />
        <OutcomeLabel outcome={outcome} />
      </div>

      <div className="scroll-list">
        {items.map(m => {
          const k = keyOf(m)
          const revealed = shown[k]
          return (
            <div key={k} className="pw-row">
              <div className="pw-main">
                <span className="micro data truncate" title={m.origin}>{originLabel(m.origin)}</span>
                <span className="micro faint truncate">{m.username || '(no username)'}</span>
                <span className="pw-secret micro selectable">
                  {revealed !== undefined ? revealed : '••••••••••'}
                </span>
              </div>
              <div className="pw-actions">
                <button
                  type="button"
                  className="chip"
                  title={revealed !== undefined ? 'hide' : 'reveal'}
                  onClick={() => void reveal(m)}
                >
                  {revealed !== undefined ? 'hide' : 'show'}
                </button>
                <button
                  type="button"
                  className="chip"
                  title="copy the password"
                  onClick={() => void run('copying', async () => {
                    const pw = await window.rbrowse.passwords.reveal(m.origin, m.username)
                    if (pw === null) throw new Error('could not decrypt')
                    await window.rbrowse.clipboard.text(pw)
                    return 'copied'
                  })}
                >
                  <IconCopy size={10} />
                </button>
                <button
                  type="button"
                  className="chip chip-danger"
                  title="delete this saved password"
                  onClick={() => void run('deleting', async () => {
                    await window.rbrowse.passwords.remove(m.origin, m.username)
                    await refresh()
                    return 'deleted'
                  })}
                >
                  <IconTrash size={10} />
                </button>
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

function originLabel(origin: string): string {
  try {
    return new URL(origin).host || origin
  } catch {
    return origin
  }
}
