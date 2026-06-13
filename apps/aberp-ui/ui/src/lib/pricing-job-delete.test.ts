// S391/F — tests for the operator delete of a permanently-Failed pricing
// job. This package has no Svelte render harness, so the component wires
// straight to the `deleteQuotePricingJob` shim and the gate is pinned here:
// it forwards the quoteId to the `delete_quote_pricing_job` Tauri command
// and surfaces a backend 409 (JobNotDeletable) as a rejection.

import { afterEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { deleteQuotePricingJob } from "./api";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

afterEach(() => {
  vi.mocked(invoke).mockReset();
});

describe("deleteQuotePricingJob shim", () => {
  it("forwards quoteId to the delete command and resolves on success", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await expect(deleteQuotePricingJob("q-1")).resolves.toBeUndefined();
    expect(invoke).toHaveBeenCalledWith("delete_quote_pricing_job", {
      quoteId: "q-1",
    });
  });

  it("rejects when the backend refuses a non-Failed row (409)", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(
      new Error(
        'backend returned 409 Conflict for /api/quote-pricing-jobs/q: {"error":"JobNotDeletable","state":"posting_back"}',
      ),
    );
    await expect(deleteQuotePricingJob("q-1")).rejects.toThrow("409");
  });
});
