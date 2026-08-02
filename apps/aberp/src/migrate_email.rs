//! ADR-0108 Step 7 Part H — **the email / relay family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM), E (QA/QC),
//! F (dispatch) and G (purchasing) are already on this branch, so this is
//! **Part H**. Named rather than silently renumbered — the same call Parts D–G
//! made. §7's own family list puts email/relay **last** among the non-quoting
//! families, so this closes Step 7.
//!
//! # The family — grepped, not recalled
//!
//! §3.2's rule is that a table name in this plan is measured. A tree-wide sweep
//! of every `CREATE TABLE` in `apps/`, `crates/` and `modules/` returns exactly
//! **one** email/relay table:
//!
//! | table | columns | the numbers in it |
//! |---|---|---|
//! | `outbound_email_queue` | 15 | `attempt_n`, `byte_size` — both §3.2 F |
//!
//! Built by one `CREATE TABLE IF NOT EXISTS` in one module
//! (`apps/aberp/src/email_relay_queue.rs:127–143`).
//!
//! ⚠ **The census names this table by the wrong name, and a session that
//! grepped for it would have found nothing.** §3.2 F's enumerated list says
//! `email_relay_queue.byte_size`. `email_relay_queue` is the **module**; the
//! **table** is `outbound_email_queue`, and a `CREATE TABLE … email_relay_queue`
//! sweep returns **zero hits anywhere in the tree**. Zero hits read as "already
//! done" is the silent-skip shape rule 11 exists to stop, and it is the same
//! class of census gap Part G found on §3.2 F's integer list. §3.2 F now carries
//! the correction; §9 has the row.
//!
//! **There is no second table.** Measured, and each absence has a reason worth
//! stating so the next session does not go looking:
//!
//! * **no `email_outbox` / notification / delivery-record table.** The
//!   *outbox-poll* daemon (`email_outbox_poll_daemon.rs`, ADR-0009) keeps **no
//!   persistent state at all** — its own module docs say so: the storefront's
//!   `queued/ → claimed/ → sent/ | failed/` directory layout *is* the state
//!   machine, and ABERP polls it outbound. `SELECT` / `INSERT` / `UPDATE` /
//!   `CREATE TABLE` all return **zero** hits across
//!   `email_outbox_poll_daemon.rs`, `email_invoice.rs`, `email_relay.rs`,
//!   `email_relay_daemon.rs` and `email_relay_credentials.rs`.
//! * **no SMTP relay-state table.** The credential lives in the keychain
//!   (`email_relay_credentials.rs`), the rate limiter is an in-process sliding
//!   window (`email_relay.rs:94`), and the drain position is the `state` column
//!   on the queue row itself.
//! * **no notes↔email linkage table.** `notes` is a *column* on the invoice, not
//!   a table.
//!
//! # There is NO BLOB in this family, and that is the design rather than an
//! # omission
//!
//! `email_relay_queue.rs:24–31` states it: DuckDB `BLOB` columns degrade with
//! large binary data, so **attachments live on the filesystem** under
//! `~/.aberp/<tenant>/email-relay-attachments/<row_id>/<n>_<safe_name>` and the
//! row stores a single `attachments_dir` **rel-path** in a `VARCHAR`. So R3 has
//! nothing to bite on here: the migrator carries a path, never a byte payload,
//! and it touches the attachment tree not at all (C-II — this plan reads,
//! writes and stats nothing under `~/.aberp/`).
//!
//! **What that moves the risk onto is the rel-path.** An attachment whose
//! `attachments_dir` drifted by one byte is an attachment the daemon can no
//! longer find, and no `typeof` class and no row count can see it. The family
//! test therefore writes **real attachment bytes** through the product's own
//! `write_attachment`, carries the row, and then resolves the path **out of the
//! SQLite row** and asserts the file is byte-identical — which is the honest
//! form of "carry the attachments byte-exact" for a schema in which the bytes
//! were never in the database.
//!
//! ⚠ **`recipient_hash` is a hash and it stays `TEXT`, not `BLOB`.** It is
//! `format!("{:x}", Sha256…)` — a **hex string** (`email_relay.rs:82–92`) —
//! declared `VARCHAR` upstream and consumed as a `String` everywhere. This is
//! the same call §6.3 already took on `audit_ledger_anchors.
//! chain_head_hash_at_anchor`: typing it `BLOB` by analogy with the other hashes
//! would make every lookup and every comparison of it fail, because in SQLite
//! `BLOB` and `TEXT` never compare equal. R3 is about the *bytes* of the audit
//! chain, not about every column whose name contains "hash".
//!
//! # The numbers — two, both §3.2 F, neither money
//!
//! **This family has no money, no rate, no quantity, no decimal, no float and
//! no boolean.** Its only two non-string columns are counts:
//!
//! * `attempt_n` — `INTEGER` on DuckDB, `u32` in Rust
//!   (`OutboundEmailRow.attempt_n`), the retry counter. DuckDB's `INTEGER` is
//!   32-bit and SQLite's is 64-bit, so §3.2 H's mechanical rename **widens**
//!   losslessly — the same widening `seq`, `year` and `vat_rate_pct` already
//!   took in Parts D and G.
//! * `byte_size` — `BIGINT` on DuckDB, `u64` in Rust, the rendered size of the
//!   message. §3.2 F names it (under the wrong table name, above).
//!
//! So **R1, R2, `canonical_decimal`, `canonical_decimal_from_f64` and
//! [`finite_measurement`](crate::migrate_quality::finite_measurement) all have
//! nothing to guard here** — the same position Parts E and F were in, and it is
//! measured rather than assumed: `DECIMAL`, `DOUBLE`, `REAL`, `FLOAT`,
//! `BOOLEAN` and `BLOB` all return **zero** hits against this table's DDL.
//!
//! # The refusal, and the honest limit of it
//!
//! [`unique_natural_keys`](crate::migrate_dispatch::unique_natural_keys) is
//! reused verbatim rather than re-implemented, keyed on `id`. The gate keys its
//! per-row `BTreeMap` on `id`, so a duplicate would collapse two rows onto one
//! entry and leave the row count, the `typeof` sweep and the per-row arm all
//! green while one row is compared twice and the other never.
//!
//! ⚠ **Unlike Parts F and G, this table DOES carry a `PRIMARY KEY` on DuckDB**
//! (`id VARCHAR NOT NULL PRIMARY KEY`), so the product's own schema already
//! stops the duplicate and the refusal cannot fire against a source the product
//! wrote. Stating that plainly is the point: the guard is here for the shape a
//! `PRIMARY KEY` does not cover — a table hand-recreated by a repair, a restore
//! or an older schema — and the family test reaches it by building exactly that
//! source rather than by pretending the product can produce one. Part F made the
//! same call for `dispatches`, whose `dsp_id` is also a DuckDB `PRIMARY KEY`:
//! one function, three uses, not three mechanisms.
//!
//! # This table has no `tenant_id`, and every other crossed table does
//!
//! Measured. The database file is per-tenant (`~/.aberp/<tenant>/aberp.duckdb`),
//! so the column is redundant everywhere — but every other family carries it
//! anyway and keys on `tenant#id`. Here the natural key is the bare `id`. That
//! is a rule-7 inconsistency in the *schema convention*, not a storage hazard:
//! nothing in this family is queried across tenants, because nothing can be.
//! Recorded in one sentence rather than changed — adding a column during a
//! migration would be the migration acting as a schema-designer.
//!
//! # What this step is, and what it is NOT
//!
//! Same split as Steps 5, 7B, 7C, 7D, 7E, 7F and 7G, and Ervin's same
//! constraint: **the migrator half only.** DuckDB remains the source of truth.
//! `apps/aberp/src/email_relay_queue.rs` is untouched — every one of its
//! queries still runs against a DuckDB `Connection`, and its `ensure_schema`
//! still builds the DuckDB schema. **Nothing in this commit sends an email.**
//!
//! **No §3.4 fold is owed by this family and none is deferred.** §3.4's seven
//! arithmetic sites contain no email statement and §6.3's row for "email outbox
//! / relay queue" says "Carry." The module contains **no SQL arithmetic at
//! all** — no `SUM`, no `AVG`, no `+`, no `*` on any column; `attempt_n + 1` is
//! computed in Rust (`email_relay_queue.rs:247`) and bound as a value. The T-8
//! pending-fold register is **unchanged by this commit**: nothing joined it and
//! nothing left it.
//!
//! **Nor does this family carry an M11-shaped hazard**, and here that is worth a
//! sentence more than elsewhere because the family's payload *is* email
//! addresses. Measured: `LOWER(` and `LIKE` both return **zero** hits across
//! all six email modules — the recipient canonicalisation that M11 would be
//! about happens in **Rust**, in `hash_recipient_list`
//! (`email_relay.rs:85`, `to_ascii_lowercase()`), before the value is ever
//! bound. So no ASCII-`LOWER()` fold crosses the seam here and this family's
//! cutover owes no T-12-shaped fix.
//!
//! **There is no `ensure_columns` ladder**, measured: `ALTER TABLE` returns
//! **zero** hits against this table across `apps/`, `crates/` and `modules/`.
//! The table is created whole and has never been widened, so M8's
//! post-condition has nothing to exercise.
//!
//! ⚠ **S409's two `DROP INDEX IF EXISTS` statements are deliberately NOT
//! ported.** They exist to remove the `state` and `submitter` secondary indexes
//! that installs from `PROD_v2.27.*` (≤ .61) carry, where DuckDB's ART
//! maintenance on an `UPDATE`d indexed column could fatal mid-`mark_sent`. A
//! SQLite database created by this migrator has never had either index, so
//! porting the drops would ship two statements that can never do anything —
//! rule 12. The *outcome* is preserved: the SQLite table has no secondary index,
//! and the family test asserts it.

use std::collections::BTreeMap;

use anyhow::Result;

use aberp_db::engine::{Connection as SqliteConn, ToSql};

use crate::migrate_billing::fold_i64;
use crate::migrate_dispatch::unique_natural_keys;

/// The one table in this family.
const TABLE: &str = "outbound_email_queue";

// ---------------------------------------------------------------------------
// The STRICT DDL — column-for-column, in declaration order, with §3.2's renames
// ---------------------------------------------------------------------------

/// `outbound_email_queue` as `email_relay_queue.rs`'s `SCHEMA_SQL` creates it,
/// `STRICT`.
///
/// `VARCHAR` → `TEXT` (§3.2 H) on thirteen columns; `attempt_n` and `byte_size`
/// → `INTEGER` (§3.2 F). **No `BLOB`** — the attachment bytes are on the
/// filesystem and this row holds only a rel-path (see the module docs).
///
/// **The `PRIMARY KEY` on `id` is preserved verbatim**, because the DuckDB
/// schema declares one. That is the opposite call from Parts F and G, and for
/// the opposite reason: there, adding a key would have moved an invariant into
/// the DDL on one engine only; here, *dropping* it would remove one from the
/// copy that the source has.
///
/// **No secondary index**, matching S409's post-migration DuckDB shape — the
/// `state` index was the PROD bug trigger and the `submitter` index was dead.
/// Neither is created here, so neither has to be dropped here.
///
/// `[[no-sql-specific]]` is otherwise preserved verbatim: **no `CHECK` and no
/// `DEFAULT`** (`email_relay_queue.rs:18–22` states the posture in as many
/// words, naming the S271/S273/S279 `ADD COLUMN … DEFAULT` gotcha). The closed
/// `state` vocabulary lives in `QueueState::parse_str`; `STRICT` constrains
/// storage classes, not vocabularies, and porting the vocabulary into the DDL
/// would fork the two schemas.
const SQLITE_OUTBOUND_EMAIL_QUEUE_DDL: &str = "
CREATE TABLE IF NOT EXISTS outbound_email_queue (
    id                     TEXT    NOT NULL PRIMARY KEY,
    created_at             TEXT    NOT NULL,
    submitter              TEXT    NOT NULL,
    to_recipients_json     TEXT    NOT NULL,
    cc_recipients_json     TEXT,
    subject                TEXT    NOT NULL,
    body_text              TEXT    NOT NULL,
    body_html              TEXT,
    attachments_dir        TEXT,
    state                  TEXT    NOT NULL,
    attempt_n              INTEGER NOT NULL,
    last_error             TEXT,
    sent_at                TEXT,
    recipient_hash         TEXT    NOT NULL,
    byte_size              INTEGER NOT NULL
) STRICT;
";

/// Build the family's SQLite schema.
pub fn ensure_email_schema(conn: &SqliteConn) -> Result<()> {
    conn.execute_batch(SQLITE_OUTBOUND_EMAIL_QUEUE_DDL)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The carried rows
// ---------------------------------------------------------------------------

/// Every `TEXT` column on `outbound_email_queue`, **key first**, in the order
/// the carry reads, writes and compares them.
///
/// One list drives the `SELECT`, the `INSERT`, the per-row equality and the
/// `typeof()` sweep, so the four cannot drift into different column orders.
///
/// The gate's per-row arm covers **all fifteen** rather than a chosen subset,
/// and on this table that is load-bearing in a way it was not on partners: the
/// operator-visible payload is `subject` / `body_text` / `body_html` — which can
/// be hundreds of kilobytes of arbitrary Unicode — and the *operational*
/// payload is `to_recipients_json` and `attachments_dir`, one of which decides
/// who receives the message and the other whether its attachments can still be
/// found. There is no fold and no storage class that can see a drift in any of
/// them.
const EMAIL_TEXT_COLUMNS: &[&str] = &[
    "id",
    "created_at",
    "submitter",
    "to_recipients_json",
    "cc_recipients_json",
    "subject",
    "body_text",
    "body_html",
    "attachments_dir",
    "state",
    "last_error",
    "sent_at",
    "recipient_hash",
];

/// The two §3.2 F counts, in carry order.
const EMAIL_INT_COLUMNS: &[&str] = &["attempt_n", "byte_size"];

/// One `outbound_email_queue` row: the 13 `TEXT` columns in
/// [`EMAIL_TEXT_COLUMNS`] order plus the two §3.2 F counts.
///
/// `Eq` holds: **this family carries no float at all**, so there is no value in
/// it that compares unequal to itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmailRow {
    text: Vec<Option<String>>,
    ints: Vec<i64>,
}

impl EmailRow {
    /// `id` is `EMAIL_TEXT_COLUMNS[0]` and is `NOT NULL` on both engines.
    fn id(&self) -> &str {
        self.text[0].as_deref().unwrap_or("")
    }
}

/// What the email carry did. As everywhere else in this migrator these are
/// **not** the numbers the gate compares (B4) — the gate re-derives every one of
/// them from disk in a separate invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmailCarry {
    pub outbound_email_queue: u64,
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// Is the email/relay family present in this DuckDB database?
///
/// A ledger-only fixture is a legitimate shape, so the migrator has to be able
/// to answer. The answer is then held symmetrically by the gate: present on one
/// side and absent on the other is a hard stop, never a skip.
///
/// Presence is per **family** rather than per table here, and unlike Parts C–G
/// that is not a simplification — the family *is* one table, so the two are the
/// same question.
pub fn family_present_duckdb(conn: &duckdb::Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM information_schema.tables WHERE table_name = ?",
        [TABLE],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Is the email/relay family present in this SQLite database?
pub fn family_present_sqlite(conn: &SqliteConn) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [TABLE],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// The carry
// ---------------------------------------------------------------------------

fn insert_sql() -> String {
    let all: Vec<&str> = EMAIL_TEXT_COLUMNS
        .iter()
        .chain(EMAIL_INT_COLUMNS.iter())
        .copied()
        .collect();
    format!(
        "INSERT INTO {TABLE} ({}) VALUES ({})",
        all.join(", "),
        vec!["?"; all.len()].join(", ")
    )
}

/// Carry the family from an already-open **read-only** DuckDB connection into an
/// already-open SQLite one, inside a single `BEGIN IMMEDIATE`.
///
/// The caller owns both connections, the writer lock and the preconditions; this
/// function does the typed transformation and nothing else. In particular it
/// **does not touch the attachment tree** — the bytes are on the filesystem, the
/// row carries a rel-path, and C-II forbids this plan from reading, writing or
/// stat-ing anything under `~/.aberp/`.
pub fn carry_email(src: &duckdb::Connection, dst: &mut SqliteConn) -> Result<EmailCarry> {
    if !family_present_duckdb(src)? {
        return Ok(EmailCarry::default());
    }
    ensure_email_schema(dst)?;
    let rows = read_duckdb(src)?;

    let sql = insert_sql();
    let tx = aberp_db::engine::begin_immediate(dst)
        .map_err(|e| anyhow::anyhow!("begin the email carry transaction: {e}"))?;
    for row in &rows {
        let mut binds: Vec<&dyn ToSql> =
            Vec::with_capacity(EMAIL_TEXT_COLUMNS.len() + EMAIL_INT_COLUMNS.len());
        for v in &row.text {
            binds.push(v);
        }
        for v in &row.ints {
            binds.push(v);
        }
        tx.execute(&sql, binds.as_slice())?;
    }
    tx.commit()?;

    Ok(EmailCarry {
        outbound_email_queue: rows.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// The read sides
// ---------------------------------------------------------------------------

/// The `SELECT` both engines run.
///
/// **There is no `CAST(… AS VARCHAR)` anywhere in this module**, and that is a
/// statement about the family rather than an omission: no column in it is
/// `DECIMAL` or `DOUBLE` on DuckDB, so no value in it needs an R2 projection.
/// Ordered by `id` — the natural key — so both engines hand back the same order
/// and a diff between two runs is a real difference rather than a row order.
fn select_sql() -> String {
    let all: Vec<&str> = EMAIL_TEXT_COLUMNS
        .iter()
        .chain(EMAIL_INT_COLUMNS.iter())
        .copied()
        .collect();
    format!("SELECT {} FROM {TABLE} ORDER BY id ASC", all.join(", "))
}

/// The two engines' `Connection` types are unrelated concrete types (§2.3), so
/// each read is written once per engine.
///
/// **The DuckDB side validates; the SQLite side does not.** A duplicate key is a
/// property of the source, and [`unique_natural_keys`] must name the source's
/// defect rather than the copy's symptom.
fn read_duckdb(conn: &duckdb::Connection) -> Result<Vec<EmailRow>> {
    let mut stmt = conn.prepare(&select_sql())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(EMAIL_TEXT_COLUMNS.len());
        for i in 0..EMAIL_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        let mut ints = Vec::with_capacity(EMAIL_INT_COLUMNS.len());
        for i in 0..EMAIL_INT_COLUMNS.len() {
            ints.push(r.get::<_, i64>(EMAIL_TEXT_COLUMNS.len() + i)?);
        }
        Ok(EmailRow { text, ints })
    })?;
    let rows: Vec<EmailRow> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    unique_natural_keys(
        &rows.iter().map(|r| r.id().to_string()).collect::<Vec<_>>(),
        TABLE,
    )?;
    Ok(rows)
}

fn read_sqlite(conn: &SqliteConn) -> Result<Vec<EmailRow>> {
    let mut stmt = conn.prepare(&select_sql())?;
    let rows = stmt.query_map([], |r| {
        let mut text = Vec::with_capacity(EMAIL_TEXT_COLUMNS.len());
        for i in 0..EMAIL_TEXT_COLUMNS.len() {
            text.push(r.get::<_, Option<String>>(i)?);
        }
        let mut ints = Vec::with_capacity(EMAIL_INT_COLUMNS.len());
        for i in 0..EMAIL_INT_COLUMNS.len() {
            ints.push(r.get::<_, i64>(EMAIL_TEXT_COLUMNS.len() + i)?);
        }
        Ok(EmailRow { text, ints })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

// ---------------------------------------------------------------------------
// The reconciliation gate's arm
// ---------------------------------------------------------------------------

/// Compare the two sides, per row, over **every** column. Every number here is
/// re-derived from disk by the gate (B4).
pub fn reconcile_email(
    src: &duckdb::Connection,
    dst: &SqliteConn,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    // --- 1. presence, symmetrically ---
    match (family_present_duckdb(src)?, family_present_sqlite(dst)?) {
        (false, false) => {
            checks.push(
                "✓ email/relay family absent on BOTH sides — nothing to reconcile (a database in \
                 which no e-mail has ever been queued is a legitimate shape; an asymmetry is not)"
                    .to_string(),
            );
            return Ok(());
        }
        (true, false) => {
            hard_stops.push(format!(
                "{TABLE} exists in DuckDB but NOT in SQLite — the family was not carried. This is \
                 the silent-skip shape: every check below would have been skipped and the gate \
                 would have reported PASS"
            ));
            return Ok(());
        }
        (false, true) => {
            hard_stops.push(format!(
                "{TABLE} exists in SQLite but NOT in DuckDB — the copy has a table the source \
                 does not. There is no source of truth to reconcile against"
            ));
            return Ok(());
        }
        (true, true) => {}
    }

    // --- 2. row count ---
    let sql = format!("SELECT count(*) FROM {TABLE}");
    let d: i64 = src.query_row(&sql, [], |r| r.get(0))?;
    let l: i64 = dst.query_row(&sql, [], |r| r.get(0))?;
    if d == l {
        checks.push(format!("✓ {TABLE} row count = {d}"));
    } else {
        hard_stops.push(format!("{TABLE} row count: DuckDB {d}, SQLite {l}"));
    }

    // --- 3. per-row equality over EVERY column, keyed on the natural key ---
    let duck = read_duckdb(src)?;
    let lite: BTreeMap<String, EmailRow> = read_sqlite(dst)?
        .into_iter()
        .map(|r| (r.id().to_string(), r))
        .collect();

    let mut drift = 0usize;
    for row in &duck {
        let key = row.id();
        let Some(got) = lite.get(key) else {
            hard_stops.push(format!("queued e-mail {key} is missing on the SQLite side"));
            drift += 1;
            continue;
        };
        for (i, name) in EMAIL_TEXT_COLUMNS.iter().enumerate() {
            if row.text[i] != got.text[i] {
                // The bodies can be hundreds of kilobytes; a hard stop that
                // pasted two of them would bury every other finding in the
                // report. Length + the first differing byte offset localises it
                // without dumping the message.
                hard_stops.push(format!(
                    "queued e-mail {key}: {name} differs — {}",
                    describe_text_drift(row.text[i].as_deref(), got.text[i].as_deref())
                ));
                drift += 1;
            }
        }
        for (i, name) in EMAIL_INT_COLUMNS.iter().enumerate() {
            if row.ints[i] != got.ints[i] {
                hard_stops.push(format!(
                    "queued e-mail {key}: {name} DuckDB {}, SQLite {}",
                    row.ints[i], got.ints[i]
                ));
                drift += 1;
            }
        }
    }
    if drift == 0 {
        checks.push(format!(
            "✓ every {TABLE} column round-trips with ZERO drift ({} row(s) × {} columns)",
            duck.len(),
            EMAIL_TEXT_COLUMNS.len() + EMAIL_INT_COLUMNS.len()
        ));
    }

    // --- 4. the exact sums, folded in RUST on both sides (never SQL SUM) ---
    // Neither column is money, and the discipline applies anyway: §3.4's rule is
    // about the operation, not about the column's business meaning. `SUM` over
    // INTEGER raises on i64 overflow on one engine and silently yields REAL on
    // the other, and a column that happens to be small does not get an
    // exception (§3.4 / T-8).
    for (i, name) in EMAIL_INT_COLUMNS.iter().enumerate() {
        let d = fold_i64(duck.iter().map(|r| r.ints[i]), name)?;
        let l = fold_i64(lite.values().map(|r| r.ints[i]), name)?;
        if d == l {
            checks.push(format!(
                "✓ Σ {TABLE}.{name} = {d} (i64 fold in Rust on both sides)"
            ));
        } else {
            hard_stops.push(format!("Σ {TABLE}.{name}: DuckDB {d}, SQLite {l}"));
        }
    }

    // --- 5. typeof() over every row of every column (T-2 / M1) ---
    for col in EMAIL_TEXT_COLUMNS {
        typeof_sweep(dst, col, "text", checks, hard_stops)?;
    }
    for col in EMAIL_INT_COLUMNS {
        typeof_sweep(dst, col, "integer", checks, hard_stops)?;
    }

    Ok(())
}

/// Localise a text drift without printing the value.
///
/// `body_text` and `body_html` are the only columns in the whole migrated tree
/// that routinely hold six-figure byte counts. A hard stop that interpolated two
/// of them would make the gate's own report unreadable — and the report is what
/// an operator acts on. Byte length plus the first differing offset says which
/// column, which row and where, which is what a repair needs.
fn describe_text_drift(duck: Option<&str>, lite: Option<&str>) -> String {
    match (duck, lite) {
        (None, None) => "both NULL (unreachable — the caller compared unequal)".to_string(),
        (Some(d), None) => format!("DuckDB has {} byte(s), SQLite is NULL", d.len()),
        (None, Some(l)) => format!("DuckDB is NULL, SQLite has {} byte(s)", l.len()),
        (Some(d), Some(l)) => {
            let at = d
                .as_bytes()
                .iter()
                .zip(l.as_bytes())
                .position(|(a, b)| a != b);
            match at {
                Some(off) => format!(
                    "DuckDB {} byte(s), SQLite {} byte(s), first differing byte at offset {off}",
                    d.len(),
                    l.len()
                ),
                None => format!(
                    "one is a prefix of the other: DuckDB {} byte(s), SQLite {} byte(s)",
                    d.len(),
                    l.len()
                ),
            }
        }
    }
}

/// `SELECT typeof(col)` over every non-NULL value of one column (T-2 / M1).
///
/// NULL is permitted wherever the column is nullable; what must never appear is
/// the **wrong** non-null storage class. On `recipient_hash` this is the
/// assertion that the `TEXT`-not-`BLOB` call above actually held: a `'blob'`
/// there would never compare equal to the hex string every reader hands it.
fn typeof_sweep(
    dst: &SqliteConn,
    col: &str,
    want: &str,
    checks: &mut Vec<String>,
    hard_stops: &mut Vec<String>,
) -> Result<()> {
    let bad: i64 = dst.query_row(
        &format!("SELECT count(*) FROM {TABLE} WHERE {col} IS NOT NULL AND typeof({col}) <> ?"),
        [want],
        |r| r.get(0),
    )?;
    if bad == 0 {
        checks.push(format!("✓ typeof({TABLE}.{col}) = '{want}' on every row"));
    } else {
        hard_stops.push(format!(
            "{TABLE}.{col}: {bad} row(s) are not '{want}' — in SQLite TEXT, INTEGER and BLOB are \
             distinct storage classes that never compare equal, so a wrong one here silently \
             breaks lookups"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_db::schema::{COUNT_INT, TEXT};

    /// The carry leads with the column its key is built from, and the `SELECT`
    /// orders on the same one.
    #[test]
    fn the_carry_leads_with_the_natural_key() {
        assert_eq!(EMAIL_TEXT_COLUMNS[0], "id");
        assert!(
            select_sql().ends_with("ORDER BY id ASC"),
            "{}",
            select_sql()
        );
    }

    /// **The family carries no float, no decimal and no BLOB** — the DDL is what
    /// proves it rather than the module docs.
    ///
    /// This is the pin that fails if a later session inlines an attachment as a
    /// `BLOB` column, or adds a cost/size figure on a float, without deciding its
    /// representation — which is the moment R2, R3 and `finite_measurement` start
    /// applying to this family.
    #[test]
    fn no_column_in_this_family_is_a_float_a_decimal_or_a_blob() {
        for banned in [
            "DECIMAL", "NUMERIC", "REAL", "DOUBLE", "FLOAT", "BLOB", "BOOLEAN",
        ] {
            assert!(
                !SQLITE_OUTBOUND_EMAIL_QUEUE_DDL.contains(banned),
                "the email/relay family carries no {banned}; its attachment bytes live on the \
                 filesystem and its row holds a rel-path (email_relay_queue.rs:24-31). \
                 {banned:?} appearing here means that stopped being true:\n{SQLITE_OUTBOUND_EMAIL_QUEUE_DDL}"
            );
        }
    }

    /// `recipient_hash` is a **hex string**, so it crosses as `TEXT`.
    ///
    /// Pinned by name because "hash" in a column name is exactly what would make
    /// a later session reach for R3's `BLOB` by analogy — and in SQLite a `BLOB`
    /// never compares equal to the `TEXT` every reader of this column hands it.
    /// Same call §6.3 already took on `audit_ledger_anchors.
    /// chain_head_hash_at_anchor`.
    #[test]
    fn the_recipient_hash_is_text_not_blob() {
        assert!(declared_as("recipient_hash", "TEXT"));
        assert!(EMAIL_TEXT_COLUMNS.contains(&"recipient_hash"));
        assert!(!EMAIL_INT_COLUMNS.contains(&"recipient_hash"));
    }

    /// Is `col` declared with `ty` in the DDL? Matched on the line whose first
    /// token is the column name, so the pins below do not depend on the DDL's
    /// column alignment.
    fn declared_as(col: &str, ty: &str) -> bool {
        SQLITE_OUTBOUND_EMAIL_QUEUE_DDL
            .lines()
            .filter(|l| l.split_whitespace().next() == Some(col))
            .any(|l| l.split_whitespace().nth(1) == Some(ty))
    }

    /// The declared types this module uses are the vocabulary constants, spelled
    /// the same way, and the table is `STRICT`.
    #[test]
    fn the_ddl_uses_the_adr0108_type_vocabulary() {
        assert_eq!(TEXT, "TEXT");
        assert_eq!(COUNT_INT, "INTEGER");
        for banned in ["VARCHAR", "BIGINT"] {
            assert!(
                !SQLITE_OUTBOUND_EMAIL_QUEUE_DDL.contains(banned),
                "{TABLE} must not declare {banned}"
            );
        }
        assert!(SQLITE_OUTBOUND_EMAIL_QUEUE_DDL.contains("STRICT"));
        // §3.2 F: both counts are INTEGER, by name.
        for col in EMAIL_INT_COLUMNS {
            assert!(
                SQLITE_OUTBOUND_EMAIL_QUEUE_DDL
                    .lines()
                    .any(|l| l.trim_start().starts_with(col) && l.contains(COUNT_INT)),
                "§3.2 F names {TABLE}.{col} as an INTEGER count"
            );
        }
    }

    /// **The `PRIMARY KEY` is preserved and no index is created**, matching the
    /// DuckDB schema after S409 dropped both secondary indexes.
    ///
    /// Both halves matter. Dropping the key would remove an invariant the source
    /// has; creating an index would re-introduce the one whose `UPDATE`-time ART
    /// maintenance fataled `mark_sent` in PROD on 2026-06-14 — and `state` is
    /// `UPDATE`d on every transition of every row.
    #[test]
    fn the_primary_key_is_kept_and_no_secondary_index_is_created() {
        assert!(declared_as("id", "TEXT"));
        assert!(
            SQLITE_OUTBOUND_EMAIL_QUEUE_DDL
                .lines()
                .filter(|l| l.split_whitespace().next() == Some("id"))
                .any(|l| l.contains("PRIMARY KEY")),
            "the DuckDB schema declares `id … PRIMARY KEY`; dropping it here would remove an \
             invariant the source has"
        );
        assert!(
            !SQLITE_OUTBOUND_EMAIL_QUEUE_DDL.contains("CREATE INDEX"),
            "S409 dropped both secondary indexes; re-creating one here would fork the schemas"
        );
        for banned in ["CHECK", "DEFAULT", "UNIQUE"] {
            assert!(
                !SQLITE_OUTBOUND_EMAIL_QUEUE_DDL.contains(banned),
                "email_relay_queue.rs:18-22's [[no-sql-specific]] posture declares no {banned}"
            );
        }
    }

    /// The generated SQL names every column of the table exactly once, in the
    /// same order, in both statements — which is what makes the positional decode
    /// in [`read_duckdb`] / [`read_sqlite`] safe.
    #[test]
    fn the_select_and_the_insert_agree_on_column_order() {
        let cols: Vec<&str> = EMAIL_TEXT_COLUMNS
            .iter()
            .chain(EMAIL_INT_COLUMNS.iter())
            .copied()
            .collect();
        assert_eq!(cols.len(), 15, "the table has fifteen columns");
        let joined = cols.join(", ");
        assert!(
            select_sql().starts_with(&format!("SELECT {joined} FROM {TABLE}")),
            "{}",
            select_sql()
        );
        assert!(
            insert_sql().starts_with(&format!("INSERT INTO {TABLE} ({joined})")),
            "{}",
            insert_sql()
        );
        // And the column set is exactly the DDL's, no more and no fewer.
        let declared = SQLITE_OUTBOUND_EMAIL_QUEUE_DDL
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|w| cols.contains(w))
            .count();
        assert_eq!(
            declared,
            cols.len(),
            "the carry names {} columns but the DDL declares {declared} of them",
            cols.len()
        );
    }

    /// [`describe_text_drift`] localises without dumping: a 300 KB body that
    /// differs at one byte must produce a short message naming the offset, not
    /// 600 KB of report.
    #[test]
    fn a_body_drift_is_localised_rather_than_pasted_into_the_report() {
        // Ten ASCII bytes then 100 000 two-byte characters: 200 010 bytes, with
        // a char boundary at offset 7 so the mutation below stays valid UTF-8.
        let a = format!("{}{}", "x".repeat(10), "é".repeat(100_000));
        let mut b = a.clone();
        b.push('q');
        let msg = describe_text_drift(Some(&a), Some(&b));
        assert!(
            msg.len() < 200,
            "the message must not carry the body: {msg}"
        );
        assert!(msg.contains("200010 byte(s)"), "{msg}");
        assert!(msg.contains("200011 byte(s)"), "{msg}");
        // A pure append has no differing byte — say so rather than claim one.
        assert!(msg.contains("prefix"), "{msg}");

        // And a real mid-body difference names the offset.
        let mut c = a.clone();
        c.replace_range(7..8, "Z");
        let msg = describe_text_drift(Some(&a), Some(&c));
        assert!(msg.contains("offset 7"), "{msg}");

        // The NULL arms, which the per-row comparison reaches on every nullable
        // column and which a `both non-null` comparison would have missed.
        assert!(describe_text_drift(Some("abc"), None).contains("SQLite is NULL"));
        assert!(describe_text_drift(None, Some("abc")).contains("DuckDB is NULL"));
    }
}
