import { describe, expect, it } from "vitest";
import {
  EMPTY_PRICING_FILTER,
  PRICING_PENDING_STATES,
  compareJobs,
  filterJobs,
  isPricingFilterEmpty,
  sortJobs,
  type PricingJobSortRow,
} from "./pricing-jobs-list";

// S411 — minimal row builder. Defaults are a benign `posted` row; each
// test overrides only the fields it pins so the assertion's intent is
// visible (CLAUDE.md rule 9 — a test that can't fail when the logic
// changes is wrong).
function row(over: Partial<PricingJobSortRow> = {}): PricingJobSortRow {
  return {
    quote_id: "qpj_01",
    state: "posted",
    updated_at: "2026-06-14T10:00:00Z",
    customer_name: "Nagy Béla",
    customer_company: "Acme Kft.",
    material_grade: "S235JR",
    total_price_eur: 100,
    ...over,
  };
}

function ids(rows: PricingJobSortRow[]): string[] {
  return rows.map((r) => r.quote_id);
}

describe("sortJobs — updated_at", () => {
  const a = row({ quote_id: "a", updated_at: "2026-06-14T08:00:00Z" });
  const b = row({ quote_id: "b", updated_at: "2026-06-14T12:00:00Z" });
  const c = row({ quote_id: "c", updated_at: "2026-06-14T10:00:00Z" });

  it("ascending = oldest first", () => {
    expect(ids(sortJobs([a, b, c], "updated_at", "asc"))).toEqual([
      "a",
      "c",
      "b",
    ]);
  });
  it("descending = newest first (the operator default)", () => {
    expect(ids(sortJobs([a, b, c], "updated_at", "desc"))).toEqual([
      "b",
      "c",
      "a",
    ]);
  });
});

describe("sortJobs — customer (company-or-name, HU collation)", () => {
  // Á must sort between A and B under Hungarian collation, not after Z.
  const a = row({ quote_id: "a", customer_company: "Béta Kft." });
  const b = row({ quote_id: "b", customer_company: "Acme Kft." });
  const c = row({ quote_id: "c", customer_company: "Árpád Bt." });

  it("ascending uses Hungarian collation order (A < Á < B)", () => {
    expect(ids(sortJobs([a, b, c], "customer", "asc"))).toEqual([
      "b",
      "c",
      "a",
    ]);
  });
  it("descending reverses it", () => {
    expect(ids(sortJobs([a, b, c], "customer", "desc"))).toEqual([
      "a",
      "c",
      "b",
    ]);
  });
  it("falls back to contact name when company is blank/null", () => {
    const noCo = row({
      quote_id: "x",
      customer_company: "  ",
      customer_name: "Zwillinger",
    });
    const withCo = row({ quote_id: "y", customer_company: "Acme" });
    // "Acme" < "Zwillinger" → withCo first ascending.
    expect(ids(sortJobs([noCo, withCo], "customer", "asc"))).toEqual([
      "y",
      "x",
    ]);
  });
});

describe("sortJobs — state (pipeline-natural order)", () => {
  const fetched = row({ quote_id: "f", state: "fetched" });
  const posted = row({ quote_id: "p", state: "posted" });
  const failed = row({ quote_id: "x", state: "failed" });
  const pricing = row({ quote_id: "r", state: "pricing" });

  it("ascending = pipeline progression, failed last", () => {
    expect(
      ids(sortJobs([failed, posted, fetched, pricing], "state", "asc")),
    ).toEqual(["f", "r", "p", "x"]);
  });
  it("descending reverses it", () => {
    expect(
      ids(sortJobs([fetched, posted, failed, pricing], "state", "desc")),
    ).toEqual(["x", "p", "r", "f"]);
  });
});

describe("sortJobs — price (nulls last, both directions)", () => {
  const cheap = row({ quote_id: "c", total_price_eur: 10 });
  const dear = row({ quote_id: "d", total_price_eur: 9000 });
  const none = row({ quote_id: "n", total_price_eur: null });

  it("ascending = cheapest first, null sinks to the bottom", () => {
    expect(ids(sortJobs([none, dear, cheap], "price", "asc"))).toEqual([
      "c",
      "d",
      "n",
    ]);
  });
  it("descending = dearest first, null STILL sinks (dir-invariant)", () => {
    expect(ids(sortJobs([none, cheap, dear], "price", "desc"))).toEqual([
      "d",
      "c",
      "n",
    ]);
  });
});

describe("sortJobs — stability / tiebreak", () => {
  it("ties resolve to quote_id ascending regardless of dir", () => {
    const a = row({ quote_id: "aaa", total_price_eur: 50 });
    const b = row({ quote_id: "bbb", total_price_eur: 50 });
    expect(ids(sortJobs([b, a], "price", "asc"))).toEqual(["aaa", "bbb"]);
    expect(ids(sortJobs([b, a], "price", "desc"))).toEqual(["aaa", "bbb"]);
  });
  it("does not mutate the input array", () => {
    const input = [row({ quote_id: "z" }), row({ quote_id: "a" })];
    sortJobs(input, "updated_at", "asc");
    expect(ids(input)).toEqual(["z", "a"]);
  });
  it("compareJobs is the pure primitive sortJobs is built on", () => {
    const a = row({ quote_id: "a", updated_at: "2026-01-01T00:00:00Z" });
    const b = row({ quote_id: "b", updated_at: "2026-02-01T00:00:00Z" });
    expect(compareJobs(a, b, "updated_at", "asc")).toBeLessThan(0);
    expect(compareJobs(a, b, "updated_at", "desc")).toBeGreaterThan(0);
  });
});

describe("filterJobs — state facet", () => {
  const fetched = row({ quote_id: "fe", state: "fetched" });
  const extracting = row({ quote_id: "ex", state: "extracting" });
  const posted = row({ quote_id: "po", state: "posted" });
  const failed = row({ quote_id: "fa", state: "failed" });
  const all = [fetched, extracting, posted, failed];

  it("All passes every row", () => {
    expect(ids(filterJobs(all, { state: "All" }))).toEqual([
      "fe",
      "ex",
      "po",
      "fa",
    ]);
  });
  it("pending folds the in-flight states", () => {
    expect(ids(filterJobs(all, { state: "pending" }))).toEqual(["fe", "ex"]);
  });
  it("posted matches exactly", () => {
    expect(ids(filterJobs(all, { state: "posted" }))).toEqual(["po"]);
  });
  it("failed matches exactly — the operator's attention bucket", () => {
    expect(ids(filterJobs(all, { state: "failed" }))).toEqual(["fa"]);
  });
  it("the pending bucket membership matches the documented in-flight set", () => {
    const rows = PRICING_PENDING_STATES.map((s, i) =>
      row({ quote_id: `s${i}`, state: s }),
    );
    expect(filterJobs(rows, { state: "pending" })).toHaveLength(
      PRICING_PENDING_STATES.length,
    );
  });
});

describe("filterJobs — search (Ref / name / company / material)", () => {
  const r = row({
    quote_id: "qpj_ZEBRA",
    customer_name: "Kovács Anna",
    customer_company: "Globex Zrt.",
    material_grade: "AlMg3",
  });
  const other = row({
    quote_id: "qpj_other",
    customer_name: "X",
    customer_company: "Y",
    material_grade: "Z",
  });

  it("matches on Ref (quote_id), case-insensitive", () => {
    expect(ids(filterJobs([r, other], { search: "zebra" }))).toEqual([
      "qpj_ZEBRA",
    ]);
  });
  it("matches on customer name", () => {
    expect(ids(filterJobs([r, other], { search: "kovács" }))).toEqual([
      "qpj_ZEBRA",
    ]);
  });
  it("matches on customer company", () => {
    expect(ids(filterJobs([r, other], { search: "globex" }))).toEqual([
      "qpj_ZEBRA",
    ]);
  });
  it("matches on material grade", () => {
    expect(ids(filterJobs([r, other], { search: "almg" }))).toEqual([
      "qpj_ZEBRA",
    ]);
  });
  it("blank needle is an open gate", () => {
    expect(filterJobs([r, other], { search: "   " })).toHaveLength(2);
  });
  it("tolerates a null company haystack", () => {
    const nullCo = row({
      quote_id: "nc",
      customer_company: null,
      customer_name: "Solo",
    });
    expect(ids(filterJobs([nullCo], { search: "solo" }))).toEqual(["nc"]);
  });
});

describe("filterJobs — combined facet AND search", () => {
  const failedAcme = row({
    quote_id: "fa",
    state: "failed",
    customer_company: "Acme",
  });
  const failedGlobex = row({
    quote_id: "fg",
    state: "failed",
    customer_company: "Globex",
  });
  const postedAcme = row({
    quote_id: "pa",
    state: "posted",
    customer_company: "Acme",
  });

  it("both gates must accept the row", () => {
    expect(
      ids(
        filterJobs([failedAcme, failedGlobex, postedAcme], {
          state: "failed",
          search: "acme",
        }),
      ),
    ).toEqual(["fa"]);
  });
});

describe("isPricingFilterEmpty + EMPTY_PRICING_FILTER", () => {
  it("the empty filter is empty", () => {
    expect(isPricingFilterEmpty(EMPTY_PRICING_FILTER)).toBe(true);
  });
  it("an engaged state facet is not empty", () => {
    expect(isPricingFilterEmpty({ search: "", state: "failed" })).toBe(false);
  });
  it("a non-blank search is not empty", () => {
    expect(isPricingFilterEmpty({ search: "x", state: "All" })).toBe(false);
  });
  it("a whitespace-only search still reads as empty", () => {
    expect(isPricingFilterEmpty({ search: "   ", state: "All" })).toBe(true);
  });
});

// S411 — e2e-intent gate ([[customer-journey-e2e-gate]]). No browser
// e2e harness exists in this SPA (no playwright / no .spec.ts — see the
// S371 "not yet built" note), so the operator journey is exercised here
// at the pure-pipeline layer: the EXACT filter→sort composition the
// component's `visibleRows` derivation runs. "Operator opens the panel,
// types a needle, filters to Failed, sorts by newest" → only the
// matching failed rows survive, newest first.
describe("operator journey — filter to Failed then sort newest", () => {
  const dump: PricingJobSortRow[] = [
    row({
      quote_id: "p1",
      state: "posted",
      customer_company: "Acme",
      updated_at: "2026-06-14T09:00:00Z",
    }),
    row({
      quote_id: "f1",
      state: "failed",
      customer_company: "Acme",
      updated_at: "2026-06-14T08:00:00Z",
    }),
    row({
      quote_id: "f2",
      state: "failed",
      customer_company: "Acme",
      updated_at: "2026-06-14T11:00:00Z",
    }),
    row({
      quote_id: "f3",
      state: "failed",
      customer_company: "Globex",
      updated_at: "2026-06-14T12:00:00Z",
    }),
    row({
      quote_id: "w1",
      state: "pricing",
      customer_company: "Acme",
      updated_at: "2026-06-14T10:00:00Z",
    }),
  ];

  it("collapses a 5-row dump to the two failed Acme rows, newest first", () => {
    const visible = sortJobs(
      filterJobs(dump, { state: "failed", search: "acme" }),
      "updated_at",
      "desc",
    );
    expect(ids(visible)).toEqual(["f2", "f1"]);
  });

  it("clearing the filter restores the full list", () => {
    const visible = sortJobs(
      filterJobs(dump, EMPTY_PRICING_FILTER),
      "updated_at",
      "desc",
    );
    expect(visible).toHaveLength(dump.length);
  });
});
