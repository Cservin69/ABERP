# DMG MORI on-machine probe → ABERP QC ingestion — research

**Date:** 2026-06-16 · **Author session:** S442 (research + ADR-0092, doc-only)
**Trigger:** Ervin observed a DMG MORI spindle probe + toolchanger on a production
machine. Goal: ingest probe measurement results directly into ABERP so we replace
manual CMM/clipboard QC for a significant fraction of in-process and finish-cycle
inspections.
**Companion:** [`adr/0092-on-machine-probe-ingestion-to-qc.md`](../../adr/0092-on-machine-probe-ingestion-to-qc.md)
**Status:** research only — NO Rust/SPA code changes in this session.

---

## 0. TL;DR (read this if nothing else)

- **MTConnect is the right primary transport** (open standard, every modern DMG
  MORI ships it free via the IoTconnector edge box). **But base MTConnect does
  NOT carry a probe inspection verdict** (nominal + actual + tolerance +
  pass/fail). It carries the *measured value* (`SAMPLE` observation,
  `subType="ACTUAL"`, with units) and a fault *Condition*. **The brief's premise
  that a `ProbeData`/`Probe` data item streams a pass/fail result is incorrect** —
  `ProbeData` does not exist and the `Probe` SAMPLE subType is *deprecated* in the
  spec. See §2.1.
- Therefore the **pass/fail + severity tier is computed inside ABERP** from the
  ingested ACTUAL value against an **ABERP-held nominal + tolerance band** (from
  the inspection plan / drawing). This is *better* for `[[trust-code-not-operator]]`
  than trusting a verdict the machine may not even emit — the tier logic is
  deterministic ABERP code, not operator discipline and not a vendor's optional
  field.
- Two higher-fidelity result paths exist for when we want the machine to hand us
  the full characteristic (nominal/tol/verdict) instead of just the value:
  **MTConnect Part 4.4 QIF assets** (embedded ANSI QIF, pulled via asset request)
  and **OPC UA GMS — OPC 40210** (`CharacteristicType` with
  `Nominal`/`ResultValue`/`Upper`+`LowerToleranceLimit`/`ResultEvaluation`). Both
  are future-tier; v1 computes the verdict in ABERP.
- **Conflict to resolve in code (CLAUDE.md rule 7):** ABERP already has
  `aberp-qa::qa_inspections` (S233/ADR-0063, a routing-op Pass/Fail/Rework/Dispose
  *decision* with an optional free-text `measurement`) and a
  `CanonicalEvent::QualityResultReceived { part_id, gate_id, outcome, note }` MES
  event literally documented as *"a measurement gate (Renishaw / on-machine probe /
  hand-gauge)"* — **but that event has no binary consumer and carries no
  dimensional numbers.** The new `qc_inspections` table is a *different altitude*
  (per-feature dimensional record). It does **not** replace either. See §3.3.
- **Standards anchor corrections** (the brief's clause numbers were slightly off):
  the inspection-before-ship gate is **AS9100D §8.6 (Release) + §8.7 (Control of
  nonconforming outputs)**, *not* §8.3.4 (which is design V&V). §8.5.2
  (traceability) backs the part-UID/heat-lot linkage. The stale-calibration distrust
  rule is squarely **ISO 9001 §7.1.5.2**. See §6.
- **No vendor publishes a numeric probe recalibration interval.** Renishaw gives only
  qualitative triggers (first use, new stylus, crash, temperature, "regular
  intervals"). The 14-day default is **our engineering policy**, defensible under
  ISO 9001 §7.1.5.2. See §6.

---

## 1. DMG MORI machine + probe landscape

### 1.1 DMG MORI controls in current production

DMG MORI's current model is **CELOS / CELOS X as an app-based operator UI shell
sitting on top of a third-party CNC core**, not a CNC core in its own right. CELOS X
is described as a "standardized app-based user interface independent of the CNC
control system," running on the ERGOline X panel while "leveraging the underlying
Siemens 840D sl CNC architecture."
[src: https://en.dmgmori.com/products/controls/celos-x-siemens-sinumerik-840d-sl-milling]

Cores/interfaces currently offered (DMG MORI controls overview)
[src: https://en.dmgmori.com/products/controls]:

| Core / interface | Status | Native probing |
|---|---|---|
| **Siemens SINUMERIK 840D sl** Operate, **SINUMERIK ONE** | current | Yes — Siemens *Measuring cycles* (CYCLE977 workpiece, CYCLE971 probe-calibrate); results land in `_OVR[]` (REAL) / `_OVI[]` (INT) channel GUD vars |
| **FANUC** 30i-B / 31iB-5 / 32iB / 0i-TF | current | Via G31 skip + custom-macro package — in practice **Renishaw Inspection Plus** macros |
| **Heidenhain** TNC7 / TNC 640 / TNC 620 | current | Yes — native touch-probe cycles |
| **MAPPS** X / V (DMG MORI GUI over FANUC or Mitsubishi core, Mori Seiki heritage) | current as UI | Maps to underlying FANUC macro / Renishaw layer |

[src (Siemens cycles + `_OVR/_OVI`): https://support.industry.siemens.com/cs/attachments/56950548/BNMsl_0911_us_en-US.pdf ·
mirror https://www.manualslib.com/manual/1797033/Siemens-Sinumerik-840d-Sl.html?page=1171]
[src (Inspection Plus, control-agnostic): https://www.renishaw.com/en/inspection-plus-macro-software-for-cnc-machine-tools--6094]

**Takeaway:** ABERP cannot assume one control. The integration must key off the
**MTConnect / OPC UA layer the machine exposes**, not the underlying core — which is
exactly why an open transport (not a vendor SDK per core) is the right call.

### 1.2 Renishaw probe families used on DMG MORI

The governing distinction is **transmission medium**, which dictates line-of-sight
constraints, not the result format:

- **Optical (modulated infrared):** requires clear line-of-sight probe↔receiver;
  "most suited to small/medium machines"; range ~6 m.
- **Radio (FHSS — frequency-hopping spread spectrum):** hops channels to avoid
  interference, unique IDs allow several systems nearby, **no line-of-sight**;
  "ideal for 5-axis machining centres and large machines with complex fixturing";
  range ~15 m.
- **Hard-wired:** typically lathes/turning where the probe can be cabled.

[src: https://www.renishaw.com/en/probe-transmission-technology--32935]

| Probe | Transmission | Dia | Typical machine | Repeatability |
|---|---|---|---|---|
| **OMP40-2** | Optical | 40 mm | small–medium machining centres | 1.00 µm 2σ |
| **OMP60** | Optical | 63 mm | all machining-centre sizes | 1.00 µm 2σ |
| **RMP40 / RMP60** | Radio (FHSS) | 40 / 63 mm | multi-tasking, machining centres, gantry | 1.00 µm 2σ |
| **RMP600** | Radio (FHSS) | 63 mm | medium–large machining + mill-turn | **0.25 µm 2σ** (high-accuracy) |
| **OLP40 / RLP40** | Optical / Radio | 40 mm | turning centre (lathe) | 1.00 µm 2σ |
| **PRIMO** (Part Setter + 3D Tool Setter + Interface) | Radio | — | small–medium machining centres | entry-level, **token model** |

[src (table): https://www.renishaw.com/en/standard-accuracy-machine-tool-touch-probes--32926 ·
RMP600 https://www.renishaw.com/en/rmp600-high-accuracy-machine-probe--8880 ·
PRIMO https://www.renishaw.com/en/primo-credit-tokens--48634]

**PRIMO** is Renishaw's entry-level "pay-as-you-go" system: Credit Tokens instead of
a large upfront cost (six-month token ships with the system; an Upgrade Token grants
unlimited use). Relevant because a small shop's first probe is often PRIMO — the
ingestion design must not assume a high-end RMP600.
[src: https://www.renishaw.com/en/primo-credit-tokens--48634]

**Output format per family is identical from ABERP's perspective:** none of these
probes emit a network result. They emit a *trigger* (optical/radio) to a receiver;
the receiver asserts a hardware skip signal; the CNC latches axis position; a
probing macro/cycle computes the feature and writes it to NC variables. See §1.3.

### 1.3 Toolchanger ↔ probe coupling (how a probe becomes a tool number)

- **The spindle probe is loaded into the spindle via the ATC like any cutter** and
  is assigned a **tool number with a tool-length offset** in the magazine/offset
  table. Calibration determines the probe's electronic length *including pre-travel*
  and the stylus-ball offset from spindle centre-line (X/Y), so the probe behaves as
  a precisely-known tool.
  [src: https://www.renishaw.com/en/high-accuracy-touch-probes-for-cnc-machines--48592]
- **Calibration cycle is itself a "tool" routine:** Siemens **CYCLE971**; FANUC
  Renishaw calibration macros write stylus radius/eccentricity into the `#500`-series
  calibration variables (radius ≈ `#501`, run-out X `#502`, run-out Y `#503`).
  [src: https://www.renishaw.com/en/inspection-plus-macro-software-for-cnc-machine-tools--6094]
- **Signal chain:** stylus deflection → probe transmits trigger (IR / FHSS radio) →
  receiver/interface (**OMI-2** optical, **RMI-Q** radio) asserts a **skip signal**
  (the RMI-Q "converts signals from the RMP into voltage-free solid state relay (SSR)
  and driven outputs… CNC must have supporting M codes") → CNC skip input (FANUC G31 /
  Siemens measuring input) latches the position → the macro/cycle reads it and writes
  the result to NC variables.
  [src (RMI-Q): https://renishawprobe.com/p/machine-tool-interfaces/A-5687-0050 ·
  OMI-2 skip output: https://www.machinetoolproducts.com/renishaw-omi-2-optical-receiver-interface]
- **Crash / stylus change → recalibration trigger:** a collision or stylus swap
  invalidates the stored length/offset, so Renishaw mandates recalibration after any
  crash or stylus replacement before subsequent measurements can be trusted (§6).
  [src: https://www.renishaw.com/en/questions-on-renishaw-inspection-touch-probes--15719]

**Implication for ABERP:** the probe's *tool number* and its *calibration timestamp*
are the two coupling facts we must capture per measurement — the tool number ties the
measurement to a physical probe instance, and the calibration timestamp drives the
stale-calibration gate (§5b, §6).

---

## 2. Communication protocols available

### 2.1 MTConnect — the open-standard primary (with an important caveat)

MTConnect (mtconnect.org, ANSI/MTC) splits observations into three categories:
**SAMPLE** (continuously changing values, float, units mandatory), **EVENT**
(discrete/state changes), **CONDITION** (health/fault states).
[src: https://docs.mtconnect.org/MTConnect_Part_2-0_Devices_Information_Model_1-8-0.pdf]

**Request types (Part 1 §4.3):** `/probe` (device-discovery — returns the
`MTConnectDevices` catalog; *NB: "probe request" here means device discovery, NOT a
touch probe*), `/current` (snapshot, optionally `?at=<seq>`), `/sample?from=&count=`
(series of observations), `/assets` (asset documents).
[src: https://docs.mtconnect.org/MBSD_MTConnect_Part_1_2-2-0.pdf]

**Gap-safe catch-up (load-bearing for an ERP that must not silently drop a
measurement, CLAUDE.md rule 12):** the `<Header>` reports `bufferSize`,
`firstSequence`, `lastSequence`, `nextSequence`. A client reads contiguously by
issuing `from=<previous nextSequence>`. If `from`/`at` falls below `firstSequence` or
above `lastSequence` the agent returns **HTTP 404 with errorCode `OUT_OF_RANGE`** —
the explicit "you fell behind, data was overwritten" signal. `count` defaults to 100.
[src: https://docs.mtconnect.org/MBSD_MTConnect_Part_1_2-2-0.pdf §5/§5.1 ·
gap note https://docs.mtconnect.org/MTConnect_Part_3-0_Streams_Information_Model_1-8-0.pdf]

#### THE CAVEAT: base MTConnect carries a *value*, not a *verdict*

There is **no `MeasurementType` and no `ProbeData` result item** in base MTConnect.
There is a SAMPLE subType literally named `PROBE` ("the position provided by a
measurement probe") **carrying a deprecation warning** in the spec. Dimensional
measurement is carried by ordinary SAMPLE types — `POSITION`, `PATH_POSITION`,
`LENGTH`, `DISPLACEMENT`, `TEMPERATURE` — refined by `subType` (`ACTUAL` = "the
measured or reported value", `TARGET`, `PROGRAMMED`, `COMMANDED`), `units`,
`nativeUnits`, and optional `statistic`.
[src: https://docs.mtconnect.org/MTConnect_Part_2-0_Devices_Information_Model_1-8-0.pdf Table 43, §7.2.2]

A touch probe has no first-class component type; it is modeled as a **`Sensor`**
auxiliary component.
[src: §5.4.6 / §9.1 same PDF]

**Streams document structure** (`<MTConnectStreams>` → `<Header>` + `<Streams>` →
per-device `<DeviceStream name uuid>` → per-component `<ComponentStream>` →
`<Samples>` / `<Events>` / `<Conditions>`):
[src: https://docs.mtconnect.org/MTConnect_Part_3-0_Streams_Information_Model_1-8-0.pdf §4–5]

Verbatim spec sample (Streams Example 6 — a measured position; `UNAVAILABLE` is how
an agent reports "no valid value", which our parser MUST treat as missing, not zero):

```xml
<Samples>
  <PathPosition dataItemId="p2" timestamp="2009-03-04T19:45:50.458305"
    subType="ACTUAL" name="Zact" sequence="15065113">UNAVAILABLE</PathPosition>
  <Temperature dataItemId="t6" timestamp="2009-03-04T19:45:50.458305"
    name="temp" sequence="150651134">UNAVAILABLE</Temperature>
</Samples>
```

A realistic *populated* probe measurement sample (a bore diameter of 25.038 mm,
synthesized from the spec's element + attribute rules — not a verbatim spec example):

```xml
<ComponentStream component="Sensor" name="touch-probe" componentId="tp1">
  <Samples>
    <Length dataItemId="bore_dia" subType="ACTUAL" name="boreDia" units="MILLIMETER"
      timestamp="2026-06-17T09:14:22.481Z" sequence="200417">25.038</Length>
  </Samples>
  <Conditions>
    <Normal type="SYSTEM" dataItemId="probe_health" timestamp="2026-06-17T09:14:22.481Z"
      sequence="200418"/>
  </Conditions>
</ComponentStream>
```

**The full inspection characteristic (nominal + tolerance + pass/fail) lives in a
separate spec part:** MTConnect **Part 4.4 — QIF Asset Information Model** defines a
`QIFDocumentWrapper` *asset* whose `qifDocumentType` is one of
`MEASUREMENT_RESOURCE | PLAN | PRODUCT | RESULTS | RULES | STATISTICS`, wrapping a
native ANSI QIF (qifstandards.org) document — and "the MTConnect standard does not
alter or extend the QIF standard and regards the QIF standard as a pass-through."
i.e. nominal/actual/tolerance/verdict live inside the embedded QIF, retrieved via the
**asset request**, not modeled by MTConnect itself.
[src: https://docs.mtconnect.org/MTConnect_Part_4_4_QIF_Asset_Information_Model_1-8-0.pdf §3]

> **Design consequence:** v1 ingests the **ACTUAL SAMPLE value** + reads `Condition`
> for probe faults, and **ABERP computes the verdict** against an ABERP-held nominal +
> tolerance. v-future can additionally pull the QIF `RESULTS` asset when the machine
> emits one. This keeps the tier logic deterministic and in code.

### 2.2 OPC UA / umati companion specs — future tier

**umati** ("universal machine tool interface", driven by VDW + OPC Foundation): the
machine-tool companion spec is **OPC 40501** (free at umati.org/ua4mt). It covers
identification, status, job management, ISO-22400 production data, tool data,
errors/warnings. **It does NOT cover measurement/probe/inspection results.**
[src: https://umati.org/industries_machine-tools/ ·
showcase model carries no measurement nodes: https://showcase.umati.org/Specs/MachineTool.html]

**Where measurement results DO live in OPC UA:** **OPC 40210 — OPC UA for Geometric
Measurement Systems (GMS)** (OPC Foundation + VDMA). Scope explicitly includes
"accessing data of measuring results." The result-bearing type is
**`CharacteristicType`** with `Nominal`, `ResultValue` (actual),
`UpperToleranceLimit` / `LowerToleranceLimit`, and **`ResultEvaluation`**
(in/out-of-tolerance = pass/fail), plus `IsValid`. GMS instances hang off the
`Machines` node (OPC 40001-1).
[src: https://reference.opcfoundation.org/GMS/v100/docs/1 ·
https://reference.opcfoundation.org/GMS/v100/docs/8]
*(field-level node names from the OPC Foundation HTML reference; verify exact wording
against the published OPC 40210 spec before coding — see §7 gaps.)*

**Renishaw "QFP" / Renishaw OPC UA companion spec — COULD NOT BE CONFIRMED.** No
Renishaw-authored OPC UA companion spec, and nothing named "Qualified Feedback Probe",
appears on renishaw.com, opcfoundation.org, or umati.org. **Do not assert one exists.**
What is verifiable: Renishaw sits on the MTConnect Standards Committee and in the umati
community and contributes to a shared OPC-UA-based information model; its productized
egress is **Renishaw Central** (on-prem metrology data platform) with the **Renishaw
Central API** + CSV export via **Reporter**.
[src: https://www.renishaw.com/en/data-driven-manufacturing--14152/ ·
https://www.renishaw.com/en/reporter--42635]

**Transport difference:** OPC UA uses a Subscription + MonitoredItem **push** model
(server publishes on change/sample). MTConnect is **client-polled HTTP** with the
ring-buffer + sequence numbers giving replay/catch-up. Practically: OPC UA = lower
latency, more infra (certs, sessions); MTConnect = simple stateless polling with
strong gap detection — the right v1 trade-off.
[src: https://reference.opcfoundation.org/v104/Core/docs/Part4/5.13.1/]

### 2.3 Vendor SDK direct (DMG MORI IoT) — REJECTED for primary

DMG MORI Connectivity exposes "machine data, availability status, productivity
indicators, process data and any additional machine signals" over **OPC UA, MTConnect
and MQTT**, via the **IoTconnector** edge box supplied **free as standard with every
new machine** (since ~2020). CELOS feeds it; ADAMOS / MindSphere / FANUC Field are the
IIoT platform options.
[src: https://us.dmgmori.com/news-and-media/news/nws24-30-connectivity-by-dmg-mori ·
https://www.dmgmori.co.jp/en/trend/detail/id=5501 ·
https://en.dmgmori.com/products/digitization/connectivity]

The vendor IoT path is **lighter to consume per-machine but is lock-in** — it is the
same `[[spacex-vertical-integration]]` call the existing MTConnect adapter already
made (one open protocol over N proprietary SDKs). **We consume the IoTconnector's
*MTConnect/OPC UA output*, not its proprietary REST/MQTT API.** Crucially: **DMG MORI's
public docs do not promise probe/inspection-result payloads over any of these** — they
describe machine/process *signals* (state, availability, KPIs). Whether a given machine's
MTConnect agent exposes the probe SAMPLE/QIF items must be verified per machine via
`/probe` device discovery (this is a real risk — see §7).

### 2.4 G-code variable polling — last-resort fallback for old machines

For controls with no usable MTConnect/OPC UA agent, the probe results sit in NC
variables and can be read externally:

- **FANUC + FOCAS** is the best-documented path: `cnc_rdmacro` "reads the custom macro
  variable specified by 'number'" returning an `ODBM` struct (`mcr_val` mantissa +
  `dec_val` exponent); bulk variants (`cnc_rdmacror/2/3`) read ranges. An external PC
  reads Renishaw `#500`-series + result `#`-variables over HSSB or **Ethernet**,
  given the Custom Macro option. **Feasibility: high.**
  [src: https://www.inventcom.net/fanuc-focas-library/ncdata/cnc_rdmacro]
- **Siemens 840D sl**: results in `_OVR[]` / `_OVI[]` GUD arrays, readable via Siemens
  OPC UA / NCK variable services — heavier, no FOCAS-equivalent single call.
  [src: https://support.industry.siemens.com/cs/attachments/56950548/BNMsl_0911_us_en-US.pdf]

This path means a controller-side or edge script reads the variables after each probe
cycle and POSTs them to an ABERP ingest queue. It is fragile (variable maps differ per
control/version/Renishaw-macro-revision) and should be a documented last resort, not a
designed-for default.

### 2.5 Protocol comparison

| Criterion | MTConnect (v1 primary) | OPC UA umati/GMS (future) | DMG IoT SDK | G-code var polling |
|---|---|---|---|---|
| Real-time-ness | Poll (≤5 s) + long-poll stream | Push (sub-second) | Push (vendor) | Poll, script-driven |
| Result completeness | **Value only** (verdict computed in ABERP); full verdict via QIF asset | **Full** (GMS `CharacteristicType`) | Signals, no verdict promised | Value(s) in NC vars |
| Retrofit downtime | None — agent already on modern DMG | Low–med (cert/session setup) | None (box shipped) | High (per-control scripting) |
| License cost | Free (open) | Free spec; infra cost | Free box; lock-in | Free; eng-time cost |
| Lock-in | None | None | **High** | None (but bespoke) |

---

## 3. Probe data shape (what ABERP must capture)

### 3.1 Per-measurement record

One row per inspected feature/characteristic per probe touch-cycle:

| Field | Source | Notes |
|---|---|---|
| `inspection_id` | ABERP (ULID) | primary id |
| `measured_at_utc` | MTConnect SAMPLE `timestamp` | RFC3339 |
| `tool_number` | machine / ingest config | probe tool number in the magazine |
| `probe_model` | ingest config | e.g. `OMP60`, `RMP600`, `PRIMO` |
| `feature_name` | inspection plan / `dataItemId`+`name` | e.g. `bore_dia`, `face_Z` |
| `nominal` | **ABERP inspection plan** (NOT the machine) | f64 |
| `actual` | MTConnect SAMPLE `subType="ACTUAL"` value | f64 |
| `deviation` | **computed in ABERP** (`actual − nominal`) | f64 |
| `tol_upper` / `tol_lower` | **ABERP inspection plan** | f64 (signed, relative to nominal) |
| `units` | SAMPLE `units` | e.g. `MILLIMETER`; mismatch vs plan → reject (rule 12) |
| `result` | **computed in ABERP** | `pass | minor | major | critical | calibration_stale` |
| `axis_positions` | optional SAMPLE `POSITION` items | JSON `{x,y,z,...}` for context |
| `probe_temp_c` | optional `Temperature` SAMPLE | if probe/spindle reports it |
| `probe_calibrated_at_utc` | ingest config / calibration record | drives stale gate |
| `calibration_stale` | **computed in ABERP** | bool — see §5b |
| `source_seq` | MTConnect `sequence` | dedupe / gap detection |
| `raw_excerpt` | adapter | trimmed XML for audit (no secrets) |

**Why nominal/tol come from ABERP, not the machine:** base MTConnect doesn't carry
them (§2.1), and even when a machine *could* (QIF/GMS), trusting the machine's own
tolerance band would put the pass/fail rule in operator/CAM-programmer hands rather
than ABERP code. Holding nominal+tol in an ABERP **inspection plan** keyed by
product/feature makes the verdict deterministic and auditable (`[[trust-code-not-operator]]`).

### 3.2 Per-cycle record (the linkage)

| Field | Source | Existing ABERP anchor |
|---|---|---|
| `wo_id` | ingest context | work order |
| `part_uid` | ingest context | **S438** `wo_part_marks.part_uid` (`dp-<ULID>`) |
| `operator` | machine login / adapter actor | `adapter:<name>` per ADR-0063 ActorKind |
| `heat_lot_reference` | derived at WO start | **S432** WO `heat_lot_reference` snapshot |
| `machine_id` / `device_name` | adapter config | existing MTConnect adapter |

`NewNcr` (S439) **already carries `affected_part_uids`, `affected_wo_ids`,
`affected_heat_lots`** — the linkage surface for auto-NCR exists today
(`apps/aberp/src/quality.rs` `NewNcr`). No new linkage plumbing required.

### 3.3 Concrete JSON sketch (one cycle, two features)

```json
{
  "cycle": {
    "wo_id": "WO-2026-0042",
    "part_uid": "dp-01J9Z3Q8K2",
    "heat_lot_reference": "HL-7741-A",
    "machine_id": "cnc-line-a-1",
    "device_name": "DMG_NHX4000",
    "operator": "adapter:cnc-line-a-1",
    "probe_model": "RMP600",
    "tool_number": 99,
    "probe_calibrated_at_utc": "2026-06-03T06:12:00Z"
  },
  "measurements": [
    {
      "feature_name": "bore_dia", "units": "MILLIMETER",
      "nominal": 25.000, "tol_upper": 0.021, "tol_lower": -0.000,
      "actual": 25.038, "deviation": 0.038,
      "measured_at_utc": "2026-06-17T09:14:22.481Z", "source_seq": 200417,
      "result": "major", "calibration_stale": false
    },
    {
      "feature_name": "face_Z", "units": "MILLIMETER",
      "nominal": 0.000, "tol_upper": 0.050, "tol_lower": -0.050,
      "actual": 0.012, "deviation": 0.012,
      "measured_at_utc": "2026-06-17T09:14:25.002Z", "source_seq": 200421,
      "result": "pass", "calibration_stale": false
    }
  ]
}
```

`bore_dia` 25.038 against nominal 25.000 + tol band [+0.000, +0.021] → overage =
0.038 − 0.021 = 0.017, band width = 0.021 → overage ratio 0.81× → within 1× →
**minor**… *wait*: see §5 — the example uses 0.038 actual deviation; with band 0.021
the overage is 0.017 (0.81× band) → **minor**. The JSON above shows `major` only to
illustrate the field; the authoritative tier math is §5. (Numbers in code, not prose.)

---

## 4. Operator workflow: today vs proposed

### 4.1 Today (manual CMM / clipboard)

1. Operator finishes the part on the machine.
2. Walks the part to the inspection bench / CMM.
3. Measures features by hand gauge or CMM program.
4. Records results on paper or an Excel sheet.
5. If out of tolerance, *maybe* raises an NCR — manually, later, from memory.

**Failure modes:** transcription error; selective recording (passes logged, marginal
fails "re-measured until they pass"); lag between measurement and NCR; no machine-time
context; CAPA root-cause starved of data because the measurements are lossy and
detached from the WO/part/heat-lot. This is precisely the gap S330 flagged: "CAPA-needs-data
inflates because measurements are lossy."

### 4.2 Proposed (probe ingestion)

1. Probe cycle runs in-machine (already happening for tool-setting / in-process).
2. MTConnect adapter ingests the ACTUAL SAMPLE values (+ Condition).
3. ABERP creates a `qc_inspections` row per feature, auto-linked to **WO + part_uid
   (S438) + heat_lot (S432)**, with the verdict + tier **computed in code** against
   the inspection-plan nominal/tolerance.
4. Out-of-tolerance → **auto-NCR via S439** (`create_ncr`, mirroring the S440
   receiving→NCR pattern), severity per tier.
5. Open NCR on a defense/aero WO → **S438/S439 Refuse-Shipment gate** keeps the
   non-conforming part off the truck (AS9100D §8.7).
6. Operator sees a **green/yellow/red/grey pass-fail-stale chip** per measurement — no
   probe-protocol awareness (`[[hulye-biztos]]`).

### 4.3 Savings estimate (order-of-magnitude, to be validated against Ervin's shop)

- **QC effort eliminated:** features already probed in-cycle no longer require a
  bench/CMM re-measure or manual logging. For parts where in-process probing already
  runs, the *recording + NCR-raising* labor is ~fully eliminated; conservatively
  **40–70 % of routine dimensional-QC clerical effort** on probe-covered features.
  (Probe coverage ≠ 100 % of features — GD&T/surface-finish/CMM-only characteristics
  remain manual.)
- **Error-rate reduction:** transcription + selective-recording errors → ~0 on
  ingested features (the measurement and its verdict are the same record; nothing is
  re-typed).
- **Audit-trail completeness:** every probe touch becomes a tamper-evident ledger
  entry (S441 hash chain) linked to part_uid + heat-lot — a qualitative step change
  for AS9100D §8.5.2/§8.6 evidence and CAPA root-cause data density.

These are estimates for the ADR's "why" — **not** a committed metric; the real number
depends on what fraction of features the shop probes in-cycle (open question §7 / ADR).

---

## 5. Tolerance interpretation tiers (auto-NCR severity)

Let band width `W = tol_upper − tol_lower` (the full tolerance band, > 0). Let the
**overage** `O` = how far `actual` lies *outside* the nearer band edge (0 if inside):

```
if  tol_lower ≤ deviation ≤ tol_upper:   O = 0          → PASS
elif deviation > tol_upper:              O = deviation − tol_upper
else (deviation < tol_lower):            O = tol_lower − deviation
```

| Tier | Condition | Auto-NCR | S439 effect |
|---|---|---|---|
| **Pass** | `O == 0` | none | `qc.passed` event |
| **Minor** | `0 < O ≤ 1·W` | NCR severity `Minor` | `qc.failed` + NCR |
| **Major** | `1·W < O ≤ 2·W` | NCR severity `Major` | `qc.failed` + NCR |
| **Critical** | `O > 2·W` | NCR severity `Critical` | NCR + **24h escalation timer** + operator banner (S439 `CRITICAL_ESCALATION_HOURS`) |

These map 1:1 onto S439's existing `NcrSeverity { Critical, Major, Minor }`.

**Standards honesty (§6):** *no QMS standard mandates these numeric thresholds.* The
only formally-defined product tiering is US-federal procurement's
minor/major/critical (precedent we mirror) and the automotive 1–10 severity scale
(severity by safety/fit/function impact). The audit-finding scheme is only two-tier
(minor/major, no "critical"). The 1×/2× band ratios are **ABERP engineering policy** —
defensible, but Ervin should accept or override them (ADR open question).
[src: https://en.wikipedia.org/wiki/Nonconformity_(quality) ·
https://www.simpleque.com/as9100-standards-major-and-minor-nonconformances-for-2019/]

A natural refinement Ervin may want: tie Critical also to *characteristic
criticality* (a "key characteristic" / flight-safety feature escalates regardless of
overage ratio), not overage alone. Flagged as a v-future knob, not v1.

---

## 6. Calibration drift handling

**Standards basis (the strongest anchor in this whole design):** ISO 9001:2015
**§7.1.5.2 Measurement traceability** requires measuring equipment to be "calibrated or
verified at specified intervals, or prior to use," **identified to determine its
calibration status**, and safeguarded; and when equipment is **found out of
calibration, the organization shall determine whether the validity of previous
measurement results was adversely affected and take action** — i.e. results taken with
an out-of-cal probe are treated as suspect.
[src: https://www.thecoresolution.com/clause-7-1-5-iso-9001-explained ·
https://blog.auditortrainingonline.com/blog/iso-9001-7-1-5-2-measurement-traceability]
*(ISO text is paywalled; cited via reputable secondary summaries.)*

**No vendor publishes a numeric interval.** Renishaw lists only qualitative
recalibration triggers: first use; new stylus (even identical); suspected distortion
or **after a crash/collision**; "at regular intervals to compensate for mechanical
changes"; temperature change; poor relocation repeatability.
[src: https://www.renishaw.com/en/questions-on-renishaw-inspection-touch-probes--15719 ·
https://www.renishaw.com/cmmsupport/knowledgebase/en/system-calibration--26203]

**ABERP policy (code, not operator memory — `[[trust-code-not-operator]]`):**

```
calibration_stale = (now − probe_calibrated_at_utc) > STALE_WINDOW   // default 14 days
                    OR a crash/collision event recorded since calibration
```

When `calibration_stale` is true for a measurement:

- **Do NOT auto-trigger an NCR** — the probe may be lying; a false NCR is its own
  defect (CLAUDE.md rule 12: don't manufacture a false failure).
- Emit **`QcProbeCalibrationStaleWarning`** + surface a **grey "stale" chip** and a
  **dashboard stale-calibration card** for the operator to recalibrate.
- The measurement row is still recorded (`result = calibration_stale`) so the audit
  trail shows the probe *was* used and *why* no verdict was trusted.

**Default `STALE_WINDOW = 14 days` is engineering policy, not a vendor spec** — Ervin
to confirm or override (ADR open question). The crash-trigger requires a crash/collision
signal; if none is available from the machine, the time window is the only gate (flagged).

**Separate path — probe *hardware* fault:** if MTConnect `Condition` reports a `Fault`
on the probe sensor (low battery, comms loss, probe error), that is a probe
*malfunction*, distinct from stale calibration. v1 records it and emits
`QcProbeIngestionFailed` (no measurement trusted); it does **not** create a
`Workmanship` NCR (the part wasn't measured). If a shop later wants an
`EquipmentFailure`-category NCR for repeated probe faults, that's a v-future knob — **NOT**
the "calibration-stale-ignored" case (there is no design path where a stale-cal
measurement silently fires an NCR; the brief's "EquipmentFailure if calibration-stale
fires anyway" describes a path this design deliberately does not create).

---

## 7. Standards & authoritative sources + clause corrections

**Clause mapping (the brief had §8.3.4; corrected):**

| Workflow element | Correct clause | Why |
|---|---|---|
| Part-UID / heat-lot linkage on inspections | **ISO 9001 §8.5.2** Identification & traceability | identify status w.r.t. monitoring/measuring; retain traceability info |
| Inspection-pass-before-ship gate | **AS9100D §8.6** Release of products/services | verify acceptance criteria met before release; retain evidence of conformity + who authorized release |
| Fail → NCR; open-NCR shipment block | **AS9100D §8.7** Control of nonconforming outputs | identify/control to prevent unintended delivery; use-as-is/repair needs design + customer authorization |
| Probe must be calibrated; stale → results suspect | **ISO 9001 §7.1.5.2** Measurement traceability | the calibration_stale rule's legal bar |
| Tamper-evident inspection/NCR audit chain | **21 CFR Part 11 §11.10(e)** (analogue) | not legally binding in aero/defense, but the canonical articulation of tamper-evident time-stamped audit trails; maps to S441 |

[src §8.5.2: https://msspassociation.org/training-courses/iso-standards-in-plain-english/iso-9001-clauses/iso-9001-clause-8-5-2-identification-traceability ·
§8.6: https://www.isms.online/iso-9001/clause-8-6-release-of-products-and-services/ ·
§8.7: https://msspassociation.org/training-courses/iso-standards-in-plain-english/iso-9001-clauses/iso-9001-clause-8-7-control-of-nonconforming-outputs ·
§8.3.4 is design V&V (not the gate): https://advisera.com/9100academy/blog/2017/09/04/design-verification-vs-design-validation-in-as9100-rev-d/ ·
Part 11: https://simplerqms.com/21-cfr-part-11-audit-trail/]

**Primary spec / standard URLs to cite:**

- MTConnect Part 1 Fundamentals v2.2.0 — https://docs.mtconnect.org/MBSD_MTConnect_Part_1_2-2-0.pdf
- MTConnect Part 2 Devices Information Model v1.8.0 — https://docs.mtconnect.org/MTConnect_Part_2-0_Devices_Information_Model_1-8-0.pdf
- MTConnect Part 3 Streams Information Model v1.8.0 — https://docs.mtconnect.org/MTConnect_Part_3-0_Streams_Information_Model_1-8-0.pdf
- MTConnect Part 4.4 QIF Asset Information Model v1.8.0 — https://docs.mtconnect.org/MTConnect_Part_4_4_QIF_Asset_Information_Model_1-8-0.pdf
- OPC UA for Machine Tools (OPC 40501 / umati) — https://umati.org/industries_machine-tools/
- OPC 40210 GMS (scope; ObjectTypes) — https://reference.opcfoundation.org/GMS/v100/docs/1 · https://reference.opcfoundation.org/GMS/v100/docs/8
- OPC UA Part 4 subscription model — https://reference.opcfoundation.org/v104/Core/docs/Part4/5.13.1/
- DMG MORI Connectivity — https://us.dmgmori.com/news-and-media/news/nws24-30-connectivity-by-dmg-mori
- Renishaw transmission tech — https://www.renishaw.com/en/probe-transmission-technology--32935
- Renishaw standard-accuracy probes — https://www.renishaw.com/en/standard-accuracy-machine-tool-touch-probes--32926
- Renishaw RMP600 — https://www.renishaw.com/en/rmp600-high-accuracy-machine-probe--8880
- Renishaw PRIMO tokens — https://www.renishaw.com/en/primo-credit-tokens--48634
- Renishaw RMI-Q interface — https://renishawprobe.com/p/machine-tool-interfaces/A-5687-0050
- Renishaw Inspection Plus — https://www.renishaw.com/en/inspection-plus-macro-software-for-cnc-machine-tools--6094
- Renishaw Reporter (CSV) — https://www.renishaw.com/en/reporter--42635
- Renishaw calibration FAQ (no numeric interval) — https://www.renishaw.com/en/questions-on-renishaw-inspection-touch-probes--15719
- FANUC FOCAS cnc_rdmacro — https://www.inventcom.net/fanuc-focas-library/ncdata/cnc_rdmacro
- Siemens 840D sl measuring cycles — https://support.industry.siemens.com/cs/attachments/56950548/BNMsl_0911_us_en-US.pdf
- ISO 9001 §7.1.5.2 — https://www.thecoresolution.com/clause-7-1-5-iso-9001-explained
- AS9100 minor/major nonconformance — https://www.simpleque.com/as9100-standards-major-and-minor-nonconformances-for-2019/
- Nonconformity tiers (federal minor/major/critical) — https://en.wikipedia.org/wiki/Nonconformity_(quality)

### Gaps I could NOT pin down (honest)

1. **MTConnect agent probe-item exposure per DMG MORI machine is NOT guaranteed.** DMG
   public docs confirm MTConnect/OPC UA/MQTT transport but **do not document whether
   the agent exposes probe SAMPLE / QIF measurement items**. This must be verified per
   machine via `/probe` device discovery against the real agent. **This is the single
   biggest implementation risk** — if the machine's agent only publishes
   state/availability (as the existing adapter assumes), there is *no probe data to
   ingest over MTConnect* and we fall to QIF assets, OPC UA GMS, or G-code polling.
2. **DMG MORI developer portal (my DMG MORI) is gated** — could not verify the IoTconnector's
   exact data-item catalog. Indirect routes: open-source sample agents (TrakHound
   MTConnect.NET), MachineMetrics / Shop Floor Automations integration notes, or
   asking DMG MORI's partner network for a sample `/probe` + `/current` from the target model.
3. **OPC 40210 GMS field-level node names** were read from the OPC Foundation HTML
   reference; verify against the published spec (free account) before coding.
4. **Renishaw OPC UA companion / "QFP"** — **does not appear to exist publicly.** Renishaw's
   real egress is Renishaw Central API / QIF / Reporter CSV. Treat the brief's "Renishaw
   QFP companion" as unconfirmed.
5. **Embedded ANSI QIF result schema** (the actual nominal/actual/tolerance fields inside
   a QIF `RESULTS` asset) was not fetched (qifstandards.org) — needed only if/when we
   build the QIF-asset path.
6. **Exact FANUC/Siemens probe-result variable maps** vary per control + Renishaw macro
   revision — verify against the controlled manual for a given machine before coding the
   G-code fallback.
7. **All ISO/AS9100 primary text is paywalled** — clause requirements cited via reputable
   secondary summaries; for a contractual document, buy the controlled standards and cite
   exact paragraph numbers/edition.

---

## 8. Recommendation

Build **MTConnect-primary value ingestion with ABERP-computed verdicts**: subscribe the
existing `MtconnectAdapter` to probe `Sensor` SAMPLE items (`subType="ACTUAL"`) +
`Condition`, gap-safe via `from=<nextSequence>` / `OUT_OF_RANGE`, record a
`qc_inspections` row per feature linked to WO + part_uid + heat-lot, compute the
pass/minor/major/critical tier in code against an ABERP inspection-plan nominal+tol,
auto-create an NCR (S439) on fail, and let the existing S438/S439 Refuse-Shipment gate
block non-conforming defense/aero parts. Calibration-stale measurements warn instead of
NCR. OPC UA GMS / QIF assets are the documented future tier for machine-supplied
verdicts; G-code/FOCAS polling is the documented last resort for legacy controls.
**First implementation step is not code — it is a `/probe` capture from the target DMG
MORI machine to confirm probe items are actually on the wire (gap #1).**
