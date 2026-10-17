/**
 * The chrome's whole world, declared once.
 *
 * `preload-chrome.ts` exports `RBrowseApi` as `typeof api`, so this declaration
 * is not a hand-written mirror that can drift — it *is* the preload's shape. If
 * a channel is added there, it appears here; if one is renamed, every call site
 * in the renderer fails to compile rather than hanging on a dead channel, which
 * is the failure mode `contract.ts` warns about.
 */
import type { RBrowseApi } from '../main/preload-chrome'

declare global {
  interface Window {
    rbrowse: RBrowseApi
  }
}

export {}
