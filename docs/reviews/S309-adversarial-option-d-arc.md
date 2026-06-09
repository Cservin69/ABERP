# S309 — Adversarial review of the Option D arc (S305 ADR-0009 + S306 storefront email outbox + S307 ABERP poll daemon)

**Scope:** the architectural reversal that retired the Cloudflare Tunnel topology and shipped a polling-only email path in ~3 hours on 2026-06-09.

- **S305 (storefront, doc-only)** — `docs/adr/0009-storefront-as-queue-no-tunnel.md` (Accepted 2026-06-09). Supersedes ADR-0007 (storefront → ABERP push relay) and ADR-0008 (Cloudflare Tunnel topology, accepted 2026-06-08 night, reversed 2026-06-09 morning before its runbook was executed). Storefront main `21682ed`.
- **S306 (storefront, impl)** — `src/lib/server/email-outbox.ts` (persistence module), three sibling endpoints under `src/routes/api/internal/email-queue/`, rewired `src/lib/server/email.ts`, deprecated `src/lib/server/email-relay.ts`. Storefront main `955ec83`.
- **S307 (ABERP, impl)** — `apps/aberp/src/email_outbox_poll_daemon.rs` (new daemon, 1254 lines), 4 new EventKinds in `crates/audit-ledger/src/entry/event_kind.rs` (`quote.email_outbox_{fetched,claimed,sent,failed}`), 2 new payload types in `apps/aberp/src/audit_payloads.rs`, deprecation WARN on `handle_relay_send_email` in `serve.rs`. ABERP main `31bdb4d` — **shipped as `PROD_v2.27.10`**.

**Style:** doc-only review per `[[parallel-doc-sessions]]`. NO code changes — a sweep PR (S311 candidate) follows. Format mirrors `S296-adversarial-s286-s295-overnight.md`: 10 sections, 🔴/🟡/🟢 buckets, file:line evidence per finding, recommended fix, target session.

**Baseline gates inherited from the cuts:** ABERP cargo `2107 ok` at `31bdb4d` (+5 over the S297 baseline `2102`), vitest aberp `1079 ok` (steady since S297). Storefront vitest steady around `300+` ok. Both repos clean working trees at the start of this session.

**Disposition:** pushback-as-method per `[[pushback-as-method]]`. The Option D arc was Ervin's pushback against ADR-0008's vendor pick, taken seriously and turned into a documented architectural reversal rather than soft-peddled — and that reversal landed working code in three hours. Real win. But three hours is not enough time for a wedged-recovery story, a janitor, or a sweep across operator-discipline gaps. Where the new code lies about its retry semantics I name it; where the operator-test path silently regressed I name the trap.

---

## Executive summary

**🔴 5 critical · 🟡 14 medium · 🟢 10 confirmed-good**

*(F23 cargo-deny gap closed by `ed882e9` between session start and review completion — see §8.)*

The Option D pivot is architecturally clean: ABERP polls outbound only, no third party in the wire, the SaaS-migration story stays trivial (one URL change). The SPOC posture from S289 (`StorefrontCredentialHandle`) was reused without modification — the daemon hot-reloads bearer + URL on every cycle, no restart needed. The 4-EventKind F12 ritual fires cleanly. GDPR-safe audit payload (recipient_hash only, no plaintext). The supervisor mirrors S286 line-for-line.

**But three independent gaps stack between "green at gates" and "Ervin can run the prod pilot Wednesday":**

1. **F1 🔴 Stuck-`claimed/` is a code-design bug masked as an operator runbook.** `email_outbox_poll_daemon.rs:607` and `:632` write the comment *"the storefront entry stays in claimed/ and we'll observe it again"* — but `email-outbox.ts:218-243 listQueued` scans `queued/` only. Once an entry is in `claimed/`, the daemon never sees it again. `WritebackSentFailed` (SMTP succeeded, `/sent` POST errored) and `WritebackFailedErrored` (SMTP failed, `/failed` POST errored) are **permanent wedges**, not retryable transients. The walkthrough's section "Outbox claimed-but-stuck" (`end-to-end-auto-quote-test.md:493-497`) tells the operator to `sudo mv /data/email-outbox/claimed/<ulid>.json /data/email-outbox/queued/<ulid>.json` by hand — the canonical `[[trust-code-not-operator]]` anti-pattern. A daemon comment that promises a retry the daemon cannot perform is worse than no comment.

2. **F2 🔴 The integration test only covers the happy path.** `apps/aberp/tests/email_outbox_poll_full_cycle.rs:103` is named `s307_email_outbox_full_cycle_two_entries_succeed` — two entries, both succeed, no fake injects a writeback failure. None of the four `EntryOutcome` variants beyond `Sent` is exercised end-to-end. `WritebackSentFailed` and `WritebackFailedErrored` have NO test pin, which is exactly why F1 sailed through — a test that fakes a 500 on `/sent` would have immediately demonstrated that the next cycle never re-sees the entry. Cargo `2107 ok` is **passing on uncovered surface area**, not "fully tested".

3. **F3 🔴 The OUTBOX_DIR path is unset, unchecked, and inconsistent across three documents.** Three different absolute paths appear in the arc:
   - `email-outbox.ts:29` — `OUTBOX_DIR = process.env.ABERP_SITE_EMAIL_OUTBOX_DIR ?? './data/email-outbox'` (relative, process-CWD-dependent).
   - ADR-0009:63 + :147 — `/var/lib/aberp-site/email-outbox/`.
   - Walkthrough `:273,275,484,495,497` — `/data/email-outbox/`.
   `boot-checks.ts:32-43` validates `BODY_SIZE_LIMIT` and `ABERP_SITE_OPERATOR_EMAIL`; the outbox-dir env is **not boot-checked**. A Lightsail systemd unit that forgets to set `ABERP_SITE_EMAIL_OUTBOX_DIR` will silently write the queue under the SvelteKit process CWD (almost certainly `/srv/aberp-site/data/`, which is wiped on a fresh deploy). Mail goes to disk; first deploy rotation wipes the queue; no `claimed/` recovery either. Boot is green.

Three more critical, plus a pile of medium-severity drift to clean up before pilot day:

4. **F4 🔴 The walkthrough's audit-filter copy points at the OLD EventKind.** `end-to-end-auto-quote-test.md:489` reads *"ABERP → Audit ledger → filter on `email.relayed_storefront`"* — but S307 emits `quote.email_outbox_{fetched,claimed,sent,failed}`. The filter the operator is told to type returns ZERO rows; the operator concludes "ABERP didn't process the email" and starts manual recovery on entries that actually delivered fine. Same paragraph-block was rewritten by S306 for the queue path but left the legacy filter name in place.

5. **F5 🔴 The single-token rotation surface has TWO independent storage locations.** ADR-0009 §Authentication claims *"No new tokens introduced; no new secrets to rotate."* True for the **value**, false for the **storage**: the storefront reads `ABERP_SITE_ADMIN_TOKEN` from `process.env` (`auth.ts:8`); ABERP holds the same value in the macOS Keychain entry that feeds `StorefrontCredentialHandle` snapshot via `serve.rs:1969`. Rotating the token still requires the operator to (a) edit `/etc/aberp-site.env` on Lightsail, (b) restart `aberp-site`, AND (c) update the SPA → Quote Intake bearer field on the Mac. The PUT `/api/quote-intake/config` route hot-reloads ABERP-side, but no mechanism ties the two ends. Pilot-day failure mode: Ervin rotates the storefront env, restarts `aberp-site`, every ABERP poll cycle 401s — and the daemon emits no `EmailOutboxFetched` audit (only successful fetches do, per `event_kind.rs:1731-1734`), so the audit ledger shows silence rather than authentication-failure rows.

6. **F23 was the cargo-deny advisory gap (RUSTSEC-2025-0098/-0100 via tauri `urlpattern → unic-ucd-*`). CLOSED in flight by `ed882e9` (S303 follow-up) which added 17 RUSTSEC ids to `deny.toml::[advisories].ignore` with per-id rationale + `audit.toml` mirror + ci.yml `--ignore` flags.** Not introduced by Option D, fixed during this review's read window. See §8 for the residual concern about ADR ratification.

---

## 1. Queue atomicity + race conditions

### F1 🔴 Stuck-`claimed/` is the wedge bug; the daemon comment lies about retry semantics

- **Evidence:**
  - `apps/aberp/src/email_outbox_poll_daemon.rs:597-607`:
    ```rust
    // NO terminal audit fires — the storefront entry stays in
    // claimed/ and we'll observe it again. The duplicate-
    // send risk is acceptable per ADR-0009 Consequences §3.
    Ok(EntryOutcome::WritebackSentFailed)
    ```
  - Same comment + `WritebackFailedErrored` outcome at `:626-634`.
  - `src/lib/server/email-outbox.ts:218-243 listQueued`: `names = await readdir(stateDir('queued'))` — scans `queued/` only. Nothing in `email-outbox.ts` walks `claimed/`.
  - Walkthrough `:493-497` documents the bug as "v1 backlog": *"The storefront does NOT auto-recover claimed-but-stale entries in v1 (this is a documented ADR-0009 backlog item). Fix: move the file by hand."*
  - Integration test `tests/email_outbox_poll_full_cycle.rs:103` is happy-path only — `WritebackSentFailed` / `WritebackFailedErrored` are uncovered.
- **Why it matters:** ADR-0009 sells the polling architecture on the strength of "atomic rename atomicity is the load-bearing invariant." The atomicity is real for the `queued → claimed` rename. But `claimed → sent | failed` is a **separate** rename that depends on a writeback POST round-trip; if that POST errors after SMTP succeeded, the entry is wedged forever from the daemon's perspective. The customer got the email; the audit ledger never recorded a terminal `EmailOutboxSent`; the storefront's `claimed/` directory accumulates entries the operator must hand-walk per the walkthrough. This is the canonical `[[trust-code-not-operator]]` violation — Ervin's threat model rejected WireGuard maintenance; it should reject filesystem-rename-by-hand at least as strongly.
- **Recommended fix (composable, escalating):**
  1. **Storefront-side janitor.** Sweep `claimed/` entries older than N minutes back to `queued/`. Two lines of code, one cron entry. Closes the wedge for ALL crash classes (mid-send, mid-/sent-POST, mid-/failed-POST).
  2. **Or storefront-side `GET /api/internal/email-queue?state=claimed&claimed_before=<iso>`** — let ABERP's daemon poll for stuck entries on a slower cadence (60s) and re-process them. Keeps the recovery story in ABERP where the audit ledger lives.
  3. **Daemon-side `mark_sent` retry within the cycle.** If POST `/sent` errors, retry 3× with exponential backoff before declaring `WritebackSentFailed`. Closes the most common failure mode (network blip) without needing storefront-side recovery.
  4. **Until any of (1)-(3) lands, the daemon comment MUST be corrected** — *"we'll observe it again"* is false and misleading on a forensic walk. Replace with *"this entry is wedged; operator must hand-recover per the walkthrough."*
- **Target session:** **S311 sweep — must-fix before pilot.** Option (1) is the lowest-cost: ~20 lines in `email-outbox.ts` + ~5 in a cron. Option (3) is daemon-local but doesn't close the mid-cycle-crash class.

### F2 🟡 The since-cursor advances on `max(queued_at)` but the storefront filter is strict `<`

- **Evidence:**
  - Daemon `email_outbox_poll_daemon.rs:506,532` — `let max_queued_at: Option<String> = entries.iter().map(|e| e.queued_at.clone()).max();` then `last_seen_iso: max_queued_at.or(since)`.
  - Storefront `email-outbox.ts:238` — `if (opts.since && entry.queued_at < opts.since) continue;` (strict less-than).
  - Combined effect: an entry with `queued_at == cursor` is **not** filtered out by the storefront. The daemon would re-see it on the next cycle if it were still in `queued/`. Because the daemon ALSO claimed it last cycle, `listQueued` returns nothing (the file moved to `claimed/`). So no harm.
- **Edge case:** if the daemon's first cycle returned 50 entries AND >50 entries share the same `queued_at` ms (extreme burst — e.g. an enqueue loop in a future bulk-import), the 51st entry's `queued_at` equals the cursor. Next cycle: storefront returns it (filter is `<`). Daemon claims it. Fine, just one cycle late. NOT a bug — but the daemon at `:112` clamps `QUEUE_LIST_LIMIT = 50` and the storefront at `:219` clamps `limit ?? 200`. The cursor advances correctly across this seam ONLY because the storefront uses `<` not `<=`. A future reviewer "fixing" the storefront filter to `<=` would silently lose entries on same-ms boundaries.
- **Recommended fix:** add a test pin in `email-outbox.spec.ts` that two entries with the same `queued_at` are both returned across two listQueued calls when the cursor advances past one of them. Future-proofs the strict-`<` invariant.
- **Target session:** **S311 cleanup — non-blocking.**

### F3 🟢 The single-flight `claim → send → writeback` chain is serialized by storefront-side rename atomicity

- **Evidence:** POSIX rename within mountpoint is atomic; `email-outbox.ts:264 rename(from, to)` either succeeds or ENOENTs. Two concurrent claims of the same entry race exactly one rename to success and the other gets `null → 409`. Daemon code at `:734-737` handles both 409 and 404 cleanly.
- **Confirmed good.** This part of the design works as advertised.

### F4 🟡 Empty fsync window on enqueue

- **Evidence:** `email-outbox.ts:160-161` — `await writeFile(tmp, JSON.stringify(entry, null, 2), 'utf8'); await rename(tmp, entryPath(state, entry.id));` — Node.js `fs.writeFile` does NOT fsync. Power loss after writeFile but before fsync-of-rename leaves an empty/partial `tmp` file and no entry in `queued/`. The storefront returned 200 to the customer before the data hit the disk; the customer thinks the email is queued but it isn't.
- **Why it matters less than it sounds:** Lightsail's host filesystem is on an EBS-backed volume; power loss on the host is rare and the EBS journal protects ordered writes per-file. The exposure is "host crash AND rename-was-pending" — a narrow window. The fix is `fsync(tmp)` before rename and `fsync(dir)` after — two `fdatasync` calls per enqueue. Latency cost: ~2 ms on EBS gp3.
- **Recommended fix:** `import { fsync } from 'node:fs/promises'`, open the tmp file with a handle, `await fh.sync()` before rename. Document the trade-off in `email-outbox.ts` header.
- **Target session:** **S311 cleanup — non-blocking unless a prod power-loss event motivates it.**

---

## 2. Idempotency on retry

### F5 🟡 Successful SMTP + failed `/sent` POST → email sent, audit silent

- **Evidence:** `email_outbox_poll_daemon.rs:592-608` — `handle_one_entry_inner` for the `Ok(()) → Err(e)` case (SMTP succeeded, writeback errored) goes:
  ```rust
  Err(e) => {
      tracing::warn!(... "will retry");
      Ok(EntryOutcome::WritebackSentFailed)
  }
  ```
  No call to `emit_sent_audit`. The customer received the email; the audit ledger has nothing.
- **Why it matters:** the `EmailOutboxSent` audit is gated on writeback success, not SMTP success. Per ADR-0009 *"writeback is the terminal signal."* But the SMTP send DID happen — and `[[trust-code-not-operator]]` says the audit should reflect the highest-confidence ground truth, which is "the SMTP transport returned Ok." Coupled with F1's wedge bug, this is the actual customer-impact failure mode: customer gets the email, walks the operator into "the audit shows nothing," and the operator has no signal that the entry succeeded.
- **Recommended fix:** emit a **separate** `EmailOutboxSmtpOk` audit row immediately after `transport.send().await? Ok(())`, BEFORE attempting the writeback POST. Either: a fifth EventKind (`EmailOutboxSmtpOk`) for forensic-only "we know SMTP took it"; or fold the audit into `EmailOutboxClaimed`'s outcome string as a fourth value `"smtp_sent_writeback_pending"`. The latter is less F12-disruptive.
- **Target session:** **S311 sweep — pilot-pre-day fix.**

### F6 🟢 Storefront `markSent` idempotent on replay

- **Evidence:** `email-outbox.ts:293-294` — `const alreadySent = await readEntryFromState('sent', id); if (alreadySent) return alreadySent;`. First-writer-wins on `audit_id`. Test pin at `email-outbox.spec.ts:215-225`.
- **Confirmed good.** The replay semantics are sound; it's just that F1 makes the replay impossible from the daemon side.

### F7 🟡 Daemon's `WritebackSentFailed` retry loop relies on F1's fix

- **Evidence:** see F1. The comment claims retry will happen on the next cycle, but no code path realises that retry.
- **Why it matters:** every finding in section 2 that depends on the daemon "retrying next cycle" is fictitious until F1 is fixed. The two findings co-block.
- **Recommended fix:** dependent on F1.
- **Target session:** **S311 sweep — pilot-blocking, follows F1.**

---

## 3. Email-outbox failure paths

### F8 🟡 Daemon takes ONE shot per claim; failure → terminal

- **Evidence:** `email_outbox_poll_daemon.rs:610-624` — SMTP `Err(send_err)` → `mark_failed` immediately, no in-cycle retry. Documented at `event_kind.rs:1791-1797`: *"v1 ships no SMTP retry within a cycle — one attempt then terminal-fail per `[[trust-code-not-operator]]`."*
- **Why it matters less than F1:** this is a **deliberate** design choice (per the EventKind docstring) — don't mask delivery failures behind background retry, surface them to the operator. The trade-off is reasonable for Ervin's customer flow (single-digit emails/day). The risk is a transient SMTP 4xx ("greylisting", "temp DNS failure") that would have resolved on a second attempt — instead becomes a `failed/` row and an operator-discipline retry.
- **Recommended fix:** distinguish transient (4xx, connection-refused) from permanent (5.x.x SMTP-perm, address-syntax-invalid) at `classify_send_error` and auto-retry transients up to N times within a cycle. The `classify_send_error` at `:671-680` currently only distinguishes by error-message prefix ("compose:", "writeback:") not by SMTP status class. A surgical fix would add a `TransientSmtp` classification and a 30s-backoff in-cycle retry.
- **Target session:** **S312+ deferred — non-blocking for pilot; surfaces if Ervin sees greylisting on prod SMTP.**

### F9 🟡 Operator cannot retry a `failed/` entry from the SPA

- **Evidence:** `email-outbox.ts` has no `retryFailed(id)` function. The walkthrough `:497` documents the operator-discipline workaround: *"move the file by hand"*. The SPA panel surfaces `last_error_detail` (`email_outbox_poll_daemon.rs:349-351`) but not per-entry failed list.
- **Why it matters:** failed entries are operator-actionable but the only action is a shell command on the Lightsail box. For Ervin running this solo, that's tolerable; for a future operator, it's a slipping-discipline trap.
- **Recommended fix:** add a fifth endpoint `POST /api/internal/email-queue/{id}/retry` that renames `failed/<id>.json → queued/<id>.json` (bumping `attempt_n`). Add a "Failed entries" tab in the ABERP SPA that lists them and posts to it.
- **Target session:** **S312+ deferred — non-blocking; ship if a pilot-day transient failure happens.**

### F10 🟢 `mark_failed` itself errors after SMTP failed → daemon logs `WritebackFailedErrored`

- **Evidence:** `email_outbox_poll_daemon.rs:626-635` handles the double-failure case. Same wedge as F1 (entry stays in `claimed/` forever), but at least no false audit fires.
- **Confirmed good in posture, wedge depends on F1.**

---

## 4. Audit trail completeness

### F11 🟢 `EmailOutboxEntryAuditPayload` carries no plaintext recipients

- **Evidence:** `audit_payloads.rs:2810-2847` — only `recipient_hash` (SHA-256), `subject`, `byte_size`, `submitter`, `queue_row_id`. Test pin at `audit_payloads.rs:5337-5370` (`email_outbox_entry_payload_carries_no_recipient_plaintext`).
- **Confirmed good.** GDPR posture mirrors S281's `EmailRelayAuditPayload`.

### F12 🟡 EXACT same EventKind family `quote.*` for outbox emails, BUT S281's `email_relay_*` uses `email.*` prefix

- **Evidence:** `event_kind.rs:1741-1750`:
  > *"`quote.*` prefix family — same family as the pricing-pipeline kinds … explicitly named this prefix despite the surface being email-shaped (the email-relay strand uses `email.*` instead — this is a deliberate split because the outbox flow is part of the auto-quoting pipeline whereas the S281 relay is sister-service push from any surface)."*
- **Why it matters:** a forensic walker doing `grep '^email\.'` on the ledger will MISS the outbox events entirely. The split was deliberate per the brief, but it leaks operational complexity: there are now TWO email-event families, and the operator has to know which prefix to query for what. The walkthrough drift in F4 is the first symptom: a doc that says `filter on email.relayed_storefront` was written against the OLD family and not updated when the NEW family landed under `quote.*`.
- **Recommended fix:** either (a) emit a `quote.* → email.*` alias row alongside each outbox event so both prefixes work, or (b) document the split in the ABERP audit-ledger reference doc with a worked example showing which prefix surfaces what.
- **Target session:** **S311 doc-only — walkthrough fix per F4 covers the operator-facing surface.**

### F13 🟡 `EmailOutboxFetched` fires only on SUCCESSFUL GET; cycle errors land in SPA status only

- **Evidence:** `event_kind.rs:1731-1734` — *"A cycle that errored on the GET does NOT fire this event — only successful fetches land here so the audit row is a positive 'cycle completed' signal."*
- **Why it matters:** under F5's "daemon silently 401s" failure mode, the audit ledger shows ZERO `EmailOutboxFetched` rows during the downtime — indistinguishable from "daemon was never spawned" or "no quotes came in." The SPA `last_error_detail` field has the info, but the SPA gets wiped on tab refresh; the audit ledger is the durable record.
- **Recommended fix:** emit `EmailOutboxFetched` with `fetched_count: -1` (or a new `EmailOutboxFetchError` EventKind) so the cycle attempt is visible in the audit trail. Forensic walks should be able to answer "did the daemon try to poll during window X" without consulting volatile SPA state.
- **Target session:** **S311 sweep — non-blocking but pre-pilot if F5 isn't fully fixed.**

### F14 🟢 Deprecation WARN on `handle_relay_send_email` does fire (in non-dev mode)

- **Evidence:** `serve.rs:18795-18808` — `if !dev_mode { tracing::warn!("DEPRECATED POST /api/internal/send-email received in non-dev mode; …"); }`. Local-dev path stays silent.
- **Confirmed good** — the warning fires in prod; an operator tailing logs will see it. The only critique would be "an operator who doesn't tail logs won't notice," but the deprecation is informational, not actionable.

---

## 5. Storefront-side disk usage

### F15 🔴 `ABERP_SITE_EMAIL_OUTBOX_DIR` is unset, unchecked, and the default is process-CWD-relative

- **Evidence:**
  - `email-outbox.ts:29` — `const OUTBOX_DIR = process.env.ABERP_SITE_EMAIL_OUTBOX_DIR ?? './data/email-outbox';`. Relative path; resolved at module load via `pathResolve(OUTBOX_DIR)`.
  - `boot-checks.ts:32-80` validates `BODY_SIZE_LIMIT` and `ABERP_SITE_OPERATOR_EMAIL`. `ABERP_SITE_EMAIL_OUTBOX_DIR` is NOT checked.
  - ADR-0009:63 + :147 — `/var/lib/aberp-site/email-outbox/`.
  - Walkthrough `:273,275,484-487,495-497` — `/data/email-outbox/`.
  - Three different paths in three documents; code default doesn't match either.
- **Why it matters:** if the Lightsail systemd unit forgets to set the env var, the queue writes to `<process-cwd>/data/email-outbox/`. For an adapter-node SvelteKit deploy, the process CWD is typically `/srv/aberp-site/` or wherever the deploy script runs from — which means the queue lives on the application volume and gets wiped on every fresh deploy. Sent/failed entries from before the deploy are gone; in-flight `queued/` entries are gone; the customer's "submission received" email never goes out. Boot is green.
- **Recommended fix:**
  1. `boot-checks.ts` checks `ABERP_SITE_EMAIL_OUTBOX_DIR` resolves to an absolute path AND that the path exists and is writable.
  2. Pick ONE canonical path (`/var/lib/aberp-site/email-outbox/`) and align both ADR-0009 and the walkthrough to it.
  3. Update walkthrough's `sudo ls -1 /data/email-outbox/…` lines to the canonical path.
- **Target session:** **S311 sweep — must-fix before any prod-prod pilot.**

### F16 🟡 No size cap on attachments on the storefront-side enqueue

- **Evidence:** `email.ts:478-486` reads the priced PDF from disk and base64-encodes it; no cap. The S281 `email_relay::MAX_RELAY_BODY_BYTES = 25 MB` cap doesn't apply because the enqueue path doesn't go through the relay surface anymore. A pathological PDF render (engine bug, 500MB output) would create a 670MB queue entry on disk; the daemon would then attempt to GET it (timeout at 30s per `HTTP_TIMEOUT_SECS`), and on every cycle fetch ALL pending entries including the 670MB one.
- **Why it matters less than F15:** the PDF renderer in `aberp-quote-pdf` is bounded by sensible 5-axis-quote shape; 670MB pathologies are unlikely. But the ABERP-side relay path had explicit `MAX_ATTACHMENT_BYTES = 20 MB` (`email_relay.rs:64`) — losing that cap when migrating to the outbox path is a regression.
- **Recommended fix:** in `email.ts:enqueueSafe`, gate on attachment-total-bytes ≤ 25 MB and log+drop on overflow. Or in `email-outbox.ts:enqueueEmail`, fail-loud with a thrown error if the entry serialises above N bytes.
- **Target session:** **S311 sweep — non-blocking, defensive.**

### F17 🟡 No janitor for `sent/` and `failed/` entries — ADR-0009 OQ #2 deferred

- **Evidence:** ADR-0009 §"Open questions" #2: *"What's the cleanup policy for `sent/` and `failed/` entries? Suggest a nightly cron … Backlog for a later session — not blocking S306/S307."* No code lands the janitor.
- **Why it matters less than F15:** Ervin's volume is ~5 quotes/day × ~3 emails each = 15 entries/day. After a year: ~5500 entries × ~10KB avg + a few PDF entries × ~5MB. Total maybe 1 GB on disk. Lightsail's storage scales fine. Forensic walks via `ls + cat` still work.
- **Recommended fix:** the nightly-cron-tarball-after-90-days suggested in OQ #2 is fine; lift it out of "open question" into a tracked S313+ work item.
- **Target session:** **S313 backlog — non-blocking for first 6 months of pilot.**

---

## 6. Authentication consistency

### F18 🔴 Token rotation requires THREE coordinated edits, no boot-check ties them

- See F5 above. Restating with file-line evidence:
  - Storefront `auth.ts:8`: `const token = env.ABERP_SITE_ADMIN_TOKEN;` — read from process env at every request.
  - ABERP `serve.rs:1969` builds the daemon's `StorefrontCredentialHandle` from the boot-time keychain entry; PUT `/api/quote-intake/config` hot-reloads via `:set`.
  - ABERP poll daemon `email_outbox_poll_daemon.rs:482` snapshots the credential per cycle — bearer rotates on next cycle.
  - Storefront-side rotation requires editing `/etc/aberp-site.env` AND `systemctl restart aberp-site` (per ADR-0009 prose).
  - There is no boot-check OR runtime probe that the two ends agree. Disagreement surfaces as 401 on every daemon poll; per F13 the audit ledger is silent on this; per F1+F7 wedge bugs are not retryable.
- **Recommended fix:** add a boot-time `GET /api/health` probe on the daemon spawn that verifies the bearer authenticates. Or a one-line `tracing::error!` ladder when 3 consecutive cycles return 401. Either makes the rotation gap loud instead of silent.
- **Target session:** **S311 sweep — must-fix before pilot.**

### F19 🟢 Storefront PUT `/api/quote-intake/config` hot-reload propagates to BOTH catalogue-push AND email-outbox daemons

- **Evidence:** both daemons consume `StorefrontCredentialHandle::snapshot()` per cycle. S289 closed the catalogue-push gap; S307 used the same SPOC by construction. `serve.rs:1969` passes the same `Arc<StorefrontCredentialHandle>` clone.
- **Confirmed good.** SPOC posture survived a second consumer cleanly.

### F20 🟢 All 4 new storefront endpoints use the same `requireAdminAuth` gate

- **Evidence:** `+server.ts` files all call `requireAdminAuth(request);` as the first line. No bypass.
- **Confirmed good.**

---

## 7. ADR-0009 architecture audit — residual `email-relay.ts` invocations

### F21 🟡 `email-relay.ts` is dead code on the call graph, but the spec suite still runs against it AND keeps the deprecated envs alive

- **Evidence:**
  - `grep sendEmailViaABERP` returns hits ONLY in `email-relay.ts` itself, `email-relay.spec.ts`, and accept-flow spec env-cleanup (`accept.spec.ts:39-40`). No live caller.
  - `email-relay.ts:75-77` still reads `ABERP_INTERNAL_BASE_URL` and `ABERP_EMAIL_RELAY_TOKEN`; both env vars are alive solely to keep the spec passing.
  - `email-relay.spec.ts:15-19` sets both envs in the test fixture.
  - `email.test.ts:134` has an explicit pin *"no longer requires ABERP_INTERNAL_BASE_URL or ABERP_EMAIL_RELAY_TOKEN (ADR-0009)"* — good intent.
- **Why it matters:** the deprecation window keeps two unused envs in the deployment surface. ADR-0009's §Negative says *"Leave the secret provisioned in Ervin's keychain for local-dev / manual API testing; remove from `/etc/aberp-site.env` in a follow-up cleanup."* That follow-up cleanup is not scheduled. A future operator rotating Lightsail env vars sees `ABERP_EMAIL_RELAY_TOKEN` and `ABERP_INTERNAL_BASE_URL` and doesn't know whether they're load-bearing.
- **Recommended fix:** delete `email-relay.ts` and `email-relay.spec.ts` along with the env vars from `/etc/aberp-site.env`. The local-dev push path is the deprecated `handle_relay_send_email` on the ABERP side; the storefront half doesn't need to keep a client for it. Track as **S314 deprecation removal** — after the prod pilot confirms the outbox path.
- **Target session:** **S314 deferred — explicit deprecation removal, schedule after pilot success.**

### F22 🟢 ADR-0009 cleanly references the SaaS-migration trajectory

- **Evidence:** ADR-0009:171 — *"`[[aberp-saas-migration]]` — when ABERP moves to a real long-lived server, the polling endpoint URL is the only env var to change. The architecture transports cleanly; no third-party dependency to migrate."*
- **Confirmed good.** Per pushback-as-method §10 question — this is exactly what makes Option D superior to the discarded Cloudflare Tunnel option for a SaaS future.

---

## 8. S307 cargo-deny CI failure

### F23 🟢 RUSTSEC-2025-0098 + RUSTSEC-2025-0100 — CLOSED by `ed882e9` (S303 follow-up) during this review

- **Evidence:** `ed882e9` ("S303: CI follow-up — cargo-deny + cargo-audit policy sweep") landed on `main` between session start and review completion. Three files: `deny.toml` (17 RUSTSEC ignores with per-id rationale), new `audit.toml` (mirror), `.github/workflows/ci.yml` (explicit `--ignore` flags). Validated locally per commit message: `cargo deny check` four-green; `cargo audit --deny warnings <flags>` clean.
- **Confirmed good** — Option (B) of my pre-resolution recommended fix landed (ignore-with-rationale rather than tauri version bump).
- **Residual:** the commit message itself flags *"ADR-0007's allow-list (MIT/Apache-2.0/BSD-3-Clause/MPL-2.0) is materially wider in practice; this commit captures the reality. ADR-0007 should be amended in a separate doc-only cut to ratify."* That ratification is its own work item, NOT part of the Option D arc, NOT pilot-blocking.

### F32 🟡 Per the S303 follow-up: cargo-deny `[advisories].ignore` list now needs lifecycle hygiene

- **Evidence:** `deny.toml` ignores 17 RUSTSEC ids each with a per-id rationale comment. Three clusters per the commit msg: (1) tauri-utils → urlpattern → unic-* (5 ids); (2) Tauri 2 Linux GTK3 backend (9 ids); (3) older ecosystem transitives (4 ids). Each ignore should be revisited when its upstream gets a fix.
- **Why it matters:** an ignore list grows but doesn't shrink without explicit re-checks. A revival check is needed before each prod cut so resolved-upstream advisories are removed from the list. Per Ervin's `[[trust-code-not-operator]]`: this should be a cron-driven check, not a calendar reminder.
- **Recommended fix:** add a CI step `cargo deny check --hide-inclusion-graph 2>&1 | grep 'has no remaining transitive dependencies'` after each weekly schedule run — if an ignored RUSTSEC stops being reachable, the operator gets a "you can remove this ignore" signal. Out of scope for S311.
- **Target session:** **S313+ deferred — cleanup automation.**

---

## 9. Walkthrough freshness

### F24 🔴 Walkthrough's audit-filter copy points at the wrong EventKind family

- See F4 above. Same finding, restated for §9.

### F25 🟡 "Outbox claimed-but-stuck" section codifies the F1 wedge as a runbook

- **Evidence:** `end-to-end-auto-quote-test.md:493-497` — entire section title and recommended-fix paragraph. Reads as "this is a backlog item; here's the manual workaround."
- **Why it matters:** documents F1 as expected behavior rather than a bug. Ervin reading this cold tomorrow infers that running `sudo mv` is part of normal operation. Per `[[trust-code-not-operator]]` and pushback-as-method, this is exactly the operator-discipline gap that should NOT be papered over with a runbook step.
- **Recommended fix:** delete the section after F1 lands. Until then, prefix with **🚧 BUG — to be removed when S311 lands the janitor.**
- **Target session:** **S311 — coupled to F1.**

### F26 🟢 LD-1..LD-5 manual path was promoted to "Advanced only" per S296 F4

- **Evidence:** `:39-41` — *"Use this collapsed sequence ONLY if the launcher above is unavailable (e.g. running ABERP on Linux, or stepping through the seams for debugging). Skip otherwise."* The primary path is `./run/dev-test.sh`.
- **Confirmed good.** S296's F4 was closed by S297 / PR-272 wiring.

### F27 🟡 Walkthrough still names `ABERP_INTERNAL_BASE_URL` + `ABERP_EMAIL_RELAY_TOKEN` in LD-3 + LD-4

- **Evidence:** `:61-90`. These envs are NOT load-bearing for the new outbox path; LD-3's keychain mint is for the deprecated `aberp.email_relay.test.email_relay_token` — i.e. testing the local-dev-only deprecated push surface, not the new pull surface.
- **Why it matters:** an operator following LD-3+LD-4 cold today is exercising the deprecated path, not the new path. They'll get a green light from the old surface and miss the wedge bug in F1.
- **Recommended fix:** add an LD-3b / LD-4b block for the new pull path (the `ABERP_SITE_EMAIL_OUTBOX_DIR` env, the daemon kill-switch env, the storefront-side `ABERP_SITE_ADMIN_TOKEN`). Mark LD-3/LD-4 as "deprecated push path testing only."
- **Target session:** **S311 sweep doc-only.**

### F28 🟢 Steps 2/5/7 explicitly call out the queue path

- **Evidence:** `:269,273-275,484` — each customer-email-arrival step lists the storefront queue + ABERP poll consumption as the validation chain.
- **Confirmed good.** The customer-facing trail-through-the-walkthrough is internally consistent with ADR-0009.

---

## 10. Forward-compatibility with SaaS migration

### F29 🟢 ADR-0009 architecture transports cleanly to a public-IP ABERP

- See F22.

### F30 🟢 Polling-only is the correct fit for the email path; mid-cycle push needs would be a Stage 3 question

- **Evidence:** ADR-0009 §Consequences §Negative — *"Latency floor. Poll cadence (5s for email, 60s for new quotes) means worst-case ~5s delay between 'ABERP renders email' and 'customer receives email,' ~60s between 'customer submits' and 'ABERP starts pricing.' Vs. push-based ~2s under Cloudflare Tunnel. For indicative quotes this is fine."*
- **Confirmed good for current scope.** A future `[[aberp-stage3-manufacturing]]` shop-floor adapter pattern with sub-second tool/robot events would need a different architecture (long-lived bidirectional WebSocket or a per-machine local agent), but ADR-0009 is the right cut for the auto-quote arc.

### F31 🟡 Daemon boot-delay (30 s) means first-cycle email after restart waits up to 35 s

- **Evidence:** `email_outbox_poll_daemon.rs:451-454`:
  ```rust
  tokio::select! {
      _ = cancel.cancelled() => return,
      _ = tokio::time::sleep(Duration::from_secs(30)) => {}
  }
  ```
- **Why it matters:** ADR-0009 sells "5s round-trip from form submit to mailbox." But the first email after an ABERP restart (Ervin opens his Mac lid in the morning) waits 30s before the first poll. Total latency for the first morning email: ≥35s. Acceptable for indicative quotes; misleading if the ADR's "5s" gets quoted in customer-facing copy.
- **Recommended fix:** either reduce boot delay to 5s (match the cadence) or document the cold-start floor in ADR-0009 §Negative.
- **Target session:** **S311 cleanup — non-blocking, minor.**

---

## Pilot-test feasibility

**Verdict: Pilot is feasible THIS WEEK with TWO must-fix items first. Pilot is NOT feasible today.**

The Option D architecture works. The single happy-path integration test is green. The audit ledger gets the right EventKinds. The daemon hot-reloads bearer + URL on the same SPOC handle the catalogue-push daemon already uses, so the pre-existing ops surface didn't bifurcate.

**But the wedged-claim bug (F1) plus the boot-check gap on the outbox dir (F15) are the two findings that turn "green CI" into "Ervin runs the pilot Tuesday and discovers his queue is on a deploy-volatile path while one email got stuck in claimed/ overnight."** Both are ~20-line fixes.

The walkthrough audit-filter drift (F4) is a 1-line fix and should land in the same PR. The token-rotation gap (F18) is one boot-time HEAD-probe, ~10 lines. The integration-test gap (F2) is one new `#[tokio::test]` that injects a writeback-fail fake.

**S311 sweep candidate (pilot-blocking) — order of operations:**
1. F1 storefront-side janitor (sweep stuck `claimed/` back to `queued/`) — ~30 LoC.
2. F15 `ABERP_SITE_EMAIL_OUTBOX_DIR` boot-check + path canonicalisation across ADR-0009 + walkthrough — ~30 LoC + 3 doc edits.
3. F4 walkthrough EventKind family rename (`email.relayed_storefront` → `quote.email_outbox_sent`) — 1 line.
4. F18 boot-time bearer-verify probe — ~10 LoC.
5. F2 + F5 + F7 integration-test for `WritebackSentFailed` + `WritebackFailedErrored` — ~80 LoC test file.
6. F13 emit `EmailOutboxFetched` (or a sibling EventKind) on failed cycles — ~5 LoC.

Approximate sweep size: ~150 LoC of code + 3 doc edits + 1 test file. Tractable in one session.

**S312+ deferred (non-blocking but flag-worthy):** F8 transient-vs-permanent SMTP retry; F9 operator-retry UI for failed entries; F16 attachment-size cap; F17 sent/failed janitor; F21 `email-relay.ts` deprecation removal; F23 cargo-deny advisory sweep; F27 walkthrough LD-3b/LD-4b for the new pull path; F31 boot delay.

**🚫 NOT pilot-feasible right now:**
- The walkthrough wedged-claim section (F25) is a runbook for an unfixed bug. Telling Ervin "if it gets stuck, `sudo mv`" is the kind of operator-discipline Option D was designed to eliminate.
- Boot is green on a mis-configured Lightsail (F15) because `boot-checks.ts` doesn't see the missing env. The first deploy after pilot day silently wipes the queue.

Fix F1 + F15 + F4 in S311; the pilot is good to go. Without those three, Wednesday's prod-prod test is a coin flip with two of the failure modes hand-recoverable but invisible from the audit ledger.

---

## References

- `[[trust-code-not-operator]]` — F1, F15, F25 are direct violations.
- `[[parallel-doc-sessions]]` — this doc-only session piggybacks on ABERP main directly per the established precedent.
- `[[pushback-as-method]]` — Ervin's reversal of ADR-0008 → ADR-0009 IS the canonical example of this principle in action; this review pushes back on the speed at which Option D shipped.
- `[[aberp-smtp-spoc]]` — preserved by ADR-0009 (storefront enqueues, ABERP SMTPs). ✓
- `[[post-issue-async]]` — preserved by `email.ts:enqueueSafe` swallowing enqueue failures. ✓
- `[[aberp-saas-migration]]` — F22 + F29 confirm clean trajectory. ✓
- ADR-0009 — the subject of this review.
- S296 — prior review format template.
- S307 / PR-276 / `PROD_v2.27.10` — the ABERP-side commit under review.
- `955ec83` (storefront `main`) — the storefront-side commit under review.
