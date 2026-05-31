// PR-193 / session-193 — CSV export helpers for the three list
// screens (Invoices, Partners, Products). Pure utility module: no
// DOM access in `escapeCsvField` / `composeCsv` so vitest can pin
// the wire shape exhaustively; the only DOM-touching surface is
// `downloadCsv` (the Blob-anchor click pattern the existing
// InvoiceList PDF-download path already uses).
//
// Format: UTF-8 with BOM (Excel reads `Á É Ő` correctly only when
// the leading `﻿` is present), RFC-4180 quoting, CRLF line
// endings. Tier-4 "invisible excellence" affordance: an operator
// who wants the screen in their bookkeeping spreadsheet gets one
// click → file in Downloads.
//
// Note: only the three load-bearing exports are surfaced. No
// per-column formatter abstraction (CLAUDE.md rule 2 — premature):
// each list call site builds its own row array inline and reuses
// the existing `format.ts` formatters for currency / dates.

const BOM = "﻿";
const CRLF = "\r\n";

/** Escape one CSV cell per RFC-4180.
 *
 * - `null` / `undefined` → empty string (no quotes).
 * - `number` / `boolean` → bare `String(value)` (no quotes; no
 *   special chars possible).
 * - `string` → quoted iff it contains `,`, `"`, `\r`, or `\n`;
 *   internal `"` doubled to `""` per RFC-4180.
 * - Everything else → coerce via `String(value)` then string path.
 *
 * Empty strings render as an empty cell (no quotes) — the same
 * shape Excel emits on Save As CSV.
 */
export function escapeCsvField(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  const s = typeof value === "string" ? value : String(value);
  if (s.length === 0) return "";
  const needsQuoting =
    s.includes(",") || s.includes('"') || s.includes("\r") || s.includes("\n");
  if (!needsQuoting) return s;
  return `"${s.replace(/"/g, '""')}"`;
}

/** Compose a complete CSV document.
 *
 * Prepends the UTF-8 BOM (Excel's Hungarian-accent compat); each
 * record terminates with CRLF (including the last row — RFC-4180
 * §2.2 permits either; the explicit trailing CRLF avoids
 * application-level "last row missing newline" warnings).
 */
export function composeCsv(headers: string[], rows: unknown[][]): string {
  const lines: string[] = [];
  lines.push(headers.map(escapeCsvField).join(","));
  for (const row of rows) {
    lines.push(row.map(escapeCsvField).join(","));
  }
  return BOM + lines.join(CRLF) + CRLF;
}

/** Trigger a browser download of `csv` as `filename`.
 *
 * Mirrors the synthetic-anchor pattern `InvoiceList.svelte`'s
 * `triggerRowDownload` uses for PDFs — Tauri's webview honours
 * `download` on a same-origin Blob URL so the file lands in the
 * operator's Downloads folder without a backend route.
 *
 * The MIME type is `text/csv;charset=utf-8` so an OS that probes
 * the file (macOS Finder preview, Windows Explorer) reads it as
 * CSV-with-UTF-8 rather than guessing latin-1.
 */
export function downloadCsv(filename: string, csv: string): void {
  const blob = new Blob([csv], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Compose a `YYYYMMDD-HHMMSS` timestamp string for a CSV filename.
 *
 * Uses the operator's local clock (the SPA renders the list against
 * the local timezone anyway — a UTC stamp would diverge from what
 * the operator just saw on screen). Padded fields so chronological
 * sort on the filename matches chronological order.
 */
export function csvFilenameTimestamp(now: Date = new Date()): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${now.getFullYear()}` +
    `${pad(now.getMonth() + 1)}` +
    `${pad(now.getDate())}` +
    `-` +
    `${pad(now.getHours())}` +
    `${pad(now.getMinutes())}` +
    `${pad(now.getSeconds())}`
  );
}
