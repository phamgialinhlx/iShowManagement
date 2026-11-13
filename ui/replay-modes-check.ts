/**
 * `node --experimental-strip-types ui/replay-modes-check.ts`
 *
 * The decision behind the app-side net for a replay that lost Claude's mouse
 * modes (`src/lib/replay-modes.ts`). The real-xterm half — that the sequences
 * actually stop the wheel turning into arrow keys — is in
 * `xterm-clipboard-check.ts`, because only a real xterm can prove it.
 */
import { replayMouseRepair, type ReplayModeInput } from "./src/lib/replay-modes.ts";
import { AGENT_MOUSE_MODES } from "./src/lib/mouse-modes.ts";

let failures = 0;
function check(what: string, ok: boolean) {
  console.log(`${ok ? "PASS" : "FAIL"}  ${what}`);
  if (!ok) failures += 1;
}

const lost: ReplayModeInput = {
  provider: "claude",
  fullscreen: true,
  buffer: "alternate",
  mouseTracking: "none",
  selectMode: false,
};

check("fullscreen Claude on the alt screen with no mouse mode is repaired", replayMouseRepair(lost) === AGENT_MOUSE_MODES);
check("unset fullscreen counts as fullscreen (claude_start's default)", replayMouseRepair({ ...lost, fullscreen: undefined }) === AGENT_MOUSE_MODES);
check("the repair is button-event tracking plus SGR", AGENT_MOUSE_MODES === "\x1b[?1002h\x1b[?1006h");

check("an inline session is left alone", replayMouseRepair({ ...lost, fullscreen: false }) === "");
for (const provider of ["pi", "codex", "dsh", "terminal"]) {
  check(`${provider} is left alone`, replayMouseRepair({ ...lost, provider }) === "");
}
check("SELECT mode is respected", replayMouseRepair({ ...lost, selectMode: true }) === "");
check("the normal buffer is left alone", replayMouseRepair({ ...lost, buffer: "normal" }) === "");
for (const mode of ["x10", "vt200", "drag", "any"]) {
  check(`a live ${mode} tracking mode is left alone`, replayMouseRepair({ ...lost, mouseTracking: mode }) === "");
}

console.log(failures ? `\n${failures} FAILED` : "\nall passed");
if (failures) (globalThis as { process?: { exitCode?: number } }).process!.exitCode = 1;
