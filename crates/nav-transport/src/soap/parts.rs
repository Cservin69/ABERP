//! Reusable SOAP-envelope building blocks per NAV v3.0:
//!
//!   - [`new_request_id`] — mint a fresh `requestId` for an outgoing call.
//!   - [`request_timestamp`] — render an `OffsetDateTime` in the
//!     `YYYY-MM-DDTHH:MM:SSZ` (extended ISO 8601) form NAV's
//!     `GenericTimestampType` requires.
//!   - [`signature_timestamp`] — strip separators from the XML timestamp
//!     to produce the `YYYYMMDDhhmmss` form NAV's `requestSignature`
//!     computation consumes. MUST be called before every
//!     `request_signature` / `request_signature_manage` invocation.
//!   - [`write_header`] — `<common:header>` block.
//!   - [`write_user`] — `<common:user>` block.
//!   - [`write_software`] — `<software>` self-identification block.
//!
//! The two write helpers are `pub(super)` not `pub` — they are the
//! `crate::soap` module's internal vocabulary and are exposed to the unit
//! tests via `#[cfg(test)] pub use` in `mod.rs` only if needed. PR-7-B-1
//! does not need that escape hatch; the per-helper tests below run in
//! this module's own scope.

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;
use time::OffsetDateTime;
use ulid::Ulid;

use crate::error::NavTransportError;

use super::envelope_io;

/// Mint a fresh `requestId`. NAV's v3.0 `RequestIdType` pattern is
/// `[+a-zA-Z0-9_]{1,30}` — a 26-character Crockford-base32 ULID fits
/// cleanly inside that surface and gives us monotonic ordering for free
/// (ADR-0005). The leading `REQ` prefix is added so log lines and
/// audit-ledger entries are scannable for "this is a NAV requestId,
/// not an internal ULID" — keeps the prefix budget tiny (4 chars) and
/// the total length well inside the 30-char NAV cap.
pub fn new_request_id() -> String {
    format!("REQ{}", Ulid::new())
}

/// Render a UTC timestamp in NAV's required `YYYY-MM-DDTHH:MM:SSZ` form.
///
/// NAV's v3.0 XSD `GenericTimestampType` (used by `<common:timestamp>`)
/// pins the regex `\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z` —
/// extended ISO 8601 with the literal `T` separator and trailing `Z`.
/// We emit the integer-second form; the optional milliseconds branch
/// is unused (NAV accepts but does not require sub-second precision and
/// adding it would needlessly broaden the signature input surface).
pub fn request_timestamp(at: OffsetDateTime) -> Result<String, NavTransportError> {
    // Convert to UTC first — caller may have passed an offset-bearing
    // value (typically OffsetDateTime::now_utc() which is already UTC,
    // but the type allows arbitrary offsets and we don't want to assume).
    let utc = at.to_offset(time::UtcOffset::UTC);
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        utc.year(),
        utc.month() as u8,
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second(),
    ))
}

/// Strip separators from an XML-format timestamp for NAV signature input.
///
/// NAV v3.0 interface spec section 1.5.1: the `requestSignature` hash
/// input uses a **masked** timestamp — `YYYYMMDDhhmmss` with all
/// separators removed. The `-`, `:`, `T`, and `Z` characters that are
/// required in the XML `<timestamp>` element must NOT appear in the
/// signature computation.
///
/// Input:  `YYYY-MM-DDTHH:MM:SSZ` (as produced by [`request_timestamp`])
/// Output: `YYYYMMDDhhmmss` (14 digits, no separators)
///
/// Every call site that computes a `requestSignature` MUST pass the
/// timestamp through this function before handing it to
/// [`crate::signatures::request_signature`] or
/// [`crate::signatures::request_signature_manage`]. The XML timestamp
/// itself is unchanged — only the signature input is masked.
pub fn signature_timestamp(xml_timestamp: &str) -> String {
    xml_timestamp
        .replace("-", "")
        .replace(":", "")
        .replace("T", "")
        .replace("Z", "")
}

/// Write the `<common:header>` block with the four required elements in
/// the order NAV's XSD requires: requestId, timestamp, requestVersion,
/// headerVersion. ORDER MATTERS — XSD `sequence`, not `all`. A reordered
/// header produces `INCORRECT_REQUEST_SCHEMA` from NAV.
pub(super) fn write_header(
    w: &mut Writer<&mut Vec<u8>>,
    request_id: &str,
    request_timestamp: &str,
) -> Result<(), NavTransportError> {
    w.write_event(Event::Start(BytesStart::new("common:header")))
        .map_err(envelope_io)?;
    write_common(w, "requestId", request_id)?;
    write_common(w, "timestamp", request_timestamp)?;
    write_common(w, "requestVersion", "3.0")?;
    write_common(w, "headerVersion", "1.0")?;
    w.write_event(Event::End(BytesEnd::new("common:header")))
        .map_err(envelope_io)?;
    Ok(())
}

/// Write the `<common:user>` block. Four required elements in XSD-fixed
/// order: login, passwordHash, taxNumber, requestSignature. ORDER MATTERS.
pub(super) fn write_user(
    w: &mut Writer<&mut Vec<u8>>,
    login: &str,
    password_hash_hex: &str,
    tax_number_8: &str,
    signature_hex: &str,
) -> Result<(), NavTransportError> {
    w.write_event(Event::Start(BytesStart::new("common:user")))
        .map_err(envelope_io)?;
    write_common(w, "login", login)?;

    // passwordHash with cryptoType attribute.
    let mut pwd = BytesStart::new("common:passwordHash");
    pwd.push_attribute(("cryptoType", "SHA-512"));
    w.write_event(Event::Start(pwd)).map_err(envelope_io)?;
    w.write_event(Event::Text(BytesText::new(password_hash_hex)))
        .map_err(envelope_io)?;
    w.write_event(Event::End(BytesEnd::new("common:passwordHash")))
        .map_err(envelope_io)?;

    write_common(w, "taxNumber", tax_number_8)?;

    // requestSignature with cryptoType attribute.
    let mut sig = BytesStart::new("common:requestSignature");
    sig.push_attribute(("cryptoType", "SHA3-512"));
    w.write_event(Event::Start(sig)).map_err(envelope_io)?;
    w.write_event(Event::Text(BytesText::new(signature_hex)))
        .map_err(envelope_io)?;
    w.write_event(Event::End(BytesEnd::new("common:requestSignature")))
        .map_err(envelope_io)?;

    w.write_event(Event::End(BytesEnd::new("common:user")))
        .map_err(envelope_io)?;
    Ok(())
}

/// Write the `<software>` block identifying ABERP per the NAV
/// SoftwareType XSD. NAV uses this for incident-response routing and
/// for population reporting; the values are operator-visible.
///
/// `softwareOperation` is fixed to `LOCAL_SOFTWARE` (the v3.0
/// enumeration value for self-hosted billing software) — the
/// alternative `ONLINE_SERVICE` is for SaaS providers that submit on
/// behalf of multiple taxpayers, which ABERP is not. Operator-visible
/// fields use plain strings rather than feature-flagged config; the
/// values agree with `Cargo.toml` and `README.md` by inspection.
pub(super) fn write_software(w: &mut Writer<&mut Vec<u8>>) -> Result<(), NavTransportError> {
    w.write_event(Event::Start(BytesStart::new("software")))
        .map_err(envelope_io)?;
    // softwareId XSD pattern is `[0-9A-Z\-]{18}` — EXACTLY 18 chars
    // (length is pinned, not a max). NAV-test rejected the prior
    // 17-char value `ABERP000000000001` with `SCHEMA_VIOLATION` on
    // SoftwareIdType in session-81 / PR-61; the hyphen splits the
    // ABERP brand prefix from a numeric build slot and keeps the
    // string in the regex's character class.
    write_default(w, "softwareId", "ABERP-000000000001")?;
    write_default(w, "softwareName", "ABERP")?;
    write_default(w, "softwareOperation", "LOCAL_SOFTWARE")?;
    write_default(w, "softwareMainVersion", env!("CARGO_PKG_VERSION"))?;
    write_default(w, "softwareDevName", "Ervin Aben")?;
    write_default(w, "softwareDevContact", "ervin@aben.ch")?;
    w.write_event(Event::End(BytesEnd::new("software")))
        .map_err(envelope_io)?;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Tiny element-writing helpers — local to this module, kept here so
// callers do not duplicate the three-event ceremony per text element.
// ──────────────────────────────────────────────────────────────────────

fn write_common(
    w: &mut Writer<&mut Vec<u8>>,
    tag: &str,
    value: &str,
) -> Result<(), NavTransportError> {
    let qualified = format!("common:{tag}");
    w.write_event(Event::Start(BytesStart::new(qualified.clone())))
        .map_err(envelope_io)?;
    w.write_event(Event::Text(BytesText::new(value)))
        .map_err(envelope_io)?;
    w.write_event(Event::End(BytesEnd::new(qualified)))
        .map_err(envelope_io)?;
    Ok(())
}

fn write_default(
    w: &mut Writer<&mut Vec<u8>>,
    tag: &str,
    value: &str,
) -> Result<(), NavTransportError> {
    w.write_event(Event::Start(BytesStart::new(tag.to_string())))
        .map_err(envelope_io)?;
    w.write_event(Event::Text(BytesText::new(value)))
        .map_err(envelope_io)?;
    w.write_event(Event::End(BytesEnd::new(tag.to_string())))
        .map_err(envelope_io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_fits_nav_pattern_constraints() {
        let id = new_request_id();
        assert!(
            id.len() <= 30,
            "NAV RequestIdType max length is 30; got {} ({})",
            id.len(),
            id
        );
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '+'),
            "NAV pattern is [+a-zA-Z0-9_]; got {id}"
        );
        assert!(id.starts_with("REQ"), "REQ prefix is the scan marker");
    }

    #[test]
    fn request_timestamp_format_is_nav_extended_iso_8601() {
        // 2026-05-20 12:34:56 UTC — pinned input, pinned output.
        // If the formatter ever drifts (off-by-one on month, missing Z,
        // wrong separators), this test fails before the first NAV call.
        let dt = time::macros::datetime!(2026-05-20 12:34:56 UTC);
        let s = request_timestamp(dt).expect("format");
        assert_eq!(s, "2026-05-20T12:34:56Z");
    }

    #[test]
    fn request_timestamp_converts_non_utc_offset_to_utc() {
        // A caller that hands us a +02:00 (Hungarian summer time)
        // OffsetDateTime should still produce the UTC instant on the
        // wire. NAV would reject any other behaviour with
        // `INVALID_REQUEST_SIGNATURE` because the signature input
        // includes this exact string.
        let local = time::macros::datetime!(2026-05-20 14:34:56 +02:00);
        let s = request_timestamp(local).expect("format");
        assert_eq!(s, "2026-05-20T12:34:56Z");
    }

    /// PR-61 pin: NAV's `GenericTimestampType` XSD regex is
    /// `\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z`. NAV-test
    /// rejected the previous compact form (`20260525T161651Z`) with
    /// `SCHEMA_VIOLATION` on this exact pattern. Hand-rolled character-
    /// class check (no regex dep added) — verifies positions, separators,
    /// and the trailing `Z` against the NAV pattern.
    #[test]
    fn request_timestamp_matches_nav_generic_timestamp_regex() {
        let dt = time::macros::datetime!(2026-05-25 16:16:51 UTC);
        let s = request_timestamp(dt).expect("format");
        let bytes = s.as_bytes();
        assert_eq!(bytes.len(), 20, "extended-form length is 20 chars: {s}");
        for (i, b) in bytes.iter().enumerate() {
            let ok = match i {
                0..=3 | 5..=6 | 8..=9 | 11..=12 | 14..=15 | 17..=18 => b.is_ascii_digit(),
                4 | 7 => *b == b'-',
                10 => *b == b'T',
                13 | 16 => *b == b':',
                19 => *b == b'Z',
                _ => unreachable!(),
            };
            assert!(
                ok,
                "byte {i} = {:?} violates NAV GenericTimestampType regex (got {s})",
                *b as char
            );
        }
    }

    #[test]
    fn write_header_emits_required_children_in_xsd_order() {
        let mut buf: Vec<u8> = Vec::new();
        let mut w = Writer::new(&mut buf);
        write_header(&mut w, "REQ-x", "20260520T120000Z").expect("write");
        let s = std::str::from_utf8(&buf).expect("UTF-8");

        // XSD sequence order: requestId, timestamp, requestVersion, headerVersion.
        let r_id = s.find("<common:requestId>").expect("requestId present");
        let r_ts = s.find("<common:timestamp>").expect("timestamp present");
        let r_rv = s
            .find("<common:requestVersion>")
            .expect("requestVersion present");
        let r_hv = s
            .find("<common:headerVersion>")
            .expect("headerVersion present");
        assert!(
            r_id < r_ts && r_ts < r_rv && r_rv < r_hv,
            "header child order drifted: {s}"
        );
    }

    #[test]
    fn write_user_emits_required_children_in_xsd_order() {
        let mut buf: Vec<u8> = Vec::new();
        let mut w = Writer::new(&mut buf);
        write_user(
            &mut w,
            "TECHNICAL_LOGIN",
            // 128 hex chars
            "0000000000000000000000000000000000000000000000000000000000000000\
             0000000000000000000000000000000000000000000000000000000000000000",
            "12345678",
            "1111111111111111111111111111111111111111111111111111111111111111\
             1111111111111111111111111111111111111111111111111111111111111111",
        )
        .expect("write");
        let s = std::str::from_utf8(&buf).expect("UTF-8");

        let r_login = s.find("<common:login>").expect("login");
        let r_pwd = s.find("<common:passwordHash").expect("passwordHash");
        let r_tax = s.find("<common:taxNumber>").expect("taxNumber");
        let r_sig = s
            .find("<common:requestSignature")
            .expect("requestSignature");
        assert!(
            r_login < r_pwd && r_pwd < r_tax && r_tax < r_sig,
            "user child order drifted: {s}"
        );
        assert!(s.contains("cryptoType=\"SHA-512\""));
        assert!(s.contains("cryptoType=\"SHA3-512\""));
    }

    #[test]
    fn write_software_carries_aberp_identification() {
        let mut buf: Vec<u8> = Vec::new();
        let mut w = Writer::new(&mut buf);
        write_software(&mut w).expect("write");
        let s = std::str::from_utf8(&buf).expect("UTF-8");
        assert!(s.contains("<softwareName>ABERP</softwareName>"));
        assert!(s.contains("<softwareOperation>LOCAL_SOFTWARE</softwareOperation>"));
        // softwareMainVersion is pulled from CARGO_PKG_VERSION; assert
        // shape, not exact value (otherwise the test churns on every
        // workspace version bump).
        assert!(s.contains("<softwareMainVersion>"));
    }

    /// PR-61 pin: NAV's `SoftwareIdType` XSD regex is
    /// `[0-9A-Z\-]{18}` — EXACTLY 18 chars, uppercase alnum + hyphen.
    /// NAV-test rejected the previous 17-char `ABERP000000000001` with
    /// `SCHEMA_VIOLATION`. Hand-rolled character-class check.
    #[test]
    fn write_software_emits_softwareid_matching_nav_regex() {
        let mut buf: Vec<u8> = Vec::new();
        let mut w = Writer::new(&mut buf);
        write_software(&mut w).expect("write");
        let s = std::str::from_utf8(&buf).expect("UTF-8");

        let open = s.find("<softwareId>").expect("softwareId open tag");
        let close = s.find("</softwareId>").expect("softwareId close tag");
        let value = &s[open + "<softwareId>".len()..close];

        assert_eq!(
            value.len(),
            18,
            "NAV SoftwareIdType pattern pins length to 18; got {} ({value})",
            value.len()
        );
        assert!(
            value
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'A'..='Z' | '-')),
            "NAV SoftwareIdType pattern is [0-9A-Z-]; got {value}"
        );
    }
}
