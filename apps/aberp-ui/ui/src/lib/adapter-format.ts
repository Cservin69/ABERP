// S257 / PR-246 — pure closed-vocab formatting for the Adapters page.
//
// Extracted from AdaptersList/AdapterForm so the kind/status mappings
// are unit-testable without mounting a component (CLAUDE.md rule 9 —
// tests verify intent). The closed vocab here mirrors the Rust
// `AdapterKind` wire strings + the `adapter_health_status` vocab.

import type { AdapterKind, AdapterStatus } from "./api";

/** Display labels for each adapter kind, in picker display order. */
export const ADAPTER_KIND_LABELS: Record<AdapterKind, string> = {
  "barcode-scanner": "Barcode scanner",
  "label-printer": "Zebra label printer",
  "cnc-machine": "MTConnect CNC",
  robot: "UR RTDE robot",
};

/** Display order for the Add-wizard kind picker. */
export const ADAPTER_KIND_ORDER: AdapterKind[] = [
  "barcode-scanner",
  "label-printer",
  "cnc-machine",
  "robot",
];

/** Resolve a kind to its display label. Closed-vocab graceful degrade:
 * a SPA build older than the backend may see an unknown kind string —
 * render the raw value + log a console warning rather than crash (S257
 * forward-compat note). */
export function adapterKindLabel(kind: string): string {
  const label = ADAPTER_KIND_LABELS[kind as AdapterKind];
  if (label === undefined) {
    // eslint-disable-next-line no-console
    console.warn(`aberp: unknown adapter kind "${kind}" — rendering raw`);
    return kind;
  }
  return label;
}

/** Display labels for each live status. */
export const ADAPTER_STATUS_LABELS: Record<AdapterStatus, string> = {
  healthy: "Healthy",
  degraded: "Degraded",
  unhealthy: "Unhealthy",
  starting: "Starting",
  stopped: "Stopped",
};

export function adapterStatusLabel(status: string): string {
  return ADAPTER_STATUS_LABELS[status as AdapterStatus] ?? status;
}

/** Chip tone for a status. `degraded` + `starting` share the warning
 * tone (transitional / attention); an unknown status falls back to the
 * neutral `muted` tone rather than a misleading colour. */
export type AdapterStatusTone = "positive" | "warning" | "negative" | "muted";

export function adapterStatusTone(status: string): AdapterStatusTone {
  switch (status) {
    case "healthy":
      return "positive";
    case "degraded":
    case "starting":
      return "warning";
    case "unhealthy":
      return "negative";
    default:
      return "muted";
  }
}
