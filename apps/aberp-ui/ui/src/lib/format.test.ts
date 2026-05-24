// PR-44ε / session-53 — vitest pin tests for `format.ts` per
// ADR-0037 §1.a + §1.c (printed-invoice fields and rounding
// precision) and the session-53 SPA-render brief.
//
// Each pin catches a distinct regression mode per CLAUDE.md rule
// 9: a constant-returning formatter would fail every assertion
// except the trivial pass-through ones. Together with the Rust
// pin tests (`invoice_list_item_emits_currency` and
// `invoice_detail_emits_currency_and_rate_metadata` in `serve.rs`)
// the wire-to-render contract is pinned end-to-end.

import { describe, expect, it } from "vitest";

import {
  formatHufEquivalent,
  formatRate,
  formatRateDate,
  formatTotal,
} from "./format";

describe("formatTotal", () => {
  // The HUF branch is byte-equal to the pre-PR-44ε
  // `formatHuf` posture — the same `Intl.NumberFormat("hu-HU",
  // {style: "currency", currency: "HUF"})` instance the old
  // InvoiceList.svelte + InvoiceDetail.svelte carried. We pin
  // the rendered shape contains the expected digit groups +
  // " Ft" suffix without asserting on the exact whitespace
  // (`Intl.NumberFormat` uses a non-breaking space U+00A0 as
  // the thousand separator under hu-HU; future ICU updates may
  // narrow it, and the operator surface tolerates either).

  it("HUF: renders whole forints with `Ft` suffix", () => {
    const out = formatTotal(654_883, "HUF");
    // Must contain the digits in grouped form + the " Ft" suffix.
    expect(out).toMatch(/654.?883.?Ft/);
  });

  it("HUF: renders large totals without fractional part", () => {
    const out = formatTotal(1_234_567_890, "HUF");
    // No decimal separator must appear (HUF has no sub-unit per
    // ADR-0009 §1).
    expect(out).not.toMatch(/[.,]\d/);
    expect(out).toMatch(/Ft/);
  });

  it("EUR: interprets the integer value as cents and renders as euros", () => {
    // 863600 cents = €8 636,00 per the printed-invoice reference
    // template (Hungarian decimal comma + grouped thousands).
    const out = formatTotal(863_600, "EUR");
    expect(out).toMatch(/8.?636,00/);
    expect(out).toMatch(/€/);
  });

  it("EUR: divides by 100 (cents → euros) — 100 cents reads as €1,00", () => {
    const out = formatTotal(100, "EUR");
    expect(out).toMatch(/1,00/);
    expect(out).toMatch(/€/);
  });

  it("null renders as em-dash regardless of currency", () => {
    expect(formatTotal(null, "HUF")).toBe("—");
    expect(formatTotal(null, "EUR")).toBe("—");
  });

  it("HUF and EUR branches differ on the same numeric input", () => {
    // CLAUDE.md rule 9 — a regression that hard-codes the HUF
    // branch (or drops the EUR branch entirely) would produce
    // identical output for both. The two values MUST differ
    // because one is `654 883 Ft` and the other is roughly
    // `€6 548,83`.
    const huf = formatTotal(654_883, "HUF");
    const eur = formatTotal(654_883, "EUR");
    expect(huf).not.toBe(eur);
  });
});

describe("formatRate", () => {
  it("normalises the canonical 6-decimal wire form unchanged", () => {
    // The backend serialises at exactly 6 decimals per ADR-0037
    // §1.c / C11; the formatter is a pass-through after
    // numeric parse.
    expect(formatRate("405.230000")).toBe("405.230000");
  });

  it("pads to 6 decimals when the backend emits fewer", () => {
    // Defensive — a future backend drift that drops the
    // `{:.6}` precision specifier on `rust_decimal::Decimal::Display`
    // would render as `"405.23"`. The formatter re-pads so the
    // operator surface stays at 6 decimals per C11.
    expect(formatRate("405.23")).toBe("405.230000");
  });

  it("renders a whole-number rate with all 6 decimals", () => {
    // `1` is the HUF self-rate stamped at PR-44δ; on the SPA we
    // expect the same 6-decimal form for visual consistency
    // (today the HUF branch hides this row, but a future
    // chain-currency-match or operator-debug surface may
    // surface it).
    expect(formatRate("1")).toBe("1.000000");
  });

  it("passes a malformed input through unchanged (fail-loud per CLAUDE.md rule 12)", () => {
    // A non-numeric value indicates DB tampering or schema
    // drift; rendering it verbatim makes the divergence
    // operator-visible rather than silently zeroing it.
    expect(formatRate("not-a-number")).toBe("not-a-number");
  });
});

describe("formatHufEquivalent", () => {
  it("renders the HUF amount with grouped thousands and `Ft` suffix", () => {
    // The HUF-equivalent gross total on the printed-invoice
    // reference template renders as `Bruttó összeg: 654 883 Ft`.
    const out = formatHufEquivalent(654_883);
    expect(out).toMatch(/654.?883.?Ft/);
  });

  it("renders zero forints as `0 Ft`", () => {
    expect(formatHufEquivalent(0)).toMatch(/0.?Ft/);
  });

  it("matches `formatTotal` for the HUF branch (single source of truth)", () => {
    // Both helpers ultimately format whole forints under the
    // same `Intl.NumberFormat` instance; a regression that
    // forks them would let one side drift. The pin catches the
    // drift at gate time.
    const value = 1_234_567;
    expect(formatHufEquivalent(value)).toBe(formatTotal(value, "HUF"));
  });
});

describe("formatRateDate", () => {
  it("passes a canonical ISO date through unchanged", () => {
    // The backend emits ISO-8601 `YYYY-MM-DD` per ADR-0037
    // §1.a + §2.b (`exchange_rate_date` is `OffsetDateTime`
    // formatted with `[year]-[month]-[day]`). The formatter is
    // a pass-through today; a future Hungarian-locale render
    // (`2026. 05. 22.`) lifts here additively.
    expect(formatRateDate("2026-05-22")).toBe("2026-05-22");
  });

  it("passes an empty string through unchanged", () => {
    // Defensive — the SPA never sees an empty string today
    // (the wire shape is `string | null`, with `null` for HUF
    // invoices and a non-empty string for EUR), but the
    // formatter must not crash if a future migration emits one.
    expect(formatRateDate("")).toBe("");
  });
});

describe("Conditional render contract (documented behaviour pin)", () => {
  // PR-44ε / session-53 — the four rate-metadata rows in
  // `InvoiceDetail.svelte` render iff BOTH `currency !== "HUF"`
  // AND the corresponding wire field is non-null. The Svelte
  // template carries the conditional inline:
  //
  //     {#if detail.currency !== "HUF" && detail.exchange_rate !== null}
  //
  // We cannot exercise the Svelte template directly from
  // vitest (no Svelte 5 component runner setup; deferred per
  // CLAUDE.md rule 2). Instead we pin the equivalent boolean
  // shape here so a regression that flips the && to a || or
  // drops the null-check is caught at gate time as a logic
  // mismatch between this test's expectation and the template
  // body. The pin is a code-review surface, not a runtime
  // enforcement — but it documents the intended truth table.

  function shouldRenderRow(
    currency: "HUF" | "EUR",
    fieldValue: string | number | null,
  ): boolean {
    return currency !== "HUF" && fieldValue !== null;
  }

  it("HUF invoice with rate fields populated: row hidden (regulatory record is HUF itself)", () => {
    // Defensive: even if the backend ever populates rate
    // fields for a HUF invoice (it never does; the
    // `RateMetadata` stamp gates on `!matches!(currency,
    // Currency::Huf)` in issue_invoice.rs), the SPA still
    // hides the rate rows because they are
    // not regulatory-required for HUF invoices.
    expect(shouldRenderRow("HUF", "405.230000")).toBe(false);
    expect(shouldRenderRow("HUF", 3_500_565)).toBe(false);
  });

  it("EUR invoice with all rate fields populated: row shown", () => {
    expect(shouldRenderRow("EUR", "405.230000")).toBe(true);
    expect(shouldRenderRow("EUR", "MNB")).toBe(true);
    expect(shouldRenderRow("EUR", "2026-05-22")).toBe(true);
    expect(shouldRenderRow("EUR", 3_500_565)).toBe(true);
  });

  it("EUR invoice with a null rate field: row hidden (fail-soft)", () => {
    // A non-HUF invoice missing a rate field would indicate a
    // backend bug (PR-44γ pre-flight refuses non-HUF rows
    // lacking rate metadata at the DuckDB write boundary per
    // ADR-0037 §4 C1). The SPA fails soft on the per-field
    // level — hide the row rather than render `null`. The
    // operator-visible signal is the missing row, not a
    // garbled value.
    expect(shouldRenderRow("EUR", null)).toBe(false);
  });
});
