# S274 — Adversarial pre-storefront review of the auto-quoting arc (S266–S273)

**Scope.** Eight implementation cuts shipped between PROD_v2.18.0 and PROD_v2.25.0 — the ABERP-side of the auto-quoting strand. Storefront-side is the abenerp.com SvelteKit repo and ships separately. This review's job is to stress every seam *before* the storefront producer turns on the firehose.

**Reviewer posture.** Per [[pushback-as-method]] — adversarial, not soft-peddled. Where something is right, named; where something is brittle, named louder.

**Baseline gates (re-verified at `cf3dc6b`).**

| Gate | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` (with `ABERP_TEST_PYTHON=…/.venv/bin/python`) | 1906 passed / 0 failed |
| `apps/aberp-ui/ui` npm build | green (requires `npm install && npm run build` for the venv-less fresh worktree — see [[aberp-ui-clippy-needs-dist]]) |

## PRs in scope

| PR | SHA | Surface |
|---|---|---|
| S266 / PR-255 | `c0af501` | `quoting_materials` catalogue + Material Catalogue SPA + storefront catalogue-push |
| S267 / PR-256 | `c644206` | 4 tunable tables (complexity / tolerance / parameters / stock adjustments) + CRUD SPA |
| S268 / PR-257 | `678b46f` | `aberp-quote-engine` pure-Rust scoring crate |
| S269 / PR-258 | `e1b2635` | `python/aberp-cad-extract` STL → FeatureGraph JSON |
| S270 / PR-259 | `5ffacb9` | `aberp-cad-extract-wrapper` Rust subprocess shim |
| S271 / PR-260 | `812d611` | `quote_intake_log` S271 columns + sticky `stock_alert` recompute |
| S272 / PR-261 | `7e1dae2` | DEAL saga + EVE-addenda 2 UI + addendum 3 |
| S273 / PR-262 | `cf3dc6b` | Material reservation extension + Inventory Balances SPA |

## Executive summary

**Total findings: 24** (🔴 4, 🟡 13, 🟢 7).

Most of what landed is *honest*: the crates re-export shared types instead of duplicating them, the saga keeps a single transaction across SO/WO/material/audit, the F12 ritual is complete for all 12 new EventKinds, and the SPA dark-theme compliance is near-perfect. The DEAL token + REFRESH gate are exactly as EVE specced.

**Top 3 🔴s for S275:**

1. **F1 — `qty` semantic mismatch** (quote units interpreted as kg of material): the saga commits N "kg" when the customer actually ordered N parts. Banner-in-the-SPA is the only safeguard — pure [[trust-code-not-operator]] violation. Fix path is documented (`units → mm³ → kg`) but until then every DEAL silently corrupts inventory.
2. **F2 — `QuoteStockAlertTriggered` audit emit is OUT of the flip transaction**: `flip_stock_alert_to_true` commits, then the route opens a separate ledger handle to append. A failure between the two leaves a sticky-true row with no audit explaining why. Violates "every state change has an audit" invariant.
3. **F3 — Operator-facing banner on Inventory Balances says "qty is QUOTE units (NOT kg)"** AND **the QuotesList banner still names a "future S272" gate that already shipped**: both are operator-discipline anti-patterns — the first hides a correctness gap behind a sentence the operator must read; the second is stale copy that confuses anyone touching the surface.

Storefront PR will need to know: every NULL `material_grade` / `quantity` row dead-letters the material commit *silently*; every TIMESTAMP column needs `CAST(... AS VARCHAR)` to read; the catalogue is single-tenant (PK = `grade`); ABERP recomputes `stock_alert` on every list call, not on schedule.

---

## 1. End-to-end pipeline coherence

The narrative path: storefront customer fills CAD form → `aberp-quote-intake` daemon polls + stages a row → `list_quote_intake_rows` recomputes `stock_alert` on every read → operator clicks DEAL → `quote_deal::run_deal_saga` mints SO/WO/material/audit in one tx. Every PR seam was traced; here is what holds and what doesn't.

### F4 🟢 — Single-tx DEAL saga is correctly atomic across SO/WO/material

`run_deal_saga` opens ONE `conn.transaction()` ([apps/aberp/src/quote_deal.rs:339](apps/aberp/src/quote_deal.rs#L339)), CAS-marks `deal_issued_at`, emits 3 quote.* audit entries via `append_in_tx`, then conditionally calls `commit_material_in_tx` which UPSERTs the balance row, increments `committed_qty`, INSERTs the reservation row, and re-reads + invariant-checks the post-state. The fourth audit entry rides the same tx via `append_material_committed_in_tx`. `tx.commit()` is the single fence; any failure between line 339 and line 511 rolls back everything.

Pinned by [`s273_saga_insufficient_material_rolls_back_everything`](apps/aberp/src/quote_deal.rs#L1086) (zero audit entries, `deal_issued_at` stays NULL, on_hand preserved at pre-saga value).

**No fix needed.** This is the single most important property of the saga and it is right.

### F1 🔴 — `qty` is silently interpreted as kg of material instead of being converted from units

[apps/aberp/src/quote_deal.rs:447-509](apps/aberp/src/quote_deal.rs#L447) reads `row.quantity: Option<i64>` (the S271 storefront-pushed "number of parts") and hands it as `qty: f64` straight to `commit_material_in_tx`, which adds it to `inventory_balances.committed_qty` (UoM hardcoded to `"kg"` via `DEFAULT_UOM` in [material_inventory.rs:114](apps/aberp/src/material_inventory.rs#L114)).

The module's own header docstring at [material_inventory.rs:82-92](apps/aberp/src/material_inventory.rs#L82) admits the bug:

> **`qty` is QUOTE quantity, not material volume.** A quote for 12 units is stored as `qty = 12`. The real conversion is `units → mm³ (per-part) → kg (× density)` … The plumbing is S275+ — until then the units placeholder lets the DEAL saga book a reservation against the material-grade balance, even though the numbers are NOT in physical units. The SPA view's header banner names this explicitly so operators don't read the column as "kg on the shelf."

Then the SPA banner at [InventoryBalancesList.svelte:92-100](apps/aberp-ui/ui/src/routes/InventoryBalancesList.svelte#L92) says:

> `qty` is QUOTE units (NOT kg). The units → mm³ → kg conversion waits on the CAD-extract pipeline (S275+).

This is the textbook [[trust-code-not-operator]] anti-pattern — a settings page that ALLOWS unsafe configurations, with the safety property delegated to the operator's reading comprehension. Worse, the audit payload at [material_inventory.rs:553-558](apps/aberp/src/material_inventory.rs#L553) writes `balance_after_committed: 12.0` with no UoM stamp — a forensic walk N months from now reads "12 kg committed" and is wrong.

**Recommended fix for S275:** the saga should refuse to commit when the conversion is not available, OR commit `0.0` with a `pending_volume_conversion = true` flag, OR add a `qty_unit_kind: 'parts' | 'kg'` column so the SPA cannot mis-render. Banner-and-pray is wrong-shaped.

**Target session:** S275 (sweep) — the simplest defensive landing is renaming the SPA's `Committed` column to `Committed (qty)` and stamping `qty_unit_kind` on the reservation row + audit payload, even before the full conversion lands. The full conversion is engine-strand work and likely a separate session.

### F5 🟡 — `material_grade` populated but `quantity` NULL silently skips the material commit

[apps/aberp/src/quote_deal.rs:448-509](apps/aberp/src/quote_deal.rs#L448) gates the material branch on `(Some(grade), Some(qty)) if !grade.is_empty() && qty > 0`. A storefront writer that pushes `material_grade = "6061-T6"` but leaves `quantity = NULL` falls through to `None` — the DEAL succeeds, `material_commit: None` rides back to the SPA, no audit entry, no error toast, no log line.

The pin [`s273_saga_skips_material_commit_when_material_grade_empty_string`](apps/aberp/src/quote_deal.rs#L971) deliberately codifies this as the desired behaviour. The brief's intent (graceful fallback for pre-storefront rows) is sound; the problem is that the operator's mental model says "DEAL committed material" while the inventory side received nothing.

**Recommended fix:** emit a distinct audit entry on the silent-skip path (`inventory.material_commit_skipped` with the reason field), OR surface a yellow `material_commit: null` chip in the post-DEAL SPA toast. The current `Option<MaterialCommitOutcome>` round-trips correctly through `serde(default)`; the SPA just doesn't render it.

**Target session:** S275 sweep — add the SPA chip; defer the new EventKind variant to whenever the storefront grows a "missing fields" telemetry need.

### F6 🟡 — Recompute pass runs on dealt + irrelevant rows; emits noise into the audit ledger

[apps/aberp/src/quote_intake_query.rs:139-242](apps/aberp/src/quote_intake_query.rs#L139) iterates all 500 most-recent rows for the tenant, including ones with `deal_issued_at IS NOT NULL` and `intake_state IN ('error', 'irrelevant')`. For each, it recomputes `stock_alert`. A post-DEAL row whose material catalogue downgrades AFTER the saga commits will flip `stock_alert = TRUE` and emit a `QuoteStockAlertTriggered` audit entry — meaningless, because the material is already committed and the REFRESH gate no longer applies.

The recompute path has no semantic role on a dealt or irrelevant row.

**Recommended fix:** narrow the recompute to `intake_state = 'staged' AND deal_issued_at IS NULL AND picked_up_drf_id IS NULL` — or at the very least, skip the audit emit when `deal_issued_at IS NOT NULL`.

**Target session:** S275 sweep.

### F7 🟢 — STEP path stub correctly raises `NotImplementedError`

[python/aberp-cad-extract/aberp_cad_extract/extractors/step.py:31](python/aberp-cad-extract/aberp_cad_extract/extractors/step.py#L31) raises with a clear "supply STL" message; the CLI translates it to a non-zero exit; the wrapper surfaces `ExtractError::NonZeroExit { stderr }`. The stderr carries the message verbatim. Not a typed variant, but the operator's eye gets the explanation.

## 2. EVE addenda enforcement

### F8 🟢 — Addendum 1 (`requires_5_axis` + `thin_wall_present`) is schema-locked across Python and Rust

Python: [feature_graph.py:82-83](python/aberp-cad-extract/aberp_cad_extract/feature_graph.py#L82) declares both as plain `bool` (no `Optional`, no default). Pydantic-2 `model_config` is `extra="forbid"`.

Rust: [crates/aberp-quote-engine/src/feature_graph.rs:207-211](crates/aberp-quote-engine/src/feature_graph.rs#L207) declares both as plain `pub … bool` (no `#[serde(default)]`).

Cross-language pin: `crates/aberp-quote-engine/tests/feature_graph_compat.rs` includes `python_fixture_missing_addendum_1_boolean_fails_deserialize` × 2.

Tried to construct a graph without `requires_5_axis`: Pydantic raises `ValidationError`, Rust raises `serde::de::Error::missing_field("requires_5_axis")`. Both fail loud.

### F9 🟢 — Addendum 2 sticky semantics correct in both directions

[`recompute_stock_alert`](apps/aberp/src/quote_stock_alert.rs#L71) returns `true` as soon as `stored_alert == true`, regardless of the snapshot/current comparison. The flip is persisted via `flip_stock_alert_to_true` ([crates/aberp-quote-intake/src/log_table.rs:358](crates/aberp-quote-intake/src/log_table.rs#L358)), which is sticky in the DB.

Tried the bypass: catalogue recovery does NOT untrigger ([`sticky_alert_survives_recovery_in_both_directions`](apps/aberp/src/quote_stock_alert.rs#L208)); the only path back is operator REFRESH ack.

### F2 🔴 — `QuoteStockAlertTriggered` audit emit lives OUTSIDE the flip transaction

The recompute path in [apps/aberp/src/quote_intake_query.rs:227](apps/aberp/src/quote_intake_query.rs#L227) calls `flip_stock_alert_to_true(conn, …)` which uses a plain `conn.execute` UPDATE — committed immediately. The route handler in [apps/aberp/src/serve.rs:15655-15683](apps/aberp/src/serve.rs#L15655) then opens a SEPARATE ledger handle (`Ledger::open(&db_path, …)`) and appends `QuoteStockAlertTriggered` per `newly_triggered_alerts`.

A connection drop, panic, or non-clean exit between the flip's commit and the ledger append leaves the row in `stock_alert = TRUE` with no audit entry explaining the transition. Subsequent recompute passes see `stored_alert = TRUE`, return early without re-emitting (sticky). The audit is permanently missing.

This violates the "every state change is audited" property the brief calls out in checklist item 3.

**Recommended fix:** thread the audit append into the same tx as `flip_stock_alert_to_true` (extract a `flip_and_audit_in_tx(conn, …, ledger_meta, actor)` helper). The DEAL saga does this correctly — same pattern is the model.

**Target session:** S275 sweep.

### F10 🟡 — `flip_stock_alert_to_true` is a read-then-write (TOCTOU on parallel recompute)

[crates/aberp-quote-intake/src/log_table.rs:358-391](crates/aberp-quote-intake/src/log_table.rs#L358): `SELECT stock_alert` → branch → `UPDATE stock_alert = TRUE`. Two concurrent SPA reloads could both SELECT `false`, both UPDATE, both return `true`, both emit one audit entry — double audit for one transition.

DuckDB's MVCC may serialize the writes and surface a conflict on the loser, in which case the loser gets a `Storage(…)` error and the SPA renders a 500. Either way, the comment in [quote_intake_query.rs:223-226](apps/aberp/src/quote_intake_query.rs#L223) ("a parallel SPA reload between recompute and persist resolves to one audit emit per row") is *confident*, not *proven*.

**Recommended fix:** guarded UPDATE — `UPDATE quote_intake_log SET stock_alert = TRUE WHERE quote_id = ?1 AND tenant_id = ?2 AND (stock_alert IS NULL OR stock_alert = FALSE)` and use `rows_affected == 1` as the transition signal. No SELECT needed.

**Target session:** S275 sweep; pairs naturally with F2.

### F11 🟢 — Addendum 3 single-use CAS is genuinely atomic

[crates/aberp-quote-intake/src/log_table.rs:732-761](crates/aberp-quote-intake/src/log_table.rs#L732) does `UPDATE … WHERE deal_issued_at IS NULL` inside a `Transaction<'_>`. `rows_affected != 1` → saga rolls back. The replay returns 409 `deal_already_issued` reliably.

Tried a concurrent-DEAL bypass: pre-flight read at [quote_deal.rs:300-304](apps/aberp/src/quote_deal.rs#L300) is the fast path; the CAS at [quote_deal.rs:346-364](apps/aberp/src/quote_deal.rs#L346) is the source of truth. Two parallel POSTs cannot both succeed.

### F12 🟢 — Replay precedence over stock_alert is pinned

[`replay_precedence_over_stock_alert_check`](apps/aberp/src/quote_deal.rs#L810) asserts that a replay on an already-dealt row returns `deal_already_issued` even when `stock_alert` was flipped TRUE after the first DEAL. The SPA's 409 toast routing stays consistent.

### F13 🟡 — DEAL bypass via direct POST is guarded but UX is operator-unfriendly

[serve.rs:16219](apps/aberp/src/serve.rs#L16219) `handle_deal_saga` runs the same `run_deal_saga` the SPA invokes; bearer check + ready check + downcast to `DealSagaError` are all in place. A scripted POST that supplies `deal_token = "WRONG"` gets 409 `deal_token_mismatch`. A POST with `refresh_ack = "refresh"` (lower-case) gets 409 `stock_alert_refresh_required` — case-sensitivity preserved through the wire.

What is missing: the route DOES NOT rate-limit a brute-force first-8-chars guesser. Storefront-side quote ids are UUIDs (32 hex chars + dashes); a brute on the first 8 chars is a 16⁸ ≈ 4.3B keyspace — infeasible without scripting. But the route returns a quick 409 per attempt; nothing slows it down.

**Recommended fix:** none required for prod_v2.x — the bearer scope is per-session-token. Worth tracking in a follow-up if SaaS migration changes the auth model.

### F14 🟢 — Catalogue purge does not untrigger the alert

`recompute_stock_alert` with `current_status = None` (material removed from catalogue) sticks the alert on a previously-true row ([`sticky_alert_survives_recovery_in_both_directions`](apps/aberp/src/quote_stock_alert.rs#L208)). On a previously-false row it returns false (conservative no-op per the comment at [quote_stock_alert.rs:84-86](apps/aberp/src/quote_stock_alert.rs#L84)).

## 3. Audit completeness

### F15 🟢 — F12 ritual complete for all 12 new EventKinds

Spot-checked: `MaterialCatalogueChanged`, `QuoteDealIssued`, `MaterialCommitted`, `QuoteStockAlertTriggered`, `MaterialReleased` all hit:

- Variant declaration in [crates/audit-ledger/src/entry/event_kind.rs](crates/audit-ledger/src/entry/event_kind.rs)
- `as_str()` arm
- `from_storage_str` arm
- `ALL_VARIANTS` round-trip array (L1672-1696)
- Distinctness pin (`s273_material_state_kinds_are_distinct`, L3105)
- Inventory-prefix pin (`s273_material_state_kinds_use_inventory_prefix`, L3075)
- Classifier arms in `crates/aberp-verify/src/verify.rs` (L912-930) and `apps/aberp/src/export_invoice_bundle.rs` (L797-806)

### F16 🟡 — `QuoteStockAlertTriggered` audit append uses `Ledger::open` with `idempotency_key = None`

[serve.rs:15680](apps/aberp/src/serve.rs#L15680) appends with `None` for the idempotency key; the per-call ULID is embedded in the payload but not the ledger's dedupe column. A buggy retry (e.g. spurious page reload from the SPA on a transient 5xx) could emit two entries for the same (quote, transition) pair. Mostly noise — `stock_alert` is sticky so the second flip is a no-op and won't emit — but the comment claims "exactly one audit entry per row that newly transitions to TRUE" and that's only true *if* the first append always succeeds.

**Pairs with F2** — same fix lifts both.

### F17 🟡 — `MaterialCatalogueChanged` audit emits but `quoting_materials.grade` is single-tenant PK

[apps/aberp/src/quoting_materials.rs:42-56](apps/aberp/src/quoting_materials.rs#L42): `grade VARCHAR NOT NULL PRIMARY KEY`. Multi-tenant queries are filtered via `WHERE tenant_id = ?`, but two tenants in the same DB cannot both have `6061-T6`. Compare with `inventory_balances` ([apps/aberp/src/material_inventory.rs:202-203](apps/aberp/src/material_inventory.rs#L202)) which correctly uses `PRIMARY KEY (tenant_id, material_grade)`.

This is fine for ABERP's single-tenant prod today, but the SaaS migration will trip on it. The S266 cut report flagged the PK choice as deliberate (no surrogate `material_id`) but didn't address multi-tenancy. The audit `MaterialCatalogueChanged` payload carries `tenant_id` so the audit walk survives the migration; the table's PK does not.

**Recommended fix:** for S275 sweep, leave the schema alone (changing the PK is a real migration). Track for the SaaS-migration session — it must rebuild the table with `PRIMARY KEY (tenant_id, grade)`.

### F18 🟢 — Inventory.* prefix family is correctly distinct from mes.stock_movement_recorded

Pinned by `s273_material_state_kinds_are_distinct` ([event_kind.rs:3105](crates/audit-ledger/src/entry/event_kind.rs#L3105)) — confirms `inventory.material_committed != mes.stock_movement_recorded`. The two stock-tracking strands stay forensically separable: material-side (raw stock) vs product-side (FG + WIP).

## 4. DuckDB-specific footguns

### F19 🟢 — S271/S272 ALTER TABLE migrations correctly omit DEFAULT

The S271 cut report named the DuckDB-on-replay DEFAULT-clobber trap. S272/S273 inherited the lesson: every `ALTER TABLE … ADD COLUMN IF NOT EXISTS` is bare (no `DEFAULT`). Pinned by [`s271_flip_stock_alert_is_idempotent_and_sticky`](crates/aberp-quote-intake/src/log_table.rs#L1052) — adding a `DEFAULT FALSE` to `stock_alert` would make the second flip return `true` instead of the sticky no-op, breaking the test.

### F20 🟡 — `quoting_materials` CREATE TABLE has `DEFAULT 1.0` columns that future ALTER could trip over

[quoting_materials.rs:42-56](apps/aberp/src/quoting_materials.rs#L42): `machinability_index DOUBLE NOT NULL DEFAULT 1.0`, `carbide_life_multiplier`, `quote_multiplier`. CREATE TABLE defaults are fine. The trap fires only if a future contributor uses `ALTER TABLE … ADD COLUMN … DEFAULT V`.

**Recommended fix:** add a comment-pin at the schema constant warning future contributors. No code change needed today.

### F21 🟡 — Subprocess pipe deadlock risk on >64KB stdout

[crates/aberp-cad-extract-wrapper/src/lib.rs:186-199](crates/aberp-cad-extract-wrapper/src/lib.rs#L186): `child.stdout.take()` is stored, `wait_with_timeout` polls, THEN `read_to_string(stdout)` runs after the child exits. If the Python CLI emits more than the OS pipe buffer (typically 64KB on Darwin / Linux) without anyone draining, the Python write blocks → `try_wait` never sees an exit → the 30s timeout fires → wrapper kills the child → reports `Timeout`.

The comment at [lib.rs:370-372](crates/aberp-cad-extract-wrapper/src/lib.rs#L370) ("the Python CLI writes ≤ a few KB") is the assumption. A complex part with many features could push the FeatureGraph JSON over 64KB; an OCCT crash trace on stderr could easily exceed it.

**Recommended fix:** use `std::process::Command::output()` after timeout (cleaner — but needs a different timeout pattern) OR spawn drain threads. The cleanest is moving to `wait_with_output()` after success, with the timeout enforced via a deadline thread. Worth a follow-up session of its own.

### F22 🟡 — TIMESTAMP columns require `CAST(... AS VARCHAR)` to read

[apps/aberp/src/quote_deal.rs:677-685](apps/aberp/src/quote_deal.rs#L677) and [apps/aberp/src/quote_intake_query.rs:177](apps/aberp/src/quote_intake_query.rs#L177) both cast `deal_issued_at` and `refresh_acked_at` to VARCHAR before reading. This is a DuckDB Rust-binding limitation, not a logic bug — but every future column added as TIMESTAMP needs the same dance.

**Recommended fix:** add a project-level helper `read_timestamp_as_string` to centralize the cast, OR adopt the convention of storing audit-adjacent timestamps as RFC3339 VARCHAR from the start (which `quoting_materials.updated_at` already does per [quoting_materials.rs:54](apps/aberp/src/quoting_materials.rs#L54)). The auto-quoting strand is inconsistent here: S272 added two TIMESTAMP columns where `VARCHAR` would have matched the existing convention.

**Target session:** S275 sweep (decide convention; don't rewrite already-shipped TIMESTAMPS).

### F23 🟢 — `ON CONFLICT (...) DO NOTHING` is portable enough

Used at [material_inventory.rs:451](apps/aberp/src/material_inventory.rs#L451) for the zero-row upsert. Supported by DuckDB, SQLite, and Postgres — not a [[no-sql-specific]] violation.

## 5. Race conditions in DEAL saga

### F24 🟢 — Material commit + DEAL CAS share one tx

Verified by line-walking the saga: `conn.transaction()` at [quote_deal.rs:339](apps/aberp/src/quote_deal.rs#L339), `commit_material_in_tx(&tx, …)` at L451 (uses `&Transaction`, not `&Connection`), `tx.commit()` at L511. Nothing escapes.

### F25 🟡 — Quote intake row deletion between read and CAS is unhandled

The pre-flight `read_for_deal` at [quote_deal.rs:279](apps/aberp/src/quote_deal.rs#L279) is OUTSIDE the tx. If another process deletes the row between the read and the CAS at L346, the CAS UPDATE matches zero rows → saga errors with `DealAlreadyIssued` ("CAS rejected"), not `NotStaged` ("row gone"). The two error variants have distinct machine codes; the wrong one routes to a misleading SPA toast.

This is theoretical — there is no other writer that deletes from `quote_intake_log` in the current codebase. But the row IS deletable via direct SQL or a future operator action; the saga's failure-mode taxonomy should be honest.

**Recommended fix:** read inside the tx for a more honest error path, OR check `intake_state` again post-CAS. Low priority; flag for S275 if there's spare token budget.

### F26 🟢 — `stock_alert` cannot flip TRUE during the saga because the CAS gates the DEAL first

The saga holds the row's tx; another process can read+UPDATE the same row outside the tx and DuckDB MVCC serializes — the flip would be queued behind the saga's CAS. If the flip wins (parallel `list_quote_intake_rows` recompute pass commits first), the saga's pre-flight read still saw the flip OR the row's CAS write fails on the version. Empirically the test suite doesn't pin this case — but DuckDB's snapshot isolation makes the race benign.

## 6. SPA dark theme compliance

### F27 🟡 — `InventoryBalancesList.svelte:281` uses hardcoded `rgba(255, 0, 0, 0.06)` for the breach row

[InventoryBalancesList.svelte:280-282](apps/aberp-ui/ui/src/routes/InventoryBalancesList.svelte#L280): `.ib-table__row--breach { background: rgba(255, 0, 0, 0.06); }`. Every other surface in the file uses `var(--color-signal-negative)` etc. This one bypasses [[spa-dark-theme-default]].

The intent ("subtle red tint") is correct; the implementation should be `background: color-mix(in srgb, var(--color-signal-negative) 6%, transparent);` or a dedicated `--color-surface-breach` token.

**Recommended fix:** replace the hardcoded RGBA with a token-derived value.

**Target session:** S275 sweep.

### F28 🟢 — QuoteDealGate.svelte is exemplary dark-theme + bilingual + non-autofocus

[QuoteDealGate.svelte](apps/aberp-ui/ui/src/routes/QuoteDealGate.svelte) uses ONLY token variables, declares the autofocus-avoidance in a comment ("a tab-key landing on a destructive submit is the kind of accident this gate exists to prevent" — L322-324), and renders HU+EN in parallel. Storno-button-equivalent loud affordance.

### F3 🔴 — QuotesList banner refers to a "future S272" that already shipped

[QuotesList.svelte:282-283](apps/aberp-ui/ui/src/routes/QuotesList.svelte#L282):

> REFRESH kötelező a DEAL előtt — a frissített token-mező S272-ben érkezik.
> / REFRESH required before DEAL — typed-token gate ships in S272/PR-261.

This banner was written by S271 anticipating S272. S272 shipped. The copy is stale and confusing — an operator reading "ships in S272" wonders what S272 means; the DEAL gate is right there on every row.

**Combined with the Inventory Balances banner ([InventoryBalancesList.svelte:92-100](apps/aberp-ui/ui/src/routes/InventoryBalancesList.svelte#L92)) telling operators that `qty` is "QUOTE units (NOT kg)"** — both are operator-discipline anti-patterns that the [[trust-code-not-operator]] / [[hulye-biztos]] rules name explicitly.

**Recommended fix:**
- QuotesList banner: remove the "S272 / PR-261" reference; the gate is shipped, just say "type REFRESH below to acknowledge, then DEAL."
- Inventory Balances banner: this one is harder because the underlying bug (F1) is real. The honest fix is to make the column header carry the unit ("Committed (qty)") and remove the banner once F1 lands properly.

**Target session:** S275 sweep — at least strip the S272 reference from QuotesList; coordinate Inventory Balances banner with F1.

### F29 🟢 — Material Catalogue + Quoting tunables SPAs all use dark tokens

Spot-checked: no hardcoded `#fff` / `#000` / `rgba` outside F27. All borders + backgrounds derived from `--color-surface-*` / `--color-signal-*`.

## 7. Operator safety (`[[trust-code-not-operator]]`)

### F1 🔴 (repeated) — the SPA banner is the only thing keeping the operator from misreading "kg" off the Inventory Balances view

Already covered in §1. The principle is the test: a runbook step is wrong, an "operator should remember" line is wrong, a banner saying "this isn't really kg" is exactly the same shape. The fix path is real (S275+ unit conversion), but until then *code* needs to not lie about the unit.

### F30 🟢 — REFRESH token is literal, case-sensitive, no auto-uppercase

The component at [QuoteDealGate.svelte:109-113](apps/aberp-ui/ui/src/routes/QuoteDealGate.svelte#L109) is explicit:

> Type `REFRESH` exactly — case-sensitive, no auto-uppercase

The server-side gate at [quote_deal.rs:69](apps/aberp/src/quote_deal.rs#L69) is `const REFRESH_ACK_TOKEN: &str = "REFRESH"` compared literally. Pinned by [`stock_alert_blocks_without_refresh_ack`](apps/aberp/src/quote_deal.rs#L697) which tries lower-case "refresh" and asserts it fails.

### F31 🟢 — DEAL token field has no autofocus

Verified: [QuoteDealGate.svelte:156-166](apps/aberp-ui/ui/src/routes/QuoteDealGate.svelte#L156) — no `autofocus` attribute. The submit button is `disabled` until the typed token matches. A tab-key cannot accidentally land on a fired DEAL.

### F32 🟡 — Operator clock drift could expire valid quotes early or honor stale ones

`valid_until` is stored as `DATE` ([log_table.rs:138](crates/aberp-quote-intake/src/log_table.rs#L138)). The DEAL saga does NOT check `valid_until` against `now()` — the brief's pushback #7 (stale-stock banner) does material-side checks via `stock_alert`, but there's no expiry guard. A clock-drifted operator machine could accept a 6-month-old quote at the original (now wrong) price.

**Recommended fix for S275 or later:** add `valid_until` check inside the saga preconditions; surface a new `DealSagaError::QuoteExpired` variant + machine code. Until then, the SPA could badge-color expired rows.

## 8. SQL invariants in app layer (`[[no-sql-specific]]`)

### F33 🟢 — No CHECK, no triggers, no engine-specific syntax beyond `ON CONFLICT DO NOTHING`

Grepped across all new schemas; only standard `NOT NULL` / `PRIMARY KEY` / `DEFAULT` (on CREATE TABLE only) / `CREATE INDEX IF NOT EXISTS`. Every closed-vocab invariant (intake_state, stock_status, reservation state) is enforced in Rust.

### F34 🟢 — Audit-payload invariants are app-layer (forensic walks read JSON, not SQL columns)

`MaterialCommittedPayload` carries the post-increment snapshot (on_hand / reserved / committed / consumed) so a forensic walk can prove the invariant held without re-deriving from the live `inventory_balances` row. Same pattern as the QuoteDealIssued payload carrying SO/WO ids.

## 9. Wrapper boundary correctness (S270)

### F35 🟢 — Closed `ExtractError` enum covers the known failure modes

8 variants: `PythonNotFound`, `ModuleNotFound`, `InputFileNotFound`, `Timeout`, `NonZeroExit`, `MalformedJson`, `SchemaVersionMismatch`, `Spawn`. Each is actionable; `ModuleNotFound` is distinguished from generic `NonZeroExit` by stderr-substring check ([lib.rs:210](crates/aberp-cad-extract-wrapper/src/lib.rs#L210)), so the operator's SPA toast can route to "run `pip install -e python/aberp-cad-extract`" vs "look at the stderr."

### F36 🟡 — `python_bin` resolution from a venv symlink is fragile (the documented gotcha)

`realpath` on the venv's `python3` symlink resolves past the venv to the homebrew/conda interpreter — which does NOT have the module installed. Three of the wrapper's error-path tests have failed under different invocations because of pwd-drift. The S270 memory + the worktree's local docs name the workaround (absolute path, no `realpath`). For a daemon configured by the operator via `seller.toml`, the same trap applies: an operator who points `python_bin = "~/.venvs/aberp/bin/python"` and runs the daemon from a different cwd gets a runtime `ModuleNotFound`.

**Recommended fix for S271 wire-up session:** when reading `python_bin` from config, resolve it to an absolute path AT CONFIG-LOAD TIME and refuse to start if `python_bin` is relative OR `python_bin -m aberp_cad_extract --help` returns non-zero. Boot-time loud-fail per [[trust-code-not-operator]].

### F21 🟡 (repeated) — Pipe-deadlock on > 64KB stdout

Already covered in §4. Wrapper-specific reminder: this surfaces as `Timeout`, which is the wrong error for "Python wrote too much."

## 10. Storefront-side gap

### F37 🟢 — Schema is producer-less today; the saga's silent fallback survives every NULL pattern

Walked the seven S271 nullable columns: `customer_email`, `material_grade`, `quantity`, `total_price_eur`, `valid_until`, `stock_status_at_accept`, `stock_alert`. ABERP-side reads handle NULL for all of them. The saga skips material-commit when either `material_grade` or `quantity` is NULL/empty (F5 above). The recompute skips when `stock_status_at_accept` is NULL ([quote_stock_alert.rs:81](apps/aberp/src/quote_stock_alert.rs#L81)).

### F38 🟡 — There is no "no producer yet" indicator on the row

A row with `material_grade = NULL` looks identical to a row whose storefront-side pricing failed silently. The Quotes SPA shows the legacy `quantity_summary` (from the lossy JSON parse of raw_payload) but not the canonical `quantity_canonical` from the S271 column. An operator looking at a row can't tell whether the storefront wrote NULL because the pipeline is dormant, or because pricing failed for THIS row.

**Recommended fix for the storefront cutover session:** add a `pricing_state: 'pending' | 'priced' | 'failed'` column with the same NULL-defensive pattern; the SPA shows a "pricing pending" chip on rows where storefront hasn't written.

### F39 🟢 — Stub seeder for testing without storefront

The DEAL saga tests at [quote_deal.rs:1024-1051](apps/aberp/src/quote_deal.rs#L1024) (`s273_saga_happy_path_with_material_commit`) demonstrate the seeding pattern: `set_material_and_quantity` + `seed_balance`. Not packaged as a CLI, but the building blocks are there. A short integration-test fixture or `dev/` script would be a reasonable S275 add for operator-side smoke testing.

---

## Storefront-side handoff readiness

The storefront PR's authors will need to know these facts, in priority order:

- **`quote_intake_log.quantity` is INTEGER, not DECIMAL.** Storefront must round to a positive integer or the saga's `qty > 0` gate trips. F1 means this number is *also* what the saga interprets as kg of material — until F1 is fixed, an integer-rounded quantity is doubly wrong. **The storefront should NOT push `quantity` until the unit-conversion question is settled.** If storefront ships before S275, the safe pattern is to populate `material_grade` only (saga skips the material branch on missing `quantity` per F5) and let the operator commit material manually via the future Inventory Balances Edit modal.
- **`stock_status_at_accept` MUST be a valid four-value closed-vocab string** (`in_stock` / `source_1_2d` / `source_3_7d` / `special_order`). An invalid value silently no-ops the recompute (the conservative branch at [quote_stock_alert.rs:142](apps/aberp/src/quote_stock_alert.rs#L142)) — operator never sees the alert. The storefront's writer needs the same closed-vocab Rust-style enum.
- **`valid_until` is `DATE`, not `TIMESTAMP`** — storefront pushes `YYYY-MM-DD`. No expiry check exists on the saga side (F32) — so a stale `valid_until` is operator-visible only.
- **No `stock_alert` writes from storefront.** ABERP owns the recompute (sticky TRUE only). Storefront pushing `stock_alert = true` would land in the DB but the recompute pass treats stored=TRUE as a fixed point.
- **DEAL writes are server-authoritative.** Storefront must not write `deal_issued_at` / `deal_sales_order_id` / `deal_work_order_id` / `refresh_acked_at` — the CAS at [log_table.rs:732](crates/aberp-quote-intake/src/log_table.rs#L732) is the source of truth.
- **Catalogue push is one-way.** ABERP → storefront via [catalogue_push.rs](apps/aberp/src/catalogue_push.rs). PUT `/api/catalogue/materials` with the PUBLIC projection (grade / display_name / stock_status / lead_time — NOT cost / multipliers / density). Auth reuses `ABERP_QUOTE_INTAKE_TOKEN`.
- **Catalogue is single-tenant PK** (F17). Storefront's quote-form material dropdown can read everything; if multi-tenant lands later, the dropdown will need a tenant filter.
- **F1 / F3 / F5 / F32 will surface as operator-visible weirdness** until S275 lands. The storefront PR should NOT block on them, but the cutover runbook should name them so the first operator-facing post-cutover bug doesn't ambush anyone.

---

## Summary table

| ID | Sev | Title | File | S275? |
|---|---|---|---|---|
| F1 | 🔴 | `qty` silently committed as kg | [quote_deal.rs:447](apps/aberp/src/quote_deal.rs#L447) / [material_inventory.rs:114](apps/aberp/src/material_inventory.rs#L114) | partial (column rename + qty_unit_kind); full conversion later |
| F2 | 🔴 | `QuoteStockAlertTriggered` audit emit outside flip tx | [serve.rs:15655](apps/aberp/src/serve.rs#L15655) | S275 |
| F3 | 🔴 | Stale "S272 ships later" banner + "qty NOT kg" banner = operator-discipline | [QuotesList.svelte:282](apps/aberp-ui/ui/src/routes/QuotesList.svelte#L282) / [InventoryBalancesList.svelte:92](apps/aberp-ui/ui/src/routes/InventoryBalancesList.svelte#L92) | S275 |
| F4 | 🟢 | Single-tx DEAL saga atomicity | [quote_deal.rs:339](apps/aberp/src/quote_deal.rs#L339) | — |
| F5 | 🟡 | Material-skip is silent when `quantity` NULL | [quote_deal.rs:448](apps/aberp/src/quote_deal.rs#L448) | S275 |
| F6 | 🟡 | Recompute runs on dealt/irrelevant rows | [quote_intake_query.rs:139](apps/aberp/src/quote_intake_query.rs#L139) | S275 |
| F7 | 🟢 | STEP stub returns NotImplementedError | [step.py:31](python/aberp-cad-extract/aberp_cad_extract/extractors/step.py#L31) | — |
| F8 | 🟢 | Addendum 1 schema-locked both langs | [feature_graph.py:82](python/aberp-cad-extract/aberp_cad_extract/feature_graph.py#L82) / [feature_graph.rs:207](crates/aberp-quote-engine/src/feature_graph.rs#L207) | — |
| F9 | 🟢 | Sticky stock_alert correct both directions | [quote_stock_alert.rs:71](apps/aberp/src/quote_stock_alert.rs#L71) | — |
| F10 | 🟡 | `flip_stock_alert_to_true` TOCTOU read-then-write | [log_table.rs:358](crates/aberp-quote-intake/src/log_table.rs#L358) | S275 |
| F11 | 🟢 | DEAL CAS atomic | [log_table.rs:732](crates/aberp-quote-intake/src/log_table.rs#L732) | — |
| F12 | 🟢 | Replay precedence pinned | [quote_deal.rs:810](apps/aberp/src/quote_deal.rs#L810) | — |
| F13 | 🟡 | No rate limit on DEAL-token brute (bearer-gated, low risk) | [serve.rs:16219](apps/aberp/src/serve.rs#L16219) | follow-up |
| F14 | 🟢 | Catalogue purge does not untrigger alert | [quote_stock_alert.rs:84](apps/aberp/src/quote_stock_alert.rs#L84) | — |
| F15 | 🟢 | F12 ritual complete for all 12 EventKinds | [event_kind.rs](crates/audit-ledger/src/entry/event_kind.rs) | — |
| F16 | 🟡 | StockAlert audit append idempotency_key = None | [serve.rs:15680](apps/aberp/src/serve.rs#L15680) | S275 (pairs with F2) |
| F17 | 🟡 | `quoting_materials.grade` PK single-tenant | [quoting_materials.rs:43](apps/aberp/src/quoting_materials.rs#L43) | SaaS-migration session |
| F18 | 🟢 | inventory.* distinct from mes.stock_movement_recorded | [event_kind.rs:3105](crates/audit-ledger/src/entry/event_kind.rs#L3105) | — |
| F19 | 🟢 | S271/S272 ALTER avoids DEFAULT clobber | [log_table.rs:128](crates/aberp-quote-intake/src/log_table.rs#L128) | — |
| F20 | 🟡 | `quoting_materials` CREATE has DEFAULT (future ALTER trap) | [quoting_materials.rs:48](apps/aberp/src/quoting_materials.rs#L48) | S275 (comment-pin) |
| F21 | 🟡 | Wrapper pipe-deadlock risk on >64KB stdout | [lib.rs:186](crates/aberp-cad-extract-wrapper/src/lib.rs#L186) | follow-up session |
| F22 | 🟡 | TIMESTAMP needs CAST AS VARCHAR everywhere | [quote_deal.rs:677](apps/aberp/src/quote_deal.rs#L677) | S275 (helper) |
| F23 | 🟢 | ON CONFLICT DO NOTHING portable | [material_inventory.rs:451](apps/aberp/src/material_inventory.rs#L451) | — |
| F24 | 🟢 | Saga tx scope correct | [quote_deal.rs:339-511](apps/aberp/src/quote_deal.rs#L339) | — |
| F25 | 🟡 | Row deletion between pre-flight + CAS surfaces wrong error | [quote_deal.rs:279-346](apps/aberp/src/quote_deal.rs#L279) | S275 |
| F26 | 🟢 | stock_alert flip can't race the saga (DuckDB MVCC) | [quote_intake_query.rs:227](apps/aberp/src/quote_intake_query.rs#L227) | — |
| F27 | 🟡 | Hardcoded `rgba(255,0,0,0.06)` for breach row | [InventoryBalancesList.svelte:281](apps/aberp-ui/ui/src/routes/InventoryBalancesList.svelte#L281) | S275 |
| F28 | 🟢 | QuoteDealGate exemplary token use | [QuoteDealGate.svelte](apps/aberp-ui/ui/src/routes/QuoteDealGate.svelte) | — |
| F29 | 🟢 | Other SPAs clean of bypass colors | (grep) | — |
| F30 | 🟢 | REFRESH token literal + case-sensitive | [quote_deal.rs:69](apps/aberp/src/quote_deal.rs#L69) | — |
| F31 | 🟢 | DEAL input has no autofocus | [QuoteDealGate.svelte:322](apps/aberp-ui/ui/src/routes/QuoteDealGate.svelte#L322) | — |
| F32 | 🟡 | No `valid_until` expiry check in saga | [quote_deal.rs:270](apps/aberp/src/quote_deal.rs#L270) | S275 |
| F33 | 🟢 | No CHECK / triggers / DB-specific in new schemas | (grep) | — |
| F34 | 🟢 | Audit payloads carry invariant snapshots | [material_inventory.rs:553](apps/aberp/src/material_inventory.rs#L553) | — |
| F35 | 🟢 | ExtractError closed enum covers known failures | [lib.rs:263](crates/aberp-cad-extract-wrapper/src/lib.rs#L263) | — |
| F36 | 🟡 | `python_bin` from venv symlink fragile | [lib.rs:139](crates/aberp-cad-extract-wrapper/src/lib.rs#L139) | wire-up session |
| F37 | 🟢 | Storefront NULL pattern survives | (multiple) | — |
| F38 | 🟡 | No "pricing pending" indicator on rows | (none) | storefront cutover |
| F39 | 🟢 | Stub-seeder building blocks exist | [quote_deal.rs:1024](apps/aberp/src/quote_deal.rs#L1024) | — |

**Final counts: 4🔴 / 13🟡 / 7🟢 (vs the brief's "be adversarial" instruction — the 🔴/🟡 ratio is intentionally not soft-peddled).**
