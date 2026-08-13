//! **ADR-0110 D9 — the F-E flock's SHAPE invariants, pinned in source.**
//!
//! # Why a source-shape test and not another runtime test
//!
//! `tools/cut_gate_read_fork.sh`'s `is_flock_fenced()` is a file-level
//! `grep -qE 'acquire_or_refuse|try_acquire'`. It answers "does this file
//! mention the flock", which is not the invariant. The invariant has two more
//! parts, and both are one character away from being silently false:
//!
//! **(a) The guard must be bound to a NAMED local.** `let _guard = …` holds the
//! lock to the end of the enclosing scope. `let _ = …` binds nothing: the guard
//! is a temporary that drops at the end of the *statement*, so the lock is
//! released before the next line runs and the command does its whole
//! open/read/UPDATE/close **unlocked**. The grep sees no difference. Neither
//! does the D9 refusal test in `aberp-inventory`: the acquire still succeeds,
//! still returns `Ok`, and a contended run still refuses — the mutation only
//! shows up in the *uncontended* window it silently re-opens.
//!
//! **(b) The acquire must PRECEDE the first tenant-DB open in its function.**
//! Every "the flock makes this coherent" claim in ADR-0110 §13.2's table, and
//! the whole point of the D9 fix, is *acquire-before-open*: a flock taken after
//! the open has already let a second instance attach to a live tenant DB, and
//! for a default-pragma opener the fold is armed by that open's eventual close
//! regardless of what the lock says afterwards. Moving the call four lines down
//! is invisible to every gate in the tree.
//!
//! So this file scans the source. It is deliberately DISCOVERY-based rather than
//! census-based: it finds every production call site of
//! `db_writer_lock::{acquire_or_refuse, try_acquire}` and checks all of them, so
//! it retroactively hardens the ~18 files that were already flocked, not just
//! `rebuild-stock-cache`. A census would have pinned the one file that prompted
//! it and left the other eighteen exactly as unchecked as they were.
//!
//! Style follows `adr0110_d8_reader_schema_ddl.rs`: read the tree, strip
//! comments (these files explain the flock in prose that quotes the very tokens
//! being matched), fail LOUD if the shape it depends on is gone.
//!
//! # Scope note
//!
//! It matches on the **qualified** `db_writer_lock::` path, which is what keeps
//! `submission_lock::try_acquire` (the per-invoice NAV lock — a different lock,
//! deliberately taken per-row *inside* the loop) and `email_relay`'s rate-limiter
//! `try_acquire` out of the scan. `is_flock_fenced()`'s bare grep does not draw
//! that line; this does.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The production scan scope — mirrors `cut_gate_read_fork.sh`'s `scope_files()`
/// (`apps/*/src`, `modules`, `crates`, minus `/tests/`). `.rs` files only.
fn scoped_sources() -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> =
        vec![root.join("apps"), root.join("modules"), root.join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().unwrap_or_default().to_string_lossy();
                // Skip build output and the test trees (the scope rule), but NOT
                // `src/bin` — `rebuild-stock-cache` lives there.
                if name == "target" || name == "tests" || name == "node_modules" {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// A line is "meaningful" for statement reconstruction if it is neither blank
/// nor a `//` comment.
fn meaningful(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with("//")
}

/// True if `line` closes a statement/block — i.e. the line ABOVE a call line
/// that ends this way cannot be part of the same statement.
fn is_statement_boundary(line: &str) -> bool {
    let t = line.trim_end();
    t.ends_with(';') || t.ends_with('{') || t.ends_with('}')
}

/// Walk backward from `call_idx` to the first line of the statement containing
/// it, so a rustfmt-wrapped `let _guard =\n    acquire_or_refuse(…)` is read as
/// one statement. Stops at a blank line, a comment, or a statement boundary.
fn statement_start(lines: &[&str], call_idx: usize) -> usize {
    let mut cur = call_idx;
    while cur > 0 {
        let prev = lines[cur - 1];
        if !meaningful(prev) || is_statement_boundary(prev) {
            break;
        }
        cur -= 1;
    }
    cur
}

/// True if `line` declares a function (any visibility/asyncness/indentation).
fn is_fn_decl(line: &str) -> bool {
    let t = line.trim_start();
    for prefix in [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "unsafe fn ",
        "pub unsafe fn ",
        "pub(crate) fn ",
        "pub(crate) async fn ",
        "pub(super) fn ",
    ] {
        if t.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// The line range `[start, end)` of the function enclosing `call_idx`.
///
/// Backward to the nearest `fn` declaration — that bound is the load-bearing
/// one, because it is what stops an opener in a PRECEDING sibling function from
/// being mistaken for this function's. Forward to the next `fn` declaration,
/// which can under-shoot on a nested fn; under-shooting only ever drops openers
/// from consideration, so it can weaken this test but never fire it falsely.
fn enclosing_fn(lines: &[&str], call_idx: usize) -> (usize, usize) {
    let mut start = 0;
    for i in (0..=call_idx).rev() {
        if is_fn_decl(lines[i]) {
            start = i;
            break;
        }
    }
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(call_idx + 1) {
        if is_fn_decl(l) {
            end = i;
            break;
        }
    }
    (start, end)
}

/// Openers that attach a DuckDB instance to a tenant DB **file**. The first
/// three are fold-capable (DEFAULT pragmas — their close checkpoints and
/// truncates the WAL); `Handle::open*` is not, but the ordering rule is
/// acquire-before-ANY-open, and `snapshot.rs` states it in exactly those terms
/// ("Acquired BEFORE `open_cli_handle` so a refusal never opens the DB at all").
/// `open_in_memory` is excluded — no file, nothing to fold or contend for.
const OPENER_TOKENS: &[&str] = &[
    "Connection::open(",
    "Ledger::open(",
    "DuckDbBillingStore::open(",
    "Handle::open_default(",
    "Handle::open(",
];

const ACQUIRE_TOKENS: &[&str] = &[
    "db_writer_lock::acquire_or_refuse(",
    "db_writer_lock::try_acquire(",
];

/// The census floor. Discovery that silently finds nothing is a green test that
/// checks nothing — the exact failure mode this file exists to prevent
/// elsewhere. 19 sites across 18 files at D9; the floor is deliberately a floor
/// (adding a flocked command must not red this) but a REMOVAL below it must
/// explain itself.
const MIN_EXPECTED_SITES: usize = 19;

struct Site {
    file: String,
    /// 1-indexed, for humans.
    line: usize,
    idx: usize,
}

fn find_sites() -> Vec<(PathBuf, String, Vec<Site>)> {
    let root = repo_root();
    let mut out = Vec::new();
    for path in scoped_sources() {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !ACQUIRE_TOKENS.iter().any(|t| src.contains(t)) {
            continue;
        }
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace("../", "");
        let lines: Vec<&str> = src.lines().collect();
        let sites: Vec<Site> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| meaningful(l) && ACQUIRE_TOKENS.iter().any(|t| l.contains(t)))
            .map(|(i, _)| Site {
                file: rel.clone(),
                line: i + 1,
                idx: i,
            })
            .collect();
        if !sites.is_empty() {
            out.push((path, src, sites));
        }
    }
    out
}

/// **Invariant (a) — the guard is bound to a NAMED local.**
///
/// Mutation tooth: change any `let _db_writer_lock = …` / `let _guard = …` to
/// `let _ = …` and this goes red. Nothing else in the tree does — not the grep
/// gate, not the runtime refusal tests — while the process runs its entire
/// DB session with the lock already released.
#[test]
fn every_flock_acquire_binds_a_named_guard() {
    let files = find_sites();
    let mut checked = 0usize;

    for (_path, src, sites) in &files {
        let lines: Vec<&str> = src.lines().collect();
        for site in sites {
            checked += 1;
            let start = statement_start(&lines, site.idx);
            let stmt = lines[start].trim_start();

            assert!(
                stmt.starts_with("let "),
                "ADR-0110 D9 REGRESSION ({}:{}): the whole-DB writer flock is acquired without \
                 a `let` binding at all — the guard is a temporary that drops at the end of \
                 this statement, so the lock is released before the next line runs.\n\
                 Statement starts: `{}`",
                site.file,
                site.line,
                stmt
            );

            let after_let = stmt["let ".len()..].trim_start();
            let after_let = after_let
                .strip_prefix("mut ")
                .unwrap_or(after_let)
                .trim_start();
            let ident: String = after_let
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();

            assert!(
                !ident.is_empty() && ident != "_",
                "ADR-0110 D9 REGRESSION ({}:{}): the whole-DB writer flock guard is bound to \
                 `_` (or to nothing), not to a named local.\n\
                 `let _ = guard` does NOT hold the lock: the binding is discarded and the guard \
                 drops at the end of THIS statement, so everything after it — the DB open, the \
                 reads, the UPDATEs, the close — runs UNLOCKED. A second writer (or a live \
                 `aberp serve`) can be co-resident for that entire window, and for a \
                 default-pragma opener the close then folds the live WAL.\n\
                 This is a ONE-CHARACTER regression that `cut_gate_read_fork.sh`'s \
                 `is_flock_fenced()` grep cannot see (the token is still there) and that the \
                 runtime refusal tests cannot see either (a contended acquire still refuses). \
                 Bind it: `let _db_writer_lock = …` / `let _guard = …`.\n\
                 Statement starts: `{}`",
                site.file,
                site.line,
                stmt
            );
        }
    }

    assert!(
        checked >= MIN_EXPECTED_SITES,
        "ADR-0110 D9: this pin found only {checked} flock acquire site(s), below the {MIN_EXPECTED_SITES} \
         known at D9. Either flocked commands were deleted (fine — lower the floor in the same \
         change and say which), or the scan stopped matching (NOT fine — a discovery test that \
         finds nothing passes while checking nothing, which is the failure mode this whole file \
         exists to prevent). Check ACQUIRE_TOKENS against how the call is now written."
    );
}

/// **Invariant (b) — the acquire PRECEDES the first tenant-DB opener in its
/// function.**
///
/// Mutation tooth: move any `acquire_or_refuse` below the `Connection::open` /
/// `Handle::open*` in the same fn and this goes red. Again: the grep gate cannot
/// see it (same file, same token), and a contended run still refuses — but it
/// refuses *after* a second instance has already attached to the live tenant DB.
#[test]
fn every_flock_acquire_precedes_the_first_tenant_db_open_in_its_function() {
    let files = find_sites();
    let mut checked = 0usize;

    for (_path, src, sites) in &files {
        let lines: Vec<&str> = src.lines().collect();
        for site in sites {
            checked += 1;
            let (fn_start, fn_end) = enclosing_fn(&lines, site.idx);

            let first_opener = (fn_start..fn_end).find(|&i| {
                meaningful(lines[i])
                    && OPENER_TOKENS.iter().any(|t| lines[i].contains(t))
                    && !lines[i].contains("open_in_memory")
            });

            let Some(opener_idx) = first_opener else {
                // No opener in this function — e.g. a `run` that delegates the
                // open to a helper. Nothing to order against; (a) still applies.
                continue;
            };

            assert!(
                site.idx < opener_idx,
                "ADR-0110 D9 REGRESSION ({}): the whole-DB writer flock is acquired at line {} \
                 but a tenant-DB opener runs FIRST, at line {}:\n\
                   {}\n\
                 Acquire-before-open is the entire premise of the F-E fencing — every \"the flock \
                 makes this coherent\" row in ADR-0110 §13.2 rests on it. A flock taken AFTER the \
                 open has already allowed a second DuckDB instance to attach to a tenant DB a \
                 live `aberp serve` may be holding; for a default-pragma opener the fold is then \
                 armed by that instance's eventual close no matter what the lock says in between, \
                 and a refusal arrives too late to prevent anything.\n\
                 Move the acquire above the opener.",
                site.file,
                site.line,
                opener_idx + 1,
                lines[opener_idx].trim()
            );
        }
    }

    assert!(
        checked >= MIN_EXPECTED_SITES,
        "ADR-0110 D9: only {checked} flock acquire site(s) found, below the {MIN_EXPECTED_SITES} known \
         at D9 — see the message on `every_flock_acquire_binds_a_named_guard`."
    );
}

/// The discovery itself, asserted — so the two tests above cannot both pass by
/// scanning an empty set, and so a reader can see WHICH files are covered
/// without running a grep.
///
/// It also pins the one site that prompted D9: `rebuild-stock-cache` lives
/// outside `apps/aberp`, and a scan scoped to this package would silently miss
/// it while looking thorough.
#[test]
fn the_flock_scan_covers_the_known_commands_including_the_out_of_package_one() {
    let files = find_sites();
    let found: Vec<String> = files
        .iter()
        .flat_map(|(_, _, sites)| sites.iter().map(|s| s.file.clone()))
        .collect();

    for required in [
        "crates/aberp-inventory/src/bin/rebuild_stock_cache.rs",
        "apps/aberp/src/serve.rs",
        "apps/aberp/src/issue_invoice.rs",
        "apps/aberp/src/drain_submission_queue.rs",
        "apps/aberp/src/export_invoice_bundle.rs",
        "apps/aberp/src/recover_from_nav.rs",
        "apps/aberp/src/mark_abandoned.rs",
        "apps/aberp/src/snapshot.rs",
    ] {
        assert!(
            found.iter().any(|f| f == required),
            "ADR-0110 D9: `{required}` no longer appears in the flock scan. Either it stopped \
             taking the whole-DB writer lock (a live two-writer hazard — for \
             `rebuild_stock_cache.rs` specifically, the WAL-fold hazard D9 closed), or the scan \
             stopped reaching it. Both are red for a reason.\n\
             Files currently found: {found:#?}"
        );
    }
}
