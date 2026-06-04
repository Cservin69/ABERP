// S238 / PR-232 — Workshop demo-mode mock payload.
//
// Returns a synthetic `WorkshopDashboard` matching the real Tauri
// `get_workshop_dashboard` response shape, so `Workshop.svelte`
// renders it without knowing the source. The numbers are tuned to
// a "Q2 mid-day production" feel — not zeroed out, not overloaded,
// no red flares. Twelve to twenty recent-activity entries spread
// over the last 90 minutes give the timeline enough scroll to
// look alive without the operator having to invent a story.
//
// Timestamps are computed relative to a caller-supplied `now`
// (defaulting to `new Date()`), so the mock ages forward naturally
// while the page is open — `X perccel ezelőtt` stays believable
// across a multi-minute tour.
//
// Partner / WO / product names are deliberately neutral Hungarian-
// industry-sounding strings; the brief lists the canonical set.

import type {
  AdapterStatusSnapshot,
  RecentActivityEntry,
  WorkshopDashboard,
} from "./api";

const MIN_MS = 60 * 1000;

/** Build a mock `WorkshopDashboard`. The shape MUST match the real
 *  response — `workshop-mock-data.test.ts` pins this against the
 *  exported types so a backend widening is caught at vitest time
 *  rather than at render time. */
export function getMockDashboard(
  now: Date = new Date(),
): WorkshopDashboard {
  const todayDate = formatIsoDate(now);
  return {
    work_orders: {
      created: 4,
      released: 6,
      in_progress: 5,
      on_hold: 2,
      completed: 1,
      cancelled: 0,
    },
    low_stock_products: { count: 3 },
    qa: {
      pending: 6,
      passed: 4,
      failed: 0,
      reworking: 1,
      disposed: 0,
    },
    dispatch: {
      by_state: { drafted: 2, shipped: 5, cancelled: 0 },
      eligible_work_orders: 4,
      shipped_today: 3,
    },
    today: {
      date: todayDate,
      issued_count_huf: 6,
      issued_count_eur: 2,
      // 4,287,400 HUF gross — six small-to-medium invoices.
      gross_revenue_huf_minor: 428_740_000,
      // 12,450 EUR gross — two EUR invoices for the export side.
      gross_revenue_eur_minor: 1_245_000,
    },
    recent_activity: buildRecentActivity(now),
    adapters: buildAdapters(),
    snapshot_at_iso8601: now.toISOString(),
  };
}

// ── Recent activity ─────────────────────────────────────────────

/** Activity entries — 18 events spanning the last ~88 minutes.
 *  Order: newest first, matching the real payload's ORDER BY desc.
 *  EventKind strings mix `system.` prefixed (post-S177 convention)
 *  and bare names, so `fmtEventKind` is exercised in both modes. */
function buildRecentActivity(now: Date): RecentActivityEntry[] {
  const base = now.getTime();
  // [offsetMinutes, kind] — list is in chronological order
  // (oldest first) so editors can read it as a timeline; we reverse
  // at the end for newest-first output.
  const script: Array<[number, string]> = [
    [88, "system.work_order_created"],
    [82, "system.work_order_released"],
    [76, "system.work_order_released"],
    [71, "InvoiceDraftCreated"],
    [65, "system.qa_check_passed"],
    [60, "system.qa_check_passed"],
    [54, "system.dispatch_drafted"],
    [49, "system.work_order_in_progress"],
    [44, "InvoiceIssued"],
    [38, "system.qa_check_passed"],
    [33, "system.dispatch_shipped"],
    [28, "system.work_order_completed"],
    [24, "InvoiceDraftCreated"],
    [19, "system.qa_check_passed"],
    [15, "system.dispatch_shipped"],
    [11, "InvoiceIssued"],
    [6, "system.qa_check_reworking"],
    [2, "system.dispatch_shipped"],
  ];
  const out: RecentActivityEntry[] = [];
  // Reverse so newest is first. `seq` mirrors the audit-ledger
  // monotonic counter — seq is highest for the newest entry.
  for (let i = script.length - 1; i >= 0; i--) {
    const [offsetMin, kind] = script[i];
    const t = new Date(base - offsetMin * MIN_MS);
    out.push({
      id: `mock-${1000 + i}`,
      kind,
      at_iso8601: t.toISOString(),
      seq: 1000 + i,
    });
  }
  return out;
}

// ── Adapters ────────────────────────────────────────────────────

/** Three MES adapters — all `enabled`. The barcode scanner is the
 *  one whose rotating "Last scan" messages feed the demo-mode
 *  Workshop.svelte ticker. The rotation itself lives in
 *  `Workshop.svelte` because it needs a Svelte effect; this list
 *  just supplies the static adapter metadata. */
function buildAdapters(): AdapterStatusSnapshot[] {
  return [
    {
      name: "barcode-scanner-01",
      status: "enabled",
      kind: "barcode",
      host: "192.168.42.21",
      port: 4001,
    },
    {
      name: "mes-printer-bay-A",
      status: "enabled",
      kind: "label_printer",
      host: "192.168.42.22",
      port: 9100,
    },
    {
      name: "scale-shipping-01",
      status: "enabled",
      kind: "weight_scale",
      host: "192.168.42.23",
      port: 4003,
    },
  ];
}

// ── Scan-message ticker (demo mode polish) ──────────────────────

/** Fake "incoming scan" messages cycled by the demo-mode Workshop
 *  page on a ~3-5s rotation. The values reference plausible WO and
 *  part numbers from the brief's neutral-Hungarian-industry name
 *  set; the seconds-ago suffix is rendered by the component so the
 *  list itself stays time-independent (and trivially pinnable). */
export const MOCK_SCAN_MESSAGES: readonly string[] = [
  "WO-2026-00428 — Manifold T4",
  "PART-MFLD-T4 ×12",
  "WO-2026-00431 — Tartó 240mm",
  "PART-BRKT-240 ×24",
  "WO-2026-00429 — Burkolat 12-A",
];

// ── Spotlight rotation list (tile highlight cycle) ──────────────

/** `data-testid` keys for the tiles the demo-mode spotlight rotates
 *  through. Keeping this list here (rather than inlining it in
 *  `Workshop.svelte`) means a layout refactor doesn't drift from
 *  the rotation script. */
export const MOCK_SPOTLIGHT_TILES: readonly string[] = [
  "tile-work-orders",
  "tile-qa",
  "tile-dispatch",
  "tile-adapters",
  "tile-low-stock",
  "tile-today",
  "tile-recent-activity",
];

// ── Helpers ─────────────────────────────────────────────────────

/** YYYY-MM-DD, Budapest-local-ish (uses the host's local date).
 *  Matches the real `TodayPanel.date` shape (a bare ISO date, not
 *  a full timestamp). */
function formatIsoDate(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}
