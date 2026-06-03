// S235 / PR-231 — Workshop dashboard format helpers.
//
// Pure functions, extracted from `Workshop.svelte` so vitest can pin
// the formatting behaviour without spinning up a Svelte component
// runtime (the codebase has no @testing-library/svelte by design —
// existing tests stay pure-module per the router.test.ts / format.test.ts
// patterns).

import type { AdapterStatusSnapshot } from "./api";

/** Compact label for an audit-ledger EventKind shown in the recent-
 * activity stream. Strips a leading namespace (`"system.foo"` →
 * `"foo"`) so the dashboard row stays tight; falls back to the raw
 * string when there is no namespace prefix (so unknown kinds are
 * surfaced honestly per CLAUDE.md rule 12 — no silent hiding). */
export function fmtEventKind(kind: string): string {
  if (kind === "") return "";
  const dotIdx = kind.indexOf(".");
  if (dotIdx === -1) return kind;
  return kind.slice(dotIdx + 1);
}

/** CSS-class suffix for an adapter status dot. Closed-vocab match:
 * `"enabled"` → positive; everything else → muted. A future
 * `"degraded"` / `"unhealthy"` widening would be a one-line addition
 * here. */
export function adapterDotClass(
  status: AdapterStatusSnapshot["status"],
): "ws-dot--positive" | "ws-dot--muted" {
  return status === "enabled" ? "ws-dot--positive" : "ws-dot--muted";
}

/** Format a minor-unit amount as a major-unit currency string for
 * the given locale. HUF + EUR are both stored as minor (cents) in the
 * backend payload so the divisor is uniform. */
export function fmtMinor(
  minor: number,
  currency: "HUF" | "EUR",
  lang: "hu" | "en",
): string {
  const major = minor / 100;
  try {
    return new Intl.NumberFormat(lang === "hu" ? "hu-HU" : "en-GB", {
      style: "currency",
      currency,
      maximumFractionDigits: 0,
    }).format(major);
  } catch {
    return `${major.toFixed(0)} ${currency}`;
  }
}

/** Resolve the operator's configured poll interval against the
 * `VITE_WORKSHOP_POLL_MS` env var. Bounded to [2_000, 600_000] so a
 * typo neither hammers the backend nor stalls the dashboard. Returns
 * `defaultMs` when the env value is missing or non-numeric.
 *
 * Pure; takes the raw value in so vitest can drive the boundaries
 * without touching `import.meta.env`. */
export function resolvePollInterval(raw: string | undefined, defaultMs: number): number {
  if (raw === undefined || raw === "") return defaultMs;
  const n = Number(raw);
  if (!Number.isFinite(n)) return defaultMs;
  return Math.max(2_000, Math.min(600_000, Math.floor(n)));
}
