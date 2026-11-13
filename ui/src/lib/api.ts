import { hasBackend, invoke, isTauri, type Channel } from "./bridge";

import type { Theme } from "./theme";
import type { UpdateStatus } from "./updates";

/**
 * The theme config as Rust serves it: which theme is active, plus only the
 * operator's saved themes. The UI merges its code-defined built-ins on top — a
 * built-in may be `active` without appearing in `userThemes`.
 */
export type ThemeState = {
  active: string;
  userThemes: Theme[];
  /** Follow the OS light/dark appearance. Absent from an older backend. */
  autoSwitch?: boolean;
  lightName?: string;
  darkName?: string;
};

/** What a Redstone deployment offers. All false on one without the bridge. */
export type RedstoneCapabilities = {
  bridge: boolean;
  deviceFlow: boolean;
  orgName?: string;
  protocols?: number[];
};

/**
 * One host's enrolment.
 *
 * `enrolled` and `running` are deliberately separate: the token being on the
 * box says nothing about whether the bridge process survived.
 */
/** Who rmux is signed in to Redstone as. The token never crosses this line. */
export type SessionView = { baseUrl: string; user?: string };

export type RedstoneHost = {
  enrolled: boolean;
  running: boolean;
  hostId?: string;
  endpoint?: string;
};

/**
 * Typed wrappers over the Rust IPC surface.
 *
 * The bearer token deliberately never appears here. It lives in Rust and is
 * written to the OS keychain; the UI only ever sees an account. That means a
 * cross-site scripting bug in the webview cannot exfiltrate a session.
 */

export type Account = {
  id: string;
  username: string;
  displayName: string;
  role: string;
  photo: string | null;
  division: string;
  /** Only `/accounts/me` reports these; elsewhere they are false/0. */
  hasPin: boolean;
  hasFace: boolean;
  faceCount: number;
};

/**
 * What the lock screen needs before anything has been proved.
 *
 * `locked` means a sealed session is stored — not that the app is unusable. With
 * the lock off, every field here is empty and the workbench opens as normal.
 */
export type LockStatus = {
  locked: boolean;
  face: boolean;
  username: string;
  serverUrl: string;
};

/**
 * The result of opening the vault.
 *
 * `account` is null when the PIN was right but the stored Cowork session is no
 * longer usable. The app is unlocked either way — the workbench never needed an
 * account, and refusing entry over a lapsed token would strand you in the one
 * screen that gates everything.
 */
export type Unlocked = { account: Account | null; serverUrl: string };

export type ModelsStatus = {
  installed: boolean;
  /** Total download size, so the operator can be told before it starts. */
  bytes: number;
  dir: string;
};

export type SignedIn = {
  account: Account;
  serverUrl: string;
};

/** Which machine to run on. An absent host means the local machine. */
export type TargetRef = {
  host?: string;
  user?: string;
  port?: number;
  /**
   * Absolute local path of a private key chosen for this server (Connect a
   * server › KEY FILE). Offered to ssh as one more `-i`, additively — the
   * operator's own config and rmux's generated key are still tried.
   *
   * **Not part of the server's identity**: `serverId` ignores it, so choosing or
   * changing a key never re-keys the server's projects and sessions.
   */
  identity?: string;
};

/** A port something is listening on, on the target. */
export type ListeningPort = { port: number; process: string };

/** A session as clients see it — deliberately less than rmux's own `Session`. */
export type ControlSession = {
  id: string;
  name: string;
  host?: string;
  folder: string;
};

export type UplinkInfo = {
  localIp?: string;
  publicIp?: string;
  city?: string;
  country?: string;
  lat?: number;
  long?: number;
};

export type ControlInfo = { running: boolean; handshake?: string };

export type JiraTransition = { id: string; name: string; to?: string };

export type JiraIssue = {
  key: string;
  summary: string;
  /** Free text — a Jira admin can rename or add statuses at will. */
  status: string;
  /**
   * `todo` | `inprogress` | `done`.
   *
   * The server already collapses Jira's `new`/`indeterminate`/`done` into these
   * three (`mapCat`), and this is the only part of a status safe to reason
   * about: the *names* are per-project and renameable.
   */
  statusCategory?: string;
  assignee?: string | null;
  /** Deep link into Jira itself, built server-side from the profile's base URL. */
  url?: string;
};

export type JiraComment = { author?: string | null; created: string; bodyHtml: string };

export type JiraIssueDetail = JiraIssue & {
  /** Jira renders its own markup server-side, so this is HTML. */
  descriptionHtml: string;
  description: string;
  issueType: string;
  comments: JiraComment[];
};

export type Container = {
  id: string;
  name: string;
  image: string;
  /** Docker's own words — "restarting" and "paused" are real states a boolean would have to lie about. */
  status: string;
  running: boolean;
  cpu: number | null;
  /** Bytes, so the UI picks the unit rather than parsing one back. */
  memory: number | null;
  memoryLimit: number | null;
  ports: string;
};

/** Either a list, or the reason there is not one. */
export type DockerReport = { containers: Container[] } | { unavailable: { reason: string } };

export type AgentSession = {
  name: string;
  /** A host-side display name, if one was set. Shown instead of `name`. */
  alias?: string | null;
  pid: number | null;
  ageSeconds: number;
  attached: boolean;
  command: string;
  memory: number | null;
  cpu: number | null;
  /** Where the session is working, read from the host's procfs. Absent when it
   *  could not be determined — a foreign process, or a host without procfs. */
  cwd?: string | null;
};

export type GitChange = {
  path: string;
  origPath: string | null;
  /** Index vs HEAD: `M`, `A`, `D`, `R`, `C`, `T`, or `.`. */
  staged: string;
  /** Working tree vs index; `?` for untracked. */
  unstaged: string;
};

export type GitStatus = {
  branch: string;
  ahead: number;
  behind: number;
  /** Absent when no remote-tracking branch is configured — not the same as 0/0. */
  upstream: string | null;
  changes: GitChange[];
};

/**
 * A sub-session an agent spawned on its host (`rmux spawn`), as recorded in the
 * host's `~/.rmux/children.json` — `host_children`. `session` and `parent` are
 * daemon session names; `parent` is null for a child with no rmux parent.
 */
export type HostChild = {
  session: string;
  parent: string | null;
  kind: "claude" | "pi" | "codex" | "terminal";
  cwd: string;
  name: string;
  createdAt: number;
  /** The credential set the host bound it to (inherited from its parent). */
  profile?: string;
  /** The fallback chain it inherited. */
  chain?: string;
};

/** The sidebar's branch line — `git_branch`. `null` outside a work tree. */
export type GitBranch = {
  branch: string;
  ahead: number;
  behind: number;
  /** Absent when no remote-tracking branch is configured; the arrows are then
   *  omitted rather than reading as "in sync". */
  upstream: string | null;
};

export type GitCommit = {
  sha: string;
  short: string;
  author: string;
  /** ISO 8601, rendered in the viewer's timezone rather than the host's. */
  date: string;
  subject: string;
};

/** Two versions of a file. Monaco does the diffing — see `GitPane`. */
export type GitFileDiff = {
  path: string;
  oldText: string;
  newText: string;
  truncated: boolean;
};

/** Working tree against HEAD, or a commit against its parent. */
export type GitAgainst = { kind: "working" } | { kind: "commit"; sha: string };

export type ForwardState = "local" | "starting" | "active" | "failed" | "stopped";
export type Forward = { port: number; state: ForwardState; error?: string };

export type JiraProfile = { name: string; baseUrl: string; email: string | null };
export type JiraProject = { key: string; name: string };

export type JiraStart = { ok: boolean; authUrl: string; state: string };

export type AuthConfig = {
  redstone: boolean;
  issuer: string | null;
  accounts: boolean;
  jira: boolean;
  orgName: string | null;
};

/**
 * An IPC failure, carrying whether the UI should drop to the login screen.
 *
 * A rejected token and an unreachable server are both "errors" but need opposite
 * responses: the first means sign in again, the second means keep the session and
 * retry. Collapsing them would sign people out whenever the network hiccups.
 */
export class ApiError extends Error {
  readonly requiresSignin: boolean;

  constructor(message: string, requiresSignin: boolean) {
    super(message);
    this.name = "ApiError";
    this.requiresSignin = requiresSignin;
  }
}

function toApiError(e: unknown): ApiError {
  if (typeof e === "object" && e !== null && "message" in e) {
    const err = e as { message: string; requiresSignin?: boolean };
    return new ApiError(err.message, err.requiresSignin ?? false);
  }
  return new ApiError(String(e), false);
}

/** True when running inside the Tauri shell rather than a browser tab. Lives in
 *  `bridge.ts`; re-exported here because every feature already imports it from
 *  this module. Use it only for genuinely desktop-only features — for anything
 *  that works in the web shell too, use `hasBackend`. */
export { isTauri, hasBackend };

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // Outside both shells (a plain browser tab, used for design work) there is no
  // backend, and the call fails with a message naming the actual problem rather
  // than a network error from a server that is not there.
  if (!hasBackend()) {
    throw new ApiError(
      `Not running in the rmux desktop shell — "${cmd}" is unavailable. Use \`pnpm tauri dev\`.`,
      false,
    );
  }

  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw toApiError(e);
  }
}

export type EntryKind = "file" | "directory" | "symlink";

export type DirEntry = {
  name: string;
  kind: EntryKind;
};

/** A file as the editor should treat it. Binary and oversized files are
 *  reported rather than silently mangled — opening one as text and saving
 *  would corrupt it. */
export type FileContent =
  | { kind: "text"; text: string }
  | { kind: "binary"; bytes: number }
  | { kind: "tooLarge"; bytes: number };

/** One metrics reading. `cpuPercent` is null until a second sample exists to
 *  difference against — a cumulative counter cannot describe "now" on its own. */
export type MetricsSample = {
  cpuPercent: number | null;
  memoryUsedBytes: number;
  memoryTotalBytes: number;
  loadAverage: number;
  /** The host's own name, which need not match the ssh alias used to reach it. */
  hostname: string;
  uptimeSeconds: number;
  /** Bytes/sec, null until a second sample exists to difference against. */
  netRxBps: number | null;
  netTxBps: number | null;
  /** `ps` reports %CPU per core; this is the divisor that makes it a share. */
  cores: number;
};

export type ProcessInfo = {
  pid: number;
  name: string;
  cpuPercent: number;
  memoryPercent: number;
};

/** A host from ~/.ssh/config. `hostname` and `user` are display hints only —
 *  connecting always uses `alias`, which ssh resolves. */
export type ConfigHost = {
  alias: string;
  /** Display hints only — never the connection target (ssh resolves the alias). */
  hostname?: string;
  user?: string;
  port?: string;
};

/** What `ssh_identity_check` found about a picked key file. Never its contents. */
export type IdentityCheck = {
  path: string;
  exists: boolean;
  readable: boolean;
  isPublic: boolean;
  privateSibling?: string | null;
  mode?: number | null;
  tooOpen: boolean;
};

/** A Claude conversation, resumable by id. */
export type ClaudeSessionInfo = {
  id: string;
  /** Unix seconds of last activity. */
  modified: number;
  title?: string;
  /**
   * Where it was running, read from the transcript's own `cwd`.
   *
   * Present only in the host-wide listing, where it is the point: it lets a
   * session be resumed without the operator having located the folder first.
   */
  folder?: string;
};

/**
 * A pi conversation, resumable by id.
 *
 * `summary` is often null over SSH — only the transcript header crosses the wire,
 * so when it is absent a conversation is identified by its `cwd` and `modified`.
 * `modified` is **milliseconds** (unlike `ClaudeSessionInfo.modified`, which is
 * unix seconds); `cwd` is load-bearing, because resuming pi requires running in
 * the conversation's own directory.
 */
export type PiConversation = {
  id: string;
  cwd: string;
  summary: string | null;
  /** Unix milliseconds of last activity. */
  modified: number;
  size: number;
};

/** A non-text file encoded for the webview. */
export type PreviewContent =
  | { kind: "base64"; bytes: number; base64: string }
  | { kind: "tooLarge"; bytes: number };

export type Speaker = "user" | "assistant" | "tool" | "system";

export type TranscriptEntry = {
  speaker: Speaker;
  text: string;
  tool?: string;
  timestamp?: string;
};

export type TokenUsage = {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  turns: number;
};

export type ClaudeStatus = {
  mode?: string;
  permissionMode?: string;
  model?: string;
  /** Tokens in the newest prompt — context in use, not a running total. */
  contextTokens?: number;
};

export type Transcript = {
  sessionId: string;
  entries: TranscriptEntry[];
  usage: TokenUsage;
  perTurn: number[];
  totalBytes: number;
  readBytes: number;
  status: ClaudeStatus;
};

export type ClaudeAccount = {
  connected: boolean;
  /** Last few characters only — the credential itself never reaches the webview. */
  hint?: string;
  /** Which kind is stored, so the UI can say what it is. */
  kind?: "oauthToken" | "apiKey" | "adminKey";
  /** True once an admin key is stored — that is what makes usage readable. */
  usageAvailable: boolean;
};

export type ModelUsage = { model: string; output: number; input: number };

/** Organisation usage, from the Console Admin API. */
export type UsageReport = {
  uncachedInput: number;
  cacheRead: number;
  cacheCreation: number;
  output: number;
  byModel: ModelUsage[];
  days: number;
};

export type GlassStatus = {
  /** This machine has `NSGlassEffectView`. */
  available: boolean;
  /** It is installed on the window right now. */
  active: boolean;
};

export type GlassOptions = {
  enabled: boolean;
  /** Apple's thinner style. More wallpaper, less contrast for small text. */
  clear?: boolean;
  /** `#rrggbb`, or omitted for untinted glass. */
  tint?: string;
  tintOpacity?: number;
};

export type SearchQuery = { text: string; caseSensitive?: boolean; regex?: boolean };
export type SearchHit = { path: string; line: number; text: string };

/**
 * One entry of a worktree mirror (`crates/rmux-app/src/worktree.rs`).
 *
 * `path` is relative to the root, `/`-separated. A non-UTF-8 byte in a real
 * filename arrives as a private-use character that Rust maps back exactly, so
 * the path is only ever handed back, never edited character by character.
 * `version` is the mtime in nanoseconds, as a string — compared, never parsed.
 */
export type WorktreeEntry = {
  path: string;
  kind: "file" | "dir" | "symlink";
  size: number;
  version: string;
  ignored: boolean;
  /** A directory whose children are not in the mirror — list on demand. */
  lazy: boolean;
};

export type WorktreeSnapshot = {
  id: number;
  root: string;
  /** Patches with a `seq` at or below this are already in `entries`. */
  seq: number;
  entries: WorktreeEntry[];
  scanned: boolean;
  truncated: boolean;
  watchLimited: boolean;
};

/** A `fs-worktree` event. */
export type WorktreeEvent = { id: number; seq: number } & (
  | { kind: "patch"; upserts?: WorktreeEntry[]; removes?: string[]; renames?: { from: string; to: string }[] }
  | { kind: "scanned"; truncated: boolean; watchLimited: boolean }
  | { kind: "git" }
  | { kind: "down" }
);

/** A file and the version token a save must quote. `null` on the shell path. */
export type Versioned = { content: FileContent; version: string | null };

/** How Rust marks a save refused because the file changed on disk. */
export const CONFLICT_PREFIX = "conflict: ";

export type LogStatus = { path: string; bytes: number; hasPrevious: boolean };

/**
 * Where a downloaded file landed, and how big it turned out.
 *
 * The size is reported rather than assumed: a download that quietly produced a
 * zero-byte file looks exactly like one that worked, and the operator has no
 * other way to tell until they open it.
 */
export type Downloaded = { path: string; bytes: number };

/** `fs_download`'s progress channel, ~4/s. `total` is null until the size is known. */
export type DownloadProgress = { id: string; done: number; total: number | null };

/** One environment variable of a model profile, as the UI is allowed to see it. */
export type ProfileVar = {
  key: string;
  /** Redacted to its last four characters when `secret`. */
  value: string;
  secret: boolean;
};

/**
 * Which agent a profile's credentials are for. The host keeps one credential
 * environment per kind and gives a session only its own, so a DeepSeek key never
 * reaches a Claude session. Profiles saved before kinds existed load as `claude`.
 */
export type ProfileKind = "claude" | "deepseek" | "openai";

/** The profile kind a session of each agent runs under; `undefined` = none. */
export function profileKindFor(kind: string): ProfileKind | undefined {
  if (kind === "claude") return "claude";
  if (kind === "dsh") return "deepseek";
  if (kind === "codex") return "openai";
  return undefined;
}

export type ModelProfile = {
  id: string;
  name: string;
  kind: ProfileKind;
  /**
   * Where this profile sends requests — and the token — to. Absent means the
   * kind's own provider (Anthropic, DeepSeek, OpenAI). Shown rather than
   * inferred from the name: a profile called "GLM" can carry any URL at all.
   */
  endpoint?: string;
  hasCredential: boolean;
  vars: ProfileVar[];
};

/** A fallback chain: Claude profiles tried in order on a usage limit. */
export type ProfileChain = {
  /** Always starts `chain-`, so it never collides with a profile id. */
  id: string;
  name: string;
  kind: ProfileKind;
  members: string[];
  /** Also switch on 529 / overloaded. Off by default — overload is not the account. */
  switchOnOverloaded: boolean;
};

export type ChainsView = {
  chains: ProfileChain[];
  /** Kind → profile id: what a child of a different kind binds to. */
  defaults: Partial<Record<ProfileKind, string>>;
};

/** Whether a session's `modelProfile` names a chain rather than one profile. */
export const isChainId = (id: string | undefined | null): id is string => !!id && id.startsWith("chain-");

/** One account switch recorded on a host (`~/.rmux/fallback.json`). */
export type FallbackRecord = {
  session: string;
  chain: string;
  from: string;
  fromLabel?: string;
  to?: string;
  toLabel?: string;
  /** `usage limit`, `rate limit`, `overloaded`, `operator`. */
  reason: string;
  pattern?: string;
  line?: string;
  source?: string;
  /** `switched`, `switched_back`, `end_of_chain…`, `failed: …`. */
  outcome: string;
  /** Unix seconds. */
  at: number;
  resetsAt?: number;
};

export type FallbackBinding = {
  session: string;
  binding?: {
    kind: ProfileKind;
    set: string;
    chain?: {
      id: string;
      members: string[];
      active: string;
      exhausted?: Record<string, { at: number; resetsAt?: number }>;
      switchOnOverloaded?: boolean;
    };
  };
  labels?: Record<string, string>;
  since: number;
};

export type FallbackLog = { switches: FallbackRecord[]; bindings: FallbackBinding[] };

export type ParsedProfile = {
  vars: Record<string, string>;
  /** Keys that were dropped, named, so half a paste never passes for all of it. */
  ignored: string[];
  warnings: string[];
};

/**
 * What a session's transport made of a low-latency request.
 *
 * Phrased without naming a protocol, exactly as Rust phrases it: the caller
 * asked for a property of the session, and what it needs back is whether it
 * got one and — when it did not — a sentence it can show. `reason` is already
 * written for the operator; it is displayed verbatim rather than re-worded.
 */
export type LowLatencyOutcome =
  | { kind: "granted" }
  | { kind: "declined"; reason: string };

/**
 * What rmux's attempt to install — or upgrade — `mosh-server` on a host came to.
 *
 * Separate outcomes rather than a success flag, because they ask different
 * things of the operator: `needsPassword` is one command they can run
 * themselves and carries it; `failed` is a reason to go and look; `unsupported`
 * is nothing they can do from here; and `nothingNewer` is the one that is
 * *theirs* to decide — the host's repositories have no mosh new enough, so a
 * backport, a PPA or accepting the limitation is the only way forward, and the
 * version it carries is what that decision turns on. Flattening them into "it
 * did not work" would tell them which to act on — none of them.
 *
 * `upgraded` carries both versions because "upgraded" with no numbers is not
 * something anyone can check, and `alreadyCurrent` exists so the panel can say
 * the host is fine rather than leaving a space where a state should be.
 */
export type MoshInstallOutcome =
  | { kind: "installed"; manager: string }
  | { kind: "upgraded"; manager: string; from: string; to: string }
  | { kind: "nothingNewer"; manager: string; version: string }
  | { kind: "alreadyCurrent"; version: string }
  | { kind: "needsPassword"; command: string }
  | { kind: "failed"; reason: string }
  | { kind: "unsupported"; reason: string };

/**
 * A host's `mosh-server` version as rmux reads it.
 *
 * `version` is `null` for both "not probed" and "present but unreadable", and
 * `outdated` is false for both — guessing would mean upgrading packages on
 * someone's server because a string changed format.
 */
export type MoshServerStatus = {
  version: string | null;
  outdated: boolean;
  /** The oldest version rmux is willing to leave a host on, e.g. `1.4.0`. */
  minimum: string;
};

export const api = {
  authConfig: (serverUrl: string) => call<AuthConfig>("auth_config", { serverUrl }),

  signIn: (serverUrl: string, username: string, password: string) =>
    call<SignedIn>("sign_in", { serverUrl, username, password }),

  /** Returns null when nothing is stored — the ordinary first-run case. */
  resumeSession: (serverUrl: string) =>
    call<SignedIn | null>("resume_session", { serverUrl }),

  /**
   * Sign out, forgetting the stored session and this machine's face pairing.
   *
   * `serverUrl` matters only when signing out from the lock screen: nothing has
   * been restored at that point, so Rust has no server to clear unless it is
   * told which one.
   */
  signOut: (serverUrl?: string) => call<void>("sign_out", { serverUrl }),

  /** Begin a Jira sign-in: returns a URL to open and a state to poll on. */
  jiraStart: (serverUrl: string) => call<JiraStart>("jira_start", { serverUrl }),
  /** `null` until the operator finishes in the browser. */
  jiraPoll: (serverUrl: string, state: string) =>
    call<SignedIn | null>("jira_poll", { serverUrl, state }),
  openExternal: (url: string) => call<void>("open_external", { url }),

  /* Updates (desktop only; `src-tauri/src/updates.rs`). The webview never
     passes a URL — `updateOpen` names which link, Rust holds the URL. */
  updateStatus: () => call<UpdateStatus>("update_status"),
  updateConfigure: (auto: boolean) => call<void>("update_configure", { auto }),
  updateCheck: () => call<UpdateStatus>("update_check"),
  updateOpen: (kind: "download" | "notes") => call<void>("update_open", { kind }),
  updateDismiss: () => call<void>("update_dismiss"),
  updateRestart: () => call<void>("update_restart"),

  /** Open Settings in its own window, or focus it if it is already open. */
  openSettings: () => call<void>("open_settings"),

  /** Open another workbench window (its own layout, for another desktop).
   *  Resolves to the new window's label. */
  openWindow: () => call<string>("open_window"),

  /**
   * What a low-latency request would get on this connection, and why not when
   * it would not.
   *
   * **On demand, never polled.** The answer is cached per host for the run, so
   * asking again costs nothing — and it is asked when the TRANSPORT section is
   * opened rather than on a timer, so a widget nobody opened performs no work.
   */
  lowLatencyProbe: (target: TargetRef) =>
    call<LowLatencyOutcome>("low_latency_probe", { target }),

  /**
   * What rmux made of a host whose `mosh-server` is missing, or too old.
   *
   * **Awaits an attempt that is still running; never polled.** `lowLatencyProbe`
   * is what starts the attempt, so this is asked immediately after it returns,
   * and the promise stays pending exactly as long as the work does. `null` means
   * nothing was attempted — the switch is off, or this host is already fine.
   */
  moshInstallStatus: (target: TargetRef) =>
    call<MoshInstallOutcome | null>("mosh_install_status", { target }),

  /**
   * Is rmux changing this host's `mosh-server` right now?
   *
   * Answers immediately. Asked *before* `moshInstallStatus`, whose promise stays
   * pending for as long as the work takes — so the in-flight state is drawn only
   * where something is genuinely in flight, rather than assumed and then taken
   * back at every host where nothing was attempted.
   */
  moshInstallPending: (target: TargetRef) =>
    call<boolean>("mosh_install_pending", { target }),

  /**
   * Which `mosh-server` a host has, and whether rmux considers it too old.
   *
   * **The comparison is Rust's.** The minimum lives in one place and a second
   * version parser here would be a second chance to get `1.10` against `1.9`
   * backwards; `minimum` comes back with it because the sentence shown has to
   * name it. Reads the cache `lowLatencyProbe` filled, so it is asked after it.
   */
  moshServerStatus: (target: TargetRef) =>
    call<MoshServerStatus>("mosh_server_status", { target }),

  /** Mirror the stored auto-install preference into Rust, which holds the gate. */
  setMoshAutoInstall: (on: boolean) => call<void>("set_mosh_auto_install", { on }),

  /** The heaviest processes on a host. Polled only while the widget is open. */
  metricsProcesses: (target: TargetRef, by: "cpu" | "memory", limit?: number) =>
    call<ProcessInfo[]>("metrics_processes", { target, by, limit }),
  /** Signal a process. `hard` sends KILL instead of TERM — see `Metrics::kill`. */
  metricsKill: (target: TargetRef, pid: number, hard: boolean) =>
    call<void>("metrics_kill", { target, pid, hard }),

  /** What the target is listening on, so no port has to be known in advance. */
  portsDiscover: (target: TargetRef) => call<ListeningPort[]>("ports_discover", { target }),
  /** `ssh -L port:localhost:port`, making http://localhost:port reach the target. */
  portForward: (target: TargetRef, port: number) =>
    call<Forward>("port_forward", { target, port }),
  portUnforward: (target: TargetRef, port: number) =>
    call<void>("port_unforward", { target, port }),
  portsForwarded: (target: TargetRef) => call<Forward[]>("ports_forwarded", { target }),
  /** A SOCKS proxy onto the target — every port at once, plus its DNS. */
  portProxy: (target: TargetRef) => call<number>("port_proxy", { target }),

  /** Write a pasted image to the target; returns the path to mention to Claude. */
  claudePasteImage: (target: TargetRef, data: string, kind: string) =>
    call<{ path: string; bytes: number }>("claude_paste_image", { target, data, kind }),

  /** Jira connections the server has configured. Profile-level, so no
   *  server-side session row is required. */
  // --- the control socket ---------------------------------------------------
  //
  // Port discovery and forwarding used to live here, driving an in-app browser
  // tab. That tab is gone: rmux's webview is WKWebView, there is only one of it,
  // and it cannot be given a per-session proxy — so the page it showed always
  // needed a forwarded port, which is the manual step the feature existed to
  // remove. Those calls now belong to `rmux-control`, where a real browser can
  // ask for a SOCKS proxy per session instead.

  /** Mirror the session list down to Rust, so clients can see it. */
  controlSync: (sessions: ControlSession[], active: string | null) =>
    call<void>("control_sync", { sync: { sessions, active } }),
  /** Ask a connected browser to open a URL in this session's partition. */
  controlOpenUrl: (session: string, url: string, focus = true) =>
    call<boolean>("control_open_url", { session, url, focus }),
  /** Where a host is: LAN address, public address, coordinates. Cached per host. */
  hostUplink: (target: TargetRef, refresh = false) =>
    call<UplinkInfo>("host_uplink", { target, refresh }),
  controlInfo: () => call<ControlInfo>("control_info"),

  /**
   * Native Liquid Glass — macOS 26 only, and the UI must ask before offering it.
   *
   * `available` is false on Windows, Linux and every Mac before 26, where the
   * window keeps its ordinary translucent material. A toggle that silently does
   * nothing is worse than no toggle.
   */
  /**
   * Store a chosen background picture and get back the path to load it from.
   *
   * The bytes go to disk rather than into `localStorage`: a wallpaper is
   * megabytes, and that quota is shared with the session list.
   */
  backgroundSet: (dataBase64: string) => call<string>("background_set", { data: dataBase64 }),
  backgroundClear: () => call<void>("background_clear"),

  /**
   * Find text under a folder, on whichever machine it lives on.
   *
   * Runs `grep` on the target rather than reading files across the wire — see
   * `crates/rmux-fs/src/search.rs`. Bounded at 500 hits.
   */
  fsSearch: (target: TargetRef, root: string, query: SearchQuery) =>
    call<SearchHit[]>("fs_search", { target, root, query }),

  /**
   * Open (or join) the live mirror of a folder: the whole tree now, then
   * `fs-worktree` patches as the disk changes. Rejects with `unsupported: …` on
   * a host that is on the shell path (Windows, an older agent) — list lazily.
   */
  fsWorktreeOpen: (target: TargetRef, root: string) =>
    call<WorktreeSnapshot>("fs_worktree_open", { target, root }),
  /** Read with a version token, for an editor that will save the file back. */
  fsReadVersioned: (target: TargetRef, path: string) =>
    call<Versioned>("fs_read_versioned", { target, path }),
  /**
   * Save, refusing (with an error starting `CONFLICT_PREFIX`) when the file
   * changed on disk since `expected` was read. Resolves to the new version.
   */
  fsWriteVersioned: (target: TargetRef, path: string, contents: string, expected: string | null) =>
    call<string | null>("fs_write_versioned", { target, path, contents, expected }),

  /** Relaunch. Sessions survive it — they run under the agent on the target. */
  restartApp: () => call<void>("restart_app"),

  glassStatus: () => call<GlassStatus>("glass_status"),
  setGlass: (options: GlassOptions) => call<GlassStatus>("set_glass", { options }),

  // --- the theme (ANSI palette) ---------------------------------------------
  //
  // The file is canonical (`src-tauri/src/theme.rs`). Rust stores only the
  // operator's themes plus which one is active; the built-in palettes are the
  // UI's (`lib/theme.ts`), merged over what this returns.
  themeState: () => call<ThemeState>("theme_state"),
  /** Create or update a user theme. Rust refuses a built-in name. */
  themeSave: (theme: Theme) => call<ThemeState>("theme_save", { theme }),
  themeSetActive: (name: string) => call<ThemeState>("theme_set_active", { name }),
  themeSetAuto: (autoSwitch: boolean, lightName: string, darkName: string) =>
    call<ThemeState>("theme_set_auto", { autoSwitch, lightName, darkName }),
  themeDelete: (name: string) => call<ThemeState>("theme_delete", { name }),

  jiraProfiles: () => call<JiraProfile[]>("jira_profiles"),
  jiraProjects: (profile: string) => call<JiraProject[]>("jira_projects", { profile }),
  /** The signed-in account's assigned issues. Server-side route: /agency/missions. */
  jiraMissions: () => call<JiraIssue[]>("jira_missions"),
  jiraMission: (key: string) => call<JiraIssueDetail>("jira_mission", { key }),
  /** The moves this issue's workflow permits *right now* — asked, never assumed. */
  jiraTransitions: (key: string) => call<JiraTransition[]>("jira_transitions", { key }),
  jiraTransition: (key: string, transition: string) =>
    call<void>("jira_transition", { key, transition }),
  jiraComment: (key: string, body: string) => call<void>("jira_comment", { key, body }),
  /**
   * Create a task. Assignee and sprint are the *server's* decision — it assigns
   * to the calling account's own Jira user and discovers the project's active
   * sprint, so the app never has to know either.
   */
  jiraCreate: (project: string, summary: string) =>
    call<JiraIssue>("jira_create", { project, summary }),

  // --- the Claude credential ------------------------------------------------
  //
  // Driving the real `claude setup-token` in a pty rather than reimplementing
  // its OAuth flow. Neither the code nor the token is ever stored here.

  /** Start the flow; returns the link to authorise in a browser. */
  claudeLoginStart: (target?: TargetRef) =>
    call<{ authUrl: string }>("claude_login_start", { target }),
  /** Hand back the code from the browser; returns the new account status. */
  claudeLoginSubmit: (code: string) =>
    call<ClaudeAccount>("claude_login_submit", { code }),
  claudeLoginCancel: () => call<void>("claude_login_cancel"),

  // --- the app lock ---------------------------------------------------------
  //
  // The PIN never comes back down from Rust and neither does the device secret.
  // This side hands *up* a PIN or a face descriptor and gets back an account.

  lockStatus: (serverUrl: string) => call<LockStatus>("lock_status", { serverUrl }),
  lockEnable: (pin: string, face: boolean) =>
    call<LockStatus>("lock_enable", { request: { pin, face } }),
  lockDisable: (pin: string) => call<void>("lock_disable", { pin }),
  lockUnlock: (serverUrl: string, pin: string) =>
    call<Unlocked>("lock_unlock", { serverUrl, pin }),
  /** Takes 128 floats. The camera frame itself never leaves the webview. */
  lockUnlockFace: (serverUrl: string, descriptor: number[]) =>
    call<Unlocked>("lock_unlock_face", { serverUrl, descriptor }),
  /** Omit the descriptor to trust this machine against an already-enrolled face. */
  faceEnroll: (descriptor?: number[]) => call<void>("face_enroll", { descriptor }),
  faceStatus: () => call<Account>("face_status"),

  faceModelsStatus: () => call<ModelsStatus>("face_models_status"),
  faceModelsInstall: () => call<ModelsStatus>("face_models_install"),
  /** One model file as base64. Delivered over IPC rather than fetched. */
  faceModelFile: (name: string) => call<string>("face_model_file", { name }),

  fsList: (target: TargetRef, path: string) =>
    call<DirEntry[]>("fs_list", { target, path }),

  fsRead: (target: TargetRef, path: string) =>
    call<FileContent>("fs_read", { target, path }),

  fsWrite: (target: TargetRef, path: string, contents: string) =>
    call<void>("fs_write", { target, path, contents }),

  fsHome: (target: TargetRef) => call<string>("fs_home", { target }),

  fsPreview: (target: TargetRef, path: string) =>
    call<PreviewContent>("fs_preview", { target, path }),

  // Path arithmetic happens in Rust: the remote separator is not necessarily
  // the local one, and the webview cannot know which host it is talking to.
  fsJoin: (parent: string, name: string) => call<string>("fs_join", { parent, name }),

  fsParent: (path: string) => call<string | null>("fs_parent", { path }),

  fsCreateFile: (target: TargetRef, path: string) =>
    call<void>("fs_create_file", { target, path }),

  fsCreateDir: (target: TargetRef, path: string) =>
    call<void>("fs_create_dir", { target, path }),

  fsRename: (target: TargetRef, from: string, to: string) =>
    call<void>("fs_rename", { target, from, to }),

  fsDelete: (target: TargetRef, path: string) => call<void>("fs_delete", { target, path }),

  /**
   * Put a local file onto the target, refusing to overwrite anything.
   *
   * Base64 because the IPC bridge is JSON; Rust decodes it and hands raw bytes
   * to the far side, so binaries survive intact.
   */
  fsUpload: (target: TargetRef, path: string, base64: string) =>
    call<void>("fs_upload", { target, path, base64 }),

  /**
   * A staged upload, for files of any size: begin, append chunks, finish.
   *
   * `fsUpload` carries the whole file as one string and is capped for it; this
   * carries one chunk at a time and Rust streams the assembled file from disk.
   * `finish` emits `fs-upload-progress` while the network leg runs.
   */
  fsUploadBegin: () => call<string>("fs_upload_begin"),
  fsUploadChunk: (id: string, base64: string) => call<void>("fs_upload_chunk", { id, base64 }),
  fsUploadFinish: (target: TargetRef, path: string, id: string, size: number) =>
    call<void>("fs_upload_finish", { target, path, id, size }),
  fsUploadAbort: (id: string) => call<void>("fs_upload_abort", { id }),

  /**
   * Copy a file off the target into this machine's downloads folder.
   *
   * **The bytes never enter the webview.** Unlike `fsUpload`, which has to carry
   * its payload as a base64 string because the file was chosen in a page, a
   * download already knows both ends — so Rust reads it and writes it, and the
   * UI is handed back only where it landed and how big it was. A 200MB log would
   * otherwise be built, copied and parsed as one JavaScript string on the way to
   * a disk it never needed to bounce off.
   */
  fsDownload: (
    target: TargetRef,
    path: string,
    transfer?: string,
    progress?: Channel<DownloadProgress>,
  ) => call<Downloaded>("fs_download", { target, path, transfer, progress }),

  /** Stop a running download by the id it was started under; its staging file is removed. */
  fsDownloadCancel: (transfer: string) => call<boolean>("fs_download_cancel", { transfer }),

  /** Show a finished download in Finder (elsewhere: open its folder). Downloads folder only. */
  fsRevealDownload: (path: string) => call<void>("fs_reveal_download", { path }),

  /**
   * Saved model configurations — Kimi, GLM, a gateway, a reseller.
   *
   * The token in a profile never comes back: `vars` arrives with credentials
   * already redacted, so editing means sending a whole new block down rather
   * than round-tripping the old one through the page.
   */
  modelProfiles: () => call<ModelProfile[]>("model_profiles"),

  /** What a pasted block would become, before anything is saved. */
  modelProfileParse: (text: string, kind: ProfileKind = "claude") =>
    call<ParsedProfile>("model_profile_parse", { text, kind }),

  modelProfileSave: (id: string | null, name: string, text: string, kind: ProfileKind = "claude") =>
    call<ModelProfile[]>("model_profile_save", { id, name, text, kind }),

  modelProfileDelete: (id: string) => call<ModelProfile[]>("model_profile_delete", { id }),

  /**
   * A Claude account profile from a subscription token (`claude setup-token`).
   * Checked by prefix in Rust: an admin key or an API key is refused, with why.
   */
  modelProfileSaveToken: (id: string | null, name: string, token: string) =>
    call<ModelProfile[]>("model_profile_save_token", { id, name, token }),

  /** Fallback chains and each kind's default profile. */
  modelChains: () => call<ChainsView>("model_chains"),
  modelChainSave: (id: string | null, name: string, members: string[], switchOnOverloaded: boolean) =>
    call<ChainsView>("model_chain_save", { id, name, members, switchOnOverloaded }),
  modelChainDelete: (id: string) => call<ChainsView>("model_chain_delete", { id }),
  modelProfileDefaultSet: (kind: ProfileKind, id: string | null) =>
    call<ChainsView>("model_profile_default_set", { kind, id }),

  /**
   * Finish `claude setup-token` and save the token as a profile named `name`.
   * The token goes from the CLI into the keychain; only the redacted view
   * comes back.
   */
  claudeLoginSubmitProfile: (code: string, name: string) =>
    call<ModelProfile[]>("claude_login_submit_profile", { code, name }),

  /** Every account switch a chain made on this host, and current bindings. */
  claudeFallbackLog: (target: TargetRef) => call<FallbackLog>("claude_fallback_log", { target }),
  /** Move a chained session to `member` and relaunch it ("switch back"). */
  claudeFallbackSwitch: (target: TargetRef, session: string, member: string) =>
    call<void>("claude_fallback_switch", { target, session, member }),

  logStatus: () => call<LogStatus>("log_status"),

  /** Writes one shareable file to the desktop and returns its full path. */
  logExport: () => call<string>("log_export"),

  /** Turn benchmark (`BENCH …`) logging on or off, app-wide. */
  setDebugLogging: (on: boolean) => call<void>("set_debug_logging", { on }),

  /** Append one benchmark line to the log (no-op unless debug logging is on). */
  logEvent: (msg: string) => call<void>("log_event", { msg }),

  metricsSample: (target: TargetRef) => call<MetricsSample>("metrics_sample", { target }),

  /** Is this folder a checkout, and where is its root? */
  /** Branch + ahead/behind for a project folder; `null` when it is not a repo. */
  gitBranch: (target: TargetRef, folder: string) =>
    call<GitBranch | null>("git_branch", { target, folder }),
  gitRepo: (target: TargetRef, folder: string) =>
    call<{ root: string | null }>("git_repo", { target, folder }),
  gitStatus: (target: TargetRef, root: string) => call<GitStatus>("git_status", { target, root }),
  gitLog: (target: TargetRef, root: string, limit: number) =>
    call<GitCommit[]>("git_log", { target, root, limit }),
  gitCommitFiles: (target: TargetRef, root: string, sha: string) =>
    call<GitChange[]>("git_commit_files", { target, root, sha }),
  gitFileDiff: (target: TargetRef, root: string, path: string, against: GitAgainst) =>
    call<GitFileDiff>("git_file_diff", { target, root, path, against }),

  /** Containers on a host, joined with their live CPU and memory. */
  dockerContainers: (target: TargetRef) => call<DockerReport>("docker_containers", { target }),
  /**
   * Start, stop or restart a container.
   *
   * The action is a closed set on the Rust side too, so nothing the webview
   * sends can widen it into another `docker` subcommand.
   */
  dockerAction: (target: TargetRef, id: string, action: "start" | "stop" | "restart") =>
    call<string>("docker_action", { target, id, action }),
  /** Sessions `rmux-agent` is holding on a host — the abandoned ones included. */
  hostSessions: (target: TargetRef) => call<AgentSession[]>("host_sessions", { target }),
  /** Sub-sessions agents spawned on a host. A missing or unreadable file is
   *  `[]`; only an unreachable host rejects. */
  hostChildren: (target: TargetRef) => call<HostChild[]>("host_children", { target }),

  sshConfigHosts: () => call<ConfigHost[]>("ssh_config_hosts"),
  /** Inspect a key file before offering it: public half? readable? too open? */
  sshIdentityCheck: (path: string) => call<IdentityCheck>("ssh_identity_check", { path }),
  /** TEST CONNECTION: one `uname -s` over a fresh connection with these settings. */
  sshConnectionTest: (target: TargetRef) => call<string>("ssh_connection_test", { target }),
  /** Generate rmux's key for this host if needed and install its public half. */
  sshKeyInstall: (target: TargetRef) => call<string>("ssh_key_install", { target }),
  /**
   * The native file picker, for a private key. Opens in `~/.ssh`.
   *
   * Invoked as the plugin command directly rather than through
   * `@tauri-apps/plugin-dialog`, which would be a JS dependency for one call.
   * Needs `dialog:allow-open` in `capabilities/default.json` — without it the
   * promise rejects and nothing appears.
   */
  pickKeyFile: (defaultPath?: string) =>
    call<string | null>("plugin:dialog|open", {
      options: {
        title: "Choose a private key",
        defaultPath,
        multiple: false,
        directory: false,
        // A key has no extension; a filter would hide exactly the files wanted.
        canCreateDirectories: false,
      },
    }),

  claudeSessions: (target: TargetRef, folder: string) =>
    call<ClaudeSessionInfo[]>("claude_list_sessions", { target, folder }),
  /** Every Claude session on a host — folder included, so none has to be found. */
  claudeListAllSessions: (target: TargetRef) =>
    call<ClaudeSessionInfo[]>("claude_list_all_sessions", { target }),

  /** Every pi conversation on a host, newest-first — each carries its own `cwd`,
   *  which a resume must run in. */
  piListAllSessions: (target: TargetRef) =>
    call<PiConversation[]>("pi_list_all_sessions", { target }),

  claudeTranscript: (target: TargetRef, folder: string, session?: string, tailBytes?: number) =>
    call<Transcript>("claude_transcript", { target, folder, session, tailBytes }),

  /** A pi conversation as the shared `Transcript`. `cwd` is the conversation's
   *  directory (pi locates its sessions under a cwd-encoded dir), `session` its
   *  conversation id (omit for the newest). Same shape as `claude_transcript`. */
  piTranscript: (target: TargetRef, cwd: string, session?: string, tailBytes?: number) =>
    call<Transcript>("pi_transcript", { target, cwd, session, tailBytes }),

  /** Every recorded Codex conversation on a host, newest first — the resume list. */
  codexListAllSessions: (target: TargetRef) =>
    call<PiConversation[]>("codex_list_all_sessions", { target }),

  /** A Codex conversation's rollout, tail only, parsed tolerantly. */
  codexTranscript: (target: TargetRef, cwd: string, session?: string, tailBytes?: number) =>
    call<Transcript>("codex_transcript", { target, cwd, session, tailBytes }),

  claudeEndSession: (target: TargetRef, sessionName: string) =>
    call<void>("claude_end_session", { target, sessionName }),

  /**
   * Detach: drop this machine's view and leave the work running.
   *
   * Both take a **live handle** rather than a session name, and neither takes a
   * target. That is the safety property, not an oversight — a detach has nothing
   * to say to the host, so there is no argument through which one could end a
   * session. Compare `claudeEndSession` and `terminal_close`, which both name a
   * session on a target precisely because they carry a kill.
   */
  claudeStop: (id: string) => call<void>("claude_stop", { id }),
  terminalDetach: (id: string) => call<void>("terminal_detach", { id }),

  /**
   * Close a server's connection, keeping its sessions running on the host.
   *
   * Returns what it actually did: how many of rmux's caches were holding the
   * host, and whether a transport was asked to close. Reported rather than
   * assumed, because a disconnect that half-worked is indistinguishable from one
   * that did nothing.
   */
  serverDisconnect: (target: TargetRef) =>
    call<{ evicted: number; closed: boolean }>("server_disconnect", { target }),

  // --- Redstone bridge -----------------------------------------------------
  //
  // The token never crosses this boundary in either direction. Enrolling sends
  // one *in* (the operator pasted it) and it goes straight to the host over
  // stdin; nothing ever reads one back. `redstoneHostStatus` returns whether a
  // host is enrolled and whether its bridge is actually running — two separate
  // facts, because a host whose file is present but whose bridge died is the
  // failure that otherwise looks like success.

  /**
   * Sign in by opening Redstone's own login page in a window.
   *
   * Resolves when the operator has signed in and the session has been proved
   * against the server; rejects on cancel or timeout. No password is ever typed
   * into rmux, and the login window is a remote origin with no IPC access.
   */
  redstoneSignIn: (baseUrl: string) => call<SessionView>("redstone_sign_in", { baseUrl }),

  redstoneSession: () => call<SessionView | null>("redstone_session"),

  redstoneSignOut: () => call<void>("redstone_sign_out"),

  /** Mint a token for this host and enrol it. Needs a signed-in session. */
  redstoneEnrol: (target: TargetRef) => call<RedstoneHost>("redstone_enrol", { target }),

  /** What a deployment supports. Asked before any Redstone control is shown. */
  redstoneCapabilities: (baseUrl: string) =>
    call<RedstoneCapabilities>("redstone_capabilities", { baseUrl }),

  redstoneHostStatus: (target: TargetRef) =>
    call<RedstoneHost>("redstone_host_status", { target }),

  /**
   * Start the bridge on a host that is *already* enrolled, without minting a new
   * token — for a host that holds a token but whose bridge is down (e.g. enrolled
   * by a build that never actually started it). Re-enrolling would orphan the old
   * token; this starts the bridge on the one already there.
   */
  redstoneStartBridge: (target: TargetRef) =>
    call<RedstoneHost>("redstone_start_bridge", { target }),

  /**
   * Enrol with a token the operator pasted out of Redstone's own UI.
   *
   * The path that works without signing in — see `redstone.rs`. Minting over
   * HTTP is a convenience, not the mechanism.
   */
  redstoneEnrolWithToken: (
    target: TargetRef,
    endpoint: string,
    token: string,
    hostId?: string,
  ) =>
    call<RedstoneHost>("redstone_enrol_with_token", { target, endpoint, token, hostId }),

  redstoneUnenrol: (target: TargetRef) =>
    call<RedstoneHost>("redstone_unenrol", { target }),

  /**
   * Give a running session a display name every client can see.
   *
   * An **alias**, never a rename of the key: the key is how the daemon holds the
   * session and how every client reattaches, so changing it would orphan running
   * work.
   */
  serverAlias: (target: TargetRef, key: string, alias: string) =>
    call<void>("server_alias", { target, key, alias }),

  claudeAccountStatus: () => call<ClaudeAccount>("claude_account_status"),
  claudeAccountSave: (token: string) => call<ClaudeAccount>("claude_account_save", { token }),
  claudeAccountForget: () => call<ClaudeAccount>("claude_account_forget"),
  claudeLoginCommand: (target: TargetRef) => call<string>("claude_login_command", { target }),
  claudeUsageReport: (days?: number) => call<UsageReport>("claude_usage_report", { days }),
};
