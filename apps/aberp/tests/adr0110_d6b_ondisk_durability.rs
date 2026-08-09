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
//! | tier | models | at `380ba8a` | with ADR-0110 D3 |
//! |---|---|---|---|
//! | D6a (`SIGKILL`, not built here) | user-space buffering | would pass — §7.6 | passes |
//! | [`d6b_live_byte_copy_at_ack_carries_the_acked_row`] | "what is on disk" vs "what the process holds" | **PASSES** | passes |
//! | [`acked_money_write_must_survive_the_power_loss_durable_set`] | power loss: only fsync'd bytes survive | **FAILED — the spec** | **PASSES** |
//!
//! # What changed: ADR-0110 D3 (rev. 3)
//!
//! Tier 2 was RED when this file landed, and that red WAS the specification.
//! `Handle::durable_ack` (`crates/aberp-db/src/lib.rs`) closes it: at every
//! money-path ack the write path now `fsync`s the main file, `<db>.wal`, and
//! the tenant directory. The acked rows are in `<db>.wal`, so the WAL is now a
//! member of the power-loss durable set and the same assertions pass.
//!
//! **D3 took ADR-0110 Option B (fsync the WAL), not Option A (fold it).** The
//! main file is never rewritten on the money path, so `main_file_advanced_at_ack`
//! is still `false` at the ack — the RED spec's load-bearing premise survives
//! the fix intact rather than being tuned away.
//!
//! Crucially, tier 2 does not hard-code the WAL into its durable set. It
//! DERIVES the set from `Handle::fsynced_paths` (see [`power_loss_durable_set`]),
//! so the WAL is a member if and only if production code really `fsync`'d it.
//! Reverting the `fsync` drops it back out and tier 2 goes red again — the
//! derivation is the mutation proof, not a comment claiming one.
//!
//! ## Why the byte-copy tier passes, and why that is not reassuring
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
//!   still `run_durable_checkpoint_locked`, a `tracing::error!` stub that
//!   folds NOTHING, and `HandleConfig::checkpoint_enabled` still defaults to
//!   `false`. **D3 did not build H4**; it made the ack durable without one.
//! * `<db>.wal` — at `380ba8a`, **nothing in `crates/aberp-db/src/` called
//!   `sync_all`, `sync_data` or `fsync`** (the one grep hit was the word
//!   inside a log string), so the WAL was never durable and the acked rows
//!   died with it. Since D3, `Handle::durable_ack` fsyncs it at every
//!   money-path ack.
//!
//! So the post-power-loss durable set was *main file + mirror, without the
//! WAL* — precisely the state the 2026-08-08 recovery pass found: a clean
//! prefix, a mirror holding every event, and the business rows gone.
//! [`acked_money_write_must_survive_the_power_loss_durable_set`] reconstructs
//! that set and demands the acked `invoice` + `invoice_line` rows from it.
//!
//! The model is **not** rigged in either direction. It never hard-codes the
//! WAL in or out: [`power_loss_durable_set`] derives membership from the
//! `Handle`'s durability journal, which records a path only after a
//! SUCCESSFUL `sync_all`. And it asserts, as a load-bearing precondition,
//! that the main file did not advance at ack — a premise D3 preserves,
//! because Option B does not fold.
//! [`teeth_control_explicit_fold_and_fsync_at_ack_survives_power_loss`] is
//! the independent control: it reaches the same durable outcome via the
//! Option A primitive (explicit fold + fsync) on a hard-coded set.
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
//! **All `#[ignore]`s are gone.** These run in the bare
//! `cargo test --workspace --locked` that `ci.yml` already executes, and the
//! standalone `cut-gate.yml` additionally runs `tools/cut_gate_durable_ack.sh`,
//! which asserts every money-path ack still calls `Handle::durable_ack` —
//! §7.9's point being that one test is not a gate. They are unattended by
//! construction: hermetic `$TMPDIR` tenants, no network, no keychain, no
//! `~/.aberp` touch, well under a second each. Run them directly with
//!
//! ```text
//! cargo test --manifest-path apps/aberp/Cargo.toml \
//!     --test adr0110_d6b_ondisk_durability -- --nocapture
//! ```
//!
//! The latency measurement ADR-0110 §9 / R6 asks for is
//! [`durable_ack_latency_stays_inside_r6`], kept `#[ignore]`d because it is a
//! timing measurement and a wall-clock assertion in a shared CI runner is a
//! flake generator. Its bound is R6's own ("a few tens of milliseconds is
//! fine; a second is not"), not a tuned number. Run it with `-- --ignored
//! --nocapture` to print the figures.

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
    /// Wall-clock spent inside `Handle::durable_ack` for THIS write — i.e. the
    /// ADR-0110 D3 delta, measured where it actually falls: over a WAL that
    /// this commit just dirtied. Read by [`durable_ack_latency_stays_inside_r6`];
    /// timing it anywhere else (a repeat `fsync` over already-clean bytes)
    /// measures a no-op and flatters the result.
    durable_ack_took: std::time::Duration,
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
/// **and** the audit appends (CLAUDE.md rule 15), commit, guard-drop — whose
/// post-commit hook is the lockstep `sync_mirror` — and then the ADR-0110 D3
/// `durable_ack`.
///
/// The returned [`Acked`] **is** the ack: in `serve.rs` the HTTP handler
/// answers the operator 200 at exactly this point.
///
/// # This models the write path; it does not call it
///
/// The real `issue_invoice::issue_from_parsed` needs NAV credentials from the
/// OS keychain, an `MnbRatesProvider`, and a seller profile, so an unattended
/// hermetic test cannot drive it (the same reason `issue_invoice_eur_offline.rs`
/// pins helpers rather than the route). What is modelled here is the ORDER of
/// the four steps, and each one is the real primitive: `Handle::write`,
/// `allocate_in_tx` + `append_in_tx` in ONE tx, guard-drop's `sync_mirror`, and
/// `Handle::durable_ack`.
///
/// The five production sites that run this exact sequence, each with the same
/// `drop(guard); db.durable_ack()?` pair at its ack:
/// `issue_invoice::issue_from_parsed`, `issue_modification::modification_from_inputs`,
/// `issue_storno::storno_from_inputs`, `mark_invoice_paid::mark_paid`, and
/// `incoming_invoices::change_status`. The fourth of those is driven for real,
/// with no modelling at all, by
/// [`real_money_path_mark_paid_survives_the_power_loss_durable_set`].
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

    // ADR-0110 D3 — the durable-ack boundary, exactly as the five production
    // money paths run it: guard dropped (mirror fsync'd) FIRST, then the DB's
    // own fsync. This call is what puts `<db>.wal` — the only place the acked
    // `invoice` + `invoice_line` rows live — into the power-loss durable set.
    let ack_started = std::time::Instant::now();
    handle
        .durable_ack()
        .expect("ADR-0110 D3 durable-ack at the invoice-issuance ack");
    let durable_ack_took = ack_started.elapsed();

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
        durable_ack_took,
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

/// **The power-loss durable set at the ack instant — DERIVED, not declared.**
///
/// A power cut keeps only what was `fsync`'d. This builds that set from what
/// the code actually did, so the model cannot quietly drift away from the tree.
///
/// Two members are unconditional, and both warrants are properties of code
/// outside `aberp-db`:
///
/// * `<db>.audit.log` — `sync_mirror` `fsync`s it per append batch
///   (`crates/audit-ledger/src/mirror.rs`, `file.sync_all()`), from inside
///   `WriteGuard::drop`, i.e. before every ack. It is not the `Handle`'s own
///   `fsync`, so the `Handle` cannot certify it; that citation is the warrant.
/// * `<db>` — the modelling concession this file has always made. [`provision`]
///   folded and closed the main file long before the money write, so the model
///   treats the last-folded base as durable. Keeping it unconditional is what
///   makes a red read "the MONEY WRITE was lost" rather than the far less
///   useful "the tenant was never on disk at all". Since ADR-0110 D3 the write
///   path `fsync`s it too, so the concession is now also earned — but nothing
///   here leans on that.
///
/// **Every other file must be earned.** It joins the set only if
/// [`aberp_db::Handle::fsynced_paths`] — the durability journal, appended to
/// only on a SUCCESSFUL `sync_all` — says the production write path really
/// `fsync`'d it.
///
/// That is what keeps tier 2 honest in both directions. At `380ba8a` nothing in
/// `crates/aberp-db/src/` called `sync_all`, the journal is empty, the set is
/// exactly the two constants, and tier 2 is RED — byte-for-byte the set the
/// hard-coded RED spec used. With D3's `durable_ack` the WAL earns its place
/// and tier 2 is GREEN. **Delete the `fsync` and the WAL falls straight back
/// out of the set: the derivation IS the mutation test.**
///
/// # What this still cannot prove (unchanged from the RED spec's honest scope)
///
/// It takes the write path's word that `sync_all` reached stable storage. A
/// harness that can only read the filesystem cannot tell a page-cache write
/// from an `fsync`'d one — that needs fault injection below the FS, which is
/// not available here. On macOS `fsync(2)` is weaker than `F_FULLFSYNC` on top
/// of that. **R1's machine-restart clause is still not proven by this file.**
fn power_loss_durable_set(handle: &aberp_db::Handle) -> Vec<String> {
    let mut set = vec![DB_FILE.to_string(), MIRROR_FILE.to_string()];
    for path in handle.fsynced_paths() {
        let name = path
            .file_name()
            .expect("a journalled fsync path always names a file")
            .to_string_lossy()
            .into_owned();
        if !set.contains(&name) {
            set.push(name);
        }
    }
    set
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
/// **Not rigged.** The set is derived by [`power_loss_durable_set`], which adds
/// a file only when the production write path certifies it `fsync`'d it. At
/// `380ba8a` nothing did, the WAL was excluded, and this was RED — that red was
/// the specification. ADR-0110 D3 (`Handle::durable_ack`) `fsync`s the WAL at
/// the ack, the WAL earns its place in the set, and these same assertions pass.
/// Revert the `fsync` and it goes red again by the same derivation.
///
/// **GREEN since ADR-0110 D3.** This is now a real, un-ignored test and a
/// cut-gate arm (`tools/cut_gate_durable_ack.sh`, §7.9).
#[test]
fn acked_money_write_must_survive_the_power_loss_durable_set() {
    let live = tenant_dir("tier2");
    let db = live.join(DB_FILE);
    let series_id = SeriesId::new();
    provision(&db, series_id);

    let handle = aberp_db::Handle::open_default(&db, tenant_id()).expect("shared Handle");
    let acked = issue_one_acked(&handle, series_id);

    // The premise of the model, asserted rather than assumed: the ack does not
    // FOLD. ADR-0110 D3 took Option B (fsync the WAL) precisely so the main
    // file is never rewritten on the money path, which keeps the
    // `duckdb#23046` in-place-tearing surface closed and leaves the thirteen
    // boot openers seeing exactly what they saw before. If this ever fails, the
    // tree has started folding at commit and the whole durability analysis —
    // not just this test — needs re-deriving.
    assert!(
        !acked.main_file_advanced_at_ack,
        "ADR-0110 §2.2 premise broken: the main DB file ADVANCED at ack, so this \
         tree now folds at commit. Re-derive the analysis before re-tuning this."
    );

    // The acked row has to come from something the write path made durable, so
    // the WAL must have EARNED its place in the set. Pinned separately from the
    // survival assertion below because "the set is right" and "the row survived
    // it" are different facts, and a set that silently lost its derivation
    // would otherwise fail as a confusing baseline error.
    let durable = power_loss_durable_set(&handle);
    assert!(
        durable.iter().any(|n| n == WAL_FILE),
        "ADR-0110 D3 REGRESSION: at the ack of a money write the Handle's \
         durability journal does not contain {WAL_FILE}, so nothing fsync'd the \
         file the acked rows live in. Durable set derived: {durable:?}; journal: \
         {:?}",
        handle.fsynced_paths(),
    );

    let copy = tenant_dir("tier2-copy");
    let only: Vec<&str> = durable.iter().map(String::as_str).collect();
    let manifest = copy_on_disk_bytes(&live, &copy, Some(&only));

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
/// This is a harness prototype of ADR-0110 **Option A** (fold the WAL into the
/// main file, then fsync) — the road not taken. D3 shipped Option B instead
/// (fsync the WAL where it lies, no fold), so this stays as the independent
/// control: it reaches the same durable outcome by the other primitive, on a
/// hard-coded rather than derived set, and therefore keeps its power to tell a
/// broken harness apart from a broken write path. `run_durable_checkpoint_locked`
/// is still a stub and `checkpoint_enabled` still defaults `false`: D3 did not
/// build H4.
#[test]
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

// ──────────────────────────────────────────────────────────────────────
// The real production money path, driven with no modelling at all.
// ──────────────────────────────────────────────────────────────────────

/// **Tier 2 against production code rather than a model of it.**
///
/// The tier-2 test above reproduces the issuance sequence by hand, because the
/// real `issue_invoice::issue_from_parsed` needs the OS keychain, an
/// `MnbRatesProvider` and a seller profile, none of which an unattended test
/// may have. That leaves one honest gap: a reader has to take on faith that
/// production calls `Handle::durable_ack` at all.
///
/// `mark_invoice_paid::mark_paid` closes it. It is a real money-path ack — the
/// operator is told "marked paid" — its only inputs are the shared `Handle`,
/// the tenant and the payload, and it carries the identical
/// `drop(guard); db.durable_ack()?` pair. So this drives the actual shipped
/// function, cuts the power at its ack, and demands the payment back out of the
/// durable set, with nothing modelled anywhere in the chain.
///
/// What it reads back is `audit_query::payment_record_for` **against the copied
/// DB**, not against the mirror — deliberately. The mirror was always fsync'd,
/// so a mirror-only check would pass at `380ba8a` and prove nothing. The DB's
/// own `audit_ledger` table is also what `mark_paid`'s no-double-payment gate
/// reads, so a payment durable only in the mirror silently re-opens exactly the
/// double-payment window that gate exists to close.
#[test]
fn real_money_path_mark_paid_survives_the_power_loss_durable_set() {
    let live = tenant_dir("markpaid");
    let db = live.join(DB_FILE);
    let series_id = SeriesId::new();
    provision(&db, series_id);

    let handle = aberp_db::Handle::open_default(&db, tenant_id()).expect("shared Handle");
    let invoice_id = InvoiceId::new().to_prefixed_string();

    // THE PRODUCTION FUNCTION. Not a re-implementation of it.
    let outcome = aberp::mark_invoice_paid::mark_paid(
        &handle,
        tenant_id(),
        TEST_BINARY_HASH,
        "operator@test",
        aberp::mark_invoice_paid::MarkPaidInput {
            invoice_id: invoice_id.clone(),
            paid_at: "2026-08-08".to_string(),
            amount_minor: 3_480_653,
            currency: "HUF".to_string(),
            method: aberp::audit_payloads::PaymentMethod::BankTransfer,
            reference: Some("D6B-REAL-PATH".to_string()),
        },
    )
    .expect("mark_paid must succeed on a fresh invoice");
    // `mark_paid` has returned: in `serve.rs` this is the 200. Cut the power.

    let copy = tenant_dir("markpaid-copy");
    let durable = power_loss_durable_set(&handle);
    let only: Vec<&str> = durable.iter().map(String::as_str).collect();
    let manifest = copy_on_disk_bytes(&live, &copy, Some(&only));

    let conn = boot_shaped_open(&copy.join(DB_FILE));
    let ledger = aberp_audit_ledger::Ledger::from_connection(conn, tenant_id(), TEST_BINARY_HASH);
    let recovered = aberp::audit_query::payment_record_for(&ledger, &invoice_id)
        .expect("read payment record back from the durable set");

    assert_eq!(
        recovered.as_ref().map(|p| p.amount_minor),
        Some(outcome.payment.amount_minor),
        "ADR-0110 R1 VIOLATED on the REAL mark-paid path — the operator was told \
         invoice {invoice_id} was marked paid ({} minor units), and the \
         power-loss durable set does not have it back: {recovered:?}.\n\n\
         Durable set: {manifest:?}",
        outcome.payment.amount_minor,
    );
}

// ──────────────────────────────────────────────────────────────────────
// R6 — what the durable ack costs.
// ──────────────────────────────────────────────────────────────────────

/// **ADR-0110 R6 / §9 — measure the added latency; do not assume it.**
///
/// §9 estimated "~24 ms by analogy with the Editions MES measurement" and said,
/// in bold, that it was unmeasured on this tree. This measures it: the same
/// issuance twice over, once with the guard-drop mirror sync alone and once
/// with the D3 `durable_ack` on top, and prints both.
///
/// The assertion is R6's own wording — "a few tens of milliseconds per write is
/// fine; a second is not" — deliberately loose. A tight wall-clock bound in a
/// shared CI runner is a flake generator, and a gate that cries wolf gets
/// switched off. `#[ignore]`d for the same reason; run it with `--ignored
/// --nocapture` to read the numbers.
#[test]
#[ignore = "timing measurement, not a behavioural pin — see the doc comment (R6)"]
fn durable_ack_latency_stays_inside_r6() {
    const ROUNDS: u32 = 20;

    let live = tenant_dir("latency");
    let db = live.join(DB_FILE);
    let series_id = SeriesId::new();
    provision(&db, series_id);
    let handle = aberp_db::Handle::open_default(&db, tenant_id()).expect("shared Handle");

    // Warm the tenant so the first-write costs (schema touch, mirror backfill,
    // file creation) do not land inside either measurement.
    issue_one_acked(&handle, series_id);

    let mut with_ack = std::time::Duration::ZERO;
    let mut ack_only = std::time::Duration::ZERO;
    for _ in 0..ROUNDS {
        let t0 = std::time::Instant::now();
        // `Acked::durable_ack_took` times the `durable_ack` INSIDE this
        // issuance, over the WAL bytes this commit just dirtied. Timing a
        // second `durable_ack` afterwards instead would measure an fsync over
        // already-clean bytes, i.e. roughly nothing, and overstate the fix.
        let acked = issue_one_acked(&handle, series_id);
        with_ack += t0.elapsed();
        ack_only += acked.durable_ack_took;
    }

    let per_issue = with_ack / ROUNDS;
    let per_ack = ack_only / ROUNDS;
    println!(
        "ADR-0110 R6 measurement over {ROUNDS} issuances on this machine:\n  \
         full acked issuance (tx + mirror fsync + durable_ack): {per_issue:?}\n  \
         durable_ack alone (the D3 delta):                      {per_ack:?}\n  \
         issuance without it (derived):                         {:?}",
        per_issue.saturating_sub(per_ack),
    );

    assert!(
        per_issue < std::time::Duration::from_secs(1),
        "ADR-0110 R6 VIOLATED: an acked issuance costs {per_issue:?}. R6 allows a \
         few tens of milliseconds, not a second — the durable ack is now \
         operator-visible latency."
    );
}
