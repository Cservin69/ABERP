//! ADR-0100 Phase 1 — the storage-path root resolver seam.
//!
//! Every ABERP on-disk artifact lives under `$HOME/.aberp/` today: the
//! per-tenant DB, the audit mirror, `serve/<tenant>/` (cert + issued
//! NAV XML), `<tenant>/ap-artifacts/`, `seller.toml`, and the snapshot
//! store. ADR-0100 Phase 4 relocates those roots onto a cloud volume
//! mount; to keep that a one-line change instead of a scattered edit,
//! Phase 1 introduces THIS single resolver and routes the roots through
//! it.
//!
//! **Phase 1 is behaviour-neutral.** [`aberp_data_root`] resolves to the
//! exact `$HOME/.aberp` path it always did, in every build. When ADR-0100
//! Phase 4 lands, the `saas` arm (gated on
//! [`crate::build_profile::IS_SAAS_BUILD`]) points at the mounted volume;
//! the desktop build keeps returning `$HOME/.aberp`. No behaviour changes
//! here and now.
//!
//! Phase 1 wires ONE `serve.rs` root — `serve_artifacts_dir` (via
//! [`serve_root`]) — through this module as the anchor consumer; the
//! remaining scattered roots (`ap_artifacts_dir` — which still hand-builds
//! `~/.aberp/<tenant>/ap-artifacts` off [`home_dir`] — `email_relay_queue`,
//! `first_launch`, `incoming_invoices`, `issue_invoice`, …) are routed in
//! Phase 4 — surgical Phase-1 scope keeps them untouched (CLAUDE.md rule 3).

use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// The operator's home directory. `HOME` covers macOS + Linux;
/// `USERPROFILE` covers Windows. Loud-fail if neither is set (CLAUDE.md
/// rule 12). This is the single home-detection point — `serve.rs`
/// delegates here rather than re-implementing it.
pub fn home_dir() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    Err(anyhow!(
        "neither HOME nor USERPROFILE is set — cannot locate ~/.aberp/"
    ))
}

/// The ABERP data root — `$HOME/.aberp` today, in every build.
///
/// This is the storage-path seam. ADR-0100 Phase 4 makes this consult
/// [`crate::build_profile::IS_SAAS_BUILD`] and return the mounted cloud
/// volume under the `saas` build; the desktop build keeps returning
/// `$HOME/.aberp`. Phase 1 returns `$HOME/.aberp` unconditionally so the
/// desktop behaviour is byte-identical.
pub fn aberp_data_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".aberp"))
}

/// The per-tenant `serve/<tenant>/` root — cert PEM + key PEM +
/// fingerprint + issued NAV XML. Resolves through [`aberp_data_root`].
pub fn serve_root(tenant: &str) -> Result<PathBuf> {
    Ok(aberp_data_root()?.join("serve").join(tenant))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data root resolves to `$HOME/.aberp` — the desktop-identical
    /// value. Guards the Phase-1 invariant that the seam changes no path.
    #[test]
    fn data_root_is_home_dot_aberp() {
        let home = home_dir().expect("HOME set in test env");
        assert_eq!(aberp_data_root().unwrap(), home.join(".aberp"));
        assert_eq!(
            serve_root("prod").unwrap(),
            home.join(".aberp").join("serve").join("prod")
        );
    }
}
