// S354 / PR-42 (U16) — unit tests for operator accept-on-behalf: the
// accept gate, audit-derived "already accepted" check, form validation,
// the api shim's invoke wiring + error parsing, and the bilingual inline
// copy. Mirrors `pricing-material-edit.test.ts`.

import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  acceptQuotePricingJob,
  parseAcceptQuoteError,
  AcceptQuoteError,
  type AuditEntryView,
} from "./api";
import {
  ACCEPT_CHANNEL_OPTIONS,
  acceptErrorInlineCopy,
  hasOperatorAccepted,
  isAcceptable,
  validateAcceptForm,
} from "./pricing-operator-accept";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

afterEach(() => {
  vi.mocked(invoke).mockReset();
});

function ev(kind: string, payload: Record<string, unknown> = {}): AuditEntryView {
  return {
    seq: 1,
    kind,
    actor: "operator-ada",
    occurred_at: "2026-06-11T10:00:00Z",
    chain_base_invoice_id: null,
    payload,
  };
}

describe("isAcceptable", () => {
  it("is true only for a Posted (priced + delivered) row", () => {
    expect(isAcceptable("posted")).toBe(true);
    for (const s of ["fetched", "extracting", "pricing", "rendering", "posting_back", "failed"]) {
      expect(isAcceptable(s)).toBe(false);
    }
  });
});

describe("hasOperatorAccepted", () => {
  it("is true once a successful operator-accept event is present", () => {
    expect(
      hasOperatorAccepted([
        ev("QuotePricingPosted"),
        ev("QuotePricingOperatorAccepted", { outcome: "success" }),
      ]),
    ).toBe(true);
  });

  it("ignores a FAILED operator-accept event (retry still allowed)", () => {
    expect(
      hasOperatorAccepted([
        ev("QuotePricingOperatorAccepted", { outcome: "routing_misconfigured" }),
      ]),
    ).toBe(false);
  });

  it("is false with no operator-accept events", () => {
    expect(hasOperatorAccepted([ev("QuotePricingPosted")])).toBe(false);
  });
});

describe("validateAcceptForm", () => {
  it("requires a channel", () => {
    expect(validateAcceptForm({ channel: "", note: "ok" })).toMatch(/csatorn|channel/i);
  });
  it("requires a non-empty note", () => {
    expect(validateAcceptForm({ channel: "phone", note: "   " })).toMatch(/megjegyz|note/i);
  });
  it("passes a complete form", () => {
    expect(validateAcceptForm({ channel: "phone", note: "customer said yes" })).toBeNull();
  });
});

describe("ACCEPT_CHANNEL_OPTIONS", () => {
  it("offers exactly the four closed channels, bilingual", () => {
    expect(ACCEPT_CHANNEL_OPTIONS.map((o) => o.value)).toEqual([
      "phone",
      "email",
      "in_person",
      "other",
    ]);
    for (const o of ACCEPT_CHANNEL_OPTIONS) {
      expect(o.label.length).toBeGreaterThan(0);
    }
  });
});

describe("acceptQuotePricingJob shim", () => {
  it("forwards quoteId + body and returns the outcome verbatim", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      quote_id: "q-1",
      accepted_at_ms: 1_780_000_000_000,
      status: "accepted",
      outcome: "success",
    });
    const out = await acceptQuotePricingJob("q-1", {
      channel: "phone",
      note: "confirmed by phone",
      customer_confirmation_path: "/tmp/cc.png",
    });
    expect(invoke).toHaveBeenCalledWith("accept_quote_pricing_job", {
      quoteId: "q-1",
      body: {
        channel: "phone",
        note: "confirmed by phone",
        customer_confirmation_path: "/tmp/cc.png",
      },
    });
    expect(out.outcome).toBe("success");
    expect(out.status).toBe("accepted");
  });

  it("rejects with a typed AcceptQuoteError on a 409 already-accepted", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(
      new Error(
        'backend returned 409 Conflict for /api/quote-pricing-jobs/q/accept: {"error":"JobAlreadyAccepted"}',
      ),
    );
    await expect(
      acceptQuotePricingJob("q-1", { channel: "phone", note: "x" }),
    ).rejects.toMatchObject({ code: "JobAlreadyAccepted", status: 409 });
  });

  it("rejects with a typed AcceptQuoteError carrying the outcome on a 502 writeback failure", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(
      new Error(
        'backend returned 502 Bad Gateway for /api/quote-pricing-jobs/q/accept: {"error":"WritebackFailed","outcome":"routing_misconfigured","retry_available":true}',
      ),
    );
    await expect(
      acceptQuotePricingJob("q-1", { channel: "phone", note: "x" }),
    ).rejects.toMatchObject({
      code: "WritebackFailed",
      status: 502,
      outcome: "routing_misconfigured",
    });
  });
});

describe("parseAcceptQuoteError", () => {
  it("falls back to unknown-coded on an unparseable string", () => {
    const err = parseAcceptQuoteError(new Error("totally opaque failure"));
    expect(err).toBeInstanceOf(AcceptQuoteError);
    expect(err.code).toBe("unknown");
  });
});

describe("acceptErrorInlineCopy", () => {
  it("maps 409 already-accepted to bilingual copy", () => {
    const copy = acceptErrorInlineCopy(
      new AcceptQuoteError("JobAlreadyAccepted", 409, "raw"),
    );
    expect(copy).toMatch(/már elfogadt|already been accepted/i);
  });

  it("maps a 502 routing-misconfigured outcome to the routing copy + retry cue", () => {
    const copy = acceptErrorInlineCopy(
      new AcceptQuoteError("WritebackFailed", 502, "raw", "routing_misconfigured"),
    );
    expect(copy).toMatch(/Útvonal|Routing/i);
    expect(copy).toMatch(/retry|újra/i);
  });

  it("maps a 502 app_rejected outcome to the rejected copy", () => {
    const copy = acceptErrorInlineCopy(
      new AcceptQuoteError("WritebackFailed", 502, "raw", "app_rejected"),
    );
    expect(copy).toMatch(/elutasít|rejected/i);
  });

  it("falls back to the raw message for an unknown code", () => {
    const copy = acceptErrorInlineCopy(new AcceptQuoteError("unknown", 0, "weird thing"));
    expect(copy).toBe("weird thing");
  });
});
