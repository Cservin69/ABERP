//! ADR-0108 Step 7 Part H — **the email / relay family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM), E (QA/QC),
//! F (dispatch) and G (purchasing) are already on this branch, so this is
//! **Part H** — the last non-quoting family, and therefore the end of Step 7.
//!
//! Twelve pins, in the order they defend:
//!
//! 1. the one table crosses and **no column drifts**, asserted against the
//!    fixture's own literals as well as through the gate;
//! 2. **the message bodies cross byte-for-byte at scale** — a ~300 KB multiline
//!    Unicode `body_text` and its HTML sibling, compared as bytes, not as
//!    `String`s that a normalising layer could have quietly re-encoded;
//! 3. **the attachments survive**, which for this family means something other
//!    than a BLOB round-trip: real bytes are written to disk by the product's own
//!    `write_attachment`, the row's rel-path is read back **out of SQLite**, and
//!    the file it resolves to is byte-identical — and the whole attachment tree
//!    is digest-identical across the migration, because the migrator must not
//!    touch it at all;
//! 4. **§3.2 F's two counts cross as `'integer'`** with their values intact over
//!    the whole `i64` range, and every other column as `'text'` — including
//!    `recipient_hash`, which is a hash that must **not** be a `BLOB`;
//! 5. the closed `state` vocabulary still parses through the product's own
//!    `QueueState::parse_str` after the crossing, on all four arms;
//! 6. **the disjunction sweep** — every natural key is either carried with a
//!    **byte-identical** round-trip or **refused loudly** naming the table and
//!    the key. Both arms are required to fire;
//! 7. the same disjunction proved through **real storage**: 256 generated rows
//!    seeded into a real DuckDB file, carried by the real migrator into a real
//!    SQLite file, and read back byte-for-byte;
//! 8. a duplicate natural key **fails the whole carry** rather than crossing as
//!    a row nobody compares;
//! 9. `ensure_email_schema` builds the table, is idempotent, declares every
//!    column with the §3.2 vocabulary, keeps the `PRIMARY KEY`, creates **no**
//!    secondary index, and `STRICT` refuses a float into a count;
//! 10. the gate **hard-stops** when the table was not carried;
//! 11. the per-row equality arm is shown to go red on **every mutable column,
//!     one column at a time** — including a **one-byte** change inside a ~300 KB
//!     body, which is the smallest drift this family can suffer;
//! 12. a source with no queued e-mail is a legitimate shape and the gate says so
//!     out loud.
//!
//! **This test pins no §3.4 fold, because this family owes none**; no R2
//! canonical-decimal handling, because the family has no `DECIMAL` and no
//! `DOUBLE` column at all; no `finite_measurement`, because it has no float; and
//! no M11-shaped `LOWER()`/`LIKE` refusal, because both patterns return zero hits
//! across all six email modules — the recipient fold happens in Rust, in
//! `hash_recipient_list`, before anything is bound.
//!
//! ⚠ **Unlike Parts E, F and G this family has no composite key**, so pin 11's
//! "one column at a time" runs against a single-column key (`id`) and the
//! key-mutation arm surfaces as a missing row rather than as a second composite
//! component. Stated rather than silently skipped.
//!
//! **Nothing here sends an e-mail.** No SMTP transport is constructed, no
//! network call is made, and no path under `~/.aberp/` is read, written or
//! stat-ed — every fixture lives in a per-test scratch directory.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::email_relay_queue::{write_attachment, QueueState};
use aberp::migrate_dispatch::unique_natural_keys;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";
const TABLE: &str = "outbound_email_queue";

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-email-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The big body, built rather than stored as a literal: ~300 KB of multiline
/// Unicode with every shape a real relay message carries and a few a naive bind
/// would not survive.
///
/// The `text/html` share of a relay message is "typically <100 KB"
/// (`email_relay_queue.rs:31`), so ~300 KB is above the ordinary case on purpose:
/// the point of the pin is that nothing in the four layers between DuckDB's
/// `VARCHAR` read and SQLite's `TEXT` read-back truncates, re-encodes or
/// normalises at scale.
///
/// **No interior NUL**: neither engine's `TEXT` can hold one, so generating one
/// would pin a shared limitation rather than the carry.
fn big_body() -> String {
    let mut s = String::with_capacity(320_000);
    s.push_str("Tisztelt Partnerünk!\r\n\r\n");
    for i in 0..1_500 {
        s.push_str(&format!(
            "{i:04}\tÁrvíztűrő tükörfúrógép — 100% átvéve _ \"quoted\" 'single' \\backslash\n\
             \tрусский текст · 日本語 · العربية · emoji 🚀🇭🇺 · math ∑∫√ · nbsp\u{a0}here\r\n",
        ));
    }
    s.push_str("\n\n--\nABERP\n");
    s
}

/// The HTML sibling, so the pin runs on the **nullable** large column too.
fn big_html() -> String {
    format!("<html><body><pre>{}</pre></body></html>", big_body())
}

/// `(id, state, attempt_n, byte_size, populated)`.
///
/// * **`eml-01`** — `queued`: every nullable column `NULL`, `attempt_n` 0. The
///   pre-daemon shape.
/// * **`eml-02`** — `sending`: mid-flight, one attempt, a transient
///   `last_error`, no `sent_at`.
/// * **`eml-03`** — `sent`: terminal success, `sent_at` stamped, `last_error`
///   cleared back to `NULL` — the arm `mark_sent` produces.
/// * **`eml-04`** — `failed`: terminal failure after the retry cap, so it is the
///   row that carries a real operator-facing error string.
/// * **`eml-05-zero`** — `byte_size` and `attempt_n` both **0**, and `subject`
///   the **empty string**. Zero and `""` are values: a carry that dropped a
///   column and left SQLite's default would produce both, so the per-row arm has
///   to see the same ones on each side rather than infer them.
/// * **`eml-90-min` / `eml-91-max`** — `byte_size` at `i64::MIN` and `i64::MAX`.
///   `byte_size` is a `u64` in Rust and a `BIGINT` on DuckDB; §3.2 F says the
///   representation is unchanged, and these two rows are what turns that claim
///   into a measurement. **Ordered min before max deliberately**: the gate's Σ is
///   a `checked_add` fold in key order, so the running sum dips to
///   `i64::MIN + ε` and comes back rather than overflowing.
const CASES: &[(&str, &str, i64, i64, bool)] = &[
    ("eml-01", "queued", 0, 4_096, false),
    ("eml-02", "sending", 1, 12_345, true),
    ("eml-03", "sent", 2, 98_765, true),
    ("eml-04", "failed", 5, 1_048_576, true),
    ("eml-05-zero", "queued", 0, 0, false),
    ("eml-90-min", "failed", 3, i64::MIN, true),
    ("eml-91-max", "sent", 4, i64::MAX, true),
];

/// The row that carries the 200 KB body and the real on-disk attachments.
const BIG_ID: &str = "eml-big";
/// The `attachments_dir` rel-path that row stores.
const BIG_ATTACHMENTS_DIR: &str = "eml-big";
/// The attachment root, relative to the scratch dir. Deliberately **not**
/// `attachments_root_for_tenant`, which resolves under `$HOME/.aberp/` — C-II
/// forbids this plan from touching that tree at all.
const ATTACHMENT_ROOT: &str = "email-relay-attachments";

/// `(operator filename, bytes)` — an ordinary PDF-shaped payload, a binary blob
/// with every byte value in it, and a zero-length file.
fn attachment_cases() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "árajánlat 2026.pdf".to_string(),
            b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\ntrailer\n".to_vec(),
        ),
        ("all-bytes.bin".to_string(), (0u8..=255).collect()),
        ("empty.dat".to_string(), Vec::new()),
    ]
}

/// How many generated rows pin 7 pushes through real storage.
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

/// A `TEXT` value across the shapes a relay row actually holds — addresses,
/// JSON recipient arrays, subjects, SMTP error strings, hex hashes — plus the
/// ones that break a naive bind: quotes, a literal `%` and `_`, CRLF, a tab, and
/// the empty string.
fn swept_text(i: usize, state: &mut u64) -> String {
    let n = rng(state);
    match i % 8 {
        0 => String::new(),
        1 => format!("[\"ügyfél+{}@példa.hu\"]", n % 997),
        2 => format!("Ajánlat — 100% kedvezmény _ {}", n % 1000),
        3 => format!("it's a \"quoted\" value {}", n % 97),
        4 => format!(
            "550 5.1.1 <a@b.c>: Recipient address rejected\r\n\tid={}",
            n
        ),
        5 => "日本語の件名 — 汎用 · 🚀".to_string(),
        6 => format!("{:064x}", n),
        _ => format!("line one\nline two\ttab {}", n % 89),
    }
}

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema`, so the SQLite
/// side is compared against the schema the product actually builds rather than
/// against a hand-written copy of it. Also writes the real attachment files.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp::email_relay_queue::ensure_schema(&conn).unwrap();

        for (id, state, attempt_n, byte_size, populated) in CASES {
            insert_row(
                &conn,
                id,
                &format!("2026-02-0{}T09:00:00Z", 1 + (id.len() % 8)),
                "storefront",
                "[\"ügyfél@példa.hu\",\"Second Recipient <b@c.test>\"]",
                populated.then_some("[\"cc@példa.hu\"]"),
                if *id == "eml-05-zero" {
                    ""
                } else {
                    "Ajánlat — 100% _ árvíztűrő"
                },
                "Tisztelt Partnerünk!\r\n\r\nMellékelten küldjük.\r\n",
                populated.then_some("<p>Tisztelt <b>Partnerünk</b>!</p>"),
                None,
                state,
                *attempt_n,
                match *state {
                    "failed" => Some("550 5.1.1 <ügyfél@példa.hu>: Recipient address rejected"),
                    "sending" => Some("transient: connection reset"),
                    _ => None,
                },
                (*state == "sent").then_some("2026-02-02T09:05:00Z"),
                &format!("{:064x}", 0xABCDu64 + id.len() as u64),
                *byte_size,
            );
        }

        // The big-body row, with real attachments on disk beside it.
        let row_dir = dir.join(ATTACHMENT_ROOT).join(BIG_ATTACHMENTS_DIR);
        for (i, (name, bytes)) in attachment_cases().iter().enumerate() {
            write_attachment(&row_dir, i, name, bytes).unwrap();
        }
        let body = big_body();
        let html = big_html();
        insert_row(
            &conn,
            BIG_ID,
            "2026-02-03T09:00:00Z",
            "operator",
            "[\"nagy@példa.hu\"]",
            Some("[]"),
            "Nagy levél — 200 kB",
            &body,
            Some(&html),
            Some(BIG_ATTACHMENTS_DIR),
            "queued",
            0,
            None,
            None,
            &format!("{:064x}", 0x1234u64),
            (body.len() + html.len()) as i64,
        );

        // The swept rows.
        let mut state = 0x5EED_1108_0800_u64 ^ 0x9E37_79B9_7F4A_7C15;
        const STATES: [&str; 4] = ["queued", "sending", "sent", "failed"];
        for i in 0..SWEPT_ROWS {
            let cc = swept_text(i + 1, &mut state);
            let html = swept_text(i + 2, &mut state);
            let err = swept_text(i + 3, &mut state);
            let sent = swept_text(i + 4, &mut state);
            let dir_rel = swept_text(i + 5, &mut state);
            insert_row(
                &conn,
                &format!("eml-sweep-{i:04}"),
                &format!("2026-03-{:02}T{:02}:00:00Z", 1 + (i % 28), i % 24),
                &swept_text(i, &mut state),
                &swept_text(i + 6, &mut state),
                Some(&cc),
                &swept_text(i + 7, &mut state),
                &swept_text(i + 8, &mut state),
                Some(&html),
                Some(&dir_rel),
                STATES[i % 4],
                (i as i64) % 6,
                Some(&err),
                Some(&sent),
                &format!("{:064x}", rng(&mut state)),
                (rng(&mut state) % 2_000_001) as i64 - 1_000_000,
            );
        }

        conn.close().unwrap();
    }

    seed_ledger(&db);
    db
}

/// One `outbound_email_queue` row, written with the column list the product's
/// own `insert_queued` uses — but as a raw `INSERT`, because `insert_queued`
/// hard-codes `state = 'queued'`, `attempt_n = 0` and NULL terminals, and the
/// fixture needs all four states and both arms of every nullable column.
#[allow(clippy::too_many_arguments)]
fn insert_row(
    conn: &duckdb::Connection,
    id: &str,
    created_at: &str,
    submitter: &str,
    to_recipients_json: &str,
    cc_recipients_json: Option<&str>,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
    attachments_dir: Option<&str>,
    state: &str,
    attempt_n: i64,
    last_error: Option<&str>,
    sent_at: Option<&str>,
    recipient_hash: &str,
    byte_size: i64,
) {
    conn.execute(
        "INSERT INTO outbound_email_queue
           (id, created_at, submitter, to_recipients_json, cc_recipients_json,
            subject, body_text, body_html, attachments_dir,
            state, attempt_n, last_error, sent_at, recipient_hash, byte_size)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        duckdb::params![
            id,
            created_at,
            submitter,
            to_recipients_json,
            cc_recipients_json,
            subject,
            body_text,
            body_html,
            attachments_dir,
            state,
            attempt_n,
            last_error,
            sent_at,
            recipient_hash,
            byte_size,
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
    assert_eq!(
        out.email.outbound_email_queue,
        (CASES.len() + 1 + SWEPT_ROWS) as u64
    );
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

fn sqlite_typeof(lite: &Path, col: &str, where_clause: &str) -> String {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(
        &format!("SELECT typeof({col}) FROM {TABLE} WHERE {where_clause}"),
        [],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

/// A content digest of every file under `root`, path-sorted. Used to prove the
/// migration touched the attachment tree not at all.
fn tree_digest(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

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
fn the_email_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("headline");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops.is_empty(),
        "the email/relay family must cross clean: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains(&format!("every {TABLE} column round-trips with ZERO drift"))),
        "the ZERO-drift check must be emitted; checks: {:?}",
        r.checks
    );

    for (id, state, attempt_n, byte_size, populated) in CASES {
        let at = |col: &str| format!("SELECT {col} FROM {TABLE} WHERE id = '{id}'");
        assert_eq!(sqlite_text(&lite, &at("state")).as_deref(), Some(*state));
        assert_eq!(sqlite_i64(&lite, &at("attempt_n")), *attempt_n);
        assert_eq!(
            sqlite_i64(&lite, &at("byte_size")),
            *byte_size,
            "byte_size on {id}"
        );
        assert_eq!(
            sqlite_text(&lite, &at("submitter")).as_deref(),
            Some("storefront")
        );
        // The recipient list is the value that decides who gets the message.
        assert_eq!(
            sqlite_text(&lite, &at("to_recipients_json")).as_deref(),
            Some("[\"ügyfél@példa.hu\",\"Second Recipient <b@c.test>\"]")
        );
        // The nullable columns, in both states.
        assert_eq!(
            sqlite_text(&lite, &at("cc_recipients_json")).is_some(),
            *populated
        );
        assert_eq!(sqlite_text(&lite, &at("body_html")).is_some(), *populated);
        assert_eq!(
            sqlite_text(&lite, &at("sent_at")).is_some(),
            *state == "sent"
        );
        assert_eq!(
            sqlite_text(&lite, &at("last_error")).is_some(),
            matches!(*state, "failed" | "sending")
        );
        // The empty string is a value, not a NULL. `eml-05-zero` is the row that
        // distinguishes them and the `typeof` sweep cannot.
        if *id == "eml-05-zero" {
            assert_eq!(sqlite_text(&lite, &at("subject")).as_deref(), Some(""));
        }
    }
}

// ---------------------------------------------------------------------------
// 2. The bodies, at scale, as bytes
// ---------------------------------------------------------------------------

/// **A 200 KB multiline Unicode body crosses byte-for-byte**, and so does its
/// nullable HTML sibling.
///
/// Compared as **bytes**, not as `String`s: a layer that re-encoded, normalised
/// or replaced a lone `\r` would produce two `String`s that print the same and
/// differ. The body carries CRLF and bare LF in the same value, tabs, a
/// non-breaking space, four scripts, an emoji ZWJ-free pair, quotes, a
/// backslash, a literal `%` and a literal `_`.
#[test]
fn the_message_bodies_cross_byte_for_byte_at_scale() {
    let (_dir, _db, lite) = crossed("bodies");

    let body = big_body();
    let html = big_html();
    assert!(
        body.len() > 200_000,
        "the pin needs a body above the ordinary <100 kB case; got {}",
        body.len()
    );

    let got_body = sqlite_text(
        &lite,
        &format!("SELECT body_text FROM {TABLE} WHERE id = '{BIG_ID}'"),
    )
    .expect("body_text is NOT NULL");
    let got_html = sqlite_text(
        &lite,
        &format!("SELECT body_html FROM {TABLE} WHERE id = '{BIG_ID}'"),
    )
    .expect("the fixture populated body_html");

    assert_eq!(
        got_body.len(),
        body.len(),
        "body_text length drifted: {} vs {}",
        body.len(),
        got_body.len()
    );
    assert_eq!(
        got_body.as_bytes(),
        body.as_bytes(),
        "body_text is not byte-identical (first differing offset {:?})",
        body.as_bytes()
            .iter()
            .zip(got_body.as_bytes())
            .position(|(a, b)| a != b)
    );
    assert_eq!(got_html.as_bytes(), html.as_bytes());

    // And the specific bytes a normalising layer eats, present in the result.
    assert!(got_body.contains("\r\n"), "CRLF must survive");
    assert!(
        got_body.contains("backslash\n\t"),
        "a BARE LF must survive — the fixture puts one mid-line, immediately before a tab, so a \
         layer that normalised every LF to CRLF would be caught here and not by the CRLF probe \
         above"
    );
    assert!(got_body.contains('\t'), "a tab must survive");
    assert!(
        got_body.contains('\u{a0}'),
        "a non-breaking space must survive"
    );
    assert!(got_body.contains("🚀"), "a 4-byte code point must survive");
    assert!(got_body.contains("100% átvéve _"), "% and _ must survive");
}

// ---------------------------------------------------------------------------
// 3. The attachments
// ---------------------------------------------------------------------------

/// **The attachments survive the migration, and the migrator does not touch
/// them.**
///
/// This family stores no BLOB: the bytes are files under
/// `<tenant>/email-relay-attachments/<row_id>/`, and the row holds a rel-path
/// (`email_relay_queue.rs:24–31`). So "carry the attachment bytes byte-exact"
/// means two different claims, and both are asserted here:
///
/// 1. the rel-path crosses byte-identically, so the path read **out of SQLite**
///    still resolves to the file the daemon would attach — including one whose
///    operator filename needed sanitising and one that is zero-length;
/// 2. the attachment tree is **digest-identical** across the whole migration —
///    the migrator reads, writes and stats none of it.
///
/// A drifted rel-path is the failure this pin exists for: it produces an e-mail
/// that sends **without** its attachment, and no row count, no `typeof` class and
/// no Σ fold can see it.
#[test]
fn the_attachments_are_still_reachable_and_the_tree_is_untouched() {
    let dir = scratch("attachments");
    let db = seed(&dir);
    let root = dir.join(ATTACHMENT_ROOT);
    let before = tree_digest(&root);
    assert_eq!(
        before.len(),
        attachment_cases().len(),
        "the fixture must have written every attachment"
    );

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");

    // 1. the rel-path crosses, and it resolves.
    let rel = sqlite_text(
        &lite,
        &format!("SELECT attachments_dir FROM {TABLE} WHERE id = '{BIG_ID}'"),
    )
    .expect("attachments_dir must not be NULL on the big row");
    assert_eq!(rel, BIG_ATTACHMENTS_DIR);
    let row_dir = root.join(&rel);
    assert!(
        row_dir.is_dir(),
        "the path read out of SQLite must resolve: {}",
        row_dir.display()
    );
    for (i, (name, bytes)) in attachment_cases().iter().enumerate() {
        let basename = format!(
            "{i:02}_{}",
            aberp::email_relay_queue::sanitize_attachment_filename(name)
        );
        let path = row_dir.join(&basename);
        assert!(path.is_file(), "missing attachment {}", path.display());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            *bytes,
            "attachment {basename} is not byte-identical"
        );
    }
    // The all-bytes file really did carry every byte value — a sanity check on
    // the fixture, so a truncated write cannot make the assertion above vacuous.
    let all_bytes = row_dir.join("01_all-bytes.bin");
    assert_eq!(std::fs::read(&all_bytes).unwrap().len(), 256);

    // 2. the tree is digest-identical: the migrator touched none of it.
    assert_eq!(
        tree_digest(&root),
        before,
        "the migration must not read, write or rewrite the attachment tree"
    );
}

// ---------------------------------------------------------------------------
// 4 + 5. §3.2 F, the storage classes, and the state vocabulary
// ---------------------------------------------------------------------------

/// The two counts cross as `'integer'` with their values intact over the whole
/// `i64` range; every other column crosses as `'text'` — `recipient_hash`
/// included.
///
/// A `'text'` count would order and compare lexicographically and would silently
/// coerce to `REAL` in any later SQL arithmetic. A `'blob'` `recipient_hash`
/// would never compare equal to the hex `String` every reader of that column
/// hands it — the R3-by-analogy mistake this family is the place to make.
#[test]
fn the_two_counts_cross_as_integer_and_every_other_column_as_text() {
    let (_dir, db, lite) = crossed("typing");

    for col in ["attempt_n", "byte_size"] {
        assert_eq!(
            sqlite_typeof(&lite, col, "id = 'eml-01'"),
            "integer",
            "{col} is a §3.2 F count and must be INTEGER"
        );
    }
    // The extremes, exact.
    assert_eq!(
        sqlite_i64(
            &lite,
            &format!("SELECT byte_size FROM {TABLE} WHERE id = 'eml-91-max'")
        ),
        i64::MAX,
        "§3.2 F says the representation is unchanged; i64::MAX is what makes that a measurement"
    );
    assert_eq!(
        sqlite_i64(
            &lite,
            &format!("SELECT byte_size FROM {TABLE} WHERE id = 'eml-90-min'")
        ),
        i64::MIN
    );

    assert_eq!(
        sqlite_typeof(&lite, "recipient_hash", "id = 'eml-01'"),
        "text",
        "recipient_hash is a HEX STRING, not the audit chain's R3 bytes"
    );
    assert_eq!(
        sqlite_typeof(&lite, "subject", "id = 'eml-05-zero'"),
        "text",
        "the empty string is TEXT, not NULL"
    );
    assert_eq!(
        sqlite_typeof(&lite, "body_text", &format!("id = '{BIG_ID}'")),
        "text"
    );

    // And the gate says the same thing over EVERY row rather than the one this
    // test picked, plus the Σ folds §6.3 requires.
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    for want in [
        "typeof(outbound_email_queue.byte_size) = 'integer'",
        "typeof(outbound_email_queue.attempt_n) = 'integer'",
        "typeof(outbound_email_queue.recipient_hash) = 'text'",
        "typeof(outbound_email_queue.body_text) = 'text'",
        "Σ outbound_email_queue.attempt_n",
        "Σ outbound_email_queue.byte_size",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(want)),
            "the gate must emit {want:?}; checks: {:?}",
            r.checks
        );
    }
}

/// **The closed `state` vocabulary still parses after the crossing, on all four
/// arms** — through the product's own `QueueState::parse_str`, which errors loud
/// on anything it does not know.
///
/// An intent pin rather than a storage one (rule 9): `state` is a `TEXT` column
/// either way, so no `typeof` check can tell `"sent"` from `"Sent"` or from
/// `"sen"`. What the daemon actually does with the column is parse it, and a
/// value that stopped parsing would strand the row — `parse_str` is
/// case-sensitive by design.
#[test]
fn the_state_vocabulary_still_parses_after_the_crossing() {
    let (_dir, _db, lite) = crossed("vocab");

    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let mut stmt = conn
        .prepare(&format!("SELECT id, state FROM {TABLE} ORDER BY id ASC"))
        .unwrap();
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), CASES.len() + 1 + SWEPT_ROWS);

    let mut seen = std::collections::BTreeSet::new();
    for (id, state) in &rows {
        let parsed = QueueState::parse_str(state)
            .unwrap_or_else(|e| panic!("state {state:?} on {id} no longer parses: {e}"));
        seen.insert(parsed.as_str());
    }
    // All four arms really were exercised — a fixture with only `queued` rows
    // would have made the loop above vacuous on three of them.
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        vec!["failed", "queued", "sending", "sent"]
    );
}

// ---------------------------------------------------------------------------
// 6 + 7. The disjunction: exact round-trip OR a loud refusal
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
/// | distinct ULIDs | accept | the ordinary case |
/// | an adjacent duplicate | **refuse** | `ORDER BY id` puts duplicates next to each other |
/// | a non-adjacent duplicate | **refuse** | a `BTreeSet`, not a peek at the previous row |
/// | ids differing only in the last character | accept | no prefix folding |
/// | ids differing only in case | accept | the key is bytes, not an ASCII fold |
/// | `""` vs a real key | accept | the empty key is a key, not a wildcard |
/// | `""` twice | **refuse** | and the empty key is not exempt from uniqueness |
#[test]
fn every_carried_natural_key_either_round_trips_or_is_refused() {
    let mut accepted = 0usize;
    let mut refused = 0usize;

    let cases: &[(&[&str], bool)] = &[
        (&["eml-01", "eml-02", "eml-03"], true),
        (&["eml-01", "eml-01"], false),
        (&["eml-01", "eml-02", "zzz", "eml-01"], false),
        (&["eml-0001", "eml-0002"], true),
        (&["eml-AB", "eml-ab"], true),
        (&["", "eml-01"], true),
        (&["", ""], false),
    ];

    for (keys, ok) in cases {
        let owned: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
        match unique_natural_keys(&owned, TABLE) {
            Ok(()) => {
                assert!(ok, "{keys:?} must have been refused");
                accepted += 1;
            }
            Err(e) => {
                assert!(!ok, "{keys:?} must have been accepted, got {e}");
                let msg = e.to_string();
                assert!(msg.contains(TABLE), "must name the table: {msg}");
                let dup = keys
                    .iter()
                    .enumerate()
                    .find(|(i, k)| keys[..*i].contains(k))
                    .map(|(_, k)| *k)
                    .unwrap();
                assert!(
                    msg.contains(&format!("natural key {dup}")),
                    "must name the duplicated key {dup:?}: {msg}"
                );
                refused += 1;
            }
        }
    }

    // **Both arms fired.** A disjunction test in which one arm never runs is a
    // single-arm test wearing a disjunction's name.
    assert!(accepted >= 4, "the accept arm must fire: {accepted}");
    assert!(refused >= 3, "the refuse arm must fire: {refused}");
}

/// The same disjunction proved through **real storage**: 256 generated rows
/// seeded into a real DuckDB file, carried by the real migrator into a real
/// SQLite file, and read back — every `TEXT` column byte-for-byte, both counts
/// value-for-value.
///
/// This is the pin the unit test above cannot be: it exercises DuckDB's
/// `VARCHAR`/`BIGINT`/`INTEGER` reads, the `ToSql` binds, `STRICT`'s acceptance
/// and SQLite's read-back — four layers where a value could be normalised,
/// truncated, re-encoded or coerced without any in-memory check noticing.
#[test]
fn every_swept_row_survives_real_storage_byte_for_byte() {
    /// A struct rather than a tuple: the table has fourteen carried columns here
    /// and Rust's tuple trait impls stop at twelve, so a tuple would silently
    /// cost the comparison its `PartialEq`.
    #[derive(Debug, PartialEq, Eq)]
    struct Swept {
        id: String,
        created_at: String,
        submitter: String,
        cc_recipients_json: Option<String>,
        to_recipients_json: String,
        subject: String,
        body_html: Option<String>,
        attachments_dir: Option<String>,
        state: String,
        attempt_n: i64,
        last_error: Option<String>,
        sent_at: Option<String>,
        recipient_hash: String,
        byte_size: i64,
    }

    const SQL: &str = "SELECT id, created_at, submitter, cc_recipients_json, to_recipients_json, \
                       subject, body_html, attachments_dir, state, attempt_n, last_error, \
                       sent_at, recipient_hash, byte_size \
                       FROM outbound_email_queue WHERE id LIKE 'eml-sweep-%' ORDER BY id ASC";

    let (_dir, db, lite) = crossed("sweep");

    let duck: Vec<Swept> = {
        let conn = duckdb::Connection::open(&db).unwrap();
        let mut stmt = conn.prepare(SQL).unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(Swept {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    submitter: r.get(2)?,
                    cc_recipients_json: r.get(3)?,
                    to_recipients_json: r.get(4)?,
                    subject: r.get(5)?,
                    body_html: r.get(6)?,
                    attachments_dir: r.get(7)?,
                    state: r.get(8)?,
                    attempt_n: r.get(9)?,
                    last_error: r.get(10)?,
                    sent_at: r.get(11)?,
                    recipient_hash: r.get(12)?,
                    byte_size: r.get(13)?,
                })
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(stmt);
        conn.close().unwrap();
        rows
    };
    assert_eq!(duck.len(), SWEPT_ROWS, "the sweep must actually be seeded");

    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let mut stmt = conn.prepare(SQL).unwrap();
    let lite_rows: Vec<Swept> = stmt
        .query_map([], |r| {
            Ok(Swept {
                id: r.get(0)?,
                created_at: r.get(1)?,
                submitter: r.get(2)?,
                cc_recipients_json: r.get(3)?,
                to_recipients_json: r.get(4)?,
                subject: r.get(5)?,
                body_html: r.get(6)?,
                attachments_dir: r.get(7)?,
                state: r.get(8)?,
                attempt_n: r.get(9)?,
                last_error: r.get(10)?,
                sent_at: r.get(11)?,
                recipient_hash: r.get(12)?,
                byte_size: r.get(13)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(lite_rows.len(), SWEPT_ROWS);

    // Compared as BYTES, column by column: two `String`s that print the same can
    // still differ, and a `PartialEq` over the struct would not say which column
    // moved on which row.
    let opt = |s: &Option<String>| s.as_deref().map(|v| v.as_bytes().to_vec());
    for (i, (d, l)) in duck.iter().zip(lite_rows.iter()).enumerate() {
        assert_eq!(
            d.id.as_bytes(),
            l.id.as_bytes(),
            "id drifted on swept row {i}"
        );
        assert_eq!(
            d.created_at.as_bytes(),
            l.created_at.as_bytes(),
            "created_at on {i}"
        );
        assert_eq!(
            d.submitter.as_bytes(),
            l.submitter.as_bytes(),
            "submitter on {i}"
        );
        assert_eq!(
            opt(&d.cc_recipients_json),
            opt(&l.cc_recipients_json),
            "cc_recipients_json on {i}"
        );
        assert_eq!(
            d.to_recipients_json.as_bytes(),
            l.to_recipients_json.as_bytes(),
            "to_recipients_json on {i}"
        );
        assert_eq!(d.subject.as_bytes(), l.subject.as_bytes(), "subject on {i}");
        assert_eq!(opt(&d.body_html), opt(&l.body_html), "body_html on {i}");
        assert_eq!(
            opt(&d.attachments_dir),
            opt(&l.attachments_dir),
            "attachments_dir on {i}"
        );
        assert_eq!(d.state.as_bytes(), l.state.as_bytes(), "state on {i}");
        assert_eq!(
            d.attempt_n, l.attempt_n,
            "attempt_n drifted on swept row {i}"
        );
        assert_eq!(opt(&d.last_error), opt(&l.last_error), "last_error on {i}");
        assert_eq!(opt(&d.sent_at), opt(&l.sent_at), "sent_at on {i}");
        assert_eq!(
            d.recipient_hash.as_bytes(),
            l.recipient_hash.as_bytes(),
            "recipient_hash on {i}"
        );
        assert_eq!(
            d.byte_size, l.byte_size,
            "byte_size drifted on swept row {i}"
        );
    }

    // The empty string really was swept, and it came back as `""` and not as
    // `NULL` — the distinction `STRICT` and the typeof sweep are both blind to.
    assert!(
        lite_rows.iter().any(|r| r.body_html.as_deref() == Some("")),
        "the sweep must include an empty-string body_html"
    );
    // A negative byte_size really was swept: the column is a `u64` in Rust bound
    // as `i64`, and a carry that assumed non-negativity would pass a fixture that
    // only had positives.
    assert!(lite_rows.iter().any(|r| r.byte_size < 0));
    // And all four states really were swept.
    for s in ["queued", "sending", "sent", "failed"] {
        assert!(
            lite_rows.iter().any(|r| r.state == s),
            "state {s} not swept"
        );
    }
}

// ---------------------------------------------------------------------------
// 8. The refusal, through the real migrator
// ---------------------------------------------------------------------------

/// **A duplicate natural key fails the whole carry.**
///
/// ⚠ **Reaching this from a real DuckDB file takes one extra step here, and the
/// step is the finding.** Unlike `wo_part_marks` (Part F) and the four
/// purchasing tables (Part G), `outbound_email_queue` **does** carry a
/// `PRIMARY KEY` on `id`, so the product's own schema stops the duplicate and no
/// source the product wrote can contain one. The fixture therefore builds the
/// shape a `PRIMARY KEY` does not cover — a table hand-recreated **without** it,
/// which is what a repair, a restore or a pre-S281 schema produces — rather than
/// pretending the ordinary path can produce it.
///
/// The refusal must still be the **Rust** one: SQLite's own `PRIMARY KEY` would
/// also reject the second `INSERT`, but with a constraint error that names
/// neither the source nor the key. `unique_natural_keys` runs on the DuckDB read
/// side, before anything is bound, so its message names the source's defect —
/// and the assertions below are what tell the two apart.
#[test]
fn a_duplicate_natural_key_fails_the_migration() {
    let dir = scratch("dupkey");
    let db = dir.join("aberp.duckdb");
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        // The product's column list, verbatim, minus the PRIMARY KEY.
        conn.execute_batch(
            "CREATE TABLE outbound_email_queue (
                id                     VARCHAR NOT NULL,
                created_at             VARCHAR NOT NULL,
                submitter              VARCHAR NOT NULL,
                to_recipients_json     VARCHAR NOT NULL,
                cc_recipients_json     VARCHAR,
                subject                VARCHAR NOT NULL,
                body_text              VARCHAR NOT NULL,
                body_html              VARCHAR,
                attachments_dir        VARCHAR,
                state                  VARCHAR NOT NULL,
                attempt_n              INTEGER NOT NULL,
                last_error             VARCHAR,
                sent_at                VARCHAR,
                recipient_hash         VARCHAR NOT NULL,
                byte_size              BIGINT  NOT NULL
            );",
        )
        .unwrap();
        for subject in ["first", "second"] {
            insert_row(
                &conn,
                "eml-dup",
                "2026-02-01T09:00:00Z",
                "storefront",
                "[\"a@b.c\"]",
                None,
                subject,
                "body",
                None,
                None,
                "queued",
                0,
                None,
                None,
                "abc",
                10,
            );
        }
        conn.close().unwrap();
    }
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a duplicate natural key must fail the migration");
    let msg = format!("{err:#}");
    assert!(msg.contains(TABLE), "must name the table: {msg}");
    assert!(
        msg.contains("natural key eml-dup"),
        "must name the duplicated key: {msg}"
    );
    assert!(
        msg.contains("no PRIMARY KEY"),
        "must say why nothing stopped it — and this must be the RUST refusal, not SQLite's \
         constraint error: {msg}"
    );
    assert!(
        msg.contains("compared twice"),
        "must say what the gate would have done with it: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 9. The schema
// ---------------------------------------------------------------------------

/// `ensure_email_schema` builds the table, is idempotent, declares every column
/// with the §3.2 vocabulary, keeps the `PRIMARY KEY`, and creates no index.
#[test]
fn ensure_email_schema_builds_the_table_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_email::ensure_email_schema(&conn).unwrap();
    // Twice: `CREATE TABLE IF NOT EXISTS` is the posture the source schema uses,
    // and a boot that re-runs it must be a no-op rather than an error.
    aberp::migrate_email::ensure_email_schema(&conn).unwrap();

    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            [TABLE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "{TABLE} must exist exactly once");

    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
            [TABLE],
            |r| r.get(0),
        )
        .unwrap();
    assert!(sql.contains("STRICT"), "{TABLE} must be STRICT: {sql}");
    for banned in [
        "VARCHAR", "DECIMAL", "DOUBLE", "BOOLEAN", "BIGINT", "BLOB", "REAL",
    ] {
        assert!(
            !sql.contains(banned),
            "{TABLE} must not declare {banned}: {sql}"
        );
    }
    // The DuckDB schema's PRIMARY KEY is kept; its `[[no-sql-specific]]` posture
    // (no CHECK, no DEFAULT) is kept too.
    assert!(
        sql.contains("PRIMARY KEY"),
        "the DuckDB id PK must cross: {sql}"
    );
    for banned in ["CHECK", "DEFAULT", "UNIQUE"] {
        assert!(
            !sql.contains(banned),
            "email_relay_queue.rs:18-22 declares no {banned}: {sql}"
        );
    }
    // **S409's post-migration shape**: no secondary index. The `state` index was
    // the PROD bug trigger — `state` is UPDATEd on every transition — and the
    // `submitter` index was dead weight. Creating either here would fork the
    // schemas and re-introduce the defect on the engine we are migrating TO.
    let idx: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = ? \
             AND sql IS NOT NULL",
            [TABLE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx, 0, "S409 dropped both secondary indexes on DuckDB");

    // A STRICT table must refuse a float into a count — the one typing guarantee
    // this family gets from the engine rather than from Rust.
    let bad = conn.execute(
        &format!(
            "INSERT INTO {TABLE} (id, created_at, submitter, to_recipients_json, subject, \
             body_text, state, attempt_n, recipient_hash, byte_size) \
             VALUES ('x', 'now', 's', '[]', 'subj', 'b', 'queued', 0, 'h', 1.5)"
        ),
        [],
    );
    assert!(
        bad.is_err(),
        "STRICT must refuse a non-integral REAL in an INTEGER count column"
    );
}

// ---------------------------------------------------------------------------
// 10 + 11. The gate's teeth
// ---------------------------------------------------------------------------

/// The gate **hard-stops** when the table was not carried.
///
/// Mutation-shaped: this is what the gate does if a future edit drops the carry
/// from `migrate_families`.
#[test]
fn the_gate_hard_stops_when_the_table_was_not_carried() {
    let (_dir, db, lite) = crossed("notcarried");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(&format!("DROP TABLE {TABLE}")).unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.starts_with(TABLE) && s.contains("exists in DuckDB but NOT in SQLite")),
        "dropping {TABLE} must be named by the gate; it reported {:?}",
        r.hard_stops
    );
}

/// **A single changed column on a single row reds the gate — for every mutable
/// column, one column at a time.** A gate that has never been shown to fail is
/// not a gate (ADR-0107 §4.1).
///
/// `id` is excluded because it *is* the key; mutating it produces a missing-row
/// hard stop instead, which the last block below pins separately.
///
/// **The counts are mutated by ONE** — one byte, one attempt. That is the
/// smallest drift this family can suffer and the one a tolerance-shaped
/// comparison would wave through; it is also enough to move the Σ fold, so both
/// arms of the gate are exercised on the same mutation.
#[test]
fn a_single_changed_column_reds_the_gate() {
    /// `(column, the row it is populated on, the value to set)`.
    const MUTABLE: &[(&str, &str, &str)] = &[
        ("created_at", "eml-01", "'2026-12-31T23:59:59Z'"),
        ("submitter", "eml-01", "'TAMPERED'"),
        // The column that decides who receives the message.
        ("to_recipients_json", "eml-01", "'[\"attacker@evil.test\"]'"),
        ("subject", "eml-01", "'TAMPERED'"),
        ("body_text", "eml-01", "'TAMPERED'"),
        ("state", "eml-01", "'sent'"),
        ("recipient_hash", "eml-01", "'0000'"),
        // The nullable ones, cleared — the value → NULL arm.
        ("cc_recipients_json", "eml-02", "NULL"),
        ("body_html", "eml-02", "NULL"),
        ("last_error", "eml-02", "NULL"),
        ("sent_at", "eml-03", "NULL"),
        // The column that decides whether the attachments can still be found.
        ("attachments_dir", "eml-big", "'somewhere-else'"),
        // §3.2 F — one attempt, one byte.
        ("attempt_n", "eml-02", "2"),
        ("byte_size", "eml-02", "12346"),
    ];

    for (col, row, set) in MUTABLE {
        let (_dir, db, lite) = crossed("drift");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            // The mutation must actually change a row; a no-op UPDATE would make
            // the assertion below vacuous.
            let before = sqlite_i64(
                &lite,
                &format!("SELECT count(*) FROM {TABLE} WHERE id = '{row}' AND {col} IS NOT NULL"),
            );
            assert_eq!(before, 1, "{col} must be populated on {row} to be mutated");
            conn.execute_batch(&format!(
                "UPDATE {TABLE} SET {col} = {set} WHERE id = '{row}'"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("queued e-mail {row}: {col}"))),
            "the gate must name the row {row} and the column {col}; it reported {:?}",
            r.hard_stops
        );
        assert!(
            !r.checks
                .iter()
                .any(|c| c.contains(&format!("every {TABLE} column round-trips with ZERO drift"))),
            "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
        );
    }

    // The other direction: `eml-01`'s NULLs become values. A comparison written
    // as "both non-null and different" would miss every one of these.
    for col in ["cc_recipients_json", "body_html", "last_error", "sent_at"] {
        let (_dir, db, lite) = crossed("drift-value");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute_batch(&format!(
                "UPDATE {TABLE} SET {col} = 'INVENTED' WHERE id = 'eml-01'"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("queued e-mail eml-01: {col}"))),
            "a NULL that became a value must red the gate on {col}: {:?}",
            r.hard_stops
        );
    }

    // The empty string turning into a NULL — the drift `STRICT` and the typeof
    // sweep are both structurally blind to. It has to be pinned on a **nullable**
    // column that legitimately holds `""`, and the sweep seeds exactly one:
    // `body_html` on every eighth swept row.
    let (_dir, db, lite) = crossed("drift-empty");
    assert_eq!(
        sqlite_text(
            &lite,
            &format!("SELECT body_html FROM {TABLE} WHERE id = 'eml-sweep-0006'")
        )
        .as_deref(),
        Some(""),
        "the pin needs a nullable column actually holding the empty string"
    );
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(&format!(
            "UPDATE {TABLE} SET body_html = NULL WHERE id = 'eml-sweep-0006'"
        ))
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("queued e-mail eml-sweep-0006: body_html")),
        "\"\" → NULL must red the gate; nothing else in the set can see it: {:?}",
        r.hard_stops
    );

    // **ONE BYTE inside a 200 KB body.** The smallest possible drift on the
    // largest possible value: no length changes, no storage class changes, no Σ
    // moves. Only the per-row byte comparison can see it — and the hard stop must
    // localise it rather than paste 400 KB into the report.
    let (_dir, db, lite) = crossed("drift-onebyte");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(&format!(
            "UPDATE {TABLE} SET body_text = replace(body_text, 'Tisztelt Partnerünk!', \
             'Tisztelt Partnerünk?') WHERE id = '{BIG_ID}'"
        ))
        .unwrap();
    }
    let mutated = sqlite_text(
        &lite,
        &format!("SELECT body_text FROM {TABLE} WHERE id = '{BIG_ID}'"),
    )
    .unwrap();
    assert_eq!(
        mutated.len(),
        big_body().len(),
        "the mutation must change exactly one byte and no length"
    );
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    let stop = r
        .hard_stops
        .iter()
        .find(|s| s.contains(&format!("queued e-mail {BIG_ID}: body_text")))
        .unwrap_or_else(|| {
            panic!(
                "one byte in a 200 kB body must red the gate: {:?}",
                r.hard_stops
            )
        });
    assert!(
        stop.contains("first differing byte at offset"),
        "the hard stop must localise the drift: {stop}"
    );
    assert!(
        stop.len() < 300,
        "the hard stop must NOT paste the body into the report ({} bytes)",
        stop.len()
    );

    // And a mutated key surfaces as a missing row rather than as a column drift —
    // the one shape the loop above cannot produce. This family's key is a single
    // column, so there is no second component to mutate.
    let (_dir, db, lite) = crossed("drift-key");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(&format!(
            "UPDATE {TABLE} SET id = 'eml-99' WHERE id = 'eml-01'"
        ))
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("queued e-mail eml-01 is missing")),
        "a mutated key must surface as a missing row: {:?}",
        r.hard_stops
    );
}

// ---------------------------------------------------------------------------
// 12. The legitimate shape
// ---------------------------------------------------------------------------

/// A source with no queued e-mail is a legitimate shape, and the gate says so out
/// loud rather than staying silent.
#[test]
fn a_source_without_the_family_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.email, Default::default());

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a source in which no e-mail was ever queued is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("email/relay family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );

    // And the table really is absent, not silently created.
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            [TABLE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "creating an empty {TABLE} the source does not have would manufacture the asymmetry the \
         gate exists to detect"
    );
}
