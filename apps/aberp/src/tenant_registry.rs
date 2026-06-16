//! S433 — multi-tenant registry + switch-on-restart hint.
//!
//! Before S433 a tenant was nothing but the `--tenant` CLI string: the
//! launchers set `ABERP_TENANT`, the Tauri shell forwarded it as
//! `--tenant <slug>`, and every per-tenant artifact (DuckDB at
//! `~/.aberp/<slug>/aberp.duckdb`, keychain namespace `aberp.nav.<slug>`,
//! seller config at `~/.aberp/<slug>/seller.toml`) was keyed off that
//! string. There was no way to *enumerate* tenants, no operator-facing
//! CRUD, and switching tenants meant editing a launcher env var by hand.
//!
//! This module adds the missing menu side: a single registry file
//! `~/.aberp/tenants.toml` listing every tenant (slug + display name +
//! `Active`/`Archived` state + creation stamp), plus the switch
//! mechanism. Switching is deliberately **restart-based**, never a live
//! in-process swap: swapping the DuckDB handle, NAV credentials, and
//! keychain namespace out from under in-flight daemons mid-process is
//! exactly the class of footgun [[trust-code-not-operator]] /
//! [[hulye-biztos]] tell us to design out. Instead the switch writes a
//! one-shot hint file `~/.aberp/next_tenant`; the next boot consumes it
//! (honor-once), overriding the tenant + DuckDB path for that boot only.
//!
//! Why hand-rolled TOML and not the `toml` crate: the workspace carries
//! no `toml` dependency — `seller.toml` is hand-parsed for its
//! multi-writer section-preservation discipline. `tenants.toml` is
//! *single-writer* (only this module writes it), so the preservation
//! concern doesn't apply, but matching the codebase's zero-`toml`-dep
//! convention (rule 11) keeps the dependency surface flat. The schema is
//! a fixed 4-field array-of-tables, trivially serialised + parsed by the
//! line walker below.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};
use anyhow::{anyhow, Context, Result};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use ulid::Ulid;

/// Registry file name under `~/.aberp/`.
pub const REGISTRY_FILENAME: &str = "tenants.toml";
/// One-shot switch hint file name under `~/.aberp/`.
pub const NEXT_TENANT_FILENAME: &str = "next_tenant";
/// Per-tenant DuckDB file name under `~/.aberp/<slug>/`.
pub const TENANT_DB_FILENAME: &str = "aberp.duckdb";

/// Lifecycle state of a tenant in the registry.
///
/// - `Active` — a real operator tenant in the working pool.
/// - `Archived` — soft-deleted (still on disk, hidden from the active
///   pool, refused as a switch target until restored).
/// - `Demo` (S433) — the bundled `demo` safety-net tenant seeded on a
///   fresh install. Bootable + switchable like Active, but it can NEVER
///   be archived ([[trust-code-not-operator]]) and shows a DEMO badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantState {
    Active,
    Archived,
    Demo,
}

impl TenantState {
    /// Storage token written to / read from `tenants.toml`.
    pub fn as_token(self) -> &'static str {
        match self {
            TenantState::Active => "active",
            TenantState::Archived => "archived",
            TenantState::Demo => "demo",
        }
    }

    fn from_token(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(TenantState::Active),
            "archived" => Ok(TenantState::Archived),
            "demo" => Ok(TenantState::Demo),
            other => Err(anyhow!("unknown tenant state token {other:?}")),
        }
    }
}

/// S433 — the bundled demo tenant's slug + display name. Seeded on a
/// fresh install so a new operator lands in a usable, populated system
/// instead of an empty NeedsSetup wall.
pub const DEMO_SLUG: &str = "demo";
pub const DEMO_DISPLAY_NAME: &str = "Demo Tenant";

/// One registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantEntry {
    pub slug: String,
    pub display_name: String,
    pub state: TenantState,
    /// RFC3339 UTC creation stamp.
    pub created_at: String,
}

/// Typed errors for the state-transition invariants. Routes map these to
/// HTTP status codes; the variants are the [[trust-code-not-operator]]
/// guards expressed in the type system so no operator action can reach an
/// unsafe registry state.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TenantRegistryError {
    #[error("slug {0:?} is invalid: tenant slugs must be 1–64 chars of [A-Za-z0-9_-]")]
    InvalidSlug(String),
    #[error("display name must be 1–120 chars with no control characters")]
    InvalidDisplayName,
    #[error("a tenant with slug {0:?} already exists")]
    SlugTaken(String),
    #[error("no tenant with slug {0:?}")]
    NotFound(String),
    #[error("cannot archive the currently-running tenant {0:?} — switch to another tenant first")]
    CannotArchiveRunning(String),
    #[error("cannot archive {0:?} — it is the only Active tenant; at least one must stay Active")]
    CannotArchiveOnlyActive(String),
    #[error("cannot archive the demo tenant {0:?} — it is the bundled safety net")]
    CannotArchiveDemo(String),
}

/// In-memory view of `tenants.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TenantRegistry {
    pub tenants: Vec<TenantEntry>,
}

/// Validate a tenant slug. Restricted to `[A-Za-z0-9_-]{1,64}` so it is
/// safe as a single filesystem path component (`~/.aberp/<slug>/`) and a
/// keychain service suffix — no traversal, no separators, no spaces.
pub fn validate_slug(slug: &str) -> Result<(), TenantRegistryError> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(TenantRegistryError::InvalidSlug(slug.to_string()))
    }
}

fn validate_display_name(name: &str) -> Result<(), TenantRegistryError> {
    let ok =
        !name.is_empty() && name.chars().count() <= 120 && !name.chars().any(|c| c.is_control());
    if ok {
        Ok(())
    } else {
        Err(TenantRegistryError::InvalidDisplayName)
    }
}

impl TenantRegistry {
    pub fn find(&self, slug: &str) -> Option<&TenantEntry> {
        self.tenants.iter().find(|t| t.slug == slug)
    }

    /// A slug names a tenant that exists AND is Active. Used for the
    /// "keep ≥1 Active" archive guard (Demo does NOT count as Active).
    pub fn is_active(&self, slug: &str) -> bool {
        matches!(
            self.find(slug),
            Some(TenantEntry {
                state: TenantState::Active,
                ..
            })
        )
    }

    /// A slug names a bootable tenant (Active OR Demo) — the valid switch
    /// targets and the gate the boot-hint consumer checks. Archived
    /// tenants are not bootable until restored.
    pub fn is_bootable(&self, slug: &str) -> bool {
        matches!(
            self.find(slug),
            Some(TenantEntry {
                state: TenantState::Active | TenantState::Demo,
                ..
            })
        )
    }

    fn active_count(&self) -> usize {
        self.tenants
            .iter()
            .filter(|t| t.state == TenantState::Active)
            .count()
    }

    /// Append a new Active tenant. Pure: caller supplies `now` so tests
    /// are deterministic. Errors if the slug is invalid or already taken.
    pub fn add(
        &mut self,
        slug: &str,
        display_name: &str,
        now: OffsetDateTime,
    ) -> Result<TenantEntry, TenantRegistryError> {
        validate_slug(slug)?;
        validate_display_name(display_name)?;
        if self.find(slug).is_some() {
            return Err(TenantRegistryError::SlugTaken(slug.to_string()));
        }
        let entry = TenantEntry {
            slug: slug.to_string(),
            display_name: display_name.to_string(),
            state: TenantState::Active,
            created_at: now
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
        };
        self.tenants.push(entry.clone());
        Ok(entry)
    }

    /// Append the bundled demo tenant (state `Demo`). Errors only if a
    /// `demo` slug already exists (idempotency guard).
    pub fn add_demo(&mut self, now: OffsetDateTime) -> Result<TenantEntry, TenantRegistryError> {
        if self.find(DEMO_SLUG).is_some() {
            return Err(TenantRegistryError::SlugTaken(DEMO_SLUG.to_string()));
        }
        let entry = TenantEntry {
            slug: DEMO_SLUG.to_string(),
            display_name: DEMO_DISPLAY_NAME.to_string(),
            state: TenantState::Demo,
            created_at: now
                .format(&Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
        };
        self.tenants.push(entry.clone());
        Ok(entry)
    }

    /// Soft-delete a tenant. Refuses the two unsafe cases in code:
    /// archiving the running tenant, or archiving the last Active one.
    pub fn archive(&mut self, slug: &str, running_slug: &str) -> Result<(), TenantRegistryError> {
        if slug == running_slug {
            return Err(TenantRegistryError::CannotArchiveRunning(slug.to_string()));
        }
        let entry_state = self.find(slug).map(|t| t.state);
        let Some(state) = entry_state else {
            return Err(TenantRegistryError::NotFound(slug.to_string()));
        };
        // The bundled demo tenant is the safety net — never archivable.
        if state == TenantState::Demo {
            return Err(TenantRegistryError::CannotArchiveDemo(slug.to_string()));
        }
        let is_active = state == TenantState::Active;
        // Only block the only-Active case when the target is itself
        // Active (archiving an already-Archived tenant is a no-op error
        // path, not an only-Active concern).
        if is_active && self.active_count() <= 1 {
            return Err(TenantRegistryError::CannotArchiveOnlyActive(
                slug.to_string(),
            ));
        }
        for t in &mut self.tenants {
            if t.slug == slug {
                t.state = TenantState::Archived;
            }
        }
        Ok(())
    }

    /// Flip an Archived tenant back to Active. Errors only if absent
    /// (restoring an already-Active tenant is idempotent).
    pub fn restore(&mut self, slug: &str) -> Result<(), TenantRegistryError> {
        if self.find(slug).is_none() {
            return Err(TenantRegistryError::NotFound(slug.to_string()));
        }
        for t in &mut self.tenants {
            if t.slug == slug {
                t.state = TenantState::Active;
            }
        }
        Ok(())
    }

    /// Serialise to the `tenants.toml` body. Deterministic: entries in
    /// vector order, fields in a fixed order.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# ABERP tenant registry — managed by the Tenants admin screen (S433).\n\
             # Do not hand-edit while ABERP is running.\n",
        );
        for t in &self.tenants {
            out.push_str("\n[[tenant]]\n");
            out.push_str(&format!("slug = {}\n", quote(&t.slug)));
            out.push_str(&format!("display_name = {}\n", quote(&t.display_name)));
            out.push_str(&format!("state = {}\n", quote(t.state.as_token())));
            out.push_str(&format!("created_at = {}\n", quote(&t.created_at)));
        }
        out
    }

    /// Parse a `tenants.toml` body. Tolerant of comments + blank lines.
    /// A row is complete once `[[tenant]]` is seen; missing fields error
    /// loud (rule 12) rather than defaulting silently.
    pub fn parse_toml(body: &str) -> Result<Self> {
        let mut tenants: Vec<TenantEntry> = Vec::new();
        let mut cur: Option<PartialEntry> = None;
        for (lineno, raw) in body.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[tenant]]" {
                if let Some(p) = cur.take() {
                    tenants.push(p.finish(lineno)?);
                }
                cur = Some(PartialEntry::default());
                continue;
            }
            let (key, val) = line.split_once('=').ok_or_else(|| {
                anyhow!(
                    "tenants.toml line {} not `key = value`: {line:?}",
                    lineno + 1
                )
            })?;
            let key = key.trim();
            let val = unquote(val.trim())
                .with_context(|| format!("tenants.toml line {} value", lineno + 1))?;
            let p = cur
                .as_mut()
                .ok_or_else(|| anyhow!("tenants.toml line {} before any [[tenant]]", lineno + 1))?;
            match key {
                "slug" => p.slug = Some(val),
                "display_name" => p.display_name = Some(val),
                "state" => p.state = Some(val),
                "created_at" => p.created_at = Some(val),
                other => {
                    return Err(anyhow!(
                        "tenants.toml unknown key {other:?} at line {}",
                        lineno + 1
                    ))
                }
            }
        }
        if let Some(p) = cur.take() {
            tenants.push(p.finish(body.lines().count())?);
        }
        Ok(TenantRegistry { tenants })
    }

    /// Read the registry from `path`. A missing file is an empty registry
    /// (first boot of this version) — not an error.
    pub fn read_from(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(body) => Self::parse_toml(&body)
                .with_context(|| format!("parse tenant registry at {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read tenant registry at {}", path.display())),
        }
    }

    /// Atomically write the registry to `path` (tempfile + fsync +
    /// rename, 0600). Either the full new body lands or `path` is
    /// untouched — no half-written registry (rule 12).
    pub fn write_to(&self, path: &Path) -> Result<()> {
        write_atomic(path, self.to_toml().as_bytes())
    }
}

#[derive(Default)]
struct PartialEntry {
    slug: Option<String>,
    display_name: Option<String>,
    state: Option<String>,
    created_at: Option<String>,
}

impl PartialEntry {
    fn finish(self, lineno: usize) -> Result<TenantEntry> {
        let slug = self
            .slug
            .ok_or_else(|| anyhow!("tenants.toml [[tenant]] near line {lineno} missing slug"))?;
        let display_name = self
            .display_name
            .ok_or_else(|| anyhow!("tenants.toml tenant {slug:?} missing display_name"))?;
        let state = TenantState::from_token(
            self.state
                .as_deref()
                .ok_or_else(|| anyhow!("tenants.toml tenant {slug:?} missing state"))?,
        )?;
        let created_at = self
            .created_at
            .ok_or_else(|| anyhow!("tenants.toml tenant {slug:?} missing created_at"))?;
        Ok(TenantEntry {
            slug,
            display_name,
            state,
            created_at,
        })
    }
}

/// Quote a string for the TOML body: wrap in `"` and escape `\` + `"`.
/// Slug + state + created_at are pre-validated to contain none of these;
/// only `display_name` can, and the escape keeps the round-trip exact.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Inverse of [`quote`]. Errors loud on an unterminated / unquoted value.
fn unquote(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    if bytes.len() < 2 || bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return Err(anyhow!("expected a double-quoted string, got {s:?}"));
    }
    let inner = &s[1..s.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => return Err(anyhow!("unknown escape \\{other} in {s:?}")),
                None => return Err(anyhow!("dangling escape in {s:?}")),
            }
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

// ── Path resolution ──────────────────────────────────────────────────

/// `~/.aberp`. Errors if `$HOME` is unset — mirrors the `HOME` discipline
/// in `setup_seller_info::seller_toml_path_for_tenant` (the workspace has
/// no `dirs` dependency).
pub fn aberp_root() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow!("HOME environment variable not set; cannot resolve ~/.aberp"))?;
    Ok(PathBuf::from(home).join(".aberp"))
}

pub fn registry_path() -> Result<PathBuf> {
    Ok(aberp_root()?.join(REGISTRY_FILENAME))
}

pub fn next_tenant_hint_path() -> Result<PathBuf> {
    Ok(aberp_root()?.join(NEXT_TENANT_FILENAME))
}

/// Canonical per-tenant DuckDB path. Every registry-managed tenant lives
/// here; this is also what `run_prod.sh` sets `ABERP_DB` to, so deriving
/// the path from the slug on switch matches the existing layout exactly.
pub fn tenant_db_path(slug: &str) -> Result<PathBuf> {
    Ok(aberp_root()?.join(slug).join(TENANT_DB_FILENAME))
}

/// S433 — a fresh install is one with NO `tenants.toml` AND no existing
/// per-tenant DuckDB under `~/.aberp/<slug>/aberp.duckdb`. The second
/// clause is the backward-compat guard: a real install already in flight
/// (prod systems, dev boxes) carries a tenant DB even before this version
/// wrote a registry, so it is NOT fresh — we must not inject demo there.
pub fn is_fresh_install(root: &Path) -> Result<bool> {
    if root.join(REGISTRY_FILENAME).exists() {
        return Ok(false);
    }
    match fs::read_dir(root) {
        Ok(rd) => {
            for entry in rd.flatten() {
                if entry.path().join(TENANT_DB_FILENAME).exists() {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        // `~/.aberp` doesn't exist yet → genuinely fresh.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => {
            Err(e).with_context(|| format!("scan {} for fresh-install check", root.display()))
        }
    }
}

pub fn is_fresh_install_default() -> Result<bool> {
    is_fresh_install(&aberp_root()?)
}

pub fn read_registry() -> Result<TenantRegistry> {
    TenantRegistry::read_from(&registry_path()?)
}

pub fn write_registry(reg: &TenantRegistry) -> Result<()> {
    reg.write_to(&registry_path()?)
}

// ── Switch hint (honor-once) ─────────────────────────────────────────

pub fn write_next_tenant_hint_at(path: &Path, slug: &str) -> Result<()> {
    write_atomic(path, slug.as_bytes())
}

pub fn write_next_tenant_hint(slug: &str) -> Result<()> {
    write_next_tenant_hint_at(&next_tenant_hint_path()?, slug)
}

/// Read + DELETE the switch hint. Returns `Some(slug)` exactly once per
/// written hint; subsequent calls return `None`. The delete happens
/// before the slug is returned so a crash mid-boot can't replay the
/// switch on the *next* boot — honor-once is the contract.
pub fn consume_next_tenant_hint_at(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(body) => {
            fs::remove_file(path)
                .with_context(|| format!("delete consumed switch hint {}", path.display()))?;
            let slug = body.trim().to_string();
            if slug.is_empty() {
                Ok(None)
            } else {
                Ok(Some(slug))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read switch hint {}", path.display())),
    }
}

pub fn consume_next_tenant_hint() -> Result<Option<String>> {
    consume_next_tenant_hint_at(&next_tenant_hint_path()?)
}

// ── Per-tenant audit emit ────────────────────────────────────────────

/// Append a `tenant.*` lifecycle event into the ledger at `db_path` for
/// `tenant`. Opening the ledger by path (not the running connection)
/// lets the create + switch paths write into a DIFFERENT tenant's chain
/// than the one the binary booted with — `TenantCreated` lands in the new
/// tenant's ledger, `TenantSwitched` in the switched-to tenant's, never in
/// the caller's. Mirrors `avl_vendors::append_vendor_event`.
pub fn emit_tenant_event(
    db_path: &Path,
    tenant: TenantId,
    binary_hash: BinaryHash,
    operator_login: &str,
    kind: EventKind,
    payload: Vec<u8>,
) -> Result<()> {
    let mut ledger = Ledger::open(db_path, tenant, binary_hash)
        .context("open audit ledger to record tenant lifecycle event")?;
    let actor = Actor::from_local_cli(Ulid::new().to_string(), operator_login);
    ledger
        .append(kind, payload, actor, None)
        .context("append tenant lifecycle audit entry")?;
    Ok(())
}

// ── Atomic file write (tempfile + fsync + rename, 0600) ──────────────
//
// A focused local copy of the seller.toml write discipline — the registry
// + hint are single-writer files in `~/.aberp/`, so this stays
// self-contained rather than reaching into `setup_seller_info`'s private
// writer.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent dir", path.display()))?;
    if !parent.exists() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tenants");
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp.{}-{}-{}",
        std::process::id(),
        nanos,
        seq
    ));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .with_context(|| format!("open tempfile {}", tmp_path.display()))?;
        f.write_all(body)
            .with_context(|| format!("write tempfile {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync tempfile {}", tmp_path.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&tmp_path, perms)
            .with_context(|| format!("chmod 0600 {}", tmp_path.display()))?;
    }
    fs::rename(&tmp_path, path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    /// Unique temp dir under the system temp root. We avoid the
    /// `tempfile` dev-dep to keep the surface tight (rule 13 + matches
    /// `tests/print_invoice_render.rs`); the per-test ULID dir is leaked
    /// at end-of-test, acceptable for the OS-temp-root posture.
    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join("aberp-tenant-registry")
            .join(Ulid::new().to_string());
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn sample() -> TenantRegistry {
        let mut r = TenantRegistry::default();
        r.add("prod", "ABEN AG", dt("2026-06-16T04:46:00Z"))
            .unwrap();
        r.add("test", "ABEN Test", dt("2026-06-16T04:47:00Z"))
            .unwrap();
        r
    }

    #[test]
    fn toml_round_trips_byte_exact_after_reparse() {
        let r = sample();
        let body = r.to_toml();
        let back = TenantRegistry::parse_toml(&body).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn display_name_with_quotes_round_trips() {
        let mut r = TenantRegistry::default();
        r.add(
            "acme",
            r#"ACME "Special" Co \ Ltd"#,
            dt("2026-06-16T00:00:00Z"),
        )
        .unwrap();
        let back = TenantRegistry::parse_toml(&r.to_toml()).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.tenants[0].display_name, r#"ACME "Special" Co \ Ltd"#);
    }

    #[test]
    fn missing_file_is_empty_registry() {
        let dir = test_dir();
        let p = dir.as_path().join("tenants.toml");
        assert_eq!(
            TenantRegistry::read_from(&p).unwrap(),
            TenantRegistry::default()
        );
    }

    #[test]
    fn write_then_read_is_atomic_and_exact() {
        let dir = test_dir();
        let p = dir.as_path().join("sub").join("tenants.toml");
        let r = sample();
        r.write_to(&p).unwrap();
        assert_eq!(TenantRegistry::read_from(&p).unwrap(), r);
        // No stray tempfile left behind.
        let leftovers: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tempfile not cleaned up: {leftovers:?}"
        );
    }

    #[test]
    fn add_rejects_duplicate_and_bad_slug() {
        let mut r = sample();
        assert_eq!(
            r.add("prod", "dup", dt("2026-06-16T00:00:00Z")),
            Err(TenantRegistryError::SlugTaken("prod".into()))
        );
        assert!(matches!(
            r.add("bad slug", "x", dt("2026-06-16T00:00:00Z")),
            Err(TenantRegistryError::InvalidSlug(_))
        ));
        assert!(matches!(
            r.add("../etc", "x", dt("2026-06-16T00:00:00Z")),
            Err(TenantRegistryError::InvalidSlug(_))
        ));
    }

    #[test]
    fn archive_refuses_running_tenant() {
        let mut r = sample();
        assert_eq!(
            r.archive("prod", "prod"),
            Err(TenantRegistryError::CannotArchiveRunning("prod".into()))
        );
        // prod stays Active.
        assert!(r.is_active("prod"));
    }

    #[test]
    fn archive_refuses_only_active_tenant() {
        let mut r = TenantRegistry::default();
        r.add("solo", "Solo", dt("2026-06-16T00:00:00Z")).unwrap();
        // Not running it (running = something else), so the running guard
        // doesn't fire — the only-active guard must.
        assert_eq!(
            r.archive("solo", "other"),
            Err(TenantRegistryError::CannotArchiveOnlyActive("solo".into()))
        );
        assert!(r.is_active("solo"));
    }

    #[test]
    fn archive_then_restore_lifecycle() {
        let mut r = sample();
        // Running prod, archive test (two Active, not running test → ok).
        r.archive("test", "prod").unwrap();
        assert!(!r.is_active("test"));
        assert_eq!(r.find("test").unwrap().state, TenantState::Archived);
        r.restore("test").unwrap();
        assert!(r.is_active("test"));
    }

    #[test]
    fn archive_and_restore_unknown_slug_errors() {
        let mut r = sample();
        assert_eq!(
            r.archive("ghost", "prod"),
            Err(TenantRegistryError::NotFound("ghost".into()))
        );
        assert_eq!(
            r.restore("ghost"),
            Err(TenantRegistryError::NotFound("ghost".into()))
        );
    }

    #[test]
    fn hint_is_honor_once() {
        let dir = test_dir();
        let p = dir.as_path().join("next_tenant");
        assert_eq!(consume_next_tenant_hint_at(&p).unwrap(), None);
        write_next_tenant_hint_at(&p, "test").unwrap();
        assert_eq!(
            consume_next_tenant_hint_at(&p).unwrap(),
            Some("test".to_string())
        );
        // Second read sees nothing — the hint was consumed + deleted.
        assert_eq!(consume_next_tenant_hint_at(&p).unwrap(), None);
        assert!(!p.exists());
    }

    #[test]
    fn empty_hint_reads_as_none() {
        let dir = test_dir();
        let p = dir.as_path().join("next_tenant");
        write_next_tenant_hint_at(&p, "   ").unwrap();
        assert_eq!(consume_next_tenant_hint_at(&p).unwrap(), None);
    }

    #[test]
    fn demo_state_round_trips_and_is_bootable_not_active() {
        let mut r = TenantRegistry::default();
        r.add_demo(dt("2026-06-16T00:00:00Z")).unwrap();
        let back = TenantRegistry::parse_toml(&r.to_toml()).unwrap();
        assert_eq!(r, back);
        assert_eq!(back.find("demo").unwrap().state, TenantState::Demo);
        // Demo is bootable/switchable but does NOT count as Active (so it
        // never satisfies the keep-≥1-Active archive guard).
        assert!(back.is_bootable("demo"));
        assert!(!back.is_active("demo"));
    }

    #[test]
    fn archive_refuses_demo_tenant() {
        let mut r = TenantRegistry::default();
        r.add("prod", "Prod", dt("2026-06-16T00:00:00Z")).unwrap();
        r.add_demo(dt("2026-06-16T00:00:00Z")).unwrap();
        // Running prod, try to archive demo → refused (safety net).
        assert_eq!(
            r.archive("demo", "prod"),
            Err(TenantRegistryError::CannotArchiveDemo("demo".into()))
        );
        assert_eq!(r.find("demo").unwrap().state, TenantState::Demo);
    }

    #[test]
    fn is_fresh_install_detects_empty_vs_inflight() {
        let dir = test_dir();
        let root = dir.as_path();
        // Empty ~/.aberp dir → fresh.
        assert!(is_fresh_install(root).unwrap());
        // A registry file present → not fresh.
        let reg_path = root.join(REGISTRY_FILENAME);
        std::fs::write(&reg_path, b"# registry").unwrap();
        assert!(!is_fresh_install(root).unwrap());
        std::fs::remove_file(&reg_path).unwrap();
        // A per-tenant DB present (install in flight) → not fresh.
        let db = root.join("prod").join(TENANT_DB_FILENAME);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"duck").unwrap();
        assert!(!is_fresh_install(root).unwrap());
    }

    /// CRUD: the full Create → Switch → Archive → Restore lifecycle,
    /// persisted through disk (registry file + hint file) at each step —
    /// the same surfaces the routes drive.
    #[test]
    fn full_lifecycle_create_switch_archive_restore() {
        let dir = test_dir();
        let reg_path = dir.as_path().join("tenants.toml");
        let hint_path = dir.as_path().join("next_tenant");

        // Boot tenant prod (running) + create acme.
        let mut reg = TenantRegistry::default();
        reg.add("prod", "Prod", dt("2026-06-16T00:00:00Z")).unwrap();
        reg.add("acme", "ACME", dt("2026-06-16T01:00:00Z")).unwrap();
        reg.write_to(&reg_path).unwrap();

        // Switch to acme → hint written, honored once.
        write_next_tenant_hint_at(&hint_path, "acme").unwrap();
        assert_eq!(
            consume_next_tenant_hint_at(&hint_path).unwrap(),
            Some("acme".to_string())
        );
        assert_eq!(consume_next_tenant_hint_at(&hint_path).unwrap(), None);

        // Now running=acme: archive prod (not running, two Active → ok).
        let mut reg = TenantRegistry::read_from(&reg_path).unwrap();
        reg.archive("prod", "acme").unwrap();
        reg.write_to(&reg_path).unwrap();
        assert!(!TenantRegistry::read_from(&reg_path)
            .unwrap()
            .is_active("prod"));

        // Restore prod.
        let mut reg = TenantRegistry::read_from(&reg_path).unwrap();
        reg.restore("prod").unwrap();
        reg.write_to(&reg_path).unwrap();
        assert!(TenantRegistry::read_from(&reg_path)
            .unwrap()
            .is_active("prod"));
    }

    /// Audit isolation: TenantCreated lands in the NEW tenant's ledger,
    /// never in the caller's chain (per-tenant chain isolation).
    #[test]
    fn tenant_created_lands_in_new_tenant_ledger_not_caller() {
        let dir = test_dir();
        let bh = BinaryHash::from_bytes([7u8; 32]);
        let db_caller = dir.as_path().join("prod").join("aberp.duckdb");
        let db_new = dir.as_path().join("acme").join("aberp.duckdb");
        std::fs::create_dir_all(db_caller.parent().unwrap()).unwrap();
        std::fs::create_dir_all(db_new.parent().unwrap()).unwrap();

        // Initialise the caller's ledger so it exists but carries no
        // tenant events.
        Ledger::open(&db_caller, TenantId::new("prod").unwrap(), bh).unwrap();

        let payload = crate::audit_payloads::TenantCreatedPayload {
            slug: "acme".to_string(),
            display_name: "ACME".to_string(),
            created_at: "2026-06-16T01:00:00Z".to_string(),
            creator_login: "op".to_string(),
        };
        emit_tenant_event(
            &db_new,
            TenantId::new("acme").unwrap(),
            bh,
            "op",
            EventKind::TenantCreated,
            payload.to_bytes(),
        )
        .unwrap();

        let new_kinds: Vec<EventKind> = Ledger::open(&db_new, TenantId::new("acme").unwrap(), bh)
            .unwrap()
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert!(
            new_kinds.contains(&EventKind::TenantCreated),
            "new tenant ledger must carry TenantCreated"
        );

        let caller_kinds: Vec<EventKind> =
            Ledger::open(&db_caller, TenantId::new("prod").unwrap(), bh)
                .unwrap()
                .entries()
                .unwrap()
                .into_iter()
                .map(|e| e.kind)
                .collect();
        assert!(
            !caller_kinds.contains(&EventKind::TenantCreated),
            "caller ledger must NOT carry the new tenant's TenantCreated"
        );
    }
}
