import { describe, expect, it } from "vitest";
// Vite's `?raw` query — the component source as a string. Same posture as
// `durability-alert-banner.test.ts`: this package mounts no components, so
// the shell's contract is pinned by reading its source. Honest scope — these
// cannot prove the banner RENDERS or is legible; they catch the regressions
// with a plausible motive, listed per-test below.
import page from "../routes/StatisticsPage.svelte?raw";
import api from "./api.ts?raw";

// The backend's audit-ledger walk used to swallow any entry whose payload it
// could not decode, so a malformed payment / ack / storno silently made the
// Financial dashboard's figures wrong. It now counts them onto
// `FinancialReport.ledger_diagnostics`. These pins hold the SPA's half of
// that contract: the count must reach the operator, above the numbers it
// invalidates, and must not be silenceable.

/** The integrity banner block only — its `{#if}` guard through the closing
 * `</div>`. Sliced on the `</div>` rather than the first `{/if}`, which
 * belongs to the nested "and N more" arm. */
const banner = (() => {
  const start = page.indexOf("{#if r.ledger_diagnostics.unparseable_entries > 0}");
  expect(start, "the integrity banner's {#if} guard must exist").toBeGreaterThan(-1);
  const end = page.indexOf("</div>", start);
  expect(end, "the integrity banner must close its element").toBeGreaterThan(start);
  return page.slice(start, end);
})();

describe("StatisticsPage ledger-integrity banner", () => {
  it("is announced as an alert", () => {
    // Drop `role="alert"` and a screen-reader operator is told nothing at all
    // — the figures just quietly stay wrong, which is the original defect
    // wearing a different hat.
    expect(banner).toContain('role="alert"');
  });

  it("fires ONLY on a non-zero count, so a healthy ledger shows nothing", () => {
    // The inverse failure mode: a banner that cries wolf on every report
    // teaches the operator to ignore it, and then the one real occurrence is
    // invisible too.
    expect(page).toContain("{#if r.ledger_diagnostics.unparseable_entries > 0}");
  });

  it("says the figures may be INCOMPLETE, not merely that something failed", () => {
    // "N records could not be read" alone reads as a cosmetic glitch. The
    // operator needs to know it invalidates the numbers on screen.
    expect(banner).toMatch(/could not be read/i);
    expect(banner).toMatch(/incomplete/i);
  });

  it("names the offending audit entries so they can be found", () => {
    // A bare count is not actionable; the ids are the operator's (and
    // Ervin's) starting point in the ledger.
    expect(banner).toContain("r.ledger_diagnostics.unparseable_entry_ids");
  });

  it("discloses the backend id cap instead of implying the list is complete", () => {
    // The backend caps the id list at 50 but keeps the count exact. If the
    // SPA rendered only the ids, a 500-entry corruption would read as 50.
    expect(banner).toContain(
      "r.ledger_diagnostics.unparseable_entries > r.ledger_diagnostics.unparseable_entry_ids.length",
    );
  });

  it("has NO dismiss control", () => {
    // Nothing here may let the operator click away a data-integrity alarm;
    // it goes down when the backend stops reporting it.
    expect(banner).not.toMatch(/<button/i);
    expect(banner).not.toMatch(/dismiss|onclick/i);
  });

  it("sits ABOVE the figures, not in the collapsed deferred-notes disclosure", () => {
    // `deferred_notes` is a `<details>` labelled "Deferred to a later
    // release" — the wrong home for "today's numbers may be wrong". Folding
    // this in there would technically surface it and practically bury it.
    const bannerAt = page.indexOf("{#if r.ledger_diagnostics.unparseable_entries > 0}");
    const cardsAt = page.indexOf('<section class="stats__cards"');
    const deferredAt = page.indexOf('<details class="stats__deferred">');
    expect(cardsAt).toBeGreaterThan(-1);
    expect(deferredAt).toBeGreaterThan(-1);
    expect(bannerAt).toBeLessThan(cardsAt);
    expect(bannerAt).toBeLessThan(deferredAt);
  });
});

// The aging panels had the SAME silent drop on a different code path: an
// outstanding invoice whose `payment_deadline` was missing or unreadable
// fell out of every bucket while still counting toward the receivables /
// payables total, so the buckets summed to less than the headline above
// them. The backend now ages it as 90+ and reports
// `aging_undated_invoices`. The bucket is therefore an IMPUTATION, and the
// operator has to be told — otherwise the fix just launders a guess into
// a number that looks measured.

/** The undated-aging disclosure block only — same slicing rule as above. */
const undated = (() => {
  const start = page.indexOf("{#if r.ledger_diagnostics.aging_undated_invoices > 0}");
  expect(start, "the undated-aging disclosure's {#if} guard must exist").toBeGreaterThan(-1);
  const end = page.indexOf("</div>", start);
  expect(end, "the undated-aging disclosure must close its element").toBeGreaterThan(start);
  return page.slice(start, end);
})();

describe("StatisticsPage undated-aging disclosure", () => {
  it("says the invoices ARE counted — only their age is a guess", () => {
    // The failure mode to avoid in BOTH directions: reading as "figures
    // are missing" (they are not — that is what the fix restored), or
    // saying nothing and letting an imputed 90+ read as measured.
    expect(undated).toMatch(/counted in full/i);
    expect(undated).toMatch(/90\+/);
  });

  it("names the invoices, and discloses the id cap", () => {
    expect(undated).toContain("r.ledger_diagnostics.aging_undated_invoice_ids");
    expect(undated).toContain(
      "r.ledger_diagnostics.aging_undated_invoices > r.ledger_diagnostics.aging_undated_invoice_ids.length",
    );
  });

  it("fires ONLY on a non-zero count, and cannot be dismissed", () => {
    expect(page).toContain("{#if r.ledger_diagnostics.aging_undated_invoices > 0}");
    expect(undated).not.toMatch(/<button/i);
    expect(undated).not.toMatch(/dismiss|onclick/i);
  });

  it("sits ABOVE the figures it qualifies", () => {
    const at = page.indexOf("{#if r.ledger_diagnostics.aging_undated_invoices > 0}");
    const cardsAt = page.indexOf('<section class="stats__cards"');
    const agingAt = page.indexOf('<section class="stats__aging-grid"');
    expect(agingAt).toBeGreaterThan(-1);
    expect(at).toBeLessThan(cardsAt);
    expect(at).toBeLessThan(agingAt);
  });
});

describe("FinancialReport wire shape", () => {
  it("carries ledger_diagnostics as a required field", () => {
    // Optional (`?:`) would let the banner's guard silently short-circuit on
    // every report if the backend field were ever dropped — failing back to
    // exactly the silence this fix removed.
    expect(api).toMatch(/ledger_diagnostics:\s*LedgerDiagnostics;/);
    expect(api).toMatch(/unparseable_entries:\s*number;/);
    expect(api).toMatch(/unparseable_entry_ids:\s*string\[\];/);
  });

  it("carries the undated-aging counters as required fields too", () => {
    expect(api).toMatch(/aging_undated_invoices:\s*number;/);
    expect(api).toMatch(/aging_undated_invoice_ids:\s*string\[\];/);
  });
});
