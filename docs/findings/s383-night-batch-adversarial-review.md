# S383 — Night-batch adversarial review (S375 + S377 + S378 + S379 + S381)

**Session:** S383 (read-only research, doc-only PR)
**Date:** 2026-06-13
**Baseline:** `main @ d4d13a0` (PROD_v2.27.46), five sessions shipped overnight 2026-06-12 → 2026-06-13:

| Tag | Commit | Brief |
|---|---|---|
| PROD_v2.27.42 | `efae798` | S375 — render-before-commit atomicity + kill post-commit `Ledger::open` (DEV storno crash) |
| PROD_v2.27.43 | `442252d` | S377 — Origin header on storefront writebacks (SvelteKit CSRF 403 fix) |
| PROD_v2.27.44 | `478e114` | S378 — pre-flight NAV submission dedupe gate (F44 double-submit on 0047) |
| PROD_v2.27.45 | `523bda1` | S381 — NAV storno XSD F1–F4 sweep + modification S375 port + atomic XML writes |
| PROD_v2.27.46 | `d4d13a0` | S379 — classify listing-level no-CAD as permanent enqueue failure |

**Audits crossed against:**

- `docs/findings/s380-nav-storno-xsd-audit.md` (PR #6, off `59b648a`) — F1–F5 against storno XSD/business rules.
- `docs/findings/s382-concurrency-db-lock-hardening.md` (PR #7, off `efae798`) — F1–F10 against the concurrency/DB surface, including the explicit S383–S391 roadmap.

This review walks each fix against both audits *and* against each other (regressions, missed edges, drift), with file:line evidence. Read-only. No code changes.

---

## 0. Executive summary

**The five fixes are bulletproof on the scope they each claimed.** S380's F1/F2/F3/F4 are all closed correctly and symmetrically across storno + modification; S378's per-(tenant, invoice_id) async mutex closes the in-process F44 race at every axum-handler-driven submission entry point; S375's render-before-commit + `Ledger::from_connection` pattern was correctly ported to `issue_modification.rs` (the one un-fixed sibling S382/F1 explicitly named); S377's Origin header lands on every multipart writeback (the only Content-Type SvelteKit's default CSRF gate refuses); S379's permanent-failure classification lands a Failed row idempotently and the SPA's existing failure-row renderer surfaces it correctly.

**Three findings worth surfacing now** — none break this PROD cut:

- **🔴 F1 — S380/F5 latent risk is *no longer latent*.** S381/F1 unlocked the MODIFY submission path (previously guaranteed schema-fail via the v2.0-only `<modificationIssueDate>`). The day the first MODIFY reaches SAVED, the next storno of that base will reverse base-lines only — the WARN class `INCONSISTENT_MODIFICATION_DATA_*_NOT_ZERO*` will fire. WARN, not block; but the audit explicitly said "park behind F1; fix when MODIFY goes live" — F1 just went live.
- **🟡 F2 — S382/F4's "torn-PDF read" half is still open.** S381 made `nav_xml::write_to_path` atomic at all four NAV-XML emit sites, but `quote_pdf_rerender_daemon.rs:503`'s `std::fs::write(&pdf_path, &pdf_bytes)` (in-place overwrite of `priced.pdf`) was *not* migrated. S382/F4 named this same hazard.
- **🟡 F3 — `submit_invoice.rs:520, 553` still does post-commit `Ledger::open` (S375's same anti-pattern, in a different file).** S375 closed it in `issue_invoice.rs:822` / `issue_storno.rs:550`, and S381 ported it to `issue_modification.rs:477`. The `Ledger::open`-after-commit in `submit_invoice.rs` was *not* in S375's scope (S375 was the issuance crash) and *not* called out by S382. Hasn't been observed to crash; same class of duckdb#23046 trigger.

Other findings: small memory drifts in `project_aberp_db_concurrency_posture.md` (S382 snapshot); no corruption-survival tests for `write_to_path`; one observation that the CLI submission paths (`drain-submission-queue`, `retry-submission`, `submit-invoice`) bypass S378's gate by design (acknowledged in the doc comment, tracked as S386). PR #7's mandatory `submit_annulment` analogue not in S378 scope (annulment is a separate NAV operation; CLI-only; operator-confirmed).

---

## 1. Findings table

| # | Sev | Surface | Where (file:line) | What it is | Test coverage | Recommended action |
|---|---|---|---|---|---|---|
| F1 | 🔴 | NAV storno render (S380/F5 latent → live) | `apps/aberp/src/nav_xml.rs:794` `negated_lines = invoice.lines.iter().map(negate_line).collect()` | Storno still negates the BASE's lines only; spec §2.5.1 (p.163 EN) requires reversing base ⊕ all prior modifications. Latent under S380 because F1 (modificationIssueDate) made every MODIFY a guaranteed schema-fail; S381/F1 *removed* `<modificationIssueDate>`, so MODIFY can now reach SAVED. First MODIFY-then-STORNO chain trips `INCONSISTENT_MODIFICATION_DATA_NETAMOUNT_NOT_ZERO_NORMAL` (ID 1200) + `_VATAMOUNT_NOT_ZERO` (1220) + `_VATAMOUNT_NOT_ZERO_HUF` (1230). WARN-level. | `issue_storno_xml_round_trip.rs` pins base-only reversal; no chain-with-prior-MODIFY case. | S384 (queued behind real customer demand for MODIFY). Read the SAVED-confirmed modification entries off the chain inside `run_single_tx` and fold their effective lines into `negated_lines` before negation. The infra already exists: `saved_chain_member_ids_in_tx` (`issue_storno.rs:1247`) + payload walkers. Test pin: chain CREATE → MODIFY(SAVED) → STORNO must net to zero across summary. |
| F2 | 🟡 | PDF rerender torn-read (S382/F4 half-closed) | `apps/aberp/src/quote_pdf_rerender_daemon.rs:503` `std::fs::write(&pdf_path, &pdf_bytes)` (in-place overwrite of `priced.pdf`) | `nav_xml::write_to_path` is now atomic (tmp + fsync + rename + parent fsync, `nav_xml.rs:1918`); the PDF rerender path was not migrated. S382/F4 explicitly named this: "PDF artifacts have a producer/consumer partial-read window … rerender daemon overwrites `priced.pdf` in place while `serve.rs:17169` `std::fs::read`s it → partial-read serving a torn PDF to a customer." | None — no concurrent reader+writer test on `priced.pdf`. | Lift `write_to_path`'s tmp/fsync/rename into a `crate::fs::write_atomic` helper, adopt at `quote_pdf_rerender_daemon.rs:503`. Same-session-day work; the helper is 50 lines already written. |
| F3 | 🟡 | submit_invoice post-commit Ledger::open | `apps/aberp/src/submit_invoice.rs:520` (after TX1 commit, for mirror sync) and `:553` (after TX2 commit, for verify_chain) | Same `Connection::open`-after-commit anti-pattern S375 closed in the three issuance files, in a fourth file S375 didn't touch and S382 didn't flag. Each `Ledger::open` here forks a second in-process Database instance whose checkpoint replay walks the same duckdb#23046-prone code path. Has not crashed in DEV yet (the storno crash on 0047 happened in a *different* file); same class. | None — S375 added `from_connection` tests for invoice/storno; nothing equivalent for submit. | Port the S375 `(outcome, Connection) → from_connection` pattern: `write_attempt_audit`/`write_response_audit` return the post-commit Connection; `Ledger::from_connection` reuses it for `sync_mirror` and `verify_chain`. Tracks alongside the S388 1.5.3 bump — same code lives on both sides of that. |
| F4 | 🟡 | Memory drift — `project_aberp_db_concurrency_posture.md` | Lines 21, 22, 24, 31 of the file | The S382-snapshot memory still names as 🔴 / 🟡: `issue_modification.rs:364` post-commit `Ledger::open` (closed by S381/PR-10 modification port at `:444`/`:477`), F44 named-deferred (closed in-process by S378's gate), `nav_xml::write_to_path` non-atomic (closed by S381's atomic write). Roadmap line 31 still lists S383 modification-parity / S384 F44 / S385 write_atomic. | n/a | Mechanical refresh, no code change. Strike through-or-remove the three closed bullets; rewrite roadmap line 31 to start from S384 F5 chain-aware storno (this doc's F1). Memory hygiene only — does not impact PROD. |
| F5 | 🟡 | No corruption-survival tests for `write_to_path` | `apps/aberp/src/nav_xml.rs:1918` (production helper) vs `apps/aberp/tests/rollback_conformance.rs` (existing test surface) | The atomicity invariant under fault injection IS pinned (`rollback_conformance.rs:265, 329, 409` — render fault before commit rolls back). But there is no test that: (a) confirms a failed write leaves no `.tmp.<pid>-<nanos>-<seq>` leftover in the parent dir (the `remove_file` at `nav_xml.rs:1960` is unverified); (b) simulates a crash between `sync_all` and `rename` (no half-written file); (c) confirms concurrent writers to the same path don't interleave. | All three negative paths are uncovered. | Single-file unit test next to the existing tests block in `nav_xml.rs:1974`. Property-style: (i) corrupt-the-temp-file via `OpenOptions` mid-write → assert original path still has previous bytes; (ii) two threads racing `write_to_path` to the same path → assert final bytes are exactly one of the two inputs. Tracks alongside S389 (cross-process & crash-injection harness from S382/F8). |
| F6 | 🟢 | CLI submission paths bypass S378's gate (by design) | `apps/aberp/src/drain_submission_queue.rs:387` and `apps/aberp/src/retry_submission.rs:465` both call `manage_invoice::send_built_request` directly without acquiring `submission_gate` | This is *correct*. The gate is process-local statics (`serve.rs:6667 GATES: OnceLock<Mutex<HashMap>>`); a CLI subcommand running in a separate process cannot see it. The doc comment `serve.rs:6659–6661` calls this out explicitly: "In-process only. Cross-process … NOT covered here — that needs the flock/pidfile single-instance guard tracked separately (S386)." DuckDB's file lock is the cross-process barrier today (and the CLI subcommand fails to open the ledger while serve runs). `retry-submission` additionally consults NAV's own `queryInvoiceCheck` at `retry_submission.rs:357` (Layer-2 idempotency). | `s378_*` tests pin the in-process race closure; no harness exists for the CLI-vs-serve race per S382/F8. | No action under S378's scope. Folded into the S386 (flock single-instance guard) roadmap from S382. Worth re-confirming with the operator that no operator-runbook reaches for `aberp drain-submission-queue` while serve is up. |
| F7 | 🟢 | Storno paymentDate left at issue date (S380/F2 follow-up) | `apps/aberp/src/issue_storno.rs:357` `payment_deadline: default_calendar_date` (= storno issue date) | S380/F2's recommendation said "paymentDate is `minOccurs=0` — for a storno either copy the base's or omit it (omission is the cleaner reading; nothing falls due on a cancellation)." S381/F2 fixed the `invoiceDeliveryDate` half (now copied from the base XML) but deliberately kept `payment_deadline` = today, with a comment justifying the choice (`issue_storno.rs:346–349`). NAV does not WARN on this — the audit's wording was "preference," not "fix-must." | F2 round-trip test pins the new deliveryDate from base; payment_deadline is implicit. | None — deliberate scope call, well-commented. Note for the next storno session that the cleanest reading would be to omit `<paymentDate>` from storno bodies. |
| F8 | 🟢 | Origin header scope is sound | `apps/aberp/src/quote_pricing_pipeline.rs:1170` (priced writeback, multipart) and `apps/aberp/src/quote_pdf_rerender_daemon.rs:298–299` (repost, multipart). NOT sent at `quote_pricing_pipeline.rs:497` (status writeback, application/json), `serve.rs:17803` (operator accept, application/json), or `email_outbox_poll_daemon.rs:873, 904, 934` (no/json body). | SvelteKit's default CSRF gate refuses only the "form-like" content types (`application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`); JSON POSTs are not gated. The two multipart sites are exactly the two that need the header. | `s377_origin_from_base_url_*` (`quote_pricing_pipeline.rs:4262–4294`) pins URL transformation; `nav_xml.rs` does not own the writeback; no e2e CSRF-rejection regression. | None — scope is correct. Fragile if the storefront ever moves a JSON endpoint behind a stricter CSRF posture, but that's a contract change to detect there, not here. |
| F9 | 🟢 | S378 dedupe gate covers all in-process axum entry points | `apps/aberp/src/serve.rs:6729` gate acquired inside `submit_invoice_request`. Called from manual handler (`serve.rs:6416`), post-issue auto-submit task (`:5869`), and chain auto-submit (`:5993`, used by storno + modification chain children). | Per-(tenant, invoice_id) async Mutex. Tuple key. Held across `derive_state_for` → `submit_from_inputs` → wire send. Second waiter re-derives Pending, refused via `submission_dedupe_message` (`serve.rs:6593`). Annulment (`submit_annulment.rs`) is CLI-only and a different NAV operation — out of S378 scope. | `submission_gate_keys_per_tenant_invoice` (`serve.rs:21846`) + `submission_dedupe_message_*` tests. | None. |
| F10 | 🟢 | StornoReference / ModificationReference cosmetic dedup (S380 doc note) | `apps/aberp/src/nav_xml.rs:376` (`StornoReference`) and `:417` (`ModificationReference`) | Field-sets are now identical (S381/F1 removed the only differing field, `modification_issue_date`). The two struct names are kept distinct as "named input types for the two render functions — a future session may unify them" (`:413–415` doc comment). Explicit deferred. | n/a | Deferred per CLAUDE.md rule 2 (no speculative consolidation). Re-evaluate when a third chain-link reference shape ever appears. |

---

## 2. Cross-checks against the brief's questions

### 2.1 Does S378's `submission_gate` cover all NAV-submit entry points, including S381's modification path?

Yes for in-process axum-handler paths; intentionally no for CLI subcommands (acknowledged scope per F6).

- Manual handler `POST /invoices/:id/submit` → `submit_invoice_request` → gate (`serve.rs:6416 → 6729`).
- Post-issue auto-submit background task → `submit_invoice_request` → gate (`serve.rs:5869`).
- Chain auto-submit (storno + modification chain children, fired from the chain issuance post-tail) → `submit_invoice_request` → gate (`serve.rs:5993`).
- CLI `aberp drain-submission-queue`, `aberp retry-submission`, `aberp submit-invoice` → direct `manage_invoice::send_built_request`, NO gate (`drain_submission_queue.rs:387`, `retry_submission.rs:465`, plus `submit_invoice.rs:533` when reached via `submit_invoice::run`). DuckDB file lock and (for retry) `queryInvoiceCheck` are the cross-process barriers. Tracked as S386.

### 2.2 Missed deliverables — S380/F5, storno paymentDate, cosmetic ref dedup

- **F5:** Not fixed (deliberate per S381 scope). Newly *live* — see Findings F1.
- **storno paymentDate:** Not changed (S381 fixed deliveryDate only). See F7 — deliberate; NAV does not WARN on it.
- **Cosmetic StornoReference/ModificationReference dedup:** Field-sets unified, names kept. See F10.

### 2.3 Implementation drift — F3 negation everywhere it's needed

S381/F3 negates `quantity` (not `unit_price`) at exactly one site — `nav_xml.rs:1063 negate_line` — which is the only negation site. `render_storno_data_with_number` calls it at `:794`. Modification path renders the operator-supplied lines verbatim (no negation — MODIFY is full-replace per ADR-0024 §4). No drift.

### 2.4 Test gaps — corruption-survival for `write_atomic`

The atomicity invariant under fault injection is pinned (`rollback_conformance.rs:265, 329, 409`). Three corruption-survival behaviors are uncovered: tempfile cleanup on failure, crash between fsync and rename, concurrent same-path writers. See F5.

### 2.5 Combined-fixes — S378 dedupe + S381 modification port — modification path also through `submission_gate`?

Yes. Issuance auto-submits go through chain auto-submit (`serve.rs:5993`) → `submit_invoice_request` → gate. Modification chain children burn their own invoice_id, so the gate's per-(tenant, invoice_id) tuple naturally separates them from base/storno; the gate serializes concurrent submissions of the same modification invoice if any race appears.

### 2.6 Operator surface — S379 Failed rows display correctly in SPA

Yes. `PricingJobsList.svelte:286–298` renders Failed rows with `error_stage` ("enqueue"), `failure_kind` badge ("Permanent"), and `error_reason` ("no CAD file on listing") — these are the exact strings S379 writes (`quote_pricing_pipeline.rs:431–432`). The Retry button is rendered unconditionally for failed rows (`:305–320`); operator can manually re-fire, as the brief intended ("never auto-reset; operator Retry required"). The `insert_failed_enqueue_job` ON CONFLICT (quote_id) DO NOTHING guard at `quote_pricing_jobs.rs:361, 420` is what stops the phantom retry loop.

### 2.7 Memory drift — any "NOT cut" lines stale?

Yes — `project_aberp_db_concurrency_posture.md` lines 21–31 are partly stale (F4 in this doc). The S380 memory `project_aberp_s380_nav_storno_xsd_audit.md` lines 22–40 describes the F1–F5 findings before S381 fixed them — that file documents the *audit*, not the closure, so it's not strictly "stale," but a forward-link to S381's closure would help future-me read it without bouncing.

### 2.8 F5 latent risk when MODIFY fires in prod

See F1. F5 is no longer latent — S381/F1 unlocked MODIFY by removing the v2.0-only `<modificationIssueDate>` schema-illegal element from the body. The first MODIFY-then-STORNO chain in prod will fire the `INCONSISTENT_MODIFICATION_DATA_*_NOT_ZERO*` WARN class. Not blocking; visible in NAV's ack.

---

## 3. DEV / prod test recipes

These are the specific manual checks that would have caught (or will confirm in DEV) the findings above.

1. **F1 — chain-aware storno.** DEV: issue CREATE → submit-to-NAV-test → wait SAVED → issue MODIFY → submit → wait SAVED → issue STORNO → submit → poll-ack. Expected outcome under current code: SAVED with `INCONSISTENT_MODIFICATION_DATA_*_NOT_ZERO*` WARNs (the chain's net is no longer zero because the storno reverses base-only while NAV's chain sees base + modification). Decode `response_xml` per S184. Operator escalation: zero-sum WARNs in the ack tell us F5 needs S384.
2. **F2 — torn PDF.** DEV (or staging): trigger a quote-rerender (acceptance path) and concurrently `cat priced.pdf > /tmp/copy.pdf` 100×. Pre-fix: expect occasional `PDF parse error` / short reads. Post-fix: every copy is byte-exact one of the pre-rerender or post-rerender bytes.
3. **F3 — submit_invoice reopen.** DEV: load the audit ledger to ~50k entries (use a tenant with a real history), submit a Ready invoice, watch `tracing` logs for any `Failed to load metadata pointer` from `Ledger::open` at `submit_invoice.rs:520` or `:553`. Sample size of 1 is insufficient — needs sustained traffic.
4. **PROD smoke (positive confirmation).** Cut PROD_v2.27.46 from `d4d13a0`, issue a fresh CREATE → expect immediate SAVED on poll-ack. Issue a STORNO of the CREATE on a separate calendar day → expect SAVED. Confirm:
   - Operator double-click on Submit on a Ready invoice → second request returns 409 with `submission already in progress for this invoice` (S378 gate).
   - Storefront priced writeback returns 200 (S377 Origin header).
   - Storno's `<invoiceDeliveryDate>` in the issued XML equals the base invoice's delivery date (S381/F2).
   - Storno's `<lineNumber>` `<quantity>` negative, `<unitPrice>` positive (S381/F3 spec letter).
   - Storefront quote with missing CAD file → single Failed row in SPA, error_stage=enqueue, failure_kind=Permanent, no re-fire on the 60s cycle (S379).
5. **PROD post-deploy regression watch.** Tail `tracing` logs for an hour after deploy looking for:
   - `Failed to load metadata pointer` (F3 — submit_invoice reopen).
   - `UNINTENDED_CANCELLATION_DELIVERY_DATE` from NAV ack — should be ZERO post-S381/F2.
   - `INVOICE_NUMBER_NOT_UNIQUE` from NAV ack on submission — should be ZERO post-S378.
   - 403 from storefront on priced writeback — should be ZERO post-S377.

---

## 4. Recommended next session order

(Update of `s382-concurrency-db-lock-hardening.md` §"Hardening roadmap".)

1. **S384 — chain-aware storno (F1 above).** Closes the WARN class unlocked by S381/F1; needs `saved_chain_member_ids_in_tx` reuse + a payload walker for the SAVED modification effective-lines, folded into `render_storno_data`'s `negated_lines` build. Queued behind a customer actually wanting MODIFY (so far none).
2. **S385 — `write_atomic` helper extraction + PDF site adoption (F2 above).** Lift the tmp/fsync/rename from `nav_xml::write_to_path` into a `crate::fs::write_atomic`, adopt at `quote_pdf_rerender_daemon.rs:503`. Same-day; biggest risk-reduction per LOC of any item here.
3. **S386 — flock single-instance guard.** Unchanged from S382's roadmap. Converts CLI-vs-serve / second-serve corruption scenarios into a boot error.
4. **S387 — Snapshot/restore as product.** Unchanged from S382.
5. **S388 — DuckDB 1.5.3 bump.** Unchanged from S382. Snapshot-gated. *Bundle F3 above into this session* — port the S375 from-connection pattern to `submit_invoice.rs:520, 553` while you're already in the file.
6. **S389 — Cross-process & crash-injection test harness (F5 above).** Unchanged from S382. Add the corruption-survival tests for `write_to_path` here too.

---

## 5. Flagged uncertainties (conservative readings taken)

- **Whether the SvelteKit storefront uses default CSRF posture** — F8's analysis assumes the framework's documented default (form-content-type-only CSRF refusal). If the storefront's `hooks.server.ts` overrides this to also gate JSON POSTs, the status writeback (`quote_pricing_pipeline.rs:497`) and operator-accept (`serve.rs:17803`) would 403 too. Read-only audit cannot verify storefront source from here; flagged as a known assumption.
- **Whether `submit_invoice.rs:520, 553`'s reopen actually triggers duckdb#23046 under prod conditions** — same code shape S375 fixed in three other files, but never observed crashing in DEV/PROD on the submission path. The S382 audit did not flag it; this review escalates to 🟡 on pattern-match basis only.
- **Whether S381's F4 SAVED-only walker handles in-flight Pending modifications correctly** — a modification whose submission attempt has committed but whose NAV ack has not yet landed is in `InvoiceState::Pending`. `saved_chain_member_ids_in_tx` excludes it (rightly — NAV has not registered the index). If the operator issues a new storno on the same base in that window, the storno's `modificationIndex` would not skip the Pending modification's index — and once the Pending one ACKs SAVED, NAV would reject the *storno* with `MODIFICATION_INDEX_NOT_UNIQUE`. Narrow window; the existing in-flight Pending modification would itself block via S378's gate if its base invoice_id matches, but the gate keys per-invoice-id, not per-base. Worth flagging for the F1 / S384 chain-aware session.
