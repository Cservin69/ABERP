# ADR-0029 — per-invoice export bundle — `aberp export-invoice-bundle` reads the audit ledger, packs the invoice's chain entries + verbatim NAV XMLs + a manifest into a `.tar.zst` archive, F10 (mirror file) deferred to PR-17 and F5 (attestation signing) deferred to a future PR — both gaps NAMED LOUD in the bundle manifest (ADR-0009 §8 closure at the audit-evidence-bundle level)

- **Status:** Accepted
- **Date:** 2026-05-22
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR — first major
  reading-side PR after the ADR-0009 §6 observation surface
  closed at the audit-evidence level (ADR-0028 / PR-15). Closes
  finding F13 (per-invoice export bundle multiply-gated). The
  bundle reader is the operator-visible artifact a NAV inspector
  walks when auditing one invoice's lifecycle. Load-bearing
  deltas: §1 (CLI verb + arg shape), §2 (bundle membership —
  whose entries count for "invoice X's bundle" when chain-link
  entries reference TWO invoice ids; the surfaced-conflict
  decision is "match either side and include"), §3 (bundle file
  shape — manifest.json + chain.jsonl + nav/<seq>_<kind>.xml
  inside a `.tar.zst` archive), §4 (F5 attestation-signing gate
  disposition — DEFERRED with the gap named in the manifest, not
  pre-emptively lifted per CLAUDE.md rule 2), §5 (F10 mirror-file
  gate disposition — DEFERRED to its own PR-17 with the gap
  named in the manifest; the September-cadence sharpening "land
  F10 before per-invoice export PR" is honestly surfaced as
  Reading B rejected per CLAUDE.md rule 7), §6 (chain verification
  posture — full-chain `verify_chain()` runs at bundle time, the
  result lands in the manifest as a load-bearing assertion), §7
  (operator-visible message + no audit-write — read-only command
  per ADR-0008 §"What goes in the ledger" + §"Access"), §8 (new
  workspace dependencies — `tar` + `zstd` from the Rust
  ecosystem, pinned). Does **not** supersede ADR-0008, ADR-0009
  §8, ADR-0028, ADR-0027, ADR-0026, ADR-0025, ADR-0024, or
  ADR-0023; all remain in force.
- **Related:**
  - **ADR-0008 §"Export"** — the export-bundle posture
    statement: "A tenant's full ledger can be exported as a
    signed bundle: entries + every attestation checkpoint + the
    binary hashes referenced + the schema versions used." PR-16
    lands the per-invoice slice of that surface; the per-tenant
    full export is a future PR with the same shape.
  - **ADR-0008 §"Storage"** — mirror file `<tenant>.audit.log`.
    F10 is the gate; §5 below decides the disposition.
  - **ADR-0008 §"External attestation"** — checkpoint cadence
    + signing key. F5 is the gate; §4 below decides the
    disposition.
  - **ADR-0009 §8** — the per-invoice export bundle
    requirements: "Every audit-ledger entry for that invoice, in
    order. The verbatim request and response XML for every NAV
    interaction. The verbatim `queryTransactionStatus` responses
    across the chain. Every attestation checkpoint covering the
    entries. The binary hash of the ABERP build at submission
    time. The schema hash that validated the XML at submission
    time. A signature over the whole bundle." Bundle output
    shape: "a single signed `.tar.zst` file."
  - **ADR-0019** — storage strategy: no foreign keys; ULID
    references are the chain link. The bundle reader walks the
    ledger by `invoice_id` field-equality (and the chain-link
    payloads' `base_invoice_id` / `storno_invoice_id` /
    `modification_invoice_id` field-equality), NOT by JOIN.
  - **ADR-0021** — pre-code consolidated baseline. The new
    workspace dependencies (`tar`, `zstd`) land here per
    ADR-0021 §A1's discipline ("Only crates actually exercised
    by a landed PR appear here").
  - **ADR-0023, ADR-0024** — STORNO + MODIFY chain-link payload
    shapes (`storno_invoice_id` + `base_invoice_id` /
    `modification_invoice_id` + `base_invoice_id`). The bundle-
    membership question for chain entries is decided in §2
    below.
  - **ADR-0028** — observe-receiver-confirmation predicate. The
    `invoice.annulment_receiver_confirmation` kind that PR-15
    landed is picked up by PR-16's `invoice.*` glob alongside
    every other lifecycle kind; no special-cased handling
    needed.
  - **Session 19 handoff F13** — per-invoice export bundle
    multiply-gated (F5 + F10). §4 + §5 below decide the gate
    dispositions; F13 closes with this ADR + PR-16.
  - **Session 6 handoff F10 sharpening** ("F10 must land
    before per-invoice export PR is reviewed for merge"). §5
    below surfaces this honestly as a Reading-B rejection per
    CLAUDE.md rule 7: not blindly followed; explained.
- **Source material:** ADR-0008 §"Storage" / §"External
  attestation" / §"Export" + ADR-0009 §8 (the per-invoice
  bundle contract).

## Context

After ADR-0028 / PR-15 closed the final ADR-0009 §6 observation
gap at the audit-evidence level (the operator can now drive the
full technical-annulment lifecycle end-to-end AND observe NAV-
side receiver-confirmation), the next operator-visible artifact
the NAV inspector expects is the **per-invoice audit-evidence
bundle** — ONE artifact per invoice that carries every NAV-side
observation chronologically.

ADR-0009 §8 named the shape; ADR-0008 §"Export" named the
posture ("re-generated on demand, which keeps the canonical
state in the ledger and avoids divergence"). The build-phase
question PR-16 must answer is: **which deferred prerequisites do
we lift now, and which do we honestly defer?**

### Prerequisite-gate state at PR-16 time

Session 19's handoff names PR-16 as "gated on F5 + F10":

- **F5 — Attestation signing key type.** ADR-0008 §"External
  attestation" + §"Adversarial review" name the surface; the
  open question is which key algorithm. Session 6's review
  recommended Ed25519. The trigger named in `adr/README.md`
  §Deferred: "first PR that exercises attestation cadence
  (long-running process, integration test crossing the
  cadence threshold, or cloud attestation publishing per
  ADR-0016)." **The trigger has NOT fired today** — no
  long-running process is in scope yet; the integration tests
  do not cross the cadence threshold; cloud attestation is
  ADR-0016 territory (also deferred).
- **F10 — ADR-0008 §"Storage" mirror file `<tenant>.audit.log`.**
  ADR-0008 §"Storage" names the mirror as "best-effort
  secondary evidence, not primary." The session 6 fortnightly
  review **sharpened** the trigger to: "must land before the
  per-invoice export PR is reviewed for merge." Sharpening is
  load-bearing — the review claimed the bundle's value as a
  second-source-of-truth depends on the mirror existing.

Two readings of the F10 sharpening:

- **Reading A: F10 lands first, then PR-16.** The sharpening is
  followed literally. PR-16 is gated on a PR-15.5 that lifts
  F10. Two PRs over two sessions; PR-15.5 adds the mirror-
  writer infrastructure to the audit-ledger crate, PR-16
  consumes it.
- **Reading B: PR-16 ships without F10, names the gap LOUD in
  the bundle manifest.** The bundle is then a "DB-sourced
  primary-evidence artifact" (chain-verified, hash-chained,
  tamper-evident at entry level) that explicitly notes the
  absence of the mirror-file second-source assertion. F10
  lands as a follow-on PR-17 that extends the manifest
  declaration (mirror existed at bundle time / mirror agreed
  with DB) — additive to PR-16's shape.

Two readings of the F5 deferral:

- **Reading A: Lift F5 in PR-16.** File an attestation-signing
  ADR (likely picking Ed25519 per session 6's recommendation),
  add the keychain item, implement attestation checkpoint
  cadence, sign the bundle. Multi-hundred-LoC infrastructure
  PR alongside the bundle-reader code. Token-budget honest
  estimate: PR-16 doubles in size; the verify loop covers two
  unrelated surfaces; the reviewer cannot easily separate
  "did we get the bundle reader right?" from "did we get the
  signing infrastructure right?"
- **Reading B: Defer F5 to its own future PR.** The bundle
  ships unsigned at PR-16 time. The manifest declares
  "signing deferred (F5 trigger not fired); the chain is
  internally verifiable via the entry hashes." A future PR
  (after the F5 trigger fires per `adr/README.md` §Deferred)
  adds a `signature` field to the manifest and a signed-
  detached-file alongside the archive — additive to PR-16's
  shape.

### Surfaced conflicts (CLAUDE.md rule 7)

Three ambiguities the build-phase will otherwise paper over:

1. **Bundle membership for chain-link entries.** STORNO + MODIFY
   chain-link payloads carry TWO invoice ids:
   `InvoiceStornoIssuedPayload` has `storno_invoice_id` +
   `base_invoice_id`; `InvoiceModificationIssuedPayload` has
   `modification_invoice_id` + `base_invoice_id`. The bundle for
   the BASE invoice should include chain-link entries that
   reference it via `base_invoice_id`. The bundle for the chain
   invoice (storno OR modification, which is itself an invoice
   with its own ULID + sequence) should include the SAME chain-
   link entry — it is operationally part of BOTH lifecycles.

   Three readings:

   - **Reading A: Match any-id-field, include in both bundles**
     (this ADR's pick). The bundle reader walks every entry;
     for each entry, deserializes a permissive Probe that
     extracts every invoice-id-shaped field (`invoice_id`,
     `storno_invoice_id`, `modification_invoice_id`,
     `base_invoice_id`); if any of them equal the target, the
     entry is included. This matches the NAV inspector's
     mental model: "show me everything that happened to invoice
     X, whether X is the base, the storno, or the modification."

   - **Reading B: Match only `invoice_id`, drop chain-link
     entries from the BASE's bundle.** Rejected — the BASE's
     bundle then silently omits the storno/modification that
     was issued against it; the inspector who reads the bundle
     does not learn the invoice was cancelled or amended. This
     is the silent-omission failure mode CLAUDE.md rule 12
     specifically names.

   - **Reading C: Render the BASE's bundle and the chain
     invoice's bundle as separate but cross-link them.** Adds
     a new bundle-manifest field "see-also: bundle for invoice
     Y." Rejected — the cross-link is a pointer to a future
     export the inspector may not have. Single-bundle-per-
     invoice with chain entries inlined is the structurally
     simpler artifact; cross-linking is a UX improvement that
     can land additively if the operator pattern surfaces a
     need.

   PR-16 commits to **Reading A**. The Probe walks the four
   id-shaped fields; a future payload that adds another
   id-shaped field (e.g., a hypothetical `voided_invoice_id`)
   extends the Probe in one place. The bundle reader includes
   any entry whose Probe yields a hit; the output is
   chronological by `seq` regardless of which id-side caused
   the match.

2. **F10 mirror-file gate disposition (Reading A vs Reading B
   above).** PR-16 commits to **Reading B** — defer F10 to
   PR-17, name the gap LOUD in the manifest. Rationale:

   - The bundle's primary value is "ONE artifact per invoice
     with every NAV-side observation in chronological order
     and a verifiable hash chain." The chain verification
     works against the DB-sourced entries today (PR-3
     shipped `Ledger::verify_chain`); the mirror file's
     value is "second-source corroboration if the DB file is
     corrupted between writes," which is real but secondary
     per ADR-0008 §"Storage" itself ("The mirror is
     recoverable from the DB; the DB is not recoverable
     from the mirror. We accept that the mirror is best-
     effort secondary evidence, not primary").
   - The session 6 sharpening ("must land before the per-
     invoice export PR is reviewed for merge") is the review's
     stronger reading; surfacing it honestly here means
     stating Ervin's PR-16 review IS the sharpening's
     review — and the review explicitly declines to gate
     PR-16 on F10's infrastructure PR. The sharpening is
     respected via the loud-named manifest field
     `"mirror_file_present"` set to `false` plus an explicit
     `"mirror_file_status"` of `"deferred-per-f10"` that a
     future contributor extending PR-17 flips when the
     mirror file is wired in.
   - F10's infrastructure PR (PR-17) needs design choices
     PR-16 should not pre-emptively make:
     - Per-tenant lock discipline against concurrent
       appenders (DuckDB single-writer file-locks today, but
       a future cloud-side multi-writer per ADR-0016 would
       need a different shape).
     - Mirror-file format (JSON-Lines per entry, CBOR per
       entry, or the canonical-CBOR shape the chain hash
       uses).
     - Recovery posture on partial writes (the mirror should
       fail loud and reject the next append, NOT silently
       skip).
     None of those decisions belong inside PR-16's verify
     loop; mixing them is the CLAUDE.md rule 3 violation
     PR-16's surgical-changes posture explicitly avoids.

3. **F5 attestation-signing gate disposition (Reading A vs
   Reading B above).** PR-16 commits to **Reading B** — defer
   F5 to its named-trigger future PR; name the gap LOUD in
   the manifest. Rationale:

   - F5's trigger has NOT FIRED. `adr/README.md` §Deferred
     names it explicitly. Pre-emptively lifting it is the
     CLAUDE.md rule 2 violation ("the moment you let Claude
     add 'for future flexibility,' you've added 200 lines
     you'll delete next quarter") — the signing
     infrastructure is large, the cadence-driver does not
     exist, and the bundle's value to a NAV inspector is
     dominated by the chain-verified entry list, not by the
     external signature.
   - The bundle's internal verifiability is unaffected. A
     NAV inspector receiving an unsigned bundle still gets:
     every entry's `entry_hash`, every entry's `prev_hash`
     pointing at the prior `entry_hash`, every entry's
     `binary_hash` for the producing build, and the chain-
     verify result against the genesis. The inspector can
     re-compute every hash from the bundle bytes alone; the
     internal chain is structurally tamper-evident.
   - The external signature would protect against "an
     attacker replaces the whole bundle with a different
     bundle and signs the replacement with a forged key" —
     mitigated by F5's named cloud-attestation publishing
     per ADR-0016 (also deferred). The trust anchor for the
     external signature is the same trust anchor that
     ADR-0016 will publish; absent ADR-0016, the local-only
     signature has no externally-verifiable trust path
     anyway. Naming the gap loud is the honest posture.

   The manifest field `"signed"` is `false` at PR-16 time,
   with `"signature_status"` set to `"deferred-per-f5"`. A
   future PR additively adds a `signature_*` block to the
   manifest and a detached-signature sibling file inside
   the archive.

## Decision

### 1. Operator CLI surface for export-invoice-bundle

**Subcommand name:** `aberp export-invoice-bundle`.

**Rationale for the verb.** Distinct from every prior verb-
family (`issue-*`, `request-*`, `submit-*`, `poll-*`, `observe-*`,
`retry-*`, `mark-*`) because the surface is read-only against
the audit ledger and produces a single artifact. The verb
`export-*` is reserved for **read-only operator-facing
artifact production**; the only member today is `export-invoice-
bundle`. Future members (e.g., `export-tenant-bundle` for the
full-tenant variant per ADR-0008 §"Export") inherit the
read-only-artifact-production posture.

The verb-object family now includes:

- `issue-*`, `request-*`, `submit-*`, `poll-*`, `observe-*`,
  `retry-*`, `mark-*` — as before.
- `export-*` — **read-only artifact production**; no NAV calls,
  no audit-ledger writes (per ADR-0008 §"What goes in the
  ledger": read-only queries do not produce audit entries).

The verb split is load-bearing per CLAUDE.md rule 12: an
operator seeing `submit-invoice-bundle` would reasonably assume
"this submits something to NAV" — exactly the wrong mental model
for a local-only artifact-production command. `generate-*` was
considered and rejected (too generic; collides with future
`generate-pdf-invoice`-class names). `dump-*` was rejected
(too sysadmin-flavoured; the inspector-facing artifact has a
canonical shape, not a free-form dump). `export-*` names the
intent: "produce an external-facing artifact suitable for
sharing with a NAV inspector."

**Argument shape** (clap-flavoured) — four fields, narrower than
every NAV-call command (no `--tax-number`, no `--endpoint`):

| Flag | Type | Default | Purpose |
|---|---|---|---|
| `--invoice-id` | `String` (`inv_<ULID>`) | none (required) | The invoice whose bundle to generate. The bundle includes every audit entry whose primary or chain-link invoice-id field matches per §2. |
| `--out` | `PathBuf` | none (required) | Path to the `.tar.zst` output file. The orchestrator refuses to overwrite an existing file by default (loud-fail per CLAUDE.md rule 12); `--allow-overwrite` opt-in below. |
| `--allow-overwrite` | `bool` | `false` | Opt-in to overwriting an existing `--out` file. Default-refuse posture preserves operator-visible artifacts from accidental clobbering. |
| `--db` | `PathBuf` | `./aberp.duckdb` | Tenant DuckDB. |
| `--tenant` | `String` | `"default"` | Tenant identifier — drives the audit-ledger genesis hash. |

**What `export-invoice-bundle` does NOT do.**

- **Does NOT call NAV.** Read-only over the audit ledger; no
  network access. The keychain is not consulted.
- **Does NOT mutate any billing row.** Read-only.
- **Does NOT write an audit-ledger entry.** Per ADR-0008
  §"What goes in the ledger": "Read-only queries (those go to
  the normal log)." A bundle export is a read-only artifact
  production; the operator-visible event lands in `tracing`
  output (RUST_LOG-routed), not in the audit ledger. **No
  new `EventKind` variant lands in PR-16.** The F12 four-edit
  ritual is not exercised this PR.
- **Does NOT sign the bundle.** Per §4. The manifest declares
  the unsigned posture loud.
- **Does NOT read or assert the mirror file.** Per §5. The
  manifest declares the deferred-mirror posture loud.
- **Does NOT mutate the audit-ledger schema.** PR-16 consumes
  the existing entry shape; no `EventKind` variant, no
  payload-struct addition (the audit-ledger storage layer is
  unchanged per CLAUDE.md rule 3).
- **Does NOT take a `--format` flag.** The bundle shape is
  fixed by ADR-0009 §8 + §3 below. A future ADR may add
  alternative output shapes (e.g., a JSON-only manifest for
  programmatic consumers); pre-emptively shipping flags for
  shapes that do not yet exist is the CLAUDE.md rule 2
  violation.

### 2. Bundle membership: any-id-field-equality match (decides §"Surfaced conflict 1")

The bundle reader walks every entry in the ledger, in `seq`
order, and decides inclusion per the following classification:

1. The entry's payload is deserialized via a permissive
   `BundleMembershipProbe` struct that carries every invoice-
   id-shaped field across every payload type, all as
   `Option<String>`:

   ```rust
   #[derive(serde::Deserialize)]
   struct BundleMembershipProbe {
       invoice_id: Option<String>,
       storno_invoice_id: Option<String>,
       modification_invoice_id: Option<String>,
       base_invoice_id: Option<String>,
   }
   ```

2. If ANY of the four fields equals the target `--invoice-id`,
   the entry is included.

3. Entries whose payload fails permissive deserialization OR
   whose Probe yields no id-field match are excluded.

The match is **anchored at the invoice-id-shaped FIELDS**, not
at a string-pattern search across the payload bytes. A future
payload that adds a fifth id-shaped field needs to extend the
Probe in one place — the same single-place-extension discipline
the F12 four-edit ritual exercises on `EventKind`. A unit test
pins the Probe's field set against the full payload-type list
(see §6 below).

**Why this matches the NAV inspector's mental model.** For a
base invoice that was later stornoed, the inspector reading the
BASE's bundle sees:

```
seq=12 invoice.draft_created          (base creation)
seq=13 invoice.sequence_reserved       (base seq alloc)
seq=14 invoice.submission_attempt      (base wire submit)
seq=15 invoice.submission_response     (base NAV txid)
seq=16 invoice.ack_status              (base SAVED)
seq=23 invoice.draft_created           (the storno's own draft)
seq=24 invoice.sequence_reserved       (the storno's own seq alloc)
seq=25 invoice.storno_issued           (the chain-link entry — INCLUDED via base_invoice_id)
seq=28 invoice.submission_attempt      (the storno's wire submit)
...
```

The storno's own draft / seq-reserved entries appear because
they carry the STORNO invoice id, not the base — they're
included in the storno's own bundle but NOT in the base's. The
chain-link entry (`invoice.storno_issued`) appears in BOTH
bundles because its payload carries both ids. The same logic
applies to MODIFY chains and to the technical-annulment-lineage
shared-key chain per ADR-0028 §7.

**Why permissive deserialization is the right shape.** The
audit ledger's payload bytes are typed but heterogeneous —
fifteen `EventKind` variants today, each with its own typed
payload struct. The Probe deserializes against a permissive
JSON Object shape: any field name is accepted; only the four
named fields are looked at. A future payload whose `invoice_id`
field is renamed (which would require a new EventKind per
ADR-0008's schema-versioning discipline) does not trip the
Probe.

### 3. Bundle file shape — `manifest.json` + `chain.jsonl` + `nav/<seq>_<kind>.xml` inside `.tar.zst`

The bundle is a single `.tar.zst` archive whose internal layout
is:

```
bundle/
  manifest.json              # bundle-level metadata + gate declarations
  chain.jsonl                # every included entry, one JSON object per line, seq-ordered
  nav/
    00012_invoice_submission_attempt.xml     # verbatim NAV request_xml for seq=12
    00013_invoice_submission_response.xml    # verbatim NAV response_xml for seq=13
    00014_invoice_ack_status.xml             # verbatim NAV response_xml for seq=14
    00025_invoice_storno_issued.xml          # (NO verbatim XML — chain-link entry has no NAV bytes)
    ...
```

**Top-level directory inside the archive: `bundle/`.** A NAV
inspector untarring the archive gets a single subdirectory
rather than the archive's contents splattered into the cwd.

**`manifest.json`** — single-object top-level manifest, fields
named below; the integration test pins the field set so a
future contributor renaming a field surfaces loud:

```json
{
  "version": 1,
  "invoice_id": "inv_01ARZ3NDEKTSV4RRFFQ69G5FAV",
  "tenant_id": "default",
  "generated_at": "2026-05-22T12:34:56Z",
  "binary_hash": "ab12...64-hex-chars",
  "nav_xsd_version": "3.0",
  "chain_verified": true,
  "chain_verified_entries": 285,
  "entries_in_bundle": 12,
  "signed": false,
  "signature_status": "deferred-per-f5",
  "mirror_file_present": false,
  "mirror_file_status": "deferred-per-f10"
}
```

Field semantics:

- `version` — manifest schema version. PR-16 ships `1`. Future
  shape changes bump the version; a future bundle-verifier
  understands every version it needs to.
- `invoice_id` — the `--invoice-id` operand verbatim.
- `tenant_id` — from `LedgerMeta` (the tenant whose chain is
  being exported).
- `generated_at` — RFC3339 UTC timestamp at bundle-write
  time. Same shape ADR-0008 §"Entry shape" uses for
  `time_wall`.
- `binary_hash` — hex-encoded SHA-256 of the producing ABERP
  binary. Re-computed at bundle time, not read from the
  entries (the bundle MAY include entries from earlier
  binaries; the manifest's `binary_hash` names the build
  that PRODUCED THE BUNDLE per ADR-0008 §"Adversarial review"
  bullet 2, distinct from the per-entry `binary_hash` already
  in `chain.jsonl`).
- `nav_xsd_version` — `aberp_nav_xsd_validator::NAV_XSD_VERSION`
  constant ("3.0"). Same shape `serve.rs::handle_health` uses.
- `chain_verified` — boolean; `true` iff the full-chain
  verification succeeded against the tenant's genesis at
  bundle-time. If `false`, the bundle reader REFUSES to write
  the bundle (loud-fail per CLAUDE.md rule 12 — a tampered
  chain must not be exported as if it were valid). The
  bundle-time chain verify is over the FULL chain, not just
  the invoice's slice, because verification is structurally a
  full-chain operation per ADR-0008 §"Hash chain".
- `chain_verified_entries` — count of entries the chain verify
  walked (the FULL chain count, not the bundle's slice count).
- `entries_in_bundle` — count of entries the bundle's
  `chain.jsonl` carries (the per-invoice slice count).
- `signed` — boolean; `false` at PR-16 time per §4.
- `signature_status` — string; `"deferred-per-f5"` at PR-16
  time. A future PR that wires F5 changes this to
  `"ed25519"` (or whatever F5's named-trigger ADR picks)
  and adds sibling `signature_*` fields.
- `mirror_file_present` — boolean; `false` at PR-16 time per
  §5. A future PR-17 (F10 lift) extends the bundle to read
  the mirror file (if present) and assert agreement; the
  field becomes `true` and a sibling `mirror_file_status`
  reports `"verified-agreement"` or `"divergence-detected"`.
- `mirror_file_status` — string; `"deferred-per-f10"` at
  PR-16 time.

**`chain.jsonl`** — one JSON object per line, one line per
entry, in seq order (oldest first). Each line is the entry
serialized with every ADR-0008 §"Entry shape" field:

```jsonl
{"id":"aud_01...","seq":12,"prev_hash":"hex","time_wall":"...","time_mono":12345,"actor":"...","binary_hash":"hex","tenant_id":"default","kind":"invoice.draft_created","payload":"<base64>","idempotency_key":"key","entry_hash":"hex"}
```

The `payload` field is base64-encoded bytes (the raw JSON
bytes from the typed payload struct). Including the verbatim
bytes per ADR-0008 §"Entry shape" preserves the audit-evidence
posture: a NAV inspector reading the bundle can re-compute the
`entry_hash` from the canonical-CBOR encoding of the same fields
and verify the chain locally without needing ABERP.

**`nav/<seq-padded>_<kind>.xml`** — for every entry whose
payload's typed shape carries `request_xml` OR `response_xml`,
the verbatim bytes are written as a separate file under `nav/`
named by seq-zero-padded-to-5-digits + kind. The
`EventKind::as_str()` storage form uses dots (e.g.
`"invoice.submission_attempt"`); the bundle FILENAME transforms
dots to underscores (`invoice_submission_attempt`) so an
inspector's filename viewer does not interpret the kind name
as a multi-extension filename. The canonical dotted form is
preserved on the `kind` field of each `chain.jsonl` line — only
the filename transforms. Kinds with verbatim XML payloads
(today):

- `invoice.submission_attempt` — payload's `request_xml` →
  `nav/<seq>_invoice_submission_attempt.xml`
- `invoice.submission_response` → `response_xml`
- `invoice.ack_status` → `response_xml`
- `invoice.annulment_submission_attempt` → `request_xml`
- `invoice.annulment_submission_response` → `response_xml`
- `invoice.annulment_ack_status` → `response_xml`
- `invoice.annulment_receiver_confirmation` → `response_xml`

Kinds WITHOUT verbatim XML (no file under `nav/`): `test`,
`invoice.sequence_reserved`, `invoice.draft_created`,
`invoice.retry_requested`, `invoice.marked_abandoned`,
`invoice.storno_issued`, `invoice.modification_issued`,
`invoice.technical_annulment_requested`.

**Why one file per NAV entry instead of inlining inside
`chain.jsonl`.** A NAV inspector untarring the bundle wants to
open `nav/00012_invoice_submission_attempt.xml` in any XML
viewer and see the actual XML — not navigate a JSON encoding of
base64-encoded XML. The separate-files shape preserves
operator-friendly inspectability; the canonical bytes for hash
verification still live in the `payload` field of
`chain.jsonl` per ADR-0008 §"Entry shape".

### 4. F5 attestation-signing gate disposition: DEFERRED (decides §"Surfaced conflict 3")

The bundle ships **unsigned** at PR-16 time. Per the §"Surfaced
conflict 3" analysis above:

- The manifest declares `"signed": false` and
  `"signature_status": "deferred-per-f5"`.
- The bundle's internal verifiability is preserved by the
  entry-level hash chain (re-computable from `chain.jsonl`
  alone).
- A future PR (after F5's named trigger fires) extends the
  manifest additively with `signature_*` fields and adds a
  detached-signature sibling file inside the archive. The
  manifest schema version remains `1` if the addition is
  forward-compatible; bumps to `2` if the addition breaks
  parser compatibility (the future PR decides this on
  evidence).

**Trigger for the F5 lift, unchanged from `adr/README.md`:**
"first PR that exercises attestation cadence (long-running
process, integration test crossing the cadence threshold, or
cloud attestation publishing per ADR-0016)." The bundle reader
is single-shot, not long-running; it does not exercise the
cadence; ADR-0016 is deferred. **The trigger has not fired
through PR-16; the bundle therefore ships without signing per
CLAUDE.md rule 2.**

### 5. F10 mirror-file gate disposition: DEFERRED to PR-17 (decides §"Surfaced conflict 2")

The bundle ships **without mirror-file assertion** at PR-16
time. The session 6 sharpening ("F10 must land before per-
invoice export PR is reviewed for merge") is reviewed and
explicitly NOT followed per CLAUDE.md rule 7 + the §"Surfaced
conflict 2" analysis above:

- The manifest declares `"mirror_file_present": false` and
  `"mirror_file_status": "deferred-per-f10"`.
- PR-16's verify loop covers the bundle reader; PR-17's
  verify loop will cover the mirror writer.
- PR-17's lift extends the bundle reader additively — the
  reader, on detecting the mirror file at the conventional
  path `<db>.audit.log`, reads it, asserts agreement against
  the DB-sourced entries, and reports
  `"mirror_file_status": "verified-agreement"` (or
  `"divergence-detected"` if the mirror diverges, with the
  bundle output refused per CLAUDE.md rule 12).

**Trigger for PR-17, named here:** "first PR that lifts F10
per the session 6 fortnightly sharpening, OR first operational
incident where DB-file corruption requires the mirror as the
secondary evidence (whichever fires first)." The sharpening
remains a real review-bar item; the honest answer in this ADR
is "the bar will be cleared by PR-17 additively, with PR-16
landing the bundle reader's chassis."

### 6. Chain verification at bundle time

PR-16 runs `Ledger::verify_chain()` AT BUNDLE-WRITE TIME, before
any bytes go into the archive. The verify return value is the
load-bearing assertion in the manifest's `chain_verified` field.

**Verification posture.**

- **Full-chain verify, not slice-verify.** Per ADR-0008
  §"Hash chain", verification is a linear pass from genesis
  to head. A "verify only the entries in this bundle" shape
  is structurally impossible — the entry-hash chain requires
  the prior `entry_hash` to compute the next `prev_hash`,
  and the prior entry is not necessarily in the bundle's
  slice.
- **Bundle refused on verify failure.** If `verify_chain()`
  returns `Err(_)`, the bundle writer aborts with a loud
  operator-visible error message naming the diagnostic.
  Producing a bundle from a tampered chain would mislead the
  NAV inspector into trusting a forged history; CLAUDE.md
  rule 12 names this exact failure mode.
- **Manifest's `chain_verified_entries`** = the full-chain
  count (what `verify_chain()` returned). The slice count
  (what the bundle includes) is `entries_in_bundle`.

**Why bundle-time verification.** The operator's question when
running `export-invoice-bundle` is "is this bundle the actual
audit trail?" — the verify result is the answer. Skipping
verification would let a chain that's tampered between writes
ship a bundle that looks authoritative; the loud-failure
posture is non-negotiable per ADR-0008 §"Adversarial review"
bullet 1.

### 7. Operator-visible message — read-only, no audit-write

On success, the operator-visible message names:

- The path to the output `.tar.zst` file.
- The full-chain verify result + entry counts.
- The deferred-gate declarations (signing-deferred-per-F5,
  mirror-deferred-per-F10).

Sample shape (printed to stdout + emitted via `tracing::info!`):

```
export-invoice-bundle OK: invoice <id> -> wrote bundle to
<path> (audit chain verified across <N> entries; <M> entries
in bundle). NOTE: this bundle is UNSIGNED (signing deferred per
F5; the chain-verify result above is internally verifiable from
the bundle's chain.jsonl alone), AND ships without mirror-file
second-source assertion (deferred per F10). A future PR will
add both additively without changing the bundle's existing
shape.
```

**No audit-ledger write.** Per ADR-0008 §"What goes in the
ledger" + §"Access": "Read-only queries (those go to the
normal log)." The bundle export is a read-only operation; the
operator-visible event lands in `tracing` output (RUST_LOG-
routed), not in the audit ledger.

**Why not write a `BundleExported` audit entry.** Two
readings:

- **Reading A: Write an audit entry.** The bundle export is an
  operator-visible action; writing an audit entry would let a
  future operator see "Ervin exported the bundle for inv_X at
  T." The downside: the audit-ledger fills with read-event
  noise; the F12 four-edit ritual fires for a kind that
  carries no NAV evidence.
- **Reading B: Do not write an audit entry** (this ADR's
  pick). ADR-0008 §"What goes in the ledger" explicitly
  excludes read-only queries. Bundle export is a read-only
  query; it goes to the normal log per the ADR's posture.
  Following the ADR's existing distinction is the surgical-
  change posture CLAUDE.md rule 3 names.

PR-16 commits to **Reading B**. The `tracing::info!` line
above is the canonical record. A future operator-policy ADR
may extend the audit ledger to include read events if the
operational pattern surfaces a need; not pre-emptively here
per CLAUDE.md rule 2.

### 8. New workspace dependencies — `tar` + `zstd`

ADR-0009 §8 names the bundle output as `.tar.zst`. PR-16 adds
two new workspace dependencies per ADR-0021 §A1's discipline
("Only crates actually exercised by a landed PR appear here"):

- **`tar` crate** (pure Rust, MIT/Apache-2.0). Reads/writes
  POSIX tar archives. Default features sufficient; no native
  dependencies. Pin shape: `"0.4"` (latest stable minor as
  of 2026-05).
- **`zstd` crate** (Rust binding to libzstd, MIT/Apache-2.0).
  Provides streaming compression and decompression. Default
  features pull in the bundled libzstd via the `zstd-safe` /
  `zstd-sys` chain; no system-level dependency required. Pin
  shape: `"0.13"` (latest stable minor as of 2026-05).

**Why these two and not e.g. `tar-rs` + `flate2` (gzip).**
ADR-0009 §8 names `.tar.zst` explicitly. Zstandard's
compression ratio + speed on log-shaped data (entries +
small XML bodies) is meaningfully better than gzip; the
inspector-facing artifact is smaller and faster to read.
The dep-surface cost is two transparent stable crates.

**Why not implement tar by hand.** Tar's spec is well-defined
but its escape-hatch surfaces (long filenames, sparse files,
extended attributes) accumulate fast. CLAUDE.md rule 2:
"minimum code, no speculative abstractions" — but writing a
minimal tar writer that handles the actual edge cases
(filenames over 100 chars, file-size 64-bit fields) is more
code than the dep import. The crate is the right call.

## Open questions

Tracked against the next fortnightly adversarial review and
named external-check items in `docs/research/nav-and-billingo.md`:

- **Per-tenant bundle generation cost at scale.** PR-16's
  bundle reader runs `verify_chain()` over the FULL ledger
  on each export. For a tenant with 100K+ entries, this is
  measurable but bounded (the chain verify is a linear hash
  walk). If the operational pattern surfaces a "bundle
  generation takes minutes" complaint, the named-trigger PR
  adds a per-tenant cached-verify checkpoint that the
  bundle reader can resume from. Not pre-emptively here.
- **Verbatim XML file naming collision risk.** The
  `<seq>_<kind>.xml` naming guarantees uniqueness per
  bundle (seq is monotonic). Cross-tenant collision is not
  a concern (each bundle is single-tenant). No further
  action.
- **Manifest JSON shape stability.** PR-16 ships
  `version: 1`. A future-PR contributor extending the
  manifest must bump the version IF the change breaks
  parser compatibility (a new field that the prior
  parsers would refuse to accept). Additive additions
  preserve `version: 1`; renames/removals require a
  bump. The bundle-shape pin in `mod tests` catches a
  silent breaking change at commit time.
- **PR-17 / F10 lift cadence.** Named as the next session
  candidate after PR-16. The F10 sharpening's reviewer
  (Ervin) accepted the deferral here; if a later review
  re-asserts the original sharpening, PR-17 jumps the
  queue.

## Consequences

**What gets easier**

- A NAV inspector auditing one invoice gets ONE artifact
  (`bundle-<invoice>-<time>.tar.zst`) that carries the full
  lifecycle in chronological order + every NAV-side
  request/response XML as a directly-inspectable file. The
  ADR-0009 §8 contract is met at the audit-evidence-bundle
  level; the unsigned + no-mirror gaps are named loud in the
  manifest, not silently omitted.
- The technical-annulment lifecycle (PR-12 through PR-15)
  becomes inspector-presentable as a single artifact. Every
  lifecycle kind PR-15's `invoice.*` glob covers — including
  the new `invoice.annulment_receiver_confirmation` — lands
  in the bundle without special-cased handling.
- The F13 multiply-gated finding closes at the bundle-reader
  level. The remaining gates (F5 attestation, F10 mirror)
  are surfaced loud in the manifest and land additively in
  future trigger-driven PRs.
- The chain-link bundle membership (BASE's bundle includes
  storno + modification chain entries) is structurally
  enforced by the any-id-field-equality Probe; an inspector
  reading the BASE's bundle sees the cancellation /
  amendment lineage without manual cross-walking.
- The deferred-gate posture is operationally
  transparent — a future contributor extending PR-17 (F10)
  or the F5-lift PR sees the manifest field they extend
  named loud, alongside the deferral string they replace.
  The shape grows additively; no rewrites needed.

**What gets harder**

- The CLI surface now has **fourteen** subcommands
  (issue-invoice, submit-invoice, setup-nav-credentials,
  poll-ack, retry-submission, mark-abandoned, serve,
  issue-storno, issue-modification, request-technical-
  annulment, submit-annulment, poll-annulment-ack,
  observe-receiver-confirmation, export-invoice-bundle).
  The command-group split per ADR-0026 §"Consequences" +
  ADR-0027 §"Consequences" + ADR-0028 §"Consequences"
  remains the named future direction if operator feedback
  shows the flat list is unwieldy. The `export-*` verb
  begins a new family; the next member is the eventual
  full-tenant export per ADR-0008 §"Export".
- Two new workspace dependencies (`tar`, `zstd`). The
  `tar` crate has had stable API since 0.4 (2017); the
  `zstd` crate's bundled libzstd is the standard Rust
  pattern. Both are MIT/Apache-2.0 and pass the
  `deny.toml` license allowlist. The supply-chain surface
  grows minimally.
- The bundle-reader's permissive Probe shape (§2) means a
  future payload type that carries a fifth id-shaped field
  must extend the Probe; missing the extension is a silent
  exclusion (the entry won't appear in any bundle). The
  unit test in §6 pins this against the current payload-
  type list; a future contributor adding a new id-shaped
  field updates the test.
- The bundle-time full-chain verify is structurally an
  O(N) operation over the tenant's entire ledger. For
  typical per-tenant volumes (a single SME's annual
  invoice count), this is bounded; at hyperscale (which
  ADR-0009 §"Adversarial review" already names as out of
  scope), a different shape would be needed. The trade-
  off is documented; not pre-emptively redesigned.

**What we lock ourselves into**

- Subcommand name `aberp export-invoice-bundle` and arg
  names (`--invoice-id`, `--out`, `--allow-overwrite`,
  `--db`, `--tenant`). Rename requires an amendment ADR.
- The bundle's internal layout (`bundle/` directory with
  `manifest.json` + `chain.jsonl` + `nav/<seq>_<kind>.xml`).
  Shape changes are forward-compatible additions
  (manifest version bump on breaking changes); a complete
  restructure requires a new manifest version + a parser
  switch.
- The any-id-field-equality bundle-membership rule (§2).
  Future payload types whose id-shaped fields use a
  different name (e.g., a hypothetical
  `voided_invoice_id`) require a Probe extension.
- The unsigned-bundle posture at PR-16 time. The future
  signing PR extends additively but the historic PR-16
  bundles remain UNSIGNED in their stored form; a future
  inspector verifying an old bundle treats the absence
  of a signature as an expected condition for bundles
  generated before the F5 lift, per the manifest's
  `signature_status` field.
- The deferred-mirror posture at PR-16 time. Same
  additive-extension contract for PR-17 + future bundle
  reads.
- The decision to NOT write an audit-ledger entry on
  bundle export (§7). A future operator-policy ADR that
  reverses this requires a `BundleExported` EventKind +
  the F12 ritual.
- The `.tar.zst` archive format (§3 + §8). A future
  alternative output ADR may add a different shape
  alongside; the existing shape stays.

## Adversarial review

A hostile NAV inspector + a hostile-engineer review,
alternating. ADR-README bar is three; four surfaced because
the F5 + F10 deferrals + the any-id-field probe are load-
bearing decisions that diverge from prior PR shapes.

1. **"You defer F5 and F10 in this PR despite the session 6
   fortnightly review explicitly sharpening F10's trigger to
   'must land before per-invoice export PR is reviewed for
   merge.' This is not 'surfacing the conflict' (CLAUDE.md
   rule 7) — it is OVERRIDING THE REVIEW. A future reader of
   this ADR sees 'deferred per F10 with the gap named loud
   in the manifest' and assumes the deferral was always
   acceptable; the historic sharpening's review-bar is
   silently dropped."** The risk is real and acknowledged.
   The mitigation is in two parts:
   - Section §5 above NAMES the sharpening explicitly and
     EXPLAINS the disposition. The future reader of the ADR
     sees the override in plain text, with the rationale,
     not as an unstated assumption.
   - Ervin's review of this ADR is itself the sharpening's
     review-bar. The reviewer can override their own prior
     sharpening with a recorded rationale; the recorded
     rationale lives in §5's "Reading A vs Reading B"
     analysis.
   **Accepted with override explicitly recorded.** Future
   reviewers see the override and can re-litigate it if
   PR-17 turns out to surface operational problems that
   the sharpening would have prevented.

2. **"The bundle ships unsigned and the manifest field
   `signed: false` is the only loud-fail-bait against a
   malicious operator who could replace the bundle bytes
   with a different bundle and present it to an inspector.
   A NAV inspector who does not check `signed: false`
   could be fooled."** Accepted, surfaced. Three layers
   of mitigation:
   - Every entry in `chain.jsonl` carries `prev_hash` +
     `entry_hash`, both deterministically re-computable
     from the canonical-CBOR encoding of the entry's
     fields. A re-computation by the inspector (even
     manually) detects entry-level tampering.
   - The `chain_verified: true` field in the manifest is
     a load-bearing claim by ABERP about its own ledger
     state at bundle-time. A future bundle-verifier tool
     re-runs the verification against the bundle's
     entries alone and re-asserts the boolean — a
     mismatch surfaces immediately.
   - The F5 lift's trigger is "first PR that exercises
     attestation cadence" — when it fires, the future
     signed-bundle PR extends the manifest additively;
     bundles generated AFTER that PR get the cryptographic
     external-verification surface. Bundles generated
     BEFORE that PR (including PR-16's outputs) retain
     their internal-verifiability posture.
   The mitigation is documented; the inspector's review
   workflow is reinforced (read the manifest, run the
   verifier, compare hashes) rather than mechanized at
   PR-16's commit-time. **Accepted with trigger named.**

3. **"The any-id-field-equality probe (§2) means a future
   payload that REUSES an existing field name (e.g.,
   `invoice_id`) but for a SEMANTICALLY DIFFERENT entity
   would silently cause cross-bundle contamination. For
   instance, if a future PR adds a `CustomerNotified`
   payload whose `invoice_id` names the customer's
   reference number (which happens to collide with a
   real ABERP invoice ULID), the bundle for the colliding
   invoice would include the unrelated entry."** Accepted,
   surfaced. The mitigation:
   - ABERP's invoice ids are prefixed `inv_<ULID>` per
     ADR-0005. A field that happens to be named
     `invoice_id` but carries a customer's reference
     number is structurally a different identifier shape
     (ULIDs are 26 Crockford-base32 characters,
     prefixed `inv_`). A direct text equality match
     against a non-ULID string would not hit.
   - The discipline that the Probe field set MUST be
     reviewed against the payload-type list when a new
     payload type lands (§6 unit test) catches the
     mistake at the source: if a future payload adds an
     `invoice_id` field that's NOT an `inv_<ULID>` ULID,
     the contributor must either rename the field (the
     right call — the audit ledger should not conflate
     reference-id namespaces) OR extend the Probe with
     a typed wrapper (less likely, but possible).
   - The bundle's `chain.jsonl` line includes the
     `kind` field; an inspector reading the bundle can
     filter on kind if they're triangulating across
     entry types. A spurious entry would surface as
     "why is this kind in this invoice's bundle?" — a
     human-noticeable anomaly, not a silent inclusion.
   The risk is real and documented; the F12-like
   discipline (review Probe field set against new
   payloads) is the named mitigation.

4. **"The bundle's `version: 1` manifest field is a
   schema-stability declaration but PR-16 does not ship
   the corresponding bundle-VERIFIER tool. A NAV
   inspector reading an ABERP bundle today must either
   manually re-compute hashes (operator-unfriendly) or
   install a future ABERP verifier (which does not yet
   exist). The 'verifiable by anyone holding the
   attestation public key' phrase in ADR-0009 §8 sets a
   contract that PR-16 does not technically meet."**
   Accepted, surfaced. Two readings:
   - **Reading A: Ship the bundle reader AND the bundle
     verifier in PR-16.** Roughly doubles the PR; the
     verifier is its own non-trivial surface (it must
     re-implement the canonical-CBOR encoding, the SHA-
     256 chain verification, the manifest parse).
     CLAUDE.md rule 3 (surgical changes) — the bundle
     READER is one operator-facing artifact; the
     bundle VERIFIER is a second, distinct artifact;
     blending them is the trap rule 3 names.
   - **Reading B: Ship the reader in PR-16; the
     verifier lands as PR-18 (or as part of the F5
     lift, whichever fires first)** (this ADR's pick).
     The contract is met for the bundle's PRODUCTION
     and structural inspectability; verifier
     development can land on its own evidence (real
     bundles to test against). The named trigger:
     "first PR that produces a signed bundle OR first
     external-inspector test of an existing bundle,
     whichever fires first."
   **Accepted — the verifier is a separate artifact;
   pre-emptively shipping it conflates surfaces.**

## Alternatives considered

- **Lift F10 (mirror file) in PR-16.** Rejected per §5 +
  §"Surfaced conflict 2". The mirror writer is its own
  design surface (per-tenant lock discipline, mirror-
  file format, partial-write recovery posture); mixing it
  with the bundle reader violates CLAUDE.md rule 3.

- **Lift F5 (attestation signing) in PR-16.** Rejected per
  §4 + §"Surfaced conflict 3". F5's named trigger has
  NOT fired; pre-emptively shipping signing infrastructure
  is the CLAUDE.md rule 2 violation. The bundle's internal
  verifiability is preserved by the entry-level chain.

- **Bundle = single JSON file (no archive).** Rejected per
  §3 + §8. ADR-0009 §8 explicitly names `.tar.zst`. The
  separate-files-under-`nav/` shape preserves operator-
  friendly XML inspectability that a single-file JSON
  would compress into base64-blob fields.

- **Bundle directory output instead of an archive.**
  Rejected — the operator must hand a NAV inspector ONE
  artifact, not a directory with multiple files. A future
  `--directory` flag that produces an unpacked directory
  alongside the archive could land additively if
  operational pattern shows a need; not pre-emptively
  here per CLAUDE.md rule 2.

- **Bundle membership probe matches `invoice_id` only,
  drops chain-link entries from the BASE's bundle.**
  Rejected per §2 + §"Surfaced conflict 1". The silent-
  omission failure mode CLAUDE.md rule 12 names.

- **Write a `BundleExported` audit entry on every export.**
  Rejected per §7. ADR-0008 §"What goes in the ledger"
  explicitly excludes read-only queries.

- **Take `--format` flag with values
  `tar-zst | tar | tar-gz | json`.** Rejected per §1's
  "What this command does NOT do." Pre-emptive flag for
  shapes that do not yet exist is the CLAUDE.md rule 2
  violation; future ADR amendments can add formats with
  their own named triggers.

- **Skip chain verification at bundle time.** Rejected per
  §6. A tampered chain that ships as a bundle is the
  exact failure mode CLAUDE.md rule 12 names; the loud-
  failure posture is non-negotiable.

- **Use `cargo deny` allowlist to refuse the `tar` /
  `zstd` deps.** Considered but rejected — both are
  MIT/Apache-2.0 dual-licensed; the `tar` crate is the
  canonical Rust tar implementation; the `zstd` crate
  is the canonical Rust binding. The license allowlist
  passes; supply-chain review trivially clears.

## Follow-on PRs unblocked by this decision

- **PR-16 — per-invoice export bundle code.**
  Implements §1-§8 above plus:
  - `apps/aberp/src/export_invoice_bundle.rs`
    (orchestration).
  - `apps/aberp/src/cli.rs` (`Command::ExportInvoiceBundle`
    + `ExportInvoiceBundleArgs`).
  - `apps/aberp/src/lib.rs` (`pub mod
    export_invoice_bundle`).
  - `apps/aberp/src/main.rs` (dispatch arm + import).
  - Workspace `Cargo.toml`: `tar` + `zstd` workspace deps.
  - `apps/aberp/Cargo.toml`: per-crate `tar` + `zstd`
    membership.
  - Tests: bundle membership probe field set pin,
    permissive deserialization round-trip, full-chain
    verify gate, manifest field set pin, `nav/<seq>_
    <kind>.xml` filename composition, `.tar.zst`
    archive shape round-trip, refuse-overwrite default,
    chain-link entry inclusion (storno + modification
    in BASE's bundle).
  - One env-gated integration test that drives the full
    issue → submit → poll → export pipeline end-to-end
    and asserts the bundle contains the expected entries
    in seq order.

- **PR-17 — F10 mirror file** (the named-trigger PR per
  §5). Adds the audit-ledger crate's mirror-writer; the
  bundle reader's manifest field `mirror_file_present`
  flips to `true` + `mirror_file_status` flips from
  `"deferred-per-f10"` to `"verified-agreement"` (or
  `"divergence-detected"`). Touches `crates/audit-ledger`
  internals; bundle reader extension is additive.

- **PR-18 — bundle verifier tool** (the named-trigger PR
  per §"Adversarial review 4"). Re-implements the chain
  verification against bundle-resident entries alone;
  shipped as a separate CLI binary (`aberp-verify` or
  similar) so a NAV inspector can verify a bundle
  without trusting the producing build.

- **Future F5 lift PR** (the named-trigger PR per §4).
  Adds attestation-checkpoint cadence + bundle signing.
  Bundle's `signed: false` + `signature_status:
  deferred-per-f5` fields flip additively; sibling
  signature-detached file lands inside the archive.

- **Future per-tenant full-export PR** (per ADR-0008
  §"Export"). Same `export-*` verb family; same archive
  shape; same chain-verify gate. Inherits PR-16's
  precedent.

- **Future operator-policy ADR** (potential — only if
  operational pattern surfaces a need): write an audit
  entry on bundle export. Would re-fire the F12 four-
  edit ritual once.
