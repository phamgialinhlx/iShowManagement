/**
 * The rmux control client — main process only.
 * ============================================
 *
 * This is the other end of `crates/rmux-control`: newline-delimited JSON over a
 * unix socket, with a token read from a `0600` handshake file before the socket
 * is ever opened. Everything here mirrors `protocol.rs` field for field. A name
 * spelled differently does not fail loudly on the Rust side — `serde` rejects
 * the line, the server answers "expected a request" against id 0, and our
 * pending promise simply times out. So the wire shapes below are copied, not
 * remembered, and `checks/` asserts them against the Rust tests' expectations.
 *
 * **Main process, never the renderer.** The socket can open ssh tunnels into
 * the operator's infrastructure and the handshake file holds a bearer token;
 * neither belongs anywhere a page could reach. The renderer sees `ControlState`
 * and nothing else.
 *
 * **rmux being absent is the ordinary case.** R browser is a browser; rmux is
 * an optional backend that makes one panel work. Nothing here throws when the
 * handshake file is missing — `connect()` resolves with `connected: false` and
 * a reason, and a slow timer looks again.
 */

import { createConnection, type Socket } from 'node:net'
import { promises as fs } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

import type { Annotation, ConsoleEntry, ControlState, Viewport } from '../shared/contract.js'

/**
 * `rmux_control::protocol::VERSION`.
 *
 * Copied deliberately rather than negotiated: rmux and R browser ship
 * separately and *will* drift, and the Rust comment is explicit that a client
 * must surface a mismatch as a clear message rather than let it deserialise to
 * a default. Bumping this without reading `protocol.rs` is how a client starts
 * lying about which contract it speaks.
 */
export const PROTOCOL_VERSION = 1

/** A session as `SessionInfo` in `protocol.rs` defines it. `host` is absent —
 *  never `"localhost"` — for a session on this machine; the Rust test
 *  `a_local_session_omits_the_host_rather_than_inventing_one` pins that, and a
 *  client that invented a host might try to ssh to it. */
export interface SessionInfo {
  id: string
  name: string
  host?: string
  folder: string
}

/* ------------------------------------------------------------- reconnecting */

const BACKOFF_FLOOR_MS = 1_000
const BACKOFF_CEILING_MS = 30_000
/** How long a connection must last before it counts as having *worked*. */
export const HEALTHY_MS = 30_000
/** rmux is not installed or not running: look again on a slow timer, never a
 *  retry storm. Nothing is wrong, so nothing should be busy. */
const ABSENT_RETRY_MS = 30_000
const REQUEST_TIMEOUT_MS = 10_000

/**
 * How long to wait before redialling.
 *
 * Pure — no clock, no socket, no `this` — precisely so it can be asserted
 * without standing a server up. The repo has already paid for the impure
 * version of this decision once: the Redstone bridge reset its backoff on every
 * *clean* close, and a server that rejects a credential also closes cleanly, so
 * a revoked host redialled every second forever against production. The lesson
 * is written into the signature: a caller must say which of three things
 * happened, and there is no outcome that means "it ended politely".
 *
 * - `healthy` — the connection stayed up for {@link HEALTHY_MS}. That, and only
 *   that, earns a reset. Lasting is evidence; closing tidily is not.
 * - `brief` — it dropped sooner than that. Double, up to the ceiling.
 * - `rejected` — the handshake was refused (bad token, version mismatch).
 *   Straight to the ceiling: retrying a refused credential faster does not make
 *   it more acceptable, and a tight loop against an auth check is how a client
 *   gets itself banned.
 */
export function nextBackoff(
  previousMs: number,
  outcome: 'healthy' | 'brief' | 'rejected',
): number {
  if (outcome === 'rejected') return BACKOFF_CEILING_MS
  if (outcome === 'healthy') return BACKOFF_FLOOR_MS
  const doubled = Math.max(BACKOFF_FLOOR_MS, previousMs) * 2
  return Math.min(doubled, BACKOFF_CEILING_MS)
}

/* --------------------------------------------------------------- handshake */

interface Handshake {
  socket: string
  token: string
  version?: number
}

/**
 * Where the Rust side writes `control.json`.
 *
 * `rmux_control::runtime_dir()` is `dirs::runtime_dir()` falling back to
 * `dirs::home_dir()`, then `.rmux`. On macOS `runtime_dir()` is `None`, so it
 * is `~/.rmux`; on Linux it is usually `$XDG_RUNTIME_DIR/.rmux`. We check both
 * rather than branching on platform — the *fallback* is what differs between
 * machines, not the platform, and a client that guesses wrong reports "rmux is
 * not running" about an rmux sitting right there.
 */
function handshakeCandidates(): string[] {
  // An explicit override, checked first. It exists so `control-check.ts` can
  // drive this client against a real unix socket rather than a stubbed one —
  // the bug this file's `handshaking` flag documents was an *ordering* bug
  // across three round trips, which no mock of a socket would have caught and
  // which a real one caught on the first run. It doubles as the way to point
  // at a second rmux, which is otherwise impossible: every path here is fixed.
  const override = process.env['RB_CONTROL_HANDSHAKE']
  if (override) return [override]

  const paths: string[] = []
  const xdg = process.env['XDG_RUNTIME_DIR']
  if (xdg) paths.push(join(xdg, '.rmux', 'control.json'))
  paths.push(join(homedir(), '.rmux', 'control.json'))
  return paths
}

class Refused extends Error {}

/**
 * Read the token and socket path, or say exactly why not.
 *
 * Two failure kinds, and they are not the same thing: *absent* is normal and
 * means rmux is not running, while *refused* means the file is there and we
 * will not touch it. The Rust side sets `0600` in a `0700` directory **before**
 * writing the token, so group or other bits on this file mean something else
 * has been at it. A credential path that quietly tolerates a bad mode is not a
 * credential path — the same rule `~/.rmux/redstone.json` follows.
 */
async function readHandshake(): Promise<Handshake> {
  const candidates = handshakeCandidates()
  for (const path of candidates) {
    let stat
    try {
      stat = await fs.stat(path)
    } catch {
      continue
    }
    // eslint-disable-next-line no-bitwise
    if ((stat.mode & 0o077) !== 0) {
      const mode = (stat.mode & 0o777).toString(8)
      throw new Refused(
        `${path} is mode 0${mode}; it holds rmux's control token and must be 0600. ` +
          `Refusing to read it — restart rmux to have it rewritten.`,
      )
    }
    const raw = await fs.readFile(path, 'utf8')
    const parsed = JSON.parse(raw) as Partial<Handshake>
    if (!parsed.socket || !parsed.token) {
      throw new Refused(`${path} is missing "socket" or "token"`)
    }
    return { socket: parsed.socket, token: parsed.token, version: parsed.version }
  }
  throw new Error(`rmux is not running (looked in ${candidates.join(' and ')})`)
}

/* ------------------------------------------------------------------- client */

type Pending = {
  resolve: (value: Record<string, unknown>) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
}

export class Control {
  private socket: Socket | null = null
  private buffer = ''
  private nextId = 1
  private pending = new Map<number, Pending>()

  private current: ControlState = { connected: false, sessions: [] }
  /**
   * True for the whole of `greet`, not just the `Hello` frame.
   *
   * The handshake is **three** requests, not one: `hello`, then `listSessions`
   * and `activeSession`, because a client that connects and then renders an
   * empty session list until some later event is a client that looks broken on
   * launch. But `connected` is only published once all three have answered — so
   * a guard keyed on `connected` rejected the second and third with "not
   * connected to rmux", the catch reported that as the connection failure, and
   * **R browser never connected to a working rmux at all**. It reproduced
   * outside Electron in one run; from the UI it was indistinguishable from rmux
   * being closed, which is the ordinary case and therefore the one nobody
   * questions. `checks/control-check.ts` pins the ordering.
   */
  private handshaking = false

  /** Set once `disconnect()` or a `shuttingDown` event has been seen. A stopped
   *  client never redials — the Rust comment on `Event::ShuttingDown` says
   *  clients should stop rather than reconnect-loop, and an rmux that is quitting
   *  would otherwise be hammered by every browser window on the machine. */
  private stopped = false
  private backoffMs = BACKOFF_FLOOR_MS
  private retry: ReturnType<typeof setTimeout> | null = null
  private connectedAt = 0
  /** Resolves the first `connect()` call. Later reconnects report through
   *  `onState` instead — a promise can only settle once, and the caller has
   *  long since moved on. */
  private firstConnect: ((state: ControlState) => void) | null = null

  constructor(
    private readonly onState: (state: ControlState) => void,
    private readonly onOpenUrl: (p: { url: string; sessionId: string; focus: boolean }) => void,
  ) {}

  state(): ControlState {
    return this.current
  }

  /**
   * Resolves with the first state, connected or not.
   *
   * It does **not** throw when rmux is absent. A browser that fails to start
   * because a development tool is closed is a broken browser; the contract's
   * own words are that rmux being closed "must degrade one panel, not the app".
   */
  connect(): Promise<ControlState> {
    this.stopped = false
    return new Promise((resolve) => {
      this.firstConnect = resolve
      void this.dial()
    })
  }

  disconnect(): void {
    this.stopped = true
    if (this.retry) {
      clearTimeout(this.retry)
      this.retry = null
    }
    this.teardown('disconnected')
  }

  /* ------------------------------------------------------------- requests */

  async listSessions(): Promise<SessionInfo[]> {
    const r = await this.request({ method: 'listSessions' })
    return (r['sessions'] as SessionInfo[] | undefined) ?? []
  }

  async activeSession(): Promise<SessionInfo | null> {
    const r = await this.request({ method: 'activeSession' })
    return (r['session'] as SessionInfo | null | undefined) ?? null
  }

  async activate(id: string): Promise<void> {
    await this.request({ method: 'activate', id })
  }

  /** The SOCKS port to point a Chromium partition at, as
   *  `socks5h://127.0.0.1:{port}`. **`socks5h`, not `socks5`** — the `h` sends
   *  hostnames to the far side to resolve, which is the whole reason this beats
   *  a forwarded port. Without it an internal name is looked up on *this*
   *  machine, fails, and the proxy looks broken while working perfectly. */
  async openProxy(session: string): Promise<number> {
    const r = await this.request({ method: 'openProxy', session })
    const port = r['port']
    if (typeof port !== 'number') throw new Error('openProxy returned no port')
    return port
  }

  /**
   * `ssh -L port:localhost:port`, opened by rmux rather than by us.
   *
   * `Response::Forwarded` carries `ok` *and* an `error`, and a failed forward
   * answers with `ok: false` rather than `Response::Error` — so reading only the
   * port here would report success for every collision. The usual cause is that
   * the local port is already taken, which the operator can act on only if told.
   */
  async forwardPort(session: string, port: number): Promise<number> {
    const r = await this.request({ method: 'forwardPort', session, port })
    if (r['ok'] === false) {
      throw new Error((r['error'] as string | undefined) ?? `could not forward port ${port}`)
    }
    const got = r['port']
    return typeof got === 'number' ? got : port
  }

  async discoverPorts(session: string): Promise<number[]> {
    const r = await this.request({ method: 'discoverPorts', session })
    return (r['ports'] as number[] | undefined) ?? []
  }

  /* -------------------------------------------------------------- reports */

  /**
   * The one that closes the loop: a selector, the page it was on, and what the
   * operator typed, filed against a session.
   *
   * Two mappings worth stating, because both are silent when wrong:
   *
   * - The Rust field is **`note`**, not `comment` or `message`.
   * - The contract's `Viewport.deviceScaleFactor` is the wire's
   *   **`devicePixelRatio`**. Same number, different name on each side; sending
   *   ours verbatim drops the ratio and a 2560px shot of a 1280px viewport then
   *   reads as a desktop layout.
   *
   * `selector` is required by the wire. A note about the page as a whole has no
   * element, and `:root` is the honest answer there — it resolves, and it says
   * "the document" rather than naming a node the operator never pointed at.
   */
  async reportSelection(session: string, a: Annotation): Promise<void> {
    await this.request({
      method: 'report',
      session,
      report: 'selection',
      url: a.url,
      selector: a.element?.selector ?? ':root',
      // Omitted entirely rather than sent as null — a Rust test asserts an
      // absent optional does not appear in the JSON at all, and `omit` exists
      // so no call site has to remember that.
      ...omit('text', a.element?.text),
      ...omit('note', a.note),
      ...omit('screenshot', a.screenshot),
      ...omit('viewport', wireViewport(a.viewport)),
    })
  }

  /** `png` is base64 with **no `data:` prefix**. Anything that hands this a
   *  canvas `toDataURL()` result must strip the header first, or rmux stores a
   *  string that is not a PNG and nothing says so until someone opens it. */
  async reportScreenshot(
    session: string,
    url: string,
    png: string,
    viewport?: Viewport,
  ): Promise<void> {
    await this.request({
      method: 'report',
      session,
      report: 'screenshot',
      url,
      png,
      ...omit('viewport', wireViewport(viewport)),
    })
  }

  /** The wire's `ConsoleEntry` is `{level, text, at?}` and nothing else, while
   *  ours carries an id, a source and a location. The extras are dropped rather
   *  than smuggled — except the location, which is folded into the text, because
   *  "TypeError: x is undefined" without a file is a line nobody can act on. */
  async reportConsole(session: string, url: string, entries: ConsoleEntry[]): Promise<void> {
    await this.request({
      method: 'report',
      session,
      report: 'console',
      url,
      entries: entries.map((e) => ({
        level: e.level,
        text: e.url ? `${e.text} (${e.url}${e.line ? `:${e.line}` : ''})` : e.text,
        ...omit('at', e.at),
      })),
    })
  }

  /** HAR travels as a **string**, not as parsed JSON: it is a large, evolving
   *  schema owned by someone else, and binding to it would turn every spec
   *  revision into "the network report is empty". */
  async reportHar(session: string, url: string, har: string): Promise<void> {
    await this.request({ method: 'report', session, report: 'har', url, har })
  }

  async reportViewport(session: string, url: string, viewport: Viewport): Promise<void> {
    await this.request({
      method: 'report',
      session,
      report: 'viewport',
      url,
      viewport: wireViewport(viewport) ?? { width: 0, height: 0 },
    })
  }

  /* ------------------------------------------------------------ transport */

  private async dial(): Promise<void> {
    if (this.stopped) return

    let handshake: Handshake
    try {
      handshake = await readHandshake()
    } catch (e) {
      const refused = e instanceof Refused
      // Absent is normal; refused is a credential problem worth shouting about.
      // Neither is worth a fast retry, and the absent case must not become one:
      // a browser open all day would otherwise poll a missing file forever.
      this.publish({ connected: false, sessions: [], error: message(e) })
      this.settleFirst()
      if (refused) this.backoffMs = nextBackoff(this.backoffMs, 'rejected')
      this.scheduleRetry(refused ? this.backoffMs : ABSENT_RETRY_MS)
      return
    }

    const socket = createConnection(handshake.socket)
    this.socket = socket
    this.buffer = ''

    socket.setEncoding('utf8')
    socket.on('data', (chunk: string) => this.ingest(chunk))
    socket.on('error', (e) => this.dropped(message(e)))
    socket.on('close', () => this.dropped('rmux closed the control socket'))
    socket.once('connect', () => {
      this.connectedAt = Date.now()
      // The token never reaches a log line, an error message or a state object.
      // `ps` and logs are the two places credentials leak, and this one opens
      // ssh tunnels.
      void this.greet(handshake.token)
    })
  }

  private async greet(token: string): Promise<void> {
    this.handshaking = true
    try {
      const r = await this.request({ method: 'hello', token, client: 'rbrowse' }, true)
      const version = r['version']
      if (version !== PROTOCOL_VERSION) {
        // A clear message, not a silent default. rmux and R browser ship
        // separately, so "yours is older than the backend" is a normal Tuesday —
        // and the only thing that makes it diagnosable is saying both numbers.
        throw new Refused(
          `rmux speaks control protocol ${String(version)}, R browser speaks ` +
            `${PROTOCOL_VERSION}. Update whichever is older.`,
        )
      }
      const sessions = await this.listSessions()
      const active = await this.activeSession()
      this.publish({
        connected: true,
        app: (r['app'] as string | undefined) ?? 'rmux',
        version: PROTOCOL_VERSION,
        sessions,
        ...(active ? { activeSessionId: active.id } : {}),
      })
      this.settleFirst()
    } catch (e) {
      // A refused handshake goes to the ceiling and stays there. The socket is
      // closed first so `dropped` does not then compute a *brief* outcome and
      // halve the wait we just decided on.
      const refused = e instanceof Refused || /bad token|say hello first/i.test(message(e))
      this.publish({ connected: false, sessions: [], error: message(e) })
      this.settleFirst()
      this.teardown(message(e))
      this.backoffMs = nextBackoff(this.backoffMs, refused ? 'rejected' : 'brief')
      if (refused) console.error(`[control] handshake refused: ${message(e)}`)
      this.scheduleRetry(this.backoffMs)
    } finally {
      // `finally`, so a throw on any of the three requests lowers it too —
      // otherwise a failed handshake leaves the guard permanently open and the
      // next stray call goes out on a socket that is closing.
      this.handshaking = false
    }
  }

  /**
   * Split on newlines and route every line.
   *
   * TCP-shaped delivery: a chunk is not a message. One line can arrive in three
   * reads and three lines in one, and a screenshot report is large enough to
   * make the first case the *normal* one rather than the exotic one — which is
   * why the trailing partial is kept in `buffer` rather than parsed.
   */
  private ingest(chunk: string): void {
    this.buffer += chunk
    let cut = this.buffer.indexOf('\n')
    while (cut !== -1) {
      const line = this.buffer.slice(0, cut).trim()
      this.buffer = this.buffer.slice(cut + 1)
      cut = this.buffer.indexOf('\n')
      if (!line) continue
      let msg: Record<string, unknown>
      try {
        msg = JSON.parse(line) as Record<string, unknown>
      } catch {
        // A malformed line is rmux's problem, not a reason to tear down a
        // working connection and lose every other session update on it.
        console.error('[control] unparseable line from rmux')
        continue
      }
      this.route(msg)
    }
  }

  /**
   * Demultiplex on the presence of `id`, exactly as `protocol.rs` pins it.
   *
   * A response carries the id of its request; an event has none. Getting this
   * backwards does not error — it resolves a *random* pending request with
   * unrelated data, so `listSessions()` returns a session-activation event and
   * the panel renders nonsense with nothing in the log.
   */
  private route(msg: Record<string, unknown>): void {
    const id = msg['id']
    if (typeof id === 'number') {
      const pending = this.pending.get(id)
      if (!pending) return // Already timed out; its promise is settled.
      this.pending.delete(id)
      clearTimeout(pending.timer)
      if (msg['result'] === 'error') {
        pending.reject(new Error((msg['message'] as string | undefined) ?? 'rmux returned an error'))
      } else {
        pending.resolve(msg)
      }
      return
    }
    this.event(msg)
  }

  private event(msg: Record<string, unknown>): void {
    const session = msg['session'] as SessionInfo | undefined
    switch (msg['event']) {
      // The whole session travels with these three, not just an id — so the
      // list is updated in place and the panel never renders the previous
      // session's name under the new session's tabs.
      case 'sessionActivated':
        if (session) this.publish({ ...this.upsert(session), activeSessionId: session.id })
        break
      case 'sessionCreated':
      case 'sessionRenamed':
        if (session) this.publish(this.upsert(session))
        break
      case 'sessionClosed': {
        // `sessionClosed` is the one event carrying an `id` — the *closed
        // session's* id, a string, not a request id. It reaches here at all only
        // because the router keys on `typeof id === 'number'`; keying on the
        // key's presence would have filed every close as a response to whichever
        // request happened to be pending.
        const id = msg['id']
        const closed = typeof id === 'string' ? id : undefined
        if (!closed) break
        const sessions = this.current.sessions.filter((s) => s.id !== closed)
        const next: ControlState = { ...this.current, sessions }
        if (this.current.activeSessionId === closed) delete next.activeSessionId
        this.publish(next)
        break
      }
      case 'openUrl': {
        const url = msg['url']
        const forSession = msg['session']
        if (typeof url === 'string' && typeof forSession === 'string') {
          // Opened in *that session's* partition, where the SOCKS proxy applies
          // and `localhost:3000` means the server's localhost. Anywhere else and
          // the URL resolves against this machine.
          this.onOpenUrl({ url, sessionId: forSession, focus: msg['focus'] === true })
        }
        break
      }
      case 'shuttingDown':
        // rmux is quitting. Stop — do not redial. This is the one close that is
        // *explained*, and an explained close deserves obedience rather than a
        // backoff.
        this.stopped = true
        this.publish({ connected: false, sessions: [], error: 'rmux is shutting down' })
        this.teardown('shutting down')
        break
      default:
        break
    }
  }

  private upsert(session: SessionInfo): ControlState {
    const sessions = this.current.sessions.slice()
    const at = sessions.findIndex((s) => s.id === session.id)
    if (at === -1) sessions.push(session)
    else sessions[at] = session
    return { ...this.current, connected: true, sessions }
  }

  /**
   * One request, one promise, one timeout.
   *
   * The timeout *removes* the pending entry as well as rejecting. A map that
   * only grows is a leak in a process that stays open for days, and a promise
   * that never settles is indistinguishable from a dead app to whoever is
   * waiting on the button that made it.
   */
  private request(body: Record<string, unknown>, duringHandshake = false): Promise<Record<string, unknown>> {
    const socket = this.socket
    if (!socket || socket.destroyed) {
      return Promise.reject(new Error('not connected to rmux'))
    }
    if (!this.current.connected && !duringHandshake && !this.handshaking) {
      // Nothing is answered before `Hello`; asking anyway gets "say hello first"
      // back, which is a confusing thing to show a user who pressed a button.
      return Promise.reject(new Error('not connected to rmux'))
    }
    const id = this.nextId++
    return new Promise<Record<string, unknown>>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`rmux did not answer "${String(body['method'])}" within 10s`))
      }, REQUEST_TIMEOUT_MS)
      this.pending.set(id, { resolve, reject, timer })
      // One JSON line. Nothing in the protocol serialises with an embedded
      // newline — a Rust test asserts that for a 4800-character screenshot —
      // so `JSON.stringify` plus `\n` is the whole framing.
      socket.write(`${JSON.stringify({ id, ...body })}\n`, (e) => {
        if (!e) return
        const pending = this.pending.get(id)
        if (!pending) return
        this.pending.delete(id)
        clearTimeout(pending.timer)
        reject(e)
      })
    })
  }

  /** A connection ended. Whether that earns a backoff reset depends on how long
   *  it *lasted*, never on how tidily it ended — see {@link nextBackoff}. */
  private dropped(reason: string): void {
    if (!this.socket) return
    const lasted = Date.now() - this.connectedAt
    this.teardown(reason)
    if (this.stopped) return
    this.publish({ connected: false, sessions: [], error: reason })
    this.settleFirst()
    this.backoffMs = nextBackoff(this.backoffMs, lasted >= HEALTHY_MS ? 'healthy' : 'brief')
    this.scheduleRetry(this.backoffMs)
  }

  private teardown(reason: string): void {
    const socket = this.socket
    this.socket = null
    this.buffer = ''
    if (socket) {
      socket.removeAllListeners()
      socket.destroy()
    }
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.timer)
      this.pending.delete(id)
      // Every in-flight request is rejected rather than abandoned: a button
      // waiting on a socket that has gone spins forever otherwise.
      pending.reject(new Error(reason))
    }
  }

  private scheduleRetry(afterMs: number): void {
    if (this.stopped || this.retry) return
    this.retry = setTimeout(() => {
      this.retry = null
      void this.dial()
    }, afterMs)
    // Do not hold the app open just to redial a backend it can run without.
    this.retry.unref?.()
  }

  private publish(state: ControlState): void {
    this.current = state
    this.onState(state)
  }

  private settleFirst(): void {
    const resolve = this.firstConnect
    if (!resolve) return
    this.firstConnect = null
    resolve(this.current)
  }
}

/* ------------------------------------------------------------------ helpers */

/**
 * A field, or no field at all.
 *
 * Every optional on the Rust side is `skip_serializing_if = "Option::is_none"`,
 * and a test asserts an absent screenshot does not appear even as `null`. This
 * is the spread-friendly way to honour that, so no call site has to remember.
 */
function omit<T>(key: string, value: T | undefined | null): Record<string, T> {
  return value === undefined || value === null ? {} : ({ [key]: value } as Record<string, T>)
}

/** Ours is `deviceScaleFactor`; the wire's is `devicePixelRatio`. The rename is
 *  the entire function, and it exists so the rename happens in one place. */
function wireViewport(v: Viewport | undefined): Record<string, unknown> | undefined {
  if (!v) return undefined
  return {
    width: v.width,
    height: v.height,
    ...omit('devicePixelRatio', v.deviceScaleFactor),
  }
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}
