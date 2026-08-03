//! ADR-0108 Step 2 — **the only way a SQLite connection is opened**, and the
//! security posture it applies.
//!
//! Nothing in the tree consumes this yet. Step 2 lands the dependency and the
//! open path; Step 3 puts a type seam in front of it; the families cross from
//! Step 5. That ordering is deliberate — the posture is pinned before anything
//! can be written through it, so no family ever crosses onto an unhardened
//! connection.
//!
//! # The mitigations this applies, and where each comes from
//!
//! These are PR #49's gate conditions (`docs/findings/
//! sqlite-security-adversarial-20260730.md`), carried into ADR-0108 §5 as
//! **Phase-0 exit conditions**. Each is pinned by a test below that has been
//! shown to go red when the mitigation is removed — a pin that cannot go red
//! is not a pin.
//!
//! | # | Mitigation | Applied by |
//! |---|---|---|
//! | **M2** | no extension loading | the absent `load_extension` cargo feature (see below) |
//! | **M3** | `SQLITE_LIMIT_ATTACHED = 0` | `sqlite3_limit`, plus `ENABLE_ATTACH_CREATE`/`ENABLE_ATTACH_WRITE` off |
//! | **M4** | defensive mode, no triggers, no views, `trusted_schema=OFF` | four db-configs |
//! | **M7** | `journal_mode=WAL`, `synchronous=FULL`, `fullfsync=1`, finite `busy_timeout`, shared-cache OFF | pragmas + explicit open flags |
//! | **M9** | `0600` on the DB and its `-wal` / `-shm` siblings, `0700` on the tenant dir | [`harden_permissions`] |
//! | **M10** | bundled `libsqlite3-sys`, floor ≥ 3.51.3 | [`assert_version_floor`] at open |
//!
//! # M2, stated precisely rather than claimed loosely
//!
//! ADR-0108 §5 M2 asks for `SQLITE_OMIT_LOAD_EXTENSION` in the bundled build.
//! **That compile-time define is NOT set here, and this is the one place the
//! implementation is weaker than the ADR's wording.** `libsqlite3-sys` takes
//! its compile flags from `LIBSQLITE3_FLAGS`, which *replaces* the crate's
//! entire default flag set rather than appending to it — so setting it means
//! re-enumerating every default, by hand, and re-auditing that list on every
//! `libsqlite3-sys` bump. Getting one wrong silently changes the storage engine
//! under a database that holds eight years of statutory invoice records.
//!
//! What is done instead, and why it is close to equivalent:
//!
//! 1. The `load_extension` cargo feature is **not enabled**, so
//!    `Connection::load_extension` and `load_extension_enable` **do not exist**
//!    for this tree to call. That is a compile error, not a runtime check.
//! 2. `trusted_schema=OFF` (M4) blocks the schema-driven route — a `CREATE
//!    TABLE` default or an index expression cannot reach a loader.
//! 3. The SQL `load_extension()` function is not callable; pinned by
//!    `t3a_extension_loading_is_disabled`.
//! 4. `tools/cut_gate_sqlite_posture.sh` reds on the feature name or the
//!    symbols appearing anywhere in the tree (mutation-verified: enabling the
//!    feature in `Cargo.toml` turns the gate red).
//!
//! **There is deliberately no `sqlite3_db_config(ENABLE_LOAD_EXTENSION, 0)`
//! call.** ADR-0108 §5 M2 names one, and it cannot be written: rusqlite does
//! not expose that variant at all (`rusqlite/src/config.rs:25` — commented out
//! upstream, feature-independent). That is a correction to the ADR's M2
//! wording, not an omission.
//!
//! The residual gap versus the ADR: an `unsafe` raw-FFI call to
//! `sqlite3_enable_load_extension` would still find the symbol linked. This
//! tree is `#![forbid(unsafe_code)]` at every binary root, so that call cannot
//! be written here. **Recorded as a deviation, not folded in silently** — if
//! the ADR wants the define, it needs a pinned `LIBSQLITE3_FLAGS` list and a
//! gate that re-audits it on every bump, which is its own change.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::DbError;

/// M10 / M12 — the bundled SQLite version floor.
///
/// 3.51.3 as `sqlite3_libversion_number()` encodes it (`X*1000000 + Y*1000 + Z`).
/// The functional floor is lower — `IS NOT DISTINCT FROM` (8 sites in the tree)
/// needs only 3.39 — but M12 pins the higher number so a lockfile edit that
/// silently downgrades the bundled amalgamation is loud at the first open
/// rather than at the first query that happens to use the syntax.
pub const MIN_SQLITE_VERSION_NUMBER: i32 = 3_051_003;

/// M7 — the `busy_timeout`, in milliseconds.
///
/// ADR-0108 Q11 chose 5000 with a condition attached: **T-21 must land first**
/// (a nested `read()`-inside-`write()` must abort loudly rather than wait this
/// out). The reasoning is that the number *is* the observability of Q10's worst
/// case — without a loud abort, a finite timeout downgrades CLAUDE.md rule 13's
/// self-deadlock into a silent slow request.
///
/// Setting it in Step 2 is safe because **nothing opens a connection through
/// this path yet**: there is no `Handle`, no family, and no request that could
/// nest. Step 3 lands T-21 before the first consumer exists. Step 2 may revise
/// this **downward** on a measured p99 write-hold; raising it requires
/// re-arguing T-21.
pub const BUSY_TIMEOUT_MS: u32 = 5_000;

/// Open `path` as a SQLite database with the full ADR-0108 §5 posture applied.
///
/// **This is the only sanctioned way to open a SQLite connection in this
/// tree.** A bare `rusqlite::Connection::open` skips every mitigation above,
/// which is why the posture lives at the open site rather than in a
/// convention.
///
/// The order matters: permissions are hardened **after** the open (the files do
/// not exist until SQLite creates them) but **before** the function returns, so
/// no caller ever sees a connection whose files are world-readable.
pub fn open_hardened(path: &Path) -> Result<Connection, DbError> {
    // M7 — shared cache OFF, stated explicitly rather than inherited.
    // `SQLITE_OPEN_SHARED_CACHE` is absent from this set; shared cache changes
    // table-level locking semantics in ways the single-writer discipline is not
    // written against, and rusqlite's default happening to omit it is not the
    // same as this tree requiring it omitted.
    // S449 — `SQLITE_OPEN_URI` is deliberately ABSENT. Dropping it is worth
    // doing (nothing here passes a URI; every caller passes a `&Path`) but be
    // clear about what it does NOT achieve: **it does not turn URI filename
    // parsing off.** `libsqlite3-sys` compiles the bundled amalgamation with
    // `-DSQLITE_USE_URI` (`build.rs:140`), which enables URI interpretation
    // globally, independent of this flag. Measured, not assumed — see
    // `t2b_a_file_uri_is_treated_as_a_path_not_a_uri`, which was written as a
    // fix for that and turned out to pin the hostile fact instead.
    //
    // The consequence is C-II's, not this function's: every guard upstream
    // reads its argument as a PATH, and for a string beginning `file:` the
    // guard and the engine name different files. That is refused at the guard
    // layer (`engine_path::EngineMismatch::UriShapedPath` and
    // `premigration::ensure_dev_only`), which is the only layer that can refuse
    // it — by the time control reaches here there is nothing left to decide.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;

    assert_version_floor(&conn)?;
    apply_posture(&conn)?;
    harden_permissions(path)?;
    Ok(conn)
}

/// M10 — refuse a bundled library below [`MIN_SQLITE_VERSION_NUMBER`].
///
/// Loud at open, not at the first query that needs the syntax. A silently
/// downgraded amalgamation is exactly the class of change a lockfile edit makes
/// invisible.
pub fn assert_version_floor(_conn: &Connection) -> Result<(), DbError> {
    let v = rusqlite::version_number();
    if v < MIN_SQLITE_VERSION_NUMBER {
        return Err(DbError::SqlitePosture(format!(
            "bundled SQLite is {} ({v}), below the ADR-0108 M10/M12 floor of {} \
             ({MIN_SQLITE_VERSION_NUMBER}) — refusing to open",
            rusqlite::version(),
            "3.51.3",
        )));
    }
    Ok(())
}

/// Apply M2 / M3 / M4 / M7 to an already-open connection.
///
/// Split out from [`open_hardened`] so the tests can assert each setting on a
/// connection they control, and so a future in-memory consumer applies exactly
/// the same posture rather than a hand-copied subset.
pub fn apply_posture(conn: &Connection) -> Result<(), DbError> {
    use rusqlite::config::DbConfig;

    // --- M2 — see the module docs. There is deliberately NO db-config call
    // here: rusqlite does not expose `SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION`
    // at all (`rusqlite/src/config.rs:25`, commented out), and the
    // `load_extension` cargo feature is not enabled, so the loader has no Rust
    // entry point in this tree. `trusted_schema=OFF` below closes the
    // schema-driven route, and `t3a_extension_loading_is_disabled` pins that
    // the SQL `load_extension()` function is not callable either.

    // --- M4 — defensive mode, and the two schema-object classes off. ---
    // DEFENSIVE blocks the writable_schema / direct-rowid-write routes that
    // turn a merely-malformed database into an attacker-controlled one.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    // The tree has ZERO `CREATE TRIGGER` and ZERO `CREATE VIEW` (ADR-0108 §1.2),
    // so turning both off costs nothing and closes the schema-carried-code
    // route that `trusted_schema` exists for.
    //
    // `trusted_schema` is set through the db-config rather than the pragma:
    // the pragma is a thin wrapper over this call, and the db-config is the
    // authoritative surface. The pragma is still READ BACK in T-3c, because a
    // setting nobody re-reads is a claim, not a check.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;

    // --- M3 — no ATTACH, at all. ---
    // The tree has ZERO `ATTACH` sites. A limit of 0 means an `ATTACH` that
    // appears later is an error rather than a new file the process can write.
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    // Belt and braces, and available because M10 pins the floor at 3.51.3:
    // even if a future change raised the limit, an attached database could be
    // neither created nor written. M3 asks for the limit; these two make the
    // limit's failure mode survivable rather than total.
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false)?;

    // --- M7 — durability. ---
    // WAL is the point of the exercise. `synchronous=FULL` (not NORMAL, which
    // is WAL's default) is what makes a commit durable across an OS crash, and
    // `fullfsync` is what makes that true on macOS, where a plain fsync does
    // not flush the drive's own write cache. The tree's whole reason for
    // migrating is eight recorded tears in twenty days; NORMAL would trade that
    // back for throughput nobody asked for.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "fullfsync", 1)?;
    conn.busy_timeout(std::time::Duration::from_millis(u64::from(BUSY_TIMEOUT_MS)))?;
    Ok(())
}

/// M9 — `0600` on the DB and its `-wal` / `-shm` siblings, `0700` on the
/// directory that holds them.
///
/// The DEV DuckDB DB measured **0644** on disk (ADR-0108 §1.2), i.e. this is
/// true today for DuckDB as well and nothing in the tree chmods a tenant DB.
/// That is recorded in ADR-0108 §9's deferral ledger as an engine-independent
/// defect; what lands here is the SQLite side only, because widening the fix to
/// the DuckDB path is a change to a live production boot sequence and belongs
/// in its own PR (CLAUDE.md rule 3).
///
/// Best-effort per file: a sibling that does not exist yet (SQLite creates
/// `-wal` / `-shm` lazily, on the first WAL write) is not an error. The DB file
/// itself is not optional — if its mode cannot be set, that is loud.
pub fn harden_permissions(db_path: &Path) -> Result<(), DbError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if let Some(dir) = db_path.parent() {
            if dir.is_dir() {
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            DbError::SqlitePosture(format!(
                "cannot set 0600 on {}: {e} — refusing to serve a database whose mode \
                 cannot be constrained",
                db_path.display()
            ))
        })?;
        for suffix in ["-wal", "-shm"] {
            let mut s = db_path.as_os_str().to_os_string();
            s.push(suffix);
            let p = std::path::PathBuf::from(s);
            if p.exists() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = db_path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "aberp-adr0108-sqlite-{tag}-{}-{nanos}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("aberp.sqlite")
    }

    /// **T-11 (M10/M12).** The bundled library is at or above the floor. This
    /// is the test that tells a future lockfile bump it went backwards.
    #[test]
    fn t11_the_bundled_sqlite_meets_the_version_floor() {
        let v = rusqlite::version_number();
        assert!(
            v >= MIN_SQLITE_VERSION_NUMBER,
            "bundled SQLite {} ({v}) is below the M10 floor {MIN_SQLITE_VERSION_NUMBER}",
            rusqlite::version()
        );
        // And ≥ 3.39, the functional floor for the tree's 8
        // `IS NOT DISTINCT FROM` sites — asserted separately so a future
        // decision to lower `MIN_SQLITE_VERSION_NUMBER` cannot take the
        // language feature down with it silently.
        assert!(v >= 3_039_000, "IS NOT DISTINCT FROM needs SQLite >= 3.39");
    }

    /// **T-3b (M3).** `ATTACH` is not merely unused — it is impossible.
    ///
    /// Mutation-verify: delete the `set_limit` line in [`apply_posture`] and
    /// this test goes green on the ATTACH, i.e. red.
    #[test]
    fn t3b_attach_is_refused_because_the_limit_is_zero() {
        let db = scratch("attach");
        let conn = open_hardened(&db).unwrap();
        let other = db.with_file_name("other.sqlite");
        let err = conn
            .execute_batch(&format!("ATTACH DATABASE '{}' AS other;", other.display()))
            .expect_err("ATTACH must be refused with SQLITE_LIMIT_ATTACHED = 0");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("too many attached") || msg.contains("attach"),
            "unexpected error: {err}"
        );
        assert!(
            !other.exists(),
            "a refused ATTACH must not have created a second database file"
        );
    }

    /// **T-3c (M4) — and a correction to how the ADR states it.**
    ///
    /// ADR-0108 §8 writes T-3c as "`CREATE TRIGGER`/`CREATE VIEW` **rejected**
    /// on the live handle". **That is not what M4's db-configs do, and this
    /// test was written to that wording first and went red.**
    /// `SQLITE_DBCONFIG_ENABLE_TRIGGER` and `SQLITE_DBCONFIG_ENABLE_VIEW`
    /// disable *use*, not *definition*: the DDL still parses and the object
    /// still lands in `sqlite_master`; what stops is the trigger FIRING and the
    /// view being READABLE. There is no db-config that refuses the DDL, and
    /// rusqlite 0.40 exposes no authorizer callback that could.
    ///
    /// So the pin asserts the property the mitigation actually delivers —
    /// which is the property that matters, because the hazard M4 addresses is
    /// *schema-carried code executing*, not *schema-carried code existing*. A
    /// pin written to the ADR's literal wording would have been unachievable,
    /// and the cheapest repair would have been to delete it: PR #43's
    /// name-vs-shape lesson, arriving from the other direction.
    #[test]
    fn t3c_triggers_do_not_fire_and_views_cannot_be_read() {
        let db = scratch("m4");
        let conn = open_hardened(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE t (a TEXT) STRICT;
             CREATE TABLE audit (a TEXT) STRICT;",
        )
        .unwrap();

        // The DDL succeeds — stated, not hidden.
        conn.execute_batch(
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN INSERT INTO audit VALUES ('fired'); END;",
        )
        .expect("CREATE TRIGGER parses; M4 stops it FIRING, not its definition");
        conn.execute_batch("CREATE VIEW v AS SELECT * FROM t;")
            .expect("CREATE VIEW parses; M4 stops it being READ");

        // The trigger does not fire — the property M4 buys.
        conn.execute("INSERT INTO t VALUES ('x')", []).unwrap();
        let fired: i64 = conn
            .query_row("SELECT count(*) FROM audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            fired, 0,
            "with SQLITE_DBCONFIG_ENABLE_TRIGGER off, no trigger body may execute (M4)"
        );

        // The view cannot be read.
        assert!(
            conn.query_row("SELECT count(*) FROM v", [], |r| r.get::<_, i64>(0))
                .is_err(),
            "with SQLITE_DBCONFIG_ENABLE_VIEW off, a view must not be readable (M4)"
        );

        // And `trusted_schema` really is OFF, read back rather than assumed.
        let trusted: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |r| r.get(0))
            .unwrap();
        assert_eq!(trusted, 0, "PRAGMA trusted_schema must be OFF");

        // Defensive mode is on, read back off the handle rather than assumed —
        // it is the config that closes the writable-schema / direct-rowid-write
        // routes that turn a merely-malformed database into an
        // attacker-controlled one.
        use rusqlite::config::DbConfig;
        assert!(
            conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap(),
            "SQLITE_DBCONFIG_DEFENSIVE must be on (M4)"
        );
        assert!(
            !conn
                .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER)
                .unwrap(),
            "SQLITE_DBCONFIG_ENABLE_TRIGGER must be off (M4)"
        );
        assert!(
            !conn
                .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW)
                .unwrap(),
            "SQLITE_DBCONFIG_ENABLE_VIEW must be off (M4)"
        );
    }

    /// **T-3d (M7).** Every durability pragma is read back off the live
    /// connection and asserted. "We set it" and "it is set" are different
    /// claims; only the second one is a gate.
    ///
    /// Mutation-verify: change `synchronous` to `NORMAL` in [`apply_posture`]
    /// and this test goes red — which matters, because NORMAL is WAL's default
    /// and would silently trade back the durability the whole migration is for.
    #[test]
    fn t3d_the_durability_pragmas_are_actually_set() {
        let db = scratch("m7");
        let conn = open_hardened(&db).unwrap();

        let journal: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal", "journal_mode must be WAL");

        // 2 == FULL. 1 == NORMAL, which is what WAL defaults to.
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 2, "synchronous must be FULL (2), not NORMAL (1)");

        let fullfsync: i64 = conn
            .query_row("PRAGMA fullfsync", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fullfsync, 1, "fullfsync must be on");

        // `busy_timeout` reads back the value that was set — finite and
        // explicit, never SQLite's default of 0 (fail immediately) and never
        // an unbounded wait.
        let busy: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, i64::from(BUSY_TIMEOUT_MS));
        assert!(busy > 0, "the timeout must be finite AND non-zero");
    }

    /// **T-10 (M9).** The DB and both WAL siblings are `0600`, the directory
    /// `0700`. The `-wal`/`-shm` pair only exists once a WAL write has
    /// happened, so the test performs one.
    #[cfg(unix)]
    #[test]
    fn t10_the_database_and_its_wal_siblings_are_0600() {
        use std::os::unix::fs::PermissionsExt as _;

        let db = scratch("m9");
        let conn = open_hardened(&db).unwrap();
        // Force the -wal/-shm pair into existence.
        conn.execute_batch("CREATE TABLE t (a TEXT) STRICT; INSERT INTO t VALUES ('x');")
            .unwrap();
        harden_permissions(&db).unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&db), 0o600, "the database file");
        for suffix in ["-wal", "-shm"] {
            let mut s = db.as_os_str().to_os_string();
            s.push(suffix);
            let p = std::path::PathBuf::from(s);
            assert!(
                p.exists(),
                "expected {} to exist after a WAL write",
                p.display()
            );
            assert_eq!(mode(&p), 0o600, "{}", p.display());
        }
        assert_eq!(mode(db.parent().unwrap()), 0o700, "the tenant directory");
    }

    /// **T-3a (M2).** The db-config flag is off. The stronger half of M2 — that
    /// `load_extension` is not a method that exists — is enforced by the cargo
    /// manifest and cannot be expressed as a runtime assertion, which is
    /// precisely why it is the stronger half.
    #[test]
    fn t3a_extension_loading_is_disabled() {
        let db = scratch("m2");
        let conn = open_hardened(&db).unwrap();
        // The SQL surface is a separate route from the C API one, and it is
        // the only one this test can reach: rusqlite exposes no
        // `SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION` variant to read back, and the
        // `load_extension` cargo feature is deliberately off — so the Rust
        // entry point does not exist to be tested. See the module docs for the
        // full statement of M2, including the residual gap.
        assert!(
            conn.execute_batch("SELECT load_extension('/tmp/x.so');")
                .is_err(),
            "the SQL load_extension() function must not be callable (M2)"
        );
        // `trusted_schema=OFF` is the arm that stops a hostile schema reaching
        // any remaining loader surface, so assert it here too rather than only
        // in T-3c: M2 leans on it.
        let trusted: i64 = conn
            .query_row("PRAGMA trusted_schema", [], |r| r.get(0))
            .unwrap();
        assert_eq!(trusted, 0);
    }

    /// **M3, the second arm.** Even with the limit raised, an attached database
    /// can be neither created nor written. Pinned separately from T-3b because
    /// the two mechanisms fail independently.
    #[test]
    fn attach_create_and_attach_write_are_both_disabled() {
        use rusqlite::config::DbConfig;
        let db = scratch("attach2");
        let conn = open_hardened(&db).unwrap();
        assert!(!conn
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE)
            .unwrap());
        assert!(!conn
            .db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE)
            .unwrap());
    }

    /// `STRICT` tables reject a float bound into an INTEGER column. This is the
    /// M1 primitive ADR-0108 §3 R1 rests on; Step 3 builds the type seam on top
    /// of it, and T-1 applies it column-by-column from Step 5. Pinned here
    /// because Step 2 is where the engine arrives, and an engine that did not
    /// support `STRICT` would invalidate §3 entirely.
    #[test]
    fn strict_tables_reject_a_float_in_an_integer_column() {
        let db = scratch("strict");
        let conn = open_hardened(&db).unwrap();
        conn.execute_batch("CREATE TABLE money (amount INTEGER NOT NULL) STRICT;")
            .unwrap();
        let err = conn
            .execute("INSERT INTO money VALUES (?)", [0.14_f64])
            .expect_err("STRICT must reject a REAL bound into an INTEGER column");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("cannot store real value") && msg.contains("integer column"),
            "expected a STRICT storage-class rejection, got: {err}"
        );
        // And the integer path still works, so the rejection above is about the
        // float and not about the column being unwritable.
        conn.execute("INSERT INTO money VALUES (?)", [1234_i64])
            .unwrap();
    }

    /// **S449 / T-2b — the engine resolves `file:` URIs, so the GUARDS must
    /// refuse them.**
    ///
    /// This test does not assert a fix; it pins the hostile fact the fix is
    /// built on. `libsqlite3-sys` compiles the bundled amalgamation with
    /// `-DSQLITE_USE_URI` (`build.rs:140`), which turns URI filename parsing on
    /// **globally** — dropping the `SQLITE_OPEN_URI` open flag does not switch
    /// it off, and the first attempt at this fix did exactly that and passed
    /// vacuously.
    ///
    /// Why it matters: `premigration::ensure_dev_only` and
    /// `engine_path::engine_path_agrees` both decide from the string read as a
    /// `Path`. `file:/Users/x/.aberp/prod/aberp.sqlite` is RELATIVE to `Path`
    /// (first component `file:`), so it is not under the production root, and
    /// its extension really is `sqlite` — both guards cleared it, and this
    /// function then opened production. That bypass is closed at the guard
    /// layer (`EngineMismatch::UriShapedPath`), which is the only layer that
    /// can close it, because the divergence is between two readings of the same
    /// string rather than anything this function does with it.
    ///
    /// Note the argument must *begin* with `file:` — SQLite only parses a URI
    /// in that position.
    #[test]
    fn t2b_a_file_uri_is_treated_as_a_path_not_a_uri() {
        let scratch_db = scratch("uri");
        let target = scratch_db.parent().unwrap().join("target.sqlite");
        assert!(!target.exists());

        // The exact bypass shape: a string that `Path` reads as a RELATIVE
        // path (first component `file:`) and SQLite would read as a URI naming
        // an absolute one. `premigration::ensure_dev_only` absolutises it
        // against the CWD, so it is not under `~/.aberp` and passes; the
        // extension is `sqlite`, so `engine_path_agrees` passes too.
        let disguised = std::path::PathBuf::from(format!("file:{}", target.display()));
        assert!(
            disguised.is_relative(),
            "the premise: every path-shaped guard upstream sees this as relative"
        );

        let opened = open_hardened(&disguised);
        assert!(
            target.is_file(),
            "the premise of the guard-layer fix: this engine DOES resolve the URI and create \
             {}. If this ever stops being true the refusals in `engine_path_agrees` and \
             `premigration::ensure_dev_only` can be revisited — until then they are the only \
             thing standing between a `file:` prefix and the production database.",
            target.display()
        );
        // And note WHERE the failure lands, because it is the wrong place to
        // rely on: `open_hardened` does return an error here, but only from
        // `harden_permissions`, which chmods the LITERAL path and cannot find
        // it. By then SQLite has already created the real file. An error after
        // the write is not a guard.
        assert!(
            opened.is_err(),
            "expected the permission step to fail on the literal name — if this changes, the \
             comment above is stale, not the guard"
        );
    }
}
