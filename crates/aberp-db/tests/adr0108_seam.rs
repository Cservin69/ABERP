//! ADR-0108 Step 3 — the seam's two-connection pins.
//!
//! Unit tests can assert what one connection does. These are the properties
//! that only exist between two: M5's `BEGIN IMMEDIATE` (T-6), and the
//! lexicographic hazard that F1's Rust folds exist to avoid (Q2).
//!
//! The SQLite arms are `sqlite-engine`-gated. Under the default (DuckDB) build
//! they are compiled out rather than skipped, because a test that silently
//! passes by not running is the shape ADR-0108's gates keep finding.

#![allow(clippy::items_after_test_module)]

// ---------------------------------------------------------------------------
// Q2 — the lexicographic hazard, demonstrated on a real engine
// ---------------------------------------------------------------------------

/// **The hazard F1's folds exist to avoid, shown rather than argued.**
///
/// ADR-0108 R2 stores quantities as `TEXT` holding a canonical decimal string.
/// A SQL `<` between two such columns is **lexicographic**, and this test is
/// the demonstration: `'9' < '10'` is FALSE in SQL and TRUE in `Decimal`.
///
/// It matters because `crates/aberp-inventory/src/repository.rs` ran exactly
/// that comparison, twice, and the symptom is silent: a product at 9 units
/// against a minimum of 10 simply stops appearing on the operator's low-stock
/// list. No error, no log line, one missing row.
///
/// `low_stock_uses_numeric_not_lexicographic_comparison` (in
/// `aberp-inventory`) pins the CONSUMER's behaviour but cannot go red on
/// DuckDB, where `DECIMAL` compares numerically no matter where the comparison
/// lives. **This is the test that can.** Together they are the pin: one shows
/// the engine really does this, the other shows ABERP does not rely on it.
#[cfg(feature = "sqlite-engine")]
#[test]
fn q2_a_sql_comparison_between_two_text_decimals_is_lexicographic() {
    use rust_decimal::Decimal;
    use std::str::FromStr as _;

    let dir = std::env::temp_dir().join(format!("aberp-adr0108-q2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("q2.sqlite");
    let _ = std::fs::remove_file(&db);
    let conn = aberp_db::sqlite::open_hardened(&db).unwrap();

    conn.execute_batch(
        "CREATE TABLE products (
             id        TEXT NOT NULL,
             stock_qty TEXT,
             min_stock TEXT
         ) STRICT;
         INSERT INTO products VALUES ('a', '9',   '10');
         INSERT INTO products VALUES ('b', '100', '20');
         INSERT INTO products VALUES ('c', NULL,  '5');",
    )
    .unwrap();

    // What the removed SQL predicate would return.
    let mut stmt = conn
        .prepare("SELECT id FROM products WHERE COALESCE(stock_qty,0) < COALESCE(min_stock,0) ORDER BY id")
        .unwrap();
    let sql_says: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    // What is actually true.
    let mut stmt = conn
        .prepare("SELECT id, stock_qty, min_stock FROM products ORDER BY id")
        .unwrap();
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let rust_says: Vec<String> = rows
        .iter()
        .filter(|(_, s, m)| {
            let s = s
                .as_deref()
                .map(|v| Decimal::from_str(v).unwrap())
                .unwrap_or(Decimal::ZERO);
            let m = m
                .as_deref()
                .map(|v| Decimal::from_str(v).unwrap())
                .unwrap_or(Decimal::ZERO);
            s < m
        })
        .map(|(id, _, _)| id.clone())
        .collect();

    assert_eq!(
        rust_says,
        vec!["a".to_string(), "c".to_string()],
        "numerically: 9 < 10 and NULL(0) < 5; 100 is not < 20"
    );
    assert_eq!(
        sql_says,
        vec!["b".to_string(), "c".to_string()],
        "lexicographically SQL returns the WRONG product ('100' < '20' as text, '9' > '10') — \
         if this assertion ever changes, SQLite changed its storage-class ordering and \
         ADR-0108 §3.4 needs re-deriving, not this test relaxing"
    );
    assert_ne!(
        sql_says, rust_says,
        "the two must disagree — a test where they agree proves nothing about the hazard"
    );

    let _ = std::fs::remove_file(&db);
}

// ---------------------------------------------------------------------------
// M5 / T-6 — BEGIN IMMEDIATE and the forked chain
// ---------------------------------------------------------------------------

/// **T-6 (M5).** Two writers interleaving read-head → append must not produce
/// two links off one `prev_hash`.
///
/// The shape under test is the audit-chain append, reduced to its essentials:
/// read the current head, decide the next `seq`/`prev_hash` from it, write.
/// With a DEFERRED transaction both writers read the same head before either
/// writes, and the loser only discovers this at COMMIT — after the application
/// has already computed what to write from a stale read. That is a forked
/// ledger, and the Defense line forked four times (seq 369→416→428→515) from
/// exactly this.
///
/// `BEGIN IMMEDIATE` takes the write lock at `BEGIN`, so the second writer is
/// refused *before* it reads. Both halves are asserted: the deferred arm is
/// shown to produce the fork, and the immediate arm is shown not to. An arm
/// that has never been observed failing is not evidence.
#[cfg(feature = "sqlite-engine")]
#[test]
fn t6_two_writers_cannot_fork_the_chain_under_begin_immediate() {
    use rusqlite::TransactionBehavior;

    let dir = std::env::temp_dir().join(format!("aberp-adr0108-t6-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Shared setup: a one-row "chain head" table and two connections on it.
    let make = |name: &str| {
        let db = dir.join(name);
        let _ = std::fs::remove_file(&db);
        let c = aberp_db::sqlite::open_hardened(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE chain (seq INTEGER NOT NULL, prev_hash BLOB NOT NULL) STRICT;
             INSERT INTO chain VALUES (1, x'00');",
        )
        .unwrap();
        // `busy_timeout` is 5 s by default; a contention test must not wait it
        // out, so this pair fails fast instead.
        c.busy_timeout(std::time::Duration::from_millis(0)).unwrap();
        (db, c)
    };

    // --- arm 1: DEFERRED. Both read the head; the fork is decided before the
    //     loser finds out. ---
    let (db_a, mut a1) = make("t6-deferred.sqlite");
    let mut a2 = aberp_db::sqlite::open_hardened(&db_a).unwrap();
    a2.busy_timeout(std::time::Duration::from_millis(0))
        .unwrap();

    let tx1 = a1
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .unwrap();
    let tx2 = a2
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .unwrap();
    let head1: i64 = tx1
        .query_row("SELECT max(seq) FROM chain", [], |r| r.get(0))
        .unwrap();
    let head2: i64 = tx2
        .query_row("SELECT max(seq) FROM chain", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        head1, head2,
        "DEFERRED lets BOTH writers read the same head — this is the fork being decided"
    );
    drop(tx1);
    drop(tx2);

    // --- arm 2: IMMEDIATE, through the seam. The second writer never gets to
    //     read a head it would then act on. ---
    let (db_b, mut b1) = make("t6-immediate.sqlite");
    let mut b2 = aberp_db::sqlite::open_hardened(&db_b).unwrap();
    b2.busy_timeout(std::time::Duration::from_millis(0))
        .unwrap();

    let tx1 = aberp_db::engine::begin_immediate(&mut b1).expect("first writer begins");
    let second_is_refused = aberp_db::engine::begin_immediate(&mut b2).is_err();
    assert!(
        second_is_refused,
        "with BEGIN IMMEDIATE the second writer must be refused AT BEGIN — before it can \
         read a head and compute a link off it (M5)"
    );

    // The first writer proceeds normally and commits.
    let head: i64 = tx1
        .query_row("SELECT max(seq) FROM chain", [], |r| r.get(0))
        .unwrap();
    tx1.execute("INSERT INTO chain VALUES (?, x'11')", [head + 1])
        .unwrap();
    tx1.commit().unwrap();

    // And once it is done, the second writer sees the NEW head — not the one it
    // would have forked off.
    let tx2 = aberp_db::engine::begin_immediate(&mut b2).expect("the lock is free again");
    let head_after: i64 = tx2
        .query_row("SELECT max(seq) FROM chain", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        head_after, 2,
        "the second writer reads the committed head, not a stale one"
    );
    drop(tx2);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// M12 — the five `ON CONFLICT` sites
// ---------------------------------------------------------------------------

/// The five executable `ON CONFLICT` sites, as `(label, DDL, conflict target)`.
///
/// **The audit is empty work, and the census that said otherwise was wrong the
/// same way `ALTER COLUMN` was.** A raw grep returns 21 hits; 16 are doc
/// comments and 1 is a test assertion string (`quote_pricing_jobs.rs:3112`).
/// The 5 real ones, re-verified against the DDL at the line ADR-0108 §4.3
/// cites:
///
/// | site | table | declared PRIMARY KEY | DDL at |
/// |---|---|---|---|
/// | `material_inventory.rs:555` | `inventory_balances` | `(tenant_id, material_grade)` | `material_inventory.rs:235` |
/// | `supplier_prices.rs:470` | `quote_price_snapshots` | `(tenant_id, price_set_hash, grade)` | `supplier_prices.rs:429` |
/// | `quote_pricing_jobs.rs:415` | `quote_pricing_jobs` | `quote_id` | `quote_pricing_jobs.rs:248` |
/// | `quote_pricing_jobs.rs:476` | `quote_pricing_jobs` | `quote_id` | (same table) |
/// | `restore_from_nav_outgoing.rs:326` | `restore_lock` | `tenant_id` | `restore_from_nav_outgoing.rs:270` |
///
/// Every conflict target **is** the table's declared `PRIMARY KEY`, so SQLite
/// resolves each upsert against the PK's implicit unique index exactly as
/// DuckDB does. **Zero `UNIQUE` indexes to add, zero rewrites** — so the
/// `[[no-sql-specific]]` tension question Q5 was built around does not exist.
///
/// What DOES need confirming is the behaviour some of these sites branch on:
/// they read the affected-row count as an idempotency signal. This is a
/// **column-shape** reconstruction rather than five reaches into five modules —
/// it exercises each site's actual conflict-target arity (2-col, 3-col, 1-col
/// ×2) without duplicating five nearly-identical tests (rule 12). The DDL
/// evidence above is the other half.
#[cfg(feature = "sqlite-engine")]
const ON_CONFLICT_SITES: &[(&str, &str, &str, &str)] = &[
    (
        "material_inventory.rs:555 → inventory_balances",
        "CREATE TABLE inventory_balances (tenant_id TEXT NOT NULL, material_grade TEXT NOT NULL, \
         on_hand TEXT NOT NULL, PRIMARY KEY (tenant_id, material_grade)) STRICT",
        "INSERT INTO inventory_balances VALUES ('t', 'S355', '1') \
         ON CONFLICT (tenant_id, material_grade) DO NOTHING",
        "SELECT count(*) FROM inventory_balances",
    ),
    (
        "supplier_prices.rs:470 → quote_price_snapshots",
        "CREATE TABLE quote_price_snapshots (tenant_id TEXT NOT NULL, price_set_hash TEXT NOT NULL, \
         grade TEXT NOT NULL, cost_per_kg_eur TEXT NOT NULL, \
         PRIMARY KEY (tenant_id, price_set_hash, grade)) STRICT",
        "INSERT INTO quote_price_snapshots VALUES ('t', 'h', 'S355', '1.25') \
         ON CONFLICT (tenant_id, price_set_hash, grade) DO NOTHING",
        "SELECT count(*) FROM quote_price_snapshots",
    ),
    (
        "quote_pricing_jobs.rs:415 → quote_pricing_jobs",
        "CREATE TABLE quote_pricing_jobs (quote_id TEXT NOT NULL PRIMARY KEY, state TEXT NOT NULL) STRICT",
        "INSERT INTO quote_pricing_jobs VALUES ('q1', 'queued') ON CONFLICT (quote_id) DO NOTHING",
        "SELECT count(*) FROM quote_pricing_jobs",
    ),
    (
        "quote_pricing_jobs.rs:476 → quote_pricing_jobs (re-enqueue)",
        "CREATE TABLE quote_pricing_jobs (quote_id TEXT NOT NULL PRIMARY KEY, state TEXT NOT NULL) STRICT",
        "INSERT INTO quote_pricing_jobs VALUES ('q1', 'fetched') ON CONFLICT (quote_id) DO NOTHING",
        "SELECT count(*) FROM quote_pricing_jobs",
    ),
    (
        "restore_from_nav_outgoing.rs:326 → restore_lock",
        "CREATE TABLE restore_lock (tenant_id TEXT NOT NULL PRIMARY KEY, acquired_at TEXT NOT NULL) STRICT",
        "INSERT INTO restore_lock VALUES ('t', '2026-07-31') ON CONFLICT (tenant_id) DO NOTHING",
        "SELECT count(*) FROM restore_lock",
    ),
];

/// **M12 — each site's upsert is a no-op on a duplicate, and reports 0 changed
/// rows.**
///
/// The affected-row count is the load-bearing half: several of these sites
/// branch on it as an idempotency signal (`restore_lock` treats "0 rows
/// affected" as *a lock was already held*, which is a refusal, not a retry).
/// SQLite's `changes()` returning 0 for a skipped upsert row is the same as
/// DuckDB's — **pinned, not re-derived**, because a silent change here turns a
/// refusal into a false success on the NAV restore path.
#[cfg(feature = "sqlite-engine")]
#[test]
fn m12_every_on_conflict_site_is_a_no_op_on_a_duplicate_and_reports_zero_changes() {
    for (label, ddl, upsert, count_sql) in ON_CONFLICT_SITES {
        let conn = aberp_db::engine::Connection::open_in_memory().unwrap();
        conn.execute_batch(ddl)
            .unwrap_or_else(|e| panic!("{label}: DDL failed: {e}"));

        let first = conn
            .execute(upsert, [])
            .unwrap_or_else(|e| panic!("{label}: first upsert failed: {e}"));
        assert_eq!(first, 1, "{label}: the first insert must land");

        let second = conn
            .execute(upsert, [])
            .unwrap_or_else(|e| panic!("{label}: the duplicate upsert must NOT error: {e}"));
        assert_eq!(
            second, 0,
            "{label}: a skipped upsert must report 0 affected rows — sites branch on this \
             as an idempotency signal"
        );

        let n: i64 = conn.query_row(count_sql, [], |r| r.get(0)).unwrap();
        assert_eq!(
            n, 1,
            "{label}: the duplicate must not have created a second row"
        );
    }
}

// ---------------------------------------------------------------------------
// The seam under the DEFAULT build
// ---------------------------------------------------------------------------

/// Under the default build the seam must still be a working transaction API —
/// this is what the 84 `Handle` call sites keep using while the migration is in
/// flight, and a `begin_immediate` that only worked on SQLite would break every
/// one of them the moment they were rewritten to use it.
#[test]
fn begin_immediate_is_a_working_transaction_under_the_linked_engine() {
    let mut conn = aberp_db::engine::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE chain (seq BIGINT NOT NULL);")
        .unwrap();

    let tx = aberp_db::engine::begin_immediate(&mut conn).unwrap();
    let head: Option<i64> = tx
        .query_row("SELECT max(seq) FROM chain", [], |r| r.get(0))
        .unwrap();
    tx.execute("INSERT INTO chain VALUES (?)", [head.unwrap_or(0) + 1])
        .unwrap();
    tx.commit().unwrap();

    let n: i64 = conn
        .query_row("SELECT max(seq) FROM chain", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}
