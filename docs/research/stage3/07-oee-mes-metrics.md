# 07 — OEE / MES metrics — the operations dashboard

ABERP already has a financial dashboard (PR-221+222, S225/S226). When Stage 3 lands, the *operations* dashboard is its mirror — same UI grammar, different math. This file is the math.

## TL;DR

- **OEE = Availability × Performance × Quality.** Formula is uniform across every credible source. Worked example below.
- **World-class OEE = 85%** — but it's a 1970s Japanese-automotive benchmark; most plants today average ~60%, and a precision-machining job shop naturally scores lower than a dedicated high-volume line.
- **MES is ISA-95 Level 3, ERP is ISA-95 Level 4.** Keep the line clean: ABERP-invoicing is L4; the Stage 3 shop-floor module is L3.
- **TEEP = OEE × Loading.** Exposes the scheduling utilization axis OEE hides.
- The metrics worth surfacing in an ABERP operations dashboard: OEE (and its three components per machine), TEEP, DPMO, throughput, takt-time vs cycle-time, first-pass yield, scrap rate, WIP age, station utilization.

## The formula, cross-referenced

**OEE = Availability × Performance × Quality**

Where:
- **Availability** = Operating Time / Planned Production Time
- **Performance** = (Ideal Cycle Time × Total Count) / Operating Time
- **Quality** = Good Count / Total Count

This decomposition is uniform across every source surveyed.[^oee-com-calc][^teeptrak-oee][^leanworx-oee]

### Worked example (from leanworx.ai)[^leanworx-oee]

A machine running:
- 420 min of 480 planned → **A = 87.5%**
- 350 parts at 1 min ideal cycle in those 420 min → **P = 83.3%**
- 330 good of 350 → **Q = 94.3%**

**OEE = 0.875 × 0.833 × 0.943 = 68.7%.**

Each factor is a fraction of 1.0; OEE is the product. A machine has to score well in all three to score well overall — which is the whole point of the metric. It surfaces hidden losses: a machine that's "always running" (high A) but slowly (low P) or wastefully (low Q) doesn't get away with calling itself productive.

### The six big losses (Nakajima's taxonomy)

OEE decomposes losses into six categories[^nakajima-tpm]:

| Loss | Affects | Examples |
|---|---|---|
| Breakdowns | Availability | Machine down, mechanical failure |
| Setup & adjustments | Availability | Tool change, fixture change between jobs |
| Idling & minor stops | Performance | Operator absent, material starved |
| Reduced speed | Performance | Running below rated feed/speed |
| Defects & rework | Quality | Out-of-spec parts that need rework |
| Startup losses | Quality | First-N-parts-after-setup scrap |

Each of these maps to events ABERP can record. Setup/adjustments come from "operator scanned setup-start, then setup-end" (barcode). Idling comes from `execution=READY` periods in MTConnect. Reduced speed comes from `feedrate_override < 100%` in MTConnect SAMPLE data. Defects come from Renishaw `QualityResultReceived` events. **All six are derivable from events ABERP already plans to record** — the dashboard math is aggregation, not new instrumentation.

## World-class OEE benchmark — and its honest caveats

**85% overall OEE** is the world-class benchmark, established by **Seiichi Nakajima** at the Japan Institute of Plant Maintenance in the early 1970s as part of TPM (Total Productive Maintenance), formally published in his 1984 *Introduction to TPM*.[^oee-com-wc] Every plant winning Japan's Distinguished Plant Prize for TPM implementation exceeded 85%.

**The benchmark has caveats the popular literature usually omits.** OEE.com itself is explicit:

> "[The figure originated] in a particular place (Japan), at a particular time (1970s), and in a particular industry (automotive). Most manufacturing today averages around 60% OEE — with more plants below 45% than above 85%."[^oee-com-wc]

Industry-specific benchmarks vary:[^leanworx-oee]
- Automotive: 85%
- Food & beverage: 80%
- Pharma: 75-80%
- Electronics: 85-90%

**Critical for Áben's shop**: leanworx is explicit that **"a precision-machining job shop running many different parts will naturally score lower than a high-volume dedicated line."**[^leanworx-oee] Setup losses dominate in low-volume / high-mix work because every job change is a setup. Don't compare a 3-machine, 50-different-parts-per-month shop to a Toyota line and feel bad.

**Realistic Áben targets** (industry estimate, not vendor-cited):
- OEE 50-70% is a reasonable Year-1 baseline for a precision-machining job shop
- Improvement to 70-80% is achievable with disciplined TPM
- 85%+ requires the kind of dedicated-line setup Áben deliberately won't have

The metric's value isn't comparing to 85% — it's comparing to *yourself last quarter*. The dashboard should show trends, not just current numbers.

## TEEP — the metric OEE hides

**TEEP = Availability × Performance × Quality × Loading**, where:

- **Loading** = Planned Production Time / Total Calendar Time

TEEP exposes the *scheduling utilization* axis OEE hides.[^teep-tractian] A single-shift line with 100% OEE has **TEEP = 33%** (one 8-hour shift out of 24 hours = 1/3 loading).

For Áben's shop, this matters because the question "should we go to a second shift?" is a TEEP question, not an OEE question. If TEEP is 30% and OEE is 80%, the shop is already running well during scheduled hours — more output requires more scheduled hours, not better hours. If TEEP is 30% and OEE is 40%, the cheap fix is fixing the existing shift before adding a second.

The dashboard should display both. They answer different questions.

## DPMO — Six Sigma quality normalization

**DPMO = (Total defects / (Units inspected × Opportunities per unit)) × 1,000,000**[^teeptrak-quality]

DPMO normalizes defect rate across product mix. "5% defective" sounds the same on a part with one critical dimension as on a part with twelve, but the *opportunity space* is different. DPMO accounts for it.

For Áben, DPMO is **secondary to first-pass yield (FPY)** — DPMO requires knowing "opportunities per unit" per part type, which is an inspection-plan engineering exercise. FPY is simpler and good enough for v1.

## Takt time vs cycle time

- **Takt time** = available production time / customer demand quantity. Customer-set pace.
- **Cycle time** = how long the machine actually takes per part. Machine-set pace.

The rule: **cycle time ≤ takt**. If cycle time > takt, the line can't keep up with demand. If cycle time << takt, the machine is idle most of the time (or you're over-investing in capacity).

For a job-shop, takt is per-order-deadline, not a steady-state demand pace. The dashboard should still show cycle-time per part-family and let the operator overlay due-dates to spot trouble.

## First-Pass Yield (FPY) vs Quality

- **Quality** (in OEE) = good final parts / total starts
- **First-Pass Yield (FPY)** = parts passing first inspection without rework / starts[^teeptrak-quality]

The difference: FPY counts rework as a loss; Quality counts rework as eventually-good. FPY captures the rework cost that final Quality hides.

For a precision shop, **FPY is the operationally interesting metric.** Rework eats labor and shop time even when the part ships eventually. A high Quality with low FPY is a shop with hidden labor cost.

## Throughput, WIP age, station utilization

These are operational rather than benchmark-able:

- **Throughput** = parts/hour or parts/shift, raw count
- **WIP age** = time-in-system for parts currently in-process. Long tails signal stuck work.
- **Station utilization** = % of available time station was running (not idle, not setup)

Useful for spotting stuck work and load-balancing across stations. The financial dashboard's analog is "DSO trending up" — operations dashboard's analog is "WIP age 95th percentile creeping up over weeks."

## MES vs ERP — the ISA-95 boundary

The canonical industry separation:[^isa95-connect981][^isa95-isa]

- **ERP is Level 4.** Order entry, MRP, finance, customer-delivery commitments. Plans production and accounts for it.
- **MES is Level 3.** Dispatch, time-stamped execution data, genealogy/traceability records. Executes production and proves it happened.
- **ISA-95 (also IEC/ISO 62264)** "primarily deals with the interface between levels 3 and 4."[^isa95-connect981]
- MES has four functional areas: **Production / Quality / Maintenance / Inventory** operations management.[^isa95-connect981]
- **B2MML** provides ISA-95-aligned XML schemas for L3↔L4 messaging.[^isa95-connect981]

**For ABERP specifically**:

| ABERP today (Stage 1) | What it is in ISA-95 |
|---|---|
| Invoicing | L4 — financial accounting |
| Partners, products | L4 — master data |
| AP module | L4 — financial accounting |
| NAV submit | L4 — regulatory reporting |
| Audit ledger | Spans both — but the financial events are L4 |

| ABERP future (Stage 3) | What it is in ISA-95 |
|---|---|
| Work-order dispatch | L3 — production operations |
| Machine status, scans, robot events | L3 — production operations |
| QC results | L3 — quality operations |
| Traveler / WIP tracking | L3 — production operations |
| OEE dashboard | L3-derived; surfaces L3 events |

The clean separation: the **MES module dispatches and records; it hands a closed traveler back up to ERP, which posts cost and updates inventory.** Don't smear them. When a work order closes, that's the L3→L4 boundary crossing — and that crossing should be an explicit event (B2MML-shaped if we want full ISA-95 conformance, ABERP-native if we don't).

**Practical implication for ABERP's audit-ledger**: the existing `EventKind` enum extends naturally with L3 event types (`WorkOrderDispatched`, `MachineExecutionStateChanged`, `PartScanned`, etc.) but the L3 events should be visually separable from L4 events in the UI — an operator looking at the financial dashboard shouldn't be drowning in machine state changes.

## The operations dashboard — proposed shape

Parallel to the financial dashboard layout from S225 (PR-221 / PR-222):

```
+----------------------------------------------------------+
| OPERATIONS DASHBOARD            [Period: This week ▼]    |
+----------------------------------------------------------+
| OEE Today                                                |
|   Cell 1 (DMG-1):  68% (A 87 • P 83 • Q 94)              |
|   Cell 2 (DMG-2):  72% (A 91 • P 85 • Q 93)              |
|   Cell 3 (Laser):  55% (A 78 • P 79 • Q 88)              |
| ───────────────────────────────────────────              |
| TEEP (this week, all cells)        45%                   |
| Throughput (this week)             1240 parts            |
| First-Pass Yield (this week)       91%                   |
| Scrap rate                          3%                   |
| ───────────────────────────────────────────              |
| WIP age, 95th percentile         18.5 hr                 |
| Stuck travelers (> 48 hr)              3                 |
| ───────────────────────────────────────────              |
| Top loss reasons (this week)                             |
|   1. Setup time                 18% of A-loss            |
|   2. Material starved            7% of A-loss            |
|   3. Spindle alarm 4032          3% of A-loss            |
+----------------------------------------------------------+
```

Period selector matches the financial dashboard's vocabulary: this week / last week / this month / YTD / custom range.

**Drill-down**: each row clicks through to the underlying audit-ledger events. The OEE percentages aren't magic numbers — every operator can click and see the actual machine-state-changed events that produced them.

## Loose connections to existing ABERP

- **Audit-ledger**: every metric on the operations dashboard is derived from audit-ledger events. Same primary data plane as the financial dashboard. Same realtime-computation pattern.
- **Period selector**: reuse the same Svelte component as the financial dashboard (S225).
- **Currency-less**: operations dashboard has no currency dimension — but it does have per-cell and per-shift dimensions. The component shape generalises.
- **Per-product analytics**: bridge to S225 — "parts shipped this week × ASP" = operations × finance cross-reference. Not for v1 but a natural future card.

## Anti-patterns to avoid

- **Don't make OEE a vanity metric**. The number that matters is the trend, not the comparison to 85%.
- **Don't hide the components.** OEE alone is useless without A/P/Q breakdown — operators need to know which factor moved.
- **Don't compute OEE per-shift only.** Per-job and per-part-family views catch losses that per-shift averages smooth over.
- **Don't include "planned downtime" in unplanned downtime.** Lunch isn't a breakdown. The Availability denominator is *planned production time*, not calendar time — that's the point of separating OEE from TEEP.
- **Don't let the dashboard become an operator-blame surface.** It's for finding system losses, not punishing slow shifts.

## Recommendation framework

**For Phase α (framework ADR)** — what to decide now:

1. **Adopt OEE as the primary operations metric** in the future operations dashboard. Industry standard, well-understood, decomposable.
2. **Adopt TEEP as the secondary metric.** Single-number OEE hides the scheduling question.
3. **Use FPY, not DPMO**, as the v1 quality-secondary metric. Simpler input requirements.
4. **ISA-95 L3/L4 boundary** is an explicit design principle. MES module hands closed travelers to ERP; ERP doesn't reach into MES state.
5. **Audit-ledger as the operations data plane** — same pattern as financial dashboard. No second source-of-truth for L3 events.
6. **Operations dashboard MVP** = OEE per cell, TEEP per shop, throughput, FPY, scrap rate, WIP age 95p, top-3 loss reasons. Period selector matching the financial dashboard.
7. **Defer DPMO, takt analysis, and per-product yield drill-downs** to v2.

## What's still unknown

- Whether ABERP's existing `EventKind` set extends cleanly to L3 events without renaming.
- Whether B2MML conformance is worth pursuing (probably not for v1; vendor neutrality on the data plane is good enough).
- How operators want to slice OEE — by shift? by operator? by part-family? Need shop-floor design sessions to nail.
- Whether the financial dashboard's period-selector component generalises cleanly or needs a parallel one.

## Citations

[^oee-com-calc]: OEE.com, "Calculating OEE: Definitions, Formulas, and Examples." https://www.oee.com/calculating-oee/ — fetched 2026-06-02.
[^teeptrak-oee]: TEEPTRAK, "What is OEE? Complete Guide." https://teeptrak.com/en/what-is-oee-how-calculated-complete-guide-2027-5/ — fetched 2026-06-02.
[^leanworx-oee]: Leanworx, "OEE Formula Guide 2026." https://leanworx.ai/oee-formula-calculating-oee/ — fetched 2026-06-02.
[^nakajima-tpm]: Seiichi Nakajima, *Introduction to TPM: Total Productive Maintenance*, 1984. Productivity Press. (Referenced across OEE.com, TEEPTRAK, leanworx; primary publication paywalled.)
[^oee-com-wc]: OEE.com, "World-Class OEE." https://www.oee.com/world-class-oee/ — fetched 2026-06-02.
[^teep-tractian]: Tractian, "Total Effective Equipment Performance." https://tractian.com/en/blog/total-effective-equipment-performance-complete-guide — fetched 2026-06-02.
[^teeptrak-quality]: TEEPTRAK, "Quality Monitoring in Production Guide." https://teeptrak.com/en/quality-monitoring-in-production/ — fetched 2026-06-02.
[^isa95-connect981]: Connect981, "Understanding the MES-ERP Boundary Through ISA-95." https://connect981.com/blog-posts/isa95-mes-erp-boundary-20260122 — fetched 2026-06-02.
[^isa95-isa]: International Society of Automation, "ISA-95 Standard." https://www.isa.org/standards-and-publications/isa-standards/isa-95-standard — fetched 2026-06-02.
