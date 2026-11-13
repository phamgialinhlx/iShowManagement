/**
 * The update strip's model: when it shows, and what it says.
 *
 *     node --experimental-strip-types ui/update-check.ts
 */
import { checkOutcome, downloadPercent, stripText, stripVisible, type UpdateStatus } from "./src/lib/updates.ts";

let failed = 0;
function check(name: string, ok: boolean, detail?: unknown) {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${ok ? "" : ` — ${JSON.stringify(detail)}`}`);
  if (!ok) failed++;
}

const base: UpdateStatus = {
  current: "0.2.27",
  platform: "macos",
  phase: "idle",
  version: null,
  done: 0,
  total: null,
  selfUpdate: true,
  dismissed: false,
  checking: false,
  auto: true,
  installError: null,
};
const s = (over: Partial<UpdateStatus>): UpdateStatus => ({ ...base, ...over });

check("idle shows nothing", !stripVisible(base, null));
check("no status shows nothing", !stripVisible(null, null));
check("ready shows", stripVisible(s({ phase: "ready", version: "0.2.28" }), null));
check("ready dismissed for the run hides", !stripVisible(s({ phase: "ready", version: "0.2.28", dismissed: true }), null));
check("available shows", stripVisible(s({ phase: "available", version: "0.2.28" }), null));
check("available skipped at that version hides", !stripVisible(s({ phase: "available", version: "0.2.28" }), "0.2.28"));
check("a newer release than the skipped one shows again", stripVisible(s({ phase: "available", version: "0.2.29" }), "0.2.28"));
check("a skipped version does not hide a ready install", stripVisible(s({ phase: "ready", version: "0.2.28" }), "0.2.28"));

check("unknown size is no percentage", downloadPercent(s({ phase: "downloading", done: 10, total: null })) === null);
check("percentage is clamped", downloadPercent(s({ phase: "downloading", done: 300, total: 200 })) === 100);
check("percentage floors", downloadPercent(s({ phase: "downloading", done: 199, total: 200 })) === 99);
check("downloading text", stripText(s({ phase: "downloading", version: "0.2.28", done: 50, total: 100 })) === "Downloading rmux 0.2.28… 50%");
check("ready text", stripText(s({ phase: "ready", version: "0.2.28" })) === "rmux 0.2.28 is ready");
check("latest outcome names the version", checkOutcome(base) === "You're on the latest version (0.2.27)");

if (failed) {
  console.error(`${failed} failed`);
  (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
} else {
  console.log("all passed");
}
