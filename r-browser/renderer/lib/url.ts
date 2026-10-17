/**
 * Display helpers for a URL in the chrome.
 *
 * **Turning what the operator typed into something loadable is NOT here** — it
 * lives once, in `shared/url.ts`, because main normalises too (`Tab.navigate`)
 * and two rule sets for one question is how the address bar comes to disagree
 * with what actually loaded. This file had its own `normaliseInput` for exactly
 * one commit; the two already differed on `localhost:3000`, which is the single
 * most typed string in this app. Same lesson as the login shell fixed in
 * `rmux-agent` while `start_resuming` kept its own copy.
 */

export function hostOf(url: string): string {
  try {
    return new URL(url).host || url
  } catch {
    return url
  }
}

/** Path + query, for a network row whose host column is elsewhere. */
export function pathOf(url: string): string {
  try {
    const u = new URL(url)
    return `${u.pathname}${u.search}`
  } catch {
    return url
  }
}
