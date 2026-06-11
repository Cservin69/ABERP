//! Approved Vendor List (AVL) + DPAS priority rating types.
//!
//! Aerospace / defense procurement constrains *who* you may buy from: a
//! supplier must be qualified (AS9100D §8.4 supplier control), and a defense
//! order may carry a DPAS priority rating (FAR 11.6 / DPAS regulation 15 CFR
//! 700) that the supplier must acknowledge and prioritize. ABERP's commercial
//! core models a supplier as a 3-value `PartnerKind` flag — this module
//! introduces the qualification + rating + screening-status fields the AVL
//! (S347) attaches to a partner.
//!
//! S345 ships the enums + the [`ApprovedSupplierEntry`] record only.

use serde::{Deserialize, Serialize};

/// Reference to a partner in ABERP's partner master data.
///
/// A local newtype rather than a dependency on `apps/aberp` — this crate is
/// the leaf, the wiring layer (S347) maps it to the real partner id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartnerRef(pub String);

/// DPAS priority rating carried by a defense order (FAR 11.604 / 15 CFR 700).
///
/// `None` = unrated commercial order; `DoC1` / `DxC1` are the two rating
/// authorities (DO and DX) at the C9 program-identification level rendered
/// here as the rating prefix. DX outranks DO; both outrank unrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DpasRating {
    /// Unrated — ordinary commercial priority.
    #[default]
    None,
    /// DO-rated order (the lower of the two defense priorities).
    DoC1,
    /// DX-rated order (the higher defense priority; takes precedence over DO).
    DxC1,
}

/// Where a supplier stands against the export / denied-party screen.
///
/// Distinct from [`crate::export_control::ScreeningResult`]: that is the
/// *outcome of a screening call*, this is the *stored status* on the AVL
/// entry, which can be `NotScreened` before the first screen runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExportScreeningStatus {
    /// No screening has been performed yet.
    #[default]
    NotScreened,
    /// Screened clear — no denied-party match.
    Clear,
    /// Screened with a restriction (license required, partial match).
    Restricted,
    /// Screened to a denied-party match — must not transact.
    Denied,
}

/// The qualification level of a supplier on the AVL (AS9100D §8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualLevel {
    /// May be invited to bid, but not yet cleared to deliver.
    Bid,
    /// Fully qualified — may bid and deliver.
    Approved,
    /// Disapproved — do not use.
    Disapproved,
}

impl QualLevel {
    /// `true` if the supplier may be invited to bid. Both `Bid` and
    /// `Approved` may bid; `Disapproved` may not.
    pub fn can_bid(self) -> bool {
        matches!(self, QualLevel::Bid | QualLevel::Approved)
    }

    /// `true` only if the supplier is cleared to deliver. ONLY `Approved`
    /// suppliers may deliver.
    pub fn can_deliver(self) -> bool {
        matches!(self, QualLevel::Approved)
    }
}

/// An entry on the Approved Vendor List.
///
/// The qualification + DPAS + screening fields are the compliance overlay on
/// top of the commercial partner record; `last_audit_at_ms` is the supplier's
/// most recent qualification audit (AS9100D §8.4 re-evaluation cadence),
/// `None` until the first audit is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedSupplierEntry {
    /// The partner this AVL entry qualifies.
    pub partner_id: PartnerRef,
    /// Current qualification level.
    pub qualification_level: QualLevel,
    /// DPAS rating the supplier is approved to service.
    pub dpas: DpasRating,
    /// Stored export-screening status.
    pub screening: ExportScreeningStatus,
    /// Unix-epoch milliseconds of the last qualification audit, if any.
    pub last_audit_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests;
