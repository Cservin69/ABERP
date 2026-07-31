//! ADR-0108 R-1 — **no DDL on a `Handle::read()` connection**, in the
//! `cargo test` loop.
//!
//! THE DEFECT THIS PINS (`docs/findings/read-fork-audit-sqlite-20260731.md`,
//! finding R-1). `apps/aberp/src/incoming_invoices.rs` called `ensure_schema`
//! — `CREATE TABLE IF NOT EXISTS` + the family's `ADD COLUMN` ladder — on the
//! connection returned by `Handle::read()`, on all three of its read paths.
//!
//! Under DuckDB that is nearly invisible: `read()` is a `try_clone` of the ONE
//! instance, so the DDL lands in the Handle's own WAL. The only anomaly is that
//! it escapes the writer `Mutex` — the guard is released the instant the clone
//! is taken — so "every write is serialized by one mutex" was already, quietly,
//! not quite true.
//!
//! Under ADR-0108's `sqlite-engine` arm it is a genuine second writer:
//! `read()` becomes a real connection, `ensure_schema` takes SQLite's write
//! lock OUTSIDE the writer `Mutex`, concurrently with the `Handle`'s writer.
//! §2.4's single-writer invariant becomes false, and every AP-invoice list/get
//! request contends for the write lock — waiting out `busy_timeout` (5 s) on a
//! route that is a pure read today.
//!
//! So this is a pin on the CLASS, not on the three lines: a scope-aware scan of
//! every `.rs` file under `apps/`, `crates/` and `modules/` for a schema-
//! establishing call, an `execute_batch`, or a `CREATE`/`ALTER`/`DROP`/
//! `TRUNCATE` applied to a connection that came from `read()`.
//!
//! The scanner is MULTI-LINE-AWARE. rustfmt writes these chains as
//! `state\n    .db\n    .read()`, and a line-local pattern cannot see them —
//! the exact defect PR #43 (D1a) found in the read-fork scanner and PR #52's
//! finding R-2 found in ADR-0108's census. Do not "simplify" it back to a grep.
//!
//! `detector_detects_the_shape_it_claims_to_detect` is the anti-fail-open arm:
//! this repo has shipped three scanners that went green because their lexer
//! stranded, so the detector is run against a fixture that MUST trip it.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// One flagged site: `(file, line, the offending statement)`.
type Hit = (String, usize, String);

/// A source line joined with the rustfmt continuations that follow it, tagged
/// with the ORIGINAL 1-based line number of the statement's first line.
struct Joined {
    line_no: usize,
    text: String,
}

/// Strip `//`-comments, then fold a rustfmt-wrapped method chain back onto the
/// line that starts it. Line numbers stay anchored to the first line, which is
/// what a reader needs in the failure message.
fn join_chains(source: &str) -> Vec<Joined> {
    let mut out: Vec<Joined> = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        // Blank out whole-line comments (incl. `///` doc comments) so prose
        // that spells `db.read()` — `aberp-db`'s crate docs do — is not a site.
        let stripped = if raw.trim_start().starts_with("//") {
            ""
        } else {
            raw
        };
        if stripped.trim_start().starts_with('.') {
            if let Some(prev) = out.last_mut() {
                prev.text.push_str(stripped.trim_start());
                continue;
            }
        }
        out.push(Joined {
            line_no: idx + 1,
            text: stripped.to_string(),
        });
    }
    out
}

/// Does this joined line bind a connection from `Handle::read()`? Returns the
/// binding's identifier.
fn read_binding(text: &str) -> Option<String> {
    let after_let = text.trim_start().strip_prefix("let ")?;
    let after_let = after_let.strip_prefix("mut ").unwrap_or(after_let);
    let name: String = after_let
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    let rest = &after_let[name.len()..];
    // `= <receiver>.read()` — receiver irrelevant, but a bare `RwLock` read is
    // excluded by requiring the `?`/`.context`/`.expect`/`.unwrap` shape a
    // `Handle::read()` (which returns `Result<Connection>`) always carries.
    let eq = rest.find('=')?;
    let rhs = &rest[eq + 1..];
    if !rhs.contains(".read()") {
        return None;
    }
    Some(name)
}

/// Is this joined line a DDL statement applied to `var`?
fn ddl_on(var: &str, text: &str) -> bool {
    let t = text.trim();
    // `ensure_schema(&conn)` / `ensure_columns(&conn, …)`, however qualified.
    for f in ["ensure_schema", "ensure_columns"] {
        if let Some(at) = t.find(f) {
            let before_is_ident_char = t[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            if !before_is_ident_char {
                let args = t[at + f.len()..].trim_start();
                let inner = args
                    .strip_prefix('(')
                    .map(|a| a.trim_start().trim_start_matches('&').trim_start());
                if let Some(inner) = inner {
                    let arg: String = inner
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if arg == var {
                        return true;
                    }
                }
            }
        }
    }
    if t.contains(&format!("{var}.execute_batch(")) {
        return true;
    }
    if let Some(at) = t.find(&format!("{var}.execute(")) {
        let tail = t[at..].to_ascii_uppercase();
        for kw in ["\"CREATE", "\"ALTER", "\"DROP", "\"TRUNCATE"] {
            if tail.contains(kw) {
                return true;
            }
        }
    }
    false
}

/// Scan one file's source. For every `read()` binding, walk forward to the end
/// of its lexical scope (brace depth relative to the binding line) and flag any
/// DDL applied to it.
fn scan(path: &str, source: &str) -> Vec<Hit> {
    let joined = join_chains(source);
    let mut hits = Vec::new();
    for (i, j) in joined.iter().enumerate() {
        let Some(var) = read_binding(&j.text) else {
            continue;
        };
        let mut depth: i32 = 0;
        for k in i..joined.len() {
            if k > i && ddl_on(&var, &joined[k].text) {
                hits.push((
                    path.to_string(),
                    joined[k].line_no,
                    joined[k].text.trim().to_string(),
                ));
            }
            depth += joined[k].text.matches('{').count() as i32;
            depth -= joined[k].text.matches('}').count() as i32;
            if depth < 0 {
                break;
            }
        }
    }
    hits
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target" || n == ".git") {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// THE PIN. Zero sites, tree-wide.
#[test]
fn no_ddl_is_issued_on_a_handle_read_connection() {
    let root = repo_root();
    let mut files = Vec::new();
    for sub in ["apps", "crates", "modules"] {
        rust_files(&root.join(sub), &mut files);
    }
    assert!(
        files.len() > 200,
        "the file walk found only {} .rs files — the scan stranded, which is a \
         FAIL-OPEN scanner, not a green result",
        files.len()
    );

    // This file's own fixtures (`detector_detects_the_shape_it_claims_to_detect`)
    // are deliberately the offending shape, so scanning it would always red.
    // `file!()` rather than a hard-coded name so a rename cannot silently
    // re-introduce the self-hit.
    let self_path = file!().replace('\\', "/");
    let self_name = self_path.rsplit('/').next().unwrap_or(&self_path);

    let mut hits: Vec<Hit> = Vec::new();
    for f in &files {
        if f.file_name().is_some_and(|n| n == self_name) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();
        hits.extend(scan(&rel, &src));
    }

    assert!(
        hits.is_empty(),
        "DDL is being issued on a `Handle::read()` connection at {} site(s):\n{}\n\n\
         A `read()` connection may not run `CREATE`/`ALTER`/`DROP` or any \
         `ensure_schema`/`ensure_columns`. Under DuckDB this escapes the writer \
         mutex; under ADR-0108's `sqlite-engine` arm it is a SECOND WRITER \
         outside that mutex, which falsifies §2.4's single-writer invariant and \
         puts a 5-second `busy_timeout` on a read route.\n\
         The fix is one of: hoist the schema establishment to the family's \
         boot/first-write path (what `incoming_invoices` does since R-1), or \
         take a `Handle::write()` for it. Do NOT relax this test.\n\
         (finding R-1, docs/findings/read-fork-audit-sqlite-20260731.md)",
        hits.len(),
        hits.iter()
            .map(|(f, l, s)| format!("  {f}:{l}\n      {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// ANTI-FAIL-OPEN. Three scanners in this repo have gone green because their
/// lexer stranded (the `cfg`-test fail-open, the opener census's char-literal
/// hole, the keychain twin). A detector that cannot demonstrate a hit is not
/// evidence of absence, so this runs it against the exact shapes it exists to
/// catch — including the rustfmt-wrapped one a line-local grep cannot see.
#[test]
fn detector_detects_the_shape_it_claims_to_detect() {
    let inline = r#"
fn list_incoming(db: &aberp_db::Handle) -> Result<()> {
    let conn = db.read().context("acquire shared reader")?;
    ensure_schema(&conn).context("ensure ap_invoice schema (list)")?;
    Ok(())
}
"#;
    assert_eq!(
        scan("fixture.rs", inline).len(),
        1,
        "the single-line `ensure_schema(&conn)`-on-a-`read()` shape was NOT \
         detected — the detector is fail-open"
    );

    let wrapped = r#"
fn get_incoming(state: &AppState) -> Result<()> {
    let conn = state
        .db
        .read()
        .context("acquire shared reader")?;
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}
"#;
    assert_eq!(
        scan("fixture.rs", wrapped).len(),
        1,
        "the rustfmt-WRAPPED `state\\n.db\\n.read()` shape was NOT detected — \
         this is the PR #43 (D1a) line-local defect all over again"
    );

    let qualified = r#"
fn f(db: &aberp_db::Handle) -> Result<()> {
    let c = db.read()?;
    crate::partners::ensure_schema(&c)?;
    Ok(())
}
"#;
    assert_eq!(
        scan("fixture.rs", qualified).len(),
        1,
        "a PATH-QUALIFIED `ensure_schema` on a read connection was NOT detected"
    );

    // And the shapes that must NOT trip it: DDL on a WRITE guard, and DDL on a
    // read binding whose scope has already closed.
    let on_write = r#"
fn ingest(db: &aberp_db::Handle) -> Result<()> {
    let mut guard = db.write()?;
    ensure_schema(&guard)?;
    Ok(())
}
"#;
    assert!(
        scan("fixture.rs", on_write).is_empty(),
        "DDL on a `write()` guard is the CORRECT shape and must not be flagged"
    );

    let out_of_scope = r#"
fn a(db: &aberp_db::Handle) -> Result<()> {
    let conn = db.read()?;
    Ok(())
}
fn b(db: &aberp_db::Handle) -> Result<()> {
    let mut conn = db.write()?;
    ensure_schema(&conn)?;
    Ok(())
}
"#;
    assert!(
        scan("fixture.rs", out_of_scope).is_empty(),
        "a same-named binding in a LATER function must not be attributed to an \
         earlier `read()` — the scope walk is broken"
    );

    let in_a_doc_comment = r#"
/// let conn = db.read()?;
/// ensure_schema(&conn)?;
fn documented() {}
"#;
    assert!(
        scan("fixture.rs", in_a_doc_comment).is_empty(),
        "prose in a doc comment is not a call site"
    );
}
