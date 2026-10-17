/**
 * The saved-password store.
 *
 * **The password is encrypted with the OS keychain, never written in the
 * clear.** Electron's `safeStorage` seals a string with a key held by the
 * platform credential store (Keychain on macOS, libsecret/DPAPI elsewhere), so
 * the file on disk is useless to anything that cannot already unlock the
 * account. That is the whole reason this feature does not just drop a JSON of
 * plaintext logins next to the annotations: a dev browser accumulates real
 * credentials, and a readable file of them is a breach waiting for a backup to
 * sync it somewhere.
 *
 * Two invariants make the encryption honest rather than decorative:
 *
 *   - **If `safeStorage` is unavailable, saving fails loudly** and nothing is
 *     written. Falling back to plaintext "so the feature still works" is the
 *     exact mistake this exists to prevent — the operator would believe their
 *     passwords are protected when they are lying on disk.
 *   - **Autofill matches on the *exact* origin.** A credential saved for
 *     `https://app.example.com` is never offered to any other origin, scheme or
 *     port. That is the one guard that stops a saved login being handed to a
 *     look-alike page.
 */
import { app, safeStorage } from 'electron'
import { readFile, writeFile, rename, mkdir } from 'node:fs/promises'
import { join, dirname } from 'node:path'
import type { CredentialMeta, CredentialSubmit, SavedCredential } from '../shared/contract'

/** One stored login. `enc` is base64 of the keychain-sealed password; the
 *  plaintext is never a field here. */
interface StoredEntry {
  origin: string
  username: string
  enc: string
  updatedAt: number
}

interface StoredFile {
  entries: StoredEntry[]
  /** Origins the operator answered "never" for — no save is offered on them. */
  never: string[]
}

/** A dev browser touches many sites; bounded like every other store here so a
 *  runaway cannot fill the disk or blow the JSON parse. */
const CAP = 1000

export class Passwords {
  private readonly path = join(app.getPath('userData'), 'passwords.json')
  private queue: Promise<void> = Promise.resolve()
  private cache: StoredFile | null = null

  /** Whether sealing is possible at all. When false the manager still lists and
   *  deletes, but saving is refused with a reason rather than done insecurely. */
  available(): boolean {
    try {
      return safeStorage.isEncryptionAvailable()
    } catch {
      return false
    }
  }

  private async load(): Promise<StoredFile> {
    if (this.cache) return this.cache
    try {
      const raw = await readFile(this.path, 'utf8')
      const parsed = JSON.parse(raw) as Partial<StoredFile>
      this.cache = {
        entries: Array.isArray(parsed.entries) ? parsed.entries.filter(isEntry) : [],
        never: Array.isArray(parsed.never) ? parsed.never.filter((s) => typeof s === 'string') : [],
      }
    } catch {
      this.cache = { entries: [], never: [] }
    }
    return this.cache
  }

  private persist(file: StoredFile): Promise<void> {
    this.cache = file
    this.queue = this.queue.then(() => this.write(file)).catch(() => {})
    return this.queue
  }

  private async write(file: StoredFile): Promise<void> {
    const trimmed: StoredFile = { entries: file.entries.slice(-CAP), never: file.never.slice(-CAP) }
    await mkdir(dirname(this.path), { recursive: true })
    const tmp = `${this.path}.tmp`
    await writeFile(tmp, JSON.stringify(trimmed), 'utf8')
    await rename(tmp, this.path)
  }

  private decrypt(entry: StoredEntry): string | null {
    try {
      return safeStorage.decryptString(Buffer.from(entry.enc, 'base64'))
    } catch {
      // A credential the current keychain cannot open — a different OS account,
      // a restored backup — is dropped from results rather than throwing. It
      // stays on disk; the operator can delete it from the manager.
      return null
    }
  }

  /* --------------------------------------------------------------- reads */

  async list(): Promise<CredentialMeta[]> {
    const file = await this.load()
    return file.entries
      .map((e) => ({ origin: e.origin, username: e.username, updatedAt: e.updatedAt }))
      .sort((a, b) => b.updatedAt - a.updatedAt)
  }

  /** Every saved login for an exact origin, decrypted, most-recent first. Used
   *  only to fill that same origin's own form. */
  async forOrigin(origin: string): Promise<SavedCredential[]> {
    if (!origin) return []
    const file = await this.load()
    const out: SavedCredential[] = []
    for (const e of file.entries) {
      if (e.origin !== origin) continue
      const password = this.decrypt(e)
      if (password === null) continue
      out.push({ origin: e.origin, username: e.username, password, updatedAt: e.updatedAt })
    }
    return out.sort((a, b) => b.updatedAt - a.updatedAt)
  }

  async reveal(origin: string, username: string): Promise<string | null> {
    const file = await this.load()
    const entry = file.entries.find((e) => e.origin === origin && e.username === username)
    return entry ? this.decrypt(entry) : null
  }

  async isBlocked(origin: string): Promise<boolean> {
    return (await this.load()).never.includes(origin)
  }

  /**
   * Does this submission differ from what is stored — i.e. is an offer worth
   * making? `null` means "do not offer" (blocked origin, encryption off, or an
   * identical password already saved).
   */
  async offerKind(sub: CredentialSubmit): Promise<'new' | 'update' | null> {
    if (!this.available()) return null
    if (!sub.origin || !sub.password) return null
    const file = await this.load()
    if (file.never.includes(sub.origin)) return null
    const existing = file.entries.find((e) => e.origin === sub.origin && e.username === sub.username)
    if (!existing) return 'new'
    return this.decrypt(existing) === sub.password ? null : 'update'
  }

  /* -------------------------------------------------------------- writes */

  /** Upsert. Throws when encryption is unavailable — the caller surfaces that
   *  rather than writing plaintext. */
  async save(sub: CredentialSubmit): Promise<void> {
    if (!this.available()) throw new Error('OS keychain encryption is unavailable; refusing to store a password in the clear')
    if (!sub.origin || !sub.password) return
    const file = await this.load()
    const enc = safeStorage.encryptString(sub.password).toString('base64')
    const at = Date.now()
    const idx = file.entries.findIndex((e) => e.origin === sub.origin && e.username === sub.username)
    const entry: StoredEntry = { origin: sub.origin, username: sub.username, enc, updatedAt: at }
    const entries = idx === -1 ? [...file.entries, entry] : file.entries.map((e, i) => (i === idx ? entry : e))
    // A save clears a prior "never" for the origin — the operator changed their
    // mind by choosing Save, and a lingering block would silently suppress the
    // next offer.
    await this.persist({ entries, never: file.never.filter((o) => o !== sub.origin) })
  }

  async remove(origin: string, username: string): Promise<void> {
    const file = await this.load()
    await this.persist({
      entries: file.entries.filter((e) => !(e.origin === origin && e.username === username)),
      never: file.never,
    })
  }

  async block(origin: string): Promise<void> {
    if (!origin) return
    const file = await this.load()
    if (file.never.includes(origin)) return
    await this.persist({ entries: file.entries, never: [...file.never, origin] })
  }
}

function isEntry(v: unknown): v is StoredEntry {
  const e = v as StoredEntry
  return !!e && typeof e.origin === 'string' && typeof e.username === 'string' && typeof e.enc === 'string'
}
