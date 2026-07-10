//! PR-17 / ADR-0030 — audit-ledger mirror file (`<db>.audit.log`).
//!
//! The mirror is a per-tenant on-disk JSON-Lines artifact that
//! shadows the DuckDB `audit_ledger` table. Per ADR-0008
//! §"Storage", "the ledger is also mirrored to an append-only file
//! outside the DB on every commit, fsync'd." PR-17 realises that
//! sentence; ADR-0030 decides the format, the write-time hook
//! location, the recovery posture on partial writes, and the
//! read-time surface the bundle reader consumes.
//!
//! # Concepts
//!
//! - **Path convention** (`mirror_path_for`) — `<db_path>.audit.log`.
//!   ADR-0008 §"Storage" named `<tenant>.audit.log`; the literal-
//!   suffix convention here is operationally identical because
//!   ADR-0002 names one DB file per tenant, and avoids a separate
//!   path-resolution surface.
//! - **Write-time hook** (`sync_mirror`) — invoked by the binary
//!   path AFTER `tx.commit()`. Reads the mirror's last line,
//!   verifies it against the DB's matching entry, reads DB entries
//!   with `seq > mirror_head`, appends each as one JSON-Lines line,
//!   fsyncs. Per ADR-0030 §2, the mirror reflects committed state
//!   only — running the hook pre-commit would create permanent
//!   divergence on a rollback.
//! - **Recovery on partial writes** — fail loud (per ADR-0030 §3 +
//!   CLAUDE.md rule 12). Three new `AppendError` variants:
//!   `MirrorCorrupt` (last line not newline-terminated, or non-
//!   ascending/duplicate seqs, or JSON decode failure),
//!   `MirrorDivergent` (mirror's `entry_hash[seq]` disagrees with
//!   DB's), `MirrorIo` (filesystem error). The DB-committed entry
//!   is NOT rolled back.
//! - **Bootstrap** (`sync_mirror` when mirror file is absent) —
//!   implicit one-time backfill from the DB. INFO-level log line
//!   `audit_mirror_initialized` names the event loud per ADR-0030
//!   §7 + CLAUDE.md rule 12.
//! - **Read-time surface** (`read_mirror_entries`) — used by the
//!   per-invoice export bundle reader at
//!   `apps/aberp/src/export_invoice_bundle.rs`. Returns the
//!   seq-ordered vector of `MirrorEntry`; the bundle reader
//!   compares against DB entries at the `entry_hash` level.
//!
//! # Per-tenant lock posture (ADR-0030 §6)
//!
//! The DuckDB single-writer file-lock blocks concurrent DB commits;
//! the mirror's `fs2::FileExt::lock_exclusive` blocks concurrent
//! mirror appends. Two ABERP processes that both committed a DB
//! entry serialize on the mirror lock; the second process's
//! `sync_mirror` call sees the first process's append in the file
//! and skips ahead. Cloud multi-writer per ADR-0016 is deferred
//! unchanged.
//!
//! # What this module does and does not do
//!
//! - It DOES NOT couple to `append_in_tx` — the mirror write runs
//!   post-commit at the binary path per ADR-0030 §2 "Surfaced
//!   conflict 1, Reading B".
//! - It DOES NOT define new `EventKind` variants — the mirror
//!   records the same kinds the DB records; F12 four-edit ritual
//!   does NOT fire.
//! - It DOES NOT sign the mirror — F5 attestation signing remains
//!   deferred; the mirror's value is "best-effort secondary
//!   evidence" per ADR-0008 §"Adversarial review" bullet 1.
//! - It DOES NOT auto-sync on read paths — only the binary's post-
//!   commit code path calls `sync_mirror`. The bundle reader uses
//!   `read_mirror_entries` (pure read) and never mutates the
//!   mirror.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use duckdb::Connection;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::entry::{Actor, BinaryHash, Entry, EntryHash, EntryId, EventKind, Sequence, TenantId};
use crate::error::AppendError;
use crate::storage::LedgerMeta;

/// The literal filename suffix appended to the DB path to produce
/// the mirror path. Inlined here rather than threaded through a
/// `const PATH_SUFFIX` indirection per CLAUDE.md rule 2 — the
/// suffix never changes.
const MIRROR_PATH_SUFFIX: &str = ".audit.log";

/// Resolve the mirror file path for a given DB file path. The
/// suffix is appended to the full file name (not the
/// extension-only suffix) so `t-1.duckdb` becomes
/// `t-1.duckdb.audit.log` per ADR-0030 §1.
pub fn mirror_path_for(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push(MIRROR_PATH_SUFFIX);
    PathBuf::from(s)
}

/// One JSON-Lines record in the mirror file. Public so the bundle
/// reader can compare against DB-sourced [`Entry`] values at the
/// `entry_hash` level (which is the canonical agreement key per
/// ADR-0030 §4).
///
/// Field shape MUST match the bundle's `chain.jsonl` line shape
/// (PR-16's `ChainJsonlEntry`) so the bundle reader's mirror-file
/// consumption path is SYMMETRIC with the DB-sourced consumption
/// path per ADR-0030 §1 + CLAUDE.md rule 7 (one canonical format,
/// two consumers).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MirrorEntry {
    pub id: String,
    pub seq: u64,
    /// Hex-encoded 32-byte SHA-256.
    pub prev_hash: String,
    pub time_wall: String,
    pub time_mono: u64,
    pub actor: Actor,
    /// Hex-encoded 32-byte SHA-256 of the producing binary.
    pub binary_hash: String,
    pub tenant_id: String,
    pub kind: String,
    /// Base64-encoded payload bytes.
    pub payload: String,
    pub idempotency_key: Option<String>,
    /// Hex-encoded 32-byte SHA-256 (the chain link).
    pub entry_hash: String,
}

impl MirrorEntry {
    /// Encode an in-memory [`Entry`] into the JSON-Lines record
    /// shape. Public-crate so [`crate::storage`] and tests can
    /// reuse it.
    pub(crate) fn from_entry(entry: &Entry) -> Result<Self, AppendError> {
        let time_wall = entry.time_wall.format(&Rfc3339)?;
        Ok(Self {
            id: entry.id.to_prefixed_string(),
            seq: entry.seq.as_u64(),
            prev_hash: hex::encode(entry.prev_hash.as_bytes()),
            time_wall,
            time_mono: entry.time_mono,
            actor: entry.actor.clone(),
            binary_hash: hex::encode(entry.binary_hash.as_bytes()),
            tenant_id: entry.tenant_id.as_str().to_string(),
            kind: entry.kind.as_str().to_string(),
            payload: BASE64_STANDARD.encode(&entry.payload),
            idempotency_key: entry.idempotency_key.clone(),
            entry_hash: hex::encode(entry.entry_hash.as_bytes()),
        })
    }

    /// Inverse of [`MirrorEntry::from_entry`]: decode this JSON-Lines record
    /// back into a fully-formed in-memory [`Entry`], byte-exact. Hex-decodes the
    /// three 32-byte hash fields, base64-decodes `payload`, RFC3339-parses
    /// `time_wall`, and rebuilds `EntryId` / `Sequence` / `TenantId` /
    /// `EventKind`. Total and lossless: for any entry `e`,
    /// `MirrorEntry::from_entry(&e).to_entry()` reproduces `e`, and the decoded
    /// entry's [`crate::compute_entry_hash`] reproduces the stored `entry_hash`
    /// — the property the boot heal's full `verify_chain` (MF-1) relies on.
    ///
    /// A decode failure means the mirror line is malformed; it surfaces as
    /// [`AppendError::MirrorCorrupt`] naming the seq + field, matching the
    /// loud-fail posture the reconcile already takes on a corrupt mirror.
    pub(crate) fn to_entry(&self) -> Result<Entry, AppendError> {
        let corrupt = |field: &str, detail: String| AppendError::MirrorCorrupt {
            reason: format!("mirror row seq {}: {field} {detail}", self.seq),
        };

        let id_ulid_str = self
            .id
            .strip_prefix("aud_")
            .ok_or_else(|| corrupt("id", "missing `aud_` prefix".to_string()))?;
        let id_ulid = ulid::Ulid::from_string(id_ulid_str)
            .map_err(|e| corrupt("id", format!("is not a valid Crockford-base32 ULID: {e}")))?;

        let seq = Sequence::new(self.seq)
            .ok_or_else(|| corrupt("seq", "is 0 (must be >= 1)".to_string()))?;

        let prev_hash = decode_hash32(&self.prev_hash).map_err(|d| corrupt("prev_hash", d))?;
        let binary_hash =
            decode_hash32(&self.binary_hash).map_err(|d| corrupt("binary_hash", d))?;
        let entry_hash = decode_hash32(&self.entry_hash).map_err(|d| corrupt("entry_hash", d))?;

        let tenant_id = TenantId::new(self.tenant_id.clone())
            .ok_or_else(|| corrupt("tenant_id", "is empty or contains a null byte".to_string()))?;
        let time_wall = OffsetDateTime::parse(&self.time_wall, &Rfc3339)
            .map_err(|e| corrupt("time_wall", format!("is not RFC3339: {e}")))?;
        let kind = EventKind::from_storage_str(&self.kind)
            .map_err(|e| corrupt("kind", format!("is an unknown event kind: {e}")))?;
        let payload = BASE64_STANDARD
            .decode(&self.payload)
            .map_err(|e| corrupt("payload", format!("is not valid base64: {e}")))?;

        Ok(Entry {
            id: EntryId(id_ulid),
            seq,
            prev_hash: EntryHash::from_bytes(prev_hash),
            time_wall,
            time_mono: self.time_mono,
            actor: self.actor.clone(),
            binary_hash: BinaryHash::from_bytes(binary_hash),
            tenant_id,
            kind,
            payload,
            idempotency_key: self.idempotency_key.clone(),
            entry_hash: EntryHash::from_bytes(entry_hash),
        })
    }

    /// `seq` accessor for the bundle reader's seq-ordered scan.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// `entry_hash` accessor — hex-encoded; the canonical
    /// agreement key per ADR-0030 §4.
    pub fn entry_hash(&self) -> &str {
        &self.entry_hash
    }
}

/// Encode a [`MirrorEntry`] as one JSON-Lines line (terminating
/// `\n` included). Single-line `serde_json::to_string` — NOT
/// `to_string_pretty` — so each entry occupies exactly one line.
fn encode_line(record: &MirrorEntry) -> Result<Vec<u8>, AppendError> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Hex-decode a mirror hash field into the 32-byte SHA-256 array, or a loud
/// reason string. Shared by [`MirrorEntry::to_entry`] for `prev_hash` /
/// `binary_hash` / `entry_hash`.
fn decode_hash32(hex_str: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(hex_str).map_err(|e| format!("is not valid hex: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("decoded to {} bytes (expected 32)", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Append-only read of the mirror file. Returns the seq-ordered
/// vector of records. ADR-0030 §4.
///
/// # Errors
///
/// - `AppendError::MirrorIo(NotFound)` if the file does not exist.
///   Callers (the bundle reader) treat this as
///   `MirrorAgreementStatus::AbsentPrePr17`.
/// - `AppendError::MirrorIo(_)` for any other I/O failure.
/// - `AppendError::MirrorCorrupt { reason }` if:
///   - any line fails JSON decoding;
///   - the trailing line is non-empty AND lacks a final `\n`;
///   - seqs are non-ascending, non-contiguous from 1, or duplicate.
pub fn read_mirror_entries(mirror_path: &Path) -> Result<Vec<MirrorEntry>, AppendError> {
    let file = File::open(mirror_path).map_err(AppendError::MirrorIo)?;
    let mut reader = BufReader::new(&file);

    // Detect "trailing line lacks newline" by inspecting the last
    // byte of the file before line-iteration. An empty file is OK
    // (no entries yet); a non-empty file with no trailing newline
    // is a partial-write signal per ADR-0030 §3.
    let len = file.metadata().map_err(AppendError::MirrorIo)?.len();
    if len > 0 {
        let mut tail = [0u8; 1];
        let mut last_byte_reader = File::open(mirror_path).map_err(AppendError::MirrorIo)?;
        last_byte_reader
            .seek(SeekFrom::End(-1))
            .map_err(AppendError::MirrorIo)?;
        last_byte_reader
            .read_exact(&mut tail)
            .map_err(AppendError::MirrorIo)?;
        if tail[0] != b'\n' {
            return Err(AppendError::MirrorCorrupt {
                reason: "last line lacks trailing newline — prior write was interrupted; \
                         operator must truncate the partial line before continuing"
                    .to_string(),
            });
        }
    }

    let mut out: Vec<MirrorEntry> = Vec::new();
    let mut line_no: u64 = 0;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(AppendError::MirrorIo)?;
        if n == 0 {
            break;
        }
        line_no += 1;
        // Strip the trailing `\n` (and `\r` if a CRLF FS slipped
        // one in) before JSON-decoding.
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            return Err(AppendError::MirrorCorrupt {
                reason: format!("empty line at line {line_no}"),
            });
        }
        let record: MirrorEntry =
            serde_json::from_str(trimmed).map_err(|e| AppendError::MirrorCorrupt {
                reason: format!("JSON decode failure at line {line_no}: {e}"),
            })?;
        // Ascending-contiguous seq from 1 — same invariant
        // `verify_chain` enforces on the DB side.
        let expected = (out.len() as u64) + 1;
        if record.seq != expected {
            return Err(AppendError::MirrorCorrupt {
                reason: format!(
                    "seq jump at line {line_no}: expected seq={expected}, found seq={}",
                    record.seq
                ),
            });
        }
        out.push(record);
    }
    Ok(out)
}

// ───────────────────────────────────────────────────────────────────────────
// H1 / ADR-0099 Class 4 — unified torn-tail mirror-read policy. Ported from the
// editions ADR-0098 R1 preserve-and-refuse arms (Cservin69/ABERP-Editions
// crates/audit-ledger/src/mirror.rs @ 1a56872).
//
// A crash during a mirror append leaves the commonest artifact: EXACTLY ONE
// unterminated trailing line ("the append never durably happened", ADR-0030 §3).
// Strict `read_mirror_entries` correctly rejects it — but the boot reconciler
// historically reacted by SILENTLY `rebuild_mirror_from_db` (`.truncate(true)`),
// destroying an intact prefix that may hold entries the DB lost via a dropped
// WAL tail. This policy PRESERVES the original first, then (boot side) trims a
// lone torn tail whose intact prefix the DB head COVERS and CONTINUES, or
// REFUSES on anything deeper. It NEVER silently rebuilds-from-DB and NEVER
// truncates a prefix that may hold entries the DB lacks.
//
// PROD PRE-FIX of editions Bug 3: the editions `read_mirror_under_tail_policy`
// TRIMMED the file to the prefix INSIDE the read, BEFORE the reconciler could
// confirm the DB head covers the trimmed prefix — so a torn tail whose prefix
// was STILL AHEAD of the DB had its live file mutated even though boot then
// refused. Here the read is SIDE-EFFECT-FREE: it classifies and returns the
// intact prefix, and the boot caller applies "preserve → trim ONE torn tail
// only if DB head ≥ trimmed head → continue" (or routes a still-ahead prefix to
// the ahead-of-DB preserve+refuse) with the DB head in hand. This keeps the
// policy the ONE reusable boot+recovery mirror-read classifier (H5 reuses it).
// ───────────────────────────────────────────────────────────────────────────

/// Classification of a mirror file under the unified torn-tail policy, returned
/// by [`read_mirror_under_tail_policy`]. The read is side-effect-free; the
/// caller applies preserve/trim/refuse per its own DB-head relationship (the
/// boot reconciler [`ensure_consistent_with_db`] wires it; H5's recovery
/// mirror-read reuses the same classification).
#[derive(Debug)]
pub enum MirrorTailPolicy {
    /// Parsed clean — no corruption. Carries the entries.
    Clean(Vec<MirrorEntry>),
    /// EXACTLY one unterminated/partial FINAL line — a torn tail. `entries` is
    /// the chain-reverified intact prefix; `prefix_len` is its byte length (the
    /// durable-trim target); `dropped_bytes` is the non-durable tail length. No
    /// filesystem mutation has happened yet — the caller preserves the original
    /// and (only if the DB head covers `entries`' head) trims to `prefix_len`.
    TornTail {
        entries: Vec<MirrorEntry>,
        prefix_len: u64,
        dropped_bytes: u64,
    },
    /// Corruption DEEPER than a torn tail (a break/gap/JSON/chain mismatch NOT
    /// at the final line). The caller PRESERVES the original and REFUSES —
    /// never rebuild-from-DB, never hand-edit the JSONL.
    DeepCorrupt { reason: String },
}

/// PURE torn-tail decision core (ADR-0098 R1), I/O- and serde-free.
///
/// * `terminated` — the mirror's last byte is `\n` (no partial trailing line).
/// * `prefix_ok`  — the newline-terminated PREFIX region parses AND re-verifies
///   (JSON valid, seq ascending-contiguous from 1, hash-chain links intact).
///
/// | `terminated` | `prefix_ok` | disposition |
/// |--------------|-------------|-------------|
/// | true         | true        | `Clean`    — fully terminated, prefix intact |
/// | true         | false       | `Deep`     — a COMPLETE line is broken (not a torn tail) |
/// | false        | true        | `TornTail` — lone partial final line, prefix intact |
/// | false        | false       | `Deep`     — partial final line AND a deeper break |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailDecision {
    Clean,
    TornTail,
    Deep,
}

/// The pure R1 torn-tail branch (see [`TailDecision`]).
fn decide_tail(terminated: bool, prefix_ok: bool) -> TailDecision {
    match (terminated, prefix_ok) {
        (true, true) => TailDecision::Clean,
        (false, true) => TailDecision::TornTail,
        (_, false) => TailDecision::Deep,
    }
}

/// STRICT parse + hash-chain RE-VERIFICATION of a newline-terminated mirror
/// region (genesis→head). Same JSON + ascending-contiguous-seq-from-1 invariant
/// as [`read_mirror_entries`], PLUS a chain-LINK check (each entry's `prev_hash`
/// equals the previous entry's `entry_hash`) over the WHOLE prefix — the
/// "re-verify the chain genesis→head over the trimmed prefix" H1 requires before
/// it will accept a torn-tail trim. An empty region is vacuously clean.
fn parse_and_reverify_prefix(prefix: &[u8]) -> Result<Vec<MirrorEntry>, String> {
    let mut out: Vec<MirrorEntry> = Vec::new();
    for (idx, raw) in prefix.split_inclusive(|&b| b == b'\n').enumerate() {
        let line_no = idx as u64 + 1;
        let text = std::str::from_utf8(raw)
            .map_err(|e| format!("non-UTF8 bytes at line {line_no}: {e}"))?;
        let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.is_empty() {
            return Err(format!("empty line at line {line_no}"));
        }
        let record: MirrorEntry = serde_json::from_str(trimmed)
            .map_err(|e| format!("JSON decode failure at line {line_no}: {e}"))?;
        let expected = out.len() as u64 + 1;
        if record.seq != expected {
            return Err(format!(
                "seq jump at line {line_no}: expected seq={expected}, found seq={}",
                record.seq
            ));
        }
        if let Some(prev) = out.last() {
            if record.prev_hash != prev.entry_hash {
                return Err(format!(
                    "hash-chain break at seq {}: prev_hash does not match the seq {} entry_hash",
                    record.seq, prev.seq
                ));
            }
        }
        out.push(record);
    }
    Ok(out)
}

/// Split the mirror bytes at the last `\n`, strictly parse+re-verify the
/// terminated prefix, and classify. Returns `(decision, prefix_entries,
/// prefix_len_bytes, deep_reason)`.
fn classify_mirror_bytes(bytes: &[u8]) -> (TailDecision, Vec<MirrorEntry>, usize, Option<String>) {
    if bytes.is_empty() {
        return (TailDecision::Clean, Vec::new(), 0, None);
    }
    let terminated = bytes.last() == Some(&b'\n');
    // The terminated prefix = bytes up to AND INCLUDING the last `\n` (empty if
    // the whole file is a single unterminated line).
    let prefix_len = match bytes.iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => 0,
    };
    match parse_and_reverify_prefix(&bytes[..prefix_len]) {
        Ok(entries) => (decide_tail(terminated, true), entries, prefix_len, None),
        Err(reason) => (
            decide_tail(terminated, false),
            Vec::new(),
            prefix_len,
            Some(reason),
        ),
    }
}

/// Read the mirror under the unified torn-tail policy (ADR-0098 R1 / H1) — the
/// ONE code path the boot reconciler ([`ensure_consistent_with_db`]) and (H5)
/// the recovery mirror-read share, so the two take ONE coherent stance on a
/// torn trailing line.
///
/// The read is SIDE-EFFECT-FREE (the prod pre-fix of editions Bug 3): it only
/// reads + classifies, never mutating the file. A missing file surfaces as
/// `MirrorIo(NotFound)` (callers handle it as they always have — boot creates).
/// Any other read I/O is loud.
pub fn read_mirror_under_tail_policy(mirror_path: &Path) -> Result<MirrorTailPolicy, AppendError> {
    let bytes = std::fs::read(mirror_path).map_err(AppendError::MirrorIo)?;
    let (decision, entries, prefix_len, reason) = classify_mirror_bytes(&bytes);
    match decision {
        TailDecision::Clean => Ok(MirrorTailPolicy::Clean(entries)),
        TailDecision::TornTail => Ok(MirrorTailPolicy::TornTail {
            entries,
            prefix_len: prefix_len as u64,
            dropped_bytes: (bytes.len() - prefix_len) as u64,
        }),
        TailDecision::Deep => Ok(MirrorTailPolicy::DeepCorrupt {
            reason: reason.unwrap_or_else(|| "mirror is malformed".to_string()),
        }),
    }
}

/// Preserve the current mirror to a timestamped side file so evidence is never
/// destroyed — the torn-tail / deep-corrupt analogue of
/// [`preserve_ahead_mirror`], writing `<mirror>.corrupt-<nanos>.bak`. A
/// byte-for-byte copy; the original is left in place for the caller to trim (a
/// covered torn tail) or leave intact (a refuse arm). Returns the backup path
/// for the surfaced log/error.
fn preserve_corrupt_mirror(mirror_path: &Path) -> Result<PathBuf, AppendError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut os = mirror_path.as_os_str().to_owned();
    os.push(format!(".corrupt-{nanos}.bak"));
    let backup = PathBuf::from(os);
    std::fs::copy(mirror_path, &backup).map_err(AppendError::MirrorIo)?;
    Ok(backup)
}

/// Durably truncate the mirror to `keep_len` bytes (the verified-intact prefix),
/// dropping a non-durable torn trailing line. fsync so the trim itself survives
/// a crash. The dropped bytes were preserved by [`preserve_corrupt_mirror`]
/// FIRST, so this destroys no evidence.
fn trim_mirror_to(mirror_path: &Path, keep_len: u64) -> Result<(), AppendError> {
    let file = OpenOptions::new()
        .write(true)
        .read(true)
        .open(mirror_path)
        .map_err(AppendError::MirrorIo)?;
    file.lock_exclusive().map_err(AppendError::MirrorIo)?;
    file.set_len(keep_len).map_err(AppendError::MirrorIo)?;
    file.sync_all().map_err(AppendError::MirrorIo)?;
    Ok(())
}

/// Preserve the current (AHEAD-of-DB) mirror to a timestamped side file so the
/// evidence of what the DB lost is never destroyed (ADR-0093 chunk 3 / ADR-0082
/// reconcile safety). A byte-for-byte copy to `<mirror>.ahead-<nanos>.bak`; the
/// original mirror is left in place, so the boot reconcile keeps surfacing the
/// AHEAD condition until a human resolves it. Returns the backup path for the
/// surfaced error message.
fn preserve_ahead_mirror(mirror_path: &Path) -> Result<PathBuf, AppendError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut os = mirror_path.as_os_str().to_owned();
    os.push(format!(".ahead-{nanos}.bak"));
    let backup = PathBuf::from(os);
    std::fs::copy(mirror_path, &backup).map_err(AppendError::MirrorIo)?;
    Ok(backup)
}

/// ADR-audit-armor preserve-BEFORE-heal — copy the ahead mirror to
/// `<mirror>.healed-<nanos>.bak` BEFORE the heal touches the DB, so the exact
/// bytes that were replayed survive even the successful-heal path (the DB heal
/// is a mutation; the evidence of what it healed from must not be lost). A
/// byte-for-byte copy; the original mirror is left in place. Returns the backup
/// path for the surfaced log line.
fn preserve_healed_mirror(mirror_path: &Path) -> Result<PathBuf, AppendError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut os = mirror_path.as_os_str().to_owned();
    os.push(format!(".healed-{nanos}.bak"));
    let backup = PathBuf::from(os);
    std::fs::copy(mirror_path, &backup).map_err(AppendError::MirrorIo)?;
    Ok(backup)
}

/// Synchronise the mirror file to the DB's current head. ADR-0030
/// §2. Called by the binary path after `tx.commit()`.
///
/// Behaviour:
/// - Acquires an exclusive advisory lock on the mirror file
///   (`fs2::FileExt::lock_exclusive`) for the duration of the call;
///   the lock is released on `Drop` of the `File` handle (or
///   explicit unlock in the error paths).
/// - If the mirror file does not exist AND the DB is non-empty,
///   runs the implicit one-time backfill per ADR-0030 §7. Logs at
///   INFO level with `audit_mirror_initialized`.
/// - If the mirror file exists, reads its last line (the "head"),
///   verifies it against the DB's matching entry by `entry_hash`,
///   then appends each DB entry with `seq > mirror_head_seq`.
/// - Returns the new mirror head seq on success.
///
/// # Errors
///
/// - `AppendError::Storage(_)` for DuckDB read failures.
/// - `AppendError::MirrorCorrupt { reason }` per `read_mirror_entries`'s
///   contract, plus any partial-line detection.
/// - `AppendError::MirrorDivergent { seq, reason }` if the
///   mirror's `entry_hash[seq]` disagrees with the DB's
///   corresponding entry. Per ADR-0030 §3 the DB is NOT rolled back.
/// - `AppendError::MirrorIo(_)` for any filesystem I/O failure
///   (open, lock, seek, read, write, fsync).
pub fn sync_mirror(
    conn: &Connection,
    meta: &LedgerMeta,
    mirror_path: &Path,
) -> Result<u64, AppendError> {
    // 1. Open (or create) the mirror file in append+read mode. The
    //    advisory lock is held on this handle for the whole call.
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(mirror_path)
        .map_err(AppendError::MirrorIo)?;
    file.lock_exclusive().map_err(AppendError::MirrorIo)?;

    // 2. Re-stat now that the lock is held — the bytes we read are
    //    the bytes we own. `read_mirror_entries` opens the file
    //    separately for read; that's fine because the lock is
    //    advisory and we hold it on the directory entry.
    let bytes_at_lock = file.metadata().map_err(AppendError::MirrorIo)?.len();

    let mirror_head_seq: u64;
    let mirror_head_hash: Option<String>;

    if bytes_at_lock == 0 {
        // Empty (or just-created) mirror file. Both the "first
        // call ever on a fresh DB" and "implicit backfill on
        // a pre-PR-17 DB" paths land here; the difference is
        // resolved by whether the DB has prior entries (handled
        // below in step 5).
        mirror_head_seq = 0;
        mirror_head_hash = None;
    } else {
        // Read the last line via a tail scan. For typical per-
        // tenant volumes (annual invoice counts for one SME) the
        // mirror is bounded and reading the full file is cheap;
        // we still use the existing `read_mirror_entries`
        // function so the partial-line + non-ascending checks
        // surface uniformly. If hyperscale volume becomes a
        // pattern, F39 (ADR-0029) is the named trigger.
        let entries = read_mirror_entries(mirror_path)?;
        match entries.last() {
            Some(last) => {
                mirror_head_seq = last.seq;
                mirror_head_hash = Some(last.entry_hash.clone());
            }
            None => {
                mirror_head_seq = 0;
                mirror_head_hash = None;
            }
        }
    }

    // 3. Read the DB entries strictly after mirror_head_seq.
    let new_entries = read_db_entries_after(conn, mirror_head_seq)?;

    // 4. If the mirror has a head, verify the DB's matching entry
    //    has the same `entry_hash`. Disagreement is divergence
    //    (CLAUDE.md rule 12 — refuse the next append).
    if let Some(mirror_hash) = mirror_head_hash.as_ref() {
        let db_head_at_mirror = read_db_entry_at_seq(conn, mirror_head_seq)?;
        match db_head_at_mirror {
            None => {
                return Err(AppendError::MirrorDivergent {
                    seq: mirror_head_seq,
                    reason: format!(
                        "DB has no entry at seq={mirror_head_seq} but mirror does — \
                         mirror is ahead of DB; operator must investigate before re-running"
                    ),
                });
            }
            Some(entry) => {
                let db_hash = hex::encode(entry.entry_hash.as_bytes());
                if &db_hash != mirror_hash {
                    return Err(AppendError::MirrorDivergent {
                        seq: mirror_head_seq,
                        reason: format!(
                            "mirror entry_hash={mirror_hash} disagrees with DB entry_hash={db_hash}; \
                             operator must investigate before re-running"
                        ),
                    });
                }
            }
        }
    }

    // 5. Bootstrap detection: empty mirror + non-empty DB = the
    //    implicit one-time backfill path per ADR-0030 §7. LOUD
    //    INFO log line names the event so the operator sees it
    //    in the command's output.
    let bootstrap_count = if mirror_head_seq == 0 && !new_entries.is_empty() {
        new_entries.len()
    } else {
        0
    };

    // 6. Append every new entry as one JSON-Lines line. The
    //    `OpenOptions::append(true)` mode makes each `write_all`
    //    call append-atomic on POSIX (up to PIPE_BUF, which a
    //    single audit line never exceeds in practice). Fsync
    //    once at the end per ADR-0008 §"Storage".
    let mut appended: u64 = 0;
    for entry in &new_entries {
        let record = MirrorEntry::from_entry(entry)?;
        let line = encode_line(&record)?;
        (&file).write_all(&line).map_err(AppendError::MirrorIo)?;
        appended += 1;
    }
    if appended > 0 {
        (&file).flush().map_err(AppendError::MirrorIo)?;
        file.sync_all().map_err(AppendError::MirrorIo)?;
    }

    let new_head_seq = mirror_head_seq + appended;
    let tenant_id_str = meta.tenant_id().as_str();

    if bootstrap_count > 0 {
        tracing::info!(
            tenant = %tenant_id_str,
            mirror_path = %mirror_path.display(),
            entries_backfilled = bootstrap_count,
            new_head_seq,
            "audit_mirror_initialized"
        );
    } else if appended > 0 {
        tracing::debug!(
            tenant = %tenant_id_str,
            mirror_path = %mirror_path.display(),
            entries_appended = appended,
            new_head_seq,
            "audit_mirror_synced"
        );
    }

    // Advisory lock released by `Drop` of `file`.
    Ok(new_head_seq)
}

/// What boot-time reconciliation did to make the mirror consistent
/// with the DB. Session 152b — the mirror is a derivable cache, not a
/// source of truth: between processes, boot restores the invariant
/// instead of letting the next post-commit [`sync_mirror`] 500.
///
/// Each variant carries the entry count so the boot log names the
/// magnitude loudly per CLAUDE.md rule 12.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Mirror already agreed with the DB (head seqs equal, last
    /// `entry_hash` matched). Idempotent no-op.
    Unchanged,
    /// Mirror file was absent; created fresh from DB entries
    /// `[1..=db_max_seq]`.
    Created { entries_written: u64 },
    /// Mirror was behind the DB; replayed the missing DB entries
    /// `[mirror_max_seq+1..=db_max_seq]`. A lone torn trailing line whose
    /// intact prefix the DB head covers also lands here after the tail is
    /// preserved + trimmed (H1 / ADR-0099 Class 4).
    Extended { entries_added: u64 },
    /// Mirror was the same length as the DB but its last `entry_hash`
    /// disagreed (or the mirror file was corrupt/unparseable). Full
    /// rebuild from the DB.
    ///
    /// H1 / ADR-0099 Class 4: `ensure_consistent_with_db` NO LONGER
    /// constructs this — an ahead mirror, an equal-length head-hash
    /// divergence, and corruption deeper than a torn tail all now PRESERVE +
    /// REFUSE rather than silently rebuild. The variant is retained as public
    /// API (a rebuild remains a legitimate recovery outcome a future guarded
    /// path may report).
    Rebuilt { entries_written: u64 },

    /// ADR-audit-armor — the mirror was AHEAD of the DB and the gated auto-heal
    /// PROVED it was a benign WAL-fold loss (boundary `entry_hash` agreed, the
    /// decoded tail verified, and the post-replay full-genesis `verify_chain`
    /// passed inside the heal transaction), so the mirror's extra rows were
    /// replayed into the DB instead of refusing. `entries_replayed` is the count
    /// of mirror tail rows restored (excluding the `db.auto_recovered` forensic
    /// row the heal appends). A real fork never reaches this outcome — it fails
    /// a discriminator or the in-tx re-verify and falls back to
    /// [`AppendError::MirrorAheadOfDb`].
    Healed { entries_replayed: u64 },
}

/// Boot-time reconciliation of the mirror against the DB. Session
/// 152b / Part A. Called once per process at serve boot AFTER
/// [`crate::ensure_schema`] succeeds, and BEFORE any request can
/// trigger a per-write [`sync_mirror`].
///
/// The DB is the source of truth; the mirror is a derivable cache.
/// This function restores the between-process invariant
/// "mirror == DB" without ever mutating a DB entry. The decision
/// tree (Part B):
///
/// - mirror file missing → create fresh from DB → [`RecoveryAction::Created`]
/// - mirror behind DB → replay missing entries → [`RecoveryAction::Extended`]
/// - mirror ahead of DB (arm a) → PRESERVE the ahead mirror + REFUSE
///   ([`AppendError::MirrorAheadOfDb`]) — never silently truncated
/// - lone torn trailing line COVERED by the DB (arm b) → preserve + trim the
///   torn tail + CONTINUE (reconciles as [`RecoveryAction::Extended`] /
///   [`RecoveryAction::Unchanged`]); a torn-tail prefix still ahead → arm (a)
/// - equal length, last hash matches → [`RecoveryAction::Unchanged`]
/// - equal length, last hash differs (arm c), OR corruption deeper than a torn
///   tail → PRESERVE + REFUSE ([`AppendError::MirrorCorruptPreserved`])
///
/// Idempotent: a second call on a healthy state returns
/// [`RecoveryAction::Unchanged`].
///
/// # Errors
///
/// - `AppendError::Storage(_)` for DuckDB read failures.
/// - `AppendError::MirrorAheadOfDb` (arm a) — the mirror holds entries the DB
///   lacks; boot must refuse. The ahead mirror is preserved first.
/// - `AppendError::MirrorCorruptPreserved` (arms b-deep / c) — divergence worse
///   than a lone torn tail; the original is preserved first, then boot refuses.
/// - `AppendError::MirrorIo(_)` for filesystem I/O failures OTHER than
///   `NotFound` (a `NotFound` is the "missing mirror" case, handled
///   as `Created`). A disk/permission failure is loud, not silently
///   "recovered".
///
/// H1 / ADR-0099 Class 4: a divergence is NEVER silently rebuilt-from-DB. The
/// mirror is a derivable cache ONLY while it cannot hold entries the DB lacks;
/// once it might (ahead / deep-corrupt / equal-length divergence) the evidence
/// is preserved and boot refuses to serve.
pub fn ensure_consistent_with_db(
    conn: &Connection,
    mirror_path: &Path,
) -> Result<RecoveryAction, AppendError> {
    let db_max_seq = read_db_max_seq(conn)?;

    // Read the mirror under the unified H1 torn-tail policy (ADR-0099 Class 4).
    // The read is side-effect-free; this boot arm applies the preserve / trim /
    // refuse decisions with the DB head in hand (the prod pre-fix of editions
    // Bug 3):
    //   - clean → reconcile the parsed entries below;
    //   - torn tail COVERED by the DB (db_max_seq ≥ trimmed head) → preserve +
    //     durably trim the lone torn tail + CONTINUE on the chain-reverified
    //     prefix (audit event), reconciling against the DB below;
    //   - torn tail whose prefix is STILL AHEAD of the DB → do NOT trim; route
    //     to the ahead-of-DB preserve + refuse (arm a);
    //   - deeper corruption → preserve + REFUSE (never rebuild-from-DB);
    //   - missing mirror → (re)build from the DB (Created).
    let mirror_entries = match read_mirror_under_tail_policy(mirror_path) {
        Ok(MirrorTailPolicy::Clean(entries)) => entries,
        Ok(MirrorTailPolicy::TornTail {
            entries,
            prefix_len,
            dropped_bytes,
        }) => {
            let trimmed_head_seq = entries.last().map(|e| e.seq).unwrap_or(0);
            if db_max_seq >= trimmed_head_seq {
                // The torn tail was never durably committed AND the DB head
                // covers the chain-reverified intact prefix — preserve the
                // original, durably trim the lone torn tail, and CONTINUE (the
                // reconcile below extends/agrees against the DB). Never a silent
                // rebuild-from-DB.
                let preserved = preserve_corrupt_mirror(mirror_path)?;
                trim_mirror_to(mirror_path, prefix_len)?;
                tracing::warn!(
                    target: "audit_event",
                    event = "audit_mirror_torn_tail_trimmed",
                    mirror_path = %mirror_path.display(),
                    preserved = %preserved.display(),
                    dropped_bytes,
                    trimmed_head_seq,
                    db_max_seq,
                    "audit_mirror torn trailing line — preserved the original and trimmed to the \
                     chain-reverified intact prefix; continuing (H1 / ADR-0099 Class 4; the dropped \
                     line was never durably committed)"
                );
                entries
            } else {
                // The intact prefix is STILL AHEAD of the DB even after dropping
                // the torn tail — the fingerprint of a lost DB commit.
                //
                // ADR-audit-armor — attempt the GATED AUTO-HEAL first over the
                // chain-reverified intact prefix. On success the DB is replayed
                // up to the prefix head and the torn tail is dropped from the
                // mirror file (`trim_to = Some(prefix_len)`); on ANY refusal fall
                // back to preserve_ahead + REFUSE, leaving the on-disk mirror
                // BYTE-FOR-BYTE untouched (the H1 Bug-3 pre-fix: never trim on a
                // refuse).
                if let Some(action) = heal_from_mirror_ahead(
                    conn,
                    mirror_path,
                    &entries,
                    db_max_seq,
                    Some(prefix_len),
                )? {
                    return Ok(action);
                }
                // Heal refused — do NOT trim; preserve the ahead mirror and REFUSE.
                let preserved = preserve_ahead_mirror(mirror_path)?;
                tracing::error!(
                    target: "audit_event",
                    event = "audit_mirror_ahead_of_db_refused",
                    mirror_path = %mirror_path.display(),
                    mirror_max_seq = trimmed_head_seq,
                    db_max_seq,
                    preserved = %preserved.display(),
                    "audit_mirror torn-tail prefix is STILL AHEAD of the DB — REFUSING to \
                     auto-truncate; preserved the ahead mirror and surfacing (possible lost DB \
                     commit — investigate before re-running) (H1 / ADR-0099 Class 4)"
                );
                return Err(AppendError::MirrorAheadOfDb {
                    mirror_max_seq: trimmed_head_seq,
                    db_max_seq,
                    preserved: preserved.display().to_string(),
                });
            }
        }
        Ok(MirrorTailPolicy::DeepCorrupt { reason }) => {
            // Corruption deeper than a lone torn tail — NEVER rebuild-from-DB
            // (that could destroy a prefix the DB lacks) and NEVER hand-edit the
            // JSONL. Preserve the original byte-for-byte and REFUSE.
            let preserved = preserve_corrupt_mirror(mirror_path)?;
            tracing::error!(
                target: "audit_event",
                event = "audit_mirror_deep_corrupt_refused",
                mirror_path = %mirror_path.display(),
                preserved = %preserved.display(),
                %reason,
                "audit_mirror is corrupt beyond a torn tail — REFUSING (preserved the original; do \
                 NOT rebuild-from-DB, do NOT hand-edit the JSONL) (H1 / ADR-0099 Class 4)"
            );
            return Err(AppendError::MirrorCorruptPreserved {
                preserved: preserved.display().to_string(),
                reason,
            });
        }
        Err(AppendError::MirrorIo(io)) if io.kind() == std::io::ErrorKind::NotFound => {
            let written = rebuild_mirror_from_db(conn, mirror_path)?;
            tracing::info!(
                mirror_path = %mirror_path.display(),
                entries_written = written,
                db_max_seq,
                "audit_mirror_recovered action=created (mirror file was absent)"
            );
            return Ok(RecoveryAction::Created {
                entries_written: written,
            });
        }
        Err(other) => return Err(other),
    };

    let mirror_max_seq = mirror_entries.last().map(|e| e.seq).unwrap_or(0);

    if mirror_max_seq < db_max_seq {
        let added = append_db_entries_after(conn, mirror_path, mirror_max_seq)?;
        tracing::info!(
            mirror_path = %mirror_path.display(),
            mirror_max_seq,
            db_max_seq,
            entries_added = added,
            "audit_mirror_recovered action=extended (mirror was behind DB)"
        );
        Ok(RecoveryAction::Extended {
            entries_added: added,
        })
    } else if mirror_max_seq > db_max_seq {
        // Arm (a): a CLEAN mirror AHEAD of the DB (mirror_max_seq > db_max_seq).
        // The fingerprint of a WAL-fold tear / lost DB commit (any fresh-open
        // writer OR reader — scenario G/H) or a dev DB-nuke.
        //
        // ADR-audit-armor — attempt the GATED AUTO-HEAL first: if this is a
        // provable benign loss (boundary agrees, tail verifies, in-tx
        // full-genesis re-verify passes), replay the mirror's lost tail into the
        // DB and CONTINUE. On ANY refusal fall back to the historical
        // preserve_ahead + REFUSE (the whole mirror is the intact prefix, so
        // nothing to trim).
        if let Some(action) =
            heal_from_mirror_ahead(conn, mirror_path, &mirror_entries, db_max_seq, None)?
        {
            return Ok(action);
        }
        // Heal refused — NEVER silently truncate (that destroys the only
        // surviving record of what the DB lost). Preserve the ahead mirror to a
        // side file FIRST, then REFUSE so a human investigates.
        let entries_ahead = mirror_max_seq - db_max_seq;
        let preserved = preserve_ahead_mirror(mirror_path)?;
        tracing::error!(
            mirror_path = %mirror_path.display(),
            mirror_max_seq,
            db_max_seq,
            entries_ahead,
            preserved = %preserved.display(),
            "audit_mirror_AHEAD_of_db — REFUSING to auto-truncate; preserved the ahead \
             mirror and surfacing (possible lost DB commit — investigate before re-running)"
        );
        Err(AppendError::MirrorAheadOfDb {
            mirror_max_seq,
            db_max_seq,
            preserved: preserved.display().to_string(),
        })
    } else if db_max_seq == 0 {
        // Both empty (mirror file present but zero entries, DB empty).
        Ok(RecoveryAction::Unchanged)
    } else {
        // Arm (c): equal non-zero length. Compare the head entry_hash — the
        // chain is a hash chain, so the head is a sound proxy for the whole
        // prefix's integrity. A mismatch at equal length on prod-class data is
        // evidence of something worse than a torn tail — NEVER auto-resolve by
        // rebuilding from the DB (that would destroy the mirror's record of what
        // it holds). Preserve + REFUSE.
        let db_head = read_db_entry_at_seq(conn, db_max_seq)?;
        let db_hash = db_head.map(|e| hex::encode(e.entry_hash.as_bytes()));
        let mirror_hash = mirror_entries.last().map(|e| e.entry_hash.clone());
        if db_hash == mirror_hash {
            Ok(RecoveryAction::Unchanged)
        } else {
            let preserved = preserve_corrupt_mirror(mirror_path)?;
            tracing::error!(
                target: "audit_event",
                event = "audit_mirror_head_hash_divergence_refused",
                mirror_path = %mirror_path.display(),
                db_max_seq,
                preserved = %preserved.display(),
                ?db_hash,
                ?mirror_hash,
                "audit_mirror head entry_hash DIVERGES from the DB at equal length — REFUSING \
                 (preserved the original; never auto-resolve equal-length divergence) (H1 / \
                 ADR-0099 Class 4)"
            );
            Err(AppendError::MirrorCorruptPreserved {
                preserved: preserved.display().to_string(),
                reason: format!(
                    "mirror head entry_hash diverges from the DB at equal length (seq={db_max_seq})"
                ),
            })
        }
    }
}

/// ADR-audit-armor — attempt the gated auto-heal for a mirror that is AHEAD of
/// the DB (`mirror_max_seq > db_max_seq`), replaying the mirror's provable-loss
/// tail into the DB instead of refusing. Returns:
///   * `Ok(Some(action))` — HEALED; the caller returns it and boot continues.
///   * `Ok(None)`          — REFUSED (a discriminator or the in-tx MF-1 full
///     re-verify rejected it); the caller falls back to the historical
///     `preserve_ahead_mirror` + [`AppendError::MirrorAheadOfDb`].
///   * `Err(e)`            — a hard filesystem failure while preserving evidence
///     before the DB was touched; propagate (boot refuses loudly).
///
/// `intact` is the chain-reverified mirror prefix — the WHOLE mirror for the
/// clean-ahead arm, or the pre-torn-tail prefix for the torn-tail-still-ahead
/// arm. `trim_to` is `Some(prefix_len)` for the torn-tail arm (the on-disk
/// mirror carries a torn trailing line to drop after a successful heal) and
/// `None` for the clean arm.
///
/// # Safety (MF-1 / MF-2, `_handoffs/ADR-audit-armor-ADVERSARIAL.md`)
///
/// Discriminator 1 (boundary `entry_hash` equality) and Discriminator 2 (decoded
/// full-mirror `verify_chain`) below are a FAST-PATH EARLY REJECT, **not** the
/// safety boundary. The authoritative fork-detector is the full-genesis
/// `verify_chain` run INSIDE the heal transaction by
/// [`crate::storage::heal_replay_mirror_tail`], which rolls back and refuses on
/// any failure. A duplicate-seq fork (there is no `UNIQUE(seq)`) can pass
/// Discriminator 1 — the boundary read is `WHERE seq=? LIMIT 1`, no `ORDER BY` —
/// yet it cannot pass the in-tx full re-verify. Do not treat the discriminators
/// as the fork guard. Discriminator 2 uses the FULL `verify_chain` (recomputes
/// `entry_hash`), never the link-only `parse_and_reverify_prefix` (MF-3).
fn heal_from_mirror_ahead(
    conn: &Connection,
    mirror_path: &Path,
    intact: &[MirrorEntry],
    db_max_seq: u64,
    trim_to: Option<u64>,
) -> Result<Option<RecoveryAction>, AppendError> {
    let mirror_max_seq = intact.last().map(|e| e.seq).unwrap_or(0);
    // Precondition guard: only heal a strictly-ahead mirror over a non-empty DB
    // (a common prefix must exist to prove agreement against). Anything else
    // refuses (fall back to the existing ahead-refuse).
    if mirror_max_seq <= db_max_seq || db_max_seq == 0 {
        return Ok(None);
    }

    // ── Discriminator 1 (fast-path early reject) — boundary entry_hash equality.
    // The DB head (seq db_max_seq) entry_hash must equal the mirror's entry_hash
    // at the same seq. By the Merkle property this makes `[1..=db_max_seq]`
    // byte-identical in both — IF the DB prefix is itself internally valid, which
    // ONLY the in-tx full re-verify establishes. This is the cheap "obvious fork"
    // rejector, not the proof.
    let db_boundary = match read_db_entry_at_seq(conn, db_max_seq)? {
        Some(e) => e,
        None => return Ok(None),
    };
    let mirror_boundary = match intact.iter().find(|e| e.seq == db_max_seq) {
        Some(e) => e,
        None => return Ok(None),
    };
    if hex::encode(db_boundary.entry_hash.as_bytes()) != mirror_boundary.entry_hash {
        tracing::warn!(
            target: "audit_event",
            event = "audit_mirror_heal_boundary_mismatch",
            mirror_path = %mirror_path.display(),
            db_max_seq,
            mirror_max_seq,
            "boot auto-heal early-reject: DB head entry_hash != mirror entry_hash at the boundary \
             seq — the chains fork at/below the boundary; this is NOT a benign loss. Refusing."
        );
        return Ok(None);
    }

    // ── Discriminator 2 (fast-path early reject) — decode the FULL mirror to
    // `Entry` and run the full `verify_chain` genesis→mirror head, recomputing
    // every `entry_hash` from canonical content (MF-3: the full verifier, never
    // the link-only `parse_and_reverify_prefix`). Proves the mirror tail chains
    // cleanly onto the boundary AND is internally untampered.
    let mut decoded: Vec<Entry> = Vec::with_capacity(intact.len());
    for m in intact {
        match m.to_entry() {
            Ok(e) => decoded.push(e),
            Err(e) => {
                tracing::warn!(
                    target: "audit_event",
                    event = "audit_mirror_heal_decode_failed",
                    mirror_path = %mirror_path.display(),
                    error = %e,
                    "boot auto-heal early-reject: an ahead mirror row failed to decode — refusing"
                );
                return Ok(None);
            }
        }
    }
    let tenant = match decoded.first() {
        Some(e) => e.tenant_id.clone(),
        None => return Ok(None),
    };
    if let Err(e) = crate::chain::verify::verify_chain(&tenant, decoded.iter()) {
        tracing::warn!(
            target: "audit_event",
            event = "audit_mirror_heal_tail_unverified",
            mirror_path = %mirror_path.display(),
            error = %e,
            "boot auto-heal early-reject: the decoded ahead mirror does NOT verify genesis→head — \
             a tampered/forked tail, not a benign loss. Refusing."
        );
        return Ok(None);
    }

    // ── Preserve-before-heal: copy the ahead mirror to `<mirror>.healed-<nanos>.bak`
    //    BEFORE touching the DB, so the replayed bytes survive even on success.
    //    A copy failure is loud (we must not heal without preserving evidence).
    let preserved = preserve_healed_mirror(mirror_path)?;

    // ── The heal proper: replay `[db_max_seq+1 ..= mirror_max_seq]` verbatim +
    //    the forensic row + the MF-1 full-genesis re-verify, all in ONE tx that
    //    rolls back and refuses on any failure (THE fork guard).
    let tail: Vec<Entry> = decoded
        .iter()
        .filter(|e| e.seq.as_u64() > db_max_seq)
        .cloned()
        .collect();
    let replayed = tail.len() as u64;
    let session_id = format!(
        "audit-heal-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let actor = Actor::from_local_cli(session_id, "system:audit-heal");
    // Injection-free: every interpolation is a `u64`. Field set matches the
    // documented `DbAutoRecoveredPayload` shape (aberp-db poison-recovery
    // convention) so any typed decoder round-trips it.
    let payload = format!(
        "{{\"trigger\":\"mirror_ahead_heal\",\"source_snapshot_seq\":{db_max_seq},\
         \"snapshot_audit_count\":{db_max_seq},\"replayed_entries\":{replayed},\
         \"recovered_max_seq\":{mirror_max_seq},\"retained_corrupt_db\":null}}"
    )
    .into_bytes();
    if let Err(e) = crate::storage::heal_replay_mirror_tail(conn, &tail, payload, actor) {
        tracing::error!(
            target: "audit_event",
            event = "audit_mirror_heal_refused_reverify",
            mirror_path = %mirror_path.display(),
            db_max_seq,
            mirror_max_seq,
            error = %e,
            "boot auto-heal REFUSED at the in-tx full-genesis verify (rolled back, DB untouched) — \
             this is the fork/tamper guard, not a benign loss. Falling back to preserve+refuse."
        );
        return Ok(None);
    }

    // Success — the DB is durably `[1..=mirror_max_seq]` (== mirror) + a forensic
    // `db.auto_recovered` row at `mirror_max_seq + 1`, committed. Everything below
    // is BEST-EFFORT mirror-file hygiene: any lag self-corrects on the next boot's
    // Extended / torn-tail-covered arm, so a failure here must NOT re-brick a
    // heal that already landed.

    // Torn-tail arm: drop the never-durable torn trailing line so the on-disk
    // mirror matches the healed DB (the original — incl. the torn tail — is in
    // the `.healed` backup already).
    if let Some(len) = trim_to {
        if let Err(e) = trim_mirror_to(mirror_path, len) {
            tracing::warn!(
                target: "audit_event",
                event = "audit_mirror_heal_trim_deferred",
                mirror_path = %mirror_path.display(),
                error = %e,
                "auto-heal landed but trimming the torn tail failed; next boot's torn-tail-covered \
                 arm will clean it"
            );
        }
    }

    // Push the forensic row into the mirror so the mirror == the healed DB and
    // the next boot is `Unchanged` (not `Extended`).
    let meta = LedgerMeta::new(tenant, BinaryHash::from_bytes([0u8; 32]));
    match sync_mirror(conn, &meta, mirror_path) {
        Ok(head) => tracing::warn!(
            target: "audit_event",
            event = "audit_mirror_auto_healed",
            mirror_path = %mirror_path.display(),
            db_max_seq,
            mirror_max_seq,
            entries_replayed = replayed,
            new_head_seq = head,
            preserved = %preserved.display(),
            "audit-ledger mirror was AHEAD of the DB and the gated auto-heal PROVED a benign \
             WAL-fold loss (boundary agreed, tail verified, in-tx full-genesis re-verify passed) — \
             replayed the lost tail into the DB and continued; the ahead mirror was preserved"
        ),
        Err(e) => tracing::warn!(
            target: "audit_event",
            event = "audit_mirror_heal_resync_deferred",
            mirror_path = %mirror_path.display(),
            error = %e,
            "auto-heal landed durably; the forensic-row mirror re-sync failed and will be picked \
             up by the next boot's Extended arm"
        ),
    }

    Ok(Some(RecoveryAction::Healed {
        entries_replayed: replayed,
    }))
}

/// Read the DB's max entry seq (0 if the table is empty). Reuses the
/// storage layer's `SELECT_HEAD` projection.
fn read_db_max_seq(conn: &Connection) -> Result<u64, AppendError> {
    let mut stmt = conn.prepare(crate::storage::schema::SELECT_HEAD)?;
    let mut rows = stmt.query_map([], row_to_entry_for_mirror)?;
    match rows.next() {
        Some(r) => Ok(r?.seq.as_u64()),
        None => Ok(0),
    }
}

/// Truncate the mirror and rewrite it from the DB's full entry set
/// `[1..=db_max_seq]`. Returns the entry count written.
///
/// H1 / ADR-0099 Class 4: the ONLY remaining caller is the "mirror file
/// absent" ([`RecoveryAction::Created`]) path, where there is no on-disk
/// mirror to destroy — a fresh build from the DB is safe. The ahead /
/// equal-length-divergence / deep-corrupt paths NO LONGER rebuild-from-DB
/// (they preserve + refuse), because a rebuild there could overwrite entries
/// the DB lacks.
fn rebuild_mirror_from_db(conn: &Connection, mirror_path: &Path) -> Result<u64, AppendError> {
    let entries = read_db_entries_after(conn, 0)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .read(true)
        .open(mirror_path)
        .map_err(AppendError::MirrorIo)?;
    file.lock_exclusive().map_err(AppendError::MirrorIo)?;
    let mut written: u64 = 0;
    for entry in &entries {
        let record = MirrorEntry::from_entry(entry)?;
        let line = encode_line(&record)?;
        (&file).write_all(&line).map_err(AppendError::MirrorIo)?;
        written += 1;
    }
    (&file).flush().map_err(AppendError::MirrorIo)?;
    file.sync_all().map_err(AppendError::MirrorIo)?;
    Ok(written)
}

/// Append DB entries with `seq > after_seq` to the existing mirror.
/// The Extended recovery path. Returns the count appended.
fn append_db_entries_after(
    conn: &Connection,
    mirror_path: &Path,
    after_seq: u64,
) -> Result<u64, AppendError> {
    let entries = read_db_entries_after(conn, after_seq)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(mirror_path)
        .map_err(AppendError::MirrorIo)?;
    file.lock_exclusive().map_err(AppendError::MirrorIo)?;
    let mut added: u64 = 0;
    for entry in &entries {
        let record = MirrorEntry::from_entry(entry)?;
        let line = encode_line(&record)?;
        (&file).write_all(&line).map_err(AppendError::MirrorIo)?;
        added += 1;
    }
    if added > 0 {
        (&file).flush().map_err(AppendError::MirrorIo)?;
        file.sync_all().map_err(AppendError::MirrorIo)?;
    }
    Ok(added)
}

/// Read DB entries with `seq > after_seq`, in ascending seq order.
/// Mirror-internal helper; mirrors `Ledger::entries` but with a
/// seq-bound filter so the sync path doesn't load the full ledger
/// each time.
fn read_db_entries_after(conn: &Connection, after_seq: u64) -> Result<Vec<Entry>, AppendError> {
    let mut stmt = conn.prepare(SELECT_AFTER_SEQ)?;
    let rows = stmt.query_map([after_seq as i64], row_to_entry_for_mirror)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Read the DB entry at the given seq (if present). Used by the
/// mirror's divergence check.
fn read_db_entry_at_seq(conn: &Connection, seq: u64) -> Result<Option<Entry>, AppendError> {
    let mut stmt = conn.prepare(SELECT_AT_SEQ)?;
    let mut rows = stmt.query_map([seq as i64], row_to_entry_for_mirror)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Local mirror of the storage-layer `row_to_entry` decoder. Kept
/// here because making the storage decoder `pub(crate)` would widen
/// the crate's internal API surface unnecessarily; the row shape is
/// stable (it matches the `schema::CREATE_TABLE` column order) and
/// the duplication is small (~30 lines).
fn row_to_entry_for_mirror(row: &duckdb::Row<'_>) -> duckdb::Result<Entry> {
    use crate::entry::{BinaryHash, TenantId};
    use ulid::Ulid;

    let id_prefixed: String = row.get(0)?;
    let seq: i64 = row.get(1)?;
    let prev_hash_blob: Vec<u8> = row.get(2)?;
    let time_wall_str: String = row.get(3)?;
    let time_mono_i: i64 = row.get(4)?;
    let actor_json: String = row.get(5)?;
    let binary_hash_blob: Vec<u8> = row.get(6)?;
    let tenant_str: String = row.get(7)?;
    let kind_str: String = row.get(8)?;
    let payload: Vec<u8> = row.get(9)?;
    let idempotency_key: Option<String> = row.get(10)?;
    let entry_hash_blob: Vec<u8> = row.get(11)?;

    let id_ulid_str = id_prefixed
        .strip_prefix("aud_")
        .ok_or_else(|| decode_err("entry id missing `aud_` prefix"))?;
    let id_ulid = Ulid::from_string(id_ulid_str)
        .map_err(|_| decode_err("entry id is not a valid Crockford-base32 ULID"))?;

    let prev_hash = to_hash32(&prev_hash_blob, "prev_hash")?;
    let binary_hash = to_hash32(&binary_hash_blob, "binary_hash")?;
    let entry_hash = to_hash32(&entry_hash_blob, "entry_hash")?;

    let tenant_id = TenantId::new(tenant_str)
        .ok_or_else(|| decode_err("tenant_id is empty or contains a null byte"))?;
    let time_wall = OffsetDateTime::parse(&time_wall_str, &Rfc3339)
        .map_err(|_| decode_err("time_wall is not RFC3339"))?;
    let actor = Actor::from_storage_json(&actor_json)
        .map_err(|_| decode_err("actor JSON failed to deserialize"))?;
    let kind =
        EventKind::from_storage_str(&kind_str).map_err(|_| decode_err("unknown event kind"))?;

    Ok(Entry {
        id: EntryId(id_ulid),
        seq: Sequence(seq as u64),
        prev_hash: EntryHash::from_bytes(prev_hash),
        time_wall,
        time_mono: time_mono_i as u64,
        actor,
        binary_hash: BinaryHash::from_bytes(binary_hash),
        tenant_id,
        kind,
        payload,
        idempotency_key,
        entry_hash: EntryHash::from_bytes(entry_hash),
    })
}

fn to_hash32(blob: &[u8], field: &'static str) -> duckdb::Result<[u8; 32]> {
    if blob.len() != 32 {
        return Err(decode_err_owned(format!(
            "{field} blob has length {} (expected 32)",
            blob.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(blob);
    Ok(out)
}

fn decode_err(msg: &'static str) -> duckdb::Error {
    duckdb::Error::FromSqlConversionFailure(
        0,
        duckdb::types::Type::Text,
        Box::<dyn std::error::Error + Send + Sync>::from(msg),
    )
}

fn decode_err_owned(msg: String) -> duckdb::Error {
    duckdb::Error::FromSqlConversionFailure(
        0,
        duckdb::types::Type::Text,
        Box::<dyn std::error::Error + Send + Sync>::from(msg),
    )
}

// SQL constants for the mirror's DB reads. Same column projection
// as `schema::SELECT_ALL`; differs only in the `WHERE seq > ?`
// (after-seq) or `WHERE seq = ?` (at-seq) clause.

const SELECT_AFTER_SEQ: &str = "
SELECT id, seq, prev_hash, time_wall, time_mono, actor,
       binary_hash, tenant_id, kind, payload, idempotency_key, entry_hash
FROM audit_ledger
WHERE seq > ?
ORDER BY seq ASC;
";

const SELECT_AT_SEQ: &str = "
SELECT id, seq, prev_hash, time_wall, time_mono, actor,
       binary_hash, tenant_id, kind, payload, idempotency_key, entry_hash
FROM audit_ledger
WHERE seq = ?
LIMIT 1;
";

// ──────────────────────────────────────────────────────────────────────
// Unit tests — path resolution, line encoding, partial-line detection,
// divergence detection, bootstrap path, idempotent re-sync.
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Actor, BinaryHash, TenantId};
    use crate::storage::{append_in_tx, ensure_schema, LedgerMeta};

    fn mk_meta() -> LedgerMeta {
        LedgerMeta::new(
            TenantId::new("t-1").unwrap(),
            BinaryHash::from_bytes([0u8; 32]),
        )
    }

    fn open_conn_with_two_entries() -> (Connection, LedgerMeta) {
        let mut conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        let meta = mk_meta();
        {
            let tx = conn.transaction().unwrap();
            append_in_tx(
                &tx,
                &meta,
                EventKind::Test,
                b"payload-1".to_vec(),
                Actor::test_only(),
                Some("idem-1".to_string()),
            )
            .unwrap();
            append_in_tx(
                &tx,
                &meta,
                EventKind::Test,
                b"payload-2".to_vec(),
                Actor::test_only(),
                Some("idem-2".to_string()),
            )
            .unwrap();
            tx.commit().unwrap();
        }
        (conn, meta)
    }

    fn append_one(conn: &mut Connection, meta: &LedgerMeta, idem_tag: &str, payload: &[u8]) {
        let tx = conn.transaction().unwrap();
        append_in_tx(
            &tx,
            meta,
            EventKind::Test,
            payload.to_vec(),
            Actor::test_only(),
            Some(idem_tag.to_string()),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn mirror_path_appends_audit_log_suffix_to_full_db_filename() {
        let db = Path::new("/var/aberp/t-1.duckdb");
        let mirror = mirror_path_for(db);
        assert_eq!(mirror, Path::new("/var/aberp/t-1.duckdb.audit.log"));
    }

    #[test]
    fn mirror_path_handles_db_path_without_extension() {
        let db = Path::new("/tmp/tenant-db");
        let mirror = mirror_path_for(db);
        assert_eq!(mirror, Path::new("/tmp/tenant-db.audit.log"));
    }

    #[test]
    fn read_mirror_entries_returns_notfound_when_file_absent() {
        let dir = tempdir_under_target();
        let mirror = dir.join("absent.audit.log");
        let err = read_mirror_entries(&mirror).unwrap_err();
        match err {
            AppendError::MirrorIo(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected MirrorIo(NotFound), got {other:?}"),
        }
        // cleanup: tempdir_under_target leaves the dir; remove it.
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_mirror_entries_rejects_partial_trailing_line() {
        let dir = tempdir_under_target();
        let mirror = dir.join("partial.audit.log");
        std::fs::write(&mirror, b"{\"seq\":1,\"partial-no-newline\":true}").unwrap();
        let err = read_mirror_entries(&mirror).unwrap_err();
        match err {
            AppendError::MirrorCorrupt { reason } => {
                assert!(
                    reason.contains("trailing newline"),
                    "expected partial-line message, got {reason}"
                );
            }
            other => panic!("expected MirrorCorrupt, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_mirror_bootstrap_backfills_existing_db_entries() {
        let dir = tempdir_under_target();
        let mirror = dir.join("bootstrap.audit.log");
        let (conn, meta) = open_conn_with_two_entries();

        // Mirror does not exist yet. First sync should backfill
        // both DB entries.
        let head = sync_mirror(&conn, &meta, &mirror).unwrap();
        assert_eq!(head, 2);

        let entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[1].seq, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_mirror_is_idempotent_when_no_new_entries() {
        let dir = tempdir_under_target();
        let mirror = dir.join("idempotent.audit.log");
        let (conn, meta) = open_conn_with_two_entries();
        let head1 = sync_mirror(&conn, &meta, &mirror).unwrap();
        let head2 = sync_mirror(&conn, &meta, &mirror).unwrap();
        assert_eq!(head1, 2);
        assert_eq!(head2, 2);
        let entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 2, "second sync must not duplicate entries");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_mirror_appends_only_new_entries_on_second_call() {
        let dir = tempdir_under_target();
        let mirror = dir.join("incremental.audit.log");
        let (mut conn, meta) = open_conn_with_two_entries();
        let head_after_first = sync_mirror(&conn, &meta, &mirror).unwrap();
        assert_eq!(head_after_first, 2);

        // Append a third DB entry. Re-sync.
        append_one(&mut conn, &meta, "idem-3", b"payload-3");

        let head_after_second = sync_mirror(&conn, &meta, &mirror).unwrap();
        assert_eq!(head_after_second, 3);

        let entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].seq, 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_mirror_detects_divergence_when_mirror_hash_disagrees_with_db() {
        let dir = tempdir_under_target();
        let mirror = dir.join("divergent.audit.log");
        let (mut conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();

        // Mutate the mirror's entry_hash on seq=2 to simulate
        // divergence. The mirror is plain JSON-Lines, so we
        // re-read, mutate, and re-write.
        let entries = read_mirror_entries(&mirror).unwrap();
        let mut tampered = entries.clone();
        tampered[1].entry_hash = "00".repeat(32);
        let mut tampered_bytes = Vec::new();
        for r in &tampered {
            tampered_bytes.extend_from_slice(&encode_line(r).unwrap());
        }
        std::fs::write(&mirror, &tampered_bytes).unwrap();

        // Append a third DB entry so sync_mirror has a reason to
        // run + a head to check.
        append_one(&mut conn, &meta, "idem-3", b"payload-3");

        let err = sync_mirror(&conn, &meta, &mirror).unwrap_err();
        match err {
            AppendError::MirrorDivergent { seq, .. } => {
                assert_eq!(seq, 2, "divergence should land at the disagreeing seq");
            }
            other => panic!("expected MirrorDivergent, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_mirror_loud_fails_on_partial_trailing_line() {
        let dir = tempdir_under_target();
        let mirror = dir.join("partial-sync.audit.log");
        let (conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();

        // Truncate the trailing newline to simulate an
        // interrupted prior write.
        let bytes = std::fs::read(&mirror).unwrap();
        assert!(bytes.last().copied() == Some(b'\n'));
        std::fs::write(&mirror, &bytes[..bytes.len() - 1]).unwrap();

        let err = sync_mirror(&conn, &meta, &mirror).unwrap_err();
        match err {
            AppendError::MirrorCorrupt { reason } => {
                assert!(reason.contains("trailing newline"));
            }
            other => panic!("expected MirrorCorrupt, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mirror_entry_round_trips_through_jsonl_encoding() {
        // One handcrafted Entry; encode to mirror line; decode
        // back via read_mirror_entries; compare canonical fields.
        let dir = tempdir_under_target();
        let mirror = dir.join("roundtrip.audit.log");
        let (conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();
        let entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 2);
        // Re-encode the first entry's mirror record; the line we
        // get out must exactly match the bytes already on disk
        // (modulo the trailing newline, which encode_line
        // includes).
        let re_encoded = encode_line(&entries[0]).unwrap();
        let file_bytes = std::fs::read(&mirror).unwrap();
        assert!(
            file_bytes.starts_with(&re_encoded),
            "encoded line must match the bytes on disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ──────────────────────────────────────────────────────────────
    // Session 152b — boot-time `ensure_consistent_with_db` recovery.
    // ──────────────────────────────────────────────────────────────

    #[test]
    fn ensure_consistent_creates_empty_mirror_on_fresh_db() {
        // Fresh DB + no mirror file → create (empty) mirror, Created{0}.
        let dir = tempdir_under_target();
        let mirror = dir.join("fresh.audit.log");
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();

        let action = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(action, RecoveryAction::Created { entries_written: 0 });
        assert!(mirror.exists(), "mirror file must be created");
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_creates_mirror_backfilled_from_db() {
        // DB has entries, mirror absent → create + backfill, Created{2}.
        let dir = tempdir_under_target();
        let mirror = dir.join("missing.audit.log");
        let (conn, _meta) = open_conn_with_two_entries();
        assert!(!mirror.exists());

        let action = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(action, RecoveryAction::Created { entries_written: 2 });
        let entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].seq, 1);
        assert_eq!(entries[1].seq, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_unchanged_when_mirror_in_sync() {
        // DB + mirror in sync → Unchanged.
        let dir = tempdir_under_target();
        let mirror = dir.join("insync.audit.log");
        let (conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();

        let action = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(action, RecoveryAction::Unchanged);
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_extends_when_mirror_behind_db() {
        // DB ahead of mirror (mirror was synced, then DB grew) →
        // replay missing entries, Extended{count}.
        let dir = tempdir_under_target();
        let mirror = dir.join("behind.audit.log");
        let (mut conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 2);

        // DB grows to 4 while the mirror stays at 2.
        append_one(&mut conn, &meta, "idem-3", b"payload-3");
        append_one(&mut conn, &meta, "idem-4", b"payload-4");

        let action = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(action, RecoveryAction::Extended { entries_added: 2 });
        let entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[3].seq, 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Return the single side-file in `dir` whose name contains `needle`
    /// (e.g. `.ahead-` or `.corrupt-`), or `None`. Proves an evidence backup
    /// was written by a preserve arm.
    fn find_one_backup(dir: &Path, needle: &str) -> Option<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.contains(needle))
                    .unwrap_or(false)
            })
    }

    #[test]
    fn ensure_consistent_refuses_and_preserves_when_mirror_ahead_of_db() {
        // H1 arm (a) — THE dev-DB-nuke / lost-DB-commit case: operator rm'd the
        // DuckDB but left the mirror in place. Old DB had 2 entries; mirror
        // synced to them. A FRESH DB now holds ONE new entry → mirror is AHEAD
        // by 1. Boot must NOT truncate: it PRESERVES the ahead mirror to a
        // `.ahead-<nanos>.bak` side file, leaves the original intact, and
        // REFUSES with `MirrorAheadOfDb` (boot exits non-zero at the serve
        // call site).
        let dir = tempdir_under_target();
        let mirror = dir.join("ahead.audit.log");
        let (conn_old, meta_old) = open_conn_with_two_entries();
        sync_mirror(&conn_old, &meta_old, &mirror).unwrap();
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 2);
        let original = std::fs::read(&mirror).unwrap();

        let mut conn_fresh = Connection::open_in_memory().unwrap();
        ensure_schema(&conn_fresh).unwrap();
        let meta_fresh = mk_meta();
        append_one(&mut conn_fresh, &meta_fresh, "fresh-1", b"fresh-payload-1");

        let err = ensure_consistent_with_db(&conn_fresh, &mirror).unwrap_err();
        match err {
            AppendError::MirrorAheadOfDb {
                mirror_max_seq,
                db_max_seq,
                preserved,
            } => {
                assert_eq!(mirror_max_seq, 2);
                assert_eq!(db_max_seq, 1);
                assert!(
                    preserved.contains(".ahead-"),
                    "preserved path names the .ahead bak"
                );
            }
            other => panic!("expected MirrorAheadOfDb, got {other:?}"),
        }

        // Evidence preserved: a `.ahead-*.bak` exists AND equals the original.
        let bak = find_one_backup(&dir, ".ahead-").expect("ahead backup written");
        assert_eq!(
            std::fs::read(&bak).unwrap(),
            original,
            "backup is byte-for-byte"
        );
        // Original mirror is UNTOUCHED — never truncated.
        assert_eq!(
            std::fs::read(&mirror).unwrap(),
            original,
            "mirror left intact"
        );
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_refuses_and_preserves_on_head_hash_mismatch() {
        // H1 arm (c) — equal length but mirror head entry_hash disagrees. Only
        // the HEAD entry_hash is tampered (the inter-entry chain LINK still
        // holds, so the prefix re-verifies as "clean"), which is exactly what
        // routes it to the equal-length head-hash comparison. Preserve + REFUSE
        // with `MirrorCorruptPreserved` — never a silent rebuild-from-DB.
        let dir = tempdir_under_target();
        let mirror = dir.join("mismatch.audit.log");
        let (conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();

        let entries = read_mirror_entries(&mirror).unwrap();
        let mut tampered = entries.clone();
        tampered[1].entry_hash = "00".repeat(32);
        let mut bytes = Vec::new();
        for r in &tampered {
            bytes.extend_from_slice(&encode_line(r).unwrap());
        }
        std::fs::write(&mirror, &bytes).unwrap();
        let tampered_bytes = std::fs::read(&mirror).unwrap();

        let err = ensure_consistent_with_db(&conn, &mirror).unwrap_err();
        match err {
            AppendError::MirrorCorruptPreserved { preserved, reason } => {
                assert!(preserved.contains(".corrupt-"));
                assert!(
                    reason.contains("equal length"),
                    "reason names the equal-length divergence, got {reason}"
                );
            }
            other => panic!("expected MirrorCorruptPreserved, got {other:?}"),
        }
        // Evidence preserved AND the (tampered) original left intact — never
        // rebuilt from the DB.
        let bak = find_one_backup(&dir, ".corrupt-").expect("corrupt backup written");
        assert_eq!(std::fs::read(&bak).unwrap(), tampered_bytes);
        assert_eq!(
            std::fs::read(&mirror).unwrap(),
            tampered_bytes,
            "mirror not rebuilt"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_trims_torn_tail_and_continues() {
        // H1 arm (b) — a lone torn trailing line (interrupted append) whose
        // intact prefix the DB head COVERS. Boot PRESERVES the original to a
        // `.corrupt-<nanos>.bak`, durably TRIMS the torn tail, and CONTINUES:
        // the trimmed prefix (seq 1) is behind the DB (seq 2), so the reconcile
        // re-extends it — action = Extended{1}, mirror ends consistent with DB.
        // NEVER a silent rebuild-from-DB.
        let dir = tempdir_under_target();
        let mirror = dir.join("torn.audit.log");
        let (conn, meta) = open_conn_with_two_entries();
        sync_mirror(&conn, &meta, &mirror).unwrap();

        // Strip the final newline → the seq-2 line becomes a torn tail.
        let bytes = std::fs::read(&mirror).unwrap();
        assert_eq!(bytes.last().copied(), Some(b'\n'));
        std::fs::write(&mirror, &bytes[..bytes.len() - 1]).unwrap();
        let torn_bytes = std::fs::read(&mirror).unwrap();

        let action = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(action, RecoveryAction::Extended { entries_added: 1 });

        // Evidence preserved: the ORIGINAL torn bytes are in the .corrupt bak.
        let bak = find_one_backup(&dir, ".corrupt-").expect("corrupt backup written");
        assert_eq!(
            std::fs::read(&bak).unwrap(),
            torn_bytes,
            "backup holds the torn original"
        );

        // The live mirror is now consistent with the DB (2 entries, both heads).
        let final_entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(final_entries.len(), 2);
        assert_eq!(final_entries[1].seq, 2);
        let db_head = read_db_entry_at_seq(&conn, 2).unwrap().unwrap();
        assert_eq!(
            final_entries[1].entry_hash,
            hex::encode(db_head.entry_hash.as_bytes())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_refuses_torn_tail_prefix_still_ahead_without_trimming() {
        // H1 Bug-3 PRE-FIX proof — a torn tail whose intact prefix is STILL
        // AHEAD of the DB must NOT have its live file trimmed. Old DB has 3
        // entries; mirror synced to 3, then its final newline is stripped (seq-3
        // torn tail; intact prefix = seq 1..=2). A FRESH DB holds only 1 entry.
        // The trimmed prefix head (seq 2) is still ahead of the DB (seq 1) →
        // preserve `.ahead-*.bak` + REFUSE, and the on-disk mirror is left
        // BYTE-FOR-BYTE untouched (editions trimmed here before the DB-head gate;
        // prod does not).
        let dir = tempdir_under_target();
        let mirror = dir.join("torn-ahead.audit.log");
        let (mut conn_old, meta_old) = open_conn_with_two_entries();
        append_one(&mut conn_old, &meta_old, "idem-3", b"payload-3");
        sync_mirror(&conn_old, &meta_old, &mirror).unwrap();
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 3);

        let bytes = std::fs::read(&mirror).unwrap();
        std::fs::write(&mirror, &bytes[..bytes.len() - 1]).unwrap();
        let torn_bytes = std::fs::read(&mirror).unwrap();

        let mut conn_fresh = Connection::open_in_memory().unwrap();
        ensure_schema(&conn_fresh).unwrap();
        let meta_fresh = mk_meta();
        append_one(&mut conn_fresh, &meta_fresh, "fresh-1", b"fresh-payload-1");

        let err = ensure_consistent_with_db(&conn_fresh, &mirror).unwrap_err();
        match err {
            AppendError::MirrorAheadOfDb {
                mirror_max_seq,
                db_max_seq,
                ..
            } => {
                assert_eq!(mirror_max_seq, 2, "trimmed-prefix head is seq 2");
                assert_eq!(db_max_seq, 1);
            }
            other => panic!("expected MirrorAheadOfDb, got {other:?}"),
        }
        // The Bug-3 pre-fix: the live mirror was NOT trimmed on a refuse.
        assert_eq!(
            std::fs::read(&mirror).unwrap(),
            torn_bytes,
            "mirror must be byte-for-byte untouched when the torn-tail prefix is still ahead"
        );
        assert!(
            find_one_backup(&dir, ".ahead-").is_some(),
            "ahead evidence preserved"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_refuses_and_preserves_on_deep_corrupt_chain_break() {
        // H1 arm (b, deep) — corruption DEEPER than a lone torn tail: an
        // inter-entry hash-chain BREAK in the middle of the file (seq-2
        // prev_hash no longer matches seq-1 entry_hash), file fully terminated.
        // The genesis→head re-verification catches it → DeepCorrupt → preserve +
        // REFUSE with `MirrorCorruptPreserved`. NEVER rebuild-from-DB.
        let dir = tempdir_under_target();
        let mirror = dir.join("deep.audit.log");
        let (mut conn, meta) = open_conn_with_two_entries();
        append_one(&mut conn, &meta, "idem-3", b"payload-3");
        sync_mirror(&conn, &meta, &mirror).unwrap();

        let entries = read_mirror_entries(&mirror).unwrap();
        let mut tampered = entries.clone();
        // Break the LINK at seq 2 (not just the head) so it is a mid-file break,
        // not an equal-length head-hash divergence.
        tampered[1].prev_hash = "00".repeat(32);
        let mut bytes = Vec::new();
        for r in &tampered {
            bytes.extend_from_slice(&encode_line(r).unwrap());
        }
        std::fs::write(&mirror, &bytes).unwrap();
        let corrupt_bytes = std::fs::read(&mirror).unwrap();

        let err = ensure_consistent_with_db(&conn, &mirror).unwrap_err();
        match err {
            AppendError::MirrorCorruptPreserved { preserved, reason } => {
                assert!(preserved.contains(".corrupt-"));
                assert!(
                    reason.contains("hash-chain break"),
                    "reason names the chain break, got {reason}"
                );
            }
            other => panic!("expected MirrorCorruptPreserved, got {other:?}"),
        }
        let bak = find_one_backup(&dir, ".corrupt-").expect("corrupt backup written");
        assert_eq!(std::fs::read(&bak).unwrap(), corrupt_bytes);
        assert_eq!(
            std::fs::read(&mirror).unwrap(),
            corrupt_bytes,
            "mirror not rebuilt"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decide_tail_truth_table() {
        // The pure R1 torn-tail branch — the four (terminated, prefix_ok)
        // combinations map to exactly one disposition. No filesystem / DuckDB.
        assert_eq!(decide_tail(true, true), TailDecision::Clean);
        assert_eq!(decide_tail(false, true), TailDecision::TornTail);
        assert_eq!(decide_tail(true, false), TailDecision::Deep);
        assert_eq!(decide_tail(false, false), TailDecision::Deep);
    }

    #[test]
    fn parse_and_reverify_prefix_flags_chain_break() {
        // The genesis→head re-verification rejects an inter-entry hash-chain
        // break, a seq jump, and empty/garbage lines; an intact prefix passes.
        let (conn, meta) = open_conn_with_two_entries();
        let dir = tempdir_under_target();
        let mirror = dir.join("prefix.audit.log");
        sync_mirror(&conn, &meta, &mirror).unwrap();
        let good = std::fs::read(&mirror).unwrap();
        // Intact prefix re-verifies.
        assert!(parse_and_reverify_prefix(&good).is_ok());

        // Break the link at seq 2.
        let entries = read_mirror_entries(&mirror).unwrap();
        let mut tampered = entries.clone();
        tampered[1].prev_hash = "00".repeat(32);
        let mut bad = Vec::new();
        for r in &tampered {
            bad.extend_from_slice(&encode_line(r).unwrap());
        }
        let err = parse_and_reverify_prefix(&bad).unwrap_err();
        assert!(err.contains("hash-chain break"), "got {err}");

        // A seq jump is also rejected.
        let mut jumped = entries.clone();
        jumped[1].seq = 5;
        let mut jbytes = Vec::new();
        for r in &jumped {
            jbytes.extend_from_slice(&encode_line(r).unwrap());
        }
        assert!(parse_and_reverify_prefix(&jbytes)
            .unwrap_err()
            .contains("seq jump"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_is_idempotent() {
        // Run twice: first Created, second Unchanged.
        let dir = tempdir_under_target();
        let mirror = dir.join("idem-recover.audit.log");
        let (conn, _meta) = open_conn_with_two_entries();

        let first = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(first, RecoveryAction::Created { entries_written: 2 });
        let second = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(second, RecoveryAction::Unchanged);
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── ADR-audit-armor — the gated auto-heal (positive) + the fork/tamper
    //    refusals (negative). MF-1 (the in-tx full-genesis re-verify) is THE
    //    fork-detector; the discriminators are only an early reject.

    /// Build a BENIGN mirror-ahead state: DB `[1..=2]`, mirror `[1..=2..=extra]`
    /// where the extra rows are a valid continuation the DB lost to a WAL fold
    /// (synced while the DB held them, then rolled back). Returns
    /// `(conn, mirror_path, dir)`.
    fn benign_ahead(name: &str, extra: usize) -> (Connection, PathBuf, PathBuf) {
        let dir = tempdir_under_target();
        let mirror = dir.join(format!("{name}.audit.log"));
        let (mut conn, meta) = open_conn_with_two_entries(); // DB [1,2]
        sync_mirror(&conn, &meta, &mirror).unwrap(); // mirror [1,2]
        for i in 0..extra {
            append_one(
                &mut conn,
                &meta,
                &format!("ahead-{i}"),
                format!("ahead-payload-{i}").as_bytes(),
            );
        }
        sync_mirror(&conn, &meta, &mirror).unwrap(); // mirror [1,2,..2+extra]
                                                     // WAL-fold tear: the DB loses everything above seq 2; the mirror keeps it.
        conn.execute_batch("DELETE FROM audit_ledger WHERE seq > 2;")
            .unwrap();
        assert_eq!(read_db_max_seq(&conn).unwrap(), 2);
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 2 + extra);
        (conn, mirror, dir)
    }

    #[test]
    fn ensure_consistent_heals_benign_ahead_and_is_idempotent() {
        // The reproduced WAL-fold tear, healed: mirror ahead by 1 over a boundary
        // the DB agrees with and a tail that verifies → the heal replays the lost
        // row (+ a db.auto_recovered forensic row) and CONTINUES, leaving db ==
        // mirror. Second boot is Unchanged (idempotent — negative test (d)).
        let (conn, mirror, dir) = benign_ahead("heal", 1);

        let action = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(
            action,
            RecoveryAction::Healed {
                entries_replayed: 1
            },
            "a benign mirror-ahead must HEAL, not refuse"
        );

        // db == mirror, and both carry the replayed row + the forensic row.
        let db = read_db_entries_after(&conn, 0).unwrap();
        let mir = read_mirror_entries(&mirror).unwrap();
        assert_eq!(db.len(), 4, "DB = [1,2,3 replayed] + db.auto_recovered @4");
        assert_eq!(mir.len(), 4, "mirror re-synced to match the healed DB");
        assert_eq!(
            db.last().unwrap().kind.as_str(),
            "db.auto_recovered",
            "the heal emits a db.auto_recovered forensic row"
        );
        for (d, m) in db.iter().zip(mir.iter()) {
            assert_eq!(d.seq.as_u64(), m.seq);
            assert_eq!(
                hex::encode(d.entry_hash.as_bytes()),
                m.entry_hash,
                "db == mirror at seq {}",
                m.seq
            );
        }
        // The ahead mirror was preserved BEFORE the heal touched the DB.
        assert!(
            find_one_backup(&dir, ".healed-").is_some(),
            "ahead mirror preserved to .healed-*.bak before the heal"
        );

        // (d) idempotency — second boot is Unchanged, no loop, no further growth.
        let again = ensure_consistent_with_db(&conn, &mirror).unwrap();
        assert_eq!(
            again,
            RecoveryAction::Unchanged,
            "second boot must not loop"
        );
        assert_eq!(read_mirror_entries(&mirror).unwrap().len(), 4);
        assert_eq!(read_db_entries_after(&conn, 0).unwrap().len(), 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_refuses_duplicate_seq_fork_at_boundary() {
        // NEGATIVE (a) — THE fork the adversary found: a DUPLICATE row at
        // db_max_seq whose head hash MATCHES the mirror (there is no
        // UNIQUE(seq)). The boundary discriminator (`WHERE seq=? LIMIT 1`, no
        // ORDER BY) samples the honest row and PASSES — yet the heal must still
        // refuse. Only the in-tx full-genesis re-verify (MF-1) catches it: the
        // duplicate seq trips OutOfOrder walking SELECT_ALL. This is the test
        // that proves MF-1 — not the discriminators — is the fork-detector.
        let (conn, mirror, dir) = benign_ahead("dup-seq", 1);
        // Inject a byte-identical duplicate of seq 2 → rows at seqs {1, 2, 2}.
        conn.execute_batch("INSERT INTO audit_ledger SELECT * FROM audit_ledger WHERE seq = 2;")
            .unwrap();

        let err = ensure_consistent_with_db(&conn, &mirror).unwrap_err();
        match err {
            AppendError::MirrorAheadOfDb {
                mirror_max_seq,
                db_max_seq,
                preserved,
            } => {
                assert_eq!(mirror_max_seq, 3);
                assert_eq!(db_max_seq, 2);
                assert!(preserved.contains(".ahead-"));
            }
            other => panic!("expected MirrorAheadOfDb (fork masked → refuse), got {other:?}"),
        }
        assert!(find_one_backup(&dir, ".ahead-").is_some());
        // The DB was NOT healed: still {1,2,2}, no seq-3 / forensic row committed.
        let db = read_db_entries_after(&conn, 0).unwrap();
        assert_eq!(db.len(), 3, "duplicate fork left intact; heal rolled back");
        assert!(
            db.iter().all(|e| e.kind.as_str() != "db.auto_recovered"),
            "no forensic row on a refused heal"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_refuses_boundary_hash_mismatch() {
        // NEGATIVE (b) — the DB's boundary entry_hash DISAGREES with the mirror's
        // at db_max_seq (the chains forked at/below the boundary). Discriminator 1
        // early-rejects it; the in-tx re-verify would too. Must refuse.
        let (conn, mirror, dir) = benign_ahead("boundary", 1);
        // Tamper the DB boundary (seq 2) entry_hash so it no longer matches the
        // mirror — set it to seq 1's hash (no BLOB literal / params needed).
        conn.execute_batch(
            "UPDATE audit_ledger \
             SET entry_hash = (SELECT entry_hash FROM audit_ledger WHERE seq = 1) \
             WHERE seq = 2;",
        )
        .unwrap();

        let err = ensure_consistent_with_db(&conn, &mirror).unwrap_err();
        match err {
            AppendError::MirrorAheadOfDb {
                mirror_max_seq,
                db_max_seq,
                preserved,
            } => {
                assert_eq!(mirror_max_seq, 3);
                assert_eq!(db_max_seq, 2);
                assert!(preserved.contains(".ahead-"));
            }
            other => panic!("expected MirrorAheadOfDb (boundary fork → refuse), got {other:?}"),
        }
        // Not healed — no forensic row, DB still 2 rows.
        let db = read_db_entries_after(&conn, 0).unwrap();
        assert_eq!(db.len(), 2);
        assert!(db.iter().all(|e| e.kind.as_str() != "db.auto_recovered"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_consistent_refuses_tampered_tail_content() {
        // NEGATIVE (c) / MF-3 — a mirror tail row with VALID chain LINKS but
        // tampered CONTENT (payload changed; prev_hash + stored entry_hash left
        // honest). The link-only `parse_and_reverify_prefix` (which classifies
        // the mirror as Clean) MISSES it — proving why the heal must decode to
        // Entry and run the FULL verify_chain, which recomputes entry_hash and
        // trips TamperedAt. Must refuse.
        let (conn, mirror, dir) = benign_ahead("tamper-tail", 1);
        // Rewrite the mirror: change seq 3's payload only, keeping its stored
        // entry_hash + prev_hash (valid links, wrong content hash).
        let mut entries = read_mirror_entries(&mirror).unwrap();
        assert_eq!(entries.len(), 3);
        entries[2].payload = BASE64_STANDARD.encode(b"TAMPERED-content-not-matching-hash");
        let mut bytes = Vec::new();
        for r in &entries {
            bytes.extend_from_slice(&encode_line(r).unwrap());
        }
        std::fs::write(&mirror, &bytes).unwrap();

        let err = ensure_consistent_with_db(&conn, &mirror).unwrap_err();
        match err {
            AppendError::MirrorAheadOfDb {
                mirror_max_seq,
                db_max_seq,
                preserved,
            } => {
                assert_eq!(mirror_max_seq, 3);
                assert_eq!(db_max_seq, 2);
                assert!(preserved.contains(".ahead-"));
            }
            other => panic!("expected MirrorAheadOfDb (tampered tail → refuse), got {other:?}"),
        }
        let db = read_db_entries_after(&conn, 0).unwrap();
        assert_eq!(db.len(), 2, "tampered tail not replayed");
        assert!(db.iter().all(|e| e.kind.as_str() != "db.auto_recovered"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `CARGO_TARGET_TMPDIR` is the canonical per-crate temp dir
    /// for tests. Falls back to `std::env::temp_dir()` if unset
    /// (e.g., out-of-cargo invocations). Returns a fresh
    /// subdirectory unique to this test invocation.
    ///
    /// The suffix combines `process::id()` (cross-process guard,
    /// so parallel integration-test binaries sharing
    /// `CARGO_TARGET_TMPDIR` do not collide) with a monotonic
    /// `AtomicUsize` (within-process guard, so parallel
    /// `#[test]` threads do not collide). A `SystemTime`-based
    /// suffix is not safe here: two threads can sample the same
    /// nanosecond on a fast machine and produce the same path.
    fn tempdir_under_target() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let unique = format!(
            "aberp-mirror-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        );
        let dir = base.join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
