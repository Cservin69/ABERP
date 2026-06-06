# Auto-Quoting — Ground-Zero Design

**Status:** Draft v0 — ground-zero design only, **no code**. Session 265 / PR-254.
**Base commit:** `57139c3` (S264 red-sweep, PROD_v2.17.1).
**Author session:** S265.
**Companion ADRs (filed in this PR):** ADR-0066 (quote architecture), ADR-0067 (DEAL saga + atomicity), ADR-0068 (vendor-PO spend authorization), ADR-0069 (material reservation states — files ADR-0061 OQ#2).
**Predecessor doc:** [`docs/e2e-shop/ground-zero.md`](../e2e-shop/ground-zero.md) §8 "Quote engine decomposition" — this doc is the **Phase 5** (full CAM auto-quote) detailed design that §8 named as future-facing.

> Custom shop where clients upload CAD, configure features, and receive auto-quotes from CAM assessment + product/stock prices + manufacturing time + complexity + transport.
>
> — Ervin, S198 prep (the Stage 2 vision)

This doc is **the ground floor for the auto-quoting strand**. It ships zero product code. The deliverables S266–S275 implement against it. Every default chosen here is reversible by Ervin with a sentence; where a choice is non-obvious, it is flagged inline with the reasoning rather than asked as a blocking question (per the project's no-ask-user-question discipline — pick conservative, flag the call).

It is written **adversarially against the originating brief**. The brief encoded a v1.0 spec plus 15 sober-pushback additions. Several of those additions were right and are adopted verbatim; several rested on premises that do not hold against the committed ADR baseline (no machine-scheduling board, no sales-order module). Those are surfaced in §14 with the corrected design.

---

## 1. Where this sits — two "quote" concepts, do not conflate

ABERP already has a thing called a quote. It is **not** this.

| | **Quote intake** (ADR-0057, shipped S210/S211/S255) | **Auto-quoting** (this doc) |
|---|---|---|
| Who prices | The storefront / operator, manually | The `aberp-quote-engine`, automatically from geometry |
| Trigger | A quote already *approved* on the storefront | A raw **CAD upload** with no price yet |
| ABERP's role | Poll, stage in `quote_intake_log`, operator picks up → draft invoice | Pull the CAD, extract features, **compute the price**, email an indicative quote, run the DEAL cascade |
| Table | `quote_intake_log` | new `quotes` |
| State | staged → picked-up | `indicative → accepted → binding → converted → cancelled` |

The two are complementary, not competing. Quote-intake is the bridge for quotes priced elsewhere; auto-quoting is the engine that *does* the pricing. A future consolidation is possible (auto-quoting could feed the same pickup surface), but v1 keeps them separate tables — `quotes` is the regulated-adjacent record with its own state machine, `quote_intake_log` stays the thin staging log. Per CLAUDE.md rule 13, we do not generalise the two until a second consumer earns it.

**Architectural inheritance from ADR-0057 that auto-quoting reuses wholesale:**

- **Operator-pull, never inbound.** ABERP is a local desktop app (Tauri + loopback HTTPS, ADR-0004). It has no public IP and no webhook surface. *Everything* flows ABERP → storefront on a poll; nothing calls into ABERP. This single fact dictates §3's topology and is the reason the "ABERP-side material dropdown endpoint" in the brief is actually a **storefront-hosted** endpoint fed by an ABERP push (see §14, pushback C).
- **The regulated `invoice` surface is never auto-burned.** The DEAL cascade produces a Work Order, not an invoice. The invoice is born at Dispatch (ADR-0064) and issued by the operator's click. No background path touches NAV.
- **Daemon shape.** Boot-spawned `tokio` loop, audit-cycle entry, refuse-to-start on bad config, graceful dormancy, `Zeroizing` bearer token. Identical to S161/S178/S210.

---

## 2. The three crates + one Python program

```
┌─────────────────────────── ABERP desktop (local, no inbound) ──────────────────────────┐
│                                                                                          │
│  crates/aberp-quoting/           ← daemon: poll storefront, orchestrate, persist        │
│    ├── poll loop (reuses ADR-0057 daemon shape)                                          │
│    ├── quotes table + state machine (ADR-0067)                                           │
│    ├── DEAL saga orchestrator (ADR-0067)                                                 │
│    └── indicative-PDF generator (reuses invoice-pdf text/layout primitives)             │
│                                                                                          │
│  crates/aberp-cad-extract-wrapper/   ← Rust subprocess shim                              │
│    ├── spawns the Python program with timeout + sandbox                                  │
│    ├── validates returned JSON against a pinned schema                                   │
│    └── versioned protocol (extractor_version in every result)                            │
│             │ stdin: blob path + opts          ▲ stdout: feature-graph JSON              │
│             ▼                                   │                                        │
│  python/aberp-cad-extract/           ← Python program (NOT a Rust crate)                 │
│    ├── build123d / OCP (pythonOCC) STEP·IGES·3MF·STL·X_T loader                          │
│    └── feature extractors → JSON feature graph                                           │
│                                                                                          │
│  crates/aberp-quote-engine/          ← PURE function, no I/O, no clock, no RNG           │
│    (feature-graph JSON + catalogue snapshot + params) → quote breakdown JSON             │
│    idempotent · deterministic · property-testable                                        │
│                                                                                          │
│  DuckDB (per-tenant):                                                                     │
│    quoting_materials · quoting_complexity_rules · quoting_tolerance_multipliers          │
│    quoting_machines · quoting_parameters · quoting_stock_adjustments                     │
│    quoting_margin_profiles · quotes · stock_reservations (ADR-0069)                      │
│    vendor_pos (ADR-0068)                                                                  │
│                                                                                          │
└──────────────────────────────────────────────────────────────────────────────────────┘
        │  poll: GET /api/cad-quotes?status=pending          ▲ push: PUT /api/catalogue/materials
        │  pull blob: GET /api/cad-quotes/:id/blob           │ writeback: POST /api/cad-quotes/:id/status
        ▼                                                     │ (indicative ready / accepted)
┌──────────────────────── abenerp.com storefront (public, Vercel-EU) ─────────────────────┐
│   /quote upload page · material dropdown (from cached ABERP push) · status page          │
│   HMAC accept-link landing  ·  CAD blob object storage                                    │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

**Why this split** (full rationale in ADR-0066):

- **Python for CAD extraction** because the mature B-rep kernels (OpenCASCADE via `build123d`/OCP) are Python-first. Rewriting STEP/IGES tessellation in Rust is a multi-year sink for zero differentiation (delete-the-part: the geometry kernel should not exist in our tree at all — we shell out to one).
- **A Rust wrapper** because the Python program is an untrusted, possibly-crashing, possibly-hanging parser of customer-supplied files. The wrapper is the blast-door: timeout, memory cap, subprocess sandbox, schema validation, version stamping. The rest of ABERP only ever sees a validated `FeatureGraph` struct or a typed error — never raw Python output.
- **Pure-Rust scoring** because pricing must be deterministic, idempotent, and property-testable. Same feature graph + same catalogue snapshot + same params → byte-identical breakdown, every time, with no I/O in the path. This is what makes `feature_graph_hash` a meaningful idempotency key (§ pushback H) and what lets the learn-loop (§9) compare estimate-vs-actual cleanly.

---

## 3. End-to-end flow

```
 1. Customer            uploads CAD on storefront /quote, picks material from dropdown,
                        enters tolerance class + quantity + delivery postcode.
                        Storefront stores blob in object storage, creates a pending cad-quote.

 2. ABERP daemon        poll cycle (default 60s): GET /api/cad-quotes?status=pending.
                        For each new id not in `quotes`:
                          a. GET /api/cad-quotes/:id/blob  → encrypt-at-rest into tenant blob store
                             (AES-GCM, keychain key; audit on every later read — ADR-0014/0007).
                          b. aberp-cad-extract-wrapper(blob) → FeatureGraph JSON (or typed error).
                          c. feature_graph_hash = BLAKE3(canonical(FeatureGraph)).
                          d. catalogue snapshot: read the 8 quoting_* tables AS-OF now.
                          e. aberp-quote-engine(FeatureGraph, snapshot, params, margin_profile)
                             → QuoteBreakdown.
                          f. INSERT quotes row (state=indicative, valid_until bounded by stock
                             projection — § pushback G), audit quote.auto_estimated.
                          g. render indicative PDF, email customer an HMAC accept link
                             (30-day expiry — § pushback M). SMTP SPOC (shared creds).
                          h. writeback POST /api/cad-quotes/:id/status {indicative_ready, quote_no}.

 3. Customer            clicks the HMAC link. Storefront validates token, marks accepted,
                        surfaces it on the next ABERP poll → quotes row flips to `accepted`,
                        breakdown is FROZEN (§ pushback F). audit quote.accepted.

 4. Operator           sees `accepted` quotes in the SPA Quotes tab. Reviews the breakdown,
                        optionally OVERRIDES a line with a mandatory reason (§ pushback L),
                        then clicks the single typed DEAL token (hülye-biztos: one token,
                        not a checkbox dialog).

 5. DEAL saga          atomic cascade (ADR-0067): reserve materials (ADR-0069) → fire vendor
                        PO if short, gated by threshold (ADR-0068) → create Work Order
                        (ADR-0062) → state→binding→converted. All-or-nothing. audit
                        deal.started / deal.completed (or deal.rolled_back).

 6. Downstream         WO runs the existing Stage-3 rails (Release consumes the reservation,
                        Complete + QA, Dispatch ships + spawns the invoice DRAFT, operator
                        Issues → NAV). Auto-quoting adds NOTHING new past the WO hand-off.
```

The flow is one new module bolted onto the **top** of the existing manufacturing pipeline. Auto-quoting's job ends when a Work Order exists; from there the committed ADR-0062/0063/0064 rails carry it to a NAV invoice with no changes.

---

## 4. State machine

```
                 customer clicks            operator clicks DEAL
                 HMAC accept link           (saga commits atomically)
   ┌───────────┐    ──────────►   ┌──────────┐    ──────────►   ┌──────────┐    ──►   ┌───────────┐
   │ indicative │                 │ accepted │                  │ binding  │          │ converted │
   └───────────┘                  └──────────┘                  └──────────┘          └───────────┘
        │                              │                              │                  (terminal,
        │ valid_until elapses          │ operator re-issues           │ saga rolls back   success)
        │ OR operator cancels          │ (fresh indicative,           │ (any step fails)
        ▼                              ▼  NEW quote_id)               ▼
   ┌───────────┐                  ┌───────────┐                  back to `accepted`
   │ cancelled │  ◄───────────────│ cancelled │ ◄────────────────  (reservation released,
   └───────────┘                  └───────────┘                    no WO, no partial state)
        (terminal)
```

| State | Meaning | Who triggers entry | What is logged | Re-price allowed? |
|---|---|---|---|---|
| `indicative` | Engine output, emailed, awaiting customer | daemon | `quote.auto_estimated` | yes — regeneration mints a **new** quote_id (§ pushback N) |
| `accepted` | Customer clicked the HMAC link; price FROZEN | customer click → next poll | `quote.accepted` | **no** — operator must re-issue a fresh indicative (§ pushback F) |
| `binding` | Operator clicked DEAL; commercial commitment made; saga running/committed | operator (DEAL token) | `deal.started` | no |
| `converted` | A Work Order now realizes this quote | DEAL saga success | `deal.completed` | no (terminal) |
| `cancelled` | Abandoned from any non-terminal state | operator, or `valid_until` lapse from `indicative` | `quote.cancelled` | n/a (terminal) |

**Transition rules (app-layer, no DB CHECK — engine-agnostic per ADR-0019):**

1. `indicative → accepted` requires a valid, unexpired HMAC token bound to *this* quote_id. An expired or mismatched token is refused loud.
2. `indicative → cancelled` fires when `now > valid_until` (a sweep on the poll cycle) or on operator cancel.
3. `accepted → binding` requires the operator DEAL click. The breakdown that becomes binding is the **frozen accepted snapshot**, not a recomputation.
4. `binding → converted` is the DEAL saga's atomic success. `binding → accepted` (rollback) restores the pre-saga state — reservation released, no WO, no half-state.
5. `accepted → cancelled` is allowed (deal fell through); it releases nothing because no reservation exists until `binding`.
6. `converted` and `cancelled` are terminal. There is no un-cancel and no un-convert; the recovery from a wrong terminal is a **new** quote.

**Why `binding` and `converted` are distinct states even though the saga is atomic.** `binding` is the instant the operator commits commercially — it is the audit anchor for "Áben said yes." `converted` is the instant a manufacturing artifact (WO) exists. In the single-transaction happy path they are microseconds apart, but the rollback path needs `binding` as the state the saga *exits from* on failure, and the audit trail needs both moments distinguishable. Collapsing them would lose the "committed-but-WO-creation-failed" forensic.

> **Pushback (J):** the brief's state name `converted_to_so` assumes a **Sales Order** entity. ADR-0015 (sales/PO state machine) is still a **stub** — there is no SO module. In v1 the conversion target is a **Work Order** (ADR-0062, which exists). The state is therefore named `converted`, and the realized entity is recorded by a `converted_wo_id` pointer. When ADR-0015 unstubs and a real Sales Order entity lands, the conversion target upgrades (WO becomes downstream of SO); the state name `converted` survives the upgrade unchanged. Naming the state `converted_to_so` today would bake a forward-reference to a module that does not exist.

---

## 5. DEAL saga — exact sequence, rollback, audit chain

Full treatment in **ADR-0067**. Summary here.

The DEAL token is a **single typed token** the operator types to confirm (hülye-biztos: one deliberate token, not a lawyer-clicking checkbox dialog). On submit, the saga runs as **one DB transaction** — all-or-nothing, per trust-code-not-operator:

```
deal_saga(quote_id, operator):
  preconditions:  quote.state == accepted        else refuse loud
                  HMAC acceptance on record        else refuse loud
                  frozen breakdown present         else refuse loud
  BEGIN TX
    1. emit deal.started (audit, with frozen-breakdown hash)
    2. for each BOM material line in the frozen breakdown:
         atp = stock_qty(prd) − SUM(open reservations on prd)     (ADR-0069)
         if atp >= need:  insert stock_reservation(prd, need, quote_id)   → mes.stock_reserved
         else:            short = need − atp
                          reserve atp, and procure `short`:
                            po_eur = short × material.cost_per_kg × ...
                            if po_eur <= max_auto_po_eur AND
                               daily_running_total + po_eur <= daily_cap:   (ADR-0068)
                                 fire vendor PO → po.vendor_po_fired
                            else:
                                 emit po.auto_threshold_exceeded, PAUSE saga,
                                 surface operator gate; DO NOT roll back the reservations
                                 already taken — hold them under the paused quote.
    3. create Work Order (ADR-0062) from the BOM × qty; link converted_wo_id
    4. quote.state = binding → converted
    5. emit deal.completed (audit, with wo_id + reservation ids + po ids)
  COMMIT
  on ANY failure before COMMIT: ROLLBACK → emit deal.rolled_back (best-effort, separate tx),
     quote returns to `accepted`, zero reservations, zero WO, zero PO.
```

**Rollback semantics.** Because the whole cascade is one transaction, a failure at step 3 (e.g. master-data missing) unwinds steps 1–2 entirely: no reservation rows survive, no PO record survives, the quote is back at `accepted`. The operator sees the specific error and fixes the master data before retrying. This is the same posture ADR-0064 §5 pins for dispatch's invoice-draft spawn.

**The one deliberate non-atomic seam: the threshold gate (step 2-else-else).** When a vendor PO exceeds the auto-spend ceiling, the saga **pauses** rather than rolls back — the materials that *were* available are held reserved under the paused quote, and an operator gate appears ("PO of €X exceeds your €Y auto-limit — approve or decline"). This is intentional: rolling back a 40-line BOM because one exotic material needs a €5k PO would be hostile. The paused state is durable (a `quotes.state` sub-flag + the `po.auto_threshold_exceeded` audit entry); operator approval resumes the saga from step 2, operator decline rolls the whole thing back. See ADR-0068.

**Audit chain.** `deal.started` (open) → N × `mes.stock_reserved` + 0..N × `po.vendor_po_fired`/`po.auto_threshold_exceeded` → `deal.completed` (close, carrying every child id) **or** `deal.rolled_back`. The close entry's payload lets an auditor walk from the quote to every reservation, PO, and the WO in one hop.

---

## 6. Vendor-PO threshold gate

Full treatment in **ADR-0068**. Two knobs live in `quoting_parameters`:

- `max_auto_po_eur` — per-PO ceiling. A single PO at or below this fires automatically inside the saga.
- `auto_po_daily_cap_eur` — cumulative ceiling across all autonomous POs in a rolling 24h window. Even sub-ceiling POs stop auto-firing once the day's total would breach the cap.

Above either threshold → `po.auto_threshold_exceeded` + operator gate (the single-token approve/decline). Below both → `po.vendor_po_fired`, recorded in a new `vendor_pos` table + an email to the supplier over the SMTP SPOC.

> **Scope honesty:** there is **no purchasing/supplier master-data module** (ADR-0061 explicitly defers it). v1's "vendor PO" is a *lightweight intent record* (`vendor_pos` row: supplier name/email operator-typed or from a minimal list, material, qty, eur, state) plus an email — **not** a full procurement workflow. The auto-fire path and the full purchasing module are **out of scope for S266–S275** (the brief's own session list never schedules vendor-PO implementation). ADR-0068 is filed now so the threshold posture is decided before anyone builds it; the saga skeleton (S273) and reservation (S274) ship the *gate-shaped hole* the PO step later slots into.

---

## 7. Machine reservation — corrected

> **Pushback (B, the big one):** the brief says "the quote engine **asks** the dispatch board (PR-230/ADR-0064) for a machine slot, never writes the machine calendar directly." **ADR-0064's "dispatch board" is the *shipping* dispatch board** — it ships finished goods and spawns invoice drafts. It has no concept of machine-time, calendars, or capacity. **There is no machine-scheduling/capacity board anywhere in the codebase** (verified: no table, module, or route matches machine-schedule/calendar/capacity/slot). The premise that the engine can "ask the dispatch board for a slot" is false.

**Corrected v1 design:** the DEAL saga does **not** reserve a machine slot, because there is nothing to reserve against. `quoting_machines` exists **for pricing only** — its `hourly_rate` / `setup_hourly_rate` feed the cost breakdown. Capacity/lead-time is modeled coarsely by `material.lead_time_default_days` and `quoting_parameters.setup_amortization_threshold`, not by a live calendar. The manufacturing commitment is the **Work Order existing in the queue** (ADR-0062), which is the honest v1 "reservation."

The brief's *principle* — the quote engine must **ask, never write** a scheduling authority — is correct and preserved as a **forward constraint**: when a Scheduling/Capacity board is eventually built (its own future ADR), the engine and the DEAL saga will call its reserve API; they will never write a machine calendar directly. `MachineSlotReserved` is therefore **specced but not emitted in v1** — it is reserved in the EventKind plan as a future kind, exactly as ADR-0061 reserves `MovementReason` variants for callers that don't exist yet. We do not file a machine-scheduling ADR in this PR: writing an ADR that commits to a board nobody is building violates delete-the-part.

---

## 8. Material reservation states

Full treatment in **ADR-0069**, which **files ADR-0061 Open Question #2** (the reservation model ADR-0061 deferred until "the first work-order-queue feature that needs allocated-not-consumed" — the DEAL saga is exactly that feature).

A reservation is **not** a stock movement: reserved material is still physically on-hand, it is merely un-promisable to another quote. So reservations get their own append-only ledger `stock_reservations`, parallel to `stock_movements`, and **available-to-promise (ATP) = `stock_qty` − SUM(open reservations)**.

The four states the brief requires, as a reservation lifecycle:

| State | Meaning | Physical stock | ATP |
|---|---|---|---|
| `on_hand` | no reservation; freely promisable | present | counts toward ATP |
| `reserved` | DEAL fired a reservation against this material | present (untouched) | **decremented** |
| `committed` | WO released against the reservation; physical pull pending | present (locked) | decremented |
| `consumed` | `BomConsumption` stock_movement written; reservation closed | decremented | n/a (gone) |

`reserved → committed → consumed` is the lifecycle of one reservation row. In the single-operator shop `committed` may collapse into `consumed` near-instantly (WO Release both locks and pulls), but the state is modeled so a future multi-step pick flow has a home. Reservation states are app-layer; the ledger is append-only (reserve-row, then a close-row referencing it), mirroring ADR-0061's ledger discipline. `mes.stock_reserved` on reserve, `mes.stock_reservation_consumed` on close.

This makes the existing ADR-0062 WO-Release path **reservation-aware**: Release converts the open reservation to a `BomConsumption` movement rather than consuming blind, so a deal's materials can't be silently eaten by another deal between DEAL and Release.

---

## 9. "Learn from this job" feedback loop

When a Work Order completes, the **actuals** are known: real machine-minutes (from MES adapters / time tracking) and real material mass. The engine's *estimate* for that job is on record (the frozen breakdown). The delta is signal — but it is **never auto-applied** (trust-code-not-operator: code computes the suggestion, the operator confirms it).

**The math (deliberately boring — no ML in v1):**

- Per `(material_grade, feature_type, size_bucket)` tuple, keep a rolling window of the **last N=10** `actual / estimated` ratios.
- The suggested multiplier adjustment is the **trimmed mean**: drop the single highest and single lowest ratio (10 → 8), average the rest. Trimming kills the one-off catastrophe job (snapped tool, scrapped part) that would otherwise drag the mean.
- The suggestion surfaces in a "Tunables review" SPA surface: *"`base_time_minutes` for `drill / small` has run 1.18× your estimate over the last 10 jobs — bump from 4.0 to 4.7?"* — with the 10 underlying jobs linked.
- The operator clicks confirm (single token) → the `quoting_complexity_rules` row updates, with a **per-row history entry** (§ pushback I keeps params in DB precisely so this history exists), and `quote.parameters_learned` is audited with `{rule, before, after, n_samples, trimmed_mean}`.
- Nothing changes until the operator confirms. A suggestion the operator ignores stays a suggestion; it does not decay or auto-apply.

This is folded into the design doc rather than given its own ADR (ruthless scope — the math is ten lines and needs no architectural commitment beyond "DB-backed params with history," which ADR-0066 already pins).

---

## 10. Quote-stock validity and freeze semantics

**`valid_until` is bounded by stock projection (§ pushback G).** An indicative quote's expiry is `min(default_validity_days, projected_stock_horizon)`. If the BOM relies on a material whose `stock_status` is `low` or `on_order`, the validity shortens and the PDF carries a **stale-stock banner** ("priced against current stock; lead time may apply if not accepted by `DATE`"). This stops a customer accepting a 30-day-old quote on a material that sold out on day 2 and expecting the original price.

**The accepted snapshot is frozen (§ pushback F).** Once a customer accepts, the `quotes.calculated_breakdown_json` is immutable. Catalogue changes after acceptance (material cost up, machine rate up) **do not** re-price the accepted quote. If Áben needs a different price, the operator **re-issues a fresh indicative** (new quote_id) and the customer re-accepts — the old accepted quote is cancelled with an audit trail, never silently mutated. The frozen `feature_graph_hash` + `calculated_breakdown_json` together are the contract.

**Mid-acceptance regeneration race (§ pushback N).** If the operator regenerates a quote (catalogue changed, wants a re-quote) at the same moment the customer clicks accept on the old PDF, the two must not interleave into a corrupt state. Resolution: **regeneration always mints a new quote_id**; it never mutates an existing row. The accept click is an atomic state flip on a specific quote_id. So the customer's accept lands cleanly on the old quote (which the operator can then cancel and supersede), and the regenerated quote is a separate `indicative` row. There is no in-place edit that an accept could race against — the immutability of priced rows is the concurrency model.

---

## 11. DuckDB-backed tunable tables

All eight tuning tables live in DuckDB (the current engine; invariants in app per ADR-0019 — no CHECK on derived/business columns, closed-vocab enums validated in Rust). **Parameters are DB, not TOML (§ pushback I)** — reversing the brief's original instinct — precisely so the learn-loop (§9) can write per-row history and the operator gets a CRUD UI. A TOML blob has no row history and no CRUD ergonomics.

| Table | PK | Key columns | Notes |
|---|---|---|---|
| `quoting_materials` | `grade` | density_g_cm3, cost_per_kg, machinability_index, carbide_life_multiplier, stock_status (enum), lead_time_default_days, multiplier, notes | the storefront material dropdown is this table, pushed out (§14-C) |
| `quoting_complexity_rules` | (feature_type, size_bucket, count_min, count_max) | base_time_minutes, multiplier, setup_penalty_minutes | the learn-loop's write target |
| `quoting_tolerance_multipliers` | `tolerance_range` | multiplier | tight tolerance → higher multiplier |
| `quoting_machines` | `machine_type` | kinematics_class, hourly_rate, setup_hourly_rate, max_envelope, spindle_hp, default_calendar | **pricing only** — no live calendar (§7) |
| `quoting_parameters` | `key` (single-row-per-key) | scrap_factor, profit_margin_base, overhead_factor, setup_amortization_threshold, min_margin, exotic_material_tax, max_auto_po_eur, auto_po_daily_cap_eur | global knobs incl. the PO gate (§6) |
| `quoting_stock_adjustments` | (grade, stock_status) | price_delta_pct | ±% by stock state |
| `quoting_margin_profiles` | `profile_id` | name (regular/new/strategic/cost_plus), margin_override, notes | FK'd from `partners.quoting_margin_profile_id` (§ pushback K) |
| `quotes` | `quote_id` (`qte_` ULID) | tenant_id, partner_id, state, feature_graph_hash, cad_blob_ref, valid_until, dealed_by, calculated_breakdown_json, converted_wo_id, created_at | the regulated-adjacent record |

Every tuning-table CRUD write emits an audit entry (`quote.material_catalogue_changed`, `quote.machine_catalogue_changed`, `quote.defaults_changed`) and the SPA renders per-row history from the audit ledger — same pattern as the seller.toml-write audit posture.

**Margin profile (§ pushback K).** `partners` gains `quoting_margin_profile_id`. New partners default to the `new` profile (conservative, higher margin) until the operator promotes them to `regular`/`strategic`. `cost_plus` is the floor-protection profile (price = cost × fixed markup, ignoring `profit_margin_base`). The engine reads the partner's profile from the catalogue snapshot — it is part of what `feature_graph_hash` does **not** cover (a strategic-customer re-quote of identical geometry legitimately prices differently), so the breakdown records the `margin_profile_id` it used.

---

## 12. CAD blob handling, hashing, encryption

- **Blob storage.** The pulled CAD blob is encrypted at rest (AES-GCM, key in the OS keychain per ADR-0007) and written content-addressed by `BLAKE3(plaintext)` into the per-tenant blob dir (ADR-0014 dedup model). Every later read decrypts and **audits the access** (defensive — protects Áben if a customer claims IP leak; ADR-0014 confidentiality).
- **Two hashes, two jobs (§ pushback H).** The blob's content-address (storage/dedup) is distinct from `feature_graph_hash` (pricing idempotency). The quote keys on `feature_graph_hash = BLAKE3(canonical(FeatureGraph))` — the *extracted geometry*, not the raw bytes — so the same part exported from two CAD versions (different bytes, identical geometry) hits the same quote, and a trivial re-export does not force a re-price. Canonicalization: sorted keys, floats rounded to the extraction tolerance, before hashing.
- **HMAC accept link (§ pushback M).** The accept link carries `HMAC(quote_id ‖ expiry, secret)` with a 30-day expiry. No customer portal in v1 — the link *is* the acceptance surface. The secret is keychain-held; a click audits `quote.accepted`. An expired/forged token is refused loud and the customer is shown a "request a fresh quote" path.

---

## 13. SPA surface

- **Settings → Quoting catalogue** — CRUD for the eight tuning tables, with per-row history (from the audit ledger). Dark-theme tokens from day one (the SPA dark-theme default gotcha).
- **Quotes tab** — extends the existing Outgoing/Incoming/Quotes segmented control (S179/S211). Auto-quotes appear with state chips (`indicative`/`accepted`/`binding`/`converted`/`cancelled`); `accepted` rows carry the DEAL token affordance + the override-with-reason form.
- **Tunables review** — the learn-loop (§9) suggestions, each with its 10 linked jobs and a confirm token.
- **DEAL gate** — the single-token confirm; if a vendor PO breaches threshold, the in-saga approve/decline gate.
- **Stale-stock banner** on indicative PDFs and the quote detail.

---

## 14. Pushbacks against the brief

The brief's 15 sober-pushback additions, adjudicated. **Adopted** = right, taken verbatim. **Corrected** = premise didn't hold; better design substituted.

| # | Brief's addition | Verdict | Where |
|---|---|---|---|
| 1 | DEAL saga: exact sequence, rollback, audit chain | **Adopted** | §5, ADR-0067 |
| 2 | Vendor-PO threshold: per-PO + daily cap, operator gate above | **Adopted** | §6, ADR-0068 |
| 3 | Machine reservation: engine *asks* board, never writes calendar | **Corrected (B)** | §7 — no such board exists; ADR-0064 is *shipping* dispatch. Principle kept as forward constraint; `MachineSlotReserved` specced-not-emitted |
| 4 | Material reservation states on_hand/reserved/committed/consumed | **Adopted** | §8, ADR-0069 (files ADR-0061 OQ#2) |
| 5 | Learn-loop: trimmed-mean N=10, operator confirms, never auto-apply | **Adopted** | §9 |
| 6 | Accepted-pre-DEAL quote cannot be silently re-priced | **Adopted** | §10 |
| 7 | `valid_until` bounded by stock projection; stale-stock banner | **Adopted** | §10 |
| 8 | `feature_graph_hash` not `cad_hash` | **Adopted** | §12 |
| 9 | Parameters table is DB not TOML; CRUD + per-row history | **Adopted** | §11 |
| 10 | CAD-blob encryption at rest; audit on every read | **Adopted** | §12 |
| 11 | `quoting_margin_profile_id` FK on partner | **Adopted** | §11 |
| 12 | Per-quote override allowed but logged with mandatory reason | **Adopted** | §4 step 4, §13; `quote.operator_adjusted` carries mandatory `reason` |
| 13 | Email-link acceptance, HMAC, 30-day expiry, no portal v1 | **Adopted** | §12 |
| 14 | Regeneration mid-acceptance: accept snapshots atomically; regen → new quote_id | **Adopted** | §10 |
| 15 | DEAL email cadence: one client email post-DEAL, ops gets play-by-play | **Adopted** | one customer email on `deal.completed`; `deal.*` audit entries are the internal play-by-play |

**Additional corrections the brief did not anticipate:**

- **(J) `converted_to_so` → `converted` (to a Work Order).** No Sales Order module exists (ADR-0015 stub). v1 converts to a WO. §4.
- **(C) "ABERP-side material dropdown HTTP endpoint" → storefront-hosted, ABERP-pushed.** ABERP has no public inbound surface (ADR-0057). The dropdown endpoint lives on the **storefront**, fed by an ABERP catalogue **push** (`PUT /api/catalogue/materials`) on each poll cycle. The customer's browser reads the storefront's cache, never ABERP. §2 diagram, §11.
- **(D) Quote engine runs in ABERP, not on the storefront.** The e2e-shop ground-zero §8 sketched the storefront fetching material/stock from ABERP to price locally. This doc reverses that: the **CAD comes to ABERP**, which owns the catalogue, margin profiles, and reservations, and computes there. The storefront stays a thin CAD-collect + status + accept surface. Cleaner — pricing logic and the data it needs live in one place. §1, §2.

---

## 15. Next 10 sessions

The brief's S266–S275 sequence, refined. One refinement (flagged): **S271 and S272 swap is not made** — the brief's order is sound; the only adjustment is making explicit that the saga skeleton (S273) lands the *paused-PO-gate hole* even though the PO itself is post-S275.

| Session | Deliverable | Builds on |
|---|---|---|
| **S266** | `quoting_materials` table + CRUD SPA + the **storefront** material-dropdown push (`PUT /api/catalogue/materials` writeback during poll). `quote.material_catalogue_changed` audit. | §11, §14-C |
| **S267** | `quoting_complexity_rules` + `quoting_tolerance_multipliers` + `quoting_parameters` + `quoting_stock_adjustments` + `quoting_margin_profiles` tables + CRUD + `partners.quoting_margin_profile_id`. `quote.defaults_changed`/`quote.machine_catalogue_changed`. | §11 |
| **S268** | `crates/aberp-quote-engine` skeleton — the pure function `(FeatureGraph, snapshot, params, margin_profile) → QuoteBreakdown`, with property pins (idempotent, deterministic). No I/O. | §2, ADR-0066 |
| **S269** | `python/aberp-cad-extract` skeleton — build123d/OCP pick (ADR-0066), first extractors: bounding box, volume, hole count. Emits the pinned FeatureGraph JSON schema. | §2, ADR-0066 |
| **S270** | `crates/aberp-cad-extract-wrapper` — subprocess spawn + timeout + sandbox + JSON-schema validation + `extractor_version` stamping. | §2, ADR-0066 |
| **S271** | `quotes` table + state machine (§4) + indicative-PDF generator (reuse invoice-pdf primitives) + `valid_until` stock-bounding + stale-stock banner. `quote.auto_estimated`. | §4, §10 |
| **S272** | HMAC accept link (§12) + storefront accept writeback + `accepted` freeze + `accepted→DEAL` operator UI + per-quote override-with-reason. `quote.accepted`/`quote.operator_adjusted`. | §10, §12 |
| **S273** | DEAL saga **skeleton** — atomic cascade: WO creation + `converted_wo_id` link + audit (`deal.started`/`completed`/`rolled_back`). **No** reservation, **no** vendor PO yet — but the saga is shaped with the reservation-call and PO-gate seams as typed holes. | §5, ADR-0067 |
| **S274** | `stock_reservations` ledger + ATP + reservation states (§8); wire the DEAL saga's reservation seam; make ADR-0062 WO-Release reservation-aware. `mes.stock_reserved`/`mes.stock_reservation_consumed`. Files ADR-0061 OQ#2. | §8, ADR-0069 |
| **S275** | Adversarial review of S266–S274 + sweep PR. (Vendor-PO auto-fire / `vendor_pos` / `po.*` remains post-S275, per §6 scope honesty — ADR-0068 is the standing spec.) | all |

**Why this order holds:** tables before engine (the engine reads a catalogue snapshot — S266/S267 first); engine before extractor (the engine's input contract, the FeatureGraph schema, is defined by what the engine consumes — S268 pins it, S269 produces it); extractor before wrapper (S270 wraps a thing that exists); `quotes`+PDF before acceptance (S271 before S272); acceptance before DEAL (a quote must be acceptable before it can be DEAL'd — S272 before S273); skeleton saga before reservation (S273's atomic shell before S274 fills the reservation seam) — this lets S273 ship and be reviewed as a pure orchestration unit before reservation's inventory-coupling complexity lands.

---

## Appendix — Audit EventKind plan

New kinds for the F12 four-edit ritual (variant + `as_str` + `from_storage_str` + variants-array), in `crates/audit-ledger/src/entry/event_kind.rs`. **Three new prefix families** (`quote.*`, `deal.*`, `po.*`) + two additions to the existing inventory `mes.*` family.

| EventKind | Storage string | Emitted | Family |
|---|---|---|---|
| `QuoteAutoEstimated` | `quote.auto_estimated` | engine output | quote |
| `QuoteOperatorAdjusted` | `quote.operator_adjusted` | override (mandatory `reason`) | quote |
| `QuoteAccepted` | `quote.accepted` | HMAC click | quote |
| `QuoteCancelled` | `quote.cancelled` | abandon/expiry | quote |
| `QuoteParametersLearned` | `quote.parameters_learned` | operator-confirmed learn-loop | quote |
| `QuoteDefaultsChanged` | `quote.defaults_changed` | params CRUD | quote |
| `MaterialCatalogueChanged` | `quote.material_catalogue_changed` | materials CRUD | quote |
| `MachineCatalogueChanged` | `quote.machine_catalogue_changed` | machines CRUD | quote |
| `DealStarted` | `deal.started` | saga open | deal |
| `DealCompleted` | `deal.completed` | saga success (carries child ids) | deal |
| `DealRolledBack` | `deal.rolled_back` | saga failure | deal |
| `StockReserved` | `mes.stock_reserved` | reservation taken | mes (inventory) |
| `StockReservationConsumed` | `mes.stock_reservation_consumed` | reservation closed at WO-Release | mes (inventory) |
| `VendorPoFired` | `po.vendor_po_fired` | auto-PO under threshold | po |
| `AutoPoThresholdExceeded` | `po.auto_threshold_exceeded` | operator gate | po |
| `MachineSlotReserved` | `mes.machine_slot_reserved` | **specced, NOT emitted v1** (§7) | mes (future) |
| `MaterialCatalogueChanged` | `quote.material_catalogue_changed` | materials CRUD (S266) | quote |
| `MaterialCataloguePushed` | `quote.material_catalogue_pushed` | storefront push attempt (S266) | quote |

---

## Appendix B — Storefront catalogue-push contract (S266 / PR-255)

The implementation of §11 `quoting_materials` + §14-C lands in **S266 / PR-255**. The ABERP side is built; the **storefront side** (`abenerp.com`) is a separate ABERP-site PR. The wire contract ABERP emits:

**`PUT {storefront_base_url}/api/catalogue/materials`** (the `storefront_base_url` is the quote-intake `base_url` — same surface, SPOC).

- **Auth:** `Authorization: Bearer <token>` — the **quote-intake bearer** (`ABERP_QUOTE_INTAKE_TOKEN` / keychain `quote_intake_token`). The brief named `ABERP_SITE_ADMIN_TOKEN`; no such var exists — the storefront surface's secret is the quote-intake bearer, reused per `[[aberp-smtp-spoc]]` (one secret per surface). Catalogue-push is therefore active **iff** quote-intake is configured.
- **Body:** `{ "materials": [ { "grade", "display_name", "stock_status", "lead_time_default_days" }, … ] }` — the **public projection only**. Cost, multipliers, density, and machining factors are NEVER pushed.
- **Cadence:** every 15 min (`PUSH_CADENCE_SECS`) **plus** an immediate attempt on each operator CRUD write (on-write trigger).
- **Outcomes:** `2xx` = ok; `401` = pause the daemon + surface a "re-paste bearer" banner in Settings (resumes on next `aberp serve` boot); other non-2xx / transport error = exponential backoff `5s → 15s → 60s → cadence` (mirrors the S256 quote-intake daemon).
- **Idempotency:** the PUT is a full-snapshot replace — the storefront should treat each PUT as the complete active catalogue (a grade absent from the body has been deleted).
- **Audit:** each attempt emits `quote.material_catalogue_pushed` (`{ trigger, outcome, pushed_count, detail }`).

**Storefront responsibilities (out of scope for PR-255):** accept the PUT, validate the bearer, cache the body, and serve the `/quote` material dropdown from that cache. The customer's browser reads the storefront cache, never ABERP.
