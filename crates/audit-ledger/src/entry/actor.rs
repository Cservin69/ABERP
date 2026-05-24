//! [`Actor`] — who produced this entry, per ADR-0008 §"Entry shape":
//! "session ID + user ID + capability set used".
//!
//! PR-7-A (closes fortnightly review F15) introduced
//! [`Actor::from_local_cli`] as the real-identity constructor used by
//! the binary's command paths, and gated [`Actor::test_only`] behind
//! `cfg(test)` / `feature = "test-support"` so production code can no
//! longer reach it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Captures who produced an audit-ledger entry. Constructed via:
///
///   - [`Actor::from_local_cli`] — operator running the ABERP CLI on
///     their own workstation; identity derived from the NAV technical-
///     user login the operator authenticated against the OS keychain
///     to use. Production path PR-7-A introduces.
///   - [`Actor::test_only`] — test-only fixture, available only when
///     `cfg(test)` is on (the crate's own tests) or the `test-support`
///     feature is enabled (consumer integration tests). Not reachable
///     from a release build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub session_id: String,
    pub user_id: String,
    pub capabilities: BTreeSet<String>,
}

impl Actor {
    /// Construct an actor for an entry produced by the local ABERP CLI.
    ///
    /// `session_id` is a per-process identifier (the binary mints a
    /// fresh ULID at startup) so each CLI invocation is distinguishable
    /// in the ledger. `login` is the NAV technical-user identifier the
    /// operator authenticated against (loaded from the OS keychain by
    /// `aberp-nav-transport`). Capabilities are the constant set of
    /// operations a local-CLI session is allowed to perform pre-NAV-
    /// submission; PR-7-B will expand this when remote-submission
    /// capabilities become reachable from the binary.
    pub fn from_local_cli(session_id: String, login: &str) -> Self {
        Self {
            session_id,
            user_id: login.to_string(),
            capabilities: [
                "audit.append".to_string(),
                "billing.issue_invoice".to_string(),
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Fixed test actor for the chain-conformance test (this crate's
    /// `tests/`) and the rollback-conformance integration test (in
    /// `apps/aberp/tests/`). **Not for use outside tests** — the
    /// `#[cfg]` gate makes this unreachable from production code paths
    /// (closes fortnightly review F15). See [`Actor::from_local_cli`]
    /// for the production constructor.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_only() -> Self {
        Self {
            session_id: "test-session".to_string(),
            user_id: "test-user".to_string(),
            capabilities: ["audit.append".to_string()].into_iter().collect(),
        }
    }

    /// Serialize to a stable string for DuckDB storage. The canonical CBOR
    /// encoder ([`crate::canonical`]) does not consult this — it walks the
    /// fields directly — so this is purely a storage convenience.
    pub(crate) fn to_storage_json(&self) -> String {
        serde_json::to_string(self).expect("Actor is always JSON-serializable")
    }

    pub(crate) fn from_storage_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}
