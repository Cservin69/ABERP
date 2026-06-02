# 08 — FMS orchestration — multi-machine cell coordination

When the second CNC lands, ABERP can no longer treat "the machine" as a singleton. The shop becomes an FMS — multiple machines, automated transport, intermediate buffers, and a scheduler that decides what's worked next.

## TL;DR

- **FMS = multiple machines + automated transport + buffers + scheduler.** Pallets are the durable identity (the traceability primary key).
- **Scheduler algorithms** in order of dumber → smarter: FIFO → SPT → EDD → Critical Ratio → setup-time-minimization.
- **"Scheduling rules have much greater impact on output performance measures than differing FMS layouts"**[^fms-ieom][^fms-sciencedirect] — nail the scheduler before re-arranging the shop floor.
- **Five canonical buffer zones**: raw-stock, machine-input, machine-output, QC-waiting, packaging-waiting.
- **Mazak Palletech, DMG MORI PH Cell, Fastems FMS** are the bundled-FMS competitors. ABERP's strategic angle: **adapter-flexible MES owned by the shop, not by the machine vendor.**

## What an FMS actually is

A Flexible Manufacturing System is multiple machines + automated material transport + intermediate buffers + a scheduler that decides what's worked next.[^fms-arxiv] Pallets, jigs, and fixtures are the standardized material-handling envelope; **the pallet ID is the traceability primary key.**

This is the inflection point where ABERP transitions from "tracks one machine" to "orchestrates many." It's also the point where the architecture decisions matter most — a bad scheduler choice in Phase ι will cause years of operational pain.

## Scheduler algorithms — from dumber to smarter

In order of "dumber but obvious" → "more interesting":

### 1. FIFO (First In, First Out)

First arrival, first machined. **Use this as v1.** It's predictable, easy to explain to operators, and gives you a baseline to beat. Anything more complex starts as "is it actually better than FIFO?"

### 2. SPT (Shortest Processing Time)

Shortest job first. Minimizes average flow time / completion time; classic optimal for single-machine make-span on average. Documented best performer for **minimizing average completion times**.[^fms-ieom]

Caveat: SPT starves long jobs. A multi-hour part might wait forever behind a stream of 5-minute jobs. Mitigate with anti-starvation (boost priority of jobs waiting > N hours).

### 3. EDD (Earliest Due Date)

First due, first machined. Minimizes maximum lateness / tardiness.[^fms-ieom] **Use when due-date adherence is the goal** — which is most precision-machining shops with time-sensitive customers.

### 4. Critical Ratio

`(time remaining until due date) / (remaining processing time)`. Hybrid of EDD and SPT; the lower the ratio, the higher the urgency.[^fms-ratio]

Worked example: a job due in 5 days needing 1 day of work has critical ratio 5.0 (relaxed). A job due in 1 day needing 1 day of work has critical ratio 1.0 (urgent). A job past due needing more work has critical ratio < 1.0 (already late).

### 5. Machine-affinity / setup-time minimization

Group jobs sharing tooling/fixtures to reduce changeover. In a precision-machining shop with diverse parts this **often dominates raw cycle-time differences** — a 30-minute tool change between jobs can cost more than a slow cycle on a different machine.

### The unsurprising research finding

A 2012 ScienceDirect study and a 2016 IEOM paper both find: **"scheduling rules have much greater impact on output performance measures than differing FMS layouts."**[^fms-ieom][^fms-sciencedirect]

Said differently: **nailing the scheduler matters more than the physical layout.** This is good news for ABERP — software improvements compound across all configurations. A better algorithm helps every cell.

### Recommended sequencing for ABERP

```
v1 (Phase ι):     FIFO  — baseline, predictable
v2:               + due-date sorting (EDD overlay on FIFO)
v3:               + Critical Ratio when due-date variance is high
v4:               + setup-time-minimization when setup costs measurable
```

Don't ship v4 in Phase ι. The Phase ι goal is "multiple machines can be served" — not "optimal scheduling." The dumber the v1 scheduler, the easier to beat in v2.

## Pallet management — the traceability primary key

Pallet ID is the durable identity that survives between operations. Each pallet carries a barcode (Code 128 or GS1-128 on a metal-mounted plate) or RFID tag (UHF Class 1 Gen 2 is the de-facto standard at the pallet level — see [04-barcode-qr-scanners.md](04-barcode-qr-scanners.md)).

The pallet's database row owns:
- **Current location** (raw-stock, machine-input-N, in-process-on-machine-N, machine-output-N, QC-waiting, packaging-waiting, shipping)
- **Mounted part(s)** and their travelers — one pallet may carry multiple identical parts (batch) or one large part
- **Next operation** (what state the scheduler thinks this pallet is heading to next)
- **Timestamps for every state transition** — *this is the OEE-time-bucket source of truth* (see [07-oee-mes-metrics.md](07-oee-mes-metrics.md))

**Critical invariant**: a pallet has exactly one current location at any moment. Two simultaneous "I see pallet X" events from different stations is a data-integrity violation that should loud-fail (CLAUDE.md rule 12), not be silently reconciled.

## The five canonical buffer zones

[^fms-arxiv]

| Zone | Role | Capacity unit |
|---|---|---|
| **Raw-stock receiving / staging** | Incoming bar/plate/blank inventory | Pallet slots or floor space |
| **Machine input buffer** (per machine) | Ready-to-run pallets waiting for the machine | 1-12 pallets typical |
| **Machine output buffer** (per machine) | Completed pallets awaiting downstream pickup | 1-12 pallets typical |
| **QC / inspection waiting** | Pallets waiting on the Renishaw gate | Variable; choke point |
| **Packaging / shipping waiting** | Passed parts waiting on package + ship | Variable |

Some shops add a sixth (rework / non-conforming hold). Worth modeling in the data layer; not necessarily a physical zone.

**Buffer overflow is a real failure mode.** If a machine's output buffer is full and the robot can't pick from it, the machine has to pause — even though it's mechanically ready. The scheduler must know buffer capacities and plan around them.

## Failure modes the scheduler must handle

| Failure | Detection | Recovery |
|---|---|---|
| **Machine down** (planned or unplanned) | MTConnect `CONDITION=fault` event, or planned-maintenance flag | Re-route compatible work to other machines; alert maintenance; queue ETA |
| **Transport/robot fault** | Robot adapter `Fault` event | Pause downstream consumers; let upstream output buffer fill until it overflows; surface operator alert |
| **QC reject** | Renishaw `QualityResultReceived { passed: false }` | Pull from buffer; route to rework or scrap; decrement WIP; preserve the failed measurement for the audit trail |
| **Stock-out** | Inventory check at scheduling time | Block dispatch; surface operator request to re-order or substitute |
| **Operator override** | Operator scans "force route" or "skip QC" | Audit who-when-why; allow but loud-flag in the dashboard |

The recovery patterns share a structure: **detect the failure, surface it explicitly to the operator, never silently work around it.** Hülye-biztos.

## Industry reference systems — what we'd compete against

The bundled-FMS players, for context:

- **Mazak PALLETECH**[^mazak-palletech] — typical install: 5 machining centers, ≥42 pallets in a pallet stocker, 3 loading stations, pallet loader, cell controller. Mazak-machine-only; vendor-bundled cell controller.
- **Fastems FMS**[^fastems-dmg] — vendor-neutral pallet automation, extensive history integrating with DMG MORI machine tools. The "open" option in the bundled-FMS space; closest in spirit to ABERP's adapter approach but still a hardware-bundled platform.
- **DMG MORI PH Cell**[^dmg-phcell] — round-storage pallet handling family (PH Cell 800, PH Cell 2000) for 5-axis machining centers. DMG-machine-specific.

**ABERP's strategic angle** vs. these:

| Dimension | Bundled-FMS (Mazak Palletech, DMG PH Cell, Fastems) | ABERP adapter-FMS |
|---|---|---|
| Machine vendor lock | Bundled (or strongly preferred) | None |
| Cell controller | Vendor-bundled | Pi 5 / industrial PC, vendor-neutral (see [05-cell-controllers.md](05-cell-controllers.md)) |
| Scheduler | Vendor's algorithm | Ours; tunable |
| Audit-ledger / traceability | Vendor's; export to a separate ERP | Single ABERP audit-ledger; no L3↔L4 transcoding |
| Upgrade path | Vendor's roadmap | Ours; we own the rate |
| Cost (cell controller per cell) | €5-30k (estimate) | €300-1500 |

**The differentiator isn't features — it's owning the orchestration layer.** When Áben adds a Bystronic laser next to a Mazak CNC next to a manual bend cell, the bundled FMS can't coordinate across them. ABERP-as-adapter-FMS can.

This is the **vertical-integration argument** (Ervin's standing "SpaceX way" posture: in-house everything except regulatory dependencies) made concrete. The bundled FMS is "let the machine vendor own the orchestration layer too." We say no.

## Simulation — out of scope for v1

FlexSim, Simio, AnyLogic are the major simulation tools. Worth knowing they exist; **not worth building against in v1**.

If the scheduler turns out to be tricky, simulate with **real production data** before tuning policies. Replay historical work orders against a candidate scheduler; compare make-span, tardiness, station-utilization. No purpose-built simulator needed — the audit-ledger already records every event we'd want to feed in.

## Adapter shape for ABERP

ABERP at Phase ι serves three roles:

1. **Work-order intake** — accept work orders from Stage 2 (the customer-facing storefront / quote-acceptance flow tracked under the ABERP-site / e2e-shop strand). Each work order becomes one or more pallets.
2. **Scheduling** — for each pallet at a buffer-exit decision point, pick the next-best destination (machine, QC, packaging). v1 = FIFO; later versions add EDD / Critical Ratio.
3. **Dispatch** — issue commands to the robot adapter ("move pallet X from station Y to station Z") and consume status events back.

Canonical event types:

```
WorkOrderCreated      { order_id, pallet_ids, due_date }
PalletEntered         { pallet_id, zone, station? }
PalletScheduledFor    { pallet_id, next_zone, next_station? }
PalletDispatched      { pallet_id, from, to, robot_task_id }
PalletArrived         { pallet_id, at_zone, at_station? }
WorkOrderClosed       { order_id, all_pallets_shipped: true }
```

These chain naturally to the events from [03-robot-controllers.md](03-robot-controllers.md) (`MoveAccepted` / `MoveComplete`) and [02-renishaw-quality-gate.md](02-renishaw-quality-gate.md) (`QualityResultReceived`).

**Idempotency**: every event has a UUID. Replaying a stream produces the same final state. This matters during recovery — if a cell controller reboots mid-shift, it replays the audit-ledger to reconstruct in-flight pallet locations.

## Where this connects to existing ABERP

- **Quote-intake** (PR-204 backend / PR-210 SPA — the `aberp-quote-intake` crate and Tenant Settings surface): when a quote is approved, that's the trigger for `WorkOrderCreated`. Today the quote-intake daemon stages quotes in `quote_intake_log`; Phase ι extends to staging work orders.
- **Audit-ledger**: same primary data plane. No new infrastructure, just new EventKinds.
- **Invoicing** (Stage 1): when `WorkOrderClosed`, the cost-posting + customer-invoicing flow fires. That's the L3 → L4 boundary crossing (see [07-oee-mes-metrics.md](07-oee-mes-metrics.md)).
- **Operations dashboard** ([07-oee-mes-metrics.md](07-oee-mes-metrics.md)): OEE per cell, TEEP per shop, WIP age — all derived from these events.

## Recommendation framework

**For Phase α (framework ADR)** — what to decide now:

1. **Pallet ID is the traceability primary key.** Every L3 event references a pallet_id (where applicable). RFID-or-barcode physical tagging is a Phase β/γ concern; the data shape commits now.
2. **Adopt the five canonical buffer zones.** Add a sixth (rework hold) in the data layer; physical zones come when the shop floor lays out.
3. **v1 scheduler is FIFO.** EDD, Critical Ratio, setup-minimization are later iterations. Don't over-engineer the first version.
4. **Failure modes loud-fail.** Buffer overflow, machine down, QC reject — all surface explicitly to the operator. No silent reconciliation.
5. **Audit-ledger is the single source of truth.** Replay reconstructs in-flight state on cell-controller reboot.
6. **Bundled-FMS vendors are competitors, not partners.** We talk to their machines via open protocols; we don't run their orchestration layer.
7. **Real-data replay for scheduler tuning** — no FlexSim / Simio dependency.

## What's still unknown

- Whether Áben's existing pallet inventory (or future order) has machine-readable tags or needs new ones. RFID at €5-15 per pallet vs printed barcode at €0.50 — depends on operating environment.
- Specific machine-vendor pallet handling: does the DMG MORI PH Cell mate well with a non-DMG cell controller? Likely yes (DMG plays nice with Fastems) but verify before committing.
- Hungarian shop layout constraints — physical buffer zones depend on floor plan; not something this research can pre-decide.
- How much scheduling intelligence Stage 2's quote-acceptance flow expects — does the quote engine commit a date that the scheduler must honor, or does the scheduler push back on infeasible due-dates?

## Citations

[^fms-ieom]: IEOM Society, "Scheduling of Three FMS Layouts Using Four Scheduling Rules." http://ieomsociety.org/ieom_2016/pdfs/270.pdf — fetched 2026-06-02.
[^fms-sciencedirect]: ScienceDirect, "Performance Study of FMS Scheduling." https://www.sciencedirect.com/science/article/pii/S1877705812022400 — fetched 2026-06-02.
[^fms-ratio]: ResearchGate, "Solving the FMS Scheduling Problem by Critical Ratio-based Heuristics." https://www.researchgate.net/publication/221068662_Solving_the_FMS_Scheduling_Problem_by_Critical_Ratio-based_Heuristics_and_the_Genetic_Algorithm — fetched 2026-06-02.
[^fms-arxiv]: arXiv, "FMS Scheduling with Total Processing Time and Machine Load Balance." https://arxiv.org/pdf/1906.08926 — fetched 2026-06-02.
[^mazak-palletech]: Metalcut Products, "Mazak Palletech Manufacturing Cells." https://www.metalcutproducts.com/technology/mazak-palletech-manufacturing-cells/ — fetched 2026-06-02.
[^fastems-dmg]: Fastems, "Pallet and robot based CNC automation for DMG Mori." https://www.fastems.com/cnc-automation-for-dmg-mori/ — fetched 2026-06-02.
[^dmg-phcell]: DMG MORI, "PH Cell 800." https://us.dmgmori.com/products/automation/pallet-handling/round-storage-system/ph-cell-800 — fetched 2026-06-02.
