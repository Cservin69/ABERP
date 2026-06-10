# Aerospace Certification Gap Analysis (S330)

*Research + doc-only session. No code touched. Prepared 2026-06-10 for Ervin / Áben Consulting KFT.*

*Question answered: **what must Áben add to ABERP (and the surrounding workflow) before an AS9100D auditor walks the floor?***

---

## Executive Summary

**Where ABERP is strong.** ABERP already owns the single hardest thing to build and the single thing auditors trust least when it's bolted on late: a **tamper-evident, hash-chained, append-only audit ledger**. The `audit-ledger` crate is a real SHA-256 chain over canonical CBOR (`crates/audit-ledger/src/lib.rs:3-8`), mirrored to an fsync'd append-only file (`crates/audit-ledger/src/mirror.rs:1-30`), verified fail-loud with the precise tampered `seq` (`crates/audit-ledger/src/chain/verify.rs:1-60`), spanning **83 EventKinds** across quoting, fiscal invoicing, inventory, and Stage-3 manufacturing (`crates/audit-ledger/src/entry/event_kind.rs:234`). This is, functionally, a 21 CFR Part 11 §11.10(e) audit-trail engine that the aerospace world would normally pay a QMS vendor six figures for. The manufacturing backbone — work-orders → routing → QA pass/fail → dispatch — is also genuinely built and wired (`crates/aberp-qa`, `crates/aberp-work-orders`, `crates/aberp-dispatch`), and the invoice storno/modification flow is a working immutable revision chain (`apps/aberp/src/issue_storno.rs`, `issue_modification.rs`). ABERP is not starting from zero; it is starting from an unusually good *records spine* with the wrong *content* on it.

**Where ABERP is weak.** Every aerospace-specific quality surface is absent from shipping code. There is **no lot / heat / serial / batch identity** anywhere — material is a fungible per-grade scalar (`apps/aberp/src/material_inventory.rs:225-235`), so part-to-material genealogy is structurally impossible today. There is **no personnel/qualification model and no electronic signature** — "operator" is an unverified login string (`crates/aberp-inventory/src/types.rs:172-189`, `crates/aberp-qa/src/repository.rs:93`). There is **no NCR, no CAPA, no FAIR module, no calibration register, no certificate-of-conformance / mill-cert capture, no special-process tracking** — word-boundary greps return zero source hits for every one of these. The **Approved Supplier List** required by AS9100D §8.4 is a three-value `Customer | Supplier | Both` flag (`apps/aberp/src/partners.rs:114-119`). **Configuration management** (§8.1.2) does not exist: BOMs are flat and versioned only by a soft-retire timestamp (`crates/aberp-work-orders/src/repository.rs:57-68`), with no part revision, effectivity, or ECO/ECN. And there is **no access-control model beyond a single all-or-nothing tenant Bearer token** (`apps/aberp/src/serve.rs:19-22`) — which means the ITAR/EAR nationality-gated technical-data controls a US-defense prime would demand are 100% greenfield.

**What to build first.** The sequencing matters more than the list. Three things are *structural* — they cannot be retrofitted, because they must be captured at the moment of the first aerospace job or the genealogy is gone forever: **(1) a personnel identity + electronic-signature layer bound to the existing ledger** (every downstream quality record needs an attributable, non-repudiable signer); **(2) lot/heat/serial traceability** woven through receiving → material → work-order → dispatch; **(3) an Approved Supplier List with material-certificate capture**, because Áben *will* outsource special processes to NADCAP-accredited subs from day one and must flow down and verify those approvals. Everything else — NCR/CAPA, FAIR, calibration, configuration management — is important but *bolt-on-able* once those three primitives exist. AS9100D itself is mostly a process/documentation exercise (quality manual, internal audit, management review) that no amount of code replaces; but the code investments above are exactly the ones that turn ABERP from an invoicing tool into an auditable aerospace QMS, and the ones an auditor will photograph on the wall.

---

## Standards Landscape

A scaling precision-machining shop selling to aerospace primes faces a layered stack of standards. A recurring strategic theme runs through all of them: **a Tier-2/3 machine shop almost never holds the heavyweight approvals in its own right** — it works *under* a prime's umbrella and *flows down* requirements to accredited subcontractors. Knowing exactly where the "hold it yourself" line sits is what keeps Áben from over-investing.

### AS9100D — the must-have QMS standard

AS9100D (released September 2016 by SAE on behalf of the IAQG) is *the* aerospace quality-management-system standard. It incorporates **ISO 9001:2015 verbatim** and overlays roughly a hundred aerospace-, defense-, and space-specific requirements, following ISO's Annex SL ten-clause structure (auditable content in clauses 4–10). [[Smithers — AS9100 Requirements](https://www.smithers.com/resources/2026/february/smithers-summarizes-as9100-requirements)]

The clauses that turn into ABERP requirements:

- **§8.1.2 Configuration Management** — *"plan, implement, and control a process for configuration management"* to preserve product-definition integrity across the lifecycle. Auditors trace an ECO/ECN from requirement → drawing revision → work order → as-built record. [[IAQG — AS9100D risk requirements](https://iaqg.org/crucial-insights-into-as9100d-risk-requirements-for-aerospace-and-defense/)]
- **§8.1.4 Prevention of Counterfeit Parts** — controls for acquiring from original/authorized manufacturers, methods to verify/authenticate, and **quarantine and reporting** of suspect parts. [[Apogee QMS — §8.1.4](https://aqms.space/2023/05/as9100-8-1-4-prevention-of-counterfeit-parts)]
- **§8.4 Control of Externally Provided Processes, Products and Services** — maintain a **register of external providers (the Approved Supplier List)** with approval status and scope; ensure **flowdown** of all applicable requirements (key characteristics, test/inspection, right-of-access for organization/customer/regulator, **flowdown to sub-tier suppliers**, counterfeit-part prevention); verify purchased product.
- **§8.5.1.3 Verification after Production Stoppage** — the hook into First Article Inspection (AS9102).
- **§8.5.2 Identification and Traceability** — *"use suitable means to identify outputs … identify the status of outputs with respect to monitoring and measurement requirements … control the unique identification of the outputs when traceability is a requirement, and retain the documented information necessary to enable traceability."* The clause NOTE extends traceability to *all products manufactured from the same batch of raw material* and *a sequential record of a product's manufacture/assembly/inspection/test history.* [[Apogee QMS — §8.5.2](https://aqms.space/2023/06/as9100-8-5-2-identification-and-traceability)]
- **§7.1.5.2 Measurement Traceability** — calibration/verification against standards traceable to national/international measurement standards, with **recall of out-of-calibration equipment** and reassessment of prior measurements.
- **§7.2 Competence** — determine necessary competence and **retain documented information as evidence of competence** (training records).
- **§7.5 Documented Information** — identification, version control, protection from loss of integrity, controlled distribution, and **retention/disposition**.
- **§8.7 Control of Nonconforming Outputs** — identify and segregate nonconforming product; disposition (use-as-is/rework/scrap) must **define responsibility/authority** and, where required, **obtain customer/regulatory authority approval** (the NCR record).
- **§10.2 Nonconformity and Corrective Action** — react, root-cause, implement, verify effectiveness, flow down to suppliers, act on a timely basis (CAPA).
- **§9.2 Internal Audit / §9.3 Management Review** — planned audit programme and a management review whose inputs explicitly include **on-time-delivery performance** and external-provider performance.

AS9100D prescribes **no single numeric retention period**; §7.5.3 requires the organization to *determine* retention from customer, statutory, and regulatory requirements. Certification is a **two-stage audit** by an accredited certification body: **Stage 1** (1–2 days, readiness/documentation review — confirms the documented QMS exists and that at least one cycle of internal audit + management review has run) and **Stage 2** (1–2 months later, a full process-level conformity audit). New organizations typically spend **9–12 months implementing** before Stage 2; the commonly cited end-to-end figure is **12–18 months**. [[QMII — AS9100D step-by-step](https://www.qmii.com/achieving-as9100-rev-d-certification-a-step-by-step-guide/)] [[Smithers — what to expect during an AS9100 audit](https://www.smithers.com/resources/2026/march/summarizes-what-to-expect-during-an-as9100-audit)]

### AS9102 Rev C — First Article Inspection

AS9102 defines the **First Article Inspection Report (FAIR)** — a documented, independent verification that a representative production item (made with production tooling, processes, and personnel) meets **all** drawing/spec/DPD requirements. It uses three forms: **Form 1 — Part Number Accountability** (part/serial/FAIR identifier, drawing rev, and Rev C's now-mandatory **Field 14 "Reason for Full/Partial FAI"**, plus reviewer/approver as **different individuals**); **Form 2 — Product Accountability** (material specs, **special processes** such as heat-treat/plating/NDT, functional testing, with the corresponding certifications and CoC numbers); and **Form 3 — Characteristic Accountability** (one row per design characteristic: characteristic number, drawing reference location, requirement nominal+tolerance, and the **actual measured value**). Every inspectable characteristic on the drawing/model is enclosed in a uniquely numbered **balloon** that maps one-to-one to a Form 3 row. A **full FAIR** is triggered by a new part, a fit/form/function design change, a change of source/process/tooling/inspection method, or a **lapse in production (commonly two years)**; a **partial/delta FAIR** re-verifies only the affected characteristics. [[Ideagen InspectionXpert — AS9102 FAIR](https://www.inspectionxpert.com/fai/as9102)] [[DISCUS — AS9102 Rev C changes](https://www.discussoftware.com/news/as9102-rev-c-what-you-need-to-know/)] [[SAE AS9102 (normative)](https://www.sae.org/standards/content/as9102/)]

### AS9145 — APQP / PPAP

AS9145 establishes **Advanced Product Quality Planning** (5 phases: Planning → Product Design → Process Design → Product & Process Validation → Ongoing Production) and the **Production Part Approval Process** (11 aerospace elements: design records, design risk analysis/DFMEA, process flow, PFMEA, control plan, MSA, initial process studies, packaging/labeling approval, **FAIR per AS9102**, customer-specific requirements, and the **PPAP approval form / Part Submission Warrant**). It is **not separately certifiable** — when a customer flows it down, conformance is assessed *within* the AS9100 audit. The key distinction: **FAI approves the *part*; PPAP approves the *process*.** [[RGBSI — AS9145 11 elements / 5 phases](https://blog.rgbsi.com/as9145-requirements-for-aerospace-defense)] [[simpleQuE — AS9145](https://www.simpleque.com/as9145-apqp-and-ppap-for-aerospace/)]

### NADCAP — special-process accreditation

NADCAP (run by PRI/SAE) is a **process-level** accreditation, complementary to AS9100. AS9100 certifies the company-wide QMS; NADCAP accredits a specific **"special process"** — one whose output conformity cannot be fully verified by later inspection of the part (you cannot non-destructively confirm a heat-treat microstructure or a weld's fusion). It administers 24 critical-process programs via Audit Criteria documents: **AC7102** (heat treat), **AC7110** (welding/brazing), **AC7114** (NDT), **AC7108** (chemical processing — anodize/passivation/plating), **AC7117** (surface enhancement / shot peening), **AC7116** (nonconventional machining — EDM/laser), **AC7101** (materials testing), with **AC7004/AC7006** the underlying QMS baseline. [[PRI — NADCAP](https://www.p-r-i.org/nadcap)] [[mpofcinci — complete NADCAP guide](https://mpofcinci.com/blog/complete-nadcap-guide/)]

**The strategic point for Áben:** general CNC machining is *not* a NADCAP special process — there is no NADCAP requirement merely to mill or turn. **You only need NADCAP for special processes you perform in-house.** Most aerospace parts touch a special process, but the standard play for a scaling shop is to **outsource those to NADCAP-accredited subcontractors** and keep machining in-house. The prime still holds *you* accountable for that supply chain, so you must flow down the requirements, verify the subs' accreditation scope on eAuditNet, and keep that approved-supplier control inside your AS9100 QMS. [[Pioneer Service — NADCAP for precision machining](https://pioneerserviceinc.com/blog/nadcap-certification-and-how-it-applies-to-the-precision-machining-industry/)]

### ITAR / EAR / EU 2021/821 — export controls

**ITAR** (22 CFR 120-130, US State/DDTC, controlling USML defense articles + technical data) and **EAR** (15 CFR, US Commerce/BIS, controlling dual-use CCL items) reach a Hungarian shop whenever a US prime sends **US-origin technical data** for a controlled article, typically via contractual flowdown. The load-bearing concept for Áben is **"deemed export"**: releasing controlled technical data **to a foreign person — even inside one facility — is an export to every country of that person's citizenship.** A mixed-nationality workforce accessing controlled drawings can itself be a violation absent authorization. [[eCFR 22 CFR 120](https://www.ecfr.gov/current/title-22/chapter-I/subchapter-M/part-120)] [[Cofactr — ITAR for manufacturers](https://www.cofactr.com/articles/a-practical-guide-to-itar-compliance-for-manufacturers-and-engineers)] This translates to concrete ERP requirements: **access control gated by nationality** (not just role), **segregation** of controlled data, **encryption** at rest/in transit, an **immutable access audit trail**, and **data residency** control. The EU analogue is **Regulation (EU) 2021/821** (the Dual-Use Regulation, in force 9 Sept 2021), directly applicable in Hungary, which adds its own quasi-deemed-export controls on technical assistance — but it is **not a substitute** for ITAR where US-origin data is involved. [[EUR-Lex 2021/821](https://eur-lex.europa.eu/eli/reg/2021/821/oj/eng)]

### EASA/FAA Part 21 & 145 — not held by sub-tier shops

**Part 21** = design/production organisation approval (POA); **Part 145** = maintenance/repair (MRO) approval. A POA holder's quality system must ensure conformity of parts produced by the organization *and its subcontractors* — i.e. **a sub-tier machine shop works under the prime's POA**, not its own. EASA explicitly confirms raw-material and detail-part suppliers *"do not need and cannot obtain"* a POA; FAA mirrors this via supplier-control systems. **Recommendation: Áben should not pursue Part 21/145 itself** — it invests in AS9100 + flowdown discipline, which is exactly what lets a POA holder fold it into their approval scope. [[EASA — Production Organisation Approvals](https://www.easa.europa.eu/en/domains/aircraft-products/production-organisations-approvals)] [[EASA FAQ 19007](https://www.easa.europa.eu/en/faq/19007)]

### The specialty standards (flowed down, ERP-relevant)

- **Counterfeit prevention — AS5553** (electronic parts) and **AS6174** (non-electronic *materiel, including raw metal stock*). AS6174 is directly load-bearing for a machine shop buying bar/plate: it requires traceability to the OCM/OEM via a **Mill Test Report / Material Test Report** and mill certificate. Per **EN 10204**, a **Type 3.1 MTC** carries the manufacturer's own heat-specific test data and **Type 3.2** adds independent witness; a bare **Type 2.1 Certificate of Conformance** is a declaration with *no test data* and "cannot be relied on to confirm a part is genuine." [[SAE AS5553](https://www.sae.org/standards/as5553-counterfeit-electronic-parts-avoidance-detection-mitigation-disposition)] [[EN 10204 3.1 vs 3.2](https://blog.projectmaterials.com/epc-projects/testing-inspection/mill-test-certificates-3-1-2/)]
- **FOD — AS9146** (flowed via AS9100) and **NAS412**: a documented FOD-prevention program scaled by risk assessment, covering area designation, **tool accountability** (check-in/out, shadow boards, end-of-task counts), housekeeping, consumables control, training, and a reporting loop. [[AS9146 (SAE)](https://saemobilus.sae.org/standards/as9146-foreign-object-damage-fod-prevention-program-requirements-aviation-space-defense-organizations)] [[NAS412 (FODbag)](https://fodbag.com/fod-nas412/)]
- **Configuration management — ANSI/EIA-649B**: five functions (planning/management, **identification**, **change management**, **status accounting**, **verification/audit**) operating on **configuration items**, **baselines**, and **effectivity**. Maps directly to part-number+revision (identification), released BOM (baseline), ECO/ECN (change management), as-built history (status accounting), FAI (verification). [[Wikipedia — EIA-649](https://en.wikipedia.org/wiki/EIA-649_National_Consensus_Standard_for_Configuration_Management)] [[SAE EIA649B](https://www.sae.org/standards/content/eia649b/)]
- **Electronic records — 21 CFR Part 11** (the de-facto benchmark; aerospace QMS systems are held to equivalent expectations): §11.10(e) **secure, computer-generated, time-stamped audit trails** that record create/modify/delete without obscuring prior data and are retained as long as the record; §11.50 **signature manifestations** (printed name + UTC timestamp + **meaning** — review/approval/authorship); §11.70 **signature/record linking** so signatures cannot be excised or transferred (non-repudiation). [[Cornell LII 21 CFR 11.10](https://www.law.cornell.edu/cfr/text/21/11.10)] [[Cornell LII 21 CFR 11.50](https://www.law.cornell.edu/cfr/text/21/11.50)]
- **DPD / Model-Based Definition — ASME Y14.41-2019**: the annotated 3D model + PMI *is* the design authority. A supplier must control **model integrity** (correct, uncorrupted, correct revision), **derived-from-authority verification** (any STEP/neutral derivative verified against the authority model — note that a pure, hash-keyed feature-extraction step is essentially this control), PMI/datum revision control, and **ballooning against model PMI** for FAIR. [[ASME Y14.41](https://www.asme.org/codes-standards/find-codes-standards/y14-41-digital-product-definition-data-practices)]
- **Record retention**: AS9100D sets no number; regulatory floors are short (FAA 14 CFR 91.417: until superseded or 1 year; EASA Part-145: 3 years), but **life-limited parts** drive the long tail (EASA M.A.305: life of the component — "back-to-birth" traceability), and prime flowdowns commonly demand **life-of-the-part / life-of-the-aircraft-type + N years** — practical ranges of 7–15 years for typical work and 30–40 for safety-critical. [[EASA technical records FAQ](https://www.easa.europa.eu/en/the-agency/faqs/technical-records)] [[back-to-birth traceability](https://sassofia.com/blog/easafaa-life-limited-parts-back-to-birth-traceability/)]

---

## ABERP Current-State Inventory

All evidence from the worktree at `/Users/aben/Documents/Claude/Projects/ABERP-pr30` on branch `main`, read-only.

**The records spine (strong).** `audit-ledger` is a genuine tamper-evident hash chain. The 12-field `Entry` (`crates/audit-ledger/src/entry/mod.rs:30-44`) carries `seq`, `prev_hash`, `time_wall`, `time_mono`, `actor`, `binary_hash`, `tenant_id`, `kind`, `payload`, `idempotency_key`, and the computed `entry_hash`. Hashing is SHA-256 over a **canonical CBOR encoding** pinned to one place (`crates/audit-ledger/src/canonical.rs:1-30`); `verify_chain` walks order + chain-link + per-entry integrity and fails loud at the first divergence (`crates/audit-ledger/src/chain/verify.rs:24-58`). The DuckDB table is append-only with `UNIQUE(seq)`/`UNIQUE(id)`/`CHECK(seq>=1)` (`crates/audit-ledger/src/storage/schema.rs:23-39`) and is shadowed by an fsync'd JSON-Lines mirror reconciled post-commit (`crates/audit-ledger/src/mirror.rs:18-30`). **83 EventKinds** span quote (27), invoice (18), mes (13), system (12), inventory (4), email (3) (`crates/audit-ledger/src/entry/event_kind.rs:234`). This is a Part-11-grade audit-trail engine. Its limits: the `actor` is a free `VARCHAR` (`storage/schema.rs:29`), not a bound, attributable identity; there is **no electronic-signature manifestation** (name+timestamp+meaning linked to a record per §11.50/§11.70); external attestation checkpoints are explicitly **deferred** (`lib.rs:29`); and there is **no retention/archival policy** in the application layer (a repo-wide grep for `retention|purge|ttl|expire|archive` finds only an unrelated `seller.toml` backup rotation and an email-outbox TTL).

**Invoice revision control (strong, but invoice-only).** The storno (full reversal) and modification (correction) flows are real (`apps/aberp/src/issue_storno.rs`, `apps/aberp/src/issue_modification.rs`). A correction is itself a new invoice with its own ledger entries plus a **chain-link** entry carrying the base invoice id and a `modificationIndex` (`crates/audit-ledger/src/entry/event_kind.rs:287-321`); the base invoice's state is *derived from the existence of the link entry — the base row is never mutated*. This is a working immutable original→correction→storno version chain — but hard-wired to the fiscal-invoice entity, with no equivalent for engineering documents, drawings, or routings.

**Manufacturing backbone (built and wired).** `aberp-work-orders` owns WO + 1-level BOM + linear routing (`crates/aberp-work-orders/src/lib.rs:5-12`), with real routes in `serve.rs`. `aberp-qa` is the closest existing inspection record: one `qa_inspections` table auto-created on routing-op completion, with `QaDecision = Pass|Fail|Rework|Dispose` and a row carrying `decided_by`, `reason`, and a single freeform `measurement: Option<String>` (`crates/aberp-qa/src/repository.rs:93-95`). `aberp-dispatch` flips Drafted→Shipped and spawns a draft invoice. `aberp-inventory` is an append-only product-side `stock_movements` ledger. `aberp-mes` is a **NoopAdapter framework stub** — the "machine talks to ERP" layer named for Renishaw/hardware exists only as a trait + registry (`crates/aberp-mes/src/lib.rs:27-39`).

**The aerospace content (absent).** Word-boundary, source-only greps return **zero hits** for: NCR, nonconform, CAPA, corrective action, FAIR/first-article, calibration, inspection record, certificate of conformance, mill/material cert, heat number, weld, NDT, special process, FOD, training, qualification, competence, e-signature, effectivity, baseline, ECO/ECN, ITAR, EAR, nationality, citizenship. All such language lives only in `docs/research/stage3/` and ADRs — none in shipping Rust/Python/Svelte.

---

## Gap Analysis

Scope estimates: **S** ≈ 1 session, **M** ≈ 5 sessions, **L** ≈ 10+ sessions. Sequencing: **MUST** (before Stage 1 audit) / **EXEC** (manufacturing-execution, once the floor lights up) / **FLOW** (customer-specific flowdown) / **CI** (continuous-improvement, nice-to-have).

### 1. Records + Audit Trail — 🟢 Strong (🟡 for signatures/retention)

**Requirement:** AS9100D §7.5.3 (control, protection from loss of integrity, retention/disposition); 21 CFR Part 11 §11.10(e) tamper-evident time-stamped audit trail, §11.50/§11.70 attributable linked signatures.
**Current state:** SHA-256 hash chain + canonical CBOR + append-only DuckDB + fsync'd mirror + fail-loud verify (`crates/audit-ledger/src/lib.rs:3`, `chain/verify.rs:24`, `mirror.rs:18`, `storage/schema.rs:23`). 83 EventKinds.
**Missing:** (a) electronic-signature manifestation — `actor` is a free string (`storage/schema.rs:29`), not name+timestamp+meaning bound non-repudiably to a record; (b) a documented/enforced **retention policy**; (c) external attestation checkpoints (deferred, `lib.rs:29`).
**Scope:** S–M (the mechanism exists; signatures + retention metadata are additive EventKinds/fields). **Sequencing: MUST** — the signature layer gates every downstream quality record.

### 2. Traceability — Lot / Serial / Heat / Batch — 🔴 Missing

**Requirement:** AS9100D §8.5.2 ("control the unique identification of outputs when traceability is a requirement … all products manufactured from the same batch of raw material … a sequential record of manufacture/assembly/inspection/test history"); AS6174 heat/lot traceability via MTR; EASA back-to-birth.
**Current state:** material is a fungible per-grade scalar — `inventory_balances` keyed on `(tenant_id, material_grade)` with `on_hand/reserved/committed/consumed` quantities (`apps/aberp/src/material_inventory.rs:225-235`); `inventory_reservations.quote_id` links a *quantity of a grade* to a quote (`:240`), not a heat to a serial. `aberp-inventory` explicitly disclaims lot/serial (`crates/aberp-inventory/src/lib.rs:15-17`); `aberp-qa` defers per-unit serial to v2 (`crates/aberp-qa/src/lib.rs:29-31`).
**Missing:** heat/lot identity on receiving, a serial/lot on the work-order output, and the genealogy join (heat → material issue → WO → QA → dispatch → part serial).
**Scope:** L (threads through receiving, material, WO, QA, dispatch; new tables + every producer). **Sequencing: MUST (structural)** — genealogy is unrecoverable retroactively; it must be captured at the first aerospace job.

### 3. Document / Configuration Control & Revision — 🔴 Missing (🟡 mechanism exists)

**Requirement:** AS9100D §8.1.2 + ANSI/EIA-649B five CM functions; part-number+revision (identification), released BOM (baseline), ECO/ECN (change management), effectivity, as-built (status accounting).
**Current state:** the *mechanism* exists for invoices (immutable chain-link revisions, `issue_storno.rs`/`issue_modification.rs`), but BOMs are flat and versioned only by `retired_at` soft-retire (`crates/aberp-work-orders/src/repository.rs:57-68`, `lib.rs:9-12`); products carry no revision column (`apps/aberp/src/products.rs:236-247`). Zero hits for effectivity/baseline/ECO/ECN.
**Missing:** part revision + effectivity, a baseline snapshot, ECO/ECN change control with disposition, where-used, and revision-controlled engineering documents/drawings.
**Scope:** M–L. **Sequencing: MUST** — configuration control is a headline §8.1.2 audit item; the invoice chain proves the team can build the immutable-revision pattern.

### 4. Quality Records — NCR / CAPA / FAIR — 🔴 Missing

**Requirement:** AS9100D §8.7 (NCR with disposition authority + customer approval), §10.2 (CAPA root-cause + effectiveness), §8.5.1.3 + AS9102 (FAIR).
**Current state:** `aberp-qa` is a pass/fail queue with one freeform `measurement` string and no inspector qualification, no characteristic structure, no NCR linkage (`crates/aberp-qa/src/repository.rs:85-102`, `lib.rs:33`). Zero hits for NCR/CAPA/FAIR/first-article in code.
**Missing:** an NCR entity (nonconformity, disposition, authority, concession), a CAPA workflow (root-cause, action, effectiveness verification, supplier flowdown), and a FAIR module (Forms 1/2/3, ballooned characteristics, nominal+tolerance+actual). FAIR is the largest single item.
**Scope:** NCR/CAPA M each; FAIR L. **Sequencing: EXEC** (NCR/CAPA needed once production runs; FAIR needed at first article — both before first aerospace shipment, but after the identity/traceability primitives).

### 5. Operator / Training / Competence Records — 🔴 Missing

**Requirement:** AS9100D §7.2 (retain documented evidence of competence); 21 CFR Part 11 §11.50 (attributable signer).
**Current state:** "operator" is an unverified login string — `ActorKind::SpaOperator{operator_login}` (`crates/aberp-inventory/src/types.rs:172-189`), `decided_by: Option<String>` on QA (`crates/aberp-qa/src/repository.rs:93`). No personnel table, no role, no qualification, no training, no e-signature. Anyone can `decide_qa` (`crates/aberp-qa/src/lib.rs:33`).
**Missing:** a personnel/qualification registry (training, certification, expiry, authorized-signer scope) and an electronic-signature binding to the ledger.
**Scope:** M. **Sequencing: MUST (structural)** — it is the identity foundation for §8.7 disposition authority, FAIR reviewer/approver separation, and Part-11 signatures.

### 6. Auto-Quoting CAD Pipeline → FAIR readiness — 🟡 Partial

**Requirement:** AS9102 Form 3 (per-characteristic nominal+tolerance+actual, ballooned); ASME Y14.41 (derived-from-authority verification, PMI ballooning).
**Current state:** the `FeatureGraph` captures `bounding_box_mm`, `volume_mm3`, `material_grade`, a `Vec<Feature>`, and the booleans `requires_5_axis` / `thin_wall_present` (`crates/aberp-quote-engine/src/feature_graph.rs:181-212`); each `Feature` has only `feature_type`, `count`, `representative_size_mm` (`:162-174`). Tolerance exists only as a *part-level* `ToleranceRange` enum (`:131-144`), never per-feature. The STEP extractor populates an **empty feature list by default** (`python/aberp-cad-extract/aberp_cad_extract/extractors/step.py:193`); the booleans come from crude bbox heuristics (`heuristics.py:38-92`).
**Missing:** per-feature nominal dimension + tolerance band + GD&T + surface finish (Ra), and a numbered/balloon characteristic identity. The pipeline is a pricing-complexity estimator, not an inspection-characteristic extractor.
**Scope:** L (and partly bounded by the deferred OCCT/BREP feature-mining work). **Sequencing: FLOW** — valuable as a FAIR accelerator but not a Stage-1 gate; the structural insight is that the hash-keyed extraction step is already a Y14.41 derived-from-authority control and should be *pinned to a controlled, revision-locked model*.

### 7. Subcontractor / Supplier Management (AVL) — 🔴 Missing

**Requirement:** AS9100D §8.4 — register of external providers with approval status/scope, flowdown to sub-tiers, right-of-access, verification; NADCAP sub-accreditation verification.
**Current state:** `partners` table is a billing-contact list; the only role discriminator is `PartnerKind = Customer|Supplier|Both` (`apps/aberp/src/partners.rs:114-119`, DDL `:523-549`). No approval status, scope, qualification, expiry, scorecard, or special-process flag. Partner CRUD is deliberately un-audited per ADR-0008 (`partners.rs:21-39`).
**Missing:** supplier approval status + scope, NADCAP accreditation reference + expiry, special-process classification, flowdown record, and supplier performance/scorecard.
**Scope:** M. **Sequencing: MUST (structural)** — Áben outsources special processes from day one; the AVL must exist before the first aerospace PO so flowdown and sub-accreditation verification are captured, not reconstructed.

### 8. Material Certifications (mill cert / CoC) — 🔴 Missing

**Requirement:** AS6174 + AS9100D §8.4.2 verification of purchased product; EN 10204 Type 3.1/3.2 MTR with heat-specific data.
**Current state:** nothing. No receiving-inspection record, no certificate capture, no heat-number field. Material enters as a grade scalar (see Gap 2).
**Missing:** a receiving entity binding an incoming lot to its heat number, its MTR/CoC document, its approved supplier, and an incoming-inspection acceptance — and a rule that **uncertified material cannot be issued to an aerospace job** (the `[[trust-code-not-operator]]` posture).
**Scope:** M. **Sequencing: MUST (structural)** — heat/cert capture is part of the same receiving moment as Gap 2; build them together.

### 9. Special-Process Tracking — 🔴 Missing

**Requirement:** AS9100D §8.5.1 (validation/control of special processes), AS9102 Form 2 (special processes + certs), NADCAP flowdown.
**Current state:** nothing — zero hits for heat-treat/weld/NDT/plating/special-process.
**Missing:** a per-job special-process record (which process, which NADCAP-accredited sub or in-house procedure, lot/batch, cert returned) that feeds FAIR Form 2.
**Scope:** M. **Sequencing: EXEC** — needed once parts route through special processes; depends on AVL (Gap 7) and traceability (Gap 2).

### 10. Calibration / Measurement Equipment — 🔴 Missing

**Requirement:** AS9100D §7.1.5.2 — calibration traceable to national standards, identification of cal status, recall of out-of-cal equipment, reassessment of prior measurements.
**Current state:** nothing — only an unimplemented Renishaw MES-adapter stub (`crates/aberp-mes/src/adapters/mod.rs:28`) and incidental cost/audit vocabulary.
**Missing:** an equipment register (instrument, cal due-date, traceability cert), a status gate, and a recall workflow — ideally with QA `measurement` records *referencing* the gauge used.
**Scope:** S–M. **Sequencing: EXEC** — must exist before measured FAIR/QA data is credible; small enough to land early.

### 11. Export Control / Access Control (ITAR/EAR) — 🔴 Missing

**Requirement:** ITAR deemed-export (nationality-gated access), segregation, encryption, access audit trail, data residency; EU 2021/821.
**Current state:** a single all-or-nothing tenant **Bearer token**, no roles, no RBAC, no per-record restriction (`apps/aberp/src/serve.rs:19-22, 57-58`); tenant isolation is file-level (one DuckDB per tenant), not row-level. Zero hits for ITAR/EAR/nationality/classification.
**Missing:** a user/identity model, role-based + nationality-gated access, per-record data classification + controlled-data marking, and an access-decision audit event.
**Scope:** L. **Sequencing: FLOW** — *only* required when a US-defense prime flows ITAR/EAR down; do **not** build speculatively. But note it shares the identity foundation with Gap 5, so the personnel model should be designed to extend here.

### 12. Counterfeit Prevention & FOD — 🔴 Missing

**Requirement:** AS9100D §8.1.4 (counterfeit) + AS5553/AS6174; AS9146/NAS412 (FOD).
**Current state:** nothing.
**Missing:** counterfeit controls fold into the AVL + material-cert gates (Gaps 7/8) — authorized sourcing, quarantine of suspect material, GIDEP-style reporting. FOD is largely a *process/floor* program (tool control, FOD areas, training) with a small ERP surface: tool accountability + a FOD-event record.
**Scope:** counterfeit S (rides on Gaps 7/8); FOD S (ERP surface) + process work. **Sequencing: EXEC** (counterfeit at first material receipt; FOD when the floor runs).

**Gap tally:** 🟢 1 · 🟡 2 · 🔴 9.

---

## Recommended Roadmap

The ordering principle: **build the structural primitives first** (identity, traceability, AVL) because they cannot be retrofitted, then layer execution records on top, then add customer-specific flowdown only when a contract demands it.

### Phase 1 — Audit-Ready Foundations (Months 1–12, before Stage 1)

*Target outcome: an auditor can pull any record, see who signed it and when, trace any material to its heat and supplier, and see a controlled, revisioned product definition. The QMS documentation (manual, procedures, internal audit, management review) runs in parallel as a process exercise — see Trade-offs.*

1. **Personnel + electronic-signature layer** (M, MUST) — a personnel/qualification table (training, certification, expiry, authorized-signer scope) and a §11.50/§11.70 signature binding to the audit ledger (name + UTC timestamp + meaning, non-repudiably linked). New `personnel.*` and `signature.*` EventKinds. *This is Month 1.*
2. **Lot/heat/serial traceability spine** (L, MUST) — heat/lot identity at receiving, serial/lot on WO output, and the genealogy join across material → WO → QA → dispatch. Extends `material_inventory.rs` and the WO/dispatch chain.
3. **Material-certificate + receiving inspection** (M, MUST) — bind each incoming lot to heat number + MTR/CoC document + approved supplier + acceptance; refuse to issue uncertified material to an aerospace job.
4. **Approved Supplier List** (M, MUST) — extend `partners.rs` with approval status/scope, NADCAP accreditation + expiry, special-process classification, flowdown record, and a scorecard stub. Add the audit events ADR-0008 currently withholds from partners.
5. **Configuration management** (M–L, MUST) — part revision + effectivity on products/BOMs, a baseline snapshot, ECO/ECN change control with disposition, where-used. Reuse the invoice chain-link pattern for engineering-document revisions.
6. **Retention policy + records query** (S, MUST) — declare retention (life-of-part + N years) as enforced metadata; generalize `audit_query.rs` beyond invoice-lifecycle helpers into a records-retrieval surface.

### Phase 2 — Manufacturing Execution (once the Stage-3 floor lights up)

*Target outcome: every part that comes off a DMG-Mori carries a complete, signed, measured history; nonconformities are caught, dispositioned by authority, and closed-loop.*

7. **NCR module** (M, EXEC) — nonconformity record with disposition + authority + customer-concession, linked to WO/QA and the signer registry (§8.7).
8. **CAPA workflow** (M, EXEC) — root-cause, action, effectiveness verification, supplier flowdown (§10.2).
9. **Calibration register** (S–M, EXEC) — equipment + cal due-date + traceability cert + recall; QA `measurement` references the gauge used (§7.1.5.2).
10. **FAIR module** (L, EXEC) — Forms 1/2/3 with ballooned characteristics (number + reference + nominal + tolerance + actual), reviewer/approver separation, full/partial trigger logic (AS9102 Rev C). Form 2 consumes the special-process + material-cert records.
11. **Special-process records** (M, EXEC) — per-job process + sub/procedure + lot + cert, feeding FAIR Form 2.
12. **FOD + counterfeit controls** (S each, EXEC) — tool accountability + FOD-event record; counterfeit quarantine + GIDEP reporting riding on the AVL/material-cert gates.

### Phase 3 — Customer-Specific Flowdowns (when a prime contract demands it)

*Target outcome: ABERP can satisfy a specific prime's contractual flowdown without re-architecting.*

13. **ITAR/EAR access control** (L, FLOW) — extend the personnel model to nationality-gated, role-based, per-record-classified access with an access-decision audit event; data residency posture. *Build only against a real contract.*
14. **AS9145 APQP/PPAP integration** (L, FLOW) — a PPAP package assembler (control plan, PFMEA, MSA, FAIR) gating new-part introduction.
15. **DPD / MBD support** (M–L, FLOW) — treat the received CAD model as a controlled, revision-pinned authority; verify derived feature data against it (the hash-keyed extractor is already this control); preserve PMI for FAIR ballooning (ASME Y14.41).
16. **MIL-STD-130 UID marking** (S, FLOW) — generate + record item-unique-identifiers when a DoD contract flows it down.

### Phase 4 — Continuous Improvement

*Target outcome: the QMS measures itself and improves.*

17. **Quality KPIs / management-review dashboard** (M, CI) — on-time delivery, escape rate, NCR/CAPA cycle time as §9.3 management-review inputs (ABERP already has a financial-statistics precedent in `reports.rs`).
18. **Supplier scorecards** (S, CI) — quality/delivery performance feeding AVL re-approval.
19. **OEE / machine utilization** (M, CI) — once the MES adapter stub becomes a real Renishaw/DMG-Mori integration.
20. **Internal-audit + nonconformity-trend analytics** (S, CI).

**Phase 🔴/🟡/🟢 counts (representative items above):** Phase 1 — 🔴 4, 🟡 2 (CM mechanism + records spine reuse). Phase 2 — 🔴 6. Phase 3 — 🔴 4. Phase 4 — 🔴 4 (all new-build).

---

## Honest Trade-offs — what ABERP should NOT build in-house

1. **The QMS itself is not code.** The bulk of AS9100D — quality manual, documented procedures, internal-audit programme, management review, competence framework, the Stage-1 readiness review — is an organizational/documentation exercise. **Engage an AS9100 consultant and a certification body early.** ABERP's job is to be the *system of record* those processes run on, not to replace them. Do not let the build list crowd out the paperwork; an auditor fails a shop with perfect software and no quality manual.

2. **Do not build a CAD/CAM/CMM metrology stack.** ABERP should *capture* measured results (from Renishaw probes, CMMs, hand gauges) and *manage* calibration status, but the measurement software, GD&T verification engines, and DMIS programs are mature commercial tools. The `[[spacex-vertical-integration]]` posture has a limit here: writing a metrology engine is negative-leverage. Integrate via the MES adapter, don't reimplement.

3. **Do not pursue Part 21/145 or hold NADCAP for outsourced processes.** Confirmed above: a Tier-2/3 machine shop works under the prime's POA and flows special processes to accredited subs. Building ABERP features to support an in-house POA would be speculative abstraction (CLAUDE.md #2/#13).

4. **Do not build ITAR/EAR access control speculatively.** It is a large (L) effort and only required against a specific US-defense contract. Design the personnel model (Phase 1) to *extend* into nationality-gated access, but do not build the export-control surface until a prime flows it down. Building it early is the canonical "for future flexibility" trap.

5. **The full AS9102 FAIR-from-CAD automation is a moonshot, not a foundation.** Auto-ballooning from a model (Y14.41 PMI → Form 3 characteristics) is genuinely valuable and on-brand for ABERP's CAD investment — but it depends on OCCT/BREP feature mining that is *itself* deferred, and a manual/semi-automated FAIR module delivers audit-readiness far sooner. Build the FAIR record structure first (Phase 2); automate ballooning later (Phase 3).

6. **Electronic-records compliance is achievable in-house — keep it.** This is where ABERP's existing hash-chained ledger is a genuine moat. A bound electronic-signature layer on top of the existing chain gives Part-11-equivalent records without a vendor. *This* is the right place to vertically integrate.

---

## Open Questions (for Ervin to decide — flagged, not asked)

1. **Civil aerospace vs. defense?** The entire ITAR/EAR + MIL-STD strand (Phase 3, large effort) hinges on whether Áben targets US-defense primes or civil/commercial aerospace only. Civil-only removes the heaviest access-control build. *This is the single highest-leverage scoping decision* — it changes the roadmap by a full phase. The Hungarian/EU **military** (not dual-use) regime (Common Position 2008/944/CFSP + national transposition) was not deep-researched here and needs its own look if defense is in scope.

2. **AS9100 scope boundary.** Will the certificate cover only machining, or machining + the special processes Áben eventually brings in-house? This determines whether in-house NADCAP (and ABERP special-process *execution* features, not just outsourced-tracking) ever enter scope. Conservative recommendation: **certify machining only, outsource all special processes** — smallest scope, fastest certificate.

3. **Serialization granularity.** Per-part serial (every unit) vs. per-lot traceability? Aerospace often demands per-serial for flight-critical hardware but accepts per-lot for fasteners/brackets. This sizes Gap 2 (L) significantly. The data model should support per-serial but default to per-lot, with the customer flowdown selecting.

4. **Retention horizon + storage.** "Life of the part + N years" can mean 30–40 years of immutable records. The hash-chained ledger grows unbounded today (no purge — conservative for integrity). Confirm the retention horizon and plan cold-storage/attestation for decade-scale archives before the volume forces it.

5. **Consultant + CB timing.** AS9100 is a 12–18-month build dominated by process, not code. The Month-1 decision is arguably *"engage a consultant and book a gap audit"* in parallel with starting Gap 1. The code roadmap above assumes that process track runs alongside; if it doesn't, the software will be ready for an audit that the *organization* isn't.

---

## Sources

**AS9100D / AS9102 / AS9145**
- Smithers — AS9100 Requirements: https://www.smithers.com/resources/2026/february/smithers-summarizes-as9100-requirements
- IAQG — AS9100D Risk Requirements: https://iaqg.org/crucial-insights-into-as9100d-risk-requirements-for-aerospace-and-defense/
- Apogee QMS — §8.5.2 Identification & Traceability: https://aqms.space/2023/06/as9100-8-5-2-identification-and-traceability
- Apogee QMS — §8.1.4 Prevention of Counterfeit Parts: https://aqms.space/2023/05/as9100-8-1-4-prevention-of-counterfeit-parts
- QMII — Achieving AS9100 Rev D (Stage 1/2 guide): https://www.qmii.com/achieving-as9100-rev-d-certification-a-step-by-step-guide/
- Smithers — What to Expect During an AS9100 Audit: https://www.smithers.com/resources/2026/march/summarizes-what-to-expect-during-an-as9100-audit
- DISCUS — AS9102 Rev C changes: https://www.discussoftware.com/news/as9102-rev-c-what-you-need-to-know/
- Ideagen InspectionXpert — AS9102 FAIR: https://www.inspectionxpert.com/fai/as9102
- SAE AS9102 (normative): https://www.sae.org/standards/content/as9102/
- RGBSI — AS9145 (11 elements / 5 phases): https://blog.rgbsi.com/as9145-requirements-for-aerospace-defense
- simpleQuE — AS9145 APQP/PPAP: https://www.simpleque.com/as9145-apqp-and-ppap-for-aerospace/

**NADCAP / Export control / Airworthiness**
- PRI — NADCAP: https://www.p-r-i.org/nadcap
- mpofcinci — Complete NADCAP Guide (AC documents): https://mpofcinci.com/blog/complete-nadcap-guide/
- Pioneer Service — NADCAP for precision machining: https://pioneerserviceinc.com/blog/nadcap-certification-and-how-it-applies-to-the-precision-machining-industry/
- eCFR 22 CFR Part 120 (ITAR definitions): https://www.ecfr.gov/current/title-22/chapter-I/subchapter-M/part-120
- Cofactr — ITAR for manufacturers: https://www.cofactr.com/articles/a-practical-guide-to-itar-compliance-for-manufacturers-and-engineers
- EUR-Lex — Regulation (EU) 2021/821 (Dual-Use): https://eur-lex.europa.eu/eli/reg/2021/821/oj/eng
- EASA — Production Organisation Approvals: https://www.easa.europa.eu/en/domains/aircraft-products/production-organisations-approvals
- EASA FAQ 19007 (raw material / POA scope): https://www.easa.europa.eu/en/faq/19007
- eCFR 14 CFR Part 21: https://www.ecfr.gov/current/title-14/chapter-I/subchapter-C/part-21

**Records / Config / Counterfeit / FOD / DPD**
- Cornell LII — 21 CFR §11.10: https://www.law.cornell.edu/cfr/text/21/11.10
- Cornell LII — 21 CFR §11.50: https://www.law.cornell.edu/cfr/text/21/11.50
- Wikipedia — EIA-649 Configuration Management: https://en.wikipedia.org/wiki/EIA-649_National_Consensus_Standard_for_Configuration_Management
- SAE EIA649B: https://www.sae.org/standards/content/eia649b/
- SAE AS5553 (counterfeit electronic parts): https://www.sae.org/standards/as5553-counterfeit-electronic-parts-avoidance-detection-mitigation-disposition
- ProjectMaterials — EN 10204 3.1 vs 3.2 MTR: https://blog.projectmaterials.com/epc-projects/testing-inspection/mill-test-certificates-3-1-2/
- SAE AS9146 (FOD prevention): https://saemobilus.sae.org/standards/as9146-foreign-object-damage-fod-prevention-program-requirements-aviation-space-defense-organizations
- FODbag — NAS412 guide: https://fodbag.com/fod-nas412/
- ASME Y14.41 (DPD/MBD): https://www.asme.org/codes-standards/find-codes-standards/y14-41-digital-product-definition-data-practices
- EASA — Technical records retention FAQ: https://www.easa.europa.eu/en/the-agency/faqs/technical-records
- Sassofia — Back-to-birth traceability: https://sassofia.com/blog/easafaa-life-limited-parts-back-to-birth-traceability/
- MIL-STD-130 (item unique identification): https://en.wikipedia.org/wiki/MIL-STD-130

**Caveats on sources.** SAE standards (AS9100D, AS9102C, AS9145, AS5553/AS6174, AS9146, EIA-649B) are paywalled; clause *numbers* and function names above are verified against authoritative interpretive sources and SAE/IAQG pages, but **verbatim clause text and exact AS9102 Rev C field renumbering must be confirmed against the purchased standards** before quoting in a controlled QMS document. NADCAP AC numbers/revisions should be re-verified on eAuditNet. 21 CFR Part 11 is FDA-binding only; its use here is the recognized aerospace *equivalent-expectations* benchmark, not a regulation that binds Áben.

---

## Code Evidence Index

| Area | Primary file:line |
|---|---|
| Hash-chained ledger | `crates/audit-ledger/src/lib.rs:3`, `chain/verify.rs:24`, `canonical.rs:1`, `mirror.rs:18`, `storage/schema.rs:23` |
| Entry shape (12 fields) | `crates/audit-ledger/src/entry/mod.rs:30-44` |
| EventKind catalogue (83) | `crates/audit-ledger/src/entry/event_kind.rs:234` |
| Invoice revision chain | `apps/aberp/src/issue_storno.rs`, `issue_modification.rs`; `event_kind.rs:287-321` |
| Material as grade scalar | `apps/aberp/src/material_inventory.rs:225-256` |
| No lot/serial (inventory) | `crates/aberp-inventory/src/lib.rs:15-17` |
| QA pass/fail record | `crates/aberp-qa/src/repository.rs:85-102`, `lib.rs:29-33` |
| Work-orders + flat BOM | `crates/aberp-work-orders/src/repository.rs:57-68`, `lib.rs:9-12` |
| MES NoopAdapter stub | `crates/aberp-mes/src/lib.rs:27-39` |
| FeatureGraph schema | `crates/aberp-quote-engine/src/feature_graph.rs:162-212` |
| STEP extractor (empty features) | `python/aberp-cad-extract/aberp_cad_extract/extractors/step.py:193`, `heuristics.py:38-92` |
| Partner table / PartnerKind | `apps/aberp/src/partners.rs:114-119, 523-549` |
| Tenant Bearer token (no RBAC) | `apps/aberp/src/serve.rs:19-22, 57-58` |
| Operator = login string | `crates/aberp-inventory/src/types.rs:172-189` |
