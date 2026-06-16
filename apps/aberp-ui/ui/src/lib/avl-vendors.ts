// S431 — pure-module helpers for the SPA's Approved Vendor List (AVL)
// master-data screen. The composers, the wire-to-form mapper, the
// client-side filter, the status-chip class, and the overdue detector all
// live here so vitest can pin them without mounting a Svelte component
// (mirror of `machines.ts`).
//
// Pinned by `avl-vendors.test.ts`.

import type {
  ApprovalCategory,
  ApprovedStatus,
  AvlScreeningResult,
  AvlVendor,
  VendorEditInputs,
  VendorInputs,
} from "./api";

/** S431 — closed vocab of approval statuses with human labels. The
 * `value` strings are the EXACT storage tokens the backend expects. */
export const APPROVED_STATUSES: readonly { value: ApprovedStatus; label: string }[] = [
  { value: "pending", label: "Pending / Függőben" },
  { value: "approved", label: "Approved / Jóváhagyva" },
  { value: "conditional", label: "Conditional / Feltételes" },
  { value: "suspended", label: "Suspended / Felfüggesztve" },
  { value: "revoked", label: "Revoked / Visszavonva" },
];

/** S431 — closed vocab of approval categories (multi-select). */
export const APPROVAL_CATEGORIES: readonly { value: ApprovalCategory; label: string }[] = [
  { value: "general", label: "General / Általános" },
  { value: "itar", label: "ITAR" },
  { value: "ear99", label: "EAR99" },
  { value: "aerospace", label: "Aerospace / Légiipari" },
  { value: "defense", label: "Defense / Védelmi" },
  { value: "nuclear", label: "Nuclear / Nukleáris" },
];

/** S431 — closed vocab of "Screen vendor" results. */
export const SCREENING_RESULTS: readonly { value: AvlScreeningResult; label: string }[] = [
  { value: "skipped_no_integration", label: "Skipped — no integration (mock)" },
  { value: "pass", label: "Pass" },
  { value: "conditional", label: "Conditional" },
  { value: "fail", label: "Fail" },
];

/** S431 — human label for a status token (raw fallback). */
export function statusLabel(status: string): string {
  return APPROVED_STATUSES.find((s) => s.value === status)?.label ?? status;
}

/** S431 — human label for a category token (raw fallback). */
export function categoryLabel(category: string): string {
  return APPROVAL_CATEGORIES.find((c) => c.value === category)?.label ?? category;
}

/** S431 — `true` if a vendor in this status blocks a new PO. Mirrors
 * `ApprovedStatus::blocks_po` — only suspended/revoked block. */
export function statusBlocksPo(status: string): boolean {
  return status === "suspended" || status === "revoked";
}

/** S431 — categorical colour class for the status chip. Approved/
 * conditional → ok, pending → neutral, suspended/revoked → err. */
export function statusChipClass(status: string): string {
  if (status === "approved" || status === "conditional") return "chip chip--ok";
  if (status === "suspended" || status === "revoked") return "chip chip--err";
  return "chip chip--neutral";
}

/** S431 — operator-typed form state for the VendorForm modal. */
export interface VendorFormState {
  partnerId: string;
  approvedStatus: ApprovedStatus;
  /** Selected category tokens (multi-select). */
  categories: ApprovalCategory[];
  /** RFC-3339 date-time string, or "" for no expiry. */
  approvedUntil: string;
  screeningNotes: string;
}

/** S431 — defaults for a freshly-opened VendorForm in create mode. */
export function emptyVendorForm(): VendorFormState {
  return {
    partnerId: "",
    approvedStatus: "pending",
    categories: [],
    approvedUntil: "",
    screeningNotes: "",
  };
}

/** S431 — fold a fetched vendor into edit-mode form state. */
export function formFromVendor(vendor: AvlVendor): VendorFormState {
  return {
    partnerId: vendor.partner_id,
    approvedStatus: vendor.approved_status,
    categories: [...vendor.approval_categories],
    approvedUntil: vendor.approved_until_utc ?? "",
    screeningNotes: vendor.screening_notes,
  };
}

/** S431 — turn the form state into the create-wire `VendorInputs` body.
 * Trims `partnerId`; an empty `approvedUntil` maps to `null` (no expiry). */
export function composeVendorInputs(form: VendorFormState): VendorInputs {
  return {
    partner_id: form.partnerId.trim(),
    approved_status: form.approvedStatus,
    approval_categories: [...form.categories],
    approved_until_utc: form.approvedUntil.trim() === "" ? null : form.approvedUntil.trim(),
    screening_notes: form.screeningNotes,
  };
}

/** S431 — turn the form state into the edit-wire `VendorEditInputs` body
 * (categories / until / notes only; partner + status are not edited here). */
export function composeVendorEdit(form: VendorFormState): VendorEditInputs {
  return {
    approval_categories: [...form.categories],
    approved_until_utc: form.approvedUntil.trim() === "" ? null : form.approvedUntil.trim(),
    screening_notes: form.screeningNotes,
  };
}

/** S431 — toggle a category in/out of a selection (immutable). */
export function toggleCategory(
  selected: ApprovalCategory[],
  category: ApprovalCategory,
): ApprovalCategory[] {
  return selected.includes(category)
    ? selected.filter((c) => c !== category)
    : [...selected, category];
}

/** S431 — closed-vocab status facet for the list. `"All"` short-circuits. */
export type StatusFacet = "All" | ApprovedStatus;

/** S431 — quick-filter spec: a partner-id substring AND a status facet. */
export interface VendorFilterSpec {
  needle: string;
  status: StatusFacet;
}

/** S431 — empty filter (every facet open). */
export const EMPTY_VENDOR_FILTER: VendorFilterSpec = { needle: "", status: "All" };

/** S431 — `true` iff every facet is open. */
export function isVendorFilterEmpty(spec: VendorFilterSpec): boolean {
  return spec.needle.trim().length === 0 && spec.status === "All";
}

/** S431 — partner-id search + status facet filter. The needle is a
 * case-insensitive substring on `partner_id`; the status facet ANDs on
 * top. */
export function filterVendors(rows: AvlVendor[], spec: VendorFilterSpec): AvlVendor[] {
  const statusGated =
    spec.status === "All" ? rows : rows.filter((v) => v.approved_status === spec.status);
  const q = spec.needle.trim().toLowerCase();
  if (q.length === 0) return statusGated;
  return statusGated.filter((v) => v.partner_id.toLowerCase().includes(q));
}

/** S431 — `true` if the vendor's re-screening window has lapsed: not
 * revoked, has an `approved_until_utc`, and that instant is before `now`.
 * Mirrors `aberp::avl_vendors::vendor_is_overdue`. */
export function vendorIsOverdue(vendor: AvlVendor, now: Date): boolean {
  if (vendor.approved_status === "revoked") return false;
  if (!vendor.approved_until_utc) return false;
  const until = new Date(vendor.approved_until_utc);
  if (Number.isNaN(until.getTime())) return false;
  return until.getTime() < now.getTime();
}

/** S431 — typed 400 validation body parser (mirror of
 * `parseMachineValidationError`). */
export function parseVendorValidationError(
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
