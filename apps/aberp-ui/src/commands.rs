//! `#[tauri::command]` surface — the four read-only routes the SPA
//! consumes. Each forwards to the loopback `aberp serve` listener
//! with the bearer header attached and the response body relayed as
//! `serde_json::Value` (so the SPA can render arbitrary JSON without
//! a separate DTO layer per ADR-0021 §Part B).
//!
//! Errors are stringified at the boundary: Tauri commands serialise
//! to JSON, and `anyhow::Error` is not Serialize. Loud-fail wording
//! is preserved verbatim — the SPA renders the message in a banner
//! per rule 12.

use anyhow::Context;
use serde_json::Value;
use tauri::State;

use crate::AppState;

/// `GET /health` — unauthenticated on the backend, but we still
/// route it through the same pinned client so the SPA never bypasses
/// the trust boundary.
#[tauri::command]
pub async fn health(state: State<'_, AppState>) -> Result<Value, String> {
    forward_get(&state, "/health", false).await
}

/// `GET /invoices` — authenticated; returns the list shape derived
/// per ADR-0009 §2.
#[tauri::command]
pub async fn list_invoices(state: State<'_, AppState>) -> Result<Value, String> {
    forward_get(&state, "/invoices", true).await
}

/// `GET /invoices/<id>` — authenticated; returns the single-invoice
/// detail plus its full audit-ledger trail.
#[tauri::command]
pub async fn get_invoice(state: State<'_, AppState>, invoice_id: String) -> Result<Value, String> {
    validate_invoice_id(&invoice_id).map_err(|e| format!("{e:#}"))?;
    let path = format!("/invoices/{invoice_id}");
    forward_get(&state, &path, true).await
}

/// `GET /audit/<invoice_id>` — authenticated; the evidence-bundle
/// drill-down per ADR-0009 §8.
#[tauri::command]
pub async fn get_audit(state: State<'_, AppState>, invoice_id: String) -> Result<Value, String> {
    validate_invoice_id(&invoice_id).map_err(|e| format!("{e:#}"))?;
    let path = format!("/audit/{invoice_id}");
    forward_get(&state, &path, true).await
}

/// PR-44ε.UI / session-58 — `GET /invoices/<id>/pdf`; returns the
/// raw PDF bytes for the SPA's "Download PDF" button on the invoice
/// detail modal. Unlike the other commands here, the response body
/// is binary (`application/pdf`), not JSON; the bytes are relayed to
/// the SPA as a `Vec<u8>` and the SPA re-wraps them in a `Blob` for
/// the browser-side download trigger.
#[tauri::command]
pub async fn download_invoice_pdf(
    state: State<'_, AppState>,
    invoice_id: String,
) -> Result<Vec<u8>, String> {
    validate_invoice_id(&invoice_id).map_err(|e| format!("{e:#}"))?;
    let path = format!("/invoices/{invoice_id}/pdf");
    forward_get_bytes(&state, &path).await
}

/// PR-44ζ / session-59 — `POST /invoices/issue`; the SPA's "+ New
/// Invoice" form posts the composed body here. The body is forwarded
/// verbatim — the typed shape lives on the backend's
/// `IssueInvoiceRequest` and on the SPA's `composeIssueInvoiceBody`
/// composer (`issue-invoice.ts`); this command is the pass-through
/// seam.
///
/// Returns the backend's typed response body
/// (`{invoice_id, invoice_number, state}`); the SPA navigates the
/// detail modal open on the returned `invoice_id`.
#[tauri::command]
pub async fn issue_invoice(
    state: State<'_, AppState>,
    body: Value,
) -> Result<Value, String> {
    forward_post(&state, "/invoices/issue", body).await
}

/// PR-44η / session-60 — `POST /invoices/<id>/submit`; the SPA's
/// "Submit to NAV" button on the invoice-detail modal posts here.
/// No body — the backend resolves the on-disk NAV XML + supplier
/// tax number from the audit ledger server-side per A162.
///
/// Returns the backend's typed response body (`{invoice_id,
/// transaction_id, state, entries_verified}`). On precondition
/// mismatch (invoice not in `Ready`) the backend returns 409; the
/// SPA renders the typed error body inline per A157.
#[tauri::command]
pub async fn submit_invoice_to_nav(
    state: State<'_, AppState>,
    invoice_id: String,
) -> Result<Value, String> {
    validate_invoice_id(&invoice_id).map_err(|e| format!("{e:#}"))?;
    let path = format!("/invoices/{invoice_id}/submit");
    forward_post(&state, &path, Value::Null).await
}

/// PR-44η / session-60 — `POST /invoices/<id>/poll-ack`; the SPA's
/// "Poll ack now" button on the invoice-detail modal posts here.
/// No body — the backend resolves the NAV transactionId from the
/// audit ledger server-side per the same posture as the CLI's
/// `aberp poll-ack`.
///
/// Returns the backend's typed response body (`{invoice_id, state,
/// attempts_made, transaction_id, diagnostic, entries_verified}`).
/// On precondition mismatch (invoice not in `Submitted` or
/// `PendingNavExists`) the backend returns 409; the SPA renders the
/// typed error body inline per A157.
#[tauri::command]
pub async fn poll_ack(
    state: State<'_, AppState>,
    invoice_id: String,
) -> Result<Value, String> {
    validate_invoice_id(&invoice_id).map_err(|e| format!("{e:#}"))?;
    let path = format!("/invoices/{invoice_id}/poll-ack");
    forward_post(&state, &path, Value::Null).await
}

/// Single point of contact with the backend. Locks the backend
/// mutex briefly to grab `url + token + client` and releases before
/// the HTTP roundtrip so command latency doesn't serialise across
/// the shell.
async fn forward_get(
    state: &State<'_, AppState>,
    path: &str,
    authenticated: bool,
) -> Result<Value, String> {
    let (url, token, client) = {
        let guard = state.backend.lock().await;
        let backend = guard
            .as_ref()
            .ok_or_else(|| "backend not ready yet — wait a moment and retry".to_string())?;
        (
            format!("{}{}", backend.url, path),
            backend.session_token.clone(),
            backend.client.clone(),
        )
    };

    let mut req = client.get(&url);
    if authenticated {
        req = req.bearer_auth(&token);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTPS GET {url}"))
        .map_err(|e| format!("{e:#}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .with_context(|| format!("read body of {url}"))
        .map_err(|e| format!("{e:#}"))?;

    if !status.is_success() {
        return Err(format!("backend returned {status} for {path}: {body}"));
    }
    let value: Value = serde_json::from_str(&body)
        .with_context(|| format!("parse JSON body of {url}: `{body}`"))
        .map_err(|e| format!("{e:#}"))?;
    Ok(value)
}

/// PR-44ε.UI / session-58 — binary-body sibling of [`forward_get`].
///
/// The four pre-existing routes return JSON; the new
/// `/invoices/<id>/pdf` route returns `application/pdf` bytes. JSON
/// decoding is wrong for those bytes — a `serde_json::from_str` on a
/// PDF would always fail at the first non-JSON byte. This helper
/// reads the response as raw bytes and surfaces non-2xx as an error
/// string (matching the JSON path's posture).
async fn forward_get_bytes(state: &State<'_, AppState>, path: &str) -> Result<Vec<u8>, String> {
    let (url, token, client) = {
        let guard = state.backend.lock().await;
        let backend = guard
            .as_ref()
            .ok_or_else(|| "backend not ready yet — wait a moment and retry".to_string())?;
        (
            format!("{}{}", backend.url, path),
            backend.session_token.clone(),
            backend.client.clone(),
        )
    };

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("HTTPS GET {url}"))
        .map_err(|e| format!("{e:#}"))?;

    let status = resp.status();
    if !status.is_success() {
        // Try to surface the backend error JSON body if present so the
        // SPA renders the loud-fail message; falls back to "<no body>"
        // on a read failure rather than swallowing it silently.
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "<no body>".to_string());
        return Err(format!("backend returned {status} for {path}: {body}"));
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("read bytes of {url}"))
        .map_err(|e| format!("{e:#}"))?;
    Ok(bytes.to_vec())
}

/// PR-44ζ / session-59 — POST sibling of [`forward_get`]. Sends
/// `body` as the request's JSON body; surfaces the backend's typed
/// 4xx error message verbatim to the SPA (so the inline-error pane
/// renders the actionable "customer name is required" rather than an
/// opaque "internal error"). The four pre-existing JSON routes are
/// all GETs; this is the first POST seam — kept narrow, no shared
/// helper with `forward_get` because the body + method differ at the
/// `RequestBuilder` layer.
async fn forward_post(
    state: &State<'_, AppState>,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    let (url, token, client) = {
        let guard = state.backend.lock().await;
        let backend = guard
            .as_ref()
            .ok_or_else(|| "backend not ready yet — wait a moment and retry".to_string())?;
        (
            format!("{}{}", backend.url, path),
            backend.session_token.clone(),
            backend.client.clone(),
        )
    };

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("HTTPS POST {url}"))
        .map_err(|e| format!("{e:#}"))?;

    let status = resp.status();
    let response_body = resp
        .text()
        .await
        .with_context(|| format!("read body of {url}"))
        .map_err(|e| format!("{e:#}"))?;

    if !status.is_success() {
        // Surface the backend's typed `{ "error": "..." }` body verbatim
        // so the SPA can render the operator-actionable message inline.
        // A non-JSON body (rare) falls through as the raw text.
        return Err(format!("backend returned {status} for {path}: {response_body}"));
    }
    let value: Value = serde_json::from_str(&response_body)
        .with_context(|| format!("parse JSON body of {url}: `{response_body}`"))
        .map_err(|e| format!("{e:#}"))?;
    Ok(value)
}

/// Reject obviously malformed invoice ids before they reach the
/// backend. The backend itself has its own (looser) parsing — this
/// is defence in depth against a path-injection attempt from a
/// compromised SPA build (per the ADR-0004 §Adversarial-review
/// "semi-trusted frontend" framing).
fn validate_invoice_id(s: &str) -> anyhow::Result<()> {
    if s.is_empty() {
        anyhow::bail!("invoice_id is empty");
    }
    if s.len() > 64 {
        anyhow::bail!("invoice_id length {} exceeds 64", s.len());
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        anyhow::bail!("invoice_id `{s}` contains characters outside [A-Za-z0-9_-]");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_invoice_id_accepts_typical_prefixed_ulid() {
        // inv_<26-char Crockford-base32 ULID> is the standard shape.
        assert!(validate_invoice_id("inv_01ARZ3NDEKTSV4RRFFQ69G5FAV").is_ok());
    }

    #[test]
    fn validate_invoice_id_rejects_empty() {
        assert!(validate_invoice_id("").is_err());
    }

    #[test]
    fn validate_invoice_id_rejects_path_traversal() {
        assert!(validate_invoice_id("../etc/passwd").is_err());
        assert!(validate_invoice_id("inv/foo").is_err());
    }

    #[test]
    fn validate_invoice_id_rejects_url_metacharacters() {
        assert!(validate_invoice_id("inv?id=1").is_err());
        assert!(validate_invoice_id("inv#frag").is_err());
        assert!(validate_invoice_id("inv 01").is_err());
    }

    #[test]
    fn validate_invoice_id_rejects_overlong() {
        let s = "a".repeat(65);
        assert!(validate_invoice_id(&s).is_err());
    }
}
