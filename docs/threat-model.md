# Threat model (living document)

**Methodology:** STRIDE for technical threats, LINDDUN for privacy threats.
**Cadence:** updated every two weeks during design phase; at every release
thereafter; and immediately on any incident.

**Status:** v0.1 skeleton — to be expanded at first adversarial review.

---

## Assets

| Asset                              | Sensitivity   | Notes |
|------------------------------------|---------------|-------|
| Tenant database files              | High          | Hold all customer/financial/inventory state |
| NAV submission receipts            | High          | Legal evidence; integrity is paramount |
| Tenant registry                    | High          | Holds connection info for every tenant DB |
| Audit ledger                       | High          | Tamper-evident; if it's editable, the whole system is |
| Session tokens                     | Medium-High   | Short-lived but capable while live |
| Operator OS keychain               | High          | Holds the root secret for at-rest encryption |
| CAD/CAM artifacts                  | Medium        | Customer-confidential; may be trade secret |
| Printer / robotics local network   | Medium        | Compromise → physical-world impact |
| Build provenance (binary hashes)   | Medium        | Required to defend "which binary signed this?" |

## Actors

| Actor                          | Capability                                              |
|--------------------------------|---------------------------------------------------------|
| External attacker (internet)   | Network reach to public endpoints (later, on cloud)     |
| External attacker (LAN)        | Reach to printers, robotics, the operator's workstation |
| Malicious tenant               | Authenticated, scoped to their own data                 |
| Compromised operator session   | Authenticated as the operator, capability-scoped        |
| Operator-as-threat-actor       | Fully authenticated; may try to backdate / hide / fake  |
| Compromised dependency         | Code-exec at the level of whatever uses the dependency  |
| Compromised LLM provider       | Can return adversarial outputs to LLM-using paths       |
| Insider (us)                   | Full source access; constrained by code review and audit|

## Trust boundaries (drawn from FOUNDATION.md §3)

1. UI process ↔ backend process (even local) — wire protocol with auth token.
2. Backend ↔ tenant database — only the backend reads/writes; storage adapter mediates.
3. Backend ↔ NAV — TLS with mTLS, response signature verification.
4. Backend ↔ Billingo — TLS, API key from OS keychain.
5. Backend ↔ printer / robotics — local network, signed commands, ack required.
6. Tenant A backend process ↔ Tenant B backend process — none; separate processes.
7. Backend ↔ LLM provider (future) — TLS, inputs treated as untrusted, outputs as suggestions.

## Threats (STRIDE) — initial sketch

To be expanded at first adversarial review. The intent is that every entry
below grows into a row with: threat description, affected boundary,
likelihood, impact, mitigation (existing or required), and link to the ADR
that addresses it.

- **Spoofing** — forged session token; mitigation: token signing + short TTL + capability check.
- **Tampering** — audit ledger row edited post-hoc; mitigation: hash chain + external attestation.
- **Repudiation** — operator denies issuing an invoice; mitigation: ledger entry binds session + monotonic time + binary hash.
- **Information disclosure** — cross-tenant leak; mitigation: per-tenant DB + per-process isolation.
- **Denial of service** — local: unbounded resource use; cloud: rate limiting in ADR-0016.
- **Elevation of privilege** — capability bypass; mitigation: command-to-capability mapping is conformance-tested.

## Privacy (LINDDUN) — initial sketch

- **Linkability** — across tenants is structurally impossible (separate DBs); within a tenant is expected (it's an ERP).
- **Identifiability** — customer records are necessarily identifying; minimize fields, retention policy per ADR (TBD).
- **Non-repudiation (privacy sense)** — we want this for invoices, we do not want it leaking into customer-facing data unnecessarily.
- **Detectability** — ID schemes do not reveal volume (ADR-0005).
- **Disclosure of information** — same as STRIDE.
- **Unawareness** — what the operator does not know about their own data flows; documented in customer-facing notices.
- **Non-compliance** — Hungarian invoicing law, GDPR; tracked per ADR.

## Review log

| Date       | Reviewer | Findings filed as |
|------------|----------|-------------------|
| 2026-05-19 | Ervin    | Initial v0.1 — to be reviewed in two weeks |
