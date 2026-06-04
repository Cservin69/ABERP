// S235 / PR-231 — Workshop dashboard format helpers.
//
// Pure functions, extracted from `Workshop.svelte` so vitest can pin
// the formatting behaviour without spinning up a Svelte component
// runtime (the codebase has no @testing-library/svelte by design —
// existing tests stay pure-module per the router.test.ts / format.test.ts
// patterns).

import type { AdapterStatus } from "./api";

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

/** S240 / PR-234 — CSS-class suffix for an adapter status dot.
 * Closed-vocab match against the live-registry `AdapterStatus`. Maps:
 *
 * - `healthy`   → positive (green)   `--color-signal-positive`
 * - `degraded`  → warning  (amber)   `--color-signal-warning`
 * - `starting`  → warning  (amber)   `--color-signal-warning`
 * - `unhealthy` → negative (red)     `--color-signal-negative`
 * - `stopped`   → muted    (gray)    `--color-text-muted`
 *
 * Pure switch — no fallthrough default so TypeScript catches a new
 * variant addition at compile time (CLAUDE.md rule 7: surface drift,
 * don't blend it). */
export function adapterDotClass(
  status: AdapterStatus,
):
  | "ws-dot--positive"
  | "ws-dot--warning"
  | "ws-dot--negative"
  | "ws-dot--muted" {
  switch (status) {
    case "healthy":
      return "ws-dot--positive";
    case "degraded":
    case "starting":
      return "ws-dot--warning";
    case "unhealthy":
      return "ws-dot--negative";
    case "stopped":
      return "ws-dot--muted";
  }
}

/** S240 / PR-234 — bilingual chip label for an adapter status. Pure
 *  table; same closed-vocab discipline as `adapterDotClass`. */
export function adapterStatusLabel(
  status: AdapterStatus,
  lang: "hu" | "en",
): string {
  switch (status) {
    case "healthy":
      return lang === "hu" ? "Fut" : "Running";
    case "degraded":
      return lang === "hu" ? "Lassú" : "Degraded";
    case "unhealthy":
      return lang === "hu" ? "Leállt" : "Down";
    case "starting":
      return lang === "hu" ? "Induló" : "Starting";
    case "stopped":
      return lang === "hu" ? "Leállítva" : "Stopped";
  }
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
