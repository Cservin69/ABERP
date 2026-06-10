# Memory vs. Codebase Reconciliation (S336 / 2026-06-10)

## Summary

- **42** project memories audited (every `project_*.md` in the dispatch memory dir) against both repos (ABERP @ `49c5eec`, ABERP-site @ origin/main).
- **31 🟢 SHIPPED · 9 🟡 PARTIAL · 2 🔴 MISSING.**
- 42 memory files rewritten in place (description + dated audit section, original spec preserved); 28 MEMORY.md index lines rewritten status-first; 1 broken index link fixed (`project_aberp_python_resolver_s282.md` → `project_aberp_python_auto_discovery.md`).
- Method: 7 parallel auditor agents, one grep-and-read verification per spec component, file:line evidence mandatory for SHIPPED, confirmed-empty grep mandatory for MISSING.

The headline: **~74% of "Backlog / MUST / Bug" labels described work that was already live**, some of it for weeks. This is exactly the failure mode that produced six consecutive verify-only refusals (S319/S320/S321/S322/S326/S327).

## Per-memory verdicts

| Memory file | Verdict | Evidence anchor | Scope if missing |
|---|---|---|---|
| ap_incoming_invoices | 🟢 | `ap_sync.rs:1-80`, `incoming_invoices.rs:343` + `mark_irrelevant_requires_reason` | — |
| invoice_numbering | 🟢 | `numbering.rs:83-103`; annual-reset gate LIFTED `duckdb_store.rs:461` (PR-90) | — |
| invoice_dates | 🟢 | `invoice-dates.ts:81-148`, `pr_84_invoice_dates.rs` | — |
| invoice_draft_state | 🟢 | sibling `invoice_draft.rs` table (equiv. design), spawner `serve.rs:11891` | — |
| eur_invoicing | 🟢 | `money.rs:145`, `crates/mnb-rates/`, `nav_xml.rs:1237` | — |
| buyer_address | 🟢 | `issue_preflight.rs:603` (§169 PrivatePerson), `print_invoice.rs:1038` | — |
| nav_as_dr | 🟢 | `RestoreFromNavWizard.svelte`, `restore_from_nav_outgoing.rs:1977` (S180 `4fab624`) | — |
| nav_submit_works | 🟢 | `nav_xml.rs:564,1199`, validator pins, `serve.rs:16060` | — |
| extnav_partner_nav_gap | 🟢 | option B shipped PR-217 `eb1f061`: `ExtNavPartnerPickerModal.svelte`, `serve.rs:2790` | — |
| outgoing_issued_column | 🟢 | `InvoiceList.svelte:1096-1119` (S242/PR-236 `d8a4e38`, v2.8.5) | — |
| notes_and_email | 🟢 | PR-82/83/92/172; `nav_xml_notes_never_leak.rs` (5 tests) | — |
| tisztelt_partner_greeting | 🟢 | `email_invoice.rs:527` + test :824 (S262/PR-251 `58361a1`) | — |
| partners | 🟢 | `partners.rs:154-538`, `serve_partners_route.rs`; audit events = ADR-0008 rejection (`partners.rs:23`) | — |
| products | 🟢 | `unit_of_measure.rs:47-61` (15 variants + Own), `liter_at_15c_..._end_to_end` | — |
| financial_statistics | 🟢 | `reports.rs:133-299`, `serve.rs:3126`, `reports_financial.rs` (PR-221 `698569c`) | — |
| adapterform_warning | 🟢 | `AdapterForm.svelte:43` untrack() (PR-249 `bef43f2`) | — |
| pdf_seller_overflow | 🟢 | `invoice-pdf/src/lib.rs:334,932`, `format.rs:67` NBSP (PR-249, v2.15.3) | — |
| email_override | 🟢 | `resolve_recipient_email` `serve.rs:15661-15720`, `email_recipient_override.rs` tests (PR-203 `a2b5c4c`) | — |
| workshop_demo_mode | 🟢 | `workshop-demo-mode.ts:4-70`, `Workshop.svelte:152-165` (PR-232, v2.8.1) | — |
| workshop_tv_density | 🟢 | `serve.rs:13576-13581` caps, `.ws-rows` tiles (PR-239, v2.10.1); backend builders still untested | — |
| ui_milestone | 🟢 | `labels.ts:1-19` (PR-24/ADR-0036) + 35-route SPA | — |
| sigint_regression | 🟢 | `serve.rs:2072` + `lib.rs:550` (PR-209/PR-215); memo line refs drifted | — |
| smtp_spoc | 🟢 | storefront `email-relay.ts:16-18` (zero creds), `email_outbox_poll_daemon.rs:953` | — |
| quoting_design_addenda | 🟢 | all 3 addenda; customer PDF banner closed S318/S325 (`aberp-quote-pdf/lib.rs:118,247` + rerender daemon) | — |
| python_auto_discovery | 🟢 | `PythonResolution` `quote_pricing_pipeline.rs:1817`, `upgrade_prod.sh:326-347` (S282, v2.27.2) | — |
| site_cad_validation | 🟢 | `cad-validate.ts:78-275`, `api/quote/+server.ts:169` (PR-P `4633b81`) | — |
| site_ssr_live | 🟢 | `lightsail-bootstrap.sh:41-289` (PR-M gaps), `email.ts:435` (PR-N) | — |
| site_smtp_broken | 🟢 | superseded-by-design twice; goal achieved via ADR-0009 outbox (`email-outbox.ts` + poll daemon, v2.27.10) | — |
| smooth_cutover_v2_1 | 🟢 | `upgrade_prod.sh:74,138,299,311`, `release.sh:72`; held ~26 cutovers | — |
| prod_cutover_strategy | 🟢 | `build_profile.rs:27-139`, `serve.rs:2756`; AMENDED: per-release PROD_v branches won | — |
| golive | 🟢 | executed 2026-05-30 (PROD_v1.0); `endpoint.rs:40`, 60+ PROD_v tags | — |
| ux_roadmap | 🟡 | all T1/T4/op-console items shipped; only bulk actions unbuilt (deliberate) | — |
| auto_quoting | 🟡 | batch-1 shipped v2.18–2.27.0; batch-2 ✗ | vendor-PO, learn-loop, CAD encryption, margin profiles, `quoting_machines`/capacity |
| site_material_grades_mismatch | 🟡 | catalogue dropdown exists (S277 `acf1f80`); fallback `+page.svelte:314` is the live symptom | diagnose catalogue-push → prod delivery; grouped select; resolvability test |
| e2e_shop_stage2 | 🟡 | phases 1/2/3/5 live on abenerp.com | Phase 4 quote→ABERP invoice (`quote_deal.rs:384` stub SO/WO) |
| tenant_management | 🟡 | wizard/NeedsSetup/rotation/multi-bank PR-A..D shipped | multi-tenant CRUD, `tenants.toml`, active-tenant switcher |
| erp_roadmap | 🟡 | ordering/CAD-extract/WO/QA/dispatch/MES live (ADR-0057..0067) | purchasing/PO (ADR-0068 Proposed), CAM, BOM revisions, NCR/CAPA/FAIR |
| stage3_manufacturing | 🟡 | α–δ + ε/η shipped (4 MES adapters) | Renishaw, Trumpf, furnace, robot task queue, scheduling, offline cells |
| session_cadence | 🟡 | process memory; described workflow obsolete | rewrite kept only relay-shape guidance |
| versioning_policy | 🟡 | validator + 2.0 trigger shipped (`release.sh:72`, ADR-0056) | policy text drifted: per-module majors never adopted; de-facto = 2.x minors |
| saas_migration | 🔴 | 0 code hits (`cognito`, `webauthn`, `totp`, `MFA`, `secretsmanager`, `invoicing.abenerp`) | full 7-phase backlog, genuinely unstarted |
| defense_aerospace_pivot | 🔴 | 0 code hits (`DigitalIdProvider`, `aberp-compliance`, `heat_lot`, `AVL`, `CMMC`...); only S330 doc `89b7678` | full strategy backlog: identity/e-sig, compliance traits, lot/heat, AVL |

## Top surprises (verdict vs. label)

1. **`nav_as_dr` ("Backlog") → fully SHIPPED** since S180/PROD_v1.4.1 — a complete wizard + idempotent restore + partner/product extraction, with idempotency regression tests. Largest label-vs-reality gap found.
2. **`erp_roadmap` + `stage3_manufacturing` ("future, don't build yet") → ~70% built.** MES framework, 4 real adapters, inventory, work orders, QA, dispatch all live (ADR-0060..0067). A session trusting these memories verbatim could re-propose entire modules.
3. **`ap_incoming_invoices` ("non-prio backlog") → SHIPPED** with poll daemon, closed status vocab, and SPA tabs (S177–S179).
4. **`extnav_partner_nav_gap` ("awaiting Ervin's decision") → decided and SHIPPED** (option B manual partner link, PR-217).
5. **`quoting_design_addenda` ("Addendum 2 customer PDF banner MISSING", written 2026-06-09) → closed within 24h of being written** by S318/S325 (v2.27.12/13). The audit memory itself went stale almost immediately.

## Most impactful PARTIALs (real gaps worth code sessions)

1. **site_material_grades_mismatch** — the only defect reproducible today: prod storefront renders the generic-material fallback because the catalogue snapshot is empty. The fix is *operational/diagnostic* (why catalogue-push isn't landing on prod), not the dropdown rewrite the memory implies.
2. **auto_quoting batch-2** — vendor auto-PO + spend thresholds, learn-from-job loop, CAD encryption at rest, margin profiles, machine/capacity model. All confirmed 0-hit. This is the real next quoting arc.
3. **e2e_shop_stage2 Phase 4** — DEAL saga still emits stub SO/WO ULIDs (`quote_deal.rs:384-385`); no quote→invoice bridge. Blocks the "accepted quote becomes revenue" loop.
4. **erp_roadmap purchasing/PO** — ADR-0068 exists in Proposed status with zero implementation; material ordering is the biggest unbuilt roadmap stage.
5. **tenant_management multi-tenant half** — CRUD/switcher/`tenants.toml` never built; relevant only if multi-tenant ever becomes a goal (low urgency, single-tenant works).

## Most impactful MISSINGs (real backlog, prioritized)

1. **defense_aerospace_pivot** — current Ervin directive (2026-06-10); zero code. First implementable slices: `DigitalIdProvider` trait + mock, lot/heat traceability columns, `personnel.*`/`export.*` EventKinds.
2. **saas_migration** — explicitly unscheduled; keep as backlog, no action until Ervin schedules it.

## Patterns observed

- **Stale labels concentrate in 2026-05-23 → 2026-06-04 memories.** Anything written before the overnight-batch era ships so fast behind it that "Backlog" went false within days.
- **Memories from S260+ are mostly accurate** — recent work, shorter gap between writing and shipping.
- **Three recurring staleness shapes:** (a) backlog item shipped under a different PR than predicted (buyer_address: S150 not PR-104); (b) shipped via an equivalent-but-different design (invoice_draft sibling table; SMTP consolidation via ADR-0009 outbox instead of push relay); (c) "awaiting decision" status sections that outlived the decision (extnav).
- **Strategy memories rot differently:** their *directives* stay valid but their "nothing built yet" framing inverts (erp_roadmap, stage3). The fix applied: keep the strategy, restate which components now exist.
- **Real remaining work is small and specific:** of 42 memories, only ~7 contain genuinely actionable unshipped scope, and most of it is concentrated in quoting batch-2, Phase-4 deal→invoice, purchasing/PO, and the aerospace pivot.

## Sessions saved (estimate)

Last night's batch burned **6 of 10 sessions on verify-only refusals** (S319/S320/S321/S322/S326/S327) — all six traced to labels this audit corrected. With status-first index lines ("SHIPPED at ...", "PARTIAL — ...", "UNSHIPPED — backlog"), future briefs can refuse at dispatch time instead of at session time. Estimated savings: **4–6 verify-only sessions per future 10-session batch**, plus dispatch no longer schedules "implement X" for the 31 confirmed-shipped specs at all.

## Re-run cadence

Per `feedback_memory_audit_required.md`: re-run this audit **every ~20 PROD cuts OR after any 2+ consecutive verify-only refusals**, whichever comes first. Each run is cheap relative to one wasted code session. Index lines updated by intermediate sessions should carry the same status-first convention so drift stays visible.

---
_S336 / PR-33 · doc-only · audited by Fable 5 with 7 parallel verification agents · 2026-06-10_
