// S431 — vitest pins for the AVL pure helpers.

import { describe, it, expect } from "vitest";

import type { AvlVendor } from "./api";
import {
  composeVendorEdit,
  composeVendorInputs,
  EMPTY_VENDOR_FILTER,
  emptyVendorForm,
  filterVendors,
  formFromVendor,
  isVendorFilterEmpty,
  parseVendorValidationError,
  statusBlocksPo,
  statusChipClass,
  toggleCategory,
  vendorIsOverdue,
} from "./avl-vendors";

const SAMPLE: AvlVendor = {
  id: "avl_01HZ",
  partner_id: "partner-acme",
  approved_status: "approved",
  approval_categories: ["itar", "defense"],
  approved_until_utc: "2030-01-01T00:00:00Z",
  screening_notes: "ok",
  reviewer_login: "op",
  reviewed_at_utc: "2026-01-01T00:00:00Z",
  revoked_reason: null,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
};

describe("emptyVendorForm", () => {
  it("defaults to a pending, uncategorised, no-expiry vendor", () => {
    const f = emptyVendorForm();
    expect(f.partnerId).toBe("");
    expect(f.approvedStatus).toBe("pending");
    expect(f.categories).toEqual([]);
    expect(f.approvedUntil).toBe("");
  });
});

describe("form ↔ wire round-trip", () => {
  it("formFromVendor → composeVendorInputs preserves the editable fields", () => {
    const form = formFromVendor(SAMPLE);
    expect(form.partnerId).toBe("partner-acme");
    expect(form.categories).toEqual(["itar", "defense"]);
    const wire = composeVendorInputs(form);
    expect(wire.partner_id).toBe("partner-acme");
    expect(wire.approved_status).toBe("approved");
    expect(wire.approval_categories).toEqual(["itar", "defense"]);
    expect(wire.approved_until_utc).toBe("2030-01-01T00:00:00Z");
  });

  it("composeVendorInputs trims partner id + maps empty expiry to null", () => {
    const form = { ...emptyVendorForm(), partnerId: "  p1  ", approvedUntil: "  " };
    const wire = composeVendorInputs(form);
    expect(wire.partner_id).toBe("p1");
    expect(wire.approved_until_utc).toBeNull();
  });

  it("composeVendorEdit carries only categories/until/notes", () => {
    const form = { ...formFromVendor(SAMPLE), approvedUntil: "2031-06-01T00:00:00Z" };
    const edit = composeVendorEdit(form);
    expect(edit).toEqual({
      approval_categories: ["itar", "defense"],
      approved_until_utc: "2031-06-01T00:00:00Z",
      screening_notes: "ok",
    });
    expect(edit).not.toHaveProperty("partner_id");
    expect(edit).not.toHaveProperty("approved_status");
  });
});

describe("toggleCategory", () => {
  it("adds when absent, removes when present, immutably", () => {
    const a = toggleCategory([], "itar");
    expect(a).toEqual(["itar"]);
    const b = toggleCategory(a, "ear99");
    expect(b).toEqual(["itar", "ear99"]);
    const c = toggleCategory(b, "itar");
    expect(c).toEqual(["ear99"]);
    // original untouched
    expect(a).toEqual(["itar"]);
  });
});

describe("statusBlocksPo + statusChipClass", () => {
  it("only suspended/revoked block a PO", () => {
    expect(statusBlocksPo("pending")).toBe(false);
    expect(statusBlocksPo("approved")).toBe(false);
    expect(statusBlocksPo("conditional")).toBe(false);
    expect(statusBlocksPo("suspended")).toBe(true);
    expect(statusBlocksPo("revoked")).toBe(true);
  });

  it("chip class signals approved=ok, blocked=err, pending=neutral", () => {
    expect(statusChipClass("approved")).toContain("chip--ok");
    expect(statusChipClass("conditional")).toContain("chip--ok");
    expect(statusChipClass("suspended")).toContain("chip--err");
    expect(statusChipClass("revoked")).toContain("chip--err");
    expect(statusChipClass("pending")).toContain("chip--neutral");
  });
});

describe("filterVendors", () => {
  const rows: AvlVendor[] = [
    SAMPLE,
    { ...SAMPLE, id: "avl_2", partner_id: "partner-beta", approved_status: "suspended" },
    { ...SAMPLE, id: "avl_3", partner_id: "gamma-corp", approved_status: "revoked" },
  ];

  it("empty filter returns everything", () => {
    expect(isVendorFilterEmpty(EMPTY_VENDOR_FILTER)).toBe(true);
    expect(filterVendors(rows, EMPTY_VENDOR_FILTER)).toHaveLength(3);
  });

  it("needle matches partner id case-insensitively", () => {
    expect(filterVendors(rows, { needle: "PARTNER", status: "All" })).toHaveLength(2);
    expect(filterVendors(rows, { needle: "gamma", status: "All" })).toHaveLength(1);
  });

  it("status facet ANDs on top of the needle", () => {
    expect(filterVendors(rows, { needle: "partner", status: "suspended" })).toHaveLength(1);
    expect(filterVendors(rows, { needle: "", status: "revoked" })).toHaveLength(1);
  });
});

describe("vendorIsOverdue", () => {
  const now = new Date("2026-06-16T00:00:00Z");

  it("past approved_until on a non-revoked vendor is overdue", () => {
    const v = { ...SAMPLE, approved_until_utc: "2025-01-01T00:00:00Z" };
    expect(vendorIsOverdue(v, now)).toBe(true);
  });

  it("future approved_until is NOT overdue", () => {
    expect(vendorIsOverdue(SAMPLE, now)).toBe(false);
  });

  it("revoked vendor never reminds, even if past due", () => {
    const v = { ...SAMPLE, approved_status: "revoked" as const, approved_until_utc: "2025-01-01T00:00:00Z" };
    expect(vendorIsOverdue(v, now)).toBe(false);
  });

  it("no expiry / unparseable date is not overdue", () => {
    expect(vendorIsOverdue({ ...SAMPLE, approved_until_utc: null }, now)).toBe(false);
    expect(vendorIsOverdue({ ...SAMPLE, approved_until_utc: "not-a-date" }, now)).toBe(false);
  });
});

describe("parseVendorValidationError", () => {
  it("peels a validation_failed body out of a Tauri-wrapped error string", () => {
    const raw =
      'invoke error: {"error":"validation_failed","fields":[{"field":"partner_id","message":"required"}]}';
    const parsed = parseVendorValidationError(raw);
    expect(parsed?.error).toBe("validation_failed");
    expect(parsed?.fields[0]).toEqual({ field: "partner_id", message: "required" });
  });

  it("returns null for a non-validation error", () => {
    expect(parseVendorValidationError("some other 500")).toBeNull();
    expect(parseVendorValidationError('{"error":"nope"}')).toBeNull();
  });
});
