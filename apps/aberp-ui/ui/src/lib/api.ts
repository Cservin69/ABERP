// Tauri command surface — the SPA's ONLY path to the backend.
//
// Per ADR-0021 §Part B, the wire protocol is HTTPS+JSON. The TLS
// termination + bearer-token attachment + fingerprint pinning all
// happen in Rust (see `apps/aberp-ui/src/commands.rs`). The SPA
// never sees the URL, the cert, or the token.
//
// Per ADR-0007 §"Tauri allow-list", the SPA is treated as
// semi-trusted. Every command here has a matching `#[tauri::command]`
// handler on the Rust side; the names MUST stay in sync. The Rust
// `tauri::generate_handler!` macro lists the four names in
// `lib.rs`'s `Builder::default()` chain.

import { invoke } from "@tauri-apps/api/core";

/** PR-44ε / session-53 — typed wire mirror for the `aberp_billing::Currency`
 * enum per ADR-0037 §3. Two variants today (HUF + EUR); third-currency
 * widening is named-deferred per ADR-0037 §5 (operator-signs-a-customer
 * trigger). Wire form is the `rename_all = "UPPERCASE"` ISO 4217 string
 * — matches `Currency::iso_code()` on the Rust side. Pinned by
 * `invoice_list_item_emits_currency` +
 * `invoice_detail_emits_currency_and_rate_metadata` on the Rust side;
 * TS reads the wire shape strictly via this typed union so a backend
 * drift surfaces at `npm run check`. */
export type Currency = "HUF" | "EUR";

/** Single invoice row — shape mirrors `serve::InvoiceListItem`. */
export interface InvoiceListItem {
  invoice_id: string;
  sequence_number: number;
  fiscal_year: number;
  state: InvoiceState;
  /** Units depend on `currency` per PR-44ε / session-53: for
   * `currency === "HUF"` this is whole forints (HUF has no sub-unit;
   * the `Huf` newtype stores it as `i64`). For `currency === "EUR"`
   * this is EUR cents (the issuance-path posture per PR-44γ stores
   * EUR amounts in the underlying `i64` as cents and re-uses the
   * `Huf` wrapper at the per-line layer until PR-44δ+1 lifts
   * `LineItem` to a typed-EUR shape). `null` while billing still has
   * the invoice as a draft (no totals persisted yet); the backend
   * serialises this as `null` from `Option<i64>`. The list-row
   * formatter in `format.ts` reads `currency` to pick HUF-vs-EUR
   * display. */
  total_gross: number | null;
  /** PR-31 / session-35 — chain-link affordance for list rows
   * (session-30-named Option M). `true` iff this invoice is the
   * base of at least one InvoiceStornoIssued or
   * InvoiceModificationIssued chain entry. The list-row renderer
   * surfaces a small `↘` badge next to the state chip when this
   * is true; the badge is non-interactive (the row click already
   * opens the detail modal). Pinned by
   * `list_invoices_emits_has_chain_children` on the Rust side; TS
   * reads the wire shape strictly via this typed field so a
   * backend drift surfaces at `npm run check`. */
  has_chain_children: boolean;
  /** PR-44ε / session-53 — currency on the list-row wire shape per
   * ADR-0037 §1.a + §3. The list-row formatter consumes this
   * field to pick the HUF-vs-EUR symbol + minor-unit interpretation
   * for `total_gross`; without it, an EUR invoice's cents would
   * render as forints (off by a factor of 100 + wrong symbol).
   * Pinned by `invoice_list_item_emits_currency` on the Rust side. */
  currency: Currency;
}

/** Possible derived states from `InvoiceTrace::derive_state` on the
 * backend. Kept in lockstep with that `&'static str` ladder per
 * ADR-0036 §2 — eleven labels, lifecycle-ordered. A state the
 * backend invents without a matching union member here renders as
 * the raw string but does not break the table; the `labelMeta`
 * helper in `./labels.ts` falls back to a muted "?" pill so the
 * silent miss is visible per CLAUDE.md rule 12. */
export type InvoiceState =
  | "Unknown"
  | "Ready"
  | "Pending"
  | "PendingNavExists"
  | "Submitted"
  | "Recovered"
  | "Finalized"
  | "Rejected"
  | "Storno"
  | "Amended"
  | "Abandoned";

/** One audit-ledger entry — shape mirrors `serve::AuditEntryView`. */
export interface AuditEntryView {
  seq: number;
  kind: string;
  actor: string;
  occurred_at: string;
  /** PR-26 / session-30 — chain-link affordance for the detail
   * modal. Non-null for `InvoiceStornoIssued` /
   * `InvoiceModificationIssued` entries (the typed payload's
   * `base_invoice_id` field per ADR-0023 / ADR-0024); `null` for
   * every other kind. `InvoiceDetail.svelte` renders the field as
   * a clickable navigation to the base invoice when present.
   * Pinned by `audit_view_of_emits_chain_base_invoice_id` on the
   * Rust side; TS reads the wire shape strictly via this typed
   * field so a backend drift surfaces at `npm run check`. */
  chain_base_invoice_id: string | null;
  /** PR-27 / session-31 — full typed payload as raw JSON
   * (whatever `audit_payloads::*` serialised). Rendered by
   * `InvoiceDetail.svelte` under a per-row expansion toggle as
   * pretty-printed JSON; the operator inspects every typed payload
   * field (chain digests, idempotency keys, NAV-emitted
   * timestamps, ack-status strings) without dumping the whole
   * bundle. `unknown` keeps the TS type honest — the shape varies
   * per `EventKind` and the renderer treats it as opaque JSON. A
   * malformed payload (which would indicate direct DB tampering)
   * serialises as `null` from the backend; the renderer prints
   * `null` rather than crashing the view. Pinned by
   * `audit_view_of_emits_typed_payload` on the Rust side. */
  payload: unknown;
}

/** PR-32 / session-36 — chain-children list entry. One per storno
 * / modification invoice issued against a base. The detail-modal
 * renderer lists these in a section between the meta-grid and the
 * audit-trail table; each `invoice_id` is a clickable affordance
 * that reuses the same `onNavigate` callback as the audit-row
 * chain-link button (PR-26). Pinned by
 * `invoice_detail_emits_chain_children` on the Rust side. */
export interface ChainChildView {
  kind: ChainChildKind;
  invoice_id: string;
  /** PR-41 / session-45 — per-base chain index allocated at issuance
   * time (`InvoiceStornoIssuedPayload.modification_index` /
   * `InvoiceModificationIssuedPayload.modification_index` on the
   * Rust side). Shared name space across both kinds: the next
   * storno or modification against the same base receives
   * `max(modification_index) + 1` per
   * `next_modification_index_in_tx` in `issue_storno.rs` /
   * `issue_modification.rs`. Operator-meaningful as the per-row
   * answer to "which entry in this base's chain?"; the
   * detail-modal renderer surfaces it as a leading `#N` glyph on
   * each chain-children row. Pinned by
   * `invoice_detail_emits_chain_children` on the Rust side; TS
   * reads the wire shape strictly via this typed field so a
   * backend drift surfaces at `npm run check`. */
  modification_index: number;
}

/** PR-32 / session-36 — typed kind discriminator for chain-children
 * rows. PascalCase wire mirror of the two terminal `InvoiceState`
 * labels (`Storno` / `Amended`); the SPA's `labels.ts` carries the
 * same labels at the state-chip layer, so a chain-children row
 * renders with the same affordance the operator already
 * recognises from the list-row chip.
 *
 * PR-37 / session-41 — tightened via `Extract<InvoiceState, ...>` so
 * the PR-34 `labelMeta(kind)` dispatch's `ChainChildKind ⊆ InvoiceState`
 * invariant is pinned at the type level. If a future ADR drops or
 * renames one of the two terminal labels in `InvoiceState`, this
 * alias degenerates (to `"Amended"`, `"Storno"`, or `never`) and
 * every consumer fails `npm run check` per CLAUDE.md rule 12 (fail
 * loud) rather than silently dispatching to the muted "?" fallback.
 * The runtime shape is byte-identical pre/post PR-37 — the Extract
 * evaluates to the same `"Storno" | "Amended"` union today; only the
 * type-level dependency on `InvoiceState` is new. */
export type ChainChildKind = Extract<InvoiceState, "Storno" | "Amended">;

/** PR-33 / session-37 — typed wire mirror for the four NAV v3.0
 * `processingResult` values (Option Q). Mirrors `serve::AckStatus`
 * under serde's `rename_all = "UPPERCASE"` so the wire form is the
 * verbatim NAV literal. Two intermediate values
 * (`RECEIVED`, `PROCESSING`) and two terminal (`SAVED`, `ABORTED`)
 * per ADR-0009 §2; the deprecated pre-v3.0 `DONE` value is NOT
 * represented — the NAV-transport inbound parser rejects it and the
 * audit-ledger never persists it. Pinned by
 * `ack_status_wire_shape_pins_uppercase_strings` on the Rust side;
 * TS reads the wire shape strictly via the
 * `last_ack_status: AckStatus | null` field on `InvoiceDetail` so a
 * backend drift surfaces at `npm run check`. */
export type AckStatus = "RECEIVED" | "PROCESSING" | "SAVED" | "ABORTED";

/** The single-invoice detail — shape mirrors
 * `serve::InvoiceDetailResponse`. */
export interface InvoiceDetail {
  invoice_id: string;
  sequence_number: number;
  fiscal_year: number;
  state: InvoiceState;
  total_gross: number | null;
  audit_entries: AuditEntryView[];
  /** PR-32 / session-36 — chain-children list (Option T). For an
   * invoice that is the BASE of at least one chain entry, this
   * array enumerates every storno / modification invoice issued
   * against it, in ledger-walk (i.e., issuance) order. Empty for
   * invoices with no chain children (NOT null — the backend
   * always emits a JSON array). The detail-modal renderer
   * conditionally renders the section only when the array is
   * non-empty. Pinned by `invoice_detail_emits_chain_children` on
   * the Rust side; TS reads the wire shape strictly so a backend
   * drift surfaces at `npm run check`. */
  chain_children: ChainChildView[];
  /** PR-33 / session-37 — latest NAV ack for this invoice (Option Q).
   * `null` when no `InvoiceAckStatus` audit entry has been written
   * yet (Draft / Pending lifecycle states) OR when a persisted
   * string fails to parse as one of the four NAV v3.0 values (the
   * audit-entries drill-down still surfaces the raw string via
   * `payload`, so no information is lost). The detail-modal
   * renderer surfaces the value as a meta-grid row next to State /
   * Total (gross). Pinned by `invoice_detail_emits_last_ack_status`
   * on the Rust side; TS reads the wire shape strictly via this
   * typed field so a backend drift surfaces at `npm run check`. */
  last_ack_status: AckStatus | null;
  /** PR-44ε / session-53 — currency on the detail wire shape per
   * ADR-0037 §1.a + §3. Same union as `InvoiceListItem.currency`.
   * The detail-modal renderer reads this field to pick the
   * HUF-vs-EUR `total_gross` formatter AND to gate the conditional
   * render of the four rate-metadata rows below. Pinned by
   * `invoice_detail_emits_currency_and_rate_metadata` on the Rust
   * side. */
  currency: Currency;
  /** PR-44ε / session-53 — MNB exchange rate per ADR-0037 §1.a +
   * §1.c (rate value) / C11 (precision). Decimal-as-string at
   * exactly 6 decimal places (`"405.230000"`); `null` iff
   * `currency === "HUF"`. The detail-modal renderer surfaces the
   * value as a meta-grid row only when non-null per the
   * conditional-render shape pinned by the SPA vitest. */
  exchange_rate: string | null;
  /** PR-44ε / session-53 — MNB source identifier per ADR-0037 §1.a
   * (printed-invoice field) + §2.a (literal `"MNB"`). `null` iff
   * `currency === "HUF"`. */
  exchange_rate_source: string | null;
  /** PR-44ε / session-53 — MNB rate publication date per ADR-0037
   * §1.a + §2.b (walk-back rule). ISO-8601 `YYYY-MM-DD`; `null`
   * iff `currency === "HUF"`. */
  exchange_rate_date: string | null;
  /** PR-44ε / session-53 — HUF-equivalent gross total per ADR-0037
   * §1.a + §1.c / C5. Whole forints (HUF has no sub-unit); `null`
   * iff `currency === "HUF"`. */
  huf_equivalent_total: number | null;
}

/** `GET /health` response — `serve::HealthResponse`. */
export interface HealthResponse {
  ok: boolean;
  binary_hash: string;
  nav_xsd_version: string;
}

export async function health(): Promise<HealthResponse> {
  return invoke<HealthResponse>("health");
}

export async function listInvoices(): Promise<InvoiceListItem[]> {
  return invoke<InvoiceListItem[]>("list_invoices");
}

export async function getInvoice(invoiceId: string): Promise<InvoiceDetail> {
  return invoke<InvoiceDetail>("get_invoice", { invoiceId });
}

export async function getAudit(invoiceId: string): Promise<AuditEntryView[]> {
  return invoke<AuditEntryView[]>("get_audit", { invoiceId });
}
