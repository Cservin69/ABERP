// PR-223 / S227 — pure-helper module wiring the StatisticsPage's
// hygiene rows to InvoiceList / IncomingInvoiceList via deep-link
// URLs. The dashboard exists to surface action items
// ([[hulye-biztos]]) — a non-zero count must give the operator a
// one-click path to the rows behind it, not a number they must
// hand-translate into list filters.
//
// The plumbing has two halves:
//
//   1. `clickTargetForFlag(flag)` — given a hygiene flag identifier,
//      compose the `#/invoices?…` URL the dashboard row navigates to.
//      Returns `null` for flags we cannot deliver a hülye-biztos
//      filter for without a backend wire-shape extension (today: the
//      AR-side "outstanding past deadline" — `payment_deadline` is
//      not on `InvoiceListItem`; see brief out-of-scope clause). A
//      `null` keeps the row static.
//
//   2. `parseInvoicesUrl(hash)` — read a `#/invoices?…` hash and
//      extract the tab choice + per-tab filter init. Unknown vocab
//      is silently DISCARDED, never coerced (CLAUDE.md rule 7); a
//      drift in URL params from an old bookmark falls back to the
//      saved-prefs view rather than crashing or applying garbage.
//
// The filter init is consumed by `InvoiceList.svelte` (outgoing) and
// `IncomingInvoiceList.svelte` (incoming) — each list applies it on
// mount + on hashchange, then strips the consumed params from the
// URL via `history.replaceState` so a later refresh / browser-back
// doesn't reapply a stale init.
//
// Pinned by `hygiene-clickthrough.test.ts`.

import type { InvoiceState, RowKind } from "./api";

/** Closed-vocab of hygiene flags the StatisticsPage surfaces (mirrors
 * `HygienePanel` in `api.ts` — backend's `serve::HygienePanel`). One
 * entry per renderable row on the StatisticsPage's "Hygiene" section. */
export type HygieneFlag =
  | "outgoing_pending"
  | "outgoing_rejected"
  | "outgoing_abandoned"
  | "restored_no_partner"
  | "outstanding_past_deadline"
  | "payable_past_deadline"
  | "storno_chain"
  | "modification_chain";

/** Closed-vocab for the outgoing list's synthetic hygiene predicate.
 * Drives `filterInvoices` to AND a hygiene gate on top of the existing
 * state / currency / kind / needle gates. `null` is the default (open
 * gate). Each variant has a defined predicate on the row's existing
 * wire shape — no new field on `InvoiceListItem`, no new endpoint. */
export type OutgoingHygieneFacet = "pending" | "no_partner";

/** Closed-vocab for the incoming list's synthetic hygiene predicate.
 * Today's only variant is `past_deadline` — incoming rows already
 * carry `payment_deadline` (PR-179) so the filter is exact. */
export type IncomingHygieneFacet = "past_deadline";

/** Subset of `InvoiceState` that the dashboard's `outgoing_pending_count`
 * sums over. Mirrors `reports::CountedKind::PendingDraft` in
 * `reports.rs`: states where the invoice has a draft OR an attempt
 * but NO submission response (`Submitted` / `Recovered` count as
 * `Counted`, not `PendingDraft`, so they are excluded here). Pinned
 * by `pending-states-mirror-classify-pendingdraft`. */
export const PENDING_STATES: readonly InvoiceState[] = [
  "Ready",
  "Pending",
  "PendingNavExists",
];

/** The "Outgoing" / "Incoming" tab segmented control on the
 * `#/invoices` route. Mirrors `InvoiceTab` in
 * `./invoice-tab-persistence.ts` — kept duplicated here rather than
 * imported to avoid a circular dep between persistence and
 * URL-parsing helpers (CLAUDE.md rule 2 — one string, no new module). */
export type ClickThroughTab = "outgoing" | "incoming";

/** URL init for the outgoing tab. Each field is `undefined` when the
 * URL did not name it — the list reads the absent fields as "no
 * override, keep the saved-prefs value". */
export interface OutgoingUrlInit {
  state?: "All" | InvoiceState;
  row_kind?: "All" | RowKind;
  hygiene?: OutgoingHygieneFacet | null;
}

/** URL init for the incoming tab. */
export interface IncomingUrlInit {
  hygiene?: IncomingHygieneFacet | null;
}

/** Parsed shape of a `#/invoices?…` hash. `tab` is `null` when the
 * URL did not name `?tab=`; the App-level reader keeps the existing
 * persisted tab in that case. */
export interface InvoicesUrlInit {
  tab: ClickThroughTab | null;
  outgoing: OutgoingUrlInit;
  incoming: IncomingUrlInit;
  /** `true` iff the URL carries at least one consumed param. A `false`
   * value means the list should NOT override its saved-prefs view —
   * the operator just navigated to `#/invoices` with no init. */
  hasInit: boolean;
}

/** The empty / no-op URL init. Returned by `parseInvoicesUrl` when
 * the hash has no recognised query params. */
export const EMPTY_URL_INIT: InvoicesUrlInit = {
  tab: null,
  outgoing: {},
  incoming: {},
  hasInit: false,
};

/** Compose the `#/invoices?…` URL the StatisticsPage hygiene row
 * navigates to. Returns `null` for flags we cannot deliver an exact
 * filter for from the existing wire shape (today: the AR-side
 * `outstanding_past_deadline` — `InvoiceListItem` has no
 * `payment_deadline` and the brief's out-of-scope clause forbids
 * adding it). The dashboard renders `null`-target flags as static
 * dim rows (no chevron, no click handler).
 *
 * Mapping (mirrors S227 brief, with corrections):
 *
 *   outgoing_pending           → tab=outgoing&hygiene=pending
 *   outgoing_rejected          → tab=outgoing&state=Rejected
 *   outgoing_abandoned         → tab=outgoing&state=Abandoned
 *   restored_no_partner        → tab=outgoing&kind=ExtNav&hygiene=no_partner
 *   outstanding_past_deadline  → null (no row field; static)
 *   payable_past_deadline      → tab=incoming&hygiene=past_deadline
 *   storno_chain               → tab=outgoing&state=Storno
 *   modification_chain         → tab=outgoing&state=Amended
 *
 * The storno / modification mappings hit invoices whose derived
 * state IS `Storno` / `Amended` (i.e., the BASE of a chain). The
 * dashboard's `storno_chain_count` / `modification_chain_count`
 * counts CHAIN-ENTRY events in period (an invoice stornoed twice
 * contributes 2); the list-row count is INVOICE rows (the same
 * base would appear once). The two counts can therefore diverge —
 * an asymmetry flagged in the S227 report. */
export function clickTargetForFlag(flag: HygieneFlag): { hash: string } | null {
  switch (flag) {
    case "outgoing_pending":
      return { hash: "#/invoices?tab=outgoing&hygiene=pending" };
    case "outgoing_rejected":
      return { hash: "#/invoices?tab=outgoing&state=Rejected" };
    case "outgoing_abandoned":
      return { hash: "#/invoices?tab=outgoing&state=Abandoned" };
    case "restored_no_partner":
      return {
        hash: "#/invoices?tab=outgoing&kind=ExtNav&hygiene=no_partner",
      };
    case "outstanding_past_deadline":
      // `payment_deadline` is not on `InvoiceListItem` wire shape.
      // Adding it is a backend change the S227 brief lists as
      // out-of-scope, so the row stays static. The dashboard surfaces
      // the count; the operator narrows manually for v1.
      return null;
    case "payable_past_deadline":
      return { hash: "#/invoices?tab=incoming&hygiene=past_deadline" };
    case "storno_chain":
      return { hash: "#/invoices?tab=outgoing&state=Storno" };
    case "modification_chain":
      return { hash: "#/invoices?tab=outgoing&state=Amended" };
  }
}

/** Closed-vocab list mirrors for cheap runtime validation. The TS
 * compiler already type-checks the unions; these runtime tables
 * defend against a URL hand-typed by an operator (or pasted from a
 * stale bookmark) that names a vocab value the SPA no longer
 * recognises. */
const LEGAL_INVOICE_STATES: readonly InvoiceState[] = [
  "Unknown",
  "Ready",
  "Pending",
  "PendingNavExists",
  "Submitted",
  "Recovered",
  "Finalized",
  "Rejected",
  "Storno",
  "Amended",
  "Abandoned",
];

const LEGAL_ROW_KINDS: readonly RowKind[] = ["Own", "ExtNav"];

const LEGAL_TABS: readonly ClickThroughTab[] = ["outgoing", "incoming"];

const LEGAL_OUTGOING_HYGIENE: readonly OutgoingHygieneFacet[] = [
  "pending",
  "no_partner",
];

const LEGAL_INCOMING_HYGIENE: readonly IncomingHygieneFacet[] = [
  "past_deadline",
];

/** Parse a `#/invoices?…` hash (or just the query-string portion)
 * into an [`InvoicesUrlInit`]. Unknown vocab is silently discarded;
 * the absence of a key means the list keeps its saved-prefs value
 * on that axis.
 *
 * Accepts:
 *   - the full hash with leading `#` (`#/invoices?tab=outgoing&...`)
 *   - the hash without `#` (`/invoices?...`)
 *   - the query-string only (`tab=outgoing&...`)
 *
 * Anything else (a hash naming a different route, or a hash with no
 * `?`) returns `EMPTY_URL_INIT`. */
export function parseInvoicesUrl(hashOrQuery: string): InvoicesUrlInit {
  const query = extractQueryString(hashOrQuery);
  if (query === null) return { ...EMPTY_URL_INIT };
  const params = parseQueryString(query);
  const out: InvoicesUrlInit = {
    tab: null,
    outgoing: {},
    incoming: {},
    hasInit: false,
  };
  const tab = params.get("tab");
  if (tab !== null && LEGAL_TABS.includes(tab as ClickThroughTab)) {
    out.tab = tab as ClickThroughTab;
    out.hasInit = true;
  }
  const state = params.get("state");
  if (state !== null) {
    if (state === "All") {
      out.outgoing.state = "All";
      out.hasInit = true;
    } else if (LEGAL_INVOICE_STATES.includes(state as InvoiceState)) {
      out.outgoing.state = state as InvoiceState;
      out.hasInit = true;
    }
  }
  const kind = params.get("kind");
  if (kind !== null) {
    if (kind === "All") {
      out.outgoing.row_kind = "All";
      out.hasInit = true;
    } else if (LEGAL_ROW_KINDS.includes(kind as RowKind)) {
      out.outgoing.row_kind = kind as RowKind;
      out.hasInit = true;
    }
  }
  const hygiene = params.get("hygiene");
  if (hygiene !== null) {
    // The same `hygiene` param string drives both tabs — the legal
    // vocab differs per tab so we route the value into whichever
    // bucket it belongs to (or both, if a future vocab overlap is
    // legitimate; today the two sets are disjoint).
    if (LEGAL_OUTGOING_HYGIENE.includes(hygiene as OutgoingHygieneFacet)) {
      out.outgoing.hygiene = hygiene as OutgoingHygieneFacet;
      out.hasInit = true;
    }
    if (LEGAL_INCOMING_HYGIENE.includes(hygiene as IncomingHygieneFacet)) {
      out.incoming.hygiene = hygiene as IncomingHygieneFacet;
      out.hasInit = true;
    }
  }
  return out;
}

function extractQueryString(raw: string): string | null {
  let s = raw;
  if (s.startsWith("#")) s = s.slice(1);
  if (s.startsWith("/")) s = s.slice(1);
  const qIdx = s.indexOf("?");
  if (qIdx < 0) {
    // No query separator at all — but allow callers that pass just
    // the query string directly (no leading `?`). Detect by an `=`
    // sign or `&`; bare `invoices` returns null.
    if (s.includes("=") || s.includes("&")) return s;
    return null;
  }
  // Trim the route slug (must be `invoices` to apply); a hash naming a
  // different route returns null so a stray `?tab=` doesn't bleed
  // filter init into a non-invoices screen.
  const slug = s.slice(0, qIdx);
  if (slug !== "" && slug !== "invoices") return null;
  return s.slice(qIdx + 1);
}

function parseQueryString(qs: string): Map<string, string> {
  const out = new Map<string, string>();
  if (qs.length === 0) return out;
  for (const part of qs.split("&")) {
    if (part.length === 0) continue;
    const eqIdx = part.indexOf("=");
    if (eqIdx < 0) {
      // Bare key (no `=`) treated as the empty string; keeps the parser
      // tolerant of hand-typed URLs.
      out.set(decodeURIComponent(part), "");
    } else {
      const key = decodeURIComponent(part.slice(0, eqIdx));
      const value = decodeURIComponent(part.slice(eqIdx + 1));
      // First key wins — a duplicate `?tab=outgoing&tab=incoming`
      // keeps `outgoing`. Mirror the URLSearchParams `get` posture
      // without depending on the global (this module stays vitest-
      // mountable in jsdom-free environments).
      if (!out.has(key)) out.set(key, value);
    }
  }
  return out;
}
