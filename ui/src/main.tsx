import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "./styles/fonts.css";
import "./styles/signal-room.css";
import { App } from "./App";

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
    <App />
  </StrictMode>,
);
