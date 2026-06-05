//! `quote_intake_log` — staging table for fetched approved quotes.
//!
//! No CHECK constraints (per [[no-sql-specific]]); the `PRIMARY KEY`
//! on `quote_id` is the idempotency anchor.
//!
//! Dev nuke recipe: `DROP TABLE quote_intake_log;` — see crate
//! README. NEVER on prod (loses operator pickup queue).

use duckdb::{params, Connection};
use time::OffsetDateTime;

use crate::error::QuoteIntakeError;

pub fn ensure_schema(conn: &Connection) -> Result<(), QuoteIntakeError> {
    conn.execute_batch(SCHEMA_SQL)
        .map_err(|e| QuoteIntakeError::Storage(format!("ensure quote_intake_log schema: {e}")))?;
    // S255 / PR-244 — additive migration for the operator-pickup
    // landing column. Idempotent on a post-S255 boot; fills pre-S255
    // rows with NULL (operator never picked them up — equivalent to
    // the post-S255 "fresh row" state).
    conn.execute_batch(S255_MIGRATION_SQL).map_err(|e| {
        QuoteIntakeError::Storage(format!("apply S255 quote_intake_log migration: {e}"))
    })?;
    // S256 / PR-245 — additive `intake_state` + `intake_error` columns.
    // A malformed quote (mapping failure) is now staged as an
    // `error`-state row instead of being silently dropped (brief §A.4),
    // so the operator sees it in the Quotes tab and can retry-parse or
    // mark-irrelevant. Closed vocab is enforced in the app layer (per
    // [[no-sql-specific]]); the DEFAULT backfills pre-S256 rows to
    // `staged` (every prior row was a successful stage).
    conn.execute_batch(S256_MIGRATION_SQL).map_err(|e| {
        QuoteIntakeError::Storage(format!("apply S256 quote_intake_log migration: {e}"))
    })
}

/// Closed-vocab `intake_state` values. NOT enforced by a DuckDB CHECK
/// (per [[no-sql-specific]]); these constants are the single source of
/// truth the app-layer writers use.
pub const STATE_STAGED: &str = "staged";
pub const STATE_ERROR: &str = "error";
pub const STATE_IRRELEVANT: &str = "irrelevant";

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS quote_intake_log (
    quote_id              VARCHAR NOT NULL PRIMARY KEY,
    tenant_id             VARCHAR NOT NULL,
    invoice_id            VARCHAR NOT NULL,
    received_at           VARCHAR NOT NULL,
    intake_at             VARCHAR NOT NULL,
    status_writeback_at   VARCHAR,
    raw_payload           VARCHAR NOT NULL,
    prepared_draft        VARCHAR NOT NULL
);
CREATE INDEX IF NOT EXISTS quote_intake_log_pending_writeback_idx
    ON quote_intake_log (tenant_id, status_writeback_at);
";

/// S255 / PR-244 — `picked_up_drf_id` records the `drf_<ULID>` of the
/// invoice_draft minted when the operator clicked "Create draft
/// invoice" on this quote. NULL means "operator has not picked up
/// this quote yet" — the SPA renders the pickup button; a non-NULL
/// renders the "→ Draft #N" link instead. A re-pickup after S239
/// deletes the prior draft is allowed: the route writes the new
/// `drf_<ULID>` here, overwriting the now-orphaned id. (Idempotency
/// within a single pickup attempt rides on the audit-ledger F8 gate;
/// this column is the operator-facing tag, not the dedup key.)
const S255_MIGRATION_SQL: &str = "
ALTER TABLE quote_intake_log
    ADD COLUMN IF NOT EXISTS picked_up_drf_id VARCHAR;
";

/// S256 / PR-245 — `intake_state` (closed vocab: `staged` / `error` /
/// `irrelevant`) + `intake_error` (operator-readable message for
/// `error`-state rows). The `DEFAULT 'staged'` backfills every pre-S256
/// row, all of which were successful stages.
const S256_MIGRATION_SQL: &str = "
ALTER TABLE quote_intake_log
    ADD COLUMN IF NOT EXISTS intake_state VARCHAR DEFAULT 'staged';
ALTER TABLE quote_intake_log
    ADD COLUMN IF NOT EXISTS intake_error VARCHAR;
";

pub fn already_intook(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
) -> Result<Option<String>, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare("SELECT invoice_id FROM quote_intake_log WHERE quote_id = ?1 AND tenant_id = ?2")
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare already_intook: {e}")))?;
    let mut rows = stmt
        .query(params![quote_id, tenant_id])
        .map_err(|e| QuoteIntakeError::Storage(format!("query already_intook: {e}")))?;
    if let Some(row) = rows
        .next()
        .map_err(|e| QuoteIntakeError::Storage(format!("read already_intook row: {e}")))?
    {
        let invoice_id: String = row
            .get(0)
            .map_err(|e| QuoteIntakeError::Storage(format!("get invoice_id col: {e}")))?;
        Ok(Some(invoice_id))
    } else {
        Ok(None)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn insert_intake(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
    invoice_id: &str,
    received_at: &str,
    intake_at: OffsetDateTime,
    raw_payload_json: &str,
    prepared_draft_json: &str,
) -> Result<(), QuoteIntakeError> {
    ensure_schema(conn)?;
    let intake_at_iso = format_iso(intake_at)?;
    conn.execute(
        "INSERT INTO quote_intake_log (
             quote_id, tenant_id, invoice_id,
             received_at, intake_at,
             status_writeback_at,
             raw_payload, prepared_draft,
             intake_state, intake_error
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, NULL)",
        params![
            quote_id,
            tenant_id,
            invoice_id,
            received_at,
            intake_at_iso,
            raw_payload_json,
            prepared_draft_json,
            STATE_STAGED,
        ],
    )
    .map_err(|e| QuoteIntakeError::Storage(format!("insert quote_intake_log row: {e}")))?;
    Ok(())
}

/// S256 / PR-245 — stage a quote whose mapping FAILED as an
/// `error`-state row instead of silently dropping it (brief §A.4). The
/// raw payload is preserved verbatim so the operator's retry-parse can
/// re-run the mapping against it; `invoice_id` and `prepared_draft` are
/// placeholders until a successful retry fills them via
/// [`retry_parse_intake`]. Idempotency rides the `quote_id` PRIMARY KEY:
/// a second poll cycle's `already_intook` check sees the error row and
/// skips re-insert.
pub fn insert_error_intake(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
    received_at: &str,
    intake_at: OffsetDateTime,
    raw_payload_json: &str,
    error_message: &str,
) -> Result<(), QuoteIntakeError> {
    ensure_schema(conn)?;
    let intake_at_iso = format_iso(intake_at)?;
    conn.execute(
        "INSERT INTO quote_intake_log (
             quote_id, tenant_id, invoice_id,
             received_at, intake_at,
             status_writeback_at,
             raw_payload, prepared_draft,
             intake_state, intake_error
         ) VALUES (?1, ?2, '', ?3, ?4, NULL, ?5, '{}', ?6, ?7)",
        params![
            quote_id,
            tenant_id,
            received_at,
            intake_at_iso,
            raw_payload_json,
            STATE_ERROR,
            error_message,
        ],
    )
    .map_err(|e| QuoteIntakeError::Storage(format!("insert error quote_intake_log row: {e}")))?;
    Ok(())
}

/// S256 / PR-245 — recovery path for an `error`-state row: a successful
/// re-parse fills `invoice_id` + `prepared_draft` and flips the row back
/// to `staged`, clearing `intake_error`. Guarded on
/// `intake_state = 'error'` so it never clobbers a successfully-staged
/// or picked-up row. Returns the number of rows updated (0 = no matching
/// error row, which the route maps to 404 / no-op).
pub fn retry_parse_intake(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
    invoice_id: &str,
    prepared_draft_json: &str,
) -> Result<usize, QuoteIntakeError> {
    ensure_schema(conn)?;
    let n = conn
        .execute(
            "UPDATE quote_intake_log
                SET invoice_id = ?1,
                    prepared_draft = ?2,
                    intake_state = ?3,
                    intake_error = NULL
              WHERE quote_id = ?4 AND tenant_id = ?5 AND intake_state = ?6",
            params![
                invoice_id,
                prepared_draft_json,
                STATE_STAGED,
                quote_id,
                tenant_id,
                STATE_ERROR,
            ],
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("retry-parse update: {e}")))?;
    Ok(n)
}

/// S256 / PR-245 — operator dismisses a row (typically a dead-letter
/// `error` row that will never parse, e.g. a quote the storefront sent
/// malformed). Flips the row to `irrelevant`; it then drops out of the
/// badge count and the pickup surface. Returns rows updated.
pub fn mark_irrelevant(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
) -> Result<usize, QuoteIntakeError> {
    ensure_schema(conn)?;
    let n = conn
        .execute(
            "UPDATE quote_intake_log
                SET intake_state = ?1
              WHERE quote_id = ?2 AND tenant_id = ?3",
            params![STATE_IRRELEVANT, quote_id, tenant_id],
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("mark-irrelevant update: {e}")))?;
    Ok(n)
}

/// S256 / PR-245 — the SPA sidebar/tab badge count: un-picked-up quotes
/// that are still actionable (`staged`, not yet picked up). `error` and
/// `irrelevant` rows are excluded — an error row isn't pickable, and an
/// irrelevant row was dismissed. Recomputed from DB on every call so the
/// badge survives an app restart (adversarial-review note: don't trust
/// an in-memory counter).
pub fn count_unpicked(conn: &Connection, tenant_id: &str) -> Result<u64, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT count(*) FROM quote_intake_log
              WHERE tenant_id = ?1
                AND intake_state = ?2
                AND picked_up_drf_id IS NULL",
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare count_unpicked: {e}")))?;
    let n: i64 = stmt
        .query_row(params![tenant_id, STATE_STAGED], |row| row.get(0))
        .map_err(|e| QuoteIntakeError::Storage(format!("query count_unpicked: {e}")))?;
    Ok(n.max(0) as u64)
}

/// S256 / PR-245 — count of `error`-state rows for a tenant (surfaced
/// to the SPA so the operator knows there are dead-letter rows to
/// triage even when none are pickable).
pub fn count_errored(conn: &Connection, tenant_id: &str) -> Result<u64, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT count(*) FROM quote_intake_log
              WHERE tenant_id = ?1 AND intake_state = ?2",
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare count_errored: {e}")))?;
    let n: i64 = stmt
        .query_row(params![tenant_id, STATE_ERROR], |row| row.get(0))
        .map_err(|e| QuoteIntakeError::Storage(format!("query count_errored: {e}")))?;
    Ok(n.max(0) as u64)
}

/// S256 / PR-245 — the set of `quote_id`s that are currently staged AND
/// un-picked-up. The notifications route intersects this with the
/// `QuoteIntakeRowAdded` audit entries past the catch-up boundary to
/// compute live toast arrivals (belt-and-suspenders cross-check so an
/// already-picked-up quote never replays a toast — brief §B.8).
pub fn list_unpicked_quote_ids(
    conn: &Connection,
    tenant_id: &str,
) -> Result<Vec<String>, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT quote_id FROM quote_intake_log
              WHERE tenant_id = ?1
                AND intake_state = ?2
                AND picked_up_drf_id IS NULL",
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare list_unpicked_quote_ids: {e}")))?;
    let mut rows = stmt
        .query(params![tenant_id, STATE_STAGED])
        .map_err(|e| QuoteIntakeError::Storage(format!("query list_unpicked_quote_ids: {e}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| QuoteIntakeError::Storage(format!("read unpicked row: {e}")))?
    {
        out.push(
            row.get::<_, String>(0)
                .map_err(|e| QuoteIntakeError::Storage(format!("get quote_id: {e}")))?,
        );
    }
    Ok(out)
}

/// S256 / PR-245 — read the stored raw payload + current state for a
/// quote (used by the retry-parse route to re-run the mapping against
/// the verbatim stored payload). `Ok(None)` when no row matches.
pub fn read_raw_and_state(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
) -> Result<Option<(String, String)>, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT raw_payload, COALESCE(intake_state, ?3)
               FROM quote_intake_log
              WHERE quote_id = ?1 AND tenant_id = ?2
              LIMIT 1",
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare read_raw_and_state: {e}")))?;
    let mut rows = stmt
        .query(params![quote_id, tenant_id, STATE_STAGED])
        .map_err(|e| QuoteIntakeError::Storage(format!("query read_raw_and_state: {e}")))?;
    let Some(row) = rows
        .next()
        .map_err(|e| QuoteIntakeError::Storage(format!("read read_raw_and_state row: {e}")))?
    else {
        return Ok(None);
    };
    let raw: String = row
        .get(0)
        .map_err(|e| QuoteIntakeError::Storage(format!("get raw_payload: {e}")))?;
    let state: String = row
        .get(1)
        .map_err(|e| QuoteIntakeError::Storage(format!("get intake_state: {e}")))?;
    Ok(Some((raw, state)))
}

pub fn mark_writeback_complete(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
    when: OffsetDateTime,
) -> Result<(), QuoteIntakeError> {
    ensure_schema(conn)?;
    let when_iso = format_iso(when)?;
    conn.execute(
        "UPDATE quote_intake_log
            SET status_writeback_at = ?1
          WHERE quote_id = ?2 AND tenant_id = ?3",
        params![when_iso, quote_id, tenant_id],
    )
    .map_err(|e| QuoteIntakeError::Storage(format!("update writeback timestamp: {e}")))?;
    Ok(())
}

/// S255 / PR-244 — fetch the raw row needed by the operator-pickup
/// route: the prepared-draft JSON, the contact slice (for the SPA's
/// "creating new partner" confirm modal copy), and the existing
/// `picked_up_drf_id` (which the route's idempotency walk reads).
///
/// Returns `Ok(None)` if no row matches the `(tenant, quote_id)` —
/// the route maps this to 404.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickupSourceRow {
    pub raw_payload: String,
    pub prepared_draft: String,
    pub picked_up_drf_id: Option<String>,
}

pub fn read_for_pickup(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
) -> Result<Option<PickupSourceRow>, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT raw_payload, prepared_draft, picked_up_drf_id
               FROM quote_intake_log
              WHERE quote_id = ?1 AND tenant_id = ?2
              LIMIT 1",
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare read_for_pickup: {e}")))?;
    let mut rows = stmt
        .query(params![quote_id, tenant_id])
        .map_err(|e| QuoteIntakeError::Storage(format!("query read_for_pickup: {e}")))?;
    let Some(row) = rows
        .next()
        .map_err(|e| QuoteIntakeError::Storage(format!("read read_for_pickup row: {e}")))?
    else {
        return Ok(None);
    };
    let raw_payload: String = row
        .get(0)
        .map_err(|e| QuoteIntakeError::Storage(format!("get raw_payload col: {e}")))?;
    let prepared_draft: String = row
        .get(1)
        .map_err(|e| QuoteIntakeError::Storage(format!("get prepared_draft col: {e}")))?;
    let picked_up_drf_id: Option<String> = row
        .get(2)
        .map_err(|e| QuoteIntakeError::Storage(format!("get picked_up_drf_id col: {e}")))?;
    Ok(Some(PickupSourceRow {
        raw_payload,
        prepared_draft,
        picked_up_drf_id,
    }))
}

/// S255 / PR-244 — record the operator-minted `drf_<ULID>` on the
/// quote_intake_log row. Overwrites any prior value: a re-pickup
/// after S239 delete is intentional and the column tracks the LATEST
/// pickup, not the historical pickups (the audit ledger does that).
pub fn set_picked_up_drf_id(
    conn: &Connection,
    tenant_id: &str,
    quote_id: &str,
    drf_id: &str,
) -> Result<(), QuoteIntakeError> {
    ensure_schema(conn)?;
    conn.execute(
        "UPDATE quote_intake_log
            SET picked_up_drf_id = ?1
          WHERE quote_id = ?2 AND tenant_id = ?3",
        params![drf_id, quote_id, tenant_id],
    )
    .map_err(|e| QuoteIntakeError::Storage(format!("update picked_up_drf_id: {e}")))?;
    Ok(())
}

pub fn list_pending_writebacks(
    conn: &Connection,
    tenant_id: &str,
) -> Result<Vec<String>, QuoteIntakeError> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT quote_id FROM quote_intake_log
              WHERE tenant_id = ?1 AND status_writeback_at IS NULL",
        )
        .map_err(|e| QuoteIntakeError::Storage(format!("prepare list_pending_writebacks: {e}")))?;
    let mut rows = stmt
        .query(params![tenant_id])
        .map_err(|e| QuoteIntakeError::Storage(format!("query list_pending_writebacks: {e}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| QuoteIntakeError::Storage(format!("read pending row: {e}")))?
    {
        let q: String = row
            .get(0)
            .map_err(|e| QuoteIntakeError::Storage(format!("get quote_id col: {e}")))?;
        out.push(q);
    }
    Ok(out)
}

fn format_iso(ts: OffsetDateTime) -> Result<String, QuoteIntakeError> {
    ts.format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| QuoteIntakeError::Storage(format!("format timestamp: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_mem() -> Connection {
        Connection::open_in_memory().expect("in-memory DuckDB")
    }

    #[test]
    fn ensure_schema_is_idempotent() {
        let conn = open_mem();
        ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
        ensure_schema(&conn).unwrap();
    }

    #[test]
    fn already_intook_returns_none_for_fresh_quote() {
        let conn = open_mem();
        assert!(already_intook(&conn, "t1", "q-1").unwrap().is_none());
    }

    #[test]
    fn insert_then_already_intook_returns_some() {
        let conn = open_mem();
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        insert_intake(
            &conn,
            "t1",
            "q-1",
            "inv_01ABC",
            "2026-01-01T00:00:00Z",
            now,
            "{}",
            "{}",
        )
        .unwrap();
        assert_eq!(
            already_intook(&conn, "t1", "q-1").unwrap(),
            Some("inv_01ABC".to_string())
        );
        assert!(already_intook(&conn, "t2", "q-1").unwrap().is_none());
    }

    #[test]
    fn double_insert_loud_fails() {
        let conn = open_mem();
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        insert_intake(&conn, "t", "q", "inv_A", "r", now, "{}", "{}").unwrap();
        let err = insert_intake(&conn, "t", "q", "inv_B", "r", now, "{}", "{}").unwrap_err();
        assert!(matches!(err, QuoteIntakeError::Storage(_)));
    }

    #[test]
    fn mark_writeback_and_list_pending() {
        let conn = open_mem();
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        insert_intake(&conn, "t", "q1", "inv_1", "r", now, "{}", "{}").unwrap();
        insert_intake(&conn, "t", "q2", "inv_2", "r", now, "{}", "{}").unwrap();
        let mut pending = list_pending_writebacks(&conn, "t").unwrap();
        pending.sort();
        assert_eq!(pending, vec!["q1".to_string(), "q2".to_string()]);
        mark_writeback_complete(&conn, "t", "q1", now).unwrap();
        let pending = list_pending_writebacks(&conn, "t").unwrap();
        assert_eq!(pending, vec!["q2".to_string()]);
    }

    // ── S256 / PR-245 — state + recovery + badge count ───────────────

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
    }

    #[test]
    fn count_unpicked_excludes_picked_error_and_irrelevant() {
        let conn = open_mem();
        // staged, un-picked → counts.
        insert_intake(&conn, "t", "q-staged", "inv_A", "r", now(), "{}", "{}").unwrap();
        // staged but picked up → excluded.
        insert_intake(&conn, "t", "q-picked", "inv_B", "r", now(), "{}", "{}").unwrap();
        set_picked_up_drf_id(&conn, "t", "q-picked", "drf_X").unwrap();
        // error → excluded.
        insert_error_intake(&conn, "t", "q-err", "r", now(), "{}", "bad email").unwrap();
        // irrelevant → excluded.
        insert_intake(&conn, "t", "q-irr", "inv_C", "r", now(), "{}", "{}").unwrap();
        mark_irrelevant(&conn, "t", "q-irr").unwrap();
        // other tenant → excluded.
        insert_intake(&conn, "t2", "q-other", "inv_D", "r", now(), "{}", "{}").unwrap();

        assert_eq!(count_unpicked(&conn, "t").unwrap(), 1);
    }

    #[test]
    fn error_row_blocks_reinsert_via_already_intook() {
        let conn = open_mem();
        insert_error_intake(&conn, "t", "q-err", "r", now(), "{\"x\":1}", "no email").unwrap();
        // Daemon precheck sees the error row and skips re-inserting.
        assert!(already_intook(&conn, "t", "q-err").unwrap().is_some());
        let (raw, state) = read_raw_and_state(&conn, "t", "q-err").unwrap().unwrap();
        assert_eq!(raw, "{\"x\":1}");
        assert_eq!(state, STATE_ERROR);
    }

    #[test]
    fn retry_parse_flips_error_to_staged_and_counts() {
        let conn = open_mem();
        insert_error_intake(&conn, "t", "q-err", "r", now(), "{}", "no email").unwrap();
        assert_eq!(count_unpicked(&conn, "t").unwrap(), 0);
        let n = retry_parse_intake(&conn, "t", "q-err", "inv_Z", "{\"ok\":true}").unwrap();
        assert_eq!(n, 1);
        assert_eq!(count_unpicked(&conn, "t").unwrap(), 1);
        // Re-running retry on an already-staged row is a no-op (guarded).
        let again = retry_parse_intake(&conn, "t", "q-err", "inv_Z", "{}").unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn mark_irrelevant_idempotent_and_removes_from_count() {
        let conn = open_mem();
        insert_intake(&conn, "t", "q1", "inv_A", "r", now(), "{}", "{}").unwrap();
        assert_eq!(count_unpicked(&conn, "t").unwrap(), 1);
        assert_eq!(mark_irrelevant(&conn, "t", "q1").unwrap(), 1);
        assert_eq!(count_unpicked(&conn, "t").unwrap(), 0);
        // Idempotent: a second mark still matches the row (rows-updated=1)
        // but the state is already irrelevant.
        assert_eq!(mark_irrelevant(&conn, "t", "q1").unwrap(), 1);
        assert_eq!(count_unpicked(&conn, "t").unwrap(), 0);
    }
}
