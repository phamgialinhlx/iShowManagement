import type { CredentialOffer } from '@shared/contract'
import { OutcomeLabel } from './Bits'
import { useOutcome } from '../lib/hooks'

/**
 * "Save this password?" — raised when a login is submitted on a page.
 *
 * It lives in the chrome (a strip below the toolbar), never over the page: the
 * page is a native view painted on top, so a bar drawn in its rectangle would
 * be invisible. Sitting in the chrome it shrinks the page slot instead, the
 * same arrangement the device bar uses.
 *
 * It carries no password — main holds the plaintext against the tab and Save
 * merely names the tab. The three answers are Save, Not now (dismiss until the
 * next submit), and Never (block this origin for good). Save reports its own
 * outcome inline, because it is the one that can fail: with no OS keychain
 * available, storing a password is refused rather than done in the clear, and
 * the operator has to be told that instead of believing it was saved.
 */
export function SavePasswordBar({
  offer, onSave, onNever, onDismiss,
}: {
  offer: CredentialOffer
  onSave: () => Promise<{ ok: boolean; error?: string }>
  onNever: () => void
  onDismiss: () => void
}) {
  const { outcome, run, busy } = useOutcome()

  const host = originLabel(offer.origin)
  const who = offer.username ? ` for ${offer.username}` : ''
  const lead = offer.kind === 'update'
    ? `Update the saved password${who} on ${host}?`
    : `Save the password${who} on ${host}?`

  return (
    <div className="password-bar">
      <span className="password-bar-text micro">{lead}</span>
      <div className="password-bar-actions">
        <button
          type="button"
          className="chip"
          disabled={busy}
          onClick={() => void run('saving', async () => {
            const r = await onSave()
            if (!r.ok) throw new Error(r.error ?? 'could not save')
            return 'saved'
          })}
        >
          save
        </button>
        <button type="button" className="chip" disabled={busy} onClick={onDismiss}>
          not now
        </button>
        <button type="button" className="chip chip-danger" disabled={busy} onClick={onNever}>
          never for this site
        </button>
        <OutcomeLabel outcome={outcome} />
      </div>
    </div>
  )
}

/** `https://app.example.com:8443` → `app.example.com` — the part the operator
 *  recognises. The full origin is what actually keys storage; this is only the
 *  label. */
function originLabel(origin: string): string {
  try {
    return new URL(origin).host || origin
  } catch {
    return origin
  }
}
