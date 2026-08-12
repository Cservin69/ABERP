// ADR-0110 D7 — pure decision logic for the durability-loss banner.
//
// The component (`DurabilityAlertBanner.svelte`) is deliberately thin: the
// decisions live here as pure functions so they are unit-testable in this
// package's node/vitest environment (no DOM, no testing-library), the same
// convention `first-prod-launch.ts` established.
//
// # Why the state comes from /health and never from the browser
//
// The backend's shared `aberp_db::Handle` owns the sticky alert; the SPA only
// renders it. That is what makes the banner survive a reload, show up
// identically in every window, and stay up until it is EXPLICITLY cleared. A
// `localStorage` flag or a component-local `$state` would all be dismissible by
// accident — a refresh, a navigation, a second window opened fresh — and a
// durability alarm the operator can silence by accident is not an alarm.

import type { HealthResponse, DurabilityAlert } from "./api";

/** The fixed headline. Deliberately not derived from the backend message:
 * whatever the breach turns out to be, the operator's first line is always the
 * same two instructions, in the same place, in the same words. */
export const DURABILITY_BANNER_HEADLINE =
  "⚠ DURABILITY LOSS DETECTED — recent writes may not be persisting. Stop and recover.";

/** Whether the red banner must be up, given the latest `/health` response.
 *
 * `null` (not yet probed, or the probe failed) is treated as "do not show".
 * That is the right default in BOTH directions: a backend that cannot be
 * reached already surfaces as `backend unreachable` in the topbar, and
 * inventing a durability alarm out of a network blip is exactly the
 * cry-wolf failure that makes an operator stop reading banners. */
export function shouldShowDurabilityBanner(
  health: HealthResponse | null,
): boolean {
  return durabilityAlertOf(health) !== null;
}

/** The alert itself, or `null`. Normalises `undefined` (an older backend that
 * does not carry the key at all) to `null` so callers have one shape. */
export function durabilityAlertOf(
  health: HealthResponse | null,
): DurabilityAlert | null {
  return health?.durability_alert ?? null;
}

/** The detail line under the headline: what the backend said, and since when.
 *
 * The backend message is rendered VERBATIM — it names the specific breach
 * (`the write-ahead log VANISHED`, `… was REPLACED (inode changed)`, …), which
 * is what a recovery actually turns on. */
export function durabilityBannerDetail(alert: DurabilityAlert): string {
  const since = formatDetectedAt(alert.detected_at);
  return since ? `${alert.message} (first detected ${since})` : alert.message;
}

/** Render the RFC3339 detection instant for the banner.
 *
 * Returns `""` for a missing or unparseable stamp rather than `Invalid Date`
 * or a thrown error: the banner's job is to be UP. An alarm that fails to
 * render because a timestamp did not parse is the worst possible trade. */
export function formatDetectedAt(detectedAt: string | null | undefined): string {
  if (!detectedAt) return "";
  const t = new Date(detectedAt);
  if (Number.isNaN(t.getTime())) return "";
  return t.toLocaleString();
}
