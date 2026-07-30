# DEV crash-restart: the stale-orphan writer reap (S445)

**Scope: the DEV launcher only.** `run/run_prod.sh` and `aberp serve`'s own
refuse-to-boot are unchanged — see "What this does NOT do" at the bottom.

## The problem this closes

Crash-testing the desktop app with `kill -9` used to poison the next launch:

```
[fail] refusing to boot: another writer holds the tenant DB
       aberp serve: another ABERP writer is already running on tenant `test` …
```

`SIGKILL` runs no drop handlers, so aberp-ui's `kill_on_drop` never fires and
its `aberp serve` child survives, holding the cross-process whole-DB writer
flock (`apps/aberp/src/db_writer_lock.rs`). macOS reparents that child to
launchd. The manual fix was:

```bash
lsof apps/aberp-ui/aberp.duckdb     # find the pid
kill <pid>                          # hope it was the right one
```

`run/run_desktop.sh` now does that itself, under a predicate narrow enough that
it cannot hit a writer you actually want.

## The stale-orphan predicate

A pid is reaped only if **every** clause holds. Clause numbers match the
`# (n)` markers in `stale_orphan_writer_pids()`:

| # | Clause | Why |
|---|--------|-----|
| — | gate A: tenant is not `prod` | DEV-only, belt-and-suspenders over the existing `tenant=prod` refusal |
| — | gate B: the db resolves INSIDE this checkout | operator/prod DBs live under `~/.aberp/serve/<tenant>/` — outside the repo, so unreachable from here |
| 1 | holds a `.aberp-db-writer.*.lock` flock file next to the db | that file, not the db, is what blocks boot. Rust opens it `O_CLOEXEC`, so an inherited fd in a child of `serve` can't masquerade as a holder |
| 2 | **also** has the EXACT absolute db file open | the same identification the manual `lsof <abs-path>` used; lsof matches by inode, so a same-named db in another checkout can never match |
| 3 | argv is an `aberp serve` (argv[0] basename exactly `aberp`) | keeps `aberp-ui` and friends out |
| 3b | argv carries `--tenant <this tenant>` | authoritative tenant attribution, straight from the process |
| 4 | **PPID is 1** — a true orphan | a live parent means a legitimately-running writer. Never ours to kill |
| 5 | PGID differs from this launcher run's | can never target a sibling of the run doing the checking |

**One unattributable holder disqualifies the whole set.** If any holder of the
lock file fails any clause, the launcher kills *nothing* — not even a pid that
would have qualified on its own — prints the `lsof` table, and falls through to
today's refusal. False-negative (you kill it by hand) is an annoyance;
false-positive is data loss.

Signals: `SIGTERM` first, then up to `STALE_WRITER_TERM_WAIT_SECS` (5s) for the
kernel to release the flock on exit, then `SIGKILL` and up to
`STALE_WRITER_KILL_WAIT_SECS` (3s) more. The verdict is re-derived from the
predicate afterwards, never assumed.

`lsof`/`ps` and `kill` are not atomic, so argv and PPID are re-checked one more
time immediately before each signal — a pid that exited in between could have
been recycled onto an unrelated process. Any pid that no longer matches is
skipped and reported.

### On "not reachable via the normal handshake"

Clause 4 settles this without a fake probe. The handshake is a line on `serve`'s
**stdout pipe**, whose read end died with the parent, and the listener port is
ephemeral (`--port 0`) and recorded nowhere the launcher reads. An orphan is
therefore provably unreachable from here.

A process that has genuinely **exited** holds nothing: `flock` is released by
the kernel on close, so a leftover lock *file* on its own is inert and needs no
reaping. There is no "exited but lock held" state to handle.

## Automated coverage

```bash
./run/tests/run_desktop_stale_writer_reap_test.sh     # 27 assertions, ~25s
```

It sources the real functions (never a copy, so they cannot drift from the
shipped predicate) and drives them against live compiled mock processes:
the orphan IS reaped and the flock IS acquirable afterwards; and a
live-parented writer, a sibling of this run, a non-`aberp serve` holder,
another tenant's writer, a lock-holder without this exact db open, an
ambiguous set, a db outside the checkout, and `tenant=prod` are all left alone.

Every clause is mutation-covered: deleting any one of the five makes exactly
one scenario fail, and deleting clause 4 produces
`[FAIL] live-parented writer … WAS KILLED — data-loss regression`.

## Manual verification recipe

What the automated test can't cover: the real `aberp serve`, the real DuckDB
handle, the real Tauri shell. Do this once by hand after touching the guard.

### 1. Boot normally

```bash
cd <repo>
./run/run_desktop.sh --tenant test
```

Wait for the SPA to mount. In a second terminal, confirm the real writer:

```bash
lsof apps/aberp-ui/aberp.duckdb
ps -o pid=,ppid=,pgid=,args= -p "$(pgrep -f 'aberp serve --tenant test')"
```

Expect an `aberp serve --tenant test --db ./aberp.duckdb --port 0` whose
**PPID is the `aberp-ui` pid** (not 1). That is the shape the guard must refuse
to touch.

### 2. Manufacture the orphan

`kill -9` the Tauri shell only — not the process group, or you kill the child too:

```bash
kill -9 "$(pgrep -x aberp-ui)"
```

Confirm the writer survived and is now an orphan:

```bash
ps -o pid=,ppid=,args= -p "$(pgrep -f 'aberp serve --tenant test')"
```

Expect the same `aberp serve` with **PPID 1**. Also confirm it still holds the
flock — this is the state that used to block boot:

```bash
lsof apps/aberp-ui/.aberp-db-writer.test.lock
```

Ctrl-C the launcher terminal if tauri-CLI/Vite are still up.

### 3. Relaunch — one step, no hunt

```bash
./run/run_desktop.sh --tenant test
```

Expect, before the build/launch output:

```
[writer-lock] stale orphan `aberp serve --tenant test` pid <PID> (ppid 1)
[writer-lock] holds /…/apps/aberp-ui/aberp.duckdb and its writer flock — sending SIGTERM
[writer-lock] reaped — the DEV writer lock is free; continuing to boot.
```

and then a normal boot to a mounted SPA. `pgrep -f 'aberp serve'` should show
only the new pid. **Pass condition: no manual `lsof`/`kill`, no
`refusing to boot: another writer holds the tenant DB`.**

### 4. The refusal direction (do not skip — this is the safety half)

Prove it still declines when it should. Boot normally (step 1), leave the app
**running**, and from a second terminal start a second launcher:

```bash
./run/run_desktop.sh --tenant test
```

Expect it to keep its hands off the live writer:

```
[writer-lock] the DEV writer lock next to /…/aberp.duckdb is held by a process this
[writer-lock] launcher will NOT touch — a live-parented writer, a sibling of this
[writer-lock] run, or a holder it cannot attribute. Current holders:
[writer-lock]   aberp  <PID> …
[writer-lock] Leaving them alone. If ABERP is already running, quit that window;
```

followed by `aberp serve`'s own single-writer refusal. **Pass condition: the
first app is still running and responsive afterwards.** If the running app
died, stop and treat it as a data-loss regression.

## What this does NOT do

- It does not change `aberp serve`. `db_writer_lock::acquire_or_refuse` and
  serve's boot sequence are byte-identical; `serve` remains the authority and
  still refuses whenever the lock is held, including right after a reap that
  failed. The launcher never reports success on serve's behalf.
- It does not run on the prod path. `run/run_prod.sh` does not source
  `run_desktop.sh`, and both hard gates would refuse anyway. Auto-killing a
  writer in prod stays unacceptable — see `[[trust-code-not-operator]]`.
- It does not touch `~/.aberp/**`. Gate B confines it to a db inside the
  checkout.
- It does not delete lock files. A leftover `.aberp-db-writer.*.lock` with no
  holder is inert; unlinking it would race a peer opening it (the same posture
  as `submission_lock`).
