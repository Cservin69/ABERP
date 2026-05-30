// S166 / prod-prep PR #2 — pure decision logic for the one-time
// first-production-launch confirmation modal.
//
// The component (`FirstProdLaunchModal.svelte`) is intentionally thin:
// the two decisions that govern it live here as pure functions so they
// are unit-testable in the project's node/vitest environment (no DOM,
// no testing-library), matching the `product-combobox.ts` convention.

import type { HealthResponse } from "./api";

/** The exact confirmation token the operator must type to enable the
 * Proceed button. Case-sensitive (per the brief). */
export const FIRST_PROD_LAUNCH_CONFIRM_TOKEN = "ABERP";

/** Whether the SPA must block its main routes behind the first-launch
 * modal, given the latest `/health` response. `null` (not yet probed)
 * is treated as "not required" so the normal app is not gated on a
 * dev/test build before the first probe resolves; on a production build
 * the backend reports `first_prod_launch_required: true` and the modal
 * mounts as soon as that probe lands. */
export function shouldShowFirstProdLaunchModal(
  health: HealthResponse | null,
): boolean {
  return health?.first_prod_launch_required === true;
}

/** Whether the Proceed button is enabled: only when the operator has
 * typed the confirmation token EXACTLY (case-sensitive, no surrounding
 * whitespace tolerated — the operator must mean it). */
export function firstProdLaunchProceedEnabled(typed: string): boolean {
  return typed === FIRST_PROD_LAUNCH_CONFIRM_TOKEN;
}
