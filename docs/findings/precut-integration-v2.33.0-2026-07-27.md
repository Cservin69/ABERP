# Final pre-cut integration adversarial — `PROD_v2.32.1..origin/main`

**Date** 2026-07-27 · **Tree** `origin/main` @ `c966481` · **Target cut** `PROD_v2.33.0`

Scope: the *interactions* between the changes in this delta and integration
defects on the merged tree — deliberately NOT a re-litigation of each piece,
every one of which had its own adversarial. Full gates ENFORCE on.

## Verdict

**GO**, with the residuals below. NAV/ÁFA filing correctness — the crown jewel
— holds under every attack tried. Two defects found; the operator-copy one
(I2) is fixed on this branch, the HUF one (I1) is characterised, pinned around,
and deferred with a written fix.

## Findings

### I1 — `huf_equivalent_total` (books) diverges from `invoiceGrossAmountHUF` (wire) — CONFIRMED, DEFERRED

ADR-0037 §1.c: *"Invoice-level total HUF amount. Computed as the sum of the
per-VAT-rate HUF amounts, NOT by converting the EUR invoice total directly."*

* `nav_xml::write_summary` post-B3′ **complies**: `invoiceGrossAmountHUF` is
  `Σ round_half_even(bucket_gross × rate)`.
* `issue_invoice::finalize_rate` — which produces `RateMetadata.huf_equivalent_total`,
  the value persisted on the `invoice` row, printed on the PDF and shown in the
  SPA — does **exactly the forbidden thing**: one `round_half_even` of the
  invoice's gross cents. So does the chain-child recompute in
  `invoice_currency_metadata::inherit_rate_metadata_for_chain` (called from
  `issue_storno` and `issue_modification`).

Neither piece is wrong alone. Before B3′ there was always exactly one bucket, so
"sum of the per-rate amounts" and "direct conversion of the total" were the same
number and the two sides agreed by construction. B3′ made buckets plural and
split them apart.

**Reproduced on the merged tree** (EUR @ 356.690000, buyer `Other`, lines
`1 × 788 @27%` + `1 × 962 @5%`): bucket grosses 1000 and 1010 cents convert to
3567 and 3603 HUF → wire `invoiceGrossAmountHUF = 7170`, while `finalize_rate`
yields `huf_equivalent_total = 7169`. A 1 HUF disagreement between the printed
invoice and the NAV data report; up to ±(N−1) forints at N buckets.

**Why this is not a cut blocker.** The wire — what NAV receives and what
determines the ÁFA — is the §1.c-*correct* side; B3′ moved it into compliance.
The defect is on the books-side display field, requires a non-HUF invoice with
≥2 distinct `(kind, rate)` buckets, and no such invoice can exist correctly in
prod today: pre-0103 the emitter put every line's money in one bucket under the
first line's rate, so any multi-rate invoice already issued is a known-bad
record, and none is reported.

**Fix (its own PR, immediately post-cut).** Extract the `(kind, basis_points)`
grouping out of `write_summary` into one `pub(crate)` helper; have
`finalize_rate` compute `huf_equivalent_total` through it; change
`inherit_rate_metadata_for_chain` to take the child's `&[LineItem]` + a negate
flag instead of `child_gross_cents: i64` (both call sites already have the
lines). Round-half-even is symmetric, so the storno negation is unaffected.
Deliberately not done here: it rewrites `finalize_rate` and both chain-
inheritance call sites, which is more risk than the ≤2 HUF it removes, hours
before a prod cut.

### I2 — operator copy still states the retracted single-bucket justification — FIXED

Commit `2b8ba38` (B3′) re-founded the `MixedVatRateKindsUnsupported` preflight
guard: its variant doc (`issue_preflight.rs:176-190`) and the guard body's
comment (`:1012-1042`) now say the guard survives on the *buyer-status*
precondition, and explicitly retract the old "protect the single-bucket
summary" reason. The two **operator-visible message bodies** were not updated
and still asserted, verbatim to the operator in both languages:

* HU: `a NAV összesítő egyetlen kategóriát tartalmaz`
* EN: `NAV's summaryByVatRate is single-bucket, so a mixed-kind invoice mis-categorises the summary`

Both are false — `SummaryNormalType/summaryByVatRate` is `maxOccurs="unbounded"`,
which this same delta proved and built for. The SPA's
`issue-invoice.ts` type comment carried the same claim. All three replaced with
the re-founded reason; the operator's remedy ("split onto separate invoices") is
unchanged and still correct.

### I3 — `reports.rs` VAT is kind-blind, now disagreeing with the filing — RESIDUAL

`query_outgoing_groups` groups by `il.vat_rate_basis_points` alone and
recomputes VAT as `net × bp / 10_000`. It never reads `vat_rate_kind`. After B2
(`LineItem::vat_amount` returns zero for every non-`Percent` kind), the report
and the NAV filing disagree for any line with a non-`Percent` kind AND a
non-zero rate. Pre-B2 both were wrong together; now the filing is right and the
report is wrong.

Reachable only through a preflight-bypassing door (preflight rejects that
combination with `NonZeroPercentForExemptKind`), and management-reporting only —
nothing on the NAV wire. Belongs with the parked `vat-0pct-subtypes-reporting`
work, which owns the same kind-blindness at 0% (where `Percent@0`, `AamExempt`,
`DomesticReverseCharge` and `IntraCommunityGoods` all collapse into one report
bucket while NAV correctly receives four).

### I4 — ADR-0106 R7 remains open — CARRIED, unchanged by this delta

`POST /invoices/:id/submit`, the CLI `submit-*`/`retry-submission` verbs and the
two drain daemons reach NAV *filing* without appearing in the door registry and
without running `validate_invoice_preflight`. Already written down in
`tools/adr0106_nav_door_registry.txt:44-61` and the gate-teeth audit.

Confirmed harmless for ÁFA correctness in this delta: `submit_from_inputs` does
`std::fs::read` of the already-rendered on-disk XML and re-runs
`validate_invoice_data` on it before transmitting — it never re-renders, so the
B2/B3′/B4 corrections are on the wire regardless. R7 is a gate-*scope* gap, not
a body-correctness gap.

## Attacks tried that did NOT land

| Attack | Result |
|---|---|
| Mixed-rate + mixed-**kind** + community buyer in one body (B2 × B3′ × B4 × ADR-0101) | Unreachable — preflight's `MixedVatRateKindsUnsupported` forces every line onto one kind whenever any line is non-`Percent`. The emitter handles it correctly anyway (defence in depth). |
| Multi-bucket body rejected by the authoritative XSD shape | Holds. `summaryByVatRate` is `maxOccurs="unbounded"`; the local validator was corrected in lock-step and still rejects a bucket placed *after* the invoice-level amounts. |
| Community VAT number reaching the wire un-normalised (B4) | Holds. All three issuance entry points normalise in place before the emit/audit snapshots read `input`; a storno/modification replay re-enters through the same entry points. |
| A second `summaryByVatRate` consumer left on the single-bucket shape | None exists — only the emitter and the validator parse buckets. |
| Per-line vs summary `vat_amount` disagreement after B2 | Holds. `LineItem::vat_amount()` is the single derivation; the summary sums the same call. No duplicate VAT arithmetic anywhere on the invoice path (`purchasing::vat_minor` is a separate subsystem). |
| `EventKind` count drift from two PRs each adding kinds | Holds. Only `BomRevisionCreated` was added; 187→188 and both `const _` drift pins (`aberp-verify`, `export_invoice_bundle`) are at 188. |
| As-built drift: WO release → BOM edit → re-release under a stale pin | Unreachable. `Release` is legal only from `Created`, so a WO releases exactly once; the pin and the `BomConsumption` movements come from the same read of the active BOM; a torn active set spanning two revisions is a hard refusal, not an arbitrary pin. |
| ADR-0104 price ingestion touching a NAV/invoice path | Cannot — `ingest_price_list` still has test callers only (the operator trigger is the ADR's named deferral). `record_price_set`/`resolve_price_set` are quoting-only. |
| `db_path_guard` false-refusing a legitimate prod boot | Does not. `~/.aberp` and `~/.aberp/prod` are real directories, the path carries no `..`, nothing is unresolved, and the first component under the root equals the tenant → allowed. Fresh-install (DB file absent) also passes: `symlink_metadata` on a genuinely missing file errors, so the dangling-symlink arm does not fire. |
| This delta adding an uncensused DB opener or an unregistered NAV door | Neither. Opener census frozen at 86/21 and green; the ADR-0106 door gate green; the three routes added by ADR-0105 are GET reads. |

## Gate state on the merged tree

`cargo fmt` ✓ · `cargo build --workspace --locked --all-targets` ✓ ·
`cargo test --workspace --locked` ✓ (2971 passed, 0 failed) ·
`cargo clippy --workspace --all-targets --locked -- -D warnings` ✓

All six enforcing cut gates ✓ (opener census, write-fork, read-fork,
keychain seam, edition ratchet, ADR-0106 NAV-emission door), and all eight
negative-probe / backstop suites ✓ — 13 + 22 + 16 + … probes, zero escaped.
