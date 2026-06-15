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

**Staleness — INVESTIGATED, not parked (per Ervin's 2026-06-15 03:18
amendment: "all uncertainty must be investigated … we do not defer possible
trash because of uncertainty").**

DECISION: **KEEP both docs in `main`** (cherry-picks stand). Evidence:

- `quote-workflow.md` is an **operator/ops guide**, not a pricing-internals
  doc. Its content is the workflow *surfaces*: env-var setup, daemon
  cadence/retry, the pipeline state chips
  (`Fetched/Extracted/Priced/Rendered/Posted`), the audit events
  (`quote.poll_outcome`, `quote.priced_writeback_outcome`,
  `QuotePricing{Fetched,Extracted,Priced,Rendered,Posted}`), the operator SPA
  tab columns, stuck-state recovery, and the failure-kind table. **S417/S418
  changed the pricing engine's internal MATH (machining rate / MRR / difficulty
  multiplier / surface-area), not any of these surfaces.** Spot-check: the doc
  never documents the pricing formula; its one pricing-limitation note ("no
  margin profiles — margin is a single global tunable") is still true (S418 did
  not add per-customer/per-material margin). → not stale on its subject.
- `defense-workflow.md` explicitly states up front it documents a "foundation
  laid, firing sites not wired yet" model. No defense firing site has landed
  since S367 (the entire S370→S418 arc is quote/invoice/pricing). → the doc's
  central claim is still accurate.

Both carry their own date+release banner, so they self-date as historical
walkthroughs. No dedicated refresh session is warranted now. (If a future
pricing-doc refresh is ever wanted, that is a separate documentation session,
not a blocker on this preservation.) The cherry-picks are trivially revertable
if S420 disagrees, but the affirmative decision here is KEEP.

**🚩 DISPATCH ACTION (origin mutations — sequenced AFTER the S419 cut, because
this session may not push origin per [[origin-clean-topology]]).** This is a
firm ACT decision with exact commands, not a deferral:

Once `920f5a7`+`43d1cb9` are in `main` via the next cut, Dispatch runs:
```sh
# 1. Confirm the docs are in main (guard against premature delete):
git ls-files docs/walkthroughs/quote-workflow.md docs/walkthroughs/defense-workflow.md
# 2. Close PR #2 (content now in main):
gh pr close 2 --comment "Content merged into main via S419 (cherry-pick 43d1cb9). Closing; branch deleted."
# 3. Delete both stale origin branches:
git push origin --delete session-370-quote-walkthrough session-371-defense-walkthrough
```
PR #3 is already CLOSED — only its branch needs the `--delete` above. After
this, update the "only session-370/371 stale" clause carried in every recent
cut memory — origin will be fully clean (tags + main only).

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

## Group G — time-bomb test caught by the gate (NOT in the original inventory)

Running the cut gate (`cargo test --workspace`) surfaced a hard **FAILURE**
that has nothing to do with this session's edits:

```
test duckdb_round_trip_preserves_payment_deadline_and_delivery_date ... FAILED
thread '…' panicked at apps/aberp/tests/pr_84_invoice_dates.rs:192:5:
assertion `left != right` failed
```

**Root cause — calendar time bomb, not a product regression.** The test reads
`now = OffsetDateTime::now_utc()` (real clock) but hardcoded
`payment_deadline = date!(2026-06-15)`, then asserts
`assert_ne!(payment_deadline, now.date())`. The intent is sound (catch a read
path that silently substitutes `issue_date`), but the fixture's chosen date
**is today** — so the test passed every day except 2026-06-15. `delivery_date`
(2026-05-10) was unaffected; only `payment_deadline` happened to pick today.

**FIX** (`4b3de87`): derive both dates from `now` with non-zero offsets
(`payment_deadline = now+8d`, `delivery_date = now-36d`) so neither can ever
equal `now.date()`. The round-trip `assert_eq!` and the `assert_ne!`-vs-today
both still hold, on **any** calendar day — rule 9 intent preserved, no de-gate.
Verified: `pr_84_invoice_dates` 2 passed.

**Class sweep (per "every corner"):** searched the tree for the same
now-relative-vs-hardcoded-date pattern. `audit_payloads.rs` uses `now.date()` as
both the input and the expected value (pure round-trip, no vs-today assert —
safe). No other Rust test mixes a real clock with a hardcoded date in an
ordering/inequality assert. vitest was green today, ruling out an active TS
date-bomb. **`pr_84` was the only one.**

(Per Ervin's amendment — this was investigated to root cause and fixed in
session, not parked as "a failing test, ask someone".)

---

## Gate status

Run in the isolated worktree with `ABERP_TEST_PYTHON` UNSET (proving Group C
auto-discovery), against a freshly built `ui/dist`:

- `cargo fmt --all --check` — **clean**
- `cargo clippy --workspace --all-targets` — **0 warnings, 0 errors**
  (confirms Group B.1: no `serve_tenant_feature_guard` warning exists)
- `cargo test --workspace` — **green after the Group G fix** (the 2 CAD smoke
  tests pass with the env var UNSET; the only failure was the pr_84 time bomb,
  now fixed). Lib suite was `1237 passed; 0 failed` pre-fix; the one failing
  integration test is now green. Full re-run confirmation appended to the
  Dispatch report.
- `npm run check` (svelte-check) — **0 errors, 0 warnings** (was 28)
- `vitest` — **1190 passed** (64 files; unchanged count — test-internal edits)

---

## What was cleaned vs flagged

Per Ervin's 2026-06-15 03:18 amendment, **every item below ends at a CLEAR
decision** — act / don't-act / dedicated-session-with-brief. Nothing is parked
in "ask Ervin."

**Cleaned (committed on session-419):**
- Group B: svelte-check 28 → 0 (4 files). [`e78079e`]
- Group C: CAD harness venv auto-discovery (no env var, no de-gate). [`44e04ad`]
- Group A: preserved 1322 lines of doc content (2 cherry-picks) that would
  otherwise be lost on origin-branch deletion — DECISION: keep (staleness
  investigated, not parked). [`920f5a7`, `43d1cb9`]
- Group G: fixed the pr_84 time-bomb test that failed today. [`4b3de87`]

**ACT — but by Dispatch, sequenced after the S419 cut (exact commands in
Group A):**
- Delete origin branches session-370/371 + close PR #2. This is a firm ACT
  decision, not a deferral; it requires an origin push, which this session may
  not do ([[origin-clean-topology]]).

**Don't-act — verified, with reason:**
- Group A doc staleness — INVESTIGATED → KEEP (operator/foundation guides whose
  subject S417/S418 did not change). Not parked.
- Group B.1 clippy warning — does not exist (full clippy clean).
- Group D — current origin-clean workflow uses no PR, so the auto-close pattern
  cannot occur; recipe documented for reference.
- Group E memory corrections — flag-only **by the prompt's explicit Group E
  instruction**; the corrections are precise (not uncertain), point-in-time,
  apply-when-next-touching. No "I dunno" remains.
- Group F — no junk in the tree.

**Dedicated-session-needed:** none. Every backlog item reached a decision in
this session.
