import { readPrefixConfig, tokenLabel } from "../lib/prefix-mode";
import { tabLabel, tabsOf, type MoveSource } from "../lib/layout-tree";
import { useWorkspace } from "../lib/workspace";
import { MenuDivider, MenuItem, MenuSurface, type MenuAt } from "./Menu";

/**
 * A pane header's right-click menu: the no-drag way to move a pane between
 * tabs. "Move to new tab" is tmux's `break-pane` (also prefix `!`); "Move to
 * tab" lists this project's other tabs. Structural only — nothing here ends a
 * session, so there is no destructive item and no confirmation.
 *
 * Items that could not work are **absent**, not disabled: a pane already alone
 * in its tab is not offered a new one, and a project with one tab lists none.
 * The tab list scrolls inside its own ceiling — an agent can spawn tabs' worth
 * of children, and a menu taller than the window carries its last items off
 * the screen.
 */
export function PaneMoveMenu({
  at,
  projectId,
  tabId,
  source,
  onClose,
}: {
  at: MenuAt;
  projectId: string;
  tabId: string;
  source: MoveSource;
  onClose: () => void;
}) {
  const layout = useWorkspace((s) => s.layout);
  const sessions = useWorkspace((s) => s.sessions);
  const refusal = useWorkspace((s) => s.tabMoveRefusal);
  const moveToTab = useWorkspace((s) => s.moveToTab);
  const moveToNewTab = useWorkspace((s) => s.moveToNewTab);
  const nameOf = (id: string) => sessions.find((x) => x.id === id)?.name;
  const tabs = tabsOf(layout, projectId).tabs;
  const others = tabs
    .map((t, i) => ({ t, i }))
    .filter(({ t }) => t.id !== tabId && !refusal(source, projectId, t.id));
  const canBreak = !refusal(source, projectId, null);
  const cfg = readPrefixConfig();
  const breakKey = cfg.bindings["pane.break"]
    ? `${tokenLabel(cfg.prefixes[0] ?? "")} ${tokenLabel(cfg.bindings["pane.break"])}`
    : undefined;

  return (
    <MenuSurface at={at} onClose={onClose} minWidth={200}>
      {canBreak && (
        <MenuItem
          label="Move to new tab"
          hint={breakKey}
          onClick={() => {
            onClose();
            moveToNewTab(source, projectId);
          }}
        />
      )}
      {canBreak && others.length > 0 && <MenuDivider />}
      {others.length > 0 && (
        <>
          <div className="micro px-3 pb-1 pt-[3px]" style={{ color: "var(--text-faint)" }}>
            MOVE TO TAB
          </div>
          <div style={{ maxHeight: "40vh", overflowY: "auto" }}>
            {others.map(({ t, i }) => (
              <MenuItem
                key={t.id}
                label={tabLabel(t, i, nameOf)}
                hint={String(i + 1)}
                onClick={() => {
                  onClose();
                  moveToTab(source, projectId, t.id);
                }}
              />
            ))}
          </div>
        </>
      )}
      {!canBreak && others.length === 0 && (
        <div className="micro px-3 py-[5px]" style={{ color: "var(--text-faint)" }}>
          the only pane in the only tab — nowhere to move it
        </div>
      )}
    </MenuSurface>
  );
}
