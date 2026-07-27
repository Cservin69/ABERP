# Gate teeth-audit — 2026-07-26

**Question asked:** for every cut-gate and enforced scanner in `ABERP.git`, does it have a
negative probe proving it can go red? A gate that has never been observed failing is
decoration.

**Method:** every gate was run, and every probe suite was run, on this tree. Nothing below
is inferred from a header comment or a commit message — the arm counts are the `✓` lines
each suite actually printed.

---

## 1. The shell cut-gate layer — result: fully toothed

| # | Gate | Scanner | Probe suite | Green assertions | Liveness backstop | Teeth |
|---|---|---|---|---|---|---|
| 1 | ADR-0099 opener census (P1 count, P2 fingerprint) | `adr0098_opener_scan.awk` | `cut_gate_negative_probes.sh` | 13 † | structural — P2 diffs a non-empty frozen baseline, so a dead scanner reds | ✓ |
| 2 | ADR-0099 read-fork (CHECK N) | `adr0099_read_fork_scan.awk` | `cut_gate_read_fork_probes.sh` | 17 ‡ | `cut_gate_scanner_backstop.sh` | ✓ |
| 3 | ADR-0099 write-fork | `adr0099_write_fork_scan.awk` | `cut_gate_write_fork_probes.sh` | 23 ‡ | `cut_gate_scanner_backstop.sh` | ✓ |
| 4 | ADR-0100 keychain seam | `adr0100_keychain_seam_scan.awk` | `cut_gate_keychain_seam_probes.sh` | 17 ‡ | `cut_gate_scanner_backstop.sh` | ✓ |
| 5 | ADR-0093 product-line saw-off ratchet | structural (no awk) | `cut_gate_edition_ratchet_probes.sh` | 23 ‡ | `cut_gate_edition_ratchet_backstop.sh` | ✓ |
| 6 | **ADR-0106 NAV-emission door** *(added this session)* | `adr0106_nav_door_scan.awk` | `cut_gate_nav_emit_door_probes.sh` | 10 probes / 12 arms † | `cut_gate_scanner_backstop.sh` (CHECK N0) + scope floor | ✓ |

All six gates green; all six probe suites exit 0.

† the suite's own reported probe count (`probes passed: 13`, `probes: 10 passed`). The
ADR-0106 suite's 10 probes assert 12 arms: P1 and P3 each plant one mutation that must fire
two independent checks, verified from a single gate run.
‡ these suites report a verdict line rather than a count, so the figure is the number of
`✓` assertions they printed. The two conventions are not directly comparable and no total
is given, because adding them would imply a precision the measurement does not have.

**This is a better result than expected and it deserves to be stated plainly rather than
padded.** The green-but-blind problem in this tree was found and fixed on 2026-07-21
(findings F2/F4/F5): the keychain and ADR-0099 scanners were fail-open, three gates scored
"0 hits ⇒ green" on a broken scanner, and `apps/aberp-ui/src` was outside the census scope.
The fixes landed with the `cut_gate_scanner_backstop.sh` liveness discipline behind them.
The audit's job today was to check whether that repair actually held across the whole
layer. It did.

So the honest finding is **not** "here are the untoothed gates" — there are none at this
layer. It is that the remaining risk has moved somewhere else, and §2 is that list.

---

## 2. Prioritized remainder — where the teeth are NOT

Ranked by the brief's criterion: anything guarding NAV filing or the prod DB first.

### R1 — HIGH — no cut-gate runs in the `cargo test` loop

Every gate above is CI-only. An implementer who adds a NAV door, a DB opener or a keychain
seam gets a **green local `cargo test`** and learns about the census from a red pipeline
later, after the design has set. This is a known-and-recorded hazard for the opener census
already; ADR-0106 inherits it by construction.

*Partially closed this session, for ADR-0106 only:*
`apps/aberp/tests/adr0106_nav_door_census_pin.rs` puts three code-coupled counts into the
local loop — seven `nav_xml` wire-body emitters, four registered doors (exactly one
`direct`), and one production call site for `validate_invoice_preflight`. Adding an eighth
emitter now reds `cargo test` on the machine that wrote it.

*Not closed:* the same treatment for gates 1–5. The generalisable version is a single
`cargo test` that shells out to the gate scripts, but no existing gate does that and
forking the convention unilaterally is out of scope for a gate-hardening session. **Owner
decision needed** on whether to adopt that pattern repo-wide.

### R2 — HIGH — `derived` doors are trusted, not checked (Invariant P)

ADR-0106 CHECK N3 machine-checks only doors declared `direct`. The storno and modification
routes are declared `derived` and are trusted on their written justification. The
modification route is the weaker of the two: it surfaces **operator-editable fields** that
land in the emitted NAV body having never passed preflight, so "derives from a preflighted
base" covers only the inherited fields.

Closing this is Invariant P (preflight universality, deferred at ADR-0103), a behaviour
change on the NAV filing path that wants its own session. ADR-0106 §5 sets out the
sequencing and why the type-level witness comes after P rather than instead of it.

### R3 — MEDIUM — Invariant C (chain congruence) still has no gate

Deferred at ADR-0103 alongside P, and untouched by ADR-0106. ADR-0106 censuses *who can
reach* the emitters; it says nothing about whether a chain's storno/modification bodies are
congruent with their base. No pin exists for this.

### R3b — MEDIUM — ADR-0103's "8 doors" does not match the measured 7

ADR-0103's deferred-invariants section describes Invariant P as taking the tree from
"1-of-8 → 8-of-8 doors". That figure is not sourced to a scan. ADR-0106's scanner measures
**7** NAV-reaching entry paths: 3 HTTP routes plus 4 CLI verbs. Either ADR-0103 counts
something the scanner does not see — which would be a genuine gap in the census and the
more serious reading — or the 8 was an estimate that hardened into a number.

Not chased here: resolving it means enumerating the doors, which is the first thing the
Invariant P session has to do anyway. Recorded in ADR-0103 alongside the original claim so
the next reader meets both numbers together rather than trusting the older one.

### R3c — LOW (fixed here, recorded because it is the pattern) — probe suites are the expensive part

ADR-0106's first CI run was **cancelled at the 15-minute job timeout**, not failed: the
scanner ran one awk process per source file, and the gate is executed nine times by its own
probe suite. A cancelled gate is indistinguishable from one that never ran.

Fixed by making the scanner multi-file (one awk invocation per gate run): gate 13s → 5s,
suite 90s → 39s, plus dropping the per-arm gate re-runs and raising the job budget 15 → 25
minutes (it runs six gates now, not the five it was budgeted for). The fix's *own* first attempt used `mapfile`, absent on macOS bash 3.2,
which silently scanned zero files — so the gate also gained a **scope floor** (fewer than
100 files in scope is a hard refusal) and probe P8 to prove the floor works.

Generalisable lesson for the other five suites: they each copy the tree per probe and
re-run their gate per planted defect, and together they are the slowest thing in CI. None
is near the budget individually, but it is a **shared job budget** — the re-run passed at
12m17s against the old 15-minute limit, which is not headroom, it is the next cancellation
waiting for someone to add a probe.

### R4 — MEDIUM — ADR-0106 is textual, not semantic

A NAV path constructed through a function pointer, a trait object, or a macro-generated
body is not seen by the scanner. Same limit ADR-0098's opener census carries; recorded, not
fixed. ~~The reaching set's closure property means such a path must still *name* a censused
symbol somewhere to exist~~ — **retracted 2026-07-27**, see R7: a bypass need not name a
censused symbol at all.

---

## 2b. Added by the adversarial review of 2026-07-27

### R7 — HIGH — the ADR-0106 census stops at the body emitter, not at NAV

**A NAV-mis-filing door can still land green today.** The reaching set is anchored on
`nav_xml::render_*_data*`, but filing only requires reaching
`manage_invoice::build_request` / `send_built_request` (or `manage_annulment::call`), which
are **not in the set**. Three bypasses were written and run against the live gate; all
three passed it green:

| # | Bypass | Result |
|---|---|---|
| 1 | Route assembles the `InvoiceData` XML by hand and calls the transport directly | gate **green** |
| 2 | Route writes a hand-built body into the submission queue; `drain_submission_queue` files it | gate **green** |
| 3 | New off-convention body builder in `nav_xml.rs` (not named `render_<what>_data`) + a route calling it | gate **green** |

None emits a single record, so N1/N2/N3 are vacuously satisfied. This is the one finding on
this page under which a NAV mis-filing could reach NAV unnoticed, and it is why ADR-0106 §5's
claim has been corrected from "no NAV door" to "no NAV door *that goes through an emitter*".

**This is not hypothetical — an independent re-enumeration of the call graph found real
production doors the registry never knew about.** The registry lists four doors. Measured:

| Door the registry omits | Reaches NAV via | Preflight |
|---|---|---|
| `serve.rs::handle_submit_invoice` — **live route `POST /invoices/:id/submit`** | `submit_invoice_request` → `submit_invoice::submit_from_inputs` → `send_built_request` | **none** |
| CLI `submit-invoice` | `submit_invoice::run` → same | none |
| CLI `submit-annulment` | `submit_annulment::call_nav` → `manage_annulment::call` | none |
| CLI `retry-submission` | `retry_submission::run` → `send_built_request` | none |
| CLI `drain-submission-queue` | `drive_one_invoice` → `send_built_request` | none |
| CLI `drain-pending-retries` | `drive_one_retry` → `send_built_request` | none |

`POST /invoices/:id/submit` is the route that actually *files* an invoice to NAV, and it is
absent from `adr0106_nav_door_registry.txt` entirely. So the door count is **not 4** — it is
4 emitting doors plus at least 6 filing doors. The registry's `main.rs::main` justification
("Four verbs reach a NAV emitter… a FIFTH NAV-reaching verb cannot be added without this
file changing") is **true of emitters and false of NAV**: five further CLI verbs reach NAV
filing today, and a sixth could be added without moving one record. That line has been
corrected in the registry rather than left to read as a guarantee.

**Cost of closing it, measured not estimated:** adding the four transport symbols to the
reaching set yields ~15 new call records across `submit_invoice.rs`,
`drain_submission_queue.rs`, `drain_pending_retries.rs`, `retry_submission.rs`,
`submit_annulment.rs` and `nav_number_probe.rs`. Each caller then needs a written
disposition. **Not done here on purpose:** authoring dispositions for the submission
subsystem is real judgement about what each daemon replays, and a wrong justification
written into the registry is worse than a known gap. Sequenced like Invariant P — its own
session. **Owner decision needed.**

### R8 — HIGH (FIXED here) — every scanner treated prod-compilable cfgs as test-only

All **five** awk scanners (`adr0098_opener`, `adr0099_read_fork`, `adr0099_write_fork`,
`adr0100_keychain_seam`, `adr0106_nav_door`) shared one predicate:

```awk
if (st ~ /^#\[cfg\(/ && st ~ /test/ && st !~ /not\(test\)/) pending=1
```

That skips any cfg line whose text merely *contains* `test`. But
`#[cfg(any(test, feature = "test-support"))]` and `#[cfg(feature = "test-support")]`
**compile into non-test builds**, and this tree already uses the first idiom in
`crates/audit-ledger/src/entry/actor.rs` and `crates/nav-transport/src/credentials/mod.rs`
— i.e. on the NAV credentials path. A door, opener or keychain seam behind either was
invisible to its gate while shipping in any build with the feature on. This is the
"`#[cfg(test)]` door that drifts to prod" shape, and it was live in all six gates.

Fixed by skipping only cfgs that *require* `test` (bare `#[cfg(test)]`, or `all(...)` with
`test` as a bare conjunct); everything else is scanned, which is the fail-closed direction.
**Behaviour-neutral on today's tree** — all five scanners produce byte-identical output and
no frozen baseline moved. Pinned by a new always-enforced N0 control and probe **P9**, both
mutation-verified: with the old predicate restored, the control reports `expected 2, got 0`
and P9 fails on its named arm.

*Residual:* the teeth for this arm exist only in ADR-0106's suite. Gates 1–5 got the
predicate fix but no probe of their own for it. Same shape as R1 — recorded, not chased.

### R5 — MEDIUM — the Editions mirror does not exist

`ABERP-Editions.git` carries the same seven emitters **and** the modification route whose
preflight bypass is one of the three findings that motivated this gate. ADR-0106 is not
mirrored there. Until it is, the repo where the defect was actually found is the repo
without the gate. See §3.

### R6 — LOW — `ALL_KINDS_COUNT` and the XSD round-trips are self-proving

`EventKind::ALL_KINDS_COUNT == 188` is asserted at two sites (`crates/aberp-verify/src/verify.rs`,
`apps/aberp/src/export_invoice_bundle.rs`) as a compile-time equality — it reds by
construction when the count moves, so a separate negative probe would add nothing. The NAV
XSD validator carries 10 rejection-direction tests, and `db_path_guard` carries 11 tests
including the `..`-behind-a-missing-component fail-open fixed on 2026-07-21. No action.

*(⚠ per the ADR-0105 handoff, `ALL_KINDS_COUNT` has **three** pin sites, not two. Only two
are visible to a `--include=*.rs` grep of `crates` + `apps` from this tree. Not chased —
out of scope for this session and inside the concurrently-active BOM session's blast
radius.)*

---

## 3. Editions-side follow-ups — NOT actioned here

Named for the record; a separate repo and explicitly out of this session's scope:

- Mirror ADR-0106 onto `ABERP-Editions.git` (**R5** above — highest of these).
- The modification-route preflight bypass (Editions PR #28).
- The PDF rate-kind finding.
- The B4 side-store finding.
