// S238 / PR-232 — Workshop demo-mode toggle.
//
// Operator-only theater for the Műhely (Workshop) dashboard. A
// 5-tap-within-2s gesture on a designated "secret handle" element
// flips a localStorage flag; the SPA's `getWorkshopDashboard()`
// then short-circuits to mock data instead of the real Tauri
// invoke. Reload-survives so the operator can refresh mid-tour
// without exposing real numbers.
//
// Storage-injectable per S175 / S179 / S211 convention; tests stub
// `Storage`. The tap detector accepts an injectable `now()` so
// timer-mock pins drive it deterministically.
//
// Trigger element: the Workshop page H2 (`#ws-page-title`). The
// brief floated the global ABERP wordmark in `App.svelte` but that
// would couple global chrome to a route-local feature; scoping to
// the page heading keeps the blast radius surgical per CLAUDE.md
// rule 3.

export const DEMO_MODE_KEY = "aberp:workshop:demo-mode";

export type DemoMode = "on" | "off";

export const DEFAULT_DEMO_MODE: DemoMode = "off";

const LEGAL_VALUES: readonly DemoMode[] = ["on", "off"];

/** Read the persisted demo-mode flag. Closed-vocab discard pattern
 *  from [[invoice-list-persistence-s175]]: a stored value outside
 *  `{on, off}` falls back to `off` rather than coercing — a corrupt
 *  flag must not silently expose mock data in real mode. */
export function loadDemoMode(
  storage: Pick<Storage, "getItem"> | null = localStorageOrNull(),
): DemoMode {
  if (storage === null) return DEFAULT_DEMO_MODE;
  let raw: string | null;
  try {
    raw = storage.getItem(DEMO_MODE_KEY);
  } catch (_e) {
    return DEFAULT_DEMO_MODE;
  }
  if (raw === null) return DEFAULT_DEMO_MODE;
  if (LEGAL_VALUES.includes(raw as DemoMode)) return raw as DemoMode;
  return DEFAULT_DEMO_MODE;
}

export function saveDemoMode(
  value: DemoMode,
  storage: Pick<Storage, "setItem"> | null = localStorageOrNull(),
): void {
  if (storage === null) return;
  try {
    storage.setItem(DEMO_MODE_KEY, value);
  } catch (e) {
    // eslint-disable-next-line no-console
    console.warn("aberp: failed to persist workshop demo-mode", e);
  }
}

/** Convenience boolean wrapper used by `api.ts` to decide whether
 *  `getWorkshopDashboard()` should return mock data. */
export function isDemoMode(
  storage: Pick<Storage, "getItem"> | null = localStorageOrNull(),
): boolean {
  return loadDemoMode(storage) === "on";
}

export function setDemoMode(
  on: boolean,
  storage: Pick<Storage, "setItem"> | null = localStorageOrNull(),
): void {
  saveDemoMode(on ? "on" : "off", storage);
}

// ── Tap detector ────────────────────────────────────────────────

export interface TapDetectorOptions {
  /** Sliding-window length in ms. Default 2000. */
  windowMs?: number;
  /** Taps-within-window required to trigger. Default 5. */
  threshold?: number;
  /** Injectable clock — tests pass a controlled stamp source so
   *  fake-timer drift doesn't false-positive the detector. */
  now?: () => number;
}

export interface TapDetector {
  /** Register a click. Returns true if this click reached the
   *  threshold (and the internal counter has been reset for the
   *  next gesture). */
  tap(): boolean;
  /** Drop all pending taps. Useful when the operator navigates
   *  away mid-gesture. */
  reset(): void;
}

/** Build a sliding-window tap detector. When the operator clicks
 *  the trigger element `threshold` times within `windowMs`, the
 *  detector fires `onTrigger()` exactly once per gesture. Taps
 *  older than the window age out as new taps arrive — so a slow
 *  tap sequence never accidentally accumulates to the threshold. */
export function createTapDetector(
  onTrigger: () => void,
  opts: TapDetectorOptions = {},
): TapDetector {
  const windowMs = opts.windowMs ?? 2000;
  const threshold = opts.threshold ?? 5;
  const now = opts.now ?? (() => Date.now());
  let stamps: number[] = [];

  function tap(): boolean {
    const t = now();
    const cutoff = t - windowMs;
    // Drop expired taps from the head. `findIndex` would also work
    // but a small loop is cheaper than allocating a slice for the
    // typical 0-5 entry array.
    while (stamps.length > 0 && stamps[0] < cutoff) {
      stamps.shift();
    }
    stamps.push(t);
    if (stamps.length >= threshold) {
      stamps = [];
      onTrigger();
      return true;
    }
    return false;
  }

  function reset(): void {
    stamps = [];
  }

  return { tap, reset };
}

function localStorageOrNull(): Storage | null {
  try {
    if (typeof window === "undefined") return null;
    return window.localStorage ?? null;
  } catch (_e) {
    return null;
  }
}
