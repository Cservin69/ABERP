# ADR-0100 — SaaS migration, re-sequenced: verified current-state + lock-in-minimizing phased plan

- **Status:** Proposed — design-only scoping pass (Dispatch backlog item 8). Carries unresolved strategic forks pending Ervin's steer; lives on the local review branch `saas-migration-design`, deliberately NOT pushed (the forks must be decided before it is Accepted).
- **Date:** 2026-07-15
- **Deciders:** Ervin Áben (pending)
- **Extends / re-sequences:** ADR-0059 (SaaS migration). ADR-0059's threat model (§5 STRIDE) and cost analysis remain valid and are NOT superseded. When this ADR is Accepted it supersedes **only** ADR-0059 §4 (recommended stack) and §7 (phase sequence), and lifts ADR-0059 §13's "multi-tenant out of scope" per Ervin's 2026-07-15 steer.
- **Related:** ADR-0002 (tenant isolation: db-per-tenant — cornerstone), ADR-0007 (security baseline; §Auth "cloud auth is OIDC" is revisited here), ADR-0020 (NAV transport/creds), ADR-0030 (audit mirror), ADR-0082 (snapshot system), ADR-0083 (CAD encryption), ADR-0088 (audit-service signing key), ADR-0099 (shared `aberp_db::Handle`). Standing constraints from the item-8 directive: vendor-lock-in minimization (SpaceX vertical-integration posture), harden-over-speed, SMTP single-point-of-contact, app-layer tenant isolation, incremental (no big-bang).

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

### Phase 2 — Auth & MFA layer (behind `--features saas`)

Real identity, the ADR-0059 Phase B content, built in-binary:

- Login + WebAuthn primary (two-device enrollment: MacBook + iPhone) + TOTP fallback + printable single-use recovery code; session cookies (HttpOnly/Secure/SameSite=Strict); **step-up MFA (5-min freshness)** on the irreversible NAV routes (`issue-invoice` submit, `submit-invoice` retry, `storno`, `restore-from-nav`, `recover-from-nav`).
- **Auth becomes a middleware seam** wrapping the ~40 inline `check_bearer_rejection` sites, composing with ADR-0007's capability model (whose CI conformance test — every route declares a capability — is reused as a gate). Under the default build the middleware degrades to today's flat-bearer path (desktop unaffected).
- New `EventKind`s: `LoginAttempted/Succeeded/Failed`, `MfaStepUpRequired/Completed` (F12 ritual fires).

**Security gate (mandatory adversarial auth review):** WebAuthn parameter set (resident-key, UV-required, attestation), brute-force/rate-limit, username-timing-oracle, session-fixation/CSRF, TOTP replay window, enrollment-lockout recovery. Desktop bearer path proven unchanged.

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

**Security gate:** public-edge review — TLS 1.3/HSTS, security headers/CSP, edge + per-session rate limit, unauthenticated-route enumeration returns a uniform 401.

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

## 4. Strategic forks — need Ervin's steer (each: one-line trade-off + conservative default)

The default column applies the standing lock-in-minimization + harden-over-speed constraints. Where the default **reverses** ADR-0059, it is marked ⟲.

| # | Fork | Options | Trade-off (one line) | Recommended conservative default |
| --- | --- | --- | --- | --- |
| **A** | **Hosting target** | Self-hostable VPS (Hetzner CX22, EU-Falkenstein) **vs** managed (AWS Lightsail EU-Frankfurt) | Hetzner: ~50% cheaper, 2× resources, EU-jurisdiction, no AWS console lock-in — but bring-your-own secrets/monitoring/deploy; Lightsail: AWS ecosystem reuse (OIDC/SSM) at the cost of AWS lock-in | ⟲ **Hetzner CX22** (lock-in-min outranks ADR-0059's ecosystem-reuse argument; both are EU) |
| **B** | **Managed vs self-hosted secrets** | Self-hosted (`sops`+`age` file, or self-hosted OpenBao/Infisical) **vs** managed (AWS SSM Parameter Store / Secrets Manager) | Self-hosted: zero vendor dependency, auditable, but manual rotation; managed: KMS-backed + IAM-scoped + rotation, but binds to AWS | ⟲ **`sops`+`age`** behind the Phase-1 `SecretStore` trait (age key `/etc/aberp/age.key`, `0400`); managed is a drop-in impl if Ervin later wants rotation |
| **C** | **MFA method / factors** | WebAuthn/passkeys (2-device) + TOTP fallback **vs** TOTP-only **vs** magic-link | WebAuthn: phishing-resistant + hardware-bound + in-binary, but a two-device enrollment ritual; TOTP-only: simpler UX but a stealable shared secret; magic-link: makes email the attack vector | **WebAuthn primary (2-device) + TOTP fallback** (unchanged from ADR-0059; strongest, in-binary) |
| **D** | **Identity: in-house auth vs external IdP** | In-house WebAuthn-in-binary (`webauthn-rs`) **vs** external IdP (self-hosted Keycloak / managed Cognito/Auth0) | In-house: auth surface auditable in the ABERP binary, no third-party trust boundary, but ABERP owns the auth code; external IdP: standard OIDC + delegated user mgmt, but an opaque/second trust surface (managed = lock-in) | **In-house `webauthn-rs`** (revisits ADR-0007 §Auth "cloud auth is OIDC"; lock-in-min + 1 MAU has no user-pool admin to delegate). Self-hosted Keycloak is the swap-seam alt if standard OIDC is later required |
| **E** | **Tenant-isolation model** | db-per-tenant **vs** schema-per-tenant **vs** shared-schema (`tenant_id` + RLS) | db-per-tenant: strongest isolation (no shared row space), app-layer/process boundary, engine-swappable — highest per-tenant ops cost at scale; shared-schema: cheapest ops but "one missed `WHERE tenant_id` leaks data"; schema-per-tenant: middle, muddier audit story | **db-per-tenant** (already the ADR-0002 cornerstone AND already implemented AND the only option satisfying the app-layer-isolation standing constraint; the easy fork) |
| **F** | **Single-tenant MVP first vs multi-tenant from cutover** | Ship single-tenant "reach-from-anywhere" first (defer Phase 5) **vs** build the multi-tenant router before cutover | Single-tenant-first: fastest to Ervin's stated goal ("reach it from anywhere"), matches ADR-0059's scope, Phase 5 added when a 2nd customer appears; multi-tenant-first: no re-cutover later, but Phase 5's cost before any external tenant exists | **Single-tenant MVP first**, Phase 5 deferred until a second tenant is real (reconciles ADR-0059 scope with the multi-tenant steer without paying for it early) |

Secondary forks (lower stakes, defaults from ADR-0059 stand): **dual-target vs full Tauri removal** → dual-target (preserve rollback + NAV-as-DR offline); **edge** → Caddy + Cloudflare free tier; **instance size** → 2-vCPU/4 GB class (Hetzner CX22) for DuckDB + SPA + backend headroom.

---

## 5. First shippable increment — Phase 1 in executable detail

**Goal:** land the three seams + the token fix as a normal desktop release (a PROD_Portable / PROD line cut), byte-behavior-identical on the desktop, so every later cloud phase has a seam to fill and the most urgent pre-exposure blocker is already closed.

**Scope & exact surfaces:**

1. **CSPRNG token** — `apps/aberp/src/serve.rs:3264 generate_session_token()`: swap the `SHA-256(SystemTime‖ULID‖PID)` body for `OsRng`-filled 32 bytes → base64url. Delete the "NOT a cryptographic key" caveat (`:3255-3262`). Dependency: `getrandom` (already transitive via `rand`; confirm in `Cargo.lock`). No change to `load_or_create_session_token` (`:3229`), the shell reader (`aberp-ui/src/lib.rs:857`), or `check_bearer_rejection` (`:25628`). **Migration note:** existing tokens keep working (the value is opaque and re-read, not re-derived); no forced re-mint.
2. **`SecretStore` trait** — new module `apps/aberp/src/secret_store.rs` (trait + `KeychainSecretStore`). Re-point the 9 call sites (§1.1) — `nav-transport/.../keychain.rs`, `smtp_credentials.rs`, `cad_blob.rs`, `audit_dap_boot.rs`, `storefront_origin_secret.rs`, `email_relay_credentials.rs`, `quote_intake_credentials.rs`, `serve.rs` (session token), `aberp-ui/src/lib.rs` (session-token read). Preserve `aberp.<domain>.<tenant>` key strings verbatim (no keychain-item rename → no operator re-entry). SMTP path: `smtp_credentials::read_password` → `store.get(...)`, still the sole SMTP reader into `SecretsCache`.
3. **`saas` feature + seams** — `apps/aberp/Cargo.toml` + `apps/aberp-ui/Cargo.toml`: `saas = []`. A transport-config seam and a `paths.rs` root resolver (single source for the `$HOME/.aberp/<tenant>/` roots). All no-ops under the default build.

**Security gates (Phase 1):** `cargo fmt` + build + test + `clippy -D warnings`; the `secrets_cache_boot` panic-guard still green; a focused adversarial review confirming (a) token unguessability from OS entropy, (b) no secret escapes the `SecretStore` seam / `Zeroizing` intact, (c) SMTP SPOC still single-reader, (d) `cargo build` (desktop) diff is behavior-identical. New pins: a test asserting `generate_session_token()` output is high-entropy and non-deterministic across calls; a gate asserting every keychain access goes through `SecretStore` (grep-gate mirroring the ADR-0099 opener-census style).

**Ships without regressing desktop:** all three items are default-build no-ops except the token RNG, which changes only the *source* of an already-opaque value. The desktop launch path (`run_prod.sh`/`run_portable.sh` → `aberp-ui` → `aberp serve --port 0`, loopback + fingerprint) is untouched.

---

## 6. Consequences

**Wins.** Reach-from-anywhere (Ervin's goal); the most urgent pre-exposure blocker (weak token) closed in Phase 1 on desktop; a lock-in-minimizing stack (Hetzner + sops/age + in-binary WebAuthn) with managed options preserved behind swap seams; the same audit-ledger invariants (ADR-0008/0030/0052) survive; desktop stays the rollback + NAV-as-DR surface.

**Trade-offs.** ~€8–12/mo OpEx from €0 (Hetzner CX22 ~€4.5 + object storage + domain); ABERP owns the auth + secrets code (the cost of avoiding lock-in — mitigated by mature crates `webauthn-rs`/`sops`); the multi-tenant router (Phase 5) is genuine new architecture, deferred until a real second tenant.

**Locked in.** `webauthn-rs` becomes load-bearing (mature, widely audited — acceptable). db-per-tenant deepens ADR-0002's per-tenant ops cost at scale (accepted; the isolation floor is worth it). The self-hostable defaults deliberately trade managed conveniences (secret rotation, edge autoscaling) for portability.

---

## 7. Adversarial review

- *"You reversed ADR-0059's host and secrets picks — is that just ideology?"* No: the item-8 directive makes lock-in-minimization a **standing constraint**, which outranks ADR-0059's ecosystem-reuse argument. Both reversals stay behind swap seams (`SecretStore` trait; a deploy/host abstraction), so flipping back to AWS is an impl swap, not a rewrite. Ervin can veto either at fork §4-A/B.
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
- **Phase 1 CSPRNG has no external token consumer.** Verified the token is opaque to the shell/SPA and not derived-from elsewhere; if some external tool reconstructs it (none found), that assumption breaks.
- This ADR is **design-only**: no application code was written, nothing merged/pushed/deployed/cut. It lives on `saas-migration-design`, local.
