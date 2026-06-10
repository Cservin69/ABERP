# S338 — `/quote` material dropdown shows the generic fallback: catalogue-push delivery defect

**Status:** root cause found + fixed. Cross-repo (ABERP + storefront). Fix is
storefront-side (one regex); ABERP change is a wire-contract regression test.

## Symptom

Prod storefront `/quote` renders the hard-coded generic-material fallback
(`Aluminum / Steel / Stainless / Brass / Plastic`) instead of the
catalogue-driven dropdown. The S336 audit attributed it to
`catalogue_push.rs` "not delivering the snapshot to prod."

## Verify-first — what the push path actually does

1. **`apps/aberp/src/catalogue_push.rs`** — a boot-spawned daemon PUTs the
   public projection of `quoting_materials` to
   `{base_url}/api/catalogue/materials` every 15 min and on every operator
   write. Spawned at `serve.rs:1561`, **gated on quote-intake being
   configured** (same storefront surface, SPOC). Bearer + base_url come from
   the shared `StorefrontCredentialHandle`.
2. **Storefront receiver** — `src/routes/api/catalogue/materials/+server.ts`
   `PUT`: `requireAdminAuth` (→ `ABERP_SITE_ADMIN_TOKEN`),
   `validateSnapshotBody`, then `writeCatalogueAtomic`. `GET` serves the
   cached snapshot. The `/quote` page (`+page.svelte:28-41`) fetches it on
   mount and renders the catalogue branch **iff `catalogueMaterials.length > 0`**.
3. **Auth** — quote-intake polls `{base_url}/api/quotes` and catalogue-push
   PUTs `{base_url}/api/catalogue/materials`, both with the **same**
   `ABERP_SITE_ADMIN_TOKEN`. No bearer/URL mismatch.
4. **Source** — `seed_if_empty` runs at boot (`serve.rs:770`); the table is
   never empty on a real instance. Not an empty-source defect.

Candidate root causes from the brief, ruled out by the above: daemon-not-
spawned (spawns with quote-intake), URL/bearer mismatch (same token+URL),
trigger-broken (cadence fires regardless), empty-source (seeded at boot).

## Root cause — a grade-validation contract mismatch (receiver 400s every push)

ABERP seeds and stores **real industry grade designations** as the
`quoting_materials.grade` PRIMARY KEY: `6061-T6`, `7075-T651`, `304`, `316`,
`Ti-6Al-4V`, `Inconel 718`, `PEEK`. ABERP imposes no charset on `grade`
(only non-empty — `validate_material_inputs`).

The storefront receiver validated every pushed grade against
`GRADE_RE = /^[A-Z][A-Z0-9_]*$/` (`catalogue-store.ts`). That regex rejects:

- digit-first grades (`304`, `6061-T6`, `7075-T651`)
- hyphens (`6061-T6`, `Ti-6Al-4V`)
- spaces (`Inconel 718`)
- lowercase (`Ti-6Al-4V`)

`validateSnapshotBody` rejects the **whole snapshot** on the first bad row
(atomic, by design), so **every** push from a real ABERP instance returned
**400**. The snapshot never persisted → `GET` returned `{materials: []}` →
`/quote` rendered the fallback forever. The storefront's own test fixtures
used sanitized grades (`AL_6061_T6`, `TI_6AL_4V`), so the contract drift was
invisible to CI.

The audit trail *did* record it: each cycle wrote
`MaterialCataloguePushed` with `outcome=unexpected_status, detail=HTTP 400`.
The diagnostic breadcrumb existed; nobody read it. (No new audit events
needed — the existing EventKind already records the failure.)

## Fix (storefront-side, surgical)

Relax `GRADE_RE` to the charset real grades actually use, while keeping an
alphanumeric-first-char rule and a closed allowlist that excludes all
control chars (CR/LF/NUL) and HTML/SQL metacharacters:

```
- const GRADE_RE = /^[A-Z][A-Z0-9_]*$/;
+ const GRADE_RE = /^[A-Za-z0-9][A-Za-z0-9 ._+/-]*$/;
```

The grade stays safe as an HTML `<option value>` and as the
`material_preference` echoed back to ABERP — the relaxed set still excludes
`< > & " ' ;` and control chars. Once the snapshot lands with real grades,
both the dropdown **and** the `/api/quote` submission check
(`currentCatalogueGrades`) work, because both key off the same persisted set.

ABERP needed no behaviour change — its grades were always the canonical
real-world values.

## Regression tests

- **ABERP** (`quoting_materials.rs`):
  - `s338_catalogue_push_delivers_snapshot_to_storefront_on_change` — seed →
    `list_public` → assert non-empty AND every grade satisfies the
    storefront-accepted contract (pinned as a helper kept in lockstep with
    `GRADE_RE`).
  - `s338_contract_helper_rejects_the_old_failure_shapes` — real grades pass,
    leading-separator / control-char / injection shapes still fail.
- **Storefront**:
  - `catalogue-store.spec.ts` — the three old tests that asserted the
    over-strict (buggy) behaviour were replaced: real seed grades now
    accepted; leading-separator and control/injection chars still rejected.
  - `catalogue.spec.ts` — `s338_catalogue_push_delivers_snapshot_to_storefront_on_change`
    (real grades PUT → 200) and
    `s338_storefront_renders_catalogue_dropdown_when_snapshot_present`
    (PUT → GET round-trips a non-empty snapshot, so the page renders the
    catalogue branch, not the fallback).

## Cross-repo ordering

Storefront receiver change ships first (it only *widens* what it accepts, so
it is backward-compatible with any current/future ABERP push). ABERP's change
is test-only; no coordination hazard. No prod deploy of a stricter contract,
so no flag-day.

## Gates

- Storefront: prettier+eslint ✓, svelte-check 0/0 ✓, vitest 361/361 ✓, build ✓.
- ABERP: `cargo fmt --check` ✓, `cargo clippy -p aberp` ✓ (clean),
  `quoting_materials` tests 7/7 ✓, `cargo build --release -p aberp` ✓.
