// S238 / PR-232 — vitest pins for the Workshop demo-mode helper.
// Storage-injectable + injectable clock for the tap detector;
// follows the [[invoice-tab-persistence]] storage-stub pattern.

import { describe, expect, it } from "vitest";

import {
  DEFAULT_DEMO_MODE,
  DEMO_MODE_KEY,
  createTapDetector,
  isDemoMode,
  loadDemoMode,
  saveDemoMode,
  setDemoMode,
} from "./workshop-demo-mode";

function makeStorage(initial: Record<string, string> = {}): Storage & {
  store: Record<string, string>;
} {
  const store: Record<string, string> = { ...initial };
  return {
    store,
    getItem(key: string): string | null {
      return Object.prototype.hasOwnProperty.call(store, key)
        ? store[key]
        : null;
    },
    setItem(key: string, value: string): void {
      store[key] = value;
    },
    removeItem(key: string): void {
      delete store[key];
    },
    clear(): void {
      for (const k of Object.keys(store)) delete store[k];
    },
    key(_i: number): string | null {
      return null;
    },
    get length(): number {
      return Object.keys(store).length;
    },
  } as Storage & { store: Record<string, string> };
}

describe("workshop-demo-mode — load", () => {
  it("defaults to off on empty storage so a fresh boot never surfaces mock", () => {
    expect(loadDemoMode(makeStorage())).toBe("off");
    expect(DEFAULT_DEMO_MODE).toBe("off");
  });

  it("returns off when localStorage itself is unavailable", () => {
    expect(loadDemoMode(null)).toBe("off");
  });

  it("returns the persisted value when valid", () => {
    const storage = makeStorage({ [DEMO_MODE_KEY]: "on" });
    expect(loadDemoMode(storage)).toBe("on");
  });

  it("discards unknown vocab and falls back to off (closed-vocab discard)", () => {
    // A corrupted flag must NEVER coerce to on — that would expose
    // mock data without operator intent. Stay safe by default.
    const storage = makeStorage({ [DEMO_MODE_KEY]: "yes" });
    expect(loadDemoMode(storage)).toBe("off");
  });

  it("survives a getItem that throws (security-policy locked storage)", () => {
    const broken: Pick<Storage, "getItem"> = {
      getItem(): string | null {
        throw new Error("nope");
      },
    };
    expect(loadDemoMode(broken)).toBe("off");
  });
});

describe("workshop-demo-mode — save", () => {
  it("writes the value verbatim", () => {
    const storage = makeStorage();
    saveDemoMode("on", storage);
    expect(storage.store[DEMO_MODE_KEY]).toBe("on");
    saveDemoMode("off", storage);
    expect(storage.store[DEMO_MODE_KEY]).toBe("off");
  });

  it("no-ops when storage is null", () => {
    // Must not throw; the page just doesn't persist.
    expect(() => saveDemoMode("on", null)).not.toThrow();
  });
});

describe("workshop-demo-mode — boolean helpers", () => {
  it("isDemoMode is true iff stored value is 'on'", () => {
    expect(isDemoMode(makeStorage({ [DEMO_MODE_KEY]: "on" }))).toBe(true);
    expect(isDemoMode(makeStorage({ [DEMO_MODE_KEY]: "off" }))).toBe(false);
    expect(isDemoMode(makeStorage())).toBe(false);
  });

  it("setDemoMode round-trips through isDemoMode", () => {
    const storage = makeStorage();
    setDemoMode(true, storage);
    expect(isDemoMode(storage)).toBe(true);
    setDemoMode(false, storage);
    expect(isDemoMode(storage)).toBe(false);
  });
});

describe("workshop-demo-mode — tap detector (5-within-2s gesture)", () => {
  function makeClock(): { now: () => number; advance: (ms: number) => void } {
    let t = 1_000_000;
    return {
      now: () => t,
      advance(ms: number) {
        t += ms;
      },
    };
  }

  it("fires on the 5th tap inside a 2s window", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, { now: clock.now });
    expect(det.tap()).toBe(false); // 1
    clock.advance(100);
    expect(det.tap()).toBe(false); // 2
    clock.advance(100);
    expect(det.tap()).toBe(false); // 3
    clock.advance(100);
    expect(det.tap()).toBe(false); // 4
    clock.advance(100);
    expect(det.tap()).toBe(true); // 5 — fires
    expect(fired).toBe(1);
  });

  it("does NOT fire on the 4th tap (boundary)", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, { now: clock.now });
    for (let i = 0; i < 4; i++) {
      det.tap();
      clock.advance(50);
    }
    expect(fired).toBe(0);
  });

  it("resets count after firing so the next gesture starts fresh", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, { now: clock.now });
    for (let i = 0; i < 5; i++) {
      det.tap();
      clock.advance(100);
    }
    expect(fired).toBe(1);
    // Five more taps should fire again (toggle off semantics on
    // the caller's side, but the detector itself is symmetric).
    for (let i = 0; i < 5; i++) {
      det.tap();
      clock.advance(100);
    }
    expect(fired).toBe(2);
  });

  it("ages out taps older than the window (slow sequence never accumulates)", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, { now: clock.now });
    // Four taps inside the window…
    for (let i = 0; i < 4; i++) {
      det.tap();
      clock.advance(400);
    }
    // …then a long gap that drops all four off the head of the
    // window before the fifth tap arrives.
    clock.advance(2_500);
    expect(det.tap()).toBe(false);
    expect(fired).toBe(0);
  });

  it("a partial gesture then a new burst fires on the correct tap", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, { now: clock.now });
    // 3 taps quickly, then a gap longer than the window…
    for (let i = 0; i < 3; i++) {
      det.tap();
      clock.advance(100);
    }
    clock.advance(2_500); // all three age out
    // …then 5 fresh taps inside the new window: must fire on the
    // 5th, NOT the 2nd (which would happen if old stamps lingered).
    expect(det.tap()).toBe(false); // 1
    clock.advance(100);
    expect(det.tap()).toBe(false); // 2
    clock.advance(100);
    expect(det.tap()).toBe(false); // 3
    clock.advance(100);
    expect(det.tap()).toBe(false); // 4
    clock.advance(100);
    expect(det.tap()).toBe(true); // 5
    expect(fired).toBe(1);
  });

  it("respects an overridden threshold + window", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, {
      now: clock.now,
      threshold: 3,
      windowMs: 500,
    });
    expect(det.tap()).toBe(false);
    clock.advance(100);
    expect(det.tap()).toBe(false);
    clock.advance(100);
    expect(det.tap()).toBe(true);
    expect(fired).toBe(1);
  });

  it("reset() drops in-flight taps so the next tap starts the gesture over", () => {
    const clock = makeClock();
    let fired = 0;
    const det = createTapDetector(() => fired++, { now: clock.now });
    for (let i = 0; i < 4; i++) {
      det.tap();
      clock.advance(100);
    }
    det.reset();
    // Only one tap inside the window now — should not fire even
    // though we've technically clicked 5 total.
    expect(det.tap()).toBe(false);
    expect(fired).toBe(0);
  });
});
