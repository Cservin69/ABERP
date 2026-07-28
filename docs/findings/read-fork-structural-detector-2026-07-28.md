# Read-fork detector hardening — closing the structural blindness (2026-07-28)

Base: `main` @ `27aa689` (PROD_v2.33.1 cut). Scope: gate/detector internals only.
**No product behaviour change.** No prod source file is touched by this change.

## Why

Four instances of one bug class shipped: PR#40 (invoice PDF render), PR#41
(modification base read), PR#42 (auto-email attachment render) and E1. Every one was
the same shape — a fresh live-DB opener whose handle is read through, co-resident
with serve's shared `aberp_db::Handle`, reading the last-checkpointed subset because
H3 disables checkpointing and the Handle's writes are WAL-resident.

The PROD_v2.33.1 tag annotation named two weaknesses that would let a fifth through.

## Weakness 1 — `SERVE_HANDLE_LIVE` did not hook `Handle::open`

**Already closed, in PR#42 `4acd42b`** (`crates/aberp-db/src/lib.rs:254`), which
landed after the annotation was written. The annotation is stale on this point.

What was NOT there: **any test**. `Ledger::open` and `DuckDbBillingStore::open` each
had a `#[should_panic]` teeth test in `serve_handle_tripwire.rs`; `Handle::open` had
none. Measured: deleting the hook line left **all 8 pre-existing tripwire tests
green**. The hook was entirely unpinned — one bad merge from silently reverting.

Added (`apps/aberp/tests/serve_handle_tripwire.rs`):

- `a_request_path_business_reader_that_opens_its_own_handle_trips` — models the
  defect shape as a request-path helper handed the DB *path*, which opens its own
  `Handle` and reads a business table. Trips.
- `the_same_request_path_reader_is_clean_once_handle_routed` — the same read routed
  through the boot Handle. Clean, and returns the WAL-resident truth.
- Both go through `boot_serve_handle`, which reproduces serve's boot ORDER (open,
  *then* `register_serve_handle` — `serve.rs:1555` / `1571`) and asserts the boot
  Handle still reads clean **after** registration. So the tripwire is pinned to fire
  on a second, in-request open and never on the one instance serve legitimately
  holds.

**Mutation evidence:** removing the `assert_no_serve_handle(db_path, "Handle::open")`
line fails exactly `a_request_path_..._trips` and nothing else. Teeth confirmed.

## Weakness 2 — the read-fork scanner was a NAME allowlist

`adr0099_read_fork_scan.awk` reddened only on listed helper names
(`render_to_bytes`, `find_invoice_draft`, `read_base_line_vat_kinds`, …) and listed
enclosing-fn names (`PINNED`). The same shape under any unlisted name was invisible
— deferral D1, confirmed three times.

Now structural. The rule, with no reference to any name:

> a fresh live-DB opener bound to a local, whose handle (or anything derived from it)
> is READ THROUGH — a non-propagating method call, or the value handed to another
> function — inside a runtime fn that does not itself append.

- Opener set is by **type**, not fn name: `Ledger` / `Connection` / `Handle` /
  `DuckDbBillingStore` / `Database` `::open{,_default,_with_flags}`. `Handle::open`
  is in it, and its absence is how E1 hid: PR#40 moved a still-forking reader from
  `Ledger::open` (censused *and* tripwire-hooked) onto `Handle::open_default`
  (neither), and every detector went quiet.
- `.transaction()` / `.read()` / `.write()` / `.try_clone()` and the unwrap family
  **propagate** taint rather than counting as reads, so a read one or two
  indirections downstream is still attributed to the fork.
- `from_connection` / `open_in_memory` remain the sanctioned seams. Appends still
  belong to CHECK 10M.

### Proof (RED on unlisted name / GREEN on Handle-routed)

Synthetic controls in CHECK N0, all four directions:

| control | verdict |
|---|---|
| D2a body under `a_name_this_gate_has_never_heard_of` | **RED** ✓ |
| the same unlisted fn, Handle-routed | GREEN ✓ |
| E1's real body verbatim from `baf5095` (second `Handle::open`, read through) | **RED** ✓ |
| a factory that opens and RETURNS the handle | GREEN ✓ |

End-to-end against the real tree, a fork planted in `apps/aberp/src/reports.rs` under
a brand-new name:

```
+ apps/aberp/src/reports.rs|totally_novel_helper_nobody_listed
READ-FORK GATE: ✗ FAILED (CHECK N1 — structural ratchet)      exit=1
```
Handle-route the same fn, same name → `exit=0`. Plant a stale baseline entry →
`exit=1`. All three directions verified.

Probe **P5 inverted**: it was an `expect_silent` *asserting* the business-table blind
spot. Now a positive, with a new `P5b` Handle-routed negative. 17/17 probes pass.

## THE FINDING — 28 pre-existing structural forks, 12 live in-serve

Switching the shape rule on surfaced 28 forks no gate has ever been able to see.
Full triage with verdicts is in `tools/adr0099_read_fork_structural_baseline.txt`
(call sites checked, not inferred). Group A — **live in-serve on the serve-held
tenant DB, the exact defect class, OPEN**:

| site | note |
|---|---|
| `serve.rs\|read_invoice_currency` | twin of `read_base_currency`, which is PINNED and already migrated. Called `&state.db_path` @ serve.rs:10512 |
| `serve.rs\|read_invoice_total_gross_minor` | **the D2a shape verbatim** — fresh `Connection::open` + `billing::load_ready_invoice_by_id`. @ serve.rs:10493 |
| `serve.rs\|resolve_recipient_email` | on the email path |
| `serve.rs\|calibration_overview_request` | `&*state.db_path` |
| `serve.rs\|handle_quote_pipeline_status` | |
| `serve.rs\|handle_list_email_relay_queue` | |
| `serve.rs\|handle_get_email_relay_row` | |
| `serve.rs\|spawn_dap_audit_chain` | in-serve `Ledger::open` |
| `reports.rs\|compute_financial_report` | serve.rs:18482 / 19033 |
| `quote_pdf_rerender_daemon.rs\|prepare_rerender` | daemon runs inside serve (serve.rs:2778) |
| `audit_dap_boot.rs\|run_heartbeat_supervised` | serve.rs:27537 |
| `aberp-mes/src/ledger_writer.rs\|write_one` | also **APPENDS** via `write_mes_adapter_event`, yet **CHECK 10M reports ZERO** — the write-fork gate's append-token set does not know that helper name either. Same name-list failure, other gate. |

The first two are the sharpest: their siblings were migrated and PINNED during #41,
and they were left forking purely because nobody named them.

Group B (7): separate-process CLI one-shot helpers whose parent `run` is already
allow-listed. ⚠ two of them — `print_invoice.rs|render_to_bytes` and
`rebuild_stock_cache.rs|run` — hold **no flock** (`acquire_or_refuse`/`try_acquire`
absent from both files), so the premise that makes a CLI fresh read coherent is not
established for them.

Group C (9): not the serve-held path — boot `provision_atomic`, a *different*
tenant's DB, snapshot temp/staging files, the H2 pre-boot probe, and `Ledger::open`'s
own implementation. No fork; the discriminator is runtime, not textual, and the
tripwire stays silent on all nine.

### Why they are not fixed here — CONSERVATIVE CALL, flagged

Migrating 12 in-serve read paths (several on the invoice/NAV/ÁFA path) is product
work, each needing its own review per CLAUDE.md rule 4. Doing it inside the
detector change that found it would violate rule 3. So CHECK N1 **freezes** them:
an exact frozen set, additions RED, stale entries RED.

`CHECK N1 ✓ 0 new` means **"no fork was ADDED"** — never "the tree is fork-free". The
gate prints that caveat and the entry count on every run. The existing
`✓ ZERO non-allow-listed in-serve audit read-forks` verdict is untouched and still
means exactly what it meant, for its own (audit-ledger + PINNED) scope.

**Nothing was weakened.** Two negative controls flipped to positives (the CHECK N0
control and probe P5); both had been *asserting the blind spot*, and flipping them is
the change. No allow-list entry added; the census is untouched (0 openers added).

## Residuals for the adversarial session

1. **12 live in-serve forks open.** Highest value: `read_invoice_currency` and
   `read_invoice_total_gross_minor` (migrated siblings, unmigrated twins), and
   `ledger_writer::write_one` (a write-fork CHECK 10M cannot see).
2. **CHECK 10M has the identical name-list weakness.** The write-fork scanner keys
   off an append-token name set; `write_mes_adapter_event` is not in it, so an
   in-serve fresh-opener *append* reports ZERO. The D1 treatment applied here should
   be applied there. Not done in this change.
3. **`Handle::open` is absent from the ADR-0098 opener census** (its opener set is
   `Connection`/`Ledger`/`DuckDbBillingStore`/`Database` + `append_reopen`). The
   census treats the shared Handle as *the* sanctioned seam — sound only while there
   is exactly one. Adding it would churn the frozen fingerprint set; deliberately
   not done here. The structural scanner and the tripwire now both cover
   `Handle::open`, so the census is the last of the three still blind to it.
4. **Factory carve-out is a real hole.** A fork SPLIT across two fns (factory here,
   read in the caller) has no opener textually in the reading fn. Closing it needs
   the call graph. The runtime tripwire is call-graph-complete and now hooks
   `Handle::open`, which is the mitigation — the two halves are complements, and
   neither alone would have caught all four historical instances.
5. **Destructuring bindings are not followed** (`let Some(l) = Ledger::open(..)`) —
   taint is only tracked through simple `let` bindings. All four historical
   incidents used simple `let`.
6. `ABERP_SERVE_HANDLE_TRIPWIRE` is still **OFF by default** in serve
   (`serve.rs:1566`). Arming it is gated on the in-serve fork count reaching zero —
   which the 12 above now quantify for the first time.

## Environment note (not a code finding)

The disk hit 100% mid-session (166 MiB free); `cut_gate_read_fork_probes.sh` copies
the source tree per probe and failed with `tar: Write error` / `No space left on
device`, producing spurious `✗ META BROKEN` lines. ~180 GB sits in stale `target/`
dirs across seven old `.claude/worktrees/*`. Left alone — not mine to delete.

## Gates

`npm ci && npm run build`; `cargo fmt --check`; `cargo clippy --workspace
--all-targets -D warnings`; `cargo test --workspace` (60 suites ok, 0 failed);
all cut gates PASS; read-fork probes 17/17; edition-ratchet + backstop + probes PASS.
