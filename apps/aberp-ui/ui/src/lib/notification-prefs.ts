// S256 / PR-245 — operator notification preferences (brief §B.10/§B.11).
//
// These are PER-MACHINE desktop preferences — whether ABERP fires a
// native OS notification + a chime on a new quote arrival. They live in
// localStorage, NOT seller.toml: the seller.toml four-way write
// invariant ([[seller-toml-write-invariant]]) is a known landmine, and
// "should THIS desktop beep" is not tenant config that belongs in a
// regulated, multi-writer file. Both default OFF.
//
// Storage-injectable + closed-vocab/defensive parse, mirroring the
// invoice-list-persistence helpers.

type StorageLike = Pick<Storage, "getItem" | "setItem">;

const PREFS_KEY = "aberp:notifications:prefs";

export interface NotificationPrefs {
  /** Fire a native OS notification (survives backgrounding) on arrival. */
  nativeEnabled: boolean;
  /** Play a single subtle chime on quote arrival (suppressed in demo mode). */
  soundEnabled: boolean;
  /** S258 / PR-247 — play a single alert tone when a Workshop adapter
   *  transitions into a degraded/unhealthy state (suppressed in demo
   *  mode + during the boot-grace catch-up, like the arrival chime). */
  adapterSoundEnabled: boolean;
}

export const DEFAULT_NOTIFICATION_PREFS: NotificationPrefs = {
  nativeEnabled: false,
  soundEnabled: false,
  adapterSoundEnabled: false,
};

function defaultStorage(): StorageLike | null {
  try {
    if (typeof localStorage !== "undefined") return localStorage;
  } catch {
    // disabled / private browsing
  }
  return null;
}

function coerceBool(v: unknown): boolean {
  return v === true;
}

export function loadNotificationPrefs(
  storage: StorageLike | null = defaultStorage(),
): NotificationPrefs {
  if (storage === null) return { ...DEFAULT_NOTIFICATION_PREFS };
  try {
    const raw = storage.getItem(PREFS_KEY);
    if (raw === null) return { ...DEFAULT_NOTIFICATION_PREFS };
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object") {
      return { ...DEFAULT_NOTIFICATION_PREFS };
    }
    const obj = parsed as Record<string, unknown>;
    return {
      nativeEnabled: coerceBool(obj.nativeEnabled),
      soundEnabled: coerceBool(obj.soundEnabled),
      adapterSoundEnabled: coerceBool(obj.adapterSoundEnabled),
    };
  } catch {
    return { ...DEFAULT_NOTIFICATION_PREFS };
  }
}

export function saveNotificationPrefs(
  prefs: NotificationPrefs,
  storage: StorageLike | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    // Re-serialize only the closed-vocab keys so a hand-edited or
    // future-versioned blob never round-trips unknown fields.
    const clean: NotificationPrefs = {
      nativeEnabled: prefs.nativeEnabled === true,
      soundEnabled: prefs.soundEnabled === true,
      adapterSoundEnabled: prefs.adapterSoundEnabled === true,
    };
    storage.setItem(PREFS_KEY, JSON.stringify(clean));
  } catch {
    // best-effort
  }
}
