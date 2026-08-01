import { invoke } from "@tauri-apps/api/core";

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
};

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

/** True when running inside the Tauri shell rather than a plain browser tab. */
export const isTauri = (): boolean =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  // Outside the Tauri shell there is no IPC bridge, and `invoke` fails deep in
  // its internals with "Cannot read properties of undefined". Catching it here
  // turns that into something that names the actual problem — the UI is meant to
  // be runnable in a plain browser for design work, so this path gets hit often.
  if (!isTauri()) {
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
};

/** A host from ~/.ssh/config. `hostname` and `user` are display hints only —
 *  connecting always uses `alias`, which ssh resolves. */
export type ConfigHost = {
  alias: string;
  hostname?: string;
  user?: string;
};

/** A Claude conversation recorded for a folder, resumable by id. */
export type ClaudeSessionInfo = {
  id: string;
  /** Unix seconds of last activity. */
  modified: number;
  title?: string;
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

  // --- the app lock ---------------------------------------------------------
  //
  // The PIN never comes back down from Rust and neither does the device secret.
  // This side hands *up* a PIN or a face descriptor and gets back an account.

  lockStatus: (serverUrl: string) => call<LockStatus>("lock_status", { serverUrl }),
  lockEnable: (pin: string, face: boolean) =>
    call<LockStatus>("lock_enable", { request: { pin, face } }),
  lockDisable: (pin: string) => call<void>("lock_disable", { pin }),
  lockUnlock: (serverUrl: string, pin: string) =>
    call<SignedIn>("lock_unlock", { serverUrl, pin }),
  /** Takes 128 floats. The camera frame itself never leaves the webview. */
  lockUnlockFace: (serverUrl: string, descriptor: number[]) =>
    call<SignedIn>("lock_unlock_face", { serverUrl, descriptor }),
  /** Omit the descriptor to trust this machine against an already-enrolled face. */
  faceEnroll: (descriptor?: number[]) => call<void>("face_enroll", { descriptor }),
  faceStatus: () => call<Account>("face_status"),

  faceModelsStatus: () => call<ModelsStatus>("face_models_status"),
  faceModelsInstall: () => call<ModelsStatus>("face_models_install"),

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

  metricsSample: (target: TargetRef) => call<MetricsSample>("metrics_sample", { target }),

  sshConfigHosts: () => call<ConfigHost[]>("ssh_config_hosts"),

  claudeSessions: (target: TargetRef, folder: string) =>
    call<ClaudeSessionInfo[]>("claude_list_sessions", { target, folder }),

  claudeTranscript: (target: TargetRef, folder: string, session?: string, tailBytes?: number) =>
    call<Transcript>("claude_transcript", { target, folder, session, tailBytes }),

  claudeEndSession: (target: TargetRef, sessionName: string) =>
    call<void>("claude_end_session", { target, sessionName }),

  claudeAccountStatus: () => call<ClaudeAccount>("claude_account_status"),
  claudeAccountSave: (token: string) => call<ClaudeAccount>("claude_account_save", { token }),
  claudeAccountForget: () => call<ClaudeAccount>("claude_account_forget"),
  claudeLoginCommand: (target: TargetRef) => call<string>("claude_login_command", { target }),
  claudeUsageReport: (days?: number) => call<UsageReport>("claude_usage_report", { days }),
};
