// S231 / PR-227 — pins on the inventory v1 SPA helpers.

import { describe, expect, test } from "vitest";

import {
  MANUAL_REASONS,
  MOVEMENT_REASON_LABELS,
  formatQty,
  postMovementThenRefresh,
  reasonLabel,
} from "./stock-movements";

describe("MOVEMENT_REASON_LABELS", () => {
  test("covers every backend reason variant once", () => {
    const expected = [
      "receipt",
      "bom_consumption",
      "wo_completion",
      "adjustment",
      "dispatch",
      "scrap",
    ];
    const got = MOVEMENT_REASON_LABELS.map((r) => r.reason).sort();
    expect(got).toEqual(expected.sort());
  });

  test("MANUAL_REASONS matches ADR-0061 §6 — Receipt | Adjustment | Scrap", () => {
    // ADR-0061 §6: "Operator-supplied movements always have ref_kind =
    // Manual"; the manual form only offers Receipt / Adjustment / Scrap.
    // The other three (BomConsumption / WoCompletion / Dispatch) are
    // upstream-only per ADR-0062/0063/0064.
    expect([...MANUAL_REASONS].sort()).toEqual([
      "adjustment",
      "receipt",
      "scrap",
    ]);
  });
});

describe("reasonLabel", () => {
  test("returns bilingual labels for every reason", () => {
    expect(reasonLabel("receipt", "en")).toBe("Receipt");
    expect(reasonLabel("receipt", "hu")).toBe("Bevét");
    expect(reasonLabel("scrap", "hu")).toBe("Selejt");
    expect(reasonLabel("bom_consumption", "en")).toBe("BOM consumption");
  });
});

describe("formatQty", () => {
  test("strips DB trailing zeros from whole numbers", () => {
    expect(formatQty("10.000000")).toBe("10");
    expect(formatQty("0.000000")).toBe("0");
    expect(formatQty("-3.000000")).toBe("-3");
  });

  test("preserves real fractional digits", () => {
    expect(formatQty("-3.5")).toBe("-3.5");
    expect(formatQty("100.250000")).toBe("100.25");
    expect(formatQty("0.500000")).toBe("0.5");
  });

  test("integers without decimal point pass through unchanged", () => {
    expect(formatQty("10")).toBe("10");
    expect(formatQty("-5")).toBe("-5");
    expect(formatQty("0")).toBe("0");
  });
});

// The list row and the detail drawer must show the SAME stock figure. The
// drawer opens over the list, so after a movement posts, BOTH must re-read
// the backend's `stock_qty` — otherwise the list keeps the pre-movement
// value (prod: list 126 vs drawer 65.5 after a -60.5 movement).
describe("postMovementThenRefresh", () => {
  // Fake backend: one product's cached stock, read by both screens.
  function harness(start: string) {
    let backend = start;
    const screens = { list: start, detail: start };
    return {
      screens,
      post: (delta: number) => async () => {
        backend = String(Number(backend) + delta);
      },
      refreshDetail: async () => {
        screens.detail = backend;
      },
      refreshList: async () => {
        screens.list = backend;
      },
    };
  }

  test("after an outflow the list row agrees with the drawer", async () => {
    const h = harness("126");
    await postMovementThenRefresh(h.post(-60.5), h.refreshDetail, h.refreshList);
    expect(h.screens.detail).toBe("65.5");
    expect(h.screens.list).toBe(h.screens.detail);
  });

  test("a refused movement refreshes neither screen and surfaces the error", async () => {
    const h = harness("126");
    const refused = async () => {
      throw new Error("409 duplicate idempotency key");
    };
    await expect(
      postMovementThenRefresh(refused, h.refreshDetail, h.refreshList),
    ).rejects.toThrow("409");
    expect(h.screens).toEqual({ list: "126", detail: "126" });
  });
});
