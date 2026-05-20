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

/** Single invoice row — shape mirrors `serve::InvoiceListItem`. */
export interface InvoiceListItem {
  invoice_id: string;
  sequence_number: number;
  fiscal_year: number;
  state: InvoiceState;
  /** `null` while billing still has the invoice as a draft (no totals
   * persisted yet); the backend serialises this as `null` from
   * `Option<i64>`. Units: Hungarian forint, minor units (so always
   * an integer; HUF has no sub-unit and our `Huf` newtype stores it
   * as `i64`). */
  total_gross: number | null;
}

/** Possible derived states from `InvoiceTrace::derive_state` on the
 * backend. Kept in lockstep with that `&'static str` ladder; a state
 * the backend invents without a matching enum here renders as the
 * raw string but does not break the table. */
export type InvoiceState =
  | "Unknown"
  | "Ready"
  | "Submitted"
  | "Finalized"
  | "Rejected"
  | "Abandoned";

/** One audit-ledger entry — shape mirrors `serve::AuditEntryView`. */
export interface AuditEntryView {
  seq: number;
  kind: string;
  actor: string;
  occurred_at: string;
}

/** The single-invoice detail — shape mirrors
 * `serve::InvoiceDetailResponse`. */
export interface InvoiceDetail {
  invoice_id: string;
  sequence_number: number;
  fiscal_year: number;
  state: InvoiceState;
  total_gross: number | null;
  audit_entries: AuditEntryView[];
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
