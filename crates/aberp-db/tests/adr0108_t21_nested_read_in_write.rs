//! ADR-0108 **T-21** — a nested `read()`-inside-`write()` never reaches the
//! storage engine.
//!
//! WHY T-21 HAD TO BE REWRITTEN (finding R-3,
//! `docs/findings/read-fork-audit-sqlite-20260731.md`). The ADR specified T-21
//! as "a nested `read()`-inside-`write()` **aborts loudly** rather than waiting
//! out `busy_timeout`". That sentence describes a race between the Rust mutex
//! and SQLite's busy handler — and the race depends on an implementation choice
//! in `Handle::read()`'s SQLite arm that, when the ADR was written, nobody had
//! made:
//!
//! * keep `lock_recovering()` → the nested case deadlocks on the **Rust**
//!   mutex; SQLite is never asked for a lock, so `busy_timeout` cannot occur
//!   and there is no race to assert;
//! * drop it → the nested case becomes **legal**, the tripwire's premise
//!   disappears with it, and the "loud abort" T-21 demands must be deliberately
//!   added — it is not something anyone gets for free.
//!
//! Either way the test as specified was unwritable. **The choice is now made and
//! recorded on `Handle::read()`'s doc-comment: the SQLite arm KEEPS
//! `lock_recovering()`.** T-21 is therefore the pin on what that choice
//! produces — which is the same shape as today, and therefore not a regression:
//!
//! > A `read()` issued while this thread holds the write guard resolves against
//! > the Rust `Mutex`, not against the engine. It panics on the re-entrancy
//! > tripwire in debug; it never reaches DuckDB or SQLite; `busy_timeout` is
//! > never involved.
//!
//! Two arms, because a behavioural pin alone cannot see the decision being
//! reversed. `assert_not_reentrant` panics BEFORE the lock is taken, so a Step-5
//! SQLite arm that dropped `lock_recovering()` while keeping the tripwire would
//! leave the behavioural arm green and the invariant gone. The structural arm is
//! what actually holds R-3.
//!
//! Both arms compile and run under the default build **and** under
//! `--features sqlite-engine`.

use std::path::{Path, PathBuf};

use aberp_audit_ledger::{ensure_schema, TenantId};
use aberp_db::Handle;
use duckdb::Connection;

struct Tmp(PathBuf);
impl Tmp {
    fn new(label: &str) -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("aberp-t21-{label}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
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

fn seed(db: &Path) {
    let conn = Connection::open(db).unwrap();
    ensure_schema(&conn).unwrap();
    conn.execute_batch("CHECKPOINT;").unwrap();
}

/// T-21, behavioural arm. The nested acquire is refused by the Rust mutex's
/// tripwire — loudly, deterministically, before any engine lock is requested.
///
/// `debug_assertions`-gated because the tripwire is: in a release build the
/// nested acquire is a genuine deadlock, and a test that deadlocks does not
/// fail, it hangs the suite forever.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "RE-ENTRANCY TRIPWIRE")]
fn t21_a_nested_read_inside_write_never_reaches_the_engine() {
    let tmp = Tmp::new("nested");
    seed(&tmp.db());
    let handle = Handle::open_default(&tmp.db(), TenantId::new("prod".to_string()).unwrap())
        .expect("open handle");

    let _guard = handle.write().expect("acquire writer");
    // The nested read. Under BOTH engine arms this must resolve against the
    // Rust mutex, never against the engine's lock manager — so it panics here
    // rather than waiting out a `busy_timeout` and returning SQLITE_BUSY.
    let _nested = handle.read();
    unreachable!("`read()` inside a live `write()` must not return");
}

/// T-21, structural arm — **this is the one that holds R-3.**
///
/// `Handle::read()` must take the writer mutex via `lock_recovering()`, and no
/// engine-gated arm inside it may return before doing so. A Step-5 SQLite arm
/// that opened a fresh connection directly would satisfy every behavioural test
/// in this file (the tripwire panics before the lock either way) while silently
/// deleting the invariant the ADR now depends on.
#[test]
fn t21_the_sqlite_arm_of_read_still_takes_the_writer_mutex() {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("read aberp-db/src/lib.rs");

    let body = read_fn_body(&src).expect(
        "could not locate `pub fn read(&self) -> Result<Connection, DbError>` in \
         aberp-db/src/lib.rs — this test has stranded and is FAIL-OPEN, not green",
    );

    assert!(
        body.contains("lock_recovering()"),
        "`Handle::read()` no longer calls `lock_recovering()`.\n\n\
         ADR-0108 R-3 decided that the SQLite arm KEEPS the writer mutex, so that \
         a nested `read()`-inside-`write()` resolves against the Rust mutex and \
         NEVER reaches SQLite's busy handler. Dropping it makes the nested case \
         legal, deletes the re-entrancy tripwire's premise, and converts rule 13's \
         loud self-deadlock into a timed hang followed by SQLITE_BUSY.\n\n\
         If that is genuinely wanted, it is a reopening of R-3 — update \
         `Handle::read()`'s doc-comment and ADR-0108 §8 T-21 first, and add the \
         explicit loud abort the mutex used to provide for free. Do not just \
         delete this assertion.\n\nBody scanned:\n{body}"
    );

    // The subtler regression: an early `#[cfg(feature = "sqlite-engine")]` arm
    // that opens and returns before the surviving `lock_recovering()` line.
    let lock_at = body.find("lock_recovering()").expect("checked above");
    let prefix = &body[..lock_at];
    assert!(
        !prefix.contains("return") && !prefix.contains("sqlite-engine"),
        "`Handle::read()` has an engine-gated arm BEFORE `lock_recovering()`.\n\n\
         R-3 is not satisfied by the mutex merely appearing somewhere in the \
         function — every arm must take it. Prefix scanned:\n{prefix}"
    );
}

/// Extract the body of `pub fn read(&self) -> Result<Connection, DbError>` by
/// brace matching. Returns `None` if the signature is not found, which the
/// caller treats as a stranded (fail-open) scan rather than a pass.
fn read_fn_body(src: &str) -> Option<String> {
    let sig = "pub fn read(&self) -> Result<Connection, DbError> {";
    let start = src.find(sig)? + sig.len();
    let mut depth = 1usize;
    for (i, c) in src[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(src[start..start + i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The extractor itself must be able to fail — otherwise `read_fn_body`
/// returning `Some("")` on a renamed signature would make the structural arm
/// pass vacuously.
#[test]
fn t21_the_structural_extractor_is_not_fail_open() {
    assert!(
        read_fn_body("fn something_else() { let x = 1; }").is_none(),
        "the body extractor matched a function that is not `Handle::read()`"
    );
    let fixture = "pub fn read(&self) -> Result<Connection, DbError> { \
                   let mut inner = self.lock_recovering()?; }";
    assert_eq!(
        read_fn_body(fixture).as_deref().map(str::trim),
        Some("let mut inner = self.lock_recovering()?;"),
        "the body extractor did not return the function body it claims to"
    );
}
