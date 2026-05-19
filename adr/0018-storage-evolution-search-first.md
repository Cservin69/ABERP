# ADR-0018 — Storage evolution toward search-first / document stores

- **Status:** Superseded by [ADR-0019](0019-storage-strategy-no-fks.md) on 2026-05-19
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Related:** ADR-0003 (storage abstraction with DuckDB first), ADR-0008 (audit ledger),
  ADR-0011 (inventory model — stub), ADR-0014 (CAD/CAM artifacts — stub)

> **Note:** This ADR has been superseded by ADR-0019, which consolidates
> ADR-0003 (storage abstraction), this ADR (search-first evolution), and
> a new "no foreign keys" rule into a single cornerstone. Read ADR-0019
> instead. Text below is retained for history.

## Context

The user has stated an intent to migrate, when the project is mature, from
SQL toward a search/document-oriented store — Elasticsearch or an equivalent
(OpenSearch, Meilisearch, Qdrant, Typesense). The user notes that this
direction is "not trendy in accounting or factories now, but will be."

This is a real architectural trajectory, not a casual remark. It changes
the shape of the storage abstraction (ADR-0003), it changes how modules
think about reads vs. writes, and it touches the audit ledger (ADR-0008),
event-sourced inventory (ADR-0011 stub), and CAD/CAM artifacts (ADR-0014
stub).

It also has well-known landmines. Elasticsearch is **not a drop-in
replacement for a transactional database**. It is eventually consistent,
it has no ACID guarantees across documents, foreign-key integrity does
not exist, and refresh-vs-search latency is not zero. Treating it as
authoritative for invoice issuing would be a regulatory disaster.

The decision now is not whether to use ES, and not when. The decision now
is what we **commit to** today and what we **refuse to do** today, so that
the migration in 12-36 months is mechanical and not a rewrite.

## Decision

ABERP commits today to design choices that keep a search-first / document-
store future open, while explicitly refusing to add Elasticsearch (or any
other non-SQL backend) to the storage abstraction speculatively.

### What we commit to today

1. **Per-aggregate storage choice is allowed.** The storage abstraction
   (ADR-0003) is per-module, not global. The audit module may, in the
   future, have an Elasticsearch adapter while the billing module keeps a
   SQL adapter. The storage trait shape supports this already — each
   module owns its port.

2. **Reads and writes are conceptually separable.** Modules with heavy
   read patterns (audit log search, inventory movement history, CAD
   artifact discovery) maintain projections built from events. Today the
   projection lives in the same DuckDB file as the source. Tomorrow the
   projection can live in ES, indexed by an adapter that subscribes to
   the event bus (ADR-0006). The source of truth never moves to ES
   without an explicit ADR amendment.

3. **No business logic depends on cross-module joins.** ADR-0006 already
   forbids modules from joining each other's tables. This rule is the
   single largest enabler of a heterogeneous storage future. We restate
   it here so it is not relaxed in some future refactor.

4. **Events are first-class and durable when needed.** The phase-2
   durable event log mentioned in ADR-0006 is the bridge to a search-
   first future. When a module's projection becomes expensive to rebuild
   from the source (or needs to live in a separate store), durable events
   are how it stays consistent.

5. **Identifiers are storage-agnostic (ADR-0005).** ULIDs work the same
   in SQL, document, and search stores. No migration burden on IDs.

6. **CAD/CAM artifacts are content-addressed (ADR-0014 stub).** This is a
   natural fit for a document store. The metadata may move; the blob
   addressing does not.

### What we refuse to do today

1. **No Elasticsearch adapter in ADR-0003 today.** Speculative
   abstractions for hypothetical backends bloat the storage trait and
   make today's code worse. The trait is shaped by what DuckDB and a
   plausible Postgres adapter need.

2. **No "we might need it later" hooks** for ES — no document fields
   pre-shaped for ES indexing, no `_source` payloads, no analyzer
   configurations. When and if we add an ES adapter, it builds its own
   index from the canonical event stream; the canonical state does not
   carry ES baggage.

3. **No ES for source-of-truth transactional state.** Not now, not
   later. Invoice issuance, payment recording, sequence-number
   allocation, and inventory ownership transitions stay on a strongly-
   consistent store. ES can host *projections* and *search indices* of
   them.

4. **No "search is the database" framing.** A search store is excellent
   at finding things. It is wrong as the only store. ABERP keeps a
   strongly-consistent source of truth for every aggregate, regardless
   of what indexing layer sits in front.

### Where ES (or equivalent) likely earns its place first

These are the modules where a search/document store is a strong fit. When
the time comes, the order of adoption is roughly:

1. **Audit ledger search** (ADR-0008). The ledger is append-only,
   immutable, and search-heavy ("show me everything that touched
   invoice inv_X"). The canonical chain stays in DuckDB; ES indexes a
   projection of it. Adversarial concern: the ES projection must be
   verifiable against the canonical chain. The verifier compares
   `entry_hash` per entry; mismatches are loud.

2. **Inventory movement history** (ADR-0011 stub). Event-sourced from
   day one (proposed). The current balance is a projection. A second
   projection in a search store enables ad-hoc historical queries —
   "every movement of part prd_X in the last 18 months across all
   locations".

3. **CAD/CAM artifact discovery** (ADR-0014 stub). Already content-
   addressed. Metadata search ("find every revision of every part
   that uses tool T-12 in its CAM toolpath") is the canonical use case
   for a search store.

4. **Invoice and order full-text search.** Free-text fields (customer
   memos, line descriptions, internal notes) benefit from analyzer-
   based search. Authoritative invoice state stays SQL; the search
   index is a projection.

### What this changes in existing ADRs

- **ADR-0003 (storage abstraction)** — no change in decision; this ADR
  reinforces that the abstraction is per-module and that a second
  backend (whether Postgres or ES) is added by implementing the port,
  not by amending the trait shape.
- **ADR-0006 (module boundaries)** — no change; the no-cross-module-
  joins rule is restated here as a future-enabling commitment.
- **ADR-0008 (audit ledger)** — adds a future-direction note: the
  authoritative ledger stays in DuckDB and the mirror file; an ES
  projection is permitted later. The hash chain is verifiable on the
  projection as well as on the source.

## Consequences

**Positive**

- The migration toward search-first becomes mechanical: implement a
  port, build a projection, never touch business logic.
- Today's code stays simple. No speculative abstractions.
- The user's stated long-term direction is honored without being
  prematurely built.

**Negative**

- We commit to discipline that will feel pedantic in the short term:
  "no cross-module joins, no ES baggage on canonical state, no read-
  through-search for source-of-truth queries."
- Building two projections (DuckDB + ES) for the same data eventually
  doubles some storage cost and adds a consistency-verification job.
  Accepted.

**Neutral**

- The choice of which search/document store (Elasticsearch vs.
  OpenSearch vs. Meilisearch vs. Typesense vs. Qdrant) is deferred
  until the first module needs it. The decision criteria — license,
  operational footprint, EU data residency, analyzer ecosystem — are
  noted but not resolved here.

## Adversarial review

- *"You are sneaking in CQRS without naming it."* — Not exactly. CQRS
  is a pattern; we adopt the parts that map naturally to modular
  ownership and event-sourced histories, and we refuse the parts (e.g.,
  separate command and query *codebases*) that add ceremony without
  current benefit. Naming this is fine: we are in the lightweight-CQRS
  region of the design space.
- *"Verifying an ES projection against the canonical ledger sounds
  expensive."* — It is, occasionally. It runs in the background, not
  on the read path. The verifier walks the ledger and the projection,
  compares hashes, raises an audit alert on divergence. Expensive is
  acceptable; silent is not.
- *"What if ES is sunset by Elastic before we ever adopt it?"* — Then
  we pick the live equivalent at that moment. The ADR is deliberately
  framed as "search-first / document store"; the specific product is
  open.
- *"Eventual consistency between SQL canon and ES projection will leak
  user-visible inconsistencies."* — A read that has to be authoritative
  goes to the canonical store. A read that is search-shaped goes to
  the index. Users see consistent answers in the screen they expect.
  Where this is genuinely ambiguous, the UI shows a freshness
  indicator. Loud, not silent.
- *"Why call this out now instead of when ES is needed?"* — Because
  the structural commitments (no cross-module joins, event-sourced
  histories, content-addressed blobs) are made today and would be
  costly to retrofit. The ADR is about the *commitments*, not about
  ES itself.

## Alternatives considered

- **Stay silent until ES is needed.** Refused. The user has stated the
  direction; making the structural commitments explicit now is cheaper
  than later. Silent assumptions become silent reversals.
- **Build an ES adapter now and let DuckDB-only deployments ignore it.**
  Refused. Premature, adds code we do not exercise, doubles testing
  surface, and shapes today's domain types around a feature we may
  never deliver.
- **Make ES the source of truth eventually, retire SQL.** Refused.
  Transactional invoice issuing on an eventually-consistent store is
  a tax incident waiting to happen.
- **Use a multi-model database (e.g., FoundationDB, ArangoDB) to
  collapse the question.** Possible, but punts the choice without
  resolving it; also introduces vendor risk and operational
  complexity that the single-tenant, local-first deployment does not
  justify today.

## Open questions

- Which search/document store, when the time comes. Criteria above;
  decision deferred.
- Exact projection-verifier cadence and tolerance. Deferred to the
  first ES-using module's ADR.
- Whether projections live in the same per-tenant boundary (per-tenant
  ES index) or in a shared cluster with per-tenant routing. Strong
  default: per-tenant boundary, consistent with ADR-0002.
