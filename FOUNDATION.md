# ABERP — Architectural Foundation

**Status:** Draft v0.1 — pending adversarial review
**Owner:** Ervin
**Last updated:** 2026-05-19

This document is the spine. Every ADR must be consistent with it. If an ADR
contradicts the foundation, either the ADR is wrong or the foundation needs an
amendment — never both at once, never silently.

---

## 1. Purpose and scope

ABERP is a multi-tenant ERP. It will eventually handle billing, order management,
inventory, logistics, CAD/CAM artifacts, QR-driven no-touch warehouse flows, and
robotics-driven label printing and handling. The first shippable surface is
**NAV-compliant invoice issuing for a single tenant**, with historical invoice
ingestion from Billingo and the Hungarian tax authority (NAV) as a secondary
read-only path.

The system must be:

- **Modular** — modules can be added, removed, or replaced without rewriting the spine.
- **Multi-tenant with hard isolation** — one tenant's data cannot leak into another's, even under bugs.
- **Security-first** — designed against an explicit, updated threat model, not a vague intention.
- **Auditable** — tamper-evident audit ledger, NAV-grade evidence retention, exportable on demand.
- **Operable by one person** at first, scalable to a small team without rewriting.

It must **not** be:

- A distributed system from day one. Microservices ship later or never.
- A speculative abstraction layer over functionality we do not yet have.
- Coupled to a single database engine. DuckDB is the first implementation, not the only one.

---

## 2. Cornerstones (locked decisions)

These were settled before any ADR was written and are not up for casual revision.
Any change requires a superseding ADR, not an in-place edit.

1. **Backend language:** Rust. Memory safety, strong types, mature crypto and async ecosystems, and a single binary distribution model that suits Tauri.
2. **Tenant isolation:** Database-per-tenant. Each tenant owns its own physical store.
3. **Storage:** Pluggable. DuckDB is the first concrete backend. The module code never names DuckDB; it talks to a storage trait.
4. **Local UI:** Tauri + Svelte. Single desktop binary on the operator's workstation.
5. **Cloud UI:** TypeScript-based, designed for now, built later. The design must not retrofit; the cloud seat is reserved in the API and authn model from day one.
6. **NAV invoice issuing is the first production surface.** Everything else defers to that.
7. **Cybersecurity is a first-class requirement, not a phase.** Threat model lives in the repo; adversarial review is recurring.

---

## 3. High-level system shape

A single Rust workspace built as a **modular monolith**. Modules are crates.
Each module has the same internal shape:

```
modules/<name>/
  domain/    ← pure types and business rules; no IO, no async runtime, no DB types
  app/       ← use cases / command handlers; orchestrates domain + ports
  ports/     ← trait definitions: storage port, clock port, ID port, event bus port, etc.
  adapters/  ← concrete implementations of ports (duckdb adapter, nav-api adapter, ...)
  api/       ← the module's externally callable command + query surface; typed events emitted
```

The top-level binary wires modules together. There is one process per tenant.
Modules **do not** call each other through shared code; they communicate via the
typed event bus and well-defined command APIs.

Topology, today:

```
┌────────────────────────────────────────────────────────────┐
│ Tauri desktop app (Svelte UI)                              │
│                                                            │
│   ┌──────────────────────────────────────────────────┐     │
│   │ ABERP backend (single Rust process per tenant)   │     │
│   │                                                  │     │
│   │   billing / orders / inventory / labels / ...    │     │
│   │   ─────────────────────────────────────────────  │     │
│   │   storage port  ──►  DuckDB adapter ──► tenant.db│     │
│   │   audit ledger  ──►  append-only file + hash chain      │
│   │   nav port      ──►  NAV Online Számla adapter   │     │
│   │   billingo port ──►  Billingo read-only adapter  │     │
│   └──────────────────────────────────────────────────┘     │
└────────────────────────────────────────────────────────────┘
```

Tomorrow (designed, not built):

```
                Cloud UI (TypeScript)
                       │
                  ┌────┴─────┐
                  │  Gateway │ ← authn, tenant routing
                  └────┬─────┘
                       │
   ┌───────────────────┴───────────────────┐
   │  Per-tenant backend process (same Rust binary)
   │  storage adapter swapped to Postgres-per-tenant
   └───────────────────────────────────────┘
```

The cloud topology must work with the **same module code**. Only adapters change.

---

## 4. Identity and addressing

A single, universal ID scheme runs through every module. Detail in ADR-0005.

Summary:

- **All entity IDs are ULIDs** (26-char Crockford-base32, time-sortable, globally unique without coordination).
- **No auto-increment integers** for business identifiers. Sequences exist only for legally-required human-readable numbering (e.g., invoice number per fiscal year per tenant), and are derived, not primary keys.
- **Every ID is namespaced by entity type** at the boundary: `inv_01JD…`, `prd_01JD…`, `loc_01JD…`. The prefix is metadata only; the storage key remains the bare ULID.
- **Tenant ID is never embedded in business IDs.** Tenant scoping is structural (separate database). Embedding it would couple identifiers to a hosting decision.
- **External identifiers** (NAV invoice number, Billingo ID, customer VAT number) are stored as separate fields, never as primary keys.

This is the single decision that we cannot revisit cheaply, which is why it gets its own ADR and review.

---

## 5. Multi-tenancy

Detail in ADR-0002.

- One physical DuckDB file per tenant on local deployments. One Postgres database per tenant on cloud.
- A "tenant registry" (a small, separate store) maps tenant IDs to their connection info.
- The backend process is started **with a tenant context already bound**. There is no in-process tenant switching, ever. Cross-tenant work means cross-process.
- Reason: shared-DB-with-tenant-id designs have a single bug surface (forgetting a `WHERE tenant_id = ?`) that leaks data. We refuse that risk.

---

## 6. Module boundaries

Detail in ADR-0006.

- A module owns its tables. No other module reads its tables directly.
- Inter-module communication is one of two forms only:
  1. **Typed commands** — synchronous, return a Result.
  2. **Typed domain events** — asynchronous, published on the event bus, consumed by zero or more subscribers.
- Modules import each other's *API crate only* (types + traits). They do not import each other's domain or adapters.
- Any cross-module read that cannot be served by an event projection is a design smell and must be resolved by either (a) moving the data, (b) adding an explicit query API, or (c) merging the modules.

---

## 7. Security baseline

Detail in ADR-0007. Highlights:

- **Threat model lives in the repo** (`docs/threat-model.md`, created with ADR-0007). Updated on every adversarial review.
- **Secrets** never touch config files. OS keychain on desktop; managed secret store on cloud.
- **At-rest encryption** for the tenant database file, with the key bound to the OS user + keychain.
- **Supply chain**: `cargo-deny` and `cargo-audit` run in CI from commit #1. Pinned dependency versions. License allow-list.
- **No `unsafe` Rust** in business code without an inline justification and a review sign-off.
- **All commands carry an idempotency key** and a caller identity. No anonymous mutations, ever.
- **Audit ledger** (ADR-0008) records every state-changing action with the hash of the previous entry — tampering is detectable.

---

## 8. NAV / auditability posture

ABERP will issue invoices that the Hungarian tax authority must accept. That sets a hard bar:

- **Every invoice has a hash-chained audit trail** showing its full lifecycle (draft → submitted → NAV-accepted → finalized).
- **NAV submission responses are stored verbatim**, including signatures and timestamps, in a form that survives schema migration (raw blob + parsed projection).
- **Sequence numbers** for invoices follow Hungarian law: contiguous per series per fiscal year, no gaps, no reuse. The sequence allocator is a separate, transactional component (ADR-0009 will detail).
- **Export-on-demand**: a single tenant's entire invoice history must be exportable in NAV-compatible format on demand.
- **Time** comes from a monotonic clock + NTP reference, both recorded, so we can defend against clock-rewind disputes.

---

## 9. Cloud-readiness without cloud build

We do not build the cloud product now, but we make four design promises that prevent retrofit pain:

1. **Authn is token-based** even on local deployments. The local Tauri shell holds a session token; it does not bypass auth because it is "local".
2. **All command/query traffic is over a defined wire protocol** (gRPC or HTTPS+JSON; decision in a later ADR) — never via shared memory or direct function calls from the UI.
3. **Tenant context is always explicit on the wire**, never implicit from "the process you're talking to". A misrouted request is rejected, not silently served.
4. **Clock and randomness are injected**, never imported. Cloud deployments use a different clock source (NTP-disciplined); design must not assume monotonicity from process start.

---

## 10. Operational principles

- **Reproducible builds.** `cargo build` from a clean checkout produces a byte-identical binary on the same toolchain.
- **One-command tenant creation.** Provisioning a new tenant is a single CLI invocation that initializes the per-tenant store, registers it, and exits non-zero on any partial failure.
- **Backup is a first-class command**, not a cron script: it produces a tenant snapshot that can be restored on a different machine and verifies the audit chain on restore.
- **Migrations are forward-only and tested per-tenant** before promotion.

---

## 11. What this document is NOT

- A roadmap. The roadmap lives separately and changes; this document changes rarely.
- A list of features. Features live in ADRs and tickets.
- An architecture diagram of the cloud product. The cloud build will get its own foundation amendment when we get there.

---

## 12. Open questions deferred to ADRs

- Exact wire protocol for the UI ↔ backend boundary (gRPC vs HTTPS+JSON). Pick before code commit #1.
- Exact event bus implementation (in-process Tokio broadcast vs durable log). Pick when more than one module needs it.
- Exact label-printer vendor and protocol (deferred until QR/vignette ADR).
- Exact CAD/CAM artifact storage (deferred — likely content-addressed blob store).
- Backup encryption and offsite key escrow.
- Long-term migration toward a search/document store (Elasticsearch or equivalent). ADR-0018 makes the structural commitments today; the specific adoption is deferred.
- Visual design language is captured in ADR-0017; concrete font choice and print rendering path are deferred sub-decisions.

---

## 13. Adversarial review

This document is reviewed adversarially every two weeks during the design phase
and at every release thereafter. The review asks, at minimum:

- What would a NAV auditor reject?
- What would a red-teamer exfiltrate, and through which boundary?
- Where is a single misplaced bug enough to corrupt another tenant?
- Which decision here would be expensive to reverse, and is the justification still valid?
- Where are we using the language model to do something deterministic code should do?

Findings get filed as ADRs (new or superseding) — never as silent edits.
