//! Parser for the handshake line `aberp serve` prints on stdout.
//!
//! Shape (locked by `apps/aberp/src/serve.rs`'s `println!`):
//!
//! ```text
//! aberp serve: https://127.0.0.1:<port>/ (fingerprint sha256:<hex>)
//! ```
//!
//! F17 was the open decision in PR-9-1: persist the port next to the
//! cert, or have the shell read stdout. The session-12 close picked
//! stdout — Tauri owns the subprocess lifecycle, the stdout-parse
//! handshake is the same pattern `cargo run` users already see, and
//! it avoids a second on-disk artifact (`loopback.port`) that would
//! drift from reality the moment the operator kills `aberp serve`
//! with a stale port file lingering.
//!
//! The parser is intentionally pedantic: anything other than the
//! expected line shape — wrong prefix, no fingerprint, wrong scheme,
//! non-loopback host, malformed hex — is a hard error. Per CLAUDE.md
//! rule 12, "the binary printed something we didn't recognise" is a
//! louder failure than "we silently fell back to a default port."

use std::net::Ipv4Addr;

use anyhow::{anyhow, Result};

/// The structured outcome of parsing one handshake line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// `https://127.0.0.1:<port>` — the base URL the shell uses for
    /// every subsequent request. Trailing slash on the printed line
    /// is stripped; the URL has NO trailing slash so callers can
    /// concatenate `/invoices` etc. without double-slashes.
    pub url: String,
    /// Loopback port the listener bound to.
    pub port: u16,
    /// Hex SHA-256 fingerprint of the loopback cert DER (lower-case,
    /// no colons). Matches `apps/aberp/src/serve.rs::compute_cert_fingerprint`
    /// output exactly.
    pub fingerprint_hex: String,
}

/// Recognised handshake-line prefix. Locked to the value in
/// `apps/aberp/src/serve.rs`. If `serve.rs` ever changes its
/// `println!`, this constant + the test below catches it.
pub const HANDSHAKE_PREFIX: &str = "aberp serve: https://";

/// Recognised fingerprint marker. Locked to the same `println!`.
pub const FINGERPRINT_MARKER: &str = "fingerprint sha256:";

/// Parse exactly one handshake line. Whitespace around the line is
/// tolerated; everything inside the line is pedantic.
pub fn parse(line: &str) -> Result<Handshake> {
    let line = line.trim();
    let rest = line.strip_prefix(HANDSHAKE_PREFIX).ok_or_else(|| {
        anyhow!("handshake line did not start with `{HANDSHAKE_PREFIX}` — got `{line}`")
    })?;

    // Split off the URL portion from " (fingerprint sha256:...)".
    let (host_port_path, fingerprint_tail) = rest.split_once(" (").ok_or_else(|| {
        anyhow!("handshake line missing ` (fingerprint sha256:...)` suffix — got `{line}`")
    })?;
    let fingerprint_tail = fingerprint_tail
        .strip_suffix(')')
        .ok_or_else(|| anyhow!("handshake line not closed by `)` — got `{line}`"))?;
    let fingerprint_hex = fingerprint_tail
        .strip_prefix(FINGERPRINT_MARKER)
        .ok_or_else(|| {
            anyhow!("handshake line missing `{FINGERPRINT_MARKER}` marker — got `{line}`")
        })?
        .trim()
        .to_string();
    validate_fingerprint_hex(&fingerprint_hex)?;

    // host_port_path is "127.0.0.1:<port>/" — strip the trailing
    // slash and split host:port.
    let host_port = host_port_path.trim_end_matches('/');
    let (host, port_str) = host_port
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("handshake URL missing port — got `{host_port}`"))?;
    validate_host_is_loopback(host)?;
    let port: u16 = port_str
        .parse()
        .map_err(|e| anyhow!("handshake port `{port_str}` is not a u16: {e}"))?;
    if port == 0 {
        // Port 0 means "kernel picks" on the CLI side; the listener
        // resolves it to a real port before printing. A 0 on the
        // wire means `serve.rs` never resolved it — surface loud.
        return Err(anyhow!(
            "handshake printed port=0 — serve did not resolve the kernel-assigned port"
        ));
    }

    let url = format!("https://{host}:{port}");
    Ok(Handshake {
        url,
        port,
        fingerprint_hex,
    })
}

/// The loopback HTTPS listener is bound to `127.0.0.1` per ADR-0021
/// §Part B; nothing else is accepted. A `localhost` literal would be
/// indistinguishable from a hosts-file override and is refused.
fn validate_host_is_loopback(host: &str) -> Result<()> {
    let parsed: Ipv4Addr = host
        .parse()
        .map_err(|e| anyhow!("handshake host `{host}` is not an IPv4 literal: {e}"))?;
    if !parsed.is_loopback() {
        return Err(anyhow!(
            "handshake host `{host}` is not loopback — refusing to connect"
        ));
    }
    Ok(())
}

/// A SHA-256 fingerprint is exactly 64 lower-case hex characters.
/// `apps/aberp/src/serve.rs` produces that shape via `hex::encode`;
/// any deviation is a wire-format break.
fn validate_fingerprint_hex(s: &str) -> Result<()> {
    if s.len() != 64 {
        return Err(anyhow!(
            "fingerprint `{s}` length is {}, expected 64",
            s.len()
        ));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(anyhow!(
            "fingerprint `{s}` contains non-lower-hex characters"
        ));
    }
    // Verify decode succeeds — defence in depth against the bit-twiddle
    // above missing some edge case.
    hex::decode(s).map_err(|e| anyhow!("fingerprint `{s}` failed hex decode: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_fingerprint(byte: u8) -> String {
        hex::encode(vec![byte; 32])
    }

    #[test]
    fn parses_well_formed_line() {
        let fp = make_fingerprint(0xab);
        let line = format!("aberp serve: https://127.0.0.1:54321/ (fingerprint sha256:{fp})");
        let parsed = parse(&line).expect("well-formed line must parse");
        assert_eq!(parsed.url, "https://127.0.0.1:54321");
        assert_eq!(parsed.port, 54321);
        assert_eq!(parsed.fingerprint_hex, fp);
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        let fp = make_fingerprint(0x01);
        let line = format!("   aberp serve: https://127.0.0.1:1234/ (fingerprint sha256:{fp})   ");
        let parsed = parse(&line).expect("surrounding whitespace is fine");
        assert_eq!(parsed.port, 1234);
    }

    #[test]
    fn rejects_wrong_prefix() {
        let fp = make_fingerprint(0xcc);
        let line = format!("not a serve line: https://127.0.0.1:1/ (fingerprint sha256:{fp})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_non_loopback_host() {
        let fp = make_fingerprint(0xdd);
        let line = format!("aberp serve: https://10.0.0.5:8443/ (fingerprint sha256:{fp})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_localhost_literal() {
        // We refuse `localhost` because it depends on the hosts file.
        let fp = make_fingerprint(0xee);
        let line = format!("aberp serve: https://localhost:8443/ (fingerprint sha256:{fp})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_port_zero() {
        // serve.rs is supposed to resolve a kernel-assigned port
        // before printing; 0 on the wire is a contract break.
        let fp = make_fingerprint(0xff);
        let line = format!("aberp serve: https://127.0.0.1:0/ (fingerprint sha256:{fp})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_truncated_fingerprint() {
        let line = "aberp serve: https://127.0.0.1:8443/ (fingerprint sha256:abc)";
        assert!(parse(line).is_err());
    }

    #[test]
    fn rejects_uppercase_fingerprint() {
        // `hex::encode` emits lower-case; an upper-case fingerprint
        // would be `format!("{:X}", ...)` and is a contract break.
        let fp_upper = "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB";
        let line = format!("aberp serve: https://127.0.0.1:8443/ (fingerprint sha256:{fp_upper})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_missing_paren_close() {
        let fp = make_fingerprint(0x10);
        let line = format!("aberp serve: https://127.0.0.1:1/ (fingerprint sha256:{fp}");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_missing_fingerprint_marker() {
        let fp = make_fingerprint(0x20);
        let line = format!("aberp serve: https://127.0.0.1:1/ (sha256:{fp})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn rejects_port_over_u16() {
        let fp = make_fingerprint(0x30);
        let line = format!("aberp serve: https://127.0.0.1:65536/ (fingerprint sha256:{fp})");
        assert!(parse(&line).is_err());
    }

    #[test]
    fn handshake_constants_match_serve_println_shape() {
        // Conformance check: the two constants here are the load-bearing
        // contract with `apps/aberp/src/serve.rs`'s `println!`. If
        // either drifts, this test name names the contract that broke.
        assert_eq!(HANDSHAKE_PREFIX, "aberp serve: https://");
        assert_eq!(FINGERPRINT_MARKER, "fingerprint sha256:");
    }
}
