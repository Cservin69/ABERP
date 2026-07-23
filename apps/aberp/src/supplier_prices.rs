//! ADR-0104 — vendor/supplier price ingestion for auto-quoting, plus the
//! per-quote **price-snapshot reproducibility pin**.
//!
//! Two concerns, one "as-of-when" story:
//!
//! **(A) Ingestion.** [`parse_price_list`] parses a real-shaped CSV vendor
//! price list (`grade,cost_per_kg[,currency]`) and validates it loudly.
//! [`ingest_price_list`] normalises currency to EUR (reusing
//! [`aberp_billing::Currency`] and a caller-supplied, pinned MNB rate — no
//! silent fallback, ADR-0037) and applies it to `quoting_materials.cost_per_kg_eur`
//! in ONE transaction on the shared [`aberp_db::Handle`] writer, audited
//! per grade via [`crate::quoting_materials::set_cost_in_tx`]
//! (`MaterialCatalogueChanged`, `op="supplier_ingest"`). Unknown grades
//! reject the WHOLE batch (all-or-nothing; CLAUDE.md rule 11) — nothing is
//! silently skipped.
//!
//! **(B) Reproducibility pin.** [`record_price_set`] captures the exact
//! `(grade → cost_per_kg_eur)` set a quote priced against into the
//! content-addressed `quote_price_snapshots` table, keyed by
//! [`price_set_hash`] (FNV-1a, identical construction to S429
//! `CalibrationTable::set_hash`). The pipeline stamps that hash on the
//! priced quote; [`resolve_price_set`] re-derives the exact prices later, so
//! **re-running a quote yields the same number even after prices change** —
//! the same guarantee an issued invoice has.
//!
//! Neither concern opens a DB connection: the library takes `&Connection` /
//! `&Transaction` (all access via the shared `Handle`), so the ADR-0099
//! opener census does not move.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use duckdb::{params, Connection, Transaction};

use aberp_audit_ledger::LedgerMeta;
use aberp_billing::Currency;
use ulid::Ulid;

// ─────────────────────────────────────────────────────────────────────────
// (A) INGESTION
// ─────────────────────────────────────────────────────────────────────────

/// One validated CSV row: a grade and its per-kg cost in `currency`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedPriceRow {
    pub grade: String,
    pub cost_per_kg: f64,
    pub currency: Currency,
}

/// A single problem found while parsing, tagged with the 1-based source line
/// (the header is line 1) so the operator can find it. Collected in full — a
/// list is returned, never a fail-on-first (mirrors `validate_material_inputs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowError {
    pub line: usize,
    pub message: String,
}

#[derive(Debug)]
pub enum ParseError {
    /// The header row is missing a required column (`grade` or the cost
    /// column) — the whole file is unusable.
    Header(String),
    /// One or more data rows failed validation. Every problem is listed.
    Rows(Vec<RowError>),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Header(m) => write!(f, "price-list header error: {m}"),
            ParseError::Rows(rows) => {
                write!(f, "price-list has {} invalid row(s):", rows.len())?;
                for r in rows {
                    write!(f, " [line {}: {}]", r.line, r.message)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Map a CSV currency cell to the billing enum. Case-insensitive; only the
/// two currencies ABERP prices in are accepted (EUR native, HUF normalised).
fn parse_currency(s: &str) -> Option<Currency> {
    match s.trim().to_ascii_uppercase().as_str() {
        "EUR" => Some(Currency::Eur),
        "HUF" => Some(Currency::Huf),
        _ => None,
    }
}

/// Split a CSV line into trimmed cells. Deliberately simple: comma-separated,
/// no quoted-field / embedded-comma support — material grades never contain
/// commas (`6061-T6`, `Ti-6Al-4V`, `Inconel 718`). Flagged in ADR-0104 §2.1.
fn split_cells(line: &str) -> Vec<String> {
    line.split(',').map(|c| c.trim().to_string()).collect()
}

/// Parse + validate a CSV supplier price list.
///
/// Header (line 1) must name a `grade` column and a cost column (one of
/// `cost_per_kg` / `cost_per_kg_eur` / `cost`); a `currency` column is
/// optional. Rows missing a per-row currency fall back to `default_currency`.
/// Blank lines and `#`-prefixed comment lines are ignored. Duplicate grades
/// are rejected (ambiguous price). Costs must be finite and `>= 0`.
pub fn parse_price_list(
    csv: &str,
    default_currency: Currency,
) -> Result<Vec<ParsedPriceRow>, ParseError> {
    // Enumerate with 1-based physical line numbers for error messages, then
    // drop blanks / comments.
    let mut logical = csv
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        });

    let (header_line, header_raw) = match logical.next() {
        Some(h) => h,
        None => return Err(ParseError::Header("price list is empty".to_string())),
    };
    let header: Vec<String> = split_cells(header_raw)
        .into_iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();

    let grade_idx = header
        .iter()
        .position(|h| h == "grade")
        .ok_or_else(|| ParseError::Header(format!("no `grade` column (line {header_line})")))?;
    let cost_idx = header
        .iter()
        .position(|h| matches!(h.as_str(), "cost_per_kg" | "cost_per_kg_eur" | "cost"))
        .ok_or_else(|| {
            ParseError::Header(format!(
                "no cost column (expected one of cost_per_kg / cost_per_kg_eur / cost) (line {header_line})"
            ))
        })?;
    let currency_idx = header.iter().position(|h| h == "currency");

    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let mut seen_grades: BTreeSet<String> = BTreeSet::new();

    for (line, raw) in logical {
        let cells = split_cells(raw);
        let needed = grade_idx.max(cost_idx).max(currency_idx.unwrap_or(0)) + 1;
        if cells.len() < needed {
            errors.push(RowError {
                line,
                message: format!("expected at least {needed} columns, got {}", cells.len()),
            });
            continue;
        }

        let grade = cells[grade_idx].clone();
        if grade.is_empty() {
            errors.push(RowError {
                line,
                message: "empty grade".to_string(),
            });
            continue;
        }
        if !seen_grades.insert(grade.clone()) {
            errors.push(RowError {
                line,
                message: format!("duplicate grade `{grade}`"),
            });
            continue;
        }

        let cost = match cells[cost_idx].parse::<f64>() {
            Ok(v) if v.is_finite() && v >= 0.0 => v,
            Ok(v) => {
                errors.push(RowError {
                    line,
                    message: format!("cost must be finite and >= 0 (got {v})"),
                });
                continue;
            }
            Err(_) => {
                errors.push(RowError {
                    line,
                    message: format!("cost `{}` is not a number", cells[cost_idx]),
                });
                continue;
            }
        };

        let currency = match currency_idx {
            Some(ci) if !cells[ci].is_empty() => match parse_currency(&cells[ci]) {
                Some(c) => c,
                None => {
                    errors.push(RowError {
                        line,
                        message: format!("unknown currency `{}` (expected EUR or HUF)", cells[ci]),
                    });
                    continue;
                }
            },
            _ => default_currency,
        };

        rows.push(ParsedPriceRow {
            grade,
            cost_per_kg: cost,
            currency,
        });
    }

    if !errors.is_empty() {
        return Err(ParseError::Rows(errors));
    }
    if rows.is_empty() {
        return Err(ParseError::Header(
            "price list has a header but no data rows".to_string(),
        ));
    }
    Ok(rows)
}

/// A pinned MNB EUR rate for normalising HUF rows to EUR at ingest.
/// `huf_per_eur` is HUF for one EUR (MNB `value / unit` for EUR); `rate_date`
/// is the MNB publication date that rate was read from. Passed IN by the
/// caller (the operator entry point owns the network fetch, ADR-0037 posture)
/// so ingest is deterministic and offline-testable.
#[derive(Debug, Clone, PartialEq)]
pub struct FxToEur {
    pub huf_per_eur: f64,
    pub rate_date: String,
}

/// One grade's applied change, for the ingest outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedPrice {
    pub grade: String,
    pub old_cost_per_kg_eur: f64,
    pub new_cost_per_kg_eur: f64,
}

/// Successful-ingest summary.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestOutcome {
    /// The ULID batch id stamped into every per-grade audit event.
    pub batch_id: String,
    pub applied: Vec<AppliedPrice>,
}

#[derive(Debug)]
pub enum IngestError {
    /// One or more grades in the list are not in `quoting_materials` for the
    /// tenant. The WHOLE batch is rejected; the sorted unique list is returned
    /// so the operator adds the grades to the catalogue first.
    UnknownGrades(Vec<String>),
    /// The list has HUF rows but no FX rate was supplied — refuse rather than
    /// guess a rate (ADR-0037; CLAUDE.md rule 11).
    MissingFxRate,
    /// The supplied FX rate is not a usable positive number.
    BadFxRate(f64),
    Other(anyhow::Error),
}

impl std::fmt::Display for IngestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IngestError::UnknownGrades(g) => {
                write!(f, "unknown grade(s) not in catalogue: {}", g.join(", "))
            }
            IngestError::MissingFxRate => {
                write!(f, "price list has HUF rows but no MNB FX rate was supplied")
            }
            IngestError::BadFxRate(v) => write!(f, "FX rate must be finite and > 0 (got {v})"),
            IngestError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for IngestError {}

impl From<anyhow::Error> for IngestError {
    fn from(e: anyhow::Error) -> Self {
        IngestError::Other(e)
    }
}

/// Normalise a row's native cost to EUR using the pinned FX rate.
fn to_eur(row: &ParsedPriceRow, fx: Option<&FxToEur>) -> Result<f64, IngestError> {
    match row.currency {
        Currency::Eur => Ok(row.cost_per_kg),
        Currency::Huf => {
            let fx = fx.ok_or(IngestError::MissingFxRate)?;
            if !fx.huf_per_eur.is_finite() || fx.huf_per_eur <= 0.0 {
                return Err(IngestError::BadFxRate(fx.huf_per_eur));
            }
            Ok(row.cost_per_kg / fx.huf_per_eur)
        }
    }
}

/// Ingest a parsed supplier price list into `quoting_materials.cost_per_kg_eur`.
///
/// All-or-nothing: if any grade is unknown, or any write fails, the whole
/// transaction rolls back and nothing is applied. Each applied grade emits a
/// `MaterialCatalogueChanged(op="supplier_ingest")` audit row in the SAME
/// transaction as its business write (CLAUDE.md rule 15).
///
/// `conn` is the shared `Handle` writer (a `&mut WriteGuard` derefs to
/// `&mut Connection`); the function opens ONE transaction on it.
#[allow(clippy::too_many_arguments)]
pub fn ingest_price_list(
    conn: &mut Connection,
    meta: &LedgerMeta,
    actor_login: &str,
    tenant: &str,
    source_label: &str,
    effective_at: &str,
    rows: &[ParsedPriceRow],
    fx: Option<&FxToEur>,
) -> Result<IngestOutcome, IngestError> {
    aberp_audit_ledger::ensure_schema(conn).context("ensure audit schema for supplier ingest")?;
    crate::quoting_materials::ensure_schema(conn)
        .context("ensure quoting_materials schema for supplier ingest")?;

    // Normalise every row to EUR up front — surfaces a missing/bad FX rate
    // before any write.
    let mut eur_rows: Vec<(&ParsedPriceRow, f64)> = Vec::with_capacity(rows.len());
    for row in rows {
        eur_rows.push((row, to_eur(row, fx)?));
    }

    // Load the tenant's known grades + current costs (for the outcome's
    // old→new) in one query, then reject the batch if any list grade is absent.
    let existing = load_existing_costs(conn, tenant).map_err(IngestError::Other)?;
    let unknown: Vec<String> = {
        let mut u: BTreeSet<String> = BTreeSet::new();
        for (row, _) in &eur_rows {
            if !existing.contains_key(&row.grade) {
                u.insert(row.grade.clone());
            }
        }
        u.into_iter().collect()
    };
    if !unknown.is_empty() {
        return Err(IngestError::UnknownGrades(unknown));
    }

    let batch_id = Ulid::new().to_string();
    let tx = conn
        .transaction()
        .context("begin supplier ingest tx")
        .map_err(IngestError::Other)?;

    let mut applied = Vec::with_capacity(eur_rows.len());
    for (row, cost_eur) in &eur_rows {
        let provenance = serde_json::json!({
            "source_label": source_label,
            "effective_at": effective_at,
            "batch_id": batch_id,
            "native_currency": row.currency.iso_code(),
            "cost_per_kg_native": row.cost_per_kg,
            "fx_rate_huf_per_eur": fx.map(|f| f.huf_per_eur),
            "fx_rate_date": fx.map(|f| f.rate_date.clone()),
        });
        let updated = crate::quoting_materials::set_cost_in_tx(
            &tx,
            meta,
            actor_login,
            tenant,
            &row.grade,
            *cost_eur,
            &provenance,
        )
        .map_err(IngestError::Other)?;
        // Pre-checked existence guarantees Some; treat None as a hard invariant
        // break rather than a silent skip.
        if updated.is_none() {
            return Err(IngestError::Other(anyhow::anyhow!(
                "grade `{}` vanished mid-ingest",
                row.grade
            )));
        }
        applied.push(AppliedPrice {
            grade: row.grade.clone(),
            old_cost_per_kg_eur: existing[&row.grade],
            new_cost_per_kg_eur: *cost_eur,
        });
    }

    tx.commit()
        .context("commit supplier ingest")
        .map_err(IngestError::Other)?;
    Ok(IngestOutcome { batch_id, applied })
}

fn load_existing_costs(conn: &Connection, tenant: &str) -> Result<BTreeMap<String, f64>> {
    let mut stmt = conn
        .prepare("SELECT grade, cost_per_kg_eur FROM quoting_materials WHERE tenant_id = ?;")
        .context("prepare load_existing_costs")?;
    let rows = stmt
        .query_map(params![tenant], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .context("query load_existing_costs")?;
    let mut out = BTreeMap::new();
    for r in rows {
        let (g, c) = r.context("read existing cost row")?;
        out.insert(g, c);
    }
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────
// (B) REPRODUCIBILITY PIN — content-addressed price snapshots
// ─────────────────────────────────────────────────────────────────────────

const SNAPSHOT_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS quote_price_snapshots (
    tenant_id        VARCHAR NOT NULL,
    price_set_hash   VARCHAR NOT NULL,
    grade            VARCHAR NOT NULL,
    cost_per_kg_eur  DOUBLE  NOT NULL,
    PRIMARY KEY (tenant_id, price_set_hash, grade)
);
";

/// Create the snapshot table (idempotent). Called at pipeline boot and at the
/// top of the readers/writers below.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SNAPSHOT_SCHEMA_SQL)
        .context("ensure quote_price_snapshots schema")
}

/// Stable content hash of a `(grade → cost_per_kg_eur)` price set — the pin a
/// priced quote carries. Deterministic FNV-1a over the BTreeMap's sorted
/// entries, byte-for-byte the same construction as S429
/// `CalibrationTable::set_hash` (`grade=cost:.6;`), so the two reproducibility
/// pins read identically in the audit ledger. Empty set → the FNV offset basis.
pub fn price_set_hash(prices: &BTreeMap<String, f64>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (grade, cost) in prices {
        for byte in format!("{grade}={cost:.6};").bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// Record the exact price set a quote used, content-addressed by its hash, in
/// the caller's transaction. Idempotent: an identical set (same hash) already
/// stored is a no-op (`ON CONFLICT DO NOTHING`). Returns the hash to stamp on
/// the quote.
pub fn record_price_set(
    tx: &Transaction<'_>,
    tenant: &str,
    prices: &BTreeMap<String, f64>,
) -> Result<String> {
    let hash = price_set_hash(prices);
    for (grade, cost) in prices {
        tx.execute(
            "INSERT INTO quote_price_snapshots (tenant_id, price_set_hash, grade, cost_per_kg_eur)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (tenant_id, price_set_hash, grade) DO NOTHING;",
            params![tenant, &hash, grade, cost],
        )
        .with_context(|| format!("insert price snapshot row for grade {grade}"))?;
    }
    Ok(hash)
}

/// Re-derive the exact `(grade → cost_per_kg_eur)` set a quote pinned, by its
/// hash. Empty map if the hash is unknown for the tenant (the caller decides
/// whether that is an error). This is the mechanism behind reproducibility:
/// overlay the result onto the catalogue and re-run the pure engine → same
/// number.
pub fn resolve_price_set(
    conn: &Connection,
    tenant: &str,
    price_set_hash: &str,
) -> Result<BTreeMap<String, f64>> {
    ensure_schema(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT grade, cost_per_kg_eur FROM quote_price_snapshots
             WHERE tenant_id = ? AND price_set_hash = ?
             ORDER BY grade ASC;",
        )
        .context("prepare resolve_price_set")?;
    let rows = stmt
        .query_map(params![tenant, price_set_hash], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })
        .context("query resolve_price_set")?;
    let mut out = BTreeMap::new();
    for r in rows {
        let (g, c) = r.context("read price snapshot row")?;
        out.insert(g, c);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_audit_ledger::{ensure_schema as audit_ensure_schema, BinaryHash, TenantId};
    use aberp_quote_engine::{
        FeatureGraph, Material as EngineMaterial, QuotingParameters, StockStatus,
        ToleranceMultiplier, ToleranceRange,
    };

    const TENANT: &str = "tnt_test";

    fn meta() -> LedgerMeta {
        LedgerMeta::new(
            TenantId::new(TENANT).expect("tenant id"),
            BinaryHash::from_bytes([0u8; 32]),
        )
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().expect("open in-memory");
        audit_ensure_schema(&c).expect("audit schema");
        crate::quoting_materials::ensure_schema(&c).expect("materials schema");
        ensure_schema(&c).expect("snapshot schema");
        c
    }

    // ── (A) parsing ─────────────────────────────────────────────────────

    #[test]
    fn parses_a_real_shaped_price_list() {
        let csv = "\
# AluStock Kft — 2026 Q3 price list
grade,cost_per_kg,currency
6061-T6,7.25,EUR
Ti-6Al-4V,38.50,EUR

304,1550,HUF
";
        let rows = parse_price_list(csv, Currency::Eur).expect("parse");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].grade, "6061-T6");
        assert_eq!(rows[0].cost_per_kg, 7.25);
        assert_eq!(rows[0].currency, Currency::Eur);
        assert_eq!(rows[2].grade, "304");
        assert_eq!(rows[2].currency, Currency::Huf);
    }

    #[test]
    fn per_row_currency_falls_back_to_batch_default() {
        let csv = "grade,cost_per_kg\n6061-T6,1500\n";
        let rows = parse_price_list(csv, Currency::Huf).expect("parse");
        assert_eq!(rows[0].currency, Currency::Huf);
    }

    #[test]
    fn header_without_cost_column_is_rejected() {
        let csv = "grade,price\n6061-T6,7.0\n";
        assert!(matches!(
            parse_price_list(csv, Currency::Eur),
            Err(ParseError::Header(_))
        ));
    }

    #[test]
    fn bad_rows_are_all_reported_loud_never_skipped() {
        let csv = "\
grade,cost_per_kg,currency
6061-T6,-3,EUR
,7.0,EUR
Ti-6Al-4V,notanumber,EUR
304,5.0,YEN
6061-T6,9.0,EUR
";
        let err = parse_price_list(csv, Currency::Eur).expect_err("must fail");
        let rows = match err {
            ParseError::Rows(r) => r,
            other => panic!("expected Rows, got {other:?}"),
        };
        // negative cost, empty grade, non-numeric, unknown currency, dup grade
        assert_eq!(
            rows.len(),
            5,
            "every bad row surfaces, none silently dropped"
        );
    }

    // ── (A) ingestion + currency ────────────────────────────────────────

    fn seed_catalogue(c: &mut Connection) {
        crate::quoting_materials::seed_if_empty(c, TENANT).expect("seed");
    }

    #[test]
    fn ingest_applies_eur_prices_and_audits() {
        let mut c = conn();
        seed_catalogue(&mut c);
        let rows = parse_price_list("grade,cost_per_kg\n6061-T6,7.25\n", Currency::Eur).unwrap();
        let outcome = ingest_price_list(
            &mut c,
            &meta(),
            "ervin",
            TENANT,
            "AluStock Q3",
            "2026-07-23T00:00:00Z",
            &rows,
            None,
        )
        .expect("ingest");
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(outcome.applied[0].new_cost_per_kg_eur, 7.25);

        let mats = crate::quoting_materials::list_materials(&c, TENANT).unwrap();
        let m = mats.iter().find(|m| m.grade == "6061-T6").unwrap();
        assert_eq!(
            m.cost_per_kg_eur, 7.25,
            "live catalogue reflects the ingest"
        );
        assert_eq!(m.updated_by_actor, "ervin");

        let audits: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'quote.material_catalogue_changed';",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audits, 1, "one supplier-ingest audit row per applied grade");
    }

    #[test]
    fn huf_prices_normalise_to_eur_with_pinned_rate() {
        let mut c = conn();
        seed_catalogue(&mut c);
        // 1550 HUF/kg at 388.0 HUF/EUR → 3.9948... EUR/kg
        let rows =
            parse_price_list("grade,cost_per_kg,currency\n304,1550,HUF\n", Currency::Eur).unwrap();
        let fx = FxToEur {
            huf_per_eur: 388.0,
            rate_date: "2026-07-22".to_string(),
        };
        let outcome = ingest_price_list(
            &mut c,
            &meta(),
            "ervin",
            TENANT,
            "SteelCo",
            "2026-07-23T00:00:00Z",
            &rows,
            Some(&fx),
        )
        .expect("ingest huf");
        let expected = 1550.0 / 388.0;
        assert!((outcome.applied[0].new_cost_per_kg_eur - expected).abs() < 1e-9);
    }

    #[test]
    fn huf_without_fx_rate_is_refused_not_guessed() {
        let mut c = conn();
        seed_catalogue(&mut c);
        let rows =
            parse_price_list("grade,cost_per_kg,currency\n304,1550,HUF\n", Currency::Eur).unwrap();
        let err = ingest_price_list(
            &mut c,
            &meta(),
            "ervin",
            TENANT,
            "SteelCo",
            "2026-07-23T00:00:00Z",
            &rows,
            None,
        )
        .expect_err("must refuse");
        assert!(matches!(err, IngestError::MissingFxRate));
    }

    #[test]
    fn unknown_grade_rejects_whole_batch_atomically() {
        let mut c = conn();
        seed_catalogue(&mut c);
        let before = crate::quoting_materials::list_materials(&c, TENANT).unwrap();
        let known_before = before
            .iter()
            .find(|m| m.grade == "6061-T6")
            .unwrap()
            .cost_per_kg_eur;

        // One known grade + one unknown → the WHOLE batch must be rejected and
        // the known grade must NOT have changed (atomic rollback).
        let rows = parse_price_list(
            "grade,cost_per_kg\n6061-T6,999.0\nUNOBTANIUM,1.0\n",
            Currency::Eur,
        )
        .unwrap();
        let err = ingest_price_list(
            &mut c,
            &meta(),
            "ervin",
            TENANT,
            "X",
            "2026-07-23T00:00:00Z",
            &rows,
            None,
        )
        .expect_err("must reject");
        match err {
            IngestError::UnknownGrades(g) => assert_eq!(g, vec!["UNOBTANIUM".to_string()]),
            other => panic!("expected UnknownGrades, got {other}"),
        }
        let after = crate::quoting_materials::list_materials(&c, TENANT).unwrap();
        let known_after = after
            .iter()
            .find(|m| m.grade == "6061-T6")
            .unwrap()
            .cost_per_kg_eur;
        assert_eq!(
            known_before, known_after,
            "a rejected batch must not have applied the known grade"
        );
    }

    // ── (B) reproducibility pin ─────────────────────────────────────────

    #[test]
    fn hash_is_stable_and_order_independent() {
        let mut a = BTreeMap::new();
        a.insert("6061-T6".to_string(), 7.0);
        a.insert("304".to_string(), 4.0);
        let mut b = BTreeMap::new();
        b.insert("304".to_string(), 4.0);
        b.insert("6061-T6".to_string(), 7.0);
        assert_eq!(price_set_hash(&a), price_set_hash(&b));

        // A different price → a different hash (drift is detectable).
        let mut cmap = a.clone();
        cmap.insert("6061-T6".to_string(), 7.01);
        assert_ne!(price_set_hash(&a), price_set_hash(&cmap));
    }

    #[test]
    fn record_then_resolve_round_trips() {
        let mut c = conn();
        let mut prices = BTreeMap::new();
        prices.insert("6061-T6".to_string(), 7.0);
        prices.insert("304".to_string(), 4.0);
        let tx = c.transaction().unwrap();
        let hash = record_price_set(&tx, TENANT, &prices).unwrap();
        tx.commit().unwrap();
        let got = resolve_price_set(&c, TENANT, &hash).unwrap();
        assert_eq!(got, prices);
    }

    // ── (B) the LOAD-BEARING reproducibility invariant, end-to-end ──────

    /// Build a minimal engine input set at the catalogue's current prices.
    fn engine_quote_total(prices: &BTreeMap<String, f64>) -> f64 {
        let materials: Vec<EngineMaterial> = prices
            .iter()
            .map(|(grade, cost)| EngineMaterial {
                grade: grade.clone(),
                density_g_cm3: 2.70,
                cost_per_kg_eur: *cost,
                machining_difficulty: 1.0,
                quote_multiplier: 1.0,
                stock_status: StockStatus::InStock,
            })
            .collect();
        let graph = FeatureGraph {
            schema_version: FeatureGraph::SCHEMA_VERSION,
            bounding_box_mm: [50.0, 40.0, 30.0],
            volume_mm3: 40_000.0,
            surface_area_mm2: 9_400.0,
            material_grade: "6061-T6".to_string(),
            // No features: material cost (what the price snapshot pins) is
            // volume/bbox-driven, not feature-driven, so an empty graph keeps
            // the test focused on the reproducibility invariant without needing
            // a complexity-rule table.
            features: vec![],
            requires_5_axis: false,
            thin_wall_present: false,
        };
        let params = QuotingParameters {
            scrap_factor: 0.15,
            profit_margin_base: 0.35,
            overhead_factor: 0.20,
            setup_amortization_threshold: 5,
            min_margin: 0.10,
            exotic_material_tax: 0.05,
            machining_rate_eur_per_minute: 1.6667,
            cad_cam_rate_eur_per_hour: 100.0,
            cad_cam_base_hours: 1.0,
            mrr_rough_ref_cm3_per_min: 8.0,
            t_finish_min_per_cm2: 0.08,
            setup_base_min: 20.0,
            setup_5axis_min: 25.0,
        };
        let tolerance = vec![ToleranceMultiplier {
            tolerance_range: "standard".to_string(),
            multiplier: 1.0,
            inspection_minutes_per_feature: 0.0,
        }];
        aberp_quote_engine::quote(
            &graph,
            &materials,
            &[],
            &tolerance,
            &[],
            &params,
            10,
            ToleranceRange::Standard,
        )
        .expect("engine quote")
        .total_price
    }

    #[test]
    fn a_pinned_quote_reprices_to_the_same_number_after_a_price_change() {
        let mut c = conn();
        seed_catalogue(&mut c);

        // 1) Price a quote against the current catalogue price for 6061-T6,
        //    and pin the exact price set it used.
        let p1 = {
            let mats = crate::quoting_materials::list_materials(&c, TENANT).unwrap();
            let mut m = BTreeMap::new();
            let cost = mats
                .iter()
                .find(|m| m.grade == "6061-T6")
                .unwrap()
                .cost_per_kg_eur;
            m.insert("6061-T6".to_string(), cost);
            m
        };
        let total_1 = engine_quote_total(&p1);
        let tx = c.transaction().unwrap();
        let pinned_hash = record_price_set(&tx, TENANT, &p1).unwrap();
        tx.commit().unwrap();

        // 2) A supplier ingest DOUBLES the 6061-T6 price.
        let new_cost = p1["6061-T6"] * 2.0;
        let rows = parse_price_list(
            &format!("grade,cost_per_kg\n6061-T6,{new_cost}\n"),
            Currency::Eur,
        )
        .unwrap();
        ingest_price_list(
            &mut c,
            &meta(),
            "ervin",
            TENANT,
            "X",
            "2026-07-23T00:00:00Z",
            &rows,
            None,
        )
        .expect("ingest");

        // 3) A FRESH quote at the new price is genuinely different…
        let p2 = {
            let mats = crate::quoting_materials::list_materials(&c, TENANT).unwrap();
            let mut m = BTreeMap::new();
            let cost = mats
                .iter()
                .find(|m| m.grade == "6061-T6")
                .unwrap()
                .cost_per_kg_eur;
            m.insert("6061-T6".to_string(), cost);
            m
        };
        let total_2 = engine_quote_total(&p2);
        assert!(
            (total_2 - total_1).abs() > 1e-6,
            "the price change must actually move the fresh quote (else the test proves nothing)"
        );

        // 4) …but re-deriving from the PINNED snapshot reproduces the ORIGINAL
        //    number exactly. This is the reproducibility invariant.
        let resolved = resolve_price_set(&c, TENANT, &pinned_hash).unwrap();
        let total_reproduced = engine_quote_total(&resolved);
        assert_eq!(
            total_reproduced, total_1,
            "a re-quote from the pinned price snapshot must yield the same number"
        );
    }
}
