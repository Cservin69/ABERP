# S322 / PR-22 — Products master-data — VERIFY-ONLY

**Verdict: ALREADY SHIPPED. Re-cut REFUSED.** No `PROD_v2.27.13`.

The `project_aberp_products` spec — *"product catalog (name + unit-of-measure
closed-vocab, currency, price). MUST map unit to NAV's unitOfMeasure enum (OWN
for custom)"* — is **fully implemented and live**, shipped originally as **PR-91**
(products master-data CRUD, born "before session 124", `acc957a`) and extended
through S159 / PR-100 / PR-202 / PR-227. Live in production through the current
tag **`PROD_v2.27.12`**. This is established master-data, not a fresh feature.

Same outcome as [[project-aberp-workshop-demo-mode]] (S319), [[project-aberp-workshop-tv-density]]
(S320), and [[project-aberp-s321-partners-verify-only]] (S321) — the verify-first
discipline caught a fourth already-shipped brief in a row.

## Spec → shipped mapping

| Spec requirement | Status | Where |
|---|---|---|
| `products` table | ✅ | `apps/aberp/src/products.rs:236` (`ensure_schema`, CREATE TABLE IF NOT EXISTS) — pinned at boot `serve.rs:735` |
| name | ✅ | `products.name` column; `ProductInputs.name` |
| unit-of-measure **closed-vocab** | ✅ | `NavUnitOfMeasure` enum — **15 NAV v3.0 variants** (`modules/billing/src/domain/unit_of_measure.rs:46`): Piece/Kilogram/Ton/Kwh/Day/Hour/Minute/Month/Liter/Kilometer/CubicMeter/Meter/LinearMeter/Carton/Pack |
| currency | ✅ | `products.currency` column; `ProductInputs.currency` |
| price | ✅ | `products.unit_price_minor BIGINT` (minor units; `unit_price_minor: i64`) |
| **NAV unitOfMeasure mapping** | ✅ | `NavUnitOfMeasure::nav_token()` → `"PIECE"`/… , `from_nav_token()` round-trip (`unit_of_measure.rs:69,125`); emitted at `apps/aberp/src/nav_xml.rs::write_lines` (S159) |
| **OWN for custom** | ✅ | `ProductUnit::Own(String)` outer sum (`unit_of_measure.rs:167`) — emitter renders `<unitOfMeasure>OWN</unitOfMeasure>` + `<unitOfMeasureOwn>{label}</…>`. Canonical example `liter@15C` (temperature-corrected fuel measure, no plain LITER variant) |
| CRUD endpoints | ✅ | `serve.rs:2856-2864` — `GET/POST /api/products`, `GET/PUT/DELETE /api/products/:id`; `require_ready` + bearer auth |
| **typeahead `q=`** | ✅ | `list_products(.., search: Option<&str>)` case-insensitive `LOWER(name) LIKE` prefix (`products.rs:380`); handler `handle_list_products` (`serve.rs:9685`) |
| **invoice product typeahead (SPA)** | ✅ | `product-combobox.ts` wired into `routes/IssueInvoice.svelte:82,1389` (`productLineComboboxState`), per-line combobox with load-error/retry/no-match states |
| Products settings SPA | ✅ | `routes/ProductsList.svelte`, `ProductDetail.svelte`, `ProductForm.svelte`; `lib/products.ts` `NAV_UNIT_OPTIONS` dropdown |

## Design notes worth flagging (these are deliberate, not gaps)

1. **The brief's sketch was narrower than what shipped.** The brief proposed an
   8-variant enum (PIECE/KG/METER/LITER/M2/M3/HOUR/OWN). The shipped enum is the
   **full NAV v3.0 `unitOfMeasureType` closed vocab (15 variants)** — the brief's
   `M2`/`M3` are `CubicMeter` (`CUBIC_METER`) plus there's no square-meter variant
   in NAV v3.0; NAV uses `METER`/`LINEAR_METER`. Implementing the brief's sketch
   would have been a **regression**. ADR-0046 governs variant additions.

2. **`OWN` is modelled at the OUTER `ProductUnit` level, not as a `NavUnitOfMeasure`
   variant.** This is load-bearing: a flat `Nav(OWN)` shape would let a caller emit
   `OWN` without the paired `<unitOfMeasureOwn>` free-text — a class of bug the sum
   type makes unrepresentable. The brief's sketch (`Own` as an enum variant) would
   reintroduce that hazard.

3. **DB shape: `unit_kind VARCHAR CHECK (IN ('Nav','Own'))` + `unit_value VARCHAR`,
   NOT a JSON blob.** Two columns so the "list products by NAV unit" query is a plain
   SQL predicate (`products.rs:131` `unit_to_db_columns`). The brief's sketched
   `unit` + `unit_label` columns map to this `unit_kind`/`unit_value` pair.

4. **`Own` requires a non-empty label** — `validate_product_inputs` rejects empty
   Own labels (`products.rs:216`), matching the brief's
   `s322_product_own_unit_requires_unit_label` intent. Already pinned by
   `crud_round_trip_own_unit_liter_at_15c` + `validate_product_inputs_surfaces_every_problem_at_once`.

5. **No `sku`/`vat_pct`/`archived_at` exactly as brief-sketched.** Soft-delete IS
   present (`soft_delete_product`, `products.rs:471`, hides from get+list — test
   `soft_delete_hides_row_from_get_and_list`). SKU and per-product VAT were not part
   of the original spec and are not shipped; **not implemented here** (verify-only,
   no scope creep per [[cut-sessions-no-code]] + CLAUDE.md #3). If Áben wants
   per-product SKU/VAT that is a genuine new feature for a future IMPL session, not a
   verify-only gap.

6. **Audit events: products emits NONE** — consistent with the partners precedent
   ([[project-aberp-s321-partners-verify-only]], a deliberate ADR-0008 rejection).
   The brief's "unless the partners-precedent applies" hedge resolves to: it applies.
   No `product.created`/`updated`/`archived` EventKinds, by design.

## Brief's proposed tests — all already have shipped equivalents

| Brief test | Shipped equivalent |
|---|---|
| `s322_product_typeahead_returns_matches` | `products::tests::search_filters_by_name_prefix_case_insensitive` |
| `s322_product_nav_unit_mapping_round_trips` | `nav_unit_serde_round_trip_pin`, `product_unit_db_columns_round_trip`, `crud_round_trip_nav_unit`, billing `display_label_hu_covers_every_variant` |
| `s322_product_own_unit_requires_unit_label` | `validate_product_inputs_surfaces_every_problem_at_once` (+ `crud_round_trip_own_unit_liter_at_15c`) |

## Gates (verify-only — no code changed; baseline confirmed green)

- `cargo test -p aberp --lib products::` → **16 passed**, 0 failed
- `cargo test -p aberp-billing --lib unit_of_measure` → **1 passed** (the exhaustive
  variant pin `display_label_hu_covers_every_variant`), 0 failed
- `npx vitest run product` (SPA) → **59 passed** across 3 files
  (`product-combobox.test.ts`, `products.test.ts`, `product-list-persistence.test.ts`)
- No test-count delta: nothing implemented.

## Why no re-cut (HARD RULE)

PR-22's deliverable already exists in prod, more completely than the brief sketches.
Re-implementing per the brief's 8-variant sketch would be a NAV-compliance
regression. Per the S319–S321 precedent, the conservative/most-reversible action is
to **refuse the re-cut, file this verify-only finding, and push the branch**. No
`PROD_v2.27.13`.

Branch: `session-322/pr-22-products-master-data-verify-only`.
