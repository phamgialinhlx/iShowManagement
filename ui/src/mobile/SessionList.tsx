import type { ServerGroup, Row } from "../lib/mobile-sessions";
import { totalNeedsYou } from "../lib/mobile-sessions";
import { ChevronIcon, TerminalIcon } from "./icons";
import { SHORT_STATE, STATE_LABEL, StateDot } from "./StateDot";

const KIND: Record<string, string> = { claude: "Claude", pi: "pi", codex: "Codex", terminal: "terminal" };

function SessionRow({ row, selected, onOpen }: { row: Row; selected: boolean; onOpen: (id: string) => void }) {
  return (
    <li>
      <button
        type="button"
        className="m-row"
        aria-current={selected ? "page" : undefined}
        onClick={() => onOpen(row.id)}
        title={row.state ? STATE_LABEL[row.state] : undefined}
        // A row is two lines on purpose — the name, then kind and state.
        data-renderlint-ignore="button-text-wrap"
        data-renderlint-reason="A session row is deliberately two lines: its name, then its kind and state."
      >
        <span className="m-row-mark">
          {row.state ? <StateDot state={row.state} /> : <TerminalIcon size={16} />}
        </span>
        <span className="m-row-text">
          <span className="m-row-name">{row.name}</span>
          <span className="m-row-sub">
            {KIND[row.kind] ?? row.kind}
            {row.state ? ` · ${SHORT_STATE[row.state]}` : ""}
          </span>
        </span>
        <ChevronIcon />
      </button>
    </li>
  );
}

/**
 * Every session this browser's workspace holds, by server then project, agents
 * first and terminals as a quieter group beneath them. The list itself is the
 * scrolling element and carries its own ceiling — a host holds more sessions
 * than the one you develop against.
 */
export function SessionList({
  groups,
  selectedId,
  onOpen,
}: {
  groups: ServerGroup[];
  selectedId: string | null;
  onOpen: (id: string) => void;
}) {
  const waiting = totalNeedsYou(groups);
  return (
    <div className="m-list-screen">
      <header className="m-top">
        <h1 className="m-brand">rmux</h1>
        <span className="m-top-note" role="status">
          {waiting ? `${waiting} ${waiting === 1 ? "needs" : "need"} you` : groups.length ? "nothing waiting" : ""}
        </span>
      </header>
      <nav className="m-list" aria-label="Sessions">
        {groups.length === 0 ? (
          <div className="m-empty">
            <p className="m-empty-title">No sessions in this browser yet</p>
            <p className="m-empty-body">
              This list is the workspace rmux-web seeded this browser with. Sessions appear here once the
              host's seed file includes them; nothing on the host has been changed.
            </p>
          </div>
        ) : (
          groups.map((g) => (
            <section key={g.id} className="m-server" aria-label={g.label}>
              <h2 className="m-server-head">
                {g.state && <StateDot state={g.state} />}
                <span className="m-server-name">{g.label}</span>
                {g.needsYou > 0 && <span className="m-badge">{g.needsYou}</span>}
              </h2>
              {g.projects.map((p) => (
                <div key={p.id} className="m-project">
                  <h3 className="m-project-head" title={p.folder}>
                    <span className="m-project-name">{p.label}</span>
                    <span className="m-project-folder">{p.folder}</span>
                  </h3>
                  {p.agents.length > 0 && (
                    <ul className="m-rows">
                      {p.agents.map((r) => (
                        <SessionRow key={r.id} row={r} selected={r.id === selectedId} onOpen={onOpen} />
                      ))}
                    </ul>
                  )}
                  {p.terminals.length > 0 && (
                    <>
                      <h4 className="m-sub-head">Terminals</h4>
                      <ul className="m-rows m-rows-quiet">
                        {p.terminals.map((r) => (
                          <SessionRow key={r.id} row={r} selected={r.id === selectedId} onOpen={onOpen} />
                        ))}
                      </ul>
                    </>
                  )}
                </div>
              ))}
            </section>
          ))
        )}
      </nav>
    </div>
  );
}
