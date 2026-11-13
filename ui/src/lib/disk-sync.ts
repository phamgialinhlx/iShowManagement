import { api } from "./api";
import { isDirty } from "./buffers";
import { useWorkspace } from "./workspace";
import { onFileChange, targetKey, versionOf } from "./worktree";

/**
 * Open editors follow the disk.
 *
 * The worktree watcher reports every file whose version changed. For each open
 * buffer on that path:
 *
 * - **clean** (nothing unsaved) — reload silently. The editor keeps its cursor
 *   and scroll (`CodeEditor` swaps the text in place), so an agent's edit to
 *   the file you are reading simply appears.
 * - **dirty** — never decided for the operator. The buffer is marked
 *   `changed`, the editor shows *changed on disk — reload / keep mine*, and
 *   saves stop until they choose. Reloading would discard their edits; saving
 *   would discard the agent's.
 *
 * Our own saves come back through the watcher too. They carry the version the
 * save returned, so they compare equal and nothing happens — and a buffer that
 * is mid-save is left alone until the reply sets its version.
 */

let installed = false;

export function installDiskSync(): void {
  if (installed) return;
  installed = true;
  onFileChange((target, abs, version) => {
    const state = useWorkspace.getState();
    const key = targetKey(target);
    for (const b of Object.values(state.buffers)) {
      if (b.path !== abs || b.loading || b.saving) continue;
      if (targetKey(state.targetOfProject(b.projectId)) !== key) continue;
      if (version !== null && version === b.version) continue;
      if (version === null) {
        mark(b.key, { kind: "deleted" });
      } else if (!isDirty(b) && !b.disk) {
        void reloadFromDisk(b.key, { quiet: true });
      } else if (b.disk?.kind !== "conflict") {
        mark(b.key, { kind: "changed", version });
      }
    }
  });
}

function mark(key: string, disk: NonNullable<import("./buffers").Buffer["disk"]>) {
  useWorkspace.setState((s) => {
    const b = s.buffers[key];
    if (!b) return s;
    return { buffers: { ...s.buffers, [key]: { ...b, disk } } };
  });
}

/** Replace the buffer with what is on disk. `quiet`: only if still clean. */
export async function reloadFromDisk(key: string, opts: { quiet?: boolean } = {}): Promise<void> {
  const b = useWorkspace.getState().buffers[key];
  if (!b) return;
  const target = useWorkspace.getState().targetOfProject(b.projectId);
  try {
    const { content, version } = await api.fsReadVersioned(target, b.path);
    useWorkspace.setState((s) => {
      const cur = s.buffers[key];
      if (!cur) return s;
      // Typed into while the read was in flight: that is now a dirty buffer
      // with a newer disk, which is the operator's call, not ours.
      if (opts.quiet && isDirty(cur)) {
        return { buffers: { ...s.buffers, [key]: { ...cur, disk: version ? { kind: "changed", version } : cur.disk } } };
      }
      const text = content.kind === "text" ? content.text : "";
      return {
        buffers: {
          ...s.buffers,
          [key]: { ...cur, content, text, saved: text, version, disk: null, saveError: null, error: null },
        },
      };
    });
  } catch (e) {
    if (opts.quiet) return;
    useWorkspace.setState((s) => {
      const cur = s.buffers[key];
      if (!cur) return s;
      return { buffers: { ...s.buffers, [key]: { ...cur, saveError: e instanceof Error ? e.message : String(e) } } };
    });
  }
}

/**
 * Keep the operator's version: adopt the disk's current version as the one
 * this buffer is based on, then save over it — deliberately, now that someone
 * chose to.
 */
export async function keepMine(key: string): Promise<void> {
  const state = useWorkspace.getState();
  const b = state.buffers[key];
  if (!b?.disk) return;
  const target = state.targetOfProject(b.projectId);
  let version: string | null = null;
  if (b.disk.kind === "changed") version = b.disk.version;
  else if (b.disk.kind === "conflict") {
    version = versionOf(target, b.path);
    if (!version) {
      try {
        version = (await api.fsReadVersioned(target, b.path)).version;
      } catch {
        version = null;
      }
    }
  }
  // `deleted` keeps `null`: the save recreates the file without a check.
  useWorkspace.setState((s) => {
    const cur = s.buffers[key];
    if (!cur) return s;
    return { buffers: { ...s.buffers, [key]: { ...cur, version, disk: null } } };
  });
  await useWorkspace.getState().save(key);
}
