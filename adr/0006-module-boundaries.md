# ADR-0006 — Module boundaries and contracts

- **Status:** Accepted
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

ABERP will grow from one module (billing) to many (inventory, orders, labels,
robotics, CAD, logistics, ...). Each module will be added by someone who has
not read the others. Without a hard contract, modules will reach into each
other's tables, share types, and create a coupled tangle that is impossible
to extract for cloud deployment or to replace when a vendor changes.

## Decision

A module is a Rust workspace member with this internal shape:

```
modules/<name>/
  Cargo.toml
  src/
    domain/    ← pure types; no IO, no async, no DB types, no logging crate
    app/       ← command handlers and query handlers; orchestrates domain + ports
    ports/     ← trait definitions: storage, clock, id, event-publisher, external-api
    adapters/  ← concrete impls of ports (DuckDB adapter, NAV adapter, ...)
    api.rs     ← the externally callable surface: commands in, events out, queries
```

Modules communicate via exactly two mechanisms, in this order of preference:

1. **Typed commands** — synchronous, return `Result<Response, Error>`. Used
   when the caller needs an immediate answer (e.g., "issue invoice now").
2. **Typed domain events** — asynchronous, published on the event bus, consumed
   by zero or more subscribers. Used for "something happened" notifications
   (e.g., `InvoiceIssued`, `StockMoved`, `LabelPrinted`).

Modules **may not**:

- Read each other's tables. Ever.
- Import each other's `domain/`, `app/`, or `adapters/` modules.
- Share a database transaction across module boundaries.

Modules **may**:

- Import another module's `api.rs` types — command structs, event structs,
  error enums, the typed ID newtypes that belong to the other module.
- Subscribe to another module's events and build their own projections.

A module's external surface is its `api.rs`. Anything not exposed there is
private. Treat `api.rs` like a wire protocol: backwards-compatible changes
preferred, breaking changes get an event/version bump.

## Event bus

- Phase 1 (now): in-process `tokio::sync::broadcast`-style bus, in-memory.
  Lost on crash. Sufficient because all subscribers live in the same process
  and durable state lives in the publisher's database.
- Phase 2 (when needed): durable event log (e.g., per-tenant append-only file)
  that survives crashes. Needed when projections become expensive to rebuild
  or when the cloud topology splits modules across processes.
- Events are **immutable** once published. Schema evolution is by version field.

## Cross-module queries

If module A needs data that module B owns, the options in order of preference:

1. A subscribes to B's events and maintains its own projection. Eventually
   consistent. Best for read-heavy joins.
2. A calls a query on B's API (`B::find_x_by_y`). Synchronous. Best when
   the read is on-demand and stale data is unacceptable.
3. The data belongs in A all along — move it.
4. The modules are wrongly split — merge them.

"Just join the tables" is not on this list. It will never be on this list.

## Conformance

A `module-conformance` test ensures every module:

- Exposes only its `api.rs` types from outside the crate (enforced by `pub`/`pub(crate)` rules).
- Has at least one in-memory adapter for every port (so tests don't need real IO).
- Publishes every event type that appears in its `api.rs` (no orphan event types).

CI fails if a module violates the conformance suite.

## Consequences

- Adding a module is a copy-template-then-implement task with a clear shape.
- Refactoring a module's internals does not break callers.
- Moving a module out of process (for cloud) is mechanical: the `api.rs` stays,
  the adapter changes to a network adapter.
- We pay a cost in directness: A wanting B's data can't just `JOIN`. Accepted.
- We pay a cost in repetition: ports + adapters add files. Accepted as the
  price of testability and replaceability.

## Adversarial review

- *"This is hexagonal architecture with extra steps."* — It is hexagonal
  architecture. The "extra steps" are the bits that make it survive a
  contractor's first PR.
- *"Eventual consistency between modules will produce bugs."* — Yes, and
  those bugs are visible in the audit ledger because the binding event has
  a timestamp and an idempotency key. We choose visible bugs over invisible
  cross-module coupling.
- *"Why no transaction across modules?"* — Because that is the seam where
  modules secretly become one module. Workflows that need atomicity across
  modules use the **saga pattern**: commands + compensating commands,
  recorded in the audit ledger.
- *"How does an event consumer signal failure?"* — Consumers can fail their
  own processing without un-publishing the event. Failures are recorded; the
  consumer can replay from the durable log (phase 2) or accept lost work
  (phase 1, acceptable because publishers' state is the source of truth).
- *"Is the in-process bus a single point of failure?"* — Yes, until phase 2.
  The single process is also the database writer, so "in-process bus down"
  is "process down", which means no further events to lose.

## Alternatives considered

- **Shared schema, one big crate** — fast for one developer, terminal for any
  number of developers > 1. Refused.
- **Microservices per module from day one** — premature distribution. Refused.
- **Plugin architecture with dynamic loading** — adds attack surface and
  complexity for no current benefit. Refused for now; reconsidered if a
  third-party module ecosystem ever exists.

## Open questions

- When phase 2 (durable event log) becomes necessary. Trigger: first time a
  consumer's projection takes > 30s to rebuild from publisher state.
- Whether saga orchestration deserves its own module or lives in callers.
