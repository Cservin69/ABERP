# Auto-quote pricing model — research & day-1 parameter set (S417)

- **Date:** 2026-06-14
- **Author:** S417 research session (read-only; no engine/SPA/API code touched)
- **Branch:** `session-417` (local only, not pushed)
- **Status:** RESEARCH ONLY. Ervin reviews → approves parameters → Dispatch spawns S418 to implement.
- **Scope (Ervin, 2026-06-14 13:34):** Pure CNC. 3D-printing survey dropped.

---

## 0. TL;DR verdict

The engine is sound but **half-wired**. Today every auto-quote prices **material + overhead + margin only**;
the MACHINING line (currently called "Labor") is **always 0.00 EUR** and there is **no CAD-CAM design cost
at all**. Two root causes:

1. **The feature graph is empty.** The CAD extractor emits only bounding box + volume + two boolean flags
   for STL *and* STEP in v1 — never a populated `features[]`. The engine's machining-time loop iterates an
   empty list, so `machining_minutes = 0` → labor cost = 0. (`engine.rs:179`, both extractors hardcode
   `features or []`.) This is *honest*, not a bug — but it means the price is material-only.
2. **The machining rate is a placeholder.** `machining_rate_eur_per_minute = 1.0` is hardcoded
   (`quote_pricing_pipeline.rs:2390`), i.e. 60 EUR/h — and it multiplies zero minutes anyway.

**The fix is a model change, not a tuning change.** Ervin's instinct is exactly right: derive machining time
and complexity from the **geometry signals that actually exist** (bbox, volume, the two flags, + a new
surface-area signal), not from a feature graph that will stay empty for STL files. This report specifies:

- **Incoterms: EXW** (Ex Works, factory door) — §1.
- A **geometry-driven machining-time heuristic** (roughing from removed volume, finishing from surface area,
  setup from complexity) — §5.
- A **CAD-CAM complexity matrix** mapping geometry → 1–5 design hours @ 100 EUR/h — §4.
- Machining rate **100 EUR/machine-hour (1.6667 EUR/min)** per Ervin's spec — §8.
- A **🔴 correctness landmine**: the seeded `machinability_index` values are **semantically inverted**
  relative to how the engine consumes them; turning on machining time would price Inconel as the *cheapest*
  metal to cut. Must be fixed in the same cut — §6.
- A day-1 parameter set **calibrated against Protolabs / Xometry / Hubs** to sit *below* the premium bureaus
  on simple parts (not frightening) while clearing margin on hard jobs (not bankrupting) — §7, §8.

---

## 1. Incoterms decision

### 1.1 The three candidates

| Term | Seller does | Buyer does | Risk transfers | Fit for "quote on the factory door" |
|---|---|---|---|---|
| **EXW** (Ex Works) | Makes goods available, packed, at its own premises. Does **not** load, does **not** arrange transport, does **not** clear for export. | Loading, all transport, insurance, export **and** import clearance, all duties/taxes. | When goods placed at buyer's disposal at the named place (seller's gate). | ✅ Exact match — minimum seller obligation. |
| **FCA** (Free Carrier, seller's premises) | Loads the goods onto the buyer's collecting vehicle **and** clears for export. | Transport + import. | When loaded onto buyer's carrier at seller's premises. | ⚠️ One step more than Ervin wants (he loads + does export paperwork). ICC-recommended for exports. |
| **FOB** (Free On Board) | Delivers across the ship's rail at a named **port**. | Sea freight onward. | At ship's rail. | ❌ Sea-shipment term only; wrong for a machine shop's gate. |

### 1.2 Formal definition (Incoterms® 2020, ICC)

> Under the Incoterms 2020 rules published by the International Chamber of Commerce, **Ex Works (EXW)** places
> the **minimum obligation on the seller** and the maximum burden on the buyer. The seller delivers by placing
> the goods at the buyer's disposal at a named place (typically the seller's factory or warehouse). **The
> seller does not load the goods, does not arrange carriage, and does not clear the goods for export.** Risk
> transfers from seller to buyer when the goods are made available at the named place. EXW may be used for any
> mode of transport.

The ICC explicitly cautions that for *cross-border* trade EXW is awkward, because the buyer struggles to
complete the seller's-country export formalities; the ICC's preferred "factory-gate" term for exports is
**FCA (seller's premises)**, where the seller at least loads and clears for export.

### 1.3 Decision

**Adopt EXW** as the day-1 standard. It is the literal meaning of "price on the factory door": Ervin's
price = the finished part(s), made and packed at his shop, ready for the customer to collect. Everything
beyond the gate is the customer's.

- For **domestic / intra-EU** customers (no export clearance), EXW is clean and customary.
- If Ervin starts shipping **outside the EU**, upgrade that lane to **FCA (Áben Consulting KFT premises)** so
  *he* does the export declaration — the customer cannot file a Hungarian EX-A on his behalf. Note this as a
  future toggle, not a day-1 need.

### 1.4 Verbatim wording for PDF / email / storefront

Place a one-line incoterm note under the price total on the quote PDF, in the storefront price box, and in the
customer email. `[Város / City]` to be filled in by Ervin (his registered premises).

**English:**
> Prices are quoted **EXW (Ex Works, Incoterms® 2020) — [City], Hungary**. The price covers the manufactured
> part(s), made available and packed at our facility, ready for collection. Loading, transport, insurance,
> export/import clearance, customs duties, and any taxes beyond Hungarian VAT are the buyer's responsibility.

**Hungarian:**
> Az árak **EXW (gyári átvétel, Incoterms® 2020) — [Város], Magyarország** paritáson értendők. Az ár a
> legyártott alkatrész(eke)t tartalmazza, üzemünkben átvételre készen, csomagolva. A felrakodás, a szállítás,
> a biztosítás, az export/import vámkezelés, a vámok és a magyar áfán felüli adók a vevőt terhelik.

---

## 2. Current engine math survey

The engine (`crates/aberp-quote-engine/`) is a **pure, deterministic 16-step scorer** (`engine.rs:52`). Every
step appends one reasoning-log line; reading the log top-to-bottom reconstructs the price exactly.

### 2.1 The formula, end to end

```
material_cost  = volume_mm3 × (1 + scrap_factor) × density_g_cm3 / 1e6 × cost_per_kg_eur     [step 1–2]
               × (1 + stock_adjustment_pct)   if a (grade, stock_status) row matches          [step 10]
               × (1 + exotic_material_tax)     if grade contains "inconel"|"titanium"          [step 11]

machining_minutes = Σ over features[]  ( rule.base_time_minutes × feature.count × rule.multiplier )   [step 3–4]
                  → ALWAYS 0  (features[] is empty — see §2.3)
setup_penalty     = Σ distinct fired rules' setup_penalty_minutes  → 0
inspection_minutes = tolerance.inspection_minutes_per_feature × feature_rows  → 0

total_minutes = machining_minutes / machinability_index + inspection_minutes   → 0             [step 6]
labor_cost    = total_minutes × machining_rate_eur_per_minute × tolerance.multiplier  → 0.00
              × THIN_WALL_TIGHT_TOL_BUMP(1.15)  if thin_wall_present && tol ≥ Tight             [step 7]
              × material.quote_multiplier        if ≠ 1.0                                       [step 8]

setup_cost = qty ≥ setup_amortization_threshold ? (setup_penalty × rate / qty)
                                                : (setup_penalty × rate)   → 0                  [step 9]

subtotal   = material_cost + labor_cost + setup_cost                                            [step 12]
overhead   = subtotal × overhead_factor                                                         [step 13]
margin     = (subtotal + overhead) × profit_margin_base                                         [step 14]
total      = subtotal + overhead + margin                                                       [step 15]
gate:  margin / total ≥ min_margin   else MarginFloorViolation                                  [step 16]
```

### 2.2 Current constants (the live production seed)

| Knob | Seed value | Source |
|---|---|---|
| `scrap_factor` | **0.08** (8%) | `quoting_tunables.rs:339` |
| `profit_margin_base` | **0.35** (35%) | `quoting_tunables.rs:342` |
| `overhead_factor` | **0.20** (20%) | `quoting_tunables.rs:345` |
| `setup_amortization_threshold` | **5** | `quoting_tunables.rs:348` |
| `min_margin` | **0.10** (10% floor) | `quoting_tunables.rs:351` |
| `exotic_material_tax` | **0.05** (5%) | `quoting_tunables.rs:354` |
| `machining_rate_eur_per_minute` | **1.0** (= 60 EUR/h) — **hardcoded, not in DB** | `quote_pricing_pipeline.rs:2390` |
| `THIN_WALL_TIGHT_TOL_BUMP` | **1.15** — Rust const | `engine.rs:23` |
| default tolerance (auto-quotes) | **Standard** (mult 1.0) | `serve.rs:1686` |
| default quantity (if storefront omits) | **1** | `quote_pricing_pipeline.rs:336` |
| `complexity_rules`, `stock_adjustments` tables | **empty on first boot** (no seed) | confirmed |

Tolerance multipliers seeded: loose 0.9 / **standard 1.0** / tight 1.4 (+0.5 insp/feat) / precision 1.9 (+1.5) /
ultra_precision 2.8 (+3.0). (`quoting_tunables.rs:485`)

### 2.3 The PEEK 15 pcs → 47.17 EUR breakdown, decoded

Reported: Material 29.12 · Labor 0.00 · Setup 0.00 · Overhead 5.82 · Margin 12.23 → **TOTAL 47.17**.

Reverse-solving against PEEK seed (density 1.30 g/cm³, cost 90.0 EUR/kg, not exotic, no stock adj):

```
mass_kg      = 29.12 / 90.0           = 0.32356 kg
scrap_volume = 0.32356 × 1e6 / 1.30   = 248,884 mm³
volume_mm3   = 248,884 / 1.08         ≈ 230,448 mm³      (≈ a 61 mm solid cube of PEEK)
overhead     = 29.12 × 0.20           = 5.824   ✓
margin       = (29.12 + 5.82) × 0.35  = 12.229  ✓
total        = 29.12 + 5.82 + 12.23   = 47.17   ✓   (margin/total = 25.9% > 10% floor)
```

The math is internally perfect. The problem is what's *missing*: a 230 cm³ PEEK part that takes hours to
machine is quoted as if it were a billet handed over the counter. **Labor 0.00 / Setup 0.00 is the whole
story** — and there is no design/programming cost line at all.

### 2.4 Diagnosis — why MACHINING is always 0 EUR

Two independent zeros multiply:

1. **`machining_minutes = 0`** because `feature_graph.features` is empty. The extractor never populates it
   (§3 below). `inspection_minutes = 0` for the same reason (it's `per_feature × feature_rows`). So
   `total_minutes = 0` regardless of rate.
2. Even if minutes existed, **`rate = 1.0 EUR/min` (60 EUR/h)** is below Ervin's intended 100 EUR/h.

> **The rate is the smaller problem. The structural problem is that nothing computes minutes.** Bumping the
> rate to 1.6667 alone changes 0 × 1.6667 = still 0. The model must *derive minutes from geometry.*

---

## 3. What the CAD extractor actually gives us

`crates/aberp-cad-extract-wrapper/` (Rust) shells out to the S269 Python extractor and validates a
`FeatureGraph` JSON (schema v1). **Every output contains exactly these fields:**

| Signal | Field | Type | STL? | STEP? | Populated today? | Notes |
|---|---|---|---|---|---|---|
| Schema version | `_schema_version` | u32 | ✓ | ✓ | yes (=1) | blast-door on parse |
| Bounding box | `bounding_box_mm[3]` | f64×3 | ✓ | ✓ | **yes** | STL: vertex min/max extent; STEP: OCCT tight bbox |
| Volume | `volume_mm3` | f64 | ✓ | ✓ | **yes** | STL: signed-tetrahedra; STEP: `BRepGProp::VolumeProperties` |
| Material grade | `material_grade` | str | ✓ | ✓ | yes | operator/storefront-supplied |
| Features | `features[]` | Vec | empty | empty | **NO — hardcoded `[]`** | feature mining needs B-rep; deferred S270+ |
| 5-axis flag | `requires_5_axis` | bool | ✓ | ✓ | **yes (heuristic)** | `aspect_ratio ≥ 6 AND fill_ratio < 0.15` |
| Thin-wall flag | `thin_wall_present` | bool | ✓ | ✓ | **yes (heuristic)** | `min(bbox) < 1.5 mm` |
| **Surface area** | — | — | ✗ | ✗ | **NOT COMPUTED** | needed for finishing-time; see §5.4 |

Key facts that shape the model:

- **STL is a triangle soup — no topology, ever.** Ervin's Bambu Lab STLs will *never* yield holes/pockets.
  So the model **must not depend on `features[]`.** It must work from bbox + volume + flags (+ surface area).
- **STEP v1 also emits empty `features[]`** — feature mining is deferred. So the geometry-only model is the
  correct target for *both* paths today; a future cut can *add* feature time on top when STEP mining lands.
- **The two flags are conservative bbox proxies**, not real geometry — fine as low-confidence inputs to a
  complexity score, not as hard gates.
- **Surface area is missing and we need it.** Adding it is cheap: STL = Σ ½·‖(v1−v0)×(v2−v0)‖ over triangles;
  STEP = `BRepGProp::SurfaceProperties`. S418 should add `surface_area_mm2` and bump schema v1→v2. Until then,
  the model falls back to **bounding-box surface area** `2(xy+yz+zx)` (computed in §5 examples this way).

---

## 4. CAD-CAM complexity matrix (auto-derived, no operator input)

Ervin: *"CAD-CAM design cost based on complexity 1–5 hours, 100 EUR … operator will be me, dumb as fuck."*
So this is a **one-time engineering/CAM-programming cost**, fully auto-derived, amortized across the batch.

### 4.1 Formula

```
cad_cam_hours = clamp( 1.0 ,  1.0 + Σ signal_i × weight_i ,  5.0 )
cad_cam_cost_total = cad_cam_hours × cad_cam_rate_eur_per_hour      (= 100 EUR/h)
cad_cam_cost_per_part = cad_cam_cost_total / quantity               (always amortized — it's programming, done once)
```

`fill_ratio = volume_mm3 / (bbox_x·bbox_y·bbox_z)` — how much of its bounding box the part fills. Low fill ⇒
deep pockets / sculpted surfaces / undercuts ⇒ harder CAM.

### 4.2 Proposed signal weights (day-1)

| Signal (from geometry) | Condition | + hours | Justification / benchmark |
|---|---|---:|---|
| **base** | every part | **1.0** | minimum CAM setup, fixturing plan, toolpath review — Hubs/Protolabs treat programming as a fixed startup cost. |
| **5-axis** | `requires_5_axis` | **+1.5** | 5-axis programming "command[s] $75–130/h due to programming complexity" (hotean 2025); the premium *is* the programming time. |
| **low fill (deep concavity)** | `fill_ratio < 0.30` | **+1.0** | lots of pocketing/3D toolpaths; the part is mostly air = much removal strategy. |
| **medium fill** | `0.30 ≤ fill_ratio < 0.60` | **+0.5** | moderate pocketing. |
| **thin wall** | `thin_wall_present` | **+0.5** | careful workholding, light finishing passes, deflection planning. |
| **large envelope** | `max(bbox) ≥ 200 mm` | **+0.5** | bigger fixturing, multi-setup, longer tryout. |
| **hard material** | grade is exotic (Ti/Inconel/Monel/superalloy) | **+0.5** | conservative feeds, tool-strategy iteration, scrap-cost aversion drives more sim time. |

Sum naturally lands in 1.0–5.0; the clamp is a guard, not the usual path. A plain prismatic part = **1.0 h**
(100 EUR). A complex thin-wall 5-axis exotic part = **4.0–5.0 h** (400–500 EUR). Matches Protolabs' "$500–
$1000+ in programming and fixturing" for a *highly* complex titanium part (we sit just under, by design).

> **Tuning note for Ervin:** at qty = 1 the full CAM hour is one part's burden (100 EUR floor on a one-off).
> That is *real* — programming a single part genuinely costs ~1 h of your time — but if 1-offs look scary you
> can lower `cad_cam_rate` or `base` hours. It's a single tunable.

---

## 5. Machining-time heuristic (geometry-driven)

Replaces the dead feature-graph path as the **primary** machining-time driver. Three terms: roughing (bulk
removal, volume-driven), finishing (surface passes, area-driven), setup (fixed per job).

### 5.1 Stock model (one definition, reused for material billing — §6.4)

```
V_bbox   = bbox_x · bbox_y · bbox_z                 (mm³)
V_stock  = V_bbox × (1 + scrap_factor)              scrap_factor repurposed as stock-oversize (propose 0.15)
V_removed_cm3 = max(0, V_stock − volume_mm3) / 1000
A_surf_cm2    = surface_area_mm2 / 100              (fallback: 2(xy+yz+zx)/100 until extractor adds it)
```

### 5.2 Time terms (minutes)

```
roughing_min  = V_removed_cm3 × machining_difficulty / MRR_rough_ref       MRR_rough_ref = 8.0 cm³/min @ difficulty 1.0
finishing_min = A_surf_cm2   × t_finish × machining_difficulty             t_finish = 0.08 min/cm²
setup_min     = setup_base_min + (requires_5_axis ? setup_5axis_min : 0)   setup_base = 20, setup_5axis = +25

machining_minutes = roughing_min + finishing_min      (per part; setup amortized separately, §5.5)
```

`machining_difficulty` is the **physically-correct** per-material time multiplier (Al ≈ 1.0, Ti ≈ 3.5,
Inconel ≈ 5.0) — see the 🔴 in §6.1. Effective roughing rate = `MRR_rough_ref / difficulty`, so aluminium
roughs at 8 cm³/min and titanium at ~2.3 cm³/min, matching the literature (roughing 0.1–1.0 in³/min ≈
1.6–16 cm³/min; aluminium high, titanium low — sciencedirect/cnccookbook MRR ranges; unit-power Al 0.5 vs
SS 2.0 HP/in³/min).

### 5.3 Cost

```
machining_cost = machining_minutes × machining_rate × tolerance.multiplier
               × THIN_WALL_TIGHT_TOL_BUMP(1.15)  if thin_wall_present && tol ≥ Tight
machining_rate = 1.6667 EUR/min  (= 100 EUR/machine-hour, Ervin's spec)
setup_cost     = (setup_min × machining_rate) / (qty ≥ threshold ? qty : 1)
```

### 5.4 Surface area dependency

Finishing is area-driven, so S418 should add `surface_area_mm2` to the extractor (trivial, §3). Until then the
**bbox-surface-area fallback** is used — it under-counts sculpted parts (real area > bbox area) but is safe and
monotone. All §5.6 examples are computed with the fallback to show the floor behaviour.

### 5.5 Day-1 machining constants

| Constant | Proposed | Rationale |
|---|---:|---|
| `machining_rate_eur_per_minute` | **1.6667** (100 EUR/h) | Ervin spec; mid-range EU shop rate (€35–120/h; Hungary low-cost end). |
| `MRR_rough_ref` (cm³/min @ diff 1.0) | **8.0** | conservative small-shop aluminium roughing; harder metals scale down via difficulty. |
| `t_finish` (min/cm²) | **0.08** | balances roughing vs finishing on typical parts; CMM/inspection still via tolerance table. |
| `setup_base_min` | **20** | one fixturing + tool-load + tryout per job. |
| `setup_5axis_min` | **+25** | extra 3+2/5-axis setup & probing. |
| `scrap_factor` (now stock-oversize) | **0.15** | 15% stock margin around the bbox; also the roughing-removal basis. |

### 5.6 Sanity checks

**A) 50×50×50 mm solid aluminium-6061 cube, qty 1, Standard tol.**
`V_bbox=125 cm³`, `V_part=125`, `V_stock=143.75`, `V_removed=18.75 cm³`, `A_surf=6·25=150 cm²`, diff 1.0.
- roughing = 18.75×1.0/8 = **2.3 min**; finishing = 150×0.08×1.0 = **12.0 min** → machining 14.3 min × 1.6667 = **23.9 EUR**
- material = 143.75 cm³ × 2.70 g/cm³ = 388 g × 5.5 EUR/kg = **2.13 EUR**
- CAM = 1.0 h × 100 = **100 EUR**; setup = 20×1.6667 = **33.3 EUR** (qty 1, unamortized)
- subtotal 159.4 → overhead 31.9 → margin (191.3)×0.35 = 67.0 → **TOTAL ≈ 258 EUR**.
  (Of which 100 EUR is one-off programming. Reasonable for a 1-off faced aluminium block.)

**B) 100×60×40 mm aluminium-6061 bracket, ~35% fill, qty 1, Standard tol.**
`V_bbox=240 cm³`, `V_part≈84`, `V_stock=276`, `V_removed=192 cm³`, `A_surf(bbox)=248 cm²`, diff 1.0.
- roughing 192/8 = **24 min**; finishing 248×0.08 = **19.8 min** → 43.8 min × 1.6667 = **73.0 EUR**
- material 276 cm³×2.70 = 745 g × 5.5 = **4.10 EUR**
- CAM: fill 0.35 → +0.5 → 1.5 h = **150 EUR**; setup 20 min = **33.3 EUR**
- subtotal 260.4 → overhead 52.1 → margin (312.5)×0.35 = 109.4 → **TOTAL ≈ 422 EUR**.

These pass the smell test (§7 compares to bureaus).

---

## 6. Material catalogue — 🔴 the inverted index, and rate review

### 6.1 🔴 BLOCKING: `machinability_index` is semantically inverted

The engine **divides** machining minutes by `machinability_index` and documents *">1 = easier (faster),
<1 = harder (slower)"* (`catalogue.rs:57`, `engine.rs:238`). But the **seed values are the opposite**:

| Grade | seeded `machinability_index` | seeded `carbide_life_mult` | physical reality | engine reading (÷ index) |
|---|---:|---:|---|---|
| 6061-T6 (easy) | **0.7** | 1.0 | easiest metal | treated as HARD (×1.43 time) ❌ |
| 7075-T651 | 0.9 | 1.1 | easy | hard ❌ |
| PEEK | 0.9 | 1.0 | easy | hard ❌ |
| 304 | 1.6 | 1.8 | moderate-hard | treated as EASY ❌ |
| 316 | 1.8 | 2.0 | moderate-hard | easy ❌ |
| Monel 650 | 3.0 | 3.5 | hard | very easy ❌ |
| Ti-6Al-4V | 3.5 | 4.0 | hard | very easy ❌ |
| **Inconel 718** | **5.0** | 6.0 | **hardest** | **cheapest to cut (÷5)** ❌❌ |

The seed author clearly entered **difficulty** (Inconel 5 = hardest) but the engine treats it as **ease**
(Inconel 5 = 5× faster). This has **never bitten** only because `machining_minutes` is always 0 today. **The
moment §5 turns machining time on, Inconel would be priced as the cheapest metal to machine and aluminium as
one of the hardest** — catastrophic and exactly the "bankrupt after ten jobs" failure Ervin fears.

**Resolution (rule 7 — pick one, don't blend; rule 13 — delete the dead one):**

- Introduce **`machining_difficulty`** as a first-class per-material multiplier (>1 = slower/harder), used by
  the §5 roughing/finishing terms via *multiplication*.
- **Re-seed it with physically-correct values** (below). They happen to track the already-correct
  `carbide_life_mult`, so the numbers are familiar.
- **Delete the inverted `machinability_index` divisor** from the engine and the catalogue (it has no live
  consumer once machining time is geometry-driven). This is the clean rule-13 move.

> Note: `carbide_life_mult` (6061=1.0 … Inconel=6.0) is **correctly ordered** today and can serve as the
> sanity anchor for the new difficulty column — but keep `machining_difficulty` a distinct, explicit field
> (tool-wear ≠ cycle-time, even if correlated).

### 6.2 Material rate review (current vs real-world)

Wholesale/retail small-quantity bar/plate, mid-2025 (USD ≈ ×0.92 EUR). Sources: peekchina, tbkmetal,
partsproto, suppliertitanium, Hubs material table ($25 Al block / $300 PEEK block as stock-form anchors).

| Grade | current `cost_per_kg_eur` | real-world small-qty | verdict | proposed |
|---|---:|---|---|---:|
| 6061-T6 | 6.0 | $3.5–5/kg bulk; retail small bar higher | OK, slightly low at retail | **5.5** |
| 7075-T651 | 9.0 | ~$8–12/kg | OK | **9.0** |
| 304 | 4.0 | ~$3–5/kg | OK | **4.0** |
| 316 | 6.0 | ~$5–8/kg | OK | **6.0** |
| Ti-6Al-4V (Gr 5) | 35.0 | aerospace plate $19–43/kg | OK | **35.0** |
| Inconel 718 | 50.0 | ~$40–80/kg | OK | **52.0** |
| Monel 650 | 40.0 | Monel ~$30–60/kg | OK | **40.0** |
| PEEK | 90.0 | rod retail $80–150/kg ($30–120 bulk) | OK | **90.0** |

**The material rates are already sound** — no shake-up needed. (The bigger material lever is the *volume
basis*, §6.4, not the per-kg rate.)

### 6.3 `machining_difficulty` proposed seed

| Grade | proposed `machining_difficulty` | basis |
|---|---:|---|
| PEEK | **0.8** | softer than aluminium, gummy but light cutting |
| 6061-T6 | **1.0** | reference |
| 7075-T651 | **1.2** | harder temper |
| 304 | **2.0** | work-hardening stainless |
| 316 | **2.2** | gummier than 304 |
| Monel 650 | **3.0** | nickel alloy |
| Ti-6Al-4V | **3.5** | low thermal cond., slow feeds |
| Inconel 718 | **5.0** | superalloy, hardest |

### 6.4 🟡 Material billed on part volume, not stock

Today material = `volume_mm3 × (1+scrap_factor)` — i.e. you bill for the *finished part* + 8%. But a CNC shop
**buys the block and cuts most of it to chips.** A bracket that's 35% of its bbox means you bought ~3× the
billed material. **Recommend billing material on `V_stock` (bbox-based, §5.1)** — the same stock definition
that drives roughing. This is consistent (one stock number), matches reality, and is especially important for
titanium's high buy-to-fly ratio. `scrap_factor` is repurposed as the stock-oversize margin (propose 0.15).

---

## 7. Real-world CNC benchmarks & sample-quote comparison

### 7.1 Bureau rate cards (surveyed)

| Source | Machine-hour | Setup | Programming | Material markup | Notes |
|---|---|---|---|---|---|
| **EU shops (general, hotean/engmotion 2025)** | €35–120/h (DE/CH high end) | — | — | 18–35% | Hungary sits at the low-cost end. |
| **3-axis (hotean 2025)** | $35–55/h garage; $65–85/h mid-shop (≈€32–78) | $50–200 | $50–150/h | 18–35% (25% typ.) | |
| **5-axis (hotean 2025)** | $75–130/h (≈€69–120) | | | | premium = programming complexity |
| **Swiss turning** | $60–95/h | | | | |
| **Protolabs** | not published; min part ~$65; "$500–1000+ programming/fixturing" for complex Ti | | | | instant-quote bureau, automation-amortized |
| **Xometry** | not published; CNC no MOQ; unit price drops ~88% from 1→1000 | | | | 5,000+ supplier network |
| **Hubs (Protolabs Network)** | instant quotes; "10 simple Al parts ≈ $3500" (≈$350/part qty10); 1→5 qty ≈ −50%/part | | | | |

> **Ervin's intended 100 EUR/machine-hour ≈ $108/h** sits *above* a Hungarian 3-axis shop's raw rate but is a
> reasonable *all-in* number (his rate folds machine + his time). It is *below* premium 5-axis bureau rates.
> Defensible.

### 7.2 Sample-quote comparison (Ervin proposed model vs bureaus)

⚠️ **Method note (rule 12 — fail loud):** I could **not** drive Protolabs/Xometry/Hubs *live* configurators
(they require CAD upload + login). The competitor columns below are **derived from the published rate cards
above**, not live quotes, and are **order-of-magnitude bands**, not exact. Treat deltas as directional.

Geometry assumptions stated per row. Ervin's totals use the §7/§8 day-1 parameter set.

| Example | Geometry assumed | **Ervin model (per part)** | Bureau band (per part) | Position |
|---|---|---:|---|---|
| **PEEK, 15 pcs, 50 mm cube** | bbox 50³, solid, fill 1.0, no flags, Std tol | **≈ €73** (mat 16.8 + mach 19.1 + setup 2.2 + CAM 6.7 → +OH/margin) | PEEK simple, qty15: ~$90–160 (€83–148) | **−12% to −51% under** premium bureaus → not frightening |
| **6061-T6, 1 pc, simple bracket** | bbox 100·60·40, 35% fill, Std tol | **≈ €422** (CAM 150 dominates at qty1) | 1-off custom Al bracket: ~$300–500 (€275–460) | **mid-band**, slightly high only from one-off CAM |
| **Ti-6Al-4V, 5 pcs, complex aero bracket** | bbox 150·100·60, 15% fill, 5-axis, thin-wall | **≈ €1,960 @ Std tol** · **≈ €2,885 @ Tight tol** | complex Ti aero, qty5: ~$2,000–5,000 (€1,840–4,600) | **low-to-mid band** → competitive on hard jobs |

Per-part build of the Ti example (Std tol): material 168.6 (stock 1035 cm³×4.43×35×1.05 exotic) · machining
936.3 (562 min: 394 rough + 168 finish) · setup 15 (45 min/5) · CAM 90 (4.5 h×100/5) → subtotal 1209.9 →
overhead 242 → margin 508 → **1960**. At Tight tol the ×1.4 tolerance mult + ×1.15 thin-wall bump on machining
lift it to ~2885.

**Reading:** day-1 parameters put Ervin **below** the premium bureaus on simple/plastic parts (wins price-
sensitive jobs without alarming customers) and **competitive but profitable** on hard exotic jobs (clears
margin instead of underpricing the killer Ti removal). The one place to watch is **one-off CAM at qty 1**
(100–150 EUR floor); that's real programming cost, tunable if it deters.

---

## 8. Day-1 parameter set ("not frightening, not bankrupting")

### 8.1 Tunables / parameters

| Parameter | Current | **Proposed day-1** | Justification |
|---|---:|---:|---|
| `machining_rate_eur_per_minute` | 1.0 (hardcoded) | **1.6667** (100 EUR/h) | Ervin spec; move to DB `quoting_parameters`. |
| `cad_cam_rate_eur_per_hour` | — (none) | **100** | Ervin spec; new param. |
| `cad_cam_base_hours` | — | **1.0** | min programming. |
| `MRR_rough_ref_cm3_per_min` | — | **8.0** | small-shop aluminium roughing baseline. |
| `t_finish_min_per_cm2` | — | **0.08** | finishing pass density. |
| `setup_base_min` | — (was feature-derived, =0) | **20** | one fixturing/tryout per job. |
| `setup_5axis_min` | — | **25** | extra 5-axis setup. |
| `scrap_factor` (→ stock-oversize) | 0.08 | **0.15** | stock margin = roughing & material basis. |
| `overhead_factor` | 0.20 | **0.20** | keep; in line with bureau overheads. |
| `profit_margin_base` | 0.35 | **0.35** | keep; mid of 18–35% markup band, healthy. |
| `min_margin` (floor) | 0.10 | **0.10** | keep; refuse loss-makers. |
| `exotic_material_tax` | 0.05 | **0.05** | keep; small surcharge on Ti/Inconel. |
| `setup_amortization_threshold` | 5 | **5** | keep. |
| 5-axis machine-rate multiplier | — (flag unused) | **none day-1** (note ×1.5 future) | keep simple; benchmarks justify a later 5-axis rate premium. |
| `THIN_WALL_TIGHT_TOL_BUMP` | 1.15 | **1.15** | keep. |

### 8.2 Material catalogue

| Grade | density | `cost_per_kg_eur` (keep) | **`machining_difficulty` (NEW)** | retire `machinability_index` |
|---|---:|---:|---:|---|
| PEEK | 1.30 | 90.0 | **0.8** | ✅ delete |
| 6061-T6 | 2.70 | 5.5 | **1.0** | ✅ |
| 7075-T651 | 2.81 | 9.0 | **1.2** | ✅ |
| 304 | 8.00 | 4.0 | **2.0** | ✅ |
| 316 | 8.00 | 6.0 | **2.2** | ✅ |
| Monel 650 | 8.80 | 40.0 | **3.0** | ✅ |
| Ti-6Al-4V | 4.43 | 35.0 | **3.5** | ✅ |
| Inconel 718 | 8.19 | 52.0 | **5.0** | ✅ |

### 8.3 Keep as-is
Tolerance multipliers (0.9/1.0/1.4/1.9/2.8 + inspection minutes), default tolerance Standard, default qty 1.

---

## 9. Implementation notes for S418

### 9.1 This is a model change, not a rename-only cut

Two things land together: (a) **new geometry-driven cost model** (machining time + CAD-CAM cost), and (b) the
**labor→MACHINING vocabulary rename**. Plus the 🔴 difficulty-inversion fix. Doing the rename without the model
is pointless (the line stays 0); doing the model without fixing the inverted index is dangerous (§6.1).

### 9.2 Files to touch (rough grep-based estimate)

| File | Change | ~LOC |
|---|---|---:|
| `crates/aberp-quote-engine/src/engine.rs` | new roughing/finishing/setup/CAM terms; remove `machinability_index` divisor; `[labor]`→`[machining]` log lines | 120 |
| `crates/aberp-quote-engine/src/breakdown.rs` | add `machining_cost` (serde-rename shim `labor_cost`) + `cad_cam_cost`; comments | 25 |
| `crates/aberp-quote-engine/src/catalogue.rs` | add params (cad_cam_rate, MRR, t_finish, setup mins, move machining_rate); add `machining_difficulty`, drop `machinability_index` | 40 |
| `crates/aberp-cad-extract-wrapper/` + Python extractor | add `surface_area_mm2`; bump schema **v1→v2** (both sides same diff) | 60 |
| `apps/aberp/src/quoting_tunables.rs` | seed new params + DB migration (machining_rate column, new knobs) | 70 |
| `apps/aberp/src/quoting_materials.rs` | re-seed `machining_difficulty`, migration, drop old column | 45 |
| `apps/aberp/src/quote_pricing_pipeline.rs` | wire machining_rate from DB (delete hardcode 1.0), convert new fields, payload field rename | 45 |
| `crates/aberp-quote-pdf/src/lib.rs` | "Labor"→"Machining" row; add "CAD-CAM / Tervezés" row | 25 |
| `apps/aberp-ui/ui/src/lib/api.ts`, `pricing-job-detail.ts` | rename field/labels (HU/EN), add CAD-CAM row | 25 |
| ABERP-site storefront | breakdown key compat (`labor_cost`→`machining_cost`, read both during transition) | 15 |
| tests + golden fixtures (engine golden/branches/property, SPA, rerender daemon) | recompute all | 150 |
| **Total** | | **≈ 620 LOC** |

### 9.3 The labor→MACHINING rename map (from the full sweep)

**Breaking (wire/persisted — need serde-compat shim or migration):**
- `QuoteBreakdown.labor_cost` (`breakdown.rs:26`) — serialized to `quote_pricing_jobs.breakdown_json` *and*
  posted to storefront. Rename to `machining_cost` with `#[serde(rename="labor_cost")]` for one transition
  release, or coordinate a clean break with the storefront.
- `QuotePricingPricedPayload.labor_cost_eur` (`quote_pricing_pipeline.rs:2538`) — **audit-ledger payload key**,
  immutable history. Emit both keys for one release, then retire.
- `PricingBreakdownView.labor_cost` (`api.ts:3308`) — SPA wire mirror; follows the serde rename.
- Test goldens/fixtures: `golden.rs:36,62`, `branches.rs:28,99,100`, `property.rs:100`,
  `pricing-job-detail.test.ts:138`, `quote_pdf_rerender_daemon.rs:1155`.

**Safe (display only — free rename):**
- Reasoning-log prefixes `"[labor]"` (`engine.rs:247,262,269,289,329`).
- PDF label `"Labor"` (`aberp-quote-pdf/src/lib.rs:441`).
- SPA label `"Munkadíj / Labor"` (`pricing-job-detail.ts:145`) and Svelte help text
  (`QuotingParametersForm.svelte:163`).
- Code comments/variables in `engine.rs`, `breakdown.rs`.

**Hungarian wording:** current "Munkadíj" (work-fee). For MACHINING use **"Megmunkálás"** (HU for machining);
add CAD-CAM row as **"Tervezés (CAD-CAM)"**. Confirm with Ervin.

### 9.4 Risk factors

1. **🔴 difficulty inversion (§6.1)** — must ship in the same cut as machining time, or quotes invert.
2. **Golden fixtures** — every priced-quote golden changes numbers; recompute, don't hand-tweak (rule 9: the
   tests must still be able to fail on logic change).
3. **`labor_cost` is a live wire contract** — storefront + audit ledger persist it. Serde-compat shim or
   coordinated break; do not rename naively.
4. **Schema v1→v2** for `surface_area_mm2` — Rust `EXPECTED_SCHEMA_VERSION` and Python `SCHEMA_VERSION` must
   bump in the same diff or the wrapper refuses all graphs (it's a blast door).
5. **One-off CAM cost** may look high at qty 1 — surface to Ervin; it's tunable, not a bug.
6. **Surface-area fallback** under-counts sculpted parts until the extractor adds real area — finishing time is
   a floor, not exact, until v2 lands.
7. **Material on stock volume (§6.4)** raises prices for hollow/low-fill parts — intended, but a visible jump
   from today's part-volume billing; mention in release notes.

---

## 10. Open decisions for Ervin (override any of these)

1. **Incoterm:** EXW day-1 (FCA-seller-premises for non-EU later). OK?
2. **Machining rate** 100 EUR/h flat, no 5-axis premium day-1. Add ×1.5 for 5-axis now or later?
3. **CAD-CAM** 1–5 h × 100 EUR, amortized across qty. Accept the qty-1 one-off floor (~100–150 EUR)?
4. **Material billed on stock (bbox) volume** instead of part volume — accept the price rise on hollow parts?
5. **`machining_difficulty` seed** values (§6.3) — adjust any?
6. **Day-1 constants** (MRR_rough 8, t_finish 0.08, setup 20/+25, stock-oversize 0.15) — these are the knobs
   you'll tune in live production; comfortable starting here?

---

### Sources

- Incoterms 2020 EXW: ICC Academy (academy.iccwbo.org/incoterms — EXW vs FCA), Trade Finance Global
  (tradefinanceglobal.com/incoterms/ex-works-exw), iContainers, Shipping Solutions.
- CNC shop rates 2025: hotean.com (CNC Machining Shop Rates in 2025), engmotion.com (In-Depth Analysis of CNC
  Machining Costs), davantech.com.
- Bureau pricing: protolabs.com (FAQs / Understanding CNC Manufacturing Costs), hubs.com (CNC cost reduction;
  knowledge base), xometry.com (CNC machining service; xometry.pro Cost of CNC Machining), rapiddirect.com,
  unionfab.com.
- Material removal rate / machining time: sciencedirect.com (Material Removal Rate overview), cnccookbook.com
  (MRR), cadem.com, en.wikipedia.org/wiki/Material_removal_rate, firgelliauto.com MRR calculator.
- Material prices: peekchina.com (PEEK vs titanium), tbkmetal.com (aluminium cost guide), partsproto.com
  (cost of titanium), suppliertitanium.com, aluminumsheet.net.
