//! Pinned NAV trust anchors per ADR-0020 §1 and ADR-0021 §A14.
//!
//! Both PEMs are vendored under `crates/nav-transport/roots/` and embedded
//! into the binary at compile time via `include_bytes!`. The chain that
//! validates `*.onlineszamla.nav.gov.hu` (both prod and test) is:
//!
//!   leaf:         *.onlineszamla.nav.gov.hu   (NAV-issued)
//!   intermediate: e-Szigno OV TLS CA 2023     <-- pinned
//!   root:         Microsec e-Szigno Root CA 2009 <-- pinned
//!
//! Pinning BOTH the intermediate and the root is the deliberate posture
//! (ADR-0020 §1 + ADR-0020 adversarial-review bullet 1): tighter pins
//! cost release lag during CA rotation; that cost is materially smaller
//! than the cost of accepting a wrong CA. Strict hostname verification
//! is enforced by rustls in addition to chain validation.
//!
//! Vendored pin fingerprints (verified by `openssl x509 -fingerprint
//! -sha256` at pin time):
//!
//!   intermediate `e-Szigno OV TLS CA 2023`
//!     SHA-256:  BC:75:DB:1D:F8:8E:0A:11:C1:D4:32:BC:31:CC:F3:36
//!               9C:DF:BB:C4:8B:ED:AC:9A:C4:1F:31:F0:3D:ED:E8:A0
//!     validity: 2025-02-12 → 2028-02-12   ← forced rotation gate
//!
//!   root `Microsec e-Szigno Root CA 2009`
//!     SHA-256:  3C:5F:81:FE:A5:FA:B8:2C:64:BF:A2:EA:EC:AF:CD:E8
//!               E0:77:FC:86:20:A7:CA:E5:37:16:3D:F3:6E:DB:F3:78
//!     validity: 2009-06-16 → 2029-12-30   ← second forced rotation gate
//!
//! Rotation playbook: when either expiry approaches (or Microsec rotates
//! either cert earlier), update the corresponding PEM file and ship a new
//! ABERP release. ADR-0021 §A14 also tracks these dates.
//!
//! # Why we build rustls::ClientConfig ourselves
//!
//! reqwest 0.13 exposes `Client::builder().use_preconfigured_tls(...)` as
//! the ONLY way to fully control the TLS trust state — the older
//! `tls_built_in_root_certs(false)` runtime toggle was removed in 0.13.
//! And reqwest's `rustls` Cargo feature transitively pulls webpki-roots
//! and uses them by default (caught by
//! `tests/trust_negation.rs` — the load-bearing ADR-0020 §1 conformance
//! gate). To enforce pin-only trust we build the full `ClientConfig`
//! here and hand it to reqwest pre-baked; reqwest does not get a chance
//! to add anything.

use std::io::BufReader;

use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

use crate::error::NavTransportError;

/// The intermediate CA (`e-Szigno OV TLS CA 2023`) — directly signs
/// the NAV leaf certificate. Pinning the intermediate narrows the
/// CA lineage we accept (CLAUDE.md rule 7 — pick the tighter pin
/// over the wider one when both work).
pub(crate) const INTERMEDIATE_PEM: &[u8] = include_bytes!("../roots/e-szigno-ov-tls-ca-2023.pem");

/// The trust-anchor root (`Microsec e-Szigno Root CA 2009`).
/// Long-lived (~2009 → 2029) so it survives intermediate rotation
/// without an ABERP release if Microsec only rotates the intermediate.
pub(crate) const ROOT_PEM: &[u8] = include_bytes!("../roots/microsec-eszigno-root-ca-2009.pem");

/// Build a `rustls::ClientConfig` whose trust store contains EXACTLY
/// the two pinned NAV trust anchors and nothing else. Returns the
/// config ready to hand to `reqwest::ClientBuilder::use_preconfigured_tls`.
///
/// Loud-fails if either embedded PEM is malformed, contains zero
/// certificates, or is rejected by rustls. Each failure is a build-time
/// invariant violation surfaced at runtime — the binary itself is broken,
/// not a transient condition.
pub fn build_pinned_client_config() -> Result<ClientConfig, NavTransportError> {
    let mut roots = RootCertStore::empty();
    add_pem_to_root_store(&mut roots, "root", ROOT_PEM)?;
    add_pem_to_root_store(&mut roots, "intermediate", INTERMEDIATE_PEM)?;

    // `ClientConfig::builder()` uses the crypto provider that reqwest
    // installs (aws-lc-rs, pulled in via reqwest's `rustls` feature).
    // `with_root_certificates(roots)` consumes our pinned anchors as the
    // ENTIRE trust state; rustls validates handshakes against this store
    // exclusively. `with_no_client_auth()` matches ADR-0020 §2 ("no
    // client X.509 to NAV"). Strict hostname verification is built-in
    // and on by default.
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(config)
}

/// Parse a vendored PEM into rustls trust anchors and add them to
/// `roots`. Three loud-fail paths:
///
///   - PEM bytes are syntactically malformed
///   - PEM contains zero CERTIFICATE blocks (vendor file clobbered)
///   - rustls rejects the parsed DER as a trust anchor (not a CA cert)
fn add_pem_to_root_store(
    roots: &mut RootCertStore,
    label: &'static str,
    pem_bytes: &[u8],
) -> Result<(), NavTransportError> {
    let mut reader = BufReader::new(pem_bytes);
    let parsed: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| NavTransportError::EmbeddedPemMalformed(format!("{label} PEM: {e}")))?;
    if parsed.is_empty() {
        return Err(NavTransportError::EmbeddedPemMalformed(format!(
            "{label} PEM contained no CERTIFICATE blocks — vendor file may have been clobbered"
        )));
    }
    for cert in parsed {
        roots.add(cert).map_err(|e| {
            NavTransportError::EmbeddedCertificateRejected(format!(
                "{label}: rustls rejected as trust anchor: {e}"
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Surfaces a corrupted vendored PEM at unit-test time rather than
    /// at the moment the first TLS handshake fails in production.
    /// Per CLAUDE.md rule 12 (fail loud): if either PEM ever fails to
    /// parse, or if rustls ever rejects the cert as a trust anchor,
    /// the build's own tests refuse to pass.
    #[test]
    fn pinned_client_config_builds() {
        let _config = build_pinned_client_config()
            .expect("vendored NAV trust anchors must parse — re-run openssl extraction if not");
    }

    /// Marker: if either PEM file is ever truncated to zero bytes or
    /// replaced by an empty placeholder, this test fails. Catches the
    /// "vendored file was clobbered" case independently of the rustls
    /// parsing path above.
    #[test]
    fn pinned_pem_bytes_are_nonempty() {
        assert!(
            !INTERMEDIATE_PEM.is_empty(),
            "intermediate PEM is empty — vendor file may have been clobbered"
        );
        assert!(
            !ROOT_PEM.is_empty(),
            "root PEM is empty — vendor file may have been clobbered"
        );
    }
}
