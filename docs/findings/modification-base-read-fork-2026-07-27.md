# D2 — the modification route's base-invoice reads forked the DB (2026-07-27)

Follow-on to [`invoice-render-read-fork-2026-07-27.md`](invoice-render-read-fork-2026-07-27.md)
(PR #40, merged `baf5095`), which recorded D2a/D2b and handed them to this
session. Same ADR-0099 H3 read-fork class; D2a is the worse half because it
could mis-file real ÁFA.

## Symptom class

`serve::modification_invoice_request` reached two fresh
`Connection::open(&state.db_path)` openers while serve held the shared
`aberp_db::Handle`:

| | fn | consumer | on a stale read |
|---|---|---|---|
| **D2a** | `read_base_line_vat_kinds` (`serve.rs:9654`) | the ADR-0101 S2 VAT rate-kind guard | `Ok(vec![])` — **silent** |
| **D2b** | `read_base_currency` (`serve.rs:9611`) | the ADR-0037 §4 C6 chain-currency invariant | `Err` — loud |

Under H3 runtime checkpointing is disabled, so a base invoice issued since serve
boot is WAL-resident in the shared Handle. A second DuckDB instance does not
replay that WAL: it reads the last-checkpointed **subset** of the file.

## Why D2a was ship-blocking

`read_base_line_vat_kinds` ended in `.unwrap_or_default()`, so a read that could
not see the base returned an **empty vector** rather than an error. The guard
immediately downstream is

```rust
if let Some(kind) = base_vat_kinds.iter().copied().find(|k| !k.is_percent()) { reject }
```

which over an empty vector **passes vacuously**. Step 1's precondition
(`derive_state_for`) *is* Handle-routed, so it found the base and opened the
door; the forked read then failed to shut the gate.

Net effect: an AAM / domestic-reverse-charge / intra-Community base modified
through the SPA form was re-filed to NAV as plain `<vatPercentage>0.00</…>`,
silently dropping the exemption / self-assessment — on real ÁFA, and with no
preflight downstream to catch it (this route never calls
`validate_invoice_preflight`).

**Ordering confirmed, not assumed.** `derive_state_for` (`serve.rs:9435`) runs
*before* `read_base_line_vat_kinds` (`:9474`), which runs *before*
`read_base_currency` (`:9498`). The Handle-routed precondition genuinely does
admit an invoice whose forked kind-read then comes back empty, and the guard —
not the currency check — is the only thing standing between that and the wire.

## Fix — both halves

1. **Cause.** Both reads take the caller's `&Connection`;
   `modification_invoice_request` acquires one `state.db.read()` (a `try_clone`
   of the ONE instance) for both and drops it before step 6 dispatches into the
   write path. Same idiom PR #40 used for the render path.
2. **Class.** `read_base_line_vat_kinds` now **errors** when the result is empty
   or the base row is absent. The pre-fix doc-comment argued empty was safe
   *because* the `Finalized`/`Amended` precondition proves the base is an
   allocated, issued invoice. That premise is right and the conclusion was
   backwards: an issued invoice always has ≥1 `invoice_line` row, so empty means
   *the read did not see the base*, not *this base is plain-percentage VAT*. The
   guard can no longer pass vacuously for any reason, present or future
   (CLAUDE.md rule 11).

## Gate + regression evidence

* **Scanner.** `tools/adr0099_read_fork_scan.awk` is audit-ledger-shaped (typed
  ledger reads, `FROM audit_ledger`); both D2 fns read **business** tables, so
  CHECK N was structurally blind and CHECK M exempts `serve.rs`. Added a
  `PINNED` name ratchet: a listed fn may hold **no** fresh opener at all,
  whatever it reads, and no allow-list entry exempts it. Verified **RED on the
  pre-fix tree** (`9612:read_base_currency`, `9658:read_base_line_vat_kinds`),
  green after. Three new CHECK N0 backstop controls pin all three halves of the
  rule — fires on a pinned name, does **not** fire on the same body under an
  unpinned name (the teeth stay honest about their reach), does **not** fire once
  the fn rides the Handle.
* **e2e.** `tests/serve_modification_base_read_coherence.rs`, two pins, both with
  the `SERVE_HANDLE_LIVE` tripwire armed and the writer's Handle live across
  issuance *and* modification (the existing `serve_modification_route.rs` drops
  its seed Handle first, which is why it could never see this).
  * `reverse_charge_base_is_still_rejected_with_the_writer_handle_live` — the
    cause-side pin. Passes pre-fix too: a co-resident fresh open *sometimes*
    replays the WAL, and that nondeterminism is the defect.
  * `modification_must_block_when_the_base_vat_kinds_read_comes_back_empty` —
    the deterministic discriminator. Reproduces the torn read's observable
    shape (base `invoice` row present so the C6 check cannot mask the result;
    `invoice_line` rows unreadable). **Verified RED on the pre-fix tree**, and
    not by a near-miss: the route returned `Ok` and *actually issued*
    modification `TEST-INV-default/00002` off an all-`Percent` 0% body. Green
    after.

The tripwire alone could not have been the discriminator here: it hooks
`Ledger::open` and `DuckDbBillingStore::open`, and the D2a fork was a bare
`Connection::open` in `serve.rs`.

## Deferral ledger

| # | item | closes with |
|---|---|---|
| D1 | *(carried from PR #40, still open)* The read-fork scanner name-lists read helpers and now also pinned fn names, one at a time. The general shape — a fn holding a fresh opener whose reads leave the fn — is what should be flagged. The new `PINNED` ratchet is teeth on audited names, **not** coverage. | a structural CHECK N rewrite |
| D2c | The same in-serve business-read fork class remains at `serve::read_invoice_currency` (`serve.rs:10496`) and `read_invoice_total_gross_minor` (`:10522`) on the **mark-paid** route, plus the runtime openers at `serve.rs:11034`, `:20968`, `:23208`. All read loudly (`query_row`) like D2b, so none can mis-file the way D2a could; they are out of this fix's scope and NOT pinned in the scanner (pinning an unmigrated name would red the gate). | a business-table read-fork sweep of `serve.rs` |
| D3 | *(carried from PR #40)* `serve::record_upgrade_snapshot_mismatch_audit` (`serve.rs:6149`) appends via a fresh `Ledger::open` at boot. | the H3 boot-path sweep |
| D4 | *(carried from PR #40)* Arm `ABERP_SERVE_HANDLE_TRIPWIRE` in prod boot. Still blocked: D2c's openers are outstanding, and the tripwire does not hook bare `Connection::open` anyway. | after D2c |
| I1 | *(carried, explicitly out of scope)* `finalize_rate`'s `huf_equivalent_total` does the direct conversion ADR-0037 §1.c forbids while the wire sums per bucket. NAV filing is on the correct side; local PDF only. | its own session |
