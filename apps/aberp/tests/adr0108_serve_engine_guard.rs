//! ADR-0108 **T-13, end to end against the real binary** — the engine ↔
//! DB-path boot guard (`serve::guard_engine_matches_db_path`).
//!
//! Step 1 pinned the DECISION as a pure function, both arms, with no feature
//! enabled (`aberp_db::engine_path`). This pins the WIRING: that the decision
//! is actually consulted at boot, that it fires before anything opens the file,
//! and that the operator gets a message naming what to fix.
//!
//! Sibling of `serve_db_path_guard.rs`, same posture: only REFUSAL cases, the
//! real built binary, `HOME` pointed at a fresh temp dir so the operator's
//! `~/.aberp/**` is never touched, and the decoy file's bytes and mtime
//! asserted unchanged — the guard must refuse *before* anything opens it, which
//! is the whole content of ADR-0108 C-I.
//!
//! # Both engines, from one file
//!
//! The refusal is **symmetric**, and which direction this binary exhibits
//! depends on the feature it was built with. So the expectations are
//! `cfg`-gated rather than written for one engine:
//!
//! | build | refuses | accepts |
//! |---|---|---|
//! | default | `aberp.sqlite` | `aberp.duckdb` |
//! | `sqlite-engine` | `aberp.duckdb` | `aberp.sqlite` |
//!
//! The first draft asserted only the default build's arms, unconditionally.
//! Run under `--features sqlite-engine` it went red on **both** tests — not
//! because the guard was wrong, but because the guard was *right* and the test
//! was engine-blind. Fixing it upgrades the coverage: the arm that actually
//! carries C-I — *a `sqlite-engine` binary refuses `aberp.duckdb`* — is now
//! pinned end to end against a real binary, which the module docs had deferred
//! to Step 9 for want of a build that could exercise it. The Step-3 gate runs
//! `cargo test -p aberp --features sqlite-engine`, so that build exists.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

/// The DB path this build must REFUSE, and the extension it will name.
#[cfg(not(feature = "sqlite-engine"))]
const FOREIGN_DB: &str = "aberp.sqlite";
#[cfg(not(feature = "sqlite-engine"))]
const OWN_DB: &str = "aberp.duckdb";
#[cfg(not(feature = "sqlite-engine"))]
const OWN_EXT: &str = ".duckdb";

#[cfg(feature = "sqlite-engine")]
const FOREIGN_DB: &str = "aberp.duckdb";
#[cfg(feature = "sqlite-engine")]
const OWN_DB: &str = "aberp.sqlite";
#[cfg(feature = "sqlite-engine")]
const OWN_EXT: &str = ".sqlite";

fn test_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-adr0108-engine-guard")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp HOME");
    dir
}

fn serve_with(home: &Path, db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aberp"))
        .args(["serve", "--tenant", "test"])
        .arg("--db")
        .arg(db)
        .env("HOME", home)
        .env_remove("ABERP_DB")
        .env_remove("ABERP_TENANT")
        .output()
        .expect("spawn `aberp serve`")
}

fn mtime(p: &Path) -> SystemTime {
    fs::metadata(p).unwrap().modified().unwrap()
}

/// **The C-I arm.** This binary must refuse the OTHER engine's file, before
/// opening it.
///
/// Why it matters in both directions: during the reversible window both files
/// sit in the same directory. A mis-set `ABERP_DB` — or a launcher edited to
/// try the SQLite build and reverted halfway — points one engine at the other's
/// file. Neither engine would read the other's format; each would treat it as
/// corrupt or foreign, and the recovery paths that follow are the ones that
/// rewrite things.
///
/// Mutation-verify: delete the `guard_engine_matches_db_path` call from
/// `serve::run` and this test goes red (the process gets past the guard and
/// dies later, on the keychain, with a completely different message).
#[test]
fn t13_this_build_refuses_the_other_engines_db_path() {
    let home = test_home("refuses-foreign");
    let db = home.join(FOREIGN_DB);
    fs::write(
        &db,
        b"decoy - must never be opened by the other engine's build",
    )
    .unwrap();
    let before_bytes = fs::read(&db).unwrap();
    let before_mtime = mtime(&db);

    let out = serve_with(&home, &db);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert!(
        !out.status.success(),
        "this build must refuse {FOREIGN_DB}.\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("engine/DB-path mismatch"),
        "the refusal must name the engine mismatch, not some downstream symptom \
         (a keychain error here means the guard did not fire):\n{stderr}"
    );
    assert!(
        stderr.contains(OWN_EXT),
        "the refusal must name what to point --db at ({OWN_EXT}):\n{stderr}"
    );

    // C-I, at the byte level: refused BEFORE anything opened it.
    assert_eq!(
        fs::read(&db).unwrap(),
        before_bytes,
        "the decoy file was modified"
    );
    assert_eq!(mtime(&db), before_mtime, "the decoy file was touched");
    // And no sidecar or lock was created beside it.
    assert!(!home.join(format!("{FOREIGN_DB}.wal")).exists());
    assert!(!home.join(".aberp-db-writer.test.lock").exists());
}

/// An extension-less path is refused by either build. "No extension" must not
/// read as "any engine may open it" — that is the gap a `--db ./aberp` would
/// have walked through.
#[test]
fn t13_an_extensionless_db_path_is_refused() {
    let home = test_home("extensionless");
    let db = home.join("aberp");
    fs::write(&db, b"decoy").unwrap();

    let out = serve_with(&home, &db);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("engine/DB-path mismatch"),
        "stderr:\n{stderr}"
    );
}

/// The guard must not have made the ORDINARY path worse: this build's OWN file
/// gets past the engine guard and fails later for its own reasons (no such
/// database, no keychain, no port). Asserted by the *absence* of the
/// engine-mismatch message rather than by success — driving a real boot to
/// completion is what `serve_db_path_guard.rs` deliberately does not do either.
#[test]
fn this_builds_own_db_path_is_not_refused_by_the_engine_guard() {
    let home = test_home("own-allowed");
    let db = home.join(OWN_DB);

    let out = serve_with(&home, &db);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("engine/DB-path mismatch"),
        "the engine guard must not fire on this build's own path ({OWN_DB}):\n{stderr}"
    );
    assert!(
        !stderr.contains("DEV-only violation"),
        "the C-II arm is SQLite-only and this path is not under ~/.aberp:\n{stderr}"
    );
}
