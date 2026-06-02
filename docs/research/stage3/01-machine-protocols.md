# 01 — CNC machine integration protocols

The question this file answers: when a DMG-Mori (or any CNC) lands in the shop, what does ABERP actually talk to? There are roughly four answers in 2026 — three real protocols and one fallback. They are not equal.

## TL;DR

- **MTConnect** and **OPC-UA** are both open, royalty-free, and supported by every modern DMG-Mori machine via DMG's "IoTconnector" package.[^dmg-conn]
- **CELOS / CELOS X** is DMG's UX-and-apps layer, not a wire protocol. You don't integrate "to CELOS"; you integrate via the IoTconnector that sits underneath it.
- **OPC-UA** has stronger security primitives and richer companion specifications (machine tools, robotics, machinery). **MTConnect** has a simpler client surface and a more mature reference ecosystem for read-only telemetry.
- **Fallback** (file-watching, RS-232 DNC) still happens in 2026 — used-machine market, locked-out licenses, air-gap policies. Don't design as if it doesn't exist.

## DMG MORI CELOS, CELOS X, IoTconnector

CELOS started as DMG's machine-level operator UI (apps, dashboards, tool management). The current generation is **CELOS X**[^dmg-celosx], packaged in two layers:

- **CELOS Xperience** — operator-facing apps at the machine.
- **CELOS Xchange** — cloud-side data + multi-machine control center.

CELOS X is **not a control** — it runs on top of Siemens, HEIDENHAIN, or DMG's own MAPPS control[^dmg-celosx]. So "CELOS-equipped" tells you about UX, not about wire format.

The wire format lives one layer down, in **DMG MORI Connectivity / IoTconnector**[^dmg-conn]. Three tiers:

| Tier | Scope | ABERP relevance |
|---|---|---|
| **IoTconnector** | Standard with every new DMG machine, 2-year subscription | The primary path for any newly-bought DMG |
| **IoTconnector retrofit** | Older DMG machines (~up to 10 years) | Path for used DMG fleet |
| **IoTconnector flex** | Third-party (non-DMG) machines | Lets ABERP also talk to a Mazak or Haas via DMG's adapter, but locks the gateway to DMG — adapter-pattern conflict, avoid in core path |

The IoTconnector exposes **OPC-UA, MTConnect, and MQTT** as standard interfaces[^dmg-conn] — verbatim from the product page: *"machine data, availability status, productivity indicators, process data and any additional machine signals can be made available to the user applications via the common standard protocols OPC-UA, MTconnect and MQTT."* The OPC-UA interface is included even on HEIDENHAIN-controlled DMG machines.

**Pricing**: not public. The Connectivity page links to "License-Model" and "Subscription-Model" PDFs but no euro amounts. The "my DMG MORI" portal does flexible technology-cycle activations but those are also quotation-only. Assume any cost conversation needs a DMG Service Sales Manager.[^dmg-conn]

**WERKBLiQ caveat**: DMG's old maintenance/ticketing platform was end-of-life'd December 2025[^werkbliq-eol]. Some legacy DMG documentation still mentions it. Do not target WERKBLiQ as an integration surface — it's gone.

### The CELOS lock-in argument

CELOS X *as an apps platform* is genuinely useful for operators standing at the machine: tool master, energy management (GREENMODE), dashboards. But every CELOS-X-only feature is something ABERP would have to either duplicate, ignore, or proxy. The integration-layer choice — IoTconnector's OPC-UA/MTConnect interface — is independent of whether the operator-facing app at the machine is CELOS or some replacement HMI. Keep them decoupled: ABERP talks to IoTconnector regardless of which UX-layer sits on top of it.

## MTConnect

Open, royalty-free, governed by the **MTConnect Institute** (501(c)(6) non-profit, ~400+ member companies)[^mtc-about]. Published as **ANSI/MTC1**. Current spec version: **2.5**, released February 2025[^mtc-downloads]; SysML model browser also exposes V2.0-V2.3 snapshots[^mtc-model].

**Architectural shape**: adapter → agent → client.
- The **adapter** runs near the machine, speaks the machine's native protocol (FOCAS, FANUC ethernet, vendor-specific), and emits SHDR (Simple Hierarchical Data Representation), JSON, or MQTT into the agent.[^mtc-mtcup]
- The **agent** is an HTTP server that serves four document types: `probe` (capabilities), `current` (latest snapshot), `sample` (sliding-window time series), `asset` (durable records: cutting tools, files, raw material).
- ABERP is the **client** — GETs XML from the agent's REST endpoints.

**Why we like it for v1**: the consumer surface is *small*. Four endpoints, XML in, our canonical event types out. **No Rust crate is needed** — `reqwest` + `quick-xml` suffices.[^mtc-rust-gap]

Reference implementation: **`mtconnect/cppagent`** on GitHub[^mtc-cppagent] — modern C++, supports SHDR/JSON/MQTT input, MTConnect XML output, version-tracked to the standard, pre-built binaries + Docker images. If ABERP ever needs to *be* an agent rather than just consume one, this is the starting point (likely shelled out, not Rust-rewritten).

**Vocabulary** — three DataItem categories:
- **SAMPLE** — continuous numeric (axis position, spindle load, feedrate override).
- **EVENT** — discrete state changes (`execution=READY|ACTIVE|STOPPED`, program name, controller mode).
- **CONDITION** — fault/warning/normal-state for a subsystem.

Deep-dive on the documents and adapter architecture: [06-mtconnect-deep-dive.md](06-mtconnect-deep-dive.md).

**Vendor adoption**: confirmed on Mazak, Okuma (THINC-OSP P100/P200/P300), Haas (with Ethernet + firmware ≥ 16.05B / L09.06A), DMG MORI (via IoTconnector), FANUC (30i/31i/32i and newer), Doosan / DN Solutions, Hurco, Makino, Hyundai-Wia[^mtc-supported]. **Vendor coverage matrix per DataItem** is *not* publicly published — only confirmable by GETting `/probe` from each machine after install.

## OPC-UA

**OPC Unified Architecture**, maintained by the **OPC Foundation**. Successor to the DCOM-bound "Classic OPC." Platform-independent, layered transport (binary over TCP, SOAP/HTTPS, MQTT pub-sub).

Two transport modes:
- **Client-Server** — sessions, subscriptions, monitored items, method calls.
- **Pub-Sub** — added later: UDP multicast and MQTT carriers; lower latency, scales to many subscribers.

### Companion specifications (where OPC-UA gets interesting)

Companion specs add domain vocabulary on top of base UA. The three relevant to ABERP:

| Spec | Scope | Status |
|---|---|---|
| **OPC 40001-1 — Machinery**[^opc-machinery] | Base model for all machinery; ISO 22400-aligned KPIs, machine identification, machinery state, job lists | Stable; the default when no industry-specific spec applies |
| **OPC 40501 — Machine Tools** (a.k.a. VDMA 40501, **umati**)[^umati] | Joint VDW + OPC Foundation; ~90 companies, ~200 participants. Standardises identification, status, job monitoring (parts counts, runtime), error/warning, KPIs, tool management | Free download via umati.org. v1.02 added bidirectional Job Management |
| **OPC 40010 — Robotics**[^opc-robotics] | VDMA 40010, joint VDMA Robotics + OPC Foundation; v1.02 adds program loading, system start/stop, state monitoring | Free; relevant for the planned robot dispatch — see [03-robot-controllers.md](03-robot-controllers.md) |

VDMA maintains ~40 working groups on UA companion specs covering >600 companies — companion-spec coverage is broad and growing.

### Security

UA security is real and certificate-based:
- **Encryption** — TLS for HTTPS transports; message-level signing + encryption on the native binary protocol.
- **Authentication** — X.509 client and server certs; trust list per endpoint.
- **Authorization** — UserIdentityTokens (anonymous, username/password, X.509, JWT).
- **Security profiles** — Basic256Sha256, Aes128Sha256RsaOaep, Aes256Sha256RsaPss. Basic128Rsa15 deprecated; don't enable.

This is materially stronger than MTConnect-over-plain-HTTP. If the shop floor and office networks aren't strictly air-gapped, OPC-UA's security model is load-bearing.

## OPC-UA vs MTConnect — overlap, not competition

The two standards used to be framed as rivals. **They aren't** in 2026:

- The **MTConnect OPC UA Companion Specification** (v2.00, June 2019)[^mtc-opcua] explicitly harmonises MTConnect's information model into the UA address space. Joint working group includes Siemens, FANUC, GE, Purdue.
- **OPC 40501 (umati)** overlaps MTConnect in the machine-tool domain — both define "spindle, axis, program, runtime, parts-count" semantics. MTConnect Institute has committed to translation tools.
- Most machine builders ship **both** interfaces. DMG MORI does[^dmg-conn]. Choosing between them is a *software*-side call, not a *machine*-side call.

| Dimension | MTConnect | OPC-UA |
|---|---|---|
| Wire format | XML over HTTP REST | Binary over TCP (or SOAP/HTTPS) |
| Vocabulary | Single integrated standard | Base + companion specs |
| Security | None native (rely on TLS at the agent reverse proxy) | Certificate-based; baked in |
| Client surface | Trivially small (4 endpoints) | Sessions + subscriptions + monitored items |
| Rust ecosystem | Roll-your-own (small surface) | `async-opcua` v0.18 (active, MPL-2.0) |
| Pub-sub support | Adapter-side via MQTT input | First-class PubSub (MQTT/UDP) |
| Job dispatch (write-back) | Newer 2.x parts add some; thin | Strong via OPC 40501 Job Management |
| Best for | Read-only telemetry, OEE feeds | Bidirectional control, secure networks, mixed vendor fleets |

## Fallback — when neither protocol is available

The reality in 2026: not every machine on the shop floor will speak MTConnect or OPC-UA, even if Áben's *new* DMGs will. Used machines, mid-range controllers with the modern interface locked behind a license, air-gapped cells — all real, and ABERP should not assume them away.

### File-watching (FTP / SMB)

Most common 2026 fallback. CNC mounts an SMB share or runs an FTP server; ABERP drops `.NC` / `.H` / `.MPF` files into a watched folder; operator picks them at the control. For traceability, a watcher reads the "executed-programs" log or a heartbeat file the control writes.

**Limitation**: one-way program transport, no event semantics, no live run-state. OK for "did the operator pick up the right program," not OK for OEE.

### Serial RS-232 / DNC drip-feed

Still active for legacy fleets — programs larger than control memory, or paper-tape-era controls. Software stacks: CIMCO DNC-Max[^cimco-dnc], Predator DNC[^predator-dnc], dnc4U, DNCdevice DNC TITAN. Protocol variants include standard XON/XOFF, Heidenhain blockwise, Haas Xmodem, Fadal Xmodem. Hardware: 9-pin RS-232, RS-422 for longer runs, BTR boards for the oldest Fanuc 5M/6M.

**Not ABERP's problem to reimplement**. If a legacy machine lands and needs DNC, license CIMCO or Predator and let ABERP own the *work order* level only — the DNC server handles wire-level. This is the cleanest separation.

### Older controllers — adapter exists but isn't free

- **Heidenhain TNC 4xx/5xx/iTNC 530** — pre-"Connected Machining"; LSV-2 binary over RS-232/Ethernet. Third-party tooling (TNCremo, TNCserver) covers this.
- **Fanuc 0i / 16i / 18i / 21i** — FOCAS library (proprietary Ethernet API), late-90s onwards. FOCAS licensing is paywalled and per-machine.
- **Siemens Sinumerik 840D powerline** — OPC Classic / DCOM era; modern 840D sl exposes OPC-UA, older powerline does not.

For these, "build an MTConnect adapter" usually means *license FOCAS or LSV-2 first*, then write the translation. The cost-benefit shifts; sometimes file-watching plus a barcode scanner is simply cheaper.

### Why fallback still exists in 2026

- Used-machine market: a 15-year-old Mori Seiki NMV5000 with MAPPS II won't have UA/MTConnect.
- Licensing inertia: Heidenhain "Connected Machining" was a per-machine charge — existing customers without it can't enable the interface without paying.
- Air-gap policies: aerospace / defence cells often disable network interfaces; USB or unidirectional gateway is the only option. Probably not Áben's concern, but worth knowing the pattern exists.

## Vendor-neutrality scorecard

| Option | Open standard? | Vendor lock? | Royalty? | If we pick this |
|---|---|---|---|---|
| MTConnect | ✅ ANSI/MTC1 | None | None | First-class telemetry, low integration cost |
| OPC-UA | ✅ IEC 62541 | None | None | Future-proof, strong security, control + telemetry |
| DMG IoTconnector flex (as gateway) | Partial | DMG owns the gateway | Subscription | Useful for one-off non-DMG machines, but don't put it in the core path |
| CELOS X apps | ❌ Proprietary | Heavy | Subscription | Avoid as integration surface; fine as operator HMI |
| FOCAS / LSV-2 / Sinumerik OPC Classic | ❌ Proprietary, licensed | Heavy | Per-machine | Last-resort for legacy controls; consider DNC software instead |
| File-watching / FTP / SMB | n/a — not a protocol | None | None | Fallback when nothing else works |
| RS-232 DNC | n/a — bit-bashing | None (use COTS DNC) | n/a | Outsource to CIMCO / Predator; ABERP owns work-order level only |

## Recommendation framework

**For Phase α (framework ADR, ~2026 H2)**, the order to default to:

1. **MTConnect first** for any new DMG-Mori (or other modern CNC). Read-only telemetry, small client surface, no Rust crate dependency, ANSI-ratified open standard.
2. **OPC-UA second** when bidirectional control or job dispatch becomes a requirement (i.e. ABERP writes a work order back to the machine, not just reads status). OPC 40501 (umati) is the right vocabulary for this.
3. **Both, in parallel** if both interfaces ship on the same machine — telemetry over MTConnect, control over OPC-UA. The IoTconnector exposes both at no extra license cost on DMG[^dmg-conn].
4. **Fallback (file-watching or COTS DNC)** for any legacy machine where the modern interfaces aren't licensed.
5. **CELOS X / IoTconnector flex / vendor-proprietary** — only if forced, and only as an adapter behind ABERP's canonical event types, never as the integration model in core.

This is **not a decision** — the ADR can override based on what hardware actually lands and what licenses Áben actually buys. But it's the default starting position, and any deviation should have a sourced reason.

## What's still unknown

- DMG MORI pricing for IoTconnector tiers and CELOS X subscriptions — quotation-only. Need at least one quote before the ADR can model TCO.
- Heidenhain "Connected Machining" per-machine fee — vendor-paywalled.
- FOCAS license cost — vendor-paywalled.
- Per-machine MTConnect DataItem coverage — only knowable after install. Plan for a "first-day-on-floor" `/probe` audit step in the Phase ε bring-up.
- MTConnect v2.5 (Feb 2025) detailed change-log — release exists but contents not fully reviewed in this research pass; verify when ADR is written.

## Citations

[^dmg-conn]: DMG MORI, "Connectivity (IoTconnector)." https://en.dmgmori.com/products/digitization/connectivity — fetched 2026-06-02.
[^dmg-celosx]: DMG MORI, "CELOS X." https://en.dmgmori.com/products/digitization/celos-x — fetched 2026-06-02. US blog elaboration: https://us.dmgmori.com/news-and-media/blog-and-stories/blog/blg24-7-celos-x-machine-data-ecosystem
[^werkbliq-eol]: Makula, "DMG MORI's WERKBLiQ Shutdown — What It Means" (covers DMG's announced Dec 2025 end-of-life). https://www.makula.io/blog/dmg-moris-werkbliq-shutdown-what-it-means-and-the-best-alternatives-in-2025 — fetched 2026-06-02.
[^mtc-about]: MTConnect Institute, "About." https://www.mtconnect.org/about — fetched 2026-06-02.
[^mtc-downloads]: MTConnect Institute, "Standard Downloads." https://www.mtconnect.org/standard-download20181 — fetched 2026-06-02.
[^mtc-model]: MTConnect SysML model, V2.3 browser. https://model.mtconnect.org/Version2.3/ — fetched 2026-06-02.
[^mtc-mtcup]: MTConnect Adapter Agent Protocol reference. https://mtcup.org/Protocol — fetched 2026-06-02.
[^mtc-rust-gap]: No dedicated MTConnect crates exist on crates.io as of 2026-06-02. .NET reference: TrakHound MTConnect.NET (covers ≤ 2.5).
[^mtc-cppagent]: `mtconnect/cppagent`. https://github.com/mtconnect/cppagent — fetched 2026-06-02.
[^mtc-supported]: MTConnect Institute, "Supported Devices." https://www.mtconnect.org/step-2-supported-devices — cross-referenced with https://www.machinemetrics.com/blog/what-is-mtconnect — fetched 2026-06-02.
[^opc-machinery]: OPC Foundation, "OPC UA for Machinery." https://opcfoundation.org/markets-collaboration/opc-ua-for-machinery/ — fetched 2026-06-02.
[^umati]: umati / VDW, OPC 40501 Machine Tools. https://umati.org/industries_machine-tools/ — fetched 2026-06-02.
[^opc-robotics]: OPC Foundation, "OPC UA for Robotics." https://opcfoundation.org/markets-collaboration/robotics/ — fetched 2026-06-02.
[^mtc-opcua]: MTConnect, "OPC UA Companion Specification." https://www.mtconnect.org/opc-ua-companion-specification — fetched 2026-06-02.
[^cimco-dnc]: CIMCO DNC-Max. https://www.cimco.com/software/cimco-dnc-max/ — fetched 2026-06-02.
[^predator-dnc]: Predator DNC. https://www.predator-software.com/predator_dnc_software.htm — fetched 2026-06-02.
