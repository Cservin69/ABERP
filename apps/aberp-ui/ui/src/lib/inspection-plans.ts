// S443 / ADR-0092 — pure-module helpers for the inspection-plans
// master-data screen. The form-state shape, the wire composer, the
// edit-mode mapper, the list filter, the tolerance-band label, and the
// client-side validator all live here so vitest can pin them without
// mounting a Svelte component (mirror of `machines.ts`).
//
// Pinned by `inspection-plans.test.ts`.

import type { InspectionPlan, InspectionPlanInputs } from "./api";

/** S443 — operator-typed form state for the InspectionPlanForm modal.
 * The numeric slots are string-valued so the DOM `bind:value`
 * round-trips cleanly; `enabled` binds a checkbox. */
export interface PlanFormState {
  productId: string;
  featureName: string;
  nominalValue: string;
  upperTol: string;
  lowerTol: string;
  units: string;
  optionalProbeCycleId: string;
  enabled: boolean;
}

/** S443 — defaults for a freshly-opened form in create mode. */
export function emptyPlanForm(): PlanFormState {
  return {
    productId: "",
    featureName: "",
    nominalValue: "0",
    upperTol: "0",
    lowerTol: "0",
    units: "mm",
    optionalProbeCycleId: "",
    enabled: true,
  };
}

/** S443 — fold a fetched plan into edit-mode form state. The reverse
 * direction is [`composePlanInputs`]. */
export function formFromPlan(plan: InspectionPlan): PlanFormState {
  return {
    productId: plan.product_id,
    featureName: plan.feature_name,
    nominalValue: String(plan.nominal_value),
    upperTol: String(plan.upper_tol),
    lowerTol: String(plan.lower_tol),
    units: plan.units,
    optionalProbeCycleId: plan.optional_probe_cycle_id ?? "",
    enabled: plan.enabled,
  };
}

/** S443 — turn the form state into the wire `InspectionPlanInputs`
 * body. Pure; trims the string fields. A blank probe-cycle id emits
 * `null` (no probe binding) rather than an empty string. Numeric
 * strings parse via `parseFloat`; an unparseable value yields `NaN`
 * which the backend's validator rejects with a typed field error. */
export function composePlanInputs(form: PlanFormState): InspectionPlanInputs {
  const probe = form.optionalProbeCycleId.trim();
  return {
    product_id: form.productId.trim(),
    feature_name: form.featureName.trim(),
    nominal_value: parseFloat(form.nominalValue),
    upper_tol: parseFloat(form.upperTol),
    lower_tol: parseFloat(form.lowerTol),
    units: form.units.trim(),
    optional_probe_cycle_id: probe.length > 0 ? probe : null,
    enabled: form.enabled,
  };
}

/** S443 — case-insensitive substring search over `product_id` +
 * `feature_name`. Empty / whitespace-only needle returns the list
 * unchanged. */
export function filterPlans(
  rows: InspectionPlan[],
  needle: string,
): InspectionPlan[] {
  const q = needle.trim().toLowerCase();
  if (q.length === 0) return rows;
  return rows.filter(
    (p) =>
      p.product_id.toLowerCase().includes(q) ||
      p.feature_name.toLowerCase().includes(q),
  );
}

/** S443 — tolerance-band label for a plan row: `"[lower, upper]"`.
 * Pure formatter; pinned for the bracket + comma shape. */
export function tolBandLabel(
  plan: Pick<InspectionPlan, "lower_tol" | "upper_tol">,
): string {
  return `[${plan.lower_tol}, ${plan.upper_tol}]`;
}

/** S443 — client-side form validation. Returns a per-field error map
 * (empty when the form is valid). Mirrors the backend's invariants so
 * the operator sees the problem before the round-trip; the backend
 * re-checks authoritatively. Rules: feature non-empty, units
 * non-empty, and `upper_tol > lower_tol` (a degenerate or inverted
 * band can never produce a meaningful verdict). */
export function validatePlanForm(form: PlanFormState): Record<string, string> {
  const errors: Record<string, string> = {};
  if (form.featureName.trim().length === 0) {
    errors.feature_name = "Feature name is required.";
  }
  if (form.units.trim().length === 0) {
    errors.units = "Units are required.";
  }
  const upper = parseFloat(form.upperTol);
  const lower = parseFloat(form.lowerTol);
  if (Number.isNaN(upper) || Number.isNaN(lower)) {
    errors.upper_tol = "Tolerances must be numbers.";
  } else if (upper <= lower) {
    errors.upper_tol = "Upper tolerance must be greater than lower tolerance.";
  }
  return errors;
}

/** S443 — typed 400 validation-body parser. Mirror of
 * `parseMachineValidationError`: peel the JSON object out of the
 * Tauri-wrapped error string, accept iff the `error` discriminant is
 * `"validation_failed"`. Returns `null` for any other shape so the
 * caller falls back to a generic raw-string display. */
export function parsePlanValidationError(
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
