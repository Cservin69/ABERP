//! ADR-0108 Step 7 Part I — **the approved-vendor-list family's crossing, pinned
//! against real storage.**
//!
//! One table (`avl_vendors`, 12 columns), and **not one number in it** — every
//! column is `VARCHAR` on DuckDB. So this file cannot pin what the other Parts'
//! files pin: there is no R2 round-trip, no `finite_measurement`, no Σ fold and
//! no `typeof` = `'integer'` arm, because there is no numeric column for any of
//! them to apply to. What it pins instead is:
//!
//! 1. **every one of the twelve columns crosses byte-for-byte**, which on this
//!    table is the *whole* of the value comparison rather than a supplement to a
//!    fold;
//! 2. **the PO gate reaches the same verdict after the crossing as before it** —
//!    the business invariant this table exists to carry, and the one no storage
//!    class can see;
//! 3. **a partner's several AVL entries keep their newest-wins order**, which is
//!    the one duplicate this family legitimately produces;
//! 4. the identity refusal, both arms;
//! 5. the gate's own teeth — one changed column at a time reds it.
//!
//! ⚠ **This file does not prove purchasing can cut over.** §9 sequences that
//! behind the *Rust* halves of both this family and QA/QC, because `resolve_avl`
//! and `create_ncr` run on purchasing's own writer guard, and a migrator half
//! never holds the product's guard.
#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_dispatch::unique_natural_keys;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};
use aberp_compliance::avl::{ApprovalCategory, ApprovedStatus};

const TENANT: &str = "test";
const TABLE: &str = "avl_vendors";

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-avl-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// One seeded `avl_vendors` row, in [`AVL_COLUMNS`] order minus `tenant_id`.
///
/// `(id, partner_id, approved_status, approval_categories, approved_until_utc,
///   screening_notes, reviewer_login, reviewed_at_utc, revoked_reason,
///   created_at, updated_at)`
type Case = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    &'static str,
);

/// The five `ApprovedStatus` arms, both `blocks_po` verdicts, every nullable
/// column in both arms, and the two shapes that a carry which dropped a column
/// would silently reproduce from a default.
///
/// * **`avl_01`** — `pending`: the freshly-added shape. No expiry, no review
///   stamp, no revoke reason: **all three nullables `NULL`**. Does not block a
///   PO, which is the arm most easily got wrong (only `suspended` / `revoked`
///   block).
/// * **`avl_02`** — `approved`, every nullable populated, a **multi-category**
///   set. `approval_categories` is comma-joined storage, so this row is what
///   proves the join survives as one `TEXT` value rather than being re-split.
/// * **`avl_03`** — `conditional`, a single category. Does not block.
/// * **`avl_04`** — `suspended`: **blocks**. Carries a `revoked_reason` even
///   though it is not revoked, because the column is free-form and a carry that
///   keyed the column off the status would still pass without this row.
/// * **`avl_05`** — `revoked`: **blocks**, terminal, with the operator-facing
///   reason the UI shows.
/// * **`avl_06_empty`** — `screening_notes` and `approval_categories` both the
///   **empty string**, which is a value: `approval_categories = ''` is how the
///   product stores "no categories" (`row_to_vendor` maps it back to an empty
///   vec), and a carry that dropped either column and left SQLite's `NULL`
///   would violate `NOT NULL` — or, worse, a carry that normalised `''` to
///   `NULL` would turn a valid empty set into a schema violation on read-back.
/// * **`avl_07_dup_a` / `avl_07_dup_b`** — **the same `partner_id`**, different
///   `created_at`. This is the duplicate the family legitimately produces
///   (`get_vendor_by_partner` takes the newest of several), so it must be
///   carried rather than refused — and the newest must still win afterwards.
const CASES: &[Case] = &[
    (
        "avl_01",
        "prt_alpha",
        "pending",
        "general",
        None,
        "Új beszállító, még nincs átvilágítva.",
        "operator",
        None,
        None,
        "2026-01-01T08:00:00Z",
        "2026-01-01T08:00:00Z",
    ),
    (
        "avl_02",
        "prt_beta",
        "approved",
        "general,itar,aerospace",
        Some("2027-06-30T23:59:59Z"),
        "AS9100 audit rendben. 100% megfelelés — lásd a _jegyzőkönyvet_.",
        "auditor@aben.ch",
        Some("2026-01-02T10:30:00Z"),
        Some("n/a"),
        "2026-01-02T08:00:00Z",
        "2026-01-02T10:30:00Z",
    ),
    (
        "avl_03",
        "prt_gamma",
        "conditional",
        "ear99",
        Some("2026-12-31T23:59:59Z"),
        "Feltételes: csak EAR99 tételekre.",
        "operator",
        Some("2026-01-03T09:00:00Z"),
        None,
        "2026-01-03T08:00:00Z",
        "2026-01-03T09:00:00Z",
    ),
    (
        "avl_04",
        "prt_delta",
        "suspended",
        "defense,nuclear",
        Some("2026-03-01T00:00:00Z"),
        "Felfüggesztve — folyamatban lévő NCR.",
        "qa@aben.ch",
        Some("2026-02-01T12:00:00Z"),
        Some("NCR-2026-0042 lezárásáig"),
        "2026-01-04T08:00:00Z",
        "2026-02-01T12:00:00Z",
    ),
    (
        "avl_05",
        "prt_epsilon",
        "revoked",
        "general",
        None,
        "Visszavonva.",
        "operator",
        Some("2026-02-10T15:00:00Z"),
        Some("Ismételt szállítási hiba; 'nem javítható' — 100%"),
        "2026-01-05T08:00:00Z",
        "2026-02-10T15:00:00Z",
    ),
    (
        "avl_06_empty",
        "prt_zeta",
        "pending",
        "",
        None,
        "",
        "operator",
        None,
        None,
        "2026-01-06T08:00:00Z",
        "2026-01-06T08:00:00Z",
    ),
    (
        "avl_07_dup_a",
        "prt_eta",
        "revoked",
        "general",
        None,
        "Régi bejegyzés.",
        "operator",
        Some("2026-01-07T09:00:00Z"),
        Some("Lecserélve az új bejegyzésre"),
        "2026-01-07T08:00:00Z",
        "2026-01-07T09:00:00Z",
    ),
    (
        "avl_07_dup_b",
        "prt_eta",
        "approved",
        "general,aerospace",
        Some("2027-01-01T00:00:00Z"),
        "Új bejegyzés — ez a hatályos.",
        "operator",
        Some("2026-05-01T09:00:00Z"),
        None,
        "2026-05-01T08:00:00Z",
        "2026-05-01T09:00:00Z",
    ),
];

/// The partner with two entries, and the id of the one that must win.
const DUP_PARTNER: &str = "prt_eta";
const DUP_WINNER: &str = "avl_07_dup_b";

/// How many generated rows pin the sweep through real storage.
const SWEPT_ROWS: usize = 256;

/// A deterministic xorshift, so a failure is reproducible from the file alone.
fn rng(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// A `TEXT` value across the shapes an AVL row actually holds — operator logins,
/// RFC-3339 stamps, comma-joined category sets, free-form Hungarian screening
/// notes — plus the ones that break a naive bind: quotes, a literal `%` and `_`,
/// CRLF, a tab, and the empty string.
///
/// The literal `%` and `_` matter more here than the column count suggests: they
/// are LIKE metacharacters, and this family's cutover is the one that has to stay
/// free of a `LIKE`-shaped fold (there is none today, measured).
fn swept_text(i: usize, state: &mut u64) -> String {
    match i % 8 {
        0 => String::new(),
        1 => format!("operator{}@aben.ch", rng(state) % 1000),
        2 => format!(
            "2026-{:02}-{:02}T{:02}:00:00Z",
            1 + i % 12,
            1 + i % 28,
            i % 24
        ),
        3 => "general,itar,ear99,aerospace,defense,nuclear".to_string(),
        4 => format!("100% _ árvíztűrő 'tükörfúrógép' \"{}\"", rng(state) % 97),
        5 => format!("sor1\r\nsor2\ttab\\backslash {}", rng(state) % 89),
        6 => format!("大文字 Ünnepi ✅ {}", rng(state) % 83),
        _ => format!("avl-note-{}", rng(state)),
    }
}

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema`, so the SQLite
/// side is compared against the schema the product actually builds rather than
/// against a hand-written copy of it.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp::avl_vendors::ensure_schema(&conn).unwrap();

        for c in CASES {
            insert_row(
                &conn, c.0, TENANT, c.1, c.2, c.3, c.4, c.5, c.6, c.7, c.8, c.9, c.10,
            );
        }

        let mut st = 0x5eed_0a71_u64;
        for i in 0..SWEPT_ROWS {
            let id = format!("avl_sweep_{i:04}");
            let notes = swept_text(i, &mut st);
            let login = swept_text(i + 1, &mut st);
            let cats = swept_text(i + 3, &mut st);
            let until = (i % 3 != 0).then(|| swept_text(i + 2, &mut st));
            let reviewed = (i % 4 != 0).then(|| swept_text(i + 2, &mut st));
            let revoked = (i % 5 != 0).then(|| swept_text(i + 4, &mut st));
            insert_row(
                &conn,
                &id,
                TENANT,
                &format!("prt_sweep_{}", i % 37),
                ["pending", "approved", "conditional", "suspended", "revoked"][i % 5],
                &cats,
                until.as_deref(),
                &notes,
                &login,
                reviewed.as_deref(),
                revoked.as_deref(),
                &format!("2026-06-{:02}T08:00:00Z", 1 + i % 28),
                &format!("2026-07-{:02}T08:00:00Z", 1 + i % 28),
            );
        }
        conn.close().unwrap();
    }
    seed_ledger(&db);
    db
}

/// One `avl_vendors` row as a raw `INSERT`.
///
/// Deliberately **not** through `create_vendor`: that generates its own ULID,
/// stamps `created_at` / `updated_at` / `reviewed_at_utc` from the clock and
/// hard-codes `revoked_reason = NULL`, so it cannot produce the five status arms
/// or both arms of every nullable column that the pins below need.
#[allow(clippy::too_many_arguments)]
fn insert_row(
    conn: &duckdb::Connection,
    id: &str,
    tenant: &str,
    partner_id: &str,
    approved_status: &str,
    approval_categories: &str,
    approved_until_utc: Option<&str>,
    screening_notes: &str,
    reviewer_login: &str,
    reviewed_at_utc: Option<&str>,
    revoked_reason: Option<&str>,
    created_at: &str,
    updated_at: &str,
) {
    conn.execute(
        "INSERT INTO avl_vendors (id, tenant_id, partner_id, approved_status, \
         approval_categories, approved_until_utc, screening_notes, reviewer_login, \
         reviewed_at_utc, revoked_reason, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);",
        duckdb::params![
            id,
            tenant,
            partner_id,
            approved_status,
            approval_categories,
            approved_until_utc,
            screening_notes,
            reviewer_login,
            reviewed_at_utc,
            revoked_reason,
            created_at,
            updated_at,
        ],
    )
    .unwrap();
}

/// The audit chain + mirror + tamper-evidence layer the Step-4 gate turns on.
fn seed_ledger(db: &Path) {
    {
        let mut ledger = Ledger::open(
            db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([8u8; 32]),
        )
        .unwrap();
        for i in 0..3 {
            ledger
                .append(
                    EventKind::DbAutoRecovered,
                    format!(r#"{{"n":{i}}}"#).into_bytes(),
                    Actor::test_only(),
                    None,
                )
                .unwrap();
        }
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(db))
            .unwrap();
    }
    let conn = duckdb::Connection::open(db).unwrap();
    conn.execute_batch(
        "UPDATE audit_ledger
            SET session_id = 'sess-8',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 'sess-8', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                 '2026-08-02T00:00:00Z')",
        duckdb::params![TENANT, vec![8u8; 8]],
    )
    .unwrap();
    conn.close().unwrap();
}

/// Migrate a freshly-seeded fixture and return `(dir, duckdb, sqlite)`.
fn crossed(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(tag);
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.avl.avl_vendors, (CASES.len() + SWEPT_ROWS) as u64);
    (dir, db, lite)
}

fn sqlite_text(lite: &Path, sql: &str) -> Option<String> {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .unwrap()
}

fn sqlite_i64(lite: &Path, sql: &str) -> i64 {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

/// Every column of one row except the two key columns, read back out of SQLite
/// in `CASES` order.
fn sqlite_row(lite: &Path, id: &str) -> Vec<Option<String>> {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(
        "SELECT partner_id, approved_status, approval_categories, approved_until_utc, \
         screening_notes, reviewer_login, reviewed_at_utc, revoked_reason, created_at, updated_at \
         FROM avl_vendors WHERE tenant_id = ? AND id = ?",
        [TENANT, id],
        |r| {
            let mut out = Vec::with_capacity(10);
            for i in 0..10 {
                out.push(r.get::<_, Option<String>>(i)?);
            }
            Ok(out)
        },
    )
    .unwrap()
}

/// The twelve columns, in the migrator's carry order.
const AVL_COLUMNS: &[&str] = &[
    "tenant_id",
    "id",
    "partner_id",
    "approved_status",
    "approval_categories",
    "approved_until_utc",
    "screening_notes",
    "reviewer_login",
    "reviewed_at_utc",
    "revoked_reason",
    "created_at",
    "updated_at",
];

// ---------------------------------------------------------------------------
// 1. The headline
// ---------------------------------------------------------------------------

/// The table crosses, the gate passes, and every column read back from SQLite is
/// the value DuckDB held.
///
/// The read-back is done here as well as inside the gate: the gate compares the
/// two sides against each other, whereas the assertions below compare SQLite
/// against the literal constants the fixture was built from — so two sides that
/// were wrong in the same way would still be caught.
#[test]
fn the_avl_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("headline");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "the gate must pass: {:#?}",
        r.hard_stops
    );
    assert!(
        r.checks.iter().any(|c| c.contains("avl_vendors row count")),
        "the gate must SAY it checked the row count, not just not-fail: {:#?}",
        r.checks
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("every avl_vendors column round-trips with ZERO drift")),
        "{:#?}",
        r.checks
    );

    assert_eq!(
        sqlite_i64(&lite, "SELECT count(*) FROM avl_vendors"),
        (CASES.len() + SWEPT_ROWS) as i64
    );

    for c in CASES {
        let got = sqlite_row(&lite, c.0);
        let want: Vec<Option<String>> = vec![
            Some(c.1.to_string()),
            Some(c.2.to_string()),
            Some(c.3.to_string()),
            c.4.map(str::to_string),
            Some(c.5.to_string()),
            Some(c.6.to_string()),
            c.7.map(str::to_string),
            c.8.map(str::to_string),
            Some(c.9.to_string()),
            Some(c.10.to_string()),
        ];
        assert_eq!(got, want, "row {} drifted", c.0);
    }
}

/// **The empty string survives as the empty string, on both `NOT NULL` columns
/// that can hold one.**
///
/// Split out from the headline because `''` → `NULL` is the single most likely
/// silent normalisation across a bind boundary, and it is not a cosmetic one
/// here: `approval_categories = ''` is how the product stores "no categories"
/// (`row_to_vendor` maps it back to an empty vec), and the column is `NOT NULL`,
/// so a normalisation to `NULL` would make the row unreadable rather than merely
/// different.
#[test]
fn the_empty_string_crosses_as_the_empty_string_and_not_as_null() {
    let (_dir, _db, lite) = crossed("empty");
    for col in ["approval_categories", "screening_notes"] {
        assert_eq!(
            sqlite_text(
                &lite,
                &format!("SELECT {col} FROM avl_vendors WHERE id = 'avl_06_empty'")
            ),
            Some(String::new()),
            "{col} must be '' — not NULL, and not absent"
        );
        assert_eq!(
            sqlite_i64(
                &lite,
                &format!(
                    "SELECT count(*) FROM avl_vendors WHERE id = 'avl_06_empty' AND {col} IS NULL"
                )
            ),
            0
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Storage class
// ---------------------------------------------------------------------------

/// **Every one of the twelve columns crosses as `'text'`, and the table declares
/// no numeric column at all.**
///
/// The second half is the one worth having. This family is exempt from R1, R2,
/// R3, `finite_measurement` and the §3.4 fold rules *because* it has no number;
/// the moment a vendor price, score or numeric rating is added, every one of
/// those starts applying and nothing else in the tree would say so. A `'text'`
/// count on a column that should have been a number is the same silent
/// mis-typing in reverse: it orders and compares lexicographically and coerces to
/// `REAL` in any later SQL arithmetic.
#[test]
fn every_column_crosses_as_text_and_the_table_holds_no_number() {
    let (_dir, _db, lite) = crossed("typeof");

    for col in AVL_COLUMNS {
        let bad = sqlite_i64(
            &lite,
            &format!(
                "SELECT count(*) FROM {TABLE} WHERE {col} IS NOT NULL AND typeof({col}) <> 'text'"
            ),
        );
        assert_eq!(bad, 0, "{col} has {bad} non-text row(s)");
    }

    // And the declared schema agrees: no numeric affinity anywhere.
    let ddl = sqlite_text(
        &lite,
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='avl_vendors'",
    )
    .unwrap();
    assert!(ddl.contains("STRICT"), "{ddl}");
    for banned in [
        "INT", "REAL", "DOUBLE", "FLOAT", "DECIMAL", "NUMERIC", "BLOB",
    ] {
        assert!(
            !ddl.to_uppercase().contains(banned),
            "avl_vendors must declare no {banned} column — the family's exemption from R1/R2/R3 \
             and the §3.4 fold rules rests on it having no number at all:\n{ddl}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. The business invariant the table exists to carry
// ---------------------------------------------------------------------------

/// **The PO gate reaches the same verdict after the crossing as before it, on all
/// five status arms.**
///
/// An intent pin rather than a storage one (rule 9): `approved_status` is a
/// `TEXT` column either way, so no `typeof` check and no row count can tell
/// `"suspended"` from `"Suspended"` or from `"suspende"`. What the product
/// actually does with the column is `ApprovedStatus::from_storage_str(…)
/// .blocks_po()` (`avl_vendors.rs:546`) — and a value that stopped parsing would
/// make `po_eligibility` return `Err` on a vendor it should have blocked, which
/// is the failure mode this table exists to prevent.
///
/// Both verdicts are asserted, not just the blocking one: a crossing that
/// mangled every status into an unparseable string would fail loudly, but one
/// that mapped everything to `pending` would silently **unblock** a revoked
/// vendor, and only the `blocks_po` assertion below catches it.
#[test]
fn the_po_gate_reaches_the_same_verdict_after_the_crossing() {
    let (_dir, _db, lite) = crossed("po-gate");

    let mut blocked = 0;
    let mut allowed = 0;
    for c in CASES {
        let stored = sqlite_text(
            &lite,
            &format!(
                "SELECT approved_status FROM avl_vendors WHERE id = '{}'",
                c.0
            ),
        )
        .unwrap();
        // It still parses — through the product's own case-sensitive parser.
        let after = ApprovedStatus::from_storage_str(&stored)
            .unwrap_or_else(|e| panic!("{} stored {stored:?} no longer parses: {e}", c.0));
        // And it is the same status, with the same PO verdict, as the source's.
        let before = ApprovedStatus::from_storage_str(c.2).unwrap();
        assert_eq!(after, before, "{} changed status across the crossing", c.0);
        assert_eq!(after.blocks_po(), before.blocks_po(), "{}", c.0);
        if after.blocks_po() {
            blocked += 1;
        } else {
            allowed += 1;
        }
    }
    // The fixture actually exercises both verdicts — otherwise the assertions
    // above could all pass over a set that only ever answers one way.
    assert!(blocked >= 2, "the fixture must contain blocking vendors");
    assert!(allowed >= 2, "the fixture must contain eligible vendors");

    // Every category token still parses too — the multi-select set is stored
    // comma-joined, so a crossing that re-split or re-joined it would produce
    // tokens the vocabulary rejects.
    let cats = sqlite_text(
        &lite,
        "SELECT approval_categories FROM avl_vendors WHERE id = 'avl_02'",
    )
    .unwrap();
    assert_eq!(cats, "general,itar,aerospace");
    for tok in cats.split(',') {
        ApprovalCategory::from_storage_str(tok).unwrap_or_else(|e| panic!("{tok:?}: {e}"));
    }
    // And the empty set stays the empty set rather than becoming a one-element
    // set containing "".
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT approval_categories FROM avl_vendors WHERE id = 'avl_06_empty'"
        ),
        Some(String::new())
    );
}

/// **A partner's several AVL entries all cross, and the newest still wins.**
///
/// `(tenant_id, partner_id)` is deliberately NOT unique: `get_vendor_by_partner`
/// (`avl_vendors.rs:347`) — the lookup `purchasing.rs:605`'s `resolve_avl` makes
/// — reads `ORDER BY created_at DESC` and takes the first row. So this family
/// legitimately produces a duplicate that must be **carried**, not refused, and
/// the ordering that resolves it must survive.
///
/// The pin matters because the two entries here disagree about the PO verdict:
/// the older one is `revoked` (blocks) and the newer `approved` (does not). A
/// crossing that dropped either row, or that perturbed `created_at`, would flip
/// a live vendor's eligibility in one direction or the other.
#[test]
fn a_partner_with_several_entries_keeps_its_newest_wins_order() {
    let (_dir, _db, lite) = crossed("dup-partner");

    assert_eq!(
        sqlite_i64(
            &lite,
            &format!("SELECT count(*) FROM avl_vendors WHERE partner_id = '{DUP_PARTNER}'")
        ),
        2,
        "both of the partner's entries must be carried — this duplicate is legitimate"
    );

    let winner = sqlite_text(
        &lite,
        &format!(
            "SELECT id FROM avl_vendors WHERE partner_id = '{DUP_PARTNER}' \
             ORDER BY created_at DESC LIMIT 1"
        ),
    )
    .unwrap();
    assert_eq!(
        winner, DUP_WINNER,
        "the newest entry must still win the PO gate's lookup"
    );
    assert!(
        !ApprovedStatus::from_storage_str(
            &sqlite_text(
                &lite,
                &format!("SELECT approved_status FROM avl_vendors WHERE id = '{DUP_WINNER}'")
            )
            .unwrap()
        )
        .unwrap()
        .blocks_po(),
        "the winning entry is `approved`; if the loser won, the vendor would be blocked"
    );
}

// ---------------------------------------------------------------------------
// 4. Identity
// ---------------------------------------------------------------------------

/// **Every natural key is either accepted and round-trips byte-identically, or
/// refused loudly naming the table and the key. Both arms are required to
/// fire.**
///
/// This family has no R2 column and no float, so the disjunction is not about a
/// *value* representation — it is about *identity*.
/// [`unique_natural_keys`](aberp::migrate_dispatch::unique_natural_keys) is the
/// only refusal in the family.
///
/// The adversarial table below is measured, not asserted from the docs:
///
/// | input | arm | why |
/// |---|---|---|
/// | distinct ids in one tenant | accept | the ordinary case |
/// | the same id in two tenants | accept | the key is the composite |
/// | an adjacent duplicate | **refuse** | `ORDER BY` puts duplicates next to each other |
/// | a non-adjacent duplicate | **refuse** | a `BTreeSet`, not a peek at the previous row |
/// | ids differing only in the last character | accept | no prefix folding |
/// | ids differing only in case | accept | the key is bytes, not an ASCII fold |
/// | `""` vs a real key | accept | the empty key is a key, not a wildcard |
/// | `""` twice | **refuse** | and the empty key is not exempt from uniqueness |
///
/// ⚠ The case row is the one to keep: M11's finding is that SQLite's `LOWER()`
/// is ASCII-only, and a key comparison that had folded case anywhere would make
/// `AVL_1` and `avl_1` collide. It does not, because the key is compared as
/// bytes.
#[test]
fn every_carried_natural_key_either_round_trips_or_is_refused() {
    let k = |t: &str, i: &str| format!("{t}#{i}");

    // --- accept ---
    for keys in [
        vec![k("test", "avl_1"), k("test", "avl_2")],
        vec![k("test", "avl_1"), k("other", "avl_1")],
        vec![k("test", "avl_1"), k("test", "avl_2")],
        vec![k("test", "AVL_1"), k("test", "avl_1")],
        vec![k("test", ""), k("test", "avl_1")],
    ] {
        assert!(
            unique_natural_keys(&keys, TABLE).is_ok(),
            "must accept {keys:?}"
        );
    }

    // --- refuse ---
    for keys in [
        vec![k("test", "avl_1"), k("test", "avl_1")],
        vec![k("test", "avl_1"), k("test", "avl_9"), k("test", "avl_1")],
        vec![k("test", ""), k("test", "")],
    ] {
        let err = unique_natural_keys(&keys, TABLE)
            .expect_err("must refuse a duplicate composite")
            .to_string();
        assert!(
            err.contains(TABLE),
            "the refusal must name the table: {err}"
        );
        assert!(
            err.contains("two rows with the natural key"),
            "the refusal must name the key: {err}"
        );
    }
}

/// **A duplicate natural key fails the whole carry.**
///
/// ⚠ **Reaching this from a real DuckDB file takes one extra step, and the step
/// is the finding.** Like `outbound_email_queue` (Part H) and unlike the
/// dispatch and purchasing tables, `avl_vendors` **does** carry a `PRIMARY KEY`
/// — but on the **bare `id`**, not on the `tenant_id#id` composite the gate keys
/// on. So the fixture builds the shape the `PRIMARY KEY` does not cover: a table
/// hand-recreated without it, which is what a repair, a restore or an older
/// schema produces.
///
/// The refusal must be the **Rust** one: SQLite's own `PRIMARY KEY` would also
/// reject the second `INSERT`, but with a constraint error naming neither the
/// source nor the key. `unique_natural_keys` runs on the DuckDB read side,
/// before anything is bound, so its message names the source's defect — and the
/// assertions below are what tell the two apart.
#[test]
fn a_duplicate_natural_key_fails_the_migration() {
    let dir = scratch("dup-key");
    let db = dir.join("aberp.duckdb");
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        // The repair-shaped schema: same columns, NO PRIMARY KEY.
        conn.execute_batch(
            "CREATE TABLE avl_vendors (
                 id                  VARCHAR NOT NULL,
                 tenant_id           VARCHAR NOT NULL,
                 partner_id          VARCHAR NOT NULL,
                 approved_status     VARCHAR NOT NULL,
                 approval_categories VARCHAR NOT NULL,
                 approved_until_utc  VARCHAR,
                 screening_notes     VARCHAR NOT NULL,
                 reviewer_login      VARCHAR NOT NULL,
                 reviewed_at_utc     VARCHAR,
                 revoked_reason      VARCHAR,
                 created_at          VARCHAR NOT NULL,
                 updated_at          VARCHAR NOT NULL
             );",
        )
        .unwrap();
        for _ in 0..2 {
            insert_row(
                &conn,
                "avl_dup",
                TENANT,
                "prt_a",
                "approved",
                "general",
                None,
                "n",
                "operator",
                None,
                None,
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            );
        }
        conn.close().unwrap();
    }
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a duplicate natural key must fail the carry")
        .to_string();

    assert!(err.contains(TABLE), "{err}");
    assert!(err.contains("test#avl_dup"), "must name the key: {err}");
    // The Rust refusal, not SQLite's constraint error.
    assert!(
        err.contains("Refusing to carry"),
        "the refusal must be the Rust one, which names the SOURCE's defect rather than the copy's \
         symptom: {err}"
    );
    assert!(
        !err.to_uppercase().contains("UNIQUE CONSTRAINT"),
        "a SQLite constraint error here means the guard ran too late: {err}"
    );
}

// ---------------------------------------------------------------------------
// 5. The sweep through real storage
// ---------------------------------------------------------------------------

/// The same round-trip proved through **real storage**: 256 generated rows
/// seeded into a real DuckDB file, carried by the real migrator into a real
/// SQLite file, and read back — every one of the twelve columns byte-for-byte.
///
/// This is the pin the unit tests cannot be: it exercises DuckDB's `VARCHAR`
/// reads, the `ToSql` binds, `STRICT`'s acceptance and SQLite's read-back — four
/// layers where a value could be normalised, truncated or re-encoded without any
/// in-memory check noticing. Compared as **bytes**, because a layer that
/// re-encoded or replaced a lone `\r` would produce two `String`s that print the
/// same and differ.
#[test]
fn every_swept_row_survives_real_storage_byte_for_byte() {
    let (_dir, db, lite) = crossed("sweep");

    let want: Vec<(String, Vec<Option<String>>)> = {
        let conn = duckdb::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, partner_id, approved_status, approval_categories, approved_until_utc, \
                 screening_notes, reviewer_login, reviewed_at_utc, revoked_reason, created_at, \
                 updated_at FROM avl_vendors WHERE id LIKE 'avl_sweep_%' ORDER BY id ASC",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    (1..11)
                        .map(|i| r.get::<_, Option<String>>(i))
                        .collect::<duckdb::Result<Vec<_>>>()?,
                ))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(want.len(), SWEPT_ROWS, "the sweep must actually have run");

    for (id, cols) in &want {
        let got = sqlite_row(&lite, id);
        assert_eq!(got.len(), cols.len());
        for (i, (a, b)) in cols.iter().zip(got.iter()).enumerate() {
            assert_eq!(
                a.as_ref().map(|s| s.as_bytes()),
                b.as_ref().map(|s| s.as_bytes()),
                "row {id}, column {} drifted",
                AVL_COLUMNS[i + 2]
            );
        }
    }

    // The sweep is only worth its runtime if it actually produced both arms of
    // every nullable column and a non-empty share of empty strings.
    for col in ["approved_until_utc", "reviewed_at_utc", "revoked_reason"] {
        for pred in ["IS NULL", "IS NOT NULL"] {
            assert!(
                sqlite_i64(
                    &lite,
                    &format!(
                        "SELECT count(*) FROM avl_vendors WHERE id LIKE 'avl_sweep_%' AND {col} \
                         {pred}"
                    )
                ) > 0,
                "the sweep must exercise {col} {pred}"
            );
        }
    }
    assert!(
        sqlite_i64(
            &lite,
            "SELECT count(*) FROM avl_vendors WHERE id LIKE 'avl_sweep_%' AND screening_notes = ''"
        ) > 0,
        "the sweep must exercise the empty string"
    );
}

// ---------------------------------------------------------------------------
// 6. The schema builder
// ---------------------------------------------------------------------------

/// `ensure_avl_schema` builds the table, is idempotent, declares every column
/// with the §3.2 vocabulary, keeps the `PRIMARY KEY` on the bare `id`, and
/// creates no index.
#[test]
fn ensure_avl_schema_builds_the_table_and_is_idempotent() {
    let dir = scratch("ensure");
    let lite = dir.join("s.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_avl::ensure_avl_schema(&conn).unwrap();
    // Idempotent — and a value written between the two calls SURVIVES the
    // second, which is what proves no column carries a SQL DEFAULT that a
    // replayed `ensure` would clobber.
    conn.execute(
        "INSERT INTO avl_vendors (id, tenant_id, partner_id, approved_status, \
         approval_categories, approved_until_utc, screening_notes, reviewer_login, \
         reviewed_at_utc, revoked_reason, created_at, updated_at) \
         VALUES ('avl_x', 'test', 'p', 'approved', 'general', NULL, 'n', 'op', NULL, NULL, \
         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    aberp::migrate_avl::ensure_avl_schema(&conn).unwrap();
    let status: String = conn
        .query_row(
            "SELECT approved_status FROM avl_vendors WHERE id = 'avl_x'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "approved", "a replayed ensure must not clobber");

    let ddl: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='avl_vendors'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ddl.contains("STRICT"), "{ddl}");
    assert!(!ddl.contains("VARCHAR"), "{ddl}");
    for col in AVL_COLUMNS {
        assert!(ddl.contains(&format!("{col} ")), "{col} missing from {ddl}");
    }
    // The PRIMARY KEY is on `id` and nothing else, and there is no index.
    assert!(
        ddl.lines()
            .filter(|l| l.split_whitespace().next() == Some("id"))
            .any(|l| l.contains("PRIMARY KEY")),
        "{ddl}"
    );
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND tbl_name='avl_vendors' \
             AND sql IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        indexes, 0,
        "avl_vendors.rs:239-240 declares no index — the table is small master data scanned in full"
    );
}

// ---------------------------------------------------------------------------
// 7. The gate's teeth
// ---------------------------------------------------------------------------

/// The gate **hard-stops** when the table was not carried.
///
/// Mutation-shaped: this is what the gate does if a future edit drops the carry
/// from `migrate_families`.
#[test]
fn the_gate_hard_stops_when_the_table_was_not_carried() {
    let (_dir, db, lite) = crossed("not-carried");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("DROP TABLE avl_vendors;").unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains(TABLE) && s.contains("silent-skip")),
        "a dropped carry must hard-stop, not silently pass: {:#?}",
        r.hard_stops
    );
}

/// **A single changed column on a single row reds the gate — for every mutable
/// column, one column at a time.** A gate that has never been shown to fail is
/// not a gate (ADR-0107 §4.1).
///
/// `tenant_id` and `id` are excluded because they *are* the key; mutating either
/// produces a missing-row hard stop instead, which the last block pins
/// separately.
///
/// The mutation is **one character**, not a wholesale replacement — the smallest
/// drift this family can suffer, and the one a tolerance-shaped or
/// prefix-shaped comparison would wave through. On a family with no numeric
/// column and therefore no Σ fold, the per-row arm is the *only* thing standing
/// between a drifted `approved_status` and a PO raised against a revoked vendor.
#[test]
fn a_single_changed_column_reds_the_gate() {
    for col in &AVL_COLUMNS[2..] {
        let (_dir, db, lite) = crossed(&format!("mutate-{col}"));
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            // Append one character to a NOT NULL column, or fill a NULL one.
            conn.execute(
                &format!(
                    "UPDATE avl_vendors SET {col} = COALESCE({col}, '') || 'X' WHERE id = 'avl_02'"
                ),
                [],
            )
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains("avl_02") && s.contains(*col)),
            "a one-character drift in {col} must red the gate: {:#?}",
            r.hard_stops
        );
    }

    // And a mutated KEY produces the missing-row stop rather than a column one.
    for key_col in ["id", "tenant_id"] {
        let (_dir, db, lite) = crossed(&format!("mutate-key-{key_col}"));
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute(
                &format!("UPDATE avl_vendors SET {key_col} = {key_col} || 'X' WHERE id = 'avl_02'"),
                [],
            )
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains("is missing on the SQLite side")),
            "a mutated {key_col} must produce a missing-row hard stop: {:#?}",
            r.hard_stops
        );
    }
}

/// A deleted row reds the gate on **both** the count and the per-row arm.
///
/// Split from the mutation test because a carry that lost rows and a carry that
/// corrupted them are different defects, and a gate that only counted would miss
/// the second while a gate that only compared present rows would miss the first.
#[test]
fn a_dropped_row_reds_both_the_count_and_the_per_row_arm() {
    let (_dir, db, lite) = crossed("dropped-row");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute("DELETE FROM avl_vendors WHERE id = 'avl_05'", [])
            .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("row count") && s.contains(TABLE)),
        "{:#?}",
        r.hard_stops
    );
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("avl_05") && s.contains("is missing on the SQLite side")),
        "{:#?}",
        r.hard_stops
    );
}

/// A source with no AVL table is a legitimate shape — the table is created
/// lazily on first use — and the gate says so out loud rather than staying
/// silent.
#[test]
fn a_source_without_the_family_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.avl.avl_vendors, 0);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(r.hard_stops.is_empty(), "{:#?}", r.hard_stops);
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("AVL family absent on BOTH sides")),
        "the gate must SAY the family was absent — a silent skip is indistinguishable from a \
         passed check: {:#?}",
        r.checks
    );
}
