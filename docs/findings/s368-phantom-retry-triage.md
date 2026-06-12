# S368 — Phantom-retry triage (Scope D)

**Status:** triage only — NO code fix in S368. Recommended fix scoped to S370.
**Date:** 2026-06-12
**Trigger:** S356 reported that `next_actionable_blocking` "re-picked a Failed
job and auto-retried at +60s", contradicting the "Failed = operator-only" claim
in S290/S346.

## Verdict

**The "re-picked a Failed job" framing is INACCURATE.** Genuinely-`Failed` rows
are *not* re-picked — "Failed = operator-only" holds. But there **is** a real
silent re-pick loop, for a *different* reason: a job that hits an
**infrastructure error** (anything that returns `Err` from `advance_one_step`
rather than `StepOutcome::Failed`) is left in its non-terminal state and
silently re-attempted every cadence, with no `Failed` row, no audit row, and no
`attempt_n` escalation.

So: the +60s re-pick of a genuinely-transient infra blip is **intentional and
desirable**; the *unbounded, invisible* re-pick of a **persistent** infra error
is a **defect** (CLAUDE.md #12 "fail loud").

## Code path

### 1. The scheduler never returns `Failed` rows — claim confirmed

`apps/aberp/src/quote_pricing_jobs.rs:894` `next_actionable_job`:

```sql
WHERE tenant_id = ?
  AND state IN ('fetched','extracting','pricing','rendering','posting_back')
ORDER BY fetched_at ASC
LIMIT 1
```

`'failed'` is **not** in the `IN` list. A row that reached `set_failed`
(`quote_pricing_jobs.rs:631`+, state → `Failed`) is frozen until the operator's
`retry_job` (`:679`) resets it to `Fetched` and bumps `attempt_n`. Tests
`next_actionable_skips_terminal_states` (`:1299`) and the S347 "must NOT pick up
a Permanent Failed row" assertion (`quote_pricing_pipeline.rs:3655`) both pin
this. **"Failed = operator-only" is true.**

### 2. Domain failures DO reach `Failed`

Every stage funnels a *domain* failure through `emit_failure`
(`quote_pricing_pipeline.rs:1978`) → `set_failed` → `state = Failed`,
`StepOutcome::Failed`:

- extract: `extractor.extract(...) Err` → `emit_failure("extract", …)` (`:607-619`)
- price / render: same shape.
- post: typed `WritebackOutcome` non-success (incl. `RoutingMisconfigured`,
  the 404-masked-as-200 case) → `emit_failure("post", …)` (`:1024-1035`).

So even the RoutingMisconfigured writeback that motivated this fix session lands
the job at `Failed`, frozen, operator-only. No auto-retry there.

### 3. The actual loop — infrastructure `Err` leaves the row non-terminal

`run()` cycle (`quote_pricing_pipeline.rs:203-218`):

```rust
match self.advance_one_step(row).await {
    Ok(StepOutcome::Advanced) => summary.advanced += 1,
    Ok(StepOutcome::Posted)   => { … }
    Ok(StepOutcome::Failed)   => { … }      // ← set_failed already ran
    Err(e) => {
        tracing::warn!(error = %e, "pricing-pipeline advance error");
        summary.error = Some(format!("advance: {e:#}"));
        break;                               // ← row state UNCHANGED
    }
}
```

The `Err(e)` arm only logs + breaks the cycle. The row keeps whatever
non-terminal state it had (`fetched`/`extracting`/`pricing`/`rendering`/`posting_back`),
so the **next** cadence (~60s) re-selects it via `next_actionable_job`.

Every `?` in a stage that is *not* the domain-failure match arm produces this
`Err`. In `advance_extract` alone (`:519-624`): `Connection::open` (`:520`),
`audit_ensure_schema` (`:521`), `jobs::ensure_schema` (`:522`), `set_state`
(`:527`), `get_job_artifacts` (`:543`), `to_vec`/`commit` encode+tx (`:594,604`),
and the `spawn_blocking` join (`:624`). `advance_post` adds `bearer_header()`,
`build_priced_multipart`, and `resolved_writeback_url` build failures.

### 4. No `attempt_n` escalation, no max-attempts guard

`attempt_n` is bumped **only** by operator `retry_job` (`quote_pricing_jobs.rs:696`)
and `amend_material_grade` (`:790`). The internal-`Err` re-pick path never
touches it, and there is no max-attempts ceiling anywhere. A persistent infra
error (corrupt CAD that makes `get_job_artifacts`/extract throw a non-domain
error, a misconfigured `python_bin`, a permanently bad artifact path, a DB the
process can't open) therefore loops **forever**, `attempt_n` frozen at its
entry value, emitting only a `tracing::warn!` per cycle — invisible in the SPA
(the row still shows `pricing`/`posting_back`, "in progress") and absent from
the audit ledger.

## Intentional vs defect

| Behaviour | Verdict |
|---|---|
| Genuinely-`Failed` rows are never auto-retried | **Intentional & correct** (S290 design, `:881-893`) |
| Domain failures (incl. RoutingMisconfigured writeback) → `Failed`, frozen | **Intentional & correct** |
| Single-cycle re-pick after a *transient* infra blip (DB momentarily locked → next cycle succeeds) | **Intentional & desirable** (cheap resilience) |
| *Unbounded, silent* re-pick after a *persistent* infra `Err`: no `Failed` row, no audit, no `attempt_n` bump, no SPA visibility | **DEFECT** — violates CLAUDE.md #12 (fail loud) |

The S356 observation was real but mislabelled: what looked like "a Failed job
auto-retrying" was a job that **never reached `Failed`** because the failing
path was an infrastructure `Err`, not a domain `StepOutcome::Failed`.

## Recommended fix scope (S370)

1. **Bound the internal-`Err` re-pick.** Track consecutive internal-error
   attempts per job. The cleanest carrier is the existing `attempt_n` column —
   the `Err` arm in `run()` (or a wrapper around `advance_one_step`) bumps a
   per-job internal-attempt counter.
2. **Escalate to `Failed` after K consecutive internal errors** (K small, e.g.
   3) via `set_failed` with a synthetic `internal:<stage>` reason and a new
   `FailureKind::Transient`/`Unknown` classification, so the row surfaces in the
   SPA Pricing tab and the audit ledger exactly like every other Failed row, and
   freezes for operator action. Keep the first 1–2 retries silent so a true
   one-cycle blip still self-heals.
3. **Do NOT change** the `Failed`-is-operator-only scheduler semantics or the
   domain-failure → `Failed` path; both are correct.

Out of scope for the fix, but worth a glance in S370: whether `summary.error` +
`break` on the *first* internal error should instead `continue` to the next job
(today one stuck job blocks the rest of the cycle's `MAX_JOBS_PER_CYCLE` budget,
because the oldest-first `next_actionable_job` keeps returning the same wedged
row first).
