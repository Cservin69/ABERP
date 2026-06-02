# 06 — MTConnect deep-dive

Prerequisite: [01-machine-protocols.md](01-machine-protocols.md). This file zooms into MTConnect specifically — the open standard that's the most likely backbone of ABERP's CNC integration, because the read-only telemetry surface is small enough that one Rust developer can own the entire adapter.

## Why MTConnect deserves its own file

OPC-UA covers more ground (control, robots, companion specs across industries), but it's also a bigger surface to implement correctly. **MTConnect is, in 2026, the option where ABERP can ship something useful in the smallest amount of Rust** — no library, no companion-spec generators, just HTTP and XML.

If Phase β starts with barcode/QR (cheapest hardware, immediate utility — see [04-barcode-qr-scanners.md](04-barcode-qr-scanners.md)) and Phase ε is the first DMG-Mori adapter, MTConnect is what Phase ε most likely targets first.

## The standard, in numbers

- **Governing body**: MTConnect Institute, a 501(c)(6) non-profit standards-development organization. ~400+ member companies. Originally championed by AMT (Association For Manufacturing Technology).[^mtc-about]
- **ANSI status**: published as **ANSI/MTC1**. Latest ANSI revision in print: ANSI/MTC1.4-2018; the standard has continued to evolve in version-2.x past the last ANSI bump.[^ansi-pdf]
- **Current spec version**: **2.5**, released **February 2025**.[^mtc-downloads]
- **SysML browser**: live model snapshots for V2.0, V2.1, V2.2, **V2.3**.[^mtc-model]
- **Licensing**: royalty-free, open-spec.
- **Reference implementation**: `mtconnect/cppagent` — modern C++, SHDR/JSON/MQTT input, MTConnect XML output, version-tracked to the standard, pre-built Linux+Docker binaries.[^mtc-cppagent]

## The 1.x → 2.x architectural shift

The headline change between 1.x and 2.x is **how the spec is authored**, not how it's consumed:

- **1.x**: distributed as text PDFs (Parts 1-5). Cross-document drift was a known frustration.
- **2.x**: the normative spec is a **SysML XMI model** (Cameo Enterprise Architect). PDFs + XSDs are auto-generated. The model is machine-readable, browsable in the V2.x SysML viewer, and integrated across sections.[^amt-2x]

For ABERP, the practical implication: when we want to know whether a DataItem exists in the vocabulary, we *don't* PDF-grep — we use the SysML browser. The XSDs we'd validate XML responses against are generated from the model. Vocabulary expansions in 2.x have added **additive manufacturing and robotic-integration** categories; for our use case (CNC + Renishaw + robot), 2.x is the right baseline.

## The agent–adapter architecture

```
+----------------+     +-------------+     +--------------+     +---------+
|   CNC machine  | --> |   adapter   | --> |    agent     | --> |  ABERP  |
| (FOCAS/SHDR/?) |     |             |     | (HTTP/XML)   |     | client  |
+----------------+     +-------------+     +--------------+     +---------+
       vendor             SHDR/JSON           MTConnect            our
      protocol            /MQTT in            REST out             code
```

- **Adapter** sits next to the machine, speaks the machine's native protocol (FOCAS for Fanuc, vendor-specific socket protocol for others, file-watching for legacy), and emits **SHDR** (Simple Hierarchical Data Representation — a compact line-based format), JSON, or MQTT *into* the agent.[^mtc-mtcup]
- **Agent** is the HTTP server. It accepts adapter input, maintains state, and serves the four MTConnect document types over REST.
- **ABERP** is the client. We GET XML, parse, translate to canonical event types, persist to audit-ledger.

**For Phase ε**, we run a stock `cppagent` (or one shipped with the machine — DMG IoTconnector includes one). We don't write an adapter or agent. We only write the client.

## The four documents

The agent exposes four document types via REST. Each `GET` returns XML.

### `GET /probe`

The **device probe**. Lists what the agent claims the machine can report: device tree, components (axes, spindles, controller path), available DataItems with their IDs, names, types, units, categories. This is the machine's self-description — read it once at adapter-init, cache, validate against ABERP's canonical schema.

Sample shape (illustrative, one line):

```xml
<DataItem id="Xpos" name="Xabs" category="SAMPLE" type="POSITION" subType="ACTUAL" units="MILLIMETER" />
```

The `/probe` response is also the **only published source-of-truth for per-machine coverage** — vendor websites say "Mazak supports MTConnect," but only the actual `/probe` from your actual machine tells you which DataItems are populated.

### `GET /current`

Latest snapshot of every DataItem. One value per item. Use for: "show me the live status now." Idempotent — call as often as needed; cheap.

### `GET /sample`

Sliding-window time series. Parameters: `from=<sequenceNumber>`, `count=<N>`, `interval=<ms>` (long-poll). This is the **streaming endpoint** — open a long-poll connection and consume events as they arrive. For OEE aggregation and audit-trail population, this is the workhorse.

### `GET /asset`

Durable, non-time-series records. Asset types in 1.x and continuing in 2.x:
- **Cutting tools** (Part 4.1) — tool number, life, geometry, wear state
- **Files** (Part 4.2) — NC programs, fixture offsets
- **Raw material** (Part 4.3) — stock barcode, dimensions, lot
- **QIF** (Part 4.4) — Quality Information Framework integration; the bridge to Renishaw-style measurement results

For Áben's shop, **cutting tools** and **raw material** are the assets that map to ABERP's existing inventory concepts. **QIF** is the formal hand-off from a measurement system (Renishaw) — see [02-renishaw-quality-gate.md](02-renishaw-quality-gate.md).

### `GET /error` shape

Malformed-request response. Documented for completeness; ABERP won't normally hit it once the client is correct.

## DataItem categories — the vocabulary in three buckets

| Category | What it carries | Examples | ABERP audit-ledger shape |
|---|---|---|---|
| **SAMPLE** | Continuous numeric | Axis position, spindle load, feedrate override, temperature | Stored as time series; aggregated for OEE; not every sample is an audit event |
| **EVENT** | Discrete state changes | `execution=READY/ACTIVE/STOPPED`, program name, controller mode, operator login | Each transition becomes a `MachineStateChanged` audit event |
| **CONDITION** | Fault/warning/normal-state per subsystem | Spindle overload, axis overtravel, coolant low | Each transition into "warning" or "fault" becomes a `MachineConditionRaised` audit event |

The mapping is: SAMPLEs feed the metrics pipeline (OEE — see [07-oee-mes-metrics.md](07-oee-mes-metrics.md)), EVENTs and CONDITIONs feed the audit ledger directly.

## Adoption — who supports it

Confirmed support on modern CNCs, per the MTConnect Institute supported-devices page and integrator KBs[^mtc-supported]:

- **Mazak** — early adopter, mature on machines from ~2012; the institute notes >200 factory-prepped machines.
- **Okuma** — plug-and-play via THINC-OSP (P100 Type II, P200/P200A, P300/P300S).
- **Haas** — supported when controller has Ethernet + firmware ≥ 16.05B (mills) / L09.06A (lathes).
- **DMG MORI** — via IoTconnector, on every new DMG (see [01-machine-protocols.md](01-machine-protocols.md)).
- **FANUC** — supported on Series 30i/31i/32i and newer.
- **Doosan / DN Solutions, Hurco, Makino, Hyundai-Wia** — broad adoption across major builders.

**What "supported" doesn't tell you**: the DataItem *coverage* per model and firmware varies. Two Mazaks of different vintage might publish very different `/probe` documents. Plan for a per-machine onboarding step at install.

## Rust client implementation — there is no crate

As of 2026-06-02, **no dedicated MTConnect crates exist on crates.io**.[^mtc-rust-gap] This is not a problem — it's actually a feature. The consumer surface is small enough that:

```
reqwest      — HTTP client (already in ABERP)
quick-xml    — streaming XML parser (well-maintained, zero-copy)
serde        — for XSD-derived types (optional; many adapters skip schema validation)
tokio        — async runtime (already in ABERP)
```

That's the whole stack for a Phase ε read-only client. No FFI, no MPL-2.0 compatibility check, no upstream maintainer to chase.

The .NET ecosystem has **TrakHound MTConnect.NET** — feature-complete C# library, covers ≤ 2.5, builds Agents + Adapters + Clients on Windows and Linux. Useful as a reference when implementing the client, even though we won't depend on it.

When ABERP eventually needs to *be* an agent (Phase ζ, when ABERP is the central point that aggregates multiple cells), the answer is probably: **don't rewrite cppagent in Rust**. Run cppagent in a sidecar container, have ABERP feed it via SHDR. The cppagent codebase has years of conformance work baked in; replicating that in Rust is not a good use of one developer's time.

## HU + EN translation surface

MTConnect's vocabulary is English XML. The DataItem `name` and `type` attributes are English keywords (`Xabs`, `POSITION`, `ACTUAL`, `EXECUTION`). The operator-facing UI is Hungarian-and-English bilingual per Áben's existing convention.

**The mapping happens in the adapter layer**, not in the UI:

```
MTConnect XML  →  canonical ABERP event type   →  i18n string in SPA
"EXECUTION"        MachineExecutionStateChanged   "Gép futás állapot változott" / "Machine execution state changed"
"ACTIVE"           ExecutionState::Active         "Aktív" / "Active"
"STOPPED"          ExecutionState::Stopped        "Leállítva" / "Stopped"
```

This is the same pattern ABERP already uses for NAV — NAV speaks Hungarian XML, the canonical event types in code are English, the UI re-renders bilingually. Reuse the pattern.

## Asset documents and traceability

The Asset endpoint is the underrated piece of MTConnect for a precision-machining shop. Two of the four asset types are directly relevant:

- **Raw material** — when stock is loaded, the operator scans its barcode (Phase β capability — [04-barcode-qr-scanners.md](04-barcode-qr-scanners.md)) and ABERP can register that material as an MTConnect asset on the machine's agent. The machine's `/probe` then includes the loaded material; the audit-ledger has the traceability primary key.
- **Cutting tools** — tool master records (geometry, life, wear) ride the asset endpoint. The DMG CELOS Tool Master app (and analogues on Mazak / Okuma) already do this; ABERP can consume it rather than re-implementing.

The **QIF** asset type is the bridge to Renishaw and other measurement systems. See [02-renishaw-quality-gate.md](02-renishaw-quality-gate.md) for the measurement-side concerns.

## Limitations worth naming

1. **No write-back vocabulary for full job dispatch.** MTConnect's history is read-only telemetry. 2.x has added some interfaces (Interfaces Part 4, MTConnect-Interfaces), but for "load this program and start running it" the practical answer is OPC 40501 (umati Job Management) or vendor-specific. Don't try to make MTConnect do bidirectional control in v1.
2. **Security isn't built in.** The agent is HTTP. If the shop floor and office aren't air-gapped, put a reverse proxy with TLS in front, or use OPC-UA where security is baked in.
3. **Sample frequency limits.** For sub-second axis-position tracking, the adapter-to-agent SHDR rate matters; not every adapter publishes at machine-native sample rates. Verify per machine.
4. **Vendor SHDR adapter quality varies.** Some adapters are excellent; others ship buggy. The cppagent reference is solid; vendor-supplied adapters less consistent.

## Recommendation framework

**For Phase ε (first DMG-Mori adapter)**:

1. **Use the agent the machine ships with** (DMG IoTconnector). Don't run our own agent.
2. **Write the Rust client only.** `reqwest` + `quick-xml` + `tokio`. No external MTConnect crate.
3. **Long-poll `/sample`** as the primary feed; cache the `/probe` once at session start.
4. **EVENTs and CONDITIONs → audit-ledger.** SAMPLEs → metrics pipeline. Don't audit every sample.
5. **Assets → inventory + traceability.** Tie raw-material asset records to the existing partner_id / product_id model where possible.
6. **Hold off on writing-back via MTConnect Interfaces** until at least two machines are integrated and the read-only pattern is stable. If write-back is needed sooner, route via OPC 40501 instead.

## What's still unknown (verify at Phase ε)

- DMG IoTconnector's MTConnect agent version (`cppagent`? vendor fork?) and `/probe` shape for the specific DMG model Áben buys.
- MTConnect 2.5 (Feb 2025) detailed change-log — what new DataItems exist that ABERP should map.
- Whether DMG's QIF asset support is plumbed end-to-end into Renishaw output, or whether that's an integration gap ABERP fills.
- Sample rate ceiling on DMG IoTconnector — is it adequate for OEE Performance calculation, which needs cycle-start/cycle-end precision?

## Citations

[^mtc-about]: MTConnect Institute, "About." https://www.mtconnect.org/about — fetched 2026-06-02.
[^ansi-pdf]: ANSI/MTC1.4-2018 PDF. https://mtconnect.squarespace.com/s/ANSI_MTC1_4-2018.pdf — fetched 2026-06-02.
[^mtc-downloads]: MTConnect Institute, "Standard Downloads." https://www.mtconnect.org/standard-download20181 — fetched 2026-06-02.
[^mtc-model]: MTConnect SysML model V2.3. https://model.mtconnect.org/Version2.3/ — fetched 2026-06-02.
[^mtc-cppagent]: `mtconnect/cppagent` reference implementation. https://github.com/mtconnect/cppagent — fetched 2026-06-02.
[^amt-2x]: AMTonline, "MTConnect Institute Releases Version 2.0." https://www.amtonline.org/article/mtconnect-institute-releases-version-2-0-of-the-mtconnect-standard — fetched 2026-06-02.
[^mtc-mtcup]: MTConnect Adapter Agent Protocol reference. https://mtcup.org/Protocol — fetched 2026-06-02.
[^mtc-supported]: MTConnect Institute, "Supported Devices." https://www.mtconnect.org/step-2-supported-devices — fetched 2026-06-02. Cross-referenced with https://www.machinemetrics.com/blog/what-is-mtconnect.
[^mtc-rust-gap]: crates.io search for "mtconnect" returns no maintained crates as of 2026-06-02.
