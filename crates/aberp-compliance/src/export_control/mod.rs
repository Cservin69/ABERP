//! Export-control classification + denied-party screening (ITAR / EAR).
//!
//! Two distinct compliance questions live here:
//!
//! 1. **Classification** — *what is this item?* An exported part / technical
//!    drawing / software carries an EAR ECCN, a USML category (ITAR), or the
//!    catch-all EAR99. Mis-classification is a felony, so the real answer
//!    comes from a licensed classification service / commodity-jurisdiction
//!    determination — never inferred here.
//! 2. **Screening** — *who is the party?* Every consignee / end-user is
//!    screened against the consolidated denied-party lists (BIS Entity List,
//!    OFAC SDN, State DDTC debarred, …). A hit blocks the shipment.
//!
//! S345 ships the [`ExportControlProvider`] trait (the swap-point) and one
//! implementation, [`MockExportControlProvider`], which answers
//! [`ExportClassification::NotClassified`] + [`ScreeningResult::Clear`] for
//! everything. The real backends slot in behind the same trait later.

mod mock;

pub use mock::MockExportControlProvider;

use serde::{Deserialize, Serialize};

/// The export-control classification of an item.
///
/// `ECCN` / `USMLCategory` carry the determined code string; `EAR99` is the
/// EAR catch-all (commercial items subject to the EAR but not on the Commerce
/// Control List); `NotClassified` means no determination has been made yet
/// (the mock's answer); `Pending` means a determination is in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportClassification {
    /// Export Control Classification Number (EAR / Commerce Control List),
    /// e.g. `"7A994"`. The string is the determined ECCN.
    #[allow(clippy::upper_case_acronyms)]
    ECCN(String),
    /// United States Munitions List category (ITAR / USML), e.g. `"VIII(h)"`.
    #[allow(clippy::upper_case_acronyms)]
    USMLCategory(String),
    /// EAR99 — subject to the EAR but not listed on the CCL.
    EAR99,
    /// No determination has been made.
    NotClassified,
    /// A determination is in progress.
    Pending,
}

/// An item that can be submitted for export classification.
///
/// The provider keys on a short, stable descriptor (part number, commodity
/// description, material grade). The trait is intentionally minimal —
/// classification is the provider's job, not the caller's.
pub trait Classifiable {
    /// A short, stable descriptor of the item — the key a classification
    /// service would dereference (part number, commodity description, …).
    fn classification_descriptor(&self) -> String;
}

/// A party (consignee / end-user / intermediate consignee) to be screened
/// against the denied-party lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyRef {
    /// Legal name as it appears on the order.
    pub name: String,
    /// ISO 3166-1 alpha-2 country code, when known — embargo screening keys
    /// on destination country as well as name.
    pub country: Option<String>,
}

/// The outcome of screening a [`PartyRef`] against the denied-party lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreeningResult {
    /// No match — the party is clear to transact with.
    Clear,
    /// A match that restricts (but does not outright deny) the transaction —
    /// e.g. requires a license. The string names the list / reason.
    Restricted(String),
    /// A denied-party match — the transaction must not proceed. The string
    /// names the list / reason.
    Denied(String),
}

/// Failure modes a [`ExportControlProvider`] can surface.
///
/// Typed (not stringly) so the boot/audit layer can branch — a backend that
/// is unconfigured is a different posture from one that is configured but
/// unreachable.
#[derive(Debug, thiserror::Error)]
pub enum ExportControlError {
    /// The classification/screening backend is not configured.
    #[error("export-control backend not configured")]
    NotConfigured,
    /// The backend is configured but could not be reached / answered.
    #[error("export-control backend unavailable: {0}")]
    BackendUnavailable(String),
}

/// The abstraction every export-sensitive operation will consult for
/// classification + denied-party screening.
///
/// `Send + Sync` so a single `Arc<dyn ExportControlProvider>` can be shared
/// into `AppState` across every handler + daemon, the same way the S344
/// `DigitalIdProvider` is shared.
pub trait ExportControlProvider: Send + Sync {
    /// Short backend tag, e.g. `"mock"`, `"bis-api"`. Used in the boot log
    /// line and as a fast discriminator in tests.
    fn name(&self) -> &str;

    /// Determine the export classification of an item.
    fn classify(&self, item: &dyn Classifiable)
        -> Result<ExportClassification, ExportControlError>;

    /// Screen a party against the denied-party lists.
    fn screen_party(&self, party: &PartyRef) -> Result<ScreeningResult, ExportControlError>;
}
