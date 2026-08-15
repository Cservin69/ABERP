# ADR-0111 — `ap_invoice.deadline_origin`: re-key the settled-undated exclusion on ORIGIN, not null-ness

- **Status:** **Proposed (revision 2)** — design only. No code, no schema, no
  migration has been written. This document is the durable fix ("Option B") the
  operator approved for the `ap_sync` undated-payable exposure opened by
  PR #68 / #69.
- **Date:** 2026-08-14. **Revised:** 2026-08-15 after adversarial plan review.
- **Deciders:** Ervin
- **Context repo:** `Cservin69/ABERP` (production line), at `033112b`
  (merge of PR #69). Frozen prod tag `PROD_v2.34.0` (`9a758d2`) is untouched.
- **Related:** ADR-0043 (invoice dates — the three NAV date rules), ADR-0022
  (NAV XSD runtime validator), ADR-0082 (DB snapshot system), ADR-0099 (prod
  durability-hardening lane), ADR-0110 (durable-commit contract — **D9's flock
  and D7.6's armed WAL fence are now load-bearing for §6**).
- **Supersedes:** nothing. **Amends** the blanket "no recorded deadline ⇒ settled"
  rule landed by PR #68 / #69 in `apps/aberp/src/reports.rs::aging_placement`,
  for go-forward AP rows only.

### Revision 2 — what the adversarial review changed

Seven blockers and eight should-fixes. Every one was re-verified against the tree
at `033112b` before being written in; §12 records the verification, including
**two facts the review itself did not have** (`aberp snapshot now` is *also*
flock-gated, and its CLI help text says the opposite).

| # | Was | Now |
| --- | --- | --- |
| B1 | "the dashboard moves exactly once, in step 4" | **False.** Rollout reordered: migration+coercion → **re-key** → write paths → backfill (§7). The honest invariant replaces the false one. |
| B2 | read coercion `NULL → absent_legacy`, and `absent_legacy + dated` loud-fails | **Contradiction.** Coercion is now **pair-keyed** (§4.4); NULL-origin + dated buckets normally. |
| B3 | loud-fail inside `build_financial_report` | **Moved to the write boundary.** The read path excludes one row + ERROR + a new counter (§4.4, §5.1). |
| B4 | "running against a live `serve` is the cheaper option and is safe" | **False and dangerous.** The backfill takes the F-E flock and *refuses* against a live serve (§6.1). |
| B5 | no index handling | Backfill **must** `rebuild_secondary_indexes_audited` before its first UPDATE, in autocommit (§6.2). |
| B6 | "one transaction per pass" gives resumability | It does not. Pass A commits per batch; resumability is `WHERE deadline_origin IS NULL` (§6.4). |
| B7 | "err late" | **Inverted.** Err **early** (§6.5, §10). |
| S5 | digest `<paymentDate>` is the primary mechanism | **Demoted to an optimisation.** The primary mechanism is the sync path's existing XML fetch (§3.3). |

---

## 1. Context

### 1.1 What PR #68 / #69 established

`reports::aging_placement` (`apps/aberp/src/reports.rs:1870-1923`) is the single
decision point for whether an otherwise-outstanding invoice is in **outstanding**
at all, and if so which aging bucket it lands in. Since PR #69 it returns `None`
whenever `payment_deadline` is NULL **or** unparseable, and the caller then
excludes the row from the receivables/payables total, from every aging bucket, and
from the past-deadline hygiene counters, together. That joint exclusion is what
holds the invariant the panel is read against:

> every invoice counted in the receivables / payables TOTAL lands in exactly one
> aging bucket, so `sum(buckets) == total`, always.

The exclusion is not silent: it is tallied into `SettledUndated`
(`reports.rs:1761-1784`), surfaced on the wire as
`LedgerDiagnostics::aging_settled_undated{,_receivables,_payables,_invoice_ids}`
(`reports.rs:224-263`), and summarised by one aggregate `tracing::warn!` carrying
the count **and** the excluded gross per currency (`reports.rs:2246-2266`). The
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
and binds a canonical `YYYY-MM-DD` (`duckdb_store.rs:816-855`). So a NULL AR
deadline can only be a pre-PR-84 row. The rule is closed at the top by the writer.

**AP has no such closure, and the writer is still open.** Verified against the
tree at `033112b`:

| Claim | Verified | Anchor |
| --- | --- | --- |
| `ap_sync` hardcodes `payment_deadline: None` on every NAV-synced payable | **yes** | `apps/aberp/src/ap_sync.rs:971` (the brief said `:977`; `:1683` and `:2056` are test fixtures) |
| …and hardcodes `delivery_date: None` too | **yes — wider than the brief** | `ap_sync.rs:970` |
| The sync is ongoing, not one-shot | **yes** | `CADENCE_SECS = 30 * 60` (`ap_sync.rs:113`), slept on at `ap_sync.rs:300` |
| Nothing in the app can set `payment_deadline` on an existing `ap_invoice` row | **yes** | The only two UPDATE sites are `set_nav_xml_path` (`incoming_invoices.rs:910-915`, sets `nav_xml_path` + `updated_at`) and the status change (`incoming_invoices.rs:1058-1068`, sets `local_status` + `irrelevant_reason` + `updated_at`). Neither touches the date. There is no route for it either (`serve.rs:4292-4332`). |
| `ap_invoice` has no migration mechanism at all | **yes** | `ensure_schema` is `execute_batch(AP_INVOICE_SCHEMA_SQL)`, `CREATE TABLE IF NOT EXISTS` only (`incoming_invoices.rs:382-426`); no `ALTER TABLE` for `ap_invoice` exists anywhere under `apps/aberp/src`. |

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
(`ap_sync.rs:117`) through the *same* `digest_to_ingestion_input`. Every row it
created carries `payment_deadline` NULL for the same reason every row created
this morning does.

Worse: the bootstrap sweep is **year-to-date**, so it necessarily ingested
invoices issued *after* ABERP's AP go-live — invoices that may be genuinely
unpaid — alongside the genuinely legacy ones. A `created_at`-based cohort would
sweep both into `absent_legacy`. This is the single highest-risk decision in the
plan and it is **Open Question (1)**.

### 1.5 NAV's payment date — what is proven and what is assumed

**Proven (invoiceData side).** `<paymentDate>` is a real, optional, date-shaped
element of `invoiceDetail` in the NAV OSA 3.0 InvoiceData XSD:
`crates/nav-xsd-validator/src/validate.rs:677-712` lists it in `ALLOWED` but not
in `ORDERED_REQUIRED`, and runs `ensure_date_shape` on it (`validate.rs:1601-1616`
— exactly ten ASCII characters, so `2026-05-20+02:00` is rejected). ADR-0043
§"paymentDate" documents it as "when payment is due". ABERP already reads it back
out of stored NAV XML: `nav_xml::read_invoice_payment_date_from_xml`
(`apps/aberp/src/nav_xml.rs:2294-2348`).

**Proven (the stored artifact).** `nav_xml_path` holds the **base64-decoded inner
`<InvoiceData>` XML** — the supplier's own document, not the SOAP envelope —
written by `persist_xml_for_row` (`ap_sync.rs:670-746`, file write at `:735-737`,
UPDATE at `:738`). So `<paymentDate>`, when the supplier sent one, is on disk
today under `~/.aberp/<tenant>/ap-artifacts/<apinv-id>.xml`. The XML backstop
rests on firm ground, and §3.3 now makes it the **primary** mechanism.

**Assumed (digest side).** The brief states the Editions review saw
`<paymentDate>` in `queryInvoiceDigest`. In *this* tree the only supporting
evidence is the parser's own doc comment, which says NAV's digest XSD "names many
more fields (insertion date, totals in HUF, **payment date**, etc.)" and that
adding them is additive (`crates/nav-transport/src/operations/query_invoice_digest.rs:59-66`).
No XSD, no fixture, and no captured prod response in this tree names the element.
`query_invoice_digest::call` returns only the typed `QueryInvoiceDigestPage` and
does not hand the raw bytes back to the caller (`query_invoice_digest.rs:129-166`),
so nothing on disk can settle it either. **Revision 2 demotes this arm to an
optimisation** (§3.4) precisely because it is the one unverified link in the
chain, and the plan must not depend on it.

**Coverage caveat, load-bearing for §5.** `nav_xml_path` is legitimately NULL for
a large share of INBOUND rows: NAV returns `funcCode=OK` with no `<invoiceData>`
whenever the supplier has not exposed the XML to the buyer (paper invoices,
partial-data submissions, opt-out) — `ap_sync.rs:655-704`, PR-215 / S217, "every
one of the 13/13 2026-06-01 prod cycle failures falls under this branch". The XML
recovery therefore covers a **partial** cohort by construction, and even a present
XML may carry no `<paymentDate>` (it is optional). Whatever is left over is
exactly the population that needs the `nav_absent` treatment in §5.

### 1.6 Three durability facts that constrain §6 (revision 2)

Verified at `033112b`. None of these were in revision 1, and each one alone is
enough to turn the backfill into a prod incident.

1. **The F-E whole-DB writer flock is mandatory, and `aberp serve` holds it for
   its entire process lifetime** (`serve.rs:896-911`). Every DB-mutating one-shot
   takes `db_writer_lock::acquire_or_refuse` before opening the tenant DB
   (`aberp-db/src/lib.rs:487-497`) and therefore **refuses** against a live serve
   rather than folding its WAL. The ADR-0110 D7.6 WAL fence is **armed**
   (`wal_fence_enabled` defaults `true` as of 2026-08-13, `lib.rs:453-454`), so a
   fold is no longer silent: it fails the *next* `durable_ack`, which propagates
   via `?` — "a committed invoice reported as failed, NAV handoff skipped"
   (`lib.rs:471-479`). An unflocked backfill against a live serve is a money-path
   outage, not a race.
2. **A CLI never gets the ART index repair.** `rebuild_secondary_indexes_audited`
   runs at `serve` boot only (`serve.rs:1504-1550`). The 2026-08-03 prod incident
   was an `UPDATE ap_invoice` against missing `ap_invoice_tenant_status_idx` /
   `ap_invoice_tenant_issue_idx` entries **fatally invalidating the instance**
   (`crates/aberp-db/src/index_integrity.rs:1-44`). The backfill is an
   `UPDATE ap_invoice` run from a CLI — the exact statement shape, on the exact
   table, from the one context that has no repair. §6.2 fixes this.
3. **`aberp snapshot now` and `aberp snapshot restore` are *also* flock-gated**
   (`snapshot.rs:333-334`, `:406-410`) and therefore also refuse against a live
   serve. **The `cli.rs` help text is stale and says the opposite** — "Safe to run
   while `aberp serve` is up (in-process DuckDB instance sharing) or stopped"
   (`cli.rs:637-639`). That comment predates D9 and must be corrected; an operator
   following it will hit a refusal at exactly the wrong moment.

---

## 2. Decision — overview

1. **Add `ap_invoice.deadline_origin`**, an app-enforced closed vocabulary
   recording *what wrote (or failed to supply) the deadline*. Additive migration,
   no backfill inside it, **pair-keyed** read-side coercion. (§4)
2. **Re-key the settled exclusion on the `(origin, deadline)` pair**, not on
   null-ness. Introduce a sixth aging bucket, `no_due_date`, so a genuinely unpaid
   undated payable is **outstanding** and still lands in exactly one bucket. (§5)
   Landing this *before* any write-path change is provably delta-zero — §7.
3. **Pass NAV's payment date through on the sync path** — primarily by extending
   the artifact-persist step that already runs every cycle; the digest field is a
   later optimisation. (§3)
4. **Backfill existing prod rows** under the durability rules: flocked,
   index-repaired, snapshot-gated, dry-run-first, idempotent, re-runnable,
   audited. (§6)
5. **Sequence the rollout** so that no dashboard number can move before the
   go-forward fix lands, and every movement after it is attributable to exactly
   one named cause. (§7)

---

## 3. NAV payment-date pass-through

### 3.1 What the sync path already does (the finding that reorders this section)

`run_sync_cycle` does **not** stop at the digest. For every row it ingests, and
for every already-existing row whose `nav_xml_path` is still NULL, it queues an
`XmlFetchTarget` (`ap_sync.rs:456-503`) and then performs a real
`queryInvoiceData` INBOUND fetch per target (`ap_sync.rs:538-576`). On success
`persist_xml_for_row` decodes the inner `<InvoiceData>`, writes it to
`ap-artifacts/<id>.xml`, and calls `incoming_invoices::set_nav_xml_path`
(`ap_sync.rs:735-744`).

So the supplier's own XML — the document that provably carries `<paymentDate>`
(§1.5) — is already in memory, on the write path, once per row, on the daemon's
ordinary schedule. **That is the mechanism to use.** Revision 1 put the digest
field first and left `set_nav_xml_path` explicitly "unchanged"; that inverted the
evidence.

**Precision, because it bounds the coverage.** The fetch is *not* re-run for a row
that already has a `nav_xml_path` — the `AlreadyExists` arm queues a target only
on `get_nav_xml_path(...) == Ok(None)` (`ap_sync.rs:485-494`). So extending the
persist step covers:

- every row ingested from here on, and
- every already-ingested row that does **not** yet have an artifact (retried each
  cycle until one lands, or forever if NAV never exposes one).

It does **not** cover rows whose artifact already landed before the change. Those
are Pass A's population (§6.5), and this is why Pass A survives revision 2 rather
than being replaced by the sync path.

### 3.2 The change — `set_nav_xml_path` becomes deadline-aware

`set_nav_xml_path` (`incoming_invoices.rs:895-925`) currently sets `nav_xml_path`
+ `updated_at` under the shared writer. It gains an optional recovered deadline:

```rust
pub fn set_nav_xml_path_and_deadline(
    db: &aberp_db::Handle,
    tenant: &str,
    ap_invoice_id: &str,
    xml_path: &str,
    recovered_deadline: Option<&str>,   // canonical YYYY-MM-DD, already validated
) -> Result<()>
```

with the UPDATE widened to, and **only** to, the still-unset case:

```sql
UPDATE ap_invoice
   SET nav_xml_path     = ?,
       payment_deadline = COALESCE(payment_deadline, ?),
       deadline_origin  = CASE
                            WHEN payment_deadline IS NULL AND ? IS NOT NULL
                              THEN 'nav_supplied'
                            ELSE deadline_origin
                          END,
       updated_at       = ?
 WHERE tenant_id = ? AND id = ?
```

Three properties this shape is chosen for:

- **It never overwrites a deadline that is already there.** An `operator_set`
  correction (§5 step 5's edit endpoint) survives every subsequent sync cycle.
  Without the `COALESCE`, a daemon tick would silently revert the operator.
- **It never downgrades an origin.** A row that already reads `operator_set` keeps
  it.
- **It cannot manufacture a bad date.** The caller runs
  `incoming_invoices::is_canonical_iso_date` (`incoming_invoices.rs:1149-1152`)
  on the extracted string and passes `None` on failure — see §3.5.

The caller is `persist_xml_for_row`, which already holds `inner` (the decoded
bytes) at `ap_sync.rs:736`. Extraction happens on those bytes, not by re-reading
the file it just wrote.

`set_nav_xml_path`'s existing "matched 0 rows ⇒ loud error" guard
(`incoming_invoices.rs:916-923`) is retained verbatim.

### 3.3 Ingest mapping (`apps/aberp`) — the birth stamp

`digest_to_ingestion_input` (`ap_sync.rs:913-978`) returns `payment_deadline:
None` unconditionally at `:971`. It gains an origin stamp, and — **only if and
when §3.4's digest field is confirmed and landed** — a date:

```rust
// NAV supplying no payment date is the ORDINARY case (the element is
// optional on both the digest and the InvoiceData side). The row is
// stamped nav_absent and lands in the `no_due_date` bucket, NOT
// excluded as settled. §3.2's artifact step may upgrade it to
// nav_supplied later in this same cycle.
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

Until §3.4 lands, the same block reads `payment_deadline: None` +
`deadline_origin: DeadlineOrigin::NavAbsent` — which is the whole go-forward fix
on its own, because §5 makes `nav_absent` outstanding.

Empty-string filtering matters even so: `append_optional` will happily produce
`Some("")` from `<paymentDate/>`, and an empty string is not a date. The existing
`aging_placement` unparseable arm would log it at ERROR once per dashboard load
per row (`reports.rs:1881-1900`) — exactly the noise the `None` arm was quieted to
avoid.

### 3.4 Digest parser (`crates/nav-transport`) — an optimisation, not a dependency

Three additive edits, mirroring `issue_date` exactly, **gated on Step 0 confirming
the element name against a real INBOUND response** (§1.5):

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
belongs to the ingest side.

**Why this is now second, not first.** It is the one link in the chain with no
evidence in this tree, it recovers strictly less than the artifact (the digest is
a summary; the artifact is the supplier's document), and §3.2 already closes the
go-forward exposure without it. If Step 0 cannot confirm the element, this
subsection is dropped and nothing else in the ADR changes.

### 3.5 The XML extractor — a sibling, and what it must reject

`read_invoice_payment_date_from_xml` (`nav_xml.rs:2294`) **loud-fails** when
`<paymentDate>` is absent or empty (`:2329-2343`) — correct for its caller
(`issue_storno.rs:416`, where a missing base date is a real fault), wrong here
where absence is the ordinary case. Add a sibling
`read_optional_invoice_payment_date_from_xml(bytes) -> Result<Option<String>>`
that returns `Ok(None)` for absent/empty and reserves `Err` for malformed XML, and
re-express the strict one in terms of it. Take **bytes**, not a path, so
`persist_xml_for_row` can call it on `inner` without a round-trip through the
filesystem; Pass A reads the file and calls the same function.

**Two risks that must be pinned, because the existing extractor was written for a
different document (S4).**

1. **It is first-match-any-depth.** The matcher is
   `e.name().local_name().as_ref() == b"paymentDate"` with no parent or depth
   check (`nav_xml.rs:2316-2318`), and its doc comment states the assumption
   openly: "Matches the FIRST `<paymentDate>` (the base body carries exactly
   one)" (`:2286-2287`). That assumption is about **our own outgoing** XML. AP
   artifacts are **supplier** documents. Our own XSD validator models
   `invoiceMain → invoice` as a single child (`validate.rs:176-180`,
   `expect_single_child_then_close`), but the NAV OSA 3.0 schema also admits
   `batchInvoice` — a multi-invoice document — and **the validator is never run
   against AP artifacts at all** (no `nav_xsd_validator` reference exists in
   `ap_sync.rs` or `incoming_invoices.rs`). So on a batch artifact the extractor
   would return the first invoice's payment date and attribute it to whichever
   `ap_invoice` row the file is named for.
   **Requirement:** the sibling must reject a document containing more than one
   `<paymentDate>`, returning `Ok(None)` + a `warn!` naming the id. Cheap (count
   matches instead of returning on the first), and it converts a silent
   mis-attribution into a row that falls through to `nav_absent` / Pass C.
2. **Non-canonical `xs:date` is legal XML and illegal here.** `xs:date` permits a
   timezone offset — `2026-05-20+02:00` — and our `ensure_date_shape` rejects it
   (exactly ten ASCII chars, `validate.rs:1602-1608`), as does
   `is_canonical_iso_date` (`time::Date::parse` with `[year]-[month]-[day]`
   refuses trailing characters, `incoming_invoices.rs:1149-1152`). But **neither
   runs on this path**: the artifact is never validated, and Pass A / §3.2 write
   via UPDATE rather than through `validate_ingestion_input`
   (`incoming_invoices.rs:670-706`, which *does* gate `payment_deadline` at
   `:692-698`).
   **Requirement:** every recovered string is passed through
   `is_canonical_iso_date` before it is bound. Failure ⇒ `Ok(None)` + `warn!` ⇒
   the row falls through to Pass C and lands in `no_due_date`. A recovered value
   is untrusted input; the read path must never be handed a string it will later
   report as unparseable.

---

## 4. `ap_invoice.deadline_origin`

### 4.1 The four states, and why exactly these

The column records **provenance of the deadline field**, not the deadline itself.
Two axes collapse into it: *who wrote the row* × *whether a date came with it*.

| State | Meaning | `payment_deadline` |
| --- | --- | --- |
| `nav_supplied` | NAV gave a date — recovered from the supplier's InvoiceData artifact (§3.2), from the digest (§3.4), or by Pass A (§6.5) | non-NULL |
| `nav_absent` | `ap_sync` wrote the row and NAV gave **no** date. Genuinely unknown, **not** a claim of settlement | NULL |
| `operator_set` | A human wrote the row (manual `POST /ingest`, or the §7 step-5 edit endpoint) | either |
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
  door exists" defect should be closed by a small edit endpoint (Open Question 3).
  Without this state, an operator-corrected deadline would have to masquerade as
  `nav_supplied`, which would be a lie in the audit trail.
- **No fifth state.** `operator_set` + NULL is reachable (manual ingest omitting
  the deadline) and is handled by making the §5 classifier a function of the
  **pair**, not of origin alone. Adding an `operator_absent` state would buy
  nothing the pair does not already express.

Rust surface: a `DeadlineOrigin` enum in `incoming_invoices.rs` alongside
`IncomingInvoiceStatus` (`:187-230`), with the identical `as_str` /
`from_storage_str` round-trip-proven pair and the same "reject unknown values on
read" posture (pinned the way
`incoming_status_from_storage_str_rejects_out_of_vocab`,
`incoming_invoices.rs:1164-1173`, pins the status vocabulary). Per S410 /
`[[no-sql-specific]]` (the note above `AP_INVOICE_SCHEMA_SQL`,
`incoming_invoices.rs:377-381`), the closed vocabulary is enforced in Rust,
**not** as a DB-level `CHECK`.

### 4.2 Migration — additive, no SQL DEFAULT, error propagated

```sql
-- S<NNN> — additive provenance column for the AP payment deadline.
-- Idempotent via ADD COLUMN IF NOT EXISTS; a no-op on fresh DBs, whose
-- CREATE TABLE IF NOT EXISTS already carries it.
--
-- NO SQL DEFAULT, deliberately. DuckDB re-applies DEFAULT on every replay
-- of `ALTER TABLE ... ADD COLUMN IF NOT EXISTS ... DEFAULT V`, which would
-- clobber values the backfill (and every later ingest) wrote — the exact
-- trap pinned for `quote_intake_log.stock_alert`
-- (apps/aberp/src/quote_intake_query.rs:551-556) and restated for the
-- S361 AVL columns (partners.rs:695-706): `ensure_schema` runs at the top
-- of every writer, so a DEFAULT-bearing column is re-clobbered on every
-- unrelated call. The "safe default" is applied in the APP layer instead,
-- by `coerce_deadline_origin` (§4.4).
ALTER TABLE ap_invoice ADD COLUMN IF NOT EXISTS deadline_origin VARCHAR;
```

`ap_invoice` has no migration mechanism today (§1.2), so this also introduces the
first `AP_INVOICE_MIGRATE_*_SQL` const and appends its `execute_batch` to
`ensure_schema` (`incoming_invoices.rs:423-426`). Three constraints ride along:

- `ensure_schema` is **DDL, i.e. a WRITE**, and may only ever be handed a
  `Handle::write()` connection or the boot-phase opener — never `Handle::read()`
  (`incoming_invoices.rs:414-422`, finding R-1,
  `docs/findings/read-fork-audit-sqlite-20260731.md`, pinned by
  `apps/aberp/tests/no_ddl_on_read_handle.rs`). Adding an ALTER makes that
  already-true rule sharper; the existing pin covers it.
- The new column must be added to `AP_INVOICE_SCHEMA_SQL` (`:382-408`) as well, so
  fresh DBs get it from `CREATE TABLE` and the ALTER is a true no-op there.
- **The ALTER's error must be propagated, not swallowed (S2).** `ensure_schema`
  runs on **every financial-report load** — `let _ = crate::incoming_invoices::
  ensure_schema(&guard);` (`reports.rs:1996`), inside the writer-guard block at
  `:1980-2006` whose siblings all use `.context(...)?`. Today that `let _` is
  harmless (a `CREATE TABLE IF NOT EXISTS` that fails means the DB is already
  broken and the next statement will say so). Once the same call carries the
  ALTER, a swallowed failure means the column silently does not exist, the
  projection at `:1338-1346` fails on the *next* statement with a column error,
  and the operator sees a report failure whose cause was discarded one line
  earlier. Change `reports.rs:1996` (and `:1997` for the sibling
  `restore_from_nav_outgoing::ensure_schema`) to
  `.context("ensure ap_invoice schema for financial report")?`. No-DEFAULT is
  right and stays; the `let _` is not.

**No backfill runs inside the migration.** It is a separate, operator-driven,
flocked, snapshot-gated command (§6). A migration that rewrites prod rows on boot
has no dry-run, no snapshot gate, and no way to be inspected before it fires.

### 4.3 Every write path, and what it stamps

| # | Path | Anchor | Stamps |
| --- | --- | --- | --- |
| 1 | `ap_sync` daemon + bootstrap ingest | `ap_sync.rs:913-978` → `incoming_invoices::ingest_incoming_invoice` | `nav_absent`; `nav_supplied` once §3.4 lands and NAV gave a digest date (§3.3) |
| 2 | **`set_nav_xml_path` — the primary recovery (§3.2)** | `incoming_invoices.rs:895-925`, called from `ap_sync.rs:738` | `nav_supplied` + the recovered date, **only when the row is still unset** (`COALESCE`). Never downgrades an existing origin |
| 3 | Manual `POST /api/incoming-invoices/ingest` | `serve.rs:4294`, `serve.rs:17881-17884` | `operator_set` — **stamped by the handler, not by serde** (S1, below) |
| 4 | The INSERT itself | `incoming_invoices.rs:569-594` | add `deadline_origin` to the column list and `params!` |
| 5 | Status change (`mark-paid` / `-outstanding` / `-irrelevant`) | `incoming_invoices.rs:1050-1069` | **unchanged.** Payment status is orthogonal to deadline provenance |
| 6 | Backfill command (new) | §6 | `nav_supplied` \| `absent_legacy` \| `nav_absent` \| `operator_set` per pass |
| 7 | Deadline-edit endpoint (§7 step 5) | — | `operator_set` + an `IncomingInvoiceDeadlineChanged` audit event |

**S1 — `#[serde(skip)]`, not `#[serde(default)]`.** `IngestionInput` derives
`Deserialize` (`incoming_invoices.rs:274`) and the handler deserialises straight
into it: `Json(input): Json<incoming_invoices::IngestionInput>`
(`serve.rs:17884`). `#[serde(default = "…")]` sets the field *only when the client
omits it* — a client that **sends** `"deadline_origin": "nav_supplied"` gets it,
and can forge NAV provenance on a hand-posted row. `#[serde(skip)]` makes the
field unconditionally `Default::default()` on the wire path, and
`handle_ingest_incoming_invoice` then stamps
`input.deadline_origin = DeadlineOrigin::OperatorSet;` **unconditionally** before
calling `ingest_incoming_invoice`. `ap_sync` constructs the struct in Rust and is
unaffected.

*Stronger alternative, worth taking if the diff allows:* drop the field from
`IngestionInput` entirely and pass `DeadlineOrigin` as a separate argument to
`ingest_incoming_invoice`. Then the type system, not a `#[serde]` attribute,
guarantees the wire cannot reach it, and there is no `Default` to get wrong. The
`#[serde(skip)]` form is the smaller change; the argument form is the safer one.

`IncomingInvoiceIngestedPayload` (`audit_payloads.rs:2172-2210`) already carries
`payment_deadline`; add `deadline_origin` beside it so the audit trail records
*why* a row is dated or not. That payload is round-trip pinned
(`audit_payloads.rs:4929-4975`) — the new field needs the same treatment and must
be `#[serde(default)]` so historical entries still deserialise.

### 4.4 Read path — PAIR-KEYED coercion (B2), and no loud-fail here (B3)

Revision 1 said "`None` (the pre-backfill NULL) → `absent_legacy`", and §5.1 then
declared `absent_legacy` + a present deadline "unreachable by construction —
loud-fail". **Those two statements contradict each other on real prod data.**
Manual ingest can supply a deadline (`IngestionInput.payment_deadline: Option
<String>`, `incoming_invoices.rs:282`, bound into the INSERT at `:585`), so
NULL-origin **dated** rows exist today. That is precisely why revision 1 also
needed a Pass D to sweep them (§6.5). Revision 1's own backfill proved its own
classifier wrong.

The coercion is therefore keyed on the **pair**:

```rust
/// Coerce the stored origin for one row. NULL is the pre-backfill /
/// pre-migration state and is NOT a claim about provenance — what it
/// means depends on whether the row is dated.
fn coerce_deadline_origin(
    stored: Option<&str>,
    deadline: Option<&str>,
) -> Result<DeadlineOrigin, UnknownOrigin> {
    match stored {
        // Explicit: round-trip through the closed vocabulary. An unknown
        // NON-NULL string is schema/wire drift — see the error handling
        // below; it is NEVER coerced to any state, least of all settled.
        Some(s) => DeadlineOrigin::from_storage_str(s),
        // NULL + no date: indistinguishable from today's excluded
        // population. Behave exactly as today.
        None if deadline.is_none() => Ok(DeadlineOrigin::AbsentLegacy),
        // NULL + a date: an un-backfilled row that someone dated.
        // Trust the date. It buckets normally, exactly as today.
        None => Ok(DeadlineOrigin::Unclassified),
    }
}
```

`Unclassified` is a **read-side-only** state — it is never written to the column,
never in the storage vocabulary, and disappears the moment the backfill runs
(Pass D restamps those rows `operator_set`). It exists so the classifier's input
type can express "we do not know the provenance but we do have a date", which is
a real state of prod data during steps 1–3 of the rollout. An implementation that
prefers fewer types may instead drop `Unclassified` and say *a present, parseable
deadline buckets normally regardless of origin*; that is the same rule expressed
in the classifier rather than in the coercion, and it is equally correct. Pick one
and pin it.

**The delta-zero property this buys.** Pre-backfill, every row has NULL origin, so
every row takes one of the two `None` arms — undated ⇒ excluded (as today), dated
⇒ bucketed (as today). The read path is byte-identical to `033112b`. This is what
makes §7's reorder safe, and it is *not* true of revision 1's coercion.

**Unknown non-NULL origin does not fail the report (B3).** See §5.1.

---

## 5. Re-keying the aging exclusion

### 5.1 The classifier

`aging_placement` (`reports.rs:1870-1923`) stops branching on `deadline` alone and
branches on the **pair**. Total function; every combination is reachable and
handled:

| `deadline_origin` | `payment_deadline` | Outcome | Rationale |
| --- | --- | --- | --- |
| `absent_legacy` | NULL | **Excluded as settled.** Tallied into `SettledUndated` exactly as today | The operator's ruling, now recorded per row instead of inferred from null-ness |
| `nav_supplied` | parses | **Outstanding**, normal bucket via `aging_bucket_for` | A real NAV date; nothing special |
| `operator_set` | parses | **Outstanding**, normal bucket | A real human-supplied date |
| `Unclassified` (read-side) | parses | **Outstanding**, normal bucket | Un-backfilled but dated — identical to today's behaviour. §4.4 |
| **`nav_absent`** | NULL | **Outstanding**, `AgingBucket::NoDueDate` | **The crux — §5.2** |
| `operator_set` | NULL | **Outstanding**, `AgingBucket::NoDueDate` | Same honesty as `nav_absent`: a human ingested it and gave no date; that is unknown, not settled |
| any | present but unparseable | **Excluded**, ERROR-level line, as today (`reports.rs:1881-1900`). Unchanged | Genuinely wrong data; both writers validate on the way in, so reaching here means something bypassed them |
| `absent_legacy` / `nav_absent` | **non-NULL** | **This ROW excluded**, ERROR line, `LedgerDiagnostics::aging_origin_conflict` +1 | A writer disagrees with itself. Recorded loudly, contained to one row — see below |
| unknown non-NULL origin string | any | **This ROW excluded**, ERROR line, `aging_origin_conflict` +1 | Schema/wire drift. Same containment |

**B3 — where the loud-fail goes.** Revision 1 put a hard failure for the last two
rows inside `build_financial_report`. That is the wrong boundary, and this tree
has already made the opposite call twice:

- `aging_placement`'s unparseable arm deliberately **degrades one row**: ERROR
  line, `settled.record(...)`, `None` (`reports.rs:1881-1900`). It does not
  return `Err`.
- The `SUBSTR(CAST(a.payment_deadline AS VARCHAR), 1, 10)` guard was added
  precisely so a column type change could not "fail the entire financial report"
  (`reports.rs:1330-1337`, verbatim).

`aggregate_ap` returns `ApAggregate`, not `Result` (`reports.rs:2331`), so a hard
failure there would have to change its signature and propagate to the whole
report. The financial report is **one endpoint carrying AR, AP, expenses, VAT,
DSO and cash-flow together** — one drifted AP row would blank the operator's
entire dashboard, including receivables that have nothing to do with it. A
data-integrity signal that takes the dashboard down is a worse outage than the
drift it reports.

So: **exclude the row, log ERROR with the id and the offending pair, and increment
a new counter.** The counter is new rather than folded into
`aging_settled_undated` because the two mean different things — one is a
deliberate exclusion, the other is a bug:

| Field | Shape | Notes |
| --- | --- | --- |
| `LedgerDiagnostics::aging_origin_conflict` | `u64`, `#[serde(default)]` | count of rows excluded for an impossible or unknown `(origin, deadline)` pair |
| `LedgerDiagnostics::aging_origin_conflict_invoice_ids` | `Vec<String>`, `#[serde(default)]`, capped at `MAX_UNPARSEABLE_ENTRY_IDS` (`reports.rs:270`) | same "count exact, ids are a starting point" contract as `unparseable_entry_ids` |

Any non-zero value is a defect, not a migration artefact, and should raise the
integrity banner. **Note the invariant cost, and accept it deliberately:** an
excluded row is out of the total *and* out of every bucket, so `sum(buckets) ==
total` still holds — the same joint-exclusion mechanism as the unparseable arm.

**Hard rejection stays at the boundaries where it belongs:**

- **Ingest / write.** `DeadlineOrigin::from_storage_str` rejects out-of-vocab
  values, and the write paths (§4.3) are the only producers. A row can only become
  drifted by something bypassing them.
- **The backfill.** §6.3's preflight aborts the whole run on any unknown origin
  string or impossible pair, before it takes a single write. A batch job that
  mutates prod should refuse to start on data it does not understand; a dashboard
  read should not.

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
disagree (`reports.rs:1800-1813`). A `nav_absent` row now answers *yes* to the
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

### 5.3 Blast radius — including the signature changes revision 1 missed (S8)

**The `aging_placement` signature must change, and so must the SPA's.** This is
not additive, and it is the reason step 4 of the old rollout could never have been
a drop-in.

`NoDueDate` carries **no date**. Today the function returns
`Option<(AgingBucket, Date)>` (`reports.rs:1877`) and the AR caller destructures
`deadline_d` unconditionally at `reports.rs:1647` and then does date arithmetic on
it — `let days_out = (deadline_d - today).whole_days();` (`:1673`) — inside the
cash-flow-forward projection. There is no date to hand it for a `NoDueDate` row.

So the return type becomes `Option<(AgingBucket, Option<Date>)>`, and:

- The AR consumer (`reports.rs:1647-1691`) must guard the cash-flow block on
  `Some(d)`. The guard is already `if matches!(bucket, AgingBucket::Current)`
  (`:1672`), and `NoDueDate` is not `Current`, so the arithmetic is unreachable
  in practice — but "unreachable in practice" is exactly what `deadline_d` being
  a bare `Date` currently asserts *in the type*, and widening the type without
  fixing the destructure will not compile. Make the guard
  `if let (AgingBucket::Current, Some(d)) = (bucket, deadline_d)`.
- The AP consumer (`reports.rs:2371`) already binds `_deadline_d` and needs only
  the pattern update.

**The SPA classifier's signature changes for the same reason.**
`agingBucketFor(todayIso, deadlineIso)` (`aging.ts:170-188`) derives the bucket
**from the deadline alone** and returns `null` when it will not parse (`:174-175`).
`no_due` cannot come out of that function — there is no input that produces it. It
must take the origin:

```ts
export function agingBucketFor(
  todayIso: string,
  deadlineIso: string | null | undefined,
  origin: DeadlineOrigin | null,      // NEW — from IncomingInvoice.deadline_origin
): AgingBucket | null
```

and `incomingAgingMatches` (`aging-facets.ts:73-82`) must change with it: its
line 80, `if (hasNoRecordedDeadline(inv.payment_deadline)) return false;`, is the
AP exclusion predicate and becomes wrong the moment `no_due` exists. The AR facet
(`outgoingAgingMatches`) keeps today's shape — §5.5.

Full surface:

| Surface | Anchor | Change |
| --- | --- | --- |
| `enum AgingBucket` | `reports.rs:1728-1735` | `+ NoDueDate` |
| `aging_slot` | `reports.rs:1927-1935` | `+ NoDueDate => &mut panel.no_due_date` |
| **`aging_placement` signature** | `reports.rs:1870-1877` | `+ origin` param; return `Option<(AgingBucket, Option<Date>)>` |
| **AR consumer** | `reports.rs:1647-1691` | destructure + cash-flow guard, per above |
| `pub struct AgingPanel` (wire) | `reports.rs:332-338` | `+ pub no_due_date: AmountAggregate` — additive JSON |
| `LedgerDiagnostics` | `reports.rs:224-264` | `+ aging_origin_conflict`, `+ aging_origin_conflict_invoice_ids` (§5.1) |
| AP SQL projection | `reports.rs:1338-1346` | `+ a.deadline_origin`; keep the `SUBSTR(CAST(… AS VARCHAR), 1, 10)` on the deadline |
| `struct ApRow` | `reports.rs:1030-1041` | `+ deadline_origin: Option<String>` |
| `aggregate_ap` | `reports.rs:2331-2401` | pass origin into `aging_placement`; widen the hygiene gate at `:2394` |
| AR side | `reports.rs:1638-1691` | AR has no `deadline_origin` column; it passes `None` and keeps today's rule — §5.5 |
| SPA closed vocab | `apps/aberp-ui/ui/src/lib/aging.ts:26-71` | `AgingBucket` type, `AGING_BUCKETS` order, `AGING_LABELS` (HU primary / EN secondary), `panelField` exhaustive switch, `LEGAL` set (deep-link token, e.g. `no_due`) |
| **SPA classifier** | `aging.ts:170-188` | `agingBucketFor` takes `origin` |
| SPA wire types | `apps/aberp-ui/ui/src/lib/api.ts:4793-4799`, `:3701-3722` | `AgingPanel.no_due_date`; `IncomingInvoice.deadline_origin` |
| **SPA facet predicates** | `aging-facets.ts:73-82` (`incomingAgingMatches`), `hygiene-clickthrough.ts` | drill-down must match `no_due`; `hasNoRecordedDeadline` (`aging.ts:88-107`) must stop meaning "excluded" for AP rows |
| Parity pins | `reports.rs:3377-3430` + `aging.test.ts`, `aging-facet-lockstep.test.ts`, `statistics-integrity-banner.test.ts` | the duplicated-verbatim classifier table moves on both sides together, by design |

The SPA change is the fiddly half. `aging.ts`'s header states the boundaries
"MIRROR the backend `reports::aging_bucket_for` EXACTLY … If the two ever drift,
the operator clicks '31–60 nap = 3 invoices' and lands on a list showing 2". The
sixth bucket must land on both sides in one change.

### 5.4 This partially reverses PR #69 — deliberately, and only for AP

For AP rows stamped `nav_absent` or `operator_set`, "no recorded deadline ⇒
settled" is **reversed**: they return to outstanding. For AP rows stamped
`absent_legacy`, and for the entire AR side, it **stands**. The reversal is scoped
by the column, which is why the column has to exist before the reversal can be
expressed at all.

### 5.5 AR is deliberately out of scope

The AR `invoice` table gets no `deadline_origin`. Its NULL cohort is closed by the
PR-84 timeline (§1.2) and its writer cannot produce new NULLs, so there is no
go-forward exposure to fix and every AR NULL is genuinely `absent_legacy`. AR
passes `None` for origin and keeps today's rule verbatim;
`AgingPanel.no_due_date` simply stays zero on the receivables panel. Widening AR
would be a change with no defect behind it.

---

## 6. Backfill of existing prod rows

### 6.1 Posture — and the hard requirement revision 1 got backwards

A new subcommand, `aberp ap-deadline-backfill`, alongside `aberp snapshot`
(`cli.rs:631`, `main.rs:48-50`). Not a boot-time migration, not a daemon.

> **B4 — HARD REQUIREMENT. The backfill takes the F-E whole-DB writer flock and
> therefore REFUSES to run against a live `aberp serve`. Revision 1's claim that
> "running against a live `serve` is the cheaper option and is safe under the
> shared Handle" is FALSE and would have caused a money-path outage.**

Why it is false, in full (ADR-0110 D9, `aberp-db/src/lib.rs:487-497`):

- A CLI is a **separate OS process**. It cannot borrow serve's in-process shared
  `Handle`. "Safe under the shared Handle" describes in-process callers only.
- A second opener against a live tenant DB folds serve's WAL on its close. With
  the **D7.6 fence now ARMED** (`wal_fence_enabled` defaults `true` since
  2026-08-13, `lib.rs:453-454`), that fold is no longer silent: the **next**
  invoice issuance or mark-paid fails `durable_ack`, and the failure propagates
  via `?` — "a committed invoice reported as failed, NAV handoff skipped"
  (`lib.rs:471-479`). The ADR-0110 authors called this "strictly worse than the
  bug it detects", which is why the fence shipped dark for two months.
- `aberp serve` holds the flock for its whole process lifetime
  (`serve.rs:896-911`), so the refusal is automatic and total.

**Shape rules — all three are enforced by pins, and all three are one character
away from being silently false** (`apps/aberp/tests/adr0110_d9_flock_shape.rs:1-45`):

1. **Bind the guard to a NAMED local: `let _db_writer_lock = …`.** `let _ = …`
   binds nothing — the guard is a temporary that drops at the end of the
   *statement*, and the command then does its whole open/read/UPDATE/close
   **unlocked**. The file-level grep in `cut_gate_read_fork.sh` cannot see the
   difference; the D9 refusal test cannot either (a contended run still refuses —
   the mutation only shows in the uncontended window it silently re-opens).
2. **Acquire BEFORE the first tenant-DB open in the function.** A flock taken
   after the open has already let a second instance attach to a live tenant DB.
   Follow `snapshot::run_now` verbatim: `acquire_or_refuse` at `snapshot.rs:333`,
   `open_cli_handle` at `:336` — never the other way round.
3. **Open through `Handle::open_default`, not `Connection::open`.** That is the
   sanctioned single-instance seam and it applies
   `disable_checkpoint_on_shutdown`, so its close cannot fold a WAL
   (`snapshot.rs:510-526`). Any new independent opener also breaks
   `tools/cut_gate_opener_census.sh`, which freezes both the per-file opener count
   (CHECK P1) and the exact fingerprint set (CHECK P2).

**Sequencing `aberp snapshot now` — and a stale doc to fix.** The snapshot CLI is
flock-gated too (`snapshot.rs:333-334`), as is `snapshot restore`
(`snapshot.rs:406-410`, which flocks the **live** DB because it appends
`SnapshotRestored` to the live ledger even though it writes only to `--to`). So
the pre-flight snapshot **also** requires serve stopped, and the whole procedure
runs inside one maintenance window:

```
1.  stop `aberp serve`                       # releases the flock
2.  aberp snapshot now --db … --tenant …     # takes + releases the flock
3.  aberp ap-deadline-backfill …             # dry-run: takes + releases the flock
4.  aberp ap-deadline-backfill … --apply     # takes + releases the flock
5.  aberp ap-deadline-backfill …             # dry-run again: must report 0 unclassified
6.  start `aberp serve`                      # takes the flock for its lifetime
```

`cli.rs:637-639` currently tells the operator that `snapshot now` is "Safe to run
while `aberp serve` is up (in-process DuckDB instance sharing) or stopped". **That
comment predates D9 and is now false** — the flock refuses. Correcting it is a
one-line docs fix and belongs in this work, because the runbook above depends on
the operator not believing it.

The rest of the posture:

- **Dry-run is the default.** `--apply` must be typed explicitly.
- **Snapshot-gated** — see §6.3 for the gate's actual predicate (revision 1's was
  unsatisfiable).
- **Idempotent and re-runnable.** Every pass is `WHERE deadline_origin IS NULL`.
  A second run is a no-op; a crashed run resumes (§6.4).
- **Audited** — §6.6.
- **Verify after.** `aberp snapshot list` + a re-run of the dry-run, which must
  now report zero unclassified rows.

### 6.2 Rebuild the ART secondary indexes first (B5)

> **HARD REQUIREMENT. Before its first UPDATE — after the flock, before the first
> transaction — the backfill MUST call
> `aberp_db::index_integrity::rebuild_secondary_indexes_audited(&conn, &tenant,
> &mirror_path)`, in AUTOCOMMIT.**

The 2026-08-03 prod incident is this exact statement shape on this exact table:
`UPDATE ap_invoice SET local_status='Paid' WHERE id=…` against
`ap_invoice_tenant_status_idx` / `ap_invoice_tenant_issue_idx` that were **missing
entries**, which **fatally invalidated the live instance**
(`crates/aberp-db/src/index_integrity.rs:1-44`; full write-up at
`docs/findings/ap-invoice-art-index-desync-2026-08-03.md`; distilled fixture at
`apps/aberp/tests/fixtures/ap_invoice_index_desync_20260803.duckdb.zst`).

Two properties make this non-optional for a CLI:

- **The repair is unconditional and boot-only.** `serve::run` calls it at
  `serve.rs:1504-1550` — after the mirror reconcile, after every `ensure_schema`,
  on the boot-phase connection — and **refuses to boot** if it fails. A CLI
  one-shot never reaches that path. A backfill run while serve is stopped is
  therefore operating on a DB that has not been repaired since the last boot, and
  the row it is about to UPDATE may be one appended after a WAL tear.
- **There is no non-destructive detector.** A rolled-back probe never reaches
  `CommitState::CommitDelete` and reports clean; a committed probe invalidates the
  instance it was meant to protect (`index_integrity.rs:32-39`, measured on the
  real prod file). "Check first, repair if needed" is not available. Repair
  unconditionally. It costs ~40 ms for all 25 indexes on the 25 MB prod DB.

**AUTOCOMMIT, never inside `BEGIN`/`COMMIT`.** `DROP INDEX` + `CREATE INDEX`
inside an explicit transaction **crashes DuckDB 1.5.x outright** ("Pure virtual
function called!"), measured on the same file (`index_integrity.rs:52-55`). The
rebuild must complete and return before the first `tx = conn.transaction()`.

Ordering inside the command, which is not negotiable:

```
flock (acquire_or_refuse)                    §6.1
  → Handle::open_default                     §6.1 rule 3
  → ensure_schema  (the ALTER, §4.2)         DDL, on the write handle
  → rebuild_secondary_indexes_audited        AUTOCOMMIT — §6.2
  → preflight + snapshot gate                §6.3
  → Pass A / B / C / D                       §6.5
  → postcondition assert                     §6.5
```

`ensure_schema` precedes the rebuild for the same reason serve does it that way:
the enumeration must see the full index set (`serve.rs:1522-1526`).

### 6.3 Preflight and the snapshot gate (S7)

**Preflight, before any write.** A single read-only sweep that aborts the entire
run — this is where §5.1's hard rejection lives:

```sql
-- Must return zero rows. Any hit aborts before the first UPDATE.
SELECT id, deadline_origin, payment_deadline
  FROM ap_invoice
 WHERE tenant_id = ?
   AND deadline_origin IS NOT NULL
   AND (
     deadline_origin NOT IN ('nav_supplied','nav_absent','operator_set','absent_legacy')
     OR (deadline_origin IN ('nav_absent','absent_legacy') AND payment_deadline IS NOT NULL)
   );
```

A batch job that mutates prod must refuse to start on data it does not understand.
(The dashboard, by contrast, degrades one row — §5.1, B3. Different boundary,
different posture, deliberately.)

**The snapshot gate.** Revision 1 required "a snapshot whose timestamp is newer
than the process start". **That is unsatisfiable**: the operator takes the
snapshot *before* launching the backfill (step 2 before step 3 in §6.1's runbook),
and — since `snapshot now` is itself flock-gated — it cannot be taken while the
backfill holds the lock either. The gate as written can never pass.

Replace with a predicate about the **data**, which is what the gate is actually
for:

- **Primary:** the newest snapshot's timestamp is **newer than
  `MAX(updated_at)` over `ap_invoice` for this tenant**. Serve is stopped and the
  backfill holds the flock, so nothing can be writing; if the snapshot postdates
  the last AP write, it captures the exact state about to be modified. Cheap
  (`SELECT MAX(updated_at) FROM ap_invoice WHERE tenant_id = ?`) and exact.
- **Secondary, belt-and-braces:** the snapshot is **within N minutes of now**
  (`--max-snapshot-age-mins`, default 60). Catches the case where `ap_invoice` is
  empty or `MAX(updated_at)` is ancient while other tables have moved on.
- The banner prints the snapshot **seq**, its timestamp, and `MAX(updated_at)`,
  so the operator can see the gate's reasoning rather than trust it.

`tools/snapshot-prod.sh` remains the belt-and-braces full-tenant tar and
explicitly covers `aberp.duckdb` and `ap-artifacts/<apinv-id>.xml`
(`tools/snapshot-prod.sh:16-29`) — worth running alongside, since Pass A reads
those artifacts.

### 6.4 Transactions and what resumability actually is (B6)

> Revision 1 said "**one transaction per pass**, not per run — so a Pass A failure
> on row 400 does not lose rows 1-399's recovered dates." **That is exactly
> backwards.** One transaction per pass means a failure at row 400 rolls back rows
> 1-399. The sentence describes the property it prevents.

The correct split follows what each pass actually *is*:

| Pass | Shape | Transaction |
| --- | --- | --- |
| **A** | Per-row Rust: filesystem read + untrusted XML parse + validate + one UPDATE. Many fallible steps per row, none of them SQL-set-shaped | **Commit per row, or per batch of ~500.** A failure loses at most the current batch |
| **B, C, D** | One set-based `UPDATE … WHERE …` statement each | **One transaction each** — correct, because the statement is already atomic and there is nothing partial to preserve |

**The real resumability property is the WHERE clause, not the transaction
boundary.** Every pass is guarded by `WHERE deadline_origin IS NULL`, so a row
that has been classified is invisible to every subsequent run. A crashed run
resumes by simply being re-run: it sees only what it has not yet done. That is
what makes the command idempotent, and it is independent of how the writes were
batched — the batching only bounds how much work a crash discards.

Pass A additionally re-checks `AND deadline_origin IS NULL` in its per-row UPDATE
(§6.5), so a concurrent writer — impossible under the flock, but free to assert —
cannot be overwritten.

### 6.5 The passes

Ordered. Each is `WHERE deadline_origin IS NULL`, so the order is also the
precedence.

**Pass A — recover real dates from stored XML.** Rust, not SQL: for each row with
`deadline_origin IS NULL AND payment_deadline IS NULL AND nav_xml_path IS NOT NULL`,
read the artifact and call `read_optional_invoice_payment_date_from_xml` (§3.5).

```sql
-- per row, only when the extractor returned Some(date) AND
-- is_canonical_iso_date(date) passed:
UPDATE ap_invoice
   SET payment_deadline = ?,          -- the recovered <paymentDate>
       deadline_origin  = 'nav_supplied',
       updated_at       = ?           -- this pass changes real data
 WHERE tenant_id = ? AND id = ? AND deadline_origin IS NULL;
```

- `Ok(None)` ⇒ leave for a later pass.
- `Err` (unreadable/malformed file) ⇒ `warn!`, count it, leave for a later pass;
  one bad artifact must not abort the run.
- **Multiple `<paymentDate>` matches** (batch artifact) ⇒ `Ok(None)` + `warn!`
  naming the id — §3.5 risk 1. Never guess which invoice the date belonged to.
- **Non-canonical date** (e.g. `2026-05-20+02:00`) ⇒ `Ok(None)` + `warn!` — §3.5
  risk 2. `is_canonical_iso_date` (`incoming_invoices.rs:1149-1152`) is the gate;
  a recovered string is untrusted input and must never reach the column in a shape
  the read path will later report as unparseable.

Everything Pass A declines falls through to Pass C and lands in `no_due_date` —
visible, and recoverable by the operator. That is the correct failure direction.

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
a classification, not a data change.

> **B7 — the cutoff errs EARLY, not late. Revision 1 had this inverted.**
>
> The predicate is `issue_date < cutoff → absent_legacy`. A **later** cutoff makes
> **more** rows satisfy it, so **more** rows are called legacy and **excluded** —
> a **larger** under-count. Revision 1 recommended "err late" while describing the
> consequences of erring early; the recommendation and its justification pointed
> in opposite directions.
>
> **Choose an EARLIER cutoff.** Fewer rows called legacy, more rows landing in
> `no_due_date`.
>
> **Why over-count is the safer error.** An over-count lands in `no_due_date`: it
> is in the payables total, visible on a tile, clickable through to a list, and
> clearable one row at a time with `mark-paid` (`incoming_invoices.rs:1050-1069`,
> already exists, already audited). An under-count lands in `absent_legacy`: it is
> in no total, no bucket, no hygiene counter, and — critically — **nothing in the
> system ever revisits `absent_legacy`.** Every pass is `WHERE deadline_origin IS
> NULL`, so a re-run skips it; no read path reconsiders it; only a hand-written
> UPDATE brings it back. Over-count is noisy and self-healing; under-count is
> silent and permanent. **The cutoff value is Open Question (1).**

**Pass C — the honest remainder.**

```sql
UPDATE ap_invoice
   SET deadline_origin = 'nav_absent'
 WHERE tenant_id = ?
   AND deadline_origin IS NULL
   AND payment_deadline IS NULL;
```

Everything undated and not ruled legacy is *unknown*, which is what `nav_absent`
means. These are the rows that become outstanding when the backfill lands.

**Pass D — pre-existing dated rows.**

```sql
UPDATE ap_invoice
   SET deadline_origin = 'operator_set'
 WHERE tenant_id = ?
   AND deadline_origin IS NULL
   AND payment_deadline IS NOT NULL;
```

Sound by elimination, and the elimination argument **holds** (re-verified):
pre-fix `ap_sync` wrote `payment_deadline: None` unconditionally
(`ap_sync.rs:971`) and no UPDATE path could set it (§1.2 table), so the only
writer that could have produced a non-NULL deadline on a pre-migration row is
`POST /api/incoming-invoices/ingest` — an operator. The dry-run census must report
this count; **if it is not small, the elimination argument is wrong and the run
must stop for review.** These rows are also the existence proof behind §4.4's
pair-keyed coercion: if Pass D can ever match a row, then NULL-origin dated rows
exist, and a coercion that maps NULL → `absent_legacy` unconditionally is wrong.

**Postcondition, asserted by the command before it exits:**
`SELECT COUNT(*) FROM ap_invoice WHERE tenant_id = ? AND deadline_origin IS NULL`
must be `0`.

### 6.6 Audit (S6)

**One audit entry per PASS**, not one per run and not one per row —
`ApDeadlineBackfillPassCompleted`, carrying the pass letter, the rows matched, the
cutoff (Pass B), the snapshot seq, and the per-class gross by currency.

- **Passes B, C, D:** the audit append rides **inside the same transaction as the
  UPDATE**, per the ingest precedent — "INSERT + audit append in ONE transaction
  so a crash leaves neither a row without an audit entry nor an audit entry
  without a row" (`incoming_invoices.rs:549-568`). One statement, one entry, one
  commit.
- **Pass A:** there is no single transaction to ride (§6.4), so its entry is
  appended in its own small transaction immediately after the last batch commits,
  and it reports **what this attempt converted**. State the consequence plainly:
  a crash mid-Pass-A leaves the rows correctly classified and **no** Pass A entry;
  the re-run's entry then covers only the remainder. The audit trail for Pass A is
  therefore per-attempt, not per-pass-total. The postcondition assert and the
  re-run dry-run (§6.1 step 5) are the reconciliation.

A run-level `ApDeadlineBackfillCompleted` entry may be added on top; it is
convenience, not the record. The per-pass counts are the evidence that the
dashboard's movement was the intended movement.

### 6.7 The dry-run census (S3) — and what it must filter

Before deciding anything, the dry-run prints the class census. It can be run
today, offline, against a **restored** prod snapshot, with **no code changes at
all**:

```sql
-- Class census for NULL-deadline rows. `local_status` is in the GROUP BY
-- because it is the only column that separates rows the dashboard will
-- ever look at from rows it will not.
SELECT CASE
         WHEN nav_xml_path IS NOT NULL THEN 'has-xml (Pass A candidate)'
         ELSE 'no-xml'
       END                                        AS xml,
       SUBSTR(CAST(issue_date AS VARCHAR), 1, 7)  AS issue_month,
       SUBSTR(CAST(created_at AS VARCHAR), 1, 7)  AS ingest_month,
       local_status,
       currency,
       COUNT(*)                                   AS n,
       SUM(total_gross_minor)                     AS gross_minor
  FROM ap_invoice
 WHERE tenant_id = ? AND payment_deadline IS NULL
 GROUP BY 1, 2, 3, 4, 5
 ORDER BY 2, 3;
```

The `issue_month` × `ingest_month` cross-tab is what lets Ervin *see* the legacy
cohort rather than guess at it: the bootstrap-year sweep shows up as a single
tight `ingest_month` spread across a year of `issue_month`s.

**The payables-delta figure is a different query, and revision 1's was wrong
(S3).** "The number of rows that will leave the settled-excluded population" is
not the number of rows the *tile* will move by. `aggregate_ap` never even reaches
the aging block unless `local_status == "Outstanding"` (`reports.rs:2358`), and
`"Irrelevant"` rows are `continue`d far earlier (`:2334`). It also only ever sees
rows inside the operator's selected **window** and **date basis**, applied by
`query_ap_rows` via `date_col_sql_ap` (`reports.rs:1053-1058`, `:1319-1328`):

| Basis | Window column |
| --- | --- |
| `DateBasis::Issued` | `a.issue_date` |
| `DateBasis::Teljesites` | `COALESCE(a.delivery_date, a.issue_date)` — and `ap_sync` writes `delivery_date: None` (`ap_sync.rs:970`), so for every NAV-synced row this **is** `issue_date` |

So the delta query must mirror all three filters:

```sql
-- Size of the payables-total movement, for ONE window + basis.
-- :date_col is 'a.issue_date' or 'COALESCE(a.delivery_date, a.issue_date)'.
SELECT currency, COUNT(*) AS n, SUM(total_gross_minor) AS gross_minor
  FROM ap_invoice a
 WHERE a.tenant_id = ?
   AND a.local_status = 'Outstanding'          -- ← revision 1 omitted this
   AND :date_col >= ? AND :date_col <= ?       -- ← and this
   AND a.payment_deadline IS NULL
   AND SUBSTR(CAST(a.issue_date AS VARCHAR), 1, 10) >= ?   -- NOT legacy: >= cutoff
 GROUP BY currency;
```

The `--apply` banner should print this for the operator's *default* dashboard
window and basis, and say which window it used. A delta computed over the whole
table would overstate the tile movement by every `Paid` and `Irrelevant` row and
by everything outside the window — which is most of the book.

---

## 7. Rollout sequencing (B1 — reordered)

### 7.1 Why revision 1's order was wrong

Revision 1 promised: *"the dashboard moves exactly once, in step 4, by an amount
printed in step 3."* Its order was **migration → write paths → backfill →
re-key**, and the promise is false at its own step 2.

At revision-1 step 2 the write paths begin stamping a **real `payment_deadline`**
(§3.2 / §3.3, replacing `ap_sync.rs:971`'s `None`). The reporting re-key has not
landed, so the **old, deadline-only** classifier is still live —
`aging_placement` branching on `deadline` alone (`reports.rs:1878-1922`). A row
that now arrives *with* a date therefore enters the payables total and a real
aging bucket **immediately**, on the next dashboard load. And it happens again
every `CADENCE_SECS = 30 * 60` (`ap_sync.rs:113`), forever, in an amount nobody
printed and nobody bounded. The dashboard would have been drifting upward for the
entire interval between steps 2 and 4 — exactly the uncontrolled movement the
sequencing exists to prevent.

### 7.2 The corrected order

**Step 0 — probes, no code.** Run the four commands in §10.5 on a restored
snapshot: the class census, the artifact hit-rate, the payables delta, and the
Pass D probe. Separately, confirm the digest element name for §3.4 against a real
INBOUND `queryInvoiceDigest` response — `call` does not return raw bytes
(`query_invoice_digest.rs:129-166`), so this needs a one-off probe binary in the
`nav_number_probe.rs` mould, a temporary `trace!` of the response body, or the NAV
OSA 3.0 `InvoiceDigestType` XSD read directly. **§3.4 is the only thing gated on
that confirmation**; everything else proceeds without it. Nothing downstream
starts until Step 0's numbers exist.

**Step 1 — additive migration (§4.2) + pair-keyed read coercion (§4.4).** Column
added, `ensure_schema` error propagated (S2), `(NULL, NULL) → absent_legacy` and
`(NULL, dated) → bucket normally` in the app layer.
**Dashboard delta: zero, by construction** — both arms reproduce today's behaviour
exactly. Ships and sits.

**Step 2 — the reporting re-key (§5), backend + SPA together.** The sixth bucket,
the signature changes (S8), the `aging_origin_conflict` counter (B3), the widened
hygiene gate, the SPA vocabulary and facets.
**Dashboard delta: still zero, provably.** Every row in the DB has `deadline_origin
IS NULL`. Undated rows coerce to `absent_legacy` → excluded, byte-identical to
today. Dated rows coerce to `Unclassified` → bucket normally, byte-identical to
today. `no_due_date` renders as a zero row on both panels. **No row in the
database can reach the new bucket, because nothing has written the column yet.**
This is the whole reason the re-key can safely precede the writes — and it is only
true because of §4.4's pair-keying.

**Step 3 — write paths stamp the column (§3.2, §3.3, §4.3).** From here the
go-forward fix is live: newly-synced undated payables are stamped `nav_absent` and
land in `no_due_date`; rows whose artifact yields a canonical date are stamped
`nav_supplied` and land in a real bucket.
**Dashboard delta: a monotonic trickle of NEW rows only** — this is the fix
working, and it is the first step at which any number can move at all. It is
bounded by the arrival rate of new payables (not by the size of the book), it is
attributable by construction (the backfill has not run, so every stamped row is
one this step created), and it is observable: `LedgerDiagnostics
::aging_settled_undated_payables` stops growing at exactly this moment, which is
the tripwire inverting as predicted in §8.

**Step 4 — backfill (§6), dry-run then `--apply`.** Every pre-existing row gains
an explicit origin. `absent_legacy` stays excluded; Pass A rows gain real dates and
real buckets; Pass C rows enter `no_due_date`.
**Dashboard delta: one bounded step, sized in advance** by §6.7's delta query.
This is the deliberate flip.

**Step 5 — the missing door.** A deadline-edit endpoint stamping `operator_set`,
so the diagnostic's "fix the deadline and it returns" advice becomes true and
`no_due_date` is drainable by something other than `mark-paid`. Also correct the
two diagnostic message texts, which currently promise a fix that is impossible
(`reports.rs:1893-1896`), and the stale `cli.rs:637-639` snapshot help text
(§1.6). **Open Question (3) recommends folding this into the main work rather than
deferring it.**

### 7.3 The honest invariant, replacing the false one

> **No dashboard number can move before step 3. Every movement from step 3 onward
> is attributable to exactly one named cause: step 3's is new rows arriving (small,
> monotonic, ongoing — the fix working); step 4's is the backfill (one bounded
> step, sized by the dry-run beforehand).**

That is weaker than "moves exactly once", and it is true. A plan cannot both ship
a go-forward fix at step 3 and promise no movement until step 4 — the go-forward
fix *is* movement, and pretending otherwise is what made revision 1's step 2
dangerous.

**Rollback.** Steps 1-2 are additive and inert (step 2 is a binary revert; the
column stays and the previous binary's null-check reads every row exactly as
before). Step 3 is a binary revert — already-stamped rows keep their origin and
keep reading correctly under step 2's classifier, so the revert is clean. Step 4
is a snapshot restore, which is why it comes last and why §6.3's gate matters.

**Editions parity (follow-on).** `Cservin69/ABERP-Editions` carries the same
`digest_to_ingestion_input` defect. The parity port is explicitly **not** part of
this ADR; it should land after step 4 proves out on prod, as its own PR, with the
same step ordering. The digest-parser change (§3.4) is in `crates/nav-transport`
and is the piece most likely to port verbatim.

---

## 8. Interaction with the just-merged PR #68 / #69 work

**Stays, unchanged:**

- The joint-exclusion structure of `aging_placement` — one function taking both
  decisions from one reading. It is why the invariant cannot break; §5 widens its
  output, not its shape.
- The `SUBSTR(CAST(… AS VARCHAR), 1, 10)` guards on all three date projections
  (`reports.rs:1115-1116`, `:1340`, `:1451`) and the reasoning above them
  (`:1330-1337`) — which §5.1 now also cites as precedent for *not* failing the
  whole report on one bad row.
- The unparseable arm, its ERROR-level line, and its exclusion
  (`reports.rs:1881-1900`). A malformed date stays a malformed date regardless of
  origin.
- `SettledUndated`, the per-currency gross tallies, and the aggregate
  `tracing::warn!` (`reports.rs:1761-1784`, `:2246-2266`).
- The AR side entirely (§5.5).
- The classifier-parity discipline: the table duplicated verbatim between
  `reports.rs:3417-3430` and `aging.test.ts` moves on both sides together.

**Changes:**

- `aging_placement`'s **signature** and branch structure — takes origin, returns
  `Option<(AgingBucket, Option<Date>)>`, `NoDueDate` for the undated-but-
  outstanding case (§5.1, §5.3). The AR cash-flow consumer at `reports.rs:1647`
  / `:1673` changes with it.
- `LedgerDiagnostics::aging_settled_undated*` narrows in meaning: it now counts
  only rows genuinely ruled settled (`absent_legacy`) plus unparseables. Its
  tripwire role **inverts** — pre-fix it was "watch this number, a real payable
  may be hiding in it"; post-fix a moving number on a non-migrating book means a
  *classification* fault, because `nav_absent` rows no longer land in it. The doc
  comments at `reports.rs:224-263` and `:1848-1861` must be rewritten, not merely
  amended: the residual they describe is the thing being fixed.
- `LedgerDiagnostics` gains `aging_origin_conflict{,_invoice_ids}` (§5.1) — a
  counter whose only correct value is zero.
- `hasNoRecordedDeadline` (`aging.ts:88-107`) stops being the AP exclusion
  predicate. Its doc comment — "Such an invoice is a LEGACY import from NAV …
  the operator's ruling is that they are all paid" — becomes false for AP rows
  and must be re-scoped to AR, or the function split per side. `agingBucketFor`
  and `incomingAgingMatches` change signature (§5.3).
- The statistics integrity banner's source-text pins
  (`statistics-integrity-banner.test.ts:147-195`) will need updating alongside.
- `reports.rs:1996-1997`'s two `let _ = …ensure_schema(…)` calls become
  `?`-propagating (§4.2, S2).
- Both diagnostic message texts, plus `cli.rs:637-639`, per step 5.

---

## 9. Consequences

- **The under-count closes, and stays closed at the writer.** A NAV-synced payable
  with no date is outstanding from the moment it lands. No timeline argument is
  needed, because the column records the fact rather than inferring it.
- **The payables total will rise at step 3 (a trickle) and step 4 (one step)**, by
  amounts that are respectively bounded-by-arrival-rate and known-in-advance. If
  the step-4 census shows that rise is large, that is the measure of how much
  money the dashboard has been under-reporting — not a reason to soften the fix.
- **The backfill needs a maintenance window, not a quiet moment.** §6.1's flock
  requirement means serve must be stopped for the snapshot *and* the backfill.
  That is a real operational cost revision 1 did not price in; it is also the only
  posture that does not risk a money-path outage.
- **A sixth bucket widens a closed vocabulary that is duplicated across the wire**,
  and — unlike revision 1's account — it is **not purely additive**: two backend
  signatures and two SPA signatures change (§5.3). That is real ongoing cost,
  mitigated only by the existing parity pins.
- **`absent_legacy` is a recorded operator judgement, and it is a one-way door.**
  Nothing in the system revisits it (§6.5, B7). If the cutoff turns out wrong, the
  correction is one targeted UPDATE over an auditable set — but somebody has to
  notice first, and nothing will tell them. This is the strongest argument both
  for an early cutoff and for folding in the edit endpoint.
- **XML recovery coverage is partial by construction** (§1.5) and no amount of
  work makes it total; `no_due_date` is the permanent home for what NAV never
  sends.
- **`ap_invoice` gains its first migration**, which is a small durability surface
  of its own (§4.2's three constraints).

---

## 10. OPEN QUESTIONS — for Ervin

*(Revision 1's questions (a) and (c) are now decided — the sixth bucket, and eager
recovery on the sync path — and moved into §5.2 and §3.2 as decisions. Four
remain, renumbered.)*

### (1) The cutoff date for Pass B — the highest-risk decision

**The cutoff should be ABERP's AP go-live date** — the day you stopped settling
payables in the prior system — **stated as a fact, not inferred from the data.**
There is no schema marker for the legacy cohort: `ap_sync` is the only bulk writer
and the "legacy import" was itself an `ap_sync` run (§1.4), so provenance cannot
separate them. `issue_date`, not `created_at` (§6.5).

**Err EARLIER, not later** (B7). The predicate is `issue_date < cutoff →
absent_legacy`; a later cutoff sweeps *more* rows into the silent, permanent,
never-revisited state. An earlier cutoff leaves genuinely-settled invoices sitting
in `no_due_date` — visible on a tile, clickable, and clearable one at a time with
`mark-paid`. Noisy-and-recoverable beats silent-and-permanent.

**Decide it from the §6.7 census, which now groups by `local_status` as well.**
Rows already marked `Paid` or `Irrelevant` never reach the aging classifier at all
(`reports.rs:2334`, `:2358`), so they are not at risk in either direction — only
the `Outstanding` slice matters, and seeing it separately is what makes the cutoff
choosable rather than guessable. The `issue_month` × `ingest_month` cross-tab makes
the bootstrap-year burst visible as a single tight `ingest_month` band.

### (2) Confirm the sixth-bucket label — but run the hit-rate probe first

Proposed label `"Nincs fizetési határidő / No due date"`, deep-link token
`no_due`. **Before confirming, run:**

```sh
grep -l '<paymentDate>' ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l
ls ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l
```

The ratio decides how much of the book this bucket ends up holding — and therefore
how prominent and how carefully-worded its label needs to be. **If the hit rate is
low AND Step 0 finds no `<paymentDate>` in the digest either, then `no_due_date`
holds essentially the entire payables book**, permanently. That is still the
correct and honest design — an undated payable *is* undated — but it changes the
label from a footnote to the primary tile, and it is worth knowing before the
label is pinned on both sides of the wire. It would also mean Pass A recovers
almost nothing and could be dropped, with §3.2's sync-path stamp doing all the
go-forward work.

### (3) Fold in the deadline-edit endpoint — recommendation REVERSED

Revision 1 listed the edit endpoint as a deferred follow-on. **Recommendation is
now to fold it into this work**, for a reason revision 1 did not state: it is the
**only recoverable exit** from a wrong classification that does not require raw
SQL against prod.

- A row wrongly in `absent_legacy` is invisible — no total, no bucket, no counter —
  and nothing revisits it (§6.5). Today the only fix is a hand-written UPDATE.
- A row correctly in `no_due_date` whose real due date the operator *knows* can
  only be cleared by `mark-paid`, which asserts something false if it is not
  actually paid.
- The diagnostic already tells the operator to "fix the deadline and it returns"
  (`reports.rs:1893-1896`) — advice that has been false since PR #69 shipped.

The endpoint is small: one route, one UPDATE (`payment_deadline`,
`deadline_origin = 'operator_set'`, `updated_at`), one
`IncomingInvoiceDeadlineChanged` audit event, one SPA field. §4.1's vocabulary
already accommodates it and §3.2's `COALESCE` already protects its result from
being reverted by the next sync cycle. **Confirm: fold in, or keep deferred?**

### (4) Manual `POST /ingest` — keep the deadline optional

**Recommendation: leave it optional**, unchanged. `IngestionInput.payment_deadline`
is `Option<String>` (`incoming_invoices.rs:282`) and making it required would
eliminate the `operator_set` + NULL combination and shrink §5.1's table by a row —
but it is a wire-breaking change for any script that posts without one, and the
pair-based classifier already handles the combination honestly (`no_due_date`,
visible, not settled). The cost of the extra table row is one line; the cost of
breaking a caller is an outage. Overturn only if you know of no such caller.

### (5) The four read-only commands to run today

All four are read-only and touch no live DB. **Commands 1, 3 and 4 run against a
snapshot restored to a side path**, not against `~/.aberp/<tenant>/aberp.duckdb` —
opening the live file from a second process is the exact D9 hazard §6.1 exists to
prevent. Producing that side-path copy needs one serve-stop window
(`aberp snapshot restore <seq> --to <side path> --confirm` is itself flock-gated,
`snapshot.rs:406-410`); if you already have a restored copy, use it. **Command 2
needs nothing at all** — it reads files.

```sh
# ── 1. Class census — the cohort picture, and the input to Open Question (1).
duckdb <side-path>.duckdb -c "
SELECT CASE WHEN nav_xml_path IS NOT NULL THEN 'has-xml' ELSE 'no-xml' END AS xml,
       SUBSTR(CAST(issue_date AS VARCHAR),1,7)  AS issue_month,
       SUBSTR(CAST(created_at AS VARCHAR),1,7)  AS ingest_month,
       local_status, currency,
       COUNT(*) AS n, SUM(total_gross_minor) AS gross_minor
  FROM ap_invoice
 WHERE tenant_id='<tenant>' AND payment_deadline IS NULL
 GROUP BY 1,2,3,4,5 ORDER BY 2,3;"

# ── 2. Artifact hit-rate — Open Question (2), and whether Pass A is worth building.
ls   ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l
grep -l '<paymentDate>' ~/.aberp/<tenant>/ap-artifacts/*.xml | wc -l

# ── 3. Payables-total delta — the size of step 4's movement, for one candidate
#       cutoff. Outstanding-only + window + basis, per §6.7 (S3).
duckdb <side-path>.duckdb -c "
SELECT currency, COUNT(*) AS n, SUM(total_gross_minor) AS gross_minor
  FROM ap_invoice a
 WHERE a.tenant_id='<tenant>'
   AND a.local_status='Outstanding'
   AND a.issue_date >= '<window-from>' AND a.issue_date <= '<window-to>'
   AND a.payment_deadline IS NULL
   AND SUBSTR(CAST(a.issue_date AS VARCHAR),1,10) >= '<candidate-cutoff>'
 GROUP BY currency;"
#  Re-run per candidate cutoff. Under DateBasis::Teljesites substitute
#  COALESCE(a.delivery_date, a.issue_date) for a.issue_date in the two window
#  predicates — though ap_sync writes delivery_date NULL (ap_sync.rs:970), so on
#  NAV-synced rows the two are identical today.

# ── 4. Pass D probe — does the §6.5 elimination argument hold?
#       Expect a SMALL number (manual POST /ingest only). A large one means a
#       writer we have not accounted for, and the plan stops for review.
duckdb <side-path>.duckdb -c "
SELECT COUNT(*) AS dated_rows, MIN(issue_date) AS earliest, MAX(issue_date) AS latest
  FROM ap_invoice
 WHERE tenant_id='<tenant>' AND payment_deadline IS NOT NULL;"
```

---

## 11. What the review confirmed HOLDS — unchanged in revision 2

Recorded so a third review does not re-litigate them:

- **No SQL `DEFAULT` on the new column** (§4.2). The DuckDB DEFAULT-on-replay trap
  is real and doubly pinned — `quote_intake_query.rs:551-556` and
  `partners.rs:695-706`, the latter stating explicitly that "`ensure_schema` runs
  at the top of every writer, so a DEFAULT-bearing column would be clobbered on
  every unrelated … call". The app-layer coercion is the right substitute. (Only
  the swallowed error changed — S2.)
- **`sum(buckets) == total` survives the sixth bucket** (§5.2). The invariant holds
  because `aging_placement` takes both decisions from one reading; widening the
  output set does not touch that proof.
- **`nav_absent` stays outstanding and out of the hygiene counters** (§5.2). You
  cannot assert lateness against a deadline nobody recorded.
- **`issue_date`, not `created_at`, for Pass B** (§6.5). The bootstrap-year sweep
  ingested a full year of issue dates in one burst, so a `created_at` cutoff would
  classify genuinely-unpaid recent payables as settled.
- **Pass D's elimination argument** (§6.5) — re-verified against `ap_sync.rs:971`
  and the two-UPDATE-sites finding in §1.2.
- **The transition-window safety** — given B1's reorder, and *only* given it. The
  property depends on §4.4's pair-keyed coercion making step 2 provably delta-zero.

---

## 12. Verification notes

### 12.1 Carried forward from revision 1

- `payment_deadline: None` is at **`ap_sync.rs:971`**, not `:977`. `delivery_date:
  None` is on the line above (`:970`). Same defect family; `delivery_date` feeds
  the `DateBasis::Teljesites` window selector via
  `COALESCE(a.delivery_date, a.issue_date)` (`reports.rs:1055`), so the fallback
  makes it non-load-bearing today. Worth a separate note.
- "Nothing in the app can set `payment_deadline` on an existing `ap_invoice` row"
  — **confirmed** at `033112b`. (A worktree at
  `.claude/worktrees/fix+aging-undated-hardening` contains
  `UPDATE ap_invoice SET payment_deadline = ?` lines; those are not on `main`.)
- `<paymentDate>` in `queryInvoiceDigest` is **not verified in this tree** — §1.5
  and §7.2 Step 0. It *is* verified for the invoiceData side
  (`nav-xsd-validator/src/validate.rs:677-712`), which is what `nav_xml_path`
  stores — and revision 2 makes that the primary mechanism precisely because it is
  the verified one.

### 12.2 New in revision 2 — every blocker anchor re-verified

| Claim | Verified at `033112b` |
| --- | --- |
| `aging_placement` branches on `deadline` alone; returns `Option<(AgingBucket, Date)>` | `reports.rs:1870-1923` |
| AR consumer destructures the date unconditionally and does arithmetic on it | `reports.rs:1647`, `:1673` |
| `aggregate_ap` gates on `local_status == "Outstanding"`; `"Irrelevant"` skipped earlier | `reports.rs:2358`, `:2334` |
| `aggregate_ap` returns `ApAggregate`, not `Result` — a loud-fail there changes the whole report's signature | `reports.rs:2331` |
| The unparseable arm degrades ONE row (ERROR + tally + `None`) rather than failing | `reports.rs:1881-1900` |
| `SUBSTR(CAST(…))` was added to avoid failing the entire report | `reports.rs:1330-1337` |
| Manual ingest can supply a deadline (⇒ NULL-origin dated rows exist) | `incoming_invoices.rs:282`, bound at `:585`; validated by `is_canonical_iso_date` at `:692-698` |
| `ensure_schema` error swallowed on every report load | `reports.rs:1996` (`let _ = …`), sibling `:1997`; the DEFAULT-replay rationale at `partners.rs:695-706` |
| CLI one-shots must take the F-E flock; serve holds it for its lifetime; fence ARMED | `aberp-db/src/lib.rs:453-454`, `:471-479`, `:487-497`; `serve.rs:896-911` |
| Flock shape rules: named `let _guard`, acquire-before-open | `apps/aberp/tests/adr0110_d9_flock_shape.rs:1-45`; exemplar `snapshot.rs:333`→`:336` |
| Opener census freezes count + fingerprints | `tools/cut_gate_opener_census.sh` CHECK P1 / P2 |
| ART rebuild is boot-only and unconditional; no non-destructive detector exists | `serve.rs:1504-1550`; `index_integrity.rs:1-44` |
| `DROP`/`CREATE INDEX` inside `BEGIN`/`COMMIT` crashes DuckDB 1.5.x | `index_integrity.rs:52-55` |
| `rebuild_secondary_indexes_audited(conn, tenant, mirror_path)` is the audited entry point | `index_integrity.rs:207-218` |
| `CADENCE_SECS = 30 * 60` | `ap_sync.rs:113` |
| The sync path fetches full InvoiceData XML per row and holds the decoded bytes at the persist step | `ap_sync.rs:456-503` (targets), `:538-576` (fetch), `:735-744` (write + `set_nav_xml_path`) |
| `set_nav_xml_path` sets only `nav_xml_path` + `updated_at` | `incoming_invoices.rs:910-915` |
| `IngestionInput` derives `Deserialize`; the handler deserialises the wire straight into it | `incoming_invoices.rs:274`; `serve.rs:17884` |
| Ingest audit rides inside the INSERT's transaction | `incoming_invoices.rs:549-568` |
| The XML extractor is first-match-any-depth, written for our own single-invoice body | `nav_xml.rs:2286-2287`, `:2316-2318` |
| Our XSD validator models `invoiceMain → invoice` as a single child, and is **never run against AP artifacts** | `validate.rs:176-180`; no `nav_xsd_validator` reference in `ap_sync.rs` / `incoming_invoices.rs` |
| `ensure_date_shape` / `is_canonical_iso_date` both reject `2026-05-20+02:00`, and neither runs on the artifact path | `validate.rs:1601-1616`; `incoming_invoices.rs:1149-1152` |
| SPA `agingBucketFor` derives the bucket from the deadline alone | `aging.ts:170-188` |
| SPA `incomingAgingMatches` excludes deadline-less rows | `aging-facets.ts:73-82` (line 80) |

### 12.3 Two findings the review did not have

1. **`aberp snapshot now` is itself flock-gated** (`snapshot.rs:326-334`) — as is
   `snapshot restore`, which flocks the **live** DB even though it writes only to
   `--to`, because it appends `SnapshotRestored` to the live ledger
   (`snapshot.rs:398-410`). So the pre-flight snapshot cannot be taken against a
   running serve either; the whole procedure is one maintenance window (§6.1).
2. **`cli.rs:637-639` is stale and says the opposite** — "Safe to run while `aberp
   serve` is up (in-process DuckDB instance sharing) or stopped". It predates D9.
   An operator following it will hit a refusal at exactly the wrong moment. Fix it
   in step 5.

- `[[nav-gotchas]]` could not be consulted: this session's memory store is empty.
