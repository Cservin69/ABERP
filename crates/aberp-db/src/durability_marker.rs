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

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    f.flush()?;
    f.sync_all()
}

/// Read the marker.
///
/// # Failure posture
///
/// A missing file is the normal case (every healthy tenant) and reads as
/// `Default`. An unreadable file, or a line that will not parse, must NOT be
/// able to take the banner down — that would make corrupting one line a way to
/// silence the alarm. So both are reported as a loss:
///
/// * an unreadable file yields a loss at [`OffsetDateTime::UNIX_EPOCH`];
/// * an unparseable `loss` line contributes a loss at the same instant.
///
/// UNIX_EPOCH rather than "now" on purpose. It fails toward the banner being
/// UP, but it stays **acknowledgeable**: a later `ack` line out-ranks it. An
/// alarm that cannot be cleared is one an operator routes around, which is the
/// failure mode the acknowledge route exists to prevent.
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
            Err(e) => {
                tracing::error!(
                    error = %e,
                    marker = %path.display(),
                    "aberp-db: ADR-0110 D5 — unreadable line in the durability-alert marker; \
                     counting it as an unacknowledged loss"
                );
                note_loss(&mut state, OffsetDateTime::UNIX_EPOCH, None);
                continue;
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
                if let Some(at) = f
                    .get(2)
                    .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                {
                    if state.ack.is_none_or(|prev| at > prev) {
                        state.ack = Some(at);
                    }
                }
                // An unparseable ACK is simply not counted. That direction is
                // safe by itself: the worst case is a banner that stays up.
            }
            _ => tracing::warn!(
                marker = %path.display(),
                "aberp-db: ADR-0110 D5 — unrecognised record in the durability-alert marker \
                 (a newer format?); skipping it"
            ),
        }
    }
    state
}

fn note_loss(state: &mut MarkerState, at: OffsetDateTime, breach: Option<WalBreach>) {
    if state.loss.is_none_or(|(prev, _)| at > prev) {
        state.loss = Some((at, breach));
    }
}
