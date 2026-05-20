# ADR-0022 — NAV InvoiceData runtime XSD validator (PR-9-0)

- **Status:** Accepted
- **Date:** 2026-05-20
- **Deciders:** Ervin
- **Class:** Build-phase just-in-time ADR (per ADR-0021 §"Items
  deferred to build phase" — "XSD runtime validation crate" entry).
- **Related:**
  - ADR-0009 §1 (Schema-drift detection — NAV v3.0 XSD files vendored,
    on-mismatch the NAV adapter refuses to submit).
  - ADR-0009 §2 (`Ready` typestate — "passed local XSD validation").
  - ADR-0021 §"Items deferred to build phase" (this is the named-and-
    deferred item whose trigger has now fired).
  - ADR-0021 §Part A §8 (XML / SOAP — quick-xml; codegen-from-XSD
    rejected for the SOAP envelope; this ADR scopes the *runtime*
    validator separately).
  - PR-7-C `poll_ack::run_one_attempt`'s
    `QueryTransactionStatusResponseParse` arm comment ("If a future PR
    adds the XSD validator, this arm graduates to NonRetryable") — this
    ADR is that future PR.

## Context

ADR-0009 §1 calls for runtime XSD validation of the on-disk
`<InvoiceData>` payload before it goes on the wire. ADR-0021
§"Items deferred to build phase" deferred the choice of validator
crate (libxml FFI vs hand-rolled invariant check vs pure-Rust
validator-when-mature) with a named trigger: *"first PR
implementing schema-drift detection per ADR-0009 §1."*

PR-9 (operator UI scaffold) is that PR's near sibling — the UI
scaffold makes the issuance + submit + retry pipelines operator-
visible, and the operator-facing failure mode "we sent malformed
XML and NAV rejected it 10 seconds later" is exactly what the
validator's loud-fail-before-the-wire posture closes. ADR-0021's
named trigger has fired.

Three options have been on the table since ADR-0021's deferral:

| Option | What it is | Pros | Cons |
|---|---|---|---|
| **A. libxml FFI** | `libxml-rs` / `xmltree-rs` wrapping `libxml2` | Full XSD 1.0 validator, battle-tested | C dep — breaks reproducible single-static-binary posture (ADR-0001, ADR-0007 §Supply chain); `unsafe` FFI surface |
| **B. Hand-rolled invariant check** | Walk the parsed `<InvoiceData>` tree with `quick-xml` against an allowlist of required elements + cardinalities | Pure Rust; reuses the existing `quick-xml` 0.36 pin; no new dep, no C; single static binary preserved | Not literally XSD validation — catches structural drift, not every type-constraint XSD encodes |
| **C. Pure-Rust XSD validator** | `xmlschema-rs` or similar | Pure Rust + full XSD | Not mature; `xmlschema-rs` is alpha-quality with known correctness gaps as of 2026-05 |

### Constraints inherited from other ADRs

- **ADR-0001 + ADR-0007 §Supply chain.** Single static binary;
  reproducible build; license-clean dep set. A C dep on libxml2
  breaks the single-static-binary posture and adds a CVE surface
  outside the `cargo-deny` / `cargo-audit` toolchain.
- **ADR-0021 Part A §8.** `quick-xml` is the project's XML toolkit
  (`serialize` feature for both serialization on the way out and
  deserialization on the way back). No additional XML crate is
  needed for a hand-rolled walker.
- **ADR-0009 §1.** "On mismatch the NAV adapter **refuses to
  submit**" — loud-fail posture per CLAUDE.md rule 12. The
  validator's job is to halt the call site before the wire HTTP
  request happens; it is NOT to translate / repair / fall-back-
  warn.
- **ADR-0009 §2.** The `Ready` typestate is gated on "passed local
  XSD validation." Today the `IssueInvoice` flow builds the XML
  and writes it to disk without a validator pass; this ADR closes
  that gap as part of the issuance path.

### What is the validator validating

The on-disk `<InvoiceData>` XML bytes — the file that
`aberp issue-invoice --out ...` produces and that
`aberp submit-invoice` / `aberp retry-submission` reads back and
sends to NAV. The validator does NOT validate the surrounding NAV
SOAP envelope (the envelope is hand-constructed per
`crates/nav-transport/src/soap.rs` and its correctness is asserted
by the integration tests that exercise the live NAV endpoint per
PR-7-A/B/C/8). The narrow scope is intentional — the SOAP envelope
is single-author code that we own; the `<InvoiceData>` payload is
where future divergence between our XML builder and NAV's schema
expectations would surface.

## Decision

**Option B — hand-rolled invariant check, in a new
`crates/nav-xsd-validator` workspace crate.** The crate exposes a
single typed entry point — `validate_invoice_data(&[u8]) ->
Result<(), NavXsdValidationError>` — that walks the parsed
`<InvoiceData>` tree against a hand-rolled allowlist of required
elements + cardinalities + ASCII-shape constraints on numeric and
date fields, returning a typed error on any divergence.

### What "invariant check" covers in concrete terms

The validator's allowlist is hand-written against the v3.0
`InvoiceData` schema as exercised by the existing
`apps/aberp/src/nav_xml.rs` builder. Concretely, the validator
asserts:

1. **Root element.** `<InvoiceData>` with namespace
   `http://schemas.nav.gov.hu/OSA/3.0/data`. Any other root
   element or namespace is a loud-fail.
2. **Required top-level children.** `<invoiceNumber>`,
   `<invoiceIssueDate>`, `<invoiceMain>`. Order-sensitive per
   v3.0; any missing or out-of-order child is a loud-fail.
3. **`<invoiceIssueDate>` shape.** `YYYY-MM-DD` ASCII. Any other
   format is a loud-fail. (XSD `xs:date` accepts more, but NAV
   v3.0 specs the narrower form; ADR-0009 §1 names "loud-fail on
   schema-drift" — surfacing here keeps the failure off the wire.)
4. **`<invoiceMain>/<invoice>/<invoiceHead>` structure.**
   `<supplierInfo>` and `<customerInfo>` are required in that
   order. Inside each, the required leaf elements named by NAV
   v3.0 are required; unknown elements are loud-failed.
5. **`<invoiceLines>/<line>+` cardinality.** At least one
   `<line>`; each `<line>` has the required leaves
   (`<lineNumber>`, `<lineDescription>`, `<quantity>`,
   `<unitOfMeasure>`, `<unitPrice>`, `<lineNetAmount>`,
   `<lineVatRate>`, `<lineVatData>`).
6. **`<invoiceSummary>/<summaryNormal>`** required; its required
   leaf elements per NAV v3.0 enforced.
7. **Numeric shape on numeric fields.** Anything we send as
   "amount" is ASCII digits + optional decimal point per NAV
   v3.0; non-numeric content is a loud-fail.

The crate is intentionally narrow — it does NOT attempt every
XSD construct (no `xs:choice` modeling, no full
namespace-qualified attribute walks, no inheritance from
`xs:complexType` base types). The scope is the elements PR-5
through PR-8 actually emit; future PRs that add new optional
NAV elements (`deliveryDate`, `paymentDate`, storno
`invoiceReference`, etc.) extend the allowlist in the same
commit that adds the emitter — same F12-style four-edits
posture the existing project lives with.

### Wiring into the existing pipelines

The validator is called at **three** call sites:

1. **`issue_invoice::run`** — after `nav_xml::render_invoice_data`
   produces the XML bytes and immediately before
   `std::fs::write(&args.out, ...)`. If validation fails,
   `issue_invoice::run` loud-fails and the typestate does NOT
   advance to `Ready` (per ADR-0009 §2). This closes the
   ADR-0009 §2 `Ready`-state gate that has been latent since PR-5.
2. **`submit_invoice::run`** — after `std::fs::read(&args.invoice_xml)`
   and before any NAV call. If validation fails, no `tokenExchange`
   happens and no audit entry lands. This catches the case where
   the on-disk XML has been hand-edited between `issue-invoice` and
   `submit-invoice`.
3. **`retry_submission::run`** — same posture as
   `submit_invoice::run`. Mirrored at the same point in the
   pipeline (after the `std::fs::read`, before the NAV runtime
   builds).

### Graduating the poll loop's parse-failure arm

`poll_ack::run_one_attempt`'s
`QueryTransactionStatusResponseParse` arm carries a comment from
PR-7-C: *"If a future PR adds the XSD validator, this arm
graduates to NonRetryable."* This ADR is that future PR. The arm
is flipped to `NonRetryable` in the same commit that lands the
validator. Rationale: with the validator in place at issuance,
any parse failure on a NAV *response* means NAV is sending us a
shape we cannot parse — that is schema-drift on NAV's side, not a
transient transport blip, and retrying does not help.

### Conformance check

The validator's allowlist is the source of truth for "what the
NAV XML builder is allowed to emit." A second source of truth is
the builder itself (`apps/aberp/src/nav_xml.rs`). Divergence
between the two is exactly the failure mode CLAUDE.md rule 7
names. Two trap-doors close the divergence:

- **`tests/round_trip_invoice_data.rs`** — every fixture in
  `fixtures/` is round-tripped through the builder + validator.
  If the builder emits something the validator rejects, the test
  fails loud at commit time. Today's only fixture is
  `invoice_minimal.json`; future fixtures land in the same
  directory under the same convention.
- **`#[doc = include_str!]` of `nav_xml.rs`'s module header into
  the validator's module header.** A future contributor reading
  the validator sees the same scope-of-emission text the builder
  documents; mismatches surface at code-review time.

### Crate placement

`crates/nav-xsd-validator`. Sits at the same level as
`crates/audit-ledger` and `crates/nav-transport`. Library crate
(not module) because:

- It is consumed by the binary (`apps/aberp`) AND by future
  module-internal callers (the storno-chain PR-10+ flow will
  build storno-modify `<InvoiceData>` payloads inside the
  billing module's `invoice.rs` and validate before emitting
  them).
- It is not a billing concern — it is a NAV-wire-protocol
  concern, and the billing module's `api.rs` is the wrong
  surface for "is this XML on-the-wire-valid."
- The `crates/` directory is the project's convention for
  NAV-protocol-shaped reusable surfaces; the existing
  `crates/nav-transport` is its near sibling.

### thiserror discipline

Per ADR-0021 Part A §2: library crates expose typed errors via
`thiserror`. The validator's public error type is
`NavXsdValidationError` with one variant per failure class
(missing-required-child, unexpected-element, malformed-date,
non-numeric-amount, root-namespace-mismatch, …). The binary's
`anyhow::Result` boundary at the call sites converts via `?` /
`.context(...)` as elsewhere.

### What this ADR does NOT cover

- The vendored XSD files themselves (ADR-0009 §1's SHA-256
  allow-list posture). The hand-rolled validator does NOT load
  XSD files at runtime; the allowlist lives in Rust code. If a
  future maintainer wants to add the XSD-file SHA-256 check as
  defense-in-depth, that is a separate PR that builds on this
  one.
- Full XSD 1.0 conformance. The crate name is
  `nav-xsd-validator` because the *role* it plays is XSD-style
  validation at runtime; the *implementation* is the
  hand-rolled invariant check. The crate's README must lead
  with this clarification so a future contributor does not
  expect a generic XSD library.
- Schema versioning for NAV v3.0 → v3.1+. The allowlist is
  v3.0-only. NAV's v3.0 → v3.x migrations are an operational
  event handled by a release that extends the allowlist; the
  crate must surface the assumed version loudly (a public
  `pub const NAV_XSD_VERSION: &str = "3.0";`).

## Consequences

**What gets easier**

- The `Ready` typestate in ADR-0009 §2 is no longer aspirational
  — there is now a validator gate that the typestate corresponds
  to in code.
- `poll_ack`'s response-parse arm can flip to `NonRetryable`
  cleanly without inventing a "wait for the validator" comment;
  the comment trail closes.
- The "we sent malformed XML and NAV rejected it 10 seconds
  later" operator-visible failure mode is replaced with "we
  refused to submit because the on-disk XML diverges from v3.0
  expected shape at line N" — which is actionable.
- Future PRs that emit new NAV elements have a single place
  (`nav-xsd-validator/src/lib.rs`) to extend; the test that
  catches drift is in one place too.

**What gets harder**

- The hand-rolled allowlist is hand-maintained. NAV's v3.0 → v3.x
  migrations require an explicit edit; there is no XSD-driven
  codegen. The trade is small for one counterparty (NAV) and
  large in correctness clarity — same trade as ADR-0021 §A8 made
  for the SOAP envelope.
- New optional NAV elements will require an emitter-and-validator
  pair landed in the same commit (F12-style four-edits trap
  pattern, but only two edits now: emitter side + validator side).

**What we lock ourselves into**

- The pure-Rust posture for XML handling end-to-end. A future
  ADR that wants to switch to libxml2 must supersede this one
  AND ADR-0021 §A8 jointly.
- The "structural invariant check is sufficient for v3.0"
  position. If NAV's v3.x or v4 introduces XSD constructs the
  hand-rolled walker cannot model (`xs:choice` with non-trivial
  cardinality, deep type inheritance, etc.), this ADR is
  superseded by an Option-A or Option-C ADR — the supersede path
  is documented.

## Adversarial review

Build-phase. Bar is ≥3 concerns answered or accepted.

- *"You picked the option that is not literally XSD validation. A
  NAV inspector will not accept 'we did our own structural check'
  as evidence."* — Acknowledged. The audit-evidence bundle per
  ADR-0009 §8 does not claim XSD-validated XML; it claims "the
  XML that went on the wire" plus "the NAV response." The
  validator is a *defensive gate* against shipping malformed XML
  to NAV; the legal evidence is NAV's own SAVED ack. Accepted.

- *"libxml2 has decades of correctness investment. Hand-rolling
  rejects that for ergonomic reasons."* — Rejected as framing.
  The constraint is single-static-binary (ADR-0001) +
  reproducible build (ADR-0007 §Supply chain). libxml2 forces a
  C dep and an `unsafe` FFI surface that ADR-0001 §Consequences
  explicitly disclaims. The decision is constraint-driven, not
  ergonomic. Accepted.

- *"`xmlschema-rs` will mature. Why bake in the hand-rolled
  posture instead of waiting?"* — The trigger condition has
  fired (PR-9 needs the gate). Waiting means the gate is absent
  for every PR between now and `xmlschema-rs` maturity. The
  supersede path is documented (this ADR's §"What we lock
  ourselves into") — when `xmlschema-rs` is mature, a successor
  ADR can swap the implementation with a one-call-site change,
  because the crate exposes one typed entry point. Accepted.

- *"You did not enumerate the failure-mode error variants. The
  conformance check (per CLAUDE.md rule 12) needs that
  enumeration."* — Correct, and the implementation has the
  enumeration in `NavXsdValidationError`. The ADR text lists
  variant *classes* (missing-required-child, unexpected-element,
  …) because future emitter PRs will extend the variant set, and
  enumerating today freezes a moving target. The crate's
  unit tests pin the variants pair-wise (each variant's
  `Display` is asserted distinct from every other variant's
  `Display`) so a future merge that accidentally collapses two
  variants fails loud. Accepted with crate-side enforcement.

## Alternatives considered

Already enumerated in §Context (options A / B / C). The decision
above is Option B.

## Open questions

- **Should the validator additionally hash-check vendored XSD
  files at startup per ADR-0009 §1?** Not in this PR. The
  hand-rolled allowlist supersedes the need for a vendored XSD
  file in PR-9-0; if a future contributor wants defense-in-depth
  against the allowlist drifting from NAV's actual XSD, that is
  a separate PR.
- **Should ABERP add a CI check that diffs the validator's
  allowlist against a downloaded NAV v3.0 XSD?** Out of scope —
  NAV does not publish the XSD at a stable URL with a stable
  versioning policy, and a daily CI fetch is an external-trust-
  surface ADR (ADR-0007 §Supply chain) which is a much larger
  decision than this PR carries.
