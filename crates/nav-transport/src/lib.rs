//! `aberp-nav-transport` — NAV TLS transport, credentials, SOAP envelope,
//! signature primitives, AES-128/ECB exchange-token decryption, and the
//! typed `tokenExchange` / `manageInvoice` operations.
//!
//! See ADR-0009 §4 (NAV authentication and credentials), ADR-0020 §1-3
//! (transport / credential / threat-model correction), ADR-0021 §A9 +
//! §A14 (AES-128/ECB / keychain).
//!
//! # PR-7-A scope (landed)
//!
//!   - [`NavEndpoint`], [`NavTransport`] — pinned-trust reqwest client.
//!   - [`NavCredentials`] — four-artifact keychain bundle.
//!
//! # PR-7-B-1 scope (this PR's first commit)
//!
//!   - [`signatures`] — SHA-512 `passwordHash`, SHA3-512
//!     `requestSignature` (with per-invoice-index extension for
//!     `manageInvoice` / `manageAnnulment`).
//!   - [`soap`] — hand-rolled NAV v3.0 SOAP envelope assembly
//!     (`<TokenExchangeRequest>`, `<ManageInvoiceRequest>`) per
//!     ADR-0021 §A8.
//!
//! # PR-7-B-2 scope (this PR's second commit)
//!
//!   - [`cipher`] — AES-128/ECB decryption of NAV's exchangeToken
//!     envelope per ADR-0020 §2 + ADR-0021 §A9 ("protocol-imposed by
//!     NAV; must not generalize").
//!   - [`operations::token_exchange`] — `tokenExchange` call against
//!     the pinned [`NavTransport`].
//!
//! # PR-7-B-3 scope (landed)
//!
//!   - [`operations::manage_invoice`] — `manageInvoice` call + typed
//!     response parsing + retryable/non-retryable error mapping per
//!     ADR-0009 §5.
//!
//! # PR-7-C-1 scope (landed)
//!
//!   - [`operations::query_transaction_status`] —
//!     `queryTransactionStatus` call + typed
//!     [`operations::query_transaction_status::ProcessingStatus`]
//!     (`RECEIVED` / `PROCESSING` / `SAVED` / `ABORTED`) parse. The
//!     bounded poll loop, audit-ledger emission per poll, and the
//!     `SubmittedInvoice → {Finalized, Rejected, SubmissionStuck}`
//!     typestate advance live in the binary
//!     (`apps/aberp/src/poll_ack.rs`, landed in PR-7-C-2).
//!
//! # PR-13 scope (this PR — ADR-0026 §3)
//!
//!   - [`operations::manage_annulment`] — `manageAnnulment` call +
//!     typed [`operations::manage_annulment::ManageAnnulmentOutcome`]
//!     (transactionId + verbatim request/response bytes for the
//!     audit-evidence pair). Wire half of the technical-annulment
//!     surface; the request half landed in PR-12 (ADR-0025) as
//!     `apps/aberp/src/request_technical_annulment.rs`.
//!   - [`soap::render_manage_annulment_request`] +
//!     [`soap::ManageAnnulmentItem`] — `<ManageAnnulmentRequest>`
//!     envelope renderer. Structural mirror of
//!     `render_manage_invoice_request` per ADR-0026 §3 (three
//!     element-name renames + the literal `"ANNUL"` operation).
//!   - Five new error variants in [`error::NavTransportError`]
//!     (`ManageAnnulmentEmpty`, `ManageAnnulmentTooManyItems`,
//!     `ManageAnnulmentHttp`, `ManageAnnulmentHttpStatus`,
//!     `ManageAnnulmentResponseParse`,
//!     `ManageAnnulmentNonRetryable`, `ManageAnnulmentRetryable`).
//!
//! # PR-15 scope (this PR — ADR-0028 §3)
//!
//!   - [`operations::query_invoice_data`] — `queryInvoiceData`
//!     call + verbatim request/response bytes for the audit-
//!     evidence pair. Receiver-confirmation observation surface
//!     of the technical-annulment lifecycle; pairs with
//!     PR-14's `poll-annulment-ack`. PR-15 does NOT parse a
//!     receiver-confirmation field out of the response per
//!     ADR-0028 §"Surfaced conflict 3" — verbatim-bytes-only
//!     posture until NAV-testbed verification surfaces the
//!     actual response shape; a future amendment ADR adds the
//!     parsed `receiver_state` enum additively.
//!   - [`soap::render_query_invoice_data_request`] +
//!     [`soap::InvoiceDirection`] —
//!     `<QueryInvoiceDataRequest>` envelope renderer + the typed
//!     `OUTBOUND`/`INBOUND` enum NAV v3.0 names. Same non-
//!     `manageInvoice` request-signature shape as
//!     `queryTransactionStatus`.
//!   - Five new error variants in [`error::NavTransportError`]
//!     (`QueryInvoiceDataHttp`, `QueryInvoiceDataHttpStatus`,
//!     `QueryInvoiceDataResponseParse`,
//!     `QueryInvoiceDataNonRetryable`,
//!     `QueryInvoiceDataRetryable`).
//!
//! # What this crate still does NOT provide
//!
//!   - `queryInvoiceCheck` (Layer-2 idempotency disambiguation per
//!     ADR-0009 §5; future PR).
//!   - `queryInvoiceDigest` / `queryInvoiceChainDigest` /
//!     `queryTransactionList` (the broader NAV historical /
//!     reconciliation read-path operations; future PR per the
//!     deferred ADR named in `adr/README.md` §Deferred).
//!   - Parsed receiver-confirmation status field on
//!     `queryInvoiceData` responses (future amendment ADR after
//!     NAV-testbed verification per ADR-0028 §"Surfaced conflict
//!     3").
//!   - Audit-ledger writes — those are the binary's responsibility,
//!     called from the NAV submission paths in
//!     `apps/aberp/src/submit_invoice.rs` (PR-7-B-3),
//!     `apps/aberp/src/poll_ack.rs` (PR-7-C-2),
//!     `apps/aberp/src/submit_annulment.rs` (PR-13),
//!     `apps/aberp/src/poll_annulment_ack.rs` (PR-14), and
//!     `apps/aberp/src/observe_receiver_confirmation.rs` (PR-15).

#![forbid(unsafe_code)]

pub mod cipher;
pub mod credentials;
pub mod endpoint;
pub mod error;
pub mod operations;
pub mod signatures;
pub mod soap;
pub mod trust;

mod client;

pub use client::NavTransport;
pub use credentials::NavCredentials;
pub use endpoint::NavEndpoint;
pub use error::NavTransportError;
