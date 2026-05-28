// PR-91 — vitest pins for the products helper module.
//
// Mirror invariant per A156: a backend drift that renames a field on
// `aberp::products::Product` / `aberp::products::ProductInputs` would
// surface here first via the snake_case wire-shape assertions.

import { describe, expect, it } from "vitest";

import type { NavUnitOfMeasure, Product } from "./api";
import {
  composeProductInputs,
  emptyProductForm,
  filterProducts,
  formFromProduct,
  NAV_UNIT_OPTIONS,
  OWN_UNIT_SENTINEL,
  parseProductValidationError,
  unitLabel,
} from "./products";

// The complete closed-vocab of NavUnitOfMeasure tokens. Pinned here so
// a Rust-side widening that adds a token without extending the SPA
// surfaces as a coverage gap. Mirrors the `nav_unit_serde_round_trip_pin`
// Rust test.
const ALL_NAV_TOKENS: NavUnitOfMeasure[] = [
  "PIECE",
  "KILOGRAM",
  "TON",
  "KWH",
  "DAY",
  "HOUR",
  "MINUTE",
  "MONTH",
  "LITER",
  "KILOMETER",
  "CUBIC_METER",
  "METER",
  "LINEAR_METER",
  "CARTON",
  "PACK",
];

const SAMPLE_NAV_PRODUCT: Product = {
  id: "prd_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  name: "Tanácsadói nap",
  unit: { kind: "Nav", value: "DAY" },
  currency: "HUF",
  unit_price_minor: 250_000,
  created_at: "2026-05-27T08:00:00Z",
  updated_at: "2026-05-27T08:00:00Z",
  deleted_at: null,
};

const SAMPLE_OWN_PRODUCT: Product = {
  id: "prd_01ARZ3NDEKTSV4RRFFQ69G5FBW",
  name: "Gázolaj",
  unit: { kind: "Own", value: "liter@15C" },
  currency: "HUF",
  unit_price_minor: 650,
  created_at: "2026-05-27T08:00:00Z",
  updated_at: "2026-05-27T08:00:00Z",
  deleted_at: null,
};

describe("NAV_UNIT_OPTIONS", () => {
  it("covers every NavUnitOfMeasure token exactly once", () => {
    // Coverage pin: if a future Rust widening adds (or drops) a token,
    // this assertion fires. Adding a token requires extending the
    // ALL_NAV_TOKENS list above AND the dropdown registry — single-
    // point coverage so the dropdown can't silently lose an option.
    const registryTokens = NAV_UNIT_OPTIONS.map((o) => o.token).sort();
    const expected = [...ALL_NAV_TOKENS].sort();
    expect(registryTokens).toEqual(expected);
  });

  it("includes Hungarian labels for Ervin's load-bearing examples", () => {
    const byToken = new Map(NAV_UNIT_OPTIONS.map((o) => [o.token, o.label_hu]));
    expect(byToken.get("PIECE")).toMatch(/db/);
    expect(byToken.get("DAY")).toBe("nap");
    expect(byToken.get("TON")).toBe("tonna");
    expect(byToken.get("KILOGRAM")).toBe("kg");
    expect(byToken.get("HOUR")).toBe("óra");
    expect(byToken.get("LITER")).toBe("liter");
  });

  it("does NOT include OWN — that's the outer escape hatch", () => {
    // OWN lives at the ProductUnit level (via OWN_UNIT_SENTINEL),
    // never as a NavUnitOfMeasure variant — the dropdown reveals a
    // free-text input on the sentinel selection. Pinning that this
    // invariant holds at the registry level.
    const tokens = NAV_UNIT_OPTIONS.map((o) => o.token as string);
    expect(tokens).not.toContain("OWN");
  });
});

describe("emptyProductForm", () => {
  it("defaults to PIECE / HUF (operator's most-used combo)", () => {
    const form = emptyProductForm();
    expect(form.unitSelection).toBe("PIECE");
    expect(form.currency).toBe("HUF");
    expect(form.name).toBe("");
    expect(form.unitOwnLabel).toBe("");
    expect(form.unitPriceInput).toBe("");
  });
});

describe("formFromProduct (Nav variant)", () => {
  it("maps a Nav product into the dropdown selection", () => {
    const form = formFromProduct(SAMPLE_NAV_PRODUCT);
    expect(form.name).toBe("Tanácsadói nap");
    expect(form.unitSelection).toBe("DAY");
    expect(form.unitOwnLabel).toBe("");
    expect(form.currency).toBe("HUF");
    // 250_000 forints (HUF is 0-decimal) → canonical "250000".
    expect(form.unitPriceInput).toBe("250000");
  });
});

describe("formFromProduct (Own variant — liter@15C)", () => {
  it("routes the Own variant through the OWN sentinel + label field", () => {
    const form = formFromProduct(SAMPLE_OWN_PRODUCT);
    expect(form.unitSelection).toBe(OWN_UNIT_SENTINEL);
    expect(form.unitOwnLabel).toBe("liter@15C");
    // 650 forints → "650".
    expect(form.unitPriceInput).toBe("650");
  });
});

describe("composeProductInputs (Nav variant)", () => {
  it("wraps the dropdown selection in the Nav branch and parses the price", () => {
    const inputs = composeProductInputs({
      name: "  Tanácsadói nap  ",
      unitSelection: "DAY",
      unitOwnLabel: "ignored when not OWN",
      currency: "HUF",
      unitPriceInput: "250 000",
    });
    expect(inputs.name).toBe("Tanácsadói nap");
    expect(inputs.unit).toEqual({ kind: "Nav", value: "DAY" });
    expect(inputs.currency).toBe("HUF");
    // PR-88 rule: "250 000" parses to 250000 minor units in HUF.
    expect(inputs.unit_price_minor).toBe(250_000);
  });

  it("parses an EUR price with comma as 2-decimal cents per PR-88", () => {
    const inputs = composeProductInputs({
      name: "Service",
      unitSelection: "HOUR",
      unitOwnLabel: "",
      currency: "EUR",
      unitPriceInput: "340,50",
    });
    // PR-88: "340,50" EUR → 34050 cents.
    expect(inputs.unit_price_minor).toBe(34050);
    expect(inputs.currency).toBe("EUR");
  });

  it("parses bare EUR int as WHOLE euros (PR-88 anti-cents-shift)", () => {
    // The PR-88 P0 bug class: a bare integer must mean MAJOR units,
    // never auto-cents. "340" EUR is 340.00 EUR = 34000 cents.
    const inputs = composeProductInputs({
      name: "X",
      unitSelection: "HOUR",
      unitOwnLabel: "",
      currency: "EUR",
      unitPriceInput: "340",
    });
    expect(inputs.unit_price_minor).toBe(34000);
  });
});

describe("composeProductInputs (Own variant — the liter@15C path)", () => {
  it("wraps the free-text label in the Own branch", () => {
    const inputs = composeProductInputs({
      name: "Gázolaj",
      unitSelection: OWN_UNIT_SENTINEL,
      unitOwnLabel: "  liter@15C  ",
      currency: "HUF",
      unitPriceInput: "650",
    });
    expect(inputs.unit).toEqual({ kind: "Own", value: "liter@15C" });
  });

  it("preserves the empty label so the backend's validator can reject it", () => {
    // Operator selected OWN but typed nothing in the label. The
    // composer trims to empty; the backend's validate_product_inputs
    // surfaces the structured per-field error. Per CLAUDE.md rule 12
    // (fail loud), the SPA doesn't pre-empt the backend rule.
    const inputs = composeProductInputs({
      name: "X",
      unitSelection: OWN_UNIT_SENTINEL,
      unitOwnLabel: "   ",
      currency: "HUF",
      unitPriceInput: "1",
    });
    expect(inputs.unit).toEqual({ kind: "Own", value: "" });
  });
});

describe("filterProducts", () => {
  const rows: Product[] = [
    { ...SAMPLE_NAV_PRODUCT, name: "Apple juice" },
    { ...SAMPLE_OWN_PRODUCT, name: "apricot" },
    { ...SAMPLE_NAV_PRODUCT, id: "prd_x", name: "Banana" },
  ];

  it("returns the full list for an empty / whitespace needle", () => {
    expect(filterProducts(rows, "").length).toBe(3);
    expect(filterProducts(rows, "   ").length).toBe(3);
  });

  it("filters case-insensitive substring on name", () => {
    const hits = filterProducts(rows, "ap");
    expect(hits.map((r) => r.name)).toEqual(["Apple juice", "apricot"]);
  });
});

describe("unitLabel", () => {
  it("renders Hungarian dropdown label for Nav variant", () => {
    expect(unitLabel({ kind: "Nav", value: "DAY" })).toBe("nap");
    expect(unitLabel({ kind: "Nav", value: "TON" })).toBe("tonna");
  });

  it("renders the free-text label verbatim for Own variant", () => {
    // The load-bearing case — `liter@15C` IS its own label.
    expect(unitLabel({ kind: "Own", value: "liter@15C" })).toBe("liter@15C");
  });
});

describe("parseProductValidationError", () => {
  it("parses the A157 validation_failed envelope", () => {
    const raw =
      'backend returned 400 for /api/products: {"error":"validation_failed","fields":[{"field":"name","message":"name is required"}]}';
    const parsed = parseProductValidationError(raw);
    expect(parsed).not.toBeNull();
    expect(parsed!.fields[0]).toEqual({
      field: "name",
      message: "name is required",
    });
  });

  it("returns null for an unrelated error body so the caller falls through", () => {
    expect(parseProductValidationError("plain string")).toBeNull();
    expect(parseProductValidationError('{"error":"something_else"}')).toBeNull();
  });
});
