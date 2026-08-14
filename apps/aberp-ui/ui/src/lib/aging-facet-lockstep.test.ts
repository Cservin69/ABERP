import { describe, expect, it } from "vitest";
import {
  incomingAgingMatches,
  incomingPastDeadlineMatches,
  outgoingAgingMatches,
} from "./aging-facets";
import { AGING_BUCKETS, type AgingBucket } from "./aging";
import type { IncomingInvoice, InvoiceListItem } from "./api";
// Vite's `?raw` — the component sources as strings. Used ONLY for the
// delegation checks at the bottom, which are the one thing a behaviour
// test cannot see: that the components actually call these predicates
// rather than keeping a private copy.
import outgoing from "../routes/InvoiceList.svelte?raw";
import incoming from "../routes/IncomingInvoiceList.svelte?raw";

// ─────────────────────────────────────────────────────────────────────
// An otherwise-outstanding invoice with NO recorded `payment_deadline`
// (missing, or a value that will not parse) is a legacy import taken as
// SETTLED: `reports::aging_placement` drops it from the receivables /
// payables total, from every aging bucket, and from the past-deadline
// hygiene counters together. Each of those tiles is CLICKABLE — it deep
// links into one of these two lists, which re-run the classification
// client-side. So BOTH facets must exclude those rows, or the operator
// clicks a tile reading 0 and lands on a list of legacy invoices.
//
// THESE PINS ASSERT BEHAVIOUR, NOT SOURCE TEXT. The previous cut checked
// the component sources with `?raw` + regex, which caught a deleted
// exclusion but NOT a flipped verdict: turning
//
//     if (bucket === null) return false;   →   return true;
//
// keeps every grepped token, inverts the meaning into "matches every
// bucket", and left the whole suite green. That is why the rules moved
// into `aging-facets.ts` — so the tests can call them and check which
// rows come back.
// ─────────────────────────────────────────────────────────────────────

const TODAY = "2026-06-30";

/** Every shape of "no recorded deadline" the backend excludes:
 * missing, empty, unparseable, and — since the two classifiers were
 * unified — an impossible calendar date that JS would otherwise roll
 * over into a real one. */
const UNDATED: ReadonlyArray<string | null> = [
  null,
  "",
  "not-a-date",
  "30/06/2026",
  "2026-13-45",
  "2026-02-30",
];

function arRow(payment_deadline: string | null): InvoiceListItem {
  // Only the fields the predicate reads are meaningful; the rest are
  // filled to satisfy the wire type. Cast is confined to this builder.
  return {
    invoice_id: "inv_1",
    state: "Submitted",
    is_storno: false,
    payment: null,
    payment_deadline,
  } as unknown as InvoiceListItem;
}

function apRow(
  payment_deadline: string | null,
  local_status = "Outstanding",
): IncomingInvoice {
  return {
    id: "ap_1",
    payment_deadline,
    local_status,
  } as unknown as IncomingInvoice;
}

describe("outgoing aging facet — deadline-less rows are excluded from EVERY bucket", () => {
  for (const deadline of UNDATED) {
    it(`${JSON.stringify(deadline)} matches no bucket`, () => {
      // The verdict-flip mutation (`return true` where the exclusion
      // belongs) reds here: the row would match all five facets.
      const matched = AGING_BUCKETS.filter((b) =>
        outgoingAgingMatches(arRow(deadline), b, TODAY),
      );
      expect(matched, "a settled legacy invoice belongs under no aging bucket").toEqual([]);
    });
  }

  it("a dated receivable still lands in exactly ONE bucket", () => {
    // The other direction: an exclusion that widened to swallow healthy
    // rows would empty the drill-down while every tile still showed
    // counts. Exactly one, so a `return true` flip fails here too.
    const matched = AGING_BUCKETS.filter((b) =>
      outgoingAgingMatches(arRow("2026-05-31"), b, TODAY),
    );
    expect(matched).toEqual<AgingBucket[]>(["d1_30"]);
  });

  it("a dated but PAID receivable is out regardless of bucket", () => {
    const paid = { ...arRow("2026-05-31"), payment: {} } as unknown as InvoiceListItem;
    expect(AGING_BUCKETS.filter((b) => outgoingAgingMatches(paid, b, TODAY))).toEqual([]);
  });

  it("no facet clicked means no aging filtering at all", () => {
    // The undated row must still be listable when the operator is just
    // browsing — the exclusion is about the DRILL-DOWN, not the list.
    expect(outgoingAgingMatches(arRow(null), null, TODAY)).toBe(true);
  });
});

describe("incoming aging facet — deadline-less rows are excluded from EVERY bucket", () => {
  for (const deadline of UNDATED) {
    it(`${JSON.stringify(deadline)} matches no bucket`, () => {
      // Load-bearing on this side: `ap_sync` records no deadline for
      // NAV-synced payables, so on a legacy book this is most of the
      // book. A flipped verdict would fill this drill-down against
      // tiles that are all zero.
      const matched = AGING_BUCKETS.filter((b) =>
        incomingAgingMatches(apRow(deadline), b, TODAY),
      );
      expect(matched).toEqual([]);
    });
  }

  it("a dated outstanding payable still lands in exactly ONE bucket", () => {
    const matched = AGING_BUCKETS.filter((b) =>
      incomingAgingMatches(apRow("2026-04-15"), b, TODAY),
    );
    expect(matched).toEqual<AgingBucket[]>(["d61_90"]);
  });

  it("a non-Outstanding row is out regardless of bucket", () => {
    const settled = apRow("2026-04-15", "Paid");
    expect(AGING_BUCKETS.filter((b) => incomingAgingMatches(settled, b, TODAY))).toEqual([]);
  });

  it("no facet clicked means no aging filtering at all", () => {
    expect(incomingAgingMatches(apRow(null), null, TODAY)).toBe(true);
  });
});

describe("the past-deadline HYGIENE facet keeps excluding undated rows", () => {
  // Unchanged in effect, and it was already correct — but it must not be
  // "corrected" the other way now that the aging facet agrees with it.
  // Two independent reasons point the same way: a settled invoice is not
  // late, and an unreadable deadline is unknown lateness.
  for (const deadline of UNDATED) {
    it(`${JSON.stringify(deadline)} is not past deadline`, () => {
      expect(incomingPastDeadlineMatches(apRow(deadline), TODAY)).toBe(false);
    });
  }

  it("a deadline strictly before today IS past deadline", () => {
    expect(incomingPastDeadlineMatches(apRow("2026-06-29"), TODAY)).toBe(true);
  });

  it("a deadline of today or later is NOT past deadline", () => {
    expect(incomingPastDeadlineMatches(apRow(TODAY), TODAY)).toBe(false);
    expect(incomingPastDeadlineMatches(apRow("2026-07-01"), TODAY)).toBe(false);
  });

  it("a non-Outstanding row is never past deadline", () => {
    expect(incomingPastDeadlineMatches(apRow("2026-06-29", "Paid"), TODAY)).toBe(false);
  });
});

describe("the two incoming facets agree about which rows are deadline-less", () => {
  // Under PR #68 these deliberately DISAGREED and a pin held them apart.
  // They now have to match, so a change to one that is not made to the
  // other is caught here rather than by Ervin clicking a tile.
  it("every undated row is excluded by BOTH", () => {
    for (const deadline of UNDATED) {
      const inAnyBucket = AGING_BUCKETS.some((b) =>
        incomingAgingMatches(apRow(deadline), b, TODAY),
      );
      expect(inAnyBucket, `aging facet / ${deadline}`).toBe(false);
      expect(incomingPastDeadlineMatches(apRow(deadline), TODAY), `hygiene / ${deadline}`).toBe(
        false,
      );
    }
  });
});

describe("the components delegate to these predicates", () => {
  // The one contract a behaviour test cannot observe from here: this
  // package mounts no components, so nothing else would notice a
  // component quietly reinstating a private copy of the rule and
  // drifting from the module the pins above exercise.
  it("outgoing list calls the shared outgoing predicate", () => {
    expect(outgoing).toContain("outgoingAgingMatches(");
  });

  it("incoming list calls both shared incoming predicates", () => {
    expect(incoming).toContain("incomingAgingMatches(");
    expect(incoming).toContain("incomingPastDeadlineMatches(");
  });

  it("neither component classifies deadlines on its own any more", () => {
    // A local `agingBucketFor` call or a hand-rolled `payment_deadline
    // === null` in a component is the drift back to two sources of
    // truth — and the `=== null` form is the one that silently keeps the
    // unparseable half.
    for (const [name, source] of [
      ["InvoiceList", outgoing],
      ["IncomingInvoiceList", incoming],
    ] as const) {
      expect(source, `${name} must not classify deadlines itself`).not.toMatch(
        /agingBucketFor\(|payment_deadline\s*(===?|!==?)\s*null/,
      );
    }
  });
});
