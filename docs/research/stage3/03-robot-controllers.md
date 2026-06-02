# 03 — Robot controllers — FMS dispatch integration

When a robot moves a part from stock to CNC to QC to packaging, ABERP is the brain telling it *what* to move and *where* to put it. This file surveys the major vendors' integration surfaces so that when the time comes to pick a brand, the choice is made with eyes open.

## TL;DR

- **Universal Robots** has the most open published interface surface — RTDE, Dashboard, Modbus, XML-RPC, free Docker simulator. No vendor portal account needed.
- **ABB** is the only major vendor with a first-class REST + WebSocket surface (Robot Web Services).
- **KUKA** ships RSI (4-12 ms hard-real-time) and KRL but no documented high-level ERP-friendly interface — adapter requires a custom KRL socket server on the controller.
- **Fanuc** OPC-UA exists but requires option **R553** at purchase; full docs live behind a vendor portal account.
- **Yaskawa** is ROS-2-native via MotoROS2 — easiest robot-side, but pulls in micro-ROS + FastDDS dependency.
- **ROS-Industrial's `industrial_msgs/RobotStatus`** is the de-facto event vocabulary. Lift it including the `TriState` pattern.
- **All robot pricing is reseller/estimate**, not vendor-published. Realistic Hungarian envelope: **€55-130 k installed** per cobot depending on payload.

## Adapter shape — the universal pattern

Every robot adapter follows the same shape regardless of vendor. ABERP wants:

```
ABERP                    Adapter                     Robot controller
  |                         |                              |
  |--- move(part_id, ...) ->|                              |
  |                         |--- vendor-specific motion -->|
  |                         |                              |
  |<-- MoveAccepted --------|<-- ack -----------------------|
  |<-- MoveInProgress ------|<-- state stream --------------|
  |<-- MoveComplete --------|<-- end-of-program ------------|
  |<-- Fault(...) ----------|<-- safety / error -----------|
```

The wire format differs per vendor. **The event vocabulary is shared.** That's the whole adapter-pattern argument.

### Canonical event vocabulary (lifted from ROS-Industrial + ABERP shop-floor needs)

```
MoveAccepted    { task_id }
MoveInProgress  { task_id, current_pose? }
MoveComplete    { task_id, final_pose? }
WaitingOnInput  { task_id, awaiting: "operator-confirm" | "sensor-X" | "interlock-Y" }
Fault           { task_id, kind: EStop | Collision | DrivesOff | UnknownError, code? }
HeartbeatLost   { robot_id }
```

**`WaitingOnInput` is the gap in every vendor's published vocabulary.** None of UR / ABB / ROS-I model "robot is waiting on a sensor" as a first-class field. The pragmatic patterns in industry: (a) reserve a custom error_code range, (b) read a CNC/PLC handshake I/O register the robot program writes when blocked, or (c) the controller publishes a "user message" string event the adapter parses. ABERP should treat `WaitingOnInput` as a first-class adapter state because "why isn't the robot moving" is the most common shop-floor operator question.

**`TriState{TRUE,FALSE,UNKNOWN}`** from ROS-I is worth lifting wholesale into the adapter trait — it makes the "we lost the heartbeat" case explicit in the type system. Matches `fail loud` from CLAUDE.md.

## Universal Robots — most open of the lot

The whole client-interface map is published. Verbatim from UR's documentation:[^ur-interfaces][^ur-dashboard][^ur-tcpip]

| Interface | TCP port | Purpose |
|---|---|---|
| **Primary** | 30001 | URScript in, robot state out (10 Hz) |
| **Secondary** | 30002 | URScript in, robot state out (10 Hz) |
| **Real-Time** | 30003 | URScript in, robot state out (higher rate) |
| **RTDE** | 30004 | Bidirectional state + setpoints + registers; **cannot** send URScript |
| Read-only mirrors | 30011 / 30012 / 30013 | Survive in Local mode |
| **Dashboard Server** | 29999 | Newline-terminated ASCII commands: load/play/stop/pause/safety status |
| **Interpreter Mode** | 30020 | Live URScript injection |
| **Modbus TCP** | 502 | Standard Modbus |
| **XML-RPC** | (variable) | UR controller is the *client* — calls out from a script |
| Ethernet/IP | 2222, 40000, 44818 | Standard EtherNet/IP |
| Profinet | 34964, 40002, 49152 | Standard Profinet |

**Local-mode caveat**: in Local mode (operator working at the teach pendant), ports 30001-30003 close — only 30004 (RTDE) and the read-only mirrors stay up.[^ur-interfaces][^ur-tcpip]

For ABERP, **RTDE + Dashboard is the right pair**: RTDE for streaming state (configurable 125-500 Hz, more than enough), Dashboard for program lifecycle (load this program, start it, stop it, get safety status).

### Simulator

**URSim** is free, runs as a Docker image (`universalrobots/ursim_e-series`, `universalrobots/ursim_polyscopex`).[^ursim] Runs on Mac/Linux/Windows since the Docker variants shipped — no Hyper-V or Parallels nonsense. **Useful for ABERP adapter development without owning a robot.**

### URCap SDK

Free, downloadable. Now has a **PolyScope X** variant in beta on UR+.[^urcap-x] URCaps run *on the robot's pendant* — operator-facing UIs. Probably not where ABERP plays first (ABERP plays the orchestration role from a separate host), but worth knowing the surface exists.

## ABB — first-class REST + WebSocket

**Robot Web Services (RWS)** is ABB's official remote-control surface, REST over HTTP with XML or JSON payloads. Runs on both IRC5 (RobotWare 6.x) and OmniCore controllers.[^abb-rws-intro][^abb-rws]

Hard requirements:[^abb-rws]
- **Authentication required** — digest auth, session cookies (`ABBCX`, `http-session`)
- **Subscriptions via WebSocket** — clients can subscribe to up to 1,000 resources total with High/Medium/Low priority delivery (immediate to ~5s)
- **Embedded OPC-UA server** also available via IoT Gateway add-on (IRC5 + OmniCore), plus an Embedded OPC UA Server inside RobotStudio on TCP port 4880 for simulation[^abb-opcua]
- **RAPID** is the on-controller programming language
- **EGM** (Externally Guided Motion) is a separate, real-time external-control add-on
- **RobotStudio** is freely downloadable[^abb-downloads]; includes a virtual controller for offline programming

ABB is the cleanest mapping for ABERP — a documented REST surface with WebSocket push is the closest thing to "what every modern software developer expects from an API." But it's gated by authentication (good for security, bad for "just LAN-trust" simplicity).

## KUKA — strong real-time, weak high-level

KUKA's surface is awkward for ERP-style integration:[^kuka-kss][^kuka-rsi]

- **KRL** (KUKA Robot Language) — Pascal-like, on-controller
- **KUKA.RobotSensorInterface (RSI)** — real-time sensor/control interface. Ethernet UDP/IP (TCP variant exists), XML payloads. Cycles at **12 ms (IPO)** or **4 ms (IPO_FAST)**.[^kuka-rsi]
- The host must respond within the cycle window or the connection drops — this is *hard real-time*, intended for closed-loop control, not for ERP dispatch.
- **KUKA.OfficeLite**[^kuka-officelite] — virtual KRC controller; identical SmartHMI and KRL syntax to a real KR C4/C5; delivered as a Hyper-V image from KSS 8.6 onward.
- **KUKA.Sim** — separate geometric simulator (not a controller emulator).
- **KUKA Connect** — KUKA's old cloud platform; search coverage in 2026 is thin, suggesting reduced emphasis. **Flag for verification** before depending on it.

**Realistic ABERP integration for KUKA**: write a KRL program that listens on a TCP socket and translates JSON commands → KRL motion calls. ABERP plays peer; the KRL program is effectively a custom adapter living on the controller. More work than UR or ABB, but tractable.

## Fanuc — gated by option code

- **PCDK** (PC Developer's Kit) — Fanuc's official PC-to-controller library; requires the PCDK option on the controller
- **Karel / TP** — on-controller languages; "production-grade Karel + vision" reported as hard to staff[^fanuc-forum]
- **OPC-UA server** — embedded, **requires option R553 ("HMI Device (SNPX)")**.[^fanuc-opcua] Runs on R30-iB+ and later controllers; exposes robot model, alarms, motion data, digital I/O, holding registers
- **Web Server** requires base Web Server (HTTP) + R626 Web Server Enhancements on the controller[^fanuc-forum]
- **Roboguide** — offline simulator, paid, license-gated
- Full Fanuc detail (OPC-UA node hierarchy, OS-version matrices) lives behind the **Fanuc Customer Resource Center / tech-transfer portal** — requires vendor portal account[^fanuc-opcua]

For ABERP, **Fanuc OPC-UA-with-R553** is the cleanest open path, but **expect to negotiate option codes at purchase**. If Áben buys a Fanuc CRX cobot, make sure R553 is in the order — retrofit costs are higher than at-purchase.

A community-maintained PCDK alternative exists: `gavanderhoorn/dominh`,[^fanuc-dominh] which speaks Fanuc's RPC. Useful as a reference but not production-grade.

## Yaskawa / Motoman — ROS 2 native

**MotoROS2** is the standout. Verbatim from the project README: a native ROS 2 node that runs *directly on the controller* as a MotoPlus `.out` application, using **micro-ROS over UDP** to bridge to a host PC running a micro-ROS Agent.[^motoros2-yaskawa][^motoros2-github]

Hard specs:[^motoros2-github]
- **Supported controllers**: DX200, YRC1000, YRC1000micro
- **ROS 2 distributions**: Foxy, Galactic, Humble, Jazzy — **not** Iron, **not** Rolling
- **DDS middleware**: **FastDDS only** — Cyclone DDS won't work
- **Exposed interfaces**: `joint_states` topic, `robot_status` topic, TF transforms, **`FollowJointTrajectory` action server**, services for mode/error management

For ABERP, Yaskawa is the easiest robot-side via ROS 2 — but the host has to run a micro-ROS Agent + FastDDS. That's a non-trivial dependency for what's otherwise a Rust shop. Worth noting that micro-ROS has Rust bindings emerging but they're not yet mature.

Older alternative: **MotoCom32** — legacy Windows SDK, mostly superseded. Don't build against it.

## ROS-Industrial — the cross-vendor abstraction

ROS-Industrial Consortium publishes a standardized **`industrial_msgs/RobotStatus`** message that defines the universal robot-status vocabulary.[^rosi-status-melodic][^rosi-status-kinetic]

```
Header   header
RobotMode mode
TriState e_stopped
TriState drives_powered
TriState motion_possible
TriState in_motion
TriState in_error
int32    error_code
```

`TriState = {TRUE, FALSE, UNKNOWN}` — explicit modeling of "we don't know."[^rosi-status-melodic]

**Important context for 2026**: per ROS-I project leadership, the legacy `industrial_core` is no longer used in ROS 2; current policy is to **let OEMs ship their own ROS 2 drivers**.[^ros-industrial][^rosi-faq] UR, Yaskawa MotoROS2, Mitsubishi MELFA, ABB all have first-party or community ROS 2 drivers; there's no single ROS-I umbrella driver anymore. **But the `RobotStatus` shape remains the de-facto event vocabulary** even when OEM drivers diverge in motion APIs. ABERP should adopt it.

## Cost reality (2025-2026)

**Critical caveat across the board**: UR, ABB, KUKA, Fanuc, and Yaskawa **do not publish list prices**. Every number below comes from reseller quotes, third-party price-guide blogs, or QVIRO crowd-sourced figures. Hungarian/EU pricing typically runs 10-20% above US list once VAT and EU-channel margin stack. Source labels are explicit.

### Cobots (most likely fit for CNC machine tending)

| Model | Payload | Typical US 2025-26 quote | Source |
|---|---|---|---|
| **UR10e** | 12.5 kg | $45-60 k robot-only; ~$48 k common median | Devonics, Standard Bots[^ur-price] |
| **UR20** | 20 kg | $65-85 k integrated | Standard Bots price guide[^ur20-price] |
| **UR30** | 30 kg | No firm 2025 figure published | — |
| **Fanuc CRX-5iA** | 5 kg | ~$43 k | Vention guide[^fanuc-price] |
| **Fanuc CRX-10iA/L** | 10 kg | ~$51 k | Vention guide[^fanuc-price] |
| **Fanuc CRX-20iA/L** | 20 kg | ~$55 k | Vention guide[^fanuc-price] |
| **ABB GoFa CRB 15000** | 5 kg | ~$50-70 k robot-only (no public price, industry estimate) | Standard Bots[^abb-price] |
| **Yaskawa HC10 / HC20** | 10 / 20 kg | $25-50 k+ | Standard Bots[^yaskawa-price] |

### Industrial small arms (six-axis, non-cobot)

| Model | Payload | Typical US 2025-26 | Source |
|---|---|---|---|
| **Yaskawa GP7 / GP8** | 7-8 kg | $14-20 k new (industry estimate) | Standard Bots[^yaskawa-price] |
| **Yaskawa GP12 / GP25** | 12-25 kg | $20-35 k new | Standard Bots[^yaskawa-price] |
| **KUKA KR Agilus** | 6-10 kg | $25-35 k | Standard Bots[^kuka-price] |

### Reality-check overhead

Every credible source agrees: **accessories, integration, end-of-arm tooling (EOAT), safety scanners, and training can double the base price.**[^ur-price][^fanuc-price][^yaskawa-price] Fencing-free cobots save **$10-30 k** in safety infrastructure vs. caged industrial arms.[^fanuc-price]

For a Hungarian shop budgeting **one robot for CNC tending**: **€55-90 k installed** for a UR10e or CRX-class cobot is the realistic envelope. **€80-130 k installed** if you go UR20 / HC20 class. Industry estimate — verify with vendor quote before any procurement.

## Vendor-neutrality scorecard

| Vendor | Open interface? | Vendor portal needed? | OPC-UA support | Free simulator? | ABERP integration cost |
|---|---|---|---|---|---|
| **UR** | ✅ Multiple TCP interfaces, all documented | No | Via 3rd-party Modbus / proprietary | ✅ URSim Docker | **Low** |
| **ABB** | ✅ RWS (REST+WS) | Free dev account helpful | ✅ Via IoT Gateway | ✅ RobotStudio | **Low-Medium** |
| **KUKA** | Partial — RSI only, hard-RT | Yes (for docs) | Limited; companion spec emerging | ✅ OfficeLite (Hyper-V) | **Medium-High** |
| **Fanuc** | Partial — OPC-UA gated by R553 | ✅ Required | ✅ With R553 option | Roboguide (paid) | **Medium** |
| **Yaskawa** | ✅ MotoROS2 native | No | Some via OPC-UA add-on | Via ROS 2 sim | **Medium** (FastDDS dep) |

## Recommendation framework

**For Phase η (robot adapter + first auto-transport task)**:

1. **Pick the robot based on the work, not the integration.** Payload, reach, cycle time, and price-installed matter more than which one has the prettiest API. ABERP's adapter-pattern means the integration cost is a one-time delta, not an ongoing tax.
2. **Default preference: UR or ABB.** Most-open published surfaces, freest simulators, cleanest adapter shape. If the payload and reach work, lean here.
3. **Fanuc is fine** if Áben buys it for non-integration reasons (price, reach, EU service network) — just **negotiate option R553 at purchase**. Retrofit is more expensive than at-purchase.
4. **Yaskawa is also fine** for ROS-friendly teams — but be honest about the FastDDS + micro-ROS dependency on the host. Most Rust shops don't already have that stack.
5. **KUKA only if Áben has a specific reach or payload requirement** the others don't fill. RSI is too low-level for ABERP dispatch; a custom KRL socket server is the realistic adapter shape and that's more work.
6. **Lift the event vocabulary from ROS-Industrial `RobotStatus`** including `TriState`. Add `WaitingOnInput` as ABERP-specific.
7. **Free simulators are load-bearing for the adapter test harness.** URSim Docker is the gold standard; RobotStudio and OfficeLite are usable. Roboguide costs money — don't pay for it unless we've committed to Fanuc.

## What's still unknown

- Hungarian distributor pricing for any of these — all figures above are US street prices; EU adds 10-20% before VAT.
- Whether DMG-Mori has a preferred cobot pairing — DMG sells "DMG MORI Automation" packages but the brand mix isn't public.
- Fanuc R553 retrofit cost vs at-purchase delta.
- KUKA Connect's current product status (cloud platform; thin coverage in 2026; may have been deprioritized).
- micro-ROS Rust bindings maturity for the Yaskawa path.

## Citations

[^ur-interfaces]: Universal Robots, "Overview of client interfaces." https://www.universal-robots.com/articles/ur/interface-communication/overview-of-client-interfaces/ — fetched 2026-06-02.
[^ur-dashboard]: Universal Robots, "Dashboard Server." https://www.universal-robots.com/developer/communication-protocol/dashboard-server/ — fetched 2026-06-02.
[^ur-tcpip]: Universal Robots, "Remote Control Via TCP/IP." https://www.universal-robots.com/articles/ur/interface-communication/remote-control-via-tcpip/ — fetched 2026-06-02.
[^ursim]: Docker Hub, `universalrobots/ursim_e-series`. https://hub.docker.com/r/universalrobots/ursim_e-series — fetched 2026-06-02.
[^urcap-x]: GitHub, `UniversalRobots/PolyScopeX_URCap_SDK`. https://github.com/UniversalRobots/PolyScopeX_URCap_SDK — fetched 2026-06-02.
[^abb-rws-intro]: ABB, "Robot Web Services Introduction." https://developercenter.robotstudio.com/api/rwsApi/ — fetched 2026-06-02.
[^abb-rws]: ABB developer center; subscription model documented at the same URL.
[^abb-opcua]: ABB, "Embedded OPC UA Server" (application manual PDF). https://www.uzivatelskadokumentace.cz/Software%20Products/Production%20Monitoring%20%26%20Data%20Management%20Software/en/3HAC085436-001.pdf — fetched 2026-06-02.
[^abb-downloads]: ABB, "Robotics downloads (RobotStudio)." https://www.abb.com/global/en/areas/robotics/downloads — fetched 2026-06-02.
[^kuka-kss]: KUKA, "KUKA System Software (KSS)." https://www.kuka.com/en-us/products/robotics-systems/software/system-software/kuka_systemsoftware — fetched 2026-06-02.
[^kuka-rsi]: KUKA, "RSI 3.1 system technology" (PDF). http://supportwop.com/IntegrationRobot/content/6-Syst%C3%A8mes_int%C3%A9grations/RobotSensorInterface/KST_RSI_31_en.pdf — fetched 2026-06-02. Ethernet RSI XML 1.1: http://www.wtech.com.tw/public/download/manual/kuka/krc2ed05/KUKA%20Ethernet%20RSI_XML.pdf
[^kuka-officelite]: KUKA, "KUKA.OfficeLite." https://www.kuka.com/en-us/products/robotics-systems/software/simulation-planning-optimization/kuka_officelite — fetched 2026-06-02.
[^fanuc-forum]: Fanuc OPC-UA forum discussion (Robot-Forum). https://www.robot-forum.com/robotforum/thread/35864-opc-ua-in-roboguide/ — fetched 2026-06-02.
[^fanuc-opcua]: Fanuc America, "OPC UA tech transfer" (vendor portal — requires account). https://techtransfer.fanucamerica.com/tech-transfer/opc-ua
[^fanuc-dominh]: GitHub, `gavanderhoorn/dominh` (Fanuc PCDK-alternative RPC). https://github.com/gavanderhoorn/dominh — fetched 2026-06-02.
[^motoros2-yaskawa]: Yaskawa, "MotoROS2 product page." https://www.yaskawa.co.uk/products/software/productdetail/product/motoros2_20580 — fetched 2026-06-02.
[^motoros2-github]: GitHub, `Yaskawa-Global/motoros2` README. https://github.com/Yaskawa-Global/motoros2/blob/main/README.md — fetched 2026-06-02.
[^rosi-status-melodic]: `industrial_msgs/RobotStatus.msg` (melodic). https://github.com/ros-industrial/industrial_core/blob/melodic/industrial_msgs/msg/RobotStatus.msg — fetched 2026-06-02.
[^rosi-status-kinetic]: `industrial_msgs/RobotStatus` docs. http://docs.ros.org/en/kinetic/api/industrial_msgs/html/msg/RobotStatus.html — fetched 2026-06-02.
[^ros-industrial]: ROS-Industrial main site. https://rosindustrial.org/ — fetched 2026-06-02.
[^rosi-faq]: ROS-Industrial FAQ. https://rosindustrial.org/about/faq — fetched 2026-06-02. UR ROS 2 driver: https://github.com/UniversalRobots/Universal_Robots_ROS2_Driver
[^ur-price]: Standard Bots, "Universal Robots price guide." https://standardbots.com/blog/universal-robot-price — fetched 2026-06-02.
[^ur20-price]: QVIRO, "UR20 reviews & price." https://qviro.com/product/universal-robots/ur20 — fetched 2026-06-02. UR10e: https://qviro.com/product/universal-robots/ur10e
[^fanuc-price]: Vention, "FANUC robot cost guide 2025." https://vention.io/blogs/industrial-automation-design/fanuc-robot-costs-guide-1004 — fetched 2026-06-02.
[^abb-price]: Standard Bots, "10 best cobot manufacturers 2026." https://standardbots.com/blog/the-10-best-cobot-manufacturers-in-2024 — fetched 2026-06-02.
[^yaskawa-price]: Standard Bots, "Yaskawa robot prices 2026." https://standardbots.com/blog/yaskawa-robot-price — fetched 2026-06-02.
[^kuka-price]: Standard Bots, "KUKA robot pricing 2026." https://standardbots.com/blog/kuka-robot-pricing — fetched 2026-06-02.
