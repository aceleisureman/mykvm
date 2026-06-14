import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'

const nonPassiveCapture = { capture: true, passive: false } as AddEventListenerOptions

function preventDefaultZoomGesture(event: Event) {
  if (event.cancelable) {
    event.preventDefault()
  }
}

function preventWebviewPageZoom() {
  for (const eventName of ['gesturestart', 'gesturechange', 'gestureend']) {
    window.addEventListener(eventName, preventDefaultZoomGesture, nonPassiveCapture)
  }

  window.addEventListener(
    'wheel',
    (event) => {
      if (event.ctrlKey || event.metaKey) {
        preventDefaultZoomGesture(event)
      }
    },
    nonPassiveCapture,
  )

  window.addEventListener(
    'keydown',
    (event) => {
      if (!(event.metaKey || event.ctrlKey)) {
        return
      }
      if (
        ['=', '+', '-', '_', '0'].includes(event.key) ||
        ['Equal', 'Minus', 'Digit0', 'NumpadAdd', 'NumpadSubtract'].includes(event.code)
      ) {
        preventDefaultZoomGesture(event)
      }
    },
    nonPassiveCapture,
  )
}

preventWebviewPageZoom()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
