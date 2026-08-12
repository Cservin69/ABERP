import { describe, expect, it } from "vitest";
import {
  DURABILITY_BANNER_HEADLINE,
  durabilityAlertOf,
  durabilityBannerDetail,
  formatDetectedAt,
  shouldShowDurabilityBanner,
} from "./durability-alert";
import type { DurabilityAlert, HealthResponse } from "./api";

function health(overrides: Partial<HealthResponse> = {}): HealthResponse {
  return {
    ok: true,
    binary_hash: "deadbeef",
    nav_xsd_version: "3.0",
    is_production_build: false,
    first_prod_launch_required: false,
    durability_alert: null,
    ...overrides,
  };
}

function alert(overrides: Partial<DurabilityAlert> = {}): DurabilityAlert {
  return {
    breach: "wal_vanished",
    message:
      "Durability loss detected on the tenant database: the write-ahead log VANISHED. " +
      "Recent writes may not have reached disk. Stop and recover.",
    detected_at: "2026-08-12T09:15:00Z",
    ...overrides,
  };
}

describe("shouldShowDurabilityBanner", () => {
  it("shows the banner when /health carries an alert", () => {
    expect(shouldShowDurabilityBanner(health({ durability_alert: alert() }))).toBe(
      true,
    );
  });

  it("stays down on a healthy tenant", () => {
    expect(shouldShowDurabilityBanner(health())).toBe(false);
  });

  it("stays down before the first probe resolves", () => {
    // A not-yet-probed backend must not manufacture an alarm. The topbar
    // already reports `probing backend…`; inventing a durability loss out of
    // a slow first probe is the cry-wolf failure that teaches an operator to
    // ignore this banner.
    expect(shouldShowDurabilityBanner(null)).toBe(false);
  });

  it("stays down against a pre-D7 backend that omits the key", () => {
    const older = health() as Partial<HealthResponse>;
    delete older.durability_alert;
    expect(shouldShowDurabilityBanner(older as HealthResponse)).toBe(false);
  });

  it("comes back up on the next poll — the banner holds no state of its own", () => {
    // The SPA re-probes every 10 s and re-mounts on reload. Because the
    // decision is a pure function of the LATEST response, and the backend
    // keeps the alert sticky, a reload cannot dismiss it. This pins the SPA
    // half of that contract: three successive responses, three showings.
    const responses = [
      health({ durability_alert: alert() }),
      health({ durability_alert: alert() }),
      health({ durability_alert: alert() }),
    ];
    expect(responses.map(shouldShowDurabilityBanner)).toEqual([true, true, true]);
  });

  it("only goes down when the BACKEND stops reporting it", () => {
    // i.e. after `Handle::clear_durability_alert`. Nothing in the SPA may
    // decide the alarm is over.
    expect(shouldShowDurabilityBanner(health({ durability_alert: alert() }))).toBe(
      true,
    );
    expect(shouldShowDurabilityBanner(health({ durability_alert: null }))).toBe(
      false,
    );
  });
});

describe("durabilityAlertOf", () => {
  it("normalises null and undefined to null", () => {
    expect(durabilityAlertOf(null)).toBeNull();
    expect(durabilityAlertOf(health())).toBeNull();
    const older = health() as Partial<HealthResponse>;
    delete older.durability_alert;
    expect(durabilityAlertOf(older as HealthResponse)).toBeNull();
  });

  it("passes the alert through unchanged", () => {
    const a = alert();
    expect(durabilityAlertOf(health({ durability_alert: a }))).toEqual(a);
  });
});

describe("DURABILITY_BANNER_HEADLINE", () => {
  it("carries the two instructions the operator must act on", () => {
    // Pinned as text because it IS the deliverable: the headline is fixed and
    // identical whatever the breach, so the operator reads the same two
    // instructions in the same place every time.
    expect(DURABILITY_BANNER_HEADLINE).toContain("DURABILITY LOSS DETECTED");
    expect(DURABILITY_BANNER_HEADLINE).toContain(
      "recent writes may not be persisting",
    );
    expect(DURABILITY_BANNER_HEADLINE).toContain("Stop and recover");
  });
});

describe("durabilityBannerDetail", () => {
  it("renders the backend message VERBATIM plus when it started", () => {
    // The backend message names the specific breach, which is what a recovery
    // actually turns on. It must not be paraphrased or truncated here.
    const detail = durabilityBannerDetail(alert());
    expect(detail).toContain("the write-ahead log VANISHED");
    expect(detail).toContain("first detected");
  });

  it("still renders the message when the timestamp is unusable", () => {
    // An alarm that fails to render because a timestamp did not parse is the
    // worst possible trade.
    const detail = durabilityBannerDetail(alert({ detected_at: "not-a-date" }));
    expect(detail).toContain("the write-ahead log VANISHED");
    expect(detail).not.toContain("Invalid Date");
    expect(detail).not.toContain("first detected");
  });
});

describe("formatDetectedAt", () => {
  it("formats a real RFC3339 instant", () => {
    expect(formatDetectedAt("2026-08-12T09:15:00Z")).not.toBe("");
  });

  it("returns empty for missing or unparseable input rather than throwing", () => {
    expect(formatDetectedAt(null)).toBe("");
    expect(formatDetectedAt(undefined)).toBe("");
    expect(formatDetectedAt("")).toBe("");
    expect(formatDetectedAt("not-a-date")).toBe("");
  });
});
