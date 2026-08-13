//! **ADR-0110 D5 — the durability-alert marker: a NON-CHAINED sidecar for
//! machine-spawned durability diagnostics.**
//!
//! # Why this exists (D5-B1, Ervin 2026-08-13 — route (a))
//!
//! D5's first cut recorded a mirror freeze by appending a
//! `db.durability_loss_detected` row to the hash-chained `audit_ledger`. That
//! was wrong, and it was wrong in a way that could brick the tenant.
//!
//! The freeze is detected precisely when the DB head has REGRESSED below the
//! append-only mirror's. Appending anything to the ledger at that moment
//! consumes the next DB `seq` — a seq the mirror already holds a *different*
//! entry for. The two chains fork at that seq, and the next boot's gated
//! auto-heal (which proves benignness by matching the DB head's `entry_hash`
//! against the mirror's at the same seq) then REFUSES. `serve` exits non-zero
//! and does not boot. The diagnostic that says "stop and recover" would have
//! been the thing that stopped the operator recovering.
//!
//! The rule this store encodes: **a machine-spawned durability diagnostic is
//! not a business event and must never consume a ledger seq.** The ledger is
//! for operator and workflow events. An alarm about the ledger's own substrate
//! cannot live inside it.
//!
//! # Shape
//!
//! `<db>.durability-alert`, append-only, one record per line:
//!
//! ```text
//! v1<TAB>loss<TAB><RFC3339><TAB><trigger><TAB><breach-code><TAB><u64 detail>
//! v1<TAB>ack<TAB><RFC3339>
//! ```
//!
//! Every field is a `&'static str` from a closed vocabulary, an RFC3339 instant
//! this crate formatted, or a `u64`. **No free-form string ever reaches this
//! file** — no path, no operator login, no formatted error — so there is
//! nothing to escape and no way to forge a record by writing one. That is why
//! the format is tab-separated rather than JSON: with no free-form fields, JSON
//! buys only a parser (and this crate has no `serde` dependency to spend on
//! one).
//!
//! The attributable record of an acknowledgement is still the hash-chained
//! `db.durability_alert_acknowledged` ledger row — an operator act, which
//! belongs on the chain. The `ack` line here is the alert STATE, not the
//! evidence, and it is appended rather than deleting the loss line: clearing a
//! banner must not erase the record of what raised it.
//!
//! # Durability and concurrency
//!
//! Each append is `write_all` + `flush` + `sync_all` on a handle opened
//! `create(true).append(true)`. Under POSIX `O_APPEND` a single small write is
//! atomic, so concurrent appenders interleave whole lines and never tear one —
//! the same argument `sync_mirror` makes for the audit mirror, and the reason
//! this needs no lock of its own. (Cross-process there is already the ADR-0099
//! F-E whole-DB writer flock; in-process the loss append happens under the
//! writer mutex.)
//!
//! It is deliberately NOT registered in [`crate::Handle::fsynced_paths`]: that
//! journal exists to let the ADR-0110 D6b power-loss tier derive the DB's
//! durable set, and this file is not part of the database.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::WalBreach;

/// Record format version. Present so a future format can be recognised rather
/// than mis-parsed; unknown versions are skipped by the reader (loudly).
const FORMAT_VERSION: &str = "v1";
const EVENT_LOSS: &str = "loss";
const EVENT_ACK: &str = "ack";

/// `<db>.durability-alert` — the marker beside the tenant DB, derived exactly
/// the way [`aberp_audit_ledger::mirror_path_for`] derives the mirror. One
/// derivation, one place: a second copy of this rule would be a second store.
pub(crate) fn marker_path_for(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_owned();
    os.push(".durability-alert");
    PathBuf::from(os)
}

/// What the marker says right now.
#[derive(Debug, Default)]
pub(crate) struct MarkerState {
    /// Newest loss episode: when, and which breach shape raised it. The breach
    /// is `None` only if the file carries a code this build does not know.
    pub(crate) loss: Option<(OffsetDateTime, Option<WalBreach>)>,
    /// Newest acknowledgement.
    pub(crate) ack: Option<OffsetDateTime>,
}

/// Append a loss episode.
///
/// `trigger` and `breach` are `&'static str` by signature, not by convention —
/// that is what keeps the no-free-form-strings property from eroding.
pub(crate) fn record_loss(
    path: &Path,
    at: OffsetDateTime,
    trigger: &'static str,
    breach: WalBreach,
    detail: u64,
) -> std::io::Result<()> {
    let at = at
        .format(&Rfc3339)
        .map_err(|e| std::io::Error::other(format!("format marker timestamp: {e}")))?;
    append_line(
        path,
        &format!(
            "{FORMAT_VERSION}\t{EVENT_LOSS}\t{at}\t{trigger}\t{}\t{detail}\n",
            breach.code()
        ),
    )
}

/// Append an acknowledgement, taking the banner down for good.
pub(crate) fn record_ack(path: &Path, at: OffsetDateTime) -> std::io::Result<()> {
    let at = at
        .format(&Rfc3339)
        .map_err(|e| std::io::Error::other(format!("format marker timestamp: {e}")))?;
    append_line(path, &format!("{FORMAT_VERSION}\t{EVENT_ACK}\t{at}\n"))
}

/// Append one record, first TERMINATING a torn one if the file ends mid-record.
///
/// The separator matters as much as the record. A crash can cut a write, and
/// then the file does not end in a newline; appending straight onto that
/// SPLICES the two records into one line that parses as neither — which loses
/// the record being written. When the record being written is the operator's
/// acknowledgement, that turns a torn marker into an alarm that genuinely
/// cannot be cleared: the exact failure [`read`]'s UNIX_EPOCH stamp exists to
/// avoid, reintroduced one layer down. Found by
/// `a_torn_marker_record_is_still_acknowledgeable`.
///
/// The torn record is terminated, never rewritten or dropped: it stays on its
/// own line, and [`read`] still counts it as a loss. Evidence is preserved and
/// the new record is readable. A crash between the two writes leaves the file
/// newline-terminated with the new record absent, which is the same safe state
/// as never having started.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    let len = f.metadata()?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::End(-1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            // `O_APPEND` writes go to the end regardless of the read cursor.
            f.write_all(b"\n")?;
        }
    }
    f.write_all(line.as_bytes())?;
    f.flush()?;
    f.sync_all()
}

/// Read the marker.
///
/// # Failure posture
///
/// A missing file is the normal case (every healthy tenant) and reads as
/// `Default`. Everything else fails toward the banner being UP, because the one
/// thing this file must never allow is for damaging it to be a way to silence
/// the alarm.
///
/// The rule, stated as the code implements it: **a line counts as an
/// acknowledgement only if it parses completely as one. Every other non-empty
/// line counts as a loss.** Blank lines are skipped.
///
/// That is stricter than "skip what we do not recognise", and R5-N1 is why.
/// A record is written with one `write_all`, but a crash can still cut it: of
/// the 75 truncation points on a real loss record, six (`v`, `v1`, `v1<TAB>`,
/// `v1<TAB>l`, `v1<TAB>lo`, `v1<TAB>los`) leave a fragment whose *event field*
/// is incomplete. Skipping those with a `warn!` — the first cut — meant a
/// torn FIRST record left the banner DOWN, the exact opposite of what this
/// section promised. Anything that is not a complete `ack` is now a loss, which
/// covers every one of those six and every future format this build cannot
/// read. A torn `ack` counts as a loss too, and correctly: an acknowledgement
/// whose write was interrupted is not a durable acknowledgement.
///
/// # Two failure classes, and only one of them is clearable
///
/// **Damaged CONTENT — clearable.** A torn line, an unrecognised record, an
/// unparseable timestamp: each contributes a loss at
/// [`OffsetDateTime::UNIX_EPOCH`]. UNIX_EPOCH rather than "now" on purpose —
/// the banner goes up, and it stays **acknowledgeable**, because any later
/// `ack` out-ranks an instant nothing can precede. An alarm that cannot be
/// cleared is one an operator routes around.
///
/// **An unreadable or unwritable FILE — NOT clearable, deliberately** (R5-N2).
/// If the file exists but cannot be read, this reports a loss (banner up) and
/// no acknowledgement can be seen. And since the same fault almost always
/// blocks [`record_ack`], [`crate::Handle::clear_durability_alert`] returns
/// `Err` and the banner stays up until the filesystem fault is fixed. That is
/// not a silent stick, and it is not a bug to route around: **clearing a real
/// loss without a durable acknowledgement is the amnesia D7.4b closed**, so
/// when the durable half cannot be written, the only honest answer is to keep
/// the banner up and say so. The condition is an operator-attention filesystem
/// fault on a path next to the tenant DB — permissions, a full or read-only
/// volume, something else occupying the path — every occurrence is logged at
/// ERROR with the path, and fixing it makes the alert immediately
/// acknowledgeable again. Do not "fix" this by clearing on a failed write.
pub(crate) fn read(path: &Path) -> MarkerState {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return MarkerState::default(),
        Err(e) => {
            tracing::error!(
                error = %e,
                marker = %path.display(),
                "aberp-db: ADR-0110 D5 — the durability-alert marker EXISTS but could not be \
                 read. Treating it as an unacknowledged loss: an unreadable alarm must not be \
                 a silent one."
            );
            return MarkerState {
                loss: Some((OffsetDateTime::UNIX_EPOCH, None)),
                ack: None,
            };
        }
    };

    let mut state = MarkerState::default();
    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(l) => l,
            // A read error on the STREAM, not on one record: count the loss and
            // STOP. `continue` here would be an infinite loop — the errors that
            // reach this arm are properties of the file, not of the line, so
            // the next read returns the same error forever (a directory
            // occupying the marker path yields `EISDIR` on every read, which is
            // exactly how this was found). Nothing after an unreadable point is
            // trustworthy anyway.
            Err(e) => {
                tracing::error!(
                    error = %e,
                    marker = %path.display(),
                    "aberp-db: ADR-0110 D5 — the durability-alert marker became unreadable \
                     mid-file; counting it as an unacknowledged loss and stopping the scan"
                );
                note_loss(&mut state, OffsetDateTime::UNIX_EPOCH, None);
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        match (f.first().copied(), f.get(1).copied()) {
            (Some(FORMAT_VERSION), Some(EVENT_LOSS)) => {
                match f
                    .get(2)
                    .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                {
                    Some(at) => {
                        let breach = f.get(4).and_then(|c| WalBreach::from_code(c));
                        note_loss(&mut state, at, breach);
                    }
                    None => {
                        tracing::error!(
                            marker = %path.display(),
                            "aberp-db: ADR-0110 D5 — a loss line in the durability-alert marker \
                             has an unparseable timestamp; counting it as an unacknowledged loss \
                             rather than dropping it"
                        );
                        note_loss(&mut state, OffsetDateTime::UNIX_EPOCH, None);
                    }
                }
            }
            (Some(FORMAT_VERSION), Some(EVENT_ACK)) => {
                match f
                    .get(2)
                    .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                {
                    Some(at) => {
                        if state.ack.is_none_or(|prev| at > prev) {
                            state.ack = Some(at);
                        }
                    }
                    // A TORN acknowledgement is not an acknowledgement. Its
                    // write was interrupted, so the durable half of taking the
                    // banner down never landed — counting it as a loss keeps
                    // the banner up until the operator acknowledges again.
                    None => {
                        tracing::error!(
                            marker = %path.display(),
                            "aberp-db: ADR-0110 D5 — an ack line in the durability-alert marker \
                             has an unparseable timestamp, so its write was interrupted; \
                             counting it as an unacknowledged loss rather than as a clear"
                        );
                        note_loss(&mut state, OffsetDateTime::UNIX_EPOCH, None);
                    }
                }
            }
            // R5-N1 — everything else is a LOSS, not a skip. A crash can cut a
            // record before its event field completes (`v`, `v1`, `v1<TAB>lo`,
            // …), and skipping those left a torn FIRST record with the banner
            // DOWN — the opposite of this module's stated posture. A record
            // this build cannot positively read as an acknowledgement is
            // treated as "something happened here", which is both true and the
            // safe direction; it stays acknowledgeable because UNIX_EPOCH is
            // out-ranked by any real `ack`.
            _ => {
                tracing::error!(
                    marker = %path.display(),
                    "aberp-db: ADR-0110 D5 — a record in the durability-alert marker is TORN or \
                     unrecognised (an interrupted write, or a newer format). Counting it as an \
                     unacknowledged loss: damaging this file must never be a way to take the \
                     banner down."
                );
                note_loss(&mut state, OffsetDateTime::UNIX_EPOCH, None);
            }
        }
    }
    state
}

fn note_loss(state: &mut MarkerState, at: OffsetDateTime, breach: Option<WalBreach>) {
    if state.loss.is_none_or(|(prev, _)| at > prev) {
        state.loss = Some((at, breach));
    }
}
