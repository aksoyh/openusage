import "@testing-library/jest-dom/vitest"
import { cleanup } from "@testing-library/react"
import { afterEach, vi } from "vitest"

// Mock @aptabase/tauri globally — it calls window.__TAURI_IPC__ which doesn't exist in jsdom
vi.mock("@aptabase/tauri", () => ({
  trackEvent: vi.fn(),
}))

// Tests describe macOS behavior (Cmd/Option labels) unless they override this.
Object.defineProperty(window.navigator, "platform", {
  configurable: true,
  get: () => "MacIntel",
})

afterEach(() => {
  cleanup()
})

class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}

if (!globalThis.ResizeObserver) {
  globalThis.ResizeObserver = ResizeObserverMock as typeof ResizeObserver
}

if (!window.matchMedia) {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }),
  })
}
