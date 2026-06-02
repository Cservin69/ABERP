# 09 — Laser workflow — parallel sheet-metal strand

The CNC + Renishaw + robot side of the shop is one strand. The laser cutter + manual ops is another, running in parallel. The integration shape is mostly the same (status events, audit-ledger, work-order tracking) but the *workflow* differs in three key ways: CAM nesting matters, there's no auto-transport robot between stations, and there's no Renishaw gate.

## TL;DR

- **CAD → CAM nesting → laser cut → manual ops** (bend, weld, deburr, paint, ...). Manual ops dominate the post-cut time budget.
- **Status-only automation** in v1: every manual step has a station ID, work order has a barcode, scan-at-start / scan-at-end. No auto-robot.
- **Bystronic** supports **OPC-UA on ByMotion** (and OPC Classic on CNC 02 ByVision)[^bystronic-mm]. **Trumpf** is OPC-UA-first via Oseon[^trumpf-connect]. **Mazak Optonics** is MTConnect-native[^mazak-mtc].
- **CAM nesting software** (Lantek, Radan, SigmaNEST, JETCAM) produces **nest reports** that name parts/quantities/sheet/material — this is what ABERP consumes, not the NC programs.
- **Bystronic OPC-UA endpoint** is documented on TCP **port 56000** with a configurable address space.[^bystronic-mm]

## The workflow shape

```
Customer order → ABERP quote → CAD file
                                   |
                                   v
                              CAM nesting
                              (Lantek/Radan/SigmaNEST/JETCAM)
                                   |
                          +--------+--------+
                          |                 |
                          v                 v
                    NC programs        nest report
                    (to laser)         (parts × qty,
                                        sheet, material)
                                              |
                                              v
                                         ABERP work-order
                                          generation
                                              |
                          +-------------------+
                          |
                          v
                       Laser cut
                       (Bystronic / Trumpf / Mazak Opto / Amada / Prima Power)
                          |
                          v (parts come off the sheet)
                          |
                          v
                      Manual ops in sequence
                          |
                          ├── Bend
                          ├── Weld
                          ├── Deburr
                          ├── Tap / drill / countersink
                          ├── Paint / powder coat
                          └── Pack / ship
                          |
                          v
                       (back to invoicing per Stage 1)
```

Each manual op is a station with a barcode reader and a work-order. Operator scans work-order at start, scans at end, ABERP records timestamps. **That's the v1 surface.**

## Differences from the CNC + Renishaw strand

| Dimension | CNC strand | Laser strand |
|---|---|---|
| Material transport | Auto (robot) | Manual (operator carries) |
| Quality gate | Auto (Renishaw) | Manual (visual / hand-gauge) |
| Cycle time per part | Minutes-hours | Seconds-to-minutes (cut) + hours (manual ops) |
| Setup cost | Tool change, fixture | Sheet load, nest validation |
| Throughput rhythm | Pallet-based | Sheet-based, then loose-parts |
| WIP age | Per-pallet | Per-work-order, summed across stations |

**The biggest implication**: in the laser strand, the **manual ops dominate the time budget**, not the cut itself. ABERP's value-add is tracking the *manual ops*, not optimizing the laser. That changes the integration priority — get the manual-station scan workflow rock-solid before worrying about live laser telemetry.

## Laser cutter vendors — by integration surface

### Bystronic

- **Control software**: ByVision (touch-driven, runs on both fiber and CO₂ lasers, plus press brakes).[^bystronic-byvision]
- **Production-control software**: Plant Manager (formerly part of BySoft suite).
- **OPC Classic** on all Bystronic lasers with CNC 02 ByVision from version P8008.[^bystronic-mm]
- **OPC UA standard** on ByVision cutting and bending systems with **ByMotion control**.[^bystronic-mm]
- MachineMetrics documents the OPC UA endpoint on **TCP port 56000**, configurable in Administration.[^bystronic-mm]

**Why Bystronic matters for Áben**: Swiss, strong EU service network. The OPC-UA-on-ByMotion path is the cleanest integration surface in the laser space.

### Trumpf

- **TruTops Fab → rebranded as Oseon** (production-control platform); legacy TruTops Fab functions remain inside Oseon.[^trumpf-oseon]
- **OPC UA** is supported on most current Trumpf machines; for older machines Trumpf sells an **OPC UA Retrofit Extension Cube**.[^trumpf-connect]
- **Basic Connectivity Kit** exposes three minimum status signals: *machine idle without malfunction* / *machine idle with malfunction* / *machine producing.*[^trumpf-connect]
- Oseon Analytics ingests these and renders the same dashboard as third-party machines.
- **No specific MTConnect support found** in primary Trumpf material — they have standardized on OPC UA. (Gap: open question whether a Trumpf can act as an MTConnect data source via a third-party adapter; the published API path is OPC UA.)

**Why Trumpf matters for Áben**: German, EU service strong. The "three minimum status signals" connectivity kit is interesting — minimum-viable OPC-UA for ABERP's status board needs.

### Mazak Optonics

- Native **MTConnect** support. Mazak (the parent group's optics arm) actively backs MTConnect, with **"over 200 machine models prepped to accept the MTConnect adapter before they leave the factory."**[^mazak-mtc]
- CNCnetPDM ships a Mazak MTConnect device driver for Mazatrol/Matrix controllers giving near-real-time machine/process/quality data.

**Why Mazak Optonics matters for Áben**: if Áben already has MTConnect tooling in ABERP for the CNC strand (Phase ε), a Mazak laser plugs into the same code path. Reuse the adapter.

### Amada / Prima Power / LVD / Mitsubishi

Coverage thinner from public primary sources. **Lantek Expert lists Amada, Bystronic, Esab, Ficep, Flow, HK Laser, Koike, Mazak, Messer, Prima Power, Salvagnini, and Trumpf** as supported post-processor targets[^lantek-expert] — implying all of these have *some* programmable NC interface, but not that all expose live OPC UA / MTConnect status.

Practical posture: **specify OPC UA or MTConnect as a procurement requirement** when buying. Don't accept "Modbus-only" or "vendor-proprietary-only" lasers without budgeting for an adapter.

## CAM nesting software — what they output

The CAM nest software produces **two things**:

1. **NC program(s)** — the machine code the laser executes. ABERP doesn't consume these.
2. **Nest layout report** — XML or CSV or PDF, names parts, quantities, sheet size, material grade, sometimes where each part lives on the sheet. **This is what ABERP consumes.**

| Software | Vendor / pedigree | Notes |
|---|---|---|
| **Lantek Expert** | Lantek (ES) | Postprocessor matrix is the broadest in the industry: Amada, Bystronic, Trumpf, Prima Power, Mazak, etc.[^lantek-expert] |
| **Radan** | Hexagon | Machine-independent across laser, plasma, waterjet, flame; modules for punching, bending, nesting.[^vanenkhuizen-cam] |
| **SigmaNEST** | Cribmaster / SigmaTEK | Multi-process: tube, beam line, press brake, unfolding, DSTV conversion.[^vanenkhuizen-cam] |
| **JETCAM** | JETCAM Intl | Strong on Amada G98 code output; long history with punch/laser combo machines.[^jetcam-amada] |

All four produce machine NC programs plus a nest layout report. **The exact schema varies per product and is generally behind login walls.** ABERP will need test files from the actual installed CAM to design against — vendor RFI required for production design.

### Why the nest report matters for ABERP

The nest report is the **bill of "what comes off this sheet"**. From it ABERP can:
- Pre-create work-orders for each part (one work-order per unique part-design × quantity)
- Tag each work-order with the source sheet (traceability: which sheet did this part come from, in case of material issue later)
- Stage the work-orders in downstream stations (bend, weld, etc.) so operators see expected work
- Compute material yield (parts area / sheet area)

**Without the nest report**, the operator has to manually record "I cut 5 of part X, 3 of part Y, 1 of part Z from this sheet." That's brittle and slow.

## Manual ops — the post-cut hours

The post-laser workflow varies by job but typical sequence:

1. **De-nesting** — operator pulls parts from the cut sheet. Sometimes parts stay on the sheet as tabs and need to be snapped off / sawn out.
2. **Deburr** — sharp edges from laser cuts get rounded. Manual file or auto deburr machine.
3. **Bend** — press brake forms 3D shapes from flat stock. Common for sheet-metal parts.
4. **Drill / tap** — secondary holes that weren't laser-cut (threaded holes, etc.).
5. **Weld** — multiple parts joined.
6. **Sand / polish** — surface finish for aesthetics or sealing.
7. **Paint / powder coat** — finishing.
8. **Inspect / pack** — final QC and packaging.

**Not all steps for all parts.** Many parts only need deburr + bend + paint. Some need all eight.

**The integration is simple**: each station has a barcode reader and a "current work order" display. Operator scans work-order at arrival, ABERP marks it `InProgress`. When done, scans again, ABERP marks it `CompletedAt(station)`. Same pattern as the FMS pallet handoff (see [08-fms-orchestration.md](08-fms-orchestration.md)) but with humans as the transport.

Optional: a **station-specific data capture** — e.g. press brake operator records bend angle achieved (for QC). This is per-station design, not a v1 concern.

### Press brake — special case

Press brakes (the "bend" station) are often modern enough to speak OPC-UA themselves. Bystronic, Trumpf, LVD all ship press brakes with OPC-UA. If Áben has a modern press brake, treat it as a *partial laser-strand machine* — auto-status from OPC-UA, manual operator scan-attribution.

## Status events for the laser strand

```
LaserNestStarted     { nest_id, sheet_id, started_at }
LaserNestCompleted   { nest_id, parts_produced[], completed_at }
LaserMachineState    { machine_id, state: Producing|IdleNoFault|IdleWithFault, at }
LaserMachineFault    { machine_id, code, message, at }

PartArrivedAtStation { part_id, station_id, operator_id, at }
PartLeftStation      { part_id, station_id, operator_id, at, notes? }

PartScrapped         { part_id, station_id, reason, at }
PartReworked         { part_id, station_id, return_to_station, at }
```

These map to ABERP's canonical event types — `LaserMachineState` is a specialization of `MachineStateChanged` from the CNC strand. **Don't proliferate event types unnecessarily**; reuse the canonical names where the semantics match.

## CAM integration — future, not v1

A natural Phase ι+ evolution is **ABERP-driven CAM dispatch**: customer order → CAD → CAM nest → ABERP work-orders → laser dispatch → manual stations. Tighter integration than v1 ("operator manually exports nest report to ABERP").

But this is **far out**. v1 = "operator copies nest report PDF to ABERP's import folder, ABERP parses it." v2 = "CAM software writes nest report into a shared folder, ABERP watches." v3 (years later) = "ABERP commands CAM software via API." Don't sequence backward.

## HU + EN considerations

Press brake control screens, laser HMIs, CAM software UIs — most are English or Hungarian-localized by the vendor. ABERP's operator-facing strings stay bilingual (HU + EN) — the laser-strand operator-facing UI for manual stations follows the same pattern as the CNC strand: scan event arrives, bilingual confirmation appears, operator presses confirm.

Vendor manuals are universally English; that's documentation territory, not operator-runtime.

## Recommendation framework

**For Phase θ (laser adapter + CAD→CAM dispatch UI)**:

1. **Prefer Bystronic with ByMotion + OPC-UA**[^bystronic-mm] or **Mazak Optonics with MTConnect**[^mazak-mtc] for any new laser purchase. Procurement should require open-protocol support; don't accept Modbus-only.
2. **Trumpf is fine** if Áben buys it for non-integration reasons — Oseon is the platform, OPC-UA the wire. The Basic Connectivity Kit's three minimum signals are the minimum-viable status feed.
3. **v1 doesn't auto-integrate with CAM software.** Operator exports nest report manually; ABERP parses. Re-evaluate after the manual flow is proven.
4. **Manual ops get barcode-scan tracking from day one** ([04-barcode-qr-scanners.md](04-barcode-qr-scanners.md)). Even if the laser itself doesn't have a status feed, the post-laser stations do. **This is the highest-ROI Phase θ work.**
5. **Reuse the CNC-strand event vocabulary** wherever semantics match. `MachineStateChanged` is `MachineStateChanged` whether the machine is a CNC or a laser.
6. **No Renishaw gate on the laser strand** in v1. QC is manual; record operator pass/fail. Auto-QC for sheet metal is a much later concern.

## What's still unknown

- Áben's actual CAM software choice — different schemas means different parsers. Need test files.
- Whether Áben's existing or planned laser has OPC-UA already on-board or needs the Trumpf retrofit cube.
- Bystronic OPC-UA address space details (port 56000 confirmed, but the actual nodes / tags need a vendor address-space export).
- Whether Trumpf's "Basic Connectivity Kit" is included with new machines or a paid add-on.
- How the manual-ops sequence varies by job — is it fixed per part-design, or operator-driven each time? Affects scheduler design.

## Citations

[^bystronic-byvision]: Bystronic, "Software." https://www.bystronic.com/en/products/software/ — fetched 2026-06-02.
[^bystronic-mm]: MachineMetrics, "Connecting Bystronic Machines with OPC-UA." https://support.machinemetrics.com/hc/en-us/articles/32582076154899-Connecting-Bystronic-Machines-with-OPC-UA — fetched 2026-06-02.
[^trumpf-oseon]: TRUMPF, "TruTops Fab production control." https://www.trumpf.com/en_US/products/software/trutops-fab/ — fetched 2026-06-02.
[^trumpf-connect]: TRUMPF, "Connectivity." https://www.trumpf.com/en_US/products/services/services-machines-systems-and-lasers/monitoring-analysis/connectivity/ — fetched 2026-06-02.
[^mazak-mtc]: MachineMetrics, "Collect Data from Mazak CNC with MTConnect." https://www.machinemetrics.com/connectivity/machines-controls/mazak — fetched 2026-06-02.
[^lantek-expert]: Lantek, "CAD/CAM Nesting Software — Lantek Expert." https://www.lantek.com/us/cad-cam-nesting-software-oxycut-plasma-laser-waterjet — fetched 2026-06-02.
[^vanenkhuizen-cam]: Luke van Enkhuizen, "Top 10 CAD/CAM Software for Sheet Metal." https://vanenkhuizen.com/en/articles/the-10-best-cad-cam-software-solutions-for-sheet-metal-and-tube-processing/ — fetched 2026-06-02.
[^jetcam-amada]: JETCAM, "Amada G98 CADCAM nesting software support." https://www.jetcam.net/Amada_g98.htm — fetched 2026-06-02.
