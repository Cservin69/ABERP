# ADR-0002 — Tenant isolation: database-per-tenant

- **Status:** Accepted (cornerstone — pre-decided)
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

ABERP is multi-tenant from design but single-tenant on day one. The tenant
isolation model is the single most expensive decision to reverse later, because
it touches every table, every query, every backup, every audit export, and the
deployment topology.

Three options exist: shared DB with a `tenant_id` column on every table; shared
DB with one Postgres schema per tenant; or one physical database per tenant.

## Decision

**One physical database per tenant.** On local deployments, each tenant is a
separate DuckDB file. On cloud deployments (future), each tenant is a separate
Postgres database (not just a schema). The ABERP backend process is started
with a single tenant context already bound, and cannot switch tenants in-process.
Cross-tenant work is done by running multiple processes.

A small, separate "tenant registry" store maps tenant identifiers to their
database connection info and metadata (provisioned date, owner identity, status).
The tenant registry is never read by business modules; only the bootstrap layer
consults it.

## Consequences

- **Strongest possible isolation.** A bug in a WHERE clause cannot leak data
  across tenants because there is no shared row space.
- **Trivial single-tenant export** for NAV audits and GDPR data-subject requests
  — the tenant's file *is* the export.
- **Backup per tenant is straightforward.** Hash the file, sign it, ship it.
- **Higher operational cost on cloud.** Provisioning a new tenant is a database
  creation, not an `INSERT`. Migrations run per-tenant. We accept this and
  commit to making per-tenant operations fast and observable.
- **Schema drift risk between tenants.** Mitigated by an explicit migration
  registry: every tenant's database records the migration version it is at,
  and any divergence is surfaced loudly (foundation §10, ADR-0008 audit).
- **No cross-tenant analytics** without an explicit, audited export pipeline.
  This is a feature, not a bug — cross-tenant analytics in an ERP without
  explicit consent is a compliance problem.

## Adversarial review

- *"Per-tenant DBs at 1000 tenants is operationally painful."* — Correct, and
  we accept that pain in exchange for the safety floor. If we ever hit that
  scale we will revisit, but the alternative is auditing every query in the
  codebase for a missed `WHERE tenant_id`, which is an even worse problem.
- *"What if a module needs to know aggregate data across tenants?"* — It does
  not. The product is per-tenant. Aggregate analytics for the operator (us)
  is an explicit, audited pipeline that reads exported snapshots, not live data.
- *"What about a shared lookup table like country codes?"* — Reference data
  that is universal lives in code or in a read-only resource file shipped with
  the binary, not in the tenant database, and not in a shared DB either.
- *"How do you stop a misconfigured process from opening the wrong tenant file?"*
  — Tenant bootstrap is by ID, the file path is derived not user-supplied, and
  the audit ledger records the tenant ID at start. ADR-0007 elaborates.
- *"Where does the tenant registry live, and what protects it?"* — On local
  deployments, in a separate DuckDB file under the same encrypted store as
  the tenant DBs. On cloud, in its own Postgres database with the strictest
  access policy. ADR-0007 elaborates.

## Alternatives considered

- **Shared DB + `tenant_id` column + Row-Level Security** — One missed RLS
  policy or a service account with broad rights = cross-tenant leak. The
  failure mode is "data leak nobody notices for a year". Refused.
- **One DB, schema-per-tenant** — Better than RLS, but Postgres schema count
  is practically capped, migrations are awkward across schemas, and the audit
  story is muddier. Refused.

## Open questions

- Cloud provisioning model — Postgres-per-tenant on a managed RDS-style cluster,
  or one cluster per N tenants? Deferred to ADR-0016 (cloud sync).
- Tenant deletion (GDPR right-to-erasure) — when is the file destroyed, what is
  retained for legal compliance? Deferred to a dedicated retention ADR.
