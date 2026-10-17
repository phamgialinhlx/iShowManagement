/**
 * The rmux control client, against a **real** unix socket.
 *
 * Not a stubbed socket, and that distinction is the whole reason this file
 * exists. The bug it pins was an ordering bug spread across three round trips:
 * `hello` carried a handshake bypass, the `listSessions` and `activeSession`
 * that complete the handshake did not, and `connected` was only published after
 * all three — so the second request was rejected with "not connected to rmux",
 * the catch reported that as the connection failing, and R Browser **never
 * connected to a working rmux**. From the UI it looked exactly like rmux being
 * closed, which is the ordinary case and therefore the one nobody questions.
 *
 * A mock that answers `hello` proves nothing about that. A server that answers
 * three requests in sequence proves it, and reproduced it on the first run.
 */
import { createServer, type Server, type Socket } from 'node:net'
import { mkdtempSync, writeFileSync, chmodSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Control } from '../main/control'
import type { ControlState } from '../shared/contract'

let failed = 0
const ok = (cond: boolean, what: string) => {
  if (!cond) failed++
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${what}`)
}

const dir = mkdtempSync(join(tmpdir(), 'rb-control-'))
const sockPath = join(dir, 'control.sock')
const handshakePath = join(dir, 'control.json')
const TOKEN = 'test-token-not-a-real-credential'

writeFileSync(handshakePath, JSON.stringify({ socket: sockPath, token: TOKEN, version: 1 }))
// The client refuses a handshake file that anyone else can read, so the
// fixture has to be a credential-shaped file too.
chmodSync(handshakePath, 0o600)
process.env['RB_CONTROL_HANDSHAKE'] = handshakePath

/** What the fake rmux was asked, in order. The sequence is the assertion. */
const seen: string[] = []
let sawToken = false

/**
 * A stand-in for `rmux-control`'s server, answering the same three methods in
 * the same shapes `protocol.rs` defines. Deliberately strict about ordering:
 * it refuses anything before `hello`, exactly as the real one does, because a
 * permissive fake would have passed while the real server rejected.
 */
function fakeRmux(): Promise<Server> {
  const server = createServer((socket: Socket) => {
    let greeted = false
    let buffer = ''
    socket.on('data', (chunk) => {
      buffer += chunk.toString()
      let i: number
      while ((i = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, i)
        buffer = buffer.slice(i + 1)
        if (!line.trim()) continue
        const msg = JSON.parse(line) as Record<string, unknown>
        const id = msg['id']
        const method = String(msg['method'])
        seen.push(method)

        if (method === 'hello') {
          if (msg['token'] === TOKEN) sawToken = true
          greeted = true
          socket.write(`${JSON.stringify({ id, result: 'hello', version: 1, app: 'rmux' })}\n`)
          continue
        }
        if (!greeted) {
          socket.write(`${JSON.stringify({ id, result: 'error', message: 'say hello first' })}\n`)
          continue
        }
        if (method === 'listSessions') {
          socket.write(`${JSON.stringify({
            id, result: 'sessions',
            sessions: [
              { id: 's-1', name: 'redstone-agent', host: 'build-box', folder: '/home/dev/redstone-agent' },
              { id: 's-2', name: 'local', folder: '/Users/dev/thing' },
            ],
          })}\n`)
          continue
        }
        if (method === 'activeSession') {
          socket.write(`${JSON.stringify({
            id, result: 'session',
            session: { id: 's-2', name: 'local', folder: '/Users/dev/thing' },
          })}\n`)
          continue
        }
        socket.write(`${JSON.stringify({ id, result: 'error', message: `unsupported: ${method}` })}\n`)
      }
    })
    socket.on('error', () => { /* the client closing mid-write is ordinary */ })
  })
  return new Promise((resolve) => server.listen(sockPath, () => resolve(server)))
}

const server = await fakeRmux()

console.log('rmux control client\n')

const states: ControlState[] = []
const control = new Control((s) => states.push(s), () => {})
const state = await control.connect()

// --- the regression -------------------------------------------------------
ok(state.connected, 'connect() reports CONNECTED against a working rmux')
ok(!state.error, `no error reported (got: ${state.error ?? 'none'})`)
ok(state.app === 'rmux', 'the app name comes back')
ok(state.version === 1, 'the protocol version comes back')

// The handshake is three requests. If a future guard rejects the second and
// third again, this is the line that goes red.
ok(
  seen.join(',') === 'hello,listSessions,activeSession',
  `the whole handshake goes out in order (saw: ${seen.join(',') || 'nothing'})`,
)
ok(sawToken, 'the token is sent on hello')

// --- the payload ----------------------------------------------------------
ok(state.sessions.length === 2, `sessions arrive with the first state (got ${state.sessions.length})`)
ok(state.activeSessionId === 's-2', 'the active session is known on connect, not after some later event')
ok(
  state.sessions[0]?.host === 'build-box' && state.sessions[1]?.host === undefined,
  'a local session omits host rather than inventing one',
)

// --- the state is published, not just returned ----------------------------
ok(states.some((s) => s.connected), 'a connected state was pushed to the subscriber')

control.disconnect()
await new Promise<void>((resolve) => server.close(() => resolve()))

console.log(`\n${failed ? `${failed} FAILED` : 'all passed'}`)
process.exit(failed ? 1 : 0)
