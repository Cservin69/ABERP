# ADR-0065 — Adapter task-lifecycle consolidation (and what was deliberately NOT consolidated)

- **Status:** Accepted
- **Date:** 2026-06-05
- **Deciders:** Ervin (via S259 / PR-248 dedup brief)

## Context

S249's adversarial review flagged that the three Phase-δ adapters —
`zebra` (PR-238, TCP ZPL printer), `mtconnect` (PR-240, HTTP poll), and
`ur_rtde` (PR-241, TCP RTDE stream) — each re-implement "socket lifecycle,
exponential backoff, health-state transitions, and periodic probe loops."
Three copies = bug-fix-three-times. The brief asked: consolidate before
adapter #4 (laser / Renishaw / robot packaging) lands — **but push back if
the three don't share enough shape to justify an abstraction.**

This ADR records the design call, because the call — not the LOC saved — is
the deliverable. The honest finding: **the review's premise was ~30% right.**
One of the four named concerns (task lifecycle + health-state machine) is
genuinely shared and worth consolidating. The other three (socket lifecycle,
backoff, probe loops) are *not* shared deeply enough to abstract without
hiding more than they save.

## What the four adapters actually share — measured, not assumed

We read all four adapters (the three δ adapters plus `barcode_scanner`,
PR-225) and laid their structure side by side:

| Concern | zebra | mtconnect | ur_rtde | barcode_scanner |
|---|---|---|---|---|
| Socket role | TCP **client** (probe + on-demand write) | HTTP **client** (poll) | TCP **client** (persistent stream) | TCP **server** (bind + accept) |
| Wire loop | interval TCP-connect *probe* | interval HTTP *poll* + event emit | connect→handshake→read-frames→**reconnect** | accept-loop + per-conn reader |
| Backoff | none (1 retry on print) | none (fixed interval) | **exponential 500ms→30s** | none |
| Health storage | `Arc<Mutex<AdapterHealth>>` | `Arc<Mutex<AdapterHealth>>` | `Arc<Mutex<AdapterHealth>>` | `AtomicU8` state machine |
| Idempotent start/stop + cancel-token + join-handle | ✅ identical | ✅ identical | ✅ identical | ≈ (CAS-based, bind can fail) |
| `classify_io_error` | ✅ (only consumer) | `classify_reqwest_error` (different) | inline, different reason strings | — |

The only row that is **byte-for-byte identical across three adapters** is the
task-lifecycle handshake: the `Stopped → Starting` idempotency guard, minting
+ storing a `CancellationToken`, spawning the loop, and the
`take → cancel → await(+log panic) → set Stopped` teardown. Every other row
is either a different shape per adapter, or has only one consumer.

## Decision

### 1. Extract `AdapterLifecycle` (the genuine 3-way dedup)

`crates/aberp-mes/src/adapters/common.rs` ships a plain **struct** (not a
trait) owning the three interior-mutable cells the δ adapters duplicated —
`health: Arc<Mutex<AdapterHealth>>`, `cancel`, `task` — with the methods
`new`, `health`, `set_health`, `health_slot`, `begin_start`, `attach`, `stop`.

The three δ adapters now hold one `lifecycle: AdapterLifecycle` field instead
of three (`health`/`cancel`/`<x>_handle`), and their `start`/`stop`/`health`
bodies collapse to thin delegations. Each adapter **keeps spawning its own
concrete loop function** — `AdapterLifecycle` brokers only the lifecycle
*around* the loop, never the loop itself.

**Why this one clears the bar:** the health state machine is load-bearing.
S258 (`AdapterHealthTransitioned`) audits every `Healthy/Degraded/Unhealthy/
Starting/Stopped` transition. Having **one** place that writes `Starting` and
`Stopped` is a correctness win, not just a line-count win — the wire-observable
state semantics are now single-sourced. (`begin_start` mints the cancel token
before the synchronous initial probe in zebra/mtconnect; the token is unused
until the spawn, so ordering is unchanged and behaviour is identical.)

**Why a struct, not a trait:** a struct field `self.lifecycle.stop(id)` is
fully greppable with no dynamic dispatch — it *improves* the "three concrete
modules are easier to grep than one generic + N trait impls" concern the brief
raised, rather than worsening it. `tokio::spawn` stays visible in each
adapter's `start()` for the same debuggability reason.

### 2. Deliberately NOT consolidated (pushback)

- **Socket / transport loops.** Probe (zebra), poll (mtconnect), and
  reconnect-stream (ur_rtde) are three genuinely different shapes. A unifying
  `run_loop<F>` closure would have to thread `Arc`s through move-closures and
  fold in mtconnect's inner cancel-race and event emission — more indirection
  than the ~10-line skeletons it would replace. Left concrete.
- **Backoff (`next_backoff`).** Only `ur_rtde` reconnects with backoff — **1 of
  3 consumers.** Promoting it now is designing for a hypothetical adapter #4
  off a single example. Deferred to the rule-of-three: when adapter #4 needs
  backoff, *then* lift it.
- **`classify_io_error`.** Lives in `zebra`, its only consumer. `ur_rtde`
  formats connect errors differently and `mtconnect` classifies `reqwest`
  errors, not `io` errors. Unifying would **change operator-visible reason
  strings** — forbidden by the "net behaviour unchanged on the wire" bar.
- **`barcode_scanner`.** A TCP *server* (binds + accepts inbound), tracks
  health in a lock-free `AtomicU8`, and its `start()` can fail (bind error).
  Adopting `AdapterLifecycle` would be a *behaviour change* (atomic→mutex,
  CAS→guard) to a working adapter, not a dedup. Left as-is.

## Consequences

- **The brief's "each per-adapter module sheds 25–35% LOC" target is not met,
  and shouldn't be.** That metric measures the wrong thing: the δ adapter
  files are 870 / 1170 / 1954 lines, but the *duplicated lifecycle code* was
  only ~45 lines each. Net file shrink is 39 / 30 / 32 lines (≈4.5% / 2.6% /
  1.6%). Measured against the **lifecycle boilerplate specifically**, ~75% of
  it is gone (3 copies → 1). Total production LOC ticks up slightly (~+20)
  once the shared module is counted; the win is single-source-of-truth for the
  audited state machine, not raw line reduction. We state this plainly rather
  than inflate the diff to hit a number.
- **No wire behaviour changes.** No protocol, probe interval, backoff schedule,
  or health-transition semantics changed. The one cosmetic delta: the
  panic-during-stop log line now uses a unified `adapter = <id>` field instead
  of per-adapter `printer_id`/`machine_id`/`robot_id` — the identifier value is
  preserved, only the field name is unified. Error-path diagnostic only.
- **No DB / schema / SQL touched** (honours `feedback_no_sql_specific.md`).
- **Adapter #4's first hour** gets a real head-start on the lifecycle plumbing
  while inheriting zero premature transport/backoff abstraction it might not
  fit. When #4 (or a second backoff consumer) arrives, revisit promoting
  `next_backoff` per the rule-of-three.
