//! S171 / PR-171 — boot-time upgrade-snapshot check.
//!
//! `tools/snapshot-prod.sh` is the operator's pre-upgrade safety
//! script. Among other things it drops a file at
//! `~/.aberp/<tenant>/.upgrade-snapshot.toml` containing the
//! `[seller.smtp]` and `[seller.numbering]` sections extracted from
//! the current `seller.toml`. The next boot of the ABERP binary
//! reads that file, compares the two sections against the current
//! `seller.toml`, and:
//!
//!   * **Absent** — no snapshot file present → no-op (this is the
//!     normal case: not every boot follows an upgrade).
//!   * **Matches** — sections are byte-equal at the field level →
//!     boot proceeds, snapshot file is renamed to
//!     `.upgrade-snapshot.toml.verified-<ISO-timestamp>` so the next
//!     boot does not re-trigger it.
//!   * **Mismatch** — at least one load-bearing field differs → boot
//!     is refused with a bilingual error AND a permanent
//!     `UpgradeSnapshotMismatch` audit entry is written before
//!     exit(1). The operator either restores from the snapshot
//!     tarball (`tools/snapshot-prod.sh` produces one) OR explicitly
//!     acknowledges the drift by `mv`-ing the snapshot file to
//!     `.upgrade-snapshot.toml.acknowledged-<timestamp>`.
//!
//! Principle ([[trust-code-not-operator]]): safety properties belong
//! in code, never in operator discipline. The S170 root cause was an
//! identity-write surface that silently dropped `[seller.smtp]` +
//! `[seller.numbering]`; PR-170 fixed the write path, this PR adds
//! the boot-time guard so any FUTURE regression in any write surface
//! is caught at upgrade time without requiring the operator to
//! remember to verify.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::numbering::{self, NumberingTemplate, ResetPolicy, Segment, YearDigits};
use crate::smtp_config::{self, SmtpConfig, SmtpSecurity};

/// Outcome of [`check_upgrade_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No `.upgrade-snapshot.toml` present — normal boot path; nothing
    /// to compare against. Not an error.
    Absent,
    /// Snapshot present and every load-bearing field matches the
    /// current `seller.toml`. Boot may proceed; the caller should rename
    /// the snapshot file via [`mark_verified`] so the next boot does not
    /// re-trigger the check.
    Matches,
    /// Snapshot present and at least one field differs. Boot MUST be
    /// refused. The `deltas` list names the offending fields and their
    /// expected (snapshot-side) vs actual (current-`seller.toml`-side)
    /// values for both the operator-visible error and the audit
    /// payload.
    Mismatch { deltas: Vec<Delta> },
}

/// One disagreement between the snapshot and the current seller.toml.
/// `field` is a dotted path (e.g. `"seller.smtp.host"`); `expected` is
/// the snapshot's value rendered for display; `actual` is the current
/// `seller.toml`'s value rendered for display. Both string-typed so the
/// payload schema is one shape across SMTP (heterogeneous types) and
/// numbering (composite types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

/// Run the check. Both paths are full file paths; both are allowed to
/// not exist (`Absent` is returned if the snapshot is missing; an
/// absent seller.toml is treated as "all fields are None" and compared
/// against the snapshot from there).
///
/// Pure I/O + comparison; never panics, never writes. The caller owns
/// the side-effecting steps (audit append, file rename, exit).
pub fn check_upgrade_snapshot(seller_toml: &Path, snapshot: &Path) -> Result<Outcome> {
    if !snapshot.exists() {
        return Ok(Outcome::Absent);
    }

    let snapshot_smtp = read_smtp(snapshot).context("parse snapshot SMTP section")?;
    let snapshot_numbering =
        read_numbering(snapshot).context("parse snapshot numbering section")?;
    let current_smtp = read_smtp(seller_toml).context("parse current seller.toml SMTP section")?;
    let current_numbering =
        read_numbering(seller_toml).context("parse current seller.toml numbering section")?;

    let mut deltas = Vec::new();
    diff_smtp(&snapshot_smtp, &current_smtp, &mut deltas);
    diff_numbering(&snapshot_numbering, &current_numbering, &mut deltas);

    if deltas.is_empty() {
        Ok(Outcome::Matches)
    } else {
        Ok(Outcome::Mismatch { deltas })
    }
}

/// Rename the snapshot file to `.verified-<ts>` so the next boot does
/// not re-trigger the check. The forensic trail survives (a future
/// audit can verify when the verification happened) — the operator can
/// `rm` the `.verified-*` files manually when they want.
///
/// `ts` is opaque; the caller picks the timestamp (RFC3339 in the live
/// path; a fixed string in tests).
pub fn mark_verified(snapshot: &Path, ts: &str) -> Result<()> {
    let parent = snapshot
        .parent()
        .ok_or_else(|| anyhow!("snapshot path has no parent: {}", snapshot.display()))?;
    let name = snapshot
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("snapshot path has no file name: {}", snapshot.display()))?;
    let renamed = parent.join(format!("{name}.verified-{ts}"));
    fs::rename(snapshot, &renamed)
        .with_context(|| format!("rename {} to {}", snapshot.display(), renamed.display()))?;
    Ok(())
}

/// Render the bilingual operator-visible error for an [`Outcome::Mismatch`].
/// Pure; the caller prints it to stderr.
pub fn format_bilingual_error(deltas: &[Delta], snapshot: &Path) -> String {
    let mut out = String::new();
    out.push_str("⚠️  Upgrade snapshot mismatch — pre-upgrade safety check failed.\n");
    out.push_str(
        "    Frissítés-pillanatkép eltérés — a frissítés előtti biztonsági ellenőrzés sikertelen.\n\n",
    );
    out.push_str(
        "    The following settings differ between your pre-upgrade snapshot and the current seller.toml:\n",
    );
    for d in deltas {
        out.push_str(&format!(
            "      • {}: snapshot={}, current={}\n",
            d.field, d.expected, d.actual
        ));
    }
    out.push('\n');
    out.push_str("    This usually means a data-loss bug regressed during the upgrade.\n\n");
    out.push_str("    To resolve:\n");
    out.push_str(
        "      1. Restore from the latest snapshot tarball (see tools/snapshot-prod.sh + runbook Step 9),\n",
    );
    out.push_str("         OR\n");
    out.push_str(
        "      2. Explicitly acknowledge the drift (you reviewed it and accept the changes):\n",
    );
    out.push_str(&format!(
        "         mv {p} {p}.acknowledged-$(date +%Y%m%dT%H%M%S)\n\n",
        p = snapshot.display()
    ));
    out.push_str("    Refusing to start until resolved.\n");
    out.push_str("    Az indítás megtagadva, amíg az eltérést nem oldod fel.\n");
    out
}

fn read_smtp(path: &Path) -> Result<Option<SmtpConfig>> {
    smtp_config::read_smtp_config(path)
}

fn read_numbering(path: &Path) -> Result<Option<NumberingTemplate>> {
    numbering::read_numbering_section_if_present(path)
}

fn diff_smtp(expected: &Option<SmtpConfig>, actual: &Option<SmtpConfig>, out: &mut Vec<Delta>) {
    match (expected, actual) {
        (None, None) => {}
        (Some(e), None) => {
            // Whole section dropped. Surface all five tracked fields as
            // explicit deltas so the operator sees exactly what would
            // be lost rather than one opaque "[seller.smtp] missing".
            push(out, "seller.smtp.host", &e.host, "(missing)");
            push(out, "seller.smtp.port", &e.port.to_string(), "(missing)");
            push(
                out,
                "seller.smtp.from_address",
                &e.from_address,
                "(missing)",
            );
            push(out, "seller.smtp.username", &e.username, "(missing)");
            push(
                out,
                "seller.smtp.security",
                render_security(e.security),
                "(missing)",
            );
        }
        (None, Some(a)) => {
            push(out, "seller.smtp.host", "(missing)", &a.host);
            push(out, "seller.smtp.port", "(missing)", &a.port.to_string());
            push(
                out,
                "seller.smtp.from_address",
                "(missing)",
                &a.from_address,
            );
            push(out, "seller.smtp.username", "(missing)", &a.username);
            push(
                out,
                "seller.smtp.security",
                "(missing)",
                render_security(a.security),
            );
        }
        (Some(e), Some(a)) => {
            if e.host != a.host {
                push(out, "seller.smtp.host", &e.host, &a.host);
            }
            if e.port != a.port {
                push(
                    out,
                    "seller.smtp.port",
                    &e.port.to_string(),
                    &a.port.to_string(),
                );
            }
            if e.from_address != a.from_address {
                push(
                    out,
                    "seller.smtp.from_address",
                    &e.from_address,
                    &a.from_address,
                );
            }
            if e.username != a.username {
                push(out, "seller.smtp.username", &e.username, &a.username);
            }
            if e.security != a.security {
                push(
                    out,
                    "seller.smtp.security",
                    render_security(e.security),
                    render_security(a.security),
                );
            }
        }
    }
}

fn diff_numbering(
    expected: &Option<NumberingTemplate>,
    actual: &Option<NumberingTemplate>,
    out: &mut Vec<Delta>,
) {
    match (expected, actual) {
        (None, None) => {}
        (Some(e), None) => {
            push(
                out,
                "seller.numbering.segments",
                &render_segments(&e.segments),
                "(missing)",
            );
            push(
                out,
                "seller.numbering.reset_policy",
                render_reset_policy(e.reset_policy),
                "(missing)",
            );
            push(
                out,
                "seller.numbering.start_value",
                &e.start_value.to_string(),
                "(missing)",
            );
        }
        (None, Some(a)) => {
            push(
                out,
                "seller.numbering.segments",
                "(missing)",
                &render_segments(&a.segments),
            );
            push(
                out,
                "seller.numbering.reset_policy",
                "(missing)",
                render_reset_policy(a.reset_policy),
            );
            push(
                out,
                "seller.numbering.start_value",
                "(missing)",
                &a.start_value.to_string(),
            );
        }
        (Some(e), Some(a)) => {
            let es = render_segments(&e.segments);
            let as_ = render_segments(&a.segments);
            if es != as_ {
                push(out, "seller.numbering.segments", &es, &as_);
            }
            if e.reset_policy != a.reset_policy {
                push(
                    out,
                    "seller.numbering.reset_policy",
                    render_reset_policy(e.reset_policy),
                    render_reset_policy(a.reset_policy),
                );
            }
            if e.start_value != a.start_value {
                push(
                    out,
                    "seller.numbering.start_value",
                    &e.start_value.to_string(),
                    &a.start_value.to_string(),
                );
            }
        }
    }
}

fn push(out: &mut Vec<Delta>, field: &str, expected: &str, actual: &str) {
    out.push(Delta {
        field: field.to_string(),
        expected: expected.to_string(),
        actual: actual.to_string(),
    });
}

fn render_security(s: SmtpSecurity) -> &'static str {
    match s {
        SmtpSecurity::StartTls => "StartTls",
        SmtpSecurity::Tls => "Tls",
    }
}

fn render_reset_policy(p: ResetPolicy) -> &'static str {
    match p {
        ResetPolicy::Never => "never",
        ResetPolicy::OnYearChange => "on_year_change",
    }
}

/// Render a segment list to a stable, human-readable string. Display
/// form mirrors `NumberingTemplate::render(2026, 1)` to give the
/// operator a recognizable preview, but uses explicit segment
/// delimiters so two templates that happen to render the same on a
/// specific (year, sequence) but are structurally different still show
/// a visible difference.
fn render_segments(segments: &[Segment]) -> String {
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            out.push('|');
        }
        match seg {
            Segment::Literal(s) => out.push_str(&format!("Lit({s})")),
            Segment::Year { digits } => {
                let d = match digits {
                    YearDigits::Two => 2,
                    YearDigits::Four => 4,
                };
                out.push_str(&format!("Year({d})"));
            }
            Segment::Counter { pad_width } => {
                out.push_str(&format!("Counter({pad_width})"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write(path: &Path, body: &str) {
        fs::write(path, body).expect("write temp file");
    }

    fn matching_seller_toml() -> &'static str {
        r#"
[seller]
name = "Test Kft."
tax_number = "12345678-1-23"

[seller.smtp]
host = "smtppro.zoho.eu"
port = 465
from_address = "user@example.com"
username = "user@example.com"
security = "Tls"
attach_xml = true

[seller.numbering]
segments = [{ kind = "Literal", text = "ABERP/" }, { kind = "Year", digits = 4 }, { kind = "Counter", pad_width = 5 }]
reset_policy = "on_year_change"
start_value = 1
"#
    }

    fn matching_snapshot() -> &'static str {
        r#"
# ABERP upgrade snapshot — written by tools/snapshot-prod.sh

[seller.smtp]
host = "smtppro.zoho.eu"
port = 465
from_address = "user@example.com"
username = "user@example.com"
security = "Tls"
attach_xml = true

[seller.numbering]
segments = [{ kind = "Literal", text = "ABERP/" }, { kind = "Year", digits = 4 }, { kind = "Counter", pad_width = 5 }]
reset_policy = "on_year_change"
start_value = 1
"#
    }

    /// Per-test scratch dir under the platform temp root. The workspace
    /// does not pin `tempfile` as a dev-dep; ULID + thread-id give
    /// collision-free uniqueness for concurrent test threads. The
    /// caller is responsible for best-effort cleanup at the end.
    struct Scratch {
        dir: PathBuf,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn scratch() -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "aberp-s171-snapshot-{}-{:?}",
            ulid::Ulid::new(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch { dir }
    }

    fn paths(s: &Scratch) -> (PathBuf, PathBuf) {
        (
            s.dir.join("seller.toml"),
            s.dir.join(".upgrade-snapshot.toml"),
        )
    }

    /// Scenario 1: snapshot file absent → Absent. Boot must proceed
    /// in the caller; no rename happens.
    #[test]
    fn absent_snapshot_returns_absent() {
        let s = scratch();
        let (seller, snapshot) = paths(&s);
        write(&seller, matching_seller_toml());
        assert!(!snapshot.exists());

        let out = check_upgrade_snapshot(&seller, &snapshot).expect("check ok");
        assert_eq!(out, Outcome::Absent);
    }

    /// Scenario 2: snapshot and seller.toml carry identical SMTP +
    /// numbering → Matches. The caller is expected to rename the
    /// snapshot file via `mark_verified`; this test exercises that
    /// rename too.
    #[test]
    fn matching_snapshot_returns_matches_and_can_be_marked_verified() {
        let s = scratch();
        let (seller, snapshot) = paths(&s);
        write(&seller, matching_seller_toml());
        write(&snapshot, matching_snapshot());

        let out = check_upgrade_snapshot(&seller, &snapshot).expect("check ok");
        assert_eq!(out, Outcome::Matches);

        mark_verified(&snapshot, "20260530T120000Z").expect("rename ok");
        assert!(!snapshot.exists(), "original snapshot file should be gone");
        let renamed = s
            .dir
            .join(".upgrade-snapshot.toml.verified-20260530T120000Z");
        assert!(renamed.exists(), "renamed file should exist: {renamed:?}");
    }

    /// Scenario 3: snapshot's SMTP host differs from current → Mismatch
    /// naming the SMTP field. Mirrors the S170 root cause (the
    /// identity-write path was silently dropping `[seller.smtp]`).
    #[test]
    fn smtp_host_differs_yields_mismatch() {
        let s = scratch();
        let (seller, snapshot) = paths(&s);
        // Current seller.toml's SMTP host has been silently changed to
        // a default (the simulated S170-class regression).
        let current =
            matching_seller_toml().replace("host = \"smtppro.zoho.eu\"", "host = \"localhost\"");
        write(&seller, &current);
        write(&snapshot, matching_snapshot());

        let out = check_upgrade_snapshot(&seller, &snapshot).expect("check ok");
        match out {
            Outcome::Mismatch { deltas } => {
                assert!(
                    deltas.iter().any(|d| d.field == "seller.smtp.host"
                        && d.expected == "smtppro.zoho.eu"
                        && d.actual == "localhost"),
                    "expected an SMTP host delta, got {deltas:?}"
                );
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    /// Scenario 4: snapshot's numbering segments differ from current →
    /// Mismatch naming the numbering field. The S170 RCA also showed
    /// the identity-write path dropping `[seller.numbering]`,
    /// silently reverting the operator to the bare `INV-default`
    /// template.
    #[test]
    fn numbering_segments_differ_yields_mismatch() {
        let s = scratch();
        let (seller, snapshot) = paths(&s);
        // Current seller.toml's numbering segments have been silently
        // reverted to a different shape. The policy must drop to
        // "never" simultaneously because `on_year_change` without a
        // Year segment fails validation — i.e. the real S170-class
        // regression revert-to-default flips BOTH fields.
        let current = matching_seller_toml()
            .replace(
                "segments = [{ kind = \"Literal\", text = \"ABERP/\" }, { kind = \"Year\", digits = 4 }, { kind = \"Counter\", pad_width = 5 }]",
                "segments = [{ kind = \"Literal\", text = \"INV-default/\" }, { kind = \"Counter\", pad_width = 4 }]",
            )
            .replace(
                "reset_policy = \"on_year_change\"",
                "reset_policy = \"never\"",
            );
        write(&seller, &current);
        write(&snapshot, matching_snapshot());

        let out = check_upgrade_snapshot(&seller, &snapshot).expect("check ok");
        match out {
            Outcome::Mismatch { deltas } => {
                assert!(
                    deltas
                        .iter()
                        .any(|d| d.field == "seller.numbering.segments"),
                    "expected a numbering.segments delta, got {deltas:?}"
                );
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn bilingual_error_names_each_delta() {
        let snap_path = std::path::PathBuf::from("/Users/op/.aberp/prod/.upgrade-snapshot.toml");
        let deltas = vec![
            Delta {
                field: "seller.smtp.host".to_string(),
                expected: "smtppro.zoho.eu".to_string(),
                actual: "(missing)".to_string(),
            },
            Delta {
                field: "seller.numbering.start_value".to_string(),
                expected: "1247".to_string(),
                actual: "1".to_string(),
            },
        ];
        let msg = format_bilingual_error(&deltas, &snap_path);
        assert!(msg.contains("Upgrade snapshot mismatch"));
        assert!(msg.contains("Frissítés-pillanatkép eltérés"));
        assert!(msg.contains("seller.smtp.host"));
        assert!(msg.contains("seller.numbering.start_value"));
        assert!(msg.contains("smtppro.zoho.eu"));
        assert!(msg.contains("(missing)"));
        assert!(msg.contains("acknowledged-"));
    }
}
