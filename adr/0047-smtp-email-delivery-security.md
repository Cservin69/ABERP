# ADR-0047 — SMTP email delivery: security posture

**Status:** Accepted (PR-92, 2026-05-27)
**Supersedes:** none
**Related:** [ADR-0007 §Secrets / §Transport](0007-trust-and-security-axioms.md), [ADR-0009 §8](0009-invoice-state-machine-and-audit-trail.md), [ADR-0021 §A6 (rustls-everywhere)](0021-pre-code-consolidated-architecture.md), [ADR-0040 (multi-bank schema)](0040-multi-bank-account-schema.md), [ADR-0042 (notes-never-in-NAV-XML)](0042-invoice-notes-never-in-nav-xml.md)

## Context

ABERP must deliver issued invoices (and stornos / modifications) to
the buyer's email address as the **buyer-facing** half of the
product (the NAV submission is the regulatory compliance layer, not
the point). Per `[[aberp-notes-and-email]]`:

> "Invoicing is not for NAV but a regulatory supplement — we send
> invoices to buyers, and sending stornos where telling why we did
> it. That is why the SMTP is a must."

The SMTP subsystem is the **highest-risk surface in the app**: it
combines stored credentials, outbound network, customer PII in email
bodies, and partner-controlled fields that interpolate into email
headers. The session-92 brief explicitly demanded over-scrutiny:

> "Ervin is cybersec-paranoid and explicitly demanded over-scrutiny.
> Build with these from the start, and write a 'Security surfaces
> for adversarial review' section in the handoff enumerating them."

A dedicated adversarial security review (PR-93) is a hard gate
before SMTP is considered shippable. This ADR pins the security
posture so PR-92's build is auditable and PR-93's review has a
testable target.

## Decision

### §1 — TLS is mandatory; plaintext SMTP is NOT a configurable option

The closed-vocab `SmtpSecurity` enum (`apps/aberp/src/smtp_config.rs`)
has exactly two variants:

- `StartTls` — STARTTLS upgrade after the initial connection on port
  587 (conventional). The wire connection MUST upgrade to TLS before
  any AUTH dialogue; if STARTTLS negotiation fails, the send fails.
  Never falls back to plaintext.
- `Tls` — implicit TLS from byte zero on port 465 (conventional). No
  plaintext dialogue ever occurs.

There is NO `Plain` / `None` / `Plaintext` variant. The TOML reader
`SmtpSecurity::from_token` loud-fails on any unknown token (`"plain"`,
`"plaintext"`, `"None"`) so a future contributor cannot slip a
plaintext path in without ADR rework.

The lettre `AsyncSmtpTransport` is constructed via `relay(host)` or
`starttls_relay(host)` only. The build helper `build_transport`
(`apps/aberp/src/email_invoice.rs`) is grep-pinned by
`build_transport_source_has_no_plaintext_fallback` — the test reads
the source file and asserts no `Tls::None` / `unencrypted_localhost`
strings appear.

Certificate validation uses lettre's rustls-tls default trust
configuration (system root store via the rustls platform verifier
chain). No blind-accept / `dangerous_disable_certificate_validation`
construction path exists.

**Why this matters:** SMTP credentials submitted over plaintext are
the single fastest path to an account-compromise incident. Many
managed mailbox providers still accept plaintext AUTH PLAIN on port
25 / 587; defaulting to TLS-only makes plaintext send mathematically
unreachable from operator-typed config.

### §2 — Secrets in keychain, non-secrets in seller.toml; password NEVER on disk

Per the keychain/TOML split anchored by `[[aberp-tenant-management]]`:

- **Secret:** the SMTP password. Lives in the OS keychain under
  service `aberp.smtp.<tenant>` / item `smtp_password`. Read at send
  time, wrapped in `zeroize::Zeroizing<String>` so the buffer is
  overwritten on drop. NEVER written to disk, NEVER logged, NEVER
  carried back over the SPA's HTTPS wire.
- **Non-secrets:** `host`, `port`, `from_address`, `from_display_name`
  (optional), `username`, `security` (closed vocab), `attach_xml`
  (bool). Stored in `[seller.smtp]` section of
  `~/.aberp/<tenant>/seller.toml` via the same merge-not-replace
  pattern PR-72's bank section + PR-89's numbering section use
  (`smtp_config::merge_smtp_section`). The PR-75 lesson
  (`[[project_seller_toml_write_invariant]]`) is honored — writing
  the SMTP section preserves every other section verbatim.

The SPA's GET /api/smtp-config response includes a `passwordSet: bool`
probe of the keychain so the UI can render a "password is set"
indicator without ever round-tripping the password. PUT
/api/smtp-config accepts an OPTIONAL `password` field — blank /
absent leaves the existing keychain entry untouched, so the operator
can edit the host without re-typing the password.

The service-name fork `aberp.smtp.<tenant>` is distinct from the NAV
service name `aberp.nav.<tenant>` (pinned by
`smtp_service_name_does_not_collide_with_nav`); a tenant-level
keychain collision is structurally prevented.

### §3 — Email-header injection: CR/LF rejected on every header-bound field

The #1 SMTP injection risk: a partner-controlled field (buyer name,
buyer email, invoice number) interpolated into a header that allows
the attacker to inject an additional header (e.g., `Bcc:
attacker@evil.com`) by embedding `\r\n` in their input. Defence is
input validation at every seam:

- **Recipient address (`To:`):** validated by
  `email_invoice::validate_no_crlf` BEFORE being handed to lettre's
  `Address::new`. CR/LF in the address → `HeaderInjection` error.
- **Recipient display name (`To: Name <addr@…>`):** same CR/LF
  guard; the display name flows into `Mailbox::new` only after the
  guard passes.
- **From address + display name:** validated server-side at PUT
  /api/smtp-config write time (`SmtpConfig::validate`); validated
  again on every send (`send_invoice_email`). Belt-and-braces.
- **Subject line:** composed by `compose_subject(invoice_number)`
  from an internal template + the invoice number; the invoice
  number passes through `validate_no_crlf` because it traces back
  to the NAV invoice-number XSD charset (constrained to
  `[0-9A-Za-z\-/]` per ADR-0045), but defense-in-depth re-validates
  at the send seam.
- **Supplier legal name (body greeting):** validated for CR/LF;
  origin is `seller.toml`'s operator-typed legal name which passes
  through `setup_seller_info`'s own validator at write time.

Recipient validation is two-stage: the CR/LF guard (loud-fail on
control bytes) runs first, then lettre's RFC-5322 `Address::new`
parses the local-part + domain. A malformed address fails the second
stage; a CR/LF-injected address fails the first.

### §4 — Audit-ledger event records every send WITHOUT secrets

A new `EventKind::InvoiceEmailedSent` variant (PR-92, F12 four-edit
ritual applied) records every send attempt — both successful sends
and transport/TLS/auth/recipient-rejected failures. The payload
`InvoiceEmailedSentPayload` (`apps/aberp/src/audit_payloads.rs`)
carries:

- `invoice_id` — prefixed-ULID.
- `idempotency_key` — minted per attempt (ADR-0005 F8 carry-forward).
- `recipient` — the to-address actually used. Operator-visible by
  design (the partner table + the printed PDF already carry it).
- `subject` — the verbatim subject line sent.
- `outcome` — closed-vocab `"succeeded"` | `"failed"`.
- `error_class` — closed-vocab on failure (`"transport"` | `"tls"`
  | `"auth"` | `"recipient_rejected"` | `"compose"` | `"other"`);
  `None` on success.
- `error_detail` — operator-readable explanation, ALREADY-SCRUBBED
  of secrets at the
  `email_invoice::EmailSendError::scrubbed_detail` boundary.
- `auto: bool` — `true` for the post-issue auto-send;
  `false` for the operator-clicked manual send.
- `attached_xml: bool` — `true` iff the NAV XML rode alongside the
  PDF.

The payload **MUST NOT** carry:

- The SMTP password (lives in keychain only).
- The SMTP host or port (seller.toml has its own audit trail;
  smearing server identity across every email row is an
  unnecessary information leak in any ledger handover).
- The email body bytes.
- The PDF bytes or the operator's NAV credentials.

This invariant is pinned by `audit_payload_emailed_carries_no_secrets`
— the test serialises a sample payload and asserts the JSON byte
stream does NOT contain `password`, `credentials`, `host`, `port`,
`body`, or `pdf_bytes` substrings. Any future field addition that
violates the invariant fails the pin.

The `invoice.emailed_sent` storage string carries the `invoice.`
prefix so the per-invoice export bundle's (`ADR-0009 §8`)
`invoice.*` glob picks it up alongside every other lifecycle entry
— same silent-omission-failure-mode posture every prior PR's
prefix-pin test names.

### §5 — Default-on with per-invoice operator opt-out

Per `[[aberp-notes-and-email]]` ("default set ON but can be switched
off on operator button"):

- The SPA's IssueInvoice form renders a checkbox **defaulted to
  `true`** ("Email to buyer / Számla kiküldése a vevőnek"). The
  operator can flip it off PER INVOICE before issuing.
- The backend `IssueInvoiceRequest.email_buyer_on_issue` is
  `Option<bool>`; absent is treated as `true` server-side
  (defence-in-depth — a future composer regression that drops the
  field still produces the default-on behaviour).
- After a successful issue, if the toggle was on, the backend's
  `handle_issue_invoice` calls `send_invoice_email_route` with
  `SendTrigger::AutoOnIssue`. The audit-ledger entry is written
  regardless of outcome (success OR failure — never silently
  skipped).
- The operator can ALSO send (or resend) manually from
  InvoiceDetail's action bar via `POST /api/invoices/:id/email`
  with `SendTrigger::Manual`. The audit payload's `auto: bool`
  field distinguishes the two paths.

The auto-send outcome is echoed inline on the issue response
(`IssueInvoiceResponse.email`) so the SPA can surface the success
flash with the email recipient + outcome.

### §6 — Wrong-recipient guard: no fallback recipient

The send path looks up the buyer's contact email by joining the
invoice's customer tax_number (from the side-stored PR-47α
`input.json`) against the `partners` table's `contact_email` column.
If no partner matches, OR the partner has no email, OR the email is
blank, the send is REFUSED with `EmailSendError::MissingRecipient`.
No fallback "send to the operator instead" path exists — silent
fan-out to an unintended recipient is the worst-class buyer-comms
failure mode.

### §7 — No SSRF / arbitrary-host

The SMTP host comes EXCLUSIVELY from the operator-typed
`[seller.smtp]` config (bearer-authed PUT route, writes to disk).
NEVER from partner data, invoice data, or any wire-supplied input.
A future request to "use the buyer's SMTP host" would require ADR
rework.

### §8 — Attachment filename sanitization

The PDF / XML attachment filename is composed from
`invoice_<sanitised-number>.{pdf,xml}` where
`sanitize_invoice_number_for_filename` keeps ASCII alphanumeric +
`-` + `_` and replaces every other byte with `_`. This eliminates
path-traversal (`../`) and RFC-2047 header-injection risk via a
hostile invoice number. The NAV invoice-number XSD (ADR-0045)
already constrains the upstream charset to `[0-9A-Za-z\-/]`, so the
only filtered character on a well-formed number is `/` (replaced
with `_`).

### §9 — No credentials in logs

The send path is structured so the password never reaches a
`Display` / `tracing` / `Debug` boundary:

- Read from keychain as `Zeroizing<String>`.
- Passed by reference to `email_invoice::send_invoice_email`.
- Passed by clone (lettre's API takes `String`) into
  `Credentials::new` only — never logged.
- The send-path errors (`EmailSendError`) are constructed with
  scrubbed detail strings; the audit payload's `error_detail` field
  is `scrubbed_detail()` form, NOT the raw `Display` of the inner
  error.

A belt-and-braces `scrub_secrets` helper runs over every error
detail string and replaces tokens that follow auth-keywords
(`password`, `credentials`, `pw`, `secret`) with `<scrubbed>` — a
last-line defence against a future contributor accidentally
threading a credential through an `anyhow::Error` chain.

## Consequences

- The SMTP send path is byte-for-byte deterministic given operator
  config + the rendered PDF; the audit-ledger entry is byte-for-byte
  deterministic given the send outcome + idempotency key. Both are
  testable from unit pins without spinning a real SMTP server (the
  live roundtrip is Ervin's to test).
- Adding a third `SmtpSecurity` variant (e.g., DANE-validated TLS)
  is a deliberate widening here + a `from_token` arm + an
  operator-facing UI dropdown option — same closed-vocab discipline
  as the every other ABERP closed-vocab enum (ADR-0036 InvoiceState,
  ADR-0039 PaymentMethod, ADR-0045 invoice-number segments).
- The bilingual email body is a fixed template (Hungarian +
  English). No templating engine, no operator-edited body. A future
  PR could add per-tenant body customization; today the brief
  pinned "no templating engine beyond a simple bilingual body" as
  out of scope.
- Bulk send / scheduled send / queued retry are out of scope. The
  send path is synchronous-per-invoice; a transport failure surfaces
  to the operator via the SPA's inline outcome banner and the
  audit-ledger row.

## Adversarial review checklist (PR-93)

PR-93 must verify, at minimum:

1. **TLS mandatory**: prove no construction path in `email_invoice`
   produces a non-TLS lettre transport. Trace the binary.
2. **No plaintext SMTP**: confirm `SmtpSecurity::from_token` rejects
   every plaintext-spelling token (`plain`, `none`, `plaintext`,
   `Plain`, `PLAIN`, empty).
3. **No password in logs**: grep the source for `password` /
   `Credentials` usage and prove none reach `tracing::*!` or
   `format!` outside the keychain-write seam.
4. **No password in audit**: prove
   `InvoiceEmailedSentPayload`'s JSON output cannot carry the
   password under any input (the test
   `audit_payload_emailed_carries_no_secrets` is necessary but
   maybe not sufficient — review for any new field that might
   regress).
5. **Header injection**: fuzz `validate_no_crlf` against every
   header-bound field. Try CR-only, LF-only, CRLF, Unicode line
   separators (U+2028, U+2029), encoded variants.
6. **Recipient validation**: prove a malformed address never reaches
   lettre's transport.send.
7. **Wrong-recipient guard**: prove the send path has no fallback
   recipient seam.
8. **No SSRF**: prove the SMTP host never originates from
   partner / invoice / wire data.
9. **Attachment filename**: fuzz `sanitize_invoice_number_for_filename`
   for path-traversal escapes / Unicode normalisation tricks.
10. **TOML merge invariant**: prove writing `[seller.smtp]` cannot
    clobber the identity / bank / numbering sections (PR-75 lesson).
11. **Resend / replay**: review whether the operator-clicked
    "Email to buyer" button has any rate-limit / dedupe — today it
    does not, deliberately (the audit-ledger record IS the
    deduplication evidence; a future bulk-send PR may add a rate
    limit).
12. **Keychain failure modes**: review the typed
    `SmtpCredentialsError::Backend` path — does a locked keychain
    leak any sensitive info in its error message?

Any finding from PR-93 that names a specific defect MUST land as a
follow-up PR before the SMTP feature is considered shippable. This
ADR is the testable contract.

## Out of scope (named-deferred)

- DKIM / SPF / DMARC alignment — operator-config responsibility.
  ABERP sends via the operator's SMTP server; the alignment is
  the server's job.
- Bulk send — explicit non-goal for PR-92.
- Per-tenant body templating — explicit non-goal.
- Linux Secret Service + Windows Credential Manager — same
  cross-platform-keychain item ADR-0007 carries; today macOS-only.
- Email-receipt confirmation (DSN / read receipts) — not in scope.
- Rate-limiting on the manual send button — named-deferred.

## Pins (canonical)

Look for these test names; they are the load-bearing invariants:

- `aberp::smtp_credentials::tests::smtp_service_name_does_not_collide_with_nav`
- `aberp::smtp_config::tests::security_token_rejects_plaintext_variant`
- `aberp::smtp_config::tests::merge_smtp_section_preserves_other_sections`
- `aberp::smtp_config::tests::validate_rejects_crlf_in_from_address`
- `aberp::email_invoice::tests::build_transport_source_has_no_plaintext_fallback`
- `aberp::email_invoice::tests::sanitize_invoice_number_rejects_path_traversal`
- `aberp::email_invoice::tests::validate_no_crlf_rejects_lf`
- `aberp::audit_payloads::tests::audit_payload_emailed_carries_no_secrets`
- `aberp_audit_ledger::EventKind::tests::pr_92_emailed_sent_kind_uses_invoice_prefix`
- `aberp_audit_ledger::EventKind::tests::round_trip_for_every_variant`
  (catches the F12 four-edit ritual failures)
