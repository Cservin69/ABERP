#!/usr/bin/env bash
#
# cut_gate_sqlite_posture.sh — ADR-0108 Step 2 (M2 / M3 / §5's standing
# prohibition). The grep half of the SQLite security posture.
#
# The runtime half — pragmas, db-configs, limits, file modes, version floor —
# is pinned by the mutation-verified tests in `crates/aberp-db/src/sqlite.rs`
# (T-3a..d, T-10, T-11). Those assert what an OPEN CONNECTION looks like. This
# gate asserts what the SOURCE TREE is allowed to contain, which is the half a
# runtime test structurally cannot see: a call site that has not run yet.
#
# CHECK S1 — no `rusqlite` feature that re-opens a closed door.
#   `load_extension` (M2) is the sharp one: enabling it makes
#   `Connection::load_extension` exist, and M2's strongest arm is that the
#   method IS NOT THERE TO CALL. `hooks` / `vtab` / `functions` / `series` are
#   PR #49 §8's standing prohibition (RUSTSEC-2021-0128, CVE-2020-35866).
#
# CHECK S2 — no call to the forbidden rusqlite APIs.
#   They are attractive during exactly this migration: a custom collation to
#   replace SQLite's ASCII-only `LOWER()` (M11 routes case-folding through Rust
#   precisely so that temptation has an answer), or an `update_hook` to feed
#   the audit ledger. Both would put ABERP logic inside a C callback that runs
#   under the storage engine's lock.
#
# CHECK S3 — no `ATTACH` in SQL (M3). The tree has zero today and the limit is
#   set to 0; this stops one being ADDED and quietly failing at runtime instead
#   of at review.
#
# CHECK S4 — no `rusqlite::Connection::open` outside the one sanctioned
#   hardened opener. A bare open skips EVERY mitigation above. This is the
#   ADR-0098 opener-census idea, applied to the new engine from its first day
#   rather than retrofitted after 227 openers exist.
#
# Scope: apps/*/src, crates/*/src, modules/*/src (source, not tests) plus every
# Cargo.toml. `crates/aberp-db/src/sqlite.rs` is the sanctioned seam and is
# excluded from S4 only — it is subject to S1..S3 like everything else.
#
# Exit 0 = green.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail=0
note() { printf '  %s\n' "$*"; }
echo "ADR-0108 SQLite posture cut-gate — root: $ROOT"

SRC_DIRS=()
for d in apps/*/src crates/*/src modules/*/src; do [[ -d "$d" ]] && SRC_DIRS+=("$d"); done
[[ ${#SRC_DIRS[@]} -gt 0 ]] || { note "✗ FAIL: no source directories found — the gate would be vacuously green"; exit 1; }

# The non-test source stream, as `<file>:<line>:<text>`.
#
# `#[cfg(test)]` items are excluded because a gate that cannot tell a
# PROHIBITION from its own PROOF is a gate that gets deleted: the test proving
# `ATTACH` is refused necessarily contains the string `ATTACH DATABASE`.
#
# The skip is per-ITEM, not a truncation. The first draft truncated each file at
# its first `#[cfg(test)]` — and the mutation probes escaped, because Rust puts
# `#[cfg(test)] mod tests` LAST, so everything appended after it was invisible.
# A gate blind to the end of every file is a fail-open, and it is exactly the
# shape PR #43's name-vs-shape lesson names.
#
# So: a top-level `#[cfg(test)]` (column 0) starts a skip that ends at the next
# `}` in column 0 — the close of that one item. Scanning resumes after it.
# Only a cfg that REQUIRES `test` counts (bare `#[cfg(test)]`, or `all(...)`
# with `test` as a bare conjunct) — `#[cfg(feature = "test-support")]` and
# `#[cfg(not(test))]` COMPILE INTO NON-TEST BUILDS and must stay scanned. That
# is the ADR-0106 sweep's finding, applied here from day one rather than after
# a fail-open is found in production.
nontest_source() {
  local d f
  for d in "${SRC_DIRS[@]}"; do
    find "$d" -name '*.rs' -type f | sort | while IFS= read -r f; do
      awk -v F="$f" '
        skip == 1 { if ($0 ~ /^\}/) skip = 0; next }
        /^#\[cfg\(test\)\]/ { skip = 1; next }
        /^#\[cfg\(all\(/ && /(\(|,)[ \t]*test[ \t]*(,|\))/ && !/not\(test\)/ { skip = 1; next }
        { print F ":" NR ":" $0 }
      ' "$f"
    done
  done
}
SOURCE_STREAM="$(nontest_source)"
[[ -n "$SOURCE_STREAM" ]] || { note "✗ FAIL: the non-test source stream is EMPTY — the gate would be vacuously green"; exit 1; }

# --- CHECK S1: forbidden rusqlite cargo features -----------------------------
# Matches the feature name inside a rusqlite feature list. Deliberately a plain
# token match over every Cargo.toml: a feature can be enabled from a dependency
# table, a workspace table, or a per-crate override, and naming all three shapes
# is how a gate ends up matching text instead of structure (PR #43).
S1_BAD=(load_extension hooks vtab functions series preupdate_hook)
MANIFESTS="$(find . -name Cargo.toml -not -path './target/*' -not -path './node_modules/*' | sort)"
MANIFEST_COUNT="$(printf '%s\n' "$MANIFESTS" | wc -l | tr -d ' ')"
[[ "$MANIFEST_COUNT" -gt 0 ]] || { note "✗ FAIL: no Cargo.toml found — S1 would be vacuously green"; exit 1; }
MANIFEST_LINES="$(printf '%s\n' "$MANIFESTS" | tr '\n' '\0' | xargs -0 grep -n '' 2>/dev/null | grep -v ':[0-9]*:[[:space:]]*#' || true)"
s1_fail=0
for f in "${S1_BAD[@]}"; do
  # S449: the quote class is part of TOML's SYNTAX, not part of the feature
  # NAME. `features = ['load_extension']` is valid TOML (a literal string) and
  # cargo accepts it, so a gate that matches only `"load_extension"` is
  # defeated by one keystroke. Mutation-verified in
  # `cut_gate_sqlite_posture_probes.sh` probe P1. Match either quote.
  hits=$(printf '%s\n' "$MANIFEST_LINES" | grep -E "[\"']${f}[\"']" || true)
  if [[ -n "$hits" ]]; then
    note "✗ FAIL (S1): forbidden rusqlite feature \"$f\" appears in a manifest:"
    printf '      %s\n' "$hits"
    fail=1; s1_fail=1
  fi
done
[[ "$s1_fail" -eq 0 ]] && note "✓ S1: no forbidden rusqlite cargo feature in $MANIFEST_COUNT manifests"

# --- CHECK S2: forbidden rusqlite API calls ----------------------------------
S2_BAD=(
  create_scalar_function
  create_aggregate_function
  create_window_function
  create_collation
  commit_hook
  rollback_hook
  update_hook
  preupdate_hook
  load_extension
  load_extension_enable
)
s2_fail=0
for sym in "${S2_BAD[@]}"; do
  hits=$(printf '%s\n' "$SOURCE_STREAM" | grep "\.${sym}(\|::${sym}(" | grep -v ':[0-9]*:[[:space:]]*//' || true)
  if [[ -n "$hits" ]]; then
    note "✗ FAIL (S2): forbidden rusqlite API \`$sym\` is called:"
    printf '      %s\n' "$hits"
    fail=1; s2_fail=1
  fi
done
[[ "$s2_fail" -eq 0 ]] && note "✓ S2: none of the ${#S2_BAD[@]} forbidden rusqlite APIs is called"

# --- CHECK S3: no ATTACH in SQL ----------------------------------------------
# Word-boundary match so `attachment` / `attached_pdf` do not trip it.
#
# S449: `-i`. SQL keywords are CASE-INSENSITIVE — `attach database 'x' as y`
# executes exactly like the upper-case form, and the case-sensitive pattern
# waved it straight through (probe P2). The case in which a keyword is typed is
# a style choice, so a gate keyed on it is keyed on style, not on shape.
# Case-folding widens the false-positive surface to identifiers like
# `attach database` in prose; the `\b` + `DATABASE|'` structure already carries
# that, and comments are stripped below.
attach_hits=$(printf '%s\n' "$SOURCE_STREAM" | grep -Ei '\bATTACH[[:space:]]+(DATABASE|'"'"')' | grep -v ':[0-9]*:[[:space:]]*//' || true)
if [[ -n "$attach_hits" ]]; then
  note "✗ FAIL (S3): an ATTACH appears in SQL (M3 sets SQLITE_LIMIT_ATTACHED = 0):"
  printf '      %s\n' "$attach_hits"
  fail=1
else
  note "✓ S3: zero ATTACH sites"
fi

# --- CHECK S4: every SQLite open goes through open_hardened ------------------
SEAM="crates/aberp-db/src/sqlite.rs"
[[ -f "$SEAM" ]] || { note "✗ FAIL (S4): the sanctioned seam $SEAM is missing — the gate cannot be evaluated"; fail=1; }

# Matching `rusqlite::Connection::open` textually would miss the ONE form
# anyone actually writes — `use rusqlite::Connection;` then `Connection::open`
# — while `duckdb::Connection::open` is everywhere and must not trip. So the
# check is STRUCTURAL, in two stages: a file is in scope iff it names
# `rusqlite`; within those files, any `Connection::open` is a hit. Shape, not
# name (PR #43).
# In scope iff the file IMPORTS rusqlite or names `rusqlite::Connection`.
# Merely mentioning `rusqlite` is not enough: `crates/aberp-db/src/lib.rs`
# names `rusqlite::Error` in a `#[from]` variant while every `Connection::open`
# in it is DuckDB's. That false positive is why the predicate is the import and
# not the word.
#
# S449 — THE HOLE THIS CLOSES, and it is the one that matters most for Steps
# 5-9. The predicate above only sees files that name `rusqlite`. But the whole
# point of ADR-0108 §2.3's type seam is that NO file outside
# `crates/aberp-db/src/engine.rs` ever names either engine crate again: from
# Step 5 the tree writes `use aberp_db::engine::Connection`, and under
# `--features sqlite-engine` that alias IS `rusqlite::Connection`. So a bare
# `Connection::open(path)` in a migrated family is a bare rusqlite open that
# this gate could not see — S4 would go structurally blind at exactly the step
# it exists to police. Mutation-verified: probe P3 plants
# `use crate::engine::Connection as EngineConn; EngineConn::open(p)` and the
# pre-S449 gate reported "✓ S4" on it.
#
# The seam alias is therefore in scope too. That is not over-matching: a
# migrated family must reach the DB through `aberp_db::Handle` (CLAUDE.md rule
# 13), so a bare `engine::Connection::open` outside the seam is the defect
# under BOTH engines, not only under SQLite.
RUSQLITE_FILES=$(printf '%s\n' "$SOURCE_STREAM" \
  | grep -E ':[0-9]+:[[:space:]]*(pub )?use rusqlite|:[0-9]+:.*rusqlite::Connection|:[0-9]+:[[:space:]]*(pub )?use (aberp_db|crate)::engine|:[0-9]+:.*(aberp_db|crate)::engine::Connection' \
  | cut -d: -f1 | sort -u || true)
open_hits=""
if [[ -n "$RUSQLITE_FILES" ]]; then
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    [[ "$f" == "$SEAM" ]] && continue
    # Any `X::open…(` in a rusqlite-bearing file, not just `Connection::open`.
    # A probe defeated the narrower pattern with three characters —
    # `use rusqlite::Connection as PC; PC::open(...)` — and an aliased import
    # is not an exotic construct, it is the first thing anyone reaches for when
    # two crates export the same type name (which is exactly this migration's
    # situation). Matching the CALL SHAPE rather than the type NAME is the only
    # form an alias cannot slip past. The std filesystem idioms are allowlisted
    # by name because they are unambiguous and common.
    h=$(printf '%s\n' "$SOURCE_STREAM" | grep "^${f}:" \
      | grep -E '::(open|open_with_flags|open_in_memory)[[:space:]]*\(' \
      | grep -vE '\b(File|OpenOptions|fs)::open' \
      | grep -v ':[0-9]*:[[:space:]]*//' || true)
    [[ -n "$h" ]] && open_hits="${open_hits}${h}"$'\n'
  done <<< "$RUSQLITE_FILES"
fi
if [[ -n "${open_hits// /}" ]]; then
  note "✗ FAIL (S4): a SQLite connection is opened outside \`aberp_db::sqlite::open_hardened\`:"
  printf '      %s\n' "$open_hits"
  note "      A bare open skips M2/M3/M4/M7/M9/M10. Route it through open_hardened."
  fail=1
else
  note "✓ S4: every SQLite open goes through the hardened seam ($(printf '%s\n' "$RUSQLITE_FILES" | grep -c . || true) rusqlite-bearing file(s) scanned)"
fi

# --- LIVENESS: the gate must be able to see something -------------------------
# The K0/N0/M0 lesson: a scanner that finds no input reports green. If the seam
# exists, it MUST be in the rusqlite-file set AND its own opener must match the
# S4 pattern — otherwise S4 is structurally blind and its "✓" means nothing.
if [[ -f "$SEAM" ]]; then
  # NB: results are captured into variables rather than tested through
  # `| grep -q`. Under `set -o pipefail`, `grep -q`'s early exit SIGPIPEs the
  # upstream stage and the whole pipeline reports failure — which reads as
  # "no match" and turns a liveness check into a permanent false alarm. (It did,
  # here, before this comment existed.)
  seam_in_set=$(printf '%s\n' "$RUSQLITE_FILES" | grep -x "$SEAM" || true)
  seam_opener=$(printf '%s\n' "$SOURCE_STREAM" | grep "^${SEAM}:" \
    | grep -E '::(open|open_with_flags|open_in_memory)[[:space:]]*\(' \
    | grep -vE '\b(File|OpenOptions|fs)::open' || true)
  if [[ -z "$seam_in_set" ]]; then
    note "✗ FAIL (liveness): the seam is not in the rusqlite-file set — S4 is blind"
    fail=1
  elif [[ -z "$seam_opener" ]]; then
    note "✗ FAIL (liveness): the S4 pattern matches nothing even in the seam itself — S4 is blind"
    fail=1
  else
    note "✓ liveness: the S4 pattern demonstrably matches the seam's own opener"
  fi
fi

echo
if [[ "$fail" -eq 0 ]]; then echo "CUT-GATE: ✓ PASSED"; exit 0; fi
echo "CUT-GATE: ✗ FAILED"
exit 1
