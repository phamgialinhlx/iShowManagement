/**
 * What the operator typed, turned into something that can be loaded.
 *
 * Deliberately simple and deliberately biased toward *localhost*: this is a
 * development browser, and `localhost:3000` is the single most typed string in
 * it. Treating that as a search — which every consumer browser does, because
 * it has no colon-port heuristic — would be wrong on the most common input.
 */
export function normaliseUrl(raw: string): string {
  const value = raw.trim()
  if (!value) return 'about:blank'

  // **Host-and-port is tested before scheme, and that order is the whole
  // bug.** `localhost:3000` satisfies any reasonable "starts with a scheme"
  // pattern — `localhost` is a legal scheme name and it is followed by a colon
  // — so a scheme-first reading hands Electron `localhost:` as a protocol,
  // which loads nothing and reports nothing. It failed on the single most
  // typed string in a development browser while passing on every public URL,
  // which is exactly the shape of bug that ships. `url-check.ts` pins it.
  if (/^localhost(:\d+)?([/?#]|$)/i.test(value)) return `http://${value}`
  if (/^\d{1,3}(\.\d{1,3}){3}(:\d+)?([/?#]|$)/.test(value)) return `http://${value}`
  if (/^\[[0-9a-f:]+\](:\d+)?([/?#]|$)/i.test(value)) return `http://${value}`

  // A scheme, now that the host:port forms are out of the way. The lookahead
  // is belt and braces for the same confusion: a "scheme" whose body is only
  // digits is a port, never a protocol.
  if (/^[a-z][a-z0-9+.-]*:(?!\d)/i.test(value)) return value

  // A bare host carrying an explicit port is a dev server too — a machine on
  // the LAN by name, which is how a phone-testing box is usually reached.
  if (/^[^\s/:]+\.[^\s/:]{2,}:\d+([/?#]|$)/.test(value)) return `http://${value}`
  if (/^[^\s/]+\.[^\s/]{2,}(\/|$|:)/.test(value)) return `https://${value}`
  return `https://duckduckgo.com/?q=${encodeURIComponent(value)}`
}
