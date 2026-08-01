import { api } from "./api";

/**
 * Turning a camera frame into a face descriptor.
 *
 * **Nothing here is loaded unless face unlock is used.** The library is reached
 * through a dynamic `import()` so Vite splits it into its own chunk, and the
 * 6.7 MB of model weights are downloaded by Rust on first use rather than
 * shipped — see `src-tauri/src/face_models.rs`. Someone who never enables face
 * unlock pays for none of it.
 *
 * The descriptor is 128 floats, and it is the *only* thing that leaves this
 * module. Video frames stay in the webview; the canvas they are drawn on is
 * discarded with the stream.
 *
 * The model version is not a free choice: a descriptor is only comparable to the
 * ones already enrolled if it came from the same weights. That is why both this
 * import and the download pin `@vladmandic/face-api@1.7.15`, matching what the
 * previous desktop app enrolled faces with.
 */

/** face-api's own type surface, narrowed to what is used here. */
type FaceApi = typeof import("@vladmandic/face-api");

let loading: Promise<FaceApi> | null = null;

/** TFJS's manifest shape, narrowed to what loading needs. */
type WeightsManifest = { paths: string[]; weights: unknown[] }[];

function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

/**
 * Load one network from bytes handed over IPC.
 *
 * **Deliberately not `loadFromUri`.** That fetches, and every URL this app can
 * offer the webview failed: `asset:` does not compose with the
 * base-plus-filename path face-api builds, and a custom scheme brings CSP and
 * cross-origin rules with it. Each failure surfaced as WebKit's bare "Load
 * failed", which says nothing about which of those it was.
 *
 * `loadFromWeightMap` is what `loadFromUri` calls once it has the bytes, so
 * this is the same path with the fetch removed — and IPC is the channel every
 * other command already uses.
 */
async function loadNet(
  faceapi: FaceApi,
  net: { loadFromWeightMap(map: unknown): void },
  prefix: string,
) {
  const manifest = JSON.parse(
    new TextDecoder().decode(base64ToBytes(await api.faceModelFile(`${prefix}-weights_manifest.json`))),
  ) as WeightsManifest;

  // A manifest may split its weights over several files; concatenating them in
  // manifest order is what `tf.io` expects, and getting the order wrong yields
  // tensors of the right shape holding the wrong numbers — which would fail as
  // "no face recognised", never as a load error.
  const parts: Uint8Array[] = [];
  for (const group of manifest) {
    for (const path of group.paths) {
      parts.push(base64ToBytes(await api.faceModelFile(path)));
    }
  }

  const total = parts.reduce((n, p) => n + p.length, 0);
  const buffer = new Uint8Array(total);
  let at = 0;
  for (const part of parts) {
    buffer.set(part, at);
    at += part.length;
  }

  const specs = manifest.flatMap((group) => group.weights);
  const tfio = (faceapi.tf as unknown as {
    io: { decodeWeights(data: ArrayBuffer, specs: unknown[]): unknown };
  }).io;

  net.loadFromWeightMap(tfio.decodeWeights(buffer.buffer as ArrayBuffer, specs));
}

/**
 * Load the library and its three networks, once.
 *
 * The promise is cached rather than a boolean flag: two callers arriving at the
 * same moment — which is exactly what a lock screen with an auto-start camera
 * does — must not both start a load.
 */
/** Told what the engine is doing, since the first load takes real time. */
export let onLoadProgress: ((message: string) => void) | null = null;
export function setLoadProgress(fn: ((message: string) => void) | null) {
  onLoadProgress = fn;
}

export async function loadFaceEngine(): Promise<FaceApi> {
  if (loading) return loading;

  loading = (async () => {
    let faceapi: FaceApi;
    try {
      faceapi = await import("@vladmandic/face-api");
    } catch (e) {
      const detail = e instanceof Error ? e.message : String(e);
      throw new Error(`could not load the face library — ${detail}`);
    }

    // WebGL, because the WASM backend's binaries are not bundled and would be
    // another network fetch. CPU is the fallback and is slow but correct.
    //
    // Cast because the package's bundled type surface omits these two, though
    // both exist at runtime — tfjs-core exports them and face-api re-exports the
    // whole namespace. Narrowed to the two functions rather than `any`, so a
    // typo here is still a compile error.
    const tf = faceapi.tf as unknown as {
      setBackend(name: string): Promise<boolean>;
      ready(): Promise<void>;
    };
    try {
      await tf.setBackend("webgl");
    } catch {
      await tf.setBackend("cpu");
    }
    await tf.ready();

    const status = await api.faceModelsStatus();
    if (!status.installed) {
      throw new Error("the face models are not installed yet");
    }

    // Named as they load, so a failure says which of the three it was rather
    // than leaving the next person to guess as I did.
    const nets: [string, string, { loadFromWeightMap(map: unknown): void }][] = [
      ["face detector", "tiny_face_detector_model", faceapi.nets.tinyFaceDetector],
      ["landmark model", "face_landmark_68_model", faceapi.nets.faceLandmark68Net],
      ["recogniser", "face_recognition_model", faceapi.nets.faceRecognitionNet],
    ];
    for (const [label, prefix, net] of nets) {
      try {
        // 6.7MB arrives over IPC as base64 and is decoded here; on a cold
        // start that is seconds of nothing, which reads as the camera being
        // broken rather than as work in progress.
        onLoadProgress?.(`LOADING THE ${label.toUpperCase()}`);
        await loadNet(faceapi, net, prefix);
      } catch (e) {
        const detail = e instanceof Error ? e.message : String(e);
        throw new Error(`could not load the ${label} — ${detail}`);
      }
    }

    return faceapi;
  })();

  // A failed load must not poison every later attempt: the usual cause is that
  // the models were still downloading, and retrying then works.
  loading.catch(() => {
    loading = null;
  });

  return loading;
}

/** Detector settings, matching what the enrolled descriptors were made with. */
const DETECTOR = { inputSize: 320, scoreThreshold: 0.5 };

/**
 * Compute a descriptor for the most prominent face in a frame.
 *
 * `null` means no face was found — the ordinary case for most frames of a live
 * capture loop, not an error.
 */
export async function describeFace(
  source: HTMLVideoElement | HTMLCanvasElement | HTMLImageElement,
): Promise<number[] | null> {
  const faceapi = await loadFaceEngine();

  const detection = await faceapi
    .detectSingleFace(source, new faceapi.TinyFaceDetectorOptions(DETECTOR))
    .withFaceLandmarks()
    .withFaceDescriptor();

  return detection ? Array.from(detection.descriptor) : null;
}

/** Whether this machine can see a camera at all, before offering to use one. */
export function cameraAvailable(): boolean {
  return typeof navigator !== "undefined" && !!navigator.mediaDevices?.getUserMedia;
}

/**
 * Ask for the camera, with the failure cases named.
 *
 * `getUserMedia` rejects with a `DOMException` whose message is not something to
 * show anyone, and the two common failures need opposite responses: a denied
 * permission is fixed in System Settings, an absent camera cannot be fixed at
 * all. Reporting either as "camera error" leaves the operator with nowhere to go.
 */
export async function openCamera(): Promise<MediaStream> {
  if (!cameraAvailable()) {
    throw new Error("this machine has no camera rmux can use");
  }

  try {
    return await navigator.mediaDevices.getUserMedia({
      video: { width: 480, height: 480 },
      audio: false,
    });
  } catch (e) {
    const name = e instanceof DOMException ? e.name : "";
    if (name === "NotAllowedError" || name === "SecurityError") {
      throw new Error("camera access was refused — allow it in System Settings › Privacy");
    }
    if (name === "NotFoundError" || name === "OverconstrainedError") {
      throw new Error("no camera was found");
    }
    if (name === "NotReadableError") {
      throw new Error("the camera is in use by another app");
    }
    throw new Error(e instanceof Error ? e.message : String(e));
  }
}

/** Human-readable download size, for asking before spending someone's bandwidth. */
export function formatBytes(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * Should the capture loop try again after this failure?
 *
 * The distinction is what stops the lock screen spinning forever. "Not
 * recognised" is worth another frame — people blink, and lighting changes. "This
 * machine is not trusted" and "no face is enrolled" **cannot** start working by
 * pointing a camera at them for longer; those need the PIN and a trip to
 * settings, so the loop must stop and say so.
 *
 * Matching on the message rather than a code because that is what crosses the
 * IPC boundary — `AuthError` carries a string and a signin flag, and the flag is
 * true for all three. The phrases come from `rmux_cowork::face::face_error`,
 * which is the only thing that produces them.
 */
export function faceRetryable(message: string): boolean {
  const terminal = ["trusted", "enrolled", "not usable", "descriptor is"];
  return !terminal.some((phrase) => message.includes(phrase));
}
