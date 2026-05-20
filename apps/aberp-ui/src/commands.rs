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
