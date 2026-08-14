# ADR-0111 — `ap_invoice.deadline_origin`: re-key the settled-undated exclusion on ORIGIN, not null-ness

- **Status:** **Proposed** — design only. No code, no schema, no migration has been
  written. This document is the durable fix ("Option B") the operator approved for
  the `ap_sync` undated-payable exposure opened by PR #68 / #69. It is to be
  adversarially reviewed before any implementation begins.
- **Date:** 2026-08-14
- **Deciders:** Ervin
- **Context repo:** `Cservin69/ABERP` (production line), at `033112b`
  (merge of PR #69). Frozen prod tag `PROD_v2.34.0` (`9a758d2`) is untouched.
- **Related:** ADR-0043 (invoice dates — the three NAV date rules), ADR-0022
  (NAV XSD runtime validator), ADR-0082 (DB snapshot system), ADR-0099 (prod
  durability-hardening lane), ADR-0110 (durable-commit contract).
- **Supersedes:** nothing. **Amends** the blanket "no recorded deadline ⇒ settled"
  rule landed by PR #68 / #69 in `apps/aberp/src/reports.rs::aging_placement`,
  for go-forward AP rows only.

---

## 1. Context

### 1.1 What PR #68 / #69 established

`reports::aging_placement` (`apps/aberp/src/reports.rs:1874-1928`) is the single
decision point for whether an otherwise-outstanding invoice is in **outstanding**
at all, and if so which aging bucket it lands in. Since PR #69 it returns `None`
whenever `payment_deadline` is NULL **or** unparseable, and the caller then
excludes the row from the receivables/payables total, from every aging bucket, and
from the past-deadline hygiene counters, together. That joint exclusion is what
holds the invariant the panel is read against:

> every invoice counted in the receivables / payables TOTAL lands in exactly one
> aging bucket, so `sum(buckets) == total`, always.

The exclusion is not silent: it is tallied into `SettledUndated`
(`reports.rs:1762-1784`), surfaced on the wire as
`LedgerDiagnostics::aging_settled_undated{,_receivables,_payables,_invoice_ids}`
(`reports.rs:232-263`), and summarised by one aggregate `tracing::warn!` carrying
the count **and** the excluded gross per currency (`reports.rs:2246-2262`). The
count exists precisely as the tripwire for the exposure this ADR closes.

### 1.2 Why the ruling is sound on AR and unsound on AP

The AR justification is a **migration timeline**, written out in full at
`reports.rs:1815-1846`: `MIGRATE_PR_84_SQL`
(`modules/billing/src/adapters/duckdb_store.rs:309-312`) added
`invoice.payment_deadline DATE` with no backfill; post-PR-84 the column cannot go
NULL again, because `DraftInvoice::payment_deadline` is a non-`Option`
`time::Date` (`modules/billing/src/domain/invoice.rs:155`), `issue_invoice`
defaults a missing input to the issue date rather than passing nothing through
(`modules/billing/src/app/issue_invoice.rs:206-222`), and the store always formats
and binds a canonical `YYYY-MM-DD`
(`duckdb_store.rs:816-855`). So a NULL AR deadline can only be a pre-PR-84 row.
The rule is closed at the top by the writer.

**AP has no such closure, and the writer is still open.** Verified against the
tree at `033112b`:

| Claim | Verified | Anchor |
| --- | --- | --- |
| `ap_sync` hardcodes `payment_deadline: None` on every NAV-synced payable | **yes** | `apps/aberp/src/ap_sync.rs:971` (the brief said `:977`; `:1683` and `:2056` are test fixtures) |
| …and hardcodes `delivery_date: None` too | **yes — wider than the brief** | `ap_sync.rs:970` |
| The sync is ongoing, not one-shot | **yes** | `CADENCE_SECS = 30 * 60` (`ap_sync.rs:114`), slept on at `ap_sync.rs:300` |
| Nothing in the app can set `payment_deadline` on an existing `ap_invoice` row | **yes** | The only two UPDATE sites are `set_nav_xml_path` (`incoming_invoices.rs:911-914`, sets `nav_xml_path` + `updated_at`) and the status change (`incoming_invoices.rs:1058-1068`, sets `local_status` + `irrelevant_reason` + `updated_at`). Neither touches the date. There is no route for it either (`serve.rs:4292-4332`). |
| `ap_invoice` has no migration mechanism at all | **yes** | `ensure_schema` is `CREATE TABLE IF NOT EXISTS` only (`incoming_invoices.rs:383-413`); no `ALTER TABLE` for `ap_invoice` exists anywhere under `apps/aberp/src`. |

So the diagnostic's advice on the unparseable arm — *"if this invoice is in fact
unpaid, fix the deadline and it returns"* (`reports.rs:1893-1896`) — points at a
door that does not exist. That is a second, smaller defect this ADR closes as a
by-product.

### 1.3 The exposure, stated precisely

A genuinely unpaid payable synced tomorrow is written with `payment_deadline`
NULL, is byte-indistinguishable from a settled legacy import, and is excluded from
the payables total. Because sync runs every 30 minutes forever, the under-count is
**unbounded and growing**. `reports.rs:1848-1861` already names this residual in
prose; this ADR is the fix.

### 1.4 The finding the brief did not anticipate

**There is no legacy/ongoing distinction available from row provenance on the AP
side, because they are the same code path.** `ap_sync` is the only bulk writer to
`ap_invoice`, and the "legacy import" was itself an `ap_sync` run: the one-shot
year-to-date bootstrap sweep, `run_bootstrap_year_once` (`ap_sync.rs:1135+`,
PR-203 / S203, `CycleTrigger::BootstrapYear` → `"bootstrap-year"` at
`ap_sync.rs:150`), which walks the year in `WINDOW_DAYS = 30` chunks
(`ap_sync.rs:118`) through the *same* `digest_to_ingestion_input`. Every row it
created carries `payment_deadline` NULL for the same reason every row created
this morning does.

Worse: the bootstrap sweep is **year-to-date**, so it necessarily ingested
invoices issued *after* ABERP's AP go-live — invoices that may be genuinely
unpaid — alongside the genuinely legacy ones. A `created_at`-based cohort would
sweep both into `absent_legacy`. This is the single highest-risk decision in the
plan and it is **Open Question (b)**.

### 1.5 NAV's payment date — what is proven and what is assumed

**Proven (invoiceData side).** `<paymentDate>` is a real, optional, date-shaped
element of `invoiceDetail` in the NAV OSA 3.0 InvoiceData XSD:
`crates/nav-xsd-validator/src/validate.rs:677-712` lists it in `ALLOWED` but not
in `ORDERED_REQUIRED`, and runs `ensure_date_shape` on it. ADR-0043 §"paymentDate"
documents it as "when payment is due". ABERP already reads it back out of stored
NAV XML: `nav_xml::read_invoice_payment_date_from_xml`
(`apps/aberp/src/nav_xml.rs:2294-2340`).

**Proven (the stored artifact).** `nav_xml_path` holds the **base64-decoded inner
`<InvoiceData>` XML** — the supplier's own document, not the SOAP envelope —
written by `persist_xml_for_row` (`ap_sync.rs:666-745`). So `<paymentDate>`, when
the supplier sent one, is on disk today under
`~/.aberp/<tenant>/ap-artifacts/<apinv-id>.xml`. The XML backstop rests on firm
ground.

**Assumed (digest side).** The brief states the Editions review saw
`<paymentDate>` in `queryInvoiceDigest`. In *this* tree the only supporting
evidence is the parser's own doc comment, which says NAV's digest XSD "names many
more fields (insertion date, totals in HUF, **payment date**, etc.)" and that
adding them is additive (`crates/nav-transport/src/operations/query_invoice_digest.rs:59-66`).
No XSD, no fixture, and no captured prod response in this tree names the element.
`query_invoice_digest::call` returns only the typed `QueryInvoiceDigestPage` and
does not hand the raw bytes back to the caller (`query_invoice_digest.rs:129-166`),
so nothing on disk can settle it either. **The exact element name and type must be
confirmed against a real INBOUND digest response before the parser arm is
written** — see §7 Step 0. (`[[nav-gotchas]]` was cited in the brief; this
session's memory store is empty, so nothing was recoverable from it.)

**Coverage caveat, load-bearing for §5.** `nav_xml_path` is legitimately NULL for
a large share of INBOUND rows: NAV returns `funcCode=OK` with no `<invoiceData>`
whenever the supplier has not exposed the XML to the buyer (paper invoices,
partial-data submissions, opt-out) — `ap_sync.rs:661-690`, PR-215 / S217, "every
one of the 13/13 2026-06-01 prod cycle failures falls under this branch". The XML
backfill therefore recovers a **partial** cohort by construction, and even a
present XML may carry no `<paymentDate>` (it is optional). Whatever is left over
is exactly the population that needs the `nav_absent` treatment in §5.

---

## 2. Decision — overview

1. **Pass NAV's payment date through** on the sync path, and recover it from
   stored XML for rows already ingested. (§3)
2. **Add `ap_invoice.deadline_origin`**, an app-enforced closed vocabulary
   recording *what wrote (or failed to supply) the deadline*. Additive migration,
   no backfill inside it. (§4)
3. **Re-key the settled exclusion on the `(origin, deadline)` pair**, not on
   null-ness. Introduce a sixth aging bucket, `no_due_date`, so a genuinely unpaid
   undated payable is **outstanding** and still lands in exactly one bucket. (§5)
4. **Backfill existing prod rows** under the durability rules: snapshot-gated,
   dry-run-first, idempotent, re-runnable, audited. (§6)
5. **Sequence the rollout** so the migration alone moves no number on the
   dashboard, and the dashboard flips exactly once, deliberately. (§7)

---

## 3. NAV payment-date pass-through

### 3.1 Digest parser (`crates/nav-transport`)

Three additive edits, mirroring `issue_date` exactly:

| Site | Anchor | Change |
| --- | --- | --- |
| `struct InvoiceDigest` | `query_invoice_digest.rs:67-101` | add `pub payment_date: Option<String>` with a doc comment naming the confirmed element + XSD type |
| `enum DigestField` | `query_invoice_digest.rs:376-386` | add `PaymentDate` |
| `digest_field_for` | `query_invoice_digest.rs:394-415` | add `else if local_name_matches(qualified, "<CONFIRMED-NAME>") { Some(DigestField::PaymentDate) }` |
| `assign_digest_field` | `query_invoice_digest.rs:428-440` | `DigestField::PaymentDate => append_optional(&mut current.payment_date, value)` |

`InvoiceDigest` derives `Default` and every optional field is `Option<String>`, so
this is source-compatible for constructors that use `..Default::default()`;
struct-literal fixtures in `ap_sync.rs`'s test module (`:1683`, `:2056`) need the
new field. Absent element ⇒ `None`, per the module's stated posture ("missing them
surfaces as `None` … rather than a parse failure", `query_invoice_digest.rs:40-42`).
**Do not** make it required — an undated digest is legitimate, and a loud-fail here
would abort whole sync cycles (the `<availableLine>` session-189 lesson,
`query_invoice_digest.rs:334-340`).

Verbatim-string posture: keep it `Option<String>`, not `time::Date`. The parser
boundary never coerces (`invoice_net_amount` doc, `:92-97`); shape validation
belongs to the ingest side, which already runs `incoming_invoices::validate`.

### 3.2 Ingest mapping (`apps/aberp`)

`digest_to_ingestion_input` (`ap_sync.rs:913-977`) currently returns
`payment_deadline: None` unconditionally at `:971`. Change to:

```rust
// NAV supplied a payment date for this digest → the row is dated at
// birth and buckets normally. NAV supplying none is the ORDINARY case
// (the element is optional); the row is stamped nav_absent and lands in
// the `no_due_date` bucket, NOT excluded as settled.
let payment_deadline = digest
    .payment_date
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_string);
let deadline_origin = match payment_deadline {
    Some(_) => DeadlineOrigin::NavSupplied,
    None    => DeadlineOrigin::NavAbsent,
};
```

Empty-string filtering matters: `append_optional` will happily produce
`Some("")` from `<paymentDate/>`, and an empty string is not a date. The existing
`aging_placement` unparseable arm would log it at ERROR once per dashboard load
per row (`reports.rs:1884-1897`) — exactly the noise the `None` arm was quieted to
avoid.

`IngestionInput` (`incoming_invoices.rs:275-294`) gains
`pub deadline_origin: DeadlineOrigin`. It is a `serde` wire type for
`POST /api/incoming-invoices/ingest`, so give it `#[serde(default = …)]` →
`OperatorSet`, so an operator/API client cannot claim NAV provenance and cannot
be broken by the new field. See §4.3.

### 3.3 Backstop for already-ingested rows

Reuse the existing extractor's logic but not its error posture:
`read_invoice_payment_date_from_xml` (`nav_xml.rs:2294`) **loud-fails** when
`<paymentDate>` is absent or empty (`:2329-2334`) — correct for its caller
(`issue_storno.rs:416`, where a missing base date is a real fault), wrong for a
backfill where absence is the ordinary case. Add a sibling
`read_optional_invoice_payment_date_from_xml(path) -> Result<Option<String>>`
that returns `Ok(None)` for absent/empty and reserves `Err` for unreadable/malformed
files, and re-express the strict one in terms of it. This is the only new
`nav_xml.rs` surface.

That sibling is the engine of backfill **Pass A** (§6.3) and of an optional
per-row lazy recovery on the `GET /api/incoming-invoices/:id` detail path —
see **Open Question (c)**.

---

## 4. `ap_invoice.deadline_origin`

### 4.1 The four states, and why exactly these

The column records **provenance of the deadline field**, not the deadline itself.
Two axes collapse into it: *who wrote the row* × *whether a date came with it*.

| State | Meaning | `payment_deadline` |
| --- | --- | --- |
| `nav_supplied` | NAV gave a date — from the digest (§3.2) or recovered from stored XML (§6.3 Pass A) | non-NULL |
| `nav_absent` | `ap_sync` wrote the row and NAV gave **no** date. Genuinely unknown, **not** a claim of settlement | NULL |
| `operator_set` | A human wrote the row (manual `POST /ingest`, or any future edit endpoint) | either |
| `absent_legacy` | A pre-existing NULL row that the §6 backfill classified as the settled legacy import. **The only state that carries the operator's settled ruling.** | NULL |

Justification for this exact set:

- **`nav_supplied` vs `nav_absent` is the whole point.** Today both are NULL and
  therefore identical; splitting them is what lets §5 stop treating an ongoing
  sync artefact as a settlement claim.
- **`absent_legacy` must be a distinct state, not "NULL + old".** The operator's
  ruling is a *judgement about a specific cohort*, not a property of the data. It
  belongs recorded, once, in a column — where it is auditable and where a later
  correction is a targeted UPDATE rather than a re-derivation. Encoding it as a
  date cutoff evaluated at read time would make every future dashboard read
  re-litigate a decision taken once, and would silently re-classify rows as the
  cutoff aged.
- **`operator_set` is needed even though no edit endpoint exists yet.** The manual
  ingest route can already supply a deadline (`IngestionInput.payment_deadline` is
  `Option<String>`, `incoming_invoices.rs:282`, bound at `:585`), and §1.2's "no
  door exists" defect is expected to be closed by a small edit endpoint. Without
  this state, an operator-corrected deadline would have to masquerade as
  `nav_supplied`, which would be a lie in the audit trail.
- **No fifth state.** `operator_set` + NULL is reachable (manual ingest omitting
  the deadline) and is handled by making the §5 classifier a function of the
  **pair**, not of origin alone. Adding an `operator_absent` state would buy
  nothing the pair does not already express. See **Open Question (d)** on whether
  the manual path should simply require a deadline.

Rust surface: a `DeadlineOrigin` enum in `incoming_invoices.rs` alongside
`IncomingInvoiceStatus` (`:187-230`), with the identical `as_str` /
`from_storage_str` round-trip-proven pair and the same "reject unknown values on
read" posture. Per S410 / `[[no-sql-specific]]` (the note above
`AP_INVOICE_SCHEMA_SQL`, `incoming_invoices.rs:377-382`), the closed vocabulary is
enforced in Rust, **not** as a DB-level `CHECK`.

### 4.2 Migration — additive, no SQL DEFAULT

```sql
-- S<NNN> — additive provenance column for the AP payment deadline.
-- Idempotent via ADD COLUMN IF NOT EXISTS; a no-op on fresh DBs, whose
-- CREATE TABLE IF NOT EXISTS already carries it.
--
-- NO SQL DEFAULT, deliberately. DuckDB re-applies DEFAULT on every replay
-- of `ALTER TABLE ... ADD COLUMN IF NOT EXISTS ... DEFAULT V`, which would
-- clobber values the backfill (and every later ingest) wrote — the exact
-- trap pinned for `quote_intake_log.stock_alert`
-- (apps/aberp/src/quote_intake_query.rs:551-556). The "safe default" the
-- brief asks for is applied in the APP layer instead, by
-- `coerce_deadline_origin` (§4.4), mirroring `quote_stock_alert::
-- coerce_stock_alert`.
ALTER TABLE ap_invoice ADD COLUMN IF NOT EXISTS deadline_origin VARCHAR;
```

`ap_invoice` has no migration mechanism today (§1.2), so this also introduces the
first `AP_INVOICE_MIGRATE_*_SQL` const and appends its `execute_batch` to
`ensure_schema` (`incoming_invoices.rs:410-413`). Two constraints ride along:

- `ensure_schema` is **DDL, i.e. a WRITE**, and may only ever be handed a
  `Handle::write()` connection or the boot-phase opener — never `Handle::read()`
  (`incoming_invoices.rs:415-426`, finding R-1,
  `docs/findings/read-fork-audit-sqlite-20260731.md`, pinned by
  `apps/aberp/tests/no_ddl_on_read_handle.rs`). Adding an ALTER makes that already-true
  rule sharper; the existing pin covers it.
- The new column must be added to `AP_INVOICE_SCHEMA_SQL` (`:383-402`) as well, so
  fresh DBs get it from `CREATE TABLE` and the ALTER is a true no-op there.

**No backfill runs inside the migration.** It is a separate, operator-driven,
snapshot-gated command (§6). A migration that rewrites prod rows on boot has no
dry-run, no snapshot gate, and no way to be inspected before it fires.

### 4.3 Every write path, and what it stamps

| # | Path | Anchor | Stamps |
| --- | --- | --- | --- |
| 1 | `ap_sync` daemon + bootstrap ingest | `ap_sync.rs:913-977` → `incoming_invoices::ingest_incoming_invoice` | `nav_supplied` if NAV gave a date, else `nav_absent` (§3.2) |
| 2 | Manual `POST /api/incoming-invoices/ingest` | `serve.rs:4294`; body deserialised into `IngestionInput` | `operator_set` — via `#[serde(default)]`, so the wire cannot assert NAV provenance |
| 3 | The INSERT itself | `incoming_invoices.rs:570-594` | add `deadline_origin` to the column list and `params!` |
| 4 | `set_nav_xml_path` | `incoming_invoices.rs:905-924` | **unchanged.** It writes only `nav_xml_path` + `updated_at`. Recovering a date from that XML is Pass A's job, not this function's — see Open Question (c) |
| 5 | Status change (`mark-paid` / `-outstanding` / `-irrelevant`) | `incoming_invoices.rs:1050-1069` | **unchanged.** Payment status is orthogonal to deadline provenance |
| 6 | Backfill command (new) | §6 | `nav_supplied` \| `absent_legacy` \| `nav_absent` \| `operator_set` per pass |
| 7 | Future deadline-edit endpoint (§1.2's missing door) | — | `operator_set` + a `IncomingInvoiceDeadlineChanged` audit event. **Out of scope here**; noted so the vocabulary already accommodates it |

`IncomingInvoiceIngestedPayload` (`audit_payloads.rs:2172-2210`) already carries
`payment_deadline`; add `deadline_origin` beside it so the audit trail records
*why* a row is dated or not. That payload is round-trip pinned
(`audit_payloads.rs:4929-4975`) — the new field needs the same treatment and must
be `#[serde(default)]` so historical entries still deserialise.

### 4.4 Read path

`coerce_deadline_origin(Option<&str>) -> DeadlineOrigin` maps `None` (the
pre-backfill NULL) → **`absent_legacy`**. That is what makes Step 1 of the rollout
a genuine no-op: with `absent_legacy` excluded-as-settled (§5), an
un-backfilled DB behaves byte-identically to today. An **unknown non-NULL**
string is a schema/wire drift and loud-fails per rule 12 — it must not be coerced
to any state, least of all the settled one.

---

## 5. Re-keying the aging exclusion

### 5.1 The classifier

`aging_placement` (`reports.rs:1874-1928`) stops branching on `deadline` alone and
branches on the **pair**. Total function; five reachable combinations:

| `deadline_origin` | `payment_deadline` | Outcome | Rationale |
| --- | --- | --- | --- |
| `absent_legacy` | NULL | **Excluded as settled.** Tallied into `SettledUndated` exactly as today | The operator's ruling, now recorded per row instead of inferred from null-ness |
| `nav_supplied` | parses | **Outstanding**, normal bucket via `aging_bucket_for` | A real NAV date; nothing special |
| `operator_set` | parses | **Outstanding**, normal bucket | A real human-supplied date |
| **`nav_absent`** | NULL | **Outstanding**, `AgingBucket::NoDueDate` | **The crux — §5.2** |
| `operator_set` | NULL | **Outstanding**, `AgingBucket::NoDueDate` | Same honesty as `nav_absent`: a human ingested it and gave no date; that is unknown, not settled |
| any | present but unparseable | **Excluded**, ERROR-level line, as today | Genuinely wrong data; both writers validate on the way in, so reaching here means something bypassed them (`reports.rs:1884-1892`). Unchanged |
| `absent_legacy` / `nav_absent` | **non-NULL** | Unreachable by construction | Loud-fail. If a row is both stamped absent and dated, a writer disagrees with itself |

### 5.2 The crux: `nav_absent` aging treatment

**Recommendation: a sixth bucket, `AgingBucket::NoDueDate`, surfaced as
`AgingPanel.no_due_date`.**

The row is included in the payables (or receivables) **total**, accrued into
`no_due_date`, and **excluded from `payable_past_deadline` / the hygiene counters**
— you cannot assert lateness against a deadline nobody recorded, and the existing
hygiene gate is already `if !matches!(bucket, AgingBucket::Current)`
(`reports.rs:2394-2396`), which would otherwise count it as late by accident. The
gate becomes `if !matches!(bucket, Current | NoDueDate)`.

**`sum(buckets) == total` is preserved, structurally and for the same reason it is
preserved today:** the invariant holds because `aging_placement` takes *both*
decisions — "in the total?" and "which bucket?" — from one reading, so they cannot
disagree (`reports.rs:1801-1814`). A `nav_absent` row now answers *yes* to the
first and `NoDueDate` to the second. It is in the total once and in exactly one
bucket. Nothing about the invariant's proof changes; only the bucket set widens.

Alternatives rejected:

- **90+ with a distinct flag.** It preserves the sum, but it asserts a severity
  the data does not support: a payable synced this morning with no NAV date would
  render as more than 90 days overdue. The operator's most alarming tile would
  become the one most likely to be wrong, and the hygiene counter would count it
  as past deadline. This is the shape PR #68 shipped and PR #69 deliberately
  reversed (`reports.rs:3402-3406`, mutation proof 2).
- **Keep excluding and lean on the diagnostic count.** That is today's behaviour,
  i.e. the bug. A count in `LedgerDiagnostics` is a tripwire, not a total; nobody
  reconciles a supplier ledger against a warning line.
- **A separate top-level "undated payables" panel outside the aging table.** It
  avoids widening the closed vocabulary, but it re-opens the exact hole PR #69
  closed: a figure in the total with no bucket to click through to. The
  dashboard→list click-through (`aging-facets.ts`) is built on the assumption
  that every outstanding row is reachable from some bucket.

**Blast radius of a sixth bucket** (all additive, all pinned):

| Surface | Anchor | Change |
| --- | --- | --- |
| `enum AgingBucket` | `reports.rs:1729-1735` | `+ NoDueDate` |
| `aging_slot` | `reports.rs:1930-1938` | `+ NoDueDate => &mut panel.no_due_date` |
| `pub struct AgingPanel` (wire) | `reports.rs:332-338` | `+ pub no_due_date: AmountAggregate` — additive JSON |
| AP SQL projection | `reports.rs:1338-1345` | `+ a.deadline_origin`; keep the `SUBSTR(CAST(… AS VARCHAR), 1, 10)` on the deadline |
| `struct ApRow` | `reports.rs:1030-1041` | `+ deadline_origin: Option<String>` |
| `aggregate_ap` | `reports.rs:2330-2400` | pass origin into `aging_placement`; widen the hygiene gate |
| AR side | `reports.rs:1638-1690` | AR has no `deadline_origin` column. It keeps today's rule verbatim — see §5.4 |
| SPA closed vocab | `apps/aberp-ui/ui/src/lib/aging.ts:26-71` | `AgingBucket` type, `AGING_BUCKETS` order, `AGING_LABELS` (HU primary / EN secondary), `panelField` exhaustive switch, `LEGAL` set (deep-link token, e.g. `no_due`) |
| SPA wire types | `apps/aberp-ui/ui/src/lib/api.ts:4793-4799`, `:3701-3722` | `AgingPanel.no_due_date`; `IncomingInvoice.deadline_origin` |
| SPA facet predicates | `aging-facets.ts` (`incomingAgingMatches` / `outgoingAgingMatches`), `hygiene-clickthrough.ts` | the drill-down must match the new bucket on the AP side, and `hasNoRecordedDeadline` (`aging.ts:88-107`) must stop meaning "excluded" for AP rows |
| Parity pins | `reports.rs:3377-3430` + `aging.test.ts`, `aging-facet-lockstep.test.ts`, `statistics-integrity-banner.test.ts` | the duplicated-verbatim classifier table moves on both sides together, by design |

The SPA change is the fiddly half. `aging.ts`'s header states the boundaries
"MIRROR the backend `reports::aging_bucket_for` EXACTLY … If the two ever drift,
the operator clicks '31–60 nap = 3 invoices' and lands on a list showing 2". The
sixth bucket must land on both sides in one change.

### 5.3 This partially reverses PR #69 — deliberately, and only for AP

For AP rows stamped `nav_absent` or `operator_set`, "no recorded deadline ⇒
settled" is **reversed**: they return to outstanding. For AP rows stamped
`absent_legacy`, and for the entire AR side, it **stands**. The reversal is scoped
by the column, which is why the column has to exist before the reversal can be
expressed at all — and why the rollout order in §7 is not negotiable.

### 5.4 AR is deliberately out of scope

The AR `invoice` table gets no `deadline_origin`. Its NULL cohort is closed by the
PR-84 timeline (§1.2) and its writer cannot produce new NULLs, so there is no
go-forward exposure to fix and every AR NULL is genuinely `absent_legacy`. AR keeps
today's rule verbatim; `AgingPanel.no_due_date` simply stays zero on the
receivables panel. Widening AR would be a change with no defect behind it.

---

## 6. Backfill of existing prod rows

### 6.1 Posture

A new subcommand, `aberp ap-deadline-backfill`, alongside `aberp snapshot`
(`cli.rs:631`, `main.rs:48-50`). Not a boot-time migration, not a daemon.

- **Dry-run is the default.** `--apply` must be typed explicitly.
- **Snapshot-gated.** `--apply` refuses to run unless a snapshot exists whose
  timestamp is newer than the process start, and it prints the snapshot seq in its
  banner. Operator runs `aberp snapshot now` (ADR-0082 §CLI) first;
  `tools/snapshot-prod.sh` is the belt-and-braces full-tenant tar and explicitly
  covers `aberp.duckdb` and `ap-artifacts/<apinv-id>.xml`
  (`tools/snapshot-prod.sh:16-29`).
- **Server stopped, or a real write guard.** Every UPDATE goes through
  `Handle::write()`; nothing opens the DB file directly (ADR-0099's opener census
  is frozen — a new opener would fail `tools/cut_gate_opener_census.sh`). Running
  against a live `serve` is the cheaper option and is safe under the shared
  Handle, but it races the 30-minute sync cycle; the runbook should say *stop the
  server*.
- **Idempotent and re-runnable.** Every pass is `WHERE deadline_origin IS NULL`.
  A second run is a no-op. A crashed run resumes.
- **One transaction per pass**, not per run — so a Pass A failure on row 400 does
  not lose rows 1-399's recovered dates.
- **Audited.** One `ApDeadlineBackfillCompleted` audit entry per `--apply` run,
  carrying per-class counts, the cutoff used, and the snapshot seq. The counts are
  the evidence that the dashboard's step-4 movement was the intended movement.
- **Verify after.** `aberp snapshot list` + a re-run of the dry-run, which must
  now report zero unclassified rows.

### 6.2 The dry-run census (this is also the answer to Open Question (b))

Before deciding anything, the dry-run prints — and this can be run today, offline,
against a restored prod snapshot, with **no code changes at all**:

```sql
-- Class census for NULL-deadline rows.
SELECT CASE
         WHEN nav_xml_path IS NOT NULL THEN 'has-xml (Pass A candidate)'
         ELSE 'no-xml'
       END                                   AS xml,
       SUBSTR(CAST(issue_date  AS VARCHAR), 1, 7) AS issue_month,
       SUBSTR(CAST(created_at  AS VARCHAR), 1, 7) AS ingest_month,
       local_status,
       COUNT(*)                              AS n,
       SUM(total_gross_minor)                AS gross_minor,
       currency
  FROM ap_invoice
 WHERE tenant_id = ? AND payment_deadline IS NULL
 GROUP BY 1, 2, 3, 4, 7
 ORDER BY 2, 3;
```

Plus, for the XML cohort, a filesystem probe that needs no build at all:

```sh
# How many stored AP artifacts actually carry a <paymentDate>?
ls ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l
grep -l '<paymentDate>' ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l
```

Those two numbers decide whether Pass A is worth building at all, and the
`issue_month` × `ingest_month` cross-tab is what lets Ervin *see* the legacy
cohort rather than guess at it: the bootstrap-year sweep shows up as a single tight
`ingest_month` spread across a year of `issue_month`s.

### 6.3 The passes

Ordered. Each is `WHERE deadline_origin IS NULL`, so the order is also the
precedence.

**Pass A — recover real dates from stored XML.** Rust, not SQL: for each row with
`deadline_origin IS NULL AND payment_deadline IS NULL AND nav_xml_path IS NOT NULL`,
call `read_optional_invoice_payment_date_from_xml` (§3.3).

```sql
-- per row, only when the extractor returned Some(date):
UPDATE ap_invoice
   SET payment_deadline = ?,          -- the recovered <paymentDate>
       deadline_origin  = 'nav_supplied',
       updated_at       = ?           -- this pass changes real data
 WHERE tenant_id = ? AND id = ? AND deadline_origin IS NULL;
```

`Ok(None)` ⇒ leave for a later pass. `Err` (unreadable/malformed file) ⇒ `warn!`,
count it, leave for a later pass; one bad artifact must not abort the run. The
recovered value is validated through the same `YYYY-MM-DD` shape check the ingest
path uses before it is bound — a recovered string is untrusted input.

**Pass B — the legacy cohort.**

```sql
UPDATE ap_invoice
   SET deadline_origin = 'absent_legacy'
 WHERE tenant_id = ?
   AND deadline_origin IS NULL
   AND payment_deadline IS NULL
   AND SUBSTR(CAST(issue_date AS VARCHAR), 1, 10) < ?;   -- :cutoff
```

Keyed on **`issue_date`, not `created_at`** — deliberately, and this is the
§1.4 finding cashed out. The operator's ruling is about invoices *issued and
settled under the prior system*; `created_at` records only when ABERP happened to
ingest, and the year-to-date bootstrap sweep ingested a whole year of issue dates
in one burst. A `created_at` cutoff would sweep genuinely-unpaid recent payables
into `absent_legacy` — re-creating the very under-count this ADR exists to close,
this time permanently and invisibly. `updated_at` is left alone: this pass records
a classification, not a data change. **The cutoff value is Open Question (b).**

**Pass C — the honest remainder.**

```sql
UPDATE ap_invoice
   SET deadline_origin = 'nav_absent'
 WHERE tenant_id = ?
   AND deadline_origin IS NULL
   AND payment_deadline IS NULL;
```

Everything undated and not ruled legacy is *unknown*, which is what `nav_absent`
means. These are the rows that become outstanding at step 4.

**Pass D — pre-existing dated rows.**

```sql
UPDATE ap_invoice
   SET deadline_origin = 'operator_set'
 WHERE tenant_id = ?
   AND deadline_origin IS NULL
   AND payment_deadline IS NOT NULL;
```

Sound by elimination: pre-fix `ap_sync` wrote `payment_deadline: None`
unconditionally (`ap_sync.rs:971`) and no UPDATE path could set it (§1.2), so the
only writer that could have produced a non-NULL deadline on a pre-migration row is
`POST /api/incoming-invoices/ingest` — an operator. The dry-run census should
report this count; if it is not small, the elimination argument is wrong and the
run must stop for review.

**Postcondition, asserted by the command before it commits:**
`SELECT COUNT(*) FROM ap_invoice WHERE tenant_id = ? AND deadline_origin IS NULL`
must be `0`.

### 6.4 What the operator sees

The dry-run prints the class census, the per-class gross by currency, the exact
number of rows that will **leave** the settled-excluded population (Pass A + Pass C),
and the resulting payables-total delta. That number is the size of the step-4
dashboard movement, known before it happens.

---

## 7. Rollout sequencing

The ordering exists to guarantee **the dashboard moves exactly once, in step 4,
by an amount printed in step 3.**

**Step 0 — verify the NAV digest field (no code).** Confirm the element name and
XSD type for the digest's payment date against a real INBOUND
`queryInvoiceDigest` response. `call` does not return raw bytes
(`query_invoice_digest.rs:129-166`), so this needs either a one-off probe binary in
the `nav_number_probe.rs` mould, a temporary `trace!` of the response body, or the
NAV OSA 3.0 `InvoiceDigestType` XSD read directly. **Also run the §6.2 census and
the `grep -l '<paymentDate>'` probe on a restored snapshot** — both are free and
both feed Open Questions (b) and (c). Nothing downstream should start until
Step 0's numbers exist.

**Step 1 — additive migration (§4.2) + read-path coercion (§4.4).** Column added,
`NULL → absent_legacy` in the app layer, `absent_legacy` excluded-as-settled
exactly as NULL is today. **Dashboard delta: zero, by construction.** Ships and
sits.

**Step 2 — write paths stamp the column (§4.3).** New rows are classified
correctly from here on. Pre-existing rows are still NULL → coerced
`absent_legacy`. **Dashboard delta: still zero**, because the reporting change has
not landed. New `nav_absent` rows are classified but not yet reported differently.

**Step 3 — backfill (§6), dry-run then `--apply`.** Every row now carries an
explicit origin. `absent_legacy` remains excluded, `nav_supplied` rows recovered by
Pass A gain real dates. **Dashboard delta: only the Pass A rows** — which move from
excluded to a real bucket, and which the dry-run counted. `nav_absent` rows are
stamped but still read as excluded, because §5 has not shipped.

**Step 4 — the reporting re-key (§5), backend + SPA together.** `nav_absent` and
`operator_set`+NULL become outstanding in the new `no_due_date` bucket. **This is
the deliberate flip**, and its size was printed in step 3.

**Step 5 — the missing door.** A deadline-edit endpoint stamping `operator_set`,
so the diagnostic's "fix the deadline and it returns" advice becomes true and the
`no_due_date` bucket is drainable by the operator. Also correct the two diagnostic
message texts, which currently promise a fix that is impossible
(`reports.rs:1893-1896`).

**Why not fold steps 1-4 into one release.** Because then a backfill misjudgement
and a classifier change land together and cannot be told apart on the tile. With
this order, any unexpected movement at step 4 is attributable to exactly one
change, and step 3 is revertible by snapshot restore without touching binaries.

Rollback: steps 1-2 are additive and inert. Step 3 is a snapshot restore.
Step 4 is a binary revert — the column stays, and the previous binary's coercion
reads every origin as excluded-or-dated exactly as before.

**Editions parity (follow-on).** `Cservin69/ABERP-Editions` carries the same
`digest_to_ingestion_input` defect. The parity port is explicitly **not** part of
this ADR; it should land after step 4 proves out on prod, as its own PR, with the
same step ordering. The digest-parser change (§3.1) is in `crates/nav-transport`
and is the piece most likely to port verbatim.

---

## 8. Interaction with the just-merged PR #68 / #69 work

**Stays, unchanged:**

- The joint-exclusion structure of `aging_placement` — one function taking both
  decisions from one reading. It is why the invariant cannot break; §5 widens its
  output, not its shape.
- The `SUBSTR(CAST(… AS VARCHAR), 1, 10)` guards on all three date projections
  (`reports.rs:1115-1116`, `:1340`, `:1451`) and the reasoning above them
  (`:1330-1337`). Nothing here makes a wide-rendering date column safe; the AP
  projection gains `deadline_origin` beside them.
- The unparseable arm, its ERROR-level line, and its exclusion (`reports.rs:1884-1897`).
  A malformed date stays a malformed date regardless of origin.
- `SettledUndated`, the per-currency gross tallies, and the aggregate
  `tracing::warn!` (`reports.rs:1762-1784`, `:2246-2262`).
- The AR side entirely (§5.4).
- The classifier-parity discipline: the table duplicated verbatim between
  `reports.rs:3417-3430` and `aging.test.ts` moves on both sides together.

**Changes:**

- `aging_placement`'s signature and branch structure — takes origin, returns
  `NoDueDate` for the undated-but-outstanding case (§5.1).
- `LedgerDiagnostics::aging_settled_undated*` narrows in meaning: it now counts
  only rows genuinely ruled settled (`absent_legacy`) plus unparseables. Its
  tripwire role **inverts** — pre-fix it was "watch this number, a real payable
  may be hiding in it"; post-fix a moving number on a non-migrating book means a
  *classification* fault, because `nav_absent` rows no longer land in it. The doc
  comments at `reports.rs:232-263` and `:1848-1861` must be rewritten, not
  merely amended: the residual they describe is the thing being fixed.
- `hasNoRecordedDeadline` (`aging.ts:88-107`) stops being the AP exclusion
  predicate. Its doc comment — "Such an invoice is a LEGACY import from NAV …
  the operator's ruling is that they are all paid" — becomes false for AP rows
  and must be re-scoped to AR, or the function split per side.
- The statistics integrity banner's source-text pins
  (`statistics-integrity-banner.test.ts:147-195`) will need updating alongside.
- Both diagnostic message texts, per step 5.

---

## 9. Consequences

- **The under-count closes, and stays closed at the writer.** A NAV-synced payable
  with no date is outstanding from the moment it lands. No timeline argument is
  needed, because the column records the fact rather than inferring it.
- **The payables total will rise at step 4**, by an amount known in advance. If the
  step-3 census shows that rise is large, that is the measure of how much money the
  dashboard has been under-reporting — not a reason to soften the fix.
- **A sixth bucket widens a closed vocabulary that is duplicated across the wire.**
  That is real ongoing cost, mitigated only by the existing parity pins.
- **`absent_legacy` is a recorded operator judgement.** If the cutoff turns out
  wrong, the correction is one targeted UPDATE over an auditable set, not a
  re-derivation. This is the main reason to prefer the column over a read-time rule.
- **Pass A's coverage is partial by construction** (§1.5) and no amount of work
  makes it total; the `no_due_date` bucket is the permanent home for what NAV
  never sends.
- **`ap_invoice` gains its first migration**, which is a small durability surface
  of its own (§4.2's two constraints).

---

## 10. OPEN QUESTIONS — for Ervin

**(a) `nav_absent` aging treatment — the crux.**
The recommendation is a sixth bucket, `no_due_date`, holding the row in the
payables total, out of the past-deadline hygiene counters, and reachable by
click-through (§5.2). The cost is widening a closed vocabulary that lives on both
sides of the wire (backend enum + panel field + SPA type, order, labels, deep-link
token, facet predicates, and four test files). The alternative that costs nothing
structurally is 90+ with a flag — but it renders a payable synced this morning as
90+ days overdue and re-lights the hygiene counter, which is what PR #69
deliberately reversed. **Confirm the sixth bucket, and confirm the HU/EN label**
(proposed: `"Nincs fizetési határidő / No due date"`) **and the deep-link token**
(proposed: `no_due`).

**(b) How to identify the legacy-import cohort — the highest-risk decision.**
There is no schema marker. `ap_sync` is the only bulk writer and the "legacy
import" was itself an `ap_sync` run (the year-to-date bootstrap sweep), so origin
cannot separate the cohorts (§1.4). The proposal is an **`issue_date` cutoff**, not
`created_at`, because the bootstrap sweep ingested a full year of issue dates in
one burst and a `created_at` cutoff would classify genuinely-unpaid recent payables
as settled — permanently and invisibly. Three sub-questions:

  1. **What is the cutoff date?** The natural candidate is ABERP's AP go-live — the
     date the operator stopped settling payables in the prior system. It should be
     stated as a fact, not inferred from the data.
  2. **Run the §6.2 census first.** It is free, needs no code, and runs on a
     restored snapshot. The `issue_month` × `ingest_month` cross-tab makes the
     bootstrap burst visible. Please look at it before fixing the cutoff.
  3. **Which way should the cutoff err?** Erring **late** (fewer rows called
     legacy) leaves genuinely-settled invoices sitting in `no_due_date`, visible
     and clearable by the operator with `mark-paid`. Erring **early** hides real
     debt. Recommendation: err late — a visibly-wrong tile is recoverable, a
     silently-short one is the failure mode this whole ADR exists to prevent.

**(c) Backfill real dates from stored XML now, or lazily?**
Pass A (§6.3) is the eager option: one bounded pass over `ap-artifacts/*.xml` at
step 3, with a known cost and an auditable result. The lazy option is to recover
on read (detail-page load, or inside `set_nav_xml_path` when the daemon first
fetches an artifact), which spreads the cost but turns a read path into a write
path and makes the dashboard's numbers depend on browsing history. **Recommendation:
eager, and decide it on data** — `grep -l '<paymentDate>' ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l`
against the total artifact count is the whole decision. If the hit rate is near
zero, drop Pass A entirely, let every undated row go to `nav_absent`, and keep §3.1
(the digest field) as the only recovery mechanism — that alone closes the
go-forward exposure, which is the actual defect.

**(d) Should manual `POST /ingest` still accept a missing deadline?**
`IngestionInput.payment_deadline` is `Option<String>` (`incoming_invoices.rs:282`).
Making it required on the manual path would eliminate the `operator_set` + NULL
combination and shrink §5.1's table by a row. It is a wire-breaking change for any
script that posts without it. **Recommendation: leave it optional** (the pair-based
classifier already handles it honestly) unless you know of no such caller.

**(e) Scope check on step 5.** The deadline-edit endpoint is listed as a follow-on,
not part of this ADR. But until it exists, an operator who *knows* a payable's due
date has no way to record it, and rows can only leave `no_due_date` via `mark-paid`.
**Should step 5 be folded into this work rather than deferred?**

---

## 11. Verification notes on the brief

Recorded so the adversarial review does not have to re-derive them:

- `payment_deadline: None` is at **`ap_sync.rs:971`**, not `:977`. `reports.rs`'s
  own comments already cite `:971` correctly.
- `delivery_date: None` is hardcoded on the same line above (`ap_sync.rs:970`).
  Same defect family, not covered by this ADR; `delivery_date` feeds the
  `DateBasis::Teljesites` window selector via
  `COALESCE(a.delivery_date, a.issue_date)` (`reports.rs:1055`), so the fallback
  makes it non-load-bearing today. Worth a separate note.
- "Nothing in the app can set `payment_deadline` on an existing `ap_invoice` row"
  — **confirmed** at `033112b`. (A worktree at
  `.claude/worktrees/fix+aging-undated-hardening` contains
  `UPDATE ap_invoice SET payment_deadline = ?` lines; those are not on `main`.)
- `<paymentDate>` in `queryInvoiceDigest` is **not verified in this tree** — see
  §1.5 and Step 0. It *is* verified for the invoiceData side
  (`nav-xsd-validator/src/validate.rs:677-712`), which is what `nav_xml_path`
  stores.
- `[[nav-gotchas]]` could not be consulted: this session's memory store is empty.
