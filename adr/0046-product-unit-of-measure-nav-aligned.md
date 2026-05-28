# ADR-0046 — Product Unit of Measure: NAV-Aligned Closed Vocab with OWN Escape Hatch

**Status:** Accepted — PR-91 (2026-05-27)
**Author:** Ervin Áben (ABERP), session brief on products master-data
**Supersedes / amends:** None (new entity). Mirrors the closed-vocab
posture of ADR-0037 §3 (`Currency`) and the field-level validation
shape of PR-48α (`PartnerKind`).
**Related:** ADR-0002 (per-tenant DuckDB — products table lives there),
ADR-0005 (prefixed-ULID newtypes — `ProductId = prd_<26-char-ULID>`),
ADR-0008 (audit ledger reserved for invoice hash-chain — products do
NOT fire ledger entries, mirrors PR-48α §A-decision), ADR-0037
(`Currency` closed-vocab), ADR-0041 (operational vs maintenance area
split — products is a Master Data tile in the maintenance area),
PR-48α (Partners — the entity products mirrors), PR-79 (maintenance
dashboard tiles), PR-88 (PR-88 money-input parser — products reuses
`parseAmountToMinor`).

## Context

PR-91 introduces `products` as the second master-data entity, scoped
to per-product CRUD: name + unit of measure + currency + set price.
The catalog is the seed for future invoice-line autofill (operator
picks a saved product → autofills description / unit / unit price)
and for the eventual inventory / AP modules.

The single load-bearing design decision is the **unit-of-measure**
shape. NAV v3.0's `InvoiceData` schema requires every `<line>` to
carry a `<unitOfMeasure>` element. The schema's
`unitOfMeasureType` is a **closed enum** of tokens (`PIECE`,
`KILOGRAM`, `TON`, `KWH`, `DAY`, `HOUR`, `MINUTE`, `MONTH`, `LITER`,
`KILOMETER`, `CUBIC_METER`, `METER`, `LINEAR_METER`, `CARTON`,
`PACK`) plus a literal `OWN` token that pairs with a sibling
`<unitOfMeasureOwn>` free-text element for anything outside the enum.

Ervin's operator-typed examples cleanly map to NAV tokens (`db` =
PIECE, `nap` = DAY, `tonna` = TON, `kg` = KILOGRAM, `óra` = HOUR,
`liter` = LITER, `m` = METER, …) — except for `liter@15C`, the
temperature-corrected litre used in fuel sales. NAV has plain
LITER (volumetric) but no temperature-corrected variant; the
catalog needs a way to carry `liter@15C` end-to-end so the future
NAV emitter can render it as `OWN` + `<unitOfMeasureOwn>liter@15C</...>`
on the wire.

Three forces in tension:

1. **Round-tripping to NAV cleanly.** A future "pick product →
   autofill invoice line" feature must be able to take any saved
   product and emit a valid `<unitOfMeasure>` element pair. The
   product's stored unit MUST map deterministically to NAV's wire
   shape — no per-emit-site re-translation, no string-matching
   guesses.
2. **Operator freedom for custom labels.** Domain-specific units
   like `liter@15C` (and, eventually, `liter@20C`, `darab@batch`,
   industry-specific measures) cannot all live in the NAV enum.
   The model needs an escape hatch the operator can populate
   without ADR-amendment ceremony.
3. **Type-safety against accidental misuse.** A flat enum that
   includes `OWN` as a variant (`NavUnitOfMeasure::Own`) would
   let a caller emit `<unitOfMeasure>OWN</unitOfMeasure>` *without
   the paired `<unitOfMeasureOwn>` free-text*, producing an
   XSD-malformed line. The type system should prevent that class
   of bug at compile time.

## Decision

**Model the product's unit of measure as a sum type with two
distinct branches:**

```rust
pub enum NavUnitOfMeasure {
    Piece, Kilogram, Ton, Kwh, Day, Hour, Minute, Month, Liter,
    Kilometer, CubicMeter, Meter, LinearMeter, Carton, Pack,
    // NOTE: OWN is intentionally NOT a variant here.
}

pub enum ProductUnit {
    Nav(NavUnitOfMeasure),  // → <unitOfMeasure>{token}</unitOfMeasure>
    Own(String),            // → <unitOfMeasure>OWN</unitOfMeasure>
                            //   <unitOfMeasureOwn>{label}</unitOfMeasureOwn>
}
```

Wire shape uses serde's internally-tagged form:
- `{"kind": "Nav", "value": "PIECE"}`
- `{"kind": "Own", "value": "liter@15C"}`

DuckDB storage uses two columns (`unit_kind VARCHAR CHECK IN
('Nav','Own')` + `unit_value VARCHAR NOT NULL`) so a future "filter
by NAV unit" query is a plain SQL predicate rather than a JSON
extract.

SPA surface: the ProductForm dropdown surfaces NAV tokens with
Hungarian labels (`db (Piece)`, `nap (Day)`, `tonna (Ton)`, …) plus
a sentinel "Egyéb (Own — free text)" option that reveals a
required free-text input. The SPA's composer
(`composeProductInputs`) translates the dropdown selection +
optional label into the `ProductUnit::Nav | Own` wire body.

`Currency` reuses the existing `aberp_billing::Currency` closed-vocab
per ADR-0037; `unit_price_minor: i64` follows ADR-0037's minor-unit
storage and PR-88's `parseAmountToMinor` operator-input rule (bare
integer = WHOLE major units; cents only on explicit separator).

**Out of scope (named-deferred):**

- **Invoice-line integration.** PR-91 ships the catalog CRUD only;
  the IssueInvoice line editor does NOT yet read this table. The
  future "pick a product" affordance is a downstream PR.
- **Inventory / stock levels / price history.** Not modelled.
- **AP module / supplier-side product entries.** The `kind`
  discriminator that Partners carries (`Customer | Supplier | Both`)
  is NOT mirrored on products — Ervin's brief framed products as
  saleable items; the AP-side surface is a separate future
  entity.
- **Per-field history.** Mirrors Partners (PR-48α §A-decision):
  row-level `created_at` / `updated_at` / `deleted_at` only, no
  `aberp_audit_ledger` entries (the ledger is reserved for the
  invoice hash-chain per ADR-0008 — extending the `EventKind`
  ladder to cover catalog ops would couple product CRUD to invoice
  integrity verification, wrong surface). A future
  `products_history` append-only table is a back-compat add if
  audit becomes a compliance ask.

## Consequences

### Positive

- **NAV alignment is compile-time-checked.** Every `ProductUnit::Nav`
  variant has a known NAV token via `nav_token()`; a `Nav(token)`
  case in a future emitter cannot accidentally render `OWN`
  without a paired free-text payload. The type system catches what
  a string-based design would catch only at NAV's wire boundary
  (post-submit, ABORTED invoice — the worst-class miss per
  CLAUDE.md rule 12).
- **One escape hatch, one widening rule.** Custom units (today's
  `liter@15C`; tomorrow's `liter@20C`, `darab@batch`, …) flow
  through `Own(String)` without an ADR amendment. Adding a token
  to NAV's actual enum is a single Rust enum + serde widening +
  SPA dropdown registry update — three coordinated edits, all
  guarded by the round-trip pin (`nav_unit_serde_round_trip_pin`
  on Rust + `NAV_UNIT_OPTIONS covers every token exactly once` on
  the SPA).
- **Operator UX is honest.** The "Egyéb (Own)" sentinel surfaces
  the escape-hatch nature of the OWN path — the operator typing
  `liter@15C` sees it land where it belongs (not silently coerced
  to plain LITER and underbilling the fuel measure).
- **Round-trip pin is cheap.** The DB stores two columns; the wire
  is internally-tagged JSON; the helper functions `to_db_columns`
  / `from_db_columns` + serde derives are the only translation
  surfaces. Three pin tests cover the round-trip
  (`product_unit_serde_nav_variant_pin`,
  `product_unit_serde_own_variant_pin`,
  `product_unit_db_columns_round_trip`) + two negative pins for
  rejection of unknown tokens / kinds.

### Negative

- **The dropdown has 16 options** (15 NAV tokens + Egyéb). Not all
  are common in Ervin's invoicing today (`KWH`, `LINEAR_METER`,
  `CARTON`, `PACK` rarely apply to consulting / fuel). The order
  in `NAV_UNIT_OPTIONS` puts the most common first
  (PIECE / DAY / HOUR / MINUTE / MONTH / KILOGRAM / TON / LITER)
  to keep the operator's scan short. If the registry order
  becomes operator-painful, a future PR can hide rarely-used
  tokens behind a "More units…" sub-section without changing the
  shape.
- **Own labels are not validated against NAV's `unitOfMeasureOwn`
  charset.** NAV's XSD constrains the free-text to a printable
  subset; PR-91's backend `validate_product_inputs` only rejects
  empty-after-trim. If the operator types a label NAV rejects
  (e.g. containing a control character), the issue surfaces at
  NAV submit time, not at product save. Tracked: future
  hardening once the line-editor integration lands and exercises
  the wire round-trip end-to-end. Per CLAUDE.md rule 2 (no
  speculative abstractions), the validator stays minimal until
  the use case demands more.
- **Two columns for one logical field.** A single `unit_json VARCHAR`
  blob would have stored a tagged JSON value with the same
  information density, but DuckDB's `json_extract` predicates are
  awkward and the type system would lose visibility into the
  shape at SQL inspection time. The two-column design is verbose
  in SQL but pays back at every audit / inspection / future
  query (e.g. "list every product using OWN units" is a one-line
  `WHERE unit_kind = 'Own'`).

### Neutral

- **No backward-compat surface to preserve.** Products is a new
  entity; no migration from pre-PR-91 shapes.
- **Audit posture inherits Partners' choice.** Row-level
  timestamps + soft-delete; no ledger entries. If a future
  compliance ask requires per-field history, the append-only
  `products_history` table is the back-compat seam (mirrors the
  `partner_history` non-decision in PR-48α).
- **Currency is closed-vocab today (HUF + EUR).** Same widening
  rules as ADR-0037 apply — adding CHF / USD is a single
  `aberp_billing::Currency` variant + the DuckDB CHECK constraint
  widening + the SPA dropdown entry.

## Pins

- **Rust (16 tests):**
  - `nav_unit_serde_round_trip_pin` — every `NavUnitOfMeasure`
    variant serialises as the NAV token AND round-trips cleanly
    AND agrees with `nav_token()`.
  - `product_unit_serde_nav_variant_pin` — JSON shape is
    `{"kind":"Nav","value":"PIECE"}`.
  - `product_unit_serde_own_variant_pin` — JSON shape is
    `{"kind":"Own","value":"liter@15C"}` (the canonical OWN case).
  - `product_unit_db_columns_round_trip` — two-column form
    round-trips for both branches.
  - `product_unit_from_db_rejects_unknown_nav_token` /
    `product_unit_from_db_rejects_unknown_kind` — defence
    against hand-edited DuckDB rows (loud-fail per CLAUDE.md
    rule 12).
  - `validate_product_inputs_accepts_minimal_valid` /
    `validate_product_inputs_surfaces_every_problem_at_once` /
    `validate_product_inputs_accepts_zero_price`.
  - `product_id_renders_with_prd_prefix`.
  - Six DuckDB CRUD round-trips covering Nav unit, Own unit
    (`liter@15C`), update, soft-delete, tenant scoping, and
    name-prefix search.

- **Rust integration (6 tests in `serve_products_route.rs`):**
  Six pins on the library-helper boundary mirroring
  `serve_partners_route.rs` — create-happy (Nav), create-happy
  (Own `liter@15C`), create-invalid (structured per-field errors),
  list ordering + search + get + 404, update-bumps-updated-at + 404,
  soft-delete-then-404 + re-delete-404.

- **SPA (17 tests in `products.test.ts`):** dropdown registry
  coverage (every `NavUnitOfMeasure` token exactly once),
  Hungarian labels for Ervin's load-bearing examples,
  `formFromProduct` → `composeProductInputs` round-trip for both
  Nav and Own branches, the canonical `liter@15C` Own path,
  PR-88's `parseAmountToMinor` integration (bare-int EUR =
  WHOLE euros, NOT cents — anti-regression for the PR-88 P0 bug),
  `filterProducts`, `unitLabel` rendering, A157 validation-error
  envelope parser.

## Adjacent

- PR-91 adds a `ProductCount` tile on the maintenance dashboard
  (PR-79); the tile renders "N saved products" with the same
  failure-isolation pattern as the existing tiles.
- The future invoice-line autofill (named-deferred) reuses
  `ProductUnit` to drive `<unitOfMeasure>` on the NAV emit path —
  the design pins the round-trip semantics so the line editor's
  composer can hand the catalog entry directly to the emitter
  with no per-line translation table.
