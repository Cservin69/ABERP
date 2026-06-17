// S441 / ADR-0086 — pure view-model for the "Sign in with DÁP" stub on the
// Tenants screen. No Tauri / DOM dependency, so it is unit-testable in
// isolation (vitest). The real operator-login overlay lands when the
// szeusz.gov.hu OIDC transport replaces the mock (OidcDapTransport).

import type { TenantRow, DapMockIdentity } from "./api";

/** Whether the "Sign in with DÁP" button renders for a tenant row, and its
 * label. The button appears ONLY when the tenant has `dap_enabled` (the
 * Defense-line opt-in) — mirroring ADR-0086 §6 "the path appears only if
 * eligible, otherwise it does not render at all". */
export function dapButtonState(row: TenantRow): { show: boolean; label: string } {
  if (!row.dap_enabled) {
    return { show: false, label: "" };
  }
  return { show: true, label: "Sign in with DÁP" };
}

/** A one-line operator-facing summary of a completed mock login, for the
 * inline confirmation after the button is pressed. */
export function dapLoginSummary(identity: DapMockIdentity): string {
  const tag = identity.mock ? " (mock)" : "";
  return `Signed in as ${identity.display_name} — ${identity.subject}${tag}`;
}
