# ADR-0106 — The NAV-emission door gate

- **Status:** **Accepted** (implemented this session).
- **Date:** 2026-07-26
- **Deciders:** Ervin Áben. Implementation-pass by Claude.
- **Related:** ADR-0038 (`validate_invoice_preflight` — the choke point this gate is measured against), ADR-0098 / ADR-0099 (the opener census — the mechanism this is modelled on, and the hardened lexer this reuses verbatim), ADR-0100 §4 (keychain seam scan), ADR-0103 (NAV-emission S/V/I corrections — **Invariants P and C deferred there are the residual this gate makes visible**), ADR-0023 / ADR-0024 (storno and modification chains).

---

## 0. TL;DR

Three separate findings this month were the same defect: **a code path reached NAV filing while bypassing the single validation choke point `validate_invoice_preflight`.** F1 (the `ABERP_DB` door), the Editions modification route (Editions PR #28), and the deferred Invariants P and C are not three bugs — they are three instances of one shape. The shape is possible because **NAV wire bodies can be constructed from more than one place and nothing enumerated the places.**

This ADR adds one gate that closes the class rather than the instances:

1. **A scanner** (`tools/adr0106_nav_door_scan.awk`) that finds every runtime call to a symbol in a **reaching set**, every call to the choke point, and every definition of a NAV wire-body emitter.
2. **A reaching set closed under calls** (`tools/adr0106_nav_reach_symbols.txt`). To reach an emitter you must call something already in the set — so any new reaching function necessarily produces a new record.
3. **A door registry** (`tools/adr0106_nav_door_registry.txt`): every terminal entrypoint that can reach NAV filing, each with a **written preflight disposition**.
4. **A cut-gate with four checks** (`tools/cut_gate_nav_emit_door.sh`), ENFORCE on, wired into `.github/workflows/cut-gate.yml`.
5. **Negative probes** (`tools/cut_gate_nav_emit_door_probes.sh`) that plant one synthetic instance of **each historical form** of the defect and assert the gate reds on the named arm.

**What it does not do:** it does not assert that every door preflights. Today one does, two derive, and one does not at all. Making preflight universal is **Invariant P**, a behaviour change on the highest-consequence path in the system, and is not this gate's job. §5 explains why the type-level version is the follow-up and not the thing built here.

---

## 1. Current state (grep-verified before designing)

The scanner was written first and pointed at the tree; the table below is its output, not an assumption.

**Seven NAV wire-body emitters, all in `apps/aberp/src/nav_xml.rs`:**

| Emitter | Line |
|---|---|
| `render_invoice_data` / `_with_number` | 615 / 652 |
| `render_storno_data` / `_with_number` | 794 / 829 |
| `render_modification_data` / `_with_number` | 985 / 1011 |
| `render_annulment_data` | 1146 |

`render_annulment_data` **was missing from the hand-written seed list** and has a live prod caller (`request_technical_annulment.rs:284`). It was found by the scanner's `#emitter-def` arm during this gate's own construction — which is the first evidence that enumerating by hand is exactly the thing that fails.

**Three emit call sites, four doors:**

| Door | Reaches NAV via | Preflight? |
|---|---|---|
| `serve.rs::handle_issue_invoice` (`POST /invoices/issue`) | `issue_invoice_request` → `issue_from_parsed` → `render_invoice_data_with_number` | **yes** — `validate_invoice_preflight`, first, before anything downstream |
| `serve.rs::handle_storno_invoice` (`POST /api/invoices/:id/storno`) | `storno_invoice_request` → `storno_from_inputs` → `render_storno_data_with_number` | **no** — replays the base's preflighted `input.json` |
| `serve.rs::handle_modification_invoice` (`POST /api/invoices/:id/modification`) | `modification_invoice_request` → `modification_from_inputs` → `render_modification_data_with_number` | **no** — replays the base's `input.json`, **plus operator-editable fields that were never preflighted** |
| `main.rs::main` (CLI: `issue-invoice`, `issue-storno`, `issue-modification`, `request-technical-annulment`) | four verbs, each to its emitter | **no** — the CLI has always been the operator-trusted lane |

So the invariant "every NAV-emitting path passes the choke point" is **false in this tree today**, in three of four doors. That fact drives the whole design below.

---

## 2. Decision

Census the **reaching set**, not the emitters, and force every caller into one of exactly two boxes.

**Why not census the emitters alone.** The defect this exists for is a *new door*, and a new door usually does not construct a body itself — it calls an existing helper that does. That is literally how the modification route landed. An emitter-only census is blind to it. Probe P2 pins this: a handler that calls `storno_invoice_request` and constructs nothing reds the gate.

**Why the reaching set is sound.** The set of functions that can reach a NAV emit is closed under calls: to enter it, you must call something already in it. So censusing calls to the *whole set* means any new reaching function produces a new call record — with no exceptions and no reachability analysis required. The gate then forces that function to be either

- **added to the reaching set** (extending the census one hop outward, and re-freezing), or
- **declared a DOOR** with a written preflight disposition.

There is no third option that keeps the gate green. That dichotomy is CHECK N2, and it is the whole mechanism.

---

## 3. The four checks

| Check | Asserts | Switch |
|---|---|---|
| **N0** liveness | The scanner is handed synthetic controls in both directions, including the two lexer traps (a char literal holding a quote; a raw string holding a stray quote) that were live fail-opens in this tree. | none — **always** enforced |
| **N1** record freeze | The exact set of `<file>\|<fn>:<symbol>` records may not add, remove or content-swap. 29 records today. | `ENFORCE_NAV_DOOR_RECORDS` |
| **N2** closure | Every function observed calling a reaching symbol is itself in the set or a registered door. | `ENFORCE_NAV_DOOR_CLOSURE` |
| **N3** preflight | Every door declared `direct` must have a `validate_invoice_preflight` call observed **inside that function**. | `ENFORCE_NAV_DOOR_PREFLIGHT` |

N0 is deliberately not switchable: per finding F4, a dead scanner is a broken tool, not a policy question. Three gates in this tree scored "0 hits ⇒ green" on a scanner that had stopped working.

Fingerprints deliberately **exclude line numbers** (same convention as ADR-0098's opener fingerprints), so unrelated edits that shift lines in a 29k-line `serve.rs` do not red the gate. What is pinned is *who calls what* — the only thing that bears on reachability.

**N1 and N3 are independent arms on the same defect.** Deleting the preflight call from `handle_issue_invoice` (the F1 shape) reds N1 (a record vanished from the frozen set) *and* N3 (a `direct` door lost its witness). Probes P3 and P3b assert both.

---

## 4. Mutation verification

A gate never observed catching its own target is decoration. `tools/cut_gate_nav_emit_door_probes.sh` plants each defect into a throwaway tree copy — no real source file is mutated — and asserts RED on the **named arm**, not merely non-zero exit:

```
✓ green: P0 control — unmutated tree
✓ reds: P1 synthetic route emits a NAV body with no preflight (via CHECK N1)
✓ reds: P1 (same mutation, closure arm) unregistered NAV door
✓ reds: P2 new handler reaches NAV via an existing helper (no body of its own)
✓ reds: P3 preflight call deleted from the issue route (record set)
✓ reds: P3 preflight call deleted from the issue route (declared-direct arm)
✓ reds: P4 an eighth nav_xml wire-body emitter is added
✓ reds: P5 emitter reached through a `use … as` alias
✓ reds: P6 a broken scanner fails the gate instead of silently passing it
✓ reds: P6b a silent (rule-less) scanner fails the gate
✓ reds: P8 empty scope + empty baseline is REFUSED, not vacuously green
✓ green: P7 an unrelated non-NAV route does not red the gate

probes: 12 passed, 0 failed
```

P0 and P7 are load-bearing in the other direction. Without P0, every "reds" line could just mean "the gate is always red". P7 pins the blast radius: a gate that reds on every new route gets switched off.

### 4.1 The gate's own first CI run failed, and P8 is why

The first push scanned one file per awk process. The gate is run **nine times by its own
probe suite**, so that was ~10k process spawns, and the probes step was **CANCELLED at the
15-minute job timeout**. A cancelled gate proves nothing — it is indistinguishable from a
gate that was never run.

The fix was to make the scanner multi-file (`FILENAME` in each record, lexer reset at
`FNR==1`), one awk invocation per gate run: 13s → 5s for the gate, 90s → 49s for the whole
suite, and ~3 orders of magnitude fewer spawns.

The first attempt at that fix used `mapfile`, **which does not exist on macOS's bash 3.2**.
The array stayed silently empty, so the gate scanned *zero files*. It happened to red on N1
(the frozen baseline was non-empty) — but that is luck, not design: had anyone re-frozen the
baseline while the scope was broken, all four checks would have passed on zero evidence, and
CI (bash 5) would have been green throughout.

Hence the **scope floor**: fewer than 100 files in scope is a hard refusal, not a scan. P8
is the only thing that proves the floor works, and it plants exactly that state — empty
scope *and* empty baseline. This is the gate's own instance of the failure mode it was
built to prevent, and it is recorded rather than quietly patched.

---

## 5. Why NOT the type-level version (and what would make it right)

The strongest conceivable mechanism is a door that **cannot compile** without going through validation: make the emitters demand a `PreflightPassed` witness with a private constructor that only `validate_invoice_preflight` can mint. That is genuinely stronger than a scanner, and it was the first design considered.

It is **not achievable today**, for a specific reason: *the invariant it would encode is currently false.*

- Preflight validates an `IssueInvoiceRequest`. The storno and modification paths have no such value — they replay an `InvoiceInputJson` side-store — and the CLI paths parse their own input shape.
- So a witness type would need a second constructor for the derived paths: something like `PreflightPassed::derived_from_base(...)`. That constructor is an escape hatch, callable by anyone, which is a scanner with worse ergonomics and a compile-time *appearance* of safety it does not have.
- Adding a parameter to seven emitters and threading it through the highest-consequence path in the system, in order to encode an invariant that three of four doors violate, is the "half-built type-level version" the brief warned against.

**The correct order is:**

1. **This gate** (now) — freeze the class, make every door and its disposition written down and machine-diffed.
2. **Invariant P** (follow-up) — make preflight universal: run it on the replayed `input.json` at storno/modification time and on the CLI input shapes. This is a behaviour change on NAV filing and wants its own session with its own adversarial pass. The modification route's operator-editable fields are the strongest single motivation, and the Editions-side finding is the same hole in the same place.
3. **The witness type** (after P) — once every door genuinely preflights, the witness has no escape hatch to carve out, and the compile-time guarantee becomes real rather than decorative.

Until step 2 lands, this gate's honest claim is: **no NAV door can be added, moved or silently de-preflighted without a red gate and a registry diff.** It does not claim every door preflights, and the registry says so in plain text for each one.

---

## 6. Consequences

**Good.** The class is closed at the enumeration, which is where it was actually broken. A new NAV-reaching path now costs a registry line and a written disposition — the point at which a human has to state, in prose, whether it validates. The four dispositions are a live inventory of Invariant P's remaining surface, so the residual is countable instead of anecdotal.

**Cost.** Legitimate NAV-path refactors now red the gate and need a re-freeze. That is intended — that diff is the review signal — but it is friction on exactly the code that gets refactored under time pressure. The mitigation is that fingerprints are line-number-free, so only genuine call-graph changes trigger it.

**Known limits, stated rather than papered over.**

- **Textual, not semantic.** A call through a function pointer, a trait object, or a macro-generated body is not seen. The reaching set's closure property means such a path still has to *name* a censused symbol somewhere to exist at all, but a sufficiently indirect construction could evade it. This is the same limit ADR-0098's opener census carries.
- **`derived` is not machine-checked.** N3 verifies only `direct`. A door declared `derived` is trusted on its written justification. Invariant P is what turns those into `direct`.
- **Editions is not covered.** `ABERP-Editions.git` carries the same emitters and the modification route whose bypass started this. Mirroring this gate there is a named follow-up.
