//! S257 / PR-246 — `[[mes.adapters]]` adapter config in seller.toml.
//!
//! The 7th seller.toml preservation slot per
//! [[seller-toml-write-invariant]] (after identity / banks / numbering
//! / smtp / branding / quote_intake). Stores the durable
//! [`AdapterConfigEntry`] set the operator manages from the Settings →
//! Adapters page; the live `AdapterRegistry` (S240) provides health,
//! this is the config source of truth.
//!
//! ## Why a hand-rolled line-walker (again)
//!
//! Same call as PR-71's `seller_banks`: the accepted grammar is a flat
//! array-of-tables, a ~120-line walker is more readable than a
//! `toml::Value` path, and it avoids dragging a new direct dependency
//! into the binary for a surface that must do surgical
//! preserve-the-other-sections merges anyway. The write path mirrors
//! `seller_banks::write_seller_banks_section` exactly (snapshot →
//! merge → atomic 0600 rename).
//!
//! ## Malformed / unknown-kind entries
//!
//! A `[[mes.adapters]]` block missing a required field, or carrying a
//! `kind` outside [`AdapterKind`]'s closed vocab, is dropped on load
//! with a `tracing::warn!` rather than failing the whole read — one
//! bad entry (a hand-edit typo, or a row written by a newer binary)
//! must not brick boot for every other adapter. The warn is the loud
//! signal per CLAUDE.md rule 12; the skip keeps the blast radius to the
//! single offending row.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::{anyhow, Context as _, Result};

use aberp_mes::{AdapterConfigEntry, AdapterKind};

/// Read + parse the `[[mes.adapters]]` section of a tenant seller.toml.
/// Returns an empty vec when the file (or the section) is absent.
pub fn read_mes_adapters(path: &Path) -> Result<Vec<AdapterConfigEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = fs::read_to_string(path)
        .with_context(|| format!("read seller.toml at {}", path.display()))?;
    Ok(parse_mes_adapters_section(&body))
}

/// Internal per-block builder — every field `Option` so a missing
/// required key surfaces as a drop-with-warn at finalisation.
#[derive(Debug, Default)]
struct RawAdapter {
    kind: Option<String>,
    adapter_id: Option<String>,
    friendly_name: Option<String>,
    host: Option<String>,
    port: Option<String>,
    device_name: Option<String>,
    model: Option<String>,
}

impl RawAdapter {
    fn has_any_field(&self) -> bool {
        self.kind.is_some()
            || self.adapter_id.is_some()
            || self.friendly_name.is_some()
            || self.host.is_some()
            || self.port.is_some()
    }

    /// Finalise into a typed entry, or `None` (with a warn) when a
    /// required field is missing or the kind is outside the closed
    /// vocab.
    fn into_entry(self) -> Option<AdapterConfigEntry> {
        let kind_str = self.kind?;
        let kind = match AdapterKind::from_wire_str(&kind_str) {
            Some(k) => k,
            None => {
                tracing::warn!(
                    kind = %kind_str,
                    "seller.toml [[mes.adapters]] entry has an unknown kind; skipping. \
                     Open Settings → Adapters to re-add it on a build that supports the kind."
                );
                return None;
            }
        };
        let adapter_id = self.adapter_id.filter(|s| !s.trim().is_empty());
        let host = self.host.filter(|s| !s.trim().is_empty());
        let port = self.port.and_then(|p| p.trim().parse::<u16>().ok());
        match (adapter_id, host, port) {
            (Some(adapter_id), Some(host), Some(port)) => Some(AdapterConfigEntry {
                kind,
                friendly_name: self
                    .friendly_name
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| adapter_id.clone()),
                adapter_id,
                host,
                port,
                device_name: self.device_name.filter(|s| !s.trim().is_empty()),
                model: self.model.filter(|s| !s.trim().is_empty()),
            }),
            _ => {
                tracing::warn!(
                    "seller.toml [[mes.adapters]] entry missing a required field \
                     (adapter_id / host / port); skipping."
                );
                None
            }
        }
    }
}

/// Parse the `[[mes.adapters]]` array-of-tables out of an in-memory
/// seller.toml body. Foreign sections are skipped; malformed / unknown-
/// kind entries are dropped (see module docs).
pub fn parse_mes_adapters_section(body: &str) -> Vec<AdapterConfigEntry> {
    let mut entries: Vec<AdapterConfigEntry> = Vec::new();
    let mut current: Option<RawAdapter> = None;
    let mut in_section = false;

    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("[[") && line.ends_with("]]") {
            if let Some(e) = current.take() {
                if e.has_any_field() {
                    if let Some(entry) = e.into_entry() {
                        entries.push(entry);
                    }
                }
            }
            let inner = line[2..line.len() - 2].trim();
            if inner == "mes.adapters" {
                current = Some(RawAdapter::default());
                in_section = true;
            } else {
                in_section = false;
            }
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(e) = current.take() {
                if e.has_any_field() {
                    if let Some(entry) = e.into_entry() {
                        entries.push(entry);
                    }
                }
            }
            in_section = false;
            continue;
        }
        if !in_section {
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        let key = k.trim();
        let value = strip_quotes(v.trim()).to_string();
        if let Some(cur) = current.as_mut() {
            match key {
                "kind" => cur.kind = Some(value),
                "adapter_id" => cur.adapter_id = Some(value),
                "friendly_name" => cur.friendly_name = Some(value),
                "host" => cur.host = Some(value),
                "port" => cur.port = Some(value),
                "device_name" => cur.device_name = Some(value),
                "model" => cur.model = Some(value),
                _ => {} // forward-compat: ignore unknown keys
            }
        }
    }
    if let Some(e) = current.take() {
        if e.has_any_field() {
            if let Some(entry) = e.into_entry() {
                entries.push(entry);
            }
        }
    }
    entries
}

fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

/// Render the `[[mes.adapters]]` array to canonical TOML. Entries are
/// emitted in vec order; optional kind-specific keys are elided when
/// absent (matching `seller_banks`'s "absent == default" posture).
pub fn to_toml_section(entries: &[AdapterConfigEntry]) -> String {
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("[[mes.adapters]]\n");
        out.push_str(&format!("kind          = \"{}\"\n", e.kind.wire_str()));
        out.push_str(&format!("adapter_id    = \"{}\"\n", e.adapter_id));
        out.push_str(&format!("friendly_name = \"{}\"\n", e.friendly_name));
        out.push_str(&format!("host          = \"{}\"\n", e.host));
        out.push_str(&format!("port          = {}\n", e.port));
        if let Some(device) = &e.device_name {
            out.push_str(&format!("device_name   = \"{device}\"\n"));
        }
        if let Some(model) = &e.model {
            out.push_str(&format!("model         = \"{model}\"\n"));
        }
    }
    out
}

/// Replace ONLY the `[[mes.adapters]]` blocks of an existing seller.toml
/// body; every other section is preserved verbatim. Same line-walker
/// shape as `seller_banks::merge_bank_section`.
pub fn merge_mes_adapters_section(existing: &str, new_section: &str) -> String {
    let mut prefix = String::new();
    let mut in_section = false;
    for raw_line in existing.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            let inner = trimmed[2..trimmed.len() - 2].trim();
            in_section = inner == "mes.adapters";
            if in_section {
                continue;
            }
            prefix.push_str(raw_line);
            prefix.push('\n');
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = false;
            prefix.push_str(raw_line);
            prefix.push('\n');
            continue;
        }
        if in_section {
            continue;
        }
        prefix.push_str(raw_line);
        prefix.push('\n');
    }
    while prefix.ends_with("\n\n") {
        prefix.pop();
    }
    if prefix.is_empty() {
        return new_section.to_string();
    }
    if new_section.is_empty() {
        return prefix;
    }
    if !prefix.ends_with('\n') {
        prefix.push('\n');
    }
    prefix.push('\n');
    prefix.push_str(new_section);
    prefix
}

/// Atomically replace `path`'s `[[mes.adapters]]` section with the
/// canonical serialisation of `entries`. Preserves every other section
/// (the [[seller-toml-write-invariant]] discipline) and snapshots the
/// prior body first (PR-170 defence-in-depth).
pub fn write_mes_adapters_section(path: &Path, entries: &[AdapterConfigEntry]) -> Result<()> {
    let _ = crate::seller_toml_backup::snapshot_and_rotate(path);

    for e in entries {
        e.validate()
            .map_err(|errs| anyhow!("adapter config invariants violated pre-write: {errs:?}"))?;
    }

    let new_section = to_toml_section(entries);
    let body = if path.exists() {
        let existing = fs::read_to_string(path)
            .with_context(|| format!("read existing seller.toml at {}", path.display()))?;
        merge_mes_adapters_section(&existing, &new_section)
    } else {
        new_section
    };
    write_atomic(path, body.as_bytes())
}

fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("seller.toml path `{}` has no parent dir", path.display()))?;
    if !parent.exists() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = parent.join(format!(".seller.toml.mes.tmp.{}", std::process::id()));
    let mut f =
        fs::File::create(&tmp).with_context(|| format!("create tempfile {}", tmp.display()))?;
    f.write_all(body)
        .with_context(|| format!("write tempfile {}", tmp.display()))?;
    f.sync_all()
        .with_context(|| format!("fsync tempfile {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: AdapterKind, id: &str, host: &str, port: u16) -> AdapterConfigEntry {
        AdapterConfigEntry {
            kind,
            adapter_id: id.to_string(),
            friendly_name: format!("Friendly {id}"),
            host: host.to_string(),
            port,
            device_name: None,
            model: None,
        }
    }

    #[test]
    fn parse_returns_empty_when_section_absent() {
        let body = "[seller]\nlegal_name = \"X\"\n";
        assert!(parse_mes_adapters_section(body).is_empty());
    }

    #[test]
    fn to_toml_round_trips_through_parser() {
        let mut a = entry(AdapterKind::Cnc, "cnc-1", "10.0.0.5", 5000);
        a.device_name = Some("M1".to_string());
        let mut b = entry(AdapterKind::Robot, "robot-1", "10.0.0.6", 30004);
        b.model = Some("UR10e".to_string());
        let entries = vec![
            entry(AdapterKind::LabelPrinter, "lp-1", "10.0.0.4", 9100),
            a,
            b,
        ];
        let toml = to_toml_section(&entries);
        let back = parse_mes_adapters_section(&toml);
        assert_eq!(
            entries, back,
            "round-trip must preserve every entry + field"
        );
    }

    #[test]
    fn unknown_kind_entry_is_dropped() {
        let body = "[[mes.adapters]]\nkind = \"warp-drive\"\nadapter_id = \"w1\"\nhost = \"x\"\nport = 1\n";
        assert!(parse_mes_adapters_section(body).is_empty());
    }

    #[test]
    fn malformed_entry_missing_port_is_dropped_others_survive() {
        let body = "\
[[mes.adapters]]
kind = \"label-printer\"
adapter_id = \"good\"
friendly_name = \"Good\"
host = \"10.0.0.4\"
port = 9100

[[mes.adapters]]
kind = \"robot\"
adapter_id = \"bad\"
host = \"10.0.0.6\"
";
        let parsed = parse_mes_adapters_section(body);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].adapter_id, "good");
    }

    #[test]
    fn friendly_name_defaults_to_adapter_id_when_absent() {
        let body = "[[mes.adapters]]\nkind = \"robot\"\nadapter_id = \"r1\"\nhost = \"10.0.0.6\"\nport = 30004\n";
        let parsed = parse_mes_adapters_section(body);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].friendly_name, "r1");
    }

    #[test]
    fn merge_replaces_only_mes_adapters_preserving_other_sections() {
        let existing = "\
# ABERP seller config
[seller]
legal_name = \"Áben Consulting KFT.\"

[[seller.banks]]
currency = \"HUF\"
account_number = \"123\"
bank_name = \"B\"
swift_bic = \"GIBAHUHB\"
default = true

[[mes.adapters]]
kind = \"label-printer\"
adapter_id = \"OLD\"
friendly_name = \"Old\"
host = \"1.1.1.1\"
port = 9100

[quote_intake]
enabled = true
base_url = \"http://x\"
";
        let new_section = to_toml_section(&[entry(AdapterKind::Robot, "NEW", "2.2.2.2", 30004)]);
        let merged = merge_mes_adapters_section(existing, &new_section);
        assert!(merged.contains("legal_name = \"Áben Consulting KFT.\""));
        assert!(merged.contains("[[seller.banks]]"));
        assert!(merged.contains("account_number = \"123\""));
        assert!(merged.contains("[quote_intake]"));
        assert!(merged.contains("base_url = \"http://x\""));
        assert!(!merged.contains("OLD"), "old adapter dropped: {merged}");
        assert!(merged.contains("NEW"), "new adapter present: {merged}");
        // The replaced section re-parses to exactly the new entry.
        let reparsed = parse_mes_adapters_section(&merged);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].adapter_id, "NEW");
    }

    #[test]
    fn merge_inserts_when_section_absent() {
        let existing = "[seller]\nlegal_name = \"X\"\n";
        let new_section = to_toml_section(&[entry(AdapterKind::Cnc, "c1", "10.0.0.5", 5000)]);
        let merged = merge_mes_adapters_section(existing, &new_section);
        assert!(merged.contains("[seller]"));
        assert!(merged.contains("[[mes.adapters]]"));
        assert!(merged.contains("c1"));
    }

    #[test]
    fn empty_entries_merge_strips_the_section() {
        let existing = "[seller]\nlegal_name = \"X\"\n\n[[mes.adapters]]\nkind = \"robot\"\nadapter_id = \"r1\"\nhost = \"10.0.0.6\"\nport = 30004\n";
        let merged = merge_mes_adapters_section(existing, "");
        assert!(merged.contains("[seller]"));
        assert!(
            !merged.contains("[[mes.adapters]]"),
            "section stripped: {merged}"
        );
        assert!(!merged.contains("r1"));
    }
}
