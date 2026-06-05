import { describe, expect, it } from "vitest";

import {
  DEFAULT_NOTIFICATION_PREFS,
  loadNotificationPrefs,
  saveNotificationPrefs,
} from "./notification-prefs";

function memStorage() {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    _map: map,
  };
}

describe("notification-prefs", () => {
  it("defaults both toggles OFF", () => {
    expect(DEFAULT_NOTIFICATION_PREFS).toEqual({
      nativeEnabled: false,
      soundEnabled: false,
    });
  });

  it("returns defaults on a fresh (empty) store", () => {
    expect(loadNotificationPrefs(memStorage())).toEqual(
      DEFAULT_NOTIFICATION_PREFS,
    );
  });

  it("returns defaults when storage is null", () => {
    expect(loadNotificationPrefs(null)).toEqual(DEFAULT_NOTIFICATION_PREFS);
  });

  it("round-trips both toggles", () => {
    const s = memStorage();
    saveNotificationPrefs({ nativeEnabled: true, soundEnabled: true }, s);
    expect(loadNotificationPrefs(s)).toEqual({
      nativeEnabled: true,
      soundEnabled: true,
    });
  });

  it("coerces non-boolean / missing fields to false (closed vocab)", () => {
    const s = memStorage();
    s._map.set(
      "aberp:notifications:prefs",
      JSON.stringify({ nativeEnabled: "yes", soundEnabled: 1, extra: "x" }),
    );
    expect(loadNotificationPrefs(s)).toEqual({
      nativeEnabled: false,
      soundEnabled: false,
    });
  });

  it("never round-trips unknown keys back to storage", () => {
    const s = memStorage();
    saveNotificationPrefs(
      { nativeEnabled: true, soundEnabled: false } as never,
      s,
    );
    const stored = JSON.parse(
      s._map.get("aberp:notifications:prefs") as string,
    );
    expect(Object.keys(stored).sort()).toEqual([
      "nativeEnabled",
      "soundEnabled",
    ]);
  });

  it("tolerates a malformed blob", () => {
    const s = memStorage();
    s._map.set("aberp:notifications:prefs", "{not json");
    expect(loadNotificationPrefs(s)).toEqual(DEFAULT_NOTIFICATION_PREFS);
  });
});
