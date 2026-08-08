//! ADR-0110 D6b — the **on-disk-byte** durability tier.
//!
//! # What this pins
//!
//! ADR-0110 R1: *an acknowledged write must survive an unclean restart*.
//! The 2026-08-08 incident lost ~22 h of committed business writes on a
//! force-restart while the fsync'd audit mirror kept every event — a
//! flawless ledger sitting on frozen business rows (ADR-0110 §0, §2.4).
//!
//! # Three tiers, and what each one can and cannot prove
//!
//! ADR-0110 §5 D6 specifies two tiers. Measuring them produced a result the
//! ADR did not anticipate, so this file carries three:
//!
//! | tier | models | verdict at `380ba8a` |
//! |---|---|---|
//! | D6a (`SIGKILL`, not built here) | user-space buffering | would pass — §7.6 |
//! | [`d6b_live_byte_copy_at_ack_carries_the_acked_row`] | "what is on disk" vs "what the process holds" | **PASSES** |
//! | [`acked_money_write_must_survive_the_power_loss_durable_set`] | power loss: only fsync'd bytes survive | **FAILS — the spec** |
//!
//! ## Why the middle tier passes, and why that is not reassuring
//!
//! ADR-0110 §5 expected the byte-copy tier to be red. It is not. DuckDB
//! pushes its WAL records out to `aberp.duckdb.wal` at commit, so a copy of
//! the tenant directory taken at the ack instant *does* carry the invoice,
//! and a fresh instance booted from that copy replays it. That is a real
//! and worth-pinning property — but it is exactly as weak as `SIGKILL`,
//! and for the same reason §7.6 already gave for `SIGKILL`: **a file copy
//! reads through the OS page cache.** It measures *"did the write reach the
//! file"*, never *"did the write reach stable storage"*. A system with zero
//! fsync passes it.
//!
//! ## The tier that is actually the spec
//!
//! Power loss keeps only what was fsync'd. In this tree (verified, not
//! assumed):
//!
//! * `<db>.audit.log` — **fsync'd per append batch** by `sync_mirror`
//!   (`crates/audit-ledger/src/mirror.rs`, `file.sync_all()`). Survives.
//! * `<db>` main file — advanced only by a checkpoint. The runtime one is
//!   `run_durable_checkpoint_locked`, a `tracing::error!` stub that folds
//!   NOTHING, and `HandleConfig::checkpoint_enabled` defaults to `false`
//!   (`crates/aberp-db/src/lib.rs`). Never advances at ack.
//! * `<db>.wal` — **nothing in `crates/aberp-db/src/` calls `sync_all`,
//!   `sync_data` or `fsync`** (the one grep hit is the word inside a log
//!   string). Never durable.
//!
//! So the post-power-loss durable set is *main file + mirror, without the
//! WAL* — and that is precisely the state the 2026-08-08 recovery pass
//! found: a clean prefix, a mirror holding every event, and the business
//! rows gone. [`acked_money_write_must_survive_the_power_loss_durable_set`]
//! reconstructs that set and demands the acked `invoice` + `invoice_line`
//! rows from it.
//!
//! The model is **not** rigged to fail. It omits the WAL because nothing
//! fsyncs it, and it asserts, as a load-bearing precondition, that the main
//! file did not advance at ack. Land ADR-0110 D3 (a real H4: validated fold
//! + `atomic_install` + fsync) and the fold puts the row in the main file,
//! that precondition flips, and the same assertions pass on the same
//! durable set. [`teeth_control_explicit_fold_and_fsync_at_ack_survives_power_loss`]
//! demonstrates exactly that today, in the harness.
//!
//! # Honest scope
//!
//! Even the power-loss tier is a *model*: it derives the durable set from
//! audited facts about which code paths fsync, rather than cutting power to
//! a real disk. macOS additionally makes `fsync(2)` a weaker promise than
//! `F_FULLFSYNC`. **R1's machine-restart clause must not be claimed as
//! proven by any tier here** (ADR-0110 §5 D6, §7.6).
//!
//! # CI wiring (ADR-0110 §7.9)
//!
//! All three are `#[ignore]`d for now: `ci.yml` runs a bare
//! `cargo test --workspace --locked`, and this session is scoped to landing
//! the RED spec, not to enabling a blocking gate. They are written to run
//! unattended — hermetic `$TMPDIR` tenants, no network, no keychain, no
//! `~/.aberp` touch, ~0.6 s total — so promoting them to the cut-gate once
//! D1/D3 land is exactly: delete the `#[ignore]` lines. Run them with
//!
//! ```text
//! cargo test --manifest-path apps/aberp/Cargo.toml \
//!     --test adr0110_d6b_ondisk_durability -- --ignored --nocapture
//! ```

use aberp_audit_ledger::{
    self as audit_ledger, Actor, BinaryHash, EventKind, LedgerMeta, TenantId,
};
use aberp_billing::{
    self as billing, AllocateArgs, AllocateOutcome, BillingStore, CustomerId, DraftInvoice,
    DuckDbBillingStore, Huf, IdempotencyKey, InvoiceId, InvoiceSeries, LineItem, ResetPolicy,
    SeriesCode, SeriesId,
};
use duckdb::Connection;
use rust_decimal::Decimal;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use time::macros::datetime;
use time::OffsetDateTime;

const TEST_BINARY_HASH: BinaryHash = BinaryHash::from_bytes([0xD6; 32]);
/// The wall-clock of the 2026-08-08 recovery pass, so the fixture reads as
/// the incident it models.
const ISSUE_AT: OffsetDateTime = datetime!(2026-08-08 17:05:07 UTC);

const DB_FILE: &str = "aberp.duckdb";
const WAL_FILE: &str = "aberp.duckdb.wal";
const MIRROR_FILE: &str = "aberp.duckdb.audit.log";

fn tenant_id() -> TenantId {
    TenantId::new("tenant-adr0110-d6b").expect("test tenant id is valid")
}

fn ledger_meta() -> LedgerMeta {
    LedgerMeta::new(tenant_id(), TEST_BINARY_HASH)
}

/// A unique tenant directory under `$TMPDIR`. Never `~/.aberp`, never the
/// live tenant — every path these tests touch is created and owned here.
fn tenant_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0110-d6b-{}-{}-{:?}",
        tag,
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir tenant dir");
    dir
}

/// A boot-phase opener: a bare `Connection::open` carrying DuckDB's DEFAULT
/// pragmas — which is what all thirteen `serve.rs` boot openers are
/// (ADR-0110 §2.3). It replays the WAL on open and folds on close, so it is
/// both how prod provisions a tenant and how prod first reads the business
/// tables at boot.
fn boot_shaped_open(db: &Path) -> Connection {
    Connection::open(db).expect("open tenant DuckDB (boot-shaped)")
}

/// Provision the tenant the way boot does — billing schema, audit schema,
/// one series — and fold it into the main file before the shared `Handle`
/// opens.
///
/// The explicit `CHECKPOINT` is what gives the assertions their meaning: it
/// makes every byte written *before* the money-write durable, so anything
/// missing afterwards is unambiguously the money-write and not setup that
/// never landed.
fn provision(db: &Path, series_id: SeriesId) {
    {
        let mut store = DuckDbBillingStore::from_connection(boot_shaped_open(db));
        store.ensure_schema().expect("billing schema");
        store
            .create_series(&InvoiceSeries {
                id: series_id,
                code: SeriesCode::new("D6B").expect("series code"),
                reset_policy: ResetPolicy::AnnualOnFiscalYear,
                fiscal_year: None,
                created_at: ISSUE_AT,
            })
            .expect("create series");
    }
    let conn = boot_shaped_open(db);
    audit_ledger::ensure_schema(&conn).expect("audit schema");
    conn.execute_batch("CHECKPOINT;")
        .expect("fold the provisioned baseline into the main file");
}

/// One `invoice_line` as both stores must agree on it.
type Line = (i64, String, Decimal, i64, i64);

/// What the write path acked to the operator. Every field is a promise the
/// durable bytes must be able to honour.
#[derive(Debug)]
struct Acked {
    invoice_id: String,
    sequence_number: u64,
    /// `(ordinal, description, quantity, unit_price_minor, vat_bp)`
    lines: Vec<Line>,
    /// Mirror entry count at the moment of the ack.
    mirror_entries: usize,
    /// Did the main DB file's bytes advance between "just before the money
    /// write" and "the ack"? At `380ba8a` this is `false` — the runtime
    /// checkpoint is a stub — which is what makes the WAL the only home of
    /// the acked row. ADR-0110 §2.2 measured the same thing in production
    /// via `source_db_sha256` being identical across snapshots 52→57.
    main_file_advanced_at_ack: bool,
}

fn fixture_args(series_id: SeriesId) -> AllocateArgs {
    AllocateArgs {
        series_id,
        draft: DraftInvoice {
            id: InvoiceId::new(),
            series_id,
            customer_id: CustomerId::new(),
            lines: vec![
                LineItem {
                    description: "CNC machining — 5-axis".to_string(),
                    quantity: Decimal::from(3),
                    unit_price: Huf(120_000),
                    vat_rate_basis_points: 2700,
                    vat_rate_kind: billing::VatRateKind::Percent,
                    note: None,
                    unit: None,
                },
                LineItem {
                    description: "Surface treatment".to_string(),
                    quantity: Decimal::from(1),
                    unit_price: Huf(45_500),
                    vat_rate_basis_points: 2700,
                    vat_rate_kind: billing::VatRateKind::Percent,
                    note: None,
                    unit: None,
                },
            ],
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
        sequence_floor: None,
        durable_high_water: None,
    }
}

/// Issue ONE invoice through the production seam: the shared
/// `aberp_db::Handle` writer, one transaction carrying the billing INSERTs
/// **and** the audit appends (CLAUDE.md rule 15), commit, then guard-drop —
/// whose post-commit hook is the lockstep `sync_mirror`.
///
/// The returned [`Acked`] **is** the ack: in `serve.rs` the HTTP handler
/// answers the operator 200 at exactly this point, with no checkpoint and
/// no further durability step of any kind.
fn issue_one_acked(handle: &aberp_db::Handle, series_id: SeriesId) -> Acked {
    let meta = ledger_meta();
    let idem = IdempotencyKey::new();
    let main_before = std::fs::read(handle.db_path()).expect("read main file pre-write");

    let invoice = {
        let mut guard = handle.write().expect("shared Handle writer");
        let tx = guard.conn().transaction().expect("begin issuance tx");
        let args = aberp::issue_invoice::with_durable_high_water(&tx, fixture_args(series_id))
            .expect("stamp S444 durable high-water");
        let outcome = billing::allocate_in_tx(&tx, args, ISSUE_AT).expect("allocate_in_tx");
        let (invoice, reservation) = match outcome {
            AllocateOutcome::Fresh {
                invoice,
                reservation,
            } => (invoice, reservation),
            AllocateOutcome::Replay { .. } => panic!("fresh fixture must not Replay"),
        };
        audit_ledger::append_in_tx(
            &tx,
            &meta,
            EventKind::InvoiceSequenceReserved,
            aberp::audit_payloads::InvoiceSequenceReservedPayload::from_outcome(
                &invoice,
                &reservation,
                idem,
            )
            .to_bytes(),
            Actor::test_only(),
            Some(idem.to_canonical_string()),
        )
        .expect("append InvoiceSequenceReserved");
        audit_ledger::append_in_tx(
            &tx,
            &meta,
            EventKind::InvoiceDraftCreated,
            aberp::audit_payloads::InvoiceDraftCreatedPayload::from_invoice(&invoice, idem)
                .to_bytes(),
            Actor::test_only(),
            Some(idem.to_canonical_string()),
        )
        .expect("append InvoiceDraftCreated");
        tx.commit().expect("commit issuance");
        // Dropping the guard here runs the post-commit hook (lockstep
        // `sync_mirror`) — the last thing that happens before `serve.rs`
        // answers the operator.
        invoice
    };

    let main_after = std::fs::read(handle.db_path()).expect("read main file at ack");
    let mirror_entries = audit_ledger::read_mirror_entries(handle.mirror_path())
        .expect("read mirror at ack")
        .len();

    Acked {
        invoice_id: invoice.id.to_prefixed_string(),
        sequence_number: invoice.sequence_number,
        lines: invoice
            .lines
            .iter()
            .enumerate()
            .map(|(ordinal, l)| {
                (
                    ordinal as i64,
                    l.description.clone(),
                    l.quantity.normalize(),
                    l.unit_price.as_i64(),
                    l.vat_rate_basis_points as i64,
                )
            })
            .collect(),
        mirror_entries,
        main_file_advanced_at_ack: main_before != main_after,
    }
}

/// Copy the tenant directory's on-disk bytes as they exist right now — main
/// file, `.wal`, `.audit.log`, every sidecar. The source `Handle` stays
/// OPEN and untouched: no graceful stop, no checkpoint, no snapshot.
///
/// `only` restricts the copy to a named durable set; `None` copies
/// everything.
fn copy_on_disk_bytes(from: &Path, to: &Path, only: Option<&[&str]>) -> Vec<(String, u64)> {
    std::fs::create_dir_all(to).expect("mkdir copy tenant dir");
    let mut manifest = Vec::new();
    for entry in std::fs::read_dir(from).expect("read tenant dir") {
        let entry = entry.expect("dir entry");
        let src = entry.path();
        // DuckDB's spill directory (`<db>.tmp/`) is scratch space, never
        // replayed at boot. Nothing else directory-shaped is expected here.
        if !src.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(allow) = only {
            if !allow.contains(&name.as_str()) {
                continue;
            }
        }
        let bytes = std::fs::copy(&src, to.join(&name)).expect("copy on-disk bytes");
        manifest.push((name, bytes));
    }
    manifest.sort();
    manifest
}

fn scalar_i64(conn: &Connection, sql: &str) -> i64 {
    let mut stmt = conn.prepare(sql).expect("prepare scalar");
    let mut rows = stmt.query_map([], |r| r.get::<_, i64>(0)).expect("query");
    rows.next().map(|v| v.expect("scalar row")).unwrap_or(0)
}

/// The `invoice` row's `sequence_number` for `invoice_id`, or `None`.
fn invoice_row(conn: &Connection, invoice_id: &str) -> Option<i64> {
    let mut stmt = conn
        .prepare("SELECT sequence_number FROM invoice WHERE id = ?;")
        .expect("prepare invoice lookup");
    let mut rows = stmt
        .query_map([invoice_id], |r| r.get::<_, i64>(0))
        .expect("query invoice");
    rows.next().map(|v| v.expect("invoice row"))
}

/// The `invoice_line` rows for `invoice_id`, ordered by ordinal.
///
/// `quantity` is a `DECIMAL(18, 6)` column, so it reads back as `"3.000000"`
/// where the issued `LineItem` held `3`. Both sides are normalised to a
/// `Decimal` so the comparison is over the VALUE — a scale difference is a
/// storage detail, whereas `3` becoming `4` is the business divergence this
/// is here to catch.
fn invoice_lines(conn: &Connection, invoice_id: &str) -> Vec<Line> {
    let mut stmt = conn
        .prepare(
            "SELECT ordinal, description, CAST(quantity AS VARCHAR), unit_price,
                    vat_rate_basis_points
             FROM invoice_line WHERE invoice_id = ? ORDER BY ordinal ASC;",
        )
        .expect("prepare line lookup");
    let rows = stmt
        .query_map([invoice_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .expect("query lines");
    rows.map(|r| {
        let (ordinal, description, quantity, unit_price, vat_bp) = r.expect("line row");
        let quantity = Decimal::from_str(&quantity)
            .expect("stored invoice_line.quantity is a decimal")
            .normalize();
        (ordinal, description, quantity, unit_price, vat_bp)
    })
    .collect()
}

/// Boot a fresh instance from `copy_db` and report, in full, every way the
/// acked state failed to survive. Empty vec == the contract held.
///
/// Anti-vacuity: the baseline check comes FIRST. If the copy lost the
/// PRE-ack provisioned series then the copy itself is broken and the
/// invoice assertions would fail for the wrong reason — so that is reported
/// as its own, differently-worded, failure.
fn durability_violations(copy_db: &Path, acked: &Acked) -> Vec<String> {
    let conn = boot_shaped_open(copy_db);
    let mut out = Vec::new();

    let series = scalar_i64(&conn, "SELECT COUNT(*) FROM invoice_series;");
    if series != 1 {
        out.push(format!(
            "HARNESS/BASELINE: the durable set lost the PRE-ack provisioned series \
             (invoice_series count = {series}, expected 1). The copy is not a usable \
             tenant, so nothing below is a durability measurement."
        ));
    }

    match invoice_row(&conn, &acked.invoice_id) {
        None => out.push(format!(
            "LOST BUSINESS ROW: `invoice` id={} (acked sequence_number={}) is ABSENT. \
             The operator was told this invoice was created. The `invoice` table holds \
             {} row(s) in total.",
            acked.invoice_id,
            acked.sequence_number,
            scalar_i64(&conn, "SELECT COUNT(*) FROM invoice;"),
        )),
        Some(seq) if seq != acked.sequence_number as i64 => out.push(format!(
            "DIVERGENT BUSINESS ROW: `invoice` id={} survived with sequence_number={} \
             but the ack promised {}.",
            acked.invoice_id, seq, acked.sequence_number,
        )),
        Some(_) => {}
    }

    let lines = invoice_lines(&conn, &acked.invoice_id);
    if lines != acked.lines {
        out.push(format!(
            "LOST/DIVERGENT INVOICE LINES: `invoice_line` for invoice_id={} is {:?} \
             ({} row(s)), but the ack promised {:?} ({} row(s)).",
            acked.invoice_id,
            lines,
            lines.len(),
            acked.lines,
            acked.lines.len(),
        ));
    }

    // The incident's signature asymmetry, appended to any failure so the
    // output shows WHICH store kept the event and which did not.
    if !out.is_empty() {
        let db_ledger = scalar_i64(&conn, "SELECT COALESCE(MAX(seq), 0) FROM audit_ledger;");
        let mirror = audit_ledger::read_mirror_entries(&audit_ledger::mirror_path_for(copy_db))
            .map(|e| e.len())
            .unwrap_or(0);
        out.push(format!(
            "ASYMMETRY AT THE SAME INSTANT: the fsync'd audit mirror holds {mirror} \
             entr(ies) (the ack saw {}); the DB's own audit_ledger head is seq \
             {db_ledger}. ADR-0110 §2.4: the boot heal repairs the ledger from the \
             mirror and never the business rows — a flawless ledger on frozen rows.",
            acked.mirror_entries,
        ));
    }
    out
}

// ──────────────────────────────────────────────────────────────────────
// Tier 1 — D6b exactly as ADR-0110 §5 specifies it. Currently GREEN.
// ──────────────────────────────────────────────────────────────────────

/// ADR-0110 §5 D6b as written: at the ack boundary copy the on-disk byte
/// state into a fresh tenant directory and boot from that copy.
///
/// **This PASSES against `380ba8a`, contradicting the ADR's expectation.**
/// DuckDB pushes WAL records out to `aberp.duckdb.wal` at commit, so the
/// copy carries the invoice and the fresh instance replays it. Pinning that
/// is worth doing — if it ever regressed we would want to know — but the
/// tier cannot be Phase 1's specification, because a file copy reads
/// through the page cache and therefore cannot distinguish "reached the
/// file" from "reached stable storage" (the §7.6 argument against
/// `SIGKILL`, which applies here verbatim).
///
/// The `main_file_advanced_at_ack` assertion is the load-bearing part: it
/// records that this tier passes *while nothing durable happened at all*.
#[test]
#[ignore = "ADR-0110 D6b tier 1: GREEN, but this session is scoped to landing the \
            RED spec, not to enabling a blocking gate (§7.9). Un-ignore to promote."]
fn d6b_live_byte_copy_at_ack_carries_the_acked_row() {
    let live = tenant_dir("tier1");
    let db = live.join(DB_FILE);
    let series_id = SeriesId::new();
    provision(&db, series_id);

    let handle = aberp_db::Handle::open_default(&db, tenant_id()).expect("shared Handle");
    let acked = issue_one_acked(&handle, series_id);

    // Nothing between the ack and the copy: no graceful stop, no
    // `snapshot now`, no CHECKPOINT, no fsync. The Handle stays open,
    // exactly as a running `serve` would be when the machine goes down.
    let copy = tenant_dir("tier1-copy");
    let manifest = copy_on_disk_bytes(&live, &copy, None);

    assert!(
        !acked.main_file_advanced_at_ack,
        "ADR-0110 §2.2 premise broken: the main DB file ADVANCED at ack, so this \
         tree now folds at commit and the whole durability analysis needs redoing. \
         On-disk at ack: {manifest:?}"
    );
    assert!(
        manifest.iter().any(|(n, sz)| n == WAL_FILE && *sz > 0),
        "the acked write must be WAL-resident for this tier to mean anything — \
         on-disk at ack: {manifest:?}"
    );

    let violations = durability_violations(&copy.join(DB_FILE), &acked);
    assert!(
        violations.is_empty(),
        "the live byte-copy tier lost the acked row.\n\nAcked: {acked:#?}\n\n\
         On-disk at ack: {manifest:?}\n\n{}",
        violations.join("\n\n"),
    );
}

// ──────────────────────────────────────────────────────────────────────
// Tier 2 — the power-loss durable set. THE RED SPEC.
// ──────────────────────────────────────────────────────────────────────

/// **The ADR-0110 Phase 1 specification.** A power cut keeps only what was
/// fsync'd. Reconstruct that set at the ack instant and demand the acked
/// `invoice` + `invoice_line` rows from it.
///
/// The durable set is `<db>` + `<db>.audit.log`, without `<db>.wal`:
///
/// * the mirror is fsync'd per append batch (`mirror.rs`, `sync_all`);
/// * nothing anywhere in `crates/aberp-db/src/` calls `sync_all` /
///   `sync_data` / `fsync`, so the WAL is never durable;
/// * the main file only advances on a checkpoint, and the runtime
///   checkpoint is a stub that folds nothing.
///
/// This is byte-for-byte the state the 2026-08-08 recovery pass found, and
/// it is the same asymmetry `s444_torn_tail_number_reuse.rs` reproduces for
/// the numbering invariant — here aimed at the business rows themselves.
///
/// **Not rigged.** The WAL is omitted because nothing fsyncs it, and the
/// `main_file_advanced_at_ack` assertion below pins that premise loudly. Land
/// D3 (a real H4: validated fold + `atomic_install` + fsync at the ack
/// boundary) and the fold puts the row into the main file, the premise
/// flips, and these same assertions pass on the same durable set — which is
/// what [`teeth_control_explicit_fold_and_fsync_at_ack_survives_power_loss`]
/// shows today.
#[test]
#[ignore = "ADR-0110 D6b tier 2: RED against this tree BY DESIGN — the durable-commit \
            contract is not built yet. This red IS the specification (§6 Phase 1). \
            Un-ignore to promote to the cut-gate once it goes green."]
fn acked_money_write_must_survive_the_power_loss_durable_set() {
    let live = tenant_dir("tier2");
    let db = live.join(DB_FILE);
    let series_id = SeriesId::new();
    provision(&db, series_id);

    let handle = aberp_db::Handle::open_default(&db, tenant_id()).expect("shared Handle");
    let acked = issue_one_acked(&handle, series_id);

    // The premise of the model, asserted rather than assumed: at the ack
    // the write path did nothing that could have made the main file
    // durable. If this ever fails, the durable set below is the wrong
    // model and the test must be re-derived, not re-tuned.
    assert!(
        !acked.main_file_advanced_at_ack,
        "ADR-0110 §2.2 premise broken: the main DB file ADVANCED at ack. The \
         durable-set model in this test assumes the runtime checkpoint folds \
         nothing; re-derive it before trusting the result."
    );

    let copy = tenant_dir("tier2-copy");
    let manifest = copy_on_disk_bytes(&live, &copy, Some(&[DB_FILE, MIRROR_FILE]));

    let violations = durability_violations(&copy.join(DB_FILE), &acked);
    assert!(
        violations.is_empty(),
        "ADR-0110 R1 VIOLATED — an acknowledged money-path write is not in the \
         POWER-LOSS DURABLE SET. The operator was told the invoice was created; \
         nothing fsync'd it.\n\nAcked: {acked:#?}\n\nDurable set: {manifest:?}\n\n{}",
        violations.join("\n\n"),
    );
}

// ──────────────────────────────────────────────────────────────────────
// Tier 2 teeth — mutation sanity for the RED spec.
// ──────────────────────────────────────────────────────────────────────

/// **Mutation sanity.** Tier 2's flow with ONE difference: an explicit
/// `CHECKPOINT` + `fsync` at the ack boundary. It must go GREEN.
///
/// Without this control, a broken harness (wrong path, empty copy,
/// unbootable copy, an over-narrow durable set) would look exactly like a
/// durability failure. With it, the pair says what neither test says alone:
/// the durable set is bootable and complete, and the ONLY variable is
/// whether the acked write was made durable.
///
/// This is a test-harness prototype of ADR-0110 D3, **not** the production
/// fix — `run_durable_checkpoint_locked` is still a stub and
/// `checkpoint_enabled` still defaults to `false` in this tree.
#[test]
#[ignore = "ADR-0110 D6b tier 2 teeth: GREEN. Ignored alongside its siblings for \
            this session (§7.9); un-ignore together with tier 2."]
fn teeth_control_explicit_fold_and_fsync_at_ack_survives_power_loss() {
    let live = tenant_dir("teeth");
    let db = live.join(DB_FILE);
    let series_id = SeriesId::new();
    provision(&db, series_id);

    let handle = aberp_db::Handle::open_default(&db, tenant_id()).expect("shared Handle");
    let acked = issue_one_acked(&handle, series_id);

    // ── THE ONLY DIFFERENCE FROM TIER 2 ────────────────────────────────
    // Fold the WAL into the main file and force it to stable storage, at
    // the ack boundary, on the shared writer.
    {
        let mut guard = handle.write().expect("shared Handle writer");
        guard
            .conn()
            .execute_batch("CHECKPOINT;")
            .expect("explicit fold at ack");
    }
    for name in [DB_FILE, WAL_FILE, MIRROR_FILE] {
        let p = live.join(name);
        if p.exists() {
            std::fs::File::open(&p)
                .expect("open for fsync")
                .sync_all()
                .expect("fsync at ack");
        }
    }
    // ───────────────────────────────────────────────────────────────────

    let copy = tenant_dir("teeth-copy");
    let manifest = copy_on_disk_bytes(&live, &copy, Some(&[DB_FILE, MIRROR_FILE]));

    let violations = durability_violations(&copy.join(DB_FILE), &acked);
    assert!(
        violations.is_empty(),
        "TEETH CONTROL FAILED — with an explicit fold+fsync at ack the acked row \
         STILL did not survive the power-loss durable set. That means this harness \
         is not measuring durability and the tier-2 red proves nothing.\n\n\
         Acked: {acked:#?}\n\nDurable set: {manifest:?}\n\n{}",
        violations.join("\n\n"),
    );
}
