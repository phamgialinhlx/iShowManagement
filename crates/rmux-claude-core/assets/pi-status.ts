/**
 * rmux status extension for pi.
 *
 * rmux tells you which of your sessions is working, idle or waiting by watching
 * `~/.rmux/status/<id>.json` — the same fact Claude Code publishes for itself in
 * `~/.claude/sessions`. pi has no such file, so rmux installs this extension:
 * on each of pi's lifecycle events it shells out to `rmux-agent hook`, which is
 * the single Rust owner of that file's name, mode and schema. Nothing about the
 * status file's format lives here — only *when* to update it.
 *
 * Installed by rmux into `~/.pi/agent/extensions/rmux-status.ts`, a global
 * auto-discovered location, so it loads for every pi session without a flag.
 *
 * It is a **no-op unless pi was launched under rmux**: rmux passes
 * `RMUX_AGENT_BIN` (where to find the writer) and `RMUX_STATUS_KEY` (the id rmux
 * lists this session under). With neither set there is nobody reading the status
 * directory, so the extension does nothing and costs a pi session started by
 * hand exactly one environment check per event.
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// CommonJS `require` with bare specifiers, matching pi's own `notify.ts`
// example verbatim — the extension runtime provides these, and an ESM `import`
// of a node built-in is not guaranteed to resolve the same way across runtimes.
const { execFile } = require("child_process");
const { basename } = require("path");

/**
 * pi's own conversation id, so the status file joins the transcript rmux already
 * lists this session under. pi names transcripts `<ISO-timestamp>_<id>.jsonl`;
 * the id is the part after the last underscore. `RMUX_STATUS_KEY` wins when rmux
 * set it, because rmux then already knows the key it wants to correlate on.
 */
function sessionId(ctx: any): string | undefined {
	const fromEnv = process.env.RMUX_STATUS_KEY;
	if (fromEnv) return fromEnv;
	const file = ctx?.sessionManager?.getSessionFile?.();
	if (!file) return undefined;
	const stem = basename(String(file)).replace(/\.jsonl$/, "");
	const underscore = stem.lastIndexOf("_");
	return underscore >= 0 ? stem.slice(underscore + 1) : stem;
}

/**
 * Publish one status. Fire-and-forget: a status update must never block or fail
 * a pi turn, so every error — a missing binary, a write that races — is swallowed.
 */
function report(ctx: any, status: string): void {
	const bin = process.env.RMUX_AGENT_BIN;
	if (!bin) return; // not launched under rmux — nobody is watching
	const id = sessionId(ctx);
	if (!id) return;
	const args = [
		"hook",
		"--agent",
		"pi",
		"--session",
		id,
		"--status",
		status,
		"--pid",
		String(process.pid),
	];
	const cwd = process.cwd();
	if (cwd) args.push("--cwd", cwd);
	try {
		execFile(bin, args, () => {});
	} catch {
		// swallow — see the doc comment
	}
}

export default function (pi: ExtensionAPI) {
	// idle when the session opens, busy while a prompt runs, idle again once the
	// whole run settles (pi's `agent_settled` — after any retry/compaction/follow
	// -up, which is the "truly ready for input" edge its own notify example uses).
	pi.on("session_start", async (_event, ctx) => report(ctx, "idle"));
	pi.on("agent_start", async (_event, ctx) => report(ctx, "busy"));
	pi.on("turn_start", async (_event, ctx) => report(ctx, "busy"));
	pi.on("agent_settled", async (_event, ctx) => report(ctx, "idle"));
	// The session ended: erase the file so rmux's watcher reports it gone once.
	pi.on("session_shutdown", async (_event, ctx) => report(ctx, "gone"));
}
