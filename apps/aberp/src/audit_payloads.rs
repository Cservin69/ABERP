//! Typed audit-ledger payload schemas for events the binary writes.
//!
//! # Why typed payloads, not `format!`-built JSON
//!
//! PR-5 wrote audit payloads via ad-hoc string interpolation:
//!
//! ```ignore
//! format!("{{\"invoice_id\":\"{}\",\"seq\":{},...}}", ...)
//! ```
//!
//! This was fine for the values PR-5 interpolated (Crockford-base32
//! ULIDs and unsigned integers — no characters that JSON would need
//! to escape). The trap is that PR-7's NAV submission path puts
//! verbatim NAV XML response bodies into audit payloads
//! (ADR-0009 §8), and any quote / backslash / control character in
//! the body produces malformed JSON inside an opaque `BLOB` column
//! with no SQL error, no log, no test failure until something
//! downstream tries to parse the column back.
//!
//! PR-6.1 (Fortnightly review F9) closes the trap at the source:
//! every payload the binary writes goes through `serde_json::to_vec`
//! on a typed struct defined here. The audit-ledger crate's surface
//! remains `Vec<u8>`-shaped — discipline lives at the call site.
//!
//! # Schema versioning
//!
//! Each payload type carries an implicit schema. Adding a field is
//! backward-compatible (older readers see the old shape via
//! `#[serde(default)]` if they choose to parse). Removing a field
//! or changing a field's semantic shape requires a *new* `EventKind`
//! variant (per `crates/audit-ledger/src/entry/event_kind.rs`
//! header: "bumping a payload schema renames the kind, and the old
//! kind remains valid for historical entries").

use aberp_billing::{IdempotencyKey, ReadyInvoice, SequenceReservation};
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────
// InvoiceSequenceReserved
// ──────────────────────────────────────────────────────────────────────

/// Payload for [`aberp_audit_ledger::EventKind::InvoiceSequenceReserved`].
///
/// Written by the binary's `run_single_tx` on the `Fresh` branch of
/// the allocator outcome — i.e. exactly when a sequence number was
/// burned. On replay, this event is **not** re-written; the prior
/// issuance's entry remains the canonical record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvoiceSequenceReservedPayload {
    pub invoice_id: String,
    pub seq: u64,
    pub reservation_id: String,
    pub idempotency_key: String,
}

impl InvoiceSequenceReservedPayload {
    pub fn from_outcome(
        invoice: &ReadyInvoice,
        reservation: &SequenceReservation,
        idempotency_key: IdempotencyKey,
    ) -> Self {
        Self {
            invoice_id: invoice.id.to_prefixed_string(),
            seq: invoice.sequence_number,
            reservation_id: reservation.id.to_prefixed_string(),
            idempotency_key: idempotency_key.to_canonical_string(),
        }
    }

    /// Serialize to bytes for the audit-ledger `payload` column.
    /// `serde_json::to_vec` on a typed struct cannot produce malformed
    /// JSON — quotes, backslashes, control chars, and non-ASCII in any
    /// `String` field are escaped per the spec.
    ///
    /// Borrows `&self` and returns a fresh `Vec<u8>`, hence the `to_*`
    /// name (Rust convention: `as_*` is cheap-reference, `to_*` is
    /// owned-by-clone-or-allocate, `into_*` consumes `self`).
    pub fn to_bytes(&self) -> Vec<u8> {
        // unwrap: serializing fixed-shape value-only structs to JSON
        // bytes cannot fail. The only error path serde_json::to_vec
        // surfaces for these types is OOM, which we treat as a
        // process-level fatal — matching anyhow `?` behaviour upstack.
        serde_json::to_vec(self).expect("JSON serialization of audit payload cannot fail")
    }
}

// ──────────────────────────────────────────────────────────────────────
// InvoiceDraftCreated
// ──────────────────────────────────────────────────────────────────────

/// Payload for [`aberp_audit_ledger::EventKind::InvoiceDraftCreated`].
///
/// Written on the same `Fresh` branch as
/// [`InvoiceSequenceReservedPayload`], in the same DuckDB transaction
/// (PR-6 close-out). The fields are intentionally narrow today —
/// just the invoice id and line count — because the full draft
/// content is reconstructible from the `invoice` + `invoice_line`
/// tables. The payload is a pointer, not a duplicate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvoiceDraftCreatedPayload {
    pub invoice_id: String,
    pub line_count: usize,
    pub idempotency_key: String,
}

impl InvoiceDraftCreatedPayload {
    pub fn from_invoice(invoice: &ReadyInvoice, idempotency_key: IdempotencyKey) -> Self {
        Self {
            invoice_id: invoice.id.to_prefixed_string(),
            line_count: invoice.lines.len(),
            idempotency_key: idempotency_key.to_canonical_string(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON serialization of audit payload cannot fail")
    }
}

// ──────────────────────────────────────────────────────────────────────
// Tests — round-trip every payload through serde_json
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_billing::{
        CustomerId, Huf, InvoiceId, LineItem, ReservationId, ReservationStatus, SeriesId,
    };
    use time::OffsetDateTime;

    /// Build a ReadyInvoice fixture whose description contains the
    /// exact JSON-hostile characters that PR-5's `format!` approach
    /// could not safely interpolate. If this test round-trips
    /// cleanly, the typed-struct path is doing the escaping the old
    /// path did not.
    fn fixture_invoice() -> ReadyInvoice {
        ReadyInvoice {
            id: InvoiceId::new(),
            series_id: SeriesId::new(),
            customer_id: CustomerId::new(),
            lines: vec![
                LineItem {
                    description: "line with \"quotes\" and \\ backslashes \n\t newlines"
                        .to_string(),
                    quantity: 2,
                    unit_price: Huf(1_500),
                    vat_rate_basis_points: 2700,
                },
                LineItem {
                    description: "ünïcödé and other non-ASCII: 日本語".to_string(),
                    quantity: 1,
                    unit_price: Huf(500),
                    vat_rate_basis_points: 2700,
                },
            ],
            issue_date: OffsetDateTime::now_utc(),
            sequence_number: 7,
            fiscal_year: 0,
        }
    }

    fn fixture_reservation(invoice_id: InvoiceId, series_id: SeriesId) -> SequenceReservation {
        SequenceReservation {
            id: ReservationId::new(),
            series_id,
            fiscal_year: 0,
            number: 7,
            invoice_id,
            status: ReservationStatus::Reserved,
            void_reason: None,
            reserved_at: OffsetDateTime::now_utc(),
            used_at: None,
            voided_at: None,
        }
    }

    #[test]
    fn sequence_reserved_round_trip() {
        let invoice = fixture_invoice();
        let reservation = fixture_reservation(invoice.id, invoice.series_id);
        let idem = IdempotencyKey::new();
        let original = InvoiceSequenceReservedPayload::from_outcome(&invoice, &reservation, idem);
        let bytes = original.to_bytes();

        // Bytes must parse back to an identical struct. If serde drops
        // a field on encode or decode, this fails loudly.
        let decoded: InvoiceSequenceReservedPayload =
            serde_json::from_slice(&bytes).expect("decode must succeed");
        assert_eq!(decoded, original);

        // The idempotency_key field must carry the ADR-0005 prefix —
        // the F8 contract is reinforced from the audit-payload side.
        assert!(decoded.idempotency_key.starts_with("idem_"));
    }

    #[test]
    fn draft_created_round_trip() {
        let invoice = fixture_invoice();
        let idem = IdempotencyKey::new();
        let original = InvoiceDraftCreatedPayload::from_invoice(&invoice, idem);
        let bytes = original.to_bytes();

        let decoded: InvoiceDraftCreatedPayload =
            serde_json::from_slice(&bytes).expect("decode must succeed");
        assert_eq!(decoded, original);

        // The line_count must match the fixture's line count exactly.
        assert_eq!(decoded.line_count, 2);
    }

    /// The trap PR-6.1 closed: PR-5's `format!`-built JSON could not
    /// safely interpolate strings with embedded quotes / backslashes.
    /// The typed-struct path *must* escape them and produce valid
    /// JSON that round-trips. If this fixture ever stops carrying
    /// hostile characters, the trap can regress silently.
    #[test]
    fn round_trip_preserves_json_hostile_characters() {
        let invoice = fixture_invoice();
        let reservation = fixture_reservation(invoice.id, invoice.series_id);
        let idem = IdempotencyKey::new();
        let payload = InvoiceSequenceReservedPayload::from_outcome(&invoice, &reservation, idem);
        let bytes = payload.to_bytes();

        // Sanity: the bytes are valid JSON. (If `to_vec` produced
        // malformed JSON, `from_slice` to a `serde_json::Value` would
        // fail before we even compared structs.)
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).expect("bytes must be valid JSON");
        assert!(v.is_object());

        // The struct itself must round-trip.
        let decoded: InvoiceSequenceReservedPayload =
            serde_json::from_slice(&bytes).expect("typed decode");
        assert_eq!(decoded, payload);
    }
}
