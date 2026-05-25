//! NAV `tokenExchange` operation per ADR-0009 §4 + ADR-0020 §2.
//!
//! Flow:
//!
//!   1. Mint a fresh `requestId` + `requestTimestamp`.
//!   2. Render the `<TokenExchangeRequest>` envelope via
//!      `crate::soap::render_token_exchange_request` (signed inputs use
//!      the same request_id / timestamp).
//!   3. POST it (Content-Type: `application/xml`) to
//!      `<endpoint base url>/tokenExchange`.
//!   4. Capture the response body verbatim into `response_xml` BEFORE
//!      any parsing — this is the audit-evidence the binary will write
//!      to the ledger per ADR-0009 §8. A parser-side bug must not drop
//!      the bytes.
//!   5. If HTTP status is non-success, loud-fail
//!      (`NavTransportError::TokenExchangeHttpStatus`).
//!   6. Parse the `<common:result>` block. On `ERROR`, surface as
//!      `TokenExchangeResponseParse` (no retry classification on
//!      tokenExchange — every failure here is operator-actionable per
//!      ADR-0009 §4's "auth failures are not transient").
//!   7. Extract `<encodedExchangeToken>`, base64-decode, AES-128/ECB-
//!      decrypt with the tenant `xmlChangeKey`, UTF-8-decode.
//!   8. Return the decoded token wrapped in `Zeroizing<String>` plus
//!      the verbatim request/response bytes for audit.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use zeroize::Zeroizing;

use crate::cipher;
use crate::credentials::NavCredentials;
use crate::error::NavTransportError;
use crate::soap;
use crate::NavTransport;

use super::{find_first_text, parse_nav_fault, parse_result_block, NavResultBlock};

/// Successful tokenExchange outcome. The token IS the secret the caller
/// will include in the next modifying request; the verbatim bytes go to
/// the audit-ledger per ADR-0009 §8.
#[derive(Debug)]
pub struct TokenExchangeOutcome {
    /// Decrypted, UTF-8-decoded exchange token, in a `Zeroizing` wrapper
    /// so the buffer is overwritten on drop. The caller passes the
    /// `&str` form to `crate::soap::render_manage_invoice_request`.
    pub decoded_token: Zeroizing<String>,

    /// The exact bytes ABERP POSTed to NAV. Owned by the caller and
    /// written verbatim into the audit-ledger
    /// `InvoiceSubmissionAttemptPayload.request_xml` per ADR-0009 §8.
    pub request_xml: Vec<u8>,

    /// The exact bytes NAV returned. Owned by the caller and written
    /// verbatim into the audit-ledger
    /// `InvoiceSubmissionAttemptPayload.response_xml` per ADR-0009 §8
    /// — tokenExchange's request/response pair is one entry, not two,
    /// because the operation is conceptually one round-trip.
    pub response_xml: Vec<u8>,
}

/// Call `tokenExchange` against `transport`. Async because reqwest's
/// async client is the recommended one for hold-the-config-and-go usage;
/// the binary path runs inside a tokio runtime opened in `main.rs`
/// (PR-7-B-2 wires this).
///
/// `tax_number_8` is the 8-digit base of the tenant's tax number per
/// ADR-0009 §4. The caller is responsible for extracting it from the
/// dashed full form (`12345678-1-42` → `"12345678"`); passing the
/// dashed form here produces `INVALID_SECURITY_USER` from NAV.
pub async fn call(
    transport: &NavTransport,
    credentials: &NavCredentials,
    tax_number_8: &str,
) -> Result<TokenExchangeOutcome, NavTransportError> {
    let request_id = soap::parts::new_request_id();
    let request_timestamp = soap::parts::request_timestamp(time::OffsetDateTime::now_utc())?;

    let request_xml = soap::render_token_exchange_request(
        credentials,
        tax_number_8,
        &request_id,
        &request_timestamp,
    )?;

    let url = format!("{}tokenExchange", transport.endpoint().base_url());

    // Session-83 / PR-63 — log the resolved POST URL at `info!` so
    // operators can confirm at a glance which NAV environment ABERP
    // is hitting. The session-82 retry's `INVALID_REQUEST_SIGNATURE`
    // raised the hypothesis that a refactor since PR-57 / session-77
    // might have silently dropped the Test-vs-Production selector, in
    // which case test creds would be signing requests bound for the
    // production endpoint (which would reject the signature without
    // disclosing the env-mismatch root cause). The URL is NOT secret —
    // it is one of two static constants in `endpoint::NavEndpoint`,
    // both already in the source code and on the wire. Logged here
    // (HTTP-call site) rather than in `signatures.rs` because the
    // signature function is pure and does not know about transport;
    // the operations layer is where the URL is constructed. Emitted
    // BEFORE `.post(&url)` so the line surfaces even if the POST
    // itself hangs or errors at the network layer.
    tracing::info!(
        target: "aberp_nav_transport::operations::token_exchange",
        nav_endpoint_url = %url,
        nav_environment = ?transport.endpoint(),
        "POSTing tokenExchange"
    );

    let response = transport
        .client()
        .post(&url)
        .header("Content-Type", "application/xml")
        .header("Accept", "application/xml")
        .body(request_xml.clone())
        .send()
        .await
        .map_err(NavTransportError::TokenExchangeHttp)?;

    let status = response.status();
    let response_xml = response
        .bytes()
        .await
        .map_err(NavTransportError::TokenExchangeHttp)?
        .to_vec();

    if !status.is_success() {
        // Loud-fail on non-success status. PR-58 / session-78 — pre-PR-58
        // this dropped the response body and only carried the HTTP
        // status code, which made every NAV 400 indistinguishable. We
        // now best-effort-parse the body for a NAV fault shape
        // (`<errorCode>` + `<message>` OR SOAP `<faultcode>` +
        // `<faultstring>`) and carry both the parsed pair AND a
        // body preview on the error variant. PR-59 / session-79 —
        // also carry the per-rule `<technicalValidationMessages>`
        // array, which is where NAV's actual reject reason lives for
        // a 400 (the top-level `<errorCode>` is just `INVALID_REQUEST`
        // wrapper). The verbatim bytes are NOT lost — the caller still
        // receives them separately on its audit-payload path (a future
        // audit amendment may attach the response_xml even on the
        // tokenExchange failure path; out of scope for PR-59).
        let fault = parse_nav_fault(&response_xml);
        return Err(NavTransportError::TokenExchangeHttpStatus {
            status: status.as_u16(),
            fault_code: fault.fault_code,
            fault_message: fault.fault_message,
            technical_validations: fault.technical_validations,
            body_preview: fault.body_preview,
        });
    }

    // Parse the <common:result> block. tokenExchange failures here are
    // operator-actionable (per ADR-0009 §4 "Auth failures are not
    // retried"); we surface them as parse-failures with the NAV code in
    // the diagnostic, which the caller logs and the operator triages.
    match parse_result_block(&response_xml, NavTransportError::TokenExchangeResponseParse)? {
        NavResultBlock::Ok => {}
        NavResultBlock::Error { code, message } => {
            return Err(NavTransportError::TokenExchangeResponseParse(format!(
                "NAV returned funcCode=ERROR: {code} — {message}"
            )));
        }
    }

    let encoded = find_first_text(&response_xml, "encodedExchangeToken")?.ok_or_else(|| {
        NavTransportError::TokenExchangeResponseParse(
            "OK response missing <encodedExchangeToken>".to_string(),
        )
    })?;

    let ciphertext = BASE64_STANDARD
        .decode(encoded.as_bytes())
        .map_err(|e| NavTransportError::TokenExchangeBase64Decode(e.to_string()))?;

    let plaintext_bytes =
        cipher::decrypt_exchange_token(credentials.change_key_bytes(), &ciphertext)?;

    let decoded_token = String::from_utf8(plaintext_bytes).map_err(|e| {
        NavTransportError::TokenExchangeDecryptFailed(format!(
            "decrypted token is not valid UTF-8: {e}"
        ))
    })?;

    Ok(TokenExchangeOutcome {
        decoded_token: Zeroizing::new(decoded_token),
        request_xml,
        response_xml,
    })
}
