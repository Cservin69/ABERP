// Row-shaped facet predicates for the two dashboard click-through
// drill-downs. Pure functions over a row + the clicked facet + today —
// no DOM, no component state, no fetch.
//
// WHY THESE LIVE HERE AND NOT INLINE IN THE COMPONENTS. They used to be
// inline, and the only thing pinning them was `?raw` source-text regex in
// `aging-facet-lockstep.test.ts` — this package mounts no components, so
// there was nothing else available. Text pins catch a DELETION; they do
// not catch a VERDICT FLIP. Turning
//
//     if (bucket === null) return false;   →   return true;
//
// leaves every grepped token in place, inverts "exclude this row" into
// "this row matches every bucket", and kept the whole suite green. The
// adversarial review found exactly that hole. Moving the predicates into
// a module the tests can CALL is the fix: the pins now assert which rows
// come back, so a flipped verdict fails on the row set regardless of how
// the source happens to read.
//
// The components keep their own concerns (needle search, status filters,
// sorting); what moved is only the part that has to agree, row for row,
// with a figure the backend already computed.

import {
  agingBucketFor,
  canonicalDeadlineIso,
  hasNoRecordedDeadline,
  type AgingBucket,
} from "./aging";
import type { IncomingInvoice, InvoiceListItem } from "./api";

/** The three outgoing states the dashboard's receivables aging counts —
 * its approximation of the backend's NAV-accepted ("counted") set. */
const RECEIVABLE_STATES: ReadonlySet<string> = new Set([
  "Submitted",
  "Recovered",
  "Finalized",
]);

/** Does this outgoing row belong under the clicked aging bucket?
 *
 * Mirrors the backend `reports::aggregate_outgoing` receivables-aging
 * gate: a NAV-accepted invoice that is unpaid and is not a storno child,
 * classified by `payment_deadline` into the clicked bucket.
 *
 * A row with NO recorded deadline is a settled legacy import — excluded
 * from the receivables total and from every bucket by
 * `reports::aging_placement` — so it must match NO facet here. Returning
 * `true` for it would put legacy invoices in a drill-down whose tile
 * counts none of them.
 *
 * The "counted" set is approximated by the three revenue-recognized
 * states; storno/amended BASE rows (a rarer edge) are excluded, so the
 * list count can diverge slightly from the dashboard bucket count in
 * those cases — the same best-effort posture S227 documented for the
 * storno-chain row. */
export function outgoingAgingMatches(
  row: InvoiceListItem,
  facet: AgingBucket | null,
  todayIso: string,
): boolean {
  if (facet === null) return true;
  if (row.payment !== null) return false; // paid → not a receivable
  if (row.is_storno) return false; // storno child
  if (!RECEIVABLE_STATES.has(row.state)) return false;
  const bucket = agingBucketFor(todayIso, row.payment_deadline);
  if (bucket === null) return false; // settled legacy — under no bucket
  return bucket === facet;
}

/** Does this incoming row belong under the clicked aging bucket?
 *
 * Mirrors the dashboard's `payables_aging` panel: Outstanding rows only,
 * classified into the same buckets. The deadline-less exclusion is
 * load-bearing on this side — `ap_sync` records no deadline at all for
 * NAV-synced payables, so on a legacy book keeping them would fill this
 * drill-down against tiles that are all zero. */
export function incomingAgingMatches(
  inv: IncomingInvoice,
  facet: AgingBucket | null,
  todayIso: string,
): boolean {
  if (facet === null) return true;
  if (inv.local_status !== "Outstanding") return false;
  if (hasNoRecordedDeadline(inv.payment_deadline)) return false;
  return agingBucketFor(todayIso, inv.payment_deadline) === facet;
}

/** Does this incoming row belong under the `past_deadline` hygiene
 * facet? Mirrors the dashboard's `payable_past_deadline_count`: unpaid
 * (`local_status === "Outstanding"`) AND a recorded deadline strictly
 * before today.
 *
 * Deadline-less rows are excluded through the SAME predicate the aging
 * facet uses. Two independent reasons point the same way: a settled
 * invoice is not late, and a deadline nobody can read is unknown
 * lateness rather than lateness. The shared predicate is what keeps the
 * two facets from drifting apart one edit at a time — a local
 * `=== null` here would silently keep the unparseable half. */
export function incomingPastDeadlineMatches(
  inv: IncomingInvoice,
  todayIso: string,
): boolean {
  if (inv.local_status !== "Outstanding") return false;
  // Both sides go through the classifier before the compare. The compare
  // itself is still lexicographic — sound on two canonical `YYYY-MM-DD`
  // strings — but it is now comparing the classifier's OUTPUT, not the
  // raw column.
  //
  // It used to read `(deadline as string) < todayIso` off the stored
  // value, under a comment claiming the predicate above had made it
  // canonical. It had not: `hasNoRecordedDeadline` trims before it
  // matches, so `" 2026-12-30"` passes as a readable deadline and then
  // sorts before every real date on the leading space — a payable due in
  // December reported as past due in August. Normalising is also what
  // pads a year below 1000 to four digits, so `"0026-06-15"` sorts
  // before today instead of after it.
  const deadline = canonicalDeadlineIso(inv.payment_deadline);
  if (deadline === null) return false; // no recorded deadline
  // `todayIso` is canonical by construction (`todayIsoLocal`), but it
  // arrives here as a plain string parameter; normalising it too means a
  // caller cannot produce a verdict from a today the backend would have
  // rejected. Unreadable today → no row is past deadline, which is the
  // conservative direction.
  const today = canonicalDeadlineIso(todayIso);
  if (today === null) return false;
  return deadline < today;
}
