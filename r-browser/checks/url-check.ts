/**
 * `normaliseUrl` — what the operator typed, turned into something loadable.
 *
 * Pinned by a harness because the rules pull against each other: a development
 * browser must treat `localhost:3000` as an address while a consumer browser
 * treats a colon like that as a search, and both behaviours are defensible in
 * isolation. The test is the only place that disagreement is settled.
 */
import { normaliseUrl } from '../shared/url'

let failed = 0
const is = (input: string, expected: string, why: string) => {
  const actual = normaliseUrl(input)
  const ok = actual === expected
  if (!ok) failed++
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${JSON.stringify(input).padEnd(34)} -> ${actual}${ok ? '' : `\n      expected ${expected}\n      (${why})`}`)
}

console.log('normaliseUrl\n')

// The single most typed string in this app. Treating it as a search would be
// wrong on the most common input there is.
is('localhost:3000', 'http://localhost:3000', 'the dev server case')
is('localhost', 'http://localhost', 'bare localhost')
is('localhost:5273/office-check.html', 'http://localhost:5273/office-check.html', 'path preserved')

// http, not https: a dev server almost never has a certificate, and an https
// upgrade here fails with a TLS error that reads as "the server is down".
is('127.0.0.1:8080', 'http://127.0.0.1:8080', 'loopback by address')
is('192.168.1.40:3000', 'http://192.168.1.40:3000', 'a LAN device')

// An explicit scheme is never second-guessed.
is('https://example.com', 'https://example.com', 'already absolute')
is('http://example.com', 'http://example.com', 'http stays http')
is('about:blank', 'about:blank', 'about: is a scheme')
is('file:///tmp/x.html', 'file:///tmp/x.html', 'file: is a scheme')
is('chrome://gpu', 'chrome://gpu', 'chrome: is a scheme')
is('data:text/html,<b>x', 'data:text/html,<b>x', 'data: is a scheme')

// A public host gets https, because everything public is https now and
// defaulting to http costs a redirect on every single navigation.
is('example.com', 'https://example.com', 'a bare domain')
is('example.com/a/b', 'https://example.com/a/b', 'domain with a path')
is('sub.example.co.uk', 'https://sub.example.co.uk', 'multi-label domain')

// Anything that is not host-shaped is a search. The test is a dot, because a
// word with no dot cannot be a host that resolves.
is('how do i center a div', 'https://duckduckgo.com/?q=how%20do%20i%20center%20a%20div', 'a sentence')
is('react', 'https://duckduckgo.com/?q=react', 'one bare word')

// Whitespace is the operator's, not the URL's — a pasted line routinely
// carries a trailing newline and a URL with one in it does not load.
is('  example.com  ', 'https://example.com', 'trimmed')
is('', 'about:blank', 'empty is a blank page, never a search for nothing')

// The regression that prompted the reordering. `localhost` is a legal scheme
// name, so a scheme-first reading passes this through untouched and it loads
// nothing at all.
is('localhost:3000/api?x=1', 'http://localhost:3000/api?x=1', 'query survives')
is('[::1]:8080', 'http://[::1]:8080', 'ipv6 loopback')
is('build-box.local:3000', 'http://build-box.local:3000', 'a named box on the LAN')
is('mailto:a@b.com', 'mailto:a@b.com', 'a real non-http scheme still passes')
is('example.com:8080/x', 'http://example.com:8080/x', 'explicit port means a dev server, so http')

console.log(`\n${failed ? `${failed} FAILED` : 'all passed'}`)
process.exit(failed ? 1 : 0)
