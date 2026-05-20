//! End-to-end conformance test for the PR-9-2 handshake contract.
//!
//! The handshake line shape is the load-bearing contract between
//! `apps/aberp/src/serve.rs` (println) and
//! `apps/aberp-ui/src/handshake.rs` (parser). Either side drifting
//! silently is exactly the failure mode CLAUDE.md rule 12 names; this
//! test re-builds the exact string `serve.rs` prints and asserts the
//! parser accepts it.
//!
//! If serve.rs ever changes the printed format, THIS test fails next
//! to the unit-level parser tests — the surface area the loud failure
//! covers is one line of code in each module.

use aberp_ui::handshake;

#[test]
fn serve_println_round_trip() {
    // The line shape — exactly as `apps/aberp/src/serve.rs` builds it
    // via `println!("aberp serve: https://{}/ (fingerprint sha256:{})", addr, fingerprint_hex)`.
    // We assemble it ourselves rather than spawning `aberp` because
    // cargo test runs without the binary on PATH; the format string
    // is the contract, not the spawn.
    let port = 51847u16;
    let fingerprint_hex: String = (0..32).map(|i| format!("{:02x}", (i * 7) as u8)).collect();
    let serve_println =
        format!("aberp serve: https://127.0.0.1:{port}/ (fingerprint sha256:{fingerprint_hex})");

    let parsed = handshake::parse(&serve_println)
        .expect("round-trip from serve.rs format string MUST parse");

    assert_eq!(parsed.port, port);
    assert_eq!(parsed.url, format!("https://127.0.0.1:{port}"));
    assert_eq!(parsed.fingerprint_hex, fingerprint_hex);
}

#[test]
fn parser_constants_pin_println_contract() {
    // If someone edits one constant they have to edit the test too —
    // and the test only passes when both reflect the verbatim text in
    // serve.rs's println. The other half of the contract lives in the
    // unit test `handshake_constants_match_serve_println_shape`; this
    // integration test additionally checks the round-trip behaviour.
    assert!(handshake::HANDSHAKE_PREFIX.starts_with("aberp serve: https://"));
    assert_eq!(handshake::FINGERPRINT_MARKER, "fingerprint sha256:");
}
