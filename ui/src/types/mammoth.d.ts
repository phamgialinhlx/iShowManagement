/**
 * mammoth ships no types for its browser bundle.
 *
 * Only the one call rmux makes is declared. A wider `declare module` with `any`
 * would compile just as well and silently accept a misuse of the rest of the
 * API, which is the failure this file exists to prevent.
 */
declare module "mammoth/mammoth.browser.js" {
  export function convertToHtml(input: { arrayBuffer: ArrayBuffer }): Promise<{
    value: string;
    messages: { type: string; message: string }[];
  }>;
}
