# Operator runbook — snapshot restore: deliberate ROLLBACK vs corruption RECOVERY

**Audience:** Ervin / prod operator. **Time to resolve:** ~2 min of extra care
during a restore. **Verified against:** `crates/aberp-snapshot/src/take.rs`
(`restore_into`), `crates/audit-ledger/src/mirror.rs`, and the boot heal at
`apps/aberp/src/serve.rs` (`ensure_consistent_with_db`) @ `da7388a`
(`prod-durability-adr0099`). This is code-traced truth, not theory.

---

## The one fact that explains everything

A snapshot restore rebuilds **only the DuckDB file**. It does **not** touch the
tenant's audit-ledger **mirror sibling** `<db>.audit.log` (nor the preserved
`.healed-*` / `.ahead-*` / `.corrupt-*` `.bak` copies next to it). `restore_into`
swaps in the rebuilt DB and clears the DB's own `.wal`, and deliberately leaves
the mirror alone.

On the next `aberp serve` boot, the **gated auto-heal** (ADR-audit-armor) checks
the mirror against the DB. If the mirror is **ahead** of the DB and the heal can
prove a benign WAL-fold loss, it **replays the mirror's newer tail FORWARD into
the DB** and continues to `READY`.

That forward-heal is **load-bearing and correct for corruption recovery** — the
June recovery procedure is literally *restore the last good snapshot, then let
the mirror replay the newer audit tail back in*. Resetting the mirror
automatically inside `restore_into` would **break** that recovery path, so the
code does not do it. **Recovery-intent and rollback-intent are opposite
operations that look identical on disk.**

- **Corruption RECOVERY** — DB is damaged, mirror is the source of truth. You
  *want* the boot to heal the DB forward from the mirror. **Do nothing extra.**
- **Deliberate ROLLBACK** — DB is fine, you are intentionally reverting the
  tenant to an *older* state and intend to **stay there**. Here the newer mirror
  is exactly what you are trying to discard, so the boot auto-heal will
  **silently undo your rollback** by replaying the newer tail back in.

---

## If you are doing a DELIBERATE ROLLBACK (and intend to STAY there)

After you restore the older snapshot to a side path and **before** you swap the
DB into the live tenant home (serve stopped), clear the mirror + its preserved
siblings for that tenant, so there is no newer audit tail left to heal forward:

```sh
# serve MUST be stopped first (see below). <db> = the tenant DB you are
# swapping in, e.g. ~/.aberp/prod/aberp.duckdb
rm -f "<db>.audit.log" \
      "<db>.audit.log".healed-*.bak \
      "<db>.audit.log".ahead-*.bak \
      "<db>.audit.log".corrupt-*.bak
```

Then swap the restored DB into place and start serve. The next boot sees no
mirror (or a mirror that is not ahead), so it rebuilds the mirror from the
rolled-back DB instead of healing the DB forward — the rollback sticks.

> **Do NOT run this for a corruption recovery.** Deleting the mirror there throws
> away the exact tail the recovery is supposed to replay.

### Sequencing (both cases)

> **Precondition: `aberp serve` must be STOPPED before you run `snapshot
> restore`.** This is not advisory — the restore appends `snapshot.restored` to
> the LIVE tenant ledger, so it takes the ADR-0099 F-E whole-DB writer lock and
> **refuses** while serve holds it (`… another ABERP writer is already running on
> tenant 'prod' …`). There is no override flag, and none is needed: you have to
> stop serve to swap the file anyway. The lock is an `fs2` flock released by the
> kernel when the holding process's fd closes — including on a crash or SIGKILL —
> so a dead or crash-looping serve never leaves a stale lock behind. If the
> refusal persists, a serve process really is still alive; find and stop it.

1. **Stop `aberp serve`** in the `run_prod.sh` terminal (Ctrl-C). Never swap a
   live file under a running serve — and the restore in step 2 will refuse until
   you have.
2. `aberp snapshot restore <selector> --to <side-path> --confirm --tenant prod`
   (the guard refuses a `--to` under any live `~/.aberp/` home).
3. Verify the restored side-path DB.
4. **Rollback only:** clear the mirror siblings (command above).
5. Swap the restored DB into the tenant home.
6. Start serve; confirm the boot log shows the reconcile action you expect
   (`action=Healed{…}` for recovery; `action=created`/`Unchanged` for a clean
   rollback with the mirror cleared).

---

## Post-cut follow-up (NOT done in the cut prep — do NOT rush it)

The proper code fix is an **explicit intent flag** on the restore/swap path —
e.g. `--rollback` (clear the mirror + siblings automatically) vs `--recover`
(keep the mirror, heal forward) — so the operator declares intent and the binary
does the right thing instead of relying on this runbook. That is a change to the
**recovery path** and must not be rushed in ahead of a prod ship; it is a
deliberate post-cut follow-up. Until then, this runbook is the control.

---

## Why this is documentation, not a code change (for the cut)

The integration review's merge bar for this edge case was exactly "one runbook
line," because it is a **deliberate-operator-action** edge case, not a runtime
risk — a normal boot, a normal recovery, and a normal restore-to-side-path all
behave correctly with no change. Auto-resetting the mirror in `restore_into`
would be the wrong fix: it would break the corruption-recovery path that
intentionally heals forward. So the caveat lives here, in the `restore` CLI
`--help` text, and in the post-restore CLI message.
