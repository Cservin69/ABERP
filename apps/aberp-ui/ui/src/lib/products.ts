// PR-91 — pure-module helpers for the Products master-data screen.
// Mirrors the partners.ts pattern: form state shape, empty defaults,
// wire→form / form→wire mappers, the typed validation-error parser.
// Pinned by `products.test.ts`.

import type {
  Currency,
  NavUnitOfMeasure,
  Product,
  ProductInputs,
  ProductUnit,
} from "./api";
import { formatMinorToInput, parseAmountToMinor } from "./format";

/** PR-91 — every NAV unitOfMeasure token, paired with a Hungarian +
 * English operator-facing label. Order is roughly by Hungarian
 * commerce frequency: piece / time units first, then weight / volume
 * / distance / energy / packaging. The order is the dropdown's
 * display order.
 *
 * `OWN` is intentionally NOT here — it's the outer escape-hatch on
 * [`ProductUnit`]; the SPA's dropdown adds an "Egyéb (Own)" sentinel
 * which reveals a free-text input. See ADR-0046.
 *
 * Adding a token: extend the [`NavUnitOfMeasure`] union in `api.ts`,
 * add an entry here. The `nav_unit_serde_round_trip_pin` Rust test
 * + the SPA's exhaustive coverage pin in `products.test.ts` keep the
 * three surfaces (Rust enum / TS union / dropdown registry) in sync. */
export const NAV_UNIT_OPTIONS: ReadonlyArray<{
  token: NavUnitOfMeasure;
  label_hu: string;
  label_en: string;
}> = [
  { token: "PIECE", label_hu: "db (darab)", label_en: "Piece" },
  { token: "DAY", label_hu: "nap", label_en: "Day" },
  { token: "HOUR", label_hu: "óra", label_en: "Hour" },
  { token: "MINUTE", label_hu: "perc", label_en: "Minute" },
  { token: "MONTH", label_hu: "hónap", label_en: "Month" },
  { token: "KILOGRAM", label_hu: "kg", label_en: "Kilogram" },
  { token: "TON", label_hu: "tonna", label_en: "Ton" },
  { token: "LITER", label_hu: "liter", label_en: "Liter" },
  { token: "CUBIC_METER", label_hu: "m³", label_en: "Cubic meter" },
  { token: "METER", label_hu: "m", label_en: "Meter" },
  { token: "LINEAR_METER", label_hu: "fm (folyóméter)", label_en: "Linear meter" },
  { token: "KILOMETER", label_hu: "km", label_en: "Kilometer" },
  { token: "KWH", label_hu: "kWh", label_en: "Kilowatt-hour" },
  { token: "CARTON", label_hu: "karton", label_en: "Carton" },
  { token: "PACK", label_hu: "csomag", label_en: "Pack" },
];

/** PR-91 — sentinel selected when the operator wants a free-text
 * unit (the `Own` branch). The dropdown surfaces this as the LAST
 * option, labelled "Egyéb (Own)". The form reveals the free-text
 * input when this sentinel is selected; on submit the composer
 * translates it to `ProductUnit::Own(label)`. */
export const OWN_UNIT_SENTINEL = "__OWN__" as const;

/** PR-91 — operator-typed form state for the ProductForm modal.
 * Strings throughout so DOM `bind:value` round-trips cleanly. */
export interface ProductFormState {
  name: string;
  /** Either one of the [`NavUnitOfMeasure`] tokens or
   * [`OWN_UNIT_SENTINEL`]. */
  unitSelection: NavUnitOfMeasure | typeof OWN_UNIT_SENTINEL;
  /** Free-text label rendered only when `unitSelection === OWN_UNIT_SENTINEL`.
   * Ignored on submit otherwise. */
  unitOwnLabel: string;
  currency: Currency;
  /** Operator's typed unit-price string. Parsed via PR-88's
   * `parseAmountToMinor` on submit; same rules as the IssueInvoice
   * line editor (bare ints = WHOLE major units, `.` and `,` both
   * accepted as decimal separator, spaces/NBSP stripped). */
  unitPriceInput: string;
}

/** PR-91 — defaults for a freshly-opened ProductForm in create mode.
 * `PIECE` is the most-used unit (Ervin's `db` example); HUF is the
 * default currency (tenant base currency per ADR-0037). */
export function emptyProductForm(): ProductFormState {
  return {
    name: "",
    unitSelection: "PIECE",
    unitOwnLabel: "",
    currency: "HUF",
    unitPriceInput: "",
  };
}

/** PR-91 — fold a fetched Product into the form state for edit mode.
 * The price re-renders via [`formatMinorToInput`] (PR-88) so the
 * operator sees a canonical re-parseable string. */
export function formFromProduct(product: Product): ProductFormState {
  if (product.unit.kind === "Nav") {
    return {
      name: product.name,
      unitSelection: product.unit.value,
      unitOwnLabel: "",
      currency: product.currency,
      unitPriceInput: formatMinorToInput(product.unit_price_minor, product.currency),
    };
  }
  return {
    name: product.name,
    unitSelection: OWN_UNIT_SENTINEL,
    unitOwnLabel: product.unit.value,
    currency: product.currency,
    unitPriceInput: formatMinorToInput(product.unit_price_minor, product.currency),
  };
}

/** PR-91 — fold the form state into the wire `ProductInputs` body.
 * Pure; no DOM, no fetch. The price parser may return `null` for
 * malformed input — the composer maps that to `0` on the wire, the
 * backend's `validate_product_inputs` does not reject zero, so the
 * operator gets the catalog row saved with a zero price (a known
 * placeholder). If/when zero placeholders become undesirable the
 * validator gains a non-zero rule and the SPA renders the inline
 * error from the existing A157 envelope.
 *
 * The `Own` branch trims the label; the backend rejects empty-after-
 * trim via `validate_product_inputs`. */
export function composeProductInputs(form: ProductFormState): ProductInputs {
  const unit: ProductUnit =
    form.unitSelection === OWN_UNIT_SENTINEL
      ? { kind: "Own", value: form.unitOwnLabel.trim() }
      : { kind: "Nav", value: form.unitSelection };
  return {
    name: form.name.trim(),
    unit,
    currency: form.currency,
    unit_price_minor: parseAmountToMinor(form.unitPriceInput, form.currency) ?? 0,
  };
}

/** PR-91 — client-side admin-mode filter for the ProductsList screen.
 * Case-insensitive substring match on `name` (the only operator-
 * meaningful searchable field — units are dropdown-picked, price is
 * not a search target). Mirrors `filterPartners`. */
export function filterProducts(rows: Product[], needle: string): Product[] {
  const q = needle.trim().toLowerCase();
  if (q.length === 0) return rows;
  return rows.filter((p) => p.name.toLowerCase().includes(q));
}

/** PR-91 — operator-facing label for a product's unit. Hungarian by
 * default (the operator's locale); falls back to the raw NAV token
 * for unknown unions, falls through to the free-text label for
 * `Own`. Used by the list view's "Unit" column.
 *
 * `liter@15C` (the canonical Own case) renders verbatim — the label
 * IS the unit. */
export function unitLabel(unit: ProductUnit): string {
  if (unit.kind === "Own") return unit.value;
  const opt = NAV_UNIT_OPTIONS.find((o) => o.token === unit.value);
  return opt?.label_hu ?? unit.value;
}

/** PR-91 — typed 400 validation body parser. Same shape as Partners
 * (the A157 inline-error envelope); the dispatcher accepts the
 * partners parser would too, but we duplicate the function here so a
 * future product-specific field-error type is a local widening. */
export function parseProductValidationError(
  raw: string,
):
  | { error: "validation_failed"; fields: Array<{ field: string; message: string }> }
  | null {
  const start = raw.indexOf("{");
  const end = raw.lastIndexOf("}");
  if (start < 0 || end <= start) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw.slice(start, end + 1));
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const obj = parsed as Record<string, unknown>;
  if (obj.error !== "validation_failed") return null;
  if (!Array.isArray(obj.fields)) return null;
  const fields: Array<{ field: string; message: string }> = [];
  for (const entry of obj.fields) {
    if (typeof entry !== "object" || entry === null) return null;
    const e = entry as Record<string, unknown>;
    if (typeof e.field !== "string" || typeof e.message !== "string") {
      return null;
    }
    fields.push({ field: e.field, message: e.message });
  }
  return { error: "validation_failed", fields };
}
