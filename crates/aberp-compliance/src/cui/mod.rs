//! Controlled Unclassified Information (CUI) marking + classification levels.
//!
//! A [`CuiMarking`] tags a record / document / drawing with its sensitivity
//! so downstream handling (storage, transmission, marking on PDFs, access
//! control) can enforce the right safeguards. The vocabulary spans the
//! unclassified-but-controlled band (CUI, with a category from the DoD CUI
//! Registry) and the national-security classification levels.
//!
//! S345 ships the enums + marking helpers only. Wiring a marking onto a
//! record and enforcing handling rules lands later.

use serde::{Deserialize, Serialize};

/// The sensitivity marking of a record.
///
/// Ordered least → most sensitive. `Cui` carries the specific category from
/// the DoD CUI Registry; the three classification variants are the national
/// security levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CuiMarking {
    /// No control required.
    Unclassified,
    /// Controlled Unclassified Information, with its registry category.
    Cui(CuiCategory),
    /// Classified — Confidential.
    Confidential,
    /// Classified — Secret.
    Secret,
    /// Classified — Top Secret.
    TopSecret,
}

/// A CUI category from the DoD CUI Registry (the most common organizational
/// index groupings). The banner marking renders as `CUI//<abbrev>`.
///
/// This is a deliberate starter subset, not the full registry — S346+ extend
/// it as real flowdowns demand specific categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CuiCategory {
    /// Controlled Technical Information.
    Cti,
    /// Privacy.
    Prvcy,
    /// Export Control.
    Expt,
    /// Critical Infrastructure.
    Crit,
    /// Law Enforcement.
    Lei,
    /// Intelligence.
    Ifg,
    /// Information Systems Vulnerability Information (general infrastructure).
    Inf,
    /// Information Systems Vulnerability Information.
    Isvi,
    /// Procurement and Acquisition.
    Proc,
    /// Proprietary Business Information.
    Prop,
}

impl CuiCategory {
    /// The banner abbreviation as it appears after the `CUI//` control marking,
    /// e.g. `CUI//SP-EXPT`-style index groupings collapse here to the registry
    /// abbreviation (`CTI`, `PRVCY`, `EXPT`, …).
    pub fn abbreviation(self) -> &'static str {
        match self {
            CuiCategory::Cti => "CTI",
            CuiCategory::Prvcy => "PRVCY",
            CuiCategory::Expt => "EXPT",
            CuiCategory::Crit => "CRIT",
            CuiCategory::Lei => "LEI",
            CuiCategory::Ifg => "IFG",
            CuiCategory::Inf => "INF",
            CuiCategory::Isvi => "ISVI",
            CuiCategory::Proc => "PROC",
            CuiCategory::Prop => "PROP",
        }
    }
}

impl CuiMarking {
    /// `true` only for the [`CuiMarking::Cui`] band — controlled but
    /// unclassified.
    pub fn is_cui(&self) -> bool {
        matches!(self, CuiMarking::Cui(_))
    }

    /// `true` for the national-security classification levels (Confidential
    /// and above). `Unclassified` and `Cui` are NOT classified.
    pub fn is_classified(&self) -> bool {
        matches!(
            self,
            CuiMarking::Confidential | CuiMarking::Secret | CuiMarking::TopSecret
        )
    }

    /// The banner marking string per DoD marking conventions:
    /// `UNCLASSIFIED`, `CUI//<ABBREV>`, `CONFIDENTIAL`, `SECRET`,
    /// `TOP SECRET`.
    pub fn display_marking(&self) -> String {
        match self {
            CuiMarking::Unclassified => "UNCLASSIFIED".to_string(),
            CuiMarking::Cui(cat) => format!("CUI//{}", cat.abbreviation()),
            CuiMarking::Confidential => "CONFIDENTIAL".to_string(),
            CuiMarking::Secret => "SECRET".to_string(),
            CuiMarking::TopSecret => "TOP SECRET".to_string(),
        }
    }
}

#[cfg(test)]
mod tests;
