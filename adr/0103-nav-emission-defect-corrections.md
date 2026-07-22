# ADR-0103 — Correcting five confirmed NAV-emission defects: summary bucketing, VAT derivation, preflight universality, and validate/emit identity

- **Status:** **Proposed (design-only, 2026-07-20).** No application code written in this session. This ADR is the design a later implementation session executes against, in the sequence of §7.
- **Date:** 2026-07-20
- **Deciders:** Ervin Áben (identified and confirmed all five defects; set the two-scope framing of §1). Design-pass by Dispatch.
- **Base:** all five defects grep-confirmed present on `main` @ `76e9dad`. Every file:line in §2 was read in this session, not inferred.
- **Related:** **ADR-0101** (per-line `vat_rate_kind` — §3.4 of which *specified* the summary mirror this ADR finds unbuilt; see §9), **ADR-0102** (EU-partner `Other` / `communityVatNumber` — B4 lives on the path it opened, and its §4 cross-field matrix is load-bearing for §3.1's guard decision), ADR-0049 (storno/modification replay the side-stored `input.json` — the shape B1 must not break), ADR-0048 (customer VAT status closed vocab), ADR-0042 (notes never on the NAV wire — a firewall none of these fixes may breach), ADR-0022 (`nav-xsd-validator`). NAV wire-shape gotchas: `reference-nav-gotchas` memory §1–§4.
- **⛔ BLOCKED ON §9** — a parallel archaeology session must complete before implementation begins. See §9 for why this is a hard gate and not a nicety.

---

## 0. TL;DR

Five defects, all live on `main`, all on the invoice→NAV/ÁFA path — CLAUDE.md rule 4's named "genuinely consequential checkpoint". Ranked by exposure:

| # | Defect | Reachable from | Wrongness class |
|---|---|---|---|
| **B3′** | `summaryByVatRate` emits **one** bucket, rate+kind from `lines.first()`, net/vat/gross summed over **all** lines | the ordinary SPA **Issue** button | **silent wrong ÁFA** — worst class (rule 11) |
| **B3** | same root cause, mixed-*kind* arm; also on the storno path | storno | silent wrong ÁFA |
| **B2** | `vat_amount()` is rate-driven and never reads `vat_rate_kind` | any ungated door (B1) | `<vatExemption>` **and** a non-zero `<lineVatAmount>` on one line |
| **B1** | `validate_invoice_preflight` has **one** non-test call site — 1 of 8 invoice-originating doors | 7 ungated doors; CLI storno can *originate* a wrong filing | unbounded |
| **B4** | community VAT number is **normalised for validation, emitted raw** | `Other` (EU) buyers | what passed the gate is not what was transmitted |

The corrections are **not five patches**. They are four invariants (§3) plus one structural relocation (§3.4). Each is stated as a property that cannot be violated, because — per the house rule — safety belongs in code, not operator discipline. Where Ervin's starting position and this design differ, §8 says so explicitly rather than quietly adopting one.

**Headline disagreement (detail in §3.1 / §8.1):** grouping `summaryByVatRate` by `(kind, rate)` does **not** retire `MixedVatRateKindsUnsupported`. It retires the guard's *stated* reason and leaves a different, stronger one standing. The guard must be **re-founded**, not deleted.

---

## 1. Scope framing (Ervin's explicit direction — recorded here so the next session does not rediscover it)

This ADR uses a **two-scope** model. It is a scoping decision, not a technical one, and it governs what "done" means in §6.

- **Scope A — the operating business (Ervin's own company).** The pair `run/run_prod.sh` (live) **+** `run/run_desktop.sh` (dev), **verified together, in that pairing**. Live is where real ÁFA is filed; dev is the surface Ervin actually drives. A fix is not Scope-A-done until both have been exercised. This is the scope that carries the regulatory exposure and therefore sets the bar.
- **Scope B — Defense (`ABERP-Editions.git`).** **First-class**, not an afterthought: it is the future CNC-company product. Every invariant in §3 applies to it identically, and it gets its own sign-off line in §6. It is a *separate repository* — the port is a deliberate act, not an inheritance, and must be tracked as such.
- **Portable — PARKED.** It compiles from the same source, so it **inherits every fix in this ADR for free**. It gets **no test matrix and no sign-off gate.** This is intentional: adding a third verification surface for a build that cannot diverge from the source is verification theatre. If Portable ever forks its emit path, this parking decision expires and must be revisited in a new ADR.

> Do not re-litigate this in the implementation session. If the scoping is wrong, change it here first.

---

## 2. Current state (read in this session at `76e9dad` — every line verified, none inferred)

| # | Site | What is there |
|---|---|---|
| B3′/B3 | `apps/aberp/src/nav_xml.rs:1896` (`write_summary`) | `if let Some(first) = lines.first()` → **one** `summaryByVatRate`; `write_vat_rate(w, first.vat_rate_kind, first.vat_rate_basis_points)` — bucket key from line 0 — wrapped around `net_total`/`vat_total`/`gross_total` accumulated over **every** line in the loop at `:1869-1879`. The in-file comment at `:1885-1889` states the collapse as a known posture and defers it to "a future PR". |
| B3 | `apps/aberp/src/issue_storno.rs:1486` (`build_storno_command`) | carries `vat_rate_kind` through faithfully — the storno body is correct; it is `write_summary` downstream that collapses it. Same root cause, second reachable path. |
| B3′ non-catch | `apps/aberp/src/issue_preflight.rs:1019-1031` | `let any_non_percent = request.lines.iter().any(|l| !l.vat_rate_kind.is_percent()); if any_non_percent { … }`. **All-`Percent` invoices short-circuit out of the guard entirely.** The comment at `:1017-1018` names the gap: *"The pre-existing mixed-`Percent`-RATE single-bucket gap is out of ADR-0101 scope and intentionally NOT gated here."* |
| B2 | `modules/billing/src/domain/invoice.rs:78-83` (`vat_amount`) | `let vat = net.checked_mul(self.vat_rate_basis_points as i64)?; Some(Huf(vat / 10_000))`. `self.vat_rate_kind` is **never read**. `gross_total()` (`:86-88`) composes on top of it. |
| B1 | `apps/aberp/src/serve.rs:7381` | the **only** non-test call site of `validate_invoice_preflight`. |
| B1 | `apps/aberp/src/serve.rs:9355` (modification), `:8975` (storno), `apps/aberp/src/main.rs:29/36/37` (4 CLI paths) | **ungated.** `serve.rs:9415` says so in-line: *"this route never calls `validate_invoice_preflight` anyway, so nothing downstream could catch it."* |
| B1 | `serve.rs` modification substitute guard | reads the **base** invoice's persisted `invoice_line.vat_rate_kind`. Never inspects `request.lines`. |
| B1 | `apps/aberp/src/cli.rs:946-952` (`IssueStornoArgs.r#in`) | *"Path to the input JSON file describing the storno's own line content."* Arbitrary operator JSON, **never cross-checked against the base invoice.** This is not a bypass of a check — it is an **origination** channel for a filing that has no correct base to be wrong about. |
| B4 | `apps/aberp/src/nav_xml.rs:337-342` | `validate_community_vat_number` normalises (`filter(!is_whitespace)`, `to_uppercase`) into a local `normalized`, validates **that**, and **discards it** — returns `Result<(), String>`. |
| B4 | `apps/aberp/src/nav_xml.rs:1329` | `text_element(w, "communityVatNumber", community_vat_number)?` — the **raw** `c.community_vat_number`. |
| B4 counter-example | `apps/aberp/src/nav_xml.rs:395` (`validate_country_code`) | *"VERBATIM-strict (no trim, no case-fold): `write_customer_address` emits `country_code` byte-for-byte, so anything the wire would reject … must reject here too, or the guard would pass a value NAV still bounces."* **The correct lens, written out in full, fifty lines above the defect.** |

**Structural fact that decides §3.4:** all three shared library entry points already take the *same* body type.

| Entry point | Signature (verified) |
|---|---|
| `issue_invoice::issue_from_parsed` (`:532`) | `(input: InvoiceInputJson, db, tenant, series, currency: Currency, …)` |
| `issue_storno::storno_from_inputs` (`:236`) | `(input: InvoiceInputJson, db, tenant, series, references, …)` |
| `issue_modification::modification_from_inputs` (`:208`) | `(input: InvoiceInputJson, db, tenant, series, references, modification_date, …)` |

`validate_invoice_preflight` (`issue_preflight.rs:814`) takes `&IssueInvoiceRequest` — an **HTTP DTO** (`serve.rs:6875`) that carries `currency` but **no `supplier`**. `InvoiceInputJson` (`issue_invoice.rs:136`) carries `supplier` but **no `currency`**. That mismatch is the entire reason the preflight is stuck on the HTTP handler. §3.4 resolves it.

---

## 3. The corrections, as invariants

Each subsection states the property first. The implementation is whatever satisfies the property; the property is what the regression test pins.

### 3.1 — B3 + B3′ · Summary/line coverage

> **INVARIANT S — Summary coverage.** *For every emitted invoice, the multiset of `(vat_rate_kind, vat_rate_basis_points)` over `summaryByVatRate` buckets equals the distinct set over the lines; and for each bucket, `vatRateNetAmount` / `vatRateVatAmount` / `vatRateGrossAmount` are the sums over **exactly the lines in that group** — no line contributes to a bucket it is not in, and no line contributes to zero buckets.*

Stated that way, "one bucket keyed on line 0" is not a shortfall of the invariant; it is a violation of it, and a test that asserts the invariant cannot pass while the current code stands.

**Design.** `write_summary` groups lines by the composite key `(vat_rate_kind, vat_rate_basis_points)`, accumulates the three totals per group, and emits one `summaryByVatRate` per group. The invoice-level `invoiceNetAmount`/`invoiceVatAmount`/`invoiceGrossAmount` become the **sum over buckets** — which is what the `nav_xml.rs:1885-1889` comment already anticipated would be needed ("the per-rate HUF amounts here need to be summed").

Two things this must get right that the naive form does not:

1. **Deterministic bucket order.** Emission order must be a stable sort on `(kind ordinal, basis_points)`, not `HashMap` iteration order. Non-deterministic order makes every golden-bytes test flake and makes two renders of one invoice differ — unacceptable when the on-disk XML is the canonical record of what NAV saw (`reference-nav-gotchas §3`).
2. **HUF conversion is per-bucket, not post-hoc.** `huf_equivalent_for` must run on each bucket's native total, and the invoice-level HUF figures must be the sum of the per-bucket HUF figures — not a fresh conversion of the native grand total. This is ADR-0037 §1.c, restated by the existing comment. Converting the grand total instead would reintroduce a rounding discrepancy the moment there is more than one bucket.

**Single-bucket back-compat:** for a single-rate, single-kind invoice — every invoice ever issued to date — the grouping yields exactly one group and the emitted bytes are **identical**. This is the regression pin of §5.

**Does `MixedVatRateKindsUnsupported` retire? NO — it must stay, re-founded.** (Ervin's position was that grouping "removes the reason it exists"; this design disagrees. See §8.1.)

The guard's *stated* reason — the single-bucket summary — does dissolve. But a second, independent and stronger reason stands: **ADR-0102 §4(a)'s buyer-status ↔ line-kind matrix is invoice-scoped.** `IntraCommunityGoods` requires `customerVatStatus = Other`; `DomesticReverseCharge` requires `Domestic`. An invoice carrying one line of each demands both simultaneously — **unsatisfiable**. Post-grouping, such an invoice would produce two structurally well-formed buckets and pass the local validator while being semantically impossible. Deleting the guard would open exactly that door.

So the guard is **kept and its justification rewritten** — from *"the summary is single-bucket"* to *"ADR-0102 §4(a) admits at most one operative non-`Percent` kind per invoice, because customer VAT status is an invoice-level field."* The behaviour is unchanged; the comment block at `issue_preflight.rs:1010-1018` is replaced wholesale, **including the sentence that names the B3′ gap as intentionally-not-gated** — that sentence becomes false the moment Invariant S lands, and a stale comment asserting a live defect is acceptable is precisely how B3′ survived (§9).

This is **not** defence-in-depth. It is a different invariant that happened to be enforced by the same code.

### 3.2 — B2 · VAT derivation

> **INVARIANT V — Kind-consistent VAT.** *A line whose `vat_rate_kind` is not `Percent` has `vat_amount() == 0`, unconditionally, for every value of `vat_rate_basis_points`. There exists no caller — present or future, in-repo or out — that can obtain a non-zero VAT amount for such a line.*

**Design.** `LineItem::vat_amount()` matches on `self.vat_rate_kind` and returns `Huf::ZERO` for every non-`Percent` arm; only the `Percent` arm reaches the existing `net × basis_points / 10_000` computation. `gross_total()` needs no change — it composes, so `gross == net` falls out for exempt/reverse-charge lines automatically, which is correct.

**Why this is the right altitude.** Preflight already carries ADR-0101 §4's `NonZeroPercentForExemptKind`. But preflight is a *gate*, and B1 is the proof that gates get bypassed — 7 of 8 doors do not run it. `vat_amount()` is the *derivation*, and every emit path in the repo goes through it. Moving the truth from the gate to the derivation is the difference between "no one has yet found a way in" and "there is no way in".

**Back-compat is exact, and this is checkable rather than hoped-for:** a correctly-issued non-`Percent` line already has `basis_points == 0` (forced by ADR-0101 §4), so `vat_amount()` already returns `0` for it. The fix therefore changes the emitted bytes of **zero** correctly-issued invoices. It changes bytes only on the path where preflight was bypassed — i.e. exactly the B1 doors. B2 and B1 are two views of one failure.

**What this deliberately does *not* do.** A stricter form would make `vat_rate_basis_points` structurally unreachable for non-`Percent` kinds (an enum carrying the rate only in its `Percent` variant). That is the *correct* model and it is **rejected for this ADR**: it is a type-level refactor across the billing domain, the DuckDB adapter, the side-stored `input.json` shape (ADR-0049 chain-load-bearing) and the SPA wire, for a defect that the accessor fix closes completely. CLAUDE.md rule 3. Flagged in §10.2 as the honest residual, not silently dropped.

### 3.3 — B4 · Validate/emit identity

> **INVARIANT I — One value.** *For every field that is both validated and transmitted, the bytes validated, the bytes persisted to the side-store, the bytes stamped on the audit payload, and the bytes written to the NAV XML are **the same bytes**. There is no transformation between the gate and the wire.*

This is `validate_country_code`'s reasoning (`nav_xml.rs:395`), generalised. That function achieved Invariant I by making the *validator* verbatim-strict. `validate_community_vat_number` broke it by making the validator lenient without making the emitter agree.

**Design — and here this ADR corrects Ervin's point 4.** The instruction was "emit the normalised community VAT". Emitting the normalised form **at emit time** would satisfy validate-vs-wire and *violate* Invariant I on the other two legs: ADR-0102 §3.2 snapshots the community VAT number onto **three** artifacts — the side-stored wire body, the on-disk NAV XML, and the audit payload. Normalising inside `write_customer` leaves the side-store and the audit payload holding the raw string while NAV received the normalised one. That is a smaller version of the same defect, moved.

The fix is therefore **normalise once, at ingest**:

1. `validate_community_vat_number` changes its signature to **return the normalised `String`** (`Result<String, String>`) instead of discarding it — the normalisation already exists at `:338-342` and is simply thrown away today.
2. The **preflight / partner-form gate** writes the returned normalised value back into `CustomerJson.community_vat_number` before the body is persisted or rendered.
3. `write_customer` (`:1329`) is **unchanged** — it keeps emitting the stored field verbatim, which is now the normalised value by construction.

Result: one normalisation, at the boundary, and validator / side-store / audit / wire all read one field. The emitter stays dumb, which is where dumbness belongs.

**Why normalise rather than go verbatim-strict like the country code?** Because the two fields differ in operator reality. VIES numbers are *published and pasted with spaces* (`ATU 123 45678`), so verbatim-strict rejection would be hostile to a correct input. A lowercase country code is a typo. Different fields, different lenience — but **the same invariant**, satisfied at different points. Both are now consistent with Invariant I; today only one is.

### 3.4 — B1 · Preflight universality

> **INVARIANT P — Universal gate.** *No `InvoiceInputJson` reaches NAV-XML rendering or a DB write without having passed `validate_invoice_preflight`. The property holds by construction — because the gate sits on the only shared path to those effects — not by every call site remembering to call it.*

**The relocation — and here this ADR refines Ervin's point 3.** The instruction was to lift preflight "above the shared library entry points", and to address that the storno and modification request shapes are not `IssueInvoiceRequest`.

They do not need to be. **The common shape already exists.** As §2 verified, all three entry points take **`InvoiceInputJson`**. The obstruction is not the request shapes — it is that the preflight is written against `IssueInvoiceRequest`, an HTTP DTO that has no business being the vocabulary of a library-level invariant.

**Design:** re-target `validate_invoice_preflight` to `(input: &InvoiceInputJson, currency: Currency) -> Vec<InvoicePreflightError>` and call it as the **first statement** of each of the three entry points, immediately after the existing `input.lines.is_empty()` check. The HTTP handler at `serve.rs:7381` **converts, then calls** rather than calling then converting — it loses its own preflight call, because it now inherits it.

Consequences, each deliberate:

- **1-of-8 → 8-of-8 by construction.** The four CLI doors, both serve chain routes, and the SPA route all pass through one of the three entry points. There is no fourth way to render invoice XML.
- **`supplier` is now in scope.** `InvoiceInputJson` carries `supplier`; `IssueInvoiceRequest` did not. The existing `nav_xml::validate_supplier_info` — presently called *twice*, from `issue_from_parsed` **and** from `handle_issue_invoice` (`nav_xml.rs:407-411`) — **folds into the same chokepoint and the duplicate call is deleted.** A net deletion (CLAUDE.md rule 12); take it.
- **`currency` stays a parameter.** It is not on `InvoiceInputJson` and must not be added there — that would change the side-stored `input.json` shape, which ADR-0049 makes chain-load-bearing. Pass it alongside.
- **Sign safety on the storno path — checked, not assumed.** A preflight that rejects non-positive amounts would false-reject a storno if storno bodies were negative at the gate. They are not: `build_storno_command` (`issue_storno.rs:1479-1490`) builds lines with `Huf(l.unit_price)` **positive** from the base's `input.json`, and negation happens downstream at `nav_xml::render_storno_data` (per the module doc at `issue_storno.rs:48`). Preflight at the entry point therefore sees positive lines on all three paths. **This is a precondition, so it gets its own pin** (§6) — if a future change moves negation earlier, the pin fails rather than the invoices.
- **Expect fallout, and treat it as signal.** Seven doors have never been gated. Existing CLI fixtures and integration bodies will start failing preflight. Each failure is either (a) a fixture that was always invalid and was never caught — fix the fixture, and record what it was, because it is evidence for §9 — or (b) a preflight rule too strict for a legitimate chain body — fix the rule. **Neither is resolved by weakening the gate for chain paths.** A "chain bodies skip rule X" escape hatch reintroduces B1 in miniature and is forbidden by this ADR.

**B1's second half — base congruence — is a different invariant and needs its own guard.**

Preflight validates a body *in isolation*. It structurally cannot answer "is this modification/storno consistent with the invoice it claims to correct?", because it never sees the base. Two of the confirmed findings are exactly that question:

- the modification substitute guard reads the **base**'s persisted kinds and never `request.lines`;
- **CLI storno accepts arbitrary `--in` JSON never cross-checked against the base** — the finding Ervin correctly called *worse than a bypass*, because it originates rather than merely permits.

These are one defect with two front doors, so they get one guard:

> **INVARIANT C — Chain congruence.** *A storno or modification body is accepted only if its lines are congruent with the base invoice's persisted lines: same line count, and per line the same `vat_rate_kind` and the same `vat_rate_basis_points`. Divergence is a loud typed rejection naming the line index and both values — never a silent substitution, never a default.*

One function, `validate_against_base(input: &InvoiceInputJson, base_lines: &[…]) -> Vec<ChainCongruenceError>`, called from **both** `storno_from_inputs` and `modification_from_inputs` — where it covers the CLI and serve arms of both simultaneously, which is the whole point of putting it at the library boundary rather than on the routes.

Note what Invariant C makes possible as a **consequence, not a goal**: the ADR-0101/S2 modification guard that rejects any non-`Percent` base outright exists because the SPA does not thread `vat_rate_kind` and the route would silently downgrade. Under Invariant C that downgrade becomes a loud `ChainCongruenceError` instead of a silent one. **Whether the S2 guard can then be relaxed is explicitly out of scope here** — it is a feature decision (does the SPA thread the kind?), not a defect correction, and folding it in would be exactly the scope creep CLAUDE.md rule 3 forbids. Recorded in §10.4.

---

## 4. Invariant → defect map

| Invariant | Property | Closes | Enforced at |
|---|---|---|---|
| **S** — Summary coverage | buckets partition the lines exactly | B3, B3′ | `nav_xml::write_summary` (emit) |
| **V** — Kind-consistent VAT | non-`Percent` ⇒ VAT is 0, always | B2 | `LineItem::vat_amount` (derivation) |
| **I** — One value | validated bytes == persisted == audited == transmitted | B4 | ingest boundary (normalise once) |
| **P** — Universal gate | no body reaches render/write ungated | B1a | the 3 library entry points |
| **C** — Chain congruence | chain body matches its base, per line | B1b | `storno_from_inputs` + `modification_from_inputs` |

Note where each lives: S at emit, V at derivation, I at ingest, P and C at the library boundary. **None of them lives on an HTTP handler.** That is the shape of the whole correction — every one of these defects existed because a rule was written at a door instead of in the machinery.

---

## 5. Backward compatibility (HARD)

1. **Single-rate, single-kind invoices emit byte-identically.** Every invoice issued to date is single-bucket; grouping yields one group; bytes unchanged. This is the load-bearing pin (§6-T1).
2. **Invariant V changes no correctly-issued invoice.** Non-`Percent` lines already carry `basis_points == 0` (ADR-0101 §4), so `vat_amount()` already returns 0 for them. Bytes change only where preflight was bypassed.
3. **No retroactive re-emit.** Issued invoices' side-stored `input.json` and on-disk NAV XML are never rewritten (`reference-nav-gotchas §3`). These fixes change what *future* renders produce; historical records stand as the record of what NAV actually saw.
4. **No wire-shape change.** `InvoiceInputJson`, `LineJson`, `CustomerJson` gain no fields and lose none. `validate_community_vat_number`'s signature changes (internal Rust only). ADR-0049's chain replay is untouched.
5. **Invariant I is bytes-neutral for already-clean input.** A community VAT number already stored uppercase-and-spaceless normalises to itself.

---

## 6. Proof — the test plan (every regression pin MUST be mutation-verified)

Standing rule, restated because it is the point: **reverting the fix must make the test fail.** A test that passes against both the fixed and the broken code proves nothing and is worse than no test, because it reports coverage that does not exist. Each pin below names its mutation. The implementation session records the observed failure for each — a pin whose mutation was not run is not landed.

| Pin | Test | Mutation that MUST turn it red |
|---|---|---|
| **T1** | **⭐ Two different VAT percentages on one invoice — 27% + 5%, all-`Percent`.** Assert exactly **two** `summaryByVatRate` buckets; assert each bucket's net/vat/gross equals its own line's, not the sum; assert invoice-level totals equal the sum over buckets. **This case has ZERO coverage today** — it is the single most important test in this ADR, because it is B3′ (SPA-reachable, live since go-live) and nothing in the suite would notice it. | revert `write_summary` to `lines.first()` |
| **T2** | Mixed *kind* — one `Percent` 27% line + one `AamExempt` line: two buckets, correct choice element per bucket, exempt bucket's `vatRateVatAmount == 0`. | same revert |
| **T3** | Storno of a multi-rate base emits the multi-bucket summary with negated per-bucket amounts (B3's second path). | same revert |
| **T4** | Deterministic order: render the same multi-rate invoice N times, assert byte-identical output. | replace the stable sort with `HashMap` iteration |
| **T5** | **Golden single-bucket:** an existing single-rate invoice emits byte-for-byte the pre-0103 bytes. | any change to the single-group path — this is the back-compat pin (§5.1) |
| **T6** | `vat_amount()` returns `0` for a non-`Percent` line **with a deliberately non-zero `vat_rate_basis_points`** — i.e. the state preflight forbids, which is exactly the state B1's ungated doors admit. | revert `vat_amount` to rate-only |
| **T7** | Emit pin: a line carrying `vatExemption` never emits a non-zero `lineVatAmount`, and its summary bucket's VAT is 0. | same revert |
| **T8** | Round-trip identity: a community VAT number entered as `"at u123 45678"` is validated, persisted, audited **and** emitted as one identical normalised string. Assert equality across **all four** artifacts, not just gate-vs-wire — the four-way assertion is what makes T8 a test of Invariant I rather than of a patch. | revert to discarding the normalised value |
| **T9** | Preflight universality, **one test per door, all 8**: an invalid body is rejected via each of the 4 CLI paths, the storno route, the modification route, the SPA route, and direct library invocation. | remove the preflight call from any one entry point — that door's test, and only that door's, goes red |
| **T10** | **Structural** universality pin: a source-level assertion that each of the three entry points calls `validate_invoice_preflight` before any render/write — sibling to the existing `tests/mnb_no_nested_runtime.rs` source-assertion pattern (`issue_invoice.rs` is already read as `ISSUE_INVOICE_SRC` there, so the harness exists). This catches a **fourth** entry point added later, which T9 by construction cannot. | add a 4th entry point without the call |
| **T11** | Storno sign precondition (§3.4): assert `build_storno_command` produces **positive** unit prices and that negation occurs at `render_storno_data`. | move negation upstream into `build_storno_command` |
| **T12** | Chain congruence: a CLI storno whose `--in` diverges from the base — wrong line count; right count, wrong `vat_rate_kind`; right kind, wrong `basis_points` — is rejected loudly, naming line index and both values, on each of the 3 divergences. | remove the `validate_against_base` call |
| **T13** | Mixed-kind guard still rejects `KBAET + DRC` on one invoice **after** grouping lands — the §3.1 re-founding is behaviourally verified, not just re-commented. | delete the guard |

**Scope gates (§1).** Scope A: the pinned suite green **plus** an exercised `run_prod.sh` **and** `run_desktop.sh`, verified as a pair. Scope B: the same invariants ported to `ABERP-Editions.git` with their own green run and its own sign-off line. Portable: **no gate** — parked, inherits by compilation. Plus the standing per-step gates (CLAUDE.md rule 4): `cargo fmt` + build + test + `clippy -D warnings` + coherence/regression pins, every step.

---

## 7. Ordering and dependency

Two genuine dependencies; the rest is risk sequencing.

```
B4  ──(independent)──────────────────────────────┐
B2  ──(must precede B3)──▶  B3/B3′  ──▶  B1a  ──▶  B1b
```

| Step | Fix | Depends on | Why here |
|---|---|---|---|
| **1** | **B4** (Invariant I) | nothing | Smallest, fully isolated to the `Other`-buyer path. Cheap gate-green step to open on. |
| **2** | **B2** (Invariant V) | nothing | **Hard prerequisite for step 3:** B3′'s grouping sums `vat_amount()` per bucket. With B2 unfixed, a non-`Percent` bucket emits a non-zero `vatRateVatAmount` — the grouping would faithfully carry the wrong number into a newly-correct structure. |
| **3** | **B3 + B3′** (Invariant S) | **B2** | One root cause, two symptoms — land together with the storno mirror and the §3.1 guard re-founding. Highest-value step: closes the SPA-reachable silent-wrong-ÁFA path. |
| **4** | **B1a** (Invariant P) | **B3/B3′** | *Deliberately last of the emit work.* Landing B1a first would gate 7 new doors against a **still-broken emitter** — hardening the door to a room that is on fire. Gate the doors once what is behind them is correct. Also the highest-blast-radius step (§3.4 fallout), so it wants the most stable base. |
| **5** | **B1b** (Invariant C) | **B1a** | Needs the library-boundary chokepoint B1a establishes. Smallest surface, lands on the most-settled base. |

**Independent:** B4 (step 1) — could ship any time, ordered first only for a cheap opener.
**Dependent:** B2 → B3/B3′ (real: shared computation). B1a → B1b (real: shared chokepoint). B3/B3′ → B1a (**risk sequencing, not a compile dependency**) — it could be reordered, and this ADR argues it should not be.

Steps 1–3 and 5 are ordinary gated increments. **Step 4 is the consequential checkpoint** (CLAUDE.md rule 4): it changes what every invoice-originating door accepts. It gets the full adversarial pass; the others get the standing gates. Adversarial-after-every-increment is the analysis-paralysis failure mode and is not what this ADR asks for.

---

## 8. Where this design disagrees with the starting position

Recorded explicitly, per CLAUDE.md rule 7 — surface conflicts, do not average them.

1. **§3.1 — `MixedVatRateKindsUnsupported` must NOT be retired.** The starting position was that grouping "removes the reason it exists". It removes the *stated* reason. A stronger one stands: ADR-0102 §4(a)'s buyer-status matrix is invoice-scoped, so a `KBAET + DRC` invoice is unsatisfiable regardless of how many buckets the summary has. **Keep the guard, rewrite its justification, pin the behaviour (T13).** Not defence-in-depth — a different invariant that happened to share an implementation.
2. **§3.3 — do not normalise at emit; normalise at ingest.** The starting position was "emit the normalised community VAT". Emitting normalised satisfies gate-vs-wire but breaks the *other two* legs of ADR-0102 §3.2's snapshot triple: the side-store and audit payload would still hold the raw string. That relocates the defect rather than closing it. Normalise once at the boundary and let the emitter stay verbatim. **T8 asserts all four artifacts, not two** — that four-way assertion is the difference.
3. **§3.4 — the obstruction is not the request shapes.** The starting position asked what lifting preflight means for the storno/modification shapes, "which are not `IssueInvoiceRequest`". They need not become it. **All three entry points already take `InvoiceInputJson`** (verified, §2). Re-target the preflight onto that plus `Currency` and the question dissolves — one shape, three call sites, no adapters. `IssueInvoiceRequest` was never the right vocabulary for a library invariant; it is an HTTP DTO.
4. **§3.4 — point 5 is not a separate fix.** "CLI storno must validate `--in` against the base" and "the modification guard never reads `request.lines`" are **one** defect (Invariant C) with two front doors. One `validate_against_base` at the library boundary covers the CLI and serve arms of both. Treating them separately would have produced two guards that drift.
5. **§7 — B1a lands after B3/B3′, not first.** Instinct says close the 7 open doors first. But preflight does not catch B3′ (all-`Percent` bodies pass it), so gating first buys nothing while pointing seven newly-gated doors at a broken emitter.
6. **§3.2 — the fully-structural form of Invariant V is declined.** Making `vat_rate_basis_points` unreachable for non-`Percent` kinds is the *correct* model and is rejected as out-of-scope: a type-level refactor across the domain, the DuckDB adapter, the ADR-0049 side-store shape and the SPA wire, for a defect the accessor fix closes completely. Recorded as a residual (§10.2), not silently dropped.

---

## 9. ⛔ BLOCKING PRE-IMPLEMENTATION INPUT — archaeology handoff (DO NOT BEGIN IMPLEMENTATION UNTIL THIS SECTION IS FILLED IN)

**This section is a hard gate, not a formality. It is titled so it cannot be quietly skipped. If it is still empty, the implementation session must stop and say so.**

A parallel archaeology session is establishing two things this design session deliberately did **not** try to answer:

- **(a) Regression history** — was any of B1–B4 fixed before and subsequently regressed? A regression is a different problem from a defect: it means a guard existed and was removed or bypassed, and the fix must also close whatever allowed the removal. A pin that would have caught the regression is a required deliverable if the answer is yes.
- **(b) ⚠ The root-cause question — does any ADR or doc describe the defective behaviour as *correct*?** If such a document exists, **it is the root cause and this ADR is insufficient on its own.** Code contradicting a document loses: the next session reads the document, concludes the code is right, and restores the defect. Any such document **must be superseded in the same change that lands the fix**, not "later".

**Found in passing while reading the code for §2 — candidates for (b), NOT the archaeology result, and each requires confirmation:**

1. **`nav_xml.rs:1885-1889`** — the in-file comment states the single-bucket collapse as a known posture with a "future PR" deferral. Describes B3′ as *deferred*, not as *correct*. Likely benign but must be **deleted**, since after Invariant S it asserts a limitation that no longer exists.
2. **`issue_preflight.rs:1017-1018`** — *"The pre-existing mixed-`Percent`-RATE single-bucket gap is out of ADR-0101 scope and intentionally NOT gated here."* This is the strongest candidate: it **names B3′ precisely and records a decision not to close it.** A future reader encountering the mixed-rate case would find this sentence and conclude it is by design. §3.1 already requires deleting it; the archaeology session should determine whether the decision it records is written up anywhere more authoritative.
3. **ADR-0101 §3.4** — *specified* the summary mirror, including *"the round-trip test must assert line **and** summary agree."* The kind-mirror shipped; the **rate**-mirror did not, and neither did the multi-rate test. **This is a specified-but-unbuilt requirement, which is a materially different finding from an unknown defect** — the design was right and the implementation was partial. Determine whether ADR-0101 needs an amendment recording that §3.4 was only half-landed, so the record is not read as "§3.4 shipped".
4. **`serve.rs:9415`** — *"this route never calls `validate_invoice_preflight` anyway"* — states B1 as accepted context for a different guard's reasoning.

**Required outputs of the archaeology session, before step 1 of §7 begins:**
- for each of B1–B4: regressed / never-fixed / unknown, with evidence;
- the list of documents (ADR / `docs/` / load-bearing code comment) that describe any defective behaviour as correct or acceptable;
- for each such document, whether it is **superseded**, **amended**, or **deleted** — and by which step of §7.

Until then this ADR stays **Proposed**. It does not advance to Accepted.

---

## 10. Flagged assumptions and named residuals (conservative; no AskUserQuestion per session constraint)

1. **NAV multi-bucket `summaryByVatRate` cardinality is assumed unbounded** (`maxOccurs="unbounded"`). The repo vendors no XSD (ADR-0101 §2.1), so this is best-knowledge, not repo-verified. It is the single NAV-shape assumption Invariant S rests on — **⚠ confirm against the published NAV OSA 3.0 `invoiceApi.xsd` `SummaryNormalType` before step 3 lands.** The local `nav-xsd-validator` is deliberately loose (ADR-0022) and will **not** catch a cardinality error; do not read its acceptance as confirmation.
2. **Residual (§3.2):** `vat_rate_basis_points` remains readable on non-`Percent` lines. Invariant V closes every consumer that exists; a future consumer bypassing `vat_amount()` could still read a stale rate. The structural fix (rate carried only in the `Percent` variant) is the correct model and is deferred as its own ADR. **Named, not dropped.**
3. **Residual (§3.4):** the seven newly-gated doors will surface fixture failures. Each is either an always-invalid fixture (evidence for §9) or an over-strict rule. **Neither is resolved by a chain-path escape hatch** — that would reintroduce B1 in miniature and is forbidden by this ADR.
4. **Out of scope (§3.4):** whether the ADR-0101/S2 non-`Percent`-base modification guard can be relaxed once Invariant C lands. That is a feature decision (does the SPA thread `vat_rate_kind` through `composeModificationBody`?), not a defect correction. **Separate ADR.**
5. **Out of scope:** the invoice-level HUF rounding posture beyond ADR-0037 §1.c's per-bucket-then-sum rule. Invariant S must implement that rule; it does not revisit it.
6. **Scope B (Defense) is a port, not an inheritance.** `ABERP-Editions.git` is a separate repository; the fixes do not arrive by rebuild. Each step of §7 needs an explicit Scope-B port and its own green run (§6). Portable, by contrast, genuinely inherits by compilation — the asymmetry is why one is gated and the other parked (§1).
7. **No implementation code was written in this session.** Every file:line in §2 was read at `76e9dad`. Where this ADR asserts current behaviour, it was verified; where it asserts NAV behaviour, it is flagged ⚠ (item 1).
