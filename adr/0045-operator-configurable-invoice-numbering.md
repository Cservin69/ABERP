# ADR-0045 — Operator-Configurable Invoice Number Template

**Status:** Accepted — PR-89 (2026-05-27)
**Author:** Ervin Áben (ABERP), session brief on operator-configurable numbering
**Supersedes / amends:** ADR-0009 §3 (Invoice series + sequence
reservation — extended, not replaced; the atomic allocator still
guarantees gap-free per `(series, fiscal_year)`); pre-PR-89 hardcoded
`format!("{}/{:05}", series_code, sequence)` at eight emit sites
(replaced by the template renderer).
**Related:** ADR-0022 (NAV XSD runtime validator — the XSD pattern this
PR adopts for Literal segments), ADR-0037 (EUR-invoicing — the
closed-vocab posture this PR mirrors), ADR-0040 (multi-bank-account
schema — same `seller.toml` write-merge pattern this PR follows).

## Context

ABERP's pre-PR-89 invoice number was hardcoded as
`{series_code}/{sequence:05}` at eight emit sites in the binary
(`apps/aberp/src/nav_xml.rs` × 3 renderers, `issue_invoice.rs`,
`issue_storno.rs` × 2, `issue_modification.rs`,
`request_technical_annulment.rs`). Operators got `INV-default/00001`
shape with no affordance to customise.

Ervin's go-live (~2026-06-10) requires his real ABERP-branded sequence
day one, likely `ABERP-2026/000001` with annual reset, possibly
starting at a non-1 value to continue an external Billingo sequence
(`start_value = 1247`). The Hungarian numbering convention is
"`PREFIX-YEAR/NNNNN`" with the counter resetting every fiscal year;
the operator chooses prefix shape + pad width + reset policy + start
value.

Three forces in tension:

1. **Operator freedom** to assemble any shape (literal text, year, pad
   width, reorderable segments) — Ervin's spec named
   `ABERP-2026/000001`, `0001/2026-ABEDIFFERENT`, mid-year start at
   1247, etc.
2. **Hungarian §169 compliance** — continuous gap-free numbering per
   series. The operator can SET a starting value but cannot SKIP/REWIND
   post-issuance.
3. **NAV `invoiceNumber` XSD charset** — `[0-9A-Za-z\-/]{1,50}`. Any
   character outside this set must loud-fail at config time, not at
   NAV submit (where it ABORTS the invoice).

Pre-PR-89 had ResetPolicy::AnnualOnFiscalYear in the schema but
gated the allocator with `BillingError::AnnualResetUnimplemented` —
PR-89 lifts that gate.

## Decision

A **closed-vocab segment template** persisted in
`~/.aberp/<tenant>/seller.toml` under `[seller.numbering]`. The eight
emit sites flow through one renderer
(`numbering::format_invoice_number`); the operator builds the
template via a click-to-assemble UI in Tenant Settings.

### 1. Segment vocabulary (closed)

```rust
enum Segment {
    Literal(String),       // operator-typed text; NAV-charset gated
    Year { digits: YearDigits },  // YearDigits = Two | Four
    Counter { pad_width: u8 },    // pad is FLOOR, not cap (overflow grows)
}
```

Adding a fourth kind (e.g. `Month`) is a deliberate one-line widening
of this enum + a render arm + a validate arm + a parser arm + a UI
chip kind. Deny-default.

### 2. Render semantics

`template.render(year, sequence)` folds the segments in declaration
order:

- `Literal(s)` emits `s` verbatim.
- `Year { Two }` emits `year mod 100` as 2-digit ASCII (`26` for 2026).
- `Year { Four }` emits the full year as 4-digit ASCII (`2026`).
- `Counter { pad_width }` emits `sequence` zero-padded to the
  MINIMUM width. **Pad is a floor.** Overflow grows naturally
  (`pad_width=2`: `01`..`99`..`100`..`101`). This is explicit in
  Ervin's spec and pinned by tests on both sides.

### 3. Reset policy (the decision fork)

`ResetPolicy::Never` runs continuous (the pre-PR-89 `INV-default`
behaviour); `ResetPolicy::OnYearChange` resets the counter to
`start_value` on Jan 1. The atomic allocator's
`invoice_sequence_state` row is keyed by `(series_id, fiscal_year)`
— PR-89 lifts the `AnnualResetUnimplemented` gate and drives
`fiscal_year` from the issue-date year when `OnYearChange` is in
effect. Gap-free within each year is preserved by the existing
allocator.

**Default selected by PR-89:** when the operator chooses a template
with a `Year` segment, the SPA's reset-policy chip defaults to
`OnYearChange` (the common Hungarian convention). When the template
has no Year segment, defaults to `Never`. Flagged in PR-89's handoff
for Ervin's confirmation — this is the load-bearing design fork.

**Validator gate:** `OnYearChange` + no `Year` segment is rejected
(silent duplicate numbers across years). `OnYearChange` requires a
Year segment.

### 4. NAV charset gate

Literal segments are constrained to the NAV `invoiceNumber` XSD
pattern: ASCII alphanumeric + dash + slash (`[0-9A-Za-z\-/]`). The
validator surfaces an `InvalidLiteralCharacter` error with the
offending character + segment index; the SPA builder UI shows an
inline error message. Backslash, dot, underscore, space, `#`, `@` —
all rejected at config time so a NAV submit ABORT is impossible.

Total-length check: the minimum-width render must not exceed the NAV
`invoiceNumber` 50-character maximum.

### 5. Gap-free + start_value invariants

Hungarian §169 forbids gaps in the issued sequence. Setting
`start_value > 1` is a SETUP/MIGRATION action (e.g. "continue from
Billingo at 1247"); after the first invoice burns at `start_value`,
the counter increments by exactly 1 monotonically.

**PR-89 v1 gate (deferred):** the SPA save endpoint accepts any
`start_value`. The "lock after first allocation" gate is named-deferred
to PR-90 (requires a DB lookup at save time). PR-89 mitigates by
documenting the rule loudly in the handoff and surfacing the warning
inline in the builder UI.

### 6. Storage + write merge

Persisted as a `[seller.numbering]` section of
`~/.aberp/<tenant>/seller.toml`:

```toml
[seller.numbering]
segments = [{ kind = "Literal", text = "ABERP-" }, { kind = "Year", digits = 4 }, { kind = "Literal", text = "/" }, { kind = "Counter", pad_width = 6 }]
reset_policy = "on_year_change"
start_value = 1247
```

`numbering::write_numbering_section` is a non-destructive POSIX-atomic
merge: identity sections (`[seller]`, `[seller.address]`),
bank-account block (`[[seller.banks]]`), and any comment prefix are
preserved verbatim. Mirrors PR-72's
`seller_banks::write_seller_banks_section` posture (read-modify-write
only the numbering section). Pinned by the integration test
`tests/serve_numbering_route.rs::put_preserves_identity_and_bank_sections`.

### 7. Migration (existing tenants)

Tenants without `[seller.numbering]` in their seller.toml get the
DEFAULT template on load:

```
Literal("INV-default/") + Counter{pad: 5}, reset_policy: Never, start_value: 1
```

This renders byte-for-byte identical to the pre-PR-89
`format!("{}/{:05}", "INV-default", seq)` shape. Existing invoices
keep their numbers with zero migration churn.

### 8. Renderer surface (eight emit sites → one)

Each NAV XML renderer (`render_invoice_data`, `render_storno_data`,
`render_modification_data`) gains a sibling `*_with_number` variant
that accepts a pre-rendered `Option<&str>` override. The legacy
signatures stay (delegating with `None`) so the existing test corpus
runs unchanged. Production paths (`issue_invoice::run`,
`issue_storno::run`, `issue_modification::run`) compute the rendered
number via `numbering::format_invoice_number(seller_toml_path, year,
sequence)` and pass `Some(rendered)`.

Storno + modification chain references render the BASE invoice's
number against the BASE's issue year (captured in-tx as
`base_issue_year`), so a cross-year storno still emits
`ABERP-2025/000017` even when the storno itself is issued in 2026.

### 9. Out-of-scope (deferred)

- **start_value lock-after-first-allocation gate.** PR-90.
- **invoice_number column on the invoice row.** Today the renderer
  re-renders historical invoices from the current template — a
  template edit after issuance re-renders historical display strings.
  Mitigated by: (a) default template is byte-identical to pre-PR-89;
  (b) documented caveat "set up your template before issuing real
  invoices". The persist-rendered-number-on-issue posture lands in
  PR-90.
- **Technical-annulment template wiring.** `request_technical_
  annulment.rs:408` keeps the legacy `format!("{}/{:05}", series_default,
  seq)` shape — annulment of post-PR-89 invoices that used a custom
  template is named-deferred (the audit-ledger walk would need to
  carry the base's issue year + the template-at-issue-time). Practical
  impact: Ervin issues zero annulments at go-live.
- **Month segment.** Some HU sequences include `YYYY-MM/NNNN`.
  Closed-vocab widening — one-line addition when the first operator
  asks.

## Consequences

**Positive:**
- Operator owns the invoice-number shape via UI; no `cp seller.toml.example`
  + hand-edit.
- One template + one renderer replaces eight hardcoded `format!()` sites.
- NAV-charset gate at config time, not submit time — illegal templates
  cannot reach the wire.
- Annual-reset works for the first time in ABERP (the
  `AnnualOnFiscalYear` ResetPolicy was schema-present but allocator-
  gated pre-PR-89; PR-89 lifts the gate).
- Migration path is non-destructive: pre-PR-89 tenants see zero change;
  Ervin opts into the new shape via the SPA.

**Negative:**
- A template change after invoices exist re-renders historical
  display strings (the on-the-wire XML + the printed PDF were already
  written at issue time; the SPA listing's invoice-number column is
  the affected surface). PR-90 stamps the rendered number on the
  invoice row to fix this forward-proof.
- `start_value` is currently free to edit at any time — the operator
  could set it lower than the current allocator state and produce a
  collision. PR-90 adds the lock-after-first-allocation gate. v1
  mitigates by documenting + UI warning.
- Technical annulment still hardcodes `INV-default` for the base
  invoice number reference. Acceptable because Ervin has no annulments
  scheduled at go-live; if needed, the legacy emit path keeps working.

## Test coverage (the load-bearing pins)

Rust (`apps/aberp/src/numbering.rs::tests` + `tests/serve_numbering_route.rs`):
- Default template renders the pre-PR-89 shape byte-for-byte.
- Pad-as-floor: width-2 counter renders 01..99..100..101 (Ervin's named case).
- Segment order drives render order (Ervin's `0001/2026-ABEDIFFERENT` shape).
- Validator rejects zero counters / multiple counters / empty
  template / empty Literal / backslash + other NAV-illegal characters
  / OnYearChange without Year segment / zero start_value.
- Validator accepts NAV-legal special characters (dash, slash,
  alphanumeric).
- Ervin's primary template (`ABERP-2026/000001` + annual reset)
  renders + validates.
- TOML section round-trips (write → parse → equal).
- Merge preserves identity + bank sections + comment preamble.
- `format_invoice_number` with absent file falls back to default;
  with persisted template renders against it.

SPA (`apps/aberp-ui/ui/src/lib/invoice-numbering.test.ts`):
- 24 pins mirroring the Rust invariants: pad-as-floor, segment order,
  exactly-one-counter, NAV-charset rejection, OnYearChange/Year gate,
  Ervin's primary shape, bilingual error messages, move/remove pure
  helpers.

## Closed-vocab review (CLAUDE.md rule 7)

- `Segment::{Literal, Year, Counter}` — closed.
- `YearDigits::{Two, Four}` — closed.
- `ResetPolicy::{Never, OnYearChange}` — closed.
- `NumberingError` — closed (one variant per validate-time failure).
- `SegmentWire::{Literal, Year, Counter}` — closed (mirrors Rust enum).
- `NumberingResetPolicy` on the SPA — closed (`"never" | "on_year_change"`).

Every widening point is a deliberate enum-extension at one file +
mirror at the SPA side. No fall-through "Other" buckets.
