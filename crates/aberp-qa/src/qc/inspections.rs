//! S443 / ADR-0092 — `qc_inspections` rows + the `record_inspection`
//! write path.
//!
//! `record_inspection` is the single chokepoint: it computes the verdict
//! in code, writes one row, and emits the inspection audit events inside
//! the caller's transaction (the aberp-qa idiom — row + audit ride one
//! commit). It does NOT create the NCR — that crosses into the app layer
//! (`apps/aberp/src/quality.rs`), which aberp-qa cannot depend on. The
//! caller inspects [`RecordedInspection::auto_ncr_recommended`] and, on a
//! failing verdict, calls `quality::create_ncr` then links it back via
//! [`link_auto_ncr`] (emitting `QcAutoNcrCreated`).

use duckdb::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use ulid::Ulid;

use aberp_audit_ledger::{append_in_tx, Actor, EventKind, LedgerMeta};
use aberp_inventory::ActorKind;

use super::error::QcError;
use super::plans::InspectionPlan;
use super::verdict::{compute_verdict, Verdict};

/// Where a measurement came from. Manual operator entry today; `Probe`
/// when a real `ProbeIngestionSource` lands. `Cmm`/`Other` reserved for
/// future bench/CMM imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QcSource {
    Manual,
    Probe,
    Cmm,
    Other,
}

impl QcSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            QcSource::Manual => "manual",
            QcSource::Probe => "probe",
            QcSource::Cmm => "cmm",
            QcSource::Other => "other",
        }
    }

    pub fn from_storage_str(s: &str) -> Result<Self, &'static str> {
        match s {
            "manual" => Ok(QcSource::Manual),
            "probe" => Ok(QcSource::Probe),
            "cmm" => Ok(QcSource::Cmm),
            "other" => Ok(QcSource::Other),
            _ => Err("unknown QcSource storage string"),
        }
    }
}

/// One `qc_inspections` row (plan tolerances are denormalised snapshots).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QcInspection {
    /// `qci_<ULID>`.
    pub qci_id: String,
    pub measured_at_utc: String,
    pub source: QcSource,
    pub source_event_id: Option<String>,
    pub inspection_plan_id: String,
    pub feature_name: String,
    pub nominal_value: f64,
    pub upper_tol: f64,
    pub lower_tol: f64,
    pub units: String,
    pub actual_value: f64,
    pub deviation: f64,
    pub verdict: Verdict,
    pub probe_serial: Option<String>,
    pub last_calibration_at_utc: Option<String>,
    pub calibration_stale_at_event: bool,
    pub auto_ncr_id: Option<String>,
    pub linked_part_uid: Option<String>,
    pub linked_heat_lot: Option<String>,
    pub linked_wo_id: Option<String>,
    pub recorded_by: String,
    pub created_at: String,
}

/// Context for the write path (mirrors [`crate::QaWriteContext`]).
#[derive(Debug)]
pub struct QcWriteContext<'a> {
    pub tenant: &'a str,
    pub actor: ActorKind,
    pub ledger_meta: &'a LedgerMeta,
    pub ledger_actor: Actor,
}

/// Inputs to [`record_inspection`]. `current_time` + `stale_window_seconds`
/// are passed in (not read from a clock here) so the verdict is fully
/// deterministic and testable.
#[derive(Debug)]
pub struct RecordInspectionInputs<'a> {
    pub plan: &'a InspectionPlan,
    pub source: QcSource,
    pub source_event_id: Option<String>,
    pub actual_value: f64,
    /// The measurement's units — must match the plan's (fail loud, never
    /// coerce). For manual entry the UI auto-derives this from the plan.
    pub units: String,
    pub probe_serial: Option<String>,
    pub last_calibration_at: Option<OffsetDateTime>,
    pub measured_at: OffsetDateTime,
    pub current_time: OffsetDateTime,
    pub stale_window_seconds: u64,
    pub linked_part_uid: Option<String>,
    pub linked_heat_lot: Option<String>,
    pub linked_wo_id: Option<String>,
    pub recorded_by: String,
}

/// Outcome of a recorded inspection.
#[derive(Debug, Clone)]
pub struct RecordedInspection {
    pub inspection: QcInspection,
    pub verdict: Verdict,
    /// True iff the verdict is a failing tier (Minor/Major/Critical) → the
    /// caller should spawn an NCR. A `CalibrationStale` verdict is NOT a
    /// failure (warning only).
    pub auto_ncr_recommended: bool,
}

fn rfc3339(ts: OffsetDateTime) -> Result<String, QcError> {
    ts.format(&Rfc3339)
        .map_err(|e| QcError::Storage(anyhow::anyhow!("format timestamp: {e}")))
}

/// Compute the verdict, write one `qc_inspections` row, and emit
/// `QcInspectionRecorded` + (`QcInspectionPassed` | `QcInspectionFailed`
/// | `QcProbeCalibrationStaleWarning`) in `tx`. Returns the row + verdict.
///
/// Fails loud on a units mismatch (no row written; the caller emits
/// `QcProbeIngestionFailed` for a probe-sourced event).
pub fn record_inspection(
    tx: &Transaction<'_>,
    ctx: &QcWriteContext<'_>,
    inputs: RecordInspectionInputs<'_>,
) -> Result<RecordedInspection, QcError> {
    // Units must match the plan — never silently coerce (CLAUDE.md rule 12).
    if !inputs
        .units
        .trim()
        .eq_ignore_ascii_case(inputs.plan.units.trim())
    {
        return Err(QcError::UnitsMismatch {
            plan_id: inputs.plan.plan_id.clone(),
            expected: inputs.plan.units.clone(),
            got: inputs.units.clone(),
        });
    }

    let verdict = compute_verdict(
        inputs.plan,
        inputs.actual_value,
        inputs.last_calibration_at,
        inputs.current_time,
        inputs.stale_window_seconds,
    );
    let calibration_stale = verdict == Verdict::CalibrationStale;
    let deviation = inputs.actual_value - inputs.plan.nominal_value;

    let qci_id = format!("qci_{}", Ulid::new());
    let measured_at = rfc3339(inputs.measured_at)?;
    let created_at = rfc3339(inputs.current_time)?;
    let last_cal = inputs.last_calibration_at.map(rfc3339).transpose()?;

    tx.execute(
        "INSERT INTO qc_inspections (
            qci_id, tenant_id, measured_at_utc, source, source_event_id,
            inspection_plan_id, feature_name, nominal_value, upper_tol, lower_tol,
            units, actual_value, deviation, verdict, probe_serial,
            last_calibration_at_utc, calibration_stale_at_event, auto_ncr_id,
            linked_part_uid, linked_heat_lot, linked_wo_id, recorded_by, created_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?);",
        params![
            &qci_id,
            ctx.tenant,
            &measured_at,
            inputs.source.as_str(),
            inputs.source_event_id.as_deref(),
            &inputs.plan.plan_id,
            inputs.plan.feature_name.trim(),
            inputs.plan.nominal_value,
            inputs.plan.upper_tol,
            inputs.plan.lower_tol,
            inputs.plan.units.trim(),
            inputs.actual_value,
            deviation,
            verdict.as_str(),
            inputs.probe_serial.as_deref(),
            last_cal.as_deref(),
            calibration_stale,
            inputs.linked_part_uid.as_deref(),
            inputs.linked_heat_lot.as_deref(),
            inputs.linked_wo_id.as_deref(),
            inputs.recorded_by.trim(),
            &created_at,
        ],
    )
    .map_err(|e| QcError::Storage(anyhow::anyhow!("INSERT qc_inspections: {e}")))?;

    // Always: QcInspectionRecorded.
    emit(
        tx,
        ctx,
        EventKind::QcInspectionRecorded,
        json!({
            "qci_id": qci_id,
            "wo_id": inputs.linked_wo_id,
            "part_uid": inputs.linked_part_uid,
            "feature_name": inputs.plan.feature_name.trim(),
            "actual": inputs.actual_value,
            "deviation": deviation,
            "verdict": verdict.as_str(),
            "source": inputs.source.as_str(),
        }),
        format!("qc_recorded:{qci_id}"),
    )?;

    // Verdict-specific twin.
    match verdict {
        Verdict::Pass => emit(
            tx,
            ctx,
            EventKind::QcInspectionPassed,
            json!({
                "qci_id": qci_id,
                "wo_id": inputs.linked_wo_id,
                "feature_name": inputs.plan.feature_name.trim(),
            }),
            format!("qc_passed:{qci_id}"),
        )?,
        Verdict::Minor | Verdict::Major | Verdict::Critical => emit(
            tx,
            ctx,
            EventKind::QcInspectionFailed,
            json!({
                "qci_id": qci_id,
                "wo_id": inputs.linked_wo_id,
                "feature_name": inputs.plan.feature_name.trim(),
                "verdict": verdict.as_str(),
                "deviation": deviation,
            }),
            format!("qc_failed:{qci_id}"),
        )?,
        Verdict::CalibrationStale => emit(
            tx,
            ctx,
            EventKind::QcProbeCalibrationStaleWarning,
            json!({
                "qci_id": qci_id,
                "probe_serial": inputs.probe_serial,
                "last_calibration_at_utc": last_cal,
                "stale_by_seconds": inputs
                    .last_calibration_at
                    .map(|c| (inputs.current_time - c).whole_seconds()),
            }),
            format!("qc_stale:{qci_id}"),
        )?,
    }

    let inspection = QcInspection {
        qci_id,
        measured_at_utc: measured_at,
        source: inputs.source,
        source_event_id: inputs.source_event_id,
        inspection_plan_id: inputs.plan.plan_id.clone(),
        feature_name: inputs.plan.feature_name.trim().to_string(),
        nominal_value: inputs.plan.nominal_value,
        upper_tol: inputs.plan.upper_tol,
        lower_tol: inputs.plan.lower_tol,
        units: inputs.plan.units.trim().to_string(),
        actual_value: inputs.actual_value,
        deviation,
        verdict,
        probe_serial: inputs.probe_serial,
        last_calibration_at_utc: last_cal,
        calibration_stale_at_event: calibration_stale,
        auto_ncr_id: None,
        linked_part_uid: inputs.linked_part_uid,
        linked_heat_lot: inputs.linked_heat_lot,
        linked_wo_id: inputs.linked_wo_id,
        recorded_by: inputs.recorded_by.trim().to_string(),
        created_at,
    };

    Ok(RecordedInspection {
        inspection,
        verdict,
        auto_ncr_recommended: verdict.is_failing(),
    })
}

/// Link an auto-spawned NCR back onto an inspection row + emit
/// `QcAutoNcrCreated` (the audit cross-link). Called by the app
/// orchestrator after `quality::create_ncr`, in a fresh transaction.
pub fn link_auto_ncr(
    tx: &Transaction<'_>,
    ctx: &QcWriteContext<'_>,
    qci_id: &str,
    ncr_id: &str,
    verdict: Verdict,
) -> Result<(), QcError> {
    tx.execute(
        "UPDATE qc_inspections SET auto_ncr_id = ? WHERE tenant_id = ? AND qci_id = ?;",
        params![ncr_id, ctx.tenant, qci_id],
    )
    .map_err(|e| QcError::Storage(anyhow::anyhow!("UPDATE qc_inspections.auto_ncr_id: {e}")))?;
    emit(
        tx,
        ctx,
        EventKind::QcAutoNcrCreated,
        json!({ "qci_id": qci_id, "ncr_id": ncr_id, "verdict": verdict.as_str() }),
        format!("qc_auto_ncr:{qci_id}"),
    )
}

/// Emit `QcProbeIngestionFailed` — a probe event that could not become an
/// inspection (units mismatch, missing/unparseable value, probe fault).
/// Fails loud, never silent (CLAUDE.md rule 12).
pub fn record_ingestion_failure(
    tx: &Transaction<'_>,
    ctx: &QcWriteContext<'_>,
    reason: &str,
    raw_excerpt: &str,
) -> Result<(), QcError> {
    emit(
        tx,
        ctx,
        EventKind::QcProbeIngestionFailed,
        json!({ "reason": reason, "raw_excerpt": raw_excerpt }),
        format!("qc_ingest_fail:{}", Ulid::new()),
    )
}

fn emit(
    tx: &Transaction<'_>,
    ctx: &QcWriteContext<'_>,
    kind: EventKind,
    payload: serde_json::Value,
    idempotency_key: String,
) -> Result<(), QcError> {
    let kind_str = kind.as_str();
    append_in_tx(
        tx,
        ctx.ledger_meta,
        kind,
        serde_json::to_vec(&payload).expect("serialize qc payload"),
        ctx.ledger_actor.clone(),
        Some(idempotency_key),
    )
    .map_err(|e| QcError::Storage(anyhow::anyhow!("audit append {kind_str}: {e}")))?;
    Ok(())
}

// ── Reads (Rust-side filter, no index dependency — S341/S410 precedent) ──

/// All inspections for a work order, newest first.
pub fn list_inspections_for_wo(
    conn: &Connection,
    tenant: &str,
    wo_id: &str,
) -> Result<Vec<QcInspection>, QcError> {
    query_inspections(
        conn,
        "WHERE tenant_id = ? AND linked_wo_id = ? ORDER BY measured_at_utc DESC, qci_id DESC",
        params![tenant, wo_id],
    )
}

/// All inspections for a part UID, newest first (per-part probe history).
pub fn list_inspections_for_part(
    conn: &Connection,
    tenant: &str,
    part_uid: &str,
) -> Result<Vec<QcInspection>, QcError> {
    query_inspections(
        conn,
        "WHERE tenant_id = ? AND linked_part_uid = ? ORDER BY measured_at_utc DESC, qci_id DESC",
        params![tenant, part_uid],
    )
}

/// Stale-calibration inspections in the last `window_seconds` (dashboard
/// card). `now` is passed in so the cutoff is deterministic.
pub fn list_recent_stale_calibration(
    conn: &Connection,
    tenant: &str,
    now: OffsetDateTime,
    window_seconds: i64,
) -> Result<Vec<QcInspection>, QcError> {
    let cutoff = rfc3339(now - time::Duration::seconds(window_seconds))?;
    query_inspections(
        conn,
        "WHERE tenant_id = ? AND calibration_stale_at_event = true AND measured_at_utc >= ? \
         ORDER BY measured_at_utc DESC, qci_id DESC",
        params![tenant, cutoff],
    )
}

fn query_inspections(
    conn: &Connection,
    where_order: &str,
    p: impl duckdb::Params,
) -> Result<Vec<QcInspection>, QcError> {
    let sql = format!(
        "SELECT qci_id, measured_at_utc, source, source_event_id, inspection_plan_id,
                feature_name, nominal_value, upper_tol, lower_tol, units, actual_value,
                deviation, verdict, probe_serial, last_calibration_at_utc,
                calibration_stale_at_event, auto_ncr_id, linked_part_uid, linked_heat_lot,
                linked_wo_id, recorded_by, created_at
         FROM qc_inspections {where_order};"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| QcError::Storage(anyhow::anyhow!("prepare inspections query: {e}")))?;
    let rows = stmt
        .query_map(p, parse_inspection_row)
        .map_err(|e| QcError::Storage(anyhow::anyhow!("query inspections: {e}")))?;
    let mut acc = Vec::new();
    for r in rows {
        let parsed =
            r.map_err(|e| QcError::Storage(anyhow::anyhow!("read inspection row: {e}")))?;
        acc.push(parsed.map_err(QcError::Storage)?);
    }
    Ok(acc)
}

fn parse_inspection_row(
    row: &duckdb::Row<'_>,
) -> duckdb::Result<Result<QcInspection, anyhow::Error>> {
    let verdict_str: String = row.get(12)?;
    let source_str: String = row.get(2)?;
    Ok((|| {
        Ok(QcInspection {
            qci_id: row.get(0)?,
            measured_at_utc: row.get(1)?,
            source: QcSource::from_storage_str(&source_str)
                .map_err(|e| anyhow::anyhow!("{e}: {source_str:?}"))?,
            source_event_id: row.get(3)?,
            inspection_plan_id: row.get(4)?,
            feature_name: row.get(5)?,
            nominal_value: row.get(6)?,
            upper_tol: row.get(7)?,
            lower_tol: row.get(8)?,
            units: row.get(9)?,
            actual_value: row.get(10)?,
            deviation: row.get(11)?,
            verdict: Verdict::from_storage_str(&verdict_str)
                .map_err(|e| anyhow::anyhow!("{e}: {verdict_str:?}"))?,
            probe_serial: row.get(13)?,
            last_calibration_at_utc: row.get(14)?,
            calibration_stale_at_event: row.get(15)?,
            auto_ncr_id: row.get(16)?,
            linked_part_uid: row.get(17)?,
            linked_heat_lot: row.get(18)?,
            linked_wo_id: row.get(19)?,
            recorded_by: row.get(20)?,
            created_at: row.get(21)?,
        })
    })())
}
