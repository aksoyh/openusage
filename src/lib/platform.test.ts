import { afterEach, describe, expect, it, vi } from "vitest"
import { getTrayIconTint, getTrayStaticIconResource, isMacPlatform } from "@/lib/platform"

function setPlatform(value: string) {
  Object.defineProperty(window.navigator, "platform", {
    configurable: true,
    get: () => value,
  })
}

describe("isMacPlatform", () => {
  afterEach(() => setPlatform("MacIntel"))

  it("detects macOS webviews", () => {
    setPlatform("MacIntel")
    expect(isMacPlatform()).toBe(true)
  })

  it("is false on Windows and Linux webviews", () => {
    setPlatform("Win32")
    expect(isMacPlatform()).toBe(false)
    setPlatform("Linux aarch64")
    expect(isMacPlatform()).toBe(false)
  })
})

describe("tray icon platform helpers", () => {
  afterEach(() => {
    Object.defineProperty(window.navigator, "platform", { configurable: true, get: () => "MacIntel" })
    vi.restoreAllMocks()
  })

  function setPlatformTo(value: string) {
    Object.defineProperty(window.navigator, "platform", { configurable: true, get: () => value })
  }

  it("leaves macOS template icons untinted", () => {
    setPlatformTo("MacIntel")
    expect(getTrayIconTint()).toBeUndefined()
    expect(getTrayStaticIconResource()).toBe("icons/tray-icon.png")
  })

  it("draws white icons on Linux panels", () => {
    setPlatformTo("Linux x86_64")
    expect(getTrayIconTint()).toEqual([255, 255, 255])
    expect(getTrayStaticIconResource()).toBe("icons/32x32.png")
  })

  it("follows the system theme on Windows", () => {
    setPlatformTo("Win32")
    const matchMedia = vi.spyOn(window, "matchMedia")
    matchMedia.mockReturnValue({ matches: true } as MediaQueryList)
    expect(getTrayIconTint()).toEqual([255, 255, 255])
    matchMedia.mockReturnValue({ matches: false } as MediaQueryList)
    expect(getTrayIconTint()).toEqual([0, 0, 0])
  })
})
