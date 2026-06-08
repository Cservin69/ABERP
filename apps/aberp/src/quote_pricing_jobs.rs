//! S279 / PR-265 — `quote_pricing_jobs` table + state machine for the
//! ABERP-side auto-quoting producer pipeline.
//!
//! ## Posture
//!
//! Distinct from [`crate::quote_intake_query`]'s `quote_intake_log` —
//! that table is "approved quotes awaiting DEAL", this table is
//! "pending quotes needing pricing." The two never overlap: a
//! storefront quote starts in `received` state, the pricing pipeline
//! drives it through this table until the storefront flips it to
//! `quoted`. Customer accept then later promotes it to `approved`,
//! which the existing intake daemon picks up into `quote_intake_log`.
//!
//! ## State machine
//!
//! ```text
//!   Fetched ──► Extracting ──► Pricing ──► Rendering ──► PostingBack ──► Posted
//!      │            │              │            │              │            (terminal)
//!      ▼            ▼              ▼            ▼              ▼
//!   Failed       Failed         Failed       Failed        Failed
//!  (operator retry re-enqueues at Fetched)
//! ```
//!
//! Per [[no-sql-specific]] the state is **app-layer enforced**, not a
//! DuckDB CHECK. Closed-vocab strings ([`STATE_FETCHED`] etc.) are the
//! single source of truth; any `from_storage_str` failure is loud.
//!
//! ## No CHECK constraints, no DEFAULTs
//!
//! Same DuckDB gotcha as [[material-reservation-s273]] and S271's
//! storefront projection columns: `ALTER TABLE ... ADD COLUMN IF NOT
//! EXISTS col TYPE DEFAULT V` silently re-applies the default on every
//! replay. Every column is nullable; the app layer enforces required-
//! vs-optional on insert/update.

use anyhow::{anyhow, Context, Result};
use duckdb::{params, Connection};
use time::OffsetDateTime;

/// State name as stored in the table's `state` VARCHAR column.
///
/// Closed-vocab — any string parsed as state goes through
/// [`JobState::parse_str`] which errors on unknown values (CLAUDE.md
/// rule 12).
pub const STATE_FETCHED: &str = "fetched";
/// Daemon has begun the CAD subprocess via `aberp-cad-extract-wrapper`.
pub const STATE_EXTRACTING: &str = "extracting";
/// Subprocess succeeded; daemon is now calling `aberp_quote_engine::quote`.
pub const STATE_PRICING: &str = "pricing";
/// Engine returned `Ok(_)`; daemon is now rendering the PDF.
pub const STATE_RENDERING: &str = "rendering";
/// PDF rendered; daemon is POSTing the priced multipart to the
/// storefront's `/api/quotes/{id}/priced` endpoint per ADR-0004.
pub const STATE_POSTING_BACK: &str = "posting_back";
/// Storefront returned 200 (incl. idempotent replay). Terminal.
pub const STATE_POSTED: &str = "posted";
/// Any stage failure lands here. Operator can retry from the SPA,
/// which re-enqueues at `Fetched`.
pub const STATE_FAILED: &str = "failed";

/// Closed-vocab `state` value the pricing pipeline tracks for each
/// row. Wire shape on insert + read is the lowercase string above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Fetched from storefront; awaiting extract.
    Fetched,
    /// Extract subprocess running.
    Extracting,
    /// Engine running.
    Pricing,
    /// PDF rendering.
    Rendering,
    /// POSTing the multipart writeback.
    PostingBack,
    /// Terminal success.
    Posted,
    /// Terminal failure (operator can retry).
    Failed,
}

impl JobState {
    /// DB storage string.
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Fetched => STATE_FETCHED,
            JobState::Extracting => STATE_EXTRACTING,
            JobState::Pricing => STATE_PRICING,
            JobState::Rendering => STATE_RENDERING,
            JobState::PostingBack => STATE_POSTING_BACK,
            JobState::Posted => STATE_POSTED,
            JobState::Failed => STATE_FAILED,
        }
    }

    /// Round-trip parse. Errors loud on unknown — silent-fallback
    /// would mask schema drift. Named `parse_str` rather than
    /// `from_str` so we don't accidentally shadow the std
    /// `FromStr::from_str` signature (clippy warns about the trait
    /// confusion; we don't want or need the `FromStr` trait here
    /// because the error type carries `anyhow` context).
    pub fn parse_str(s: &str) -> Result<Self> {
        match s {
            STATE_FETCHED => Ok(JobState::Fetched),
            STATE_EXTRACTING => Ok(JobState::Extracting),
            STATE_PRICING => Ok(JobState::Pricing),
            STATE_RENDERING => Ok(JobState::Rendering),
            STATE_POSTING_BACK => Ok(JobState::PostingBack),
            STATE_POSTED => Ok(JobState::Posted),
            STATE_FAILED => Ok(JobState::Failed),
            other => Err(anyhow!("unknown quote_pricing_jobs.state: {other:?}")),
        }
    }
}

/// One row of `quote_pricing_jobs`. Read-only projection used by the
/// SPA list + the daemon's state machine.
#[derive(Debug, Clone)]
pub struct PricingJobRow {
    /// Storefront UUID — the durable identity.
    pub quote_id: String,
    pub tenant_id: String,
    pub state: JobState,
    /// ISO-8601 timestamps for the operator-visible "since" column.
    pub fetched_at: String,
    pub updated_at: String,
    pub customer_email: String,
    pub customer_name: String,
    pub material_grade: String,
    pub quantity: u32,
    /// `Some(_)` once Extracting succeeded; carries the blake3 hash
    /// the storefront uses as the priced-writeback idempotency key.
    pub feature_graph_hash: Option<String>,
    /// `Some(_)` once Pricing succeeded; total in EUR, surfaced in
    /// the SPA list column.
    pub total_price_eur: Option<f64>,
    /// `Some(_)` on Failed rows; operator-readable stage + reason.
    pub error_stage: Option<String>,
    pub error_reason: Option<String>,
    /// Increments on every retry — disambiguates the audit-failure
    /// idempotency key suffix so a re-failure doesn't UNIQUE-collide.
    pub attempt_n: u32,
}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS quote_pricing_jobs (
    quote_id              VARCHAR NOT NULL PRIMARY KEY,
    tenant_id             VARCHAR NOT NULL,
    state                 VARCHAR NOT NULL,
    fetched_at            VARCHAR NOT NULL,
    updated_at            VARCHAR NOT NULL,
    customer_email        VARCHAR NOT NULL,
    customer_name         VARCHAR NOT NULL,
    material_grade        VARCHAR NOT NULL,
    quantity              INTEGER NOT NULL,
    cad_filename          VARCHAR NOT NULL,
    cad_local_path        VARCHAR NOT NULL,
    feature_graph_hash    VARCHAR,
    feature_graph_json    VARCHAR,
    breakdown_json        VARCHAR,
    pdf_path              VARCHAR,
    total_price_eur       DOUBLE,
    valid_until_iso       VARCHAR,
    error_stage           VARCHAR,
    error_reason          VARCHAR,
    attempt_n             INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS quote_pricing_jobs_tenant_state_idx
    ON quote_pricing_jobs (tenant_id, state);
";

/// Idempotent — call at every writer entry.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)
        .with_context(|| "ensure quote_pricing_jobs schema")
}

/// Insert a new `Fetched` job. Idempotent via `quote_id` PK: a re-
/// fetch on an existing row returns `Ok(false)`. The caller emits
/// the `QuotePricingFetched` audit row ONLY when this returns
/// `Ok(true)`.
#[allow(clippy::too_many_arguments)]
pub fn insert_fetched_job(
    conn: &Connection,
    quote_id: &str,
    tenant_id: &str,
    customer_email: &str,
    customer_name: &str,
    material_grade: &str,
    quantity: u32,
    cad_filename: &str,
    cad_local_path: &str,
    now: OffsetDateTime,
) -> Result<bool> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format fetched_at")?;
    let rows = conn
        .execute(
            "INSERT INTO quote_pricing_jobs (
                quote_id, tenant_id, state, fetched_at, updated_at,
                customer_email, customer_name, material_grade, quantity,
                cad_filename, cad_local_path, attempt_n
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
            ON CONFLICT (quote_id) DO NOTHING",
            params![
                quote_id,
                tenant_id,
                STATE_FETCHED,
                ts,
                ts,
                customer_email,
                customer_name,
                material_grade,
                quantity as i64,
                cad_filename,
                cad_local_path,
            ],
        )
        .context("insert quote_pricing_jobs row")?;
    Ok(rows > 0)
}

/// Flip state to `to`. Caller is responsible for ordering — this
/// helper is unconditional, the state machine lives in the daemon
/// pipeline module.
pub fn set_state(
    conn: &Connection,
    quote_id: &str,
    tenant_id: &str,
    to: JobState,
    now: OffsetDateTime,
) -> Result<()> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format updated_at")?;
    conn.execute(
        "UPDATE quote_pricing_jobs
            SET state = ?, updated_at = ?
            WHERE quote_id = ? AND tenant_id = ?",
        params![to.as_str(), ts, quote_id, tenant_id],
    )
    .context("set_state UPDATE")?;
    Ok(())
}

/// Stamp the extracted FeatureGraph + its hash, then flip state to
/// `Pricing`. One transaction so a crash mid-update can't leave a
/// row with `feature_graph_hash` set but `state` still `Extracting`.
pub fn set_extracted(
    conn: &mut Connection,
    quote_id: &str,
    tenant_id: &str,
    feature_graph_hash: &str,
    feature_graph_json: &str,
    now: OffsetDateTime,
) -> Result<()> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format updated_at")?;
    let tx = conn.transaction().context("open set_extracted tx")?;
    tx.execute(
        "UPDATE quote_pricing_jobs
            SET state = ?, updated_at = ?, feature_graph_hash = ?, feature_graph_json = ?
            WHERE quote_id = ? AND tenant_id = ?",
        params![
            STATE_PRICING,
            ts,
            feature_graph_hash,
            feature_graph_json,
            quote_id,
            tenant_id,
        ],
    )
    .context("set_extracted UPDATE")?;
    tx.commit().context("commit set_extracted")?;
    Ok(())
}

/// Stamp the priced breakdown + total, flip to `Rendering`.
pub fn set_priced(
    conn: &mut Connection,
    quote_id: &str,
    tenant_id: &str,
    breakdown_json: &str,
    total_price_eur: f64,
    now: OffsetDateTime,
) -> Result<()> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format updated_at")?;
    let tx = conn.transaction().context("open set_priced tx")?;
    tx.execute(
        "UPDATE quote_pricing_jobs
            SET state = ?, updated_at = ?, breakdown_json = ?, total_price_eur = ?
            WHERE quote_id = ? AND tenant_id = ?",
        params![
            STATE_RENDERING,
            ts,
            breakdown_json,
            total_price_eur,
            quote_id,
            tenant_id,
        ],
    )
    .context("set_priced UPDATE")?;
    tx.commit().context("commit set_priced")?;
    Ok(())
}

/// Stamp the PDF on-disk path + valid_until, flip to `PostingBack`.
pub fn set_rendered(
    conn: &mut Connection,
    quote_id: &str,
    tenant_id: &str,
    pdf_path: &str,
    valid_until_iso: &str,
    now: OffsetDateTime,
) -> Result<()> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format updated_at")?;
    let tx = conn.transaction().context("open set_rendered tx")?;
    tx.execute(
        "UPDATE quote_pricing_jobs
            SET state = ?, updated_at = ?, pdf_path = ?, valid_until_iso = ?
            WHERE quote_id = ? AND tenant_id = ?",
        params![
            STATE_POSTING_BACK,
            ts,
            pdf_path,
            valid_until_iso,
            quote_id,
            tenant_id,
        ],
    )
    .context("set_rendered UPDATE")?;
    tx.commit().context("commit set_rendered")?;
    Ok(())
}

/// Mark `Failed` with the stage + reason that broke the pipeline.
/// Truncates `reason` to 1000 chars (CR/LF/NUL stripped).
pub fn set_failed(
    conn: &mut Connection,
    quote_id: &str,
    tenant_id: &str,
    stage: &str,
    reason: &str,
    now: OffsetDateTime,
) -> Result<()> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format updated_at")?;
    let safe = sanitize_reason(reason);
    let tx = conn.transaction().context("open set_failed tx")?;
    tx.execute(
        "UPDATE quote_pricing_jobs
            SET state = ?, updated_at = ?, error_stage = ?, error_reason = ?
            WHERE quote_id = ? AND tenant_id = ?",
        params![STATE_FAILED, ts, stage, safe, quote_id, tenant_id],
    )
    .context("set_failed UPDATE")?;
    tx.commit().context("commit set_failed")?;
    Ok(())
}

/// Operator retry — bump `attempt_n`, clear error fields, reset state
/// to `Fetched`. The daemon's next sweep picks it up again. Returns
/// the new `attempt_n` so the caller's audit-key suffix stays unique.
pub fn retry_job(
    conn: &mut Connection,
    quote_id: &str,
    tenant_id: &str,
    now: OffsetDateTime,
) -> Result<u32> {
    ensure_schema(conn)?;
    let ts = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format updated_at")?;
    let tx = conn.transaction().context("open retry_job tx")?;
    tx.execute(
        "UPDATE quote_pricing_jobs
            SET state = ?, updated_at = ?, error_stage = NULL, error_reason = NULL,
                attempt_n = attempt_n + 1
            WHERE quote_id = ? AND tenant_id = ? AND state = ?",
        params![STATE_FETCHED, ts, quote_id, tenant_id, STATE_FAILED],
    )
    .context("retry_job UPDATE")?;
    let new_n: i64 = tx
        .query_row(
            "SELECT attempt_n FROM quote_pricing_jobs
                WHERE quote_id = ? AND tenant_id = ?",
            params![quote_id, tenant_id],
            |r| r.get(0),
        )
        .context("read attempt_n")?;
    tx.commit().context("commit retry_job")?;
    Ok(new_n as u32)
}

/// SPA + daemon read path. Returns rows newest-first.
pub fn list_jobs(conn: &Connection, tenant_id: &str) -> Result<Vec<PricingJobRow>> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT quote_id, tenant_id, state, fetched_at, updated_at,
                    customer_email, customer_name, material_grade, quantity,
                    feature_graph_hash, total_price_eur, error_stage, error_reason,
                    attempt_n
                FROM quote_pricing_jobs
                WHERE tenant_id = ?
                ORDER BY fetched_at DESC",
        )
        .context("prepare list_jobs")?;
    let mut rows = stmt
        .query(params![tenant_id])
        .context("execute list_jobs")?;
    let mut out: Vec<PricingJobRow> = Vec::new();
    while let Some(r) = rows.next().context("step list_jobs")? {
        let state_str: String = r.get(2).context("get state")?;
        let state = JobState::parse_str(&state_str)?;
        let qty: i64 = r.get(8).context("get quantity")?;
        let attempt_n: i64 = r.get(13).context("get attempt_n")?;
        out.push(PricingJobRow {
            quote_id: r.get(0).context("get quote_id")?,
            tenant_id: r.get(1).context("get tenant_id")?,
            state,
            fetched_at: r.get(3).context("get fetched_at")?,
            updated_at: r.get(4).context("get updated_at")?,
            customer_email: r.get(5).context("get customer_email")?,
            customer_name: r.get(6).context("get customer_name")?,
            material_grade: r.get(7).context("get material_grade")?,
            quantity: qty.max(0) as u32,
            feature_graph_hash: r.get(9).ok(),
            total_price_eur: r.get(10).ok(),
            error_stage: r.get(11).ok(),
            error_reason: r.get(12).ok(),
            attempt_n: attempt_n.max(0) as u32,
        });
    }
    Ok(out)
}

/// Daemon read path — find the oldest non-terminal, non-failed job
/// to advance. Returns `None` if the queue is empty.
pub fn next_actionable_job(conn: &Connection, tenant_id: &str) -> Result<Option<PricingJobRow>> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT quote_id, tenant_id, state, fetched_at, updated_at,
                    customer_email, customer_name, material_grade, quantity,
                    feature_graph_hash, total_price_eur, error_stage, error_reason,
                    attempt_n
                FROM quote_pricing_jobs
                WHERE tenant_id = ?
                  AND state IN ('fetched','extracting','pricing','rendering','posting_back')
                ORDER BY fetched_at ASC
                LIMIT 1",
        )
        .context("prepare next_actionable_job")?;
    let mut rows = stmt
        .query(params![tenant_id])
        .context("execute next_actionable_job")?;
    if let Some(r) = rows.next().context("step next_actionable_job")? {
        let state_str: String = r.get(2).context("get state")?;
        let state = JobState::parse_str(&state_str)?;
        let qty: i64 = r.get(8).context("get quantity")?;
        let attempt_n: i64 = r.get(13).context("get attempt_n")?;
        Ok(Some(PricingJobRow {
            quote_id: r.get(0).context("get quote_id")?,
            tenant_id: r.get(1).context("get tenant_id")?,
            state,
            fetched_at: r.get(3).context("get fetched_at")?,
            updated_at: r.get(4).context("get updated_at")?,
            customer_email: r.get(5).context("get customer_email")?,
            customer_name: r.get(6).context("get customer_name")?,
            material_grade: r.get(7).context("get material_grade")?,
            quantity: qty.max(0) as u32,
            feature_graph_hash: r.get(9).ok(),
            total_price_eur: r.get(10).ok(),
            error_stage: r.get(11).ok(),
            error_reason: r.get(12).ok(),
            attempt_n: attempt_n.max(0) as u32,
        }))
    } else {
        Ok(None)
    }
}

/// Read FeatureGraph JSON + breakdown JSON + CAD path for a single
/// row. Used by the pipeline when advancing past `Extracting`.
pub fn get_job_artifacts(
    conn: &Connection,
    quote_id: &str,
    tenant_id: &str,
) -> Result<JobArtifacts> {
    let mut stmt = conn
        .prepare(
            "SELECT cad_local_path, feature_graph_json, breakdown_json, pdf_path
                FROM quote_pricing_jobs
                WHERE quote_id = ? AND tenant_id = ?",
        )
        .context("prepare get_job_artifacts")?;
    let mut rows = stmt
        .query(params![quote_id, tenant_id])
        .context("execute get_job_artifacts")?;
    if let Some(r) = rows.next().context("step get_job_artifacts")? {
        Ok(JobArtifacts {
            cad_local_path: r.get(0).context("get cad_local_path")?,
            feature_graph_json: r.get(1).ok(),
            breakdown_json: r.get(2).ok(),
            pdf_path: r.get(3).ok(),
        })
    } else {
        Err(anyhow!("no quote_pricing_jobs row for {quote_id}"))
    }
}

/// Artifacts attached to one job row. The fields go `Some(_)` as the
/// pipeline advances; reading from a `Failed` row gives whichever
/// state the failure happened at.
#[derive(Debug, Clone)]
pub struct JobArtifacts {
    pub cad_local_path: String,
    pub feature_graph_json: Option<String>,
    pub breakdown_json: Option<String>,
    pub pdf_path: Option<String>,
}

/// Trim CR/LF/NUL out of a free-text error reason and truncate to
/// 1000 chars. The reason rides into the SPA error column +
/// audit-payload `reason` field; we never want header-injection chars
/// to leak into a log line.
fn sanitize_reason(reason: &str) -> String {
    let cleaned: String = reason
        .chars()
        .filter(|c| !matches!(*c, '\r' | '\n' | '\0'))
        .collect();
    if cleaned.chars().count() <= 1000 {
        cleaned
    } else {
        cleaned.chars().take(1000).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;
    use time::OffsetDateTime;

    fn open_mem() -> Connection {
        let conn = Connection::open_in_memory().expect("open mem");
        ensure_schema(&conn).expect("schema");
        conn
    }

    fn fixed_ts() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap()
    }

    #[test]
    fn ensure_schema_is_idempotent() {
        let conn = open_mem();
        ensure_schema(&conn).expect("re-apply");
        ensure_schema(&conn).expect("third");
    }

    #[test]
    fn insert_then_advance_through_states() {
        let mut conn = open_mem();
        let inserted = insert_fetched_job(
            &conn,
            "q1",
            "T",
            "alice@example.com",
            "Alice",
            "AL_6061_T6",
            10,
            "cube.stl",
            "/tmp/cube.stl",
            fixed_ts(),
        )
        .expect("insert");
        assert!(inserted);

        // Re-insert same quote_id → no-op (idempotent).
        let inserted_again = insert_fetched_job(
            &conn,
            "q1",
            "T",
            "alice@example.com",
            "Alice",
            "AL_6061_T6",
            10,
            "cube.stl",
            "/tmp/cube.stl",
            fixed_ts(),
        )
        .expect("re-insert");
        assert!(!inserted_again);

        set_state(&conn, "q1", "T", JobState::Extracting, fixed_ts()).expect("set Extracting");
        set_extracted(&mut conn, "q1", "T", "blake3:dead", "{}", fixed_ts()).expect("extracted");
        set_priced(&mut conn, "q1", "T", "{\"k\":1}", 42.0, fixed_ts()).expect("priced");
        set_rendered(
            &mut conn,
            "q1",
            "T",
            "/tmp/q1/priced.pdf",
            "2026-07-06",
            fixed_ts(),
        )
        .expect("rendered");
        set_state(&conn, "q1", "T", JobState::Posted, fixed_ts()).expect("posted");

        let rows = list_jobs(&conn, "T").expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, JobState::Posted);
        assert_eq!(rows[0].feature_graph_hash.as_deref(), Some("blake3:dead"));
        assert_eq!(rows[0].total_price_eur, Some(42.0));
        assert_eq!(rows[0].quantity, 10);
    }

    #[test]
    fn set_failed_marks_terminal_and_carries_reason() {
        let mut conn = open_mem();
        insert_fetched_job(
            &conn,
            "q2",
            "T",
            "b@x",
            "Bob",
            "AL",
            1,
            "p.stl",
            "/tmp/p.stl",
            fixed_ts(),
        )
        .expect("ins");
        set_state(&conn, "q2", "T", JobState::Extracting, fixed_ts()).expect("ex");
        set_failed(&mut conn, "q2", "T", "extract", "OCCT crash", fixed_ts()).expect("fail");
        let rows = list_jobs(&conn, "T").expect("list");
        assert_eq!(rows[0].state, JobState::Failed);
        assert_eq!(rows[0].error_stage.as_deref(), Some("extract"));
        assert_eq!(rows[0].error_reason.as_deref(), Some("OCCT crash"));
        assert_eq!(rows[0].attempt_n, 0);
    }

    #[test]
    fn retry_bumps_attempt_and_resets_state() {
        let mut conn = open_mem();
        insert_fetched_job(
            &conn,
            "q3",
            "T",
            "b@x",
            "Bob",
            "AL",
            1,
            "p.stl",
            "/tmp/p.stl",
            fixed_ts(),
        )
        .expect("ins");
        set_failed(&mut conn, "q3", "T", "post", "503", fixed_ts()).expect("fail");
        let n1 = retry_job(&mut conn, "q3", "T", fixed_ts()).expect("retry1");
        assert_eq!(n1, 1);
        let rows = list_jobs(&conn, "T").expect("list");
        assert_eq!(rows[0].state, JobState::Fetched);
        assert!(rows[0].error_stage.is_none());
        assert_eq!(rows[0].attempt_n, 1);
        // Re-fail + re-retry → attempt 2.
        set_failed(&mut conn, "q3", "T", "post", "again", fixed_ts()).expect("fail2");
        let n2 = retry_job(&mut conn, "q3", "T", fixed_ts()).expect("retry2");
        assert_eq!(n2, 2);
    }

    #[test]
    fn retry_is_a_noop_when_state_is_not_failed() {
        let mut conn = open_mem();
        insert_fetched_job(
            &conn,
            "q4",
            "T",
            "b@x",
            "Bob",
            "AL",
            1,
            "p.stl",
            "/tmp/p.stl",
            fixed_ts(),
        )
        .expect("ins");
        // No `Failed` state — retry must NOT advance.
        let n = retry_job(&mut conn, "q4", "T", fixed_ts()).expect("retry");
        // attempt_n stays 0 because the WHERE filter didn't match.
        assert_eq!(n, 0);
        let rows = list_jobs(&conn, "T").expect("list");
        assert_eq!(rows[0].state, JobState::Fetched);
    }

    #[test]
    fn next_actionable_skips_terminal_states() {
        let mut conn = open_mem();
        insert_fetched_job(
            &conn,
            "a",
            "T",
            "b@x",
            "Bob",
            "AL",
            1,
            "p.stl",
            "/tmp/p.stl",
            fixed_ts(),
        )
        .expect("a");
        insert_fetched_job(
            &conn,
            "b",
            "T",
            "b@x",
            "Bob",
            "AL",
            1,
            "p.stl",
            "/tmp/p.stl",
            fixed_ts() + time::Duration::seconds(1),
        )
        .expect("b");
        set_state(&conn, "a", "T", JobState::Posted, fixed_ts()).expect("posted");
        set_failed(&mut conn, "b", "T", "extract", "oops", fixed_ts()).expect("fail");
        // a is Posted (terminal), b is Failed (terminal-for-daemon) →
        // nothing actionable.
        let nxt = next_actionable_job(&conn, "T").expect("next");
        assert!(nxt.is_none());
        // Inserting a new fresh row → that's the actionable one.
        insert_fetched_job(
            &conn,
            "c",
            "T",
            "c@x",
            "Carol",
            "AL",
            1,
            "p.stl",
            "/tmp/p.stl",
            fixed_ts() + time::Duration::seconds(2),
        )
        .expect("c");
        let nxt = next_actionable_job(&conn, "T").expect("next2");
        assert_eq!(nxt.expect("c is next").quote_id, "c");
    }

    #[test]
    fn job_state_round_trip() {
        let cases = [
            JobState::Fetched,
            JobState::Extracting,
            JobState::Pricing,
            JobState::Rendering,
            JobState::PostingBack,
            JobState::Posted,
            JobState::Failed,
        ];
        for s in cases {
            assert_eq!(JobState::parse_str(s.as_str()).unwrap(), s);
        }
        assert!(JobState::parse_str("not_a_state").is_err());
    }

    #[test]
    fn sanitize_reason_strips_control_chars_and_truncates() {
        let s = "boom\r\nfollowup\0tail";
        assert_eq!(sanitize_reason(s), "boomfollowuptail");
        let long: String = "a".repeat(2000);
        let cleaned = sanitize_reason(&long);
        assert_eq!(cleaned.chars().count(), 1000);
    }
}
