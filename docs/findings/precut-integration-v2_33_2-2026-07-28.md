# PRE-CUT integration adversarial — PROD_v2.33.2

**Delta:** `PROD_v2.33.1 (27aa689)` → `origin/main (3cd6df6)`
**Date:** 2026-07-28 **Verdict: GO**

Three changes in the delta: the structural read-fork gate (#43, `a57f714`), the
partner `customer_type` casing fix (`0d89185`), and the invoice-PDF column /
logo change (#44, `88b3630`). The gate change carries a gate-internals edit, so
this review doubles as its owed adversarial and attacks it hardest.

---

## F1 — the "structural" read-fork rule was defeated by rustfmt. FIXED.

**Severity: high (detector). Introduced by #43. Fixed on this branch.**

#43's whole thesis is that a NAME allow-list cannot protect against a bug class
and the SHAPE must be detected instead. The shape rule it shipped, however, was
recognised only in the line-local forms its author happened to write. Three
variants of the same shape came back clean:

| # | shape | why it was invisible |
|---|-------|----------------------|
| 1 | read call **wrapped across lines** — `load(\n &tx,\n id,\n)?` | the argument-list rule needs the `(`/`,` *and* the callee `ident(` on ONE line |
| 2 | `h.read()?.query_row(…)` — propagate-then-read on one line | only the FIRST link of the method chain was inspected, and it is a propagator |
| 3 | `Connection::open(p)?.query_row(…)` — read chained onto the opener | the `let` binds the RESULT of the read, so nothing is ever tainted |

(1) is the serious one. It is **not an evasion anyone has to contrive —
`rustfmt` emits it automatically** for any read-through call past 100 columns.
Verified by running `rustfmt` on a realistic call: it produced the invisible
form unprompted. The rule's reach therefore depended on the character length of
the callee's path: adding one argument to a caught fork silently un-caught it.

**What it was hiding.** Switching all three on surfaced **five more pre-existing
forks** the D1 census could not see (28 → 33):

- `serve.rs|handle_relay_send_email` — **live in-serve.** `POST
  /api/internal/send-email` does `duckdb::Connection::open(&state.db_path)`
  inside its `spawn_blocking` and hands the connection to
  `email_relay_queue::insert_queued(…)` — a call rustfmt wraps. Co-resident with
  the shared Handle (`state.db.clone()` is used a few lines below) and it
  **writes**, so it is a write-fork too. Its sibling *readers* of the same table
  (`handle_list_email_relay_queue`, `handle_get_email_relay_row`) were already
  frozen in GROUP A: **a fused family split in half by a formatting accident**
  (CLAUDE.md rule 14).
- `issue_invoice|run_with_provider`, `issue_modification|run`,
  `issue_storno|run`, `poll_ack|run` — the four DUAL-CONTEXT fns. The read-fork
  allow-list header has named these for months as the ones deliberately *not*
  exempted ("they DO run in-serve… the in-serve path must read via the Handle"),
  but they were in **no** list at all: the audit rule cannot see them (they open
  `Handle::open_default`, absent from its token set) and D1 could not see them
  (rustfmt-wrapped reads). The gate documented a worklist that was empty of them.
  Re-verified: all four hold the F-E flock and every non-comment call site is
  `main.rs`. `poll_ack`'s in-serve entry point is `run_nav_poll_daemon`, a
  different fn. **Coherent today → GROUP B**, frozen rather than exempted.

**All five are PRE-EXISTING.** Re-running the fixed scanner against `27aa689`
gives the same `serve.rs` hit set — the delta introduced none.

**Fix (this branch):** walk the whole method chain rather than its first link;
treat a rustfmt continuation-argument line (`&tx,` alone, no parens on the line)
as a read; scan the chain to the right of the opener on the opener's own line.
Four new RED/GREEN control pairs in CHECK N0 pin each shape, including a
Handle-routed twin of the wrapped call so "wrap anything and it reds" cannot
pass. The five forks are **frozen in the baseline with triage, not silenced** —
same posture #43 itself took.

**Known residual, fail-CLOSED:** a tuple-returning factory (`Ok((h, meta))`)
reports as a read. It over-reports, never under-reports; costs a baseline triage,
never a missed fork. Left as-is deliberately.

## F2 — "the two halves are complements" is true of the test suite, not of prod.

**Severity: low (documentation accuracy). Corrected in the header.**

#43's argument for the factory-split carve-out is that the SERVE_HANDLE_LIVE
runtime tripwire is call-graph-complete and covers what the scanner cannot.
`assert_no_serve_handle` is `#[cfg(debug_assertions)]` and an explicit no-op in
release, so **in a prod build the runtime half is inert** and the static scanner
stands alone. The factory-split residual has no production backstop. Documented
in the scanner header rather than changed — the gating itself is intentional.

---

## What held (attacks tried that failed to land)

**#43 — read-fork shapes planted, scanner caught all of them.** Fifteen probes.
Caught: brand-new never-listed fn name + business-table helper read;
`Handle::open_default` under a new name; `Ledger::open` handed to a brand-new
helper; opener stored in a struct field then read via the field; opener inside a
closure; opener in a trait-impl method; rustfmt-wrapped call (first and later
argument positions); read chained onto the opener; propagate-then-read on one
line. Correctly silent on: the factory that opens and RETURNS (boot opener stays
green), the Handle-routed twin of every positive, and `#[cfg(test)]` bodies.

**#43 — tripwire.** 10/10 tests pass. It fires on a SECOND in-request
`Handle::open` on the registered path and does **not** false-trip on the
legitimate boot Handle — structurally impossible, since `serve::run` opens at
`serve.rs:1555` and registers at `:1571`. Also clean on: a different tenant path,
after guard drop, nested refcounted registration, the shared Handle's own
read/write/append, and the background `sync_mirror` + debounced-checkpoint paths.

**#43 — no sibling gate weakened.** The delta touches only its own four gate
assets. Opener census (81 openers, P1+P2), NAV-emission door (29 records, closure
+ preflight), write-fork, keychain-seam, edition-ratchet baselines all untouched
and green. The probes file was *strengthened*: P5 (business read) was inverted
from `expect_silent` — which asserted the blind spot — to `expect_emit`, with a
Handle-routed negative twin added.

**#2 casing fix — no over-loosening.** All eight variants × {domestic HU,
foreign SK} save through the real serialized SPA body, and the response hands
the same literal back. Verified independently that serde's wire string equals
`as_db_str()` for all eight, and that a present-but-invalid value is **refused,
not coerced**: `"Industrial"`, `"PrototypeShop"`, `"Unset"` (the pre-fix
PascalCase forms), `"INDUSTRIAL"`, `"prototypeshop"`, `"med_tech"`, `""` all
error. `#[serde(default)]` applies only to an ABSENT key, as intended. A ninth
variant cannot silently fork the vocabulary: `customer_type_index` is an
exhaustive match, so it fails to compile until handled. Nothing NAV-adjacent
changed; audit payloads already carried `as_db_str()`. The SPA-side change is
one line gating `tax_number` on `Domestic`, matching the server validator that
was already rejecting it — client-side only, no server loosening.

**#3 PDF fix — columns hold.** Metrics verified against real Adobe Helvetica AFM
(space .278, percent .889, digits .556, hyphen .333, F .611, t .278 — exact
match). Rendered samples and analysed the painted text runs geometrically out of
the PDF content stream rather than by eye. **Zero overlaps** in the magnitude
sweep (8/9/10/11-digit grosses on one invoice) and in the 40-line render.
Derived edges: QTY 283 / UNIT 367 / NET 440 / VAT 468 / GROSS 541.
Headroom measured: gross values stay clear to **10 digits** (2pt gutter left);
11 digits would overlap by 3pt. The stated worst case is 9 digits (63pt) — so
there is one digit of real headroom beyond the pin, and the pin fails loudly
before a customer PDF would. The ÁFA cell renders `format!("{}%", …)` only —
never an exemption code — so `AAM`/`K.AFA` (19pt/26pt, over the 18pt budget)
cannot reach it; that attack does not land. Logo: 1224×876 (under the 4096 cap),
scaled by `min(box/w, box/h)` — **uniform, aspect preserved, no distortion** —
and the title cluster shifts by the full `LOGO_BOX_SIDE` so a wide logo cannot
overlap it. The asset itself still has **zero code references**; it is a runtime
operator asset loaded from `~/.aberp/<tenant>/logo.png`, so the updated PNG in
the repo does not ship into any PDF by itself.

---

## Out of scope — pre-existing, NOT delta regressions

Recorded here, not fixed; neither blocks the cut.

- **D-1: the invoice PDF has no pagination.** `"Count" => 1` is hardcoded and
  there is no line-count guard. Measured threshold: content stays on the page to
  **15 line items**; at 16 it reaches y=28, at 17 y=0, and **at 18+ it is drawn
  at negative y — off the physical page, invisible and unrecoverable**, taking
  the totals block and the ÁFA summary with it on longer invoices. This is the
  "completed successfully with 14% of records silently skipped" failure mode
  (CLAUDE.md rule 11) on a legally-required document. #44 is purely horizontal
  and neither caused nor worsened it. Needs its own change: at minimum a loud
  refusal above the fitting line count, properly a second page.
- **D-2: `handle_relay_send_email` and the twelve other GROUP A forks** remain
  open. Now frozen and visible; migration is product work per route.
- An unbreakable token wider than the description column overflows into the
  neighbouring columns. This is **documented and deliberate** — `wrap_to_width`
  prefers visible overflow to silent truncation. Not a defect.

---

## Gates on the merged tree

`cargo fmt --check` clean · workspace build clean · full test suite green ·
`clippy -D warnings` clean · read-fork gate ENFORCING (16/16 controls, CHECK N1
`0 new` at 33 frozen) · opener census P1+P2 (81) · NAV-emission door N1+N2+N3
(29 records, 4 doors, preflight held) · write-fork CHECK M · keychain seam CHECK
K (280 files) · edition ratchet E1–E4 · negative-probe gates 17/11/22/16/all.

`CHECK N1 ✓ 0 new` means **no fork was ADDED**. 33 remain OPEN — read the
baseline for what.
