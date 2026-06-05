// S256 / PR-245 — pure logic behind the quote-arrival toast (brief
// §B.7 / §B.8). The backend's `/api/quote-intake/notifications` already
// excludes catch-up arrivals (anything before the boot + grace
// boundary) and already-picked-up quotes. This module adds the
// SPA-side de-dup so:
//
//   - two poll cycles completing seconds apart coalesce into ONE toast
//     (the adversarial "toast race" note), and
//   - a page reload mid-session does NOT re-toast arrivals the operator
//     already saw (the §B.8 belt-and-suspenders) — the seen-set is
//     persisted to localStorage.
//
// Kept framework-free + storage-injectable so it is unit-testable
// without a DOM (mirrors the workshop-demo-mode / list-persistence
// helpers' posture).

import type { QuoteArrival } from "./api";

const SEEN_KEY = "aberp:quote-intake:toasted";

/** A minimal Storage shape so tests can inject a stub. */
type StorageLike = Pick<Storage, "getItem" | "setItem">;

function defaultStorage(): StorageLike | null {
  try {
    if (typeof localStorage !== "undefined") return localStorage;
  } catch {
    // Private-browsing / disabled storage — degrade to in-memory only.
  }
  return null;
}

/** Load the set of quote_ids already toasted (persisted across reloads
 * so a refresh doesn't replay old arrivals). Defensive: any parse error
 * yields an empty set rather than throwing. */
export function loadSeen(storage: StorageLike | null = defaultStorage()): Set<string> {
  if (storage === null) return new Set();
  try {
    const raw = storage.getItem(SEEN_KEY);
    if (raw === null) return new Set();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((v): v is string => typeof v === "string"));
  } catch {
    return new Set();
  }
}

/** Persist the seen-set. Caps at the most recent 200 ids so the key
 * can't grow unbounded over a long-lived session. Best-effort. */
export function saveSeen(
  seen: Set<string>,
  storage: StorageLike | null = defaultStorage(),
): void {
  if (storage === null) return;
  try {
    const arr = Array.from(seen);
    const capped = arr.length > 200 ? arr.slice(arr.length - 200) : arr;
    storage.setItem(SEEN_KEY, JSON.stringify(capped));
  } catch {
    // Quota / disabled — the toast still fired; we just can't dedup
    // across a reload. Acceptable degradation.
  }
}

/** Pure: arrivals not yet in `seen`. These are the ones to toast. */
export function freshArrivals(
  arrivals: QuoteArrival[],
  seen: Set<string>,
): QuoteArrival[] {
  return arrivals.filter((a) => !seen.has(a.quote_id));
}

/** Bilingual coalesced toast copy for `count` new arrivals. */
export function arrivalToastMessage(count: number): { hu: string; en: string } {
  if (count <= 1) {
    return {
      hu: "1 új ajánlat érkezett — kattints a megnyitáshoz",
      en: "1 new quote — click to view",
    };
  }
  return {
    hu: `${count} új ajánlat érkezett — kattints a megnyitáshoz`,
    en: `${count} new quotes — click to view`,
  };
}
