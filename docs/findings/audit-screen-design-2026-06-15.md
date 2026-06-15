# Audit screen — design report

**Session:** session-423 (RESEARCH ONLY — no production code touched)
**Date:** 2026-06-15
**Base:** main @ 400469f (PROD_v2.27.67; PROD_v2.27.68 cut in flight)
**Author brief (Ervin, 2026-06-15 05:31):** *"Audit screen. all operator activity which is currently
logged and workflow. Design can be spawn now, Code sessions for tomorrow."*

**Purpose:** a complete, ship-ready design so tomorrow's implementation session(s) (S424+) can build the
audit screen with no scope ambiguity. This report decides the wire shape, the SPA component, and the
state-machine views; it surfaces the genuine open decisions for Ervin in §7 rather than guessing them.

> **Reading note.** The audit ledger is *already* partially surfaced: the backend has `AuditEntryView`
> + `GET /audit/:invoice_id` (`handle_get_audit`) and the operator dashboard's recent-activity tile
> (`recent_entries`, limit 10); the SPA has `InvoiceTimeline.svelte` and `PricingJobDetail.svelte`
> consuming per-subject audit trails. **This screen is the general, cross-domain, filterable view of the
> whole ledger** — it generalises `get_audit_for_invoice` (one invoice's chain) into "all operator
> activity, any domain, paginated + filtered."

---

## 1. EventKind inventory

`EventKind` lives in `crates/audit-ledger/src/entry/event_kind.rs` (one enum, **106 variants**). Each
variant maps to a stable wire string via `EventKind::as_str()` (round-tripped by `from_storage_str`);
the string carries a **domain prefix** (`invoice.` / `quote.` / `system.` / `mes.` / `inventory.` /
`email.` / `personnel.` / `material.` / `part.` / `export.` / `cui.` / `supplier.` / `incident.`). That
prefix is the canonical grouping and is what the SPA filter chips key on.

**Payload encoding (verified).** Every payload is **`serde_json::to_vec(...)` → JSON bytes**, stored in
the DuckDB `payload BLOB` column. There is **zero CBOR in payloads** (CBOR is used only to compute the
entry hash — see §2). `serve.rs::audit_view_of` already decodes payloads with
`serde_json::from_slice::<serde_json::Value>(&entry.payload)`. Typed payload structs live in
`apps/aberp/src/audit_payloads.rs` (`*Payload::to_bytes()`); newer events serialise inline
`serde_json::json!({...})`. Binary blobs inside payloads (NAV request/response XML) are base64 strings —
**these are large** (see the size warning in §3).

**Emission paths (verified).** Two append APIs, no single wrapper:
- `Ledger::append(kind, payload_bytes, actor, idempotency_key)` — when a `Ledger` is in scope.
- `aberp_audit_ledger::append_in_tx(&tx, &meta, kind, payload_bytes, actor, idempotency_key)` — inside a
  shared DuckDB transaction (the invoice issue/storno path appends *in the same tx* as the billing write).
- `append_reopen(...)` — reopen-per-write for the high-frequency daemons.

The `Actor` (who) is `{ session_id, user_id, capabilities }`; `user_id` is the NAV technical-user login
(`Actor::from_local_cli`). **`user_id` is the operator-identity field** the screen's "operator" filter
targets.

### 1.1 Full variant table

Columns: **Wire kind** (`as_str`) · **Variant** · **Emitter (file)** · **Payload** (✅ = field list read
at emit site; ◑ = summarised from the variant's doc-comment / name — confirm exact fields against the
`event_kind.rs` doc-comments at impl time).

| Wire kind | Variant | Emitter (apps/aberp/src unless noted) | Payload |
|---|---|---|---|
| `test` | `Test` | tests only | ✅ free JSON (test fixture) |
| **Invoicing — outgoing invoice lifecycle (NAV)** |
| `invoice.sequence_reserved` | `InvoiceSequenceReserved` | `issue_invoice.rs` | ✅ `invoice_id, seq, reservation_id, idempotency_key` |
| `invoice.draft_created` | `InvoiceDraftCreated` | `issue_invoice.rs` | ✅ `invoice_id, line_count, idempotency_key` + nav_xml_path, currency, exchange_rate{,_source,_date}, huf_equivalent_total, bank_account_id×5, invoice_note, line_notes[], payment_deadline, delivery_date{,_override}, customer_vat_status (all Option) |
| `invoice.submission_attempt` | `InvoiceSubmissionAttempt` | `submission_queue.rs` | ✅ `invoice_id, idempotency_key, endpoint, request_xml` (base64 — **large**) |
| `invoice.submission_response` | `InvoiceSubmissionResponse` | `submission_queue.rs` | ✅ `invoice_id, idempotency_key, transaction_id, response_xml` (base64 — **large**) |
| `invoice.ack_status` | `InvoiceAckStatus` | `poll_ack.rs` | ✅ `invoice_id, transaction_id, ack_status, response_xml` (base64). `audit_view_of` enriches with `technical_validation_messages` |
| `invoice.retry_requested` | `InvoiceRetryRequested` | `retry_submission.rs` | ✅ `invoice_id, idempotency_key, prior_transaction_id?, prior_last_ack_status?, reason` |
| `invoice.marked_abandoned` | `InvoiceMarkedAbandoned` | `drain_pending_retries.rs` | ✅ same shape as retry_requested |
| `invoice.storno_issued` | `InvoiceStornoIssued` | `issue_storno.rs` | ✅ `storno_invoice_id, storno_seq, storno_reservation_id, idempotency_key, base_invoice_id, base_sequence_number, modification_index` |
| `invoice.modification_issued` | `InvoiceModificationIssued` | `issue_modification.rs` | ✅ as storno + `modification_issue_date` |
| `invoice.technical_annulment_requested` | `InvoiceTechnicalAnnulmentRequested` | `request_technical_annulment.rs` | ◑ invoice_id, annulment code/reason, idempotency_key |
| `invoice.annulment_submission_attempt` | `InvoiceAnnulmentSubmissionAttempt` | `submit_annulment.rs` | ◑ invoice_id, request_xml (base64), endpoint |
| `invoice.annulment_submission_response` | `InvoiceAnnulmentSubmissionResponse` | `submit_annulment.rs` | ◑ invoice_id, transaction_id, response_xml (base64) |
| `invoice.annulment_ack_status` | `InvoiceAnnulmentAckStatus` | `poll_annulment_ack.rs` | ◑ invoice_id, transaction_id, ack_status, response_xml |
| `invoice.annulment_receiver_confirmation` | `InvoiceAnnulmentReceiverConfirmation` | `observe_receiver_confirmation.rs` | ◑ invoice_id, confirmation verdict/timestamp |
| `invoice.submission_attempt_failed` | `InvoiceSubmissionAttemptFailed` | `submission_queue.rs` | ◑ invoice_id, idempotency_key, failure_class, message |
| `invoice.check_performed` | `InvoiceCheckPerformed` | `retry_submission.rs` | ✅ `invoice_id, idempotency_key, endpoint, nav_invoice_number, outcome, request_xml, response_xml?, failure_class?, failure_code?, failure_message?` |
| `invoice.payment_recorded` | `InvoicePaymentRecorded` | `serve.rs` (mark-paid route) | ✅ `invoice_id, idempotency_key, paid_at, amount_minor, currency, method, reference?` |
| `invoice.emailed_sent` | `InvoiceEmailedSent` | `email_relay_daemon.rs` | ✅ `invoice_id, idempotency_key, recipient, subject, outcome, error_class?, error_detail?, auto, attached_xml` — **no SMTP secrets** (ADR-0047 §4) |
| `invoice.staged` | `InvoiceStaged` | `invoice_draft.rs` | ◑ invoice_id, staging source/draft fields |
| `invoice.draft_deleted` | `InvoiceDraftDeleted` | `invoice_draft.rs` | ◑ invoice_id, reason |
| `invoice.picked_up_from_quote` | `InvoicePickedUpFromQuote` | `quote_pickup.rs` | ◑ invoice_id, quote_id link |
| **Quoting — auto-quote pricing pipeline + intake + tunables** |
| `quote.pricing_fetched` | `QuotePricingFetched` | `quote_pricing_pipeline.rs` | ✅ `quote_id, tenant_id, customer_email, material_grade, quantity, cad_filename, cad_local_path, actor, idempotency_key, fetched_at` |
| `quote.pricing_extracted` | `QuotePricingExtracted` | `quote_pricing_pipeline.rs` | ◑ quote_id, CAD feature/geometry extract summary |
| `quote.pricing_priced` | `QuotePricingPriced` | `quote_pricing_pipeline.rs` | ◑ quote_id, price breakdown, currency |
| `quote.pricing_rendered` | `QuotePricingRendered` | `quote_pricing_pipeline.rs` | ◑ quote_id, pdf path/hash |
| `quote.pricing_posted` | `QuotePricingPosted` | `quote_pricing_pipeline.rs` | ✅ `quote_id, tenant_id, feature_graph_hash, idempotent, valid_until_iso, actor, idempotency_key` |
| `quote.pricing_failed` | `QuotePricingFailed` | `quote_pricing_pipeline.rs` | ◑ quote_id, failure stage/class/message |
| `quote.pricing_failure_classified` | `QuotePricingFailureClassified` | `quote_pricing_pipeline.rs` | ◑ quote_id, classification (permanent/transient) |
| `quote.priced_writeback_outcome` | `QuotePricedWritebackOutcome` | `quote_pricing_pipeline.rs` | ◑ quote_id, writeback status (storefront) |
| `quote.poll_outcome` | `QuotePollOutcome` | `quote_pricing_pipeline.rs` | ◑ poll summary, counts |
| `quote.material_grade_edited` | `QuotePricingMaterialEdited` | `quote_pricing_pipeline.rs` | ◑ quote_id, old/new material grade |
| `quote.pricing_failure_deleted` | `QuotePricingFailureDeleted` | `quote_pricing_pipeline.rs` | ◑ quote_id, deleted failed-job ref (S391 F) |
| `quote.operator_accepted` | `QuotePricingOperatorAccepted` | `serve.rs` | ◑ quote_id, actor, accepted_at |
| `quote.operator_refused` | `QuoteOperatorRefused` | `quote_refuse.rs` | ✅ `quote_id, reason, refused_at, customer_email_present, actor, idempotency_key` (S403) |
| `quote.pipeline_python_resolved` | `PipelinePythonResolved` | `quote_pricing_pipeline.rs` | ◑ resolved interpreter path, modules importable (S421) |
| `quote.pricing_daemon_panicked` | `QuotePricingDaemonPanicked` | `quote_pricing_pipeline.rs` | ◑ panic message, stage |
| `quote.pricing_jobs_index_migrated` | `QuotePricingJobsIndexMigrated` | `quote_pricing_pipeline.rs` | ◑ migration counts |
| `quote.email_outbox_fetched` | `EmailOutboxFetched` | `email_outbox_poll_daemon.rs` | ◑ batch size |
| `quote.email_outbox_claimed` | `EmailOutboxClaimed` | `email_outbox_poll_daemon.rs` | ◑ queue_row_id |
| `quote.email_outbox_sent` | `EmailOutboxSent` | `email_outbox_poll_daemon.rs` | ◑ queue_row_id, recipient (dedup guard S391 E) |
| `quote.email_outbox_failed` | `EmailOutboxFailed` | `email_outbox_poll_daemon.rs` | ◑ queue_row_id, error_class |
| `quote.pdf_rerender_enqueued` | `QuotePdfRerenderEnqueued` | `quote_pricing_pipeline.rs` | ◑ quote_id |
| `quote.pdf_rerendered` | `QuotePdfRerendered` | `quote_pricing_pipeline.rs` | ◑ quote_id, pdf hash |
| `quote.pdf_rerender_failed` | `QuotePdfRerenderFailed` | `quote_pricing_pipeline.rs` | ◑ quote_id, error |
| `quote.material_catalogue_changed` | `MaterialCatalogueChanged` | `quoting_tunables.rs` | ◑ before/after diff of catalogue |
| `quote.material_catalogue_pushed` | `MaterialCataloguePushed` | `quoting_tunables.rs` | ◑ push target, count |
| `quote.complexity_rules_changed` | `ComplexityRulesChanged` | `quoting_tunables.rs` | ◑ rules diff |
| `quote.tolerance_multipliers_changed` | `ToleranceMultipliersChanged` | `quoting_tunables.rs` | ◑ multipliers diff |
| `quote.parameters_changed` | `ParametersChanged` | `quoting_tunables.rs` | ◑ params diff (7 tunables S418) |
| `quote.stock_adjustments_changed` | `StockAdjustmentsChanged` | `quoting_tunables.rs` | ◑ stock adjustment diff |
| `quote.stock_alert_triggered` | `QuoteStockAlertTriggered` | `serve.rs` | ◑ quote_id, material, shortfall |
| `quote.deal_issued` | `QuoteDealIssued` | `quote_deal.rs` | ◑ quote_id, deal/invoice link |
| `quote.sales_order_created` | `QuoteSalesOrderCreated` | `quote_deal.rs` | ◑ quote_id, sales_order_id |
| `quote.work_order_created` | `QuoteWorkOrderCreated` | `quote_deal.rs` | ◑ quote_id, work_order_id |
| **Quote intake (storefront poller)** |
| `system.quote_intake_poll_attempted` | `QuoteIntakePollAttempted` | `serve.rs` | ◑ poll heartbeat (high-frequency — filtered from dashboard) |
| `system.quote_intake_poll_completed` | `QuoteIntakePollCompleted` | `serve.rs` / `aberp-quote-intake` | ◑ poll summary (v1, superseded) |
| `system.quote_intake_poll_failed` | `QuoteIntakePollFailed` | `serve.rs` | ◑ error |
| `system.quote_intake_row_added` | `QuoteIntakeRowAdded` | `serve.rs` | ◑ quote_id ingested |
| **Inventory (material reservations)** |
| `inventory.material_reserved` | `MaterialReserved` | `material_inventory.rs` | ◑ material, qty, reservation ref |
| `inventory.material_committed` | `MaterialCommitted` | `material_inventory.rs` | ◑ material, qty |
| `inventory.material_consumed` | `MaterialConsumed` | `material_inventory.rs` | ◑ material, qty |
| `inventory.material_released` | `MaterialReleased` | `material_inventory.rs` | ◑ material, qty |
| **MES — Stage-3 manufacturing + adapter registry** |
| `mes.stock_movement_recorded` | `StockMovementRecorded` | `material_inventory.rs` | ◑ movement delta |
| `mes.work_order_created` | `WorkOrderCreated` | `crates/aberp-work-orders` | ◑ work_order_id, qty, part |
| `mes.work_order_state_changed` | `WorkOrderStateChanged` | `crates/aberp-work-orders` | ◑ work_order_id, old/new state |
| `mes.routing_op_state_changed` | `RoutingOpStateChanged` | `crates/aberp-work-orders` | ◑ op_id, old/new state |
| `mes.qa_inspection_created` | `QaInspectionCreated` | `crates/aberp-qa` | ◑ inspection_id |
| `mes.qa_inspection_decided` | `QaInspectionDecided` | `crates/aberp-qa` | ◑ inspection_id, verdict |
| `mes.dispatch_created` | `DispatchCreated` | `crates/aberp-dispatch` | ◑ dispatch_id |
| `mes.dispatch_shipped` | `DispatchShipped` | `crates/aberp-dispatch` | ◑ dispatch_id, carrier |
| `mes.adapter_event` | `MesAdapterEvent` | `mes_manager.rs` | ◑ adapter event passthrough |
| `mes.adapter_added` | `AdapterAdded` | `mes_manager.rs` | ◑ adapter_id, config |
| `mes.adapter_updated` | `AdapterUpdated` | `mes_manager.rs` | ◑ adapter_id, diff |
| `mes.adapter_removed` | `AdapterRemoved` | `mes_manager.rs` | ◑ adapter_id |
| `mes.adapter_health_transitioned` | `AdapterHealthTransitioned` | `serve.rs` | ◑ adapter_id, old/new health |
| **NAV — AP-incoming + restore-from-NAV** |
| `system.incoming_invoice_ingested` | `IncomingInvoiceIngested` | `incoming_invoices.rs` | ◑ nav invoice number, partner |
| `system.incoming_invoice_status_changed` | `IncomingInvoiceStatusChanged` | `incoming_invoices.rs` | ◑ invoice ref, old/new status |
| `system.incoming_invoice_sync_cycle_completed` | `IncomingInvoiceSyncCycleCompleted` | `incoming_invoices.rs` | ◑ cycle counts |
| `system.invoice_restored_from_nav` | `InvoiceRestoredFromNav` | `restore_from_nav_outgoing.rs` | ◑ invoice number restored |
| `system.extnav_partner_manual_link` | `ExtNavPartnerManualLink` | `restore_from_nav_outgoing.rs` | ◑ partner link |
| `system.restore_from_nav_run` | `RestoreFromNavRun` | `restore_from_nav_outgoing.rs` | ◑ run summary |
| `system.restore_buyer_backfill_cycle_completed` | `RestoreBuyerBackfillCycleCompleted` | `restore_from_nav_buyer_backfill.rs` | ◑ backfill counts |
| **Mail relay (outgoing invoice email)** |
| `email.relay_queued` | `EmailRelayQueued` | `email_outbox_poll_daemon.rs` | ◑ queue row, recipient |
| `email.relay_sent` | `EmailRelaySent` | `email_outbox_poll_daemon.rs` | ◑ queue row, recipient |
| `email.relay_failed` | `EmailRelayFailed` | `email_outbox_poll_daemon.rs` | ◑ queue row, error_class |
| **System lifecycle / numbering** |
| `system.first_prod_launch_acknowledged` | `FirstProdLaunchAcknowledged` | `serve.rs` | ◑ acknowledged_at, operator |
| `system.upgrade_snapshot_mismatch` | `UpgradeSnapshotMismatch` | `serve.rs` | ◑ expected/found snapshot |
| `system.daemon_shutdown_completed` | `DaemonShutdownCompleted` | `shutdown.rs` | ◑ clean-shutdown summary |
| `system.numbering_template_changed` | `NumberingTemplateChanged` | `serve.rs` | ✅ `old_start_value, new_start_value, preview, changed_at, actor` (S401/S394) |
| **Compliance / defense (personnel, material genealogy, export, CUI, supplier, incident)** |
| `personnel.id_registered` | `PersonnelIdRegistered` | `crates/aberp-digital-id` / `crates/aberp-compliance` | ◑ personnel id ref |
| `personnel.signature_applied` | `PersonnelSignatureApplied` | `crates/aberp-digital-id` | ◑ signed artifact ref |
| `personnel.access_granted` | `PersonnelAccessGranted` | `crates/aberp-compliance` | ◑ subject, capability |
| `personnel.access_denied` | `PersonnelAccessDenied` | `crates/aberp-compliance` | ◑ subject, capability, reason |
| `material.cert_attached` | `MaterialCertAttached` | `crates/aberp-compliance` | ◑ material, cert ref |
| `material.heat_lot_assigned` | `MaterialHeatLotAssigned` | `crates/aberp-compliance` | ◑ material, heat lot |
| `part.serial_assigned` | `PartSerialAssigned` | `crates/aberp-compliance` | ◑ part, serial |
| `part.uid_marked` | `PartUidMarked` | `crates/aberp-compliance` | ◑ part, UID mark |
| `export.classification_set` | `ExportClassificationSet` | `crates/aberp-compliance` | ◑ item, ECCN/classification |
| `export.access_check` | `ExportAccessCheck` | `crates/aberp-compliance` | ◑ subject, item, verdict |
| `export.shipment_logged` | `ExportShipmentLogged` | `crates/aberp-compliance` | ◑ shipment, destination |
| `cui.marking_applied` | `CuiMarkingApplied` | `crates/aberp-compliance` | ◑ artifact, CUI marking |
| `cui.access_event` | `CuiAccessEvent` | `crates/aberp-compliance` | ◑ subject, artifact |
| `supplier.dpas_priority_set` | `SupplierDpasPrioritySet` | `crates/aberp-compliance` | ◑ supplier, DPAS rating (`DpasRating::as_str`) |
| `supplier.export_screened` | `SupplierExportScreened` | `crates/aberp-compliance` | ◑ supplier, screening verdict |
| `incident.cyber_detected` | `IncidentCyberDetected` | `crates/aberp-compliance` | ◑ severity (`IncidentSeverity::as_str`), source (`DetectionSource::as_str`) |

> **Honesty note (rule 12).** The ✅ rows had their payload fields read at the emit site by the survey.
> The ◑ rows are summarised from the variant's `event_kind.rs` doc-comment and name; **S424 must read the
> exact `#[doc]` block above each variant in `event_kind.rs` before building any kind-specific payload
> summariser.** Do not ship a summariser that silently mislabels a field.

---

## 2. Audit-ledger schema + invariants

### 2.1 Storage schema (`crates/audit-ledger/src/storage/schema.rs`)

Single table, one row per entry, **one DuckDB file per tenant** (ADR-0002 — file *is* the tenant scope):

```sql
CREATE TABLE IF NOT EXISTS audit_ledger (
    id              VARCHAR  NOT NULL,   -- aud_<ULID> (prefixed)
    seq             BIGINT   NOT NULL,   -- contiguous from 1, gap-free
    prev_hash       BLOB     NOT NULL,   -- 32B SHA-256 chain link
    time_wall       VARCHAR  NOT NULL,   -- RFC3339 wall clock
    time_mono       BIGINT   NOT NULL,   -- monotonic nanos since process_start
    actor           VARCHAR  NOT NULL,   -- JSON {session_id,user_id,capabilities}
    binary_hash     BLOB     NOT NULL,   -- 32B SHA-256 of the producing binary
    tenant_id       VARCHAR  NOT NULL,   -- redundant w/ file scope; chain genesis input
    kind            VARCHAR  NOT NULL,   -- EventKind::as_str()
    payload         BLOB     NOT NULL,   -- serde_json bytes (NOT cbor)
    idempotency_key VARCHAR,             -- nullable
    entry_hash      BLOB     NOT NULL    -- 32B SHA-256 of this entry's canonical CBOR
);
```

**No secondary indexes. No CHECK constraints.** Both were deliberately removed:
- **`UNIQUE(seq)` / `UNIQUE(id)`** dropped in S341 — they were the only ART indexes, and DuckDB 1.5.x
  corrupts a file-backed ART on insert (`duckdb/duckdb#23046`). `migrate_drop_unique_art_if_present`
  transparently rebuilds legacy files at boot.
- **`CHECK (seq >= 1)` / `CHECK (time_mono >= 0)`** dropped in S410 ([[no-sql-specific]]).

This is **load-bearing for the screen's query design (§3, §6):** there is no index to filter or sort on.
Every read is a full table scan in `seq` order. **S424 must not add an index** — that re-enters the
duckdb#23046 corruption class and violates [[no-sql-specific]]. Filtering/sorting/pagination happen in
Rust over the scanned rows (acceptable — per-tenant ledgers are thousands of rows, not millions).

**Existing read SQL** (the only queries; all index-free scans):
- `SELECT_ALL` — every row `ORDER BY seq ASC`.
- `SELECT_HEAD` — highest seq (append path).
- `SELECT_RECENT` — `ORDER BY seq DESC LIMIT ?` (dashboard tile).

### 2.2 Sequence newtype + validation (`entry/ids.rs`)

`Sequence(u64)`: floor of `1` enforced in code (`Sequence::new(0) → None`), `FIRST = 1`, `next()` is
checked-add. The `>= 1` invariant that DDL used to carry now lives here; a forged `seq` is caught by
`verify_chain`.

### 2.3 Hash chain (`chain/verify.rs`, `chain/compute.rs`, `chain/genesis.rs`)

`verify_chain(tenant, entries)` walks entries and checks **four invariants**, failing loud at the first
divergence (returns the offending `seq` + reason):
1. **Order** — `seq` starts at 1, advances by exactly 1, contiguously.
2. **Chain link** — `entry[N].prev_hash == entry[N-1].entry_hash` (or `genesis_hash(tenant)` for N=1).
3. **Per-entry integrity** — `entry[N].entry_hash == SHA-256(canonical_CBOR(entry without entry_hash))`.
   The canonical encoder (`crate::canonical`, RFC 8949 §4.2.1 ordering) is the *only* place CBOR is used.
4. **Loud failure** — stops at the first tampered/out-of-order entry; does not continue.

> **Critical design consequence for the screen.** `verify_chain` is **whole-chain from seq=1** — you
> cannot verify an arbitrary page (e.g. seq 500–550) in isolation, because invariants 1 & 2 need the
> prior entry's hash and a contiguous run from genesis. **But invariant 3 (per-entry integrity) IS
> independently checkable per row** via the re-exported `compute_entry_hash(entry)`. This split drives
> the chain-status design in §3.4.

### 2.4 Public read API (the building blocks the screen uses)

Exported from `crates/audit-ledger/src/lib.rs`:
- `Ledger::open(path, tenant, binary_hash)` / `Ledger::from_connection(...)` — attach.
- `Ledger::entries() -> Vec<Entry>` — full scan, seq order.
- `Ledger::recent(limit) -> Vec<Entry>` (free fn `recent_entries(conn, limit)`) — DESC, limit.
- `Ledger::verify_chain() -> Result<u64, _>` — whole-chain verify.
- `compute_entry_hash(&Entry)` + `genesis_hash(&TenantId)` — per-entry / anchor primitives (PR-22).

**No new crate function is strictly required** for a first cut: `entries()` + `compute_entry_hash` cover
filtering, pagination, and per-row integrity entirely in `serve.rs`. (An optional, additive
`recent_entries_before(conn, seq, limit)` could make backward pagination a smaller scan later — **not
needed for v1, and it must remain index-free**.)

---

## 3. Wire shape for `/api/audit-events`

### 3.1 Architecture bridge (3 thin layers — verified)

```
SPA (Svelte)  --invoke("audit_events", {query})-->  apps/aberp-ui/src/commands.rs
   commands.rs  --forward_get("/audit-events?<qs>", authed=true)-->  loopback HTTP
      serve.rs  handle_audit_events(State, Query)  -->  Ledger::open + entries() + filter/page/verify
```

`commands.rs` relays the backend JSON as opaque `serde_json::Value` (no DTO duplication, per ADR-0021
§Part B). So the new endpoint is **one axum handler + one tauri pass-through command + one api.ts
wrapper** on the plumbing side.

### 3.2 Request — query struct (axum `Query<...>`, all optional, `#[serde(default)]`)

```rust
#[derive(Debug, Deserialize)]
pub struct AuditEventsQuery {
    // Filtering
    pub from: Option<String>,            // ISO-8601 inclusive lower bound on time_wall
    pub to: Option<String>,              // ISO-8601 inclusive upper bound
    pub kinds: Option<String>,           // comma-joined as_str values e.g. "invoice.payment_recorded,quote.operator_refused"
    pub subject: Option<String>,         // substring match on resolved subject id (invoice_id/quote_id/partner_id) extracted from payload
    pub operator: Option<String>,        // exact/substring match on actor.user_id
    pub q: Option<String>,               // free-text needle across kind + actor + payload JSON text
    // Sorting
    pub sort: Option<String>,            // "seq" (default) | "occurred_at"
    pub dir: Option<String>,             // "desc" (default) | "asc"
    // Pagination (cursor on seq)
    pub after_seq: Option<u64>,          // forward page: rows with seq < after_seq (when dir=desc)
    pub before_seq: Option<u64>,         // backward page
    pub limit: Option<u32>,              // default 50, clamp 1..=200
}
```

**Why seq-cursor, not offset.** `seq` is dense, gap-free, and monotonic — a perfect stable cursor. Offset
pagination would re-scan + skip; a seq cursor lets the handler stop scanning once it has `limit` matches
past the cursor. (The scan itself is still O(N) because there's no index, but the *response* is bounded
and stable under concurrent appends.)

### 3.3 Response

```rust
#[derive(Debug, Serialize)]
pub struct AuditEventsResponse {
    pub events: Vec<AuditEventRow>,
    pub page: PageInfo,
    pub chain: ChainStatus,              // whole-ledger verdict (see §3.4)
}

#[derive(Debug, Serialize)]
pub struct AuditEventRow {
    pub id: String,                      // aud_<ULID>
    pub seq: u64,
    pub kind: String,                    // EventKind::as_str()
    pub occurred_at: String,             // RFC3339 (time_wall)
    pub actor: String,                   // actor.user_id (operator)
    pub subject: Option<String>,         // resolved invoice_id/quote_id/partner_id (kind-aware extractor)
    pub summary: String,                 // ONE-LINE human summary (kind-aware; NOT the full payload)
    pub hash_ok: bool,                   // per-entry integrity: compute_entry_hash == stored entry_hash
    pub has_payload: bool,               // true if a non-empty JSON payload exists to drill into
    pub prev_hash_hex: String,           // 64-char hex (chain anchor, for the expanded row)
    pub entry_hash_hex: String,          // 64-char hex
}

#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub returned: u32,
    pub next_cursor: Option<u64>,        // seq to pass as after_seq for the next page
    pub prev_cursor: Option<u64>,
    pub total_matched: u32,              // count of rows matching filters (full scan already in hand)
}

#[derive(Debug, Serialize)]
pub struct ChainStatus {
    pub verified: bool,                  // whole chain genesis..head intact?
    pub head_seq: u64,
    pub first_divergence_seq: Option<u64>, // Some(seq) + reason if verify_chain failed
    pub reason: Option<String>,
}
```

**Payload is NOT in the list row.** This is the single most important sizing decision. Several payloads
embed base64 NAV request/response XML (`InvoiceSubmissionAttempt/Response`, `InvoiceAckStatus`,
annulment variants) that run **tens of KB each**; 50 of them would be multiple MB. So:
- **List rows carry only a one-line `summary`** (kind-aware: e.g. `invoice.payment_recorded` →
  `"€1,234.00 BankTransfer · INV-2026/00042"`). Reuse the existing kind→label vocabulary already in the
  SPA's `invoice-timeline.ts` / `pricing-job-detail.ts`.
- **Full payload is fetched lazily on row expansion** via the *existing* drill-downs
  (`GET /audit/:invoice_id` → `AuditEntryView` with full `payload: serde_json::Value`) where the subject
  is an invoice, or a small new `GET /audit-events/:seq` returning the single full entry for any kind.

### 3.4 Chain-status design (resolves the whole-chain-vs-per-row tension)

- **Per-row `hash_ok`** = `compute_entry_hash(entry) == entry.entry_hash` (invariant 3, independently
  checkable). Renders the ✓/✗ in the chain-status column for every visible row, cheaply.
- **Whole-ledger `chain.verified`** = one `verify_chain()` call per request (reads full scan we already
  have; O(N), N small). If it fails, the SPA shows a red banner with `first_divergence_seq` + `reason`,
  and rows at/after that seq render ✗ regardless of `hash_ok`.

This gives a meaningful per-row tick **and** the tamper-evident whole-chain verdict, without pretending a
single page can self-verify against genesis.

### 3.5 Response-size estimate (one page, 50 events)

Per row ≈ id(34) + seq(8) + kind(40) + occurred_at(25) + actor(20) + subject(20) + summary(≤120) +
hash_ok(5) + has_payload(5) + 2×hash_hex(2×64) + JSON key overhead (~120) ≈ **~600 B/row**. 50 rows ≈
**~30 KB** + envelope. Comfortably one SPA response. (Contrast: returning full payloads could be 2–5 MB —
the reason §3.3 keeps them out.)

---

## 4. SPA component design (PricingJobsList / S411 pattern)

**Stack (verified):** Svelte 5.0, Vite 6, Vitest 4.1.7, in a Tauri 2.0 shell. SPA root:
`apps/aberp-ui/ui`. Dark-theme tokens in `src/lib/tokens.css` (default + only theme — [[spa-dark-theme-default]]).
Router is the zero-dep hash router `src/lib/router.ts` (`AppRoute` union + render arm in `App.svelte`).

### 4.1 Files to add (mirrors S411 exactly)

| File | Role |
|---|---|
| `src/lib/audit-events-list.ts` | **Pure** `sortAuditEvents` / `filterAuditEvents` / `parseSearch` + `EMPTY_AUDIT_FILTER` / `isAuditFilterEmpty` (the S411 `pricing-jobs-list.ts` shape) |
| `src/lib/audit-events-list.test.ts` | Vitest — sort stability, filter composition, search-syntax parser, operator journey |
| `src/routes/AuditEvents.svelte` | List screen: sortable headers, filter chips, search box, expandable rows, empty state |
| `src/lib/AuditEventDetail.svelte` (or inline expansion) | Row expansion: formatted payload JSON + hash anchors (reuse `InvoiceTimeline.svelte` idioms) |
| `src/lib/api.ts` | `listAuditEvents(query): Promise<AuditEventsResponse>` (one `invoke` wrapper) |
| `src/lib/router.ts` + `App.svelte` | add `"audit-events"` to `AppRoute` + a render arm + a nav entry |

### 4.2 Pure helpers (port of `sortJobs`/`filterJobs`)

```ts
export type AuditSortKey = "seq" | "occurred_at" | "kind" | "actor";
export type SortDir = "asc" | "desc";

export interface AuditFilterSpec {
  kinds: Set<string>;          // empty = all
  search: string;              // free text + special syntax (see 4.4)
  from?: string; to?: string;  // ISO date bounds (optional client pre-filter)
}

export function sortAuditEvents<R extends AuditEventRow>(rows: R[], key: AuditSortKey, dir: SortDir): R[];
export function filterAuditEvents<R extends AuditEventRow>(rows: R[], spec: AuditFilterSpec): R[];
```

Default sort `seq` desc (newest first), tie-break by `seq` (already unique → deterministic). Visible rows:
`$derived(sortAuditEvents(filterAuditEvents(rows, filter), sort.key, sort.dir))` — identical to S411.

> **Server vs client filtering.** v1 can do the *common* filter (kind chips, date, operator, subject)
> **server-side** in the handler (the scan is there anyway), and keep the pure-lib filter for
> **client-side refinement of the already-fetched page** (live search box) so typing doesn't round-trip.
> Pick ONE source of truth per facet to avoid the rule-7 blend: **chips + date + operator + subject =
> server query params; free-text `q` = client-side over the loaded page** (and also passed to the server
> as `q` for cross-page search when the box is non-trivial). Document this split in the component header.

### 4.3 Columns

`timestamp` (relative + hover-absolute, per `invoice-timeline.ts` S195 idiom) · `seq` (mono) · `kind`
(chip-coloured by domain) · `subject` · `operator` · `summary` · `chain` (✓ `hash_ok` / ✗). Sortable:
timestamp, seq, kind, operator (quiet-chrome `<button>` in `<th>` with ▲/▼ glyph + `aria-sort`, S411).

### 4.4 Search box — free-text + special syntax

`parseSearch(raw)` → `{ kinds?: string[], subject?: string, operator?: string, text?: string }`:
- `kind:quote.operator_refused` (or shorthand `kind:operator_refused`) → kind facet
- `quote:8d839e` / `invoice:INV-2026` → subject substring
- `op:ervin` → operator substring
- bare words → free-text needle over kind + actor + summary + (server-side) payload JSON

Multiple tokens AND together. This parser is **pure + unit-tested** (the highest-value test in the suite —
it's the one piece of real logic). Bilingual chip labels (HU / EN) per existing convention.

### 4.5 Filter chips

Top-N most common domains as chips: **All · Invoice · Quote · NAV/System · Email · MES · Compliance**
(map each chip to its set of `as_str` prefixes). Rarer kinds reachable via the `kind:` search syntax or a
"more kinds…" picker. Active chip = `aria-pressed` + accent bg (S411). Heartbeat noise
(`quote_intake_poll_attempted/completed`) excluded by default exactly as the dashboard tile filters them
(`serve.rs:14205`), with a "show heartbeats" toggle.

### 4.6 Row expansion

Expand → lazy-fetch full entry (§3.3), render: (1) **payload** as pretty JSON (collapse base64 XML blobs
behind a "show raw XML / decoded" toggle — they're huge), (2) **hash anchors**: `prev_hash` → `entry_hash`
with the per-row ✓/✗, mirroring `InvoiceTimeline.svelte`'s evidence styling. For invoice/quote subjects,
a "view full chain" link into the existing `InvoiceTimeline` / `PricingJobDetail`.

### 4.7 Empty state

`"Nincs a szűrőnek megfelelő esemény / No events match"` + a **Clear** chip rendered only when
`!isAuditFilterEmpty(filter)` (rule 12 — no no-op affordance), per S411.

---

## 5. State-machine reverse-engineering (the workflow sidebar)

For a chosen subject, the screen can reconstruct its canonical path from the subject's ordered events and
show a **step-checklist with per-step status** (✓ reached / ⏳ pending / ✗ failed). The data already
exists per-subject (`get_audit_for_invoice` returns one invoice's chain). Recommended viz: **checklist
first** (see §7 — Sankey is a stretch). Five worked domains:

### 5.1 Outgoing invoice issuance → NAV → paid
**Success:** `InvoiceSequenceReserved → InvoiceDraftCreated → InvoiceStaged?* → InvoiceSubmissionAttempt →
InvoiceSubmissionResponse → InvoiceAckStatus(RECEIVED→PROCESSING→SAVED) → InvoicePaymentRecorded` (+
`InvoiceEmailedSent`).
**Failure:** `InvoiceSubmissionAttemptFailed` (transport), or `InvoiceAckStatus(ABORTED)` (NAV reject) →
`InvoiceRetryRequested` (loop) → terminal `InvoiceMarkedAbandoned`. `InvoiceCheckPerformed` is the
pre-flight existence probe (S392).

### 5.2 Storno / modification of an issued invoice
**Success:** `InvoiceStornoIssued` (or `InvoiceModificationIssued`) → its own
`Submission* → AckStatus(SAVED)`. Chains link back via `base_invoice_id` / `base_sequence_number` +
`modification_index` in the payload (the SPA already extracts `chain_base_invoice_id`).

### 5.3 Technical annulment
**Success:** `InvoiceTechnicalAnnulmentRequested → InvoiceAnnulmentSubmissionAttempt →
InvoiceAnnulmentSubmissionResponse → InvoiceAnnulmentAckStatus(SAVED) →
InvoiceAnnulmentReceiverConfirmation`.
**Failure:** annulment ack `ABORTED`, or no receiver confirmation (stuck).

### 5.4 Auto-quote pricing pipeline
**Success:** `QuotePricingFetched → QuotePricingExtracted → QuotePricingPriced → QuotePricingRendered →
QuotePricingPosted` (writeback `QuotePricedWritebackOutcome`; email via `EmailOutbox*`). Operator
endpoints: `QuotePricingOperatorAccepted` (→ DEAL) **or** `QuoteOperatorRefused` (S403).
**Failure:** `QuotePricingFailed → QuotePricingFailureClassified` (permanent vs transient); permanent
failures may be cleared via `QuotePricingFailureDeleted`; `QuotePricingDaemonPanicked` is the crash arm.

### 5.5 Quote → DEAL → production
**Success:** `QuoteDealIssued → QuoteSalesOrderCreated → QuoteWorkOrderCreated → (WorkOrderStateChanged*,
RoutingOpStateChanged*) → QaInspectionCreated → QaInspectionDecided → DispatchCreated → DispatchShipped`,
with inventory `MaterialReserved → MaterialCommitted → MaterialConsumed` (or `MaterialReleased` on cancel).

> The state-machine definitions live as **pure data** (a per-domain ordered step list keyed by EventKind)
> in `audit-events-list.ts` so they're unit-tested and reused by the checklist renderer. This is the
> "what does invoice X's chain look like" sidebar.

---

## 6. Implementation estimate

### 6.1 Files to touch

**Backend (Rust):**
| File | Change | ~LOC |
|---|---|---|
| `apps/aberp/src/serve.rs` | `AuditEventsQuery`, `AuditEventsResponse`/`AuditEventRow`/`PageInfo`/`ChainStatus`, `handle_audit_events`, `audit_events_request` (open ledger, scan, filter, page, per-row `hash_ok`, whole-chain verify), route reg; optional `GET /audit-events/:seq` for full single entry; **kind-aware `subject_of(entry)` + `summary_of(entry)` extractors** | ~320 |
| `apps/aberp/src/audit_payloads.rs` *(or new `audit_summary.rs`)* | one-line summariser per kind (reuse typed payloads; ◑ rows need their doc-comments read first) | ~180 |
| `crates/audit-ledger` | **none required for v1** (reuse `entries` + `compute_entry_hash`). *Optional later:* additive `recent_entries_before(conn, seq, limit)` — index-free | 0 (v1) |

**Bridge (Tauri):**
| File | Change | ~LOC |
|---|---|---|
| `apps/aberp-ui/src/commands.rs` | `#[tauri::command] audit_events(state, query: Value)` → `forward_get("/audit-events?<qs>", true)`; `audit_event(seq)` for detail | ~30 |
| `apps/aberp-ui/src/lib.rs` | register the new command(s) in the invoke handler | ~2 |

**SPA:**
| File | Change | ~LOC |
|---|---|---|
| `src/lib/audit-events-list.ts` | pure sort/filter/`parseSearch` + state-machine step data | ~260 |
| `src/lib/api.ts` | `listAuditEvents` + `getAuditEvent` wrappers + types | ~60 |
| `src/routes/AuditEvents.svelte` | list screen (headers/chips/search/rows/empty) | ~420 |
| `src/lib/AuditEventDetail.svelte` | expansion (payload + hash anchors + checklist sidebar) | ~220 |
| `src/lib/router.ts` + `App.svelte` + nav | wire the route | ~20 |

**Tests:**
| File | Change | ~LOC |
|---|---|---|
| `apps/aberp/tests/audit_events_*.rs` | filters (kind/date/operator/subject), seq-cursor pagination, per-row `hash_ok`, whole-chain `verified`+divergence, payload-omitted-from-list | ~280 |
| `src/lib/audit-events-list.test.ts` | sort stability, filter composition, **search-syntax parser** (the load-bearing one), operator journey, state-machine step mapping | ~320 |

**Rough total: ~2,100–2,400 LOC** across ~13 files. Comfortably 1 session for backend+bridge and 1 for
SPA, or a single focused session if the summariser scope (◑ rows) is trimmed to the high-traffic kinds.

### 6.2 Risk factors (scope-expanders to watch)

1. **No new indexes allowed.** Any temptation to "index by kind/time for fast filter" violates
   [[no-sql-specific]] **and** re-enters duckdb#23046. The scan-in-Rust design is the *only* allowed
   shape. **Flag, hard constraint.**
2. **Payload bloat.** If the list row accidentally carries full payloads, a page of NAV-submission events
   is multi-MB. The summary-in-list / lazy-full-on-expand split (§3.3) is mandatory, not optional.
3. **◑ payload summarisers.** The long-tail variants need their `event_kind.rs` doc-comments read to write
   a correct one-liner. Under-scope risk: a generic `format!("{:?}", kind)` summary is acceptable as the
   fallback for rare kinds — **fail-soft to the kind name, never mislabel a field** (rule 12).
4. **Full-scan cost at scale.** Per-tenant ledgers are small today, but `verify_chain` on every request is
   O(N). If a tenant's ledger grows large, cache the last-verified head per process (verify only the tail
   since last check). **Not needed for v1** — note it as a follow-up, don't build it speculatively (rule 2).
5. **Whole-chain verify reads everything.** The handler must read all entries to verify even if the page
   is 50 rows. That's the same cost the dashboard tile already pays; acceptable, but it means
   "pagination" bounds the *response*, not the *scan*.
6. **Heartbeat volume.** `quote_intake_poll_attempted` is high-frequency; default-excluding it (with a
   toggle) keeps the screen useful, matching the dashboard's existing filter.

---

## 7. Open decisions for Ervin

Each line states the trade-off; the **bold** is the recommendation.

1. **Tenant scope.** ABERP is single-tenant-per-process (`AppState.tenant`, one DuckDB file). A
   super-operator multi-tenant view would need cross-file reads the architecture doesn't currently do.
   **Recommend: single-tenant (current process's ledger) for v1** — matches every other screen; revisit
   only if a console-of-tenants is wanted.
2. **Retention / purge UI.** The ledger is append-only and tamper-evident *by design* — there is no
   delete API, and per ADR-0030 the mirror is the durability anchor. A "purge older than X" control would
   break the hash chain and contradict the whole point. **Recommend: read-only screen, NO purge.** (If
   archival is ever needed, it's an export-bundle + offline-archive flow, not an in-app delete.) Confirm
   the retention policy you want documented.
3. **Payload privacy / redaction.** Most payloads are business data. Two carry sensitivity:
   `InvoiceEmailedSent` already scrubs SMTP secrets (ADR-0047 §4); NAV request/response XML may contain
   customer tax numbers / addresses. `binary_hash` and `actor.capabilities` are operational, not secret.
   **Recommend: no new redaction for v1** (operator-only tool, single tenant, data already visible in the
   invoice screens) — but **decide whether NAV XML blobs should be collapsed-by-default** in the
   expansion (they are large and contain customer PII).
4. **State-machine viz: Sankey vs checklist.** A Sankey is pretty but needs flow-volume data and a charting
   dep; the checklist (step list + per-step ✓/⏳/✗ from the subject's events) is buildable from data we
   already have, accessible, and on-theme. **Recommend: step-checklist for v1; Sankey deferred** unless you
   specifically want aggregate flow visualisation across many subjects.
5. **Real-time vs polling vs static.** The dashboard tile is fetch-on-load. WebSocket/push is a new
   transport this stack doesn't have. **Recommend: fetch-on-load + a manual Refresh button (+ optional
   30s poll toggle)** — matches `PricingJobsList`'s posture; no new infra.

---

## Appendix — verification posture

- All ✅ payload field lists, the schema, `verify_chain`, the read API, the Tauri↔axum bridge
  (`commands.rs::forward_get` → `serve.rs::handle_get_audit`), and the existing `AuditEntryView` /
  recent-activity tile were **read directly** in this session.
- ◑ payload rows are summarised from variant names + `event_kind.rs` doc-comments and are flagged for
  confirmation at implementation time.
- **No production code was modified.** This file is the sole deliverable, on local branch `session-423`.
- Counts pinned: **106 EventKind variants** (`grep -cE` on the enum arms), domains by `as_str` prefix.
