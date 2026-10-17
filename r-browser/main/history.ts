/**
 * Browsing history and bookmarks — what the address bar suggests from.
 *
 * A dev browser is reopened against the same servers and the same routes all
 * day; without this, every reopen starts from a blank address bar and the
 * operator retypes a URL the app has already seen a hundred times. So every full
 * navigation is remembered, and typing into the address bar ranks the matches by
 * **frecency** — frequency and recency together, the same signal a real browser
 * uses — with bookmarked addresses always on top.
 *
 * Neither store holds a secret, so (unlike `passwords.ts`) they are plain JSON
 * in the app's data directory. They are bounded and pruned on write for the same
 * reason every store here is: a history that only grows reads as fine until the
 * JSON parse gives out.
 */
import { app } from 'electron'
import { readFile, writeFile, rename, mkdir } from 'node:fs/promises'
import { join, dirname } from 'node:path'
import type { Bookmark, Suggestion } from '../shared/contract'

interface Visit {
  url: string
  title?: string
  visitCount: number
  lastVisit: number
}

const HISTORY_CAP = 5000
const BOOKMARK_CAP = 2000
const DAY = 24 * 60 * 60 * 1000

/** A tiny base to persist one keyed list as JSON, serialised through one write
 *  chain so overlapping saves cannot interleave into truncated JSON. */
abstract class JsonStore<T> {
  protected abstract readonly file: string
  private queue: Promise<void> = Promise.resolve()
  private cache: T[] | null = null

  private get path(): string {
    return join(app.getPath('userData'), this.file)
  }

  protected abstract valid(v: unknown): v is T

  protected async rows(): Promise<T[]> {
    if (this.cache) return this.cache
    try {
      const parsed: unknown = JSON.parse(await readFile(this.path, 'utf8'))
      this.cache = Array.isArray(parsed) ? parsed.filter((v): v is T => this.valid(v)) : []
    } catch {
      this.cache = []
    }
    return this.cache
  }

  protected save(rows: T[]): Promise<void> {
    this.cache = rows
    this.queue = this.queue.then(() => this.write(rows)).catch(() => {})
    return this.queue
  }

  private async write(rows: T[]): Promise<void> {
    await mkdir(dirname(this.path), { recursive: true })
    const tmp = `${this.path}.tmp`
    await writeFile(tmp, JSON.stringify(rows), 'utf8')
    await rename(tmp, this.path)
  }
}

export class History extends JsonStore<Visit> {
  protected readonly file = 'history.json'
  protected valid(v: unknown): v is Visit {
    return !!v && typeof (v as Visit).url === 'string'
  }

  /** Record a full navigation. An existing URL is bumped, not duplicated, so the
   *  count is the real number of visits and the address bar can rank by it. */
  async visit(url: string, title?: string): Promise<void> {
    if (!suggestable(url)) return
    const rows = await this.rows()
    const at = Date.now()
    const existing = rows.find((r) => r.url === url)
    if (existing) {
      existing.visitCount += 1
      existing.lastVisit = at
      if (title) existing.title = title
    } else {
      rows.push({ url, title, visitCount: 1, lastVisit: at })
    }
    // Prune the stalest when over the cap — oldest `lastVisit` goes first, which
    // keeps the frequently-returned-to even if they are not the newest.
    const kept = rows.length > HISTORY_CAP
      ? [...rows].sort((a, b) => b.lastVisit - a.lastVisit).slice(0, HISTORY_CAP)
      : rows
    await this.save(kept)
  }

  /** Update a title without counting a visit — `page-title-updated` fires after
   *  the navigation that already recorded one. */
  async retitle(url: string, title: string): Promise<void> {
    if (!title) return
    const rows = await this.rows()
    const existing = rows.find((r) => r.url === url)
    if (!existing || existing.title === title) return
    existing.title = title
    await this.save(rows)
  }

  async clear(): Promise<void> {
    await this.save([])
  }

  /**
   * Rank history for a typed query. Empty query returns the most-visited (the
   * "where do I usually go" an empty focused address bar should show); otherwise
   * substring match on URL or title, scored by frecency plus a boost when the
   * query matches the *start* of the host or URL — the match the operator most
   * likely means.
   */
  async suggest(query: string, limit: number): Promise<Suggestion[]> {
    const rows = await this.rows()
    const q = query.trim().toLowerCase()
    const now = Date.now()
    const frecency = (r: Visit) => r.visitCount + Math.max(0, 14 - (now - r.lastVisit) / DAY)

    if (!q) {
      return [...rows]
        .sort((a, b) => frecency(b) - frecency(a))
        .slice(0, limit)
        .map(toSuggestion)
    }

    const scored: Array<{ r: Visit; score: number }> = []
    for (const r of rows) {
      const url = r.url.toLowerCase()
      const title = (r.title ?? '').toLowerCase()
      if (!url.includes(q) && !title.includes(q)) continue
      let score = frecency(r)
      if (hostOrUrlStartsWith(r.url, q)) score += 60
      else if (url.startsWith(q)) score += 40
      scored.push({ r, score })
    }
    return scored
      .sort((a, b) => b.score - a.score)
      .slice(0, limit)
      .map((s) => toSuggestion(s.r))
  }
}

export class Bookmarks extends JsonStore<Bookmark> {
  protected readonly file = 'bookmarks.json'
  protected valid(v: unknown): v is Bookmark {
    return !!v && typeof (v as Bookmark).url === 'string'
  }

  async list(): Promise<Bookmark[]> {
    return [...(await this.rows())].sort((a, b) => b.at - a.at)
  }

  async has(url: string): Promise<boolean> {
    return (await this.rows()).some((b) => b.url === url)
  }

  async add(url: string, title?: string): Promise<void> {
    if (!suggestable(url)) return
    const rows = await this.rows()
    if (rows.some((b) => b.url === url)) return
    await this.save([...rows, { url, title, at: Date.now() }].slice(-BOOKMARK_CAP))
  }

  async remove(url: string): Promise<void> {
    const rows = await this.rows()
    await this.save(rows.filter((b) => b.url !== url))
  }

  /** Bookmarks that match a query (or all, for an empty one), newest first. The
   *  caller puts these ahead of history. */
  async suggest(query: string, limit: number): Promise<Suggestion[]> {
    const q = query.trim().toLowerCase()
    const rows = await this.list()
    const hit = q
      ? rows.filter((b) => b.url.toLowerCase().includes(q) || (b.title ?? '').toLowerCase().includes(q))
      : rows
    return hit.slice(0, limit).map((b) => ({ url: b.url, title: b.title, kind: 'bookmark' as const }))
  }
}

function toSuggestion(r: Visit): Suggestion {
  return { url: r.url, title: r.title, kind: 'history', visitCount: r.visitCount }
}

/** Only http(s) is worth suggesting — `about:blank`, `devtools://` and the like
 *  are noise in an address bar. */
function suggestable(url: string): boolean {
  return /^https?:\/\//i.test(url)
}

function hostOrUrlStartsWith(url: string, q: string): boolean {
  try {
    const host = new URL(url).host.toLowerCase()
    return host.startsWith(q) || host.replace(/^www\./, '').startsWith(q)
  } catch {
    return url.toLowerCase().startsWith(q)
  }
}
