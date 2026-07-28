#!/usr/bin/env bash
#
# cut_gate_read_fork.sh — ADR-0099 H3 audit-ledger READ-FORK gate (CHECK N).
#
# The DUAL of the write-fork gate (CHECK 10M). That gate scans for an independent
# opener that APPENDS; it is STRUCTURALLY BLIND to an independent opener that
# READS. Once ANY audit writer is on the shared aberp_db::Handle (waves 1-2e
# already are) the Handle's audit rows are WAL-resident (checkpoint disabled in
# H3), so a FRESH `Ledger::open` / `Connection::open` reader sees only the folded
# SUBSET on the main file — a silent torn read. Proved in wave-2e: machine_crud's
# fresh read-back saw 1 of 3 events; the Handle read saw all 3. A gate that cannot
# see a bug class does not protect against it — this closes that gap.
#
# MODE (H3 STEP 7 acceptance — the worklist hit ZERO, so ENFORCE is now the
# DEFAULT; the acceptance state is fork-zero-ENFORCED):
#   default             — ENFORCING: exit non-zero if ANY non-allow-listed in-serve
#                         read-fork remains. Also set explicitly in cut-gate.yml so
#                         the flip is visible at the CI surface.
#   ENFORCE_READ_FORK=0 — INFORMATIONAL: print the in-serve read-forks + count,
#                         exit 0. Retained only as a local diagnostic probe.
#
# Scope: apps/aberp/src + modules + crates, minus */tests/* and /aberp-db/.
# Allow-listed (tools/adr0099_read_fork_allowlist.txt): SEPARATE-PROCESS CLI
# one-shots only (no live Handle; flock-fenced) — their fresh reads are coherent.
#
# STATIC LIMITATION (see ADR-0099 §CHECK N): the allow-list encodes a runtime-
# reachability assumption (a fn's process). Four DUAL-CONTEXT fns (issue_storno,
# issue_modification, poll_ack, submit_invoice) run in BOTH serve and CLI — the
# same fn is coherent in CLI but hazardous in-serve; they are (correctly) NOT
# allow-listed, so they land in the worklist. The RUNTIME TRIPWIRE
# (SERVE_HANDLE_LIVE, proposed) is the backstop static scoping cannot provide.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SCAN="tools/adr0099_read_fork_scan.awk"
ALLOW="tools/adr0099_read_fork_allowlist.txt"
BACKSTOP="tools/cut_gate_scanner_backstop.sh"
for req in "$SCAN" "$ALLOW" "$BACKSTOP"; do
  [[ -f "$req" ]] || { echo "✗ FAIL: required gate asset missing: $req"; exit 1; }
done
# shellcheck source=tools/cut_gate_scanner_backstop.sh
source "$BACKSTOP"; bs_init

enforce="${ENFORCE_READ_FORK:-1}"
echo "ADR-0099 H3 read-fork gate (CHECK N) — root: $ROOT  (mode: $([[ "$enforce" == "1" ]] && echo ENFORCING || echo informational))"

# ── CHECK N0 — scanner liveness (ALWAYS ENFORCED, never informational) ──
# This gate was "zero forks ⇒ green" with `2>/dev/null`: a crashed or silent
# scanner reported ZERO and PASSED. The raw-string control is not hypothetical —
# this scanner WAS fail-open on it until 2026-07-21 (its char-literal rule did
# not cover raw strings), so a `Ledger::open` + `.entries()` read-fork sitting
# after any `r##"… " …"##` in the file was invisible.
echo "[CHECK N0] scanner liveness — the scanner sees a planted read-fork, incl. behind lexer traps"
bs_check "$SCAN" 1 "positive: Ledger::open + .entries()" <<'RS'
fn control() {
    let l = Ledger::open(p).unwrap();
    let _ = l.entries();
}
RS
bs_check "$SCAN" 1 "positive: read-fork shielded by a char literal" <<'RS'
fn control() {
    out.push('"');
    let l = Ledger::open(p).unwrap();
    let _ = l.entries();
}
RS
bs_check "$SCAN" 1 "positive: read-fork shielded by a raw string" <<'RS'
fn control() {
    let s = r##"a "# b"##;
    let l = Ledger::open(p).unwrap();
    let _ = l.entries();
}
RS
bs_check "$SCAN" 1 "positive: raw SQL read — Connection::open + FROM audit_ledger" <<'RS'
fn control() {
    let c = Connection::open(p).unwrap();
    let _ = c.query("SELECT seq FROM audit_ledger");
}
RS
bs_check "$SCAN" 0 "negative: Handle-routed from_connection read (must NOT hit)" <<'RS'
fn control(db: &Handle) {
    let l = Ledger::from_connection(db.read().try_clone().unwrap());
    let _ = l.entries();
}
RS
# D2 (2026-07-27) — the PINNED-name ratchet. A pinned fn forks on the OPENER
# ALONE, with no audit-ledger token anywhere: that is the whole point (its reads
# are BUSINESS-table, which this scanner is structurally blind to). These three
# controls pin all three halves of the rule — it fires on a pinned name, it does
# NOT fire on the same body under an unpinned name (so the teeth stay honest
# about their reach), and it still does NOT fire once the fn rides the Handle.
bs_check "$SCAN" 1 "positive: PINNED name + fresh Connection::open, business read only" <<'RS'
fn read_base_line_vat_kinds(db_path: &Path, invoice_id: &str) -> Result<Vec<VatRateKind>> {
    let mut conn = Connection::open(db_path)?;
    let tx = conn.transaction()?;
    let pair = billing::load_ready_invoice_by_id(&tx, invoice_id)?;
    Ok(pair.map(|(i, _)| i.lines).unwrap_or_default())
}
RS
bs_check "$SCAN" 0 "negative: PINNED name reading through the shared Handle (must NOT hit)" <<'RS'
fn read_base_currency(conn: &Connection, invoice_id: &str) -> Result<Currency> {
    let mut conn = conn.try_clone()?;
    let tx = conn.transaction()?;
    Ok(load_invoice_currency_metadata_in_tx(&tx, invoice_id)?.currency)
}
RS

# ── D1 (2026-07-28) — the STRUCTURAL controls. THE TEETH TEST. ──
# Until today the control directly below was a NEGATIVE, asserting the blind spot:
# the D2a body under an UNPINNED name did not hit. That is the hole four shipped
# defects went through (#40 render, #41 modification base, #42 auto-email, E1), and
# flipping it to a POSITIVE is the point of this change — a scanner that only reds
# on names it was told about cannot protect against the class. The controls come in
# RED/GREEN pairs so "it hits everything" cannot pass either.
bs_check "$SCAN" 1 "STRUCTURAL positive: D2a body under a BRAND-NEW never-listed name" <<'RS'
fn a_name_this_gate_has_never_heard_of(db_path: &Path, invoice_id: &str) -> Result<Vec<VatRateKind>> {
    let mut conn = Connection::open(db_path)?;
    let tx = conn.transaction()?;
    let pair = billing::load_ready_invoice_by_id(&tx, invoice_id)?;
    Ok(pair.map(|(i, _)| i.lines).unwrap_or_default())
}
RS
bs_check "$SCAN" 0 "STRUCTURAL negative: the SAME unlisted fn once Handle-routed (must NOT hit)" <<'RS'
fn a_name_this_gate_has_never_heard_of(conn: &Connection, invoice_id: &str) -> Result<Vec<VatRateKind>> {
    let mut conn = conn.try_clone()?;
    let tx = conn.transaction()?;
    let pair = billing::load_ready_invoice_by_id(&tx, invoice_id)?;
    Ok(pair.map(|(i, _)| i.lines).unwrap_or_default())
}
RS
# E1's ACTUAL body, verbatim from `baf5095` (post-#40, pre-#42). `Handle::open` is
# in neither the ADR-0098 opener census nor — until 2026-07-28 — the
# SERVE_HANDLE_LIVE tripwire, so when #40 moved this fn off `Ledger::open` onto its
# own Handle EVERY detector went quiet on a path that still forked. This control is
# the regression pin for that specific escape.
bs_check "$SCAN" 1 "STRUCTURAL positive: E1's real body — a second Handle::open, read through" <<'RS'
pub fn render_to_bytes(invoice_id: &str, db: &Path, tenant: &str) -> Result<RenderedInvoice> {
    let handle = aberp_db::Handle::open_default(db, tenant_id).with_context(|| {
        format!("open shared DuckDB handle at {}", db.display())
    })?;
    let conn = handle
        .read()
        .context("acquire process-local reader")?;
    render_to_bytes_on_conn(invoice_id, &conn, tenant, seller_toml)
}
RS
# The FACTORY carve-out, both directions. `serve::open_tenant_handle` opens and
# RETURNS — the sanctioned boot opener, and it must stay green or the rule is
# unusable. The cost is stated in the scanner header: a fork SPLIT across a factory
# and its caller has no opener in the reading fn and is invisible here. That gap is
# the runtime tripwire's (it fires on the OPEN itself, whoever did it, and now hooks
# `Handle::open`) — the two halves are complements, not duplicates.
bs_check "$SCAN" 0 "STRUCTURAL negative: factory that opens and RETURNS the handle" <<'RS'
fn open_tenant_handle(db_path: &Path, tenant: TenantId) -> Result<Arc<Handle>> {
    let h = aberp_db::Handle::open_default(db_path, tenant)?;
    Ok(h)
}
RS
bs_controls_ok || { echo; echo "READ-FORK GATE: ✗ FAILED (scanner liveness)"; exit 1; }
echo

# SCOPE FIX (finding F5, 2026-07-21) — was `apps/aberp/src …`, which excluded
# apps/aberp-ui/src (a crate that resolves $ABERP_DB itself, lib.rs:762). See the
# cut_gate_opener_census.sh header for the full finding.
scope_files() { find apps/*/src modules crates -name '*.rs' | grep -vE '/tests/|/aberp-db/' | sort; }
allow_set="$(grep -vE '^\s*#' "$ALLOW" | sed '/^\s*$/d' | sort -u)"
is_allowed() { grep -qxF "$1" <<< "$allow_set"; }

# ── EXEMPTION ↔ PREMISE COUPLING (hard invariant, ALWAYS enforced) ──
# The CLI allow-list is sound ONLY because the cross-process F-E flock refuses a
# second writer — proven by two PERMANENT process-level tests. If the allow-list
# exempts anything, those tests MUST exist; otherwise the premise that justifies
# every entry has silently rotted and the exemptions are void. Couple them so
# neither can be removed alone.
FLOCK_TEST_FILE="apps/aberp/tests/db_writer_lock_e2e.rs"
FLOCK_REFUSE_TEST="second_process_is_refused_the_whole_db_writer_lock"
FLOCK_SIGKILL_TEST="lock_is_released_when_the_holder_is_sigkilled"
if [[ -n "$allow_set" ]]; then
  miss=""
  grep -q "fn ${FLOCK_REFUSE_TEST}" "$FLOCK_TEST_FILE" 2>/dev/null || miss="$miss $FLOCK_REFUSE_TEST"
  grep -q "fn ${FLOCK_SIGKILL_TEST}" "$FLOCK_TEST_FILE" 2>/dev/null || miss="$miss $FLOCK_SIGKILL_TEST"
  if [[ -n "$miss" ]]; then
    echo "✗ EXEMPTION PREMISE UNTESTED — the CLI read-fork allow-list exempts $(wc -l <<< "$allow_set" | tr -d ' ') entry-lines"
    echo "  on the cross-process flock, but its proving test(s) are MISSING:$miss"
    echo "  (expected in $FLOCK_TEST_FILE). The premise that justifies EVERY allow-list entry is"
    echo "  gone → the exemption is VOID. Restore the flock test, or empty the allow-list."
    echo
    echo "READ-FORK GATE: ✗ FAILED (exemption/premise decoupled — a hard invariant, not informational)"
    exit 1
  fi
fi

# A CLI one-shot's fresh audit read is coherent ONLY because the cross-process
# whole-DB writer flock (F-E, db_writer_lock::acquire_or_refuse) makes it mutually
# exclusive with serve — aberp-db's single-writer is a process-LOCAL Mutex and
# cannot fence a second process. So an allow-list entry is honoured ONLY if the
# file actually acquires that flock; a "CLI" file that opens the DB WITHOUT the
# flock can run concurrently with serve, read a stale main-file head, and (if it
# then appends) fork the chain — the incident's primitive. The exemption must be
# EARNED by the flock, never granted on the filename.
is_flock_fenced() { grep -qE 'acquire_or_refuse|try_acquire' "$1"; }

remaining=0
worklist="$(mktemp)"; allowed_hits="$(mktemp)"; unfenced="$(mktemp)"; structhits="$(mktemp)"
while IFS= read -r f; do
  while IFS= read -r rec; do
    [[ -z "$rec" ]] && continue
    fname="$(cut -d: -f2 <<< "$rec")"
    key="$f:$fname"
    # D1 structural hits are ratcheted (CHECK N1 below), not enforced at zero —
    # turning the shape rule on surfaced 28 PRE-EXISTING forks. Allow-listed +
    # flock-fenced CLI one-shots fall through to the same exemption as before.
    if [[ "$rec" == *:structfork@* ]]; then
      if is_allowed "$key" && is_flock_fenced "$f"; then
        printf '%s:%s\n' "$f" "$rec" >> "$allowed_hits"; continue
      fi
      printf '%s|%s\n' "$f" "$fname" >> "$structhits"; continue
    fi
    if is_allowed "$key"; then
      if is_flock_fenced "$f"; then
        printf '%s:%s\n' "$f" "$rec" >> "$allowed_hits"; continue
      fi
      # Allow-listed but NOT flock-fenced → the exemption's justification is
      # absent → it is a live cross-process hazard, NOT an accepted one.
      printf '%s:%s\n' "$f" "$rec" >> "$unfenced"
      printf '%s:%s\n' "$f" "$rec" >> "$worklist"
      remaining=$((remaining + 1)); continue
    fi
    printf '%s:%s\n' "$f" "$rec" >> "$worklist"
    remaining=$((remaining + 1))
  done < <(bs_scan "$SCAN" "$f")
done < <(scope_files)

if ! bs_scan_ok; then
  rm -f "$worklist" "$allowed_hits" "$unfenced"
  echo
  echo "READ-FORK GATE: ✗ FAILED (the scanner failed mid-scan — a zero-fork verdict is not trustworthy)"
  exit 1
fi

if [[ -s "$unfenced" ]]; then
  echo "  ✗ ALLOW-LISTED BUT NOT FLOCK-FENCED (exemption VOID — these read audit fresh with NO"
  echo "    cross-process mutual exclusion against serve; add db_writer_lock::acquire_or_refuse"
  echo "    or migrate to a Handle read):"
  sort "$unfenced" | sed 's/^/      /'
fi
rm -f "$unfenced"

na="$(wc -l < "$allowed_hits" | tr -d ' ')"
echo "  ($na CLI one-shot read(s) allow-listed as coherent — separate process, flock-fenced.)"

# ── CHECK N1 — the D1 STRUCTURAL RATCHET (ALWAYS ENFORCED) ──
# The shape rule (fresh opener → handle read through, under ANY fn name) is what
# four shipped defects needed and none of them had. Switching it on found 28
# pre-existing forks — NINE of them live in-serve on `state.db_path`. Migrating
# those is product work with its own review; this gate does NOT pretend they are
# fixed. It freezes them and refuses ANY ADDITION, so a FIFTH instance of the class
# cannot ship the way the first four did.
#
# The baseline is an exact set, both directions — an added entry is a new fork, and
# a STALE entry (a fork that was fixed, or renamed) must be removed in the SAME
# change, so the file can never drift into a list of names that no longer mean
# anything. This is the ADR-0098 CHECK P2 frozen-fingerprint posture.
#
# NOT a coherence claim. `✓ 0 new` means "no fork was ADDED", never "the tree has no
# forks" — read the baseline for what is still open.
echo
echo "[CHECK N1] D1 structural read-fork ratchet — frozen baseline, additions REFUSED"
STRUCT_BASE="tools/adr0099_read_fork_structural_baseline.txt"
[[ -f "$STRUCT_BASE" ]] || { echo "  ✗ FAIL: baseline missing: $STRUCT_BASE"; rm -f "$worklist" "$allowed_hits" "$structhits"; exit 1; }
base_set="$(grep -vE '^\s*#' "$STRUCT_BASE" | sed '/^\s*$/d' | sort -u)"
now_set="$(sort -u "$structhits" 2>/dev/null)"
added="$(comm -13 <(printf '%s\n' "$base_set") <(printf '%s\n' "$now_set"))"
stale="$(comm -23 <(printf '%s\n' "$base_set") <(printf '%s\n' "$now_set"))"
rm -f "$structhits"
struct_bad=0
if [[ -n "$added" ]]; then
  echo "  ✗ NEW structural read-fork(s) — a fresh DB opener whose handle is read through,"
  echo "    in a function the baseline does not list. This is the shape that shipped four"
  echo "    times (#40/#41/#42/E1). Route the read through the shared Handle"
  echo "    (state.db.read()/.write() → try_clone), do NOT add it to the baseline:"
  sed 's/^/      + /' <<< "$added"
  struct_bad=1
fi
if [[ -n "$stale" ]]; then
  echo "  ✗ STALE baseline entr(ies) — listed but no longer detected (fixed? renamed?)."
  echo "    Remove them from $STRUCT_BASE in the same change, or the ratchet decays:"
  sed 's/^/      - /' <<< "$stale"
  struct_bad=1
fi
if [[ "$struct_bad" -ne 0 ]]; then
  rm -f "$worklist" "$allowed_hits"
  echo
  echo "READ-FORK GATE: ✗ FAILED (CHECK N1 — structural ratchet)"
  exit 1
fi
nb="$(grep -c . <<< "$base_set")"
echo "  ✓ 0 new — $nb known structural fork(s) held at the frozen baseline."
echo "    (NOT a coherence claim: $nb forks are still OPEN. See $STRUCT_BASE.)"
echo

if [[ "$remaining" -eq 0 ]]; then
  echo "✓ ZERO non-allow-listed in-serve audit read-forks — every in-serve audit reader is on the shared Handle."
  rm -f "$worklist" "$allowed_hits"
  exit 0
fi

echo "  $remaining in-serve audit read-fork(s) remain (the CHECK N worklist — read via the Handle):"
sort "$worklist" | sed 's/^/    /'
rm -f "$worklist" "$allowed_hits"
echo
if [[ "$enforce" == "1" ]]; then
  echo "READ-FORK GATE: ✗ FAILED — $remaining in-serve reader(s) must read the audit ledger through the shared Handle"
  echo "  (db.read()/db.write() → try_clone → Ledger::from_connection; NO fresh Ledger::open)."
  exit 1
fi
echo "READ-FORK GATE: (informational) — $remaining in-serve read-fork(s) to migrate; gate flips to ENFORCING (ENFORCE_READ_FORK=1) at zero."
exit 0
