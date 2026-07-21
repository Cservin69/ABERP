# ADR-0100 — SaaS migration, re-sequenced: verified current-state + lock-in-minimizing phased plan

- **Status:** **Accepted** (2026-07-15) — all six strategic forks steered by Ervin (see §0). Phase 1 (§5) is implemented alongside this finalization on branch `saas-phase1` (off `saas-migration-design`); Phases 2–7 remain design. Originally a design-only scoping pass (Dispatch backlog item 8).
- **Date:** 2026-07-15
- **Deciders:** Ervin Áben (all six forks decided 2026-07-15).
- **Extends / re-sequences:** ADR-0059 (SaaS migration). ADR-0059's threat model (§5 STRIDE) and cost analysis remain valid and are NOT superseded. This ADR supersedes **ADR-0059 §4 (recommended stack)** and **§7 (phase sequence)**, and lifts ADR-0059 §13's "multi-tenant out of scope" per Ervin's 2026-07-15 steer. Two picks explicitly **reverse** ADR-0059 (§0 decisions 1 + 2): host Lightsail→**Hetzner** and secrets SSM→**self-hosted sops+age** — the lock-in-minimization standing constraint outranks ADR-0059's AWS-ecosystem-reuse argument. The identity pick (§0 decision 4, **self-hosted Keycloak / OIDC**) re-aligns with **ADR-0007 §Auth** ("cloud auth is OIDC") rather than reversing it — the design-pass draft had proposed reversing ADR-0007 with in-house `webauthn-rs`; Ervin's Keycloak steer restores the OIDC posture.
- **Related:** ADR-0002 (tenant isolation: db-per-tenant — cornerstone), ADR-0007 (security baseline; §Auth "cloud auth is OIDC" is revisited here), ADR-0020 (NAV transport/creds), ADR-0030 (audit mirror), ADR-0082 (snapshot system), ADR-0083 (CAD encryption), ADR-0088 (audit-service signing key), ADR-0099 (shared `aberp_db::Handle`). Standing constraints from the item-8 directive: vendor-lock-in minimization (SpaceX vertical-integration posture), harden-over-speed, SMTP single-point-of-contact, app-layer tenant isolation, incremental (no big-bang).

---

## 0. Decisions locked (Ervin, 2026-07-15)

The six §4 strategic forks are resolved. Each is recorded verbatim-in-intent below; the phase plan (§3) and fork table (§4) are updated to match.

| # | Fork (§4) | **Decision** | vs the design-pass default | vs ADR-0059 |
| --- | --- | --- | --- | --- |
| 1 | **A — Hosting** | **Hetzner CX22** (EU-Falkenstein). | = default | ⟲ **reverses** ADR-0059 (Lightsail) |
| 2 | **B — Secrets** | **Self-hosted**, `sops`+`age` behind the Phase-1 `SecretStore` trait. | = default | ⟲ **reverses** ADR-0059 (SSM) |
| 3 | **C — MFA** | *"cheap but safe MFA is preferable."* Read as: **Keycloak with TOTP required** as the cheap-but-safe baseline, **WebAuthn kept available as a step-up** for the NAV-irreversible routes (mapped to Keycloak ACR/LoA). **Flagged: this is the Dispatch reading of a short directive — correctable** if Ervin meant TOTP-only or WebAuthn-primary. | narrows the default (was WebAuthn-primary + TOTP-fallback, in-binary) | n/a |
| 4 | **D — Identity** | **Self-hosted Keycloak in a container** (standard OIDC). Delegated user mgmt + MFA; ABERP is an OIDC relying party, not an auth author. | **reverses** the design-pass default (in-house `webauthn-rs`) | re-aligns with **ADR-0007 §Auth** (OIDC) |
| 5 | **E — Tenant isolation** | **db-per-tenant.** | = default | = ADR-0002 cornerstone |
| 6 | **F — Sequencing** | **Single-tenant MVP first** (Phase 5 deferred until a real 2nd concurrent tenant). | = default | = ADR-0059 scope |

**What decision 4 changes (the load-bearing one).** Identity + MFA (Phase 2) is **no longer in-binary WebAuthn** — it is **delegated to a self-hosted Keycloak container speaking OIDC**. ABERP becomes an OIDC relying party: it redirects to Keycloak for login + MFA and consumes the returned ID/access token. This trades "auth code auditable inside the ABERP binary" for "a mature, standard, separately-hardened IdP we run ourselves" — still zero managed-vendor lock-in (Keycloak is self-hosted, Apache-2.0, swap-seam to any OIDC IdP), which is why it satisfies the lock-in-minimization constraint while restoring ADR-0007's OIDC posture. §3 Phase 2 is rewritten accordingly; the `webauthn-rs`-in-binary design is withdrawn.

**New infra decision 4 + decision 3 bring in** (folded into §4-A/B and Phase 2/6):
- **Keycloak needs its own datastore** — a small dedicated **PostgreSQL** instance (Keycloak's supported production DB) alongside ABERP's per-tenant DuckDB. This is the first non-DuckDB store in the deployment; it holds only Keycloak's realm/user/credential data, never ABERP business data (isolation invariant unaffected — Keycloak is an out-of-band auth service, not a tenant store).
- **Keycloak's own secrets live in the `sops`+`age` store** (decision 2): its admin bootstrap password, its DB connection password, and the OIDC **client secret** ABERP uses as a relying party. So the Phase-1 `SecretStore` seam (§5) and the Phase-3 `sops`+`age` backend now also carry Keycloak's credentials — one secrets substrate for the whole deployment.
- **TOTP-required + WebAuthn-step-up (decision 3)** are **Keycloak realm/authentication-flow configuration**, not ABERP code: TOTP is a required action on the realm; the WebAuthn step-up on NAV-irreversible routes maps to a Keycloak **ACR / LoA** the relying party requests (`acr_values`) and enforces by checking the returned `acr` claim before those routes run.

The remaining forks (secondary, §4 tail) keep the design-pass defaults: dual-target (rollback + NAV-as-DR), Caddy + Cloudflare edge, Hetzner CX22 sizing.

---

## 1. Context

ADR-0059 (2026-06-02, Proposed) already designed the move of ABERP from the local Tauri desktop + loopback deployment to public-internet SaaS at `invoicing.abenerp.com`. It was never built — **zero phases started** (audited, not assumed: `grep -riE 'feature.*"saas"|webauthn|totp|SecretStore|Ssm' apps crates Cargo.toml` returns only `node_modules` noise; there is no `saas` cargo feature, no auth/MFA code, no secrets-manager code).

Two things changed since ADR-0059, and both are the reason for this re-sequenced ADR rather than an in-place edit:

1. **Ervin's 2026-07-15 steer elevates constraints ADR-0059 treated as secondary.** Vendor-lock-in minimization moves from a *pushback note* to a **standing constraint**: in-house everything except genuine regulatory dependencies; where a managed cloud service is the fast path, present it as a fork with a portable/self-hostable alternative behind a swap seam, and recommend the lock-in-minimizing option by default. Multi-tenancy — which ADR-0059 §13 explicitly deferred — is now **in scope and must be sequenced explicitly**.

2. **The current state, verified against code, differs materially from what ADR-0059 and project memory assumed.** This ADR is built on a grep-verified current-state audit (§2). Three memory/ADR claims were found false and are corrected below.

### 1.1 Verified current state (every surface grep-confirmed; file:line evidence)

**Runtime & network.** Two binaries: the Tauri 2 shell `aberp-ui` (`apps/aberp-ui/`) spawns the backend `aberp serve` as a plain subprocess (`apps/aberp-ui/src/backend.rs:131` — `Command::new(aberp_bin).arg("serve")…arg("--port").arg("0")`). The backend binds **loopback only** — `127.0.0.1:{port}` (`apps/aberp/src/serve.rs:1606`), kernel-assigned port by default (`cli.rs:931` `--port` default `0`); there is **no `0.0.0.0`** anywhere in `serve.rs`. TLS is a self-signed `rcgen` cert at `~/.aberp/serve/<tenant>/loopback.crt.pem` (`serve.rs:3123 ensure_loopback_cert`, `:1462`), and the Tauri client pins it by SHA-256 leaf-DER fingerprint with no CA chain (`apps/aberp-ui/src/pinned_client.rs:19`).

**Authentication (today).** A single **flat per-tenant bearer** `session_token`, minted at boot into the keychain (`serve.rs:3229 load_or_create_session_token`), read by the shell and attached server-side to every proxied call, checked per-handler by a constant-time compare (`serve.rs:25628 check_bearer_rejection`, ~40 inline call sites, **not** middleware), `/health` exempt. **There is no user, password, login, session-rotation, or MFA concept** (confirmed by absence). **🔴 The token's entropy is `SHA-256(SystemTime nanos ‖ per-process ULID ‖ PID)`, explicitly commented "NOT a cryptographic key" (`serve.rs:3255-3262`, `:3264 generate_session_token`).** On loopback under the physical-access threat model this is acceptable; on the public internet it is a **hard pre-exposure blocker** (a guessable bearer = full NAV-submit capability under tax number `24904362-2-41`).

**Secrets.** `keyring` v2 (`Cargo.toml:233`), no `SecretStore` abstraction of any kind — **grep for `trait .*Secret` / `SecretStore` returns zero.** Secrets are read via direct `keyring::Entry` calls scattered across **8 modules / 9 call sites**: NAV blob (login+password+xmlSignKey+xmlChangeKey in one JSON item, `nav-transport/src/credentials/keychain.rs:211`), session token (`serve.rs:3233`), SMTP password (`smtp_credentials.rs:118`), CAD AES-256-GCM key (`cad_blob.rs:203`), audit-service ed25519 signing key (`audit_dap_boot.rs:61`), quote-intake bearer (`quote_intake_credentials.rs:74`), email-relay bearer (`email_relay_credentials.rs:71`), storefront origin secret (`storefront_origin_secret.rs:134`). All keyed `aberp.<domain>.<tenant>`. Everything is `Zeroizing`-wrapped.

**SMTP single point of contact — invariant confirmed.** Exactly one keychain SMTP reader exists: `smtp_credentials::read_password` (`smtp_credentials.rs:114`), called once at boot by `SecretsCache::init_at_boot` (`secrets_cache.rs:83`, guarded by `[seller.smtp]` present) into an in-memory `Zeroizing` cache; every runtime SMTP consumer reads the cache, never the keychain (regression-guarded by `tests/secrets_cache_boot.rs`, whose mock keychain panics on any post-boot read). The storefront credential snapshot carries only `base_url` + `bearer` — **zero SMTP fields** (`storefront_credential.rs:56`). No SMTP secret is written to `seller.toml`.

**Tenancy — memory claim corrected.** The claim "multi-tenant CRUD and a tenant switcher were never built (CLI-only)" is **FALSE**. Full tenant CRUD + a **restart-based** switcher shipped (S433/S434): `tenant_registry.rs` implements `~/.aberp/tenants.toml` (`add`/`archive`/`restore`/`set_nav_enabled`); HTTP routes `GET/POST /api/tenants` and `/api/tenants/:slug/{switch,archive,restore,toggle-nav}` (`serve.rs:3835-3847`); Tauri commands `create_tenant`/`switch_tenant` (`commands.rs:746`); SPA page `TenantsList.svelte`. Switching is **restart-based by deliberate design** — it writes a one-shot `~/.aberp/next_tenant` hint consumed at next boot (`tenant_registry.rs:14-20`, `serve.rs:426 resolve_effective_serve_args`). Isolation is **database-per-tenant** — `~/.aberp/<slug>/aberp.duckdb` (`tenant_registry.rs:673`), per ADR-0002. FOUNDATION.md:130 + ADR-0002 pin the hard invariant: **the process is started with one tenant bound and cannot switch tenants in-process; cross-tenant work is cross-process.** (Several tables carry a `tenant_id` column as belt-and-suspenders — e.g. `material_inventory.rs:227`, `quote_calibration.rs:51` — but isolation is the per-file boundary, not those columns.)

**Durability.** Shared single-writer `aberp_db::Handle` constructed once at boot and threaded into `AppState.db` (`serve.rs:1520`, ADR-0099). Audit mirror `<db>.audit.log` appended lockstep post-commit (`mirror.rs:94`, ADR-0030). Snapshot system (ADR-0082) is **logical** `EXPORT DATABASE (FORMAT PARQUET)` → re-validate → `IMPORT DATABASE` into staging + atomic rename; CLI `aberp snapshot now|list|restore` (`cli.rs:631`); restore refuses a live `~/.aberp` path (restore-to-side + manual swap) and does **not** rebuild the mirror. **The DB path is already parameterized** — `--db` / `ABERP_DB` (default `./aberp.duckdb`, `cli.rs:906`); the shell wrappers bridge the env. **Backup is purely local** — no S3/remote anywhere; `tools/snapshot-prod.sh` writes a local gzip tar + an encrypted local zip of keychain secrets (and notably omits `aberp.cad.<tenant>` — a pre-existing gap).

**Build profile.** Only two cargo features exist: `production` and `test-support`. No `saas`. `build_profile.rs` is the single source of truth; `guard_tenant_matches_build` (`serve.rs:242`) couples the `production` build to `ABERP_TENANT=prod` and fatally exits on mismatch.

### 1.2 The three corrected assumptions (flagged)

| Claim (memory / ADR-0059) | Verified reality | Consequence |
| --- | --- | --- |
| "Multi-tenant CRUD + switcher never built; CLI-only" | Built (S433/S434), restart-based, db-per-tenant | The missing SaaS piece is a **multi-process router**, not CRUD (§3 Phase 5) |
| ADR-0059 Phase C: "SSM loader behind the `SecretStore` trait Keychain implements today" | **No `SecretStore` trait exists** | Phase 3 gains a real prerequisite: introduce the seam first (folded into Phase 1) |
| ADR-0059 §3d/§5.4: "five secrets" | **8 keychain items / 9 call sites**, incl. CAD + audit symmetric keys | The secrets migration must carry the symmetric keys, or snapshots become undecryptable (§3 Phase 3/4) |

---

## 2. Decision drivers (unchanged from ADR-0059 §2, plus the new standing constraints)

Carry ADR-0059 §2 (NAV compliance bar > typical SaaS; ≤ €15–25/mo; hülye-biztos operator UX; laptop = rollback target; 1 MAU today; EU data residency). Add the item-8 standing constraints as first-class decision drivers:

- **Vendor-lock-in minimization (load-bearing, not advisory).** Every external managed dependency is presented as a fork with a self-hostable alternative behind a swap seam; the lock-in-minimizing option is the default recommendation. This **reverses** two ADR-0059 recommendations (host: Lightsail→Hetzner; secrets: SSM→self-hosted) — see §4 forks.
- **Harden over speed.** Real libraries, real validation, zero PII/secrets in code or logs. Auth, MFA, secrets, and multi-tenant isolation are security-critical; **an adversarial security review is a gate on every implementation phase** (not just pre-cutover as ADR-0059 §5 proposed).
- **App-layer tenant isolation as a hard invariant.** Enforced in application code (session-bound tenant + process boundary), never engine-specific CHECK/trigger logic — the DB engine stays swappable per ADR-0019 / [[no-sql-specific]].
- **Incremental, no big-bang.** The desktop/loopback deployment keeps working unchanged at every phase; cloud capability is added behind seams that compile out under the default (desktop) build.

---

## 3. Decision — the re-sequenced phase plan (7 implementation phases)

Phase A is ADR-0059 + this ADR (design/threat-model; no code). The seven **implementation** phases below each ship with a clean rollback to the desktop deployment and each closes on an adversarial security review gate. Sequencing is driven by dependencies, not by ADR-0059's original letter order.

> **Seam-first principle.** Phases 2–6 add cloud capability behind seams introduced in Phase 1. Under the default build (`cargo build` / `--features production`) the desktop binary is byte-behavior-identical to PROD_v2.30.0. Cloud behavior lives behind `--features saas`.

### Phase 1 — Seams & token hardening (desktop-identical) — **the first shippable increment; see §5**

Three self-contained, desktop-safe changes that are pure security wins on loopback too and unblock every later phase:

1. **CSPRNG session token.** Replace `generate_session_token()` (`serve.rs:3264`, the `SHA-256(time‖ULID‖PID)` derivation) with an OS-CSPRNG token (`getrandom`/`rand_core::OsRng`, ≥256-bit, base64url). No schema change; the token stays an opaque keychain string; the shell/`check_bearer_rejection` paths are untouched. **This is the single most urgent pre-exposure fix and the anchor of Phase 1.**
2. **`SecretStore` trait seam.** Introduce one trait (`get`/`set`/`delete`, `Zeroizing` returns) with `KeychainSecretStore` as the only impl, and route all 9 keychain call sites (§1.1) through it. The SMTP SPOC is preserved *through* the seam: `smtp_credentials::read_password`'s single boot read becomes `store.get(smtp_key)`, still the one SMTP reader feeding `SecretsCache`. Pure refactor; no behavior change.
3. **`saas` cargo feature skeleton + config seams.** Add `saas = []` (default off, compiles to a no-op) plus two thin config seams the later phases fill: a transport seam (loopback+fingerprint vs public-TLS) and a path-root seam (the `$HOME/.aberp/<tenant>/` roots → one resolver).

**Security gate:** adversarial review of (a) token entropy now sourced from the OS CSPRNG and unguessable; (b) no secret leaks through the `SecretStore` seam and `Zeroizing`/drop semantics intact; (c) SMTP SPOC still a single reader; (d) `cargo build` (desktop) behavior-identical, `secrets_cache_boot` guard still green.

### Phase 2 — Auth & MFA via self-hosted Keycloak / OIDC (behind `--features saas`)

Real identity, **delegated to a self-hosted Keycloak container** (§0 decision 4), NOT built in-binary. ABERP is an **OIDC relying party**; Keycloak owns login, MFA, and user management. (The design-pass `webauthn-rs`-in-binary proposal is withdrawn.)

- **Keycloak container + its own small PostgreSQL.** One Keycloak instance in a container next to the ABERP backend, backed by a dedicated small Postgres (Keycloak's data only — realm/users/credentials; never ABERP business data). Keycloak's own secrets (admin bootstrap, DB password, and the ABERP client secret) live in the `sops`+`age` store (§0, decision 2). Realm exported as config (low lock-in; portable to any OIDC IdP).
- **OIDC relying-party flow in ABERP.** Authorization-code + PKCE against Keycloak; ABERP validates the ID token (issuer, audience, signature via Keycloak's JWKS, expiry) and establishes a session cookie (HttpOnly/Secure/SameSite=Strict). The flat per-tenant bearer stays as the desktop path.
- **MFA is Keycloak configuration, not ABERP code (§0 decision 3).** **TOTP is a required action on the realm** — the cheap-but-safe baseline every login clears. **WebAuthn is kept available as a step-up** for the irreversible NAV routes (`issue-invoice` submit, `submit-invoice` retry, `storno`, `restore-from-nav`, `recover-from-nav`): those routes are mapped to a higher Keycloak **ACR / LoA**, which ABERP requests via `acr_values` and enforces by checking the returned `acr` claim (with a freshness window) before the route runs. **Flagged: this TOTP-baseline + WebAuthn-step-up split is the Dispatch reading of Ervin's "cheap but safe MFA is preferable" — correctable to TOTP-only or WebAuthn-primary at Keycloak-realm-config time (no ABERP code change either way, which is the point of delegating to the IdP).**
- **Auth becomes a middleware seam** wrapping the ~40 inline `check_bearer_rejection` sites, composing with ADR-0007's capability model (whose CI conformance test — every route declares a capability — is reused as a gate). Under the default (desktop) build the middleware degrades to today's flat-bearer path (desktop unaffected).
- New `EventKind`s: `LoginAttempted/Succeeded/Failed`, `MfaStepUpRequired/Completed` (F12 ritual fires) — recorded from the relying-party side (the OIDC callback + the ACR-enforcement checkpoint), since Keycloak owns the credential events.

**Security gate (mandatory adversarial auth review):** OIDC token validation (issuer/audience/signature/expiry, JWKS rotation, `nonce`/PKCE, no `alg=none`), Keycloak realm hardening (TOTP required-action enforced, brute-force detection on, admin console not publicly exposed, client secret in `sops`+`age` not in config), the ACR/LoA step-up actually gating the NAV-irreversible routes (a forged or downgraded `acr` claim is rejected), session-fixation/CSRF on the callback, and the Keycloak Postgres reachable only from the Keycloak container. Desktop bearer path proven unchanged.

### Phase 3 — Secrets off the keychain (behind the `SecretStore` seam from Phase 1)

Add a **self-hostable** secrets backend as the default and a managed-cloud alternative behind the same trait (fork §4-B). **Must carry all 8 secret categories including the CAD AES-256-GCM key and the audit-service signing key** — a DB snapshot alone cannot decrypt CAD blobs without the keychain-held key, so the secrets backend and the backup path must be co-designed. Preserve the SMTP SPOC: still one reader → `SecretsCache`. Boot-time load, `Zeroizing`, fail-loud on load error ([[trust-code-not-operator]]).

**Security gate:** secret-load review — nothing logged, no route returns a secret, load failure aborts boot; the CAD/audit symmetric keys survive a restore round-trip.

### Phase 4 — Storage relocation + off-machine encrypted backup

- Route the `$HOME/.aberp/<tenant>/` roots through Phase 1's path-root seam: DB (already `--db`-configurable), `<db>.audit.log`, `~/.aberp/serve/<tenant>/issued/` (outgoing NAV XML + `<ULID>.input.json`), `~/.aberp/<tenant>/ap-artifacts/`, `seller.toml`/`logo.png`/touchfiles, the HTTPS cert dir, and the snapshot store — so a cloud volume mount needs no scattered edits.
- Add **encrypted off-machine snapshot push** (self-hostable object store default, managed alt behind the seam — fork §4-A/B); restore path must clear the mirror + `.bak` siblings (the ADR-0082 manual step, now scripted). NAV-as-DR remains the secondary recovery surface ([[aberp-nav-as-dr]]).

**Security gate:** at-rest encryption + backup-key escrow review (**fires the deferred backup-encryption-key ADR** named in `adr/README.md` §Deferred); snapshot contains no plaintext secret; restore chain-verifies (ADR-0052).

### Phase 5 — Multi-tenant router (the real SaaS-tenancy piece — Ervin's elevation)

Tenant CRUD already exists (§1.1), and ADR-0002 **forbids in-process tenant switching**. So real multi-customer SaaS is a **front router / supervisor**, not more CRUD: an authenticated session resolves to a tenant, and the router forwards to that tenant's dedicated `aberp serve` process (one process per tenant — "cross-tenant = cross-process"), avoiding the restart-to-switch model when multiple tenants are concurrently active. The **tenant-isolation hard invariant** is enforced at the router/app layer: a session is bound to exactly one tenant, the router never forwards across tenants, and the audit ledger records the tenant at process start (ADR-0007). No engine CHECK/trigger — isolation stays the process/path boundary, engine-swappable.

**Depends on Phase 2** (a session must carry *who* and *which tenant*). **Flagged:** for a single-operator "Ervin reaches his own instance from anywhere" MVP this phase is deferrable past first cutover (that MVP is single-tenant — exactly ADR-0059's original scope); it becomes load-bearing only when a second customer logs in concurrently (fork §4-F).

**Security gate (mandatory adversarial tenant-isolation review):** attempt cross-tenant routing via forged/replayed session→tenant binding; confirm one bug cannot cross the process/path boundary; confirm no shared-row surface was introduced.

### Phase 6 — Tauri detachment (dual-target) + cloud deploy pipeline

- Finish `--features saas`: Tauri-bound paths (`tauri::command`, shell lifecycle, fingerprint handoff) compile out; the transport seam flips to public-TLS at the edge; the backend serves the SPA. `cargo build --features saas` yields a Tauri-symbol-free binary; `cargo build` (laptop) is unchanged from PROD_v2.30.0 (**dual-target — the desktop binary is the rollback target**, per ADR-0059 §3g).
- Provision the host (fork §4-A), edge TLS (Caddy + Cloudflare free tier), CI deploy (self-hostable default vs managed OIDC — fork behind the deploy seam). `cargo-deny`/`cargo-audit` (already in CI) are the supply-chain gate.

**Security gate:** public-edge review — TLS 1.3/HSTS, security headers/CSP, edge + per-session rate limit, unauthenticated-route enumeration returns a uniform 401. **Must-fix carried from Phase 1 (adversarial finding #4):** `aberp-digital-id::mock::constant_time_eq` early-returns on length mismatch (timing-leaks input length); make it fully length-independent before public exposure (see §5 review-fixes note 4).

### Phase 7 — Cutover

Enroll passkeys on a staging subdomain first; snapshot the desktop's PROD state; restore to cloud; deploy `--features "production saas"`; the **first real NAV submission is the validation** ([[no-smoke-test-in-prod]]); keep the desktop binary as the rollback target for 30 days of clean cloud operation.

**Security gate:** first real invoice end-to-end on cloud (NAV submit + ack + email + audit) succeeds; rollback to desktop rehearsed.

### 3.1 Dependency ordering (summary)

```
Phase 1 (seams+token) ─┬─► Phase 2 (auth/MFA) ─┬─► Phase 5 (multi-tenant router)*
                       │                        └─► Phase 6 (dual-target + deploy) ─► Phase 7 (cutover)
                       ├─► Phase 3 (secrets) ───────► Phase 4 (storage+backup) ──────┘
                       └─► (path/transport seams consumed by 4 and 6)
* Phase 5 deferrable past cutover for a single-operator MVP (fork §4-F).
```

---

## 4. Strategic forks — DECIDED by Ervin 2026-07-15 (see §0)

All six are now resolved; this table is retained for the trade-off record and the swap-seam alternatives. The final column is the **accepted decision** (was "recommended conservative default" in the design pass). Where a decision **reverses** ADR-0059, it is marked ⟲.

| # | Fork | Options | Trade-off (one line) | ✅ Accepted decision (§0) |
| --- | --- | --- | --- | --- |
| **A** | **Hosting target** | Self-hostable VPS (Hetzner CX22, EU-Falkenstein) **vs** managed (AWS Lightsail EU-Frankfurt) | Hetzner: ~50% cheaper, 2× resources, EU-jurisdiction, no AWS console lock-in — but bring-your-own secrets/monitoring/deploy; Lightsail: AWS ecosystem reuse (OIDC/SSM) at the cost of AWS lock-in | ✅ ⟲ **Hetzner CX22** (lock-in-min outranks ADR-0059's ecosystem-reuse argument; both are EU) |
| **B** | **Managed vs self-hosted secrets** | Self-hosted (`sops`+`age` file, or self-hosted OpenBao/Infisical) **vs** managed (AWS SSM Parameter Store / Secrets Manager) | Self-hosted: zero vendor dependency, auditable, but manual rotation; managed: KMS-backed + IAM-scoped + rotation, but binds to AWS | ✅ ⟲ **`sops`+`age`** behind the Phase-1 `SecretStore` trait (age key `/etc/aberp/age.key`, `0400`); also carries Keycloak's secrets (§0-4); managed is a drop-in impl if Ervin later wants rotation |
| **C** | **MFA method / factors** | WebAuthn/passkeys (2-device) + TOTP fallback **vs** TOTP-only **vs** magic-link | WebAuthn: phishing-resistant + hardware-bound, but a two-device enrollment ritual; TOTP-only: simpler UX but a stealable shared secret; magic-link: makes email the attack vector | ✅ **DECIDED (§0-3): Keycloak TOTP-required baseline + WebAuthn step-up** on the NAV-irreversible routes (via Keycloak ACR/LoA). Configured in the IdP, not ABERP code. *Dispatch reading of "cheap but safe MFA is preferable" — correctable.* |
| **D** | **Identity: in-house auth vs external IdP** | In-house WebAuthn-in-binary (`webauthn-rs`) **vs** external IdP (self-hosted Keycloak / managed Cognito/Auth0) | In-house: auth surface auditable in the ABERP binary, no third-party trust boundary, but ABERP owns the auth code; self-hosted external IdP: standard OIDC + delegated user+MFA mgmt, a separately-hardened service, still no managed-vendor lock-in; managed IdP: least work but vendor lock-in | ✅ **DECIDED (§0-4): self-hosted Keycloak in a container (OIDC).** Re-aligns with ADR-0007 §Auth. Brings its own small Postgres + its secrets into the `sops`+`age` store. The `webauthn-rs`-in-binary design-pass default is **withdrawn** (WebAuthn survives only as the Keycloak step-up factor, fork C). |
| **E** | **Tenant-isolation model** | db-per-tenant **vs** schema-per-tenant **vs** shared-schema (`tenant_id` + RLS) | db-per-tenant: strongest isolation (no shared row space), app-layer/process boundary, engine-swappable — highest per-tenant ops cost at scale; shared-schema: cheapest ops but "one missed `WHERE tenant_id` leaks data"; schema-per-tenant: middle, muddier audit story | ✅ **db-per-tenant** (already the ADR-0002 cornerstone AND already implemented AND the only option satisfying the app-layer-isolation standing constraint; the easy fork) |
| **F** | **Single-tenant MVP first vs multi-tenant from cutover** | Ship single-tenant "reach-from-anywhere" first (defer Phase 5) **vs** build the multi-tenant router before cutover | Single-tenant-first: fastest to Ervin's stated goal ("reach it from anywhere"), matches ADR-0059's scope, Phase 5 added when a 2nd customer appears; multi-tenant-first: no re-cutover later, but Phase 5's cost before any external tenant exists | ✅ **Single-tenant MVP first**, Phase 5 deferred until a second tenant is real (reconciles ADR-0059 scope with the multi-tenant steer without paying for it early) |

Secondary forks (lower stakes, defaults from ADR-0059 stand): **dual-target vs full Tauri removal** → dual-target (preserve rollback + NAV-as-DR offline); **edge** → Caddy + Cloudflare free tier; **instance size** → 2-vCPU/4 GB class (Hetzner CX22) for DuckDB + SPA + backend headroom.

---

## 5. First shippable increment — Phase 1 in executable detail

**Goal:** land the three seams + the token fix as a normal desktop release (a PROD_Portable / PROD line cut — in the event it shipped as `PROD_v2.31.0`; the `PROD_Portable` option no longer exists in this repo, that line having moved to `ABERP-Editions.git`), byte-behavior-identical on the desktop, so every later cloud phase has a seam to fill and the most urgent pre-exposure blocker is already closed.

**Scope & exact surfaces:**

1. **CSPRNG token** — `apps/aberp/src/serve.rs:3264 generate_session_token()`: swap the `SHA-256(SystemTime‖ULID‖PID)` body for `OsRng`-filled 32 bytes → base64url. Delete the "NOT a cryptographic key" caveat (`:3255-3262`). Dependency: `getrandom` (already transitive via `rand`; confirm in `Cargo.lock`). No change to `load_or_create_session_token` (`:3229`), the shell reader (`aberp-ui/src/lib.rs:857`), or `check_bearer_rejection` (`:25628`). **Migration note:** existing tokens keep working (the value is opaque and re-read, not re-derived); no forced re-mint.
2. **`SecretStore` trait** — new module `apps/aberp/src/secret_store.rs` (trait + `KeychainSecretStore`). Re-point the 9 call sites (§1.1) — `nav-transport/.../keychain.rs`, `smtp_credentials.rs`, `cad_blob.rs`, `audit_dap_boot.rs`, `storefront_origin_secret.rs`, `email_relay_credentials.rs`, `quote_intake_credentials.rs`, `serve.rs` (session token), `aberp-ui/src/lib.rs` (session-token read). Preserve `aberp.<domain>.<tenant>` key strings verbatim (no keychain-item rename → no operator re-entry). SMTP path: `smtp_credentials::read_password` → `store.get(...)`, still the sole SMTP reader into `SecretsCache`.
3. **`saas` feature + seams** — `apps/aberp/Cargo.toml` + `apps/aberp-ui/Cargo.toml`: `saas = []`. A transport-config seam and a `paths.rs` root resolver (single source for the `$HOME/.aberp/<tenant>/` roots). All no-ops under the default build.

**Security gates (Phase 1):** `cargo fmt` + build + test + `clippy -D warnings`; the `secrets_cache_boot` panic-guard still green; a focused adversarial review confirming (a) token unguessability from OS entropy, (b) no secret escapes the `SecretStore` seam / `Zeroizing` intact, (c) SMTP SPOC still single-reader, (d) `cargo build` (desktop) diff is behavior-identical. New pins: a test asserting `generate_session_token()` output is high-entropy and non-deterministic across calls; a gate asserting every keychain access goes through `SecretStore` (grep-gate mirroring the ADR-0099 opener-census style) — committed in the review-fix pass as `tools/cut_gate_keychain_seam.sh` (see the review-fixes note below).

**Ships without regressing desktop:** all three items are default-build no-ops except the token RNG, which changes only the *source* of an already-opaque value. The desktop launch path (`run_prod.sh` → `aberp-ui` → `aberp serve --port 0`, loopback + fingerprint) is untouched. (`run_portable.sh` was named here when this ADR was written; it was deleted on 2026-07-21 when the Portable line moved to `ABERP-Editions.git`.)

**As-built (2026-07-15, branch `saas-phase1`)** — three deltas from the design sketch above, each a deliberate improvement, flagged for the reviewer:
1. **`SecretStore` lives in a new shared crate `crates/aberp-secret-store`, not `apps/aberp/src/secret_store.rs`.** The trait must be reachable from three crates — `nav-transport` (a leaf `apps/aberp` depends on, so it *cannot* import a module defined in `apps/aberp` without a dependency cycle), `apps/aberp`, and `apps/aberp-ui` (deliberately decoupled from the backend). A shared leaf crate is the only cycle-free home; it re-exports the `Zeroizing` return so consumers need no extra dep. Routing removed the direct `keyring` dependency from both `nav-transport` and `aberp-ui` (the seam now owns it); `apps/aberp` keeps `keyring` for its test mocks.
2. **Ten call sites, not nine.** The audit found a tenth direct `keyring::Entry` read the design's "9" omitted — `setup_nav_credentials.rs::blob_already_populated` (the CLI `--refuse-overwrite` probe). It is routed too, so **no direct `keyring::Entry` access remains outside the seam**. *At the Phase-1 cut this invariant HELD but was not yet ENFORCED — the enforcing grep-gate the security gate names was added in the post-cut review-fix pass (see the review-fixes note below), closing the adversarial finding that the "now holds" claim originally out-ran a committed gate.*
3. **`getrandom` added as a direct dep of `apps/aberp`** (it was only transitive). The token is 32 `getrandom` bytes → `URL_SAFE_NO_PAD` base64 (43 chars); `rand`/`rand_core` were deliberately NOT pulled (one fewer manifest entry). The `NavTransportError::KeychainBackend` `source` type changed from `keyring::Error` to `aberp_secret_store::SecretStoreError` (no consumer pattern-matched the old concrete type — verified — so this is source-compatible). Transport-bind + storage-path seams landed as `serve.rs::transport_bind_host()` (loopback in every build) and `apps/aberp/src/paths.rs` (`$HOME/.aberp` root); the remaining scattered roots are routed in Phase 4 (surgical Phase-1 scope).

**Post-cut review-fixes (2026-07-15, branch `saas-phase1-review-fixes` off `saas-phase1`; behaviour-identical — enforcement + dead-code removal + docs only, changes NO runtime behaviour):**

1. **The enforcing keychain-seam gate now genuinely exists (adversarial finding #1, MEDIUM).** The As-built #2 claim that the grep-gate "now holds" out-ran reality: the invariant held (zero direct `keyring::Entry` outside the seam) but no gate was committed to enforce it against drift. Now committed, mirroring the ADR-0099 opener-census gate convention: `tools/adr0100_keychain_seam_scan.awk` (comment/string/`#[cfg(test)]`-aware scanner), `tools/cut_gate_keychain_seam.sh` (CHECK K — scope = `apps/*/src` + `modules` + `crates` minus the `aberp-secret-store` seam crate and `*/tests/*` mocks; `ENFORCE_KEYCHAIN_SEAM` flag), `tools/cut_gate_keychain_seam_probes.sh` (negative-probe teeth harness), CI wiring in `.github/workflows/cut-gate.yml`, and `apps/aberp/tests/keychain_seam_gate.rs` so it also runs under `cargo test --workspace`. It catches every bypass form the review named — `keyring::Entry`, `.get/.set/.delete_password(`, `use keyring::Entry as X`, `use keyring::*`, `::keyring::`, `Entry::new_with_target` — verified with a red/green mutation (planted a direct `keyring::Entry` in `paths.rs` → gate RED; reverted → GREEN).
2. **Dead `paths::tenant_root()` removed (finding #2, LOW).** It had zero consumers (CLAUDE.md rule 12); its module doc also overclaimed Phase 1 wired "the ap-artifacts dir" through the seam — in fact only `serve_artifacts_dir` (via `serve_root`) is the anchor consumer; `ap_artifacts_dir` still hand-builds `~/.aberp/<tenant>/ap-artifacts` off `home_dir`. Doc corrected to match reality.
3. **Service-string agreement pin deferred (finding #3, LOW).** The `aberp.nav.<tenant>` format has three independent definitions — `apps/aberp-ui/src/lib.rs` (inline `format!`), `serve.rs::keychain_service_for` (private fn), and `nav-transport::credentials::keychain::service_name` (already pinned by its own `service_name_format_is_stable` unit test). A single cross-definition agreement test cannot be written without cross-crate test plumbing / making private items public / extracting the aberp-ui inline `format!` — none of which is clean or in Phase-1 scope. Deferred to the Phase-2 auth work (which reworks these call sites anyway). A single-source-of-truth `service_name` shared through the seam is the durable fix.
4. **`constant_time_eq` length-leak marked a Phase-6 must-fix (finding #4).** `crates/aberp-digital-id/src/mock.rs::constant_time_eq` early-returns on a length mismatch, leaking input length via timing. Acceptable on loopback / for the fixed-width mock MAC; it is a **public-exposure fix owed at Phase 6** (public-edge review) — make the compare fully length-independent (e.g. `subtle::ConstantTimeEq`). A doc note at the impl site carries the same marker so the finding is not lost. (Untouched this pass by design — it is not a Phase-1 concern.)

---

## 6. Consequences

**Wins.** Reach-from-anywhere (Ervin's goal); the most urgent pre-exposure blocker (weak token) closed in Phase 1 on desktop; a lock-in-minimizing stack (Hetzner + sops/age + **self-hosted Keycloak/OIDC**) with managed options preserved behind swap seams; ABERP no longer authors auth code (a mature IdP does), shrinking the security surface it owns; the same audit-ledger invariants (ADR-0008/0030/0052) survive; desktop stays the rollback + NAV-as-DR surface.

**Trade-offs.** ~€8–12/mo OpEx from €0 (Hetzner CX22 ~€4.5 + object storage + domain); ABERP runs (but does not author) an auth stack — a self-hosted **Keycloak** container **plus its own small Postgres** — as the cost of avoiding a managed IdP; the multi-tenant router (Phase 5) is genuine new architecture, deferred until a real second tenant.

**Locked in.** **Keycloak** becomes the load-bearing IdP (mature, widely deployed, self-hosted so no managed-vendor lock-in; the realm is exported config, portable to any OIDC IdP — acceptable). A second datastore (Keycloak's Postgres) now exists in the deployment, holding only auth data. db-per-tenant deepens ADR-0002's per-tenant ops cost at scale (accepted; the isolation floor is worth it). The self-hostable defaults deliberately trade managed conveniences (secret rotation, edge autoscaling, managed IdP) for portability.

---

## 7. Adversarial review

- *"You reversed ADR-0059's host and secrets picks — is that just ideology?"* No: the item-8 directive makes lock-in-minimization a **standing constraint**, which outranks ADR-0059's ecosystem-reuse argument. Both reversals stay behind swap seams (`SecretStore` trait; a deploy/host abstraction), so flipping back to AWS is an impl swap, not a rewrite. Ervin can veto either at fork §4-A/B.
- *"The design pass recommended in-house `webauthn-rs`; you now delegate to Keycloak — which is right?"* Ervin steered to **self-hosted Keycloak (OIDC)** (§0-4), and it is the stronger pick: it removes ABERP-authored auth code entirely (smaller surface ABERP must get right on the NAV-submit path), keeps zero managed-vendor lock-in (Keycloak is self-hosted, Apache-2.0, realm exported as portable config), and **re-aligns with ADR-0007 §Auth's OIDC posture** the design draft had proposed reversing. WebAuthn is not lost — it survives as the Keycloak **step-up factor** for the NAV-irreversible routes (§0-3). Cost: a second service (Keycloak) + its Postgres to run and harden — accepted, and it gets a mandatory adversarial auth review (Phase 2 gate).
- *"'Cheap but safe MFA' is a one-line steer — are you over-reading it?"* Possibly, and it is **flagged as correctable** (§0-3, Phase 2). The Dispatch reading is TOTP-required baseline (cheap, safe, no hardware) + WebAuthn step-up on the irreversible routes (safe where it matters most). Because MFA is Keycloak realm config, moving to TOTP-only or WebAuthn-primary is a config change, not an ABERP code change — the delegation makes the decision cheap to revise.
- *"Phase 1 changes the session-token RNG — could it lock Ervin out?"* No. The token is an opaque keychain string that is read, never re-derived; existing tokens keep validating. Only newly-minted tokens draw from `OsRng`. No forced re-mint, no schema change.
- *"Multi-tenant isolation in the app layer is weaker than DB-enforced RLS."* The opposite here: db-per-tenant has **no shared row space**, so there is no `WHERE tenant_id` to forget — the failure mode RLS is exposed to does not exist. The router is the only new cross-tenant surface and gets a mandatory adversarial review (Phase 5 gate).
- *"A single instance is a single point of failure."* Accepted at 1 MAU (ADR-0059 §9); desktop remains the manual fallback; Cloudflare cache covers brief origin outages. HA is a 4× cost step that doesn't earn its keep yet.
- *"Deferring the multi-tenant router (§4-F) contradicts 'sequence it explicitly.'"* It *is* sequenced (Phase 5) with an explicit trigger (a real second concurrent tenant). Sequencing ≠ building it before it is load-bearing (CLAUDE.md rules 2 + 12).
- *"The secrets migration could strand CAD blobs."* Flagged and designed against: Phase 3 must carry the CAD + audit symmetric keys, and Phase 4's backup is co-designed with it; `tools/snapshot-prod.sh` already omits `aberp.cad.<tenant>` today — that pre-existing gap is closed in Phase 3/4, not inherited.

---

## 8. Open questions

Beyond the §4 forks (each blocking its phase): (1) the deferred **backup-encryption-key escrow** ADR fires at Phase 4; (2) a **third-party security review** (Hungarian firm, NAV-aware; ADR-0059 §8.7, ~€1–3k) is strongly recommended before Phase 7 cutover — the per-phase adversarial gates reduce but do not replace it for the NAV-submission + operator-account surfaces; (3) **domain/DNS** (`invoicing.abenerp.com`) provider — Cloudflare free tier assumed, exportable zone (low lock-in); (4) whether Phase 1 ships as its own PROD_Portable patch cut or rides the next feature cut (a cut-time decision, not an ADR one).

## 9. Assumptions flagged

- **ADR numbering.** This file is `0100` — monotonic after the highest present (`0099`); `0093-0098` are unfiled on ABERP.git but are referenced *by those numbers* as **ABERP-Editions** ADRs inside ADR-0099, so `0100` deliberately sidesteps that range. If Ervin prefers `0093`, rename before Accept.
- **"Reach from anywhere" ≈ single-operator single-tenant at launch.** I read Ervin's stated goal as reaching his own instance; multi-*customer* SaaS is the larger posture (fork §4-F). If the intent is multi-customer from day one, Phase 5 moves before Phase 7.
- **In-process tenant switching stays forbidden (ADR-0002).** The multi-process-router design (Phase 5) is my inference from that cornerstone; if ADR-0002 is up for revision, the whole tenancy phase changes shape.
- **No cloud infra exists to verify.** §4-A/B/D recommendations are design-level (there is no current cloud deployment to grep); confidence is high on the seams (code-verified) and medium on the external-service picks (conventional at 1 MAU, un-reviewed by an external party — same caveat as ADR-0059 §12).
- **Phase 1 CSPRNG has no external token consumer.** Verified the token is opaque to the shell/SPA and not derived-from elsewhere. Additionally confirmed the shell's handshake parser validates the *cert fingerprint* (64-hex), NOT the session token, so the base64url token shape change cannot break the handshake; the constant-time bearer compare is length-agnostic. Existing hex tokens keep validating (opaque, re-read). No external tool reconstructs the token (none found); if one did, that assumption breaks.
- **Status update.** This ADR is no longer design-only: **Phase 1 is implemented** on branch `saas-phase1` (off `saas-migration-design`) and lands with this finalized ADR — all gates green, desktop behaviour-identical. Phases 2–7 remain design. Nothing merged to `main` or pushed to origin; this is handed back for the standing adversarial review + cut.
