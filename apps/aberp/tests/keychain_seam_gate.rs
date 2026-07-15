//! ADR-0100 Phase 1 — the keychain-seam ENFORCE test.
//!
//! Phase 1 routed every keychain access through the
//! `aberp-secret-store::SecretStore` seam (ADR-0100 §5). The invariant "no
//! direct keychain access outside the seam" held at the cut, but a claim that a
//! grep-gate *enforced* it was made before any gate existed (the adversarial
//! review's finding #1). This test IS that enforcement inside
//! `cargo test --workspace`: it runs the committed cut-gate
//! (`tools/cut_gate_keychain_seam.sh`) and fails if any direct keychain access
//! has drifted back in outside the seam crate + its designated test mocks.
//!
//! The gate's *teeth* (that it goes red on a planted bypass and only on a real
//! one) are proved separately by `tools/cut_gate_keychain_seam_probes.sh`, wired
//! into `.github/workflows/cut-gate.yml` alongside the ADR-0099 census probes.

use std::path::PathBuf;
use std::process::Command;

/// Repo root = `apps/aberp/../..`. Robust to the test binary's CWD.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("canonicalize repo root")
}

#[test]
fn keychain_access_stays_behind_the_secret_store_seam() {
    let root = repo_root();
    let gate = root.join("tools/cut_gate_keychain_seam.sh");
    assert!(
        gate.exists(),
        "keychain-seam cut-gate missing at {}",
        gate.display()
    );

    let output = Command::new("bash")
        .arg(&gate)
        .current_dir(&root)
        .output()
        .expect("run tools/cut_gate_keychain_seam.sh");

    if !output.status.success() {
        panic!(
            "keychain-seam gate FAILED — a direct keychain access exists outside \
             the aberp-secret-store seam. Route it through \
             aberp_secret_store::SecretStore.\n\n--- gate stdout ---\n{}\n--- gate stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
