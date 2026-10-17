/**
 * The annotation store.
 *
 * **On disk in the app's user-data directory, not in `localStorage`.** A single
 * element screenshot is around a megabyte of base64, and `localStorage` is one
 * shared quota per origin — so a morning's annotations would start failing to
 * save with no error anyone sees, and the failure would take the tab list with
 * it. rmux learned this with its background picture: a wallpaper stored as a
 * data URL cost someone their session list to gain an image.
 */
import { app } from 'electron'
import { readFile, writeFile, rename, mkdir } from 'node:fs/promises'
import { join, dirname } from 'node:path'
import type { Annotation } from '../shared/contract'

/**
 * How many annotations are kept.
 *
 * Bounded because everything here is: the list is fed by a gesture cheap enough
 * to repeat a hundred times in an afternoon, and each entry can carry a
 * megabyte of PNG. An unbounded store reads as bounded right up until the disk
 * or the JSON parse gives out, which is the same failure shape the activity
 * store's `hours` bucket had.
 */
const CAP = 500

export class Store {
  private readonly path = join(app.getPath('userData'), 'annotations.json')
  private queue: Promise<void> = Promise.resolve()

  async get(): Promise<Annotation[]> {
    try {
      const raw = await readFile(this.path, 'utf8')
      const parsed: unknown = JSON.parse(raw)
      if (!Array.isArray(parsed)) return []
      // Never bind strictly to our own past schema either: an annotation
      // written by an older build must survive a field being added, or an
      // upgrade silently empties the operator's notes.
      return parsed.filter((a): a is Annotation =>
        !!a && typeof a === 'object' && typeof (a as Annotation).id === 'string')
    } catch {
      // Absent is the ordinary first-run case and is not an error.
      return []
    }
  }

  /**
   * Writes are serialised through one chain.
   *
   * The chrome saves on a 400ms debounce and the operator types faster than
   * that during a burst of notes; two overlapping writes to the same path
   * interleave and leave truncated JSON, which reads on next launch as "all my
   * notes are gone".
   */
  set(annotations: Annotation[]): Promise<void> {
    this.queue = this.queue.then(() => this.write(annotations)).catch(() => {})
    return this.queue
  }

  private async write(annotations: Annotation[]): Promise<void> {
    const kept = annotations.slice(-CAP)
    await mkdir(dirname(this.path), { recursive: true })
    const tmp = `${this.path}.tmp`
    await writeFile(tmp, JSON.stringify(kept), 'utf8')
    // Write-then-rename, so a crash mid-write leaves the previous file intact
    // rather than a half-written one. This is *our* file in *our* data
    // directory — the repo's "never `mv` onto the original" rule is about
    // editing a user's own files over ssh, where a replaced inode discards
    // permissions, ownership and hard links. Neither applies here.
    await rename(tmp, this.path)
  }
}
