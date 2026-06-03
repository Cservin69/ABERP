//! Work-orders repository — the load-bearing surface per ADR-0062.
//!
//! Three groups of operations:
//!
//! 1. **Schema** — [`ensure_schema`] applies V001__work_orders.sql.
//!    Idempotent CREATE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS
//!    posture; safe to call against an already-migrated tenant DB.
//!
//! 2. **BOM authorship** — [`list_active_bom_for_product`] +
//!    [`replace_bom_for_product`]. Soft-retire-old-rows + insert-new
//!    pattern per ADR-0062 §6 (BOM rows are NEVER DELETEd).
//!
//! 3. **Work-order lifecycle** — [`create_work_order`] +
//!    [`transition_work_order`] + read helpers
//!    ([`list_work_orders`], [`get_work_order`]).
//!
//! All write paths take a `&Transaction` so the caller commits; the
//! audit-ledger entry + every secondary write (stock_movements,
//! routing-op cascade) rides the same DB commit. Same posture as
//! `aberp_inventory::record_movement`.

use anyhow::{anyhow, Context};
use duckdb::{params, Connection, Transaction};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use ulid::Ulid;

use aberp_audit_ledger::{append_in_tx, Actor, EventKind, LedgerMeta};

use crate::audit::{WorkOrderCreatedPayload, WorkOrderStateChangedPayload};
use crate::error::WorkOrderError;
use crate::state::next_state;
use crate::types::{RoutingOpState, WoAction, WorkOrderState};

// ── Schema ─────────────────────────────────────────────────────────

/// Ensure the work_orders / boms / routings tables exist. Idempotent;
/// mirrors `aberp_inventory::ensure_schema`. Must be called AFTER the
/// products migration so a future schema-extension that joins against
/// products has the parent table in place.
pub fn ensure_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(include_str!("../migrations/V001__work_orders.sql"))
        .context("ensure work-orders schema")
}

// ── BOM (ADR-0062 §1, §6) ───────────────────────────────────────────

/// One BOM line row. The active set for a product is the rows where
/// `retired_at IS NULL` — the Release handler snapshots that set per
/// ADR-0062 §5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BomLine {
    /// `bml_<ULID>`.
    pub bom_line_id: String,
    /// Parent product (the finished good).
    pub product_id: String,
    /// Child product (the component being consumed).
    pub component_id: String,
    pub qty_per_unit: Decimal,
    pub created_at: String,
    /// `Some(_)` once retired per ADR-0062 §6; otherwise `None`.
    pub retired_at: Option<String>,
}

/// Operator-supplied BOM line at author time (no id; the repository
/// mints `bml_<ULID>`).
#[derive(Debug, Clone, Deserialize)]
pub struct BomLineInput {
    pub component_id: String,
    pub qty_per_unit: Decimal,
}

/// DoS bound per [[trust-code-not-operator]] — the POST author route
/// caps the number of lines per request to defend against a malicious
/// client sending an enormous body.
pub const MAX_BOM_LINES_PER_REQUEST: usize = 200;

/// List the ACTIVE BOM lines for a product (rows where
/// `retired_at IS NULL`). Returned in stable order so the SPA renders
/// deterministically.
pub fn list_active_bom_for_product(
    conn: &Connection,
    tenant: &str,
    product_id: &str,
) -> anyhow::Result<Vec<BomLine>> {
    let mut stmt = conn.prepare(
        "SELECT bom_line_id, product_id, component_id,
                CAST(qty_per_unit AS VARCHAR), created_at, retired_at
         FROM boms
         WHERE tenant_id = ? AND product_id = ? AND retired_at IS NULL
         ORDER BY created_at ASC, bom_line_id ASC;",
    )?;
    let rows = stmt.query_map(params![tenant, product_id], |row| {
        let bom_line_id: String = row.get(0)?;
        let product_id: String = row.get(1)?;
        let component_id: String = row.get(2)?;
        let qty_str: String = row.get(3)?;
        let created_at: String = row.get(4)?;
        let retired_at: Option<String> = row.get(5)?;
        Ok((
            bom_line_id,
            product_id,
            component_id,
            qty_str,
            created_at,
            retired_at,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (bom_line_id, product_id, component_id, qty_str, created_at, retired_at) = r?;
        out.push(BomLine {
            bom_line_id,
            product_id,
            component_id,
            qty_per_unit: Decimal::from_str(&qty_str)
                .with_context(|| format!("parse qty_per_unit {qty_str:?}"))?,
            created_at,
            retired_at,
        });
    }
    Ok(out)
}

/// Author a new BOM for a product. Soft-retires any prior active rows
/// (sets their `retired_at` to now) and inserts the new lines per
/// ADR-0062 §6 — the table is append-only by application discipline.
///
/// Per ADR-0062: no audit kind for BOM mutations in v1 (it's reference
/// data, not regulated state).
pub fn replace_bom_for_product(
    tx: &Transaction<'_>,
    tenant: &str,
    product_id: &str,
    lines: &[BomLineInput],
) -> Result<Vec<BomLine>, WorkOrderError> {
    if lines.len() > MAX_BOM_LINES_PER_REQUEST {
        return Err(WorkOrderError::Validation(format!(
            "BOM has {} lines; max is {MAX_BOM_LINES_PER_REQUEST}",
            lines.len()
        )));
    }
    // Validate every line — zero / negative qty is structurally
    // meaningless (a Release would consume 0 of a component).
    for (i, line) in lines.iter().enumerate() {
        if line.qty_per_unit <= Decimal::ZERO {
            return Err(WorkOrderError::Validation(format!(
                "line {i}: qty_per_unit must be > 0, got {}",
                line.qty_per_unit
            )));
        }
        if line.component_id.trim().is_empty() {
            return Err(WorkOrderError::Validation(format!(
                "line {i}: component_id is empty"
            )));
        }
    }

    // Product-must-exist gate; we do NOT lean on a FK per
    // [[no-sql-specific]]. A typo'd product_id would otherwise mint
    // BOM rows pointing at nothing.
    let product_exists: bool = tx
        .query_row(
            "SELECT 1 FROM products
             WHERE tenant_id = ? AND id = ? AND deleted_at IS NULL
             LIMIT 1;",
            params![tenant, product_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|_| true)
        .unwrap_or(false);
    if !product_exists {
        return Err(WorkOrderError::ProductNotFound(product_id.to_string()));
    }

    let now = now_rfc3339()?;

    // Retire any prior active rows.
    tx.execute(
        "UPDATE boms SET retired_at = ?
         WHERE tenant_id = ? AND product_id = ? AND retired_at IS NULL;",
        params![&now, tenant, product_id],
    )
    .map_err(|e| WorkOrderError::Storage(anyhow!("retire prior BOM rows: {e}")))?;

    // Insert the new lines.
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let bom_line_id = format!("bml_{}", Ulid::new());
        tx.execute(
            "INSERT INTO boms (
                bom_line_id, tenant_id, product_id, component_id,
                qty_per_unit, created_at, retired_at
             ) VALUES (?, ?, ?, ?, ?, ?, NULL);",
            params![
                &bom_line_id,
                tenant,
                product_id,
                &line.component_id,
                line.qty_per_unit.to_string(),
                &now,
            ],
        )
        .map_err(|e| WorkOrderError::Storage(anyhow!("INSERT BOM line: {e}")))?;
        out.push(BomLine {
            bom_line_id,
            product_id: product_id.to_string(),
            component_id: line.component_id.clone(),
            qty_per_unit: line.qty_per_unit,
            created_at: now.clone(),
            retired_at: None,
        });
    }
    Ok(out)
}

// ── Work order (ADR-0062 §1, §3, §5) ────────────────────────────────

/// One row from `work_orders`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkOrder {
    /// `wo_<ULID>`.
    pub wo_id: String,
    /// Operator-visible WO number (e.g. `WO-2026-0042`).
    pub wo_number: String,
    pub product_id: String,
    pub qty_target: Decimal,
    pub state: WorkOrderState,
    pub created_at: String,
    pub released_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub hold_reason: Option<String>,
    pub notes: Option<String>,
}

/// One row from `routings`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingOp {
    /// `rop_<ULID>`.
    pub routing_op_id: String,
    pub wo_id: String,
    pub sequence: i32,
    pub op_name: String,
    pub est_time_min: Option<i32>,
    pub est_cost_huf: Option<Decimal>,
    pub state: RoutingOpState,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Operator-supplied routing-op input at WO-create time (no id;
/// repository mints `rop_<ULID>`).
#[derive(Debug, Clone, Deserialize)]
pub struct RoutingOpInput {
    pub op_name: String,
    pub est_time_min: Option<i32>,
    pub est_cost_huf: Option<Decimal>,
}

/// Inputs to [`create_work_order`]. Per ADR-0062 §"SPA surface" the
/// operator supplies product_id + qty_target + routing-op rows.
/// `wo_number` is operator-supplied; the route layer mints a default
/// when the SPA leaves it blank (ADR-0062 §"Out of scope" — auto-
/// numbering is an extension, v1 is operator-typed).
#[derive(Debug, Clone)]
pub struct CreateWorkOrderInputs {
    pub wo_number: String,
    pub product_id: String,
    pub qty_target: Decimal,
    pub notes: Option<String>,
    pub routing_ops: Vec<RoutingOpInput>,
    pub idempotency_key: String,
}

/// DoS bound per [[trust-code-not-operator]] — the POST create route
/// caps the routing-op count per request.
pub const MAX_ROUTING_OPS_PER_WO: usize = 200;

/// Context for create / transition write paths. The caller (route
/// layer / future adapter handler) supplies the actor + audit-ledger
/// meta; the repository writes everything in one tx.
#[derive(Debug)]
pub struct WoWriteContext<'a> {
    pub tenant: &'a str,
    pub actor: aberp_inventory::ActorKind,
    pub ledger_meta: &'a LedgerMeta,
    pub ledger_actor: Actor,
}

/// Create a WO + its routing operations + emit one
/// `WorkOrderCreated` audit entry, all in the supplied transaction.
///
/// Validation per CLAUDE.md rule 12 (fail loud):
///   - `qty_target > 0`
///   - `routing_ops.len() >= 1` and `<= MAX_ROUTING_OPS_PER_WO`
///   - every `op_name` non-empty
///   - product_id exists in the tenant
///   - `wo_number` unique within the tenant (allocator-style)
///   - idempotency_key not previously used for a WO-create entry
pub fn create_work_order(
    tx: &Transaction<'_>,
    ctx: &WoWriteContext<'_>,
    inputs: CreateWorkOrderInputs,
) -> Result<(WorkOrder, Vec<RoutingOp>), WorkOrderError> {
    if inputs.qty_target <= Decimal::ZERO {
        return Err(WorkOrderError::Validation(format!(
            "qty_target must be > 0, got {}",
            inputs.qty_target
        )));
    }
    if inputs.routing_ops.is_empty() {
        return Err(WorkOrderError::Validation(
            "WO must have at least one routing operation".to_string(),
        ));
    }
    if inputs.routing_ops.len() > MAX_ROUTING_OPS_PER_WO {
        return Err(WorkOrderError::Validation(format!(
            "WO has {} routing ops; max is {MAX_ROUTING_OPS_PER_WO}",
            inputs.routing_ops.len()
        )));
    }
    for (i, op) in inputs.routing_ops.iter().enumerate() {
        if op.op_name.trim().is_empty() {
            return Err(WorkOrderError::Validation(format!(
                "routing_ops[{i}]: op_name is empty"
            )));
        }
    }
    if inputs.wo_number.trim().is_empty() {
        return Err(WorkOrderError::Validation(
            "wo_number must be non-empty".to_string(),
        ));
    }
    if inputs.idempotency_key.trim().is_empty() {
        return Err(WorkOrderError::Validation(
            "idempotency_key must be non-empty".to_string(),
        ));
    }

    // Product-must-exist gate per [[no-sql-specific]].
    let product_exists: bool = tx
        .query_row(
            "SELECT 1 FROM products
             WHERE tenant_id = ? AND id = ? AND deleted_at IS NULL
             LIMIT 1;",
            params![ctx.tenant, &inputs.product_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|_| true)
        .unwrap_or(false);
    if !product_exists {
        return Err(WorkOrderError::ProductNotFound(inputs.product_id.clone()));
    }

    // wo_number uniqueness probe inside the same tx (the allocator
    // gate per ADR-0062 §1's index commentary).
    let wo_number_exists: bool = tx
        .query_row(
            "SELECT 1 FROM work_orders
             WHERE tenant_id = ? AND wo_number = ? LIMIT 1;",
            params![ctx.tenant, &inputs.wo_number],
            |row| row.get::<_, i64>(0),
        )
        .map(|_| true)
        .unwrap_or(false);
    if wo_number_exists {
        return Err(WorkOrderError::Validation(format!(
            "wo_number {:?} already exists in this tenant",
            inputs.wo_number
        )));
    }

    let now = now_rfc3339()?;
    let wo_id = format!("wo_{}", Ulid::new());

    // 1. INSERT work_orders.
    tx.execute(
        "INSERT INTO work_orders (
            wo_id, tenant_id, wo_number, product_id, qty_target,
            state, created_at, released_at, started_at, completed_at,
            cancelled_at, hold_reason, notes
         ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, NULL, ?);",
        params![
            &wo_id,
            ctx.tenant,
            inputs.wo_number.trim(),
            &inputs.product_id,
            inputs.qty_target.to_string(),
            WorkOrderState::Created.as_str(),
            &now,
            inputs.notes.as_deref(),
        ],
    )
    .map_err(|e| WorkOrderError::Storage(anyhow!("INSERT work_orders: {e}")))?;

    // 2. INSERT routings — one row per op, sequence 1..N.
    let mut routing_ops_out = Vec::with_capacity(inputs.routing_ops.len());
    let mut routing_op_ids = Vec::with_capacity(inputs.routing_ops.len());
    for (i, op) in inputs.routing_ops.iter().enumerate() {
        let rop_id = format!("rop_{}", Ulid::new());
        let sequence = (i as i32) + 1;
        tx.execute(
            "INSERT INTO routings (
                routing_op_id, tenant_id, wo_id, sequence, op_name,
                est_time_min, est_cost_huf, state, started_at, completed_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL);",
            params![
                &rop_id,
                ctx.tenant,
                &wo_id,
                sequence,
                op.op_name.trim(),
                op.est_time_min,
                op.est_cost_huf.map(|d| d.to_string()),
                RoutingOpState::Pending.as_str(),
            ],
        )
        .map_err(|e| WorkOrderError::Storage(anyhow!("INSERT routing: {e}")))?;
        routing_op_ids.push(rop_id.clone());
        routing_ops_out.push(RoutingOp {
            routing_op_id: rop_id,
            wo_id: wo_id.clone(),
            sequence,
            op_name: op.op_name.trim().to_string(),
            est_time_min: op.est_time_min,
            est_cost_huf: op.est_cost_huf,
            state: RoutingOpState::Pending,
            started_at: None,
            completed_at: None,
        });
    }

    // 3. Emit audit-ledger entry.
    let actor_str = ctx.actor.as_operator_string();
    let payload = WorkOrderCreatedPayload {
        wo_id: wo_id.clone(),
        wo_number: inputs.wo_number.trim().to_string(),
        product_id: inputs.product_id.clone(),
        qty_target: inputs.qty_target,
        routing_op_ids,
        actor: actor_str,
        idempotency_key: inputs.idempotency_key.clone(),
    };
    append_in_tx(
        tx,
        ctx.ledger_meta,
        EventKind::WorkOrderCreated,
        payload.to_bytes(),
        ctx.ledger_actor.clone(),
        Some(format!("create:{}", inputs.idempotency_key)),
    )
    .map_err(|e| WorkOrderError::Storage(anyhow!("audit append WorkOrderCreated: {e}")))?;

    let wo = WorkOrder {
        wo_id,
        wo_number: inputs.wo_number.trim().to_string(),
        product_id: inputs.product_id,
        qty_target: inputs.qty_target,
        state: WorkOrderState::Created,
        created_at: now,
        released_at: None,
        started_at: None,
        completed_at: None,
        cancelled_at: None,
        hold_reason: None,
        notes: inputs.notes,
    };
    Ok((wo, routing_ops_out))
}

/// Outcome of a transition. The handler may report warnings
/// (e.g. insufficient component stock on Release per ADR-0061's
/// negative-stock policy + ADR-0062 §5) without refusing the
/// transition — the route layer surfaces them on the response.
#[derive(Debug, Clone)]
pub struct WorkOrderTransitionOutcome {
    pub wo: WorkOrder,
    pub warnings: Vec<String>,
}

/// Inputs to [`transition_work_order`].
#[derive(Debug, Clone)]
pub struct TransitionInputs {
    pub action: WoAction,
    /// Optional operator-supplied reason. Required-ish for `Hold`
    /// (stored on `work_orders.hold_reason`); free-form note for
    /// other actions.
    pub reason: Option<String>,
    /// **Load-bearing** per ADR-0062 §4 + invariant 7. Always
    /// supplied explicitly: `None` for SPA-button-driven
    /// transitions, `Some(ULID)` for adapter-driven transitions.
    pub source_event_id: Option<String>,
    pub idempotency_key: String,
}

/// Transition a WO state per ADR-0062 §3. SPA buttons and future
/// adapter events both call this function — actor is captured into
/// the audit entry only; the state-transition logic does NOT branch
/// on actor (ADR-0062 invariant 6).
///
/// On Released:  emits one `BomConsumption` `stock_movement` per
///               active BOM row (ADR-0062 §5). Stamps `released_at`.
/// On Completed: emits one `WoCompletion` `stock_movement` for the
///               finished good (ADR-0062 §5). Stamps `completed_at`.
/// On Cancelled: no automatic stock reversal (ADR-0062 §5 paragraph 3).
/// On OnHold:    stamps `hold_reason` from `inputs.reason`.
pub fn transition_work_order(
    tx: &Transaction<'_>,
    ctx: &WoWriteContext<'_>,
    wo_id: &str,
    inputs: TransitionInputs,
) -> Result<WorkOrderTransitionOutcome, WorkOrderError> {
    if inputs.idempotency_key.trim().is_empty() {
        return Err(WorkOrderError::Validation(
            "idempotency_key must be non-empty".to_string(),
        ));
    }

    // Read the current state inside the tx for the optimistic
    // concurrency check per ADR-0062 §"Adversarial review" #5.
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT state, product_id, CAST(qty_target AS VARCHAR)
             FROM work_orders
             WHERE tenant_id = ? AND wo_id = ? LIMIT 1;",
            params![ctx.tenant, wo_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            duckdb::Error::QueryReturnedNoRows => Ok(None),
            other => Err(anyhow!("read work_orders for transition: {other}")),
        })?;
    let (current_state_str, product_id, qty_target_str) =
        row.ok_or_else(|| WorkOrderError::WorkOrderNotFound(wo_id.to_string()))?;
    let current_state = WorkOrderState::from_storage_str(&current_state_str)
        .map_err(|e| WorkOrderError::Storage(anyhow!("{e}: {current_state_str:?}")))?;
    let qty_target = Decimal::from_str(&qty_target_str).map_err(|e| {
        WorkOrderError::Storage(anyhow!("parse qty_target {qty_target_str:?}: {e}"))
    })?;

    // Refuse illegal edges loud per ADR-0062 §2's transition table.
    let next = next_state(current_state, inputs.action)
        .map_err(|e| WorkOrderError::IllegalTransition(format!("{e}")))?;

    let mut warnings: Vec<String> = Vec::new();

    // ── Action-specific side effects ────────────────────────────
    let now = now_rfc3339()?;
    match inputs.action {
        WoAction::Release => {
            // Snapshot active BOM rows + emit one BomConsumption
            // movement per row, all in this tx (ADR-0062 §5).
            let bom = list_active_bom_for_product(tx, ctx.tenant, &product_id)
                .map_err(|e| WorkOrderError::Storage(anyhow!("read BOM for release: {e}")))?;
            if bom.is_empty() {
                return Err(WorkOrderError::NoActiveBomForProduct(product_id.clone()));
            }
            for line in &bom {
                let qty_delta = -(line.qty_per_unit * qty_target);
                let movement_ctx = aberp_inventory::RecordMovementContext {
                    tenant: ctx.tenant,
                    actor: ctx.actor.clone(),
                    ledger_meta: ctx.ledger_meta,
                    ledger_actor: ctx.ledger_actor.clone(),
                };
                let movement_inputs = aberp_inventory::RecordMovementInputs {
                    product_id: line.component_id.clone(),
                    qty_delta,
                    reason: aberp_inventory::MovementReason::BomConsumption,
                    ref_kind: aberp_inventory::MovementRefKind::WorkOrder,
                    ref_id: Some(wo_id.to_string()),
                    notes: None,
                    idempotency_key: format!(
                        "{}:release:{}",
                        inputs.idempotency_key, line.bom_line_id
                    ),
                };
                aberp_inventory::record_movement(tx, &movement_ctx, movement_inputs)
                    .map_err(map_inventory_err_into_wo)?;
                // Negative-stock warning per ADR-0061 §"Adversarial
                // review" #3 + ADR-0062 §5: allow the movement but
                // surface a heads-up. Read the cache AFTER the
                // movement landed in the same tx.
                if let Ok(Some(cur)) =
                    aberp_inventory::current_stock(tx, ctx.tenant, &line.component_id)
                {
                    if cur < Decimal::ZERO {
                        warnings.push(format!(
                            "component {} stock is now {} (allowed by ADR-0061 negative-stock policy)",
                            line.component_id, cur
                        ));
                    }
                }
            }
        }
        WoAction::Complete => {
            // Emit one positive WoCompletion movement for the
            // finished good per ADR-0062 §5.
            let movement_ctx = aberp_inventory::RecordMovementContext {
                tenant: ctx.tenant,
                actor: ctx.actor.clone(),
                ledger_meta: ctx.ledger_meta,
                ledger_actor: ctx.ledger_actor.clone(),
            };
            let movement_inputs = aberp_inventory::RecordMovementInputs {
                product_id: product_id.clone(),
                qty_delta: qty_target,
                reason: aberp_inventory::MovementReason::WoCompletion,
                ref_kind: aberp_inventory::MovementRefKind::WorkOrder,
                ref_id: Some(wo_id.to_string()),
                notes: None,
                idempotency_key: format!("{}:complete", inputs.idempotency_key),
            };
            aberp_inventory::record_movement(tx, &movement_ctx, movement_inputs)
                .map_err(map_inventory_err_into_wo)?;
        }
        WoAction::Start | WoAction::Cancel | WoAction::Hold | WoAction::Resume => {
            // No inventory side-effects per ADR-0062 §5.
        }
    }

    // ── Stamp the row + audit-ledger entry ──────────────────────
    let new_state = next;
    // Pick the timestamp column to update based on the destination
    // state. `hold_reason` is set only when transitioning into
    // OnHold; cleared when leaving it.
    let released_at: Option<String>;
    let completed_at: Option<String>;
    let cancelled_at: Option<String>;
    let hold_reason: Option<String>;
    match new_state {
        WorkOrderState::Released => {
            released_at = Some(now.clone());
            completed_at = None;
            cancelled_at = None;
            hold_reason = None;
        }
        WorkOrderState::InProgress => {
            // started_at is stamped via the CASE-WHEN in the UPDATE
            // below — set only on the FIRST entry into InProgress
            // (coming from Released). Resuming from OnHold leaves it
            // as-is.
            released_at = None;
            completed_at = None;
            cancelled_at = None;
            hold_reason = None;
        }
        WorkOrderState::Completed => {
            released_at = None;
            completed_at = Some(now.clone());
            cancelled_at = None;
            hold_reason = None;
        }
        WorkOrderState::Cancelled => {
            released_at = None;
            completed_at = None;
            cancelled_at = Some(now.clone());
            hold_reason = None;
        }
        WorkOrderState::OnHold => {
            released_at = None;
            completed_at = None;
            cancelled_at = None;
            hold_reason = inputs.reason.clone();
        }
        WorkOrderState::Created => unreachable!("no transition lands in Created"),
    };

    // Set state + clear hold_reason on every transition out of OnHold.
    // The match above sets hold_reason to Some(..) only for
    // OnHold-destination transitions; for everything else we clear it.
    let clear_hold_reason = !matches!(new_state, WorkOrderState::OnHold);

    tx.execute(
        "UPDATE work_orders SET
            state         = ?,
            released_at   = COALESCE(released_at, ?),
            started_at    = CASE WHEN ? IS NOT NULL AND ?::VARCHAR = 'in_progress'
                                   AND started_at IS NULL
                                THEN ? ELSE started_at END,
            completed_at  = COALESCE(completed_at, ?),
            cancelled_at  = COALESCE(cancelled_at, ?),
            hold_reason   = CASE WHEN ? THEN NULL
                                 ELSE COALESCE(?, hold_reason) END
         WHERE tenant_id = ? AND wo_id = ?;",
        params![
            new_state.as_str(),
            released_at.as_deref(),
            // CASE-WHEN args: only stamp started_at when entering
            // InProgress AND the column is still NULL (first entry).
            // The 1-arg sentinel is "1" or "null"; we pass the
            // destination state's storage string and check it inside
            // the CASE.
            Some(now.clone()),
            new_state.as_str(),
            Some(now.clone()),
            completed_at.as_deref(),
            cancelled_at.as_deref(),
            clear_hold_reason,
            hold_reason.as_deref(),
            ctx.tenant,
            wo_id,
        ],
    )
    .map_err(|e| WorkOrderError::Storage(anyhow!("UPDATE work_orders: {e}")))?;

    // Audit entry.
    let actor_str = ctx.actor.as_operator_string();
    let payload = WorkOrderStateChangedPayload {
        wo_id: wo_id.to_string(),
        from_state: current_state,
        to_state: new_state,
        reason: inputs.reason.clone(),
        actor: actor_str,
        source_event_id: inputs.source_event_id.clone(),
        idempotency_key: inputs.idempotency_key.clone(),
    };
    append_in_tx(
        tx,
        ctx.ledger_meta,
        EventKind::WorkOrderStateChanged,
        payload.to_bytes(),
        ctx.ledger_actor.clone(),
        Some(format!(
            "transition:{}:{}",
            inputs.action.as_str(),
            inputs.idempotency_key
        )),
    )
    .map_err(|e| WorkOrderError::Storage(anyhow!("audit append WorkOrderStateChanged: {e}")))?;

    // Read back the updated row for the outcome.
    let wo = read_work_order(tx, ctx.tenant, wo_id)?
        .ok_or_else(|| WorkOrderError::WorkOrderNotFound(wo_id.to_string()))?;
    Ok(WorkOrderTransitionOutcome { wo, warnings })
}

fn map_inventory_err_into_wo(e: aberp_inventory::InventoryError) -> WorkOrderError {
    use aberp_inventory::InventoryError as IE;
    match e {
        IE::DuplicateIdempotencyKey(k) => WorkOrderError::DuplicateIdempotencyKey(k),
        IE::ProductNotFound(p) => WorkOrderError::ProductNotFound(p),
        IE::WrongSignForReason {
            reason,
            required,
            got,
        } => WorkOrderError::Validation(format!(
            "stock movement sign-violation: reason {reason} requires {required:?}, got {got}"
        )),
        IE::Storage(e) => WorkOrderError::Storage(e),
    }
}

/// Read a single WO row by id, scoped to the tenant. `None` for unknown.
pub fn read_work_order(
    conn: &(impl WorkOrderReader + ?Sized),
    tenant: &str,
    wo_id: &str,
) -> Result<Option<WorkOrder>, WorkOrderError> {
    conn.read_wo(tenant, wo_id)
}

/// Internal helper so both `Connection` and `Transaction` can be used
/// as the read backend. Mirrors the pattern aberp-inventory uses
/// (`current_stock` takes &Connection but Transaction implements
/// `&Connection`-compatible methods).
pub trait WorkOrderReader {
    fn read_wo(&self, tenant: &str, wo_id: &str) -> Result<Option<WorkOrder>, WorkOrderError>;
}

fn read_wo_inner(
    query_one: impl FnOnce(
        &str,
        &[&dyn duckdb::ToSql],
    ) -> duckdb::Result<
        Option<(
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    >,
    tenant: &str,
    wo_id: &str,
) -> Result<Option<WorkOrder>, WorkOrderError> {
    let row = query_one(
        "SELECT wo_id, wo_number, product_id, CAST(qty_target AS VARCHAR),
                state, created_at, released_at, started_at,
                completed_at, cancelled_at, hold_reason, notes
         FROM work_orders WHERE tenant_id = ? AND wo_id = ? LIMIT 1;",
        &[&tenant, &wo_id],
    )
    .map_err(|e| WorkOrderError::Storage(anyhow!("SELECT work_orders: {e}")))?;
    match row {
        None => Ok(None),
        Some((
            wo_id,
            wo_number,
            product_id,
            qty_target_str,
            state_str,
            created_at,
            released_at,
            started_at,
            completed_at,
            cancelled_at,
            hold_reason,
            notes,
        )) => Ok(Some(WorkOrder {
            wo_id,
            wo_number,
            product_id,
            qty_target: Decimal::from_str(&qty_target_str)
                .map_err(|e| WorkOrderError::Storage(anyhow!("parse qty_target: {e}")))?,
            state: WorkOrderState::from_storage_str(&state_str)
                .map_err(|e| WorkOrderError::Storage(anyhow!("{e}: {state_str:?}")))?,
            created_at,
            released_at,
            started_at,
            completed_at,
            cancelled_at,
            hold_reason,
            notes,
        })),
    }
}

impl WorkOrderReader for Connection {
    fn read_wo(&self, tenant: &str, wo_id: &str) -> Result<Option<WorkOrder>, WorkOrderError> {
        read_wo_inner(
            |sql, params| {
                self.query_row(sql, params, |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                })
                .map(Some)
                .or_else(|e| match e {
                    duckdb::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
            },
            tenant,
            wo_id,
        )
    }
}

impl WorkOrderReader for Transaction<'_> {
    fn read_wo(&self, tenant: &str, wo_id: &str) -> Result<Option<WorkOrder>, WorkOrderError> {
        read_wo_inner(
            |sql, params| {
                self.query_row(sql, params, |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                        row.get(11)?,
                    ))
                })
                .map(Some)
                .or_else(|e| match e {
                    duckdb::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
            },
            tenant,
            wo_id,
        )
    }
}

/// List WOs in the tenant, optionally filtering by state. Ordered by
/// `created_at DESC, wo_id DESC` so the most-recent shows first.
pub fn list_work_orders(
    conn: &Connection,
    tenant: &str,
    state_filter: Option<WorkOrderState>,
    limit: u32,
    offset: u32,
) -> anyhow::Result<Vec<WorkOrder>> {
    let mut sql = String::from(
        "SELECT wo_id, wo_number, product_id, CAST(qty_target AS VARCHAR),
                state, created_at, released_at, started_at,
                completed_at, cancelled_at, hold_reason, notes
         FROM work_orders WHERE tenant_id = ?",
    );
    if state_filter.is_some() {
        sql.push_str(" AND state = ?");
    }
    sql.push_str(" ORDER BY created_at DESC, wo_id DESC LIMIT ? OFFSET ?;");

    let mut stmt = conn.prepare(&sql)?;
    let rows_iter: Vec<WorkOrder> = match state_filter {
        Some(s) => {
            let rows = stmt.query_map(params![tenant, s.as_str(), limit, offset], row_to_wo)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r??);
            }
            out
        }
        None => {
            let rows = stmt.query_map(params![tenant, limit, offset], row_to_wo)?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r??);
            }
            out
        }
    };
    Ok(rows_iter)
}

#[allow(clippy::type_complexity)]
fn row_to_wo(row: &duckdb::Row<'_>) -> duckdb::Result<anyhow::Result<WorkOrder>> {
    let wo_id: String = row.get(0)?;
    let wo_number: String = row.get(1)?;
    let product_id: String = row.get(2)?;
    let qty_target_str: String = row.get(3)?;
    let state_str: String = row.get(4)?;
    let created_at: String = row.get(5)?;
    let released_at: Option<String> = row.get(6)?;
    let started_at: Option<String> = row.get(7)?;
    let completed_at: Option<String> = row.get(8)?;
    let cancelled_at: Option<String> = row.get(9)?;
    let hold_reason: Option<String> = row.get(10)?;
    let notes: Option<String> = row.get(11)?;

    let parse = || -> anyhow::Result<WorkOrder> {
        Ok(WorkOrder {
            wo_id,
            wo_number,
            product_id,
            qty_target: Decimal::from_str(&qty_target_str)
                .with_context(|| format!("parse qty_target {qty_target_str:?}"))?,
            state: WorkOrderState::from_storage_str(&state_str)
                .map_err(|e| anyhow!("{e}: {state_str:?}"))?,
            created_at,
            released_at,
            started_at,
            completed_at,
            cancelled_at,
            hold_reason,
            notes,
        })
    };
    Ok(parse())
}

/// List the routing operations for a WO, ordered by sequence.
pub fn list_routing_ops_for_wo(
    conn: &Connection,
    tenant: &str,
    wo_id: &str,
) -> anyhow::Result<Vec<RoutingOp>> {
    let mut stmt = conn.prepare(
        "SELECT routing_op_id, wo_id, sequence, op_name,
                est_time_min, CAST(est_cost_huf AS VARCHAR),
                state, started_at, completed_at
         FROM routings
         WHERE tenant_id = ? AND wo_id = ?
         ORDER BY sequence ASC, routing_op_id ASC;",
    )?;
    let rows = stmt.query_map(params![tenant, wo_id], |row| {
        let routing_op_id: String = row.get(0)?;
        let wo_id: String = row.get(1)?;
        let sequence: i32 = row.get(2)?;
        let op_name: String = row.get(3)?;
        let est_time_min: Option<i32> = row.get(4)?;
        let est_cost_huf_str: Option<String> = row.get(5)?;
        let state_str: String = row.get(6)?;
        let started_at: Option<String> = row.get(7)?;
        let completed_at: Option<String> = row.get(8)?;
        Ok((
            routing_op_id,
            wo_id,
            sequence,
            op_name,
            est_time_min,
            est_cost_huf_str,
            state_str,
            started_at,
            completed_at,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (
            routing_op_id,
            wo_id,
            sequence,
            op_name,
            est_time_min,
            est_cost_huf_str,
            state_str,
            started_at,
            completed_at,
        ) = r?;
        let est_cost_huf = match est_cost_huf_str {
            Some(s) => {
                Some(Decimal::from_str(&s).with_context(|| format!("parse est_cost {s:?}"))?)
            }
            None => None,
        };
        out.push(RoutingOp {
            routing_op_id,
            wo_id,
            sequence,
            op_name,
            est_time_min,
            est_cost_huf,
            state: RoutingOpState::from_storage_str(&state_str)
                .map_err(|e| anyhow!("{e}: {state_str:?}"))?,
            started_at,
            completed_at,
        });
    }
    Ok(out)
}

fn now_rfc3339() -> anyhow::Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| anyhow!("format Rfc3339: {e}"))
}
