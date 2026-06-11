// S354 / PR-42 (U16) — pure helpers for operator accept-on-behalf in
// `PricingJobDetail.svelte`. Kept out of the Svelte component (no
// render-test harness in this package) so the accept gate, the channel
// vocab, the form validation, and the inline-error copy are unit-pinned.
// The component wires straight to these.

import type { AcceptQuoteError, AuditEntryView } from "./api";

/** Off-channel acceptance medium. Closed vocab — mirrors the Rust
 * `operator_accept::ACCEPT_CHANNELS` and the storefront. */
export type AcceptChannel = "phone" | "email" | "in_person" | "other";

/** Channel options for the select, in display order, bilingual HU/EN. */
export interface AcceptChannelOption {
  value: AcceptChannel;
  label: string;
}

export const ACCEPT_CHANNEL_OPTIONS: readonly AcceptChannelOption[] = [
  { value: "phone", label: "Telefon / Phone" },
  { value: "email", label: "E-mail" },
  { value: "in_person", label: "Személyes / In person" },
  { value: "other", label: "Egyéb / Other" },
];

/** The JobState in which an operator may accept on the customer's behalf:
 * only once the quote was priced AND delivered to the storefront
 * (`Posted` ⇒ storefront `quoted`, awaiting acceptance). Mirrors the
 * backend `accept_quote_precheck` gate. */
export function isAcceptable(state: string): boolean {
  return state === "posted";
}

/** Has a SUCCESSFUL operator-accept already landed for this row? Derived
 * from the loaded audit page (a `QuotePricingOperatorAccepted` entry whose
 * payload `outcome === "success"`). Drives hiding the Accept button after
 * a synced accept — the backend is the safety net (409 on a re-attempt),
 * this is just UX. */
export function hasOperatorAccepted(events: AuditEntryView[]): boolean {
  return events.some(
    (e) =>
      e.kind === "QuotePricingOperatorAccepted" &&
      (e.payload as { outcome?: unknown } | null)?.outcome === "success",
  );
}

export interface AcceptFormDraft {
  channel: AcceptChannel | "";
  note: string;
}

/** Validate the accept form. Returns a bilingual inline error string, or
 * `null` when the form is submittable. Note is required; channel must be
 * chosen. */
export function validateAcceptForm(draft: AcceptFormDraft): string | null {
  if (draft.channel === "") {
    return "Válassz csatornát. / Pick a channel.";
  }
  if (draft.note.trim().length === 0) {
    return "Adj meg egy megjegyzést (mit mondott az ügyfél, mikor). / Add a note (what the customer said, when).";
  }
  return null;
}

/** Bilingual HU/EN inline message for a failed accept, keyed on the typed
 * code (and the `WritebackOutcome` tag for a 502 sync failure). */
export function acceptErrorInlineCopy(err: AcceptQuoteError): string {
  switch (err.code) {
    case "JobAlreadyAccepted":
      return "Ezt az ajánlatot már elfogadták. / This quote has already been accepted.";
    case "JobNotAcceptable":
      return "Ez a sor még nem fogadható el (csak árazott + elküldött ajánlat). / This row cannot be accepted yet (only a priced + delivered quote).";
    case "InvalidChannel":
      return "Érvénytelen csatorna. / Invalid channel.";
    case "EmptyNote":
      return "A megjegyzés kötelező. / The note is required.";
    case "NoteTooLong":
      return "A megjegyzés túl hosszú. / The note is too long.";
    case "StorefrontNotConfigured":
      return "Nincs beállítva a webshop kapcsolat (Beállítások → Ajánlatkérés). / Storefront not configured (Settings → Quote Intake).";
    case "WritebackFailed":
      return writebackFailureCopy(err.outcome);
    default:
      return err.message;
  }
}

/** The 502 sync-failure copy, keyed on the `WritebackOutcome` tag. The
 * local accept WAS recorded; only the storefront sync failed, so every
 * arm ends with the operator-actionable "retry available" cue. */
function writebackFailureCopy(outcome: string | null): string {
  const tail =
    " Az elfogadás rögzítve, de a webshop szinkron sikertelen — próbáld újra. / The accept was recorded but the storefront sync failed — retry.";
  switch (outcome) {
    case "routing_misconfigured":
      return `🛑 Útvonal-hiba — a webshop HTML-t adott vissza JSON helyett. / Routing misconfigured — the storefront returned HTML.${tail}`;
    case "unauthorized":
    case "forbidden":
      return `🛑 Hitelesítési hiba — token eltérés. / Auth failed — token mismatch.${tail}`;
    case "app_rejected":
      return `A webshop elutasította az elfogadást. / The storefront rejected the accept.${tail}`;
    case "app_errored":
      return `A webshop szerverhibát adott. / The storefront returned a server error.${tail}`;
    case "timeout":
    case "transport_error":
      return `Hálózati hiba a webshop felé. / Network error reaching the storefront.${tail}`;
    default:
      return `A webshop szinkron sikertelen. / The storefront sync failed.${tail}`;
  }
}
