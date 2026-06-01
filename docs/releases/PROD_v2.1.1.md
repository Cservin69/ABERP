# PROD_v2.1.1 — diagnostics for AP queryInvoiceData INBOUND failures

**Cutover date:** TBD (S216 / PR-214; release branch + tag deferred to the push utility).
**Predecessor:** `PROD_v2.1` (S215 / PR-213, virtual-union invoices list).
**Scope:** patch — diagnostic-only, no behavior change to any working code path.

## Headline

**Diagnostic capture for the opaque queryInvoiceData INBOUND failure mode.**
After upgrading to PROD_v2.1, Ervin's prod cycle logs surfaced 13/13 XML
fetches failing with the same message: `queryInvoiceData response missing
<invoiceData> element`. The S197 AP-sync follow-on fetch calls
`queryInvoiceData INBOUND` to hydrate each `ap_invoice` row's
`nav_xml_path` column; on a successful HTTP 200 + `funcCode=OK` response,
the parser expects an inner `<invoiceData>` base64 blob and loud-fails
if absent. NAV is returning that exact shape on prod for every recent
inbound invoice, and the existing error message gives the operator no
way to see what NAV actually returned.

PR-214 adds **best-effort diagnostic capture**: on every extraction
failure, the raw response bytes are saved to
`~/.aberp/<tenant>/ap-artifacts/.failed/<ap_invoice_id>.xml` and a
500-byte preview (with HU tax IDs redacted) is emitted in the warn!
log line itself. The next cycle after upgrade will land 13 (or more)
capture files the operator can share for next-session triage to
identify whether this is a NAV-side schema drift, a privacy-scope
restriction on INBOUND queries, or a transient endpoint regression.

**This is not a fix.** PR-214 ships diagnostics only, per the brief's
"don't ship a speculative fix" rule when the root cause is opaque.
The next cycle's capture files unlock the actual fix in a follow-on PR.

## What about "Incoming tab shows zero rows"?

The PR-214 brief framed two symptoms; on diagnosis only one was real.
The Incoming tab does **not** show zero rows on prod — the local
DuckDB carries 81 `ap_invoice` rows (all under `tenant_id='prod'`,
all with the right schema, 79 already marked `Paid` by Ervin in
PROD_v2.0). The list-route SQL (`incoming_invoices::list_incoming`)
filters only by `tenant_id` and the optional `local_status` query
parameter — there is no year filter, no `nav_xml_path` gate, and S215
touched only the Outgoing list. After upgrade to PROD_v2.1.1, all 81
existing rows continue to render in the Incoming tab unchanged.

The brief's natural-language report ("no incoming for this year
although I have around 20") reads most plausibly as: NAV's prod portal
shows ~20 invoices Ervin expects to come through, but the daemon's
XML-fetch retries are failing (Symptom B above), so the existing
digest-only rows aren't gaining their XML payload and no NEW rows
appear because the most recent 30-day window is fully populated from
prior bootstrap cycles. PR-214 ships a defensive pin test
(`incoming_invoices::tests::list_incoming_returns_rows_with_null_nav_xml_path`)
that guarantees a future contributor cannot accidentally add a
`WHERE nav_xml_path IS NOT NULL` filter that would convert this
non-bug into a real one.

## Operator-facing changes after upgrade

After `./run/upgrade_prod.sh PROD_v2.1.1` completes and `aberp serve`
restarts:

1. **All 81 existing AP rows continue to render in the Incoming tab.**
   Same supplier names, same statuses, same totals — no migration,
   no schema bump, no behavioral change on the read path.
2. **The 30-min AP-sync cycle continues to fail its XML fetches**
   until NAV's response shape changes OR a follow-on PR adapts the
   parser. The failure path is now informative:
   - `~/.aberp/prod/ap-artifacts/.failed/apinv_<ULID>.xml` carries
     the raw NAV response bytes for every failure.
   - The cycle log's warn! line carries a `preview=` field with the
     first 500 bytes, HU tax IDs redacted.
3. **No new audit kind, no new schema, no new keychain entry.**
   The diagnostic capture is filesystem-only.

## Breaking changes

**NONE.** Every read path, write path, audit shape, and route
contract is preserved. The change is additive (a `.failed/`
subdirectory under `ap-artifacts/`) plus more verbose warn! logging
on a path that was already loud-failing.

## Files touched

- `apps/aberp/src/ap_sync.rs` — `persist_xml_for_row` wraps the
  `extract_inner_invoice_data_xml` call in a match; on `Err`, calls
  `capture_failing_response` before propagating the original error
  with the same `with_context` message. Three new private helpers:
  `capture_failing_response`, `sanitise_response_preview`,
  `redact_hu_tax_ids`. Six new unit tests.
- `apps/aberp/src/incoming_invoices.rs` — one new pin test
  (`list_incoming_returns_rows_with_null_nav_xml_path`) defending
  the no-`nav_xml_path` filter posture.
- `docs/releases/PROD_v2.1.1.md` — this file.

## Verification

See the PR commit message + the gates report attached to S216.

## Rollback

`./run/upgrade_prod.sh PROD_v2.1` restores the prior release. The
new `.failed/` directory is harmless and need not be cleaned up
(re-creating it is a no-op for v2.1).
