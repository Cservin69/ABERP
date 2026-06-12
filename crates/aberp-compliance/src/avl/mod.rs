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

impl DpasRating {
    /// Render in the on-disk / audit-payload form — the canonical string the
    /// S361 `supplier.dpas_priority_set` firing site writes and the partners
    /// `dpas_rating` column stores. Paired with [`DpasRating::from_storage_str`]
    /// as a round-trip-proven pair, mirroring the export-control
    /// [`crate::export_control::Jurisdiction`] discipline (S359). A free-text
    /// rating can never reach the ledger or the column — it must round-trip
    /// through this typed pair first.
    pub fn as_str(&self) -> &'static str {
        match self {
            DpasRating::None => "NONE",
            DpasRating::DoC1 => "DO-C1",
            DpasRating::DxC1 => "DX-C1",
        }
    }

    /// Parse the on-disk / audit-payload form back into a `DpasRating`. Errors
    /// on unknown strings — silent fallback would mask schema drift (CLAUDE.md
    /// rule 12, "fail loud"); a mis-parse of an unrecognised rating to
    /// [`DpasRating::None`] would silently strip a defense order's priority.
    pub fn from_storage_str(s: &str) -> Result<Self, &'static str> {
        match s {
            "NONE" => Ok(DpasRating::None),
            "DO-C1" => Ok(DpasRating::DoC1),
            "DX-C1" => Ok(DpasRating::DxC1),
            _ => Err("unknown DpasRating storage string"),
        }
    }
}

/// The outcome of screening a supplier against the export-control denied-party
/// lists — the stored status on the AVL entry (S361, ADR-0078).
///
/// Distinct from [`crate::export_control::ScreeningResult`]: that is the typed
/// *adjudication* of a single screening call (clear / restricted-with-reason /
/// denied-with-reason); this is the *stored screening-outcome status* on the
/// AVL entry, which can be `NotScreened` before the first screen runs.
///
/// S361 reshapes the S345 scaffold vocabulary onto the denial-list-screening
/// outcome the BIS Consolidated Screening List / OFAC / State DDTC actually
/// return — `Clear` (no match), `Hit` (a denied-party match), `Inconclusive`
/// (a partial / common-name match needing manual review). The placeholder
/// `Restricted` / `Denied` variants the scaffold guessed are dropped: the
/// restricted-vs-denied *adjudication* is the job of
/// [`crate::export_control::ScreeningResult`], not the stored AVL status. These
/// are the exact tokens the `supplier.export_screened` payload `screening_result`
/// field and the partners `export_screening_status` column carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExportScreeningStatus {
    /// No screening has been performed yet.
    #[default]
    NotScreened,
    /// Screened clear — no denied-party match.
    Clear,
    /// Screened to a denied-party match (e.g. BIS Entity List / OFAC SDN) —
    /// must not transact until adjudicated.
    Hit,
    /// Screened with an inconclusive result — a partial / common-name match
    /// that needs manual review before the supplier may transact.
    Inconclusive,
}

impl ExportScreeningStatus {
    /// Render in the on-disk / audit-payload form — the canonical string the
    /// S361 `supplier.export_screened` firing site writes and the partners
    /// `export_screening_status` column stores. Round-trip-proven with
    /// [`ExportScreeningStatus::from_storage_str`].
    pub fn as_str(&self) -> &'static str {
        match self {
            ExportScreeningStatus::NotScreened => "not_screened",
            ExportScreeningStatus::Clear => "clear",
            ExportScreeningStatus::Hit => "hit",
            ExportScreeningStatus::Inconclusive => "inconclusive",
        }
    }

    /// Parse the on-disk / audit-payload form back into an
    /// `ExportScreeningStatus`. Errors on unknown strings — a silent fallback
    /// to `Clear` would be the worst-class export-control bug (it would mark an
    /// unscreened / hit supplier as clear to transact). Fail loud (CLAUDE.md
    /// rule 12).
    pub fn from_storage_str(s: &str) -> Result<Self, &'static str> {
        match s {
            "not_screened" => Ok(ExportScreeningStatus::NotScreened),
            "clear" => Ok(ExportScreeningStatus::Clear),
            "hit" => Ok(ExportScreeningStatus::Hit),
            "inconclusive" => Ok(ExportScreeningStatus::Inconclusive),
            _ => Err("unknown ExportScreeningStatus storage string"),
        }
    }
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
