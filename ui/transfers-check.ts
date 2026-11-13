/**
 * The transfers model: what a row may say, and what is allowed to be stored.
 *
 *     node --experimental-strip-types ui/transfers-check.ts
 *
 * Pinned rather than eyeballed because every failure here *looks* fine: a bar
 * at 103%, a "done" for a transfer that never finished, a retry button on an
 * upload whose file the page no longer holds, a store that grows until the
 * quota it shares with the session list runs out.
 */
import {
  MAX_AGE_MS,
  MAX_ENTRIES,
  STALE_MS,
  canCancel,
  canRetry,
  cancel,
  createEntry,
  fail,
  finish,
  interrupt,
  merge,
  parse,
  percent,
  progress,
  prune,
  reapStale,
  retry,
  serialize,
  start,
  type TransferEntry,
} from "./src/lib/transfers.ts";

let failures = 0;
function check(what: string, ok: boolean) {
  if (ok) console.log(`PASS  ${what}`);
  else {
    failures += 1;
    console.error(`FAIL  ${what}`);
  }
}

const T0 = 1_700_000_000_000;
const make = (over: Partial<TransferEntry> = {}, dir: "up" | "down" = "down") => ({
  ...createEntry(
    {
      id: over.id ?? "a",
      direction: dir,
      server: "me@host",
      target: { host: "host", user: "me" },
      remotePath: "/srv/log.txt",
      name: "log.txt",
      owner: "w1",
    },
    T0,
  ),
  ...over,
});

// ── State transitions ─────────────────────────────────────────────────────
{
  const q = make();
  check("a new entry is queued", q.state === "queued");
  const r = start(q, T0 + 1);
  check("queued → running", r.state === "running");
  const d = finish(r, T0 + 2, { bytes: 10, localPath: "/Users/me/Downloads/log.txt" });
  check("running → done, with where it landed", d.state === "done" && d.localPath?.endsWith("log.txt") === true);
  check("done is terminal: a late progress event changes nothing", progress(d, 3, 10, T0 + 3) === d);
  check("done is terminal: cannot fail afterwards", fail(d, "x", T0 + 3) === d);
  const c = cancel(r, T0 + 2);
  check("running → cancelled", c.state === "cancelled" && c.endedAt === T0 + 2);
  check("cancelled cannot become done", finish(c, T0 + 3).state === "cancelled");
  const f = fail(r, "short read from /srv/log.txt", T0 + 2);
  check("running → failed keeps the error", f.state === "failed" && f.error?.includes("short read") === true);
  check("cancel is offered only while queued or running", canCancel(q) && canCancel(r) && !canCancel(d) && !canCancel(f));
  check("a queued entry cannot finish without running", finish(q, T0 + 1).state === "queued");
}

// ── Progress never exceeds the total, and never runs backwards within a leg ─
{
  let r = start(make(), T0);
  r = progress(r, 50, 100, T0 + 1);
  check("progress records bytes and total", r.done === 50 && r.total === 100 && percent(r) === 50);
  r = progress(r, 140, 100, T0 + 2);
  check("progress is clamped to the total", r.done === 100 && percent(r) === 100);
  r = progress(r, 30, 100, T0 + 3);
  check("progress does not jitter backwards within a leg", r.done === 100);
  let u = start(make({}, "up"), T0);
  u = progress(u, 100, 100, T0 + 1, "reading");
  u = progress(u, 5, 100, T0 + 2, "sending");
  check("a new upload leg starts its own count", u.done === 5 && u.phase === "sending" && u.phaseAt === T0 + 2);
}

// ── Unknown total: no invented percentage ─────────────────────────────────
{
  let r = start(make(), T0);
  check("an unknown total has no percentage", percent(r) === null && r.total === null);
  r = progress(r, 4096, null, T0 + 1);
  check("bytes without a total still have no percentage", percent(r) === null && r.done === 4096);
  r = progress(r, 4096, undefined, T0 + 2);
  check("an update that does not mention the total leaves it unknown", r.total === null);
}

// ── Restart: running → interrupted, honestly ──────────────────────────────
{
  const down = start(make({ id: "d" }), T0);
  const up = start(make({ id: "u" }, "up"), T0);
  const done = finish(start(make({ id: "x" }), T0), T0 + 5, { bytes: 1 });
  const reaped = reapStale([down, up, done], "w2", T0 + STALE_MS + 1);
  const [rd, ru, rx] = reaped;
  check("a running download from a dead window is interrupted", rd?.state === "interrupted");
  check("a running upload from a dead window is interrupted", ru?.state === "interrupted");
  check("an interrupted transfer is never reported done", reaped.every((e) => e.id === "x" || e.state !== "done"));
  check("a finished entry is left alone", rx === done);
  check("the interrupted download says why", rd?.error?.includes("closed") === true);
  check("the interrupted upload says to drop the file again", ru?.error?.includes("Drop the file again") === true);
  check("an interrupted download offers retry", rd !== undefined && canRetry(rd));
  check("an interrupted upload offers no retry at all", ru !== undefined && !canRetry(ru));
  check("a failed upload offers no retry either", !canRetry(fail(start(make({}, "up"), T0), "x", T0)));
  const fresh = reapStale([down], "w2", T0 + STALE_MS - 1);
  check("a running entry another live window is still beating is left running", fresh[0]?.state === "running");
  const own = reapStale([down], "w1", T0 + STALE_MS * 10);
  check("this window's own running entry is never reaped", own[0]?.state === "running");
  check("interrupting a finished entry does nothing", interrupt(done, T0 + 9) === done);
  if (rd) {
    const again = retry(rd, "w2", T0 + 100);
    check("retry puts the same row back to queued, owned by the retrying window", again.state === "queued" && again.id === "d" && again.owner === "w2" && again.error === undefined && again.done === 0);
  }
}

// ── Pruning by count and by age ───────────────────────────────────────────
{
  const now = T0 + MAX_AGE_MS * 2;
  const old = finish(start(make({ id: "old", startedAt: T0 }), T0), T0 + 1);
  const oldRunning = start(make({ id: "oldrun", startedAt: T0 }), T0);
  const recent = Array.from({ length: MAX_ENTRIES + 20 }, (_, i) =>
    finish(start(make({ id: `r${i}`, startedAt: now - i * 1000 }), now), now - i * 1000 + 1),
  );
  const kept = prune([old, oldRunning, ...recent], now);
  check(`pruned to at most ${MAX_ENTRIES}`, kept.length === MAX_ENTRIES);
  check("entries older than 7 days are dropped", !kept.some((e) => e.id === "old"));
  check("a running entry is never pruned, however old", kept.some((e) => e.id === "oldrun"));
  check("the newest finished entries are the ones kept", kept.some((e) => e.id === "r0") && !kept.some((e) => e.id === `r${MAX_ENTRIES + 19}`));
  const ordered = kept.every((e, i) => i === 0 || kept[i - 1]!.startedAt >= e.startedAt);
  check("newest first", ordered);
}

// ── The persisted shape holds metadata only ───────────────────────────────
{
  const withJunk = {
    ...finish(start(make(), T0), T0 + 1, { bytes: 3 }),
    // Things that must never reach storage, even if someone attaches them.
    bytes: new Uint8Array([1, 2, 3]),
    base64: "AAEC",
    file: { name: "x" },
  } as unknown as TransferEntry;
  const raw = serialize([withJunk], T0 + 2);
  const parsed = JSON.parse(raw) as Record<string, unknown>[];
  const keys = Object.keys(parsed[0] ?? {});
  check("no bytes, base64 or File objects are persisted", !keys.includes("bytes") && !keys.includes("base64") && !keys.includes("file"));
  check("the persisted row is plain metadata", keys.every((k) => ["id", "direction", "server", "target", "remotePath", "name", "localPath", "done", "total", "state", "phase", "error", "startedAt", "endedAt", "phaseAt", "uploadId", "owner", "seenAt", "cancelRequested"].includes(k)));
  check("the target carries no extra fields", JSON.stringify(parsed[0]?.target) === JSON.stringify({ host: "host", user: "me" }));
  const back = parse(raw);
  check("it round-trips", back.length === 1 && back[0]?.state === "done" && back[0]?.done === 3);
  check("garbage in storage reads as an empty list", parse("{not json").length === 0 && parse('[{"id":1}]').length === 0);
  check("serialize prunes on every write", JSON.parse(serialize([finish(start(make({ startedAt: T0 }), T0), T0)], T0 + MAX_AGE_MS * 2)).length === 0);
}

// ── Windows: the owner is the authority for what it runs ──────────────────
{
  const mine = progress(start(make({ id: "m", owner: "w1" }), T0), 80, 100, T0 + 5);
  const theirsOfMine = { ...start(make({ id: "m", owner: "w1" }), T0), cancelRequested: true };
  const other = start(make({ id: "o", owner: "w2" }), T0);
  const merged = merge([mine], [theirsOfMine, other], "w1");
  const m = merged.find((e) => e.id === "m");
  check("my running entry keeps my progress", m?.done === 80);
  check("a cancel asked for in another window reaches the owner", m?.cancelRequested === true);
  check("another window's entry is taken as theirs", merged.some((e) => e.id === "o"));
  const mineDone = finish(mine, T0 + 9);
  check("my finished entry dismissed elsewhere stays dismissed", !merge([mineDone], [other], "w1").some((e) => e.id === "m"));
  check("my running entry missing elsewhere is kept", merge([mine], [other], "w1").some((e) => e.id === "m"));
}

if (failures) {
  console.error(`\n${failures} check(s) failed`);
  (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
} else {
  console.log("\nall transfers checks passed");
}
