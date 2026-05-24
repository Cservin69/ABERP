//! [`NavEndpoint`] — the two NAV Online Számla v3.0 endpoints per
//! ADR-0009 §1 and ADR-0020 §1.
//!
//! Production and test share the same TLS trust chain (verified at PR-7-A
//! pin time by `openssl s_client` against both endpoints — identical chain,
//! one pin set covers both). They differ only in the base URL the
//! reqwest::Client targets.
//!
//! No `Default` impl: choosing prod vs test silently is exactly the failure
//! mode CLAUDE.md rule 12 names. Every constructor of `NavTransport`
//! takes a `NavEndpoint` explicitly.

/// Which NAV environment the transport targets.
///
/// PR-7-A wires both URLs but only `Test` is exercised by the env-gated
/// integration test against `api-test.onlineszamla.nav.gov.hu`. Production
/// submission lands in PR-7-B; until then a `NavEndpoint::Production`
/// transport is constructable but not called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavEndpoint {
    /// `https://api-test.onlineszamla.nav.gov.hu/invoiceService/v3/`.
    /// Used by conformance against a NAV-provisioned test taxpayer
    /// per ADR-0009 §9. No real fiscal effect.
    Test,

    /// `https://api.onlineszamla.nav.gov.hu/invoiceService/v3/`.
    /// Used only after a tenant has passed conformance per ADR-0009 §9.
    /// PR-7-A constructs but does not call this endpoint.
    Production,
}

impl NavEndpoint {
    /// Base URL for the NAV invoiceService v3 endpoint, with trailing slash.
    /// The trailing slash matters when concatenating operation paths
    /// (`tokenExchange`, `manageInvoice`, etc.); kept here so call sites
    /// don't reinvent the joining rule.
    pub fn base_url(self) -> &'static str {
        match self {
            NavEndpoint::Test => "https://api-test.onlineszamla.nav.gov.hu/invoiceService/v3/",
            NavEndpoint::Production => "https://api.onlineszamla.nav.gov.hu/invoiceService/v3/",
        }
    }

    /// The hostname rustls/reqwest will enforce via SNI + hostname
    /// verification. Returned for log lines and tests that assert
    /// "we connected to the right host"; not used by the production
    /// client path (reqwest derives SNI from the URL).
    pub fn hostname(self) -> &'static str {
        match self {
            NavEndpoint::Test => "api-test.onlineszamla.nav.gov.hu",
            NavEndpoint::Production => "api.onlineszamla.nav.gov.hu",
        }
    }
}
