# S325 — customer-PDF stock-alert re-render producer (EVE addendum 2)

**Session:** S325 / PR-25
**Branch:** `session-325/pr-25-pdf-rerender-producer`
**Date:** 2026-06-10
**Status:** producer shipped ABERP-side; closes the S318/S323 dormant
customer-facing stock-alert banner end-to-end.

---

## 1. What was dormant before this session

- **S318 (PROD_v2.27.12)** shipped the PDF *render capability*:
  `QuoteInputs.stock_alert` + a red top-of-page band in `aberp-quote-pdf`.
  But `advance_render` always set `stock_alert: false` (first render is
  pre-acceptance) and there was **no producer** to ever re-render with
  `true`.
- **S323 (storefront `c318850`)** relaxed `POST /api/quotes/{id}/priced`
  to **overwrite the stored PDF + flip the customer-side flag** on a
  same-hash, `stock_alert:true` re-post (was an idempotent no-op before).
- **Today both were dead** because ABERP never re-rendered + re-POSTed.

This session builds the missing ABERP-side producer.

## 2. Verify-first findings

1. `quote_pricing_pipeline.rs` `advance_render` QuoteInputs still
   hardcoded `stock_alert: false` (now ~line 717); `build_priced_multipart`
   meta still hardcoded `"stock_alert": false` (~line 1626). Confirmed.
2. `grep QuotePdfRerender|pdf_rerender apps/aberp/src/` → **no prior
   implementation**. Confirmed greenfield.
3. The FALSE→TRUE transition is observed **read-side** in
   `quote_intake_query::list_quote_intake_rows` (returns
   `newly_triggered_alerts`) and persisted in
   `serve.rs::handle_list_quote_intake` via `flip_and_audit_in_tx`.
   Confirmed — `recompute_stock_alert` stays pure; the seam wraps the
   call site.
4. `storefront_credential.rs` `StorefrontCredentialHandle` =
   `Arc<RwLock<Option<{base_url, Zeroizing<bearer>}>>>` with
   `snapshot()`/`set()`/`is_configured()`. Reused for the daemon.
5. `email_outbox_poll_daemon.rs` is the canonical daemon shape
   (deps struct + `poll_once` + supervisor + env-resolved cadence +
   kill switch). Modeled on it.

## 3. Design decision — in-memory queue (Option 1)

Per the brief, the re-render queue is an in-memory
`Mutex<HashSet<QuoteId>>` (`quote_pdf_rerender_queue.rs`), NOT a DB table:
- smaller surface, no schema migration, no DuckDB DEFAULT-on-replay care;
- idempotent enqueue (HashSet);
- **restart-tolerant via read-side re-detection**: a lost (undrained)
  entry is re-enqueued the next time an operator views the Quotes tab —
  bounded by the 5s daemon cadence + single-digit-quotes/day flow, the
  loss window is tiny.

**Caveat flagged:** once `stock_alert` is sticky-TRUE in
`quote_intake_log`, a subsequent operator view does NOT re-detect (the
`!stored_alert` guard). So the restart-tolerance promise holds only for
the (small) window between enqueue and the first drain. Acceptable per the
brief; a DB-backed queue is the upgrade path if that window ever matters.

## 4. Pieces shipped

- **A — detection seam.**
  `quote_intake_query::persist_alerts_and_enqueue_rerender` extracts the
  read-side flip+audit loop out of `serve.rs` and adds, on a confirmed
  flip, the `QuotePdfRerenderEnqueued` audit (same tx as the
  `QuoteStockAlertTriggered` flip — atomic) + a post-commit
  `queue.enqueue`. Testable without an HTTP server.
- **B — re-render daemon** (`quote_pdf_rerender_daemon.rs`): drains the
  queue every `ABERP_PDF_RERENDER_POLL_SECS` (default 5, clamp `[1,3600]`),
  loads `quote_pricing_jobs` artifacts, re-renders with
  `stock_alert=true`, best-effort overwrites the on-disk `priced.pdf`,
  re-POSTs via the `StorefrontCredentialHandle`. Pluggable
  `PricedReposter` trait (prod = reqwest + `build_priced_multipart`).
  Supervisor (S286/S307 panic-catch + backoff). Kill switch
  `ABERP_PDF_RERENDER_DISABLED`.
- **C — 3 audit EventKinds:** `quote.pdf_rerender_enqueued` /
  `quote.pdf_rerendered` / `quote.pdf_rerender_failed`, F12-ritualed.
- **D — storage:** in-memory (above).

## 5. Failure classification (mirrors `FailureKind`)

| outcome | verdict | queue action |
|---|---|---|
| 2xx (`rerendered`/`idempotent`) | success | drained |
| `409` | success (already-flipped / terminal) | drained |
| `5xx` | Transient | re-enqueued |
| transport (timeout/conn) | Transient | re-enqueued |
| `4xx` (≠409) | Permanent | dropped + audit |
| artifacts missing / render fail | Permanent | dropped + audit |
| unexpected `1xx`/`3xx` | Unknown | dropped + audit (no hot-loop) |

`409`-as-success follows the brief literally. Missing-artifacts =
Permanent rests on the ordering guarantee: `stock_alert` only transitions
post-acceptance, strictly after the pricing job reached `Posted`, so a
missing artifact is a genuine anomaly, not a race — re-queueing cannot
fix it, so it fails loud instead of hot-looping.

## 6. Flagged conservative calls

1. **In-memory queue** (per brief) — restart loss tolerated via
   re-detection; sticky-TRUE caveat noted above.
2. **`advance_render` literal left `stock_alert: false`** — it is
   genuinely first-render-only (pre-acceptance); only
   `build_priced_multipart` gained the `stock_alert` param. `post_priced_
   writeback` was NOT given a passthrough param (it is the first-render
   path; adding an always-false param would be speculative per CLAUDE.md
   #13). The re-render daemon builds its own `true`-valued multipart.
3. **Re-render tolerance = `ToleranceRange::Standard`** — the v1 pipeline
   quotes every job at `Standard` (the storefront form collects no
   tolerance), so this keeps the re-rendered PDF byte-faithful to the
   first render except for the banner. If a per-job tolerance is ever
   stored, read it here.
4. **No SPA status surface for the daemon** — out of scope; observability
   is via the 3 audit EventKinds + tracing. (The email-outbox daemon has
   an SPA panel; this one can grow one if operators need it.)
5. **Missing-artifacts → Permanent** (fail-loud, not silent-drop, not
   hot-loop) — see §5.

## 7. Gates

- `cargo fmt --check` ✓
- `cargo clippy -p aberp -p aberp-verify -p aberp-audit-ledger
  --all-targets -- -D warnings` ✓
- `cargo test`: audit-ledger 103 (+2 s325); aberp lib +17 s325
  (queue 3 / daemon 9 / query 2 / pipeline 1 + 2 env/classify helpers)
- `cargo build --release -p aberp` ✓
- `npm run build` ✓ (no frontend files changed) / `vitest` 1079 ✓

## 8. Daemon boot wiring

`serve.rs` boot daemon chain, right after the S307 email-outbox spawn:
kill-switch check → resolve cadence → build `QuotePdfRerenderDaemonDeps`
(db_path / tenant / binary_hash / operator_login /
`storefront_credential` / `quote_pdf_rerender_queue` / poll_interval) →
`run_supervised` registered with the shutdown coordinator as
`"pdf-rerender"`.
