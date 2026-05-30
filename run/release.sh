#!/usr/bin/env bash
#
# release.sh — S169 / PR-169 — per-release-branch publisher.
#
# WHAT IT DOES (in order):
#   1. Refuses to run from the dev checkout (path-substring sentinel —
#      anything under `/Documents/Claude/Projects/` is considered dev).
#   2. Validates we're on `main` with a CLEAN working tree.
#   3. Validates the version arg matches `PROD_vMAJOR.MINOR` (e.g. PROD_v1.0).
#      Note the case + separator: uppercase PROD, underscore, two dot
#      numbers. This is intentional — the new release model uses branch
#      names, not tags, and the uppercase shape makes them visually
#      distinct from feature branches in `git branch -a`.
#   4. Refuses if the release branch already exists on origin (suggests
#      the next minor as the conservative next pick).
#   5. Pushes `origin main:PROD_vX.Y` — the branch IS the release marker.
#      No annotated tag, no local build, no codesign. The operator clones
#      from this branch on the prod machine and builds there.
#   6. Prints the `git clone --branch <name>` command the operator runs
#      next on the prod machine.
#
# WHY THE MODEL CHANGED (S167 → S169):
#   S167's release.sh built locally + tagged locally + pushed a tag. That
#   model couples the dev machine to the build artifact, which the 2026-05-30
#   cutover proved fragile (an icons/ regression on the dev machine reached
#   prod silently). The new model decouples: dev publishes a release ref;
#   prod machine clones + builds. The build happens on the operator's prod
#   machine, with that machine's tooling, against a known git ref. Smaller
#   blast radius if dev tooling drifts.
#
# WHAT IT DELIBERATELY DOES NOT DO:
#   - Does NOT cargo fmt / clippy / build. Main HEAD is assumed ready —
#     those gates live in the dev workflow, not at publish time.
#   - Does NOT codesign or notarise (handled later on the prod machine).
#   - Does NOT push from a dev clone with uncommitted changes — the
#     `git status --porcelain` gate refuses dirty trees.
#   - Does NOT create a tag. The branch IS the release. If you need a
#     fixed pointer to a specific commit, the branch's HEAD is it.
#
# USAGE:
#   ./run/release.sh PROD_v1.0
#   ./run/release.sh PROD_v1.1
#   ./run/release.sh --help
#
# FLAGS:
#   --help, -h         print this header and exit
#
# EXIT CODES:
#   0  release branch pushed to origin
#   2  arg / preflight failure (wrong branch, dirty tree, bad version,
#      branch exists, dev-sentinel match)
#   5  git push failed

set -euo pipefail

# ---------- self-syntax-check (mirrors run_desktop.sh PR-55) ----------------
if ! bash -n "$0" 2>/dev/null; then
  echo "[fail] $0 failed 'bash -n' syntax check — refusing to run" >&2
  bash -n "$0"
  exit 2
fi

readonly MAIN_BRANCH="main"
readonly VERSION_RE='^PROD_v[0-9]+\.[0-9]+$'
# Dev-sentinel: any checkout under this path subtree is the dev workspace.
# release.sh must be invoked from the OPERATOR's prod clone, not the dev
# clone. See header note (S169 model decouples publish from build).
readonly DEV_SENTINEL_PATH_SUBSTR="/Documents/Claude/Projects/"

# Resolve script location (not $PWD — the operator might `cd` elsewhere
# before invoking). pwd -P dereferences symlinks; we want the real
# physical path for the sentinel check.
script_path="$(cd "$(dirname "$0")" && pwd -P)"
readonly SCRIPT_PATH="$script_path"

# ---------- colour helpers (no-op when stdout is not a terminal) ------------
if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
  c_red=$'\033[1;31m'; c_yel=$'\033[1;33m'; c_grn=$'\033[1;32m'
  c_dim=$'\033[2m';    c_rst=$'\033[0m'
else
  c_red=""; c_yel=""; c_grn=""; c_dim=""; c_rst=""
fi

die()  { echo "${c_red}[fail]${c_rst} $*" >&2; exit "${2:-2}"; }
warn() { echo "${c_yel}[warn]${c_rst} $*" >&2; }
info() { echo "${c_dim}[info]${c_rst} $*"; }
ok()   { echo "${c_grn}[ ok ]${c_rst} $*"; }

print_help() {
  sed -n '2,60p' "$0" | sed 's/^# \{0,1\}//'
}

# ---------- arg parsing -----------------------------------------------------
version=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      print_help
      exit 0
      ;;
    -*)
      die "unknown flag: $1"
      ;;
    *)
      if [[ -z "$version" ]]; then
        version="$1"
        shift
      else
        die "unexpected positional arg: $1 (version already set to $version)"
      fi
      ;;
  esac
done

if [[ -z "$version" ]]; then
  echo "usage: $(basename "$0") <PROD_vMAJOR.MINOR>" >&2
  echo "       $(basename "$0") --help" >&2
  exit 2
fi

if [[ ! "$version" =~ $VERSION_RE ]]; then
  die "version '$version' does not match $VERSION_RE — expected e.g. PROD_v1.0
HU: A '$version' nem felel meg a $VERSION_RE mintának — pl. PROD_v1.0"
fi

# ---------- preflight: dev-sentinel ----------------------------------------
# Refuse to publish from the dev checkout. The new release model expects
# release.sh to be invoked from the OPERATOR's prod clone (which is a
# fresh clone of the previous release branch, sitting somewhere outside
# the dev workspace tree).
if [[ "$SCRIPT_PATH" == *"$DEV_SENTINEL_PATH_SUBSTR"* ]]; then
  die "release.sh is running from the DEV workspace
   path: $SCRIPT_PATH

   The S169 release model publishes from the operator's prod clone, not
   the dev clone. Steps:

   1. Make sure dev work has landed on main and pushed: git push origin main
   2. Clone (or pull) the prod working dir somewhere outside
      $DEV_SENTINEL_PATH_SUBSTR
   3. From THAT clone: ./run/release.sh $version

   HU: A release.sh-t a fejlesztői munkamappából futtatod. A kiadást
   az operátor prod-mappájából kell indítani, nem innen."
fi

# ---------- preflight: must be on main + clean tree ------------------------
cd "$SCRIPT_PATH/.." || die "could not cd to repo root"

current_branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "$current_branch" != "$MAIN_BRANCH" ]]; then
  die "must be on '$MAIN_BRANCH' to publish a release (currently on '$current_branch')
HU: A kiadáshoz '$MAIN_BRANCH' ágon kell lenned (jelenleg '$current_branch')."
fi

if [[ -n "$(git status --porcelain)" ]]; then
  die "working tree is dirty — commit / stash before publishing:
$(git status --short)
HU: A munkafa piszkos — commitold vagy stash-old a változtatásokat publikálás előtt."
fi

ok "on $MAIN_BRANCH with clean working tree"

# ---------- preflight: branch must not already exist on origin -------------
info "git fetch origin (refresh remote refs) ..."
if ! git fetch --quiet origin; then
  warn "git fetch failed — proceeding with possibly-stale remote refs"
fi

existing_ref="$(git ls-remote --heads origin "$version" 2>/dev/null | awk '{print $1}')"
if [[ -n "$existing_ref" ]]; then
  major="$(echo "$version" | sed -E 's/^PROD_v([0-9]+)\.([0-9]+)$/\1/')"
  minor="$(echo "$version" | sed -E 's/^PROD_v([0-9]+)\.([0-9]+)$/\2/')"
  next_minor="PROD_v${major}.$((minor + 1))"
  die "release branch '$version' already exists on origin at ${existing_ref:0:12}.
   Pick a new version — next minor would be: $next_minor
HU: A '$version' release-ág már létezik a távolin. Válassz új verziót, pl.: $next_minor"
fi
ok "release branch '$version' is free on origin"

# ---------- the push --------------------------------------------------------
main_sha="$(git rev-parse "$MAIN_BRANCH")"

echo
echo "${c_yel}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${c_rst}"
echo "${c_yel}  Publishing release branch ${version}${c_rst}"
echo "${c_yel}  Kiadási ág publikálása: ${version}${c_rst}"
echo "${c_yel}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${c_rst}"
echo "  main HEAD: ${main_sha}"
echo "  ref:       refs/heads/${version}"
echo

info "git push origin ${MAIN_BRANCH}:refs/heads/${version}"
if ! git push origin "${MAIN_BRANCH}:refs/heads/${version}"; then
  die "git push failed — network down, or no write permission to refs/heads/${version}" 5
fi

ok "pushed origin/${version} → ${main_sha:0:12}"

# ---------- summary + operator next-step -----------------------------------
origin_url="$(git remote get-url origin 2>/dev/null || echo '<origin-url>')"

echo
echo "${c_grn}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${c_rst}"
echo "${c_grn}  RELEASE ${version} PUBLISHED${c_rst}"
echo "${c_grn}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${c_rst}"
echo
echo "  Branch:   ${version}"
echo "  Commit:   ${main_sha}"
echo "  Origin:   ${origin_url}"
echo
echo "${c_yel}Next on the prod machine:${c_rst}"
echo "  ${c_dim}git clone --branch ${version} ${origin_url} <target-dir>${c_rst}"
echo "  ${c_dim}cd <target-dir>${c_rst}"
echo "  ${c_dim}./run/run_prod.sh${c_rst}"
echo
echo "${c_yel}Következő lépés az éles gépen:${c_rst}"
echo "  Klónozd a $version ágról és futtasd a run_prod.sh-t."
echo

exit 0
