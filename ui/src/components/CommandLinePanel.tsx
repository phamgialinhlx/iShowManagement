import { useEffect, useState } from "react";

import { invoke, isTauri } from "../lib/bridge";

/** What `cli_status` / `cli_install` / `cli_uninstall` report. */
type CliStatus = {
  /** Where the `rmux` link is, when one exists. */
  link: string | null;
  /** What it points at. */
  target: string | null;
  /** Whether the link points at *this* app's bundled agent. */
  current: boolean;
  /** The agent binary inside this app, which an install links to. */
  bundled: string | null;
  /** Whether the link's directory is on the login shell's PATH. */
  onPath: boolean;
  /** A sentence for the operator: what happened, or what to do next. */
  note: string | null;
};

/**
 * Settings › COMMAND LINE — put `rmux` on PATH.
 *
 * The command is the agent binary already inside this app, so installing is a
 * symlink and nothing is copied: the next app update moves the command with it.
 * Every press reports its outcome beside the button (CLAUDE.md: a mutating
 * control reports inline, errors persist until the next attempt).
 */
export function CommandLinePanel() {
  const [status, setStatus] = useState<CliStatus | null>(null);
  const [busy, setBusy] = useState<"install" | "uninstall" | null>(null);
  const [outcome, setOutcome] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    if (!isTauri()) return;
    invoke<CliStatus>("cli_status")
      .then(setStatus)
      .catch((e) => setOutcome({ ok: false, text: String((e as { message?: string })?.message ?? e) }));
  }, []);

  const run = (cmd: "cli_install" | "cli_uninstall") => {
    setBusy(cmd === "cli_install" ? "install" : "uninstall");
    setOutcome(null);
    invoke<CliStatus>(cmd)
      .then((s) => {
        setStatus(s);
        setOutcome({
          ok: true,
          text: s.note ?? (cmd === "cli_install" ? `Installed at ${s.link}` : "Removed"),
        });
      })
      .catch((e) => setOutcome({ ok: false, text: String((e as { message?: string })?.message ?? e) }))
      .finally(() => setBusy(null));
  };

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-1">
        <span className="kicker">COMMAND LINE</span>
        <p className="data text-[11px] leading-[1.6]" style={{ color: "var(--text-soft)" }}>
          Use <span className="micro">rmux</span> from any terminal — list, attach, start and end
          sessions on this Mac and on every server you have added, tmux-style.
        </p>
      </div>

      <div className="inset flex flex-col gap-3 p-4">
        <div className="data text-[11px]" style={{ color: "var(--text)" }}>
          {status === null
            ? "Checking…"
            : status.link
              ? <>Installed at <span className="micro">{status.link}</span>{status.current ? "" : " (points at another copy of rmux)"}</>
              : "Not installed"}
        </div>

        <div className="flex items-center gap-3">
          <button
            className="btn"
            disabled={busy !== null || !status?.bundled}
            onClick={() => run("cli_install")}
          >
            {busy === "install" ? "INSTALLING…" : status?.link ? "REINSTALL" : "INSTALL rmux COMMAND"}
          </button>
          {status?.link && (
            <button className="btn" disabled={busy !== null} onClick={() => run("cli_uninstall")}>
              {busy === "uninstall" ? "REMOVING…" : "UNINSTALL"}
            </button>
          )}
        </div>

        {outcome && (
          <p
            className="data text-[10.5px] leading-[1.5]"
            style={{ color: outcome.ok ? "var(--text-soft)" : "var(--warn)" }}
          >
            {outcome.text}
          </p>
        )}
        {status && !status.bundled && (
          <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
            This build of rmux carries no agent binary to link to, so there is nothing to install.
          </p>
        )}
        {status?.link && !status.onPath && (
          <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
            That folder is not on your PATH yet. Add this line to your shell profile
            (<span className="micro">~/.zshrc</span> or <span className="micro">~/.bashrc</span>):{" "}
            <span className="micro">export PATH="$HOME/.local/bin:$PATH"</span>
          </p>
        )}

        <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
          Links <span className="micro">/usr/local/bin/rmux</span> when that folder is writable,
          otherwise <span className="micro">~/.local/bin/rmux</span>. Try{" "}
          <span className="micro">rmux ls</span>, <span className="micro">rmux attach -t server:name</span>{" "}
          and <span className="micro">rmux new -s name</span>; detach with Ctrl-B then D. Servers
          are the ones in this app's list.
        </p>
      </div>
    </div>
  );
}
