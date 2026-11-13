/**
 * Light and dark follow the device (`prefers-color-scheme`), using the same
 * herdr palettes and the same derived tokens as the desktop — Catppuccin Latte
 * and Mocha, the desktop's own auto-switch defaults. A phone has no theme file
 * and no Settings window, so there is nothing to reconcile against.
 */
import { DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME } from "../lib/theme";
import { applyTheme, resolve } from "../lib/theme-runtime";

export function followSystemTheme(): void {
  const dark = window.matchMedia("(prefers-color-scheme: dark)");
  const apply = () => applyTheme(resolve(dark.matches ? DEFAULT_DARK_THEME : DEFAULT_LIGHT_THEME));
  apply();
  dark.addEventListener("change", apply);
}
