//! Library face of the ABERP binary — re-exports the internal modules so
//! integration tests under `apps/aberp/tests/` can drive the same
//! orchestration the `aberp` binary uses.
//!
//! # Why a library at the binary boundary
//!
//! Cargo does not let an integration test reach into `src/main.rs`'s
//! sibling modules. PR-7-B-3 needs an end-to-end conformance test
//! ("issue an invoice → submit it → assert transactionId persisted →
//! assert audit chain still verifies") that drives `submit_invoice::run`
//! directly. Splitting the binary crate into a thin `lib.rs` + a
//! `main.rs` that delegates is the standard Cargo workaround.
//!
//! The library exposes the modules at their existing paths so the
//! binary code (`main.rs`) and the integration tests share one set
//! of imports. Public surface is intentionally narrow: each module
//! is `pub` here only because the integration tests need it; nothing
//! is re-exported at the crate root because no other crate imports
//! `aberp`.

#![forbid(unsafe_code)]

pub mod ap_sync;
pub mod audit_payloads;
pub mod audit_query;
pub mod audit_summary;
pub mod avl_vendors;
pub mod binary_hash;
pub mod branding_config;
pub mod build_profile;
pub mod catalogue_push;
pub mod cli;
pub mod db_path_guard;
pub mod db_writer_lock;
pub mod drain_pending_retries;
pub mod drain_submission_queue;
// S275 / PR-264 / F22 — DuckDB-binding helpers + the project's
// RFC3339-VARCHAR timestamp-storage convention.
pub mod duckdb_helpers;
pub mod email_invoice;
// S281 / PR-266 — storefront email-relay surface (ADR-0007). The
// storefront POSTs `/api/internal/send-email` to ABERP; we validate +
// persist to `outbound_email_queue` (email_relay_queue) and let the
// background drain (email_relay_daemon) send via the existing SMTP
// creds per [[aberp-smtp-spoc]]. Dedicated keychain entry
// (email_relay_credentials) for independent rotation per ADR-0007 §Auth.
pub mod email_outbox_poll_daemon;
pub mod email_relay;
pub mod email_relay_credentials;
pub mod email_relay_daemon;
pub mod email_relay_queue;
pub mod export_invoice_bundle;
pub mod first_launch;
pub mod fs;
pub mod incoming_invoices;
pub mod invoice_bank_snapshot;
pub mod invoice_currency_metadata;
// S236 / PR-230b — pre-allocation Draft state. New `invoice_draft`
// table + `BillingInvoiceSpawner` replaces `NoopInvoiceSpawner` from
// PR-230. Closes the Stage 3 → Stage 1 hand-off without burning a
// gap-free sequence slot per ADR-0009 §169.
pub mod invoice_draft;
pub mod issue_invoice;
pub mod issue_modification;
pub mod issue_preflight;
pub mod issue_storno;
pub mod mark_abandoned;
pub mod mark_invoice_paid;
// S273 / PR-262 / ADR-0069 — material-side inventory balances +
// reservations. The DEAL saga writes through here to increment
// `committed_qty` on `inventory_balances` and insert a paired
// `inventory_reservations` row in the same tx.
pub mod material_inventory;
// S432 (ADR-0085) — heat-lot chain-of-custody traceability report.
pub mod material_traceability;
// S438 (ADR-0089) — per-unit part UID / serial marking + Part UID Lookup.
pub mod part_marking;
pub mod paths;
// S439 (ADR-0090) — defense quality management: NCR + CAPA workflow.
pub mod quality;
// S443 (ADR-0092) — QC dimensional inspection orchestrator (verdict +
// auto-NCR; manual entry today, probe sources stubbed).
pub mod qc_inspection;
// S229 / PR-225 — Stage 3 manufacturing-adapter framework boot wiring.
// Reads `ABERP_BARCODE_SCANNER_*` env vars and spawns the barcode
// scanner adapter + per-adapter ledger-writer task. Default-off.
pub mod mes_boot;
// S257 / PR-246 — `[[mes.adapters]]` seller.toml slot (7th preservation
// section) + operator-managed adapter lifecycle (Settings → Adapters).
pub mod margin_profiles;
pub mod mes_adapters_config;
pub mod mes_manager;
/// ADR-0108 Step 7 Part I — the approved-vendor-list family's STRICT DDL, its
/// typed carry and the reconciliation gate's arm. **One table** (`avl_vendors`,
/// 12 columns) and **not one number in it**: every column is `VARCHAR` on
/// DuckDB, so R1, R2, R3 and the §3.4 fold rules all have nothing to guard here.
/// Unblocks purchasing's cutover, which §9 sequences behind this family.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_avl;
/// ADR-0108 Step 5 — the invoice family's STRICT DDL, its typed carry, and the
/// reconciliation gate's arm for it. Same feature gate as the Step-4 migrator
/// it plugs into.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_billing;
/// ADR-0108 Step 7 Part F — the dispatch/shipment family's STRICT schema, carry
/// and gate arm. Two tables across two owning modules, fused by the one SQL
/// statement that joins them (`part_marking.rs:504`). **No money, no quantity
/// and no float at all**: its only number is `wo_part_marks.unit_index` (§3.2
/// F). Its one refusal is a duplicate natural key, because `wo_part_marks` has
/// no `PRIMARY KEY` for the gate's per-row arm to rely on.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_dispatch;
/// ADR-0108 Step 7 Part H — the email/relay family's STRICT DDL, its typed
/// carry and the reconciliation gate's arm. **One table**
/// (`outbound_email_queue`, 15 columns) and **no BLOB**: the attachment bytes
/// live on the filesystem and the row holds a rel-path, so what the carry has to
/// protect is the path and the potentially six-figure-byte message bodies.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_email;
/// ADR-0108 Step 7 Part B — the partners family's STRICT DDL, its typed carry,
/// and the reconciliation gate's arm for it. Same feature gate as the Step-4
/// migrator it plugs into.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_partners;
/// ADR-0108 Step 7 Part C — the products/inventory family's STRICT schema,
/// carry and gate arm, including the rule-7 resolution that brings the five
/// `DOUBLE` quantity columns onto the same exact representation as
/// `stock_movements.qty_delta`.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_products;
/// ADR-0108 Step 7 Part G — the purchasing / purchase-order family's STRICT
/// schema, carry and gate arm. Four tables from one owning module, and the
/// family that carries **most of the tree's remaining money**: five §3.2 A
/// `BIGINT` → `INTEGER` minor-unit columns. Its quantities are exact `i64`
/// counts (§3.2 F), not R2 decimals, and none of its four tables has a
/// `PRIMARY KEY` for the gate's per-row arm to rely on.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_purchasing;
/// ADR-0108 Step 7 Part E — the QA/QC family's STRICT schema, carry and gate
/// arm. Six tables across two owning modules, **no money and no quantity**: its
/// only numbers are one sequence integer (§3.2 F), eight dimensional
/// measurements that stay `REAL` (§3.2 E) and two booleans (§3.2 H).
#[cfg(feature = "sqlite-engine")]
pub mod migrate_quality;
/// ADR-0108 **Step 8** — the quoting family's STRICT schema, carry and gate arm.
/// Eleven tables across eight owning modules; **seven carry rows and four are
/// schema-only** (§6.3 drops quoting job history). It lands §3.2 D's six
/// float-money column instances as R2 canonical-decimal `TEXT` — three proved
/// per row, three declared without a row behind them — and never as `REAL`.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_quoting;
/// ADR-0108 Step 4 — the migrator + the reconciliation gate. Behind the
/// default-OFF `sqlite-engine` feature: this is the ONE binary that links both
/// engines, because it reads DuckDB and writes SQLite.
#[cfg(feature = "sqlite-engine")]
pub mod migrate_to_sqlite;
/// ADR-0108 Step 7 Part D — the work-orders/BOM family's STRICT schema, carry
/// and gate arm. Carries the tree's one money column that is R2 rather than R1
/// (`routings.est_cost_huf`, §3.2 B) and the family's one remaining float
/// (`work_orders.actual_machining_minutes`, §3.2 E).
#[cfg(feature = "sqlite-engine")]
pub mod migrate_work_orders;
pub mod mnb_rates_provider;
pub mod nav_number_probe;
pub mod nav_xml;
pub mod notes_history;
pub mod numbering;
pub mod observe_receiver_confirmation;
pub mod partners;
pub mod poll_ack;
pub mod poll_annulment_ack;
/// ADR-0108 Step 1 — the pre-migration snapshot + manifest, and the verifier
/// `run/rollback_to_duckdb.sh` calls to make the rollback *verified* (C-IV).
pub mod premigration;
pub mod print_invoice;
pub mod products;
pub mod purchasing;
pub mod quote_intake_config;
pub mod quote_intake_credentials;
pub mod quote_intake_query;
pub mod quote_margin;
// S255 / PR-244 — operator-clicked "Create draft invoice" on a staged
// quote_intake_log row. Mints an `invoice_draft` row with
// `source_quote_id` set + emits `InvoicePickedUpFromQuote`.
pub mod quote_pickup;
// S272 / PR-261 — DEAL saga (ADR-0067). Operator clicks DEAL on a quote
// intake row → single-tx mint of SO/WO placeholder ids + 3 audit
// entries. Replay-protected via a CAS on `deal_issued_at IS NULL`.
// Enforces EVE addendum 2 (REFRESH-typed ack when `stock_alert`) and
// addendum 3 (BIG/RED/single-use DEAL token, validated server-side).
pub mod quote_deal;
// S403 — operator REFUSE-with-reason saga (the DEAL step's negative
// counterpart). CAS-flips the intake row to `refused`, audits
// `quote.operator_refused`, and atomically queues the bilingual customer
// notification e-mail. No draft invoice is staged. The route layer
// (`serve.rs`) validates the reason + best-effort writes back the
// storefront `rejected` status.
pub mod quote_refuse;
// S271 / PR-260 — pure `stock_alert` recompute (EVE addendum 2). Used by
// the SPA Quotes list route to mark accepted quotes whose material has
// downgraded since acceptance. Sticky: only operator REFRESH (S272+)
// untriggers an alert.
pub mod quote_stock_alert;
// S279 / PR-265 — pricing-pipeline state machine + jobs table.
// Distinct from `quote_intake_log` (approved quotes awaiting DEAL);
// this table tracks `received → quoted` storefront-side state-flips
// the ABERP-driven pricing daemon walks rows through.
pub mod quote_pricing_jobs;
// S279 / PR-265 — orchestration glue around the three crates
// (`aberp-cad-extract-wrapper` extract / `aberp-quote-engine` price /
// `aberp-quote-pdf` render) + the storefront priced-writeback POST.
pub mod quote_calibration;
pub mod quote_pricing_pipeline;
// S430 / ADR-0083 — AES-256-GCM CAD-blob encryption-at-rest + read-audit.
pub mod cad_blob;
// S441 / ADR-0087 + ADR-0088 — DÁP/QES timestamp-anchored audit-chain boot
// wiring (service session + crash recovery + heartbeat actor).
pub mod audit_dap_boot;
// S325 / PR-25 — EVE addendum-2 customer-facing stock-alert banner
// producer: in-memory re-render queue + the daemon that drains it and
// re-POSTs `priced.pdf` with `stock_alert:true` to the storefront.
pub mod quote_pdf_rerender_daemon;
pub mod quote_pdf_rerender_queue;
pub mod quoting_machines;
pub mod quoting_materials;
// S267 / PR-256 — four tunable tables feeding the future
// `aberp-quote-engine`: complexity rules, tolerance multipliers, the
// global parameters singleton, and per-material × stock-status price
// adjustments. None of these push to the storefront — they are
// quoting-engine internals.
pub mod quoting_tunables;
pub mod recover_from_nav;
pub mod reports;
pub mod request_technical_annulment;
pub mod restore_from_nav_extract;
pub mod restore_from_nav_outgoing;
pub mod retry_submission;
pub mod runtime_discovery;
pub mod secrets_cache;
pub mod seller_banks;
pub mod seller_toml_backup;
pub mod serve;
pub mod setup_nav_credentials;
pub mod setup_seller_info;
pub mod shutdown;
pub mod smtp_config;
pub mod smtp_credentials;
pub mod snapshot;
pub mod storefront_credential;
pub mod storefront_origin_secret;
pub mod submission_lock;
pub mod submission_queue;
pub mod submit_annulment;
pub mod submit_invoice;
pub mod supplier_prices;
pub mod tenant_registry;
pub mod upgrade_snapshot;
