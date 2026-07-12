1. think before coding: state assumptions, don't guess. the model can't read your mind, stop hoping it will

2. simplicity first: minimum code, no speculative abstractions. the moment you let Claude add "for future flexibility," you've added 200 lines you'll delete next quarter

3. surgical changes: touch only what you must. don't let it improve adjacent code, that's how PRs blow up. out-of-scope issue found mid-session? record it in the deferral ledger with the step that closes it and keep moving on the primary goal — don't rabbit-hole. nothing is "done" until the ledger is clear or the residual is explicitly signed off

4. gated increments toward the objective: define success criteria upfront (without them Claude loops forever or stops too early), then execute in steps, each closed by the gates — cargo fmt + build + test + clippy -D warnings + the coherence/regression pins — so every step lands on a gate-green base (steps 5 and 6 atop a broken step 4 went unnoticed for an hour). the gates are the per-step trust surface; reserve full adversarial/deep review for genuinely consequential checkpoints (the invoice→NAV/ÁFA path) and the final pre-cut review. adversarial-after-every-increment is the analysis-paralysis failure mode

5. use the model only for judgment calls: classification, drafting, summarization, extraction. NOT routing, retries, status-code handling, deterministic transforms. if code can answer, code answers

6. token budgets are not advisory: per-task 4000, per-session 30000. by message 40 of a long debug, Claude is re-suggesting fixes you rejected at message 5

7. surface conflicts, don't average them: two patterns in the codebase? pick one. Claude blending them is how errors get swallowed twice

8. read before you write: read exports, callers, shared utilities. Claude will happily add a duplicate function next to an identical one it never read

9. tests verify intent, not just behavior: a test that can't fail when business logic changes is wrong. all 12 of Claude's tests can pass while the function returns a constant

10. match the codebase conventions: class components? don't fork to hooks silently. testing patterns assumed componentDidMount, hooks broke them without surfacing

11. fail loud: "completed successfully" with 14% of records silently skipped is the worst class of bug. surface uncertainty, don't hide it

12. delete before optimize ("delete the part"): question every struct, wrapper, parameter, helper — should this exist at all? optimizing a thing that shouldn't exist is the worst waste of effort. "for future flexibility" wrappers around one consumed field get deleted inline, not simplified. if you're not adding back at least 10% of what you delete, you weren't aggressive enough

13. one Handle, all access: every DB touch of a migrated subsystem goes through the shared `aberp_db::Handle` (`state.db.write()`/`.read()` — try_clones of the ONE instance). a co-resident fresh `Connection::open` checkpoint-tears the Handle's WAL on close, and a fresh reader of Handle-WAL-resident data reads stale. never nest `write()` while holding a guard — self-deadlock

14. all-or-nothing per subsystem: migrate a family's writers AND readers together. half-migrated (writes on the Handle, reads on fresh `Connection::open`, or vice versa) is worse than unmigrated — it tears and fails open. stop at fused-family boundaries, never mid-family

15. audit atomically: business INSERTs + the audit `append_in_tx` in ONE transaction on the shared WriteGuard (see `create_ncr`). business-commit-then-audit-append leaves a written-but-unaudited torn row on any audit error or crash
