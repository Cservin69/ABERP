//! The **DB-path shape rule** for the boot guard trio, as a pure function.
//!
//! `guard_tenant_matches_build` clears the tenant NAME; `guard_db_matches_tenant`
//! clears the resolved PATH against the tenant. Neither says anything about the
//! path being a well-formed DuckDB database path at all, and this module is that
//! third check:
//!
//! * **Extension agreement.** The resolved DB path must end in `.duckdb`.
//!   `extension()` is `None` for `:memory:` and for an extension-less path; both
//!   are refusals, not exemptions.
//! * **No URI-shaped path.** A name beginning with `file:` is refused, because a
//!   URI-aware opener reads it as a scheme and opens the ABSOLUTE path that
//!   follows, while `Path` reads the same string as a RELATIVE path — so the
//!   guards and the opener would be talking about different files.
//!
//! **Pure.** No `cfg!`, no environment, no filesystem — the path is the only
//! input, which is what makes every arm testable from one build. Comparison is
//! lexical on the components given: the caller canonicalises first if it wants
//! that, and a lexical check cannot be defeated by a path that does not exist
//! yet.
//!
//! History: this began as the engine ↔ DB-path cross-check of the retired
//! DuckDB→SQLite migration experiment (ADR-0108 C-I/C-II). The experiment is
//! withdrawn and the tree is DuckDB-only again, so the engine-selector half is
//! gone; the two refusals above were live in the default build and are kept
//! exactly as they were. See `adr/0107` and `adr/0109` for why the engine did
//! not prove swappable.

use std::path::{Path, PathBuf};

/// Why a resolved DB path is not acceptable.
///
/// Every variant is a **refusal**, and the message names the path and what was
/// expected — CLAUDE.md rule 11: the caller must not be able to paper this over
/// into a default.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbPathMismatch {
    /// The path's extension is not `duckdb`.
    #[error(
        "DB-path mismatch: the resolved DB path `{path}` does not end in `.{expected}` — \
         the transactional store is DuckDB and its file must be named as one"
    )]
    Extension {
        /// The resolved DB path that was refused.
        path: PathBuf,
        /// The extension required.
        expected: &'static str,
    },

    /// The path is URI-shaped, so the opener and every path-based guard would
    /// disagree about which file it names.
    #[error(
        "URI-shaped DB path: `{path}` begins with a `file:` scheme. A URI-aware opener \
         parses such a name as a URI and opens the ABSOLUTE path inside it — while `Path` \
         reads the same string as a RELATIVE path and every guard here clears it on that \
         reading. Pass a plain filesystem path"
    )]
    UriShapedPath {
        /// The URI-shaped path that was refused.
        path: PathBuf,
    },
}

/// The extension a DuckDB database path must carry.
pub const REQUIRED_EXTENSION: &str = "duckdb";

/// Is `path` a well-formed DuckDB database path?
pub fn db_path_agrees(path: &Path) -> Result<(), DbPathMismatch> {
    // The URI arm goes FIRST, because it is the one that makes the extension
    // arm below unsound rather than merely incomplete: it decides from the
    // string read as a `Path`, and for exactly these names the guard and the
    // opener are talking about different files.
    //
    // Refused rather than normalised: rewriting a URI into the path it denotes
    // would mean re-implementing a URI parser (query parameters, `%` escapes,
    // authority) in a security guard, and being wrong there is the same failure
    // with more code. Nothing in this tree passes a URI.
    if path
        .as_os_str()
        .to_str()
        .is_some_and(|s| s.starts_with("file:"))
    {
        return Err(DbPathMismatch::UriShapedPath {
            path: path.to_path_buf(),
        });
    }

    if path.extension().and_then(|e| e.to_str()) != Some(REQUIRED_EXTENSION) {
        return Err(DbPathMismatch::Extension {
            path: path.to_path_buf(),
            expected: REQUIRED_EXTENSION,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duckdb_file_is_accepted() {
        assert!(db_path_agrees(Path::new("/tmp/dev/aberp.duckdb")).is_ok());
    }

    #[test]
    fn a_non_duckdb_file_is_refused() {
        assert!(matches!(
            db_path_agrees(Path::new("/tmp/dev/aberp.sqlite")),
            Err(DbPathMismatch::Extension { .. })
        ));
    }

    /// A DuckDB path under the production root is ordinary production
    /// operation. Refusing it here would break every prod boot, which is the
    /// opposite of what this guard is for.
    #[test]
    fn a_duckdb_file_under_the_production_root_is_accepted() {
        assert!(db_path_agrees(Path::new("/Users/x/.aberp/prod/aberp.duckdb")).is_ok());
    }

    #[test]
    fn extensionless_and_in_memory_paths_are_refused() {
        for p in [":memory:", "/tmp/dev/aberp", "/tmp/dev/"] {
            assert!(
                matches!(
                    db_path_agrees(Path::new(p)),
                    Err(DbPathMismatch::Extension { .. })
                ),
                "`{p}` must be refused: it names no DuckDB file"
            );
        }
    }

    #[test]
    fn a_uri_shaped_path_is_refused_before_any_other_arm() {
        // The premise, asserted rather than assumed: this name carries the
        // required extension and would otherwise clear every arm, so the URI
        // refusal is doing real work rather than duplicating the extension one.
        let uri = Path::new("file:/Users/x/.aberp/prod/aberp.duckdb");
        assert_eq!(uri.extension().and_then(|e| e.to_str()), Some("duckdb"));
        assert!(matches!(
            db_path_agrees(uri),
            Err(DbPathMismatch::UriShapedPath { .. })
        ));
    }

    #[test]
    fn a_path_that_merely_contains_file_colon_is_not_refused() {
        // The arm is anchored at the START of the string; a directory named
        // `file:` further along is a legitimate (if odd) filesystem path.
        assert!(db_path_agrees(Path::new("/tmp/file:x/aberp.duckdb")).is_ok());
    }
}
