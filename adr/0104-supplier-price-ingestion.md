# ADR-0104 — Vendor/supplier price ingestion for auto-quoting

- **Status:** **Accepted** (implemented this session). The last absent piece of the auto-quoting strand: a path to ingest supplier/vendor material prices into the quote cost model, with a reproducibility guarantee.
- **Date:** 2026-07-23
- **Deciders:** Ervin Áben (feature owner). Implementation-pass by Claude.
- **Related:** ADR-0037 (MNB rate posture — no silent fallback), ADR-0057 (no public inbound HTTP surface), ADR-0074 (`material.*` audit family), ADR-0099 (shared `Handle` / opener census), S266 (`quoting_materials` table), S271 (`quote_pricing_pipeline` wiring), S418 (geometry pricing model), S428 (margin profiles), S429 (calibration `set_hash` reproducibility precedent).

---

## 0. TL;DR

Material stock prices live in `quoting_materials.cost_per_kg_eur` (EUR/kg, per-tenant), operator-maintained via audited CRUD (`quoting_materials.rs`). At quote time the pipeline reads them **live** (`quote_pricing_pipeline.rs:850`, `list_materials`) and feeds them to the pure engine. There was **no bulk path to ingest a vendor price list**, and **no record of which prices a given quote used** — a re-quote after any price edit silently yields a different number.

This ADR adds:

1. **Ingestion** (`supplier_prices::parse_price_list` + `ingest_price_list`): parse a real-shaped CSV supplier price list, validate it in Rust, normalise currency to EUR, and apply it to `quoting_materials.cost_per_kg_eur` in **one transaction on the shared `Handle`**, audited in-tx (reusing `MaterialCatalogueChanged`, `op="supplier_ingest"`).
2. **Reproducibility pin** (`supplier_prices::record_price_set` + `resolve_price_set`): a content-addressed `quote_price_snapshots` table. At price time the pipeline records the exact `(grade → cost_per_kg_eur)` set the engine used, hashes it (FNV-1a, identical construction to S429 `CalibrationTable::set_hash`), stamps the hash on the priced quote, and can re-derive that exact price set later → **re-running yields the same number** even after prices change.

---

## 1. Current state (grep-verified)

| Layer | Today | File:line |
|---|---|---|
| Material price source | `quoting_materials.cost_per_kg_eur DOUBLE NOT NULL`, per-tenant, operator CRUD, audited (`MaterialCatalogueChanged`) | `apps/aberp/src/quoting_materials.rs:59`, CRUD `:627/:699/:770` |
| Engine consumption | `material_cost = mass_kg × cost_per_kg_eur` (pure) | `crates/aberp-quote-engine/src/engine.rs:212` |
| Price read at quote time | `list_materials()` read **live** per pricing pass | `apps/aberp/src/quote_pricing_pipeline.rs:850` |
| Priced-quote record | `QuoteBreakdown` JSON + `QuotePricingPricedPayload` (output figures only — **no input-price provenance**) | `quote_pricing_pipeline.rs:1045` |
| Reproducibility precedent | S429 calibration stamps `coefficient_set_hash` (FNV-1a over the input set) on the quote | `crates/aberp-quote-engine/src/calibration.rs:121`; wired `quote_pricing_pipeline.rs:1082` |
| Currency | Quotes EUR-native; MNB HUF↔foreign conversion only at invoice issuance; **no silent fallback rate** | `crates/mnb-rates/src/lib.rs`, `apps/aberp/src/mnb_rates_provider.rs` |
| External ingestion patterns | Inbound quotes = **poll + stage** in a log table; catalogue = **outbound push**; NAV recovery = **XML file parse**. **No CSV price importer exists.** | `crates/aberp-quote-intake/`, `apps/aberp/src/catalogue_push.rs`, `apps/aberp/src/recover_from_nav.rs` |
| Opener census | Frozen surface; a new `Connection::open`-bearing file must be registered or route via the shared `Handle` | `tools/cut_gate_opener_census.sh`, `tools/adr0098_prod_frozen_residuals.txt` |

**The gap:** the price *feed* (bulk ingestion) and the price *as-of record* (reproducibility). Manual per-grade CRUD already exists; it is not a bulk vendor feed, and it leaves no trace of which price a quote consumed.

---

## 2. Design decisions (each flagged; the conservative option taken)

### 2.1 Ingestion shape → **CSV file import**, not an API poll ⚑

**Decision:** ingest a **CSV price list** (a file the operator receives from a vendor), parsed + validated in Rust.

**Why (conservative):**
- ABERP has **no public inbound HTTP surface** (ADR-0057) and no supplier API contract. The only external fetch is MNB (government SOAP), which the codebase treats as a special, no-fallback dependency (ADR-0037). Inventing a supplier polling API would be speculative abstraction (CLAUDE.md rule 2) against a contract that does not exist.
- Real vendor price lists arrive as files (CSV/XLSX/PDF). CSV is the lowest-assumption, fully-testable shape.
- Manual entry already exists (`quoting_materials` CRUD); the gap is **bulk** ingestion, not another single-row editor.

**Shape:** header `grade,cost_per_kg[,currency]`; one row per material grade. `currency` is optional per-row and falls back to a batch-level default. Parse is strict and **loud** (rule 11): unknown headers, non-finite/negative costs, and unknown currencies are rejected with the offending row number — never silently skipped.

**Flagged residual:** XLSX/PDF price lists are out of scope — the operator exports to CSV first. Recorded as a deferral (§7).

### 2.2 Unknown grades → **reject the whole batch** (all-or-nothing) ⚑

**Decision:** if any CSV row names a `grade` not present in `quoting_materials` for the tenant, **the entire ingest fails** and returns the list of unknown grades. Nothing is applied.

**Why (conservative):** partial-apply-and-skip is exactly the "completed successfully with 14% silently skipped" failure CLAUDE.md rule 11 forbids. A supplier list referencing an unknown grade is an operator data-mismatch to resolve (add the grade to the catalogue first), not something to paper over. All-or-nothing also keeps the applied set congruent with a single audit batch.

**Flagged alternative:** "create missing grades on ingest." Rejected — a price row carries no density / machining-difficulty / stock-status, so an auto-created grade would be an under-specified catalogue row the engine could mis-price. Grade creation stays the operator's explicit CRUD act.

### 2.3 Currency → reuse `aberp_billing::Currency`; **pin the FX rate into the record** ⚑

**Decision:** the library accepts EUR and HUF rows. HUF is normalised to EUR at ingest with an **explicit, caller-supplied MNB rate** (`FxToEur { huf_per_eur, rate_date }`), and both the native amount and the pinned rate/date are recorded in the audit payload.

**Why:**
- Reuses the existing `aberp_billing::Currency` vocab (rule 8 — read/reuse, don't reinvent) and the MNB **no-silent-fallback** posture (ADR-0037): if the rate is unavailable, ingest never runs — it does not guess.
- The FX rate is **passed in**, not fetched inside the library, so the ingest is deterministic and unit-testable offline, mirroring `mnb_rates_provider`'s injection posture. The network fetch belongs to the operator-facing caller (the deferred UI / CLI), exactly where issuance puts it (`issue_invoice::run_with_provider`).
- Pinning the rate + date makes a HUF ingest **auditable and reproducible** the same way an EUR-branch invoice pins its issuance rate.

Conversion: MNB quotes `value` HUF per `unit` foreign units, so `cost_eur = cost_huf × unit / value = cost_huf / huf_per_eur`.

### 2.4 Reproducibility / effective-dating → **content-addressed per-quote price snapshot** ⚑

**The requirement:** "a quote records which price snapshot it used, so a re-quote is reproducible."

**Decision:** at price time, capture the exact `(grade → cost_per_kg_eur)` set the engine consumed, hash it, persist it content-addressed in `quote_price_snapshots`, and stamp the hash on the priced quote. Re-derivation loads that exact set by hash and re-runs the engine → identical total.

**Why this shape and not the obvious alternatives:**
- **Not "make the engine read from an effective-dated feed table" (single source of truth).** That would re-plumb the hot pricing path and half-migrate a shipped subsystem — CLAUDE.md rule 14 (all-or-nothing per subsystem) forbids leaving reads on one source and writes on another. The engine keeps reading the live `quoting_materials` column unchanged.
- **Not "pin the latest supplier `snapshot_id`."** The live column can also be moved by manual CRUD, so "latest supplier batch" is not guaranteed to equal what the engine used. Pinning **exactly what was used** is robust to *every* writer (supplier feed or manual edit) and is the same guarantee S429 already gives for calibration coefficients.
- **Content-addressing** means identical price sets dedupe to one row-group (`INSERT ... ON CONFLICT DO NOTHING`); the hash is the natural key and doubles as the drift detector.

**"As of when":** the immutable audit event (`MaterialCatalogueChanged`, `op="supplier_ingest"`) carries the ingest timestamp, `effective_at`, source label, currency and pinned FX — that is the *feed's* as-of. The *quote's* as-of is its pinned `price_snapshot_hash`, which resolves to the exact prices. No separate feed table is needed (the ledger is the feed history — same posture the catalogue CRUD already uses).

---

## 3. Schema

One new table (per-tenant, lazy `CREATE TABLE IF NOT EXISTS`, plain columns + PK, no CHECK/triggers — `[[no-sql-specific]]`, matching `quoting_materials`):

```sql
CREATE TABLE IF NOT EXISTS quote_price_snapshots (
    tenant_id        VARCHAR NOT NULL,
    price_set_hash   VARCHAR NOT NULL,   -- FNV-1a over sorted (grade=cost:.6;)
    grade            VARCHAR NOT NULL,
    cost_per_kg_eur  DOUBLE  NOT NULL,
    PRIMARY KEY (tenant_id, price_set_hash, grade)
);
```

Content-addressed: the same price set written by two quotes collapses to one row-group; re-recording is idempotent (`ON CONFLICT DO NOTHING`).

No new column on `quoting_materials`. The ingest **writes** the existing `cost_per_kg_eur`. The priced-quote pin rides `QuotePricingPricedPayload` (new additive `price_snapshot_hash` field) — no schema change to `quote_pricing_jobs`.

---

## 4. Flow

**Ingest** (`ingest_price_list`, one tx on a `&mut WriteGuard`):
1. `parse_price_list(csv)` → `Vec<ParsedPriceRow>` (loud validation).
2. Normalise each row to EUR (`FxToEur` for HUF; identity for EUR).
3. Verify every grade exists in `quoting_materials` — else `Err(UnknownGrades(..))`, nothing applied.
4. In one tx: `UPDATE quoting_materials SET cost_per_kg_eur=? ...` per grade **and** `append_in_tx(MaterialCatalogueChanged, op="supplier_ingest", <batch payload>)` per grade → `commit()` (business + audit atomic, rule 15).

**Pin** (in `advance_price`, inside the existing priced-audit tx):
1. After loading `materials`, `record_price_set(&tx, tenant, &engine_materials)` → `price_snapshot_hash` (`INSERT ON CONFLICT DO NOTHING`).
2. Stamp the hash into `QuotePricingPricedPayload`.

**Re-derive** (`resolve_price_set(conn, tenant, hash)` → `BTreeMap<grade, cost>`): overlay onto the catalogue, re-run the engine → identical figure. (A one-click operator "reprice" endpoint is deferred UI — §7.)

---

## 5. Alternatives rejected

- **Supplier API poll** — no contract, no inbound HTTP (§2.1).
- **Effective-dated feed as the engine's price source** — half-migration of a shipped subsystem (§2.4, rule 14).
- **Store only the hash, not the price set** — gives drift *detection* but not *re-derivation*; the task requires the number to be reproducible.
- **New `EventKind::SupplierPriceIngested`** — the audit-ledger EventKind extension ritual is heavier surface for no semantic gain; a supplier ingest **is** a catalogue change, so `MaterialCatalogueChanged` with a distinct `op` is the surgical fit (rule 3), and per-grade events preserve the existing per-grade history posture.

---

## 6. Consequences

- Bulk vendor price lists now feed the cost model through one validated, audited, atomic entry point.
- **Every** priced quote (not only supplier-fed ones) now pins the exact prices it used — the reproducibility guarantee is universal, matching how invoices are auditable.
- One new content-addressed table; one additive audit-payload field; one new writer of `cost_per_kg_eur`. No change to the pure engine, no change to `quoting_materials`/`quote_pricing_jobs` schema, no new DB opener in the library path (all DB access via the shared `Handle`/`&Connection`), so the opener census does not move.

---

## 7. Deferrals (explicit)

- **Operator UI** — the SPA upload screen and its HTTP route are **not** built this session. The library `ingest_price_list` is the seam a route/CLI calls; the live MNB-rate fetch for a HUF upload lands with that UI (the library already normalises HUF given the rate). *(If a CLI `supplier-prices ingest` subcommand ships alongside this ADR, it registers its one `Connection::open` in the census honestly — noted in the session report.)*
- **Non-CSV formats** (XLSX/PDF) — operator exports to CSV first.
- **One-click "reprice from pinned snapshot" endpoint** — `resolve_price_set` provides the mechanism; the operator-facing button is UI.
