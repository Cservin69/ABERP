//! ADR-0099 H2 / ADR-0095 §1·§2 — DuckDB-backed end-to-end for atomic initial
//! creation + the validated boot safe-open. These open REAL DuckDB files, so
//! they build only where the bundled libduckdb amalgamation builds — the CI
//! gate — exactly like the crate's other DuckDB-touching suites. The pure
//! crash-safe COMMIT property (plain files, no DuckDB) is unit-tested inline in
//! `src/crash_safe.rs` and runs everywhere.
//!
//! This is the prod H2 REFUSE-ARM analogue of the editions `boot_crash_recovery
//! _e2e.rs`: where editions drives `aberp recover` (auto-recovery), H2 proves
//! the boot chokepoint fn the serve path calls — `probe_open_or_preserve` —
//! REFUSES a torn live DB with preserved evidence (auto-recovery is H5). Driving
//! the full `aberp serve` process is intentionally avoided: boot reads the OS
//! keychain before this chokepoint, which prompts/fails headless; the exact fn
//! the boot invokes is exercised directly instead (no divergent second path).

use std::path::{Path, PathBuf};

use duckdb::Connection;

struct Tmp(PathBuf);
impl Tmp {
    fn new(label: &str) -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "aberp-crashsafe-e2e-{label}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn dir(&self) -> &Path {
        &self.0
    }
    fn db(&self) -> PathBuf {
        self.0.join("aberp.duckdb")
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The billing-seed-shaped init the boot chokepoint hands `provision_atomic`:
/// build a valid, openable DuckDB aside at `creating` (never the live path).
fn seed_init(creating: &Path) -> Result<(), duckdb::Error> {
    let conn = Connection::open(creating)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS provisioned (id BIGINT);")?;
    conn.execute("INSERT INTO provisioned VALUES (1)", [])?;
    Ok(())
}

/// A clean DB opens + answers the catalog probe the safe-open uses.
fn opens_cleanly(db: &Path) -> bool {
    Connection::open(db)
        .and_then(|c| c.execute_batch("PRAGMA database_list;"))
        .is_ok()
}

fn corrupt_copies(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".CORRUPT-"))
        })
        .collect()
}

fn creating_staging(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".creating-"))
        })
        .collect()
}

/// Inject the exact on-disk signature a process killed mid-checkpoint leaves: a
/// torn / unopenable DuckDB file (ADR-0095 root cause #1).
fn make_torn(path: &Path) {
    std::fs::write(path, b"TORN-DUCKDB-HEADER-meta_block=0x0\x00\x00").unwrap();
}

// ── atomic creation (ADR-0095 §1·§2) ─────────────────────────────────────

/// DB-missing ⇒ the live path appears atomically as a valid, openable DuckDB
/// carrying the seed, with a verified-good marker and no staging litter.
#[test]
fn provision_atomic_creates_valid_openable_db_with_marker() {
    let t = Tmp::new("provision");
    let db = t.db();
    assert!(!db.exists(), "precondition: DB absent");

    aberp_snapshot::provision_atomic(&db, seed_init).expect("provision_atomic");

    assert!(db.exists(), "the live DB now exists (installed atomically)");
    assert!(opens_cleanly(&db), "the installed DB opens cleanly");
    assert!(
        aberp_snapshot::checkpoint_is_current(&db),
        "a verified-good marker covers the freshly installed file"
    );
    assert!(
        creating_staging(t.dir()).is_empty(),
        "no `.creating-*` staging litter remains after a clean provision"
    );

    // The seed row survived the fold + atomic swap.
    let conn = Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT count(*) FROM provisioned", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "the seed row is present in the installed DB");
}

/// A prior provision that a crash interrupted leaves a `<db>.creating-*` temp;
/// the next provision sweeps it before building (ADR-0095 §2), and the good DB
/// still installs.
#[test]
fn provision_atomic_sweeps_stale_creating_staging() {
    let t = Tmp::new("stale");
    let db = t.db();
    // Orphan staging from an interrupted prior creation.
    let orphan = t.dir().join("aberp.duckdb.creating-99999-1.duckdb");
    std::fs::write(&orphan, b"half-written-orphan").unwrap();

    aberp_snapshot::provision_atomic(&db, seed_init).expect("provision_atomic");

    assert!(!orphan.exists(), "the stale `.creating-*` orphan was swept");
    assert!(opens_cleanly(&db), "the good DB still installed cleanly");
    assert!(
        creating_staging(t.dir()).is_empty(),
        "no staging litter remains"
    );
}

// ── validated safe-open (ADR-0099 H2 refuse arm) ─────────────────────────

/// A freshly provisioned DB passes the boot probe unchanged.
#[test]
fn probe_open_or_preserve_ok_on_a_clean_db() {
    let t = Tmp::new("probe-ok");
    let db = t.db();
    aberp_snapshot::provision_atomic(&db, seed_init).expect("provision_atomic");

    aberp_snapshot::probe_open_or_preserve(&db).expect("clean DB must pass the probe");
    assert!(
        corrupt_copies(t.dir()).is_empty(),
        "a clean probe preserves nothing"
    );
}

/// RED-before/GREEN-after: pre-H2 a torn live file was opened by the FIRST boot
/// subsystem, 500-ing (or worse, self-folding) mid-boot. H2's chokepoint detects
/// it FIRST, preserves `<db>.CORRUPT-<ts>` byte-for-byte (original left in
/// place), and REFUSES — the serve boot maps the returned error to a non-zero
/// exit. No auto-recovery (that is H5).
#[test]
fn probe_open_or_preserve_refuses_and_preserves_a_torn_live_db() {
    let t = Tmp::new("torn");
    let db = t.db();
    make_torn(&db);
    assert!(
        !opens_cleanly(&db),
        "precondition: the torn file will not open"
    );

    let err = aberp_snapshot::probe_open_or_preserve(&db)
        .expect_err("a torn live DB must make the probe REFUSE");

    match err {
        aberp_snapshot::SnapshotError::DbCorruptPreserved {
            ref path,
            ref preserved,
            ..
        } => {
            assert_eq!(path, &db, "the refusal names the live DB path");
            assert!(
                preserved.exists(),
                "the corrupt DB was preserved as evidence"
            );
            assert!(
                preserved
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".CORRUPT-")),
                "evidence lands at `<db>.CORRUPT-<ts>`"
            );
            assert_eq!(
                std::fs::read(preserved).unwrap(),
                std::fs::read(&db).unwrap(),
                "evidence is a byte-for-byte copy of the torn live file"
            );
        }
        other => panic!("expected DbCorruptPreserved, got {other:?}"),
    }

    // Preserve is a COPY: the original torn file is still at the live path (H5
    // consumes both later); exactly one evidence copy was retained.
    assert!(
        db.exists(),
        "the original live file is left in place (COPY, not move)"
    );
    assert_eq!(
        corrupt_copies(t.dir()).len(),
        1,
        "exactly one `.CORRUPT-*` evidence copy retained"
    );
}
