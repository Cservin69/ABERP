//! S441 / ADR-0086 — the DÁP eAzonosítás OpenID4VP transport seam.
//!
//! ADR-0086 §1 quarantines the szeusz.gov.hu wire protocol behind a private
//! `DapTransport` seam so the confirmed-on-RP-registration protocol is a
//! single-impl swap. This module lands that seam as the **structural floor**:
//! the trait + value types + a [`MockDapTransport`] (used by tests, dev
//! builds, and the SPA "Sign in with DÁP" stub) and an [`OidcDapTransport`]
//! that is constructible (so it links into the Defense binary) but `todo!`
//! on every method until RP creds + spec access arrive.
//!
//! Note this is the *protocol* seam, distinct from the [`crate::provider`]
//! `DigitalIdProvider` trait (the identity-consumer surface). `DapProvider`
//! (a future session) wraps a `DapTransport` to implement `DigitalIdProvider`.

/// Inputs to start a DÁP login (ADR-0086 §2/§3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DapLoginContext {
    /// The tenant initiating the login.
    pub tenant: String,
    /// `"sandbox"` | `"production"` — selects the endpoint set (`DAP_ENV`).
    pub dap_env: String,
    /// OS-assigned loopback port the wallet redirects back to
    /// (`127.0.0.1:<port>/auth/dap/callback`, RFC 8252 §7.3).
    pub callback_port: u16,
}

/// The challenge presented to the operator: a QR / deep-link carrying the
/// OpenID4VP request object, plus the loopback callback it will redirect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DapChallenge {
    /// Opaque per-login flow id (correlates initiate ↔ complete).
    pub flow_id: String,
    /// The QR payload (an `openid4vp://…` request URI in production).
    pub qr_payload: String,
    /// Same-device deep link, when the DÁP app is local.
    pub deep_link: String,
    /// The loopback callback URL the request object carries.
    pub callback_url: String,
}

/// What the loopback listener captured from the wallet redirect — the raw
/// presentation to validate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackResponse {
    /// Echoes [`DapChallenge::flow_id`].
    pub flow_id: String,
    /// The raw `vp_token` / presentation bytes (mdoc or SD-JWT-VC — the
    /// credential format is TO-BE-CONFIRMED on RP registration).
    pub raw_presentation: Vec<u8>,
}

/// A validated DÁP identity (the PID-derived natural-person attestation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DapIdentity {
    /// The stable gov.hu citizen identifier (`operator_dap_subject`).
    pub subject: String,
    /// PID display name (surname + given name).
    pub display_name: String,
    /// RFC3339 UTC instant the presentation was validated.
    pub attested_at_utc: String,
    /// Opaque attestation token (the validated presentation / its proof).
    /// Carried so the audit anchor can bind it; never interpreted here.
    pub attestation_token_bytes: Vec<u8>,
}

/// Failures from the DÁP transport.
#[derive(Debug, thiserror::Error)]
pub enum DapError {
    #[error("DÁP flow timed out or the wallet was unreachable")]
    Unreachable,
    #[error("DÁP presentation failed validation: {0}")]
    InvalidPresentation(String),
    #[error("callback flow_id mismatch (replay or stale callback)")]
    FlowMismatch,
    #[error("DÁP transport not yet implemented: {0}")]
    NotImplemented(&'static str),
}

/// The OpenID4VP transport behind DÁP eAzonosítás. Two methods bracket one
/// login: build the request object ([`DapTransport::initiate_login`]) and
/// validate the captured presentation ([`DapTransport::complete_login`]).
pub trait DapTransport: Send + Sync {
    /// Build the request object + challenge (QR / deep-link) for a login.
    fn initiate_login(&self, ctx: &DapLoginContext) -> Result<DapChallenge, DapError>;

    /// Validate the wallet's callback presentation into a [`DapIdentity`].
    fn complete_login(&self, callback: &CallbackResponse) -> Result<DapIdentity, DapError>;
}

/// Deterministic mock transport for tests, dev builds, and the SPA stub.
///
/// `initiate_login` returns a synthetic challenge; `complete_login` returns
/// the identity it was seeded with (a "synthetic login"), ignoring the
/// callback contents beyond a flow-id sanity check. This is what the
/// "Sign in with DÁP" button calls until the real OIDC transport lands.
#[derive(Debug, Clone)]
pub struct MockDapTransport {
    identity: DapIdentity,
}

impl MockDapTransport {
    /// Seed the mock with the synthetic identity it will return on
    /// `complete_login`.
    pub fn new(identity: DapIdentity) -> Self {
        Self { identity }
    }

    /// A ready-made test operator.
    pub fn with_test_operator() -> Self {
        Self::new(DapIdentity {
            subject: "hu-mock-citizen-0001".to_string(),
            display_name: "Mock DÁP Operator".to_string(),
            attested_at_utc: "2026-06-17T00:00:00Z".to_string(),
            attestation_token_bytes: b"mock-dap-attestation".to_vec(),
        })
    }
}

impl DapTransport for MockDapTransport {
    fn initiate_login(&self, ctx: &DapLoginContext) -> Result<DapChallenge, DapError> {
        let flow_id = format!("mock-flow-{}-{}", ctx.tenant, ctx.callback_port);
        let callback_url = format!("http://127.0.0.1:{}/auth/dap/callback", ctx.callback_port);
        Ok(DapChallenge {
            flow_id: flow_id.clone(),
            qr_payload: format!("openid4vp://mock?flow={flow_id}"),
            deep_link: format!("dap://login?flow={flow_id}"),
            callback_url,
        })
    }

    fn complete_login(&self, _callback: &CallbackResponse) -> Result<DapIdentity, DapError> {
        // The mock ignores the (synthetic) callback bytes and returns its
        // seeded identity — a deterministic "synthetic login".
        Ok(self.identity.clone())
    }
}

/// The REAL DÁP OpenID4VP transport — STUB.
///
/// Constructible so it links into the Defense binary, but every method
/// panics with a `todo!` until szeusz.gov.hu RP registration completes
/// (ADR-0086 §1 "TO-BE-CONFIRMED").
///
/// ## What the real impl must fill (ADR-0086 §1–§3)
/// - `initiate_login`: build an **OpenID4VP 1.0 + DCQL** request object for
///   the PID claim set (surname, given name, birth place/date, issuing
///   authority/country, expiry, citizenship), targeting the loopback
///   `callback_url`; optionally carry `hash(session_pubkey‖tenant)` in the
///   request `nonce` (the cheap transitive binding, ADR-0086 fact #3). The
///   credential format (mdoc vs SD-JWT-VC), exact endpoint paths, scope /
///   claim identifiers, and redirect-URI rules are **gated behind KAÜ login
///   on szeusz.gov.hu** and unknown until RP registration.
/// - `complete_login`: validate the signed presentation against the DÁP
///   trust anchors, parse the PID claims into a [`DapIdentity`], and carry
///   the validated proof as `attestation_token_bytes`.
/// - The `DAP_ENV` (`sandbox`|`production`) endpoint sets are filled from
///   the spec at RP-registration time; an unknown value is a loud boot
///   error (ADR-0086 §3), handled at construction by the caller.
#[derive(Debug, Clone)]
pub struct OidcDapTransport {
    _dap_env: String,
}

impl OidcDapTransport {
    /// Construct against a `DAP_ENV` value. Does NOT contact DÁP (the
    /// `todo!` fires only on use).
    pub fn new(dap_env: impl Into<String>) -> Self {
        Self {
            _dap_env: dap_env.into(),
        }
    }
}

impl DapTransport for OidcDapTransport {
    fn initiate_login(&self, _ctx: &DapLoginContext) -> Result<DapChallenge, DapError> {
        todo!("real DAP OIDC integration — pending szeusz.gov.hu RP creds + spec access")
    }

    fn complete_login(&self, _callback: &CallbackResponse) -> Result<DapIdentity, DapError> {
        todo!("real DAP OIDC integration — pending szeusz.gov.hu RP creds + spec access")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_transport_round_trips_a_synthetic_login() {
        let transport = MockDapTransport::with_test_operator();
        let ctx = DapLoginContext {
            tenant: "prod".to_string(),
            dap_env: "production".to_string(),
            callback_port: 54321,
        };
        let challenge = transport.initiate_login(&ctx).unwrap();
        assert!(challenge.callback_url.contains("54321"));

        let identity = transport
            .complete_login(&CallbackResponse {
                flow_id: challenge.flow_id,
                raw_presentation: b"ignored-by-mock".to_vec(),
            })
            .unwrap();
        assert_eq!(identity.subject, "hu-mock-citizen-0001");
        assert!(!identity.attestation_token_bytes.is_empty());
    }

    #[test]
    #[should_panic(expected = "pending szeusz.gov.hu RP creds")]
    fn oidc_transport_initiate_is_todo() {
        let ctx = DapLoginContext {
            tenant: "prod".to_string(),
            dap_env: "production".to_string(),
            callback_port: 0,
        };
        let _ = OidcDapTransport::new("production").initiate_login(&ctx);
    }
}
