# Commit #1 success criterion

Per project memory: *"the smallest thing that exercises ADR-0008 (audit
ledger), ADR-0009 (NAV invoice issuing), ADR-0019 (storage strategy)
end-to-end — likely a binary that generates a NAV-compatible invoice
XML on disk without submitting."*

The `aberp issue-invoice` subcommand is that binary.

## How to demonstrate

```
cargo build --release
./target/release/aberp issue-invoice \
    --in   fixtures/invoice_minimal.json \
    --out  /tmp/invoice.xml \
    --db   /tmp/aberp.duckdb
```

Expected stdout:

```
issued invoice INV-default/00001 -> /tmp/invoice.xml (audit chain verified across 2 entries)
```

Expected outputs:

- `/tmp/invoice.xml` — NAV v3.0 `InvoiceData` XML, structurally
  inspectable against the public XSDs.
- `/tmp/aberp.duckdb` — populated with the billing tables
  (`invoice_series`, `invoice_sequence_state`,
  `invoice_sequence_reservation`, `invoice`, `invoice_line`) and the
  `audit_ledger` table.

## What this demonstrates

- **ADR-0019**: relational source-of-truth in DuckDB, no foreign keys,
  per-module storage adapter.
- **ADR-0009 §3**: atomic sequence allocator, gap-free; PR-4's
  conformance tests already prove the invariant under three retry
  and void scenarios.
- **ADR-0008**: tamper-evident audit ledger with a hash chain — PR-3's
  conformance tests prove `Ledger::verify_chain` rejects payload and
  prev_hash mutations; this binary exercises the real write path under
  invoice-issuance kinds and verifies the chain before exit.
- **ADR-0021**: the pinned crate baseline compiles cleanly under stable
  Rust on Apple Silicon; CI runs the same set under Ubuntu.

## What this does NOT demonstrate

- **XSD validation** — the XML is structurally NAV-compatible by
  inspection, but PR-5 does not run an XSD validator. That crate
  choice is deferred per ADR-0021 §Items deferred ("XSD runtime
  validation crate", trigger: first PR implementing schema-drift
  detection per ADR-0009 §1).
- **NAV submission** — the binary writes the XML to disk; submission
  belongs to the NAV adapter PR (out-of-scope per the handoff:
  "without submitting").
- **Cross-crate transactional audit** — the audit-ledger writes happen
  in a separate DuckDB transaction from the billing writes. ADR-0008
  §Storage requires them to be in the same transaction; the binary's
  pipeline does not yet satisfy that. Tracked for the next adversarial
  review; the mitigation in the meantime is that a crash between the
  billing commit and the audit append leaves an invoice without its
  audit entries, which the reconciliation scan would surface as an
  anomaly (loud-failure mode per ADR-0007).
- **Real authentication** — the audit-ledger entries use
  `Actor::test_only`. The keychain-bound credential flow ships with
  the keychain ADR (named trigger: first PR that loads keychain-bound
  material).

## Re-running

The binary is idempotent under the same `IdempotencyKey` ULID (per
ADR-0009 §5 Layer 1). Each invocation of the CLI generates a fresh
`IdempotencyKey`, so re-running this command produces a new invoice
(sequence number 2, 3, ...). To exercise the replay path, the
billing-module integration test
(`modules/billing/tests/sequence_allocator.rs::idempotent_retry`) is
the canonical reference.
