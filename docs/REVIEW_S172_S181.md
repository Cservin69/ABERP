# Adversarial review — Sessions 172-181 (PRs 172-181)

**Review date:** 2026-05-31
**Scope:** commits `905d35c..b283deb` on main (10 PRs)
**Reviewer:** Session 182 (read-only second-pair-of-eyes pass)

## Summary

Solid overall — the chain shipped a coherent AP module v1 (S177/178/179) plus a
NAV-as-DR restore wizard (S180) plus four operator-ergonomics PRs (S172/174/175/181)
plus two prod-safety fixes (S173/176). No production-blocking 🔴 bugs found.
The biggest 🟡 concern is the **S173 year-source divergence**: annulment uses
`Entry.time_wall.year()` while every other path uses `issue_date.year()` — latent
today (default template has no `Segment::Year`) but lights up the moment a tenant
adopts a year-bearing numbering template. Second-biggest concern is the **O(n×N)
chain-verify cost in S180's restore** which will surprise the operator on a
first-ever restore of a year with hundreds of invoices.

## 🔴 Real bugs / production risks (action required before Monday)

None. The latent S173 divergence below would be 🔴 if anyone had a year-bearing
template configured — confirm none does and it stays 🟡.

## 🟡 Hidden risks / smells (worth eyeballing this week)

- **S173 — annulment uses a different year source than every other render-call
  site.** `apps/aberp/src/request_technical_annulment.rs:425` renders the base
  invoice number with `latest_sequence_year` captured from
  `Entry.time_wall.year()` (the audit-ledger wall clock at sequence reservation
  time). Meanwhile `issue_invoice.rs:759`, `issue_modification.rs:407+409`,
  `issue_storno.rs:471+473`, and `observe_receiver_confirmation.rs:369` all
  source the year from `billing.invoice.issue_date.year()` (per
  `outcome.base_issue_year = base_invoice.issue_date.year()` at
  `issue_storno.rs:975` and `issue_modification.rs:887`). For tenants on the
  default template these year values are unused (default has no `Segment::Year`),
  so the divergence is invisible. But for any tenant who adopts a year-bearing
  template the moment an invoice is back-dated or post-dated across a year
  boundary (a common end-of-year-bookkeeping case in HU) — annulment will silently
  send the wrong `<annulmentReference>` to NAV. Recommended future PR: capture
  `base_issue_year` from the `InvoiceDraftCreated` payload's `issue_date` field
  (it's already serialized there per the storno chain), or open the billing DB
  read-side during annulment.
  > **ADDRESSED-BY-PR-183.** `request_technical_annulment.rs` now opens the
  > billing store and reads `base_invoice.issue_date.year()` via the new
  > `load_base_invoice_issue_year` helper (mirroring
  > `observe_receiver_confirmation::load_base_nav_invoice_number`'s posture),
  > then passes that year into `check_base_is_annullable` for the
  > `template.render_for_build` call. The walker's old
  > `Entry.time_wall.year()` capture is deleted. Tests
  > `check_base_is_annullable_renders_year_from_base_issue_date_param` and
  > `check_base_is_annullable_cross_year_cites_base_original_year` pin the
  > new contract. (Aside on the original recommendation: the
  > `InvoiceDraftCreatedPayload` does NOT actually carry an `issue_date`
  > field — only `IncomingInvoiceIngestedPayload` and
  > `InvoiceRestoredFromNavPayload` do. The billing-row read matches the
  > posture every other base-reference render uses.)

- **S176 — PNG decoder has no explicit decompression-bomb cap.** 
  `crates/invoice-pdf/src/logo.rs:74` does `vec![0u8; reader.output_buffer_size()]`.
  The `png` crate's default `Limits` (currently 64 MB) is the only thing standing
  between a malicious/corrupt `logo.png` and a gigabyte allocation. A future
  `png` crate upgrade that relaxes the default would silently re-enable the bomb.
  The operator drops the file directly so the attack surface is operator-shoots-self;
  still, the brief explicitly named this risk and the code does not address it.
  Recommended future PR: call `decoder.set_limits(png::Limits { bytes: 16 * 1024 * 1024 })`
  defensively at the top of `from_png_bytes`.
  > **ADDRESSED-BY-PR-185.** Three defences in depth: (1) a 2 MiB
  > file-size cap in `print_invoice::load_tenant_logo` (new
  > `MAX_LOGO_FILE_BYTES`), checked via `fs::metadata` BEFORE the bytes
  > hit `fs::read`; (2) a `MAX_LOGO_DIMENSION` (4096×4096) check on
  > `read_info()`'s reported width/height in `from_png_bytes`, BEFORE
  > the `output_buffer_size()` allocation; (3) an explicit
  > `Decoder::set_limits(png::Limits { bytes: MAX_PNG_DECODE_BYTES })`
  > matched to the dimension cap, defending against a future png-crate
  > upgrade that relaxes the default. New tests
  > `rejects_png_width_above_cap` / `rejects_png_height_above_cap` /
  > `load_tenant_logo_oversize_file_returns_none` /
  > `load_tenant_logo_oversize_dimensions_returns_none` pin all three
  > layers. The dimension cap composes with the Fix-A fallback below
  > so an oversized PNG also degrades to a text-only header.

- **S176 — alpha-drop produces visible non-zero RGB on transparent pixels, contradicting
  the doc comment.** `crates/invoice-pdf/src/logo.rs:80-99` drops the alpha channel
  without premultiplication. The test at line 154-163 even asserts this:
  `Rgba pixel [200, 100, 50, 0]` → RGB `[200, 100, 50]` (i.e., a fully transparent
  pixel renders as visible orange against the white page background). The doc
  comment at the test claims "transparent-edge logos display their RGB ink, not a
  black halo" — true in the sense that there is no black halo, but the actual
  behavior is "your transparent regions display whatever RGB happened to be
  encoded under the alpha". PNG optimizers commonly fill alpha=0 pixels with
  garbage RGB. Recommended future PR: composite `rgb = rgb * alpha + white * (1-alpha)`
  in the Rgba arm.
  > **ADDRESSED-BY-PR-185.** New `composite_over_white(src, alpha)`
  > helper in `crates/invoice-pdf/src/logo.rs` integer-blends every
  > non-opaque pixel against the white page background:
  > `out = round((src·α + 255·(255-α)) / 255)`. Both the Rgba arm
  > and the GrayscaleAlpha arm now route through it. α=255 stays
  > lossless (the +127 rounding term plus `255·0` collapses to src);
  > α=0 collapses to 255 (white); α=128 produces the expected
  > half-blend. The pre-existing `decodes_rgba_drops_alpha` test
  > (which asserted the BUGGY behaviour `[200,100,50,0] → [200,100,50]`)
  > is replaced by `rgba_transparent_pixels_composite_to_white`
  > (`α=0 → [255,255,255]`), plus three new pins:
  > `rgba_opaque_pixels_pass_rgb_through_unchanged`,
  > `rgba_half_alpha_blends_with_white`, and
  > `grayscale_alpha_transparent_pixels_composite_to_white`.
  > `composite_over_white_helper_matches_formula` locks the integer
  > rounding to known points so future refactors can't silently
  > re-introduce off-by-one drift.

- **S176 — a malformed `logo.png` blocks all printed PDFs (download + email
  attach + CLI print).** `apps/aberp/src/print_invoice.rs:204-205` wraps the
  decode in `with_context(...)?` and `load_tenant_logo` at line 111-141 returns
  an error on any decode failure. That propagates to a 500 from the PDF route
  and a non-zero exit from `aberp print-invoice`. Recovery is "delete the file",
  but the operator may not know to look there. Issue path is NOT affected (only
  PDF render). Recommended future PR: downgrade decode failure to a tracing
  `warn!` + render text-only, mirroring the absent-file path.
  > **ADDRESSED-BY-PR-185.** `load_tenant_logo` in
  > `apps/aberp/src/print_invoice.rs` no longer propagates errors on
  > the logo path. Every failure mode — missing seller_toml parent,
  > stat failure, file over the 2 MiB cap, read IO error, PNG decode
  > failure (including the new dimension cap from Fix B) — is caught
  > and downgraded to a bilingual `tracing::warn!` (EN + HU) +
  > `Ok(None)`, so the renderer falls back to the pre-PR-176
  > text-only header. The `Result<_>` return type is retained for
  > forward compatibility but every current arm returns `Ok(...)`,
  > making the function effectively infallible from the orchestrator's
  > perspective. The legal document renders; the branding asset
  > defects surface in the operator's log instead of in a 500.
  > Test `load_tenant_logo_malformed_png_returns_none_not_error`
  > pins the contract by writing `[0xde, 0xad, 0xbe, 0xef]` to
  > `logo.png` and asserting the render proceeds with `Ok(None)`
  > (pre-PR-185 this returned `Err(...)`).

- **S177 — `find_existing_id` + INSERT are not atomic.** 
  `apps/aberp/src/incoming_invoices.rs:429-504`: the idempotency check at line 429
  happens BEFORE `conn.transaction()` at line 475. Two concurrent ingests of the
  same `(tenant, supplier_tax_number, nav_invoice_number)` (the daemon racing
  with a manual `/sync-now`, or two daemon ticks if the cadence somehow
  overlaps) can both pass the check. The first INSERT succeeds; the second
  trips the UNIQUE constraint and returns `IngestError::Other(anyhow)` → 500
  to the caller, instead of `IngestOutcome::AlreadyExists` → 200. The audit
  ledger is fine (no entry written by the failing tx), but the operator/daemon
  sees a confusing 500. Recommended future PR: catch the UNIQUE constraint
  violation inside the INSERT arm and re-look-up the existing id, returning
  `AlreadyExists`.

- **S178 — synchronous DuckDB + chain-verify on the tokio runtime.**
  `apps/aberp/src/ap_sync.rs:281-305`: `ingest_incoming_invoice` is sync DuckDB
  (open + INSERT + audit-append + drop + reopen + verify_chain + sync_mirror).
  Called in a sequential for-loop inside an async function. A first-ever cycle
  on a tenant with 10K supplier invoices in NAV becomes ~10K blocking
  open/insert/verify cycles on the tokio worker thread, blocking every other
  HTTP request for the duration. Same shape in `S180`'s `process_digest`. Not
  a daily-cadence problem (steady-state has near-zero new invoices), but a
  real one for the boot tick on a tenant with a 30-day backlog or for the first
  S180 restore of a heavy year. Recommended future PR: wrap the per-digest
  body in `tokio::task::spawn_blocking`.

- **S180 — `already_restored` is O(N) per digest with a full audit-ledger walk + a
  fresh Ledger handle each call.** `apps/aberp/src/restore_from_nav_outgoing.rs:573-603`:
  every digest opens a new `Ledger`, calls `entries()` (full table scan), walks
  it backward. For 1000 digests in a year + 10K prior audit entries on the
  tenant, that's 1000 connection opens + 10M JSON decodes worst-case. Compounded
  by `process_digest` ALSO doing a `Connection::open` + a second `Ledger::open` for
  post-commit chain verify (3 connection opens per digest). The restore IS a DR
  operation (rare), so this is "operator waits a long time" not "data corrupts",
  but a 1000-invoice year could take minutes-to-tens-of-minutes. Recommended
  future PR: pre-load the set of already-restored `source_nav_invoice_number`s
  into a `HashSet<String>` ONCE at the top of `run`, and pass `&conn` through
  the loop instead of re-opening per digest.

- **S180 — backend has no equivalent of the SPA's "type RESTORE" ceremony.**
  `apps/aberp/src/serve.rs:6713-6738`: the route gates on `require_ready` +
  bearer + year-bounds. The literal `RESTORE` token gate lives ONLY in
  `apps/aberp-ui/ui/src/lib/restore-wizard.ts:67`. A buggy SPA build, a
  malicious extension, or a curl-with-bearer can POST `/api/restore-from-nav-outgoing`
  with no token. Blast radius is limited (idempotent, NAV is read-only, data
  lives in a separate `restored_invoice` table), so this is "the ceremony is
  cosmetic" not "data corrupts". The brief calls the ceremony
  "operator-discipline" — name it as SPA-side-only in the route's doc-comment
  if that's intentional, or add a `confirmation_token: String` field to the
  request body checked against `"RESTORE"` server-side.

- **S180 — `validate_year` uses UTC-derived "current year".**
  `apps/aberp/src/restore_from_nav_outgoing.rs:292-307` reads
  `now_utc.date().year()`. On NYE in Hungary (UTC+1 in winter), between
  23:00-23:59 CET local the validator already calls the next year "current".
  An operator triggering restore for year 2027 at 23:30 on 2026-12-31 CET
  would succeed (validator says current = 2027), then walk NAV and get zero
  digests. Mild surprise, not a bug. The SPA's `validateYearInput` at
  `restore-wizard.ts:43` passes `currentYear` from `new Date().getFullYear()`
  (local time) so SPA and backend can disagree on NYE. Worth one comment.
  > **ADDRESSED-BY-PR-183.** `validate_year` now computes `current_year`
  > via a fixed UTC+1 offset (Europe/Budapest in winter — the only window
  > with a year-flip, since DST runs late March to late October), aligning
  > the backend with the SPA's local-time `getFullYear()`. New test
  > `validate_year_nye_budapest_accepts_local_year` pins that at
  > 23:30 UTC on Dec 31 of year N (= 00:30 CET on Jan 1 of N+1) the
  > operator can type N+1 without being rejected as "future".
  > `month_window_december_covers_nye_budapest_invoice` adds a defence
  > pin that `month_window(YYYY, 12)` returns `YYYY-12-31` as the upper
  > bound (NAV's `<invoiceIssueDate>` is date-only, so the existing
  > calendar-arithmetic path already covered the invoice-loss concern).

- **Auth-order smell across the new AP routes (and pre-existing pattern).**
  `apps/aberp/src/serve.rs:6504-6517`: `handle_mark_incoming_irrelevant` uses
  `Json(body): Json<MarkIrrelevantRequest>` as a parameter, meaning axum parses
  the body BEFORE the function runs the `check_bearer_rejection`. An
  unauthenticated POST with a malformed body returns 400 (axum's JSON parse
  failure) rather than 401. Pre-existing pattern across `serve.rs` — likely
  not S177 specific — but worth a global pass to flip extractor order or wrap
  in a `Bearer<Json<T>>` extractor.

## 🟢 Missing test coverage / edge cases (backlog)

- **S172** — no test for what happens when notes contain a literal `\n`
  (multi-line textarea content). Behavior is well-defined (preserved through
  serde) but pinning it prevents future trim-line-feeds-aggressively regressions.
- **S172** — no test for the exact 50-entry limit boundary: if 51 unique notes
  exist and the operator requests `limit=50`, do we get the 50 most-recent
  including the boundary case? Manual reading says yes; pin would lock it.
- **S174** — `InvoiceLineFields.svelte` is used by Modify but NOT by Issue
  (intentional, documented). No HTML-output snapshot test pins that Modify's
  rendered DOM matches Issue's for the common subset of fields, so a future
  drift won't surface until the operator notices.
- **S175** — no test for a localStorage quota-exceeded write path (the helper
  has the catch + warn, but no test that asserts the catch fires and the
  operator-visible state stays consistent post-throw).
- **S176** — no test for a PNG with extreme aspect ratio (e.g., 1×10000 strip)
  to confirm the 50×50-pt placement matrix doesn't divide-by-zero or scale to
  invisible.
- **S177** — no test for the race condition between concurrent `find_existing_id` +
  INSERT (the bug flagged above). A unit test with two tokio tasks would catch
  the regression after a fix.
- **S177** — `transition_allowed` is unit-tested, but no integration test at the
  HTTP layer pins that `POST /mark-irrelevant` on a `Paid` row returns a 400
  with `InvalidTransition`. A future route-vs-graph drift would slip through.
- **S178** — no test for the parser's behaviour when NAV returns
  `available_page = 0` with non-empty `<invoiceDigest>` children (malformed but
  not impossible — defensive coverage is cheap).
- **S178** — no test for `compute_date_window` underflow at the epoch lower
  bound (the `?` on `checked_sub` is unreachable in practice but documented
  as a possible error path).
- **S180** — no test for the partial-commit-then-chain-verify-failure recovery
  path. The fix-flow: row + audit committed, chain-verify fails, error
  returned, operator re-runs, second run should detect the prior entry and
  skip. Unit test against a tampered ledger would prove the recovery loop.
- **S180** — no test for NYE-UTC-vs-CET boundary on `validate_year`.

## 💭 Architectural questions for Ervin

- **S173 year-source divergence.** Is the choice of `Entry.time_wall.year()` in
  annulment a deliberate "audit ledger is the source of truth" call, or an
  oversight from the original CLI-only annulment (where billing-DB access from
  this code path was intentionally avoided)? If deliberate, we need to commit
  to ALSO using `time_wall.year()` from `issue_invoice` etc. for consistency
  (currently they all use `issue_date.year()`). If oversight, fix annulment to
  match the others.

- **S180 + S178 chain-verify cost.** `Ledger::verify_chain` after each insert
  is O(N) over the full chain. A 1000-row restore is O(N²). Should we
  amortize — e.g., one chain verify per page, or per cycle, instead of per
  insert? Or accept that DR + first-cycle ingest are operator-paced "go drink
  coffee" operations and document the expected wait time?

- **S180 RESTORE token.** Should the ceremony move server-side, or is the SPA
  gate the intended layer? If the latter, the route's doc-comment should
  acknowledge that the ceremony is cosmetic from the backend's perspective.

- **AP module status-change audit shape.** The `IncomingInvoiceStatusChanged`
  payload carries only `ap_invoice_id`, not the
  `(supplier_tax_number, nav_invoice_number)` dedup tuple. If a future export
  needs cross-tenant traceability without joining against the (mutable)
  `ap_invoice` row, the payload would need extending. Worth deciding now or
  acknowledging the deferred decision.

- **Runbook & snapshot script coverage of new artifacts.** S177 introduces
  `~/.aberp/<tenant>/ap-artifacts/` and the `ap_invoice` table. S180
  introduces the `restored_invoice` table. `docs/CUTOVER_RUNBOOK.md`
  Appendix A "File and keychain inventory" does NOT mention either; the
  snapshot-prod.sh docstring doesn't mention them either. The script
  captures them incidentally (it tars all of `~/.aberp/<tenant>/`), but the
  operator-visible inventory is now incomplete. Worth a one-line doc add.

## ✅ Cross-cuts checked clean

- **S172 XSS on note rendering.** `NotesAutocomplete.svelte:203` renders
  `{suggestion}` as plain text (no `{@html}`). Svelte auto-escapes — a note
  containing `<script>` displays inert. ✓
- **S172 Hungarian special-char handling.** UTF-8 through serde JSON + DuckDB
  VARCHAR + `toLocaleLowerCase` round-trips Á/É/Ű/ő correctly. Pinned by the
  `dedupe_preserves_case` and `inv_003 " Köszönjük "` tests. ✓
- **S172 ARIA listbox correctness.** `aria-expanded`, `aria-selected`,
  `aria-autocomplete="list"` all wired. One minor wart: `id="notes-autocomplete-listbox"`
  is hardcoded — three NotesAutocomplete components on one page would
  duplicate the id, but the at-most-one-open-dropdown UX makes this invisible.
  Noting, not flagging.
- **S175 tab persistence (S179) vs S175 sort persistence.** Separate
  localStorage keys (`aberp:invoice-tab` vs `aberp:invoice-list:prefs`) and
  S175's validator discards corrupt blobs to default. A corrupted tab pref
  does not break the list, and vice versa. ✓
- **S180 NAV-as-DR (OUTBOUND) vs S178 daemon (INBOUND).** Both call
  `queryInvoiceDigest` but with opposite `InvoiceDirection`. NAV-side state is
  read-only for both. No collision. ✓
- **S178 daemon-collision risk.** DuckDB's file lock prevents two `aberp serve`
  processes from running against the same tenant DB simultaneously, so two
  daemons cannot tick the same tenant. ✓
- **S179 future-third-tab.** `App.svelte`'s tabbed switch is a two-value
  `if/else` plus `loadInvoiceTab` with a `LEGAL_TABS` closed-vocab — adding a
  third tab is an additive change in both files, and a legacy `outgoing`/`incoming`
  pref blob would NOT silently become the new third tab. ✓
- **S181 partner-kind filter not persisted — intentional.** PartnersList has
  NO kind facet today; S181's brief excludes adding one. The persistence shape
  carries only `{ filter: { needle } }` and the validator drops unknown
  fields, so a future PR can additively introduce a kind facet without a
  migration step. ✓ (matches brief's "separation, not consolidation" note.)
- **S176 logo file-lock safety.** A concurrent finder-copy mid-`fs::read` is
  detected as a malformed PNG by the decoder → loud error. No silent
  half-decode. ✓
- **S180 cross-tenant scoping in `already_restored`.** The defensive filter
  at line 588 (`entry.tenant_id.as_str() != tenant.as_str()`) is paranoid
  given the per-tenant DB convention, but harmless. ✓

## Notes for the next session

- The S173 annulment year-source bug is the highest-priority follow-on. A
  one-file PR that captures the issue-date from the `InvoiceDraftCreated`
  payload would close it.
- The S180 restore-perf issues are real but discoverable only on a heavy
  first-ever DR run. Worth flagging in the doc-comment + maybe a `tracing::info!`
  at the start of each month-walk so the operator sees progress.
- AP module v2 follow-ons (per-row queryInvoiceData fetch, partner/product
  extraction) are explicitly deferred in S177-S180 docs — no need to chase
  them in a review pass.
- Runbook + snapshot-script docstring need a one-line additive update to
  mention `ap-artifacts/` and the new tables. Pure docs; can ride with the
  next prod-prep PR.
