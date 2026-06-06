// S266 / PR-255 — pure helpers for the Settings → Material Catalogue
// page: the closed-vocab stock-status labels/tones and the sortable-table
// comparator. Kept out of the .svelte component so it is unit-testable
// without a DOM (mirrors lib/invoice-list.ts + lib/adapter-format.ts).

import type { QuotingMaterial, StockStatus } from "./api";

/** Display order of the closed-vocab stock status (kept in sync with the
 * Rust `StockStatus::ALL`). */
export const STOCK_STATUS_ORDER: readonly StockStatus[] = [
  "in_stock",
  "source_1_2d",
  "source_3_7d",
  "special_order",
];

const STOCK_STATUS_LABELS: Record<StockStatus, string> = {
  in_stock: "Raktáron / In stock",
  source_1_2d: "Beszerzés 1–2 nap / Source 1–2d",
  source_3_7d: "Beszerzés 3–7 nap / Source 3–7d",
  special_order: "Egyedi rendelés / Special order",
};

/** Operator-facing label for a stock status; degrades to the raw value
 * (and warns) on an unknown string rather than rendering blank. */
export function stockStatusLabel(s: string): string {
  if (s in STOCK_STATUS_LABELS) {
    return STOCK_STATUS_LABELS[s as StockStatus];
  }
  console.warn(`unknown stock_status: ${s}`);
  return s;
}

/** Chip tone for a stock status — maps to the `.mat-chip--<tone>` classes,
 * which resolve to tokens.css signal colours. */
export function stockStatusTone(
  s: string,
): "positive" | "warning" | "muted" | "neutral" {
  switch (s) {
    case "in_stock":
      return "positive";
    case "source_1_2d":
      return "neutral";
    case "source_3_7d":
      return "warning";
    case "special_order":
      return "muted";
    default:
      return "muted";
  }
}

// ── Sorting ───────────────────────────────────────────────────────────

export type SortKey =
  | "grade"
  | "display_name"
  | "density_g_cm3"
  | "cost_per_kg_eur"
  | "machinability_index"
  | "carbide_life_multiplier"
  | "stock_status"
  | "lead_time_default_days"
  | "quote_multiplier"
  | "updated_at";

export type SortDir = "asc" | "desc";

export interface SortState {
  key: SortKey | null;
  dir: SortDir;
}

/** Three-click cycle: (unsorted) → asc → desc → (unsorted). Pure: returns
 * the next state, mirroring InvoiceList's `onSortClick`. */
export function toggleSort(prev: SortState, key: SortKey): SortState {
  if (prev.key !== key) return { key, dir: "asc" };
  if (prev.dir === "asc") return { key, dir: "desc" };
  return { key: null, dir: "asc" };
}

const NUMERIC_KEYS: ReadonlySet<SortKey> = new Set<SortKey>([
  "density_g_cm3",
  "cost_per_kg_eur",
  "machinability_index",
  "carbide_life_multiplier",
  "lead_time_default_days",
  "quote_multiplier",
]);

function rawCompare(a: QuotingMaterial, b: QuotingMaterial, key: SortKey): number {
  if (key === "stock_status") {
    // Sort by the sourcing tier order, not alphabetically.
    return (
      STOCK_STATUS_ORDER.indexOf(a.stock_status) -
      STOCK_STATUS_ORDER.indexOf(b.stock_status)
    );
  }
  if (NUMERIC_KEYS.has(key)) {
    return (a[key] as number) - (b[key] as number);
  }
  // string keys: grade, display_name, updated_at (RFC3339 sorts lexically)
  return String(a[key]).localeCompare(String(b[key]));
}

/** Stable comparator. Ties break on `grade` ascending so re-sorts are
 * deterministic across refreshes. */
export function compareMaterials(
  a: QuotingMaterial,
  b: QuotingMaterial,
  key: SortKey,
  dir: SortDir,
): number {
  const cmp = rawCompare(a, b, key);
  if (cmp !== 0) return dir === "asc" ? cmp : -cmp;
  return a.grade.localeCompare(b.grade);
}

/** Apply a sort state to a copy of the rows (never mutates input). With no
 * sort key, returns the backend's grade-ascending order untouched. */
export function sortMaterials(
  rows: readonly QuotingMaterial[],
  sort: SortState,
): QuotingMaterial[] {
  const out = [...rows];
  if (sort.key === null) return out;
  const key = sort.key;
  out.sort((a, b) => compareMaterials(a, b, key, sort.dir));
  return out;
}
