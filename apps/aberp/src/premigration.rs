//! ADR-0108 Step 1 — the **pre-migration snapshot + manifest**, and the
//! verifier that re-derives it.
//!
//! # What this is for
//!
//! ADR-0108's constraint C-IV is *single-command verified rollback*. Verified
//! means the rollback compares the restored database against numbers taken
//! **before** anything moved — otherwise "restored successfully" is a
//! self-report. This module produces those numbers ([`run_snapshot`]) and
//! re-derives them from a live database to compare ([`run_verify`]).
//!
//! # The three things the manifest records that are easy to leave out
//!
//! 1. **`aberp.duckdb.wal` is a first-class artefact** (B3, §2.5). It is *part
//!    of the database's content*, not a temp file. A restore that pairs a
//!    snapshot's main file with whatever `.wal` happens to be on disk does not
//!    fail — DuckDB replays it on the next open and **corrupts** it. So the
//!    snapshot captures main + WAL as an atomic pair and the manifest records
//!    whether a WAL existed, so a restore can refuse rather than guess.
//!
//! 2. **Every `aberp.duckdb.audit.log.*.bak`** — the ADR-0030 preservation
//!    files (`.ahead-*`, `.healed-*`, `.devstale-*`). They are the forensic
//!    record of prior incidents; a snapshot that drops them is a snapshot that
//!    silently discards evidence.
//!
//! 3. **The two tamper-evidence counts** (B1): the number of `audit_ledger`
//!    rows with a non-NULL `event_sig`, and the `audit_ledger_anchors` row
//!    count. These are the two numbers the ADR-0108 §6.3 reconciliation gate
//!    hard-stops on, and §6.2 step 7 re-asserts on the way back. Recording them
//!    here — before any engine code exists — is what makes them a *baseline*
//!    rather than a self-report.
//!
//! # Why not `ChainVerdict::fully_anchored`
//!
//! Because it is `true` when `anchors_pending == 0`, which **includes "there
//! are no anchors at all"** (`chain/verify.rs:188`). The strongest-sounding
//! field in the verdict struct reads its most reassuring value on the most
//! thoroughly gutted input. This module records and compares **counts**, and
//! `verify` requires the baseline's signature and anchor counts to be non-zero
//! before an equality between them means anything — an equality between two
//! zeros is not a check.
//!
//! # Scope
//!
//! DEV-only (C-II). Both entry points refuse a database under the production
//! root, and both refuse while another writer holds the tenant's whole-DB
//! writer lock — a snapshot of a live database is a torn snapshot, and a
//! verification against a live database is a race.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::{PremigrationSnapshotArgs, VerifyAgainstManifestArgs};

/// Manifest schema version. Bumped when a field's meaning changes, so a
/// rollback can refuse an incompatible manifest rather than mis-compare it.
const MANIFEST_VERSION: u32 = 1;

/// The manifest file's name inside the snapshot directory.
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// One captured artefact: name relative to the snapshot dir, size, digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artefact {
    /// File name as it sits beside the DB (and inside the snapshot dir).
    pub name: String,
    /// Byte length at snapshot time.
    pub len: u64,
    /// Lowercase hex SHA-256 of the bytes.
    pub sha256: String,
}

/// The tamper-evidence baseline (B1) plus the chain head. Every field here is
/// a number a later gate compares; none of them is a verdict flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerBaseline {
    /// Highest `audit_ledger.seq`.
    pub head_seq: u64,
    /// Hex `audit_ledger.entry_hash` at `head_seq`.
    pub head_entry_hash: String,
    /// Total `audit_ledger` rows.
    pub entry_count: u64,
    /// `SELECT COUNT(*) FROM audit_ledger WHERE event_sig IS NOT NULL`.
    /// **B1's first hard-stop number.**
    pub signed_entry_count: u64,
    /// `SELECT COUNT(*) FROM audit_ledger_anchors`. **B1's second.**
    pub anchor_count: u64,
    /// Hex `entry_hash` of the mirror's last entry, when a mirror is present.
    pub mirror_tail_entry_hash: Option<String>,
    /// `seq` of the mirror's last entry, when a mirror is present.
    pub mirror_tail_seq: Option<u64>,
}

/// Everything a rollback needs to say PASS or FAIL without trusting anybody.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PremigrationManifest {
    /// See [`MANIFEST_VERSION`].
    pub version: u32,
    /// Tenant the snapshot was taken for.
    pub tenant: String,
    /// The DB file name (not the full path — the snapshot is relocatable).
    pub db_file_name: String,
    /// RFC-3339 UTC timestamp of the snapshot.
    pub taken_at_utc: String,
    /// Main DB + `.wal` + mirror + every `.audit.log.*.bak`, all digested.
    pub artefacts: Vec<Artefact>,
    /// `true` when a **non-empty** `<db>.wal` existed at snapshot time. A
    /// restore must reproduce this exactly: restoring the main file beside a
    /// foreign-generation WAL corrupts it, and deleting a WAL that held
    /// committed-but-unfolded transactions silently discards committed data.
    pub had_unfolded_wal: bool,
    /// Every table in `main`, with its row count.
    pub table_row_counts: Vec<(String, u64)>,
    /// The tamper-evidence baseline.
    pub ledger: LedgerBaseline,
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// C-II. Refuse any database under the production root.
///
/// ADR-0108's execution scope is the DEV tenant only; nothing in §7 may read,
/// write, or stat `~/.aberp/**`. This is the tooling's own guard and is
/// deliberately **engine-independent** — unlike
/// [`aberp_db::engine_path::engine_path_agrees`], whose prod-root arm applies
/// only to a `sqlite-engine` build (a DuckDB build under `~/.aberp/` is
/// ordinary production operation; this *tool* has no business there either
/// way).
fn ensure_dev_only(db: &Path) -> Result<()> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        // No HOME to compare against. Refuse rather than wave through: a tool
        // that touches the audit ledger does not get to fail open (rule 11).
        bail!(
            "cannot resolve $HOME, so the ADR-0108 DEV-only guard (C-II) cannot be evaluated \
             for `{}` — refusing",
            db.display()
        );
    };
    let prod_root = home.join(".aberp");
    // Compare the absolutised path so a relative `../../.aberp/...` cannot
    // slip past a lexical check.
    let abs = if db.is_absolute() {
        db.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolve cwd for the DEV-only guard")?
            .join(db)
    };
    let abs = normalise(&abs);
    if abs.starts_with(&prod_root) {
        bail!(
            "DEV-only violation: `{}` is under the production root `{}` — the ADR-0108 \
             migration tooling is authorised for the DEV tenant only (C-II)",
            abs.display(),
            prod_root.display()
        );
    }
    Ok(())
}

/// Lexically resolve `.` and `..` without touching the filesystem (the DB or
/// snapshot dir may not exist yet). `..` pops, which is what makes the
/// prod-root check above resistant to `dev/../../.aberp/prod/aberp.duckdb`.
fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Reading the baseline
// ---------------------------------------------------------------------------

/// A table identifier is only ever interpolated into SQL after passing this.
///
/// Table names come from `information_schema`, i.e. from runtime data, so they
/// are validated to an ASCII identifier shape and **refused** otherwise rather
/// than quoted-and-hoped. There is no legitimate ABERP table this rejects.
fn safe_identifier(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(anyhow!(
            "refusing to interpolate a non-identifier table name into SQL: {name:?}"
        ))
    }
}

/// Every table in schema `main`, with its row count, ordered by name so two
/// manifests of the same database compare byte-for-byte.
fn read_table_row_counts(conn: &Connection) -> Result<Vec<(String, u64)>> {
    // `information_schema` is DuckDB-side only: this tool never reads the
    // SQLite file, so ADR-0108 §1.1 G-3's "SQLite has no information_schema"
    // does not reach here.
    let mut stmt = conn
        .prepare(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'main' ORDER BY table_name",
        )
        .context("list tables")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let ident = safe_identifier(&name)?;
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM \"{ident}\""), [], |r| {
                r.get(0)
            })
            .with_context(|| format!("count rows in {name}"))?;
        out.push((name, u64::try_from(n).unwrap_or(0)));
    }
    Ok(out)
}

/// Read the tamper-evidence baseline. **Every absence is an error**: a
/// database with no `audit_ledger`, or no `audit_ledger_anchors`, is not a
/// database this exercise can take a baseline of, and returning zeros would
/// manufacture exactly the "equality between two zeros" B1 warns about.
fn read_ledger_baseline(conn: &Connection, mirror_path: &Path) -> Result<LedgerBaseline> {
    let (head_seq, head_hash): (i64, Vec<u8>) = conn
        .query_row(
            "SELECT seq, entry_hash FROM audit_ledger ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context(
            "read the audit_ledger head — a DB with no audit_ledger rows has no \
             tamper-evidence baseline to record (ADR-0108 B1)",
        )?;

    let entry_count: i64 = conn
        .query_row("SELECT count(*) FROM audit_ledger", [], |r| r.get(0))
        .context("count audit_ledger rows")?;

    let signed_entry_count: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit_ledger WHERE event_sig IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .context("count signed audit_ledger rows (B1)")?;

    let anchor_count: i64 = conn
        .query_row("SELECT count(*) FROM audit_ledger_anchors", [], |r| {
            r.get(0)
        })
        .context("count audit_ledger_anchors rows (B1)")?;

    // The mirror is a cross-check arm, never a source (ADR-0108 §6.3, Q7).
    // Recording its tail lets the rollback verify the DB and the mirror still
    // agree; it is not used to reconstruct anything.
    let (mirror_tail_entry_hash, mirror_tail_seq) = if mirror_path.exists() {
        let entries = aberp_audit_ledger::read_mirror_entries(mirror_path)
            .with_context(|| format!("read mirror {}", mirror_path.display()))?;
        match entries.last() {
            Some(e) => (Some(e.entry_hash().to_string()), Some(e.seq())),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    Ok(LedgerBaseline {
        head_seq: u64::try_from(head_seq).unwrap_or(0),
        head_entry_hash: hex::encode(head_hash),
        entry_count: u64::try_from(entry_count).unwrap_or(0),
        signed_entry_count: u64::try_from(signed_entry_count).unwrap_or(0),
        anchor_count: u64::try_from(anchor_count).unwrap_or(0),
        mirror_tail_entry_hash,
        mirror_tail_seq,
    })
}

/// Every artefact that belongs to the database, in a stable order:
/// the main file, its `.wal`, the mirror, and every `<mirror>.*.bak`.
///
/// The `.bak` sweep is a directory listing rather than a fixed list because
/// the preservation files are named with timestamps (`.ahead-*`, `.healed-*`,
/// `.devstale-*`) and nobody knows in advance which exist.
fn artefact_paths(db: &Path) -> Result<Vec<PathBuf>> {
    let mirror = aberp_audit_ledger::mirror_path_for(db);
    let mut out = vec![
        db.to_path_buf(),
        aberp_db::readonly::wal_path_for(db),
        mirror.clone(),
    ];

    let dir = db
        .parent()
        .ok_or_else(|| anyhow!("db path has no parent directory: {}", db.display()))?;
    let mirror_name = mirror
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("mirror path has no file name"))?
        .to_string();
    let mut baks: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("list {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with(&mirror_name) && n.ends_with(".bak"))
        })
        .collect();
    baks.sort();
    out.extend(baks);
    Ok(out)
}

fn digest_file(p: &Path) -> Result<Artefact> {
    let bytes = std::fs::read(p).with_context(|| format!("read {}", p.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(Artefact {
        name: p
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("artefact has no file name: {}", p.display()))?
            .to_string(),
        len: bytes.len() as u64,
        sha256: hex::encode(h.finalize()),
    })
}

// ---------------------------------------------------------------------------
// `aberp premigration-snapshot`
// ---------------------------------------------------------------------------

/// Take the pre-migration snapshot: copy every artefact into a fresh
/// `.aberp-premigration-<ts>/` and write the manifest beside them.
///
/// **All or none.** Everything is assembled inside a `<dir>.partial` staging
/// directory and moved into place with a single `rename` at the end, so an
/// interrupted run leaves a `.partial` (obviously incomplete) rather than a
/// half-populated snapshot dir that looks usable.
pub fn run(args: &PremigrationSnapshotArgs) -> Result<()> {
    run_snapshot(&args.db, &args.tenant, args.out_dir.as_deref()).map(|dir| {
        println!("PASS premigration snapshot → {}", dir.display());
    })
}

/// The library face of [`run`], so tests can drive it without a `clap` parse.
pub fn run_snapshot(db: &Path, tenant: &str, out_dir: Option<&Path>) -> Result<PathBuf> {
    ensure_dev_only(db)?;
    if !db.exists() {
        bail!("no database at {}", db.display());
    }

    // Rule 13 for a one-shot tool: a fresh opener reads Handle-WAL-resident
    // data STALE, so a snapshot taken while `serve` is live is a short
    // snapshot. Refuse — never wait, never force. Held for the whole run.
    let _writer_lock =
        crate::db_writer_lock::acquire_or_refuse(db, tenant, "aberp premigration-snapshot")?;

    // A non-empty WAL means committed-but-unfolded transactions the read-only
    // open cannot see (B3). Snapshotting anyway would record row counts that
    // are short of what the file actually contains — and the manifest is the
    // number a rollback trusts.
    if let Some(len) = aberp_db::readonly::unfolded_wal_len(db)? {
        bail!(
            "refusing to snapshot: {} holds an unfolded WAL of {len} bytes. A read-only open \
             cannot replay it, so the manifest's counts would be silently short. Boot the \
             DuckDB build once to fold it (clean shutdown), then retry.",
            aberp_db::readonly::wal_path_for(db).display()
        );
    }

    let conn = aberp_db::readonly::open_read_only(db)
        .with_context(|| format!("open {} read-only", db.display()))?;
    let table_row_counts = read_table_row_counts(&conn)?;
    let ledger = read_ledger_baseline(&conn, &aberp_audit_ledger::mirror_path_for(db))?;
    drop(conn);

    let now = time::OffsetDateTime::now_utc();
    let taken_at_utc = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("format the snapshot timestamp")?;
    let stamp = now
        .format(time::macros::format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("format the snapshot directory stamp")?;
    let parent = db
        .parent()
        .ok_or_else(|| anyhow!("db path has no parent: {}", db.display()))?;
    let final_dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => parent.join(format!(".aberp-premigration-{stamp}")),
    };
    if final_dir.exists() {
        bail!("snapshot directory already exists: {}", final_dir.display());
    }
    let staging = final_dir.with_extension("partial");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("clear stale staging dir {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;

    let mut artefacts = Vec::new();
    let mut had_unfolded_wal = false;
    for p in artefact_paths(db)? {
        if !p.exists() {
            continue;
        }
        let a = digest_file(&p)?;
        if p == aberp_db::readonly::wal_path_for(db) && a.len > 0 {
            had_unfolded_wal = true;
        }
        std::fs::copy(&p, staging.join(&a.name))
            .with_context(|| format!("copy {} into the snapshot", p.display()))?;
        artefacts.push(a);
    }

    let manifest = PremigrationManifest {
        version: MANIFEST_VERSION,
        tenant: tenant.to_string(),
        db_file_name: db
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("db path has no file name"))?
            .to_string(),
        taken_at_utc,
        artefacts,
        had_unfolded_wal,
        table_row_counts,
        ledger,
    };
    std::fs::write(
        staging.join(MANIFEST_FILENAME),
        serde_json::to_vec_pretty(&manifest).context("serialise manifest")?,
    )
    .context("write manifest")?;

    // The single atomic step. Before it, nothing usable exists; after it,
    // everything does.
    std::fs::rename(&staging, &final_dir).with_context(|| {
        format!(
            "promote staging {} to {}",
            staging.display(),
            final_dir.display()
        )
    })?;
    Ok(final_dir)
}

// ---------------------------------------------------------------------------
// `aberp rollback-restore` — the atomic-set restore (§6.2 steps 4–5)
// ---------------------------------------------------------------------------

/// Restore the DuckDB artefact set from `snapshot_dir` into the directory that
/// holds `db` — **all or none** — and move anything that does not belong aside.
///
/// This is Rust rather than shell on purpose. §6.2's restore has to read the
/// manifest, digest-verify every artefact, and make a WAL-pairing decision; a
/// shell implementation would parse JSON with `grep`/`sed` and make that
/// decision from a string match. CLAUDE.md rule 5: if code can answer, code
/// answers. `run/rollback_to_duckdb.sh` orchestrates and this does the part
/// where being wrong corrupts the database.
///
/// The B3 rules, in order:
///
/// 1. Everything is staged and **digest-verified against the manifest** before
///    anything moves. A snapshot that does not match its own manifest is
///    refused — a partial move is a failed restore, not a corrupted one.
/// 2. The main file and its `.wal` move as one set. **Never the main file
///    alone.**
/// 3. A `.wal` on disk that the manifest did not record belongs to a
///    *different generation* of the file. It is **moved aside**, never deleted
///    (a deleted artefact cannot be post-mortemed) and never left in place
///    (DuckDB would replay it over a restored main file and corrupt it).
pub fn restore_from_snapshot(snapshot_dir: &Path, db: &Path, preserve_dir: &Path) -> Result<()> {
    ensure_dev_only(db)?;
    let manifest_path = snapshot_dir.join(MANIFEST_FILENAME);
    let raw = std::fs::read(&manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;
    let m: PremigrationManifest =
        serde_json::from_slice(&raw).context("parse the pre-migration manifest")?;
    if m.version != MANIFEST_VERSION {
        bail!(
            "manifest schema version {} is not {MANIFEST_VERSION}",
            m.version
        );
    }
    let db_dir = db
        .parent()
        .ok_or_else(|| anyhow!("db path has no parent: {}", db.display()))?;
    let db_name = db
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("db path has no file name"))?;
    if m.db_file_name != db_name {
        bail!(
            "manifest is for `{}` but the restore target is `{db_name}`",
            m.db_file_name
        );
    }

    // --- 1. stage + digest-verify EVERYTHING before anything moves ---
    let staging = preserve_dir.join("restore-staging");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("create staging dir {}", staging.display()))?;

    for a in &m.artefacts {
        let src = snapshot_dir.join(&a.name);
        if !src.is_file() {
            bail!(
                "manifest names `{}` but the snapshot does not contain it — refusing a partial \
                 restore",
                a.name
            );
        }
        let got = digest_file(&src)?;
        if got.sha256 != a.sha256 || got.len != a.len {
            bail!(
                "digest mismatch for `{}` inside {} — refusing to restore a snapshot that does \
                 not match its own manifest",
                a.name,
                snapshot_dir.display()
            );
        }
        std::fs::copy(&src, staging.join(&a.name)).with_context(|| format!("stage {}", a.name))?;
    }

    // --- 2/3. the WAL pairing ---
    let live_wal = aberp_db::readonly::wal_path_for(db);
    let wal_name = live_wal
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("wal path has no file name"))?
        .to_string();
    let snapshot_has_wal = m.artefacts.iter().any(|a| a.name == wal_name);
    if !snapshot_has_wal && live_wal.exists() {
        std::fs::create_dir_all(preserve_dir)?;
        std::fs::rename(
            &live_wal,
            preserve_dir.join(format!("foreign-generation-{wal_name}")),
        )
        .with_context(|| {
            format!(
                "move the foreign-generation WAL {} aside",
                live_wal.display()
            )
        })?;
    }

    // --- the set moves ---
    for a in &m.artefacts {
        std::fs::rename(staging.join(&a.name), db_dir.join(&a.name))
            .with_context(|| format!("place {}", a.name))?;
    }
    let _ = std::fs::remove_dir(&staging);
    Ok(())
}

/// CLI face of [`restore_from_snapshot`].
pub fn run_rollback_restore(args: &crate::cli::RollbackRestoreArgs) -> Result<()> {
    restore_from_snapshot(&args.from, &args.db, &args.preserve_dir)?;
    println!(
        "PASS rollback-restore — atomic set restored from {}",
        args.from.display()
    );
    Ok(())
}

/// Does the on-disk DB still match the digest the manifest recorded for it?
///
/// The **normal** case in ADR-0108 is `true`: C-I says the DuckDB file is
/// byte-unmodified for the whole exercise because the SQLite build never opens
/// it. `false` means something did touch it, and the rollback restores.
pub fn db_matches_manifest(db: &Path, snapshot_dir: &Path) -> Result<bool> {
    let raw = std::fs::read(snapshot_dir.join(MANIFEST_FILENAME))?;
    let m: PremigrationManifest = serde_json::from_slice(&raw)?;
    let want = m
        .artefacts
        .iter()
        .find(|a| Some(a.name.as_str()) == db.file_name().and_then(|s| s.to_str()))
        .ok_or_else(|| anyhow!("manifest records no digest for the DB file itself"))?;
    Ok(digest_file(db)?.sha256 == want.sha256)
}

/// CLI face of [`db_matches_manifest`]. Exit 0 = matches (nothing to restore).
pub fn run_db_matches_manifest(args: &crate::cli::DbMatchesManifestArgs) -> Result<()> {
    if db_matches_manifest(&args.db, &args.from)? {
        println!("MATCH");
        Ok(())
    } else {
        println!("DIFFERS");
        std::process::exit(2);
    }
}

// ---------------------------------------------------------------------------
// `aberp verify-against-manifest`
// ---------------------------------------------------------------------------

/// Re-derive every number in the manifest from `db` and compare.
///
/// This is §6.2 step 7 — the part that makes the rollback *verified*. It is
/// invoked by `run/rollback_to_duckdb.sh` after the restore, and it exits
/// non-zero on any mismatch. It never prints "restored successfully" with a
/// count off.
pub fn run_verify_cmd(args: &VerifyAgainstManifestArgs) -> Result<()> {
    let report = run_verify(&args.db, &args.tenant, &args.manifest)?;
    for line in &report.checks {
        println!("  {line}");
    }
    if report.mismatches.is_empty() {
        println!(
            "PASS verify-against-manifest ({} checks)",
            report.checks.len()
        );
        Ok(())
    } else {
        for m in &report.mismatches {
            eprintln!("  ✗ {m}");
        }
        bail!(
            "FAIL verify-against-manifest: {} mismatch(es) against {}",
            report.mismatches.len(),
            args.manifest.display()
        )
    }
}

/// Outcome of a verification run.
#[derive(Debug, Default)]
pub struct VerifyReport {
    /// One line per check that passed.
    pub checks: Vec<String>,
    /// One line per check that failed. Non-empty ⇒ the run failed.
    pub mismatches: Vec<String>,
}

/// The library face of [`run_verify_cmd`].
pub fn run_verify(db: &Path, tenant: &str, manifest_path: &Path) -> Result<VerifyReport> {
    ensure_dev_only(db)?;
    let raw = std::fs::read(manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;
    let m: PremigrationManifest =
        serde_json::from_slice(&raw).context("parse the pre-migration manifest")?;
    if m.version != MANIFEST_VERSION {
        bail!(
            "manifest schema version {} is not {MANIFEST_VERSION} — refusing to compare \
             fields whose meaning may have changed",
            m.version
        );
    }
    if m.tenant != tenant {
        bail!(
            "manifest was taken for tenant `{}` but this run is for `{tenant}`",
            m.tenant
        );
    }

    let _writer_lock =
        crate::db_writer_lock::acquire_or_refuse(db, tenant, "aberp verify-against-manifest")?;
    if let Some(len) = aberp_db::readonly::unfolded_wal_len(db)? {
        bail!(
            "refusing to verify: {} holds an unfolded WAL of {len} bytes, so a read-only open \
             cannot see all committed data",
            aberp_db::readonly::wal_path_for(db).display()
        );
    }

    let conn = aberp_db::readonly::open_read_only(db)
        .with_context(|| format!("open {} read-only", db.display()))?;
    let counts = read_table_row_counts(&conn)?;
    let ledger = read_ledger_baseline(&conn, &aberp_audit_ledger::mirror_path_for(db))?;
    drop(conn);

    let mut r = VerifyReport::default();
    let eq = |label: &str, want: String, got: String, r: &mut VerifyReport| {
        if want == got {
            r.checks.push(format!("✓ {label} = {got}"));
        } else {
            r.mismatches
                .push(format!("{label}: manifest {want}, live {got}"));
        }
    };

    // --- B1: the two tamper-evidence counts, and the non-zero precondition ---
    //
    // An equality between two zeros is not a check. If the BASELINE recorded
    // zero signatures or zero anchors there is nothing to protect and saying so
    // is more honest than reporting a green equality.
    if m.ledger.signed_entry_count == 0 {
        r.mismatches.push(
            "baseline recorded 0 signed audit_ledger rows — there is no tamper-evidence \
             coverage to verify (ADR-0108 B1: an equality between two zeros is not a check)"
                .to_string(),
        );
    }
    if m.ledger.anchor_count == 0 {
        r.mismatches.push(
            "baseline recorded 0 audit_ledger_anchors rows — there is no anchor coverage to \
             verify (ADR-0108 B1)"
                .to_string(),
        );
    }
    eq(
        "audit_ledger rows with a non-NULL event_sig (B1)",
        m.ledger.signed_entry_count.to_string(),
        ledger.signed_entry_count.to_string(),
        &mut r,
    );
    eq(
        "audit_ledger_anchors row count (B1)",
        m.ledger.anchor_count.to_string(),
        ledger.anchor_count.to_string(),
        &mut r,
    );

    // --- chain head + mirror agreement ---
    eq(
        "audit_ledger head seq",
        m.ledger.head_seq.to_string(),
        ledger.head_seq.to_string(),
        &mut r,
    );
    eq(
        "audit_ledger head entry_hash",
        m.ledger.head_entry_hash.clone(),
        ledger.head_entry_hash.clone(),
        &mut r,
    );
    eq(
        "audit_ledger row count",
        m.ledger.entry_count.to_string(),
        ledger.entry_count.to_string(),
        &mut r,
    );
    eq(
        "mirror tail entry_hash",
        format!("{:?}", m.ledger.mirror_tail_entry_hash),
        format!("{:?}", ledger.mirror_tail_entry_hash),
        &mut r,
    );

    // --- per-table row counts ---
    let live: std::collections::BTreeMap<&str, u64> =
        counts.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    for (name, want) in &m.table_row_counts {
        match live.get(name.as_str()) {
            Some(got) => eq(
                &format!("rows in {name}"),
                want.to_string(),
                got.to_string(),
                &mut r,
            ),
            None => r.mismatches.push(format!(
                "table {name} is in the manifest but not in the live DB"
            )),
        }
    }
    for (name, got) in &counts {
        if !m.table_row_counts.iter().any(|(n, _)| n == name) {
            r.mismatches.push(format!(
                "table {name} ({got} rows) exists in the live DB but not in the manifest"
            ));
        }
    }

    // --- the WAL pairing (B3) ---
    let live_wal = aberp_db::readonly::unfolded_wal_len(db)?.is_some();
    eq(
        "unfolded WAL present",
        m.had_unfolded_wal.to_string(),
        live_wal.to_string(),
        &mut r,
    );

    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_identifier_accepts_real_table_names_and_refuses_injection() {
        for good in ["audit_ledger", "invoice_line", "_x", "t9"] {
            safe_identifier(good).unwrap();
        }
        for bad in [
            "",
            "9t",
            "audit_ledger; DROP TABLE x",
            "a-b",
            "\"q\"",
            "a b",
        ] {
            assert!(
                safe_identifier(bad).is_err(),
                "must refuse a non-identifier table name: {bad:?}"
            );
        }
    }

    #[test]
    fn normalise_pops_parent_components() {
        assert_eq!(
            normalise(Path::new("/a/dev/../../.aberp/prod/aberp.duckdb")),
            Path::new("/.aberp/prod/aberp.duckdb")
        );
    }

    /// C-II must survive a traversal: a path that *lexically* looks like it is
    /// outside `~/.aberp` but resolves inside it is still refused.
    #[test]
    fn dev_only_guard_refuses_a_traversal_into_the_production_root() {
        let home = std::env::var("HOME").expect("HOME is set in the test environment");
        let sneaky = PathBuf::from(&home)
            .join("dev")
            .join("..")
            .join(".aberp")
            .join("prod")
            .join("aberp.duckdb");
        let err = ensure_dev_only(&sneaky).expect_err("a traversal into ~/.aberp must be refused");
        assert!(
            err.to_string().contains("DEV-only violation"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn dev_only_guard_allows_a_dev_path() {
        ensure_dev_only(Path::new("/tmp/aberp-dev/apps/aberp-ui/aberp.duckdb"))
            .expect("a dev path is fine");
    }
}
