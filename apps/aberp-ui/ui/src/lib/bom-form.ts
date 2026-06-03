// S232 / PR-228 follow-up — pure-module helpers for the ProductDetail
// BOM authoring tab. Mirrors `stock-movements.ts` / `products.ts`: form
// state shape, defaults, row-mutation functions, the form→wire composer,
// and a component-name resolver. The Svelte component owns the fetch +
// the DOM; this module owns the "given the operator's typed rows + the
// loaded products catalog, what does the form look like + what wire body
// do we POST" decision. Pinned by `bom-form.test.ts`.
//
// The helper is intentionally pure (no Svelte runes, no DOM, no
// backend calls) so vitest can pin the add/remove + compose invariants
// without mounting a component or stubbing `invoke`.
//
// Backend boundary: the POST route is full-replace per S232 — the
// server soft-retires every prior active row and writes the supplied
// list as the new active set. Mirrors the PR-72 multi-bank-account
// posture. The next GET reflects only the new active rows.

import type { BomLine, Product, PutProductBomBody, PutProductBomLine } from "./api";

/** S232 — one editable row in the BOM authoring form.
 *
 * `component_id` is empty string until the operator picks a product;
 * `qty_per_unit_input` is the operator-typed Decimal string (we keep
 * the typed text rather than a parsed number so we don't fight the
 * operator's intermediate keystrokes — same posture as
 * `ProductFormState.unitPriceInput`). */
export interface BomFormRow {
  component_id: string;
  qty_per_unit_input: string;
}

/** S232 — full form state for the BOM tab. */
export interface BomFormState {
  rows: BomFormRow[];
}

/** S232 — empty form (no rows). The renderer surfaces an explicit
 * "Add component" button so the operator's first action is overt
 * rather than typing into a blank phantom row. */
export function emptyBomForm(): BomFormState {
  return { rows: [] };
}

/** S232 — fold the GET response (the current active BOM rows) into
 * the form so the operator edits in place rather than retyping every
 * component on every open. */
export function formFromBomLines(lines: BomLine[]): BomFormState {
  return {
    rows: lines.map((l) => ({
      component_id: l.component_id,
      qty_per_unit_input: l.qty_per_unit,
    })),
  };
}

/** S232 — append a blank row. Returned as a NEW state object so a
 * Svelte rune assignment triggers reactivity cleanly. */
export function addBomRow(state: BomFormState): BomFormState {
  return {
    rows: [...state.rows, { component_id: "", qty_per_unit_input: "" }],
  };
}

/** S232 — drop the row at `index`. Out-of-range indices return the
 * state unchanged (loud-fail would be wrong here — the renderer's
 * delete button cannot point at a stale index because every render
 * keys on the array position, but defence in depth costs nothing). */
export function removeBomRow(state: BomFormState, index: number): BomFormState {
  if (index < 0 || index >= state.rows.length) return state;
  return { rows: state.rows.filter((_, i) => i !== index) };
}

/** S232 — update one row in place. Returns a new state object. */
export function updateBomRow(
  state: BomFormState,
  index: number,
  patch: Partial<BomFormRow>,
): BomFormState {
  if (index < 0 || index >= state.rows.length) return state;
  return {
    rows: state.rows.map((r, i) => (i === index ? { ...r, ...patch } : r)),
  };
}

/** S232 — `true` iff every row has a component picked AND a non-empty
 * qty string. Drives the Save button's disabled state. We intentionally
 * accept any non-empty qty string here (rather than re-parsing the
 * Decimal) — the backend's `validate` will loud-fail malformed input
 * via the A157 envelope, surfacing the actual rule violated. */
export function isBomFormSubmittable(state: BomFormState): boolean {
  if (state.rows.length === 0) return true; // empty BOM = "clear my recipe"
  return state.rows.every(
    (r) => r.component_id.length > 0 && r.qty_per_unit_input.trim().length > 0,
  );
}

/** S232 — fold the form into the wire `PutProductBomBody`. Trims the
 * qty string so the backend's Decimal parser sees `"1.5"` not `" 1.5 "`.
 * Rows that fail [`isBomFormSubmittable`] still serialise here — the
 * caller decides whether to POST; the backend is the final loud-fail
 * gate per CLAUDE.md rule 12. */
export function composeBomBody(state: BomFormState): PutProductBomBody {
  const lines: PutProductBomLine[] = state.rows.map((r) => ({
    component_id: r.component_id,
    qty_per_unit: r.qty_per_unit_input.trim(),
  }));
  return { lines };
}

/** S232 — resolve a component-product id to a display name using the
 * loaded products catalog. Returns the raw id (with an em-dash hint)
 * when the id is unknown — operators see the durable identifier
 * rather than a blank cell, surfacing a catalog drift loud. */
export function componentName(componentId: string, catalog: Product[]): string {
  if (componentId.length === 0) return "—";
  const hit = catalog.find((p) => p.id === componentId);
  return hit?.name ?? `${componentId} (unknown)`;
}
