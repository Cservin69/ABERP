#!/usr/bin/env bash
#
# gitignore_migration_artefacts_test.sh — ADR-0108 Step 1, test T-17.
#
# The repository is PUBLIC. Every artefact listed below holds partner bank
# accounts, tax numbers and every invoice ABERP has ever issued.
#
# ADR-0108 §2.5 measured the coverage rather than assuming it, and found a real
# gap: `aberp.sqlite`, `aberp.sqlite-wal`, `aberp.sqlite-shm` and BOTH snapshot
# directories (`.aberp-premigration-*/`, `.aberp-rolledback-*/`) were untracked
# AND unignored. The snapshot directories are the more dangerous half — each one
# holds a byte copy of the entire DuckDB database and its audit mirror.
#
# Existing coverage comes from FOUR independent globs, not one `*.duckdb*`:
# `*.duckdb`, `*.duckdb.wal`, `*.duckdb-wal`, `*.audit.log` and `*.bak`. This
# test asserts the whole set including the pre-existing entries, because a
# future `.gitignore` tidy-up that collapses those globs must not silently drop
# one of them.
#
# ASSERTED, NOT ASSUMED: `git check-ignore` is the same matcher `git add` uses,
# so this is the property itself and not a re-implementation of it.
#
# Mutation-verify: delete the `*.sqlite*` line from `.gitignore` and this test
# goes red on three paths.
#
# Exit 0 = every artefact is ignored. Non-zero = at least one would be
# committable.

set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

fail=0
note() { printf '  %s\n' "$*"; }

echo "ADR-0108 T-17 — .gitignore coverage for migration artefacts — root: $ROOT"

# Paths are relative to the repo root and are checked as PATHS, not as files:
# `git check-ignore` does not require them to exist, which is the point — the
# gate must be green before the migrator has ever produced one.
ARTEFACTS=(
  # --- the ADR-0108 gap this test exists for (B3) ---
  "apps/aberp-ui/aberp.sqlite"
  "apps/aberp-ui/aberp.sqlite-wal"
  "apps/aberp-ui/aberp.sqlite-shm"
  "apps/aberp-ui/aberp.sqlite.audit.log"
  "apps/aberp-ui/.aberp-premigration-20260731T101500Z/aberp.duckdb"
  "apps/aberp-ui/.aberp-premigration-20260731T101500Z/manifest.json"
  "apps/aberp-ui/.aberp-rolledback-20260731T101500Z/aberp.sqlite"
  "apps/aberp-ui/.aberp-rolledback-20260731T101500Z/pre-restore/aberp.duckdb"
  # --- pre-existing coverage, re-asserted so a tidy-up cannot drop it ---
  "apps/aberp-ui/aberp.duckdb"
  "apps/aberp-ui/aberp.duckdb.wal"
  "apps/aberp-ui/aberp.duckdb.audit.log"
  "apps/aberp-ui/aberp.duckdb.audit.log.healed-1.bak"
  "apps/aberp-ui/aberp.duckdb.audit.log.ahead-20260719.bak"
  "apps/aberp-ui/aberp.duckdb.audit.log.devstale-20260728.bak"
)

for a in "${ARTEFACTS[@]}"; do
  if git check-ignore -q -- "$a"; then
    note "✓ ignored: $a"
  else
    note "✗ FAIL: NOT ignored, would be committable to a PUBLIC repo: $a"
    fail=1
  fi
done

# The staging directory the snapshot writes before its atomic promote. It is
# `<dir>.partial`, which the `.aberp-premigration-*/` glob does not match
# (the glob has a trailing slash and `.partial` is a sibling name), so it is
# checked explicitly rather than assumed to ride along.
STAGING="apps/aberp-ui/.aberp-premigration-20260731T101500Z.partial"
if git check-ignore -q -- "$STAGING"; then
  note "✓ ignored: $STAGING"
else
  note "✗ FAIL: the snapshot staging directory is NOT ignored: $STAGING"
  fail=1
fi

echo
if [[ "$fail" -eq 0 ]]; then
  echo "T-17: ✓ PASSED"
  exit 0
fi
echo "T-17: ✗ FAILED — a migration artefact holding partner data is committable."
exit 1
