//! NAV application-level authentication hashes per ADR-0009 §4 + ADR-0020 §2.
//!
//! Two artifacts go into every NAV SOAP envelope's `<user>` block:
//!
//!   1. `passwordHash` — SHA-512 of the technical-user password.
//!      `cryptoType="SHA-512"`. Recomputed per request.
//!   2. `requestSignature` — SHA3-512 over the documented input string for
//!      the called operation. `cryptoType="SHA3-512"`. For non-`manageInvoice`
//!      / non-`manageAnnulment` operations the input is exactly:
//!
//!      ```text
//!      requestId || requestTimestamp(YYYY-MM-DDTHH:MM:SSZ, UTC) || xmlSignKey
//!      ```
//!
//!      `xmlSignKey` is **leading/trailing ASCII-whitespace-trimmed**
//!      before hashing (PR-62 / session-82). NAV's `xmlSignKey` is
//!      documented as alphanumeric ASCII; operator paste artifacts
//!      (trailing newline from TextEdit, leading/trailing space from
//!      a portal copy) used to silently land in the keychain blob and
//!      produce `INVALID_REQUEST_SIGNATURE` rejections. See
//!      [`trim_ascii_ws`] for the full rationale.
//!
//!      Each signature computation also emits one `tracing::info!`
//!      line with lengths, non-alphanumeric byte counts, the raw
//!      key's first and last byte (hex), and the first 8 hex chars
//!      of the resulting digest. PR-62 emitted this at `debug!` —
//!      session-83 / PR-63 promoted it to `info!` so the diagnostic
//!      is visible under the default `RUST_LOG=info` without operators
//!      having to discover the magic env-var dance. See
//!      [`log_signature_diagnostics`] for the disclosure-budget
//!      reasoning.
//!
//!      For `manageInvoice` and `manageAnnulment` the input is extended by a
//!      per-invoice-index suffix:
//!
//!      ```text
//!      ... || SHA3-512_hex(operation || base64(invoiceData))   per index,
//!                                                                concatenated
//!      ```
//!
//! # Output encoding
//!
//! Both hashes are emitted as **uppercase hexadecimal** strings. The NAV v3.0
//! XSD types `SHA512Type` and `SHA3-512Type` both pin `[0-9A-F]{128}`. Lower-
//! case hex is REJECTED by NAV with `INVALID_REQUEST_SIGNATURE`. Verified by
//! inspection against the v3.0 XSD; cross-checked against `pzs/php-nav-online-
//! szamla` (PHP) and `angro-kft/nav-online-szamla` (Node) — both emit upper-
//! case hex.
//!
//! # What this module does NOT do
//!
//!   - It does not load credentials (that is `crate::credentials`).
//!   - It does not build the SOAP envelope (that is `crate::soap`).
//!   - It does not call NAV (that is `crate::operations`).
//!
//! This module is pure: same inputs → same outputs, byte-for-byte. The unit
//! tests below assert that property against fixed inputs so regressions
//! surface at unit-test time, not at the first failed NAV submission.
//!
//! # Why the per-invoice extension is built as a separate helper
//!
//! `request_signature_manage` is exposed alongside `request_signature` rather
//! than collapsed into a single "maybe-pass-Vec" function because (a) the two
//! input strings are different shapes (extra suffix vs not), (b) the call
//! sites are different operations with different validation rules, and (c)
//! the failure mode of "accidentally pass `&[]` to the manage form when you
//! meant to call the non-manage form" is exactly the silent-degradation
//! pattern CLAUDE.md rule 12 names.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use sha2::{Digest as _, Sha512};
// `sha3::Sha3_512` is the RustCrypto **FIPS 202 / Keccak** SHA3-512
// — NOT SHA-2/512. NAV's v3.0 `<requestSignature cryptoType="SHA3-512">`
// names FIPS 202. Distinct crate (`sha3` vs `sha2`), distinct hash family.
// Session-83 / PR-63 audit pinned this against the SHA-2/512 lookalike;
// see `request_signature_pins_known_sha3_512_vector` below — the test
// hardcodes a precomputed Keccak digest that differs byte-for-byte from
// the SHA-2/512 digest of the same input, so an accidental swap to
// `sha2::Sha512` here would loud-fail at test time, not at NAV reject time.
use sha3::Sha3_512;

/// SHA-512 of `password` rendered as uppercase hex. Used as the per-request
/// `<passwordHash>` value with `cryptoType="SHA-512"`.
///
/// Note the input is `&[u8]` (not `&str`): the password is a secret the
/// caller fetched from the keychain via `NavCredentials::password_bytes()`,
/// and the byte slice form keeps the caller from accidentally `Display`-
/// formatting it through a `&str` somewhere.
pub fn password_hash(password: &[u8]) -> String {
    let mut hasher = Sha512::new();
    hasher.update(password);
    hex_upper(&hasher.finalize())
}

/// SHA3-512 of the request-signature input for a non-`manageInvoice` /
/// non-`manageAnnulment` operation (e.g. `tokenExchange`,
/// `queryTransactionStatus`, `queryInvoiceCheck`, `queryInvoiceDigest`).
///
/// Input concatenation per ADR-0009 §4 + ADR-0020 §2:
///
///   request_id || request_timestamp || xml_sign_key
///
/// `request_timestamp` must already be in the NAV-mandated form
/// `YYYY-MM-DDTHH:MM:SSZ` UTC (see [`crate::soap::parts::request_timestamp`]).
/// `xml_sign_key` is the bytes returned by
/// `NavCredentials::sign_key_bytes()`. ASCII whitespace at either end is
/// trimmed before hashing (see [`trim_ascii_ws`]) — defends against
/// operator paste artifacts in the keychain blob (PR-62 / session-82).
pub fn request_signature(request_id: &str, request_timestamp: &str, xml_sign_key: &[u8]) -> String {
    let key = trim_ascii_ws(xml_sign_key);
    let mut hasher = Sha3_512::new();
    // Concatenation audit pinned in session-83 / PR-63:
    //   - ORDER:     request_id, then request_timestamp, then sign_key.
    //                Matches ADR-0009 §4 + ADR-0020 §2. Reordering produces
    //                INVALID_REQUEST_SIGNATURE; the three `update()` calls
    //                below ARE the wire order.
    //   - SEPARATOR: none. NAV does not insert any byte between the parts.
    //                Three back-to-back `update()`s with no padding byte
    //                (verified by the empty-input test below, which equals
    //                SHA3-512("") exactly).
    //   - CASE:      `.as_bytes()` preserves the caller's case verbatim.
    //                NAV is case-sensitive on all three inputs.
    //   - ENCODING:  `.as_bytes()` returns the UTF-8 byte stream of the
    //                `&str`, NOT codepoints. ASCII inputs hash identically
    //                to their byte form. The sign_key is already `&[u8]`,
    //                so no transcoding is possible there.
    hasher.update(request_id.as_bytes());
    hasher.update(request_timestamp.as_bytes());
    hasher.update(key);
    let out = hex_upper(&hasher.finalize());
    log_signature_diagnostics(
        "tokenExchange/query",
        request_id,
        request_timestamp,
        xml_sign_key,
        key,
        &out,
    );
    out
}

/// SHA3-512 of the request-signature input for `manageInvoice` /
/// `manageAnnulment`.
///
/// Same prefix as [`request_signature`], extended by — for each per-index
/// invoice in `invoice_inputs` (in index order; index 1, 2, 3, ...) — the
/// **uppercase-hex** SHA3-512 of:
///
/// ```text
/// operation || base64(invoice_data_xml)
/// ```
///
/// concatenated onto the running input. Per ADR-0009 §4:
///
///   "`manageInvoice` / `manageAnnulment`: same input, plus per
///    invoice-index a SHA3-512 of `operation || base64(invoiceData)`,
///    concatenated in index order."
///
/// `operation` is the SOAP `manageInvoiceOperation/operation` enum value
/// (`"CREATE"`, `"MODIFY"`, `"STORNO"` for `manageInvoice`; `"ANNUL"` for
/// `manageAnnulment`). `invoice_data_xml` is the raw XML bytes of the
/// `<InvoiceData>` element (the same bytes the binary writes to disk in
/// PR-5, base64-encoded onto the wire). The base64 encoding here is
/// **standard alphabet with padding** per RFC 4648 §4 — NAV does NOT
/// accept URL-safe or unpadded forms.
///
/// **Length constraint:** NAV's v3.0 `manageInvoice` request caps the per-
/// index `<invoiceOperation>` block at 100 entries. This function does not
/// enforce that — the caller (the `manage_invoice` operation in
/// `crate::operations`) validates length before invoking. Keeping the
/// validation upstream of the signature lets the unit tests here exercise
/// the signature math without inheriting a business-rule constraint.
pub fn request_signature_manage(
    request_id: &str,
    request_timestamp: &str,
    xml_sign_key: &[u8],
    invoice_inputs: &[InvoiceSignatureInput<'_>],
) -> String {
    let key = trim_ascii_ws(xml_sign_key);
    let mut hasher = Sha3_512::new();
    // Same concatenation properties as `request_signature` above
    // (order, no separator, case-preserved, UTF-8 bytes). Pinned in
    // session-83 / PR-63 audit. The manage variant appends per-index
    // suffix hex strings AFTER the sign_key, in the index order the
    // caller supplied (which must equal the wire order — enforced at
    // the call site in `crate::soap::render_manage_invoice_request`).
    hasher.update(request_id.as_bytes());
    hasher.update(request_timestamp.as_bytes());
    hasher.update(key);
    for input in invoice_inputs {
        let suffix_hex = per_invoice_hex(input);
        hasher.update(suffix_hex.as_bytes());
    }
    let out = hex_upper(&hasher.finalize());
    log_signature_diagnostics(
        "manageInvoice/manageAnnulment",
        request_id,
        request_timestamp,
        xml_sign_key,
        key,
        &out,
    );
    out
}

/// One per-invoice-index contribution to a `manageInvoice` /
/// `manageAnnulment` signature.
///
/// Borrowing both fields keeps the caller's payload-ownership model
/// unaltered — the invoice XML stays in its `Vec<u8>`/`&[u8]` original
/// without an additional copy.
#[derive(Debug, Clone, Copy)]
pub struct InvoiceSignatureInput<'a> {
    /// `"CREATE"` | `"MODIFY"` | `"STORNO"` for `manageInvoice`;
    /// `"ANNUL"` for `manageAnnulment`. Passed through verbatim — the
    /// operation must already match what the SOAP envelope's
    /// `<invoiceOperation>/<operation>` element carries; mismatch
    /// produces `INVALID_REQUEST_SIGNATURE` from NAV.
    pub operation: &'a str,

    /// Raw `<InvoiceData>` XML bytes for this index. Base64-encoded
    /// (standard alphabet with padding) before hashing per ADR-0009 §4.
    pub invoice_data_xml: &'a [u8],
}

// ──────────────────────────────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────────────────────────────

/// Compute the per-invoice-index hex suffix for one input.
/// Extracted so the unit test below can exercise the per-index math
/// directly without going through the full `request_signature_manage`.
fn per_invoice_hex(input: &InvoiceSignatureInput<'_>) -> String {
    let base64_xml = BASE64_STANDARD.encode(input.invoice_data_xml);
    let mut hasher = Sha3_512::new();
    hasher.update(input.operation.as_bytes());
    hasher.update(base64_xml.as_bytes());
    hex_upper(&hasher.finalize())
}

/// PR-62 / session-82 — trim leading and trailing ASCII whitespace
/// (space, tab, CR, LF, vertical-tab, form-feed) from `xml_sign_key`
/// before hashing.
///
/// **Why this exists.** NAV's `xmlSignKey` is documented as an
/// alphanumeric ASCII string with no whitespace. Operators paste it
/// from NAV's portal into ABERP's setup wizard or rotate form. Pastes
/// regularly carry a trailing `\n` (TextEdit autocompletion) or a
/// leading/trailing space (the portal copies the value with the
/// surrounding cell whitespace on some browsers). Both pre-PR-62 paths
/// — the CLI's `read_line` and the HTTP wizard's JSON deserialisation
/// — wrote the value verbatim into the keychain blob; the trailing
/// whitespace then participated in the SHA3-512 input, while NAV
/// (which holds the clean key in its own DB) computed a different
/// signature and rejected the request with `INVALID_REQUEST_SIGNATURE`
/// (session 82 — Hungarian `"Érvénytelen kérés aláírás!"`).
///
/// **Why the fix lives here, not at write-time.** The keychain blob
/// shape and the write path (`keychain::write_blob`, the setup-wizard
/// HTTP route, the rotate route) are out of scope for PR-62
/// (signature-only — see the session-82 brief). Trimming inside the
/// signature path also survives **existing dirty blobs** — an operator
/// whose previous setup baked in a trailing newline does not have to
/// re-enter the key. The keychain still holds the dirty bytes; the
/// signature path normalises them at use-time.
///
/// **What is NOT trimmed.** Only the xmlSignKey input to the signature
/// computation. The keychain bytes themselves are unchanged. The
/// password (separate `passwordHash` flow) and the change_key (AES
/// decode flow) are NOT trimmed here — those are different code paths
/// and out of scope for this PR.
fn trim_ascii_ws(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(bytes.len());
    if start >= end {
        return &[];
    }
    &bytes[start..end]
}

/// Session-83 / PR-63 — emit one structured `tracing::info!` line per
/// signature computation. PR-62 introduced this at `debug!` so it was
/// silent under the default `RUST_LOG=info` and Ervin's terminal showed
/// nothing after the retry; the second `INVALID_REQUEST_SIGNATURE` we
/// could not triangulate. Promoted to `info!` so it is **always**
/// visible in the operator's terminal without extra env vars — one
/// line per submit, ~250 bytes, well inside any reasonable log budget.
///
/// **No secret bytes are logged.** Disclosure budget per the
/// session-83 brief:
///
///   - `request_id`, `request_id_len`, `request_timestamp`:
///     not secret — NAV echoes both back verbatim in every response,
///     and they appear in the audit-ledger payload. Logging them lets
///     us correlate this line with NAV's reject body byte-for-byte.
///   - `sign_key_len_raw`, `sign_key_len_trimmed`,
///     `sign_key_nonalnum_raw`, `sign_key_nonalnum_trimmed`:
///     lengths and class-counts only, never bytes. If `raw != trimmed`
///     the keychain blob has boundary whitespace and PR-62's trim
///     path saved the request. If `nonalnum_trimmed > 0` the key
///     still has non-alphanumeric bytes after trim (BOM, mid-string
///     whitespace, non-ASCII) — surface for follow-up.
///   - `sign_key_first_byte` / `sign_key_last_byte`:
///     two single-byte hex values. Two bytes out of (typically) 32
///     leak too little entropy to attack the key, but they catch the
///     entire "is the boundary clean?" class of bug — a NAV-portal
///     copy whose first byte is `\x20` (space) or `\xef` (UTF-8 BOM
///     lead) is instantly visible. Reported on the **raw** key so
///     the trim path's effect is observable from the log.
///   - `signature_hex_prefix_8`:
///     first eight hex chars of the SHA3-512 output. The full digest
///     is 128 hex chars; eight is 32 bits, not enough to attack a
///     SHA3-512 inversion, but enough to triangulate against a
///     NAV-side echo when one becomes available, and enough to
///     distinguish two same-input calls (which should match) from
///     two different-input calls (which should not).
///
/// Format: one structured `tracing` event with all fields named, so
/// `RUST_LOG=info` operators see a single human-readable line and
/// machine-readable shippers (JSON sink) get the fields tagged.
fn log_signature_diagnostics(
    operation_family: &'static str,
    request_id: &str,
    request_timestamp: &str,
    sign_key_raw: &[u8],
    sign_key_trimmed: &[u8],
    signature_hex: &str,
) {
    let nonalnum_raw = sign_key_raw
        .iter()
        .filter(|b| !b.is_ascii_alphanumeric())
        .count();
    let nonalnum_trimmed = sign_key_trimmed
        .iter()
        .filter(|b| !b.is_ascii_alphanumeric())
        .count();
    // Single-byte first/last reports on the RAW key, so the operator
    // sees what landed in the keychain blob — the trimmed key just
    // hides paste artifacts. Empty key → report 0x00 sentinel; NAV
    // would reject an empty key for a different reason anyway, but
    // the log must not panic.
    let first_byte = sign_key_raw.first().copied().unwrap_or(0);
    let last_byte = sign_key_raw.last().copied().unwrap_or(0);
    let prefix_end = signature_hex.len().min(8);
    tracing::info!(
        target: "aberp_nav_transport::signatures",
        operation_family,
        request_id,
        request_id_len = request_id.len(),
        request_timestamp,
        sign_key_len_raw = sign_key_raw.len(),
        sign_key_len_trimmed = sign_key_trimmed.len(),
        sign_key_nonalnum_raw = nonalnum_raw,
        sign_key_nonalnum_trimmed = nonalnum_trimmed,
        sign_key_first_byte = format_args!("\\x{:02x}", first_byte),
        sign_key_last_byte = format_args!("\\x{:02x}", last_byte),
        signature_hex_prefix_8 = &signature_hex[..prefix_end],
        "NAV requestSignature input metadata (no secret bytes logged)"
    );
}

/// Encode a hash as uppercase hex. NAV's XSD types pin `[0-9A-F]{128}`;
/// lowercase or mixed-case is rejected with `INVALID_REQUEST_SIGNATURE`.
/// Kept private so all hex emission for NAV goes through one place.
fn hex_upper(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // `format!` with `{:02X}` is the obvious option but allocates
        // per byte; a hand loop with the lookup table is constant-time
        // (cache-line) and avoids the per-call allocation. The hex
        // crate would do this too but adds a dep for two ten-line
        // helpers (CLAUDE.md rule 2 — simplicity first).
        const TABLE: &[u8; 16] = b"0123456789ABCDEF";
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── password_hash ────────────────────────────────────────────────

    /// SHA-512("") expected output, uppercase hex. Pinned against NIST's
    /// FIPS-180-4 published test vector for the empty input. If this
    /// fails, the sha2 crate is doing something exotic — surface loud.
    const EMPTY_SHA512_HEX: &str =
        "CF83E1357EEFB8BDF1542850D66D8007D620E4050B5715DC83F4A921D36CE9CE\
         47D0D13C5D85F2B0FF8318D2877EEC2F63B931BD47417A81A538327AF927DA3E";

    #[test]
    fn password_hash_matches_fips_180_4_empty_vector() {
        assert_eq!(
            password_hash(b""),
            EMPTY_SHA512_HEX.replace([' ', '\n'], "")
        );
    }

    #[test]
    fn password_hash_is_deterministic() {
        let a = password_hash(b"correct horse battery staple");
        let b = password_hash(b"correct horse battery staple");
        assert_eq!(a, b);
        assert_eq!(a.len(), 128, "SHA-512 hex is exactly 128 chars");
    }

    #[test]
    fn password_hash_is_uppercase_hex_only() {
        let h = password_hash(b"hunter2");
        assert!(
            h.chars().all(|c| matches!(c, '0'..='9' | 'A'..='F')),
            "NAV requires uppercase hex; got: {h}"
        );
    }

    #[test]
    fn password_hash_handles_non_ascii_password_bytes() {
        // Passwords sourced from the keychain are byte-shaped; UTF-8
        // sequences must hash by their bytes, not by their codepoints.
        // The test guards against a future contributor reaching for a
        // `&str`-only API on `sha2`.
        let utf8_password = "jelszó-Á-日本".as_bytes();
        let h = password_hash(utf8_password);
        assert_eq!(h.len(), 128);
    }

    // ── request_signature (non-manage) ───────────────────────────────

    #[test]
    fn request_signature_is_deterministic() {
        let a = request_signature("REQ-1", "20260520T120000Z", b"sign-key");
        let b = request_signature("REQ-1", "20260520T120000Z", b"sign-key");
        assert_eq!(a, b);
        assert_eq!(a.len(), 128);
    }

    #[test]
    fn request_signature_distinguishes_inputs() {
        let base = request_signature("REQ-1", "20260520T120000Z", b"sign-key");
        let other_id = request_signature("REQ-2", "20260520T120000Z", b"sign-key");
        let other_ts = request_signature("REQ-1", "20260520T120001Z", b"sign-key");
        let other_key = request_signature("REQ-1", "20260520T120000Z", b"sign-key-2");
        assert_ne!(base, other_id);
        assert_ne!(base, other_ts);
        assert_ne!(base, other_key);
    }

    /// SHA3-512("") expected output (FIPS 202 / Keccak Team test vector).
    /// Concatenating three empty inputs is the same as hashing the empty
    /// string — guards against a future contributor accidentally inserting
    /// a separator byte between `update()` calls.
    const EMPTY_SHA3_512_HEX: &str =
        "A69F73CCA23A9AC5C8B567DC185A756E97C982164FE25859E0D1DCC1475C80A6\
         15B2123AF1F5F94C11E3E9402C3AC558F500199D95B6D3E301758586281DCD26";

    #[test]
    fn request_signature_concatenation_has_no_implicit_separator() {
        let h = request_signature("", "", b"");
        assert_eq!(h, EMPTY_SHA3_512_HEX.replace([' ', '\n'], ""));
    }

    /// Session-83 / PR-63 — algorithm + concatenation pin against a
    /// hardcoded triple.
    ///
    /// Hashes
    /// `requestId="REQ00TESTREQ" || timestamp="2024-01-01T00:00:00Z"
    ///  || xml_sign_key="testkey1234567890"`
    /// — the 49-byte string
    /// `"REQ00TESTREQ2024-01-01T00:00:00Ztestkey1234567890"` —
    /// against the FIPS 202 SHA3-512 digest precomputed externally
    /// (Python `hashlib.sha3_512(...).hexdigest().upper()`).
    ///
    /// What this pin catches that nothing else does:
    ///
    ///   1. **Algorithm swap to SHA-2/512.** A future contributor
    ///      reaching for `sha2::Sha512` (which is already in this
    ///      file's `use` for the password hash) would compute
    ///      `0834CD7F...` — a totally different digest. The test
    ///      loud-fails before NAV does.
    ///
    ///   2. **Implicit separator byte.** Any padding insert between
    ///      the three `update()` calls changes the digest. The empty-
    ///      input test above catches the simplest case; this one
    ///      catches the "separator only between non-empty parts"
    ///      flavour that the empty test cannot.
    ///
    ///   3. **Case-folding.** If a future `.to_lowercase()` slips in
    ///      anywhere on the input strings, the digest changes
    ///      (uppercase `REQ` → `req`, uppercase `T`/`Z` in the
    ///      timestamp → `t`/`z`). The test pin holds the case
    ///      exactly as NAV requires.
    ///
    ///   4. **UTF-8 vs codepoint hashing.** All three inputs are
    ///      ASCII, so this particular vector wouldn't catch a
    ///      codepoint regression — but the empty-input test's
    ///      sibling on the manage form, plus the `password_hash`
    ///      non-ASCII test, cover the byte-shape pin. Documenting
    ///      the gap here so a future session knows to extend.
    ///
    /// If a published NAV worked example surfaces in a future session,
    /// add it here as a second pin — the only extra benefit beyond
    /// this internal pin is verification that **NAV computes the same
    /// digest as we do**, not just "we are self-consistent". Mechanical
    /// addition.
    const PIN_REQ_ID: &str = "REQ00TESTREQ";
    const PIN_TIMESTAMP: &str = "2024-01-01T00:00:00Z";
    const PIN_SIGN_KEY: &[u8] = b"testkey1234567890";
    const PIN_EXPECTED_HEX: &str = "575FB340945F3781F0EC37A9748E6F4C5FE370264940152B824FEE3BF865CAD0\
                                    7D76BFB7C8C5F9BFAC4368FE628B186655B8AFC357FCB50CE5F4A398F46D5B5E";

    #[test]
    fn request_signature_pins_known_sha3_512_vector() {
        let observed = request_signature(PIN_REQ_ID, PIN_TIMESTAMP, PIN_SIGN_KEY);
        assert_eq!(
            observed,
            PIN_EXPECTED_HEX.replace([' ', '\n'], ""),
            "SHA3-512 of REQ00TESTREQ+timestamp+sign_key drifted — \
             algorithm swap to SHA-2, separator insert, case-fold, or \
             concatenation reorder. NAV will reject every request."
        );
    }

    #[test]
    fn request_signature_pin_vector_differs_from_sha2_512() {
        // Cross-check: the same input under SHA-2/512 produces a
        // totally different digest. If this test ever passes (i.e.
        // SHA-2 and SHA-3 produced the same bytes), reality has
        // broken and the universe has bigger problems than NAV.
        const SHA2_512_OF_PIN_INPUT: &str =
            "0834CD7F945581D8207E2A966A739AC9917B56383C0393BD2DBCFB8D11072E53\
             92539AE80B1A63824F7857CD1B4198843930E47E748AFA4CF7C1C3D490387593";
        let observed = request_signature(PIN_REQ_ID, PIN_TIMESTAMP, PIN_SIGN_KEY);
        assert_ne!(
            observed,
            SHA2_512_OF_PIN_INPUT.replace([' ', '\n'], ""),
            "request_signature computed the SHA-2/512 of the input, \
             not the SHA3-512 — algorithm swap from sha3::Sha3_512 \
             to sha2::Sha512 is the most likely cause."
        );
    }

    // ── request_signature_manage ─────────────────────────────────────

    #[test]
    fn manage_signature_with_zero_invoices_equals_non_manage() {
        // A manage call with no invoice inputs reduces to the same hash
        // as the non-manage helper. This protects against a future
        // contributor inserting "if invoices.is_empty() { return X }"
        // shortcut that takes a different code path.
        let manage = request_signature_manage("REQ-1", "20260520T120000Z", b"sk", &[]);
        let plain = request_signature("REQ-1", "20260520T120000Z", b"sk");
        assert_eq!(manage, plain);
    }

    #[test]
    fn manage_signature_changes_when_invoice_payload_changes() {
        let h_a = request_signature_manage(
            "REQ-1",
            "20260520T120000Z",
            b"sk",
            &[InvoiceSignatureInput {
                operation: "CREATE",
                invoice_data_xml: b"<InvoiceData>A</InvoiceData>",
            }],
        );
        let h_b = request_signature_manage(
            "REQ-1",
            "20260520T120000Z",
            b"sk",
            &[InvoiceSignatureInput {
                operation: "CREATE",
                invoice_data_xml: b"<InvoiceData>B</InvoiceData>",
            }],
        );
        assert_ne!(h_a, h_b, "different payload → different signature");
    }

    #[test]
    fn manage_signature_is_order_sensitive() {
        let a = InvoiceSignatureInput {
            operation: "CREATE",
            invoice_data_xml: b"<InvoiceData>1</InvoiceData>",
        };
        let b = InvoiceSignatureInput {
            operation: "CREATE",
            invoice_data_xml: b"<InvoiceData>2</InvoiceData>",
        };
        let ab = request_signature_manage("REQ-1", "20260520T120000Z", b"sk", &[a, b]);
        let ba = request_signature_manage("REQ-1", "20260520T120000Z", b"sk", &[b, a]);
        assert_ne!(ab, ba, "per-ADR-0009 §4: concatenated in index order");
    }

    #[test]
    fn manage_signature_distinguishes_operation() {
        let xml = b"<InvoiceData>x</InvoiceData>";
        let create = request_signature_manage(
            "REQ-1",
            "20260520T120000Z",
            b"sk",
            &[InvoiceSignatureInput {
                operation: "CREATE",
                invoice_data_xml: xml,
            }],
        );
        let storno = request_signature_manage(
            "REQ-1",
            "20260520T120000Z",
            b"sk",
            &[InvoiceSignatureInput {
                operation: "STORNO",
                invoice_data_xml: xml,
            }],
        );
        assert_ne!(create, storno);
    }

    // ── per_invoice_hex internal ────────────────────────────────────

    #[test]
    fn per_invoice_hex_uses_standard_base64_with_padding() {
        // base64("ab") == "YWI=" (one pad). If the implementation ever
        // switches to URL-safe or unpadded form the suffix changes and
        // NAV rejects the request — catch it here, not in production.
        //
        // We don't pin the SHA3-512 hex (the value will need updating if
        // base64 alphabet ever changes); we just assert that the suffix
        // is exactly SHA3-512_hex("OP" || "YWI=").
        let observed = per_invoice_hex(&InvoiceSignatureInput {
            operation: "OP",
            invoice_data_xml: b"ab",
        });
        let mut expected = Sha3_512::new();
        expected.update(b"OP");
        expected.update(b"YWI=");
        let expected_hex = hex_upper(&expected.finalize());
        assert_eq!(observed, expected_hex);
    }

    // ── hex_upper invariant ─────────────────────────────────────────

    // ── PR-62 / session-82: xml_sign_key whitespace-trim pins ──────

    /// The signature MUST be byte-equal whether the operator's
    /// keychain blob carries a clean key, a trailing newline (the
    /// TextEdit / wizard-paste pattern), a leading space (the portal
    /// cell-copy pattern), or all of the above. NAV holds the clean
    /// key on its side and computes the signature against it; our
    /// signature has to match. If this test fails, the trim path
    /// regressed and operator submits will get
    /// `INVALID_REQUEST_SIGNATURE` again.
    #[test]
    fn request_signature_trims_trailing_newline_from_sign_key() {
        let clean = request_signature(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b"abcdefghijklmnopqrstuvwxyz012345",
        );
        let trailing_lf = request_signature(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b"abcdefghijklmnopqrstuvwxyz012345\n",
        );
        let trailing_crlf = request_signature(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b"abcdefghijklmnopqrstuvwxyz012345\r\n",
        );
        let leading_space = request_signature(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b"  abcdefghijklmnopqrstuvwxyz012345",
        );
        let both_ends = request_signature(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b" \t abcdefghijklmnopqrstuvwxyz012345 \r\n",
        );
        assert_eq!(clean, trailing_lf);
        assert_eq!(clean, trailing_crlf);
        assert_eq!(clean, leading_space);
        assert_eq!(clean, both_ends);
    }

    /// `request_signature_manage` shares the same trim path as
    /// `request_signature`. A regression that only trims one would
    /// break manageInvoice silently — pin it explicitly.
    #[test]
    fn request_signature_manage_trims_trailing_newline_from_sign_key() {
        let inputs = [InvoiceSignatureInput {
            operation: "CREATE",
            invoice_data_xml: b"<InvoiceData>x</InvoiceData>",
        }];
        let clean = request_signature_manage(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b"abcdefghijklmnopqrstuvwxyz012345",
            &inputs,
        );
        let dirty = request_signature_manage(
            "REQ12345ABCDEFG",
            "2026-05-25T16:41:07Z",
            b"  abcdefghijklmnopqrstuvwxyz012345\r\n",
            &inputs,
        );
        assert_eq!(clean, dirty);
    }

    /// Interior whitespace (e.g. a stray space in the middle of the
    /// key) is **not** trimmed — `trim_ascii_ws` only trims at the
    /// boundaries. NAV holds the canonical key; if an operator's key
    /// genuinely had interior whitespace they'd have a different bug
    /// (NAV's portal won't accept such a key in the first place), but
    /// we pin the boundary-only behaviour so a future contributor
    /// doesn't reach for `.iter().filter(|b| !b.is_ascii_whitespace())`
    /// thinking it's "more defensive". It isn't — it'd change the
    /// signature for any operator whose key legitimately contains
    /// alphanumerics on both sides of a paste artifact at position 5.
    #[test]
    fn request_signature_does_not_trim_interior_whitespace() {
        let no_interior = request_signature(
            "REQ-1",
            "2026-05-25T16:41:07Z",
            b"abcdefghij",
        );
        let with_interior = request_signature(
            "REQ-1",
            "2026-05-25T16:41:07Z",
            b"abcde fghij",
        );
        assert_ne!(no_interior, with_interior);
    }

    /// `trim_ascii_ws` on an all-whitespace input returns the empty
    /// slice. NAV will reject the resulting signature for a different
    /// reason (the sign_key in NAV's DB is not empty), but the function
    /// must not panic or index out of bounds.
    #[test]
    fn trim_ascii_ws_on_all_whitespace_returns_empty() {
        assert_eq!(trim_ascii_ws(b"   \t\r\n  "), b"");
        assert_eq!(trim_ascii_ws(b""), b"");
        assert_eq!(trim_ascii_ws(b"x"), b"x");
        assert_eq!(trim_ascii_ws(b"  x  "), b"x");
    }

    /// Byte-equality pin for the trim helper used by the signature
    /// path. Locks the exact byte-shape of the trimmed slice so a
    /// future refactor can't quietly broaden it (e.g. to also trim
    /// non-ASCII Unicode whitespace, which would over-trim a key
    /// that legitimately starts with a high-bit-set byte — NAV's
    /// xmlSignKey is ASCII so the over-trim wouldn't show up in
    /// production, but it'd be a silent behaviour drift).
    #[test]
    fn trim_ascii_ws_only_recognises_ascii_whitespace_bytes() {
        // 0xA0 is a non-breaking space in Latin-1 — NOT
        // `u8::is_ascii_whitespace()`. Stays in the result.
        assert_eq!(trim_ascii_ws(&[0xA0, b'x', 0xA0]), &[0xA0, b'x', 0xA0]);
        // Rust's `u8::is_ascii_whitespace` matches U+0020 SPACE,
        // U+0009 HT, U+000A LF, U+000C FF, U+000D CR. Vertical-tab
        // (0x0B) is intentionally NOT in that set — match Rust's
        // definition exactly so the pin doesn't quietly drift if a
        // future contributor reaches for a hand-rolled char check.
        assert_eq!(trim_ascii_ws(b" \t\r\nx\x0C"), b"x");
        // VT (0x0B) is NOT whitespace per Rust, so it survives the
        // trim. Pinning the negative case is what keeps the helper
        // honest about which bytes it accepts.
        assert_eq!(trim_ascii_ws(b"\x0Bx\x0B"), b"\x0Bx\x0B");
    }

    #[test]
    fn hex_upper_round_trips_zero_byte_and_high_byte() {
        // Hash bytes span the full 0x00..=0xFF range; ensure the
        // table-lookup hex emitter handles both ends. A handful of
        // bytes is enough to surface "the high nibble was masked"
        // class of bugs.
        assert_eq!(hex_upper(&[0x00]), "00");
        assert_eq!(hex_upper(&[0xFF]), "FF");
        assert_eq!(hex_upper(&[0xAB, 0xCD]), "ABCD");
        assert_eq!(hex_upper(&[0x12, 0x34, 0x56, 0x78]), "12345678");
    }
}
