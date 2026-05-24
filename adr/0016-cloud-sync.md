# ADR-0016 — Cloud sync and remote UI

- **Status:** Proposed (stub — scope only, no decisions yet)
- **Date:** 2026-05-19
- **Deciders:** Ervin
- **Depends on:** ADR-0001 … 0008 (all spine ADRs)

## Scope

A cloud-hosted topology that runs the same ABERP backend with a Postgres-per-
tenant storage adapter, fronted by a TypeScript UI. The local Tauri app
remains the primary surface; cloud is for remote access, multi-user
collaboration, and external attestation publishing.

Decisions to be made:

- **Hosting model**: single-tenant per VM/container vs. one cluster many
  databases; matches the per-tenant DB cornerstone (ADR-0002).
- **Authn**: OIDC provider(s) supported; how local users map to cloud
  identities; whether local-only tenants can ever go cloud.
- **Authz**: capability model from ADR-0007 unchanged; capability assignment
  per identity.
- **Sync model** for tenants that exist in both local and cloud: not "sync"
  in the general sense — one side is authoritative for each entity, and the
  other is a projection. Avoids merge conflicts entirely.
- **Network**: API gateway, rate limiting, mTLS for tenant-to-tenant trust
  anchors.
- **Attestation publishing** for the audit ledger (ADR-0008) — to an external
  trust anchor reachable by NAV inspectors and disputing parties.
- **Disaster recovery**: backup encryption, restore drills, tenant migration
  between regions.

## Open questions

- Whether cloud is sold as managed (we host) or self-hosted (customer hosts).
- Multi-region story for the EU-specific compliance posture.
- Sub-processor list for GDPR Article 28.

## Not in scope

- Selling / pricing the cloud product.
- Building the cloud UI (it's a separate codebase; this ADR governs the
  backend-side contract it consumes).
