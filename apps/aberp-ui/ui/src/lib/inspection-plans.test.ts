// S443 — vitest pins for the inspection-plans helper module. Composer
// + mapper + filter + band label + validator are pure functions (no
// Svelte, no Tauri); pinning them in isolation surfaces regressions
// before the dev-loop renders the form. Mirror of `machines.test.ts`.

import { describe, expect, it } from "vitest";

import type { InspectionPlan } from "./api";
import {
  composePlanInputs,
  emptyPlanForm,
  filterPlans,
  formFromPlan,
  parsePlanValidationError,
  tolBandLabel,
  validatePlanForm,
} from "./inspection-plans";

const SAMPLE_PLAN: InspectionPlan = {
  plan_id: "qcp_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  product_id: "prod_bracket_a",
  feature_name: "Bore diameter",
  nominal_value: 12.5,
  upper_tol: 0.05,
  lower_tol: -0.05,
  units: "mm",
  optional_probe_cycle_id: "cycle-7",
  enabled: true,
  created_at: "2026-06-15T08:00:00Z",
  archived_at: null,
};

describe("emptyPlanForm", () => {
  it("defaults units=mm, enabled, blank product/feature/probe", () => {
    const form = emptyPlanForm();
    expect(form.units).toBe("mm");
    expect(form.enabled).toBe(true);
    expect(form.productId).toBe("");
    expect(form.featureName).toBe("");
    expect(form.optionalProbeCycleId).toBe("");
  });
});

describe("formFromPlan / composePlanInputs round-trip", () => {
  it("formFromPlan stringifies the numeric fields one-to-one", () => {
    const form = formFromPlan(SAMPLE_PLAN);
    expect(form.productId).toBe("prod_bracket_a");
    expect(form.featureName).toBe("Bore diameter");
    expect(form.nominalValue).toBe("12.5");
    expect(form.upperTol).toBe("0.05");
    expect(form.lowerTol).toBe("-0.05");
    expect(form.units).toBe("mm");
    expect(form.optionalProbeCycleId).toBe("cycle-7");
    expect(form.enabled).toBe(true);
  });

  it("maps a null probe-cycle id to an empty string in the form", () => {
    const form = formFromPlan({ ...SAMPLE_PLAN, optional_probe_cycle_id: null });
    expect(form.optionalProbeCycleId).toBe("");
  });

  it("composePlanInputs trims strings + parses numbers", () => {
    const body = composePlanInputs({
      ...emptyPlanForm(),
      productId: "  prod_x  ",
      featureName: "  Length  ",
      nominalValue: "100.25",
      upperTol: "0.1",
      lowerTol: "-0.2",
      units: "  mm  ",
    });
    expect(body.product_id).toBe("prod_x");
    expect(body.feature_name).toBe("Length");
    expect(body.nominal_value).toBe(100.25);
    expect(body.upper_tol).toBe(0.1);
    expect(body.lower_tol).toBe(-0.2);
    expect(body.units).toBe("mm");
  });

  it("composePlanInputs emits null for a blank probe-cycle id", () => {
    const body = composePlanInputs({ ...emptyPlanForm(), optionalProbeCycleId: "   " });
    expect(body.optional_probe_cycle_id).toBeNull();
  });

  it("composePlanInputs keeps a non-blank probe-cycle id", () => {
    const body = composePlanInputs({ ...emptyPlanForm(), optionalProbeCycleId: " c9 " });
    expect(body.optional_probe_cycle_id).toBe("c9");
  });

  it("emits snake_case wire keys (no camelCase leak)", () => {
    const body = composePlanInputs(formFromPlan(SAMPLE_PLAN));
    expect("optional_probe_cycle_id" in body).toBe(true);
    expect("nominal_value" in body).toBe(true);
    expect("optionalProbeCycleId" in body).toBe(false);
    expect("nominalValue" in body).toBe(false);
  });
});

describe("filterPlans", () => {
  const rows: InspectionPlan[] = [
    { ...SAMPLE_PLAN, plan_id: "a", product_id: "prod_bracket", feature_name: "Bore diameter" },
    { ...SAMPLE_PLAN, plan_id: "b", product_id: "prod_shaft", feature_name: "Length" },
    { ...SAMPLE_PLAN, plan_id: "c", product_id: "prod_flange", feature_name: "Flatness" },
  ];

  it("empty needle returns the list unchanged", () => {
    expect(filterPlans(rows, "")).toEqual(rows);
    expect(filterPlans(rows, "   ")).toEqual(rows);
  });

  it("matches case-insensitively on product_id", () => {
    expect(filterPlans(rows, "SHAFT").map((p) => p.plan_id)).toEqual(["b"]);
  });

  it("matches case-insensitively on feature_name", () => {
    expect(filterPlans(rows, "flat").map((p) => p.plan_id)).toEqual(["c"]);
  });

  it("returns multiple matches when the needle hits several rows", () => {
    expect(filterPlans(rows, "prod_").map((p) => p.plan_id)).toEqual([
      "a",
      "b",
      "c",
    ]);
  });
});

describe("tolBandLabel", () => {
  it("formats as [lower, upper]", () => {
    expect(tolBandLabel({ lower_tol: -0.05, upper_tol: 0.05 })).toBe(
      "[-0.05, 0.05]",
    );
    expect(tolBandLabel({ lower_tol: 0, upper_tol: 0.1 })).toBe("[0, 0.1]");
  });
});

describe("validatePlanForm", () => {
  const valid = {
    ...emptyPlanForm(),
    productId: "prod_x",
    featureName: "Bore",
    units: "mm",
    upperTol: "0.05",
    lowerTol: "-0.05",
  };

  it("accepts a well-formed form (no field errors)", () => {
    expect(validatePlanForm(valid)).toEqual({});
  });

  it("rejects a blank feature name", () => {
    expect(validatePlanForm({ ...valid, featureName: "  " })).toHaveProperty(
      "feature_name",
    );
  });

  it("rejects blank units", () => {
    expect(validatePlanForm({ ...valid, units: "" })).toHaveProperty("units");
  });

  it("rejects upper_tol <= lower_tol", () => {
    expect(
      validatePlanForm({ ...valid, upperTol: "0.05", lowerTol: "0.05" }),
    ).toHaveProperty("upper_tol");
    expect(
      validatePlanForm({ ...valid, upperTol: "-0.1", lowerTol: "0.1" }),
    ).toHaveProperty("upper_tol");
  });

  it("rejects non-numeric tolerances", () => {
    expect(
      validatePlanForm({ ...valid, upperTol: "abc", lowerTol: "0" }),
    ).toHaveProperty("upper_tol");
  });
});

describe("parsePlanValidationError", () => {
  it("extracts the typed body from a Tauri-wrapped error string", () => {
    const raw =
      "backend returned 400 Bad Request for /api/inspection-plans: " +
      '{"error":"validation_failed","fields":[' +
      '{"field":"feature_name","message":"required"}]}';
    const parsed = parsePlanValidationError(raw);
    expect(parsed).not.toBeNull();
    expect(parsed!.fields[0].field).toBe("feature_name");
  });

  it("returns null for a non-validation body", () => {
    expect(parsePlanValidationError("network error")).toBeNull();
    expect(
      parsePlanValidationError('{"error":"plan not found"}'),
    ).toBeNull();
  });
});
