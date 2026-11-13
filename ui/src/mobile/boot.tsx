// The mobile app proper, loaded by `ui/mobile/main.tsx` once the web shell has
// seeded storage. Same fonts and design tokens as the desktop bundle.
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import "../styles/fonts.css";
import "../styles/signal-room.css";
import "../styles/layout.css";
import "./mobile.css";
import { followSystemTheme } from "./theme";
import { MobileApp } from "./MobileApp";

// Before the first paint, so the page never flashes the other palette.
followSystemTheme();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <MobileApp />
  </StrictMode>,
);
