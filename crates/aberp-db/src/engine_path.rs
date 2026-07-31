//! ADR-0108 Step 1 — the **engine ↔ DB-path agreement rule**, as a pure function.
//!
//! # What it defends (ADR-0108 C-I / C-II)
//!
//! The whole reversibility argument of the SQLite migration reduces to one
//! property: **a `sqlite-engine` build never opens `aberp.duckdb`.** It opens
//! `aberp.sqlite`, and the DuckDB file is therefore byte-unmodified for the
//! entire reversible window. ADR-0108 §6.1 states that property; this module is
//! the mechanism that makes it *checked* rather than hoped.
//!
//! Two arms, both here:
//!
//! * **C-I — extension agreement.** A [`Engine::Sqlite`] binary whose resolved
//!   DB path does not end in `.sqlite` is refused; a [`Engine::DuckDb`] binary
//!   whose path does not end in `.duckdb` is refused.
//! * **C-II — DEV-only.** A [`Engine::Sqlite`] binary whose resolved path lands
//!   anywhere under the production root (`~/.aberp/`) is refused. The migration
//!   is authorised for the DEV tenant only; nothing in ADR-0108 §7 may read,
//!   write, or stat production data.
//!
//! # Why it is a pure function and not a `cfg!` block (ADR-0108 §7 Step 1, F6)
//!
//! As first drafted the refusal was "inert while no `sqlite-engine` feature
//! exists" — which would leave the arm that actually carries C-I
//! (*a SQLite binary refuses a non-`.sqlite` path*) **unbuildable, and therefore
//! unpinned, across Steps 1 and 2** — including Step 2, the step that links
//! `rusqlite` into the tree. Taking the engine as an *argument* instead of
//! reading `cfg!` makes **both arms unit-testable and mutation-verifiable in
//! Step 1 with no feature enabled at all**. Step 3 adds only the three-line
//! `cfg!`-driven caller at the boot site.
//!
//! The generalised ordering rule ADR-0108 draws from this: **a refusal whose
//! test cannot be written yet is not landed yet.**
//!
//! # Deliberately NOT wired into `serve` yet
//!
//! Step 1 lands the decision and its pins. There is no caller: with only one
//! engine in the tree the DuckDB arm would start refusing the many legitimate
//! non-`.duckdb` DB paths that tests and tooling use, which is scope this step
//! does not own. Step 3 wires the boot cross-check when the second engine (and
//! therefore the choice) actually exists.

use std::path::{Path, PathBuf};

/// Which storage engine a binary was **built** with. ADR-0108 §2.2 D1: the
/// selector is compile-time (`sqlite-engine` cargo feature, default OFF), so
/// exactly one engine is linked into any given binary. This enum is the
/// *value* form of that choice, so the decision below can be tested for both
/// engines from one build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The default build. Opens `*.duckdb`.
    DuckDb,
    /// The `sqlite-engine` build. Opens `*.sqlite`.
    Sqlite,
}

impl Engine {
    /// The file extension this engine's DB path must carry.
    ///
    /// This is the whole of C-I: the two engines use **two different files**,
    /// which is where the reversibility comes from — not from the selector
    /// (ADR-0108 §2.2, Q1).
    pub fn required_extension(self) -> &'static str {
        match self {
            Engine::DuckDb => "duckdb",
            Engine::Sqlite => "sqlite",
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Engine::DuckDb => f.write_str("duckdb"),
            Engine::Sqlite => f.write_str("sqlite"),
        }
    }
}

/// Why a resolved DB path is not acceptable for the engine that was built.
///
/// Every variant is a **refusal**, and the message names the engine, the path,
/// and what was expected — CLAUDE.md rule 11: the caller must not be able to
/// paper this over into a default.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineMismatch {
    /// C-I. The path's extension does not match the linked engine.
    #[error(
        "engine/DB-path mismatch: this is a `{engine}` build but the resolved DB path \
         `{path}` does not end in `.{expected}` — a {engine} build must never open the \
         other engine's file (ADR-0108 C-I)"
    )]
    Extension {
        /// The engine linked into this binary.
        engine: Engine,
        /// The resolved DB path that was refused.
        path: PathBuf,
        /// The extension the engine requires.
        expected: &'static str,
    },

    /// C-II. A `sqlite-engine` build resolved a path under the production root.
    #[error(
        "DEV-only violation: this is a `sqlite` build but the resolved DB path `{path}` \
         is under the production root `{prod_root}` — the ADR-0108 migration is \
         authorised for the DEV tenant only (C-II)"
    )]
    SqliteUnderProdRoot {
        /// The resolved DB path that was refused.
        path: PathBuf,
        /// The production root it landed under.
        prod_root: PathBuf,
    },
}

/// Does `path` agree with the `engine` this binary was built with?
///
/// **Pure.** No `cfg!`, no environment, no filesystem — every input is an
/// argument, which is what makes both arms testable from a single default
/// build (see the module docs, ADR-0108 F6).
///
/// `prod_root` is the production data root, normally `~/.aberp` — passed in
/// rather than resolved here so this stays pure and so a test can point it at
/// a scratch directory. `None` skips the C-II arm (the caller could not resolve
/// a home directory); it does **not** weaken C-I.
///
/// Path comparison is **lexical on the components given**, deliberately: the
/// caller resolves/canonicalises before calling if it wants that, and a purely
/// lexical check cannot be defeated by a path that does not exist yet — which
/// is the case for `aberp.sqlite` before the migrator has run.
pub fn engine_path_agrees(
    engine: Engine,
    path: &Path,
    prod_root: Option<&Path>,
) -> Result<(), EngineMismatch> {
    // C-I — extension agreement. `extension()` is None for `:memory:` and for
    // an extension-less path; both are refusals, not exemptions.
    let expected = engine.required_extension();
    let actual = path.extension().and_then(|e| e.to_str());
    if actual != Some(expected) {
        return Err(EngineMismatch::Extension {
            engine,
            path: path.to_path_buf(),
            expected,
        });
    }

    // C-II — DEV-only, and ONLY for the SQLite build. A DuckDB build under
    // `~/.aberp/` is ordinary production operation; refusing it here would
    // break every prod boot, which is the opposite of what this guard is for.
    if engine == Engine::Sqlite {
        if let Some(root) = prod_root {
            if path.starts_with(root) {
                return Err(EngineMismatch::SqliteUnderProdRoot {
                    path: path.to_path_buf(),
                    prod_root: root.to_path_buf(),
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prod_root() -> PathBuf {
        PathBuf::from("/Users/op/.aberp")
    }

    /// T-13 arm 1 — the arm the whole reversibility argument rests on
    /// (ADR-0108 §6.1): a SQLite build must refuse the DuckDB file.
    ///
    /// This is the mutation-verified pin: delete the extension check in
    /// [`engine_path_agrees`] and this test goes red.
    #[test]
    fn sqlite_build_refuses_the_duckdb_file() {
        let err = engine_path_agrees(
            Engine::Sqlite,
            Path::new("/dev/apps/aberp-ui/aberp.duckdb"),
            Some(&prod_root()),
        )
        .expect_err("a sqlite build must never open the duckdb file");
        assert!(
            matches!(
                err,
                EngineMismatch::Extension {
                    engine: Engine::Sqlite,
                    ..
                }
            ),
            "expected an extension refusal, got {err:?}"
        );
        // The message must name what to fix, not just that something is wrong.
        let msg = err.to_string();
        assert!(
            msg.contains(".sqlite"),
            "refusal must name the expected extension: {msg}"
        );
        assert!(
            msg.contains("aberp.duckdb"),
            "refusal must name the offending path: {msg}"
        );
    }

    /// T-13 arm 2 — the other direction. A default (DuckDB) build must refuse
    /// the SQLite file, so a mis-set `ABERP_DB` cannot point the DuckDB engine
    /// at a file it would rewrite in its own format.
    #[test]
    fn duckdb_build_refuses_the_sqlite_file() {
        let err = engine_path_agrees(
            Engine::DuckDb,
            Path::new("/dev/apps/aberp-ui/aberp.sqlite"),
            Some(&prod_root()),
        )
        .expect_err("a duckdb build must never open the sqlite file");
        assert!(matches!(
            err,
            EngineMismatch::Extension {
                engine: Engine::DuckDb,
                expected: "duckdb",
                ..
            }
        ));
    }

    /// T-13 arm 3 (C-II) — a SQLite build is refused anywhere under the
    /// production root, even with the correct extension. The migration is
    /// DEV-only; a `.sqlite` file under `~/.aberp/prod/` is exactly the
    /// operator mistake this arm exists to stop.
    #[test]
    fn sqlite_build_refuses_a_path_under_the_production_root() {
        let err = engine_path_agrees(
            Engine::Sqlite,
            Path::new("/Users/op/.aberp/prod/aberp.sqlite"),
            Some(&prod_root()),
        )
        .expect_err("the ADR-0108 migration is DEV-only");
        assert!(
            matches!(err, EngineMismatch::SqliteUnderProdRoot { .. }),
            "{err:?}"
        );
    }

    /// C-II must NOT fire for the DuckDB engine: production runs DuckDB under
    /// `~/.aberp/` and always has. A guard that refuses that would take prod
    /// down, which is why the arm is engine-scoped rather than path-only.
    #[test]
    fn duckdb_build_is_allowed_under_the_production_root() {
        engine_path_agrees(
            Engine::DuckDb,
            Path::new("/Users/op/.aberp/prod/aberp.duckdb"),
            Some(&prod_root()),
        )
        .expect("production DuckDB under ~/.aberp is ordinary operation");
    }

    /// The happy path both engines are actually built for (ADR-0108 §2.5).
    #[test]
    fn each_engine_accepts_its_own_dev_file() {
        engine_path_agrees(
            Engine::DuckDb,
            Path::new("/dev/apps/aberp-ui/aberp.duckdb"),
            Some(&prod_root()),
        )
        .expect("duckdb build on the duckdb dev file");
        engine_path_agrees(
            Engine::Sqlite,
            Path::new("/dev/apps/aberp-ui/aberp.sqlite"),
            Some(&prod_root()),
        )
        .expect("sqlite build on the sqlite dev file");
    }

    /// An extension-less path and `:memory:` are refusals, not exemptions —
    /// "no extension" must not read as "any engine may open it".
    #[test]
    fn extensionless_and_in_memory_paths_are_refused() {
        for p in ["/dev/apps/aberp-ui/aberp", ":memory:"] {
            engine_path_agrees(Engine::Sqlite, Path::new(p), Some(&prod_root()))
                .expect_err("an extension-less path must be refused, not waved through");
        }
    }

    /// `prod_root: None` (the caller could not resolve a home directory) skips
    /// C-II but must NOT weaken C-I — a failure to resolve `~` is not a licence
    /// to open the other engine's file.
    #[test]
    fn unresolvable_prod_root_does_not_weaken_the_extension_arm() {
        engine_path_agrees(Engine::Sqlite, Path::new("/x/aberp.duckdb"), None)
            .expect_err("C-I holds regardless of whether the prod root is known");
    }

    /// A sibling-named directory must not count as "under" the prod root:
    /// `/Users/op/.aberp-scratch/` is not `/Users/op/.aberp/`. `Path::starts_with`
    /// is component-wise (not a string prefix), and this pins that.
    #[test]
    fn prod_root_match_is_component_wise_not_a_string_prefix() {
        engine_path_agrees(
            Engine::Sqlite,
            Path::new("/Users/op/.aberp-scratch/aberp.sqlite"),
            Some(&prod_root()),
        )
        .expect("a sibling directory is not under the production root");
    }
}
