//! ADR-0108 **Step 8 — the quoting family crosses.** The last family, and the
//! one §3.2 D reserves the right to abandon rather than migrate carelessly.
//!
//! # The family — grepped, not recalled
//!
//! §3.2's own rule is that a table name in this plan is measured, and this Part
//! is the fourth in a row to find the census wrong about names. A tree-wide
//! sweep of every `CREATE TABLE` in `apps/`, `crates/` and `modules/` outside a
//! `#[cfg(test)]` module returns **eleven** quoting tables, built by **eight**
//! modules:
//!
//! | table | module | cols | carried? |
//! |---|---|---|---|
//! | `margin_profiles` | `margin_profiles.rs:155` | 11 | **yes** |
//! | `quoting_machines` | `quoting_machines.rs:162` | 13 | **yes** |
//! | `quoting_materials` | `quoting_materials.rs:54` (+S357) | 17 | **yes** |
//! | `quoting_parameters` | `quoting_tunables.rs:467` (+S418) | 18 | **yes** |
//! | `quoting_complexity_rules` | `quoting_tunables.rs:438` | 12 | **yes** |
//! | `quoting_tolerance_multipliers` | `quoting_tunables.rs:455` | 7 | **yes** |
//! | `quoting_stock_adjustments` | `quoting_tunables.rs:513` | 8 | **yes** |
//! | `quote_pricing_jobs` | `quote_pricing_jobs.rs:247` (+7 ALTERs) | 29 | **no — §6.3** |
//! | `quote_intake_log` | `log_table.rs:74` (+14 ALTERs) | 22 | **no — §6.3** |
//! | `quote_price_snapshots` | `supplier_prices.rs:424` | 4 | **no — §6.3** |
//! | `quote_calibration_samples` | `quote_calibration.rs:49` | 7 | **no — §6.3** |
//!
//! ⚠ **§7's Step-8 line names five FILES and none of these eleven tables**, and
//! the five it names (`log_table.rs`, `quote_intake_query.rs`,
//! `quote_pricing_jobs.rs`, `quoting_tunables.rs`, `quoting_materials.rs`)
//! account for **six** of the eleven. `margin_profiles`, `quoting_machines`,
//! `quote_price_snapshots` and `quote_calibration_samples` are built by modules
//! §7 does not mention at all. A session that took §7's file list as the family
//! boundary would have left four tables — including one of §3.2 D's five money
//! columns — uncrossed and read the list as complete. That is the same
//! silent-skip shape Part I was written to correct, one level up: there, a table
//! no *family boundary* named; here, four tables no *step description* named.
//!
//! ⚠ **Three census names in §3.2 do not exist in the tree, and one column no
//! longer does.** This is the fourth consecutive Part to find one (G:
//! `purchase_order_sequence`; H: `email_relay_queue`; I: the AVL family had no
//! entry at all):
//!
//! * **`quote_calibration`** (§3.2 E) — the table is
//!   **`quote_calibration_samples`**. `CREATE TABLE … quote_calibration (`
//!   returns zero hits anywhere.
//! * **`quoting_materials.machinability_index`** (§3.2 E) — **the column is
//!   gone.** S418 replaced it with `machining_difficulty` and dropped it
//!   (`quoting_materials.rs:131`). A census that still lists it invites a later
//!   session to carry a column that does not exist.
//! * **`quoting_parameters`'s knob set is incomplete in §3.2 E** — it lists
//!   `setup_base_min` and `cad_cam_base_hours` but not `mrr_rough_ref_cm3_per_min`,
//!   `t_finish_min_per_cm2` or `setup_5axis_min`, all three `DOUBLE`, all three
//!   landed by the same S418 commit.
//! * **§3.2 F names not one quoting integer**, and the family has six:
//!   `quoting_materials.lead_time_default_days`,
//!   `quoting_parameters.{id, setup_amortization_threshold}`,
//!   `quoting_complexity_rules.{count_min, count_max}`, plus
//!   `quote_pricing_jobs.{quantity, attempt_n, lead_time_days,
//!   lead_time_override_days}` and `quote_intake_log.quantity` on the
//!   not-carried side. Part G raised exactly this gap for purchasing and §3.2 F
//!   was corrected for purchasing only. All are exact counts and §3.2 H's
//!   mechanical `INTEGER` rename settles every one, so no new decision was
//!   needed — but the gap is recorded because the census is what a later session
//!   greps.
//! * **`TIMESTAMP` has no §3.2 H row.** `quote_intake_log.deal_issued_at` and
//!   `.refresh_acked_at` are declared `TIMESTAMP` (`log_table.rs:183,189`);
//!   §3.2 H lists `VARCHAR`, `BIGINT`, `DOUBLE`, `BOOLEAN` and `DATE` and stops.
//!   They cross as `TEXT`, like `DATE`, for the same reason.
//!
//! # The money — §3.2 D, and what this Part does and does NOT prove
//!
//! §3.2 D names **five money columns on a float**, and its pre-commitment is
//! unambiguous: convert to R2 canonical decimal `TEXT`, or **stop and leave
//! quoting on DuckDB** — never carry them as `REAL`. Every one of the six
//! column-instances is declared `TEXT` here:
//!
//! | column | table carried? | R2 proved on real rows? |
//! |---|---|---|
//! | `quoting_materials.cost_per_kg_eur` | yes | **yes** |
//! | `quoting_parameters.machining_rate_eur_per_minute` | yes | **yes** |
//! | `quoting_parameters.cad_cam_rate_eur_per_hour` | yes | **yes** |
//! | `quote_pricing_jobs.total_price_eur` | no (§6.3) | **no — DDL only** |
//! | `quote_intake_log.total_price_eur` | no (§6.3) | **no — DDL only** |
//! | `quote_price_snapshots.cost_per_kg_eur` | no (§6.3) | **no — DDL only** |
//!
//! ⚠ **Stated plainly rather than left to inference: §6.3's drop of the job
//! history means three of the six money columns cross with a declared type and
//! no row behind it.** Their guard here is the DDL and the gate's
//! `pragma_table_info` arm, which asserts the declared type on a table with zero
//! rows — because `typeof()` over zero rows is vacuous and would report PASS on
//! a `REAL` declaration. That is weaker than the per-row round-trip the three
//! carried columns get, and it is the honest limit of what Step 8 proves.
//!
//! **Why the drop is followed rather than overridden.** §6.3, §7 and Q8 all
//! close on *drop*, and the reason survives the fact that Part C has since built
//! the converter §6.3 said it did not want to write: `total_price_eur` is the
//! **output of the f64 pricing pipeline**, so an ordinary DEV row carries a
//! value at a scale R2 refuses (this is Part E's `qc_inspections.deviation`
//! lesson — an R2 carry hard-fails the whole migration on an *ordinary* row, not
//! a pathological one). Carrying it would force exactly the choice §3.2 D exists
//! to forbid: relax R2, or round. The DEV DB is disposable
//! (`[[feedback_dev_db_disposable]]`); operator-configured tunables are not, and
//! those are precisely the seven that cross.
//!
//! # The representation, column by column
//!
//! * **R2 money** — the three carried instances cross through
//!   [`canonical_decimal_from_f64`](crate::migrate_products::canonical_decimal_from_f64),
//!   reused verbatim from Part C rather than re-implemented (one function, five
//!   uses). It renders the `f64` as the shortest decimal that round-trips it,
//!   parses with **`Decimal::from_str_exact`** (never the lenient `from_str`),
//!   refuses a non-finite value before the bind, refuses above scale 6, and
//!   refuses again unless the `Decimal` converts back to the **identical**
//!   `f64`. **A value that cannot cross exactly fails the migration; it is never
//!   rounded into range** (rule 11).
//! * **§3.2 E floats** — 21 carried `DOUBLE` columns cross as `REAL`, bit-exact,
//!   each through
//!   [`finite_measurement`](crate::migrate_quality::finite_measurement). Part E's
//!   general lesson is applied rather than re-derived: **SQLite has no `NaN`** —
//!   a bound `f64::NAN` is stored as `NULL` — so on a `NOT NULL` column it
//!   surfaces as an unattributable constraint error and on a nullable one as a
//!   silent `NULL` the `typeof` sweep cannot see. Every one of the 21 is
//!   `NOT NULL`; the guard runs anyway, because the sweep is what makes the
//!   claim measured.
//! * **§3.2 F integers** — six carried, `BIGINT`/`INTEGER` → `INTEGER`. They are
//!   **not** R2: Part C's five became `TEXT` because they were *floats*; these
//!   are exact whole-unit counts (a lead time in days, a feature count, a
//!   threshold), and rendering an `i64` as a decimal string would *introduce* a
//!   representation rather than preserve one — Part G's ruling, applied.
//! * **`BOOLEAN` → `INTEGER`** (§3.2 H): `margin_profiles.enabled`,
//!   `quoting_machines.enabled`, and on the not-carried side
//!   `quote_pricing_jobs.margin_below_floor` and `quote_intake_log.stock_alert`.
//! * **No `BLOB` anywhere in this family** — measured: `BLOB`, `BYTEA` and
//!   `BINARY` return zero hits across all eleven tables' DDL. The CAD artefacts
//!   §6.3 mentions are **files**; `quote_pricing_jobs` holds `cad_local_path`, a
//!   `VARCHAR`, and C-II forbids this plan from touching anything under
//!   `~/.aberp/`. What has to survive is the path, and this Part does not carry
//!   even that (the table is job history).
//!
//! # The DEFAULTs, and the one that is a storage-class trap
//!
//! Every source `DEFAULT` is preserved — dropping one would remove an invariant
//! the source has. The two on R2 columns are rewritten as **quoted decimal
//! strings** (`DEFAULT '1.6667'`, `DEFAULT '100'`), for Part C's reason: under
//! `STRICT` a bare `DEFAULT 1.6667` puts a REAL into a `TEXT` column's default
//! slot, which is the storage-class fork R2 exists to stop, one indirection
//! away — and `STRICT` does **not** catch it, because REAL → TEXT converts
//! losslessly and `typeof()` then reads `'text'` (§3.1's corrected table). The
//! canonical form of the DuckDB default `100.0` is `'100'`, which is what
//! `canonical_decimal_from_f64` would have produced for it.
//!
//! # The refusal
//!
//! [`unique_natural_keys`](crate::migrate_dispatch::unique_natural_keys) runs on
//! **every one of the seven carried tables**, keyed on the tenant-scoped
//! composite the gate's `BTreeMap` uses. All seven declare a `PRIMARY KEY`, so
//! the product's own schema stops a duplicate — but ⚠ **not one of those keys is
//! tenant-scoped**: `quoting_materials`'s PK is the bare `grade`,
//! `quoting_tolerance_multipliers`'s the bare `tolerance_range`,
//! `quoting_parameters`'s the bare `id`. So the source's key and the gate's key
//! are **different keys**, and the refusal is not redundant here the way it was
//! on `avl_vendors`: a database in which two tenants hold the same grade is one
//! the *gate* must refuse and the *product's PK* would never have stopped —
//! except that the same PK makes such a database unreachable through the
//! product. That the product is single-tenant-shaped in its quoting PKs is a
//! **pre-existing** modelling defect, recorded in §9 and not fixed here
//! (rule 3).
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5 and 7B–7I: **the migrator half only.** DuckDB remains
//! the source of truth. None of the eight product modules is touched — every one
//! of their queries still runs against a DuckDB `Connection`, and every
//! `ensure_schema` still builds the DuckDB schema. **The five `f64` Rust types
//! §7 schedules for this step are NOT converted here**; that is the Rust half,
//! it changes `quote_pricing_pipeline`'s arithmetic and its calibration tests,
//! and it is the half §3.2 D's stop rule is really about.
//!
//! ⚠ **This family is downstream of nothing and upstream of one thing.**
//! `quote_refuse.rs::run_refuse_saga` opens **one** transaction (`:230`) and
//! writes three families inside it — `quote_intake_log` (**here**),
//! `audit_ledger` (Step 4) and `outbound_email_queue` (Part H) — then commits
//! once. A transaction cannot span two engines, so Part H's finding stands in
//! the other direction too: **the email family's Rust half cannot cross unless
//! this one does.** Nothing is split today, because all three writes still
//! execute on one DuckDB connection.
//!
//! **No §3.4 fold is owed by this family and none is deferred.** T-8's census
//! already carries all four of this family's R2 names (`total_price_eur`,
//! `cost_per_kg_eur`, `cad_cam_rate_eur_per_hour`,
//! `machining_rate_eur_per_minute`) and the gate is green over the whole tree,
//! so there is no unregistered arithmetic on them; a direct sweep of the eight
//! modules confirms it. The pending-fold register is **unchanged by this
//! commit**: nothing joined it and nothing left it.
//!
//! **No M11-shaped hazard.** `LOWER(` returns zero hits across the eight
//! modules. `LIKE` appears only in `quote_intake_query.rs`'s operator search
//! over `raw_payload` — a JSON blob on a **not-carried** table — so no
//! ASCII-`LOWER()` fold crosses the seam here.
//!
//! # Presence is per TABLE and never per COLUMN, and that is deliberate
//!
//! The DuckDB `SELECT` names each table's **full current column set**, ladder
//! columns included, so a source table that has never had its `ALTER TABLE`
//! ladder applied fails the carry with a binder error naming the column. That is
//! the same call Parts B–I make (`migrate_products::products_select` reads its
//! S432 overlay the same way) and it is the loud branch, not the fail-open one:
//! each of these modules runs its ladder inside the `ensure_schema` it calls at
//! the top of **every** reader and writer, so a table with the ladder missing is
//! a table no current build has ever opened — and carrying it minus four columns
//! would be the silent-skip shape rule 11 exists to stop. A **missing table** is
//! a legitimate source shape and is handled; a half-built one is not.

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use rust_decimal::Decimal;

use aberp_db::engine::{Connection as SqliteConn, ToSql};
use aberp_db::schema::ensure_columns;

use crate::migrate_dispatch::unique_natural_keys;
use crate::migrate_products::{canonical_decimal_from_f64, f64_from_canonical_decimal};
use crate::migrate_quality::finite_measurement;

// ---------------------------------------------------------------------------
// The table specs — one list per table drives the DDL check, the SELECT, the
// INSERT, the per-row comparison and the typeof sweep, so the five cannot drift
// into different column orders.
// ---------------------------------------------------------------------------

/// One carried table: its columns bucketed by ADR-0108 representation class, and
/// the two-part tenant-scoped key the gate compares on.
struct Spec {
    table: &'static str,
    /// `TEXT` — §3.2 H's `VARCHAR`/`DATE`/`TIMESTAMP` rename.
    text: &'static [&'static str],
    /// `REAL` — §3.2 E, bit-exact, guarded by [`finite_measurement`].
    real: &'static [&'static str],
    /// `INTEGER` — §3.2 F exact counts.
    int: &'static [&'static str],
    /// `INTEGER` — §3.2 H's `BOOLEAN`.
    ///
    /// A bucket of its own rather than a member of `int`, and the reason is
    /// measured rather than stylistic: **`duckdb`'s row decoder refuses
    /// `BOOLEAN` → `i64`** (`Invalid column type Boolean`), so the class has to
    /// be read as `bool`. Part E's carry does the same — read `bool`, bind
    /// `bool`, let `rusqlite` map it to `INTEGER` 0/1 — so the carry never sees
    /// the integer at all and the `typeof` sweep asserting `'integer'` is what
    /// proves the mapping happened rather than a `'text'` `"true"` sneaking in.
    flag: &'static [&'static str],
    /// `TEXT` holding a canonical `rust_decimal` string — §3.2 D money on a
    /// float today.
    money: &'static [&'static str],
    /// `real` columns that are **nullable in the SOURCE**, with the value the
    /// product's own reader coalesces a `NULL` to.
    ///
    /// ⚠ **This is not a convenience and it is deliberately not a blanket.** A
    /// `real` column absent from this list that arrives `NULL` is a **hard
    /// error** naming the table, the column and the row — which is stricter
    /// than what a bare `r.get::<_, f64>()` gave (an unattributed
    /// `InvalidColumnType`), not looser. Only a column whose source contract
    /// genuinely permits `NULL` may be listed, and the coalesce value must be
    /// the one the product already uses, not a new choice made here.
    null_real: &'static [(&'static str, f64)],
    /// The gate's key. Always tenant-scoped, unlike every one of these tables'
    /// own `PRIMARY KEY`.
    key: [&'static str; 2],
}

impl Spec {
    /// Every column, in carry order: text, real, int, flag, money.
    fn columns(&self) -> Vec<&'static str> {
        let mut v = Vec::with_capacity(self.len());
        v.extend_from_slice(self.text);
        v.extend_from_slice(self.real);
        v.extend_from_slice(self.int);
        v.extend_from_slice(self.flag);
        v.extend_from_slice(self.money);
        v
    }

    fn len(&self) -> usize {
        self.text.len() + self.real.len() + self.int.len() + self.flag.len() + self.money.len()
    }

    fn select_sql(&self) -> String {
        format!(
            "SELECT {} FROM {} ORDER BY {} ASC, {} ASC",
            self.columns().join(", "),
            self.table,
            self.key[0],
            self.key[1]
        )
    }

    fn insert_sql(&self) -> String {
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table,
            self.columns().join(", "),
            vec!["?"; self.len()].join(", ")
        )
    }
}

/// What a `NULL` `quoting_materials.machining_difficulty` coalesces to.
///
/// **1.0 is the 6061-T6 reference multiplier, and it is the product's number,
/// not this migrator's.** `row_to_material` (`quoting_materials.rs:952`) reads
/// the column as `row.get::<_, Option<f64>>(4)?.unwrap_or(1.0)`; the carry
/// matches that contract rather than inventing a second answer to the same
/// question (rule 7).
const MACHINING_DIFFICULTY_DEFAULT: f64 = 1.0;

const MARGIN_PROFILES: Spec = Spec {
    table: "margin_profiles",
    text: &[
        "tenant_id",
        "id",
        "name",
        "customer_type",
        "notes",
        "created_at",
        "updated_at",
        "archived_at",
    ],
    real: &["gross_margin_pct", "min_margin_pct"],
    int: &[],
    flag: &["enabled"],
    money: &[],
    null_real: &[],
    key: ["tenant_id", "id"],
};

const QUOTING_MACHINES: Spec = Spec {
    table: "quoting_machines",
    text: &[
        "tenant_id",
        "id",
        "name",
        "family",
        "created_at",
        "updated_at",
        "archived_at",
    ],
    real: &[
        "max_envelope_x_mm",
        "max_envelope_y_mm",
        "max_envelope_z_mm",
        "daily_hours_avail",
        "buffer_pct",
    ],
    int: &[],
    flag: &["enabled"],
    money: &[],
    null_real: &[],
    key: ["tenant_id", "id"],
};

const QUOTING_MATERIALS: Spec = Spec {
    table: "quoting_materials",
    text: &[
        "tenant_id",
        "grade",
        "display_name",
        "stock_status",
        "notes",
        "updated_at",
        "updated_by_actor",
        // S357 / ADR-0074 — the material-traceability overlay.
        "current_lot_id",
        "current_heat_id",
        "cert_url",
        "cert_attached_at",
    ],
    real: &[
        "density_g_cm3",
        "machining_difficulty",
        "carbide_life_multiplier",
        "quote_multiplier",
    ],
    int: &["lead_time_default_days"],
    flag: &[],
    money: &["cost_per_kg_eur"],
    // ⚠ THE FAMILY'S ONLY NULL-SURVIVABLE FLOAT. See MACHINING_DIFFICULTY_NULL.
    null_real: &[("machining_difficulty", MACHINING_DIFFICULTY_DEFAULT)],
    key: ["tenant_id", "grade"],
};

const QUOTING_PARAMETERS: Spec = Spec {
    table: "quoting_parameters",
    text: &["tenant_id", "notes", "updated_at", "updated_by_actor"],
    real: &[
        "scrap_factor",
        "profit_margin_base",
        "overhead_factor",
        "min_margin",
        "exotic_material_tax",
        // S418's geometry-model knobs. Three of these five are named in no
        // census section at all.
        "cad_cam_base_hours",
        "mrr_rough_ref_cm3_per_min",
        "t_finish_min_per_cm2",
        "setup_base_min",
        "setup_5axis_min",
    ],
    int: &["id", "setup_amortization_threshold"],
    flag: &[],
    money: &["machining_rate_eur_per_minute", "cad_cam_rate_eur_per_hour"],
    // Empty, and MEASURED rather than assumed: `backfill_s418_parameters`
    // (`quoting_tunables.rs`) is `UPDATE … WHERE <col> IS NULL` with **no row
    // filter**, so every one of the seven S418 knobs — including both money
    // knobs — is populated on every row of every migrated tenant.
    null_real: &[],
    key: ["tenant_id", "id"],
};

const QUOTING_COMPLEXITY_RULES: Spec = Spec {
    table: "quoting_complexity_rules",
    text: &[
        "tenant_id",
        "id",
        "feature_type",
        "size_bucket",
        "notes",
        "updated_at",
        "updated_by_actor",
    ],
    real: &["base_time_minutes", "multiplier", "setup_penalty_minutes"],
    int: &["count_min", "count_max"],
    flag: &[],
    money: &[],
    null_real: &[],
    key: ["tenant_id", "id"],
};

const QUOTING_TOLERANCE_MULTIPLIERS: Spec = Spec {
    table: "quoting_tolerance_multipliers",
    text: &[
        "tenant_id",
        "tolerance_range",
        "notes",
        "updated_at",
        "updated_by_actor",
    ],
    real: &["multiplier", "inspection_minutes_per_feature"],
    int: &[],
    flag: &[],
    money: &[],
    null_real: &[],
    key: ["tenant_id", "tolerance_range"],
};

const QUOTING_STOCK_ADJUSTMENTS: Spec = Spec {
    table: "quoting_stock_adjustments",
    text: &[
        "tenant_id",
        "id",
        "grade",
        "stock_status",
        "notes",
        "updated_at",
        "updated_by_actor",
    ],
    real: &["price_adjustment_pct"],
    int: &[],
    flag: &[],
    money: &[],
    null_real: &[],
    key: ["tenant_id", "id"],
};

/// The seven carried tables, in carry order.
const CARRIED: &[&Spec] = &[
    &MARGIN_PROFILES,
    &QUOTING_MACHINES,
    &QUOTING_MATERIALS,
    &QUOTING_PARAMETERS,
    &QUOTING_COMPLEXITY_RULES,
    &QUOTING_TOLERANCE_MULTIPLIERS,
    &QUOTING_STOCK_ADJUSTMENTS,
];

/// The four **job-history** tables §6.3 drops: their schema crosses, their rows
/// do not.
///
/// They are enumerated as data rather than left implicit so the gate can hold
/// each one to "exists, and is empty **on purpose**" — the difference between a
/// deliberate drop and a dropped carry arm is exactly what rule 11 is about, and
/// a table nobody names is a table nobody checks.
const NOT_CARRIED: &[&str] = &[
    "quote_pricing_jobs",
    "quote_intake_log",
    "quote_price_snapshots",
    "quote_calibration_samples",
];

/// The §3.2 D money columns on the not-carried tables, with the declared type
/// the gate asserts through `pragma_table_info`.
///
/// This is the whole guard those three columns get (see the module docs): with
/// zero rows, `typeof()` sweeps vacuously green over a `REAL` declaration, so
/// the *declaration* is what must be checked.
const NOT_CARRIED_MONEY: &[(&str, &str)] = &[
    ("quote_pricing_jobs", "total_price_eur"),
    ("quote_intake_log", "total_price_eur"),
    ("quote_price_snapshots", "cost_per_kg_eur"),
];

// ---------------------------------------------------------------------------
// The STRICT DDL — column-for-column, in each source's declaration order
// ---------------------------------------------------------------------------

/// `margin_profiles` as `margin_profiles.rs:155` creates it, `STRICT`.
///
/// `[[no-sql-specific]]` preserved verbatim: no `CHECK`, no index. The
/// `CustomerType` vocabulary is validated in Rust before a value is bound.
const DDL_MARGIN_PROFILES: &str = "
CREATE TABLE IF NOT EXISTS margin_profiles (
    id               TEXT NOT NULL PRIMARY KEY,
    tenant_id        TEXT NOT NULL,
    name             TEXT NOT NULL,
    customer_type    TEXT NOT NULL,
    gross_margin_pct REAL NOT NULL,
    min_margin_pct   REAL NOT NULL,
    notes            TEXT,
    enabled          INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    archived_at      TEXT
) STRICT;
";

/// `quoting_machines` as `quoting_machines.rs:162` creates it, `STRICT`.
const DDL_QUOTING_MACHINES: &str = "
CREATE TABLE IF NOT EXISTS quoting_machines (
    id                TEXT NOT NULL PRIMARY KEY,
    tenant_id         TEXT NOT NULL,
    name              TEXT NOT NULL,
    family            TEXT NOT NULL,
    max_envelope_x_mm REAL NOT NULL,
    max_envelope_y_mm REAL NOT NULL,
    max_envelope_z_mm REAL NOT NULL,
    daily_hours_avail REAL NOT NULL,
    buffer_pct        REAL NOT NULL,
    enabled           INTEGER NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    archived_at       TEXT
) STRICT;
";

/// `quoting_materials`' base table (`quoting_materials.rs:54`), `STRICT`.
///
/// **`cost_per_kg_eur` is `TEXT`, not `REAL`** — §3.2 D, the one money column in
/// this table and one of the three this Part proves on real rows.
///
/// The three `DEFAULT 1.0`s are preserved on their `REAL` columns; `machinability_index`
/// is absent because S418 dropped it (`quoting_materials.rs:131`), so the fresh
/// SQLite table never has it and there is nothing for a `DROP COLUMN` to do —
/// which is the correct outcome and, unlike the S157 ladder §3.2 C worries
/// about, reached by construction rather than by a fail-open probe.
const DDL_QUOTING_MATERIALS: &str = "
CREATE TABLE IF NOT EXISTS quoting_materials (
    grade                   TEXT NOT NULL PRIMARY KEY,
    tenant_id               TEXT NOT NULL,
    display_name            TEXT NOT NULL,
    density_g_cm3           REAL NOT NULL,
    cost_per_kg_eur         TEXT NOT NULL,
    machining_difficulty    REAL NOT NULL DEFAULT 1.0,
    carbide_life_multiplier REAL NOT NULL DEFAULT 1.0,
    stock_status            TEXT NOT NULL,
    lead_time_default_days  INTEGER NOT NULL,
    quote_multiplier        REAL NOT NULL DEFAULT 1.0,
    notes                   TEXT,
    updated_at              TEXT NOT NULL,
    updated_by_actor        TEXT NOT NULL
) STRICT;
";

/// S357 / PR-44 (ADR-0074) — the material-traceability overlay, mirrored as a
/// ladder because the source adds it by `ALTER TABLE` and a DuckDB file written
/// before S357 legitimately lacks all four.
///
/// All four nullable with **no** `DEFAULT`, exactly as on DuckDB: NULL is the
/// "not yet captured" sentinel the future firing site reads.
const QUOTING_MATERIALS_S357: &[(&str, &str)] = &[
    ("current_lot_id", aberp_db::schema::TEXT),
    ("current_heat_id", aberp_db::schema::TEXT),
    ("cert_url", aberp_db::schema::TEXT),
    ("cert_attached_at", aberp_db::schema::TEXT),
];

/// `quoting_parameters`, the per-tenant tunables singleton
/// (`quoting_tunables.rs:467`), `STRICT`.
///
/// **Two `TEXT` columns here are money** — `machining_rate_eur_per_minute` and
/// `cad_cam_rate_eur_per_hour`, §3.2 D — and their `DEFAULT`s are quoted decimal
/// strings for the storage-class reason in the module docs. `DEFAULT '100'` is
/// the canonical form of DuckDB's `DEFAULT 100.0`.
const DDL_QUOTING_PARAMETERS: &str = "
CREATE TABLE IF NOT EXISTS quoting_parameters (
    id                            INTEGER NOT NULL PRIMARY KEY,
    tenant_id                     TEXT NOT NULL,
    scrap_factor                  REAL NOT NULL DEFAULT 0.15,
    profit_margin_base            REAL NOT NULL DEFAULT 0.35,
    overhead_factor               REAL NOT NULL DEFAULT 0.20,
    setup_amortization_threshold  INTEGER NOT NULL DEFAULT 5,
    min_margin                    REAL NOT NULL DEFAULT 0.10,
    exotic_material_tax           REAL NOT NULL DEFAULT 0.05,
    machining_rate_eur_per_minute TEXT NOT NULL DEFAULT '1.6667',
    cad_cam_rate_eur_per_hour     TEXT NOT NULL DEFAULT '100',
    cad_cam_base_hours            REAL NOT NULL DEFAULT 1.0,
    mrr_rough_ref_cm3_per_min     REAL NOT NULL DEFAULT 8.0,
    t_finish_min_per_cm2          REAL NOT NULL DEFAULT 0.08,
    setup_base_min                REAL NOT NULL DEFAULT 20.0,
    setup_5axis_min               REAL NOT NULL DEFAULT 25.0,
    notes                         TEXT,
    updated_at                    TEXT NOT NULL,
    updated_by_actor              TEXT NOT NULL
) STRICT;
";

/// S418 — the geometry-model knobs, mirrored as a ladder because the source adds
/// them by `ALTER TABLE` (`quoting_tunables.rs:501–507`) and a pre-S418 DuckDB
/// file legitimately lacks all seven.
///
/// ⚠ **Two of the seven are the money columns**, so their ladder type is `TEXT`
/// and not `REAL`. A ladder that added them as `REAL` would land F-6a's
/// float-money class on exactly the tenant whose database predates S418 — the
/// one case the fresh `CREATE TABLE` above cannot cover.
const QUOTING_PARAMETERS_S418: &[(&str, &str)] = &[
    (
        "machining_rate_eur_per_minute",
        aberp_db::schema::EXACT_DECIMAL,
    ),
    ("cad_cam_rate_eur_per_hour", aberp_db::schema::EXACT_DECIMAL),
    ("cad_cam_base_hours", aberp_db::schema::NON_MONEY_FLOAT),
    (
        "mrr_rough_ref_cm3_per_min",
        aberp_db::schema::NON_MONEY_FLOAT,
    ),
    ("t_finish_min_per_cm2", aberp_db::schema::NON_MONEY_FLOAT),
    ("setup_base_min", aberp_db::schema::NON_MONEY_FLOAT),
    ("setup_5axis_min", aberp_db::schema::NON_MONEY_FLOAT),
];

/// `quoting_complexity_rules` (`quoting_tunables.rs:438`), `STRICT`.
///
/// `count_max` is the family's only nullable integer — an open-ended top bucket.
const DDL_QUOTING_COMPLEXITY_RULES: &str = "
CREATE TABLE IF NOT EXISTS quoting_complexity_rules (
    id                    TEXT NOT NULL PRIMARY KEY,
    tenant_id             TEXT NOT NULL,
    feature_type          TEXT NOT NULL,
    size_bucket           TEXT NOT NULL,
    count_min             INTEGER NOT NULL,
    count_max             INTEGER,
    base_time_minutes     REAL NOT NULL,
    multiplier            REAL NOT NULL DEFAULT 1.0,
    setup_penalty_minutes REAL NOT NULL DEFAULT 0.0,
    notes                 TEXT,
    updated_at            TEXT NOT NULL,
    updated_by_actor      TEXT NOT NULL
) STRICT;
";

/// `quoting_tolerance_multipliers` (`quoting_tunables.rs:455`), `STRICT`.
const DDL_QUOTING_TOLERANCE_MULTIPLIERS: &str = "
CREATE TABLE IF NOT EXISTS quoting_tolerance_multipliers (
    tolerance_range                TEXT NOT NULL PRIMARY KEY,
    tenant_id                      TEXT NOT NULL,
    multiplier                     REAL NOT NULL DEFAULT 1.0,
    inspection_minutes_per_feature REAL NOT NULL DEFAULT 0.0,
    notes                          TEXT,
    updated_at                     TEXT NOT NULL,
    updated_by_actor               TEXT NOT NULL
) STRICT;
";

/// `quoting_stock_adjustments` (`quoting_tunables.rs:513`), `STRICT`.
///
/// `price_adjustment_pct` is a **percentage**, not money — the same call §3.2 E
/// makes for `margin_profiles.gross_margin_pct` — so it crosses as `REAL`.
/// It is named in no census section; the classification is stated here.
const DDL_QUOTING_STOCK_ADJUSTMENTS: &str = "
CREATE TABLE IF NOT EXISTS quoting_stock_adjustments (
    id                    TEXT NOT NULL PRIMARY KEY,
    tenant_id             TEXT NOT NULL,
    grade                 TEXT NOT NULL,
    stock_status          TEXT NOT NULL,
    price_adjustment_pct  REAL NOT NULL,
    notes                 TEXT,
    updated_at            TEXT NOT NULL,
    updated_by_actor      TEXT NOT NULL
) STRICT;
";

// ---------------------------------------------------------------------------
// The four job-history tables, SCHEMA ONLY (§6.3)
//
// Declared in their **final** shape — the fresh `CREATE TABLE` unioned with
// every `ALTER TABLE … ADD COLUMN` its module runs — rather than as a base plus
// a ladder. The seven carried tables mirror their source's ladder because a
// pre-ladder DuckDB file is a real source shape their carry must handle; here
// no row crosses, so there is no source shape to mirror and the ladder would be
// mechanism without a purpose (rule 12). What the DDL is here for is the
// declared **type** of three money columns, and that is what the gate checks.
//
// * `quote_pricing_jobs` — 22 base + 7 ALTER columns.
//   `total_price_eur` **`TEXT`** (§3.2 D), `margin_below_floor` `INTEGER`
//   (§3.2 H `BOOLEAN`), `margin_{override,floor}_pct` `REAL` (§3.2 E),
//   `quantity` / `attempt_n` / `lead_time_days` / `lead_time_override_days`
//   `INTEGER` (§3.2 F, none of them in the census).
//   The source's `DROP INDEX IF EXISTS quote_pricing_jobs_tenant_state_idx`
//   (S286) has nothing to drop on a fresh table and is not reproduced.
// * `quote_intake_log` — 8 base + 14 ALTER columns. `total_price_eur`
//   **`TEXT`**; `valid_until` was `DATE` and `deal_issued_at` /
//   `refresh_acked_at` were `TIMESTAMP` → all `TEXT` (§3.2 H, which has no
//   `TIMESTAMP` row); `stock_alert` `INTEGER`. The source's one index is
//   preserved — dropping it would remove something the source has.
//   `intake_state`'s `DEFAULT 'staged'` is preserved as a quoted `TEXT`
//   literal, which it already was.
// * `quote_price_snapshots` — `cost_per_kg_eur` **`TEXT`**; the natural
//   composite PK preserved.
// * `quote_calibration_samples` — two `DOUBLE` durations → `REAL` (§3.2 E's
//   `quote_calibration.{estimated,actual}_minutes`, under the table's real
//   name). No money in it.
/// `quote_pricing_jobs`, final shape, `STRICT`.
const DDL_QUOTE_PRICING_JOBS: &str = "
CREATE TABLE IF NOT EXISTS quote_pricing_jobs (
    quote_id                TEXT NOT NULL PRIMARY KEY,
    tenant_id               TEXT NOT NULL,
    state                   TEXT NOT NULL,
    fetched_at              TEXT NOT NULL,
    updated_at              TEXT NOT NULL,
    customer_email          TEXT NOT NULL,
    customer_name           TEXT NOT NULL,
    customer_company        TEXT,
    material_grade          TEXT NOT NULL,
    quantity                INTEGER NOT NULL,
    cad_filename            TEXT NOT NULL,
    cad_local_path          TEXT NOT NULL,
    feature_graph_hash      TEXT,
    feature_graph_json      TEXT,
    breakdown_json          TEXT,
    pdf_path                TEXT,
    total_price_eur         TEXT,
    valid_until_iso         TEXT,
    error_stage             TEXT,
    error_reason            TEXT,
    attempt_n               INTEGER NOT NULL,
    failure_kind            TEXT,
    lead_time_days          INTEGER,
    lead_time_override_days INTEGER,
    buyer_partner_id        TEXT,
    margin_override_pct     REAL,
    margin_below_floor      INTEGER,
    margin_floor_pct        REAL,
    price_snapshot_hash     TEXT
) STRICT;
";

/// `quote_intake_log`, final shape, `STRICT`, with the source's one index.
const DDL_QUOTE_INTAKE_LOG: &str = "
CREATE TABLE IF NOT EXISTS quote_intake_log (
    quote_id               TEXT NOT NULL PRIMARY KEY,
    tenant_id              TEXT NOT NULL,
    invoice_id             TEXT NOT NULL,
    received_at            TEXT NOT NULL,
    intake_at              TEXT NOT NULL,
    status_writeback_at    TEXT,
    raw_payload            TEXT NOT NULL,
    prepared_draft         TEXT NOT NULL,
    picked_up_drf_id       TEXT,
    intake_state           TEXT DEFAULT 'staged',
    intake_error           TEXT,
    customer_email         TEXT,
    material_grade         TEXT,
    quantity               INTEGER,
    total_price_eur        TEXT,
    valid_until            TEXT,
    stock_status_at_accept TEXT,
    stock_alert            INTEGER,
    deal_issued_at         TEXT,
    deal_sales_order_id    TEXT,
    deal_work_order_id     TEXT,
    refresh_acked_at       TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS quote_intake_log_pending_writeback_idx
    ON quote_intake_log (tenant_id, status_writeback_at);
";

/// `quote_price_snapshots`, `STRICT` (`supplier_prices.rs:424`).
const DDL_QUOTE_PRICE_SNAPSHOTS: &str = "
CREATE TABLE IF NOT EXISTS quote_price_snapshots (
    tenant_id        TEXT NOT NULL,
    price_set_hash   TEXT NOT NULL,
    grade            TEXT NOT NULL,
    cost_per_kg_eur  TEXT NOT NULL,
    PRIMARY KEY (tenant_id, price_set_hash, grade)
) STRICT;
";

/// `quote_calibration_samples`, `STRICT` (`quote_calibration.rs:49`).
const DDL_QUOTE_CALIBRATION_SAMPLES: &str = "
CREATE TABLE IF NOT EXISTS quote_calibration_samples (
    id                TEXT NOT NULL PRIMARY KEY,
    tenant_id         TEXT NOT NULL,
    job_id            TEXT NOT NULL,
    machine_family    TEXT NOT NULL,
    estimated_minutes REAL NOT NULL,
    actual_minutes    REAL NOT NULL,
    sample_at_utc     TEXT NOT NULL
) STRICT;
";

/// Build the whole family's SQLite schema — all eleven tables.
///
/// Ladder order matches each source's build order, so the two schemas line up
/// statement for statement on the tables that have one.
pub fn ensure_quoting_schema(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(DDL_MARGIN_PROFILES)?;
    conn.execute_batch(DDL_QUOTING_MACHINES)?;
    ensure_quoting_materials_table(conn)?;
    ensure_quoting_parameters_table(conn)?;
    conn.execute_batch(DDL_QUOTING_COMPLEXITY_RULES)?;
    conn.execute_batch(DDL_QUOTING_TOLERANCE_MULTIPLIERS)?;
    conn.execute_batch(DDL_QUOTING_STOCK_ADJUSTMENTS)?;
    ensure_job_history_schema(conn)?;
    Ok(())
}

fn ensure_quoting_materials_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(DDL_QUOTING_MATERIALS)?;
    ensure_columns(conn, "quoting_materials", QUOTING_MATERIALS_S357)?;
    Ok(())
}

fn ensure_quoting_parameters_table(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(DDL_QUOTING_PARAMETERS)?;
    ensure_columns(conn, "quoting_parameters", QUOTING_PARAMETERS_S418)?;
    Ok(())
}

/// The four job-history tables' schema. **No rows cross into any of them**
/// (§6.3); this exists so the three §3.2 D money columns on them have a landed
/// `TEXT` declaration that the gate can assert.
fn ensure_job_history_schema(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(DDL_QUOTE_PRICING_JOBS)?;
    conn.execute_batch(DDL_QUOTE_INTAKE_LOG)?;
    conn.execute_batch(DDL_QUOTE_PRICE_SNAPSHOTS)?;
    conn.execute_batch(DDL_QUOTE_CALIBRATION_SAMPLES)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// One row of any carried table, bucketed exactly as its [`Spec`] is.
///
/// ⚠ **`Eq` deliberately does NOT hold**: `real` and `money` are `f64`, and the
/// whole point of the money arm is that a value which does not compare equal to
/// its own round-trip must fail the migration rather than be waved through. A
/// derived `Eq` on a struct holding a `NaN` would be a lie about that.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    text: Vec<Option<String>>,
    real: Vec<f64>,
    int: Vec<Option<i64>>,
    /// §3.2 H's `BOOLEAN`, read and bound as `bool` on both sides.
    flag: Vec<bool>,
    /// The `f64` DuckDB holds, on the DuckDB side; on the SQLite side the `f64`
    /// the stored canonical decimal string converts back to. Equality between
    /// the two **is** the round-trip proof.
    money: Vec<f64>,
}

/// A source row **before** its nullable `real` columns are resolved (M-1).
///
/// Separate from [`Row`] rather than making `Row.real` an `Option<f64>` so that
/// the resolution is a *step with a type change*: everything downstream of
/// `read_duckdb` — the finite guard, the per-row comparison, the bind — holds a
/// plain `f64` and cannot silently propagate a `None`.
#[derive(Debug, Clone)]
struct RawRow {
    text: Vec<Option<String>>,
    real: Vec<Option<f64>>,
    int: Vec<Option<i64>>,
    flag: Vec<bool>,
    money: Vec<f64>,
}

/// What the quoting carry did. As everywhere else in this migrator these are
/// **not** the numbers the gate compares (B4) — the gate re-derives every one of
/// them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuotingCarry {
    pub margin_profiles: u64,
    pub quoting_machines: u64,
    pub quoting_materials: u64,
    pub quoting_parameters: u64,
    pub quoting_complexity_rules: u64,
    pub quoting_tolerance_multipliers: u64,
    pub quoting_stock_adjustments: u64,
    /// §6.3 — job-history rows **deliberately** left behind, counted on the
    /// DuckDB side.
    ///
    /// Carried as a number rather than as a silence: "0 rows in
    /// `quote_pricing_jobs`" and "4 118 rows dropped on purpose" look identical
    /// on the SQLite side, and only one of them is what §6.3 authorised
    /// (rule 11).
    pub job_history_rows_dropped: u64,
}

impl QuotingCarry {
    fn set(&mut self, table: &str, n: u64) {
        match table {
            "margin_profiles" => self.margin_profiles = n,
            "quoting_machines" => self.quoting_machines = n,
            "quoting_materials" => self.quoting_materials = n,
            "quoting_parameters" => self.quoting_parameters = n,
            "quoting_complexity_rules" => self.quoting_complexity_rules = n,
            "quoting_tolerance_multipliers" => self.quoting_tolerance_multipliers = n,
            "quoting_stock_adjustments" => self.quoting_stock_adjustments = n,
            other => unreachable!("{other} is not a carried quoting table"),
        }
    }
}

// ---------------------------------------------------------------------------
// Presence — per TABLE, because eight modules build the eleven
// ---------------------------------------------------------------------------

/// Is `table` present in this DuckDB database?
///
/// Presence is per **table** here for the strongest reason any Part has had:
/// **eight** modules build the eleven tables, and they landed across a dozen
/// sprints (`quoting_materials` at S330-ish, the S357 overlay later, S410's
/// complexity rules, S418's knobs, S428's margin profiles). A DEV database
/// predating any of them legitimately has a subset, so an absent table is a
/// source shape and not a dropped carry arm — and the gate holds each one
/// symmetrically, so the two are still distinguishable.
fn present_duckdb(conn: &duckdb::Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = ?",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn present_sqlite(conn: &SqliteConn, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Is any table of the family present on the DuckDB side?
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    for spec in CARRIED {
        if present_duckdb(conn, spec.table)? {
            return Ok(true);
        }
    }
    for table in NOT_CARRIED {
        if present_duckdb(conn, table)? {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// The carry
// ---------------------------------------------------------------------------

/// Carry the family from an already-open **read-only** DuckDB connection into an
/// already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// The caller owns both connections, the writer lock and the preconditions; this
/// function does the typed transformation and nothing else.
///
/// Every table's schema is built **before** the transaction opens, matching
/// Parts C–I: the DDL is idempotent and belongs to `ensure_*`, and a `CREATE
/// TABLE` inside the carry's own transaction would make a schema failure look
/// like a carry failure.
pub fn carry_quoting(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<QuotingCarry> {
    let mut out = QuotingCarry::default();
    if !family_present_duckdb(src)? {
        return Ok(out);
    }

    // --- schema first, for every table the source has ---
    let mut present: Vec<&Spec> = Vec::with_capacity(CARRIED.len());
    for spec in CARRIED {
        if present_duckdb(src, spec.table)? {
            present.push(spec);
        }
    }
    if !present.is_empty() {
        ensure_quoting_schema(dst)?;
    }

    // --- read and canonicalise every carried row BEFORE the write opens, so a
    //     refusal (a NaN, an f64 money value that will not round-trip) costs
    //     nothing and names its own row.
    let mut staged: Vec<(&Spec, Vec<(Row, Vec<String>)>)> = Vec::with_capacity(present.len());
    for spec in present {
        staged.push((spec, read_duckdb(src, spec)?));
    }

    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the quoting carry transaction: {e}"))?;
    for (spec, rows) in &staged {
        let sql = spec.insert_sql();
        for (row, money) in rows {
            let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(spec.len());
            for v in &row.text {
                binds.push(v as &dyn ToSql);
            }
            for v in &row.real {
                binds.push(v as &dyn ToSql);
            }
            for v in &row.int {
                binds.push(v as &dyn ToSql);
            }
            for v in &row.flag {
                binds.push(v as &dyn ToSql);
            }
            for s in money {
                binds.push(s as &dyn ToSql);
            }
            tx.execute(&sql, binds.as_slice())?;
        }
        out.set(spec.table, rows.len() as u64);
    }
    tx.commit()?;

    // --- §6.3: count what was deliberately NOT carried, and say so ---
    let mut dropped = 0u64;
    for table in NOT_CARRIED {
        if present_duckdb(src, table)? {
            let n: i64 =
                src.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            dropped += n as u64;
        }
    }
    out.job_history_rows_dropped = dropped;

    Ok(out)
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

/// `tenant#<second key part>`, rendered from whichever bucket each key column
/// lives in.
///
/// A key column may be `TEXT` (six of the seven tables) or `INTEGER`
/// (`quoting_parameters.id`, the tunables singleton). It may **never** be a
/// float — a key that cannot compare equal to itself is not a key — so the
/// `real` and `money` buckets are a hard error rather than a fallthrough.
fn key_of(spec: &Spec, row: &Row) -> Result<String> {
    key_from(spec, &row.text, &row.int)
}

/// The same key, off a [`RawRow`] — needed because M-1's NULL resolution reports
/// the row it refuses, so the key has to exist *before* the reals are resolved.
/// Neither key bucket changes type between the two structs, so this is one
/// implementation and not two.
fn key_of_raw(spec: &Spec, row: &RawRow) -> Result<String> {
    key_from(spec, &row.text, &row.int)
}

fn key_from(spec: &Spec, text: &[Option<String>], int: &[Option<i64>]) -> Result<String> {
    let mut parts = Vec::with_capacity(2);
    for want in spec.key {
        if let Some(i) = spec.text.iter().position(|c| *c == want) {
            parts.push(text[i].clone().unwrap_or_default());
        } else if let Some(i) = spec.int.iter().position(|c| *c == want) {
            parts.push(
                int[i]
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "NULL".to_string()),
            );
        } else {
            bail!(
                "{}.{want} is the gate's key but is neither TEXT nor INTEGER. A float key cannot \
                 compare equal to itself, so the per-row arm would silently compare nothing",
                spec.table
            );
        }
    }
    Ok(format!("{}#{}", parts[0], parts[1]))
}

/// The two engines' `Connection` types are unrelated concrete types (§2.3), so
/// each read is written once per engine.
///
/// **The DuckDB side validates; the SQLite side does not.** A duplicate key is a
/// property of the source, and [`unique_natural_keys`] must name the source's
/// defect rather than the copy's symptom.
///
/// Returns the row **and** the canonical money strings that will be bound, so
/// the carry binds exactly what this function proved rather than re-deriving it.
fn read_duckdb(conn: &duckdb::Connection, spec: &Spec) -> Result<Vec<(Row, Vec<String>)>> {
    let nt = spec.text.len();
    let nr = spec.real.len();
    let ni = spec.int.len();
    let nf = spec.flag.len();
    let nm = spec.money.len();

    let mut stmt = conn.prepare(&spec.select_sql())?;
    let raw: Vec<RawRow> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(nt);
            for i in 0..nt {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            // Read as Option and resolve BELOW rather than as a bare `f64`:
            // a bare read turns a source NULL into an unattributed
            // `InvalidColumnType`, which at cutover names neither the column
            // nor the row (M-1).
            let mut real = Vec::with_capacity(nr);
            for i in 0..nr {
                real.push(r.get::<_, Option<f64>>(nt + i)?);
            }
            let mut int = Vec::with_capacity(ni);
            for i in 0..ni {
                int.push(r.get::<_, Option<i64>>(nt + nr + i)?);
            }
            let mut flag = Vec::with_capacity(nf);
            for i in 0..nf {
                flag.push(r.get::<_, bool>(nt + nr + ni + i)?);
            }
            let mut money = Vec::with_capacity(nm);
            for i in 0..nm {
                money.push(r.get::<_, f64>(nt + nr + ni + nf + i)?);
            }
            Ok(RawRow {
                text,
                real,
                int,
                flag,
                money,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut keys = Vec::with_capacity(raw.len());
    let mut out = Vec::with_capacity(raw.len());
    for raw_row in raw {
        let key = key_of_raw(spec, &raw_row)?;
        // M-1: resolve every source NULL against the table's DECLARED coalesce
        // set, or fail loud naming the column and the row.
        let mut real = Vec::with_capacity(spec.real.len());
        for (i, col) in spec.real.iter().enumerate() {
            real.push(resolve_null_real(spec, col, raw_row.real[i], &key)?);
        }
        // §3.2 E: a non-finite measurement is refused before the bind, because
        // SQLite has no NaN and would store it as NULL (Part E).
        for (i, col) in spec.real.iter().enumerate() {
            finite_measurement(real[i], &key, spec.table, col)?;
        }
        // §3.2 D: refuse-never-round, exact round-trip or fail loud.
        let money = spec
            .money
            .iter()
            .zip(&raw_row.money)
            .map(|(col, v)| canonical_decimal_from_f64(*v, &key, &format!("{}.{col}", spec.table)))
            .collect::<Result<Vec<_>>>()?;
        keys.push(key);
        out.push((
            Row {
                text: raw_row.text,
                real,
                int: raw_row.int,
                flag: raw_row.flag,
                money: raw_row.money,
            },
            money,
        ));
    }
    unique_natural_keys(&keys, spec.table)?;
    Ok(out)
}

/// Resolve one source `real` value that may be `NULL`.
///
/// **M-1, and the shape is deliberately asymmetric.** A `NULL` on a column named
/// in the table's [`Spec::null_real`] coalesces to the value the *product's own
/// reader* uses; a `NULL` anywhere else is an `Err` that names the table, the
/// column and the row.
///
/// The second half is the part worth keeping: before this existed the carry read
/// every `real` as a bare `f64`, so a source `NULL` surfaced as
/// `InvalidColumnType(4, "machining_difficulty", Null)` from deep inside
/// `query_map` — an abort at cutover that names no row and no tenant. Coalescing
/// one column did not require making the rest quieter, and it did not.
fn resolve_null_real(spec: &Spec, col: &str, v: Option<f64>, key: &str) -> Result<f64> {
    if let Some(v) = v {
        return Ok(v);
    }
    match spec.null_real.iter().find(|(c, _)| *c == col) {
        Some((_, default)) => Ok(*default),
        None => bail!(
            "{}.{col} is NULL on {key}, and no coalesce is declared for it. The target column is \
             NOT NULL, so the carry cannot bind this row — and it will not invent a value. If \
             this column is genuinely nullable in the source, add it to that table's \
             `null_real` with the value the PRODUCT's own reader coalesces to, and pin it \
             (ADR-0108 §3.2 E, CLAUDE.md rules 7 and 11)",
            spec.table
        ),
    }
}

/// Read a carried table back from SQLite, converting each R2 money string to the
/// `f64` it must equal. **This is the round-trip proof**, and it runs on what is
/// on disk rather than on anything the migrator held (B4).
fn read_sqlite(conn: &SqliteConn, spec: &Spec) -> Result<Vec<Row>> {
    let nt = spec.text.len();
    let nr = spec.real.len();
    let ni = spec.int.len();
    let nf = spec.flag.len();
    let nm = spec.money.len();

    let mut stmt = conn.prepare(&spec.select_sql())?;
    let raw: Vec<(Row, Vec<String>)> = stmt
        .query_map([], |r| {
            let mut text = Vec::with_capacity(nt);
            for i in 0..nt {
                text.push(r.get::<_, Option<String>>(i)?);
            }
            let mut real = Vec::with_capacity(nr);
            for i in 0..nr {
                real.push(r.get::<_, f64>(nt + i)?);
            }
            let mut int = Vec::with_capacity(ni);
            for i in 0..ni {
                int.push(r.get::<_, Option<i64>>(nt + nr + i)?);
            }
            let mut flag = Vec::with_capacity(nf);
            for i in 0..nf {
                flag.push(r.get::<_, bool>(nt + nr + ni + i)?);
            }
            let mut money = Vec::with_capacity(nm);
            for i in 0..nm {
                money.push(r.get::<_, String>(nt + nr + ni + nf + i)?);
            }
            Ok((
                Row {
                    text,
                    real,
                    int,
                    flag,
                    money: Vec::new(),
                },
                money,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    raw.into_iter()
        .map(|(probe, strings)| {
            let key = key_of(spec, &probe)?;
            let money = spec
                .money
                .iter()
                .zip(&strings)
                .map(|(col, s)| {
                    f64_from_canonical_decimal(s, &key, &format!("{}.{col}", spec.table))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(Row { money, ..probe })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides. Every number here is re-derived from disk by the gate
/// (B4); none of it comes from [`carry_quoting`].
pub fn reconcile_quoting(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let mut any = false;

    for spec in CARRIED {
        match (
            present_duckdb(src, spec.table)?,
            present_sqlite(dst, spec.table)?,
        ) {
            (false, false) => continue,
            (true, false) => {
                hard_stops.push(format!(
                    "{} exists in DuckDB but NOT in SQLite — the table was not carried. This is \
                     the silent-skip shape: every check below would have been skipped and the \
                     gate would have reported PASS",
                    spec.table
                ));
                continue;
            }
            (false, true) => {
                hard_stops.push(format!(
                    "{} exists in SQLite but NOT in DuckDB — the copy has a table the source does \
                     not. There is no source of truth to reconcile against",
                    spec.table
                ));
                continue;
            }
            (true, true) => any = true,
        }
        reconcile_table(src, dst, spec, checks, hard_stops)?;
    }

    reconcile_job_history(src, dst, checks, hard_stops, &mut any)?;

    if !any {
        checks.push(
            "✓ quoting family absent on BOTH sides — nothing to reconcile (a database on which \
             no quote has ever been priced and no tunable ever saved is a legitimate shape; an \
             asymmetry is not)"
                .to_string(),
        );
    }
    Ok(())
}

/// One carried table: row count, per-row equality over **every** column, the
/// money Σ checksum, and the `typeof()` sweep.
fn reconcile_table(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    spec: &Spec,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let table = spec.table;

    // --- 1. row count ---
    let sql = format!("SELECT count(*) FROM {table}");
    let d: i64 = src.query_row(&sql, [], |r| r.get(0))?;
    let l: i64 = dst.query_row(&sql, [], |r| r.get(0))?;
    if d == l {
        checks.push(format!("✓ {table} row count = {d}"));
    } else {
        hard_stops.push(format!("{table} row count: DuckDB {d}, SQLite {l}"));
    }

    // --- 2. per-row equality over EVERY column (B3) ---
    let duck: Vec<(Row, Vec<String>)> = read_duckdb(src, spec)?;
    let mut lite: BTreeMap<String, Row> = BTreeMap::new();
    for row in read_sqlite(dst, spec)? {
        lite.insert(key_of(spec, &row)?, row);
    }

    let mut drift = 0usize;
    for (row, _) in &duck {
        let key = key_of(spec, row)?;
        let Some(got) = lite.get(&key) else {
            hard_stops.push(format!("{table} row {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        for (i, name) in spec.text.iter().enumerate() {
            if row.text[i] != got.text[i] {
                hard_stops.push(format!(
                    "{table} {key}: {name} differs — DuckDB {:?}, SQLite {:?}",
                    row.text[i], got.text[i]
                ));
                drift += 1;
            }
        }
        for (i, name) in spec.real.iter().enumerate() {
            if row.real[i] != got.real[i] {
                hard_stops.push(format!(
                    "{table} {key}: {name} differs — DuckDB {}, SQLite {}. DOUBLE → REAL is \
                     bit-exact for every finite f64, so a difference here is a lost value and \
                     not a rounding",
                    row.real[i], got.real[i]
                ));
                drift += 1;
            }
        }
        for (i, name) in spec.int.iter().enumerate() {
            if row.int[i] != got.int[i] {
                hard_stops.push(format!(
                    "{table} {key}: {name} differs — DuckDB {:?}, SQLite {:?}",
                    row.int[i], got.int[i]
                ));
                drift += 1;
            }
        }
        for (i, name) in spec.flag.iter().enumerate() {
            if row.flag[i] != got.flag[i] {
                hard_stops.push(format!(
                    "{table} {key}: {name} differs — DuckDB {}, SQLite {}",
                    row.flag[i], got.flag[i]
                ));
                drift += 1;
            }
        }
        // THE §3.2 D round-trip. `got.money` was derived by parsing the stored
        // decimal string and converting back; `row.money` is the DOUBLE DuckDB
        // holds. Equal means the canonicalisation moved no money.
        for (i, name) in spec.money.iter().enumerate() {
            if row.money[i] != got.money[i] {
                hard_stops.push(format!(
                    "{table} {key}: {name} DuckDB {}, SQLite {} — §3.2 D carries this float \
                     money column to an exact decimal string and it MUST convert back to the \
                     identical value",
                    row.money[i], got.money[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every {table} column round-trips with ZERO drift ({} row(s) × {} columns{})",
            duck.len(),
            spec.len(),
            if spec.money.is_empty() {
                String::new()
            } else {
                format!(
                    ", incl. {} §3.2 D money column(s) proved value-identical to their DOUBLEs",
                    spec.money.len()
                )
            }
        ));
    }

    // --- 3. §6.3's per-money-column exact sum, folded in Rust on BOTH sides ---
    //
    // A checksum over the carried set, not a business total: summing a rate per
    // hour is not a figure anyone would quote. It is here because §6.3 asks for
    // it by name and because it is the one arm whose output an operator reading
    // the gate can eyeball. Never a SQL `SUM` (§3.4 / T-8).
    for (i, name) in spec.money.iter().enumerate() {
        let ds = sum_decimals(
            duck.iter().map(|(_, money)| money[i].as_str()),
            table,
            name,
            "DuckDB",
        )?;
        let ls = sum_decimals(
            read_money_strings(dst, table, name)?
                .iter()
                .map(String::as_str),
            table,
            name,
            "SQLite",
        )?;
        if ds == ls {
            checks.push(format!(
                "✓ Σ {table}.{name} = {ds} (folded in Rust over exact decimals, both sides)"
            ));
        } else {
            hard_stops.push(format!("Σ {table}.{name}: DuckDB {ds}, SQLite {ls}"));
        }
    }

    // --- 4. typeof() over every row of every column (T-2 / M1) ---
    for col in spec.text {
        typeof_sweep(dst, table, col, "text", checks, hard_stops)?;
    }
    for col in spec.real {
        typeof_sweep(dst, table, col, "real", checks, hard_stops)?;
    }
    for col in spec.int {
        typeof_sweep(dst, table, col, "integer", checks, hard_stops)?;
    }
    // §3.2 H: `BOOLEAN` → `INTEGER`. A `'text'` here would be a `"true"` bound
    // as a string, which every downstream `WHERE enabled = 1` would then miss.
    for col in spec.flag {
        typeof_sweep(dst, table, col, "integer", checks, hard_stops)?;
    }
    // The load-bearing sweep of this commit: these were `DOUBLE` money. A
    // `'real'` here is F-6a's float-money class arriving on the quoting path.
    for col in spec.money {
        typeof_sweep(dst, table, col, "text", checks, hard_stops)?;
    }
    Ok(())
}

/// The four job-history tables (§6.3): the schema crosses, the rows do not.
///
/// Three things are asserted, and the third is the one that matters. The table
/// **exists** on SQLite (a missing one is a dropped schema, not a drop policy);
/// it is **empty** (a row here means something carried them behind §6.3's back);
/// and each of the three §3.2 D money columns on them is **declared `TEXT`**.
/// That last check is not decoration: with zero rows a `typeof()` sweep passes
/// vacuously over a `REAL` declaration, so the declaration is the only thing
/// left to check, and §3.2 D's whole point is that these columns must never be
/// `REAL`.
fn reconcile_job_history(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
    any: &mut bool,
) -> Result<()> {
    for table in NOT_CARRIED {
        let on_duck = present_duckdb(src, table)?;
        let on_lite = present_sqlite(dst, table)?;
        if !on_duck && !on_lite {
            continue;
        }
        *any = true;
        if on_duck && !on_lite {
            hard_stops.push(format!(
                "{table} exists in DuckDB but NOT in SQLite. §6.3 drops this table's ROWS, not \
                 its schema — a missing table means the schema half was dropped too, and the \
                 three §3.2 D money columns have nowhere to be declared"
            ));
            continue;
        }
        let n: i64 = dst.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        if n == 0 {
            let d: i64 = if on_duck {
                src.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?
            } else {
                0
            };
            checks.push(format!(
                "✓ {table} is empty on SQLite by design — §6.3 drops quoting job history; \
                 {d} DuckDB row(s) deliberately NOT carried"
            ));
        } else {
            hard_stops.push(format!(
                "{table} has {n} row(s) on the SQLite side. §6.3 carries NO quoting job \
                 history — these rows came from somewhere the plan did not authorise, and \
                 their float money columns were never round-trip-proved"
            ));
        }
    }

    for (table, col) in NOT_CARRIED_MONEY {
        if !present_sqlite(dst, table)? {
            continue;
        }
        match declared_type_sqlite(dst, table, col)? {
            Some(ty) if ty.eq_ignore_ascii_case("TEXT") => {
                checks.push(format!(
                    "✓ {table}.{col} is declared TEXT — §3.2 D's float-money column, typed \
                     without a row behind it"
                ));
            }
            Some(ty) => hard_stops.push(format!(
                "{table}.{col} is declared {ty}, not TEXT. §3.2 D pre-commits that this money \
                 column crosses as an exact decimal string or the step STOPS — it is never \
                 carried as REAL. The table holds no rows, so the typeof sweep cannot see this \
                 and would have reported PASS"
            )),
            None => hard_stops.push(format!(
                "{table}.{col} does not exist on the SQLite side — §3.2 D's money column was \
                 dropped from the schema rather than typed"
            )),
        }
    }
    Ok(())
}

/// Fold a column's canonical decimal strings into one exact `Decimal`.
///
/// **Never a SQL `SUM`** (§3.4 / T-8): under R2 the column is `TEXT`, and
/// SQLite would coerce `SUM(TEXT)` to `REAL` — the exact float-money class this
/// whole representation exists to eliminate, arriving inside the gate that is
/// supposed to catch it.
fn sum_decimals<'a>(
    values: impl Iterator<Item = &'a str>,
    table: &str,
    col: &str,
    side: &str,
) -> Result<Decimal> {
    let mut acc = Decimal::ZERO;
    for raw in values {
        let d = Decimal::from_str_exact(raw).map_err(|e| {
            anyhow::anyhow!("{table}.{col} on the {side} side is {raw:?}, not a decimal ({e})")
        })?;
        acc = acc
            .checked_add(d)
            .ok_or_else(|| anyhow::anyhow!("{table}.{col}: the {side}-side Σ overflowed"))?;
    }
    Ok(acc)
}

/// One money column's stored strings, read straight off the SQLite disk — never
/// via anything the migrator held (B4), and never via the `f64` the per-row arm
/// converts back to.
fn read_money_strings(conn: &SqliteConn, table: &str, col: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("SELECT {col} FROM {table}"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The declared type of one column, from `pragma_table_info`. `None` when the
/// column is absent.
fn declared_type_sqlite(conn: &SqliteConn, table: &str, col: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name, type FROM pragma_table_info('{table}')"
    ))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (name, ty) = row?;
        if name == col {
            return Ok(Some(ty));
        }
    }
    Ok(None)
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// NULL is permitted wherever the column is nullable; what must never appear is
/// the **wrong** non-null storage class.
fn typeof_sweep(
    dst: &SqliteConn,
    table: &str,
    col: &str,
    want: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let bad: i64 = dst.query_row(
        &format!("SELECT count(*) FROM {table} WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
        [want],
        |r| r.get(0),
    )?;
    if bad == 0 {
        checks.push(format!("✓ typeof({table}.{col}) = '{want}' on every row"));
    } else {
        hard_stops.push(format!(
            "{table}.{col}: {bad} row(s) are not '{want}' — in SQLite TEXT, INTEGER, REAL and \
             BLOB are distinct storage classes that never compare equal, so a wrong one here \
             silently breaks lookups and, on a money column, is F-6a's float-money class"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_db::schema::{EXACT_DECIMAL, NON_MONEY_FLOAT, TEXT};

    /// Every DDL string in this module, paired with the table it declares.
    const DDLS: &[(&str, &str)] = &[
        ("margin_profiles", DDL_MARGIN_PROFILES),
        ("quoting_machines", DDL_QUOTING_MACHINES),
        ("quoting_materials", DDL_QUOTING_MATERIALS),
        ("quoting_parameters", DDL_QUOTING_PARAMETERS),
        ("quoting_complexity_rules", DDL_QUOTING_COMPLEXITY_RULES),
        (
            "quoting_tolerance_multipliers",
            DDL_QUOTING_TOLERANCE_MULTIPLIERS,
        ),
        ("quoting_stock_adjustments", DDL_QUOTING_STOCK_ADJUSTMENTS),
        ("quote_pricing_jobs", DDL_QUOTE_PRICING_JOBS),
        ("quote_intake_log", DDL_QUOTE_INTAKE_LOG),
        ("quote_price_snapshots", DDL_QUOTE_PRICE_SNAPSHOTS),
        ("quote_calibration_samples", DDL_QUOTE_CALIBRATION_SAMPLES),
    ];

    /// Is `col` declared with `ty` in `ddl`? Matched on the line whose first
    /// token is the column name.
    ///
    /// ⚠ **The trailing comma is stripped, and that is load-bearing** — the
    /// corrected shape from Part I. A **nullable** column carries no `NOT NULL`,
    /// so its type token is the last on the line and reads as `TEXT,`; a bare
    /// `nth(1) == Some("TEXT")` silently reports "not declared TEXT" for every
    /// nullable column. That is a false RED in a positive assertion and a false
    /// **GREEN** in the `!declared_as(…)` assertions this file uses below, which
    /// is the shape that actually costs something.
    fn declared_as(ddl: &str, col: &str, ty: &str) -> bool {
        ddl.lines()
            .filter(|l| l.split_whitespace().next() == Some(col))
            .any(|l| l.split_whitespace().nth(1).map(|t| t.trim_end_matches(',')) == Some(ty))
    }

    /// [`declared_as`] sees a **nullable** column's type, and still
    /// discriminates.
    #[test]
    fn declared_as_sees_nullable_columns_too() {
        // `notes` and `archived_at` are nullable — their type token ends in a
        // comma on every table that has them.
        assert!(declared_as(DDL_MARGIN_PROFILES, "notes", "TEXT"));
        assert!(declared_as(DDL_MARGIN_PROFILES, "archived_at", "TEXT"));
        assert!(declared_as(
            DDL_QUOTING_COMPLEXITY_RULES,
            "count_max",
            "INTEGER"
        ));
        assert!(!declared_as(DDL_MARGIN_PROFILES, "notes", "INTEGER"));
        assert!(!declared_as(
            DDL_MARGIN_PROFILES,
            "nonexistent_column",
            "TEXT"
        ));
    }

    /// **The six §3.2 D money column instances are `TEXT`, and not one of them
    /// is `REAL`.**
    ///
    /// This is the pin §3.2 D's pre-commitment turns on: *if Step 8 overruns,
    /// stop and leave quoting on DuckDB — do not migrate it as `REAL`.* Both
    /// halves are asserted, because "is TEXT" and "is not REAL" are the same
    /// claim only while the DDL has exactly one line per column, and the
    /// negative is the one that fails loudly if a column is ever declared twice.
    #[test]
    fn every_section_3_2_d_money_column_is_text_and_never_real() {
        for (ddl, col) in [
            (DDL_QUOTING_MATERIALS, "cost_per_kg_eur"),
            (DDL_QUOTING_PARAMETERS, "machining_rate_eur_per_minute"),
            (DDL_QUOTING_PARAMETERS, "cad_cam_rate_eur_per_hour"),
            (DDL_QUOTE_PRICING_JOBS, "total_price_eur"),
            (DDL_QUOTE_INTAKE_LOG, "total_price_eur"),
            (DDL_QUOTE_PRICE_SNAPSHOTS, "cost_per_kg_eur"),
        ] {
            assert!(
                declared_as(ddl, col, EXACT_DECIMAL),
                "{col} is a §3.2 D money column and must be declared TEXT (R2)"
            );
            assert!(
                !declared_as(ddl, col, NON_MONEY_FLOAT),
                "{col} declared REAL — §3.2 D pre-commits that the correct fallback is to STOP, \
                 never to carry quoting money as a float"
            );
        }
        // And the S418 ladder, which is the ONLY route onto a pre-S418 tenant's
        // parameters row, types both money knobs the same way.
        for (col, ty) in QUOTING_PARAMETERS_S418 {
            let want =
                if *col == "machining_rate_eur_per_minute" || *col == "cad_cam_rate_eur_per_hour" {
                    EXACT_DECIMAL
                } else {
                    NON_MONEY_FLOAT
                };
            assert_eq!(ty, &want, "the S418 ladder types {col} wrongly");
        }
    }

    /// **The two R2 `DEFAULT`s are quoted decimal strings, not bare numbers.**
    ///
    /// Under `STRICT` a bare `DEFAULT 1.6667` puts a REAL into a `TEXT` column's
    /// default slot and the engine converts it silently — `typeof()` then reads
    /// `'text'`, so neither `STRICT` nor T-2 can see it (§3.1's corrected
    /// table). The quoting is the whole guard.
    #[test]
    fn the_r2_defaults_are_quoted_decimal_strings() {
        assert!(DDL_QUOTING_PARAMETERS
            .contains("machining_rate_eur_per_minute TEXT NOT NULL DEFAULT '1.6667'"));
        assert!(DDL_QUOTING_PARAMETERS
            .contains("cad_cam_rate_eur_per_hour     TEXT NOT NULL DEFAULT '100'"));
        // The REAL knobs keep their bare numeric defaults, which is correct for
        // a REAL column and wrong for a TEXT one — so the two forms must not be
        // confused.
        assert!(DDL_QUOTING_PARAMETERS
            .contains("scrap_factor                  REAL NOT NULL DEFAULT 0.15"));
    }

    /// Every table is `STRICT`, no `VARCHAR`/`DOUBLE`/`BOOLEAN`/`DECIMAL`
    /// survives the rename, and no `CHECK` is invented.
    #[test]
    fn the_ddl_uses_the_adr0108_type_vocabulary() {
        assert_eq!(TEXT, "TEXT");
        for (table, ddl) in DDLS {
            assert!(ddl.contains("STRICT"), "{table} must be STRICT");
            for banned in [
                "VARCHAR", "DOUBLE", "BOOLEAN", "DECIMAL", "NUMERIC", "FLOAT",
            ] {
                assert!(
                    !ddl.contains(banned),
                    "{table} still declares {banned} — §3.2 H renames it"
                );
            }
            assert!(
                !ddl.contains("CHECK"),
                "{table}: [[no-sql-specific]] — vocabularies are validated in Rust, not in a \
                 SQL CHECK, and porting one would fork the two schemas"
            );
        }
    }

    /// **No column in this family is a `BLOB`**, and no source column was one.
    ///
    /// Stated as a pin rather than a comment because the CAD artefacts are the
    /// obvious thing a later session would reach for: they are **files**, the
    /// row holds a path, and C-II forbids this plan from touching anything under
    /// `~/.aberp/`. A `BLOB` appearing here means that stopped being true.
    #[test]
    fn nothing_in_this_family_is_a_blob() {
        for (table, ddl) in DDLS {
            assert!(
                !ddl.contains("BLOB"),
                "{table} declares a BLOB — the quoting family carries no binary payload; CAD \
                 artefacts are files and the row holds a path"
            );
        }
    }

    /// Every carried spec's column set is exactly its DDL's, and the `SELECT`
    /// and `INSERT` agree on order — which is what makes the positional decode
    /// in [`read_duckdb`] / [`read_sqlite`] safe.
    #[test]
    fn the_select_and_the_insert_agree_on_column_order() {
        let expect = [
            ("margin_profiles", 11usize),
            ("quoting_machines", 13),
            ("quoting_materials", 17),
            ("quoting_parameters", 18),
            ("quoting_complexity_rules", 12),
            ("quoting_tolerance_multipliers", 7),
            ("quoting_stock_adjustments", 8),
        ];
        for (spec, (table, n)) in CARRIED.iter().zip(expect) {
            assert_eq!(spec.table, table);
            assert_eq!(spec.len(), n, "{table} column count");
            let joined = spec.columns().join(", ");
            assert!(spec
                .select_sql()
                .starts_with(&format!("SELECT {joined} FROM {table}")));
            assert!(spec
                .insert_sql()
                .starts_with(&format!("INSERT INTO {table} ({joined})")));
        }
    }

    /// **The carry names every column the DDL declares, and no others.**
    ///
    /// The S357 / S418 ladder columns count: they are declared by
    /// `ensure_columns`, not by the base DDL, so the check unions the two the
    /// same way `ensure_*_table` does.
    #[test]
    fn the_carry_covers_every_declared_column() {
        let ladders: &[(&str, &[(&str, &str)])] = &[
            ("quoting_materials", QUOTING_MATERIALS_S357),
            ("quoting_parameters", QUOTING_PARAMETERS_S418),
        ];
        for spec in CARRIED {
            let ddl = DDLS
                .iter()
                .find(|(t, _)| *t == spec.table)
                .expect("every carried table has a DDL")
                .1;
            let mut declared: Vec<String> = ddl
                .lines()
                .filter_map(|l| l.split_whitespace().next())
                .filter(|w| spec.columns().contains(w))
                .map(str::to_string)
                .collect();
            for (table, cols) in ladders {
                if *table == spec.table {
                    for (c, _) in *cols {
                        if !declared.iter().any(|d| d == c) {
                            declared.push((*c).to_string());
                        }
                    }
                }
            }
            declared.sort();
            declared.dedup();
            let mut want: Vec<String> = spec.columns().iter().map(|c| c.to_string()).collect();
            want.sort();
            assert_eq!(
                declared, want,
                "{}: the carry and the schema name different column sets",
                spec.table
            );
        }
    }

    /// Every carried table's gate key is tenant-scoped and leads the `SELECT`'s
    /// `ORDER BY`.
    ///
    /// ⚠ And the second half of the pin is the one worth having: **not one of
    /// these tables' own `PRIMARY KEY` is tenant-scoped.** So the source's key
    /// and the gate's key are different keys, and `unique_natural_keys` is doing
    /// real work rather than restating the PK.
    #[test]
    fn every_gate_key_is_tenant_scoped_and_the_pk_is_not() {
        for spec in CARRIED {
            assert_eq!(spec.key[0], "tenant_id", "{}", spec.table);
            assert!(spec.select_sql().ends_with(&format!(
                "ORDER BY {} ASC, {} ASC",
                spec.key[0], spec.key[1]
            )));
            let ddl = DDLS.iter().find(|(t, _)| *t == spec.table).unwrap().1;
            assert!(
                !ddl.lines()
                    .filter(|l| l.split_whitespace().next() == Some("tenant_id"))
                    .any(|l| l.contains("PRIMARY KEY")),
                "{}: tenant_id carries the source's PRIMARY KEY? Then the note in the module \
                 docs about the source being single-tenant-shaped is stale",
                spec.table
            );
            assert!(ddl.contains("PRIMARY KEY"), "{}", spec.table);
        }
    }

    /// The four job-history tables are named, and none of them is also a carried
    /// table.
    ///
    /// Pinned because the two lists are the whole of §6.3's drop policy: a table
    /// in neither list crosses in no arm at all and nothing reds — Part I's
    /// failure, exactly.
    #[test]
    fn the_carried_and_dropped_lists_partition_the_family() {
        assert_eq!(CARRIED.len(), 7);
        assert_eq!(NOT_CARRIED.len(), 4);
        for t in NOT_CARRIED {
            assert!(
                !CARRIED.iter().any(|s| s.table == *t),
                "{t} is in both lists"
            );
        }
        // All eleven have a DDL, and the DDL list has nothing else in it.
        assert_eq!(DDLS.len(), 11);
        for (table, _) in DDLS {
            assert!(
                CARRIED.iter().any(|s| s.table == *table) || NOT_CARRIED.contains(table),
                "{table} has a DDL but is in neither list — it would cross with a schema and no \
                 gate arm"
            );
        }
        // And every §3.2 D money column on a dropped table is registered for the
        // declared-type check, which is the only guard those three get.
        assert_eq!(NOT_CARRIED_MONEY.len(), 3);
        for (table, _) in NOT_CARRIED_MONEY {
            assert!(NOT_CARRIED.contains(table));
        }
    }

    /// The key builder renders a `TEXT` key and an `INTEGER` key, separates
    /// tenants, and **refuses a float key**.
    #[test]
    fn the_key_is_the_tenant_scoped_composite_and_never_a_float() {
        let row = |tenant: &str, id: &str| Row {
            text: (0..MARGIN_PROFILES.text.len())
                .map(|i| match i {
                    0 => Some(tenant.to_string()),
                    1 => Some(id.to_string()),
                    _ => None,
                })
                .collect(),
            real: vec![0.0; MARGIN_PROFILES.real.len()],
            int: vec![None; MARGIN_PROFILES.int.len()],
            flag: vec![false; MARGIN_PROFILES.flag.len()],
            money: vec![],
        };
        assert_eq!(
            key_of(&MARGIN_PROFILES, &row("acme", "mp_1")).unwrap(),
            "acme#mp_1"
        );
        assert_ne!(
            key_of(&MARGIN_PROFILES, &row("acme", "mp_1")).unwrap(),
            key_of(&MARGIN_PROFILES, &row("globex", "mp_1")).unwrap()
        );

        // `quoting_parameters` keys on an INTEGER `id`.
        let params = Row {
            text: vec![Some("acme".into()), None, None, None],
            real: vec![0.0; QUOTING_PARAMETERS.real.len()],
            int: vec![Some(1), Some(5)],
            flag: vec![],
            money: vec![1.0, 100.0],
        };
        assert_eq!(key_of(&QUOTING_PARAMETERS, &params).unwrap(), "acme#1");

        // A float key is refused rather than silently stringified.
        let bad = Spec {
            table: "probe",
            text: &["tenant_id"],
            real: &["ratio"],
            int: &[],
            flag: &[],
            money: &[],
            null_real: &[],
            key: ["tenant_id", "ratio"],
        };
        assert!(key_of(
            &bad,
            &Row {
                text: vec![Some("acme".into())],
                real: vec![0.5],
                int: vec![],
                flag: vec![],
                money: vec![],
            }
        )
        .is_err());
    }

    /// The identity refusal fires on a duplicate of the **composite**, and not
    /// on two tenants holding the same grade.
    #[test]
    fn the_refusal_fires_on_a_duplicate_composite() {
        assert!(unique_natural_keys(
            &["acme#6061-T6".to_string(), "acme#6061-T6".to_string()],
            "quoting_materials"
        )
        .is_err());
        assert!(unique_natural_keys(
            &["acme#6061-T6".to_string(), "globex#6061-T6".to_string()],
            "quoting_materials"
        )
        .is_ok());
    }
}
