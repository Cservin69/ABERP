//! `rebuild-stock-cache` — the recovery binary per ADR-0061 §3.
//!
//! Walks every product in the named tenant's DuckDB file and re-derives
//! `stock_qty` (+ `last_movement_at`) from `SUM(stock_movements.qty_delta)`
//! in one transaction. Idempotent; safe to re-run.
//!
//! Usage:
//!
//! ```text
//! cargo run -p aberp-inventory --bin rebuild-stock-cache -- \
//!     --tenant <tenant_id> --db <path-to-duckdb>
//! ```
//!
//! No flags beyond `--tenant` + `--db`; the binary is intentionally
//! single-purpose. A future operator-friendly wrapper (e.g. as a
//! Tauri-shell command) can call [`aberp_inventory::rebuild_stock_cache_for_tenant`]
//! directly without going through this binary.

use std::path::PathBuf;
use std::process::ExitCode;

use aberp_db::db_writer_lock::DbWriterLockError;
use anyhow::{Context, Result};
use duckdb::Connection;

fn print_usage_and_exit() -> ExitCode {
    eprintln!(
        "rebuild-stock-cache --tenant <tenant_id> --db <path-to-duckdb>\n\
         \n\
         Re-derives products.stock_qty from SUM(stock_movements.qty_delta)\n\
         per ADR-0061 §3. Run when the cache and ledger disagree."
    );
    ExitCode::from(2)
}

fn parse_args() -> Result<(String, PathBuf)> {
    let mut args = std::env::args().skip(1);
    let mut tenant: Option<String> = None;
    let mut db: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--tenant" => {
                tenant = args.next();
            }
            "--db" => {
                db = args.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                anyhow::bail!("help requested");
            }
            other => anyhow::bail!("unknown argument {other:?}"),
        }
    }
    Ok((
        tenant.context("--tenant is required")?,
        db.context("--db is required")?,
    ))
}

fn run() -> Result<u64> {
    let (tenant, db_path) = parse_args()?;

    // ADR-0099 F-E / ADR-0110 D9 — take the whole-DB writer flock BEFORE the
    // DB is opened, and hold it for the rest of the command.
    //
    // This binary is a DOCUMENTED recovery path (ADR-0061 §3: "the recovery is
    // `cargo run -- rebuild-stock-cache`"), which means an operator runs it on a
    // live shop — with `aberp serve` up, holding the tenant's `aberp_db::Handle`
    // and its WAL. The `Connection::open` below carries DuckDB's DEFAULT
    // pragmas, so its CLOSE checkpoints and TRUNCATES that WAL out from under
    // the live writer: every commit serve made since the last checkpoint is
    // gone, while `commit()` keeps returning Ok. That is the exact write-loss
    // primitive ADR-0110 D7's fence exists to detect, and this was the last
    // opener in the tree that could still arm it against a live serve.
    //
    // Named binding, not `let _ =`: `_guard` lives to the end of `run`, whereas
    // `let _` would drop the guard immediately and release the lock before the
    // first read. Declared BEFORE `conn` so the drop order is conn-then-guard —
    // the DB is closed while this process still owns the tenant.
    let _guard =
        aberp_db::db_writer_lock::acquire_or_refuse(&db_path, &tenant, "rebuild-stock-cache")?;

    let mut conn = Connection::open(&db_path)
        .with_context(|| format!("open tenant DuckDB at {}", db_path.display()))?;

    // Idempotent schema-ensure so the binary works against a fresh
    // tenant DB that has products but has not yet booted aberp serve
    // (the boot path is where ensure_schema would ordinarily run).
    aberp_inventory::ensure_schema(&conn).context("ensure inventory schema before rebuild")?;

    let touched = aberp_inventory::rebuild_stock_cache_for_tenant(&mut conn, &tenant)
        .context("rebuild_stock_cache_for_tenant")?;
    Ok(touched)
}

fn main() -> ExitCode {
    match run() {
        Ok(touched) => {
            println!(
                "rebuild-stock-cache: reconciled {} product(s) against their ledger SUM",
                touched
            );
            ExitCode::SUCCESS
        }
        // ADR-0110 D9 — a CONTENDED writer lock is a legitimate refusal, not a
        // mistyped argument. Routing it through `print_usage_and_exit` told the
        // operator "another writer is running" and then dumped the argument
        // synopsis underneath it, which reads as "…and you got the flags wrong".
        // Mid-incident that is the difference between "stop serve and retry" and
        // "re-read the usage line I already typed correctly". Plain message, no
        // synopsis. `{e}` (Display) rather than `{e:?}`: the refusal is one
        // sentence and already says everything, including the `single-writer`
        // rule. The arg-parse path below is untouched — a usage dump is exactly
        // right there.
        Err(e)
            if matches!(
                e.downcast_ref::<DbWriterLockError>(),
                Some(DbWriterLockError::Contended { .. })
            ) =>
        {
            eprintln!("rebuild-stock-cache: {e}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("rebuild-stock-cache: error: {e:?}");
            print_usage_and_exit()
        }
    }
}
