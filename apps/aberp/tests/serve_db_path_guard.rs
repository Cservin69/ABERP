//! Pins the tenant DB-binding guard in `serve::run`
//! (`guard_db_matches_tenant`). Its sibling
//! `serve_tenant_feature_guard.rs` pins the guard that inspects the
//! tenant SLUG; this one pins the guard that inspects the resolved DB
//! PATH.
//!
//! The defect being closed: `guard_tenant_matches_build` never looks at
//! the path, so `ABERP_TENANT=test ABERP_DB=~/.aberp/prod/aberp.duckdb`
//! booted a DEV binary directly onto the production DuckDB file and
//! passed every existing guard.
//!
//! These tests exec the REAL built `aberp` binary. They NEVER touch the
//! operator's `~/.aberp/**`: each spawns with `HOME` pointed at a fresh
//! temp dir, so `~/.aberp/prod/` resolves inside that temp root. The
//! decoy DB is a plain file whose mtime and bytes are asserted unchanged
//! — the guard must refuse before anything opens it.
//!
//! Only REFUSAL cases live here, deliberately. A path the guard *allows*
//! continues into the binary-hash thread, the writer lock and the OS
//! keychain, which an automated test must not drive. The allow side of
//! the rule is pinned in-process instead, against an injected root:
//! `tenant_registry::tests::db_isolation_*`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

/// Unique temp dir under the system temp root. Matches the no-`tempfile`
/// posture of `tenant_registry`'s own tests; the per-test dir is leaked
/// at end-of-test, acceptable for the OS-temp-root posture.
fn test_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-db-path-guard")
        .join(format!("{tag}-{}", std::process::id()));
    // A rerun in the same process id must start clean.
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp HOME");
    dir
}

/// Build a fake `$HOME` containing `.aberp/prod/aberp.duckdb` with
/// recognisable contents. Returns the temp home and the decoy DB path.
fn fake_home_with_prod_db(tag: &str) -> (PathBuf, PathBuf) {
    let home = test_home(tag);
    let prod_dir = home.join(".aberp").join("prod");
    fs::create_dir_all(&prod_dir).expect("create fake ~/.aberp/prod");
    let db = prod_dir.join("aberp.duckdb");
    fs::write(&db, b"decoy prod database - must never be opened").expect("write decoy DB");
    (home, db)
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .expect("stat decoy DB")
        .modified()
        .expect("read decoy DB mtime")
}

/// Spawn `aberp serve --tenant test --db <db>` with `HOME` overridden.
fn serve_with(home: &Path, db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aberp"))
        .args(["serve", "--tenant", "test"])
        .arg("--db")
        .arg(db)
        .env("HOME", home)
        // The ambient launcher env must not leak in and repoint the
        // subject-under-test at the operator's real paths.
        .env_remove("ABERP_DB")
        .env_remove("ABERP_TENANT")
        .output()
        .expect("spawn `aberp serve`")
}

/// Assert the process refused, exited 1, and named the isolation failure.
/// Returns stderr so callers can pin the case-specific wording.
fn assert_refused(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "serve must REFUSE a foreign-tenant DB path; got success exit.\nstderr:\n{stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "guard must exit(1); got {:?}\nstderr:\n{stderr}",
        output.status.code()
    );
    assert!(
        stderr.contains("tenant DB isolation"),
        "stderr must name the DB-isolation refusal; got:\n{stderr}"
    );
    stderr
}

/// Sorted listing of a directory's entry names.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("list prod dir")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// Assert the decoy prod DB was not opened or written, and that NOTHING
/// new appeared beside it.
///
/// The directory listing is the sharpest of these. With the guard
/// removed, the boot path reaches the cross-process writer lock, whose
/// `create_dir_all` + lock file land *inside prod's root* — before the
/// keychain, before DuckDB. So a stray entry here is the exact evidence
/// that the guard did not fire early enough, even when the DB file
/// itself is still byte-identical.
fn assert_db_untouched(db: &Path, before: SystemTime, before_bytes: &[u8], before_dir: &[String]) {
    let dir = db.parent().expect("prod dir");
    assert_eq!(
        listing(dir),
        before_dir,
        "new files appeared in prod's root {} — the guard fired too late (or not at all); \
         the writer-lock step runs before the keychain and DuckDB",
        dir.display()
    );
    assert_eq!(
        mtime(db),
        before,
        "decoy prod DB mtime changed — the guard let something touch the file"
    );
    assert_eq!(
        fs::read(db).expect("re-read decoy DB"),
        before_bytes,
        "decoy prod DB contents changed — the guard did not fire before the open"
    );
}

/// THE DEFECT. A dev build running as `tenant=test`, handed a `--db`
/// inside `~/.aberp/prod/`, must exit non-zero — and must not have
/// opened, created, or otherwise disturbed the file.
#[test]
#[cfg(not(feature = "production"))]
fn dev_build_refuses_db_inside_prod_tenant_root() {
    let (home, db) = fake_home_with_prod_db("direct");
    let before = mtime(&db);
    let before_bytes = fs::read(&db).expect("read decoy DB");
    let before_dir = listing(db.parent().expect("prod dir"));

    let output = serve_with(&home, &db);

    // Evidence first, wording second: if the guard regresses, the most
    // useful failure to read is "prod was touched", not "wrong string".
    assert_db_untouched(&db, before, &before_bytes, &before_dir);

    let stderr = assert_refused(&output);
    assert!(
        stderr.contains("runs as tenant 'test'"),
        "stderr must name the tenant it runs as; got:\n{stderr}"
    );
}

/// THE RESIDUAL the ported guard had to fix. Editions' version matches on
/// path COMPONENTS, so `~/link -> ~/.aberp` passed as
/// `~/link/prod/aberp.duckdb` carries no `.aberp` component and slips
/// straight through. Canonicalizing both sides catches it.
#[test]
#[cfg(all(not(feature = "production"), unix))]
fn dev_build_refuses_prod_db_reached_through_a_symlink() {
    let (home, db) = fake_home_with_prod_db("symlink");
    let before = mtime(&db);
    let before_bytes = fs::read(&db).expect("read decoy DB");
    let before_dir = listing(db.parent().expect("prod dir"));

    // ~/link -> ~/.aberp, then address the prod DB through it.
    let link = home.join("link");
    std::os::unix::fs::symlink(home.join(".aberp"), &link).expect("create symlink");
    let via_link = link.join("prod").join("aberp.duckdb");
    assert!(
        !via_link.components().any(|c| c.as_os_str() == ".aberp"),
        "test is not exercising the residual: the symlinked path still \
         carries a .aberp component, so a component-match would catch it"
    );

    let output = serve_with(&home, &via_link);
    assert_db_untouched(&db, before, &before_bytes, &before_dir);
    assert_refused(&output);
}
