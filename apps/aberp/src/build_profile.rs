//! S165 / prod-prep PR #1 — the compile-time build-profile switch.
//!
//! The `production` Cargo feature is the single hülye-biztos lever that
//! flips ABERP from the NAV test environment to the real one. It is
//! COMPILE-TIME on purpose: there is no env-var override, so a binary
//! cannot be talked into prod NAV at runtime — the compiler bakes the
//! choice in at build time (`cargo build --features production`). The
//! go-live ceremony depends on this: a binary built without the feature
//! physically cannot submit to the real NAV endpoint.
//!
//! Everything that branches on prod-vs-test reads [`IS_PRODUCTION_BUILD`]
//! (or one of the helpers below) so there is exactly one source of truth:
//!
//!   - [`nav_endpoint`] — which [`NavEndpoint`] the serve/daemon paths
//!     target. The URL strings themselves live in `nav-transport`'s
//!     `NavEndpoint::base_url`; this module only selects the variant, so
//!     the literals stay single-sourced (CLAUDE.md rule 8).
//!   - [`assert_endpoint_allowed`] — defence-in-depth gate: a dev build
//!     refuses a `Production` endpoint no matter how it was handed one.
//!   - [`INVOICE_NUMBER_TEST_PREFIX`] — the `TEST-` render prefix that
//!     dev/test builds prepend to every emitted invoice number.

use aberp_nav_transport::NavEndpoint;

/// `true` iff this binary was compiled with `--features production`.
#[cfg(feature = "production")]
pub const IS_PRODUCTION_BUILD: bool = true;
/// `false` for every non-production build (the default).
#[cfg(not(feature = "production"))]
pub const IS_PRODUCTION_BUILD: bool = false;

/// ADR-0100 Phase 1 — `true` iff this binary was compiled with
/// `--features saas`. Mirrors [`IS_PRODUCTION_BUILD`] as the single
/// compile-time source of truth for cloud-vs-desktop behaviour, so
/// every later-phase seam (transport bind, storage roots, auth
/// middleware) reads ONE constant.
///
/// **Phase 1 is deliberately behaviour-neutral in every build.** The
/// `saas` feature compiles in but changes nothing yet: the desktop
/// binary and a `--features saas` binary are byte-behaviour-identical
/// on loopback. Later phases (2/4/6) fill the `saas` arms. Under the
/// default (desktop) build this is `false` and every seam resolves to
/// today's value.
#[cfg(feature = "saas")]
pub const IS_SAAS_BUILD: bool = true;
/// `false` for every non-`saas` build (the default).
#[cfg(not(feature = "saas"))]
pub const IS_SAAS_BUILD: bool = false;

/// Render prefix prepended to every emitted invoice number on dev/test
/// builds, empty on production builds. `TEST-` is NAV-`invoiceNumber`
/// charset-legal (`[0-9A-Za-z\-/]`, hyphen — NOT underscore, which the
/// validator rejects) so a prefixed number passes XSD at submit time.
/// Purely render-side: the DB counter is unchanged, so switching builds
/// never resets or skips a sequence number.
pub const INVOICE_NUMBER_TEST_PREFIX: &str = if IS_PRODUCTION_BUILD { "" } else { "TEST-" };

/// S166 / prod-prep PR #2 — the tenant identity a build is allowed to
/// run as, used by the boot sanity check (`serve::sanity_check_environment`).
///
/// Returns `Some((tenant_name, expected_tax_number))` on a PRODUCTION
/// build — the documented prod entity (Áben Consulting Kft.). A prod
/// binary that finds a seller.toml with a different `tax_number` refuses
/// to start: hülye-biztos protection so a prod build can only ever run
/// against the one documented prod identity.
///
/// Returns `None` on a dev/test build — dev tenants can have arbitrary
/// identity, so the sanity check enforces nothing there. The value is
/// NOT hardcoded at the check site; this helper is the single source of
/// truth (CLAUDE.md rule 8).
pub fn expected_tenant_identity() -> Option<(&'static str, &'static str)> {
    if IS_PRODUCTION_BUILD {
        Some(("prod", "24904362-2-41"))
    } else {
        None
    }
}

/// The NAV endpoint this build targets. Production builds hit the real
/// `api.onlineszamla.nav.gov.hu`; every other build hits the
/// `api-test.onlineszamla.nav.gov.hu` conformance host.
pub fn nav_endpoint() -> NavEndpoint {
    if IS_PRODUCTION_BUILD {
        NavEndpoint::Production
    } else {
        NavEndpoint::Test
    }
}

/// Audit-ledger label for the endpoint this build targets — the string
/// stamped into the NAV-submit audit entries. Mirrors the
/// `NavEnv::{Test,Production}` `"test"`/`"production"` labels the CLI
/// paths already use.
pub fn nav_endpoint_audit_label() -> &'static str {
    if IS_PRODUCTION_BUILD {
        "production"
    } else {
        "test"
    }
}

/// Base URL of the NAV invoiceService v3 endpoint for this build, with
/// trailing slash. Thin delegate to [`NavEndpoint::base_url`] so the URL
/// literals stay owned by `nav-transport`.
pub fn nav_endpoint_base_url() -> &'static str {
    nav_endpoint().base_url()
}

/// Defence-in-depth prod-endpoint gate (deliverable #2).
///
/// A production build has the gate LIFTED — prod NAV calls succeed. A
/// non-production build REFUSES any `Production` endpoint, no matter how
/// it was injected: if a dev binary somehow gets handed the prod
/// endpoint, it still loud-fails rather than touching real NAV. Test
/// endpoints are always allowed.
pub fn assert_endpoint_allowed(endpoint: NavEndpoint) -> anyhow::Result<()> {
    if !IS_PRODUCTION_BUILD && endpoint == NavEndpoint::Production {
        anyhow::bail!(
            "this is a DEV build but a PRODUCTION NAV endpoint ({}) was selected — refusing to \
             submit to real NAV. Rebuild with `--features production` to target prod.",
            endpoint.hostname()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The feature is compile-time, so each build flavour can only pin
    // its own arm. `cargo test --workspace` (feature off) runs the dev
    // arm; `cargo test --features production` runs the prod arm.

    #[cfg(not(feature = "production"))]
    #[test]
    #[allow(clippy::assertions_on_constants)] // pinning the compile-time gate is the test's purpose.
    fn dev_build_targets_test_endpoint_and_prefixes() {
        assert!(!IS_PRODUCTION_BUILD);
        assert_eq!(nav_endpoint(), NavEndpoint::Test);
        assert_eq!(
            nav_endpoint_base_url(),
            "https://api-test.onlineszamla.nav.gov.hu/invoiceService/v3/"
        );
        assert_eq!(nav_endpoint_audit_label(), "test");
        assert_eq!(INVOICE_NUMBER_TEST_PREFIX, "TEST-");
    }

    #[cfg(feature = "production")]
    #[test]
    #[allow(clippy::assertions_on_constants)] // pinning the compile-time gate is the test's purpose.
    fn production_build_targets_prod_endpoint_and_no_prefix() {
        assert!(IS_PRODUCTION_BUILD);
        assert_eq!(nav_endpoint(), NavEndpoint::Production);
        assert_eq!(
            nav_endpoint_base_url(),
            "https://api.onlineszamla.nav.gov.hu/invoiceService/v3/"
        );
        assert_eq!(nav_endpoint_audit_label(), "production");
        assert_eq!(INVOICE_NUMBER_TEST_PREFIX, "");
    }

    #[cfg(not(feature = "production"))]
    #[test]
    fn dev_build_refuses_production_endpoint_but_allows_test() {
        // The gate STAYS on a dev build: Production is refused…
        assert!(assert_endpoint_allowed(NavEndpoint::Production).is_err());
        // …while Test is always fine.
        assert!(assert_endpoint_allowed(NavEndpoint::Test).is_ok());
    }

    #[cfg(feature = "production")]
    #[test]
    fn production_build_allows_both_endpoints() {
        // The gate is LIFTED on a production build.
        assert!(assert_endpoint_allowed(NavEndpoint::Production).is_ok());
        assert!(assert_endpoint_allowed(NavEndpoint::Test).is_ok());
    }

    // ADR-0100 Phase 1 — the default (desktop) build MUST have `saas`
    // off. `cargo test --workspace` runs this arm; a `--features saas`
    // build would compile the const to `true` and this test would not
    // pin it (the `saas`-on arm below does). Together they guarantee the
    // desktop-identical invariant at the single source of truth.
    #[cfg(not(feature = "saas"))]
    #[test]
    #[allow(clippy::assertions_on_constants)] // pinning the compile-time gate is the test's purpose.
    fn default_build_is_not_saas() {
        assert!(!IS_SAAS_BUILD);
    }

    #[cfg(feature = "saas")]
    #[test]
    #[allow(clippy::assertions_on_constants)] // pinning the compile-time gate is the test's purpose.
    fn saas_build_is_saas() {
        assert!(IS_SAAS_BUILD);
    }
}
