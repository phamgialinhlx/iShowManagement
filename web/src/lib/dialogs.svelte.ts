// In-app modal dialogs — promise-based replacements for window.confirm/prompt/
// alert. The macOS webview (WKWebView via wry) doesn't implement the JS dialog
// panels, so the native calls return immediately (false / null) without ever
// showing UI — which silently broke the notification toggle, password fields,
// and the docker remove/kill buttons in the desktop app. These render our own
// modal and work in every target. Mount <Dialog /> once (see App.svelte).

interface DialogReq {
  kind: 'confirm' | 'alert' | 'prompt'
  message: string
  okLabel: string
  danger: boolean
  value: string
  password: boolean
  resolve: (result: boolean | string | null) => void
}

let current = $state<DialogReq | null>(null)
const queue: DialogReq[] = []

function enqueue(req: DialogReq) {
  queue.push(req)
  if (!current) current = queue.shift()!
}

export const dialogState = {
  get current() {
    return current
  },
}

// Resolve the open dialog with the user's answer, then advance the queue.
export function settle(result: boolean | string | null) {
  const c = current
  current = queue.shift() ?? null
  c?.resolve(result)
}

export function confirmDialog(
  message: string,
  opts: { okLabel?: string; danger?: boolean } = {},
): Promise<boolean> {
  return new Promise((resolve) =>
    enqueue({
      kind: 'confirm',
      message,
      okLabel: opts.okLabel ?? 'OK',
      danger: opts.danger ?? false,
      value: '',
      password: false,
      resolve: (r) => resolve(r === true),
    }),
  )
}

export function alertDialog(message: string): Promise<void> {
  return new Promise((resolve) =>
    enqueue({
      kind: 'alert',
      message,
      okLabel: 'OK',
      danger: false,
      value: '',
      password: false,
      resolve: () => resolve(),
    }),
  )
}

export function promptDialog(
  message: string,
  opts: { value?: string; password?: boolean } = {},
): Promise<string | null> {
  return new Promise((resolve) =>
    enqueue({
      kind: 'prompt',
      message,
      okLabel: 'OK',
      danger: false,
      value: opts.value ?? '',
      password: opts.password ?? false,
      resolve: (r) => resolve(typeof r === 'string' ? r : null),
    }),
  )
}
