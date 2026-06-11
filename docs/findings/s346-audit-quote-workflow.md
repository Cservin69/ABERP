# S346 audit — Quote workflow + Auto-Quoting SPA panel

**Date:** 2026-06-11 · **Trigger:** Ervin's live observation — Auto-Quoting panel rows stuck at
`post · ? Ismeretlen / Unknown · parse priced-writeback ok JSON: <!doctype html>…` AFTER the
CloudFront routing fix landed. Verbatim: *"Audit this workflow by code, I do not think that is
production ready a lot of the things missing."* Scope extended mid-audit: *"there is no expand
view, not clickable so this ABERP UI on quotes needs a full functional audit which is wired to
the backend which is not. No sorting no delete/edit nothing"* and *"no accept, no deal button"*.

**Scope:** Part 1 (§1–§2) — end-to-end code audit of the 8-stage flow: customer CAD submit →
storefront classify → ABERP poll → price → PDF render → priced-writeback → storefront persist +
email → accept → DEAL. Part 2 (§3) — functional audit of the SPA Auto-Quoting operator panel
(`Auto-árazás folyamatban`, `PricingJobsList.svelte`) and its backend wiring. Adjacent SPA areas
(Invoices, Workshop, Dispatch) deliberately out of scope.
**Doc-only.** No code changed. Repos: ABERP @ `ae40f4d`, ABERP-site @ `bc3b3b7`.

---

## 0. Summary table

| # | Sev | One-line | Session |
|---|-----|----------|---------|
| F1 | 🔴 | No Content-Type validation on ANY storefront-facing HTTP parse — HTML accepted as "ok JSON" | S347 |
| F2 | 🔴 | Failure taxonomy is stringly-typed substring matching; everything unrecognised → `? Ismeretlen / Unknown` | S347 |
| F3 | 🔴 | SPA + docs advertise auto-retry for Transient failures; the scheduler never retries any Failed row | S348 |
| F4 | 🔴 | Default material `unknown` (and all legacy options) guarantees a Permanent failure with misleading "retry" copy; no customer clarification flow | S349 |
| F5 | 🔴 | Quote with no `.stl/.step/.stp` file is silently never priced — no job row, no audit event, no SPA surface | S348 |
| F6 | 🔴 | Every price is computed with hard-coded `machining_rate = 1.0 EUR/min` — customer-visible prices are placeholders | S350 |
| F7 | 🔴 | DEAL saga outputs are stub SO/WO ids surfaced as real chips; the draft-invoice path (Quotes-tab pickup) exists but is disjoint from DEAL — CORRECTED, see finding | S351 |
| F8 | 🟡 | `resp.text().await.unwrap_or_default()` silently swallows body-read failures on the writeback response | S347 |
| F9 | 🟡 | Multi-file quote: only the FIRST CAD file is priced; remaining files silently ignored, no audit record | — |
| F10 | 🟡 | PDF render failures have no classifier rule → land in the same `Unknown` bucket as transport garbage | S347 |
| F11 | 🟡 | Intake list non-2xx error drops the response body — the S342 diagnostics fix covered catalogue-push only | S347 |
| F12 | 🟡 | Retry after a tunables edit → new `feature_graph_hash` → storefront 409 `already_priced_with_different_hash` → Permanent dead-end with no remediation copy | — |
| F13 | 🟡 | No bulk retry / reconciliation sweep — after an infra incident every Failed row needs one operator click each | S348 |
| F14 | 🟡 | Secret rotation spans ≥3 uncoordinated surfaces (ABERP keychain, storefront env, CloudFront secret); mismatch only surfaces as Permanent rows | — |
| F15 | 🟡 | No correlation id beyond `quote_id`; storefront logs are unstructured `console.error` an operator never sees | — |
| F16 | 🟡 | No boot/cycle contract probe — a routing misconfig is only discovered by burning real customer jobs | S347 |
| F17 | 🟡 | ABERP never checks rendered-PDF size; storefront 413s at 5 MB → Permanent failure with no guidance | — |
| F18 | 🟡 | CAD download writes unvalidated bytes to disk — a misrouted response poisons the artifact as a fake `.step` | S347 |
| F19 | 🟡 | Storefront email rate-limit state is in-memory — restarts reset the 30/60s budgets | — |
| F20 | 🟡 | `error_reason` shows up to 1000 chars of raw HTML verbatim in the SPA table cell | — |
| F21 | 🟢 | Priced-writeback idempotency (feature_graph_hash replay → `{idempotent:true}`) is sound — verified |
| F22 | 🟢 | `catalogue-store.ts` error message still cites the OLD grade regex |
| F23 | 🟢 | DEAL token = first 8 chars of quote_id — deliberate, fine behind operator auth, document it |
| F24 | 🟢 | Storefront origin never serves HTML on API paths — the HTML Ervin saw was CDN-origin routing, not the Node app |

**Part 2 — SPA Auto-Quoting panel (§3):**

| # | Sev | One-line | Session |
|---|-----|----------|---------|
| U1 | 🔴 | No row click / expand / detail view — and no detail endpoint exists to feed one | S352 |
| U2 | 🟡 | No sorting — headers are plain text, server order fixed at `fetched_at DESC` | S352 |
| U3 | 🟡 | No filtering, no search box, no state-filter chips | S352 |
| U4 | 🟡 | No delete/archive — junk and test rows live in `quote_pricing_jobs` forever; no DELETE route | S353 |
| U5 | 🔴 | No edit/override — operator cannot fix material grade, customer email, or price; no PATCH route (this is the operator half of F4) | S349 |
| U6 | 🟡 | No bulk actions — no checkboxes, no bulk retry/delete (ties F13) | S348 |
| U7 | 🟡 | Retry is blunt — always resets to `fetched`, redoing extract+price+render+post even when only the writeback failed (interacts badly with F12) | S348 |
| U8 | 🟡 | No auto-refresh — one fetch at mount + manual `Frissítés / Refresh`; no polling interval, no SSE | S352 |
| U9 | 🟡 | No pagination — `list_jobs` is unbounded, the SPA renders every row | S352 |
| U10 | 🟡 | Error column is raw text dump — no severity/title/details-expand structure (ties F2/F20) | S347 |
| U11 | 🟢 | No state-chip legend or tooltips (Beérkezett/Visszaküldés/… meanings non-obvious) | — |
| U12 | 🟢 | Customer cell is inert text — no `mailto:`, no partner-profile link | — |
| U13 | 🟢 | Currency hard-coded EUR in header and engine; HUF/NAV conversion question deferred to the invoice step | — |
| U14 | 🟢 | No keyboard navigation (j/k, Enter-to-expand) | — |
| U15 | 🟡 | No per-row audit timeline — events exist in the ledger but no quote-scoped query endpoint (invoice detail already has this pattern) | S352 |
| U16 | 🔴 | No operator "Accept on customer's behalf" — and the storefront FORBIDS `approved` via Bearer by design; phone/email acceptance has NO path at all | S354 |
| U17 | 🟡 | No DEAL affordance on this panel, no cross-link to the Quotes-tab DEAL gate, no "resend quote email / fresh accept link" anywhere | S354 |
| U18 | 🟡 | No convert-to-invoice from this panel; pickup→draft exists on the Quotes tab but is disjoint from DEAL and its auto-quote coverage is unverified | S351 |
| U19 | 🟡 | No Decline/Lost workflow — backend is READY (storefront allows `rejected`, ABERP has `writeback_status`); zero UI; conversion rate untrackable | S353 |
| U20 | 🟡 | No per-row state-transition history — `updated_at` is overwritten in place; storefront `status_history` and ledger events exist but are unsurfaced | S352 |

**Tally: Part 1 — 7 🔴 / 13 🟡 / 4 🟢. Part 2 — 3 🔴 / 13 🟡 / 4 🟢. Combined: 10 🔴 / 26 🟡 / 8 🟢 (44 findings).**

---

## 1. Stage-by-stage trace (file:line evidence)

### Stage 1 — Customer submits CAD on storefront `/quote`

- Form: `ABERP-site/src/routes/quote/+page.svelte` — material dropdown populated from
  `/api/catalogue/materials` (lines 28–40); **default material value is `"unknown"`**
  (`+page.svelte:21`); fallback list when the catalogue fetch fails:
  `unknown, aluminum, steel, stainless, brass, plastic, other` (`+page.svelte:308–319`).
- Handler: `ABERP-site/src/routes/api/quote/+server.ts` — 13-format whitelist (lines 17–32),
  50 MB cap (line 13), max 10 files (line 14), per-format magic-byte validation via
  `src/lib/server/cad-validate.ts:1–313`, honeypot (lines 87–90), atomic
  `data/quotes/{id}/metadata.json` write (line 238), `status='received'` (lines 214–231).
- Material accepted if in the **legacy preference set** (lines 36–44: `unknown`, `aluminum`, …)
  OR the current catalogue grades (lines 129–136). ⚠️ The legacy set is NOT in ABERP's
  catalogue — see F4.
- Submission-received email fire-and-forget via `setImmediate` (lines 245–249).

### Stage 2 — Storefront classification + persistence

There is **no storefront-side material classification** beyond the validity check above —
`material_preference` is stored verbatim. Grade vocabulary: `src/lib/server/catalogue-store.ts:79`
(`/^[A-Za-z0-9][A-Za-z0-9 ._+/-]*$/`, S338 relaxation). The error message at
`catalogue-store.ts:101` still prints the pre-S338 regex (F22).

### Stage 3 — ABERP polls the storefront

Two pollers, both Bearer-authed from the OS keychain
(`apps/aberp/src/quote_intake_credentials.rs:30–32, 72` — service
`aberp.quote_intake.<tenant_id>`; base_url from `~/.aberp/<tenant>/seller.toml`,
`quote_intake_config.rs:40–51`):

1. **Pricing pipeline** — `apps/aberp/src/quote_pricing_pipeline.rs:224–252`
   `GET {base}/api/quotes?status=received` → `resp.json()` at lines 237–240 with **no
   Content-Type check** (F1) and the non-2xx arm at 234–236 returns
   `storefront list returned HTTP {status}` **without the body** (F11).
2. **Intake daemon** — `crates/aberp-quote-intake/src/transport.rs:48–60` (`status=approved`)
   and `:62–77` (single quote): same `.json()` with no Content-Type check (F1).

Per-quote CAD download: `quote_pricing_pipeline.rs:259–295`. Picks the **first**
`.stl/.step/.stp` (lines 264–271); any other composition → `Err("no CAD file on quote {qid}")`
→ swallowed as `tracing::warn!` at lines 247–249 (F5, F9). Downloaded bytes written to disk with
no validation (lines 293–295, F18).

### Stage 4 — Pricing

State machine `apps/aberp/src/quote_pricing_jobs.rs:45–59`:
`fetched → extracting → pricing → rendering → posting_back → posted`, with `failed` terminal.
`material_grade` is the storefront `material_preference` **verbatim**
(`quote_pricing_pipeline.rs:297`). A grade absent from the catalogue raises
`material grade `{grade}` is not in the catalogue snapshot`
(`crates/aberp-quote-engine/src/error.rs:13`) → `Permanent`
(`quote_pricing_pipeline.rs:1359`). Hard-coded
`DEFAULT_MACHINING_RATE_EUR_PER_MIN: f64 = 1.0` at `quote_pricing_pipeline.rs:1589` with the
documented gap at 1577–1588 (F6).

### Stage 5 — PDF render

`aberp_quote_pdf::render(&inputs)` at `quote_pricing_pipeline.rs:722`; success → `set_rendered`
(723–773) + `EventKind::QuotePricingRendered` (766); failure → `emit_failure(stage="render")`
(775–788) → same `Failed` bucket. `classify_failure` (1353–1412) has **zero render-stage
rules** → every render failure is `Unknown` (F10). No output-size guard (728–729) vs the
storefront's 5 MB PDF cap (F17).

### Stage 6 — Priced-writeback POST (the incident site)

`post_priced_writeback`, `quote_pricing_pipeline.rs:929–977`:

```rust
let status = resp.status();                                   // :961
let body_text = resp.text().await.unwrap_or_default();        // :962  ← F8
if !status.is_success() {
    return Err(anyhow!("priced-writeback HTTP {status} body={body_text}")); // :964
}
let parsed: PricedWritebackOk = serde_json::from_str(&body_text)
    .with_context(|| format!("parse priced-writeback ok JSON: {body_text}"))?; // :969–970 ← F1
```

A 200 with `text/html` (CDN routing the API path to the SPA origin) sails past the status check
and fails at `serde_json::from_str` — producing **exactly** Ervin's
`parse priced-writeback ok JSON: <!doctype html>…`. That reason matches no
`classify_failure` rule (1390–1410 only matches `http 4`/`http 5`/`timeout`/`connection`/`dns`
**inside the reason string**) → default `Unknown` at :1411 → SPA chip
`? Ismeretlen / Unknown` (`apps/aberp-ui/ui/src/lib/pricing-failure-kind.ts:74–79`). F1+F2.

Storefront handler `ABERP-site/src/routes/api/quotes/[id]/priced/+server.ts`: Bearer auth (:174),
6 MB body / 5 MB PDF caps (:21, :25, :179–241), meta validation (:103–171), state machine with
same-hash idempotent replay → `{idempotent:true}` and new-hash → 409
`already_priced_with_different_hash` (:248–321), atomic PDF+metadata persist (:324–353).
**Every response path is `json()`** (:368) — the origin never emits HTML here (F24, F21).

### Stage 7 — Storefront persists + emails customer

`sendPricedReadyEmail` from the priced handler, try-caught, failure swallowed with
`console.error` (`priced/+server.ts:359–366`; `src/lib/server/email.ts:462–509`): PDF attached,
HMAC accept link with 30-day expiry baked into the signature
(`src/lib/server/quote-token.ts:99–136`). Outbox per ADR-0009 at
`/home/aberp/data/email-outbox` with stale-claim recovery
(`src/lib/server/email-outbox.ts:284–318`). Rate-limit state is in-memory
(`email.ts:66–127`, F19).

### Stage 8 — Accept → DEAL → "invoice"

Accept page re-verifies HMAC then expiry (`src/routes/q/[id]/accept/+page.server.ts:131–204`),
requires the literal `ACCEPT` (:145), idempotent replay (:155–160), atomic
`status='approved'` write (:200). ABERP's intake daemon polls `status=approved`
(`transport.rs:48–60`). The operator then runs the DEAL saga in ABERP
(`apps/aberp/src/quote_deal.rs`): REFRESH ack token (:69), DEAL token = first 8 chars of the
quote id (`expected_deal_token`, :187–191, F23), CAS single-use guard, **stub ids**:

```rust
let sales_order_id = format!("so_{}", Ulid::new());   // quote_deal.rs:384
let work_order_id  = format!("wo_{}", Ulid::new());   // quote_deal.rs:385
```

Material commit is real (`material_inventory::commit_material_in_tx`, :522–592). The word
"invoice" does not appear in `quote_deal.rs` (verified by grep). **The pipeline ends here** (F7).

---

## 2. Findings — detail

### F1 🔴 No Content-Type validation on any storefront-facing JSON parse

- `quote_pricing_pipeline.rs:969–970` (priced-writeback 200 path) — the incident.
- `quote_pricing_pipeline.rs:237–240` (`?status=received` list).
- `crates/aberp-quote-intake/src/transport.rs:55–58` and `:73–76` (approved list, single quote).
- `quote_pricing_pipeline.rs:293–295` — CAD download bytes written to disk unchecked (F18).

The log literally says **"ok"** and then prints HTML. Any CDN/proxy/SPA-fallback misroute that
returns 200 is accepted as an application-level success right up to the parse, and the resulting
error is indistinguishable from a storefront contract change.

**What S347 would do:** before every `.json::<T>()` / `serde_json::from_str`, check
`response.headers().get(CONTENT_TYPE)` starts with `application/json`; on mismatch return a typed
`NonJsonResponse { got_content_type, http_status, body_excerpt }` (body capped ~300 chars).
Classifier: `NonJsonResponse` → its own kind (see F2), operator copy
`🛑 Útvonal-hiba / Routing misconfigured — a storefront HTML-t adott vissza JSON helyett`.
Tests: 200+`text/html` body at the writeback site, the list site, and the intake transport must
classify as routing-misconfig, never `Unknown`. CAD download: require the expected binary type
or at minimum reject `text/html` bodies.

### F2 🔴 No transport-vs-app error taxonomy — `? Ismeretlen / Unknown` is the catch-all

`classify_failure` (`quote_pricing_pipeline.rs:1353–1412`) substring-matches lowercased
**anyhow Display strings** (`"http 401"`, `"http 4"`, `"connection"`, `"dns"`…). Consequences:

- `parse priced-writeback ok JSON: …` → `Unknown` (default :1411).
- Render errors → `Unknown` (no rule, F10).
- Matching depends on reqwest's error wording — `"connection"`/`"dns"` are incidental
  substrings of a third-party Display impl, one reqwest upgrade away from misclassification.
- SPA shows `? Ismeretlen / Unknown` (`pricing-failure-kind.ts:74–79`) — tells the operator
  nothing about whether the customer payload, the network, the CDN, or the renderer failed.

**What S347 would do:** make the post stage return a typed error enum at the source —
`WritebackError { Timeout, Dns, Connect, TlsError, Unauthorized, AppRejected{status, body_excerpt},
AppErrored{status, body_excerpt}, NonJsonResponse{..}, BodyReadFailed }` — and have
`classify_failure` match on the variant, not the string (string fallback kept for legacy rows).
Bilingual operator labels per variant in `pricing-failure-kind.ts` (e.g. `Unauthorized` →
`🛑 Hitelesítési hiba / Auth failed — token mismatch`, `NonJsonResponse` → routing copy above).
Pin a test: every variant maps to a non-`Unknown` kind.

### F3 🔴 Advertised auto-retry does not exist

Three surfaces promise it:
- `quote_pricing_jobs.rs:82–84` — "Lets the daemon decide whether to auto-re-enqueue (Transient)…"
- `quote_pricing_jobs.rs:90–100` — Unknown "Treated as Transient up to UNKNOWN_AUTO_RETRY_CAP
  auto-retries, then frozen" (`UNKNOWN_AUTO_RETRY_CAP: u32 = 3` at :100 — **never read by the
  scheduler**).
- SPA Transient chip: `↻ Auto-retry / Átmeneti hiba` (`pricing-failure-kind.ts:69–73`).

Reality: `next_actionable_job` selects only
`state IN ('fetched','extracting','pricing','rendering','posting_back')`
(`quote_pricing_jobs.rs:762–774`); the doc-comment at :749–761 admits "Auto-retry of Transient
failures is deliberately NOT wired". `quote_pricing_pipeline.rs:1345–1347` repeats the false
claim. So a 30-second storefront blip strands a customer quote until an operator happens to look
at the Pricing tab — while the chip tells that operator it will retry itself.

**What S348 would do:** wire bounded auto-retry in the daemon for `failure_kind IN
('transient','unknown')` with `attempt_n < UNKNOWN_AUTO_RETRY_CAP` (or a transient-specific cap),
exponential delay derived from `updated_at`, emitting an audit event per auto-retry (preserves
the "audit-visible retry" objection at :758–761 — the event IS the durable record). Failing
that, the cheaper truth-fix: change the chip copy to `🛑 Operátor újrapróbálás szükséges /
Operator retry required (transient)` and delete the three lying doc-comments + the dead constant.
Either way: test that a Transient row's lifecycle matches whatever the chip says.

### F4 🔴 Default material `unknown` guarantees Permanent failure; no clarification flow

Path: `+page.svelte:21` default `"unknown"` → accepted by `api/quote/+server.ts:129–136` (legacy
set :36–44) → copied verbatim (`quote_pricing_pipeline.rs:297`) → engine
`is not in the catalogue snapshot` (`aberp-quote-engine/src/error.rs:13`; pinned test
`quote_pricing_pipeline.rs:2693`) → `Permanent` (:1359) → chip
`🛑 Operátor művelet szükséges / Operator retry required` (`pricing-failure-kind.ts:66`).

- Retry can never succeed — the copy is wrong for this failure class (MarginFloor got dedicated
  copy in S297/F6; MaterialNotInCatalogue did not).
- The legacy options `aluminum/steel/stainless/brass/plastic/other` fail identically — the
  ENTIRE fallback dropdown (shown whenever the catalogue fetch fails, `+page.svelte:308–319`)
  produces dead-on-arrival quotes.
- There is **no customer clarification flow**: no email asking the customer to pick a grade, no
  storefront status, nothing — grep of `email.ts` kinds shows only
  `submission-received / priced-ready / accepted-confirmation / operator-notify`
  (`email.ts:78–82`).

**What S349 would do:** (a) dedicated classifier-driven copy:
`🛑 Anyag-egyeztetés szükséges / Material clarification required — contact customer` keyed on
the `is not in the catalogue` rule (`pricing-failure-kind.ts`, same pattern as
`MARGIN_FLOOR_HINT` :36–41); (b) storefront: when material is `unknown`/legacy, set a
`needs_material_clarification` flag the operator list surfaces; (c) v2: a
`material-clarification` outbox email kind with a signed link letting the customer pick from the
live catalogue. (a) is one session; test: job with grade `unknown` shows the new copy, not
"retry required".

### F5 🔴 Quotes without an `.stl/.step/.stp` file silently never priced

`enqueue_one` (`quote_pricing_pipeline.rs:264–271`) errors `no CAD file on quote {qid}`; caller
maps it to `tracing::warn!` (:247–249). No job row, no audit event, no SPA row — the quote stays
`received` on the storefront, gets re-fetched and re-warned **every poll cycle forever**, and the
customer (who passed the storefront's 13-format whitelist with, say, an `.iges` or `.dxf`)
receives a "we got your submission" email and then silence. Direct CLAUDE.md rule-12 violation:
completed-looking cycle, records silently skipped.

**What S348 would do:** insert the row anyway and fail it loudly — create the job and immediately
`emit_failure(stage="extract", reason="unsupported file extension: no .stl/.step/.stp among N
files")` so the existing `unsupported file extension` rule (:1387–1388) classifies it Permanent
and it lands in the SPA + audit ledger. One test: a quote with only `.iges` produces a visible
Failed row, not a log line.

### F6 🔴 Hard-coded `machining_rate = 1.0 EUR/min` — production prices are placeholders

`quote_pricing_pipeline.rs:1589` (`DEFAULT_MACHINING_RATE_EUR_PER_MIN: f64 = 1.0`), documented
gap at :1577–1588 ("wrong-but-monotonic" until a `machining_rate` column lands in
`quoting_parameters`). Customers receive **real PDFs with prices derived from a placeholder
rate**. Flagged at S279 ship as "first follow-up"; still open 9 sessions later. A workflow can't
be production-ready while its core output is knowingly wrong.

**What S350 would do:** add `machining_rate_eur_per_min` to the `quoting_parameters` table +
Quoting Parameters SPA editor + engine plumb-through; boot-WARN (and AMBER SPA hint) while the
value is absent/defaulted. Test: changing the rate changes `total_price_eur`.

### F7 🔴 DEAL saga outputs are stubs; the invoice bridge exists only as a disjoint pickup path

**CORRECTION (same-day, during the §3 panel audit):** the first version of this finding claimed
"the operator re-types everything into the invoicing module by hand" — that is wrong. A
quote→draft-invoice path DOES exist: the Quotes-tab pickup button
(`POST /api/quotes/:quote_id/pickup-as-draft`, `serve.rs:3326–3330`; `QuotesList.svelte:87–107`,
S255/PR-244) mints a `drf_` draft and the intake service writes `invoiced` back to the
storefront (`crates/aberp-quote-intake/src/service.rs:302, :372`).

What remains true and 🔴:

- `quote_deal.rs:384–385` mints `so_<ulid>` / `wo_<ulid>` **placeholders** (design comments
  :10–22) and the SPA surfaces them as real-looking SO/WO chips after DEAL
  (`QuotesList.svelte:152–163`) — an operator can quote a phantom `so_…` reference to a customer.
- DEAL and pickup are **two disjoint operator actions with no link between them**: the DEAL saga
  neither creates nor references the draft invoice; "invoice" does not appear in `quote_deal.rs`
  (verified by grep).
- Whether the pickup's prepared-draft JSON carries the **auto-quote** pricing breakdown
  (`breakdown_json` → draft line items) is **unverified** in this audit — the pickup path predates
  the auto-quoting arc (S211/S255 vs S279). Flagged for verification before relying on it.

**What S351 would do:** verify pickup-draft coverage for auto-quote rows (breakdown → line
items); link DEAL → draft (DEAL outcome carries `drf_id` or creates the draft itself behind the
existing single-use CAS); stop rendering stub SO/WO ids as chips until the modules exist (or
label them `placeholder`); `QuoteInvoiceDrafted` EventKind.

### F8 🟡 Writeback body-read failure silently becomes empty string

`quote_pricing_pipeline.rs:962` — `resp.text().await.unwrap_or_default()`. A mid-body connection
drop yields `priced-writeback HTTP 200 body=` … then a JSON parse error on `""` → `Unknown`.
Fold into the S347 typed-error work: `BodyReadFailed` variant.

### F9 🟡 Multi-file quotes: first CAD only, silently

`quote_pricing_pipeline.rs:261–271` ("v1 is single-CAD-per-quote"). Deliberate v1, but invisible:
no audit payload field, no SPA hint, no customer-facing note that 9 of 10 uploaded files were
ignored. Minimum: count skipped files in `QuotePricingFetchedPayload` and show `1/N fájl árazva`
in the SPA row.

### F10 🟡 Render failures classified `Unknown`

No `stage == "render"` arm in `classify_failure` (:1353–1412). A deterministic render bug (bad
glyph, zero-page layout) shows the same `? Ismeretlen` as CDN garbage, and per F3 nothing
retries it anyway. S347: render errors → `Permanent` by default (deterministic input → same
output), with the typed-variant work.

### F11 🟡 Intake list non-2xx drops the response body

`quote_pricing_pipeline.rs:234–236` returns `storefront list returned HTTP {status}` — no body
excerpt, no content-type. S342 added exactly this diagnostic to catalogue-push after the same
class of opaque failure; the lesson didn't propagate to the other two clients
(`transport.rs:52–53` likewise). Same S347 sweep.

### F12 🟡 Retry-after-tunable-edit lands in a 409 dead-end

Operator edits tunables → Retry resets the job to `fetched` (`quote_pricing_jobs.rs:667–696`) →
re-price yields a NEW `feature_graph_hash` → storefront 409
`already_priced_with_different_hash` (`priced/+server.ts:248–321`) → `http 4` → `Permanent`
(:1399–1400) with no copy telling the operator the quote is already priced under the old hash.
Needs either dedicated classifier copy (the 409 body code is machine-readable) or a documented
operator runbook step.

### F13 🟡 No bulk retry / post-incident reconciliation

Retry is one POST per row (`serve.rs:3285`, handler :16885–16915 with per-row UUID guard). After
the CloudFront incident, every affected row stays red until individually clicked — which is
precisely why Ervin still sees `<!doctype html>` rows an hour after the infra fix. S348 (with
F3): "Retry all transient/unknown" bulk action + daemon sweep on boot.

### F14 🟡 Secret rotation spans ≥3 uncoordinated surfaces

Bearer: ABERP keychain `aberp.quote_intake.<tenant>` (`quote_intake_credentials.rs:30–32,72`) vs
storefront `ABERP_SITE_ADMIN_TOKEN` env (`src/lib/server/auth.ts:27–34`). CDN:
`CLOUDFRONT_SHARED_SECRET` env checked in `hooks.server.ts:70–75` (ABERP only sends the
CloudFront secret on catalogue-push, `storefront_origin_secret.rs` — the quote daemons ride the
CDN config). Catalogue-push origin secret: separate keychain entry (S339 runbook). No versioned
dual-accept window; a rotation done in the wrong order = every quote job `Permanent http 401`
until both sides agree (S309 F18 territory — still open for this pair). Needs a rotation runbook
at minimum.

### F15 🟡 No cross-system correlation id / structured storefront logs

`quote_id` is the only join key. Storefront failures are `console.error` strings
(`api/quote/+server.ts:247`, `priced/+server.ts:362`, `accept/+page.server.ts:179`) visible only
via SSH+journalctl; ABERP side has good per-step audit events (see F24 list) but no
request/trace id is propagated in either direction. Pilot-acceptable; name it so it's a decision
not an accident.

### F16 🟡 No contract/health probe before burning customer jobs

ABERP discovers a broken route/auth/CDN config only when a real job fails (and per F3, sticks).
A cheap cycle-start probe — `GET {base}/api/quotes?status=received` is already that, but its
failure is a cycle error (retried with backoff, :1027–1034) while **writeback** failures are
per-job and terminal. S347: a boot-time + on-config-change probe that validates status AND
`application/json` content-type AND auth on the writeback path (e.g. HEAD/OPTIONS or a dry-run
endpoint), surfaced as the existing Maintenance-card pattern (S342 precedent).

### F17 🟡 No ABERP-side guard on rendered PDF size

`quote_pricing_pipeline.rs:728–729` records `pdf_size` but enforces nothing; storefront caps PDF
at 5 MB / body 6 MB (`priced/+server.ts:21,25`). Oversized render → 413 → `Permanent` with no
hint. Cheap fix: check `bytes.len()` against the ADR-0004 cap at render time and fail with a
self-explanatory reason.

### F18 🟡 CAD download bytes unvalidated

`quote_pricing_pipeline.rs:293–295` writes whatever the storefront (or a misrouting CDN)
returned. An HTML body becomes a poisoned `.step` artifact whose extract failure
(`step file …` rule :1378) reads as a customer data-quality problem. Covered by the F1 sweep
(reject `text/html`, minimal magic-byte sniff — `cad-validate.ts` already has the logic
storefront-side).

### F19 🟡 Email rate-limit state in-memory

`email.ts:66–127` — restart clears global 30/60s and per-recipient budgets. Restart-loop =
unbounded sends within the loop cadence. Pilot-acceptable; note for SaaS.

### F20 🟡 Raw HTML in the SPA error cell

`error_reason` capped at 1000 chars (`quote_pricing_jobs.rs:863`,
`quote_pricing_pipeline.rs:1194,1456,1481`) and rendered verbatim
(`PricingJobsList.svelte:279`). Svelte escapes it (no XSS), but a kilobyte of `<!doctype html>`
in a table cell is operator noise. The F1 `body_excerpt` cap (~300 chars) + typed label fixes
this for free.

### F21 🟢 Idempotency verified sound

Seed-list #3 checked and **clear**: replay of the same `feature_graph_hash` is a no-op
`{idempotent:true}` (`priced/+server.ts:248–321`), ABERP distinguishes it
(`quote_pricing_pipeline.rs:966–975`, test :2354), POST is safe to repeat. The 409
different-hash arm is the residual sharp edge (F12).

### F22 🟢 Stale regex in catalogue error message

`catalogue-store.ts:101` prints `/^[A-Z][A-Z0-9_]*$/` while :79 enforces the relaxed S338
pattern. One-line doc fix.

### F23 🟢 DEAL token is the quote-id prefix

`expected_deal_token` (`quote_deal.rs:187–191`) = first 8 chars of the quote id. Deliberate
[[hulye-biztos]] confirmation gesture behind operator auth, not a secret — fine; document so a
future security pass doesn't "fix" it.

### F24 🟢 What's already good

- Audit coverage is genuinely strong: `QuotePricingFetched/Extracted/Priced/Rendered/Posted/
  Failed/FailureClassified/DaemonPanicked`, DEAL saga events, email-outbox events, re-render
  events (`crates/audit-ledger/src/entry/event_kind.rs`; success+failure per stage). The one
  hole is F5 (the never-enqueued quote emits nothing).
- Storefront security posture: timing-safe comparisons everywhere (`auth.ts:15–19`,
  `quote-token.ts:92–96`, `hooks.server.ts:73`), HMAC-with-baked-expiry accept links, atomic
  tmp+rename persistence, state-machine 409s, all-JSON API responses. The HTML Ervin saw could
  not have come from the Node origin (403 path is `text` plain, `hooks.server.ts:74`) — it was
  CDN routing to the SPA origin, consistent with the CloudFront fix resolving the curl probe.

---

## 3. SPA Auto-Quoting operator panel — functional audit (scope extension)

### 3.0 Surface inventory

The panel is `apps/aberp-ui/ui/src/routes/PricingJobsList.svelte` (403 lines), heading
`Auto-quoting pipeline / Auto-árazás folyamatban` (:117). It was **designed read-only** — the
file header says so (:2–6: "Read-only operator view … the SPA shows progress + the per-row
'Retry' button on Failed rows"). Its complete backend surface is **three routes**
(`apps/aberp/src/serve.rs`):

- `GET /api/quote-pricing-jobs` (:3280–3283) → `list_jobs` (`quote_pricing_jobs.rs:699–709`),
  fixed `ORDER BY fetched_at DESC`, **no LIMIT, no params**.
- `POST /api/quote-pricing-jobs/:quote_id/retry` (:3284–3287).
- `GET /api/quote-pipeline/status` (:3293–3296) — daemon health for the empty-state cards.

That's it. No detail GET, no PATCH, no DELETE, no bulk route, no audit-trail route. Every
"missing affordance" below is therefore missing **end-to-end**: the UI control does not exist
AND the endpoint it would call does not exist (exceptions noted per finding).

For fairness: the **adjacent Quotes tab** (`QuotesList.svelte` — "operator-pickup queue for
approved quotes awaiting DEAL", `PricingJobsList.svelte:8–10`) DOES have operator verbs: pickup
→ draft invoice (:87–107), DEAL gate (`QuoteDealGate.svelte:178–193`), dead-letter retry-parse
(:112–123), dismiss-as-irrelevant (:126–137). The gaps below are about the Auto-Quoting panel
and its missing cross-links to those verbs.

### U1 🔴 No row expand / detail view

Rows render at `PricingJobsList.svelte:240–300`; there is **no `onclick` on `<tr>`**, no detail
route, no drawer — the only interactivity on a row is the hover tooltip carrying the full quote
id (`title={row.quote_id}`, :243) and the Retry button on Failed rows (:286–298). The list
payload (`PricingJobRow`, `quote_pricing_jobs.rs:766–769`) is all the operator can ever see —
notably **NOT** included: `breakdown_json`, the FeatureGraph, the CAD filename/path, the
rendered PDF, the full (uncapped) error context, timestamps other than `fetched_at`/`updated_at`.

What a detail view should show: full submission metadata + customer contact, CAD file name and
extracted feature summary (`get_job_artifacts` already reads FeatureGraph + breakdown + CAD path,
`quote_pricing_jobs.rs:813+` — the backend read exists, it's just not routed), pricing breakdown
table, PDF preview/download, full error text, and the per-row audit timeline (U15).

**What S352 would do:** `GET /api/quote-pricing-jobs/:quote_id` returning row + artifacts +
quote-scoped ledger events (the invoice module already has exactly this shape — "single invoice
plus its full audit-ledger trail", `serve.rs:37–39`); row click opens a detail drawer; PDF
served from the artifact dir.

### U2 🟡 No sorting

`<thead>` at :227–237 — plain `<th>` text, no click handlers, no sort state anywhere in the
component, and the endpoint takes no sort params (`list_jobs`, fixed `ORDER BY fetched_at DESC`,
`quote_pricing_jobs.rs:709`). Recommended: client-side sort on Ref / Customer / Material / Qty /
State / Price / Updated (rows are already fully loaded — U9 — so no backend change needed until
pagination lands).

### U3 🟡 No filtering / search

No search box, no state-filter chips, no failure-kind filter — confirmed by reading the whole
component (the only inputs are the Refresh and Retry buttons, :123–129, :286–298). Minimum
useful: state chips (`Sikertelen / Failed` first) + free-text on customer/material/ref.

### U4 🟡 No delete / archive

No DELETE/archive route exists for `quote_pricing_jobs` (route table `serve.rs:3280–3287`), no
UI control. A junk submission, a duplicate, or a test row stays in the table forever — Ervin's
test rows from 2026-06-08/11 included. Contrast: the intake log already has exactly this verb
(`POST /api/quote-intake/:quote_id/mark-irrelevant`, `serve.rs:3321–3324`). **What S353 would
do:** mirror it — soft archive (`archived_at` column, not row delete: the ledger references
quote ids), filtered out of the default list, `QuotePricingJobArchived` EventKind.

### U5 🔴 No edit / override (the operator half of F4)

No PATCH route, no inline edit. Concretely blocked operator actions:
- **Fix material grade** — the F4 dead-end: a `unknown`/`aluminum` row fails Permanent; the only
  code-level remedy is the customer re-submitting through the storefront. An operator who
  clarified the material by phone cannot apply it. (`material_grade` is a plain column,
  `quote_pricing_jobs.rs:767`; retry re-prices from the stored row — an edited grade would flow
  through naturally.)
- **Fix customer email typo** — `customer_email` (:767) drives nothing ABERP-side today, but
  flows into DEAL-side records.
- **Price override** — no manual-price path; MarginFloor rows (F12 copy) can only be fixed by
  editing global tunables, never per-quote.

**What S349 would do** (bundled with F4): `PATCH /api/quote-pricing-jobs/:quote_id` accepting
`material_grade` (validated against `quoting_materials`), emitting a
`QuotePricingJobAmended` EventKind with old→new values, then auto-triggering the existing retry.

### U6 🟡 No bulk actions

No checkboxes, no select-all, no bulk retry/archive. Ties directly to F13 — this is the UI half
of the post-incident recovery gap. Same S348 cut.

### U7 🟡 Retry is a blunt full-redo

`retry_job` unconditionally resets `state → 'fetched'` (`quote_pricing_jobs.rs:678–684`), so the
daemon redoes extract → price → render → post (the CAD artifact on disk is reused — the
storefront fetch is the only stage not repeated). Consequences:

- A `stage=post` failure (pricing was FINE — Ervin's incident class) re-runs extraction and
  pricing for nothing.
- Worse: if tunables changed since the original run, the re-price yields a **new**
  `feature_graph_hash` → storefront 409 `already_priced_with_different_hash` → Permanent
  dead-end (F12). A writeback-only retry would have replayed the original hash and hit the
  idempotent 200 path (F21).

**What S348 would do:** stage-aware retry — `error_stage='post'` resumes at `posting_back` with
the stored artifacts (`get_job_artifacts` already returns breakdown + PDF inputs); everything
else resets to `fetched` as today.

### U8 🟡 No auto-refresh

One fetch at mount (`onMount`, :36–38) + the manual `Frissítés / Refresh` button (:123–129,
`data-testid="pricing-jobs-refresh"`). No `setInterval`, no SSE/WebSocket. The backend daemon
advances jobs every `poll_cadence_secs` (the empty-state card even prints it, :211–214) while
the table silently goes stale — an operator watching a row "in flight" sees nothing move.
Minimum: `setInterval(refresh, 30_000)` while the tab is visible; the endpoint is a cheap
single-table read.

### U9 🟡 No pagination

`list_jobs` has no LIMIT (`quote_pricing_jobs.rs:699–709`) and the SPA renders every row
(:240). At pilot volume fine; at 100+ rows the table becomes unusable and the payload grows
unbounded (each Failed row can carry a 1000-char error_reason). Cursor on `fetched_at` +
archived-row exclusion (U4) is the natural shape.

### U10 🟡 Error column is an unstructured dump

:266–283 — stage line, failure-kind badge, then raw `error_reason` (:279). Already covered as
F2/F20 backend-side; the UI half: severity badge + short typed title + collapsible details is
what the S347 typed-variant work should render into.

### U11 🟢 No state legend / tooltips

`stateLabel` chips (:75–94) are the only explanation; `Beérkezett / Visszaküldés / Sikertelen`
meanings and the pipeline order are not discoverable in-UI. A one-line legend strip or chip
tooltips ("Visszaküldés = priced result being POSTed back to the storefront") closes it.

### U12 🟢 Customer cell is inert

:248–251 — name + email as plain `<div>`s. Minimum `mailto:{row.customer_email}`; better: link
to the partner record when the email matches an existing partner.

### U13 🟢 Currency is hard-coded EUR

Header `Ár / Price (EUR)` (:233), `total_price_eur` is the only price field
(`quote_pricing_jobs.rs:768`), engine prices in EUR throughout. Consistent today (storefront is
EUR-denominated), so 🟢 — but note the open question it defers: ABERP invoicing is HUF/NAV-land,
and no EUR→HUF conversion exists anywhere in the quote→invoice path (surfaces at F7/U18).

### U14 🟢 No keyboard navigation

No `tabindex` on rows, no j/k bindings, no Enter-to-expand (nothing to expand — U1). Operator
productivity polish once U1 exists.

### U15 🟡 No per-row audit timeline

The ledger has rich per-quote events (F24 list: Fetched/Extracted/Priced/Rendered/Posted/Failed/
FailureClassified + DEAL + re-render events) but there is **no quote-scoped query endpoint** —
the route table has nothing under `/api/quote-pricing-jobs` beyond list+retry, while the invoice
module already ships "just the audit-ledger entries for one invoice" (`serve.rs:37–39`). S352:
same pattern keyed on `quote_id`, rendered in the U1 detail drawer. This also answers U20.

### U16 🔴 No operator "Accept on customer's behalf"

Ervin: *"no accept"*. Reality is worse than a missing button: **the path is forbidden
end-to-end.**

- ABERP UI: no accept control on either the Auto-Quoting panel or the Quotes tab (the Quotes tab
  only ever SEES a quote after the customer accepted — the intake daemon polls
  `status=approved`, `transport.rs:48–60`).
- Storefront: the Bearer-authed status endpoint **rejects** `approved` from any state —
  `forbidden_transition: approved is only settable by the customer accept POST`
  (`src/routes/api/quotes/[id]/status/+server.ts:66–71`), a deliberate ADR-0005 design (typed
  ACCEPT is the only path, comment :12–34).

So when a customer accepts by phone/email-reply — routine for HU SMB machining customers — the
operator has NO way to complete the loop: the quote sits `quoted` until the 30-day accept link
expires, and it never reaches the Quotes tab, DEAL, or an invoice.

**What S354 would do:** this needs an ADR-0005 amendment, not a quiet bypass — add a distinct
`operator_accepted` transition on the storefront (Bearer-authed, requires a recorded
`acceptance_channel` + free-text evidence note, lands in `status_history` and a dedicated audit
event on BOTH sides), surfaced in ABERP as an "Elfogadás rögzítése / Record acceptance" action
with a typed confirmation in the [[hulye-biztos]] style. The deliberate typed-ACCEPT property
("customer consent is customer-typed") is preserved by making the operator variant visibly
distinct in the data model, never spoofing the customer path.

### U17 🟡 No DEAL affordances on this panel; no link-refresh anywhere

Ervin: *"no deal button"*. Two distinct gaps:

1. **On this panel:** no DEAL button and no cross-link. The DEAL gate exists only on the Quotes
   tab (`QuoteDealGate.svelte:178–193`, wired in `QuotesList.svelte:144–173`) and only for rows
   the intake daemon staged after customer acceptance. A `posted` row on the Auto-Quoting panel
   is a dead end UI-wise — the operator must know to go look in the Quotes tab later, with
   nothing linking the two rows for the same quote_id. Minimum: on `posted` rows, a chip/link
   "Elfogadásra vár / Awaiting customer accept" that becomes "→ Ajánlatok / Quotes tab" once the
   intake row exists.
2. **Link lifecycle:** the customer accept link is HMAC-signed with a 30-day expiry baked in
   (`quote-token.ts:99–136`, `ACCEPT_TOKEN_LIFETIME_MS` :27). There is **no re-issue / resend
   affordance in either repo** — no storefront endpoint regenerates the priced-ready email or a
   fresh signed link (grep: the only "re-send" hit is a comment in `accept/+page.server.ts:156`),
   and ABERP has no button calling one. Expired link = customer must re-submit from scratch.
   S354: storefront `POST /api/quotes/:id/resend-priced` (Bearer, only from `quoted`,
   re-enqueues the priced-ready email with a fresh `ts`/`sig`), ABERP button next to the U17.1
   chip.

### U18 🟡 No convert-to-invoice from this panel (see corrected F7)

The pickup→draft verb exists (`serve.rs:3326–3330`) but only on the Quotes tab, only after
customer acceptance, and disjoint from DEAL (F7 correction). From the Auto-Quoting panel there
is no path at all. Once U16/U17 land, the natural row lifecycle ends with the same pickup verb
surfaced here. Blocked on F7's verification that pickup drafts carry the auto-quote breakdown.

### U19 🟡 No Decline / Lost workflow

The backend is **already capable on both sides** — only the UI is missing:

- Storefront allows `rejected` from any non-terminal state, explicitly documented as
  "operator-side decline" (`status/+server.ts:91–98`, rules comment :26).
- ABERP has the transport verb (`writeback_status`, `transport.rs:79–89`) — currently used only
  for `invoiced` (`service.rs:302, :372`).

No UI control exposes it anywhere, there is no decline-reason capture, and lost quotes are
indistinguishable from pending ones — conversion rate (quoted → accepted) is untrackable. S353:
"Elutasítva / Mark declined" with a closed-vocab reason (+free text), writeback `rejected` with
the reason in `notes`, `QuoteDeclined` EventKind, declined rows counted in a small funnel strip
(beérkezett → árazva → elfogadva → számlázva).

### U20 🟡 No per-row state-transition history

`quote_pricing_jobs` keeps only the current state — `updated_at` is overwritten in place and
retry even NULLs the error fields (`retry_job` UPDATE, `quote_pricing_jobs.rs:678–684`), so
after a retry the prior failure is gone from the row (the ledger keeps it, but see U15: there's
no way to view that). The storefront keeps a proper `status_history` array
(`quote-store.ts:13–18`) that ABERP never displays. Who moved a row (auto pipeline vs operator
click) is recorded in ledger actor fields but likewise unsurfaced. Resolution = U15's
quote-scoped ledger endpoint; no new write-side work needed.

---

## 4. What production-readiness means for this workflow

For Ervin to onboard a customer with no hand-holding, all of the following must be true:

1. **A misroute/outage cannot masquerade as anything else.** Every storefront-facing parse
   validates Content-Type; every failure carries a typed transport-vs-app verdict with bilingual
   operator copy; `? Ismeretlen / Unknown` is rare enough that one occurrence is a bug report.
   (F1, F2, F8, F11, F16, F18)
2. **No quote can silently stall.** Every submitted quote produces either a Posted row or a
   visible, classified Failed row (F5); transient failures recover without a human (F3) and
   incidents have a bulk-recovery path (F13).
3. **Failure copy tells the operator the actual next action.** "Retry required" only when retry
   can work; material-clarification, auth, routing, already-priced each get their own verb.
   (F2, F4, F12)
4. **Prices are real.** No hard-coded machining rate (F6); tunables drive the number a customer
   signs.
5. **The default path works.** A customer who accepts the form's defaults — material `unknown` —
   gets clarified, not dead-ended (F4); any whitelisted file format gets priced or visibly
   refused at upload time (F5, F9).
6. **Accept completes the business loop.** DEAL produces at least a draft invoice without
   re-typing (F7).
7. **Operations are survivable.** Documented secret-rotation order with a dual-accept window
   (F14); correlation id (or at least `quote_id`-keyed structured logs) on both sides (F15);
   PDF-size and other contract caps enforced where they originate (F17).
8. **The panel is an operator console, not a status page.** The Auto-Quoting panel was designed
   read-only (`PricingJobsList.svelte:2–6`) and its backend has exactly list+retry+status
   routes — that was right for S279 and is wrong for production. An operator must be able to
   inspect a row in full (U1, U15, U20), correct it (U5), dispose of it (U4, U19), recover in
   bulk (U6, U7), and complete the business loop from it — record a phone acceptance (U16),
   re-send an expired quote link (U17), reach DEAL and the draft invoice (U17, U18) — without
   leaving the screen or SSHing into anything.

Items 1–3 are what bit on 2026-06-11; items 4–6 are silent-failure equivalents waiting for the
first real customer; items 7–8 are what make the first incident — and the first phone-accepting
customer — survivable.

**Suggested sequencing:** S347 = typed transport errors + Content-Type sweep + probe (F1, F2,
F8, F10, F11, F16, F18, F20, U10) — one coherent cut in `quote_pricing_pipeline.rs` +
`transport.rs` + `pricing-failure-kind.ts`. S348 = retry truth + never-silent enqueue +
stage-aware/bulk retry (F3, F5, F13, U6, U7). S349 = material clarification + operator
material-grade edit (F4, U5). S350 = machining rate (F6). S351 = DEAL ↔ draft invoice link +
pickup verification (F7, U18). S352 = panel detail view + audit timeline + sort/filter/refresh/
pagination (U1, U2, U3, U8, U9, U15, U20). S353 = archive + decline/lost funnel (U4, U19).
S354 = operator-accept ADR amendment + resend-link lifecycle (U16, U17). The 🟢 polish items
(U11–U14, F22, F23) ride along whichever session touches their file.

---

*Audit conducted from worktree @ `ae40f4d` (ABERP) and `bc3b3b7` (ABERP-site); §3 added same
day from `75604ae`. No code was modified. All file:line references verified against those SHAs.
One correction was made during the extension: F7's original "operator re-types everything"
claim was wrong (the Quotes-tab pickup path exists) — see the corrected F7 for what actually
remains open.*
