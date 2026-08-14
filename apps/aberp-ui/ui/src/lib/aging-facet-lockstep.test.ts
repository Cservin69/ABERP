import { describe, expect, it } from "vitest";
// Vite's `?raw` — the component sources as strings. Same posture as
// `statistics-integrity-banner.test.ts`: this package mounts no
// components, so the contract is pinned by reading the source. Honest
// scope — these cannot prove the lists RENDER the right rows; they catch
// the one regression with a plausible motive, named per-test below.
import outgoing from "../routes/InvoiceList.svelte?raw";
import incoming from "../routes/IncomingInvoiceList.svelte?raw";

// ─────────────────────────────────────────────────────────────────────
// An otherwise-outstanding invoice with NO recorded `payment_deadline`
// (missing, or a value that will not parse) is a legacy NAV import taken
// as SETTLED: `reports::aging_placement` drops it from the receivables /
// payables total, from every aging bucket, and from the past-deadline
// hygiene counters together. Each of those tiles is CLICKABLE — it deep
// links into one of these two lists, which re-run the classification
// client-side.
//
// So BOTH facets must exclude those rows, or the operator clicks a tile
// reading 0 and lands on a list of legacy invoices. That is the same
// tile↔list incoherence the previous pins guarded, pointing the other
// way: PR #68 imputed the rows into `d90_plus` and required the aging
// facet to KEEP them, and the two facets deliberately disagreed. Under
// the settled ruling they must agree — and a half-applied revert, where
// one list re-adds the rows and the other does not, is the failure this
// file is aimed at.
//
// The exclusion is expressed through `hasNoRecordedDeadline` in both
// places rather than a local `payment_deadline === null`, because the
// local form silently keeps the UNPARSEABLE half — an exclusion that
// covers only two of the three shapes reads as correct right up until a
// malformed date appears.
// ─────────────────────────────────────────────────────────────────────

/** The shared predicate, in either call shape. */
const SHARED_EXCLUSION = /hasNoRecordedDeadline\(/;

/** A hand-rolled null check standing in for it. Deliberately broad, so
 * the pin is not sidestepped by `== null`, a falsy check, or an early
 * `!deadline` guard — each of which handles `null` and misses `"junk"`. */
const LOCAL_NULL_ONLY_CHECK = /payment_deadline\s*(===?|!==?)\s*null|!\w+\.payment_deadline/;

/** Slice `source` from `startMarker` to the first line that closes at
 * `indent` spaces, so a block's own nested closers do not end it. */
function block(source: string, startMarker: string, indent: number): string {
  const start = source.indexOf(startMarker);
  expect(start, `expected to find \`${startMarker}\``).toBeGreaterThan(-1);
  const closer = `\n${" ".repeat(indent)}}`;
  const end = source.indexOf(closer, start);
  expect(end, `expected \`${startMarker}\` to close`).toBeGreaterThan(start);
  return source.slice(start, end);
}

const outgoingAging = block(
  outgoing,
  "function agingMatches(row: InvoiceListItem): boolean {",
  2,
);
const incomingAging = block(incoming, "if (agingFacet !== null) {", 4);
const incomingHygiene = block(incoming, 'if (hygiene === "past_deadline") {', 4);

describe("aging click-through stays in lockstep with the dashboard panels", () => {
  it("outgoing list classifies through the shared helper, not its own copy", () => {
    // A local re-implementation of the bucket boundaries is the other way
    // these drift; `aging.ts` exists to be the single source.
    expect(outgoingAging).toContain("agingBucketFor(");
  });

  it("outgoing list EXCLUDES rows with no recorded deadline", () => {
    // The revert this pin exists for: dropping the exclusion puts settled
    // legacy invoices back into a bucket drill-down whose tile counts
    // none of them.
    expect(outgoingAging).toMatch(/bucket === null/);
  });

  it("incoming list classifies through the shared helper, not its own copy", () => {
    expect(incomingAging).toContain("agingBucketFor(");
  });

  it("incoming list EXCLUDES rows with no recorded deadline", () => {
    // Load-bearing on this side: `ap_sync` records no deadline at all for
    // NAV-synced payables, so on a legacy book keeping them would fill
    // the payables aging drill-down against tiles that are all zero.
    expect(incomingAging).toMatch(SHARED_EXCLUSION);
  });
});

describe("the past-deadline HYGIENE facet keeps excluding undated rows", () => {
  // Unchanged in effect, and it was already correct — but it must not be
  // "corrected" the other way now that the aging facet agrees with it.
  // `payable_past_deadline_count` is a LATENESS ASSERTION and nothing
  // supports one for an invoice with no deadline; the settled ruling adds
  // a second independent reason (a settled invoice is not late). Both
  // point the same way.
  it("still short-circuits on a row with no recorded deadline", () => {
    expect(incomingHygiene).toMatch(SHARED_EXCLUSION);
  });

  it("still requires a deadline strictly in the past", () => {
    expect(incomingHygiene).toContain("todayIso()");
  });
});

describe("both facets exclude via the SHARED predicate", () => {
  it("neither hand-rolls a null-only check that would miss unparseable dates", () => {
    // The half-fix with the most plausible motive: `=== null` reads as
    // obviously right, passes every test written with `null` in mind, and
    // silently keeps `"30/06/2026"` in a list whose tile excluded it.
    for (const [name, source] of [
      ["outgoing aging", outgoingAging],
      ["incoming aging", incomingAging],
      ["incoming hygiene", incomingHygiene],
    ] as const) {
      expect(LOCAL_NULL_ONLY_CHECK.test(source), `${name} must not hand-roll a null check`).toBe(
        false,
      );
    }
  });

  it("the two incoming facets agree about deadline-less rows", () => {
    // Under PR #68 these two blocks deliberately DISAGREED and a pin held
    // them apart. They now have to match, so a change to one that is not
    // made to the other is caught here rather than by Ervin clicking a
    // tile.
    expect(SHARED_EXCLUSION.test(incomingHygiene)).toBe(true);
    expect(SHARED_EXCLUSION.test(incomingAging)).toBe(true);
  });
});
