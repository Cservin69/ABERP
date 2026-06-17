// S441 — vitest pins for the DÁP sign-in stub view-model.
import { describe, expect, it } from "vitest";
import { dapButtonState, dapLoginSummary } from "./dap-signin";
import type { TenantRow, DapMockIdentity } from "./api";

function row(overrides: Partial<TenantRow> = {}): TenantRow {
  return {
    slug: "prod",
    display_name: "Prod",
    state: "active",
    created_at: "2026-06-17T00:00:00Z",
    running: true,
    nav_enabled: true,
    dap_enabled: false,
    qc_calibration_stale_window_seconds: 86400,
    ...overrides,
  };
}

describe("dapButtonState", () => {
  it("hides the button when dap_enabled is false", () => {
    const s = dapButtonState(row({ dap_enabled: false }));
    expect(s.show).toBe(false);
    expect(s.label).toBe("");
  });

  it("shows the button when dap_enabled is true", () => {
    const s = dapButtonState(row({ dap_enabled: true }));
    expect(s.show).toBe(true);
    expect(s.label).toBe("Sign in with DÁP");
  });
});

describe("dapLoginSummary", () => {
  it("renders a mock-tagged summary line", () => {
    const id: DapMockIdentity = {
      subject: "hu-mock-citizen-0001",
      display_name: "Mock DÁP Operator",
      attested_at_utc: "2026-06-17T00:00:00Z",
      mock: true,
    };
    expect(dapLoginSummary(id)).toBe(
      "Signed in as Mock DÁP Operator — hu-mock-citizen-0001 (mock)",
    );
  });

  it("omits the mock tag for a real identity", () => {
    const id: DapMockIdentity = {
      subject: "hu-citizen-1",
      display_name: "Real Op",
      attested_at_utc: "2026-06-17T00:00:00Z",
      mock: false,
    };
    expect(dapLoginSummary(id)).toBe("Signed in as Real Op — hu-citizen-1");
  });
});
