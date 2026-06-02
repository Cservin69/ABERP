# 02 — Renishaw quality gate

When parts come off the CNC, something has to decide pass / rework / scrap. Renishaw is the dominant vendor in this space; this file maps what they sell, what each product actually outputs, and what ABERP would consume.

## TL;DR

- Renishaw splits into **two categories**: on-machine probes (live on the spindle, feed the CNC control directly) and offline gauging (Equator, runs as its own station). ABERP's integration surface lives almost entirely on the offline side, plus the orchestration layer "Renishaw Central."
- **There is no public Renishaw SDK.** The integration story rides on open data formats: CSV, **QIF XML**, DMIS, Q-DAS, **MTConnect 1.4** (documented on Renishaw Central), and emerging **OPC-UA via umati**.
- **Renishaw Central is on-premises** — not cloud. Aligns with Áben's open-standards posture.
- For ABERP v1, target MTConnect 1.4 off Renishaw Central. If that's licensed-out, drop to CSV / QIF XML file-drop. Either is workable.

## The two categories — don't conflate them

| Category | Examples | How data leaves the device |
|---|---|---|
| **(a) On-machine probes** | OMP60 (optical), RMP60 (radio), MP250 (grinder strain-gauge), NC4 (laser tool setter) | Probe → receiver → CNC macro variables. **Never directly to a host PC.** |
| **(b) Offline gauging** | Equator, Equator-X 500 | Runs as a standalone station with a PC and the MODUS Equator software stack. Has its own output formats. |

On-machine probes (category a) are about *closing a loop with the CNC itself* — tool length compensation, in-cycle workpiece touch-off, broken-tool detection. ABERP doesn't see these directly. The data that *might* surface to ABERP comes via the macro layer above them (Inspection Plus) or — more reliably — from Equator at category (b).

### On-machine probes — what each one is

| Product | Type | Path to controller |
|---|---|---|
| **OMP60**[^omp60] | Compact 3D touch-trigger workpiece probe, optical (IR) signal transmission | Optical → OMI receiver → SSR / hard-wired I/O on the CNC |
| **RMP60**[^rmp60] | 63 mm touch-trigger probe, 2.4 GHz FHSS radio (no line-of-sight) | Radio → RMI receiver → I/O |
| **MP250**[^mp250] | First strain-gauge inspection probe for grinders, RENGAGE tech | I/O via HSI-C interface |
| **NC4 / NC4+**[^nc4] | Fixed non-contact laser tool setter (length, diameter, broken-tool detect) | Signal processed by NCi-6 → SSR digital line into CNC |

Note: ABERP doesn't need to know about these *individually*. They're listed so the next person reading this file understands what an operator means when they say "we'll put an RMP60 on the new DMG."

### Inspection Plus — the macro layer

**Inspection Plus** is a set of macro routines that live inside the CNC control (Fanuc, Mazatrol, Yasnac, Fagor, Haas variants all exist).[^inspection-plus-fagor][^inspection-plus-haas] It implements probing cycles, stores results to variables, and can `PRINT` / `DPRNT` a measurement report out the control's serial or Ethernet port. Renishaw also ships **"CNC Reporter"** software for at least Fagor controls to parse these prints into pass/fail / GO-NO-GO reports.[^inspection-plus-fagor]

For ABERP: **treat Inspection Plus output as unstructured text the CNC emits**. There is no documented structured format that's portable across controls. If we want structured measurement data, the path is Equator or Renishaw Central, not Inspection Plus.

## Equator + Equator-X — the offline gauging system

Equator is the comparative gauge that's the most likely ABERP integration target. **Equator-X 500** (2024-era refresh) adds an Absolute mode (250 mm/s, traceable CMM-style) alongside the original Compare mode (500 mm/s, reference-based).[^equator-x]

### Software stack (one layer at a time)

| Component | Role |
|---|---|
| **MODUS Equator**[^modus] | Programming / CAD-driven inspection routine authoring |
| **Organiser**[^organiser] | Shop-floor operator front-end (program selection + run) |
| **Process Monitor**[^organiser] | Detailed result view, can auto-apply tool offsets |
| **IPC** (Intelligent Process Control)[^ipc] | Closed-loop — pushes offset corrections back to the CNC |
| **EZ-IO**[^organiser] | Automation/cell wiring |
| **MODUS IM CHART**[^modus] | Graphical report viewer; consumes **QIF XML** + CAD model |

For ABERP, the load-bearing layer is whatever produces structured output. That's MODUS Equator's export step (next section).

### Output formats — the actual integration surface

Per the MODUS Equator product page,[^modus] results can be output to:

- **CSV** (Excel-readable)
- **ASCII text**
- **DMIS** (Dimensional Measuring Interface Standard)
- **QIF XML** (Quality Information Framework — see below)
- **Q-DAS** (certified output, the SPC standard in automotive)
- Direct write into a **Microsoft SQL Server** database

**No first-party REST or OPC-UA endpoint on Equator itself** was found. The live-data path goes through Renishaw Central; the static-data path goes through these export files.

### QIF — the format that actually matters

**Quality Information Framework** (ANSI QIF, ISO standardization in progress) is the structured XML format the industry has converged on for measurement plans, results, and statistics. It's the modern successor to DMIS (which is line-based and harder to parse).

Why QIF matters for ABERP:
- It's already in MTConnect — **`QIF` is a first-class asset type in the MTConnect spec** (see [06-mtconnect-deep-dive.md](06-mtconnect-deep-dive.md)).
- It carries `Characteristic` definitions with tolerance bands and `MeasuredFeature` instances with actual values — so a parser can derive pass/fail in code, not by trusting an operator's manual flag.
- It survives vendor swaps. Hexagon, Zeiss, Mitutoyo all support QIF too, so if Áben ever replaces Renishaw, the file format outlives the relationship.

ABERP's MVP measurement-ingest path is: **watch a QIF XML drop folder, parse the file, derive pass/fail per characteristic, emit `QualityResultReceived` audit event.**

## Renishaw Central — the orchestration platform

**On-premises** smart-manufacturing data platform; Renishaw's verbatim positioning: *"use local administrators, own your data, ensure your process control is not dependent on internet connections."*[^central] This alone makes it interesting — most equivalent platforms today push cloud-only.

What Renishaw Central does:
- Collects data from Renishaw devices (additive, CMMs, machine-tool probes, Equator) **and from third-party kit**[^central-3p]
- Presents it (web UI) and *actions* it (closed loops to the CNC via IPC)
- Exposes downstream feeds

What it exposes outward (from the Renishaw Central data sheet[^central-datasheet]):
- **MTConnect 1.4** output (Equator ESS 1+ through ESS 2.2.0+)
- **CSV** input and output
- **Power BI** integration (named example in the marketing)

What's emerging:
- **OPC-UA via umati**[^renishaw-umati] — Renishaw joined the umati community and sits on the MTConnect standards committee. umati support is being adopted into Renishaw Central but is not yet a fully-shipping mainstream interface.

What's *not* exposed:
- **No published Renishaw SDK.** No documented public REST API for arbitrary clients. Power BI is the example BI integration; that's it as far as developer-facing surface goes.

**Pricing**: not public. Renishaw quotes per project.

### Renishaw Central caveats

- It's a separate purchase. The MTConnect 1.4 output requires Central, not just Equator.
- The on-premises stance is great philosophically but means you host it. Adds an additional Windows / Linux server to the shop's infra inventory.
- MTConnect output is read-only — you can consume measurement events, but to send work-order context *into* the gauge, the path is MODUS Equator's program selection (Organiser), not Central.

## Adapter shape for ABERP

```
+---------------+     +-----------------+     +----------+     +---------+
| Equator       | --> | Renishaw Central| --> | ABERP    | --> | audit   |
| (or QIF drop) |     | (MTConnect 1.4) |     | adapter  |     | ledger  |
+---------------+     +-----------------+     +----------+     +---------+
                                                 |
                                                 v
                                          QualityResultReceived
                                          (per characteristic, with
                                          measured value + tolerance
                                          + pass/fail in code)
```

The canonical event type:

- `QualityResultReceived { part_id, characteristic_id, nominal, tol_minus, tol_plus, measured, passed, source: "renishaw-equator-1" }`
- Decision modeling: tolerance comes from the QIF file (Renishaw side, which got it from the operator's CAD-driven inspection program). ABERP doesn't redo the CAD tolerance math — it consumes what Equator emits and audits it.

### Edge cases the adapter has to model

- **Probe fault** (stylus broken, calibration drift detected) — emit `QualityProbeFaulted` event; quarantine downstream parts until reset.
- **Partial measurement** — Equator wrote out N of M characteristics. Emit `QualityResultPartial`; flag in UI; operator decides retry vs accept.
- **Operator override** — the operator pressed "force pass." Audit who-when-why; never silently accept. Hülye-biztos applies harder on the QC gate than anywhere.
- **No reference part for the day** — Compare mode requires a daily-calibration master part. If that calibration didn't run, every measurement is suspect; mark with calibration-stale flag.
- **Network drop** between Equator and Central — Central buffers locally, syncs when back. ABERP needs to tolerate burst syncs without double-counting (idempotency on result UUID).

## What about non-Renishaw measurement?

The QIF-and-MTConnect path is **vendor-neutral**. If Áben ever swaps to a Zeiss CMM or a Hexagon Tigo, the same QIF asset type and `QualityResultReceived` event work without code changes — the adapter wraps the new vendor's output instead. This is the adapter pattern's whole point.

## Recommendation framework

**For Phase ζ (Renishaw adapter + QC gate workflow)**:

1. **Prefer Renishaw Central → MTConnect 1.4**[^central-datasheet] as the primary integration path. ABERP consumes MTConnect's asset endpoint where QIF lives.
2. **Fall back to QIF XML file-drop** if Renishaw Central isn't bought (license cost reality may push us here). Direct from MODUS Equator's export step; ABERP watches a folder.
3. **Last resort: CSV file-drop.** Less structured, more parser fragility, but workable for v1 if Áben skips QIF authoring at MODUS-program-time.
4. **Avoid building against the SQL Server output.** It's an integration point but couples ABERP to Renishaw's schema; would be fragile across Renishaw upgrades.
5. **Don't try to integrate Inspection Plus directly** for category (a) probe data — leave that to the CNC's macro layer. If we want on-machine probe results visible in ABERP, plumb them via the CNC's MTConnect/OPC-UA feed.
6. **Watch umati / OPC-UA support** as it matures. By the time Phase ζ ships (2027+), umati on Renishaw Central may be production-ready and is the better long-term path than MTConnect for write-back integration.

## What's still unknown

- Renishaw Central licensing cost — not public. Need a Renishaw quote.
- Whether the DMG IoTconnector's QIF asset support is plumbed end-to-end into Renishaw Central, or whether that's an integration gap ABERP fills.
- Renishaw Central's exact MTConnect 1.4 schema coverage — which asset / event subset is implemented. Only knowable post-install.
- umati v1.02 Job Management compatibility with Renishaw's current Central version — emerging, not yet documented.

## Citations

[^omp60]: Renishaw OMP60 (illustrative listing). https://www.penntoolco.com/renishaw-machine-tool-probe-omp60-optical-optical-legacy-a-4038-0001/ — fetched 2026-06-02.
[^rmp60]: Renishaw, "RMP60 radio transmission probe." https://www.renishaw.com/en/rmp60-radio-transmission-probe--19257 — fetched 2026-06-02.
[^mp250]: Renishaw, "Probing systems for CNC machine tools" (PDF catalogue). https://s3.amazonaws.com/www.motionusa.com/renishaw/IndustrialMetrology/Probing_Systems_for_CNC_Machine_Tools.pdf — fetched 2026-06-02.
[^nc4]: Renishaw, "NC4 fixed non-contact tool setter." https://www.renishaw.com/en/nc4-fixed-non-contact-tool-setting-probe--15147 — fetched 2026-06-02.
[^inspection-plus-fagor]: Fagor Automation, "Renishaw Inspection Plus adaptation." http://www.fagorautomation.com/en/renishaw-inspection-plus-2/ — fetched 2026-06-02.
[^inspection-plus-haas]: Renishaw, "Inspection Plus macro software" (Haas programming manual PDF). https://www.haascnc.com/content/dam/haascnc/en/service/reference/probe/renishaw-inspection-plus-programming-manual---2008.pdf — fetched 2026-06-02.
[^equator-x]: Renishaw, "Equator-X 500 dual-method gauging." https://www.renishaw.com/en/equator-x-dual-method-gauge-for-shop-floor-inspection--49752 — fetched 2026-06-02.
[^modus]: Renishaw, "MODUS software for Equator." https://www.renishaw.com/en/modus-gauging-software--32498 — fetched 2026-06-02.
[^organiser]: Renishaw, "Equator gauging software." https://www.renishaw.com/en/equator-gauging-software--14619 — fetched 2026-06-02.
[^ipc]: Renishaw, "IPC intelligent process control." https://www.renishaw.com/en/ipc-intelligent-process-control--32497 — fetched 2026-06-02. Press release: https://www.renishaw.com/en/41132.aspx
[^central]: Renishaw, "Smart manufacturing data platform (Renishaw Central)." https://www.renishaw.com/en/smart-manufacturing-data-platform-for-industrial-process-control--47853 — fetched 2026-06-02.
[^central-datasheet]: Renishaw Central data sheet. https://www.renishaw.com/resourcecentre/download/data-sheet-renishaw-central--135134 — fetched 2026-06-02.
[^central-3p]: Renishaw Central explicitly supports third-party kit; positioned as a "smart manufacturing data platform" not a Renishaw-only platform.
[^renishaw-umati]: Machinery.co.uk, "Renishaw joins the umati community." https://www.machinery.co.uk/content/news/renishaw-joins-the-umati-community — fetched 2026-06-02.
