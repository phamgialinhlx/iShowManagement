import { faceRetryable, formatBytes } from "./src/lib/face";

/**
 * Checks for the lock screen's decision logic.
 *
 * Open `http://localhost:5273/lock-check.html` and read the console.
 *
 * Only the parts that can be exercised without a camera are here — and they are
 * the parts that matter, because the failure mode they guard is a lock screen
 * that retries forever against a condition no amount of retrying can fix. That
 * bug is invisible in a manual test with a working face.
 */

let failures = 0;

function check(name: string, condition: boolean) {
  if (condition) {
    console.log(`ok   ${name}`);
  } else {
    failures += 1;
    console.error(`FAIL ${name}`);
  }
}

// --- which failures are worth another frame ---------------------------------
//
// The exact strings are the ones `rmux_cowork::face::face_error` produces. If
// those are reworded, these break — which is the point: the wording is load
// bearing, and a silent mismatch would restore the infinite loop.

check(
  "an unrecognised face is retried",
  faceRetryable("that is not a face this account knows"),
);

check(
  "an untrusted machine stops the loop",
  !faceRetryable("this machine is no longer trusted — sign in to trust it again"),
);

check(
  "an account with no enrolled face stops the loop",
  !faceRetryable("no face is enrolled for this account"),
);

check(
  "a malformed capture stops the loop",
  !faceRetryable("that face capture is not usable"),
);

check(
  "a wrong-length descriptor stops the loop",
  !faceRetryable("a face descriptor is 128 numbers, got 3"),
);

// A server or network fault is transient by nature, so it must NOT be treated as
// terminal — giving up on a blip would send someone to the PIN unnecessarily.
check("a transport failure is retried", faceRetryable("cowork server unreachable: timed out"));
check("an unknown error is retried", faceRetryable("something nobody predicted"));

// --- the download size the operator is asked to agree to --------------------

check("6.7 MB is reported as such", formatBytes(7_025_512) === "6.7 MB");
check("a small figure keeps one decimal", formatBytes(1_048_576) === "1.0 MB");

console.log(failures === 0 ? "\nall lock checks passed" : `\n${failures} lock check(s) FAILED`);
