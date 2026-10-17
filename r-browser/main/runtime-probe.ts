/**
 * Runtime framework detection, in the page's **main world**.
 *
 * This exists because of a hard boundary that decides the whole architecture:
 * `__reactFiber$…`, `__vnode`, `__vueParentComponent`, `__svelte_meta`,
 * `__ngContext__` and `window.ng` are **expando properties** the page's own
 * scripts hang on the node. A Chromium *isolated world* — which is what our
 * preload is, with `contextIsolation: true` and `sandbox: true` — shares the
 * DOM node but not the per-world expandos. From the preload they are not
 * `undefined`, they are *not there*, and every probe returns nothing on every
 * page, silently and forever. `data-*` attributes are real DOM state and do
 * cross, which is exactly why the field drifted to build-time stamping.
 *
 * So the attribute probes live in `page/source-hint.ts` where they work, and
 * these run here, over CDP `Runtime.evaluate`, against the main world.
 *
 * ## The element is addressed by an attribute, not a selector
 *
 * The picker stamps `data-rb-pick` on what it picked before it reports. An
 * attribute crosses worlds, so the main world can find the exact node; a CSS
 * selector would have to be re-resolved and could match a different element, or
 * none, on a page that re-rendered between the click and this call.
 *
 * ## What is deliberately NOT attempted
 *
 * React 19 removed `_debugSource` (verified: zero occurrences in this repo's
 * `react-dom@19.2.8`), leaving `_debugStack` — an `Error` whose frames point
 * into the *bundle*. Turning those into source locations means fetching each
 * bundle, decoding its source map, and filtering the JSX factory frames out of
 * the top. The best-known tool that does this emits **no line number at all**
 * under Vite because the numbers come out unreliable, and a wrong `:42` is
 * worse than none: it stops the agent grepping, which is the one thing it is
 * reliably good at. So React yields a component *name* here and no file, and
 * the file comes from a build-time attribute or not at all.
 */
import type { SourceHint } from '../shared/contract'

/** The attribute the picker stamps. Also cleared by this module after a probe,
 *  so a stale marker cannot make the next pick probe the previous element. */
export const PICK_ATTR = 'data-rb-pick'

/**
 * Built as a string because it is evaluated in a world this process cannot
 * import into. Kept as one expression with no external references so it can be
 * handed straight to `Runtime.evaluate`.
 *
 * Every probe is wrapped: a page may define any of these as a throwing getter,
 * and one hostile property must not lose the other four.
 */
export const PROBE_EXPRESSION = `(() => {
  var out = null;
  try {
    var el = document.querySelector('[${PICK_ATTR}]');
    if (!el) return null;

    var pick = function (fn) { try { return fn(); } catch (e) { return undefined; } };
    var useful = function (n) {
      return typeof n === 'string' && n.length > 0 && n !== 'Unknown' &&
             n.charAt(0) !== '_' && !/^(Anonymous|Object)$/.test(n);
    };

    // Svelte first: it is the only one that hands over an exact file, line and
    // column with no source maps and no guessing, so it is never worth asking
    // anything else once it has answered.
    var svelte = pick(function () { return el.__svelte_meta; });
    if (svelte && svelte.loc && svelte.loc.file) {
      return {
        framework: 'svelte', via: 'Svelte __svelte_meta.loc',
        file: String(svelte.loc.file),
        line: typeof svelte.loc.line === 'number' ? svelte.loc.line : undefined,
        column: typeof svelte.loc.column === 'number' ? svelte.loc.column : undefined,
      };
    }

    // Vue 3, then Vue 2. \`__file\` is a real source path under a dev build.
    var vue3 = pick(function () { return el.__vueParentComponent; });
    if (vue3) {
      var t = vue3.type || {};
      var owners3 = [];
      var p = pick(function () { return vue3.parent; });
      for (var i = 0; i < 4 && p; i++) {
        var pn = (p.type && (p.type.name || p.type.__name)) || undefined;
        if (useful(pn)) owners3.push(pn);
        p = pick(function () { return p.parent; });
      }
      return {
        framework: 'vue', via: 'Vue __vueParentComponent',
        component: useful(t.name) ? t.name : (useful(t.__name) ? t.__name : undefined),
        file: typeof t.__file === 'string' ? t.__file : undefined,
        owners: owners3.length ? owners3 : undefined,
      };
    }
    var vue2 = pick(function () { return el.__vue__; });
    if (vue2 && vue2.$options) {
      return {
        framework: 'vue', via: 'Vue 2 __vue__.$options',
        component: useful(vue2.$options.name) ? vue2.$options.name : undefined,
        file: typeof vue2.$options.__file === 'string' ? vue2.$options.__file : undefined,
      };
    }

    // React: the fiber key is randomised per renderer instance, so it has to be
    // found by prefix. Walk \`_debugOwner\` rather than \`return\`: the owner
    // chain is who *wrote the JSX*, which is what a reader can grep for, while
    // \`return\` is the render tree and is full of host and wrapper nodes.
    var fiber = pick(function () {
      for (var k in el) {
        if (k.indexOf('__reactFiber$') === 0 || k.indexOf('__reactInternalInstance$') === 0) return el[k];
      }
      return null;
    });
    if (fiber) {
      var names = [];
      var f = fiber;
      for (var j = 0; j < 24 && f && names.length < 5; j++) {
        var ty = pick(function () { return f.type; });
        if (typeof ty === 'function' || (ty && typeof ty === 'object' && ty.render)) {
          var nm = (ty.displayName || ty.name);
          if (useful(nm) && names.indexOf(nm) === -1) names.push(nm);
        }
        f = pick(function () { return f._debugOwner || f.return; });
      }
      if (names.length) {
        return {
          framework: 'react',
          via: 'React fiber _debugOwner chain (names only — React 19 removed _debugSource)',
          component: names[0],
          owners: names.slice(1),
        };
      }
      return { framework: 'react', via: 'React fiber (no named owner found)' };
    }

    // Angular: a name and nothing more. Nothing in Angular carries a file.
    var ng = pick(function () { return window.ng; });
    if (ng && typeof ng.getComponent === 'function') {
      var cmp = pick(function () { return ng.getComponent(el) || ng.getOwningComponent(el); });
      var cn = cmp && cmp.constructor && cmp.constructor.name;
      if (useful(cn)) return { framework: 'angular', via: 'window.ng.getComponent', component: cn };
    }
  } catch (e) {
    out = null;
  }
  return out;
})()`

/** Clears the marker. Run unconditionally after a probe — a marker left behind
 *  makes the *next* pick resolve to the previous element, which is a wrong
 *  answer rather than a missing one. */
export const CLEAR_EXPRESSION = `(() => {
  try {
    var n = document.querySelectorAll('[${PICK_ATTR}]');
    for (var i = 0; i < n.length; i++) n[i].removeAttribute('${PICK_ATTR}');
  } catch (e) {}
  return true;
})()`

/**
 * Narrow whatever the page handed back.
 *
 * The probe runs in a document we do not control, so its result is untrusted
 * input even though we wrote the expression — a page can shadow `document.
 * querySelector` and return anything at all. Every field is checked and
 * anything unexpected is dropped rather than carried into a prompt.
 */
export function toSourceHint(raw: unknown): SourceHint | undefined {
  if (!raw || typeof raw !== 'object') return undefined
  const r = raw as Record<string, unknown>
  const framework = typeof r['framework'] === 'string' ? r['framework'] : undefined
  const via = typeof r['via'] === 'string' ? r['via'] : undefined
  if (!framework || !via) return undefined

  const str = (v: unknown, cap: number): string | undefined =>
    typeof v === 'string' && v.length > 0 && v.length <= cap ? v : undefined
  const num = (v: unknown): number | undefined =>
    typeof v === 'number' && Number.isFinite(v) && v >= 0 ? Math.round(v) : undefined

  const file = str(r['file'], 400)
  const hint: SourceHint = { framework, via }
  const component = str(r['component'], 120)
  if (component) hint.component = component
  // A bundle path is refused here as well as in the attribute parser: it points
  // at build output, and the single worst thing this payload can do is send an
  // agent to edit a file that is regenerated on the next save.
  if (file && !looksLikeBundlePath(file)) hint.file = file
  const line = num(r['line'])
  if (hint.file && line) hint.line = line
  const column = num(r['column'])
  if (hint.file && column) hint.column = column

  const owners = Array.isArray(r['owners'])
    ? r['owners'].map((o) => str(o, 120)).filter((o): o is string => !!o).slice(0, 4)
    : []
  if (owners.length) hint.owners = owners

  return hint.component || hint.file ? hint : undefined
}

/**
 * Anything that is plainly build output rather than source.
 *
 * Duplicated in spirit with `page/source-hint.ts`'s own check, deliberately:
 * these are two different worlds with two different inputs, and a shared
 * import would mean the preload bundle carrying main-process code. The rule is
 * one line; the coupling would not be.
 */
export function looksLikeBundlePath(path: string): boolean {
  return (
    // `(^|\/)` and not a bare `\/`: a relative path is the common shape here —
    // `dist/app.js` and `build/static/js/2.chunk.js` both arrive with no
    // leading slash, and requiring one let the two most obvious build
    // directories through. Caught by `runtime-probe-check.ts`.
    /(^|\/)(assets|dist|build|out|_next\/static|\.vite\/deps|node_modules)\//.test(path) ||
    /[.-][0-9a-f]{8,}\.(js|mjs|cjs|css)$/i.test(path) ||
    /^https?:\/\/[^/]+\/[^/]*\.(js|mjs)$/i.test(path)
  )
}
