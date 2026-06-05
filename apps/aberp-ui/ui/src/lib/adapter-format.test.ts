// S257 / PR-246 — pure-helper tests for the Adapters page formatting.
// Mirrors the draft-delete.test.ts posture: exercise the closed-vocab
// mappings without mounting a component (CLAUDE.md rule 9).

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ADAPTER_KIND_LABELS,
  ADAPTER_KIND_ORDER,
  adapterKindLabel,
  adapterStatusLabel,
  adapterStatusTone,
} from "./adapter-format";
import type { AdapterKind, AdapterStatus } from "./api";

describe("adapterKindLabel", () => {
  it("labels every known kind", () => {
    expect(adapterKindLabel("barcode-scanner")).toBe("Barcode scanner");
    expect(adapterKindLabel("label-printer")).toBe("Zebra label printer");
    expect(adapterKindLabel("cnc-machine")).toBe("MTConnect CNC");
    expect(adapterKindLabel("robot")).toBe("UR RTDE robot");
  });

  it("every ordered kind has a label (no orphan in the picker)", () => {
    for (const k of ADAPTER_KIND_ORDER) {
      expect(ADAPTER_KIND_LABELS[k]).toBeTruthy();
    }
    // The order set equals the label key set — no kind silently absent.
    expect(new Set(ADAPTER_KIND_ORDER).size).toBe(
      Object.keys(ADAPTER_KIND_LABELS).length,
    );
  });

  it("gracefully degrades on an unknown kind (renders raw + warns)", () => {
    // A SPA build older than the backend could receive a kind the
    // closed vocab here doesn't carry — it must NOT crash.
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const out = adapterKindLabel("warp-drive" as AdapterKind);
    expect(out).toBe("warp-drive");
    expect(warn).toHaveBeenCalledOnce();
  });
});

describe("adapterStatusLabel + adapterStatusTone", () => {
  const cases: Array<[AdapterStatus, string, string]> = [
    ["healthy", "Healthy", "positive"],
    ["degraded", "Degraded", "warning"],
    ["unhealthy", "Unhealthy", "negative"],
    ["starting", "Starting", "warning"],
    ["stopped", "Stopped", "muted"],
  ];

  it.each(cases)("status %s → label %s, tone %s", (status, label, tone) => {
    expect(adapterStatusLabel(status)).toBe(label);
    expect(adapterStatusTone(status)).toBe(tone);
  });

  it("an unknown status falls back to the raw label + muted tone", () => {
    expect(adapterStatusLabel("quantum")).toBe("quantum");
    // muted, not a misleading positive/negative colour.
    expect(adapterStatusTone("quantum")).toBe("muted");
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});
