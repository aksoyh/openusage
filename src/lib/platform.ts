/**
 * True on macOS, where shortcuts use Cmd/Option; Linux and Windows use Ctrl/Alt.
 * WKWebView reports "MacIntel", WebView2 "Win32", WebKitGTK "Linux x86_64".
 */
export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false
  return /mac/i.test(navigator.platform || navigator.userAgent || "")
}

/**
 * Color for the rendered tray icon, or `undefined` on macOS where the menu bar
 * recolors template icons itself. Linux panels are almost always dark; Windows
 * follows the system light/dark setting.
 */
export function getTrayIconTint(): readonly [number, number, number] | undefined {
  if (isMacPlatform()) return undefined
  const linux = /linux/i.test(navigator.platform || "")
  const dark =
    linux || (typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches)
  return dark ? [255, 255, 255] : [0, 0, 0]
}

/** Static tray icon shipped with the app: a template on macOS, full color elsewhere. */
export function getTrayStaticIconResource(): string {
  return isMacPlatform() ? "icons/tray-icon.png" : "icons/32x32.png"
}
