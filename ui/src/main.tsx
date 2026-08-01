import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./styles/fonts.css";
import "./styles/signal-room.css";
import { App } from "./App";
import { Settings } from "./screens/Settings";
import { applyAppearance } from "./components/AppearancePanel";
import { applyUserCss } from "./lib/user-css";

// Before the first paint: applying this in an effect would flash the default
// glass on every launch.
applyAppearance();
// Last, so it sits after the design system in document order — which is the
// whole mechanism by which it overrides anything.
applyUserCss();

/*
 * One bundle, two windows.
 *
 * Settings is a separate Tauri window that loads this same entry point, so the
 * window's own label decides what to render. The label string is the contract
 * with `src-tauri/src/settings_window.rs` — if the two ever disagree, the
 * settings window renders a second workbench, terminals and all.
 *
 * Read from the URL rather than the Tauri API because it must be known
 * synchronously, before the first render: awaiting it would flash the workbench
 * inside the settings window.
 */
const isSettingsWindow =
  new URLSearchParams(window.location.search).get("window") === "settings";

/*
 * Pause every infinite animation while the window is hidden. The design leans on
 * continuous motion (breathing meters, equalizers, scanlines) and without this
 * the compositor keeps rendering all of it behind other windows. The CSS side of
 * the switch lives in signal-room.css.
 */
const syncVisibility = () => {
  document.body.dataset.hidden = String(document.hidden);
};
document.addEventListener("visibilitychange", syncVisibility);
syncVisibility();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    {isSettingsWindow ? <Settings /> : <App />}
  </StrictMode>,
);
