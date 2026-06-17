//! S441 / ADR-0087 — the `audit_ledger_anchors` table + qualified-timestamp
//! anchoring operations.
//!
//! An anchor row is an append-only record that, at a moment in a session's
//! life (login / heartbeat / logout / service-open / service-close /
//! service-endorse), a qualified timestamp was taken over the chain head.
//! The token is the eIDAS Art. 41(2) anchor.
//!
//! # No PRIMARY KEY (duckdb#23046 — mirrors `audit_ledger`)
//!
//! Like `audit_ledger` (see `storage::schema`), this table carries NO
//! `PRIMARY KEY` / `UNIQUE` — those are the ART secondary indexes DuckDB
//! 1.5.x corrupts on file-backed insert. `id` is a ULID minted in Rust;
//! uniqueness is by construction, integrity by the timestamp tokens
//! themselves, not a DB constraint (`[[no-sql-specific]]`).
//!
//! # Open-session detection is derived, not a column
//!
//! ADR-0087's crash-recovery scan ("an open session with no clean logout")
//! is computed from anchor *kinds* — a `session_id` with an Open anchor but
//! no matching Close anchor — not from a `closed_at` column. Append-only +
//! derived state keeps the invariant in code.

use duckdb::{params, Connection};

use crate::error::AppendError;
use crate::session::tsa::{TimestampAuthority, TimestampToken, TsaError};

/// `CREATE TABLE IF NOT EXISTS` for the anchors table. No PK/UNIQUE — see
/// module docs.
pub const CREATE_ANCHORS_TABLE: &str = "
CREATE TABLE IF NOT EXISTS audit_ledger_anchors (
    id                        VARCHAR NOT NULL,
    tenant_id                 VARCHAR NOT NULL,
    session_id                VARCHAR NOT NULL,
    kind                      VARCHAR NOT NULL,
    chain_head_hash_at_anchor VARCHAR NOT NULL,
    timestamp_token_bytes     BLOB,
    tsa_identifier            VARCHAR NOT NULL,
    tsa_status                VARCHAR NOT NULL,
    created_at_utc            VARCHAR NOT NULL
);
";

const INSERT_ANCHOR: &str = "
INSERT INTO audit_ledger_anchors
    (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
     timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?);
";

const SELECT_ANCHORS_FOR_TENANT: &str = "
SELECT id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
       timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc
FROM audit_ledger_anchors
WHERE tenant_id = ?
ORDER BY created_at_utc ASC, id ASC;
";

/// The lifecycle moment an anchor records. Serialized as the brief's
/// enum-as-string set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorKind {
    /// Operator login — binds DÁP identity to the session key (ADR-0086/0087).
    LoginOpen,
    /// Periodic checkpoint over the current chain head (every 15 min).
    Heartbeat,
    /// Clean operator logout / graceful shutdown.
    LogoutClose,
    /// Service session opened at binary startup (ADR-0088).
    ServiceOpen,
    /// Service session closed at graceful shutdown (ADR-0088).
    ServiceClose,
    /// First operator login endorses the service key (ADR-0088).
    ServiceEndorse,
}

impl AnchorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AnchorKind::LoginOpen => "LoginOpen",
            AnchorKind::Heartbeat => "Heartbeat",
            AnchorKind::LogoutClose => "LogoutClose",
            AnchorKind::ServiceOpen => "ServiceOpen",
            AnchorKind::ServiceClose => "ServiceClose",
            AnchorKind::ServiceEndorse => "ServiceEndorse",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "LoginOpen" => AnchorKind::LoginOpen,
            "Heartbeat" => AnchorKind::Heartbeat,
            "LogoutClose" => AnchorKind::LogoutClose,
            "ServiceOpen" => AnchorKind::ServiceOpen,
            "ServiceClose" => AnchorKind::ServiceClose,
            "ServiceEndorse" => AnchorKind::ServiceEndorse,
            _ => return None,
        })
    }

    /// Opens a session (no preceding matching close needed).
    fn is_open(self) -> bool {
        matches!(self, AnchorKind::LoginOpen | AnchorKind::ServiceOpen)
    }

    /// Closes a session cleanly.
    fn is_close(self) -> bool {
        matches!(self, AnchorKind::LogoutClose | AnchorKind::ServiceClose)
    }
}

/// Whether a token landed or is queued (TSA outage). `Failed` is reserved
/// for a token that was rejected by the authority; the never-block path
/// uses `Pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsaStatus {
    Anchored,
    Pending,
    Failed,
}

impl TsaStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TsaStatus::Anchored => "anchored",
            TsaStatus::Pending => "pending",
            TsaStatus::Failed => "failed",
        }
    }
    fn from_token(s: &str) -> Self {
        match s {
            "anchored" => TsaStatus::Anchored,
            "failed" => TsaStatus::Failed,
            _ => TsaStatus::Pending,
        }
    }
}

/// One row of `audit_ledger_anchors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub id: String,
    pub tenant_id: String,
    pub session_id: String,
    pub kind: AnchorKind,
    pub chain_head_hash_at_anchor: String,
    pub timestamp_token_bytes: Option<Vec<u8>>,
    pub tsa_identifier: String,
    pub tsa_status: TsaStatus,
    pub created_at_utc: String,
}

/// The exact bytes a token covers for an anchor — reconstructed from the
/// stored columns at verify time, so no raw payload blob is persisted
/// (the columns already commit to it). Deterministic concatenation.
pub fn anchor_preimage(
    kind: AnchorKind,
    tenant_id: &str,
    session_id: &str,
    chain_head_hash_hex: &str,
    created_at_utc: &str,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(kind.as_str().as_bytes());
    out.push(b'|');
    out.extend_from_slice(tenant_id.as_bytes());
    out.push(b'|');
    out.extend_from_slice(session_id.as_bytes());
    out.push(b'|');
    out.extend_from_slice(chain_head_hash_hex.as_bytes());
    out.push(b'|');
    out.extend_from_slice(created_at_utc.as_bytes());
    out
}

/// Take an anchor: timestamp the chain head via `tsa`, persist the row.
///
/// **Never blocks the audit chain on the TSA** (ADR-0087 §"Heartbeat
/// failure"). A [`TsaError::Network`] failure persists a `pending` row
/// (token NULL) and returns it — the caller emits `TimestampAnchorDelayed`
/// and retries later; it does NOT propagate the error into an append path.
/// A non-network TSA error (e.g. a real rejection) is surfaced.
pub fn take_anchor(
    conn: &Connection,
    tsa: &dyn TimestampAuthority,
    tenant_id: &str,
    session_id: &str,
    kind: AnchorKind,
    chain_head_hash_hex: &str,
) -> Result<Anchor, AppendError> {
    let created_at_utc = crate::session::tsa::now_rfc3339();
    let preimage = anchor_preimage(
        kind,
        tenant_id,
        session_id,
        chain_head_hash_hex,
        &created_at_utc,
    );

    let (token_bytes, status): (Option<Vec<u8>>, TsaStatus) = match tsa.timestamp(&preimage) {
        Ok(TimestampToken { bytes, .. }) => (Some(bytes), TsaStatus::Anchored),
        Err(TsaError::Network(msg)) => {
            tracing::warn!(
                tenant = tenant_id,
                session = session_id,
                kind = kind.as_str(),
                error = %msg,
                "qualified timestamp unavailable — anchor queued pending, audit chain unaffected"
            );
            (None, TsaStatus::Pending)
        }
        Err(other) => return Err(AppendError::Tsa(other.to_string())),
    };

    let anchor = Anchor {
        id: format!("anc_{}", ulid::Ulid::new()),
        tenant_id: tenant_id.to_string(),
        session_id: session_id.to_string(),
        kind,
        chain_head_hash_at_anchor: chain_head_hash_hex.to_string(),
        timestamp_token_bytes: token_bytes,
        tsa_identifier: tsa.identifier().to_string(),
        tsa_status: status,
        created_at_utc,
    };
    insert_anchor(conn, &anchor)?;
    Ok(anchor)
}

fn insert_anchor(conn: &Connection, a: &Anchor) -> Result<(), AppendError> {
    let inserted = conn.execute(
        INSERT_ANCHOR,
        params![
            a.id,
            a.tenant_id,
            a.session_id,
            a.kind.as_str(),
            a.chain_head_hash_at_anchor,
            a.timestamp_token_bytes.as_deref(),
            a.tsa_identifier,
            a.tsa_status.as_str(),
            a.created_at_utc,
        ],
    )?;
    if inserted != 1 {
        return Err(AppendError::Anchor(format!(
            "anchor insert affected {inserted} rows (expected 1)"
        )));
    }
    Ok(())
}

/// Read every anchor for a tenant in `created_at` order.
pub fn anchors_for_tenant(conn: &Connection, tenant_id: &str) -> Result<Vec<Anchor>, AppendError> {
    let mut stmt = conn.prepare(SELECT_ANCHORS_FOR_TENANT)?;
    let rows = stmt.query_map(params![tenant_id], row_to_anchor)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Session ids that have an Open anchor but no Close anchor — the orphan
/// sessions ADR-0087 crash-recovery closes on next boot. Derived in Rust
/// from anchor kinds (`[[no-sql-specific]]`).
pub fn open_sessions_without_close(
    conn: &Connection,
    tenant_id: &str,
) -> Result<Vec<String>, AppendError> {
    let anchors = anchors_for_tenant(conn, tenant_id)?;
    let mut opened: Vec<String> = Vec::new();
    let mut closed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for a in &anchors {
        if a.kind.is_open() && !opened.contains(&a.session_id) {
            opened.push(a.session_id.clone());
        }
        if a.kind.is_close() {
            closed.insert(a.session_id.clone());
        }
    }
    Ok(opened.into_iter().filter(|s| !closed.contains(s)).collect())
}

fn row_to_anchor(row: &duckdb::Row<'_>) -> duckdb::Result<Anchor> {
    let kind_str: String = row.get(3)?;
    let status_str: String = row.get(7)?;
    Ok(Anchor {
        id: row.get(0)?,
        tenant_id: row.get(1)?,
        session_id: row.get(2)?,
        kind: AnchorKind::from_token(&kind_str).ok_or_else(|| {
            duckdb::Error::FromSqlConversionFailure(
                3,
                duckdb::types::Type::Text,
                Box::<dyn std::error::Error + Send + Sync>::from(format!(
                    "unknown anchor kind {kind_str:?}"
                )),
            )
        })?,
        chain_head_hash_at_anchor: row.get(4)?,
        timestamp_token_bytes: row.get(5)?,
        tsa_identifier: row.get(6)?,
        tsa_status: TsaStatus::from_token(&status_str),
        created_at_utc: row.get(8)?,
    })
}
