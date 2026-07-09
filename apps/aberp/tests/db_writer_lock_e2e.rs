//! ADR-0099 H3 (F-E) — CROSS-PROCESS whole-DB writer lock acceptance test.
//!
//! The unit tests in `db_writer_lock.rs` prove the exclusion logic within one
//! process. This proves the property that actually closes the Defense
//! two-`serve` corruption class: a SEPARATE OS process that finds the lock held
//! is REFUSED. It re-execs the test binary as a child that acquires and HOLDS
//! the lock, then the parent (a distinct process) asserts its own acquire is
//! refused while the child holds it, and succeeds once the child releases.
//!
//! Deterministic (readiness/release rendezvous files, bounded polling) — no
//! sleep-based races.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aberp::db_writer_lock;

fn unique_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!(
        "aberp-h3-dblock-e2e-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for(path: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        if Instant::now() > deadline {
            panic!("timed out waiting for {what} at {}", path.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// CHILD ARM: a separate process that acquires the whole-DB writer lock, signals
/// readiness, holds the lock until told to release, then exits 0. Re-exec'd by
/// the parent via the test binary itself.
#[test]
fn second_process_is_refused_the_whole_db_writer_lock() {
    if std::env::var("ABERP_DBLOCK_CHILD").is_ok() {
        let db = PathBuf::from(std::env::var("ABERP_DBLOCK_DB").unwrap());
        let ready = PathBuf::from(std::env::var("ABERP_DBLOCK_READY").unwrap());
        let release = PathBuf::from(std::env::var("ABERP_DBLOCK_RELEASE").unwrap());
        let _guard = db_writer_lock::acquire_or_refuse(&db, "prod", "child")
            .expect("child must acquire the free lock");
        std::fs::write(&ready, b"held").unwrap();
        // Hold the lock until the parent signals release (bounded).
        wait_for(&release, "release signal");
        // _guard drops here -> lock released.
        return;
    }

    let dir = unique_dir("refuse");
    let db = dir.join("tenant.duckdb");
    let ready = dir.join("child.ready");
    let release = dir.join("parent.release");

    let exe = std::env::current_exe().unwrap();
    let mut child = std::process::Command::new(exe)
        .args(["--exact", "second_process_is_refused_the_whole_db_writer_lock"])
        .env("ABERP_DBLOCK_CHILD", "1")
        .env("ABERP_DBLOCK_DB", &db)
        .env("ABERP_DBLOCK_READY", &ready)
        .env("ABERP_DBLOCK_RELEASE", &release)
        .env("RUST_TEST_THREADS", "1")
        .spawn()
        .expect("spawn child holder process");

    // Wait until the child (a SEPARATE process) holds the lock.
    wait_for(&ready, "child to acquire the lock");

    // The parent — a distinct OS process — must be REFUSED while the child holds
    // it. This is the cross-process property flock provides and the in-process
    // Handle cannot.
    match db_writer_lock::try_acquire(&db, "prod") {
        Ok(Some(_)) => panic!("parent acquired the lock while a separate process held it — flock is not cross-process!"),
        Ok(None) => { /* correct: refused while held */ }
        Err(e) => panic!("parent acquire errored: {e}"),
    }
    // acquire_or_refuse must also refuse with the operator-facing message.
    match db_writer_lock::acquire_or_refuse(&db, "prod", "aberp serve") {
        Ok(_) => panic!("acquire_or_refuse acquired while a separate process held the lock"),
        Err(e) => assert!(
            e.to_string().contains("single-writer"),
            "refusal must explain the single-writer rule: {e}"
        ),
    }

    // Release the child; after it exits, the lock is free again.
    std::fs::write(&release, b"go").unwrap();
    let status = child.wait().expect("await child");
    assert!(status.success(), "child holder must exit clean");

    let reacquired = db_writer_lock::try_acquire(&db, "prod")
        .expect("acquire after release ok")
        .expect("lock must be free once the holder process exits");
    drop(reacquired);

    let _ = std::fs::remove_dir_all(&dir);
}
