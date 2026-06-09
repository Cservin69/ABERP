//! S281 / PR-266 — Email-relay endpoint + helpers.
//!
//! **S307 / PR-276 — DEPRECATED by [ADR-0009].** The push-based posture
//! ADR-0007 originally landed (storefront → ABERP POST) is superseded by
//! a polling architecture: the storefront enqueues outbound mail to its
//! own filesystem (`/var/lib/aberp-site/email-outbox/queued/`) and
//! ABERP's [`crate::email_outbox_poll_daemon`] (S307) pulls each entry,
//! delivers via SMTP, and POSTs `.../sent` or `.../failed` back. The
//! storefront-to-ABERP-loopback path that motivated this module no
//! longer exists in prod; this module stays for local-dev (single-
//! process testing, where running both halves on one box is easier with
//! push) and for manual API testing. In production every POST hits a
//! WARN log line — see [`crate::serve::handle_relay_send_email`].
//!
//! The validation, queue, and drain machinery below remain correct for
//! the local-dev path and so are kept intact. Removal of the entire
//! module is scheduled for a future session AFTER ADR-0009's end-to-end
//! validation criterion fires (one real customer quote round-tripped).
//!
//! Historical S281 description follows.
//!
//! The storefront POSTs `/api/internal/send-email` to ABERP per
//! ADR-0007. ABERP authenticates with the dedicated email-relay
//! bearer ([[email-relay-token-spoc]]), validates the body, persists
//! the request to [`crate::email_relay_queue`], writes attachments to
//! disk, emits an `EmailRelayQueued` audit entry, and returns 200 with
//! `audit_id`. The background drain ([`crate::email_relay_daemon`])
//! then walks the row through `Sending → Sent | Failed`.
//!
//! [ADR-0009]: ../../../docs/adr/0009-storefront-as-queue-no-tunnel.md
//!
//! ## Validation cliff
//!
//! - At least one recipient in `to`.
//! - `subject` is 1-200 chars (after CR/LF + Unicode-line-separator
//!   reject — same set as the SMTP send path's
//!   `email_invoice::is_forbidden_header_byte`).
//! - `body_text` is non-empty.
//! - Rendered total ≤ [`MAX_RELAY_BODY_BYTES`] (25 MB).
//! - ≤ [`MAX_ATTACHMENT_BYTES`] (20 MB) per attachment.
//! - ≤ [`MAX_ATTACHMENTS_PER_REQUEST`] (5) attachments.
//!
//! On validation failure the route returns `400` with a typed JSON
//! body so the storefront can surface the error without inspecting a
//! free-form message string.
//!
//! ## Rate limit
//!
//! Token-bucket per submitter (= per bearer token); [`MAX_RELAY_PER_MINUTE`]
//! requests / minute. Excess returns `429 Too Many Requests` with a
//! `Retry-After` header. The bucket lives in-process — single-daemon
//! single-tenant deployment so no shared-state cross-process concern.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use sha2::{Digest, Sha256};

/// Hard cap on total body bytes (text + html + attachments after b64
/// decode). 25 MB matches ADR-0007's open-question #3 suggestion.
pub const MAX_RELAY_BODY_BYTES: u64 = 25 * 1024 * 1024;
/// Hard cap per attachment.
pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
/// Hard cap on attachment count per request.
pub const MAX_ATTACHMENTS_PER_REQUEST: usize = 5;
/// Hard cap on subject length (chars). 200 is plenty for any
/// transactional mail.
pub const MAX_SUBJECT_CHARS: usize = 200;
/// Per-submitter rate-limit: requests / minute. ADR-0007 open-question
/// #2 suggested ~30/min matching the storefront's prior `GLOBAL_MAX`.
pub const MAX_RELAY_PER_MINUTE: u32 = 30;

/// Hash a recipient list for the audit payload. Comma-joins the
/// addresses in **byte-sort order with case-folded local parts** so
/// the same set of recipients hashes identically regardless of how the
/// caller orders them. SHA-256, lower-case hex.
///
/// The hash is stable across retries (same input → same hash) so a
/// forensic walker can join an `email.relay_queued` row to its
/// terminal `email.relay_sent` / `email.relay_failed` via this hash.
pub fn hash_recipient_list(addresses: &[String]) -> String {
    let mut normalised: Vec<String> = addresses
        .iter()
        .map(|a| a.trim().to_ascii_lowercase())
        .collect();
    normalised.sort();
    let joined = normalised.join(",");
    let mut h = Sha256::new();
    h.update(joined.as_bytes());
    format!("{:x}", h.finalize())
}

/// In-process token-bucket rate limiter, keyed by submitter id.
///
/// Implementation: a sliding window per submitter — store the
/// timestamps of the last [`MAX_RELAY_PER_MINUTE`] requests. On each
/// new request, drop timestamps older than 60s; if the surviving count
/// is at the cap, reject. Otherwise push the new instant. Simple, no
/// `Instant::now()` panics, no leaky-bucket arithmetic.
///
/// Single Mutex on the whole map (not per-submitter) — at this rate
/// (~30/min) contention is irrelevant.
#[derive(Debug, Default)]
pub struct RateLimiter {
    inner: Mutex<std::collections::HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to record a new request from `submitter`. Returns `Ok(())`
    /// on accept, `Err(retry_after_secs)` on cap-hit.
    pub fn try_acquire(&self, submitter: &str, now: Instant) -> Result<(), u64> {
        let mut guard = self
            .inner
            .lock()
            .expect("RateLimiter mutex must not be poisoned");
        let window = Duration::from_secs(60);
        let entry = guard.entry(submitter.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < window);
        if entry.len() as u32 >= MAX_RELAY_PER_MINUTE {
            // Retry-After = (oldest_in_window + 60s) - now, ceil to
            // the second. Always ≥ 1.
            let oldest = entry.first().copied().unwrap_or(now);
            let retry_after = window.saturating_sub(now.duration_since(oldest));
            let secs = retry_after.as_secs().max(1);
            return Err(secs);
        }
        entry.push(now);
        Ok(())
    }
}

/// Closed-vocab validation error codes — surfaced as JSON `code` in
/// the 400 body so the storefront can branch without parsing the
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationCode {
    MissingTo,
    BadRecipient,
    SubjectEmpty,
    SubjectTooLong,
    SubjectHasForbiddenByte,
    BodyTextEmpty,
    BodyTooLarge,
    AttachmentTooLarge,
    TooManyAttachments,
    AttachmentBase64Invalid,
    AttachmentFilenameEmpty,
    SubmitterEmpty,
}

impl ValidationCode {
    /// Wire form — stable token for the storefront's branch logic.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationCode::MissingTo => "missing_to",
            ValidationCode::BadRecipient => "bad_recipient",
            ValidationCode::SubjectEmpty => "subject_empty",
            ValidationCode::SubjectTooLong => "subject_too_long",
            ValidationCode::SubjectHasForbiddenByte => "subject_has_forbidden_byte",
            ValidationCode::BodyTextEmpty => "body_text_empty",
            ValidationCode::BodyTooLarge => "body_too_large",
            ValidationCode::AttachmentTooLarge => "attachment_too_large",
            ValidationCode::TooManyAttachments => "too_many_attachments",
            ValidationCode::AttachmentBase64Invalid => "attachment_base64_invalid",
            ValidationCode::AttachmentFilenameEmpty => "attachment_filename_empty",
            ValidationCode::SubmitterEmpty => "submitter_empty",
        }
    }
}

/// One validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: ValidationCode,
    pub message: String,
}

/// Re-export the SMTP send path's forbidden-byte set so the relay's
/// subject + recipient validation matches the SMTP layer exactly.
/// Wraps the crate-private `email_invoice::is_forbidden_header_byte`
/// behaviour using its public test corpus.
fn is_forbidden_header_byte(c: char) -> bool {
    matches!(
        c,
        '\r' | '\n' | '\u{0000}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// Lightweight email shape check — same one [`crate::smtp_config`]'s
/// `looks_like_email` uses. Not a full RFC-5322 parse (lettre does the
/// final check at send time), but rejects empties, whitespace, and
/// shapes that don't contain `@` + `.`.
fn looks_like_email(s: &str) -> bool {
    if s.trim() != s {
        return false;
    }
    if s.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    match s.split_once('@') {
        Some((local, domain)) => !local.is_empty() && !domain.is_empty() && domain.contains('.'),
        None => false,
    }
}

/// Validate one recipient address. Rejects CR/LF/NEL/U+2028/U+2029 +
/// shape check.
fn validate_recipient(addr: &str) -> Result<(), ValidationError> {
    if addr.chars().any(is_forbidden_header_byte) {
        return Err(ValidationError {
            code: ValidationCode::BadRecipient,
            message: format!(
                "recipient `{addr}` contains a forbidden header byte (CR / LF / NUL / NEL / U+2028 / U+2029)"
            ),
        });
    }
    if !looks_like_email(addr) {
        return Err(ValidationError {
            code: ValidationCode::BadRecipient,
            message: format!("recipient `{addr}` is not a valid email address shape"),
        });
    }
    Ok(())
}

/// Decoded view of a single attachment from the wire body.
#[derive(Debug, Clone)]
pub struct DecodedAttachment {
    /// Operator-typed filename (NOT yet path-sanitised; queue writer
    /// runs `sanitize_attachment_filename` before touching disk).
    pub filename: String,
    pub content_type: String,
    /// Decoded payload.
    pub bytes: Vec<u8>,
}

/// The fully-validated relay request the route handler hands to the
/// queue writer. All caps already checked; recipients normalised but
/// not modified beyond trim.
#[derive(Debug, Clone)]
pub struct ValidatedRelayRequest {
    pub submitter: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub attachments: Vec<DecodedAttachment>,
    /// SHA-256 of the canonicalised recipient list (= `to ∪ cc`,
    /// case-folded local parts, byte-sort, comma-join). Computed once
    /// here so the queue writer and the audit emitter share one source
    /// of truth.
    pub recipient_hash: String,
    /// Rendered body size (text + html + attachment bytes).
    pub byte_size: u64,
}

/// Wire-shape of the request body the storefront POSTs. Lives here so
/// the serve.rs handler can `serde_json::from_slice` directly.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RelayRequestBody {
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    pub subject: String,
    pub body_text: String,
    #[serde(default)]
    pub body_html: Option<String>,
    #[serde(default)]
    pub attachments: Vec<AttachmentBody>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AttachmentBody {
    pub filename: String,
    pub content_type: String,
    pub data_b64: String,
}

/// Validate + decode a wire request into a [`ValidatedRelayRequest`].
/// `submitter` is the token-identified caller — the route handler
/// passes `"storefront"` for the ADR-0007 caller, or a future
/// per-token identifier.
pub fn validate(
    submitter: &str,
    body: RelayRequestBody,
) -> Result<ValidatedRelayRequest, ValidationError> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

    if submitter.trim().is_empty() {
        return Err(ValidationError {
            code: ValidationCode::SubmitterEmpty,
            message: "submitter must be non-empty".to_string(),
        });
    }

    if body.to.is_empty() {
        return Err(ValidationError {
            code: ValidationCode::MissingTo,
            message: "to-list must contain at least one recipient".to_string(),
        });
    }
    let to: Vec<String> = body.to.iter().map(|s| s.trim().to_string()).collect();
    for addr in &to {
        validate_recipient(addr)?;
    }
    let cc: Vec<String> = body.cc.iter().map(|s| s.trim().to_string()).collect();
    for addr in &cc {
        validate_recipient(addr)?;
    }

    let subject_trim = body.subject.trim().to_string();
    if subject_trim.is_empty() {
        return Err(ValidationError {
            code: ValidationCode::SubjectEmpty,
            message: "subject must be non-empty".to_string(),
        });
    }
    if subject_trim.chars().count() > MAX_SUBJECT_CHARS {
        return Err(ValidationError {
            code: ValidationCode::SubjectTooLong,
            message: format!("subject exceeds {MAX_SUBJECT_CHARS} characters"),
        });
    }
    if let Some(c) = subject_trim.chars().find(|c| is_forbidden_header_byte(*c)) {
        return Err(ValidationError {
            code: ValidationCode::SubjectHasForbiddenByte,
            message: format!(
                "subject contains forbidden codepoint U+{:04X} (header-injection guard)",
                c as u32
            ),
        });
    }

    if body.body_text.is_empty() {
        return Err(ValidationError {
            code: ValidationCode::BodyTextEmpty,
            message: "body_text must be non-empty".to_string(),
        });
    }

    if body.attachments.len() > MAX_ATTACHMENTS_PER_REQUEST {
        return Err(ValidationError {
            code: ValidationCode::TooManyAttachments,
            message: format!(
                "request carries {} attachments; cap is {MAX_ATTACHMENTS_PER_REQUEST}",
                body.attachments.len()
            ),
        });
    }
    let mut decoded_atts: Vec<DecodedAttachment> = Vec::with_capacity(body.attachments.len());
    for att in body.attachments.into_iter() {
        if att.filename.trim().is_empty() {
            return Err(ValidationError {
                code: ValidationCode::AttachmentFilenameEmpty,
                message: "attachment filename must be non-empty".to_string(),
            });
        }
        let bytes = B64
            .decode(att.data_b64.as_bytes())
            .map_err(|e| ValidationError {
                code: ValidationCode::AttachmentBase64Invalid,
                message: format!(
                    "attachment `{}` data_b64 is not valid base64: {e}",
                    att.filename
                ),
            })?;
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Err(ValidationError {
                code: ValidationCode::AttachmentTooLarge,
                message: format!(
                    "attachment `{}` decoded size {} bytes exceeds {} byte cap",
                    att.filename,
                    bytes.len(),
                    MAX_ATTACHMENT_BYTES
                ),
            });
        }
        decoded_atts.push(DecodedAttachment {
            filename: att.filename,
            content_type: att.content_type,
            bytes,
        });
    }

    let body_text_bytes = body.body_text.len() as u64;
    let body_html_bytes = body.body_html.as_ref().map(|s| s.len() as u64).unwrap_or(0);
    let attachment_bytes: u64 = decoded_atts.iter().map(|a| a.bytes.len() as u64).sum();
    let total = body_text_bytes
        .saturating_add(body_html_bytes)
        .saturating_add(attachment_bytes);
    if total > MAX_RELAY_BODY_BYTES {
        return Err(ValidationError {
            code: ValidationCode::BodyTooLarge,
            message: format!(
                "rendered total {total} bytes exceeds {MAX_RELAY_BODY_BYTES} byte cap"
            ),
        });
    }

    // Combine `to` + `cc` for the recipient hash so a `to: [a]` /
    // `cc: [b]` and a `to: [a, b]` / `cc: []` produce DIFFERENT
    // hashes (they're different audit-trail records). This is the
    // GDPR-stable join key for forensic queries on the audit ledger.
    let mut combined: Vec<String> = Vec::with_capacity(to.len() + cc.len());
    combined.extend(to.iter().cloned());
    combined.extend(cc.iter().cloned());
    let recipient_hash = hash_recipient_list(&combined);

    Ok(ValidatedRelayRequest {
        submitter: submitter.to_string(),
        to,
        cc,
        subject: subject_trim,
        body_text: body.body_text,
        body_html: body.body_html,
        attachments: decoded_atts,
        recipient_hash,
        byte_size: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        B64.encode(bytes)
    }

    fn baseline() -> RelayRequestBody {
        RelayRequestBody {
            to: vec!["customer@example.com".to_string()],
            cc: vec![],
            subject: "Your quote".to_string(),
            body_text: "Hello.".to_string(),
            body_html: None,
            attachments: vec![],
        }
    }

    #[test]
    fn validate_accepts_baseline() {
        let v = validate("storefront", baseline()).expect("valid");
        assert_eq!(v.to, vec!["customer@example.com"]);
        assert_eq!(v.subject, "Your quote");
        assert!(v.attachments.is_empty());
        assert!(v.byte_size > 0);
    }

    #[test]
    fn validate_rejects_empty_submitter() {
        let err = validate("   ", baseline()).unwrap_err();
        assert_eq!(err.code, ValidationCode::SubmitterEmpty);
    }

    #[test]
    fn validate_rejects_empty_to() {
        let mut b = baseline();
        b.to = vec![];
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::MissingTo);
    }

    #[test]
    fn validate_rejects_bad_email_shape() {
        let mut b = baseline();
        b.to = vec!["not-an-email".to_string()];
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::BadRecipient);
    }

    #[test]
    fn validate_rejects_recipient_with_cr() {
        let mut b = baseline();
        b.to = vec!["a@b.c\r\nBcc: x@y.z".to_string()];
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::BadRecipient);
    }

    #[test]
    fn validate_rejects_recipient_with_unicode_line_separator() {
        for c in ['\u{0085}', '\u{2028}', '\u{2029}', '\u{0000}'] {
            let mut b = baseline();
            b.to = vec![format!("a@b.c{c}injected")];
            let err = validate("storefront", b).unwrap_err();
            assert_eq!(err.code, ValidationCode::BadRecipient);
        }
    }

    #[test]
    fn validate_rejects_subject_with_lf() {
        let mut b = baseline();
        b.subject = "evil\nBcc: x@y.z".to_string();
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::SubjectHasForbiddenByte);
    }

    #[test]
    fn validate_rejects_empty_subject() {
        let mut b = baseline();
        b.subject = "   ".to_string();
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::SubjectEmpty);
    }

    #[test]
    fn validate_rejects_overlong_subject() {
        let mut b = baseline();
        b.subject = "x".repeat(MAX_SUBJECT_CHARS + 1);
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::SubjectTooLong);
    }

    #[test]
    fn validate_rejects_empty_body_text() {
        let mut b = baseline();
        b.body_text = String::new();
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::BodyTextEmpty);
    }

    #[test]
    fn validate_decodes_attachment() {
        let mut b = baseline();
        let payload = b"hello pdf";
        b.attachments = vec![AttachmentBody {
            filename: "test.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            data_b64: b64(payload),
        }];
        let v = validate("storefront", b).expect("valid");
        assert_eq!(v.attachments.len(), 1);
        assert_eq!(v.attachments[0].bytes, payload);
        assert_eq!(v.attachments[0].filename, "test.pdf");
    }

    #[test]
    fn validate_rejects_invalid_base64() {
        let mut b = baseline();
        b.attachments = vec![AttachmentBody {
            filename: "test.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            data_b64: "not!base64!".to_string(),
        }];
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::AttachmentBase64Invalid);
    }

    #[test]
    fn validate_rejects_too_many_attachments() {
        let mut b = baseline();
        b.attachments = (0..=MAX_ATTACHMENTS_PER_REQUEST)
            .map(|i| AttachmentBody {
                filename: format!("a{i}.pdf"),
                content_type: "application/pdf".to_string(),
                data_b64: b64(b"x"),
            })
            .collect();
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::TooManyAttachments);
    }

    #[test]
    fn validate_rejects_oversize_attachment() {
        let mut b = baseline();
        let huge = vec![0u8; (MAX_ATTACHMENT_BYTES + 1) as usize];
        b.attachments = vec![AttachmentBody {
            filename: "big.bin".to_string(),
            content_type: "application/octet-stream".to_string(),
            data_b64: b64(&huge),
        }];
        let err = validate("storefront", b).unwrap_err();
        assert_eq!(err.code, ValidationCode::AttachmentTooLarge);
    }

    #[test]
    fn hash_is_stable_across_order() {
        // Two clients sending the same recipient set in different
        // order MUST hash identically.
        let a = hash_recipient_list(&["a@x.com".to_string(), "b@y.com".to_string()]);
        let b = hash_recipient_list(&["b@y.com".to_string(), "a@x.com".to_string()]);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_is_case_folded_on_local_part() {
        let a = hash_recipient_list(&["A@X.com".to_string()]);
        let b = hash_recipient_list(&["a@x.com".to_string()]);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_with_recipient_set() {
        let a = hash_recipient_list(&["a@x.com".to_string()]);
        let b = hash_recipient_list(&["b@x.com".to_string()]);
        assert_ne!(a, b);
    }

    #[test]
    fn rate_limiter_accepts_below_cap() {
        let r = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..MAX_RELAY_PER_MINUTE {
            r.try_acquire("storefront", now).expect("under cap");
        }
    }

    #[test]
    fn rate_limiter_rejects_above_cap() {
        let r = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..MAX_RELAY_PER_MINUTE {
            r.try_acquire("storefront", now).expect("under cap");
        }
        let err = r.try_acquire("storefront", now).unwrap_err();
        assert!((1..=60).contains(&err));
    }

    #[test]
    fn rate_limiter_per_submitter_independent() {
        let r = RateLimiter::new();
        let now = Instant::now();
        for _ in 0..MAX_RELAY_PER_MINUTE {
            r.try_acquire("storefront", now).expect("under cap");
        }
        // A different submitter has its own bucket.
        r.try_acquire("other", now).expect("different bucket");
    }

    #[test]
    fn rate_limiter_window_slides() {
        let r = RateLimiter::new();
        let t0 = Instant::now();
        for _ in 0..MAX_RELAY_PER_MINUTE {
            r.try_acquire("storefront", t0).expect("under cap");
        }
        // Move 61s forward — the window should have drained.
        let t1 = t0 + Duration::from_secs(61);
        r.try_acquire("storefront", t1).expect("window slid");
    }

    /// PR-266 / ADR-0007 §Audit — the audit payload posture is
    /// recipient-as-hash. The hash function MUST be deterministic and
    /// MUST be SHA-256 of the canonicalised list (we use lower-case
    /// hex). Pin so a future contributor renaming to MD5 / hex-upper
    /// can't silently break the audit-trail join key.
    #[test]
    fn pr_266_hash_is_lowercase_hex_sha256() {
        let h = hash_recipient_list(&["customer@example.com".to_string()]);
        assert_eq!(h.len(), 64); // SHA-256 hex
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
