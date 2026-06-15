# House-cleaning sweep — investigation report (S419)

**Date:** 2026-06-15 · **Session:** S419 · **Base:** `790ee70` (PROD_v2.27.66)
· **Branch:** `session-419` (isolated worktree, no origin push) · **Audience:**
Dispatch + S420 adversarial review + Ervin.

Goal (Ervin verbatim): *"do the sanity backlogs … Session for clean up …
Careful investigation before deleting … I want super green across the board …
Today we only clean house but every corner, every pillow."*

Paranoid principle applied throughout: **every deletion/fix carries file:line
evidence + a before/after; anything that looks stale but can't be proven safe is
FLAGGED, not deleted.**

---

## Investigation matrix

| # | Item | Verification | Safe to act? | Action | Risk |
|---|------|--------------|--------------|--------|------|
| A | 2 stale origin doc branches (session-370/371) | `gh pr list`, `git ls-remote`, grep main | content NOT in main → delete = data loss | **Preserved** docs into session-419 (cherry-pick); origin delete/PR-close **FLAGGED for Dispatch** (can't push origin) | low (docs only) |
| B | clippy "unused import" in serve_tenant_feature_guard.rs | read file + full clippy | N/A — **no warning exists** | none (flagged as already-clean) | none |
| B | svelte-check 28 errors (4 files) | `npm run check` before/after | yes — real type gaps, not orphans | fixed → **0 errors** | low |
| C | 2 CAD py-smoke tests fail without env var | read harness + run tests | yes — discovery gap | harness auto-discovers venv; verified pass with env var UNSET | low |
| D | GitHub PR auto-CLOSED-after-rebase | reasoning + history | doc-only | recipe documented; current origin-clean workflow sidesteps it | none |
| E | Memory drift | read MEMORY.md + files | flag-only (no rewrite) | drift flagged + 1-line corrections proposed | none |
| F | Workspace junk | find swap/DS_Store/orig/tmp | nothing found | none (clean) | none |

---

## Group A — Origin doc-branch debt

**State (verified 2026-06-15):**
- `origin` heads = all `PROD_v*` tags + `main` + exactly two stale branches:
  `session-370-quote-walkthrough` (tip `2e690cc`) and
  `session-371-defense-walkthrough` (tip `23e6184`). Both branched off the old
  `fb79cd7` (PROD_v2.27.39 era).
- **PR #3** (s370, `docs/walkthroughs/quote-workflow.md` +483): **CLOSED**
  (not merged).
- **PR #2** (s371, `docs/walkthroughs/defense-workflow.md` +839): **OPEN**.
- Neither file exists in `main` (`git ls-files docs/walkthroughs/` →
  only `dr-playbook.md`, `end-to-end-auto-quote-test.md`). **The content is
  unique — deleting the branches would lose 1322 lines of documentation.**

**Decision tree (per the prompt's options):**
- Content is NOT duplicate → the "close PR + delete" arm does not apply.
- → "MERGE the doc content into main (preserve findings), then delete the
  branches from origin." Since this session may not push to origin
  ([[origin-clean-topology]]), "merge into main" = **cherry-pick both doc
  commits onto `session-419`** (`920f5a7`, `43d1cb9`, both with `-x`
  provenance) so the next Dispatch cut lands them in `main`.

**Staleness caveat (surfaced, not hidden — rule 12):** both docs are dated
2026-06-12 / PROD_v2.27.39 and carry their own date+release banners.
`quote-workflow.md` predates the S417/S418 auto-quote pricing-model rewrite, so
its pricing specifics are dated (the operator step-flow is still broadly
accurate). `defense-workflow.md` is explicitly a "foundation laid, firing sites
not wired yet" document and says so up front. They are preserved verbatim as
historical, self-dating records — **if Ervin/S420 judge them not worth carrying
in `main`, revert the two cherry-picks; the origin branches still hold the
originals until Dispatch deletes them.**

**🚩 FLAGGED FOR DISPATCH (origin mutations — out of this session's push scope):**
After these cherry-picks land in `main` via the next cut:
1. Close PR #2 (its content will then be in `main`).
2. Delete origin branches `session-370-quote-walkthrough` +
   `session-371-defense-walkthrough`.
3. Then update the "only session-370/371 stale" line carried in every recent
   cut memory — origin will be fully clean.

---

## Group B — Pre-existing warnings

### B.1 clippy "unused import" — `apps/aberp/tests/serve_tenant_feature_guard.rs`

**Verdict: no warning exists. Nothing to fix.** The file has one import,
`use std::process::Command;` (line 16), used at line 24
(`Command::new(env!("CARGO_BIN_EXE_aberp"))`). A full
`cargo clippy --workspace --all-targets` reports it clean (see gate log).
The S418 cut note ("pre-existing serve_tenant_feature_guard WARN not in diff")
appears to refer to a transient/older state already resolved. **No action; no
test added** (there is nothing to pin — adding a test to "prove" an absent
warning would be a test that can't fail, violating rule 9).

### B.2 svelte-check 28 errors → 0

`npm run check` baseline = **28 errors across 4 files** (the prompt said "3 test
files"; there are 4 — one is the mock source). After fix = **0 errors,
0 warnings, 0 files-with-problems**.

| File | Errs | Root cause | Fix |
|------|------|-----------|-----|
| `workshop-mock-data.ts` | 1 | dead `const HR` in `buildPendingQaRows` (line 306) — used `MIN_MS` directly | deleted (rule 13) |
| `workshop-mock-data.test.ts` | 24 | accesses `work_order_rows` etc. that are **intentionally optional on the wire** (PR-242/S250 finding 5: mid-upgrade backend omits them) | test-side: `const rows = b.field!` per block. A dropped field → runtime throw → fails loud (rule 9). **Type left optional** — making it required would break the mid-upgrade contract (rule 7/12) |
| `hygiene-clickthrough.test.ts` | 2 | `frow()` set `invoice_id`/`state` before `...overrides`, which overwrote them | deleted the two redundant lines (overrides' type already requires both) |
| `quote-pickup.test.ts` | 1 | `row()` omitted 10 required `QuoteIntakeRow` fields (S271+ projection); TS reports only the first | added all 10 with null/false defaults |

All four files are **real, actively-used** tests/mocks — none are orphans.

**Note:** `npm run check` (svelte-check) is NOT part of the cut gate
(gate = fmt + clippy + cargo test + vitest). These 28 errors were never
blocking, but driving them to 0 is part of "super green across the board."

### B.3 (no entry) — see Group C for the CAD env-tests.

---

## Group C — CAD py-smoke env-fail (no de-gating)

**Tests:** `cube_stl_extracts_into_feature_graph_via_real_python`
(`crates/aberp-cad-extract-wrapper/tests/extract_smoke.rs:23`),
`step_cube_extracts_into_feature_graph_via_real_python`
(`…/tests/step_extract_smoke.rs:37`).

**Root cause:** `tests/common/mod.rs::test_python_bin()` (was lines 21–26) only
read `ABERP_TEST_PYTHON`, else returned the literal `"python3"`. System
`python3` does NOT have `aberp_cad_extract` (verified:
`python3 -c "import aberp_cad_extract"` → ImportError), so a bare `cargo test`
panics. CI passes because it sets `ABERP_TEST_PYTHON=$(python -c
'import sys; print(sys.executable)')` after `pip install -e
'python/aberp-cad-extract[step]'` (`.github/workflows/ci.yml:92–117`).

**The daemon already solved this** — `resolve_pipeline_python`
(`apps/aberp/src/quote_pricing_pipeline.rs:2901`, helpers
`canonical_venv_python:2808` / `alt_venv_python:2844`) resolves
env-var → canonical venv → alt venv → system. The test harness just never
reused that order.

**Fix:** mirror that order in `test_python_bin()`:
1. `ABERP_TEST_PYTHON` (explicit; CI still wins).
2. `<repo>/python/aberp-cad-extract/.venv/bin/python` (canonical dev venv).
3. `<repo>/.venv/bin/python` (alt).
4. `python3` — last resort; **fails loud** with the downstream ImportError if
   the module isn't there (rule 12). **No `#[ignore]`** — de-gating is
   forbidden ([[all-gates-must-pass]]).

The wrapper crate cannot depend on `apps/aberp` (cycle), so the ~12-line path
logic is duplicated test-side rather than imported.

**Verification (rule 9 — the test genuinely exercises the new path):** with
`ABERP_TEST_PYTHON` UNSET and a venv at the canonical path,
`cargo test -p aberp-cad-extract-wrapper --test extract_smoke --test
step_extract_smoke` → **2 passed, 0 failed.** Since system `python3` lacks the
module, the only way they pass is via the new discovery — confirming the fix
works, not that the test is inert.

**Operator note (per [[trust-code-not-operator]]):** the venv is gitignored and
per-checkout. A dev who has run the documented `pip install -e
python/aberp-cad-extract` once gets passing tests automatically — no env var to
remember. A fresh checkout with no venv still fails loud (correct).

---

## Group D — GitHub "PR auto-CLOSED-not-MERGED after rebase" (doc-only)

**Mechanism:** a fast-forward push of `main` to a rebased SHA does not contain
the PR head's *original* commit SHAs, so GitHub cannot detect the merge and
auto-CLOSES the PR (observed S394, S405). It is harmless (the code is in `main`)
but noisy.

**Robust recipe (if PRs are ever used again):** before deleting the PR's head
branch, **push the rebased commits to the PR head branch first**, so the PR head
SHA equals the SHA now reachable from `main`; GitHub then re-evaluates and marks
it **MERGED**. Equivalently, use `gh pr merge <n> --rebase` to let GitHub
perform the rebase-merge itself.

**Current relevance: low.** The active workflow ([[origin-clean-topology]],
established S413) creates **no PR** for session branches — Dispatch fetches from
the dev worktree and ff-merges in the main checkout. With no PR there is nothing
to auto-close. Documented here for reference; **no code change made.**

---

## Group E — Memory audit (flag-only, no rewrite)

Drift found (proposed 1-line corrections; NOT applied per [[memory-audit-required]]):

1. **`aberp-quote-workflow-batch-2026-06-14` does not exist.** The prompt asked
   to "mark items SHIPPED" in this file — there is no such memory file. SHIPPED
   status is already tracked per-cut in the `PROD_v2.27.x SHIPPED` index lines.
   *Correction:* none needed; the referenced file is a dangling `[[link]]`.

2. **`project_aberp_2026_06_13_night_batch.md` (S383) — follow-ups all shipped.**
   Its three open findings — F1 (S384 chain-aware storno), F2 (S385 atomic PDF
   write), F3 (S388 submit reopen) — all SHIPPED in the S390/S391 bundles per
   MEMORY.md. The "queue as S384/S385/S388" language is now historical.
   *Correction:* append "— ALL RESOLVED in S390/S391" to that index line.
   (Point-in-time observation; low priority.)

3. **`project_aberp_db_concurrency_posture.md` lines 21–31 stale** — the
   night-batch file already noted this (its line 26); now further obsolete since
   S384/S385/S388 landed. *Correction:* mechanical refresh when next touching
   DB/locking; no PROD impact.

4. **Prompt premise correction:** the prompt stated the night-batch memory
   contains "claims about S414b auto-silence." It does **not** — that file is
   S383-era (S375/S377/S378/S379/S381) and predates S414b. The S414b WARN-once
   gate is recorded in the S413 cut memory line, which is accurate. No drift.

5. **Future drift (after Dispatch acts on Group A):** once session-370/371 are
   deleted and PR #2 closed, the "only session-370/371 stale" clause in every
   recent cut memory becomes outdated → origin fully clean.

`[[origin-clean-topology]]` / `[[local-git-clean-topology]]` are **inline
conventions** in the cut memories, not standalone files; both are **consistent**
with verified reality (origin holds exactly session-370/371 + tags + main).

---

## Group F — Workspace junk

**Verified clean.** No swap files (`*.swp/*.swo`), no `.DS_Store`, no merge
debris (`*.orig/*.rej`), no `/tmp` tarballs or scratch. `git status --ignored`
shows only the expected ignored dirs (`apps/aberp-ui/gen/`,
`apps/aberp-ui/ui/node_modules/`, `target/`). **No action.** `~/.aberp/` runtime
state not touched (out of scope).

---

## Gate status

Run in the isolated worktree with `ABERP_TEST_PYTHON` UNSET (proving Group C
auto-discovery), against a freshly built `ui/dist`:

- `cargo fmt --all --check` — see gate log
- `cargo clippy --workspace --all-targets` — see gate log
- `cargo test` (incl. the 2 CAD smoke tests, no env var) — see gate log
- `npm run check` (svelte-check) — **0 errors**
- `vitest` — see gate log

(Exact numbers appended to the S419 Dispatch report.)

---

## What was cleaned vs flagged

**Cleaned (committed on session-419):**
- Group B: svelte-check 28 → 0 (4 files).
- Group C: CAD harness venv auto-discovery (no env var, no de-gate).
- Group A: preserved 1322 lines of doc content (2 cherry-picks) that would
  otherwise be lost on origin-branch deletion.

**Flagged for Ervin / Dispatch (NOT acted on):**
- Group A origin mutations (delete branches + close PR #2) — needs origin push,
  out of session scope.
- Group A staleness judgement — whether `defense-workflow.md` /
  `quote-workflow.md` belong in `main` long-term (revertable cherry-picks).
- Group E memory corrections (flag-only by design).

**No action needed (verified clean):**
- Group B.1 clippy warning (does not exist).
- Group D (current workflow sidesteps it).
- Group F (no junk).
