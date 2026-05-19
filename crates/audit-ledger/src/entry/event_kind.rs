//! [`EventKind`] — typed event kinds per ADR-0008 §"Entry shape".
//!
//! `kind` is the type discriminant for `payload`'s schema. Schema versioning
//! is implicit in the kind name: bumping a payload schema renames the kind,
//! and the old kind remains valid for historical entries.
//!
//! No serde derive: PR-3 stores the kind as a plain text column in DuckDB
//! via [`EventKind::as_str`]. Serde will join when a serialization path
//! (export bundle, wire protocol) actually needs it.

/// PR-3 shipped only `Test`. PR-5 adds the first two invoice-lifecycle
/// kinds from ADR-0009 §2 needed by the XML-on-disk binary. Remaining
/// invoice kinds (`InvoiceSubmitted`, `InvoiceAckPending`, ...) land
/// when their state transition first fires in the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// Test-only kind used by `tests/chain_conformance.rs`. Not allowed in
    /// production code; a future conformance check should gate this.
    Test,

    /// A sequence number was reserved in `invoice_sequence_reservation`
    /// as part of the atomic allocator (ADR-0009 §3).
    InvoiceSequenceReserved,

    /// An invoice row was inserted with state `Draft` (ADR-0009 §2).
    /// In PR-5 this fires together with `InvoiceSequenceReserved`
    /// because the binary's command path goes Draft -> Ready in one
    /// allocator call. A future PR may split them.
    InvoiceDraftCreated,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Test => "test",
            EventKind::InvoiceSequenceReserved => "invoice.sequence_reserved",
            EventKind::InvoiceDraftCreated => "invoice.draft_created",
        }
    }
}
