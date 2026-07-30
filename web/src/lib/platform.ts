/**
 * Platform detection for keyboard shortcuts.
 *
 * `navigator.userAgentData` is deliberately not used: it is Chromium-only and
 * `undefined` in the WKWebView that Tauri uses on macOS — exactly the platform
 * this needs to identify. `navigator.platform` is deprecated but universally
 * implemented and synchronous, which a keydown handler requires (an async probe
 * would leave shortcuts dead until it resolved).
 */
export const isMac = /mac/i.test(navigator.platform)

/** Label for the sidebar-toggle shortcut, for tooltips. */
export const toggleSideLabel = isMac ? '⌘B' : 'Ctrl+B'
