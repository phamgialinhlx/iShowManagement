import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App'
import './styles.css'

/*
 * `StrictMode` is on deliberately, even though it double-invokes effects in
 * development. Every subscription in this app returns an unsubscribe from the
 * preload's `on` helper, and a listener that is registered twice and removed
 * once shows up as duplicated console lines — which is exactly the bug StrictMode
 * exists to surface, and exactly the bug that is invisible in production until
 * someone reloads the chrome.
 */
const host = document.getElementById('root')
if (!host) throw new Error('#root missing from index.html')

// Mark the OS so the top bar can inset past the macOS traffic lights, which a
// `hiddenInset` titlebar overlays onto the window's top-left corner.
if (window.rbrowse?.platform === 'darwin') {
  document.documentElement.classList.add('is-mac')
}

createRoot(host).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
