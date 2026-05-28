// PR-100 — vitest pins for the product-combobox state helper.
//
// Mirrors the PR-74 buyer-combobox test posture: every named failure
// mode from the brief gets its own `describe` block so a regression
// surfaces with a precise label.

import { describe, expect, it } from "vitest";

import { productLineComboboxState } from "./product-combobox";
import type { Product } from "./api";

function product(overrides: Partial<Product>): Product {
  return {
    id: "prd_01ARZ3NDEKTSV4RRFFQ69G5FAV",
    name: "Tanácsadói nap",
    unit: { kind: "Nav", value: "DAY" },
    currency: "HUF",
    unit_price_minor: 25000,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    deleted_at: null,
    ...overrides,
  };
}

describe("productLineComboboxState — happy path (typeahead returns results)", () => {
  it("returns the matching saved product once needle reaches the minChars threshold", () => {
    const saved: Product[] = [
      product({ id: "prd_a", name: "Tanácsadói nap" }),
      product({ id: "prd_b", name: "Gázolaj" }),
      product({ id: "prd_c", name: "Konzultáció" }),
    ];

    const result = productLineComboboxState({ needle: "Gáz", savedProducts: saved });

    expect(result.shouldShowDropdown).toBe(true);
    expect(result.matches.map((p) => p.id)).toEqual(["prd_b"]);
  });

  it("matches case-insensitively on the product name", () => {
    const saved: Product[] = [
      product({ id: "prd_a", name: "Widget A" }),
      product({ id: "prd_b", name: "Bocci B" }),
    ];

    expect(productLineComboboxState({ needle: "wid", savedProducts: saved }).matches.map((p) => p.id)).toEqual(["prd_a"]);
    expect(productLineComboboxState({ needle: "WID", savedProducts: saved }).matches.map((p) => p.id)).toEqual(["prd_a"]);
    expect(productLineComboboxState({ needle: "Wid", savedProducts: saved }).matches.map((p) => p.id)).toEqual(["prd_a"]);
  });
});

describe("productLineComboboxState — multi-token AND", () => {
  it("requires every whitespace-separated token to match the name", () => {
    const saved: Product[] = [
      product({ id: "prd_a", name: "Tanácsadói nap" }),
      product({ id: "prd_b", name: "Tanácsadás (egyéb)" }),
      product({ id: "prd_c", name: "Munka nap" }),
    ];

    // "tan nap" matches Tanácsadói nap (has both "tan" and "nap") but
    // NOT Tanácsadás (egyéb) (has "tan" but no "nap") and NOT Munka
    // nap (has "nap" but no "tan").
    const result = productLineComboboxState({ needle: "tan nap", savedProducts: saved });

    expect(result.matches.map((p) => p.id)).toEqual(["prd_a"]);
  });

  it("collapses consecutive whitespace and ignores trailing spaces", () => {
    const saved: Product[] = [
      product({ id: "prd_a", name: "Tanácsadói nap" }),
    ];

    expect(productLineComboboxState({ needle: "tan   nap", savedProducts: saved }).matches.map((p) => p.id)).toEqual(["prd_a"]);
    expect(productLineComboboxState({ needle: "  tan nap  ", savedProducts: saved }).matches.map((p) => p.id)).toEqual(["prd_a"]);
  });
});

describe("productLineComboboxState — ranking (prefix beats substring)", () => {
  it("places prefix matches ahead of internal-substring matches", () => {
    // Both names contain "wid"; only the first STARTS with it.
    // Source order intentionally inverted (Mini-Widget first) so the
    // sort actually has to move things; if the helper just preserves
    // input order without ranking, this test fails.
    const saved: Product[] = [
      product({ id: "prd_mini", name: "Mini-Widget B" }),
      product({ id: "prd_widget", name: "Widget A" }),
    ];

    const result = productLineComboboxState({ needle: "wid", savedProducts: saved });

    expect(result.matches.map((p) => p.id)).toEqual(["prd_widget", "prd_mini"]);
  });

  it("preserves source order for ties within the same tier", () => {
    // All three start with the same prefix; ranking ties should fall
    // back to source order so the operator sees a stable list.
    const saved: Product[] = [
      product({ id: "prd_1", name: "Konzultáció A" }),
      product({ id: "prd_2", name: "Konzultáció B" }),
      product({ id: "prd_3", name: "Konzultáció C" }),
    ];

    const result = productLineComboboxState({ needle: "kon", savedProducts: saved });

    expect(result.matches.map((p) => p.id)).toEqual(["prd_1", "prd_2", "prd_3"]);
  });
});

describe("productLineComboboxState — no-match surfaces a dropdown anyway", () => {
  it("returns empty matches with shouldShowDropdown=true when nothing matches", () => {
    // The renderer surfaces this as a 'no match — will be used as a
    // one-off description' hint; the input value flows through as a
    // free-text line description on submit. Mirrors the PR-74 buyer-
    // combobox posture exactly.
    const saved: Product[] = [
      product({ id: "prd_a", name: "Tanácsadói nap" }),
    ];

    const result = productLineComboboxState({ needle: "Xerox printer", savedProducts: saved });

    expect(result.shouldShowDropdown).toBe(true);
    expect(result.matches).toEqual([]);
  });
});

describe("productLineComboboxState — gating below minChars", () => {
  it("does not show the dropdown until the trimmed needle reaches minChars (default 2)", () => {
    const saved: Product[] = [product({ id: "prd_a", name: "Tanácsadói nap" })];

    expect(productLineComboboxState({ needle: "", savedProducts: saved }).shouldShowDropdown).toBe(false);
    expect(productLineComboboxState({ needle: "t", savedProducts: saved }).shouldShowDropdown).toBe(false);
    // Whitespace must not count toward the threshold — "  t  " is one
    // real char and should not trip the dropdown.
    expect(productLineComboboxState({ needle: "  t  ", savedProducts: saved }).shouldShowDropdown).toBe(false);
    expect(productLineComboboxState({ needle: "ta", savedProducts: saved }).shouldShowDropdown).toBe(true);
  });

  it("returns empty matches when below minChars even with a non-empty product list", () => {
    const saved: Product[] = [product({ id: "prd_a", name: "Tanácsadói nap" })];

    const result = productLineComboboxState({ needle: "t", savedProducts: saved });

    expect(result.matches).toEqual([]);
    expect(result.shouldShowDropdown).toBe(false);
  });

  it("respects an explicit minChars override", () => {
    const saved: Product[] = [product({ id: "prd_a", name: "Tanácsadói nap" })];

    // Opt into instant search (1 char threshold).
    expect(
      productLineComboboxState({ needle: "t", savedProducts: saved, minChars: 1 }).shouldShowDropdown,
    ).toBe(true);
    // Or push it higher than the default for a more conservative
    // typeahead (3-char threshold matches the buyer-combobox default).
    expect(
      productLineComboboxState({ needle: "ta", savedProducts: saved, minChars: 3 }).shouldShowDropdown,
    ).toBe(false);
  });
});

describe("productLineComboboxState — maxMatches cap", () => {
  it("caps the dropdown at the default maxMatches=5", () => {
    const saved: Product[] = Array.from({ length: 12 }).map((_, i) =>
      product({ id: `prd_${i}`, name: `Match-${i}` }),
    );

    const result = productLineComboboxState({ needle: "Match", savedProducts: saved });

    expect(result.matches.length).toBe(5);
  });

  it("respects an explicit maxMatches override", () => {
    const saved: Product[] = Array.from({ length: 12 }).map((_, i) =>
      product({ id: `prd_${i}`, name: `Match-${i}` }),
    );

    const result = productLineComboboxState({ needle: "Match", savedProducts: saved, maxMatches: 3 });

    expect(result.matches.length).toBe(3);
  });
});

describe("productLineComboboxState — empty saved-products list", () => {
  it("surfaces shouldShowDropdown=true even when savedProducts is empty", () => {
    // First-time tenant scenario: no products saved yet. The operator
    // typing a one-off description must still see the 'no match' hint
    // (rather than the dropdown never showing — which would look like
    // the feature is broken). Mirrors PR-74's empty-saved-partners pin.
    const result = productLineComboboxState({ needle: "Tanácsadói nap", savedProducts: [] });

    expect(result.shouldShowDropdown).toBe(true);
    expect(result.matches).toEqual([]);
  });
});

describe("productLineComboboxState — empty-needle short-circuit", () => {
  it("returns empty matches + hidden dropdown when needle is empty", () => {
    const saved: Product[] = [product({ id: "prd_a", name: "Tanácsadói nap" })];

    expect(productLineComboboxState({ needle: "", savedProducts: saved })).toEqual({
      matches: [],
      shouldShowDropdown: false,
    });
  });
});

describe("productLineComboboxState — live wire-shape pin", () => {
  it("matches against the verbatim snake_case JSON the GET /api/products route emits", () => {
    // Pin against the exact `aberp::products::Product` wire shape
    // (snake_case fields, internally-tagged ProductUnit). A future
    // drift (renaming `name` → `display_name`, or changing the
    // ProductUnit shape) would silently break the live combobox
    // without this test catching it. Mirrors PR-75's wire-shape pin.
    //
    // `liter@15C` is the canonical Own-unit product per ADR-0046; the
    // helper does not care about the unit shape (the search target is
    // `name` only) but the test exercises the full wire shape so a
    // ProductUnit field rename trips the pin.
    const wireShape: Product = {
      id: "prd_01HXYZABCDEFGHJKMNPQRSTVWX",
      name: "Gázolaj (liter@15C)",
      unit: { kind: "Own", value: "liter@15C" },
      currency: "HUF",
      unit_price_minor: 650,
      created_at: "2026-05-01T08:00:00Z",
      updated_at: "2026-05-20T15:30:00Z",
      deleted_at: null,
    };

    const result = productLineComboboxState({ needle: "gázolaj", savedProducts: [wireShape] });

    expect(result.shouldShowDropdown).toBe(true);
    expect(result.matches).toEqual([wireShape]);
  });
});
