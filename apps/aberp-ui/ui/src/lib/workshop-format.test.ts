// S235 / PR-231 — vitest pin for the Workshop dashboard format
// helpers. Pure-function tests; no DOM, no @testing-library/svelte.

import { describe, expect, it } from "vitest";

import {
  adapterDotClass,
  fmtEventKind,
  fmtMinor,
  resolvePollInterval,
} from "./workshop-format";

describe("fmtEventKind", () => {
  it("strips a leading namespace prefix", () => {
    // S177 / PR-177 — AP module audit kinds carry a `system.` prefix.
    expect(fmtEventKind("system.IncomingInvoiceSyncCycleCompleted")).toBe(
      "IncomingInvoiceSyncCycleCompleted",
    );
    expect(fmtEventKind("mes.dispatch_shipped")).toBe("dispatch_shipped");
  });

  it("returns the raw kind when no namespace is present", () => {
    // CLAUDE.md rule 12 — surface unknown kinds honestly rather than
    // silently dropping them.
    expect(fmtEventKind("InvoiceIssued")).toBe("InvoiceIssued");
  });

  it("handles empty input without throwing", () => {
    expect(fmtEventKind("")).toBe("");
  });
});

describe("adapterDotClass", () => {
  it("maps 'enabled' onto the positive token slot", () => {
    expect(adapterDotClass("enabled")).toBe("ws-dot--positive");
  });
  it("maps 'disabled' onto the muted token slot", () => {
    expect(adapterDotClass("disabled")).toBe("ws-dot--muted");
  });
});

describe("fmtMinor", () => {
  it("renders HUF as integer forints in the locale style", () => {
    const hu = fmtMinor(1_234_500, "HUF", "hu");
    const en = fmtMinor(1_234_500, "HUF", "en");
    // Don't pin exact whitespace — locale-specific currency formatting
    // differs by Node ICU version. Pin only that the major figure
    // (12 345) appears in both renderings.
    expect(hu).toMatch(/12\D?345/);
    expect(en).toMatch(/12,345/);
  });

  it("renders EUR with the EUR symbol", () => {
    const out = fmtMinor(150_000, "EUR", "en");
    expect(out).toMatch(/1,500/);
    expect(out).toContain("€");
  });

  it("returns zero cleanly", () => {
    expect(fmtMinor(0, "HUF", "hu")).toMatch(/0/);
  });
});

describe("resolvePollInterval", () => {
  it("uses the default when the env var is missing", () => {
    expect(resolvePollInterval(undefined, 10_000)).toBe(10_000);
    expect(resolvePollInterval("", 10_000)).toBe(10_000);
  });

  it("clamps below the lower bound (2_000 ms)", () => {
    expect(resolvePollInterval("500", 10_000)).toBe(2_000);
    expect(resolvePollInterval("0", 10_000)).toBe(2_000);
  });

  it("clamps above the upper bound (600_000 ms / 10 min)", () => {
    expect(resolvePollInterval("9000000", 10_000)).toBe(600_000);
  });

  it("returns a parsed value when within range", () => {
    expect(resolvePollInterval("30000", 10_000)).toBe(30_000);
  });

  it("falls back to the default for non-numeric input", () => {
    expect(resolvePollInterval("abc", 10_000)).toBe(10_000);
  });
});
