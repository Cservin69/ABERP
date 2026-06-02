# 04 — Barcode / QR scanners

Of all the Stage 3 hardware, scanners are the cheapest, most immediately useful, and most likely to land first. Ervin could buy a handful of $80 USB-HID scanners and label inventory before any CNC arrives. That makes this file the most "actionable in 2026" of the package — even if everything else stays paper for another year, scan-driven traveler tracking pays off in months, not years.

## TL;DR

- **USB-HID keyboard wedge** is the cheapest path, zero driver burden, and works on every OS. Trade-off: a stray scan into the wrong window types the payload there. ABERP's UI needs a focused, modal "scan target."
- **TCP** scanners are the right answer for fixed stations. **MQTT** is the right answer once we have many concurrent scanners.
- **Bluetooth** scanners fight the 2.4 GHz band with everything else in the shop — prefer proprietary 2.4 GHz RF with dedicated dongle for handheld-roaming, USB-HID for tethered.
- **Code 128** or **GS1-128** for travelers and pallet tags. **DataMatrix** for direct-part-marking. **QR** for human/phone-readable labels.
- **Zebra DS3608** (rugged, IP67, 8 ft drop) is the reference industrial handheld; commercial USB-HID handhelds are $30-150, DS3608-class is $400-700.

## Integration models — pick by station, not by vendor

### USB-HID keyboard wedge

Scanner enumerates as a USB HID keyboard; decoded payload is *typed* into whatever text field has focus, followed by a configurable suffix (usually Enter or Tab). Zero driver burden on Windows / macOS / Linux / Android.[^tera-hw]

Pros:
- No driver, no permissions ceremony
- Works in literally any UI — web, native, terminal
- Cheap

Cons:
- **The stray-scan problem**: if focus is in a wrong window, the payload types into that window. Either as a Tauri stack alert (operator sees gibberish appear in their email) or as a real bug (the payload gets entered into the wrong record).
- The mitigation is in *our* UI, not in the scanner: a focused modal "scan target" input, or a global keyboard hook that intercepts scan-shaped input regardless of focus, or a software-side scan-prefix discipline (every barcode is prefixed with a sentinel like `~SCAN:` so we can identify scan-input vs human typing).

**Most scanners also support USB-COM mode** — virtual serial port instead of HID. The host listens on a serial channel.[^tera-hw] Removes the stray-scan problem; adds a per-scanner serial reader.

### Serial RS-232 + USB-serial adapter

Older industrial pattern; still common because line-of-business apps written 1995-2010 expected COM ports. In 2026, USB-COM (above) effectively replaces this — same byte stream, no physical RS-232 dongle.

### TCP / Ethernet scanners

Industrial fixed-mount scanners increasingly expose a TCP socket the host opens to receive scans (or a webhook the scanner POSTs to). **Cognex Modular Vision Tunnels**[^cognex] are an industrial-grade reference example — Ethernet as the primary data path, scanning at conveyor speed.

For ABERP, the right integration shape for fixed stations: scanner has a static IP, ABERP cell-controller listens on a socket, scan event arrives. Network-level diagnostics (the scanner is reachable / not) come for free with normal IP tools.

### MQTT-native scanners

Newer pattern, pub-sub friendly, fits naturally with edge brokers. Cognex Modular Vision Tunnels explicitly support **"data forwarding over secure MQTT to Microsoft Azure, Amazon Web Services, and SCADA systems."**[^cognex]

For ABERP, attractive when there are many concurrent scanners (multiple cells, multiple stations): a Rust adapter subscribes to `shop/cell-3/scans/#` and never has to keep a TCP connection alive per device. Add Mosquitto on the cell controller (or one central broker on the shop network); each scanner publishes; ABERP subscribes.

**Recommended path for ABERP**: USB-HID for the first cell (cheapest, simplest), TCP for fixed stations, **MQTT once we cross ~5 concurrent scanners**.

### Bluetooth / wireless

Bluetooth handheld scanners are convenient when an operator roams (raw-stock yard, packaging area). The trade-off is real: the 2.4 GHz band is shared with Wi-Fi, microwave ovens, and every other Bluetooth device on the floor.

Tera Digital's wireless guide is blunt: **"Bluetooth scanners may suffer interference or slower reconnection times, especially around other Bluetooth devices,"** and the dongle variants **"share the same crowded 2.4 GHz band."**[^tera-bt] Their recommendation for warehouse-class: prefer **proprietary 2.4 GHz RF with a dedicated dongle** (up to ~300 ft / 90 m range, vs Bluetooth's ~30 ft / 9 m).

For Áben's likely workflow (small shop, operator at the laser walks to the bend station): Bluetooth is fine in practice if there's no Wi-Fi-heavy area. Test before committing.

## Symbology selection — what to print on the labels

| Symbology | Type | Best for | Caveats |
|---|---|---|---|
| **Code 39**[^teklynx] | 1D, alphanumeric, low density | Legacy / automotive | Old-school; lower density than 128 |
| **Code 128**[^teklynx] | 1D, full 128-ASCII, denser | Universal default for industrial 1D when you control both ends | Best 1D choice for new work |
| **GS1-128**[^gs1-link] | Code 128 + standardized GS1 AIs | Logistics, healthcare, traceability with batch/lot/serial/expiry | The "supply-chain-correct" choice |
| **DataMatrix / GS1 DataMatrix**[^gs1-dm] | 2D, very small footprint | Direct-part-marking on small machined parts | Needs a 2D imager (most modern scanners) |
| **QR**[^teklynx] | 2D, larger footprint at equivalent capacity | Any human-facing label that a phone might need to scan | Ubiquitous reader support including every smartphone |
| **PDF417**[^teklynx] | Stacked 2D, very high capacity | US driver's licences, transportation, pharma | Overkill for a machine shop |

**Practical recommendation**:
- **Code 128 or GS1-128** for travelers and pallet tags (printable on a standard thermal label printer)
- **DataMatrix** for direct-marking individual parts where space is tight (machined fittings, small precision parts)
- **QR** for any human-facing label that a phone might need to scan (operator's phone, quick lookup)

**GS1 Application Identifiers** are the killer feature of GS1-128 and GS1 DataMatrix. A single barcode can carry `(01)<GTIN>(10)<LOT>(21)<SERIAL>(17)<EXPIRY>` in one scan. Worth adopting as ABERP's traveler format from day one — even if v1 only uses `(21)` (serial), the upgrade path to lot/expiry tracking is zero code change at scan time.

## Industrial-grade vs commercial — the reference points

### Zebra DS3608 — industrial-grade reference

- Survives an **8 ft / 2.4 m drop to concrete**[^ds3608-zebra][^ds3608-bcdinc]
- **IP67** sealed[^ds3608-zebra]
- Operating range **−30 °C to +50 °C**[^ds3608-zebra]
- 3-year warranty[^ds3608-zebra]

Street price: **$400-700** (corded vs cordless, scanner-only vs kit).

### Commercial-grade reference

- **Zebra DS2208** (1D/2D, retail/light-industrial) ~$100
- **Honeywell Voyager 1200g** (1D only, light industrial) ~$79[^ds2208-cmp]

These are perfectly fine for office and traveler-scanning workstations; not fine for hanging off a wet, oily CNC.

### Fixed-mount industrial

- **Cognex DataMan** series — from ~$1,000 and rising quickly with optics
- Used at conveyor stations, fixed scan-at-pass-through workflows

### Price summary

| Tier | Use case | Street price (2026) |
|---|---|---|
| **Commercial USB-HID handheld** | Office, traveler scanning workstations | $30-150 |
| **Industrial handheld (DS3608 class)** | Shop floor, drops, sealed environment | $400-700 |
| **Fixed-mount industrial imagers** | Conveyors, automated scan-at-pass | $1,000+ |

## Adapter shape for ABERP

Three canonical event types, regardless of scanner integration model:

```
PartScanned    { code, raw_payload, station_id, operator_id?, scanned_at }
ScanIgnored    { reason, raw_payload, station_id }
ScanFailed     { error, raw_payload?, station_id }
```

The **resolution step** is in ABERP, not in the scanner:

```
scan event arrives  →  parse GS1 AIs (if applicable)
                    →  resolve to part / work order / pallet
                    →  validate against station's expected work
                    →  emit PartScanned   (if valid)
                       or ScanIgnored     (if not relevant to this station)
                       or ScanFailed      (if malformed or unknown)
```

**Operator session** binds the scanner to a user: operator scans their own employee badge at shift start, station's session-state remembers them. Every subsequent scan is attributed to that operator without re-scanning the badge each time.

**Station state**: the cell controller knows which work order is at which station. A scan that doesn't match expected work emits `ScanIgnored` and surfaces "wait, this part isn't supposed to be here" — that's a hülye-biztos failure mode the scanner reveals.

## ABERP's earliest-possible adoption path

**Phase β candidate** (per the Stage 3 phase sequencing in the README): even without any CNC integration, scanners pay off immediately.

Minimum viable: one USB-HID scanner per workstation, a thermal printer for labels (Zebra ZD220 ~€150), GS1-128 barcodes carrying `(21)<serial>` per work order. ABERP gains:
- Status-tracking on manual operations (laser → bend → weld → deburr → paint — see [09-laser-workflow.md](09-laser-workflow.md))
- WIP visibility (where is part X in the shop right now?)
- Operator-attributed scan history (who touched what when)

Cost: ~€300-500 for first cell. Time: a single PR-sized adapter crate. **The fastest possible "Stage 3 ROI" Áben can buy.**

## HU + EN considerations

Scan payloads are ASCII / binary, language-neutral. The operator-facing UI when a scan arrives is bilingual — "Beolvasva: rendelés #1234" / "Scanned: order #1234". GS1 AI names have established Hungarian translations the operator never sees (we display the *meaning*, not the AI code).

Operator badge IDs: Áben can use existing personal employee numbers from accounting; no separate "scan ID" scheme needed.

## Recommendation framework

**For Phase β (first barcode adopter, possibly 2026 ahead of CNC)**:

1. **Start with one Zebra DS2208 or equivalent** at €100 for the first cell. Light-industrial is fine for a year of "does scanning actually solve the problems we think it solves" learning.
2. **Print GS1-128 travelers** with `(21)<serial>` from the start. Upgrade-path is free.
3. **USB-HID mode** unless the stray-scan problem bites — then switch the same hardware to USB-COM.
4. **ABERP UI**: focused modal scan target. Stay simple in v1.
5. **Step up to DS3608-class** only when v1 reveals real drop/sealed-environment requirements. Don't over-buy for hypothetical durability.
6. **TCP / fixed-mount** when a workflow has a natural fixed scan point (conveyor pass-through, station-entry). Not in v1.
7. **MQTT** when scanner count crosses ~5 concurrent devices. Not in v1.

**For Phase γ (inventory module)**: scanner adoption is a prerequisite. Inventory locations get barcoded racks; raw stock arrives with vendor barcodes (often Code 128 or GS1-128 already); WIP carries the GS1-128 traveler.

## What's still unknown

- **2026 Hungarian distributor pricing** — US street prices are the only data we have. Verify before any procurement.
- **Vendor scanner pricing volatility** — scanner pricing changes per supply-chain conditions; re-quote at procurement time.
- **Whether Áben's existing thermal printers can do GS1-128** — most can but verify; some legacy printers only do Code 39.

## Citations

[^tera-hw]: Tera Digital, "Barcode Scanner Hardware Guide." https://tera-digital.com/blogs/barcodes/barcode-scanner-hardware — fetched 2026-06-02.
[^tera-bt]: Tera Digital, "Wireless Bluetooth Barcode Scanner Guide." https://tera-digital.com/blogs/barcodes/wireless-bluetooth-barcode-scanner — fetched 2026-06-02.
[^cognex]: Cognex, "Modular Vision Tunnels." https://www.cognex.com/products/modular-vision-tunnels — fetched 2026-06-02.
[^teklynx]: TEKLYNX, "Barcode Symbologies." https://www.teklynx.com/en/learn-more/barcode-symbologies — fetched 2026-06-02.
[^gs1-link]: Digital-Link, "GS1 Barcode Formats Explained." https://digital-link.com/news/gs1-barcode-formats/ — fetched 2026-06-02.
[^gs1-dm]: GS1 Canada, "GS1 DataMatrix Introduction" (PDF). https://gs1ca.org/documents/standards/GS1-DataMatrix-Introduction-and-technical-overview-v.pdf — fetched 2026-06-02.
[^ds3608-zebra]: Zebra, "DS3608-DPX / DS3678-DPX Ultra-Rugged Spec Sheet" (PDF). https://www.zebra.com/content/dam/archive_zebra_dam/en/spec-sheets/ds36x8-dpx-spec-sheet-a4-en-us.pdf — fetched 2026-06-02.
[^ds3608-bcdinc]: Barcodes Inc., "Zebra DS3608-SR." https://www.barcodesinc.com/zebra/ds3608-sr.htm — fetched 2026-06-02.
[^ds2208-cmp]: ShipScience, "Zebra DS2208 vs Honeywell Voyager 1200g." https://www.shipscience.com/zebra-ds2208-vs-honeywell-voyager-1200g-3/ — fetched 2026-06-02.
