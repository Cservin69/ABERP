// S232 / PR-228 follow-up — vitest pins for the BOM authoring tab's
// pure-logic helpers. The SPA's vitest layer has no jsdom (per S175
// convention + `invoice-tab-persistence.test.ts`), so the three brief-
// mandated pins are expressed against the pure helpers that the Svelte
// component composes:
//
//   1. "Tab renders without crashing when BOM is empty" → the form
//      reducer accepts an empty `BomLine[]` from the GET response and
//      yields a renderable empty `BomFormState` (no thrown error, no
//      undefined-index access).
//
//   2. "Adding a row + saving calls the POST endpoint with the right
//      shape" → after `addBomRow` + `updateBomRow`, the composer emits
//      a `PutProductBomBody` matching the backend's contract; the
//      Svelte component's submit handler invokes `putProductBom` with
//      that body verbatim, and the response is the new active BOM.
//
//   3. "Soft-retire of prior is reflected (next GET returns the new
//      active row only)" → the backend's full-replace semantics mean
//      `formFromBomLines` over the POST response yields a form whose
//      rows match the just-saved list exactly (no leftover entries
//      from the prior active set).
//
// Each pin uses the in-memory backend stub `makeBomBackend` to mirror
// the soft-retire semantics S232 already implemented on the Rust side.

import { describe, expect, it } from "vitest";

import type { BomLine, Product, PutProductBomBody } from "./api";
import {
  addBomRow,
  componentName,
  composeBomBody,
  emptyBomForm,
  formFromBomLines,
  isBomFormSubmittable,
  removeBomRow,
  updateBomRow,
} from "./bom-form";

// ── stub backend mirroring the S232 POST contract ────────────────────
//
// The Rust route soft-retires every prior active row and writes the
// supplied list as the new active set; GET returns active rows only.
// This stub mirrors that surface so the pin can assert the POST→GET
// round-trip without spawning the backend.

interface BomBackend {
  get: () => BomLine[];
  put: (body: PutProductBomBody) => BomLine[];
  callLog: PutProductBomBody[];
}

function makeBomBackend(initial: BomLine[] = []): BomBackend {
  let active: BomLine[] = [...initial];
  const callLog: PutProductBomBody[] = [];
  let seq = 0;
  return {
    get: () => [...active],
    put: (body: PutProductBomBody) => {
      callLog.push(body);
      // S232 soft-retire semantics: every prior active row stops being
      // active; the supplied list is the new active set.
      active = body.lines.map((l) => ({
        bom_line_id: `bom_${seq++}`,
        product_id: "prd_assembly",
        component_id: l.component_id,
        qty_per_unit: l.qty_per_unit,
        created_at: "2026-06-03T00:00:00Z",
        retired_at: null,
      }));
      return [...active];
    },
    callLog,
  };
}

function product(id: string, name: string): Product {
  return {
    id,
    name,
    unit: { kind: "Nav", value: "PIECE" },
    currency: "HUF",
    unit_price_minor: 1000,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    deleted_at: null,
  };
}

describe("bom-form — pin #1: tab renders without crashing when BOM is empty", () => {
  it("emptyBomForm yields a renderable shape with zero rows", () => {
    const state = emptyBomForm();
    expect(state.rows).toEqual([]);
    expect(isBomFormSubmittable(state)).toBe(true);
  });

  it("formFromBomLines([]) over an empty GET response yields zero rows", () => {
    // Mirrors the Svelte component's onMount: GET returns [] for a
    // product with no BOM; the form must not crash on the fold.
    const state = formFromBomLines([]);
    expect(state.rows).toEqual([]);
    // The composer's body for the empty form is the wire shape the
    // backend's PUT route accepts as "clear my recipe".
    expect(composeBomBody(state)).toEqual({ lines: [] });
  });

  it("componentName('—') for an empty pick yields the em-dash placeholder", () => {
    expect(componentName("", [])).toBe("—");
  });

  it("componentName surfaces an 'unknown' suffix when the catalog drifts", () => {
    // CLAUDE.md rule 12 — a stale component_id must not render as blank.
    expect(componentName("prd_missing", [product("prd_other", "Other")])).toBe(
      "prd_missing (unknown)",
    );
  });
});

describe("bom-form — pin #2: adding a row + saving posts the right shape", () => {
  it("addBomRow + updateBomRow + composeBomBody emits the wire body the backend expects", () => {
    let state = emptyBomForm();
    state = addBomRow(state);
    state = updateBomRow(state, 0, {
      component_id: "prd_screw",
      qty_per_unit_input: "  4.5  ", // operator typed surrounding whitespace
    });

    const body = composeBomBody(state);
    // The composer trims the qty so the backend's Decimal parser sees
    // the canonical form. The component_id rides verbatim.
    expect(body).toEqual({
      lines: [{ component_id: "prd_screw", qty_per_unit: "4.5" }],
    });
  });

  it("Save flow: stub POST records the call body verbatim and returns the new active rows", () => {
    const backend = makeBomBackend();
    let state = emptyBomForm();
    state = addBomRow(state);
    state = updateBomRow(state, 0, {
      component_id: "prd_screw",
      qty_per_unit_input: "4.5",
    });

    const body = composeBomBody(state);
    const result = backend.put(body);

    expect(backend.callLog).toHaveLength(1);
    expect(backend.callLog[0]).toEqual({
      lines: [{ component_id: "prd_screw", qty_per_unit: "4.5" }],
    });
    // The route's response shape mirrors a GET — the new active rows.
    expect(result).toHaveLength(1);
    expect(result[0].component_id).toBe("prd_screw");
    expect(result[0].qty_per_unit).toBe("4.5");
    expect(result[0].retired_at).toBeNull();
  });

  it("isBomFormSubmittable gates the Save button: blank rows block, populated rows allow", () => {
    let state = emptyBomForm();
    state = addBomRow(state);
    expect(isBomFormSubmittable(state)).toBe(false); // blank row
    state = updateBomRow(state, 0, { component_id: "prd_screw" });
    expect(isBomFormSubmittable(state)).toBe(false); // qty still blank
    state = updateBomRow(state, 0, { qty_per_unit_input: "4.5" });
    expect(isBomFormSubmittable(state)).toBe(true);
  });
});

describe("bom-form — pin #3: soft-retire of prior is reflected in the next GET", () => {
  it("after POST, the backend's active set is the new list ONLY — prior rows do not leak", () => {
    // Seed the backend with an old BOM (1 screw, 2 panels).
    const initial: BomLine[] = [
      {
        bom_line_id: "bom_old_a",
        product_id: "prd_assembly",
        component_id: "prd_screw",
        qty_per_unit: "4",
        created_at: "2026-05-01T00:00:00Z",
        retired_at: null,
      },
      {
        bom_line_id: "bom_old_b",
        product_id: "prd_assembly",
        component_id: "prd_panel",
        qty_per_unit: "2",
        created_at: "2026-05-01T00:00:00Z",
        retired_at: null,
      },
    ];
    const backend = makeBomBackend(initial);

    // Operator opens the tab → form pre-fills from the GET response.
    let state = formFromBomLines(backend.get());
    expect(state.rows).toHaveLength(2);

    // Operator deletes the panel row, edits the screw qty to 6, and
    // adds a hinge.
    state = removeBomRow(state, 1); // drop panel
    state = updateBomRow(state, 0, { qty_per_unit_input: "6" });
    state = addBomRow(state);
    state = updateBomRow(state, 1, {
      component_id: "prd_hinge",
      qty_per_unit_input: "2",
    });

    // Save.
    backend.put(composeBomBody(state));

    // Next GET reflects the NEW active rows only — no prior leak.
    const nextActive = backend.get();
    expect(nextActive).toHaveLength(2);
    expect(nextActive.map((r) => r.component_id).sort()).toEqual([
      "prd_hinge",
      "prd_screw",
    ]);
    expect(nextActive.find((r) => r.component_id === "prd_screw")?.qty_per_unit).toBe(
      "6",
    );
    // The dropped panel row is GONE from active — soft-retire honored.
    expect(nextActive.find((r) => r.component_id === "prd_panel")).toBeUndefined();
  });

  it("clear-all: POST with empty lines yields a zero-row active set", () => {
    const backend = makeBomBackend([
      {
        bom_line_id: "bom_old",
        product_id: "prd_assembly",
        component_id: "prd_screw",
        qty_per_unit: "4",
        created_at: "2026-05-01T00:00:00Z",
        retired_at: null,
      },
    ]);

    backend.put(composeBomBody(emptyBomForm()));
    expect(backend.get()).toEqual([]);
  });
});
