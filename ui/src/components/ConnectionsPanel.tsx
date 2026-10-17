import { useState } from "react";

import { moshAutoInstall, setMoshAutoInstall } from "../lib/mosh-install";

/**
 * What rmux may change on a host, as opposed to what it merely reads.
 *
 * One switch today, and it is here rather than in Diagnostics because it is not
 * an instrument — it is a standing permission. Everything else rmux does to a
 * remote machine is either the operator's own command in a terminal or an agent
 * binary in their own `~/.rmux`; installing a package touches the system, and a
 * standing permission to do that deserves to be somewhere they would think to
 * look for it rather than filed under logging.
 */
export function ConnectionsPanel() {
  const [autoInstall, setAutoInstall] = useState(moshAutoInstall());

  return (
    <div className="flex flex-col gap-5">
      <div className="flex flex-col gap-1">
        <span className="kicker">CONNECTIONS</span>
        <p className="data text-[11px] leading-[1.6]" style={{ color: "var(--text-soft)" }}>
          What rmux is allowed to change on a machine you connect to.
        </p>
      </div>

      <div className="inset flex flex-col gap-3 p-4">
        <label className="flex items-center gap-3" style={{ cursor: "pointer" }}>
          <input
            type="checkbox"
            checked={autoInstall}
            onChange={(e) => {
              setAutoInstall(e.target.checked);
              setMoshAutoInstall(e.target.checked);
            }}
            style={{ accentColor: "rgb(var(--primary))", width: 15, height: 15 }}
          />
          <span className="data text-[11px]" style={{ color: "var(--text)" }}>
            Install and update mosh-server automatically
          </span>
        </label>
        <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
          The LOW LATENCY transport needs <span className="micro">mosh-server</span> on the host.
          When a host has not got one, rmux installs it with that machine's own package manager —
          apt, dnf, yum, apk, pacman, zypper or Homebrew — in the background, so nothing you are
          waiting for is delayed. The result is reported under TRANSPORT in a session's settings.
        </p>
        <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
          A host already carrying a <span className="micro">mosh-server</span> older than 1.4 is
          upgraded the same way. Older ones lose too much across a reconnect: come back to a
          fullscreen agent pane and the wheel stops scrolling it. Where a host's repositories have
          nothing newer, TRANSPORT says so rather than reporting an upgrade that did not happen.
        </p>
        <p className="data text-[10.5px] leading-[1.5]" style={{ color: "var(--text-faint)" }}>
          It never asks a host for a password. Where installing or upgrading would need one, rmux
          stops and shows you the command to run yourself, and does not ask that machine again.
          Each host is tried once; a host that failed is left alone for a week. Turn this off for
          machines that are not yours to change — nothing is then attempted and nothing is
          recorded.
        </p>
      </div>
    </div>
  );
}
