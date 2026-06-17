# ADR-0092 — On-machine probe ingestion → QC inspections → auto-NCR

- **Status:** Proposed
- **Date:** 2026-06-16
- **Deciders:** Ervin (S442 brief — observed a DMG MORI spindle probe + toolchanger on a production machine; wants probe results in ABERP to replace manual CMM/clipboard QC). Auto-mode research + ADR; NO code in this session.
- **Implements:** automated in-process / finish-cycle dimensional inspection capture from on-machine touch probes, closing the loop from a probe touch to a `qc_inspections` record, an auto-NCR on out-of-tolerance (AS9100D §8.6/§8.7), and the existing Refuse-Shipment gate.
- **Related:** ADR-0060 (Stage-3 MES adapter framework + `CanonicalEvent` — the `MtconnectAdapter` this extends and the `QualityResultReceived` variant we must reconcile), ADR-0063 (S233 `aberp-qa` `qa_inspections` routing-op decision queue — the system `qc_inspections` does NOT replace), ADR-0089 (S438 part-UID marking — the `part_uid` an inspection links to + the shipment gate), ADR-0085 (S432 heat-lot traceability — the `heat_lot_reference` an inspection cites), ADR-0090 (S439 NCR/CAPA — the auto-NCR target + Refuse-Shipment gate), ADR-0087 (S441 timestamp-anchored audit chain — inspection records ride it), `docs/findings/dmg-mori-probe-research-2026-06-16.md` (the full research), `[[trust-code-not-operator]]`, `[[hulye-biztos]]`, `[[no-sql-specific]]`, `[[spacex-vertical-integration]]`.

## Context

**The workflow problem.** Today a defense/aero part is finished on the machine, carried to a bench or CMM, measured by hand or program, and the results are written on paper or Excel. NCRs are raised manually, later, from memory. S330's gap analysis named the consequence directly: *"CAPA-needs-data inflates because measurements are lossy."* Transcription errors, selective recording ("re-measure the marginal feature until it passes"), and the detachment of the measurement from the WO/part/heat-lot all degrade the AS9100D §8.6 release evidence and starve CAPA root-cause analysis.

**The technical opportunity.** The machine already probes. Modern DMG MORI machines ship a free IIoT interface (MTConnect / OPC UA / MQTT via the IoTconnector) as standard, and ABERP already has a shipped, tested `MtconnectAdapter` (S247/ADR-0060) polling `/current`. The probe data exists on the shop floor; ABERP simply does not capture it. Capturing it makes every probe touch a tamper-evident, linked, auto-triaged inspection record.

**Five facts from the codebase + research shaped this design (verified, not assumed):**

1. **Base MTConnect does NOT carry a probe *verdict*.** There is no `ProbeData` data item; the `Probe` SAMPLE subType is *deprecated* in the spec. MTConnect carries the measured *value* (`SAMPLE`, `subType="ACTUAL"`, with `units`) and a fault `Condition`. The full nominal/tolerance/pass-fail characteristic lives only in MTConnect Part 4.4 **QIF assets** or **OPC UA GMS (OPC 40210)** — both future-tier. *(research §2.1)* **So the pass/fail + tier is computed in ABERP** from the ingested ACTUAL value against an ABERP-held nominal + tolerance band. This is *better* for `[[trust-code-not-operator]]`: the verdict is deterministic ABERP code, not a vendor's optional field.

2. **`CanonicalEvent::QualityResultReceived { part_id, gate_id, outcome, note }` already exists** in `aberp-mes` (ADR-0060), documented as *"a measurement gate (Renishaw / on-machine probe / hand-gauge)"* — **but it has no binary consumer and carries no dimensional numbers** (only a pass/fail/hold `QualityOutcome`). The `MtconnectAdapter` today emits only `MachineStateChanged`.

3. **`aberp-qa::qa_inspections` (ADR-0063) is a different altitude.** It is a routing-op-gated Pass/Fail/Rework/Dispose *decision* with an optional free-text `measurement` string, not linked to `part_uid`/`heat_lot`, with no numeric nominal/actual/tolerance. ADR-0063 explicitly anticipated that a future `QualityResultReceived` would call the same `decide_qa` handler with `actor: Adapter(name)`.

4. **`NewNcr` (S439) already carries `affected_part_uids` / `affected_wo_ids` / `affected_heat_lots`.** The linkage surface for auto-NCR exists; S440 already auto-creates an NCR from PO receiving (`purchasing.rs` → `quality::create_ncr` with `NcrCategory::SupplierIssue`). The probe path mirrors this with `NcrCategory::Workmanship`.

5. **`ALL_KINDS_COUNT` is pinned at 180** (S441), double-entry-tested against the round-trip variant list and a hard `180` assertion. Six new kinds → a real delta of **180 → 186**.

## Decision

**Add a `qc_inspections` table fed by an extended `MtconnectAdapter` that ingests probe `SAMPLE` (`subType="ACTUAL"`) values, computes the pass/minor/major/critical tier in code against an ABERP-held inspection-plan nominal + tolerance, links each inspection to WO + part_uid (S438) + heat_lot (S432), auto-creates an S439 NCR on out-of-tolerance (severity per tier), and warns instead of NCR-ing when the probe's calibration is stale.** MTConnect is the primary transport; OPC UA umati/GMS is the future tier (when the fleet upgrades / machine-supplied verdicts are wanted); G-code/FOCAS variable polling is the documented last resort for legacy controls. **NOT the DMG MORI vendor SDK as primary** — lock-in, per `[[spacex-vertical-integration]]` (same call ADR-0060 already made; we consume the IoTconnector's *MTConnect output*, not its proprietary API). Six new `qc.*` EventKinds (count **180 → 186**).

### Reconciliation with existing quality surfaces (CLAUDE.md rule 7 — surface the conflict, don't blend)

`qc_inspections` is the **per-feature dimensional measurement** record (nominal/actual/deviation/tier). It does **not** replace `qa_inspections` (the per-routing-op decision) and does **not** repurpose `QualityResultReceived` (pass/fail-only, dimensionless). The three coexist at three altitudes:

- `qc_inspections` (new) — one row per probed feature, with numbers and a computed tier.
- `qa_inspections` (ADR-0063) — one row per routing-op, an operator/adapter Pass/Fail/Rework/Dispose decision.
- `QualityResultReceived` (ADR-0060) — a pass/fail signal variant, currently unconsumed.

**v1 leaves the qa↔qc linkage out of scope** (a qc Pass *could* later satisfy a routing-op `qa_inspections` row via `decide_qa(actor=Adapter)` as ADR-0063 foresaw — that is a clean v-future seam, named here, not built). For the new adapter event we add a **new** `CanonicalEvent::ProbeMeasurementReceived` variant carrying the dimensional fields rather than overloading `QualityResultReceived` — its pass/fail-only shape cannot carry nominal/actual/deviation, and although it has no prod consumer/data (so it *could* be evolved), a distinct variant keeps the dimensionless gate signal and the dimensional measurement semantically separate.

### Schema (additive, natural-keyed, no SQL DEFAULT / no CHECK / no index — [[no-sql-specific]])

- **`qc_inspections`** — `qci_<ULID>` natural PK. Columns per the research §3.1 data shape: `tenant_id`, `measured_at_utc`, `wo_id`, `part_uid`, `heat_lot_reference`, `machine_id`, `device_name`, `operator`, `tool_number`, `probe_model`, `feature_name`, `nominal`, `actual`, `deviation`, `tol_upper`, `tol_lower`, `units`, `result` (lowercase token `pass|minor|major|critical|calibration_stale`), `probe_calibrated_at_utc`, `calibration_stale`, `axis_positions` (JSON text), `probe_temp_c` (nullable), `source_seq`, `raw_excerpt`, `auto_ncr_id` (nullable — set when this inspection spawned an NCR). No CHECK / no DEFAULT (the DuckDB replay-clobber trap); a non-probing tenant simply has zero rows. Filter/sort/page in Rust over a full scan (no index — S341/S410 precedent).
- **`qc_inspection_plans`** (the nominal/tolerance source of truth) — keyed by `(tenant_id, product_id, feature_name)`, carrying `nominal`, `tol_upper`, `tol_lower`, `units`. This is what makes the verdict ABERP's code, not the machine's. Sparse; operator-maintained CRUD. *(If Ervin prefers nominal/tol to live on the WO routing op rather than a per-product plan, that is an open question §Open — the table shape is otherwise identical.)*

### Trust the code, not the operator ([[trust-code-not-operator]])

Every safety rule lives in code:

1. **Verdict + tier are computed, never trusted from the wire.** A pure function `classify_measurement(nominal, actual, tol_upper, tol_lower) -> QcResult` (research §5): overage `O` past the nearer band edge, band width `W = tol_upper - tol_lower`; `O==0 → Pass`, `0<O≤1·W → Minor`, `1·W<O≤2·W → Major`, `O>2·W → Critical`. Property-testable, no I/O.
2. **Units mismatch fails loud (CLAUDE.md rule 12).** If the SAMPLE `units` ≠ the plan's `units`, the measurement is rejected with `QcProbeIngestionFailed`, never silently coerced.
3. **`UNAVAILABLE` / missing value is missing, not zero.** The existing parser already treats absent leaves as `None`; an `UNAVAILABLE` actual yields no inspection row, never a spurious 0.000 pass.
4. **Stale calibration suppresses the NCR.** `calibration_stale = (now − probe_calibrated_at_utc) > STALE_WINDOW (default 14d) OR a crash recorded since calibration`. Stale → record the row with `result=calibration_stale`, emit `QcProbeCalibrationStaleWarning`, surface a grey chip + dashboard card; **do NOT auto-NCR** (a probe that may be lying must not manufacture a false defect — ISO 9001 §7.1.5.2). *(research §6)*
5. **Gap-safe ingestion.** Poll `/sample?from=<nextSequence>`; on HTTP 404 `OUT_OF_RANGE` (buffer overrun) log loudly + re-baseline from `/current` — never silently skip sequence ranges. *(research §2.1)*
6. **Out-of-tolerance → auto-NCR via the existing `create_ncr`** (mirrors S440 receiving), `NcrCategory::Workmanship`, severity = tier, `affected_part_uids=[part_uid]`, `affected_wo_ids=[wo_id]`, `affected_heat_lots=[heat_lot_reference]`. The resulting Open NCR engages the existing S438/S439 **Refuse-Shipment gate** unchanged (AS9100D §8.7).

### Operator UX ([[hulye-biztos]])

The operator sees a **green (pass) / yellow (minor·major) / red (critical) / grey (calibration-stale)** chip per measurement — no MTConnect/OPC-UA/QIF awareness required. No probe-protocol knowledge anywhere in the operator surface.

### EventKinds (count 180 → 186)

`qc.` prefix (a new sub-family under the `mes.`-adjacent operational namespace; keeps each existing prefix consumer's glob narrow per ADR-0060):

1. **`QcInspectionRecorded`** — one row written (any result, incl. stale). Payload: `qci_id`, `wo_id`, `part_uid`, `feature_name`, `actual`, `deviation`, `result`, `machine_id`.
2. **`QcInspectionPassed`** — convenience/queryability twin fired when `result=pass` (distinct so a pass-rate query needn't parse payloads — mirrors the NCR `ncr.*` split rationale).
3. **`QcInspectionFailed`** — fired on minor/major/critical. Payload adds `severity`.
4. **`QcAutoNcrCreated`** — fired when an NCR was auto-spawned. Payload: `qci_id` + the new `ncr_id` (the audit cross-link).
5. **`QcProbeCalibrationStaleWarning`** — stale measurement; no NCR. Payload: `qci_id`, `probe_model`, `probe_calibrated_at_utc`, `stale_by_days`.
6. **`QcProbeIngestionFailed`** — malformed/missing field, units mismatch, or a probe `Condition` Fault. Payload: a reason string + the offending `raw_excerpt` (no secrets). Fails loud, never silent.

All six get the full F12 ritual: round-trip `as_str`/`from_str`, `ALL_KINDS` entry, the NAV-leakage pins (both arms), and the `ALL_KINDS_COUNT` bump 180 → 186.

### Adapter extension

The `MtconnectAdapter` (`crates/aberp-mes/src/adapters/mtconnect.rs`) gains an optional probe-component subscription: when configured with the probe `Sensor` component name + the `dataItemId`→`feature_name` map, it extends `MtconnectSnapshot` with the ACTUAL SAMPLE values + `Condition` for that component, emits `ProbeMeasurementReceived` on a new measured value (deduped by `sequence`), and the ledger writer routes it into the `qc_inspections` pipeline. **Handle malformed/missing fields gracefully** — real agents omit items, emit `UNAVAILABLE`, and reorder elements; the parser already skips unknown leaves (forward-compat) and this must extend that posture (unknown `dataItemId` → ignored, not error). The existing `MachineStateChanged` path is untouched.

## Consequences

- **New** `qc_inspections` + `qc_inspection_plans` tables, six `qc.*` EventKinds (180→186), a pure `classify_measurement` + `calibration_stale` function pair, an extended MTConnect adapter + a new `ProbeMeasurementReceived` canonical event, an auto-NCR call (reusing `create_ncr`), and an SPA Quality sub-surface.
- **SPA:** a per-WO probe-inspections list, a per-part_uid probe history (folds into the existing S438 Part UID Lookup tab), and a **dashboard stale-calibration card** (mirrors the S439 critical-NCR escalation banner). Operational, no new top-level module needed — extends the existing Quality module.
- **No change** to `qa_inspections`, `QualityResultReceived` semantics, the Refuse-Shipment gate, or the commercial (non-defense) path. A non-probing tenant has zero `qc_*` rows.
- **The verdict authority moves into ABERP** (good for audit/trust), which means ABERP must hold an inspection plan per probed feature — a new master-data maintenance burden the operator (or a CAM import, v-future) takes on.
- **Implementation is gated on a real `/probe` capture** confirming the target machine's agent actually exposes probe items (see §Open / research gap #1). If it does not, v1 falls to QIF assets / OPC UA GMS / G-code polling — a materially larger build.

## Acceptance criteria (for the follow-up implementation session[s])

1. **End-to-end happy + fail path:** a simulated MTConnect probe SAMPLE event (authentic-shaped fixture XML pasted into the test, per the existing `streams_xml` helper) → `qc_inspections` row created with the correct computed `deviation`/`result` → an out-of-tolerance measurement fires `create_ncr` (Workmanship, correct severity) → the resulting Open NCR engages the S438/S439 Refuse-Shipment gate (defense WO → `mark_shipped` 409).
2. **Tier math pinned:** a property/table test over `classify_measurement` covering pass, minor (`O≤1W`), major (`1W<O≤2W`), critical (`O>2W`), both band edges, and the negative-deviation (below `tol_lower`) symmetry.
3. **Calibration-stale:** a stale measurement sets `calibration_stale`, records `result=calibration_stale`, fires `QcProbeCalibrationStaleWarning`, **fires no NCR**, and surfaces on the dashboard card.
4. **Fail-loud ingestion:** a units mismatch, a missing/`UNAVAILABLE` actual, and a probe `Condition` Fault each produce the correct outcome (reject + `QcProbeIngestionFailed`, or no-row, never a spurious pass).
5. **Authentic MTConnect XML fixture** lives in the test (a real-world-ish `<Sensor>`/`<Samples>`/`<Conditions>` streams excerpt), not a hand-trimmed stub.
6. **SPA:** per-WO inspection list, per-part_uid history (in Part UID Lookup), dashboard stale-calibration card — with the green/yellow/red/grey chip.
7. **Gates green** (ABERP_TEST_PYTHON unset): fmt, clippy `--workspace --all-targets` 0-warn, `cargo test --workspace` 0-fail incl both CAD smoke RAN, vitest, svelte 0/0; `ALL_KINDS_COUNT == 186` and the round-trip double-entry both updated.

## Open questions (Ervin to acknowledge before implementation)

1. **Target machine model + agent reality.** Which specific DMG MORI model is the target, and — the load-bearing unknown — **does its MTConnect agent actually expose probe `Sensor`/measurement items?** First implementation step is a `/probe` + `/current` capture from the real machine (or its agent if reachable remotely). If probe items aren't on the wire, the build is much larger (QIF/GMS/G-code). *(research gap #1)*
2. **Renishaw probe access today, or future planning?** Is there a Renishaw OMI/RMI + probe wired now (and which model — PRIMO vs RMP600 changes nothing in ABERP but confirms scope), or is this forward design? Determines whether v1 ships against live hardware or a simulator.
3. **Tolerance tier ratios.** Accept the 1×/2×-tolerance-band-overage tiers (Minor/Major/Critical)? **No QMS standard mandates these numbers — they are ABERP engineering policy** (research §5). Also: should a "key characteristic" / flight-safety feature escalate to Critical regardless of overage ratio (a v-future knob)?
4. **Calibration-stale window.** 14 days as the default `STALE_WINDOW`, or a different cadence? **No vendor publishes a numeric interval** (research §6) — this is our policy, anchored to ISO 9001 §7.1.5.2. And: is a crash/collision signal available from the target machine to drive the crash-trigger, or is the time window the only gate?
5. **Where does nominal/tolerance live** — a per-product `qc_inspection_plans` table (this ADR's default) or on the WO routing op? Affects who maintains the plan and how a CAM/drawing import (v-future) would populate it.
