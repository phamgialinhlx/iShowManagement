import { useEffect } from "react";

/**
 * Keep the app exactly the size of what is visible — above the on-screen keyboard.
 *
 * Chrome for Android honours `interactive-widget=resizes-content` and shrinks the
 * layout viewport itself. iOS Safari does not: the keyboard overlays the page and
 * only `visualViewport` says how much is left, and where (it may also scroll the
 * page up under the keyboard). So the shell is `position: fixed` at
 * `--vv-top` with height `--vv-height`, and the fixed-bottom composer therefore
 * sits on top of the keyboard on both.
 */
export function useVisualViewport(): void {
  useEffect(() => {
    const root = document.documentElement;
    const vv = window.visualViewport;
    const update = () => {
      root.style.setProperty("--vv-height", `${Math.round(vv ? vv.height : window.innerHeight)}px`);
      root.style.setProperty("--vv-top", `${Math.round(vv ? vv.offsetTop : 0)}px`);
    };
    update();
    vv?.addEventListener("resize", update);
    vv?.addEventListener("scroll", update);
    window.addEventListener("resize", update);
    return () => {
      vv?.removeEventListener("resize", update);
      vv?.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
    };
  }, []);
}
