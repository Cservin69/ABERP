# ADR-0003 — Storage abstraction with DuckDB as first backend

- **Status:** Accepted (cornerstone — pre-decided)
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

ABERP starts with a local-first deployment: one Tauri desktop binary, one
DuckDB file per tenant. Later, ABERP needs to run on cloud with Postgres-per-tenant
(ADR-0002). The module code must not change when the storage backend changes.
At the same time, we must not invent a leaky abstraction layer "for future
flexibility" that makes today's code worse.

DuckDB was chosen as the first concrete backend because:

- Single-file, embedded, no separate process to operate locally.
- Strong SQL support, good performance for the read-heavy analytics that ERPs do.
- Cross-platform binaries that work inside Tauri without extra plumbing.
- Easy backup (it's a file) and easy hashing for audit (it's a file).

## Decision

Each module defines its own **storage port** as a Rust trait whose methods are
in terms of *domain types*, not SQL. The trait method names describe what the
module needs (`record_invoice_issued`, `find_open_orders_for_customer`), not how
the storage achieves it. Each module also ships a **DuckDB adapter** that
implements its storage port. Tests use an **in-memory adapter** for the same trait.

Module code never imports DuckDB types. The string `duckdb` does not appear in
the domain or app layers of any module.

A shared `aberp-storage` crate provides:

- A connection pool abstraction (one logical pool per tenant DB).
- Migration runner (forward-only, versioned, recorded in a `_aberp_migrations` table).
- A transaction handle type that modules use without naming the backend.

The first non-test alternate backend will be **Postgres-per-tenant** for cloud.
We will not add a second backend speculatively. The trait is shaped by what
two real backends need, not by what an unknown third backend might need.

## Consequences

- Module tests are fast: in-memory adapter, no IO.
- Adding a backend is bounded work: implement the storage port for each module,
  not the entire system at once.
- The abstraction must stay narrow. If a module needs DuckDB-specific behavior
  (e.g., a vectorized window function for an inventory report), it goes in
  the adapter, not in the port. The port stays domain-shaped.
- We pay a small cost in expressiveness: complex cross-table queries are
  written twice if we ever add a second real backend. Accepted — this is the
  cost of not being locked to one engine.
- We **do not** use an ORM. ORMs leak storage concepts into the domain. Adapters
  use SQL directly, parameterized.

## Adversarial review

- *"You'll regret the abstraction the first time you need a vendor-specific feature."*
  — That feature goes in the adapter behind a domain-shaped method. The port
  exposes "rebalance inventory across locations", not "execute window function X".
- *"How do you keep the DuckDB and Postgres adapters in sync semantically?"*
  — A **conformance test suite** runs against every storage adapter. Adding a
  backend means making the conformance tests pass. ADR text without enforcement
  is a wish.
- *"Migrations are the hard part; you've glossed over it."* — Migrations are
  forward-only, per-tenant, versioned, idempotent, transactional where possible,
  and the migration runner refuses to start a process whose tenant DB is at a
  newer version than the binary. The runner is part of `aberp-storage`.
- *"What about partial migrations on power loss?"* — DuckDB transactions provide
  the guarantee for single-statement migrations. Multi-statement migrations
  record a checkpoint and resume. Tested with kill-9 fuzzing in CI.
- *"DuckDB has crashed under concurrent writers before."* — One writer per
  tenant process. Concurrency lives in the application above the connection,
  not below it.

## Alternatives considered

- **SQLite first** — viable; we chose DuckDB for the analytics ergonomics ERPs
  benefit from (inventory roll-ups, financial reporting). SQLite remains a
  plausible adapter if DuckDB ever disappoints.
- **Postgres locally via embedded distro** — heavier, worse Tauri story.
- **A generic SQL crate (e.g., sqlx) as "the abstraction"** — too low-level;
  it's a SQL string layer, not a domain port. We may still *use* such a crate
  inside an adapter.

## Open questions

- Exact migration framework — handwritten or crate-based. Decided before commit #1.
- Encryption at rest for the DuckDB file — DuckDB itself does not encrypt;
  we wrap it (e.g., filesystem-level encryption bound to OS keychain). Detail
  in ADR-0007.
