# ADR-0019 — Storage strategy: one trait, relational source-of-truth, search-first projections, no foreign keys

- **Status:** Accepted (cornerstone — consolidates and supersedes two earlier ADRs)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Supersedes:** ADR-0003 (storage abstraction with DuckDB first), ADR-0018 (storage evolution toward search-first / document stores)
- **Related:** ADR-0002 (DB-per-tenant), ADR-0005 (ULID), ADR-0006 (module boundaries), ADR-0008 (audit ledger)

## Context

Two earlier ADRs covered storage:

- **ADR-0003** committed to a per-module storage abstraction with DuckDB as the first backend, Postgres-per-tenant as the second.
- **ADR-0018** committed to design choices that keep a future move toward a search / document store (Elasticsearch or equivalent) mechanical rather than a rewrite, while refusing to add speculative ES code today.

In practice these are one decision, not two. Splitting them suggested that "compatible with a search store later" is a future activity. It is not — it is a present constraint on every storage choice we make today. The split misread the user's intent.

A third decision belongs in the same place and was missing: **no foreign keys, ever, in any SQL schema we write.** Foreign keys make data transport between stores difficult, are absent from document and search stores by design, and create incidental coupling (cascading deletes, lock ordering, insert ordering) that punishes us every time we move data between processes, backends, or tenants. The user has stated this as a design preference; it joins the other two as a cornerstone.

This ADR merges all three into one, so reading it once tells the full storage story.

## Decision

### 1. One per-module storage abstraction

Each module defines its own **storage port** as a Rust trait whose methods are
in terms of *domain types*, not SQL. Method names describe what the module
needs (`record_invoice_issued`, `find_open_orders_for_customer`), not how the
storage achieves it. Each module ships:

- A **DuckDB adapter** (first concrete backend; production today).
- An **in-memory adapter** (tests; same trait).

Module code never imports DuckDB types. The string `duckdb` does not appear in
the domain or app layers of any module. No ORM. Adapters use SQL directly,
parameterized.

A shared `aberp-storage` crate provides:

- Connection pool abstraction (one logical pool per tenant DB).
- Forward-only versioned migration runner, recorded in a `_aberp_migrations` table per tenant.
- A transaction handle type that modules use without naming the backend.

The trait shape is constrained by what **two** real backends need (DuckDB now,
Postgres-per-tenant for cloud later — both relational). We do not pre-shape it
for search/document stores; those land as projection adapters, not as the
primary store.

### 2. Source of truth stays strongly consistent; search comes via projections

Authoritative state for every aggregate lives in a strongly-consistent
relational store (DuckDB today, Postgres-per-tenant for cloud). This is
non-negotiable for invoice issuance, payment recording, sequence-number
allocation, and inventory ownership transitions.

Search-first / document stores (Elasticsearch, OpenSearch, Meilisearch,
Typesense, Qdrant — the specific product is deferred) earn their place as
**projections**, built from the event bus (ADR-0006) and verified against the
canonical store. The canonical state never carries projection baggage
(no analyzer configs in domain types, no `_source` envelopes, no ES-shaped
fields). When a projection is added, it builds its own index from the event
stream; if it is lost, it is rebuilt.

Likely first projection adopters, in roughly this order:

1. **Audit-ledger search** (ADR-0008). Canonical chain in DuckDB; projection
   in a search store. The projection is verifiable against the canonical
   hash chain. Mismatch is loud, never silent.
2. **Inventory movement history** (ADR-0011 stub). Event-sourced; current
   balance is one projection, ad-hoc historical search is another.
3. **CAD/CAM artifact metadata discovery** (ADR-0014 stub). Already content-
   addressed; metadata search is the canonical use case.
4. **Invoice / order free-text search** (memos, line descriptions).

A projection-verifier job runs in the background, compares hashes or counts
between the projection and the canonical store, and raises an audit alert
on divergence. The read path never silently serves stale data without a
freshness signal in the UI (ADR-0017).

### 3. No foreign keys. Ever.

No SQL schema ABERP writes will declare a `FOREIGN KEY` constraint. Not
within a module's own tables. Not between aggregates. Not on the tenant
registry. **Never.**

References between rows are stored as **typed ID columns** (prefixed ULIDs;
ADR-0005). The reference points at a ULID, not at a row enforced by the
engine. Integrity is the writer's responsibility, expressed in three layers:

1. **Type-level**: Rust newtypes for every entity ID (ADR-0005). Passing an
   `InvoiceId` where a `ProductId` is required is a compile error.
2. **Write-time validation**: the command handler validates that referenced
   entities exist (or are creatable in the same transaction) before
   committing. Validation lives in the module's `app/` layer, not in the
   database. If a referenced ID is missing, the command fails loud
   (ADR-0007, fail-loud principle).
3. **Background integrity checks**: a periodic maintenance job per module
   scans for dangling references (rows whose foreign ULIDs no longer
   resolve). Findings are logged to the audit ledger and surfaced to the
   operator. Silent dangling references are a bug, not a tolerable state.

Other constraints that are **not** foreign keys are encouraged:
- `NOT NULL` on every column where the application semantics demand it.
- `CHECK` constraints for value-range invariants (e.g., `amount_huf >= 0`).
- `UNIQUE` indexes where uniqueness is a business rule.

Hard deletes do not exist for business entities anyway (ADR-0007: tombstone
model). The classic "cascading delete" rationale for FKs does not apply.

### 4. What this trio gives us together

The three decisions are mutually reinforcing:

- **No FKs** keeps data transportable between backends, between tenants
  (export/import), and into projections.
- **Per-module storage abstraction** keeps the backend behind a port so
  swapping or adding one touches one module, not the system.
- **SoT in SQL, search via projections** preserves transactional
  guarantees where they matter and gives us search/analytics where they
  don't need to be authoritative.

A future migration from DuckDB to Postgres, or the addition of an ES
projection, is mechanical: implement the port, build the projection, run
the verifier. No FK migration. No schema-coupling surprises.

## Consequences

**Positive**

- Data is portable. A tenant's DuckDB file can be exported, hashed, signed,
  shipped, and restored without untangling FK graphs.
- Migrations are friction-free: drop a column, rename a table, restructure —
  no constraint dance.
- The path to ES / OpenSearch / etc. for search-heavy modules is
  pre-cleared. No cross-module joins, no FK rewriting, no schema migration
  to a document model.
- The discipline forces real referential integrity into application code,
  where it is testable and reviewable — not into an opaque database
  constraint that fires at commit and looks like a database error to the
  user.
- Cross-process tenant isolation (ADR-0002) stays clean: no FK can ever
  point at another tenant, because no FKs exist.

**Negative**

- We give up "the database guarantees referential integrity." Dangling
  references are possible if a write path is buggy. Mitigated by write-time
  validation, integrity-scan jobs, and the audit ledger. Accepted as the
  price of portability.
- Some queries become two-step (look up the parent ID, look up the children
  by that ID). The cost is small and the patterns are explicit.
- New contributors trained on FK-heavy schemas will need to be told. The
  rule appears in the project's coding-style document (to be written) and
  is enforced by code review.

**Neutral**

- Existing schema-design tools that visualize FK relationships will be less
  useful. We document references in the schema migration files
  themselves (a comment naming the target table and the application
  invariant). ER diagrams come from those comments, not from the database
  catalog.

## Adversarial review

- *"No FKs is reckless."* — The opposite. FKs are a database-level enforcement
  of an application invariant. Putting the enforcement in the application is
  more testable, more debuggable, more portable, and produces better error
  messages. FKs also fail silently in subtle ways (cascading deletes,
  insertion order, deadlocks under load) that are harder to reason about than
  application code.
- *"What about race conditions on validate-then-insert?"* — The validation
  and the insert run in the same transaction on the source-of-truth store.
  A concurrent delete of the referenced entity is impossible because we do
  not hard-delete business entities (ADR-0007 tombstone model). A concurrent
  *update* that would invalidate a reference is caught by the write-time
  validation; if the validation is racy, that is a design bug visible in code
  review, not in the database.
- *"Dangling references will rot the data over years."* — The integrity scan
  catches them, the audit ledger surfaces them, and the loud-failure
  principle (ADR-0007) prevents them from being hidden. We accept that this
  shifts the burden to operational visibility rather than schema enforcement.
- *"This sounds like CQRS sneaking in."* — Lightweight CQRS, named openly:
  source-of-truth on write side, projections on read side, events as the
  bridge. We adopt the parts that match modular ownership and refuse the
  ceremony (separate codebases, command/query process split) that adds
  cost without current benefit.
- *"Why merge two cornerstones instead of leaving ADR-0003 alone and
  amending ADR-0018?"* — Because the no-FK rule is what makes both work
  in practice. Splitting them would leave the FK rule orphaned and let a
  future contributor read ADR-0003 alone and write FK-heavy schemas in
  good faith.
- *"Will the projection-verifier itself be a performance problem?"* — It
  runs in the background, off the read path, on a configurable cadence per
  projection. Expensive is acceptable; silent is not.

## Alternatives considered

- **Keep FKs and rely on them for integrity.** Rejected — directly conflicts
  with portability, with search-store projection mechanics, and with the
  ES-later trajectory.
- **No FKs only across modules; FKs allowed within a module.** Rejected as
  a half-measure — within-module FKs still bite during backend migrations
  and projection-building, and the rule is harder to explain than a blanket
  ban.
- **Make the storage trait expressive enough to abstract over relational
  and document/search stores in one shape.** Rejected — the trait would
  become wishy-washy ("get-by-id" works in both, but "transaction" does
  not). One trait per backend family; projections are a separate concern.
- **Build the ES adapter today behind a feature flag.** Rejected — premature,
  shapes domain types around speculative needs, doubles the test surface,
  and bloats the trait.
- **Use a multi-model database (FoundationDB, ArangoDB, SurrealDB) to
  collapse the relational/document/search question.** Rejected for now —
  vendor risk, operational complexity, and no fit for the local-first
  desktop deployment.

## Open questions

- Specific search/document store to adopt, when the time comes. Criteria:
  license, EU data-residency posture, operational footprint, analyzer
  ecosystem. Deferred to the first ADR for a module that needs it.
- Cadence and tolerance of the projection-verifier and the integrity-scan
  jobs. Tuned with first integration test.
- Per-tenant vs shared search-store cluster on cloud. Strong default:
  per-tenant boundary, consistent with ADR-0002. Final decision in ADR-0016.
- A schema-comment convention for documenting references (which target
  table, what application invariant). Pinned down before commit #1.

## Migration of earlier ADRs

- ADR-0003 status changed to `Superseded by ADR-0019`. Original text retained.
- ADR-0018 status changed to `Superseded by ADR-0019`. Original text retained.
- FOUNDATION.md §3 (high-level shape) and §12 (open questions) updated to
  point at ADR-0019.
- `adr/README.md` index updated.
