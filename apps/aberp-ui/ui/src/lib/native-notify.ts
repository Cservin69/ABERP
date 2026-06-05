// S256 / PR-245 — native OS notification on quote arrival (brief §B.10).
//
// PUSHBACK / conservative choice: the brief names "Tauri notification
// API" (`tauri-plugin-notification`). That plugin is NOT in the offline
// cargo/npm cache; adding it would break the `cargo clippy/test
// --workspace` gates (un-fetchable dep). The Web Notifications API works
// inside the WKWebView the Tauri shell embeds, needs ZERO new
// dependency, and — critically for §B.10 — a notification posted from a
// backgrounded webview still lands in macOS Notification Center. If a
// future PR needs strictly-native behaviour, swap the body of
// `fireNativeNotification` for an `invoke("notify_native", …)` once the
// plugin is available.
//
// Permission flow (§B.10): prompt on first enable; the browser persists
// granted/denied across launches (so we don't re-prompt if denied — we
// just disable the toggle with a note). No bespoke permission storage.

export type NativePermission = "unsupported" | "default" | "granted" | "denied";

function api(): typeof Notification | null {
  if (typeof window === "undefined") return null;
  const n = (window as unknown as { Notification?: typeof Notification }).Notification;
  return n ?? null;
}

export function nativeNotificationsSupported(): boolean {
  return api() !== null;
}

/** Current OS-granted permission state (or "unsupported"). */
export function nativePermission(): NativePermission {
  const n = api();
  if (n === null) return "unsupported";
  return n.permission as NativePermission;
}

/** Request permission if it's still "default". Returns the resulting
 * state. Never re-prompts once the user has decided (the API itself
 * resolves immediately to the stored decision). */
export async function ensureNativePermission(): Promise<NativePermission> {
  const n = api();
  if (n === null) return "unsupported";
  if (n.permission !== "default") return n.permission as NativePermission;
  try {
    const result = await n.requestPermission();
    return result as NativePermission;
  } catch {
    return n.permission as NativePermission;
  }
}

/** Post a native notification if permission is granted. No-op otherwise.
 * Best-effort — never throws into the caller's poll loop. */
export function fireNativeNotification(title: string, body: string): void {
  const n = api();
  if (n === null || n.permission !== "granted") return;
  try {
    // eslint-disable-next-line no-new
    new n(title, { body });
  } catch {
    // Some webview configs throw on construction; swallow.
  }
}
