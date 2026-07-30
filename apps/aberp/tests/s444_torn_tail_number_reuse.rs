//! S444 — a torn business tail must NEVER let the allocator re-issue an
//! invoice number.
//!
//! # The bug
//!
//! `invoice_sequence_state.next_number` was the allocator's only durable
//! floor, and it is not durable. `PRAGMA disable_checkpoint_on_shutdown`
//! and `PRAGMA wal_autocheckpoint` are **per-connection** DuckDB settings,
//! so any foreign `Connection::open` on the shared instance runs with
//! engine defaults and its close folds the `aberp_db::Handle`'s WAL (see
//! `aberp_snapshot::take_snapshot`'s doc for the measurement). Committed
//! business rows that lived only in the WAL vanish, and the counter
//! rewinds onto numbers that were already handed out — and already filed
//! with NAV.
//!
//! Measured on Ervin's DEV tenant. The audit mirror
//! (`apps/aberp-ui/aberp.duckdb.audit.log`) proves each of 62, 63 and 64
//! was handed out more than once, every repeat rejected by NAV as
//! `INVOICE_NUMBER_NOT_UNIQUE`:
//!
//! | when | invoice | seq | NAV ack |
//! |---|---|---|---|
//! | 07-27 17:49 | `inv_01KYJB52…` | 62 | SAVED |
//! | 07-28 08:25 | `inv_01KYKX7X…` | **62 again** | ABORTED |
//! | 07-28 08:27 | `inv_01KYKXC5…` | 63 | SAVED |
//! | 07-29 18:28 | `inv_01KYQJ5M…` | **62 again** | ABORTED |
//! | 07-29 18:29 | `inv_01KYQJ6V…` | **63 again** | ABORTED |
//! | 07-29 18:32 | `inv_01KYQJC3…` | 64 | SAVED |
//! | 07-30 06:09 | `inv_01KYRT9K…` | **63 again** | ABORTED |
//! | 07-30 06:10 | `inv_01KYRTAZ…` | **64 again** | ABORTED |
//! | 07-30 06:11 | `inv_01KYRTCY…` | 65 | SAVED |
//!
//! At the time of capture the DB held `next_number = 64` while the ledger
//! knew 65 had been reserved — i.e. the very next issue would have re-filed
//! 64 for a third time.
//!
//! # Why this test is crash-SHAPED and not a hand-written DELETE
//!
//! It does not delete rows to fake the tear. It reproduces the real
//! durability asymmetry:
//!
//!   * The tenant DB's recent commits live in the `.wal` sidecar (the
//!     Handle's pragmas keep them there — nothing folds them).
//!   * The audit mirror `<db>.audit.log` is a SEPARATE fsync-appended
//!     file, so it survives independently.
//!
//! Losing the WAL while keeping the mirror is exactly what a folded /
//! truncated WAL leaves behind. The test copies the `.duckdb` file WITHOUT
//! its `.wal` (and copies the mirror), then runs the REAL boot reconciler
//! `ensure_consistent_with_db` — the same call `serve.rs` makes at boot,
//! and the source of the `db.auto_recovered` entry at the head of every
//! one of the DEV sessions above. That heal repairs `audit_ledger` and
//! ONLY `audit_ledger`; nothing rebuilds `invoice_sequence_state`. That
//! asymmetry IS the bug, and it is what the assertions pin.
//!
//! # Teeth
//!
//! `torn_business_tail_must_not_reissue_a_number` FAILS on the pre-S444
//! tree: the rebooted allocator reserves the same number the pre-tear
//! invoice already burned. Verified by reverting the `.max(durable_floor)`
//! term in `allocate_in_tx` — the assertion reports
//! `re-issued number 2 (already burned pre-tear)`.
//!
//! The mid-test `assert`s on the torn state (business tail behind, mirror
//! ahead) are load-bearing too: if a future change makes the copy durable,
//! they fail loudly rather than letting the final assertion pass
//! vacuously — the ADR-0101 S2 "guard passed vacuously" failure mode.

use aberp_audit_ledger::{
    self as audit_ledger, Actor, BinaryHash, EventKind, LedgerMeta, TenantId,
};
use aberp_billing::{
    self as billing, AllocateArgs, AllocateOutcome, BillingStore, CustomerId, DraftInvoice,
    DuckDbBillingStore, Huf, IdempotencyKey, InvoiceId, InvoiceSeries, LineItem, ResetPolicy,
    SeriesCode, SeriesId,
};
use duckdb::Connection;
use time::macros::datetime;
use time::OffsetDateTime;

const TEST_BINARY_HASH: BinaryHash = BinaryHash::from_bytes([0x44; 32]);
const ISSUE_AT: OffsetDateTime = datetime!(2026-07-30 06:09:55 UTC);

fn tenant() -> TenantId {
    TenantId::new("tenant-s444").expect("test tenant id is valid")
}

fn ledger_meta() -> LedgerMeta {
    LedgerMeta::new(tenant(), TEST_BINARY_HASH)
}

/// Per-test temp dir. NOT the shared `$TMPDIR` root directly — a unique
/// subdirectory, so the `.duckdb` / `.wal` / `.audit.log` triple cannot
/// collide with a sibling test run.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aberp-s444-{}-{}-{:?}",
        tag,
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir temp dir");
    dir
}

/// Open a connection carrying the SAME two pragmas `aberp_db::Handle`
/// applies to every connection it hands out. This is what keeps committed
/// rows WAL-resident instead of folded into the main file — the production
/// posture, and the precondition for the tear being reproducible at all.
fn open_handle_shaped(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).expect("open tenant DuckDB");
    conn.execute_batch("PRAGMA disable_checkpoint_on_shutdown;")
        .expect("pragma disable_checkpoint_on_shutdown");
    conn.execute_batch("PRAGMA wal_autocheckpoint='1TB';")
        .expect("pragma wal_autocheckpoint");
    conn
}

/// Create both schemas and the series through the REAL store surface, on a
/// connection carrying the Handle pragmas (so this setup never folds the
/// WAL either). The store is dropped afterwards; the caller reopens for the
/// issuance work.
fn setup(db: &std::path::Path, series_ids: &[SeriesId]) {
    {
        let mut store = DuckDbBillingStore::from_connection(open_handle_shaped(db));
        store.ensure_schema().expect("billing schema");
        for (i, series_id) in series_ids.iter().enumerate() {
            let series = InvoiceSeries {
                id: *series_id,
                code: SeriesCode::new(format!("S44{i}")).expect("series code"),
                reset_policy: ResetPolicy::AnnualOnFiscalYear,
                fiscal_year: None,
                created_at: ISSUE_AT,
            };
            store.create_series(&series).expect("create series");
        }
    }
    let conn = open_handle_shaped(db);
    audit_ledger::ensure_schema(&conn).expect("audit schema");
}

fn args(series_id: SeriesId) -> AllocateArgs {
    AllocateArgs {
        series_id,
        draft: DraftInvoice {
            id: InvoiceId::new(),
            series_id,
            customer_id: CustomerId::new(),
            lines: vec![LineItem {
                description: "Widget".to_string(),
                quantity: rust_decimal::Decimal::from(1),
                unit_price: Huf(1_000),
                vat_rate_basis_points: 2700,
                vat_rate_kind: billing::VatRateKind::Percent,
                note: None,
                unit: None,
            }],
            issue_date: ISSUE_AT,
            payment_deadline: ISSUE_AT.date(),
            delivery_date: ISSUE_AT.date(),
        },
        idempotency_key: IdempotencyKey::new(),
        currency: billing::Currency::Huf,
        rate_metadata: None,
        bank_snapshot: None,
        invoice_note: None,
        email_recipient_override: None,
        start_value: 1,
        // Production-shaped: the S392 NAV pre-flight is compiled OUT of
        // production builds, so prod reaches the allocator with no NAV
        // floor at all. The durable floor is the only guard here — which
        // is the point of the test.
        sequence_floor: None,
        durable_high_water: None,
    }
}

/// Issue one invoice exactly the way the binary's `run_single_tx` does:
/// stamp the durable floor on the allocation transaction, allocate, append
/// `InvoiceSequenceReserved` in the SAME tx, commit, then sync the mirror.
/// Returns the burned sequence number.
fn issue_one(conn: &mut Connection, series_id: SeriesId, mirror: &std::path::Path) -> u64 {
    let meta = ledger_meta();
    let seq = {
        let tx = conn.transaction().expect("begin tx");
        let allocate_args = aberp::issue_invoice::with_durable_high_water(&tx, args(series_id))
            .expect("stamp durable high-water");
        let outcome =
            billing::allocate_in_tx(&tx, allocate_args, ISSUE_AT).expect("allocate_in_tx");
        let (invoice, reservation) = match outcome {
            AllocateOutcome::Fresh {
                invoice,
                reservation,
            } => (invoice, reservation),
            AllocateOutcome::Replay { .. } => panic!("unexpected Replay"),
        };
        let payload = aberp::audit_payloads::InvoiceSequenceReservedPayload::from_outcome(
            &invoice,
            &reservation,
            IdempotencyKey::new(),
        );
        audit_ledger::append_in_tx(
            &tx,
            &meta,
            EventKind::InvoiceSequenceReserved,
            payload.to_bytes(),
            Actor::test_only(),
            None,
        )
        .expect("append InvoiceSequenceReserved");
        tx.commit().expect("commit issuance");
        invoice.sequence_number
    };
    audit_ledger::sync_mirror(conn, &ledger_meta(), mirror).expect("sync mirror");
    seq
}

fn scalar_u64(conn: &Connection, sql: &str) -> u64 {
    let mut stmt = conn.prepare(sql).expect("prepare scalar");
    let mut rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
    match rows.next() {
        Some(v) => v.expect("scalar row") as u64,
        None => 0,
    }
}

/// The invariant: after a tear that loses the most recent committed
/// issuance from the DB while the fsync'd mirror keeps it, the next
/// allocation is STRICTLY HIGHER than every number already burned.
#[test]
fn torn_business_tail_must_not_reissue_a_number() {
    let dir = temp_dir("torn-tail");
    let db = dir.join("aberp.duckdb");
    let mirror = audit_ledger::mirror_path_for(&db);
    let series_id = SeriesId::new();

    // ── 1. A live tenant that has issued one invoice, folded to disk.
    setup(&db, &[series_id]);
    let mut conn = open_handle_shaped(&db);
    let first = issue_one(&mut conn, series_id, &mirror);
    assert_eq!(first, 1, "first issuance burns number 1");
    // Fold so number 1 (and next_number = 2) are durably in the main file.
    // Everything after this point is WAL-resident only.
    conn.execute_batch("CHECKPOINT;").expect("checkpoint");

    // ── 2. A SECOND issuance — committed, mirror-synced, WAL-resident.
    //     This is the invoice the tear will eat (DEV's #65: SAVED at NAV,
    //     yet absent from the DB).
    let second = issue_one(&mut conn, series_id, &mirror);
    assert_eq!(second, 2, "second issuance burns number 2");

    // ── 3. The tear: the DB file loses its WAL tail; the separately
    //     fsync'd mirror does not. Copy the `.duckdb` WITHOUT the `.wal`.
    let torn_db = dir.join("torn.duckdb");
    std::fs::copy(&db, &torn_db).expect("copy db without its wal");
    let torn_mirror = audit_ledger::mirror_path_for(&torn_db);
    std::fs::copy(&mirror, &torn_mirror).expect("copy mirror");
    drop(conn);

    // ── 4. Reboot on the torn file. Assert the tear is REAL before
    //     asserting anything about numbering — otherwise a future change
    //     that makes the copy durable would let the final assertion pass
    //     vacuously.
    let mut torn = open_handle_shaped(&torn_db);
    let invoices = scalar_u64(&torn, "SELECT COUNT(*) FROM invoice");
    let next_number = scalar_u64(
        &torn,
        "SELECT next_number FROM invoice_sequence_state LIMIT 1",
    );
    let db_audit_max = scalar_u64(&torn, "SELECT COALESCE(MAX(seq), 0) FROM audit_ledger");
    let mirror_max = audit_ledger::read_mirror_entries(&torn_mirror)
        .expect("read mirror")
        .last()
        .map(|e| e.seq)
        .unwrap_or(0);
    assert_eq!(
        invoices, 1,
        "tear precondition: the second invoice's business row must be GONE \
         (WAL-resident, lost with the WAL)"
    );
    assert_eq!(
        next_number, 2,
        "tear precondition: the counter must have REWOUND to 2 — the number \
         the second invoice already burned"
    );
    assert!(
        mirror_max > db_audit_max,
        "tear precondition: the fsync'd mirror must be AHEAD of the DB \
         (mirror {mirror_max} vs db {db_audit_max})"
    );

    // ── 5. The REAL boot reconciler — the same call `serve.rs` makes, and
    //     the source of `db.auto_recovered`. It heals `audit_ledger` from
    //     the mirror and NOTHING else: the business tail stays torn.
    audit_ledger::ensure_consistent_with_db(&torn, &torn_mirror)
        .expect("boot mirror reconcile heals the ledger");
    assert_eq!(
        scalar_u64(&torn, "SELECT COUNT(*) FROM invoice"),
        1,
        "the boot heal repairs the AUDIT ledger only — the business tail is \
         still torn, which is exactly why the allocator needs a durable floor"
    );

    // ── 6. Issue again. THE INVARIANT.
    let after_tear = issue_one(&mut torn, series_id, &torn_mirror);
    assert!(
        after_tear > second,
        "S444 VIOLATED: allocator re-issued number {after_tear} (already burned \
         pre-tear as {second}, and in the real incident already accepted by NAV). \
         A number once reserved must never be handed out again, even after a \
         torn-tail restart."
    );
    assert_eq!(
        after_tear, 3,
        "and it must advance by exactly one past the durable high-water mark \
         (no gratuitous gap)"
    );
}

/// The floor is derived from the ledger, so it must be bucket-scoped: a
/// reservation in a DIFFERENT series/year must not push this bucket
/// forward. Without this, S444 would silently defeat the annual reset.
#[test]
fn durable_floor_is_bucket_scoped() {
    let dir = temp_dir("bucket-scope");
    let db = dir.join("aberp.duckdb");
    let mirror = audit_ledger::mirror_path_for(&db);
    let series_a = SeriesId::new();
    let series_b = SeriesId::new();

    setup(&db, &[series_a, series_b]);
    let mut conn = open_handle_shaped(&db);

    // Push series A up to 5.
    for expected in 1..=5u64 {
        assert_eq!(issue_one(&mut conn, series_a, &mirror), expected);
    }

    // Series B is a distinct bucket: it must still start at 1, not inherit
    // A's high-water mark.
    let b_first = issue_one(&mut conn, series_b, &mirror);
    assert_eq!(
        b_first, 1,
        "a second series must not inherit another series' durable floor \
         (got {b_first}); S444 stamps series_id on the reservation entry \
         precisely so the floor stays bucket-scoped"
    );
}
