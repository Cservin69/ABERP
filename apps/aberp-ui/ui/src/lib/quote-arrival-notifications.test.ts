import { describe, expect, it, vi } from "vitest";

import type { QuoteArrival } from "./api";
import {
  arrivalToastMessage,
  freshArrivals,
  loadSeen,
  saveSeen,
} from "./quote-arrival-notifications";

function arr(id: string): QuoteArrival {
  return { quote_id: id, intake_at: "2026-06-05T10:00:00Z" };
}

/** In-memory storage stub matching the injected StorageLike shape. */
function memStorage() {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    _map: map,
  };
}

describe("freshArrivals", () => {
  it("returns only arrivals not in the seen-set (coalesce/dedup)", () => {
    const seen = new Set(["a"]);
    const out = freshArrivals([arr("a"), arr("b"), arr("c")], seen);
    expect(out.map((a) => a.quote_id)).toEqual(["b", "c"]);
  });

  it("returns empty when every arrival is already seen", () => {
    const seen = new Set(["a", "b"]);
    expect(freshArrivals([arr("a"), arr("b")], seen)).toEqual([]);
  });

  it("returns empty for no arrivals", () => {
    expect(freshArrivals([], new Set())).toEqual([]);
  });
});

describe("arrivalToastMessage", () => {
  it("singular for 0 or 1", () => {
    expect(arrivalToastMessage(1).en).toContain("1 new quote");
    expect(arrivalToastMessage(0).en).toContain("1 new quote");
  });

  it("plural for >1 with the count", () => {
    expect(arrivalToastMessage(3).en).toContain("3 new quotes");
    expect(arrivalToastMessage(3).hu).toContain("3 új");
  });
});

describe("seen-set persistence (reload de-dup belt-and-suspenders)", () => {
  it("round-trips through storage", () => {
    const s = memStorage();
    saveSeen(new Set(["x", "y"]), s);
    const back = loadSeen(s);
    expect(back.has("x")).toBe(true);
    expect(back.has("y")).toBe(true);
    expect(back.size).toBe(2);
  });

  it("returns an empty set when storage is null (private browsing)", () => {
    expect(loadSeen(null).size).toBe(0);
  });

  it("tolerates a malformed blob without throwing", () => {
    const s = memStorage();
    s._map.set("aberp:quote-intake:toasted", "not json");
    expect(loadSeen(s).size).toBe(0);
  });

  it("ignores non-string entries in a tampered array", () => {
    const s = memStorage();
    s._map.set("aberp:quote-intake:toasted", JSON.stringify(["ok", 42, null]));
    const back = loadSeen(s);
    expect(back.size).toBe(1);
    expect(back.has("ok")).toBe(true);
  });

  it("caps the persisted set at 200 ids", () => {
    const s = memStorage();
    const big = new Set(Array.from({ length: 250 }, (_, i) => `q${i}`));
    saveSeen(big, s);
    const stored = JSON.parse(
      s._map.get("aberp:quote-intake:toasted") as string,
    ) as string[];
    expect(stored.length).toBe(200);
    // The most recent ids are kept (tail of insertion order).
    expect(stored).toContain("q249");
    expect(stored).not.toContain("q0");
  });

  it("never throws when setItem throws (quota)", () => {
    const throwing = {
      getItem: () => null,
      setItem: vi.fn(() => {
        throw new Error("quota");
      }),
    };
    expect(() => saveSeen(new Set(["a"]), throwing)).not.toThrow();
  });
});
