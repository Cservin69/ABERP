# 05 — Cell controllers — the local computer on the shop floor

Every cell — one CNC + its robot + its scanners + maybe its Equator — needs *something* to be the local brain. This file covers what that something is.

## TL;DR

- **Raspberry Pi 5 / CM5** on an industrial carrier (EDATEC, Elastel, Waveshare) is the right default for ~80% of cell-controller workloads.
- **Beckhoff / Advantech / Siemens / Kontron** industrial PCs are the right answer when DIN-rail integration, certifications, or 10-15-year supply guarantees are load-bearing.
- **Offline-first** is the non-negotiable architectural rule. The cell keeps producing when the WAN drops.
- **OTA strategy**: A/B partitions via RAUC (or SWUpdate, or Mender). All three are production-ready in 2026.
- **VLAN segregation** between shop-floor and office. ACLs are mandatory; **"segmentation without enforcement is cosmetic."**

## Why a cell-local computer at all

Two non-negotiable arguments against "everything-cloud":

### 1. Offline-first contracts

WAN drops, ISP outages, and weekend cloud incidents must not stop the cell from producing. The local controller owns its job queue, traveler database, and scan log; it syncs upward when the link returns.

This isn't paranoia — it's basic operations. Hungarian internet is fine but not perfect; a shop that loses 4 hours of production every time a router reboots is a shop that lost the ROI argument for the WAN-dependent architecture.

ABERP today is already a single-machine app (Tauri desktop). The cell-controller pattern is its natural extension to multiple physical locations within one tenant.

### 2. Latency-critical interlocks

Anything an operator can see — a button press lighting an LED, a scan producing a "go/no-go" beep — must respond in tens of milliseconds. Cloud-round-trip latency is too slow and too jittery.

**Important caveat**: true *safety* interlocks belong in the PLC or in the machine's safety chain, **never in software running on a Raspberry Pi**. The cell controller's "interlocks" are operational (the right work order on the right station), not safety (the door is closed before the spindle spins).

## Raspberry Pi 5 / CM5 — the cheap default

The Pi 5 family is the right starting point for shop-floor edge compute. The **Compute Module 5** is the form factor that matters for a permanent install (no SD card, soldered eMMC, robust connector).

Specs (cross-referenced):[^cnx-cm5][^elastel]
- **Broadcom BCM2712, quad-core Cortex-A76 @ 2.4 GHz** — ~3× CM4 performance
- RAM **up to 16 GB**, eMMC **up to 64 GB**
- **PCIe NVMe** support
- **Dual-channel Gigabit Ethernet with TSN** — first ever on a Pi platform

Industrial CM5-based carrier boards (EDATEC ED-IPC3100, Elastel EG510, Seeed reComputer R2000, Waveshare IPCBOX-CM5) add:
- Extended operating range **−40 °C to +85 °C**[^cnx-cm5][^elastel]
- ECC memory option
- RS-485 / RS-232 / CAN
- Isolated DI/DO
- DIN-rail mount

**Caveat**: the −40 °C to +85 °C headline applies to the *industrial carriers*, not to the bare CM5 SoM — the official Raspberry Pi datasheet specifies a tighter commercial range. (The PDF defeats automated fetch; treat the carrier-board temperature spec as the load-bearing number, not the bare module.) For a Hungarian indoor machine shop this is academic — ambient never gets near the limits — but it matters for any future "machine in a cold corner" scenario.

### Cost envelope

- Bare Pi 5 (8 GB): ~€80
- CM5 (16 GB / 64 GB eMMC): ~€150
- Industrial CM5 carrier (EDATEC / Elastel / Seeed class): ~€200-400 including the SoM
- **Total per cell**: **€250-450**

Compare this to a Beckhoff CX5130 starting around €1,200-1,800 for the bare unit. The Pi path is **3-5× cheaper** per cell — which matters when there are 4+ cells.

### Software stack on the Pi

- **Raspberry Pi OS / Debian / Ubuntu Server** — all viable. Debian stable for the most boring choice.
- **Podman** (rootless, daemonless, systemd-native) — preferred over Docker for single-host edge.
- **systemd** for ABERP cell-controller service supervision.
- **Rust toolchain** cross-compiles cleanly to aarch64; no porting drama.

## When to step up to a name-brand industrial PC

A CM5 industrial carrier covers ~80% of cell-controller workloads. Step up to **Beckhoff / Advantech / Siemens / Kontron** when *any* of these is true:

### DIN-rail integration into an existing PLC cabinet

Beckhoff's **CX series** is purpose-built for this — same EtherCAT backplane as the I/O slices.[^beckhoff-cx] The CX line spans Intel Atom (CX5100), AMD Ryzen 2-core (CX5600), AMD Ryzen 2/4-core (CX20x3), up to Intel Xeon D with 12 cores on a DIN rail.[^beckhoff-cx] Real-time TwinCAT runs on the same box; OPC UA is first-class.[^beckhoff-opcua]

### Safety / certifications

If Áben ever needs CE-Machinery, ATEX, marine, or EN 50121-4 (rail), name-brand IPCs ship the paperwork. The CM5 industrial carriers carry some certifications but the matrix is patchier.

### Real-time motion control on the same box as visualization

PREEMPT_RT on a Pi can hit single-digit-millisecond latency under good conditions — see [the timing discussion below](#real-time-and-the-aberp-role). For true sub-millisecond determinism (servo loops, EtherCAT master), Beckhoff TwinCAT is the answer, not Linux.

### 10-15-year supply guarantee

Pi product lifecycles are reasonable but not 15 years. If a cell controller has to be replaced by the identical model 12 years from now, name-brand industrial PCs win.

### EU presence

- **Beckhoff** — German, very strong EU service network, OPC-UA first-class, joined OPC Foundation in 1998
- **Advantech** — Taiwanese, EU distribution via local channels, Hungarian distributors available
- **Siemens IPC** — German, strong EU presence, integrates natively with Sinumerik
- **Kontron** — German/Austrian, embedded x86 IPC focus

For Áben's likely posture (one engineer, one shop, Pi-class budget): **Pi by default, Beckhoff if a specific cell needs DIN-rail OPC-UA on the EtherCAT backplane**. Don't over-spec on day one.

## OTA update strategy

A shop floor with 4 cell controllers can't have someone driving to each one with a USB stick for every firmware update. OTA is mandatory from day one.

Open-source landscape converges on **dual A/B partitioning** for safe rollback:[^ota-witekio][^proteanos-ota][^rugix-ota]

| Tool | Footprint | Pattern | Notable |
|---|---|---|---|
| **RAUC** | ~512 KB binary | A/B mandatory; signed bundles required by design | Cleanest architecture |
| **SWUpdate** | ~1.3 MB | A/B *or* delta-into-running | Most flexible; .swu cpio with sw-description manifest |
| **Mender** | ~6.9 MB | A/B mandatory | Most complete out-of-box (server + agent + web UI) |
| **balena** | container-based | A/B for host OS, containers for apps | Proprietary backend; container delta updates |

All four are production-ready in 2026.[^proteanos-ota]

**Recommendation for ABERP**: **RAUC + a static HTTPS bundle URL signed with a Sigstore-style key**. Minimum viable OTA for a one-engineer shop without an OTA-server budget. If the shop later wants a web dashboard for fleet management, **Mender** is the easiest path — but that's a v2 concern.

**A/B partition discipline**: every cell controller has *two* root partitions. New firmware goes to the inactive one, boots into it, runs a health-check, commits-on-success or auto-reverts on failure. **No firmware update can brick a cell controller** because the old partition is always there.

## Networking — VLAN segregation

Authoritative practice: an **OT VLAN** physically separated (or at minimum L3-firewalled with explicit ACLs) from the office IT VLAN.[^trout-vlan][^otecosystem]

The trout.software guide is blunt: **"Segmentation without enforcement is cosmetic."**[^trout-vlan] VLANs without inter-VLAN ACLs give you broadcast isolation and zero security benefit.

**For ABERP's Stage 3 topology**:

```
Office VLAN  (192.168.1.0/24)
  │
  ├── workstations
  ├── printers
  └── ABERP central server (when SaaS-migrated per the `aberp-saas-migration` planning memo)
       │
       │ (firewall + explicit ACLs)
       │
Shop-floor VLAN  (192.168.10.0/24)
  ├── cell-controller-1 (Pi 5 / CM5)
  │     ├── CNC (MTConnect / OPC-UA)
  │     ├── robot
  │     ├── Equator
  │     └── scanners
  ├── cell-controller-2
  └── ...
```

Inter-VLAN rules: **ABERP central → cell-controllers** is allowed (work-order dispatch). **Cell-controllers → ABERP central** is allowed (status events, scans). **Anything else is denied.** Industrial firewalls with DPI for OPC UA / Modbus are the recommended primary segmentation tool;[^otecosystem] host-level microsegmentation as defense-in-depth.

## Containerization

Podman and Docker both run on Raspberry Pi OS / Debian / Ubuntu Server on Pi 5 / CM5 hardware without ceremony. **Podman is the more sensible default on a single-host edge box** — rootless, daemonless, systemd-native.

Each adapter (MTConnect client, robot adapter, scanner ingest, etc.) can be packaged as a container or as a plain systemd-managed binary. **Plain systemd is fine for v1** — containers add operational surface that one engineer doesn't need yet. The decision is reversible.

## Real-time and the ABERP role

For status polling (read OPC UA every 1-5 s, push to local DB, fan out over WebSocket): **tokio async on stock Linux is comfortably good enough.** Single-digit-millisecond scheduling jitter on a Pi 5 is three to four orders of magnitude better than the polling cadence requires.

For sub-millisecond determinism: **no, not on Linux of any flavor without a real-time kernel** — and even then async runtimes aren't the right shape for the problem.

**PREEMPT_RT** (most of which is now mainlined in kernel 6.x) is good for the **20-200 µs latency range under well-controlled conditions.**[^preempt-rt] That window is for:
- Soft-real-time fieldbus stacks (EtherCAT master)
- Closed-loop servo control
- Safety reaction times that haven't already been off-loaded to dedicated hardware

If ABERP finds itself wanting any of these, **it has crossed from MES into PLC territory**. The answer is buy a PLC (or a Beckhoff CX running TwinCAT) and let the PLC own that responsibility. ABERP talks to the PLC over OPC UA.

**ABERP's role, restated**: read machine status, decide what job goes next, record what happened, display it to humans. **Never close a control loop on production safety.**

## Adapter shape — the cell controller's job

Each cell controller runs:

```
+----------------------------------------------------------+
|  Cell controller (Pi 5 / CM5 / industrial PC)            |
|                                                          |
|  +-------------+  +-------------+  +-----------------+   |
|  | MTConnect   |  | Robot       |  | Scanner ingest  |   |
|  | client      |  | adapter     |  | (MQTT or TCP)   |   |
|  +-------------+  +-------------+  +-----------------+   |
|         \             |              /                   |
|          v            v             v                    |
|         +-------------------------+                      |
|         | Local audit-queue +     |                      |
|         | event router            |                      |
|         +-------------------------+                      |
|                    |                                     |
|                    v (when online)                       |
+--------------------|-------------------------------------+
                     |
                     v
              Central ABERP
              (audit-ledger sync)
```

The **local audit queue** is the offline-first primitive. Events buffer locally; sync to central ABERP when online. Idempotency on event UUID; central ABERP dedups by `(cell_id, event_uuid)`. Same pattern ABERP already uses for the audit-ledger — extended to a multi-node topology.

## Recommendation framework

**For Phase α (framework ADR)**:

1. **Default to Raspberry Pi 5 / CM5 on industrial carrier** for the cell-controller role. ~€300-450 per cell. EDATEC, Elastel, or Seeed industrial carriers are all reasonable; pick by Hungarian distributor availability.
2. **Beckhoff CX** only when DIN-rail integration with an existing EtherCAT slice ecosystem is needed. Cost is 3-5× the Pi; budget for it deliberately.
3. **Podman over Docker** when containerization is wanted; **plain systemd** is fine for v1.
4. **RAUC for OTA** as the v1 baseline; Mender if a web dashboard for fleet management becomes a need.
5. **OT VLAN with explicit ACLs** from day one. Industrial firewall with OPC-UA DPI as nice-to-have when budget allows.
6. **Local audit-queue** is mandatory architectural shape. Offline-first is non-negotiable.
7. **Never run safety code on the cell controller.** Safety stays in the PLC or in the machine.

## What's still unknown

- Hungarian distributor availability for EDATEC / Elastel CM5 carriers — verify before procurement.
- Whether the bare CM5 official datasheet temperature spec matters for any Áben deployment — almost certainly not, but flagged.
- Whether Áben's shop already has a managed switch capable of VLAN tagging — likely yes given existing IT setup, but verify.
- OTA bundle hosting topology when ABERP is itself cloud-hosted post-SaaS migration — interaction with the planned `invoicing.abenerp.com` SaaS migration is a future ADR concern.

## Citations

[^cnx-cm5]: CNX Software, "Raspberry Pi CM5 industrial computer." https://www.cnx-software.com/2026/03/05/raspberry-pi-cm5-industrial-computer-features-rs485-rs232-can-bus-dio-interfaces-dual-ethernet-optional-4g-5g-cellular-module/ — fetched 2026-06-02.
[^elastel]: Elastel, "EG510 Industrial Raspberry Pi CM5 Edge Computer." https://www.elastel.com/products/industrial-raspberry-pi/eg510-edge-computer/ — fetched 2026-06-02.
[^beckhoff-cx]: Beckhoff USA, "Embedded PCs." https://www.beckhoff.com/en-us/products/ipc/embedded-pcs/ — fetched 2026-06-02.
[^beckhoff-opcua]: Beckhoff, "OPC UA." https://www.beckhoff.com/en-en/products/automation/opc-ua/ — fetched 2026-06-02.
[^ota-witekio]: Witekio, "Comprehensive Embedded OTA Guide." https://witekio.com/blog/ota-update-solutions-the-ultimate-guide/ — fetched 2026-06-02.
[^proteanos-ota]: ProteanOS, "OTA Updates in 2026: RAUC vs SWUpdate vs Mender." https://proteanos.com/doc/ota-updates-rauc-swupdate-mender-2026/ — fetched 2026-06-02.
[^rugix-ota]: Rugix, "Comparing Open-Source OTA Update Engines for Embedded Linux." https://rugix.org/blog/2026-02-28-ota-update-engines-compared/ — fetched 2026-06-02.
[^trout-vlan]: Trout Software, "Flat Network vs Segmented Network." https://www.trout.software/blog/flat-vs-segmented-networks-security-trade-offs-in-industrial-environments — fetched 2026-06-02.
[^otecosystem]: OT Ecosystem, "Network Segmentation Best Practices for Industrial Sites." https://otecosystem.com/network-segmentation-best-practices-for-industrial-sites/ — fetched 2026-06-02.
[^preempt-rt]: ProteanOS, "Real-Time Linux in 2026: PREEMPT_RT Basics." https://proteanos.com/doc/real-time-linux-preempt-rt-latency-2026/ — fetched 2026-06-02.
