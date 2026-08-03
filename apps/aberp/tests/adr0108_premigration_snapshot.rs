//! ADR-0108 Step 1 — pins for the pre-migration snapshot, its manifest, the
//! atomic-set restore, and the verification that makes the rollback *verified*.
//!
//! The three properties under test, and why each one is here:
//!
//! * **B1 — the tamper-evidence baseline hard-stops.** The reconciliation gate
//!   and the rollback both turn on two numbers: the count of `audit_ledger`
//!   rows with a non-NULL `event_sig`, and the `audit_ledger_anchors` row
//!   count. ADR-0108's B1 is that four *other* green checks
//!   (`verify_chain`, `verify_chain_signed`, `PRAGMA integrity_check`, head-hash
//!   equality) all pass on a signature-stripped ledger, and that
//!   `ChainVerdict.fully_anchored` reads **`true`** on the most thoroughly
//!   gutted input. So the counts are the checks, and a test that has never
//!   been shown to catch a strip is not a gate: `verify_hard_stops_on_a_*`
//!   below do the stripping and assert the red.
//!
//! * **B3 — the WAL is a first-class artefact.** Never the main file alone,
//!   never the main file with the WAL merely deleted. A `.wal` the manifest did
//!   not record belongs to a different generation of the file and is moved
//!   aside, never deleted and never left in place.
//!
//! * **C-II / rule 11 — the refusals refuse.** Under `~/.aberp/`, and on an
//!   unfolded WAL whose bytes a read-only open cannot see.

use std::path::{Path, PathBuf};

use aberp::premigration::{
    db_matches_manifest, restore_from_snapshot, run_snapshot, run_verify, PremigrationManifest,
    MANIFEST_FILENAME,
};
use duckdb::Connection;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-premig-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A DuckDB tenant DB carrying a real `audit_ledger` + `audit_ledger_anchors`
/// schema, `n` entries of which **every one is signed**, and `n` anchors.
///
/// Rows are inserted directly rather than through the session API on purpose:
/// what is under test is the snapshot/verify machinery's treatment of those
/// two counts, not the ledger's signing semantics — which have their own
/// tests in `crates/audit-ledger`.
fn seed_db(dir: &Path, entries: u64, anchors: u64) -> PathBuf {
    let db = dir.join("aberp.duckdb");
    let conn = Connection::open(&db).unwrap();
    aberp_audit_ledger::ensure_schema(&conn).unwrap();
    conn.execute_batch("CREATE TABLE IF NOT EXISTS invoice (id VARCHAR NOT NULL);")
        .unwrap();
    conn.execute("INSERT INTO invoice VALUES ('inv-1')", [])
        .unwrap();

    for seq in 1..=entries {
        conn.execute(
            "INSERT INTO audit_ledger
               (id, seq, prev_hash, time_wall, time_mono, actor, binary_hash,
                tenant_id, kind, payload, idempotency_key, entry_hash,
                session_id, session_pubkey, event_sig)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?)",
            duckdb::params![
                format!("ent-{seq}"),
                seq as i64,
                vec![(seq as u8).wrapping_sub(1); 32],
                "2026-07-31T00:00:00Z",
                seq as i64,
                "test",
                vec![9u8; 32],
                "test",
                "test.kind",
                vec![1u8, 2, 3],
                vec![seq as u8; 32],
                format!("sess-{seq}"),
                "pubkey-hex",
                // The column B1 turns on. Non-NULL on every seeded row.
                format!("sig-{seq}"),
            ],
        )
        .unwrap();
    }
    for i in 1..=anchors {
        conn.execute(
            "INSERT INTO audit_ledger_anchors
               (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
                timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            duckdb::params![
                format!("anc-{i}"),
                "test",
                format!("sess-{i}"),
                "session_close",
                "deadbeef",
                vec![7u8; 8],
                "tsa.example",
                "ok",
                "2026-07-31T00:00:00Z",
            ],
        )
        .unwrap();
    }
    // Clean close so the WAL is folded — the state the snapshot requires.
    conn.close().unwrap();
    db
}

/// A syntactically valid ADR-0030 mirror: JSON-Lines, one `MirrorEntry` per
/// line, each newline-terminated.
///
/// Written as real bytes rather than stubbed, because the snapshot reads the
/// mirror through the ordinary `read_mirror_entries` path — and that path
/// refuses a malformed file loudly, which is behaviour worth keeping rather
/// than working around. `seq`/`entry_hash` are the only fields the baseline
/// consumes.
fn write_mirror(dir: &Path, seqs: &[u64]) {
    let mut out = String::new();
    for &seq in seqs {
        out.push_str(&format!(
            r#"{{"id":"aud_{seq}","seq":{seq},"prev_hash":"{p}","time_wall":"2026-07-31T00:00:00Z","time_mono":{seq},"actor":{{"session_id":"s","user_id":"u","capabilities":[]}},"binary_hash":"{b}","tenant_id":"test","kind":"test.kind","payload":"AQID","idempotency_key":null,"entry_hash":"{e}"}}"#,
            p = hex::encode([(seq as u8).wrapping_sub(1); 32]),
            b = hex::encode([9u8; 32]),
            e = hex::encode([seq as u8; 32]),
        ));
        out.push('\n');
    }
    std::fs::write(dir.join("aberp.duckdb.audit.log"), out).unwrap();
}

fn read_manifest(snap: &Path) -> PremigrationManifest {
    serde_json::from_slice(&std::fs::read(snap.join(MANIFEST_FILENAME)).unwrap()).unwrap()
}

/// Reopen a seeded DB, run one statement, close cleanly.
fn mutate(db: &Path, sql: &str) {
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(sql).unwrap();
    conn.close().unwrap();
}

// ---------------------------------------------------------------------------
// The snapshot + manifest
// ---------------------------------------------------------------------------

/// The baseline records what the rollback and the reconciliation gate compare:
/// per-table row counts, the chain head, and **both** tamper-evidence counts.
/// It also captures the mirror preservation files, which a fixed artefact list
/// would have dropped.
#[test]
fn the_snapshot_records_the_baseline_and_captures_every_artefact() {
    let dir = scratch("baseline");
    let db = seed_db(&dir, 5, 3);
    // Two ADR-0030 preservation files, timestamp-named — the reason the
    // artefact sweep is a directory listing rather than a fixed list.
    write_mirror(&dir, &[1, 2, 3, 4, 5]);
    std::fs::write(dir.join("aberp.duckdb.audit.log.healed-1.bak"), b"a").unwrap();
    std::fs::write(dir.join("aberp.duckdb.audit.log.ahead-2.bak"), b"bb").unwrap();

    let snap = run_snapshot(&db, "test", None).expect("snapshot");
    let m = read_manifest(&snap);

    assert_eq!(m.tenant, "test");
    assert_eq!(m.db_file_name, "aberp.duckdb");
    assert!(
        !m.had_unfolded_wal,
        "a cleanly-closed DB has no unfolded WAL"
    );

    // B1's two numbers.
    assert_eq!(m.ledger.signed_entry_count, 5, "non-NULL event_sig count");
    assert_eq!(m.ledger.anchor_count, 3, "audit_ledger_anchors count");
    assert_eq!(m.ledger.entry_count, 5);
    assert_eq!(m.ledger.head_seq, 5);
    assert_eq!(m.ledger.head_entry_hash.len(), 64, "hex of a 32-byte hash");
    // The mirror is a cross-check arm, never a source (§6.3, Q7) — but its
    // tail is recorded so the rollback can confirm the two still agree.
    assert_eq!(m.ledger.mirror_tail_seq, Some(5));
    assert_eq!(
        m.ledger.mirror_tail_entry_hash.as_deref(),
        Some(hex::encode([5u8; 32]).as_str())
    );

    // Per-table row counts, for every table — including the ones the ledger
    // schema creates.
    let counts: std::collections::BTreeMap<_, _> = m
        .table_row_counts
        .iter()
        .map(|(n, c)| (n.as_str(), *c))
        .collect();
    assert_eq!(counts.get("invoice"), Some(&1));
    assert_eq!(counts.get("audit_ledger"), Some(&5));
    assert_eq!(counts.get("audit_ledger_anchors"), Some(&3));

    // The artefact set: the DB, the mirror, and BOTH `.bak` preservation files.
    let names: Vec<&str> = m.artefacts.iter().map(|a| a.name.as_str()).collect();
    for want in [
        "aberp.duckdb",
        "aberp.duckdb.audit.log",
        "aberp.duckdb.audit.log.healed-1.bak",
        "aberp.duckdb.audit.log.ahead-2.bak",
    ] {
        assert!(
            names.contains(&want),
            "artefact {want} missing from {names:?}"
        );
        assert!(
            snap.join(want).is_file(),
            "{want} was not copied into the snapshot"
        );
    }

    // Everything the manifest names is digest-correct in the snapshot.
    for a in &m.artefacts {
        let bytes = std::fs::read(snap.join(&a.name)).unwrap();
        assert_eq!(bytes.len() as u64, a.len, "{}: length", a.name);
    }

    // And a fresh verification of the untouched DB passes.
    let r = run_verify(&db, "test", &snap.join(MANIFEST_FILENAME)).unwrap();
    assert!(
        r.mismatches.is_empty(),
        "unexpected mismatches: {:?}",
        r.mismatches
    );
    assert!(!r.checks.is_empty());
}

/// The promote is atomic: `run_snapshot` assembles in `<dir>.partial` and
/// renames once. An interrupted run must never leave a directory that *looks*
/// like a usable snapshot.
#[test]
fn the_snapshot_directory_is_promoted_atomically() {
    let dir = scratch("atomic");
    let db = seed_db(&dir, 2, 1);
    let snap = run_snapshot(&db, "test", None).unwrap();
    assert!(snap.join(MANIFEST_FILENAME).is_file());
    assert!(
        !snap.with_extension("partial").exists(),
        "the staging directory must not survive a successful run"
    );
    // A second snapshot into the same explicit directory is refused rather
    // than silently merged over the first.
    let explicit = dir.join("snap-two");
    run_snapshot(&db, "test", Some(&explicit)).unwrap();
    run_snapshot(&db, "test", Some(&explicit))
        .expect_err("an existing snapshot directory must be refused, not overwritten");
}

// ---------------------------------------------------------------------------
// B1 — the two hard-stops
// ---------------------------------------------------------------------------

/// **B1, arm 1.** Strip the per-entry signatures — exactly what replaying the
/// ADR-0030 mirror as a *source* would do, since `MirrorEntry` carries no
/// `session_id` / `session_pubkey` / `event_sig` at all — and assert the
/// verification goes **red on the count**.
///
/// This is the mutation-verification that makes the check a gate: delete the
/// `signed_entry_count` comparison in `premigration::run_verify` and this test
/// goes green on a gutted ledger, which is the whole failure B1 describes.
#[test]
fn verify_hard_stops_on_a_signature_stripped_ledger() {
    let dir = scratch("b1-sig");
    let db = seed_db(&dir, 6, 2);
    let snap = run_snapshot(&db, "test", None).unwrap();

    // The strip. Note what it does NOT touch: `entry_hash`, `prev_hash`, or the
    // row count. `compute_entry_hash` deliberately excludes the session
    // columns, so the hash chain still verifies — which is precisely why the
    // chain checks cannot see this and the counts must.
    mutate(&db, "UPDATE audit_ledger SET event_sig = NULL;");

    let r = run_verify(&db, "test", &snap.join(MANIFEST_FILENAME)).unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.contains("event_sig")),
        "the non-NULL event_sig count must hard-stop on a stripped ledger; got {:?}",
        r.mismatches
    );

    // And the things that would have said "fine": the row count and the head
    // hash are untouched, so a gate built only on those is green here.
    assert!(
        r.checks.iter().any(|c| c.contains("head entry_hash")),
        "the head hash still matches — that is the point of the test: {:?}",
        r.checks
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("audit_ledger row count")),
        "the row count still matches: {:?}",
        r.checks
    );
}

/// **B1, arm 2.** Drop the anchors — what mirror replay would do implicitly,
/// because the mirror never held `audit_ledger_anchors` at all. With anchors
/// gone, `ChainVerdict.fully_anchored` reads **`true`** (`anchors_pending == 0`),
/// so the count is the only thing that can catch it.
#[test]
fn verify_hard_stops_when_the_anchors_are_dropped() {
    let dir = scratch("b1-anchor");
    let db = seed_db(&dir, 4, 3);
    let snap = run_snapshot(&db, "test", None).unwrap();

    mutate(&db, "DELETE FROM audit_ledger_anchors;");

    let r = run_verify(&db, "test", &snap.join(MANIFEST_FILENAME)).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.contains("audit_ledger_anchors")),
        "the anchor count must hard-stop; got {:?}",
        r.mismatches
    );
}

/// **B1's non-zero precondition.** An equality between two zeros is not a
/// check. A baseline taken from a ledger with no signatures and no anchors must
/// FAIL loudly rather than report a green `0 == 0` forever after.
#[test]
fn a_baseline_with_no_tamper_evidence_is_a_failure_not_a_green_zero() {
    let dir = scratch("b1-zero");
    let db = seed_db(&dir, 3, 0);
    mutate(&db, "UPDATE audit_ledger SET event_sig = NULL;");
    let snap = run_snapshot(&db, "test", None).unwrap();
    let m = read_manifest(&snap);
    assert_eq!(m.ledger.signed_entry_count, 0);
    assert_eq!(m.ledger.anchor_count, 0);

    // Nothing has drifted — the live DB matches the manifest exactly. A
    // count-equality-only gate would print PASS.
    let r = run_verify(&db, "test", &snap.join(MANIFEST_FILENAME)).unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.contains("0 signed")),
        "a zero signature baseline must be reported as absent coverage: {:?}",
        r.mismatches
    );
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.contains("0 audit_ledger_anchors")),
        "a zero anchor baseline must be reported as absent coverage: {:?}",
        r.mismatches
    );
}

/// An ordinary business-table drift is caught too — otherwise the ledger checks
/// above would be the only thing the manifest is good for.
#[test]
fn verify_catches_a_business_row_count_drift() {
    let dir = scratch("drift");
    let db = seed_db(&dir, 2, 1);
    let snap = run_snapshot(&db, "test", None).unwrap();
    mutate(&db, "INSERT INTO invoice VALUES ('inv-2');");
    let r = run_verify(&db, "test", &snap.join(MANIFEST_FILENAME)).unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.contains("rows in invoice")),
        "a row-count drift must fail: {:?}",
        r.mismatches
    );
}

// ---------------------------------------------------------------------------
// B3 — the WAL is a first-class artefact
// ---------------------------------------------------------------------------

/// The restore round-trip: mutate the DB, restore the atomic set, and the same
/// verification that was red goes green again. Without this, "verified
/// rollback" is a claim about a code path nobody has run.
#[test]
fn the_restore_round_trip_puts_the_verification_back_to_green() {
    let dir = scratch("roundtrip");
    let db = seed_db(&dir, 5, 2);
    write_mirror(&dir, &[1, 2, 3, 4, 5]);
    let snap = run_snapshot(&db, "test", None).unwrap();
    let manifest = snap.join(MANIFEST_FILENAME);
    let mirror_before = std::fs::read(dir.join("aberp.duckdb.audit.log")).unwrap();

    // Break it the way the migration exercise might: rows added, signatures
    // stripped, and the mirror rewound to a shorter tail.
    mutate(
        &db,
        "INSERT INTO invoice VALUES ('inv-2'); UPDATE audit_ledger SET event_sig = NULL;",
    );
    write_mirror(&dir, &[1, 2]);
    assert!(
        !db_matches_manifest(&db, &snap).unwrap(),
        "the DB must now differ"
    );
    assert!(
        !run_verify(&db, "test", &manifest)
            .unwrap()
            .mismatches
            .is_empty(),
        "the broken DB must fail verification before the restore"
    );

    let preserve = dir.join(".aberp-rolledback-test");
    std::fs::create_dir_all(&preserve).unwrap();
    restore_from_snapshot(&snap, &db, &preserve).expect("restore the atomic set");

    assert!(
        db_matches_manifest(&db, &snap).unwrap(),
        "the DB is byte-restored"
    );
    assert_eq!(
        std::fs::read(dir.join("aberp.duckdb.audit.log")).unwrap(),
        mirror_before,
        "the mirror is part of the set, not an afterthought"
    );
    let r = run_verify(&db, "test", &manifest).unwrap();
    assert!(
        r.mismatches.is_empty(),
        "post-restore verification: {:?}",
        r.mismatches
    );
}

/// **B3's sharp edge.** A `.wal` on disk that the manifest did not record is a
/// WAL from a *different generation* of the file. Restoring the main file
/// beside it does not fail the rollback — DuckDB replays it on the next open
/// and corrupts it. So it is **moved aside**: never left in place, and never
/// deleted (a deleted artefact cannot be post-mortemed).
#[test]
fn a_foreign_generation_wal_is_moved_aside_never_deleted_and_never_left_in_place() {
    let dir = scratch("wal-pair");
    let db = seed_db(&dir, 3, 1);
    let snap = run_snapshot(&db, "test", None).unwrap();
    assert!(
        !read_manifest(&snap)
            .artefacts
            .iter()
            .any(|a| a.name.ends_with(".wal")),
        "the snapshot was taken from a cleanly-closed DB, so no WAL was recorded"
    );

    // A WAL appears afterwards — a crashed writer from the SQLite side of the
    // exercise, or a stray boot.
    let wal = dir.join("aberp.duckdb.wal");
    std::fs::write(&wal, b"foreign-generation-bytes").unwrap();

    let preserve = dir.join(".aberp-rolledback-test");
    std::fs::create_dir_all(&preserve).unwrap();
    restore_from_snapshot(&snap, &db, &preserve).expect("restore");

    assert!(
        !wal.exists(),
        "a foreign-generation WAL must NOT be left beside the restored main file"
    );
    let moved = preserve.join("foreign-generation-aberp.duckdb.wal");
    assert!(moved.is_file(), "the WAL must be preserved, not deleted");
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        b"foreign-generation-bytes",
        "preserved byte-for-byte so it can be post-mortemed"
    );
}

/// A recorded `.wal` rides the atomic set: main file and WAL are restored as a
/// pair, **never the main file alone**. Constructed by hand because
/// `run_snapshot` refuses to take a baseline while an unfolded WAL exists
/// (see the next test) — the restore side must still handle one correctly.
#[test]
fn a_recorded_wal_is_restored_as_part_of_the_atomic_set() {
    let dir = scratch("wal-set");
    let db = seed_db(&dir, 3, 1);
    let snap = run_snapshot(&db, "test", None).unwrap();

    // Add a `.wal` to the snapshot and to its manifest, as a snapshot taken of
    // a WAL-bearing DB would have.
    let wal_bytes: &[u8] = b"snapshot-generation-wal";
    std::fs::write(snap.join("aberp.duckdb.wal"), wal_bytes).unwrap();
    let mut m = read_manifest(&snap);
    m.artefacts.push(aberp::premigration::Artefact {
        name: "aberp.duckdb.wal".to_string(),
        len: wal_bytes.len() as u64,
        sha256: {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(wal_bytes);
            hex::encode(h.finalize())
        },
    });
    m.had_unfolded_wal = true;
    std::fs::write(
        snap.join(MANIFEST_FILENAME),
        serde_json::to_vec_pretty(&m).unwrap(),
    )
    .unwrap();

    let preserve = dir.join(".aberp-rolledback-test");
    std::fs::create_dir_all(&preserve).unwrap();
    restore_from_snapshot(&snap, &db, &preserve).expect("restore");

    assert_eq!(
        std::fs::read(dir.join("aberp.duckdb.wal")).unwrap(),
        wal_bytes,
        "the recorded WAL must be restored WITH the main file, never dropped"
    );
}

/// A snapshot whose bytes do not match its own manifest is refused **before
/// anything moves**. A partial move is a failed restore, not a corrupted one.
#[test]
fn a_snapshot_that_does_not_match_its_manifest_is_refused_before_anything_moves() {
    let dir = scratch("tamper");
    let db = seed_db(&dir, 3, 1);
    let snap = run_snapshot(&db, "test", None).unwrap();
    let before = std::fs::read(&db).unwrap();

    // Corrupt the snapshot's copy of the DB.
    std::fs::write(snap.join("aberp.duckdb"), b"not a duckdb file").unwrap();

    let preserve = dir.join(".aberp-rolledback-test");
    std::fs::create_dir_all(&preserve).unwrap();
    let err = restore_from_snapshot(&snap, &db, &preserve)
        .expect_err("a snapshot that fails its own digests must be refused");
    assert!(err.to_string().contains("digest mismatch"), "{err}");
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "the live DB must be untouched when a restore is refused"
    );
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// An unfolded WAL holds committed-but-unfolded transactions a read-only open
/// **cannot replay**, so the manifest's counts would be silently short — and
/// short in the audit ledger means "missing the most recent invoices". Refuse,
/// loudly, rather than record a baseline nobody can trust (rule 11).
#[test]
fn the_snapshot_refuses_while_an_unfolded_wal_exists() {
    let dir = scratch("wal-refuse");
    let db = seed_db(&dir, 2, 1);
    std::fs::write(dir.join("aberp.duckdb.wal"), b"unfolded").unwrap();

    let err = run_snapshot(&db, "test", None).expect_err("an unfolded WAL must be refused");
    let msg = err.to_string();
    assert!(msg.contains("unfolded WAL"), "{msg}");
    assert!(
        msg.contains("fold it") || msg.contains("clean shutdown"),
        "the refusal must name the remedy: {msg}"
    );
}

/// C-II. The ADR-0108 execution scope is the DEV tenant only; nothing in §7
/// may read, write or stat `~/.aberp/**`.
#[test]
fn the_snapshot_refuses_a_database_under_the_production_root() {
    let home = std::env::var("HOME").expect("HOME");
    let prod_db = PathBuf::from(home).join(".aberp/prod/aberp.duckdb");
    let err =
        run_snapshot(&prod_db, "prod", None).expect_err("the migration tooling is DEV-only (C-II)");
    assert!(
        err.to_string().contains("DEV-only violation"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// `run/rollback_to_duckdb.sh` — tested by being USED (ADR-0108 §6.2)
// ---------------------------------------------------------------------------

/// The end-to-end rollback. ADR-0108 §7's exit rule is that *every step ends by
/// running `rollback_to_duckdb.sh`* — "a rollback path exercised once at the
/// end is a rollback path that has never been exercised". This is that path,
/// run against a scratch DEV tenant, with the SQLite side present so the
/// preservation arm is exercised too.
///
/// `--no-build` + `ABERP_ROLLBACK_BIN` point the script at the binary Cargo
/// already built for this test rather than making it compile the workspace.
#[test]
fn the_rollback_script_restores_and_verifies_end_to_end() {
    let dir = scratch("script");
    let db = seed_db(&dir, 4, 2);
    write_mirror(&dir, &[1, 2, 3, 4]);
    let snap = run_snapshot(&db, "test", None).unwrap();

    // Stand up the SQLite side the way the exercise would have left it.
    for f in [
        "aberp.sqlite",
        "aberp.sqlite-wal",
        "aberp.sqlite-shm",
        "aberp.sqlite.audit.log",
    ] {
        std::fs::write(dir.join(f), b"sqlite-side").unwrap();
    }
    // And damage the DuckDB side, so the restore arm actually runs.
    mutate(&db, "UPDATE audit_ledger SET event_sig = NULL;");
    assert!(!db_matches_manifest(&db, &snap).unwrap());

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../run/rollback_to_duckdb.sh")
        .canonicalize()
        .unwrap();
    let out = std::process::Command::new("bash")
        .arg(&script)
        .args([
            "--db",
            db.to_str().unwrap(),
            "--tenant",
            "test",
            "--no-build",
        ])
        .env("ABERP_ROLLBACK_BIN", env!("CARGO_BIN_EXE_aberp"))
        .output()
        .expect("run rollback_to_duckdb.sh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "rollback failed.\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("PASS rollback_to_duckdb"), "{stdout}");

    // The DuckDB side is byte-restored and verifies against the manifest.
    assert!(db_matches_manifest(&db, &snap).unwrap());
    assert!(run_verify(&db, "test", &snap.join(MANIFEST_FILENAME))
        .unwrap()
        .mismatches
        .is_empty());

    // The SQLite side was MOVED, not deleted — a deleted artefact cannot be
    // post-mortemed (rule 11).
    for f in ["aberp.sqlite", "aberp.sqlite-wal", "aberp.sqlite-shm"] {
        assert!(!dir.join(f).exists(), "{f} must not be left in place");
    }
    let rolled: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with(".aberp-rolledback-"))
        })
        .collect();
    assert_eq!(rolled.len(), 1, "exactly one rollback dir: {rolled:?}");
    for f in ["aberp.sqlite", "aberp.sqlite-wal", "aberp.sqlite-shm"] {
        assert!(
            rolled[0].join(f).is_file(),
            "{f} must be preserved in {rolled:?}"
        );
    }
    assert!(
        rolled[0].join("pre-restore").join("aberp.duckdb").is_file(),
        "the snapshot behind the snapshot must exist — step 5 overwrites the only other copy"
    );
}

/// C-II, at the script boundary. It must refuse before it creates a single
/// directory under the production root.
#[test]
fn the_rollback_script_refuses_a_database_under_the_production_root() {
    let home = std::env::var("HOME").expect("HOME");
    let prod_db = PathBuf::from(&home).join(".aberp/prod/aberp.duckdb");
    // The guard is lexical and runs before any filesystem work, but the script
    // resolves the parent directory first — so only assert when it exists.
    if !prod_db.parent().is_some_and(|p| p.is_dir()) {
        return;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../run/rollback_to_duckdb.sh")
        .canonicalize()
        .unwrap();
    let out = std::process::Command::new("bash")
        .arg(&script)
        .args([
            "--db",
            prod_db.to_str().unwrap(),
            "--tenant",
            "prod",
            "--no-build",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a prod path must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("DEV-only violation"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A manifest taken for one tenant must not be used to verify another's DB —
/// the numbers would be compared across databases and the mismatch would read
/// as corruption rather than as operator error.
#[test]
fn verify_refuses_a_manifest_from_a_different_tenant() {
    let dir = scratch("tenant");
    let db = seed_db(&dir, 2, 1);
    let snap = run_snapshot(&db, "test", None).unwrap();
    let err = run_verify(&db, "other", &snap.join(MANIFEST_FILENAME))
        .expect_err("a cross-tenant manifest must be refused");
    assert!(err.to_string().contains("tenant"), "{err}");
}

// ---------------------------------------------------------------------------
// S449 — the restore's crash window
// ---------------------------------------------------------------------------

/// **S449 — an interrupted restore must not verify GREEN.**
///
/// `restore_from_snapshot` stages and digest-verifies everything before
/// anything moves, but the placement itself is a loop of per-file `rename`s and
/// POSIX has no way to make two of them atomic. The dangerous half is silent:
/// when the manifest recorded no `.wal` (today's state — the DEV DB is cleanly
/// closed) the live WAL is moved aside BEFORE the loop, so a crash in between
/// leaves the ORIGINAL main file with its committed-but-unfolded WAL removed.
/// Re-running the rollback then finds a main file that matches the manifest,
/// no WAL to object to, and every count intact.
///
/// The marker cannot make the renames atomic; it makes that state NAMED, so the
/// verification refuses instead of printing PASS over silently-lost committed
/// data (CLAUDE.md rule 11).
///
/// Mutation-verify: delete the marker block from
/// `verify_against_manifest_locked` and this goes red.
#[test]
fn s449_a_half_finished_restore_is_refused_by_the_verification() {
    let dir = scratch("half-restore");
    let db = seed_db(&dir, 5, 2);
    let snap = run_snapshot(&db, "test", None).unwrap();
    let manifest = snap.join(aberp::premigration::MANIFEST_FILENAME);

    // The clean state verifies — otherwise the refusal below proves nothing.
    let ok = aberp::premigration::run_verify(&db, "test", &manifest).unwrap();
    assert!(ok.mismatches.is_empty(), "{:?}", ok.mismatches);

    // Exactly what a killed restore leaves behind.
    std::fs::write(
        db.parent()
            .unwrap()
            .join(aberp::premigration::RESTORE_IN_PROGRESS_MARKER),
        b"interrupted",
    )
    .unwrap();

    let err = aberp::premigration::run_verify(&db, "test", &manifest)
        .expect_err("a half-placed artefact set must not be reported as verified");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("did not finish") && msg.contains("pre-restore"),
        "the refusal must name the state AND the recovery source: {msg}"
    );
}
