import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { isTauri } from "../lib/api";
import {
  quietWhenWatching,
  setQuietWhenWatching,
  alertWhenUnwatched,
  setAlertWhenUnwatched,
  alertSound,
  setAlertSound,
  playAlertSound,
} from "../lib/notify";

/**
 * Notifications, and a way to find out whether they actually work.
 *
 * The test button is the point of this panel. A notification that fails is
 * **indistinguishable from one nobody looked at** — the plugin's `show()` ends
 * in `spawn(async move { let _ = notification.show(); })`, so the result is
 * discarded and nothing rmux can return tells you whether anything appeared.
 * Without a way to fire one on demand, the only test is "wait for Claude to
 * finish and hope", which is a terrible feedback loop for something that dies
 * silently on a macOS permissions reset.
 *
 * So: press it, and you learn three things at once — whether notifications are
 * permitted for the posting identity, what icon they carry, and that rmux is
 * wired to them at all.
 */
export function NotificationsPanel() {
  const [sent, setSent] = useState(false);
  const [quiet, setQuiet] = useState(quietWhenWatching);
  const [alerting, setAlerting] = useState(alertWhenUnwatched);
  const [sound, setSound] = useState(alertSound);
  const [error, setError] = useState<string | null>(null);

  const test = async () => {
    setError(null);
    try {
      // A fresh session id each time, so the de-duplication that stops a
      // polling status from notifying every tick does not swallow a second
      // press of this button.
      await invoke("notify", {
        session: `test-${Date.now()}`,
        title: "rmux",
        body: "Notifications are working. This is what Claude finishing looks like.",
      });
      setSent(true);
      setTimeout(() => setSent(false), 4000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <section className="flex max-w-[560px] flex-col gap-5">
      <header className="flex flex-col gap-1">
        <h2 className="kicker">NOTIFICATIONS</h2>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          rmux notifies you when a session's Claude finishes a turn, or stops to ask something.
          Running several at once only pays off if you can stop watching them.
        </p>
      </header>

      <div className="flex flex-col gap-2">
        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            checked={quiet}
            style={{ accentColor: "rgb(var(--primary))", marginTop: 2 }}
            onChange={(e) => {
              setQuiet(e.target.checked);
              setQuietWhenWatching(e.target.checked);
            }}
          />
          <span className="flex flex-col gap-[2px]">
            <span className="data text-[11px]" style={{ color: "var(--text)" }}>
              Stay quiet for the session I am watching
            </span>
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              Off by default. This was once the unconditional behaviour and it made the feature
              look broken — you watch a session precisely because you are waiting on it, and
              silence is indistinguishable from a bug.
            </span>
          </span>
        </label>

        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            checked={alerting}
            style={{ accentColor: "rgb(var(--primary))", marginTop: 2 }}
            onChange={(e) => {
              setAlerting(e.target.checked);
              setAlertWhenUnwatched(e.target.checked);
            }}
          />
          <span className="flex flex-col gap-[2px]">
            <span className="data text-[11px]" style={{ color: "var(--text)" }}>
              Also alert in the app, for sessions I am not watching
            </span>
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              On by default. A notification banner slides away on its own, which is fine for
              &ldquo;this finished&rdquo; and not enough for a session that has stopped to ask
              something and is blocking everything behind it. The alert stays until you deal with
              it. Turn this off to get notifications only.
            </span>
          </span>
        </label>

        <label className="flex items-start gap-2" style={{ marginLeft: 20 }}>
          <input
            type="checkbox"
            checked={sound}
            disabled={!alerting}
            style={{ accentColor: "rgb(var(--primary))", marginTop: 2 }}
            onChange={(e) => {
              setSound(e.target.checked);
              setAlertSound(e.target.checked);
              if (e.target.checked) playAlertSound();
            }}
          />
          <span className="flex flex-col gap-[2px]">
            <span
              className="data text-[11px]"
              style={{ color: alerting ? "var(--text)" : "var(--text-faint)" }}
            >
              Play a sound with the alert
            </span>
            <span className="data text-[10px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
              Ticking this plays it, so you know what you are agreeing to and that it works at all.
            </span>
          </span>
        </label>

        <div className="flex items-center gap-3">
          <button type="button" className="btn" disabled={!isTauri()} onClick={() => void test()}>
            Send a test notification
          </button>
          {sent && <span className="micro">SENT — CHECK YOUR NOTIFICATION CENTRE</span>}
        </div>

        {/* "Sent" is deliberately not "shown". rmux hands the notification to
            macOS and is told nothing about what happened next. */}
        {sent && (
          <p className="data text-[10px] leading-relaxed" style={{ color: "var(--text-faint)" }}>
            If nothing appeared, macOS is refusing it rather than rmux failing to send. Check
            System Settings › Notifications for the app named below.
          </p>
        )}

        {error && (
          <p role="alert" className="data text-[11px]" style={{ color: "rgb(var(--primary))" }}>
            {error}
          </p>
        )}
      </div>

      <div
        className="flex flex-col gap-1"
        style={{ borderTop: "1px solid var(--border)", paddingTop: 12 }}
      >
        <span className="micro">WHICH APP POSTS THEM</span>
        <p className="data text-[11px] leading-relaxed" style={{ color: "var(--text-soft)" }}>
          A development build posts as <span style={{ color: "var(--text)" }}>Terminal</span> and
          carries Terminal's icon. That is the notification plugin's own choice — a dev binary has
          no registered bundle identity to post under, so it borrows one. The built{" "}
          <span style={{ color: "var(--text)" }}>rmux.app</span> posts as itself, with rmux's icon.
          Nothing is wrong if you see Terminal here; check the icon from the bundled app.
        </p>
      </div>
    </section>
  );
}
