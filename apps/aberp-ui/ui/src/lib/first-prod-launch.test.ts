import { describe, expect, it } from "vitest";
import {
  FIRST_PROD_LAUNCH_CONFIRM_TOKEN,
  firstProdLaunchProceedEnabled,
  shouldShowFirstProdLaunchModal,
} from "./first-prod-launch";
import type { HealthResponse } from "./api";

function health(overrides: Partial<HealthResponse>): HealthResponse {
  return {
    ok: true,
    binary_hash: "deadbeef",
    nav_xsd_version: "3.0",
    is_production_build: false,
    first_prod_launch_required: false,
    ...overrides,
  };
}

describe("shouldShowFirstProdLaunchModal", () => {
  it("shows the modal when /health says required (prod, no touchfile)", () => {
    expect(
      shouldShowFirstProdLaunchModal(
        health({ is_production_build: true, first_prod_launch_required: true }),
      ),
    ).toBe(true);
  });

  it("hides the modal when /health says not required (prod, touchfile present)", () => {
    expect(
      shouldShowFirstProdLaunchModal(
        health({ is_production_build: true, first_prod_launch_required: false }),
      ),
    ).toBe(false);
  });

  it("hides the modal on a dev build regardless of the flag", () => {
    expect(
      shouldShowFirstProdLaunchModal(
        health({ is_production_build: false, first_prod_launch_required: false }),
      ),
    ).toBe(false);
  });

  it("treats a not-yet-probed (null) health as not required", () => {
    expect(shouldShowFirstProdLaunchModal(null)).toBe(false);
  });
});

describe("firstProdLaunchProceedEnabled", () => {
  it("is disabled until ABERP is typed exactly", () => {
    expect(firstProdLaunchProceedEnabled("")).toBe(false);
    expect(firstProdLaunchProceedEnabled("aberp")).toBe(false); // wrong case
    expect(firstProdLaunchProceedEnabled("ABERP ")).toBe(false); // trailing space
    expect(firstProdLaunchProceedEnabled(" ABERP")).toBe(false); // leading space
    expect(firstProdLaunchProceedEnabled("ABER")).toBe(false); // incomplete
    expect(firstProdLaunchProceedEnabled("ABERPP")).toBe(false); // extra char
  });

  it("is enabled only on the exact case-sensitive token", () => {
    expect(firstProdLaunchProceedEnabled(FIRST_PROD_LAUNCH_CONFIRM_TOKEN)).toBe(true);
    expect(firstProdLaunchProceedEnabled("ABERP")).toBe(true);
  });
});
