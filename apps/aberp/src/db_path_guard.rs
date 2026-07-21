//! The tenant DB-binding guard — refuse a database that belongs to a
//! FOREIGN tenant.
//!
//! `serve::guard_tenant_matches_build` cross-checks the build flavour
//! against the tenant SLUG, and nothing checked the resolved DB PATH. So
//! `ABERP_TENANT=test ABERP_DB=~/.aberp/prod/aberp.duckdb` booted a DEV
//! binary directly onto the production DuckDB and passed every guard —
//! the launcher-supplied path was trusted end to end. This module is the
//! path half of that pair; `serve::guard_db_matches_tenant` is its one
//! caller, and it fires before the port binds, before the keychain is
//! touched, and before DuckDB opens the file.
//!
//! Ported from the `ABERP-Editions` edition-isolation guard
//! (`tenant_registry::ensure_db_path_isolated` there). This repo has no
//! `Edition` type, so the axis is the tenant slug rather than the
//! edition, and the rule is correspondingly simpler: a build opens only
//! `~/.aberp/<its own tenant>/`. It also fixes a residual in the
//! Editions original — see [`ensure_db_path_isolated`].

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Resolve `path` as far as the filesystem allows, then re-append the
/// components that do not exist yet.
///
/// [`std::fs::canonicalize`] resolves symlinks and `..`, but fails
/// outright when any component is missing — and the DB path legitimately
/// does not exist on a first launch (its parent dir is created later in
/// the serve boot). So: canonicalize the deepest ancestor that DOES
/// exist, then re-join the missing tail. Relative inputs are made
/// absolute against the CWD first, so `./aberp.duckdb` compares as the
/// real path it names rather than as a bare filename.
///
/// **The tail is UNVERIFIED, and this function does not classify it.**
/// Whatever could not be resolved is re-appended verbatim, so the result
/// is only as trustworthy as the caller's own check of it. Two shapes are
/// actively dangerous, and [`ensure_db_path_isolated_under`] refuses both
/// before it compares anything:
///
///   * an unresolved `..`, and
///   * a tail that begins at a *dangling symlink*.
///
/// An earlier version of this comment claimed the `..` case was
/// "fail-closed, never fail-open". **That was false, and execution
/// disproved it.** The walk below stops at the deepest ancestor that
/// EXISTS, so for `<root>/<own tenant>/missing/../../prod/aberp.duckdb`
/// the deepest existing ancestor is `<root>/<own tenant>` and the
/// re-appended tail keeps the tenant's OWN segment first — it reads as
/// this tenant's own and is allowed. It was reachable, not merely
/// theoretical: `serve` runs `create_dir_all` on the parent AFTER the
/// guard, which materialises the missing component and makes that exact
/// string resolve into the foreign tenant's root. It was demonstrated
/// opening a seeded foreign DB. Both refusals are pinned by
/// `adversarial_*` tests below; do not relax them on the strength of a
/// comment.
fn canonicalize_deepest(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            // No CWD: leave it relative. A relative path cannot be
            // compared against the absolute tenant root, so the caller
            // treats it as an ordinary dev path.
            Err(_) => path.to_path_buf(),
        }
    };
    fn walk(p: &Path) -> PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(tail)) => walk(parent).join(tail),
            // Filesystem root, or a path with no nameable tail: nothing
            // left to peel.
            _ => p.to_path_buf(),
        }
    }
    walk(&abs)
}

/// Refuse any DB path that resolves into a FOREIGN tenant's root under
/// `~/.aberp/`.
///
/// A build runs as exactly one tenant (`--tenant` / `ABERP_TENANT`, which
/// `serve::guard_tenant_matches_build` has already cross-checked against
/// the compile-time prod/dev flavour). Its database therefore belongs
/// under `~/.aberp/<tenant>/` and nowhere else inside the data root. This
/// is the runtime backstop that makes "a dev build cannot open prod's
/// database" true even when `--db` / `ABERP_DB` says otherwise: with
/// `tenant != "prod"` already enforced for every dev build, a path under
/// `~/.aberp/prod/` is by construction a foreign tenant root, so the
/// single rule below subsumes the non-production-build case rather than
/// needing a second clause for it.
///
/// Paths OUTSIDE `~/.aberp/` entirely are allowed — `./aberp.duckdb` (the
/// `run_desktop.sh` default), a temp dir, a scratch copy. Those are the
/// ordinary dev and test paths; the invariant is about never reaching
/// into another tenant's data, not about forbidding every unusual
/// location.
///
/// **Fixed residual vs the Editions original.** That version matches on
/// path *components* and never canonicalizes, so a symlink into the data
/// root (`~/link -> ~/.aberp`, passed as `~/link/prod/aberp.duckdb`)
/// carries no `.aberp` component and slips straight through. Both sides
/// are canonicalized here before comparison, which closes it.
pub fn ensure_db_path_isolated(path: &Path, tenant: &str) -> Result<()> {
    ensure_db_path_isolated_under(&crate::tenant_registry::aberp_root()?, path, tenant)
}

/// The decision logic behind [`ensure_db_path_isolated`], with the data
/// root injected rather than read from `$HOME`. Mirrors the
/// `serve::sanity_check_environment` pattern: the rule is unit-testable
/// against a temp root, with no process-global `HOME` mutation and no
/// risk of a test resolving the operator's real `~/.aberp/`.
/// The shallowest ancestor of `path` that the filesystem could NOT
/// resolve — exactly where a [`canonicalize_deepest`] result stops being
/// verified and becomes re-appended text. `None` when the path fully
/// exists.
fn unresolved_boundary(path: &Path) -> Option<PathBuf> {
    let mut acc = PathBuf::new();
    for c in path.components() {
        acc.push(c);
        if acc.canonicalize().is_err() {
            return Some(acc);
        }
    }
    None
}

fn ensure_db_path_isolated_under(root: &Path, path: &Path, tenant: &str) -> Result<()> {
    let root = canonicalize_deepest(root);
    let db = canonicalize_deepest(path);

    // An UNRESOLVED `..` survives only when some component of the path does
    // not exist yet, so the filesystem could not resolve it. Such a path
    // cannot be classified: the comparison below reads the segment as
    // written, while `serve`'s writer-lock step then calls `create_dir_all`
    // on the parent — which MATERIALISES the missing component and makes
    // the very same string resolve somewhere else entirely. `<root>/<own
    // tenant>/missing/../../prod/aberp.duckdb` reads as this tenant's own
    // and lands in prod's. Unclassifiable means refused.
    if db
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(anyhow!(
            "tenant DB isolation: this build runs as tenant '{tenant}' and refuses path {} — it \
             contains a `..` that the filesystem could not resolve (a component does not exist), \
             so where it will finally point cannot be determined. Pass a path with no `..`.",
            path.display(),
        ));
    }

    // A DANGLING SYMLINK where the resolved prefix ends is unclassifiable
    // for the same reason. The segment EXISTS as a link but has no target,
    // so the comparison below reads the LINK's own name — `<root>/<own
    // tenant>` reads as this tenant's — while the file the OS eventually
    // opens is wherever the link points. Nothing in this guard sees the
    // target. Today that is caught downstream only by ACCIDENT (`serve`'s
    // `create_dir_all` gets EEXIST on a dangling link and errors out), and
    // a safety guard must not rest on a downstream accident.
    if let Some(boundary) = unresolved_boundary(&db) {
        if std::fs::symlink_metadata(&boundary).is_ok() {
            return Err(anyhow!(
                "tenant DB isolation: this build runs as tenant '{tenant}' and refuses path {} — \
                 the component {} is a symlink whose target does not exist, so the file that \
                 would finally be opened cannot be determined. Create the target, or pass the \
                 real path.",
                path.display(),
                boundary.display(),
            ));
        }
    }

    // Outside the ABERP data root altogether — an ordinary dev/test path.
    let Ok(under_root) = db.strip_prefix(&root) else {
        return Ok(());
    };

    let owned_by_this_tenant = match under_root.components().next() {
        Some(std::path::Component::Normal(seg)) => seg == std::ffi::OsStr::new(tenant),
        // The data root itself, or a path with no tenant segment
        // (`~/.aberp/tenants.toml`) — not this tenant's DB either way.
        _ => false,
    };
    if owned_by_this_tenant {
        return Ok(());
    }

    Err(anyhow!(
        "tenant DB isolation: this build runs as tenant '{tenant}' and refuses path {} — it \
         resolves to {}, inside the ABERP data root {} but NOT under this tenant's own {}/. \
         A build opens only its own tenant's database.",
        path.display(),
        db.display(),
        root.display(),
        root.join(tenant).display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use ulid::Ulid;

    /// Unique temp dir under the system temp root. Matches the
    /// no-`tempfile` posture of `tenant_registry`'s tests; the per-test
    /// dir is leaked at end-of-test, acceptable for the OS-temp-root
    /// posture. NEVER `~/.aberp`.
    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join("aberp-db-path-guard")
            .join(Ulid::new().to_string());
        fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    // The root is injected, so these never resolve — let alone touch —
    // the operator's real `~/.aberp/`. The process-level refusal pins
    // (exit code, untouched prod file) live in
    // `tests/serve_db_path_guard.rs`; what is pinned HERE is the rule
    // itself, including the paths that must stay ALLOWED — a guard that
    // refuses a legitimate launch is an outage, not a safeguard.

    /// A path under a FOREIGN tenant's root is refused. `prod` is the
    /// case that matters: a dev build runs as some non-prod tenant
    /// (`guard_tenant_matches_build` guarantees it), so prod's root is
    /// always foreign to it.
    #[test]
    fn refuses_foreign_tenant_root() {
        let root = test_dir().join(".aberp");
        for (tenant, db) in [
            ("test", root.join("prod").join("aberp.duckdb")),
            ("prod", root.join("test").join("aberp.duckdb")),
            ("demo", root.join("prod").join("aberp.duckdb")),
        ] {
            assert!(
                ensure_db_path_isolated_under(&root, &db, tenant).is_err(),
                "tenant '{tenant}' must be refused {}",
                db.display()
            );
        }
    }

    /// A path directly in the data root has no tenant segment at all —
    /// it belongs to no tenant, so it is refused too (`tenants.toml`,
    /// the registry itself).
    #[test]
    fn refuses_the_bare_data_root() {
        let root = test_dir().join(".aberp");
        assert!(ensure_db_path_isolated_under(&root, &root.join("tenants.toml"), "test").is_err());
        assert!(ensure_db_path_isolated_under(&root, &root, "test").is_err());
    }

    /// The tenant's OWN root is allowed — this is precisely what
    /// `run_prod.sh` sets `ABERP_DB` to.
    #[test]
    fn allows_own_tenant_root() {
        let root = test_dir().join(".aberp");
        for tenant in ["prod", "test", "demo"] {
            let own = root.join(tenant).join("aberp.duckdb");
            assert!(
                ensure_db_path_isolated_under(&root, &own, tenant).is_ok(),
                "tenant '{tenant}' must be allowed its own {}",
                own.display()
            );
        }
    }

    /// Paths outside the data root entirely are ordinary dev/test paths
    /// and stay allowed — `run_desktop.sh` defaults to `./aberp.duckdb`,
    /// and the test suite runs on temp dirs. The rule is "never reach
    /// into another tenant's data", not "only ever this one location".
    #[test]
    fn allows_paths_outside_the_data_root() {
        let base = test_dir();
        let root = base.join(".aberp");
        for db in [
            base.join("aberp.duckdb"),
            base.join("scratch").join("aberp.duckdb"),
            PathBuf::from("./aberp.duckdb"),
        ] {
            assert!(
                ensure_db_path_isolated_under(&root, &db, "test").is_ok(),
                "{} is outside the data root and must be allowed",
                db.display()
            );
        }
    }

    /// THE RESIDUAL. The Editions original matches on path COMPONENTS and
    /// never canonicalizes, so a symlink into the data root carries no
    /// `.aberp` component and slips through. Canonicalizing both sides
    /// catches it. Pinned here as a unit too (not only end-to-end) so the
    /// rule itself records the requirement.
    #[cfg(unix)]
    #[test]
    fn refuses_foreign_root_reached_through_a_symlink() {
        let base = test_dir();
        let root = base.join(".aberp");
        fs::create_dir_all(root.join("prod")).expect("create prod root");
        let link = base.join("link");
        std::os::unix::fs::symlink(&root, &link).expect("create symlink");

        let via_link = link.join("prod").join("aberp.duckdb");
        assert!(
            !via_link.components().any(|c| c.as_os_str() == ".aberp"),
            "test is not exercising the residual: path still carries .aberp"
        );
        assert!(
            ensure_db_path_isolated_under(&root, &via_link, "test").is_err(),
            "symlinked foreign-tenant path must be refused: {}",
            via_link.display()
        );
    }

    /// A `..` traversal that climbs out of the tenant's own root and back
    /// into a foreign one must be refused. Canonicalization resolves it
    /// when the directories exist.
    #[test]
    fn refuses_dotdot_traversal_into_foreign_root() {
        let root = test_dir().join(".aberp");
        fs::create_dir_all(root.join("test")).expect("create test root");
        fs::create_dir_all(root.join("prod")).expect("create prod root");

        let sneaky = root
            .join("test")
            .join("..")
            .join("prod")
            .join("aberp.duckdb");
        assert!(
            ensure_db_path_isolated_under(&root, &sneaky, "test").is_err(),
            "`..` traversal into prod must be refused: {}",
            sneaky.display()
        );
    }

    /// ADVERSARIAL: `..` behind a component that does not exist yet. The
    /// module docs claim this is fail-closed ("can only ever read as a
    /// non-tenant segment, which refuses"). It is not: the deepest existing
    /// ancestor is `<root>/test`, so the re-appended tail keeps `test` as
    /// the FIRST component and the guard reads it as this tenant's own.
    #[test]
    fn adversarial_dotdot_behind_a_missing_component() {
        let root = test_dir().join(".aberp");
        fs::create_dir_all(root.join("test")).expect("create test root");
        fs::create_dir_all(root.join("prod")).expect("create prod root");
        let sneaky = root
            .join("test")
            .join("does-not-exist")
            .join("..")
            .join("..")
            .join("prod")
            .join("aberp.duckdb");
        assert!(
            ensure_db_path_isolated_under(&root, &sneaky, "test").is_err(),
            "`..` behind a missing component must be refused: {}",
            sneaky.display()
        );
    }

    /// ADVERSARIAL: and the refusal must hold end-to-end, because
    /// `serve.rs`'s writer-lock step does `create_dir_all` on the parent —
    /// which MATERIALISES the missing component and makes the path resolve
    /// straight into the foreign tenant's root.
    #[test]
    fn adversarial_dotdot_path_really_reaches_the_foreign_root() {
        let root = test_dir().join(".aberp");
        fs::create_dir_all(root.join("test")).expect("create test root");
        fs::create_dir_all(root.join("prod")).expect("create prod root");
        let victim = root.join("prod").join("aberp.duckdb");
        fs::write(&victim, b"FOREIGN TENANT DATA").expect("seed foreign db");

        let sneaky = root
            .join("test")
            .join("does-not-exist")
            .join("..")
            .join("..")
            .join("prod")
            .join("aberp.duckdb");

        // serve.rs:877, verbatim.
        fs::create_dir_all(sneaky.parent().unwrap()).expect("serve's create_dir_all");
        assert_eq!(
            fs::read_to_string(&sneaky).expect("read through the sneaky path"),
            "FOREIGN TENANT DATA",
            "the path really does resolve into the foreign tenant's DB"
        );
        assert!(
            ensure_db_path_isolated_under(&root, &sneaky, "test").is_err(),
            "guard allowed a path that demonstrably opens the foreign tenant's DB"
        );
    }

    /// ADVERSARIAL (latent, not a demonstrated breach). The tenant's OWN
    /// segment is a DANGLING symlink. `canonicalize` cannot follow it, so
    /// the guard used to read the LINK's name — this tenant's own — and
    /// allow, while the file the OS would open is wherever the link
    /// points. Downstream `create_dir_all` happens to fail EEXIST on a
    /// dangling link, so nothing broke; the guard must refuse on its own
    /// merits rather than inherit that accident.
    #[cfg(unix)]
    #[test]
    fn adversarial_own_tenant_segment_is_a_dangling_symlink() {
        let root = test_dir().join(".aberp");
        fs::create_dir_all(&root).expect("create data root");
        // `<root>/test` -> `<root>/prod`, which does NOT exist yet.
        std::os::unix::fs::symlink(root.join("prod"), root.join("test")).expect("dangling link");
        assert!(
            root.join("test").symlink_metadata().is_ok() && !root.join("test").exists(),
            "test is not exercising the case: the link must exist and be dangling"
        );
        assert!(
            ensure_db_path_isolated_under(&root, &root.join("test").join("aberp.duckdb"), "test")
                .is_err(),
            "a dangling own-tenant symlink is unclassifiable and must be refused"
        );
    }

    /// The dangling-symlink refusal must NOT swallow the ordinary
    /// first-launch case it sits next to: a plainly MISSING component is
    /// not a dangling link and stays allowed. Without this, the guard
    /// above would be an outage on every fresh install.
    #[test]
    fn dangling_symlink_rule_leaves_plain_missing_paths_alone() {
        let root = test_dir().join(".aberp");
        assert!(
            ensure_db_path_isolated_under(&root, &root.join("test").join("aberp.duckdb"), "test")
                .is_ok(),
            "a merely missing tail must remain allowed"
        );
    }

    /// A DB path whose parent does not exist yet — the first-launch case
    /// — is still classified correctly. `canonicalize` alone fails here;
    /// `canonicalize_deepest` resolves the deepest existing ancestor.
    #[test]
    fn classifies_not_yet_existing_paths() {
        let root = test_dir().join(".aberp");
        // Nothing under `root` exists yet: neither tenant dir is created.
        assert!(
            ensure_db_path_isolated_under(&root, &root.join("test").join("aberp.duckdb"), "test")
                .is_ok(),
            "first-launch own-tenant path must be allowed before its dir exists"
        );
        assert!(
            ensure_db_path_isolated_under(&root, &root.join("prod").join("aberp.duckdb"), "test")
                .is_err(),
            "foreign-tenant path must be refused even before its dir exists"
        );
    }
}
