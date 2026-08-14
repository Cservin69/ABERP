//! Financial-statistics aggregation backend for the SPA Statistics page.
//!
//! S225 / PR-221 — the FIRST read-only multi-table aggregator. Produces a
//! single JSON snapshot the SPA renders as a dashboard of revenue,
//! expenses, VAT, receivables, payables, DSO, aging, hygiene, and
//! period-over-period deltas. The data sources are the three invoice
//! tables (`invoice` + `invoice_line` for outgoing native, `ap_invoice`
//! for incoming, `restored_invoice` for NAV-as-DR rows) plus the audit
//! ledger (state derivation + payment records + storno chain links).
//!
//! # Architecture choices
//!
//!   - **No new audit kinds.** Reading the dashboard is a pure view; no
//!     state transitions, no audit-ledger writes. The financial figures
//!     are derivable from existing state and must remain so per
//!     CLAUDE.md rule 12 (fail loud — silent ledger writes from a read
//!     endpoint would corrupt the operator-twin model).
//!   - **One audit-ledger walk** per request, producing a `TraceMap`
//!     keyed by outgoing invoice id with the minimal fields the
//!     aggregator needs (state classification, storno-self flag,
//!     payment record, ack status). Reuses the same payload-typed-decode
//!     posture `serve::list_invoices` takes.
//!   - **Per-line aggregation** for the line-level VAT-rate breakdown, folded
//!     in Rust through the *filing's own* functions
//!     (`aberp_billing::domain::invoice::{line_net_total, line_vat_amount}`),
//!     which are what `nav_xml::write_summary` sums. So the ÁFA figures this
//!     dashboard shows are the figures NAV was sent — see
//!     [`fold_outgoing_lines`].
//!
//!     ADR-0108 M-2 changed this, and the change moves published numbers.
//!     Until 2026-08-01 the breakdown summed the *unrounded*
//!     `quantity × unit_price` products in SQL, rounded once half-even, and
//!     derived VAT as `round_half_even(group_net × bp / 10_000)` — documented
//!     at the time as a deliberate "management view, approximate within
//!     rounding tolerance". It was approximate in a specific and unhelpful
//!     direction: on two 27% lines of 50 Ft it printed 27 Ft of VAT where the
//!     filing carried 26. A management view that disagrees with the filing is
//!     a reconciliation problem, not a simplification, so the report now ties
//!     to the filing exactly. No arithmetic on money happens in SQL (§3.4).
//!   - **Closed-vocab date basis** (`teljesites` | `issued`) per
//!     `[[aberp-invoice-dates]]`. The default `teljesites` (delivery date)
//!     is the regulatory anchor for monthly bevallás per HU VAT law;
//!     `issued` (issue date) is offered as a secondary cash-flow lens.
//!   - **Per-currency parallel totals** for HUF + EUR. No FX aggregation
//!     in v1 — flagged as v2.2.1 deferred in the meta block on the wire.
//!   - **Storno sign-flip in code.** Storno child rows have POSITIVE
//!     line amounts in `invoice_line` (negation lives in the NAV XML
//!     emit path per ADR-0049). Aggregation flips the sign at the
//!     trace lookup so the dashboard's revenue figure subtracts storno
//!     reversals, matching the SPA list view's display rule (S156).
//!
//! # Deferred for v2.2.1
//!
//!   - FX aggregation (`all-in-HUF-at-MNB-rate` tertiary column).
//!   - HIPA (Helyi Iparűzési Adó) base — needs operator categorization
//!     of which line items are "material/subcontractor"; ADR-pending.
//!   - KIVA / KATA threshold logic — the running YTD revenue is shown,
//!     but no threshold limits are encoded (regime-dependent).
//!   - AAM / reverse-charge / EU-0 VAT-rate sub-buckets — the schema
//!     does NOT distinguish them today (all are `0%` in `invoice_line`).
//!     Parsing the NAV XML to recover the tag is its own follow-on PR.
//!   - Per-VAT-rate breakdown for restored/incoming invoices —
//!     restored_invoice and ap_invoice are digest-only (no line items).

use std::collections::{BTreeMap, HashMap, HashSet};

use aberp_audit_ledger::{BinaryHash, Entry, EventKind, Ledger, TenantId};
use aberp_billing::domain::invoice::{line_net_total, line_vat_amount};
use aberp_billing::domain::money::Huf;
use aberp_billing::domain::vat_rate_kind::VatRateKind;
use anyhow::{anyhow, Context, Result};
use duckdb::{params, Connection};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use time::macros::format_description;
use time::{Date, Month, OffsetDateTime};

use crate::audit_payloads;

// ──────────────────────────────────────────────────────────────────────
// Public request / response shapes.
// ──────────────────────────────────────────────────────────────────────

/// Inputs to [`compute_financial_report`] after parsing the HTTP query
/// string. The route layer parses URL parameters into this typed shape
/// per CLAUDE.md rule 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRequest {
    pub period: PeriodKind,
    pub date_basis: DateBasis,
    /// "Today" anchor for aging / cashflow / running-YTD calculations.
    /// Always callable with `today_local()`; tests pass a fixed date.
    pub today: Date,
    /// S262 / PR-251 — number of rows the top-customers / top-vendors
    /// lists return. Operator-configurable from the SPA (`?top_n=`),
    /// defaulting to 10. Clamped at the route layer to a sane range so a
    /// hand-typed `?top_n=100000` cannot force an unbounded sort-and-emit.
    pub top_n: usize,
}

/// Closed-vocab period selector. Default `Month(YYYY, MM)` per the
/// monthly bevallás cadence. The `Custom { from, to }` arm carries
/// inclusive ISO dates; `All` skips the date filter entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodKind {
    Month(i32, u8),
    Quarter(i32, u8),
    Year(i32),
    Custom { from: Date, to: Date },
    All,
}

/// Date axis for period filtering. `Teljesites` (delivery date with
/// fallback to issue date) is the regulatory anchor for VAT-month
/// assignment per `[[aberp-invoice-dates]]`. `Issued` (issue date) is
/// the cash-flow lens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateBasis {
    Teljesites,
    Issued,
}

impl DateBasis {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            DateBasis::Teljesites => "teljesites",
            DateBasis::Issued => "issued",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "teljesites" => Some(DateBasis::Teljesites),
            "issued" => Some(DateBasis::Issued),
            _ => None,
        }
    }
}

/// Single JSON snapshot returned by `GET /api/reports/financial`. Every
/// field is deterministic from the inputs (period + date_basis + db
/// state + audit ledger) so two requests against the same state produce
/// identical bytes (modulo `today` floating each invocation, which the
/// SPA disclosures via the `period.today` echo).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FinancialReport {
    pub period: PeriodMeta,
    pub revenue: CurrencyAggregate,
    pub expenses: CurrencyAggregate,
    pub gross_profit: CurrencyPair,
    pub vat_collected: CurrencyAggregate,
    pub vat_paid: CurrencyAggregate,
    pub vat_to_pay: CurrencyPair,
    pub receivables: CurrencyAggregate,
    pub payables: CurrencyAggregate,
    /// S262 / PR-251 — currency split of NATIVE outgoing revenue,
    /// expressed in a common HUF unit so HUF and EUR are comparable on
    /// one stacked bar.
    pub currency_split: CurrencySplitPanel,
    pub receivables_aging: AgingPanel,
    pub payables_aging: AgingPanel,
    pub dso_days: DsoPanel,
    pub cashflow_forward: CashflowPanel,
    pub vat_breakdown_outgoing: Vec<VatRateBreakdownEntry>,
    pub top_customers: Vec<TopEntry>,
    pub top_vendors: Vec<TopEntry>,
    pub hygiene: HygienePanel,
    pub deltas: PeriodDeltas,
    pub annual_running: AnnualRunningPanel,
    pub deferred_notes: Vec<String>,
    /// Integrity signal for THIS run of the aggregator — see
    /// [`LedgerDiagnostics`]. Non-zero means the figures above are
    /// possibly incomplete.
    #[serde(default)]
    pub ledger_diagnostics: LedgerDiagnostics,
}

/// Per-run integrity diagnostics for the financial report.
///
/// Two independent signals, both of the same family — a silent drop made
/// loud: entries the audit-ledger walk ([`walk_ledger`]) could not decode,
/// and outstanding invoices whose payment deadline the aging pass could
/// not read ([`aging_placement`]).
///
/// The first, in detail:
///
/// The walk decodes every audit entry's JSON payload to derive per-invoice
/// state (ack status, payment, storno chain links). Before this fix, a
/// payload that failed to decode was dropped on the floor: the `if let
/// Ok(..)` / `.ok()?` arms in [`ReportTrace::merge`],
/// [`extract_chain_link_local`] and [`extract_invoice_id_local`] each fell
/// through to "nothing happened". A malformed `InvoicePaymentRecorded`
/// payload therefore made an invoice look UNPAID (inflating receivables,
/// dropping its DSO sample, and possibly counting it past-deadline); a
/// malformed `InvoiceAckStatus` payload could demote a SAVED invoice to
/// `PendingDraft` and take it out of revenue + VAT-collected entirely; a
/// malformed storno chain link left the base uncancelled. All of it
/// silently — the operator saw a clean number that was simply wrong.
///
/// The conservative posture (CLAUDE.md rule 12 — fail loud) is NOT to
/// abort the whole dashboard on one bad row: a single corrupt entry must
/// not blank the operator's entire financial screen, and every valid row's
/// arithmetic is unchanged. Instead the drop is made VISIBLE and
/// ATTRIBUTABLE — a `tracing::error!` per entry carrying its id, seq, kind
/// and the decode error, plus this machine-countable signal on the wire so
/// a caller can flag the figures as possibly-incomplete rather than
/// present them as authoritative.
///
/// An entry is counted AT MOST ONCE even when several decode attempts fail
/// on it (a payload that is not JSON at all fails both the id extraction
/// and the chain-link decode): the count answers "how many entries could
/// not be read", not "how many decode attempts failed".
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct LedgerDiagnostics {
    /// Number of audit entries whose payload the report walk could not
    /// decode, and which are therefore NOT reflected in any figure above.
    /// Exact and uncapped.
    pub unparseable_entries: u64,
    /// Audit-entry ids (`aud_<ULID>`) of those entries, so the operator
    /// can go find them. Capped at [`MAX_UNPARSEABLE_ENTRY_IDS`] — the
    /// count above stays exact, so `unparseable_entries >
    /// unparseable_entry_ids.len()` simply means "and more".
    pub unparseable_entry_ids: Vec<String>,
    /// Otherwise-outstanding invoices — receivable OR payable — with NO
    /// recorded `payment_deadline` (the column is NULL, or holds a string
    /// that will not parse), which are therefore treated as SETTLED and
    /// excluded from outstanding entirely. Exact and uncapped. See
    /// [`aging_placement`].
    ///
    /// Unlike [`Self::unparseable_entries`] this does NOT mean a figure is
    /// missing or unreadable. It is the size of a deliberate exclusion:
    /// these are legacy invoices imported from NAV, issued and settled by
    /// the prior system, and the operator's ruling is that they are all
    /// paid. None of them is in the receivables / payables total, in any
    /// aging bucket, or in the past-deadline hygiene counters.
    ///
    /// It is published because an exclusion nobody can see is how a real
    /// unpaid invoice disappears. Read alongside the aggregate
    /// `tracing::warn!` raised in `build_financial_report`.
    #[serde(default)]
    pub aging_settled_undated: u64,
    /// Ids of those invoices, sorted, so support can check the exclusion
    /// against the book. Capped at [`MAX_UNPARSEABLE_ENTRY_IDS`] on the
    /// same "count exact, ids are a starting point" contract as
    /// [`Self::unparseable_entry_ids`].
    ///
    /// MACHINE-READABLE ONLY — the SPA deliberately does not render this
    /// list. NAV-synced payables carry no deadline at all, so on a real
    /// book it would be a permanent wall of ids on the dashboard. It stays
    /// on the wire for support and debugging; the operator-facing surface
    /// is the per-side count below.
    #[serde(default)]
    pub aging_settled_undated_invoice_ids: Vec<String>,
    /// The [`Self::aging_settled_undated`] total split by side, so each
    /// aging panel can footnote its OWN excluded rows. Rendering the
    /// combined figure under both panels would double-report it.
    ///
    /// `aging_settled_undated_receivables + aging_settled_undated_payables
    /// == aging_settled_undated`.
    #[serde(default)]
    pub aging_settled_undated_receivables: u64,
    #[serde(default)]
    pub aging_settled_undated_payables: u64,
}

/// Cap on [`LedgerDiagnostics::unparseable_entry_ids`]. A systemically
/// corrupt ledger must not balloon the report's JSON payload; the count is
/// the machine-countable signal, the ids are the operator's starting
/// point.
const MAX_UNPARSEABLE_ENTRY_IDS: usize = 50;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PeriodMeta {
    pub kind: String,
    pub label: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub date_basis: String,
    pub today: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrencyAggregate {
    pub huf: AmountAggregate,
    pub eur: AmountAggregate,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct AmountAggregate {
    pub gross_minor: i64,
    pub net_minor: i64,
    pub vat_minor: i64,
    pub count: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrencyPair {
    pub huf_minor: i64,
    pub eur_minor: i64,
}

/// S262 / PR-251 — currency split of native outgoing revenue.
///
/// HUF and EUR revenue live in different units (forints vs EUR cents), so
/// a raw side-by-side bar would be meaningless (HUF dwarfs EUR ~400×). To
/// make the split comparable, the EUR portion is converted to HUF at the
/// **snapshot MNB rate stamped on each invoice at issuance** (the
/// `huf_equivalent_total` column, ADR-0037 §1.c) — NOT today's rate. The
/// SPA renders `huf_minor` + `eur_as_huf_minor` as one stacked bar and
/// discloses the native EUR figure separately.
///
/// Basis: ISSUED native outgoing invoices (the `invoice` table only —
/// restored/AP digest rows have no snapshot rate). `huf_minor` reuses the
/// storno-adjusted native-revenue aggregate; `eur_as_huf_minor` sums
/// `huf_equivalent_total` on an issued basis (EUR storno reversals are not
/// sign-flipped here — a rare-case v1 approximation noted on the tile).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrencySplitPanel {
    /// Native HUF revenue gross, in forints (storno-adjusted).
    pub huf_minor: i64,
    pub huf_count: u64,
    /// Native EUR revenue gross, in EUR cents (storno-adjusted) — shown
    /// for disclosure beside the converted figure.
    pub eur_native_minor: i64,
    pub eur_count: u64,
    /// EUR revenue converted to HUF at each invoice's snapshot rate, in
    /// forints (issued basis). The comparable EUR contribution to the bar.
    pub eur_as_huf_minor: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct AgingPanel {
    pub current: AmountAggregate,
    pub days_1_30: AmountAggregate,
    pub days_31_60: AmountAggregate,
    pub days_61_90: AmountAggregate,
    pub days_90_plus: AmountAggregate,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct DsoPanel {
    pub huf_days: Option<f64>,
    pub eur_days: Option<f64>,
    pub huf_sample_size: u64,
    pub eur_sample_size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CashflowPanel {
    pub next_30: CurrencyPair,
    pub next_60: CurrencyPair,
    pub next_90: CurrencyPair,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VatRateBreakdownEntry {
    pub rate_basis_points: i32,
    pub currency: String,
    pub net_minor: i64,
    pub vat_minor: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TopEntry {
    pub label: String,
    pub currency: String,
    pub gross_minor: i64,
    pub count: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct HygienePanel {
    /// Outgoing native invoices in terminal-bad NAV states (Rejected /
    /// Abandoned) — need attention from the operator.
    pub outgoing_rejected_count: u64,
    pub outgoing_abandoned_count: u64,
    /// Outgoing native invoices in pre-submission states (Ready, Pending,
    /// PendingNavExists) — drafts the operator may have forgotten.
    pub outgoing_pending_count: u64,
    /// Restored (ExtNav) rows without a partner_id (manual link missing).
    pub restored_no_partner_count: u64,
    /// Counted outgoing invoices whose payment_deadline has passed and
    /// no payment is recorded.
    ///
    /// EXCLUDES invoices with no recorded `payment_deadline` (missing or
    /// unreadable) — they are treated as SETTLED legacy imports and are
    /// out of outstanding altogether, so they reach neither the aging
    /// panel nor this counter. See [`aging_placement`]. Two independent
    /// reasons, either sufficient: a settled invoice is not late, and a
    /// deadline nobody can read is unknown lateness rather than lateness.
    /// They are disclosed instead via
    /// [`LedgerDiagnostics::aging_settled_undated`].
    pub outstanding_past_deadline_count: u64,
    /// Outstanding ap_invoice rows whose payment_deadline has passed.
    ///
    /// Excludes missing / unreadable deadlines for the same reason as
    /// [`Self::outstanding_past_deadline_count`], and the exclusion is
    /// load-bearing here: `ap_sync` records `payment_deadline: None` on
    /// every NAV-synced payable, so a counter that inherited those rows
    /// would report essentially the entire payables book as past
    /// deadline.
    pub payable_past_deadline_count: u64,
    /// Number of `InvoiceStornoIssued` chain entries in the period.
    pub storno_chain_count: u64,
    /// Number of `InvoiceModificationIssued` chain entries in the period.
    pub modification_chain_count: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct PeriodDeltas {
    pub mom: Option<DeltaSet>,
    pub yoy: Option<DeltaSet>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct DeltaSet {
    pub period_label: String,
    pub revenue: CurrencyAggregate,
    pub expenses: CurrencyAggregate,
    pub revenue_pct_huf: Option<f64>,
    pub revenue_pct_eur: Option<f64>,
    pub expenses_pct_huf: Option<f64>,
    pub expenses_pct_eur: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AnnualRunningPanel {
    pub year: i32,
    pub revenue: CurrencyAggregate,
}

// ──────────────────────────────────────────────────────────────────────
// Period parsing.
// ──────────────────────────────────────────────────────────────────────

/// Resolved date window for SQL filtering. `None` on either side
/// represents an open bound (only used for `PeriodKind::All`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateWindow {
    pub from: Option<Date>,
    pub to: Option<Date>,
}

impl DateWindow {
    fn unbounded() -> Self {
        Self {
            from: None,
            to: None,
        }
    }
}

/// Parse the `period` query-string parameter into a [`PeriodKind`].
///
/// Accepted forms:
///   - `2026-06` → `Month(2026, 6)`
///   - `2026-Q2` → `Quarter(2026, 2)`
///   - `2026` → `Year(2026)`
///   - `all` → `All`
///   - `2026-06-01..2026-06-30` → `Custom { from, to }`
///
/// Returns `Err` for malformed strings so the route layer can surface
/// a 400. Per CLAUDE.md rule 12 — silent coercion to a default would
/// hide an operator-typed typo in a URL parameter.
pub fn parse_period(s: &str) -> Result<PeriodKind> {
    let trimmed = s.trim();
    if trimmed.eq_ignore_ascii_case("all") {
        return Ok(PeriodKind::All);
    }
    if let Some((from_s, to_s)) = trimmed.split_once("..") {
        let from = parse_iso_date(from_s)?;
        let to = parse_iso_date(to_s)?;
        if to < from {
            return Err(anyhow!(
                "custom period `{}` has to-date before from-date",
                trimmed
            ));
        }
        return Ok(PeriodKind::Custom { from, to });
    }
    // Quarter form: `YYYY-Q[1-4]`.
    if let Some((y_s, q_s)) = trimmed.split_once("-Q") {
        let year: i32 = y_s
            .parse()
            .with_context(|| format!("quarter period `{}` has malformed year", trimmed))?;
        let q: u8 = q_s
            .parse()
            .with_context(|| format!("quarter period `{}` has malformed quarter", trimmed))?;
        if !(1..=4).contains(&q) {
            return Err(anyhow!(
                "quarter period `{}` has out-of-range quarter (1-4)",
                trimmed
            ));
        }
        return Ok(PeriodKind::Quarter(year, q));
    }
    // Month form: `YYYY-MM`.
    if let Some((y_s, m_s)) = trimmed.split_once('-') {
        let year: i32 = y_s
            .parse()
            .with_context(|| format!("month period `{}` has malformed year", trimmed))?;
        let m: u8 = m_s
            .parse()
            .with_context(|| format!("month period `{}` has malformed month", trimmed))?;
        if !(1..=12).contains(&m) {
            return Err(anyhow!(
                "month period `{}` has out-of-range month (1-12)",
                trimmed
            ));
        }
        return Ok(PeriodKind::Month(year, m));
    }
    // Bare year form: `YYYY`.
    if trimmed.len() == 4 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = trimmed.parse().with_context(|| "year parse")?;
        return Ok(PeriodKind::Year(year));
    }
    Err(anyhow!("unparseable period `{}`", trimmed))
}

fn parse_iso_date(s: &str) -> Result<Date> {
    let fmt = format_description!("[year]-[month]-[day]");
    Date::parse(s.trim(), fmt).with_context(|| format!("parse ISO date `{}`", s))
}

/// Resolve a [`PeriodKind`] to inclusive ISO-date bounds for SQL
/// filtering.
pub fn resolve_window(kind: PeriodKind) -> Result<DateWindow> {
    match kind {
        PeriodKind::All => Ok(DateWindow::unbounded()),
        PeriodKind::Month(y, m) => {
            let month = Month::try_from(m)
                .map_err(|_| anyhow!("month {} out of range when resolving period", m))?;
            let from = Date::from_calendar_date(y, month, 1)
                .with_context(|| format!("calendar date {}-{:02}-01", y, m))?;
            let next_first = next_month_first(y, m)?;
            let to = next_first.previous_day().expect("date arithmetic");
            Ok(DateWindow {
                from: Some(from),
                to: Some(to),
            })
        }
        PeriodKind::Quarter(y, q) => {
            let start_month = match q {
                1 => 1,
                2 => 4,
                3 => 7,
                4 => 10,
                _ => return Err(anyhow!("quarter {} out of range", q)),
            };
            let end_month_first_next = next_month_first(y, start_month + 2)?;
            let from = Date::from_calendar_date(y, Month::try_from(start_month).unwrap(), 1)
                .with_context(|| format!("calendar date {}-Q{}", y, q))?;
            let to = end_month_first_next
                .previous_day()
                .expect("date arithmetic");
            Ok(DateWindow {
                from: Some(from),
                to: Some(to),
            })
        }
        PeriodKind::Year(y) => {
            let from = Date::from_calendar_date(y, Month::January, 1)?;
            let to = Date::from_calendar_date(y, Month::December, 31)?;
            Ok(DateWindow {
                from: Some(from),
                to: Some(to),
            })
        }
        PeriodKind::Custom { from, to } => Ok(DateWindow {
            from: Some(from),
            to: Some(to),
        }),
    }
}

fn next_month_first(y: i32, m: u8) -> Result<Date> {
    let (ny, nm) = if m >= 12 { (y + 1, 1u8) } else { (y, m + 1) };
    let month = Month::try_from(nm).map_err(|_| anyhow!("month arithmetic failed"))?;
    Date::from_calendar_date(ny, month, 1).map_err(|e| anyhow!("date construction: {}", e))
}

fn period_label(kind: PeriodKind) -> String {
    match kind {
        PeriodKind::Month(y, m) => format!("{:04}-{:02}", y, m),
        PeriodKind::Quarter(y, q) => format!("{:04}-Q{}", y, q),
        PeriodKind::Year(y) => format!("{:04}", y),
        PeriodKind::All => "all".to_string(),
        PeriodKind::Custom { from, to } => format!("{}..{}", from, to),
    }
}

fn period_kind_label(kind: PeriodKind) -> &'static str {
    match kind {
        PeriodKind::Month(..) => "month",
        PeriodKind::Quarter(..) => "quarter",
        PeriodKind::Year(..) => "year",
        PeriodKind::All => "all",
        PeriodKind::Custom { .. } => "custom",
    }
}

/// Shift a period back to its comparable "previous month" / "previous
/// quarter" / "previous year" sibling for MoM delta computation. `None`
/// for `Custom` (would require shifting an arbitrary window — out of
/// scope for v2.2.0) and for `All`.
fn previous_period(kind: PeriodKind) -> Option<PeriodKind> {
    match kind {
        PeriodKind::Month(y, m) => {
            let (py, pm) = if m <= 1 { (y - 1, 12u8) } else { (y, m - 1) };
            Some(PeriodKind::Month(py, pm))
        }
        PeriodKind::Quarter(y, q) => {
            let (py, pq) = if q <= 1 { (y - 1, 4u8) } else { (y, q - 1) };
            Some(PeriodKind::Quarter(py, pq))
        }
        PeriodKind::Year(y) => Some(PeriodKind::Year(y - 1)),
        PeriodKind::All | PeriodKind::Custom { .. } => None,
    }
}

/// Shift a period back one year for YoY delta computation. `None` for
/// `All` (no sensible comparable) and for `Custom` (operator-defined).
fn yoy_period(kind: PeriodKind) -> Option<PeriodKind> {
    match kind {
        PeriodKind::Month(y, m) => Some(PeriodKind::Month(y - 1, m)),
        PeriodKind::Quarter(y, q) => Some(PeriodKind::Quarter(y - 1, q)),
        PeriodKind::Year(y) => Some(PeriodKind::Year(y - 1)),
        PeriodKind::All | PeriodKind::Custom { .. } => None,
    }
}

// ──────────────────────────────────────────────────────────────────────
// Audit-ledger trace walk.
// ──────────────────────────────────────────────────────────────────────

/// Minimal per-invoice trace produced by the single audit-ledger walk.
/// Mirrors the fields `serve::list_invoices` reads but trims to what
/// the aggregator needs (no chain-children, no NAV check-outcome).
#[derive(Debug, Default, Clone)]
struct ReportTrace {
    has_draft: bool,
    has_attempt: bool,
    has_submission_response: bool,
    has_marked_abandoned: bool,
    last_ack_status: Option<String>,
    /// This invoice is the base of a storno that ACTUALLY LANDED — its
    /// storno child classifies as [`CountedKind::Counted`], i.e. the
    /// reversal is in the aggregates and the pair nets to zero.
    ///
    /// NOT "a storno was issued against it". `InvoiceStornoIssued` is
    /// appended in the same transaction as the storno DRAFT, before the
    /// storno is submitted to NAV (`issue_storno.rs:1195-1213`), so the
    /// chain link alone says only that a cancellation was *attempted*. If
    /// that storno was ABORTED at NAV or never submitted, it never
    /// negates anything and the base is still a live, unpaid, legally
    /// outstanding invoice. This flag is therefore resolved AFTER the
    /// ledger walk, once the child's own ack is known — see
    /// [`resolve_landed_stornos`].
    ///
    /// Same rule the NAV chain allocator applies: only chain members
    /// whose own submission reached terminal SAVED count
    /// (`issue_storno::saved_chain_member_ids_in_tx`, S381/F4).
    has_landed_storno: bool,
    is_amended_base: bool,
    is_storno_self: bool,
    payment_paid_at: Option<String>,
    payment_amount_minor: Option<i64>,
}

/// Classification of an outgoing invoice for aggregation purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountedKind {
    /// Counts toward revenue/VAT-collected. May be storno-self → negated.
    Counted { is_storno_self: bool },
    /// Pre-submission state (Ready / Pending / PendingNavExists). Hygiene
    /// count only; not in revenue.
    PendingDraft,
    /// Terminal-bad state (Rejected / Aborted ack).
    Rejected,
    /// Operator-declared abandoned.
    Abandoned,
    /// No audit entries (orphan billing row).
    Unknown,
}

impl ReportTrace {
    fn classify(&self) -> CountedKind {
        if self.has_marked_abandoned {
            return CountedKind::Abandoned;
        }
        match self.last_ack_status.as_deref() {
            Some("SAVED") => {
                return CountedKind::Counted {
                    is_storno_self: self.is_storno_self,
                }
            }
            Some("ABORTED") => return CountedKind::Rejected,
            _ => {}
        }
        if self.has_submission_response {
            return CountedKind::Counted {
                is_storno_self: self.is_storno_self,
            };
        }
        // Storno-base / amended-base WITHOUT a SAVED ack — base rows
        // sit in earlier ledger entries; storno chain links don't
        // resurrect them. Fall through.
        if self.has_attempt || self.has_draft {
            return CountedKind::PendingDraft;
        }
        CountedKind::Unknown
    }
}

/// Result of the one-pass audit-ledger walk.
#[derive(Debug, Default, Clone)]
struct LedgerWalk {
    traces: HashMap<String, ReportTrace>,
    /// `InvoiceStornoIssued` chain entries whose audit `at` timestamp
    /// falls inside `(from, to)`. Counted for hygiene.
    storno_links_in_period: u64,
    /// `InvoiceModificationIssued` chain entries in the period.
    modification_links_in_period: u64,
    /// Entries this walk could NOT decode — see [`LedgerDiagnostics`].
    diagnostics: LedgerDiagnostics,
}

fn walk_ledger(ledger: &Ledger, period_window: DateWindow) -> Result<LedgerWalk> {
    let entries = ledger
        .entries()
        .context("read audit ledger entries for financial report")?;
    let mut walk = LedgerWalk::default();
    // (base_invoice_id, storno_invoice_id) for every storno chain link.
    // Collected during the walk but RESOLVED after it: whether a storno
    // landed depends on the child's own ack, which may be appended by a
    // later entry than the chain link itself.
    let mut storno_links: Vec<(String, String)> = Vec::new();
    for entry in &entries {
        // A decode failure on THIS entry, if any. Recorded once at the end
        // of the iteration rather than at each failing call site: a payload
        // that is not JSON at all fails both decodes below, and the
        // operator-facing count is entries-not-read, not attempts-failed.
        let mut decode_error: Option<String> = None;

        match extract_invoice_id_local(entry) {
            Ok(Some(id)) => {
                // The trace row is created either way (`or_default()` runs
                // before `merge`), exactly as before — a failed typed decode
                // leaves it at its defaults instead of vanishing the id.
                if let Err(e) = walk.traces.entry(id.clone()).or_default().merge(entry, &id) {
                    decode_error = Some(e.to_string());
                }
            }
            // Valid JSON with no `invoice_id` key — the ordinary case for
            // every event kind that is not invoice-scoped. NOT a defect.
            Ok(None) => {}
            Err(e) => decode_error = Some(e.to_string()),
        }

        match extract_chain_link_local(entry) {
            Ok(Some(link)) => {
                if link.is_storno {
                    walk.traces
                        .entry(link.child_invoice_id.clone())
                        .or_default()
                        .is_storno_self = true;
                    walk.traces.entry(link.base_invoice_id.clone()).or_default();
                    storno_links.push((link.base_invoice_id, link.child_invoice_id));
                    if entry_in_window(entry, period_window) {
                        walk.storno_links_in_period = walk.storno_links_in_period.saturating_add(1);
                    }
                } else {
                    walk.traces
                        .entry(link.base_invoice_id.clone())
                        .or_default()
                        .is_amended_base = true;
                    if entry_in_window(entry, period_window) {
                        walk.modification_links_in_period =
                            walk.modification_links_in_period.saturating_add(1);
                    }
                }
            }
            // Not a chain-link kind. The overwhelming majority of entries.
            Ok(None) => {}
            Err(e) => {
                decode_error.get_or_insert_with(|| e.to_string());
            }
        }

        if let Some(err) = decode_error {
            record_unparseable_entry(&mut walk.diagnostics, entry, &err);
        }
    }
    resolve_landed_stornos(&mut walk.traces, &storno_links);
    Ok(walk)
}

/// Make one undecodable audit entry LOUD and ATTRIBUTABLE: a structured
/// error log naming the entry, and a bump on the machine-countable signal
/// that rides out on [`FinancialReport::ledger_diagnostics`].
///
/// Deliberately not a hard error. Aborting the walk would let a single
/// corrupt row blank the operator's whole financial screen, which trades a
/// wrong number for no number at all. See [`LedgerDiagnostics`].
fn record_unparseable_entry(diag: &mut LedgerDiagnostics, entry: &Entry, error: &str) {
    let id = entry.id.to_prefixed_string();
    tracing::error!(
        entry_id = %id,
        entry_seq = entry.seq.0,
        entry_kind = ?entry.kind,
        decode_error = %error,
        "financial report: audit entry payload could not be decoded — this entry is \
         NOT reflected in the reported figures"
    );
    diag.unparseable_entries = diag.unparseable_entries.saturating_add(1);
    if diag.unparseable_entry_ids.len() < MAX_UNPARSEABLE_ENTRY_IDS {
        diag.unparseable_entry_ids.push(id);
    }
}

/// Second pass over the storno chain links: flag a base as
/// [`ReportTrace::has_landed_storno`] only if its storno child actually
/// took effect.
///
/// Split from the walk because it cannot be decided during it. The
/// `InvoiceStornoIssued` link is appended with the storno DRAFT, in the
/// same transaction, BEFORE the storno is submitted
/// (`issue_storno.rs:1195-1213`); the child's ack — the thing that says
/// whether the cancellation happened — arrives in a later entry. Setting
/// the flag at link time meant "a cancellation was attempted", which is
/// not the same claim.
///
/// "Landed" is [`CountedKind::Counted`] — deliberately the SAME condition
/// under which [`aggregate_outgoing`] adds the child's −amount to revenue.
/// That coupling is the invariant: revenue and receivables must agree
/// about whether a storno took effect. A NAV-ABORTed or never-submitted
/// storno classifies as `Rejected` / `PendingDraft`, revenue keeps the
/// base's +amount, and so must receivables — the base is still owed.
///
/// This is the report-side reading of the rule the NAV chain allocator
/// already applies via `issue_storno::saved_chain_member_ids_in_tx`
/// (S381/F4): a storno that never reached terminal SAVED never registered.
fn resolve_landed_stornos(
    traces: &mut HashMap<String, ReportTrace>,
    storno_links: &[(String, String)],
) {
    let landed_bases: Vec<String> = storno_links
        .iter()
        .filter(|(_, child)| {
            traces
                .get(child)
                .is_some_and(|t| matches!(t.classify(), CountedKind::Counted { .. }))
        })
        .map(|(base, _)| base.clone())
        .collect();
    for base in landed_bases {
        if let Some(t) = traces.get_mut(&base) {
            t.has_landed_storno = true;
        }
    }
}

impl ReportTrace {
    /// Fold one audit entry into this invoice's trace.
    ///
    /// `Err` means the entry's typed payload did NOT decode, so nothing was
    /// folded and this entry is missing from every figure derived from the
    /// trace. The caller MUST surface it — see [`LedgerDiagnostics`]. The
    /// four decode arms used to be `if let Ok(..)` with no `else`, which is
    /// precisely the silent drop.
    ///
    /// The `parsed.invoice_id == invoice_id` guards are unchanged: a
    /// successfully-decoded payload naming a DIFFERENT invoice is not a
    /// defect, it just does not belong to this trace.
    fn merge(&mut self, entry: &Entry, invoice_id: &str) -> serde_json::Result<()> {
        match entry.kind {
            EventKind::InvoiceDraftCreated => self.has_draft = true,
            EventKind::InvoiceSubmissionAttempt => {
                let parsed = serde_json::from_slice::<
                    audit_payloads::InvoiceSubmissionAttemptPayload,
                >(&entry.payload)?;
                if parsed.invoice_id == invoice_id {
                    self.has_attempt = true;
                }
            }
            EventKind::InvoiceSubmissionResponse => {
                let parsed = serde_json::from_slice::<
                    audit_payloads::InvoiceSubmissionResponsePayload,
                >(&entry.payload)?;
                if parsed.invoice_id == invoice_id {
                    self.has_submission_response = true;
                }
            }
            EventKind::InvoiceAckStatus => {
                let parsed = serde_json::from_slice::<audit_payloads::InvoiceAckStatusPayload>(
                    &entry.payload,
                )?;
                if parsed.invoice_id == invoice_id {
                    self.last_ack_status = Some(parsed.ack_status);
                }
            }
            EventKind::InvoiceMarkedAbandoned => {
                self.has_marked_abandoned = true;
            }
            EventKind::InvoicePaymentRecorded => {
                let parsed = serde_json::from_slice::<audit_payloads::InvoicePaymentRecordedPayload>(
                    &entry.payload,
                )?;
                if parsed.invoice_id == invoice_id {
                    self.payment_paid_at = Some(parsed.paid_at);
                    self.payment_amount_minor = Some(parsed.amount_minor);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

struct ChainLinkLocal {
    base_invoice_id: String,
    child_invoice_id: String,
    is_storno: bool,
}

/// `Ok(None)` = this entry is not a chain-link kind (the ordinary case).
/// `Err` = it IS one and its payload did not decode, so the chain link is
/// lost — a storno that never nets its base to zero, leaving revenue and
/// receivables overstated. Used to be `.ok()?`, indistinguishable from
/// "not a chain link" and therefore silent.
fn extract_chain_link_local(entry: &Entry) -> serde_json::Result<Option<ChainLinkLocal>> {
    match entry.kind {
        EventKind::InvoiceStornoIssued => {
            let parsed: audit_payloads::InvoiceStornoIssuedPayload =
                serde_json::from_slice(&entry.payload)?;
            Ok(Some(ChainLinkLocal {
                base_invoice_id: parsed.base_invoice_id,
                child_invoice_id: parsed.storno_invoice_id,
                is_storno: true,
            }))
        }
        EventKind::InvoiceModificationIssued => {
            let parsed: audit_payloads::InvoiceModificationIssuedPayload =
                serde_json::from_slice(&entry.payload)?;
            Ok(Some(ChainLinkLocal {
                base_invoice_id: parsed.base_invoice_id,
                child_invoice_id: parsed.modification_invoice_id,
                is_storno: false,
            }))
        }
        _ => Ok(None),
    }
}

/// `Ok(None)` = the payload is well-formed JSON that carries no
/// `invoice_id` (chain links, tenant-level events, …) — normal, and NOT a
/// diagnostic. `Err` = the payload is not JSON at all, so the entry never
/// reaches [`ReportTrace::merge`] and contributes to nothing. Used to be
/// `.ok()?`, which collapsed those two cases into one silent `None`.
fn extract_invoice_id_local(entry: &Entry) -> serde_json::Result<Option<String>> {
    let v: serde_json::Value = serde_json::from_slice(&entry.payload)?;
    Ok(v.as_object()
        .and_then(|m| m.get("invoice_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string()))
}

fn entry_in_window(entry: &Entry, window: DateWindow) -> bool {
    if window.from.is_none() && window.to.is_none() {
        return true;
    }
    let d = entry.time_wall.date();
    if let Some(from) = window.from {
        if d < from {
            return false;
        }
    }
    if let Some(to) = window.to {
        if d > to {
            return false;
        }
    }
    true
}

// ──────────────────────────────────────────────────────────────────────
// SQL aggregation rows.
// ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct OutgoingLineGroup {
    invoice_id: String,
    currency: String,
    /// `invoice.issue_date`, unconditionally — NOT the date-basis column.
    /// Its one consumer is the DSO sample, which is defined as
    /// `paid_at − issue_date`; see [`aggregate_outgoing`].
    ///
    /// Always the bare `YYYY-MM-DD` head: the projection truncates it, because
    /// the stored column is RFC3339 VARCHAR and the consumer parses date-only.
    issue_date: String,
    payment_deadline: Option<String>,
    vat_rate_basis_points: i32,
    /// Sum of the group's **per-line** `net_total()`s, in whole minor units.
    /// Positive; [`aggregate_outgoing`] applies the storno sign.
    net_minor: i64,
    /// ADR-0108 M-2 — sum of the group's **per-line** `vat_amount()`s, in
    /// whole minor units. Carried on the group rather than derived from
    /// `net_minor × basis_points` at aggregation time, because the filing's
    /// VAT is a per-line truncation and the group net has already lost the
    /// per-line remainders. Positive; the storno sign is applied with
    /// `net_minor`'s.
    vat_minor: i64,
}

/// One `invoice_line` row as the ÁFA report reads it — the raw column values,
/// with **no arithmetic done in SQL** (ADR-0108 §3.4 / T-8).
struct OutgoingLineRow {
    invoice_id: String,
    currency: String,
    issue_date: String,
    payment_deadline: Option<String>,
    vat_rate_basis_points: i32,
    vat_rate_kind: Option<String>,
    quantity: Decimal,
    unit_price: i64,
}

#[derive(Debug, Clone)]
struct ApRow {
    /// `ap_invoice.id` — carried so an aging diagnostic can NAME the
    /// offending payable instead of just counting it.
    id: String,
    supplier_name: String,
    payment_deadline: Option<String>,
    net_minor: i64,
    vat_minor: i64,
    gross_minor: i64,
    currency: String,
    local_status: String,
}

#[derive(Debug, Clone)]
struct RestoredRow {
    customer_name: Option<String>,
    net_minor: i64,
    vat_minor: i64,
    gross_minor: i64,
    currency: String,
    partner_id: Option<String>,
}

fn date_col_sql_ap(basis: DateBasis) -> &'static str {
    match basis {
        DateBasis::Teljesites => "COALESCE(a.delivery_date, a.issue_date)",
        DateBasis::Issued => "a.issue_date",
    }
}

fn date_col_sql_restored() -> &'static str {
    // `restored_invoice` has only `issue_date` — Teljesites falls back
    // to the same column.
    "r.issue_date"
}

fn date_str(d: Date) -> String {
    let fmt = format_description!("[year]-[month]-[day]");
    d.format(fmt).expect("ISO date format")
}

fn query_outgoing_groups(
    conn: &Connection,
    window: DateWindow,
    basis: DateBasis,
) -> Result<Vec<OutgoingLineGroup>> {
    // The window predicate comes from `build_date_where`, which emits the
    // canonical (teljesites) date column itself — the basis does not reach
    // this statement. The projected date is `i.issue_date` unconditionally:
    // its only consumer is the DSO sample, which is anchored on the issue
    // date by definition, regardless of the basis the operator is viewing.
    //
    // It is projected as `SUBSTR(CAST(… AS VARCHAR), 1, 10)`, NOT raw. In
    // production `invoice.issue_date` is `VARCHAR NOT NULL` holding RFC3339
    // (`draft.issue_date.format(&Rfc3339)` in the billing store), e.g.
    // `2026-06-15T12:00:00Z` — but the consumer, `parse_iso_date`, accepts
    // `[year]-[month]-[day]` and nothing else. Feeding it the raw column made
    // every DSO sample fail the parse guard and drop silently: the panel
    // rendered `— (n=0)` on all real data. The `SUBSTR` truncates to the date
    // head, which is correct for RFC3339 and a no-op for a date-only VARCHAR;
    // the `CAST` additionally keeps `row.get::<String>` from raising
    // `InvalidColumnType` on a legacy DATE-typed column — that error would
    // fail the WHOLE financial report, not just DSO.
    //
    // `payment_deadline` is projected the SAME way, and the comment used to
    // claim it already was while it in fact carried a bare `CAST` with no
    // `SUBSTR`. It is currently `DATE` and renders date-only, so the two
    // agreed by luck rather than by construction. The asymmetry was a live
    // trap under this branch's semantics: widen that column to a timestamp
    // (or let any writer store an RFC3339 string, which is exactly what
    // happened to `issue_date`) and `parse_iso_date` would reject EVERY AR
    // deadline — which no longer means "drop from the buckets" but "treat
    // as settled and remove from Receivables entirely". The whole
    // receivables book would silently go to zero behind a footnote. The
    // truncation is a no-op today and removes the trap.
    let _ = basis;
    let (where_clause, has_from, has_to) = build_date_where(window);
    // ADR-0108 §3.4 site 1 / T-8: NO arithmetic in SQL. The statement projects
    // the raw columns and the fold happens in Rust, below. `quantity` crosses
    // as `TEXT` (it is `DECIMAL(18,6)` on DuckDB and R2 `TEXT` on SQLite; the
    // `CAST(… AS VARCHAR)` renders both identically) and `unit_price` as the
    // R1 `INTEGER` it already is.
    let sql = format!(
        "SELECT i.id,
                COALESCE(i.currency, 'HUF') AS currency,
                SUBSTR(CAST(i.issue_date AS VARCHAR), 1, 10) AS issue_date,
                SUBSTR(CAST(i.payment_deadline AS VARCHAR), 1, 10) AS payment_deadline,
                il.vat_rate_basis_points,
                il.vat_rate_kind,
                CAST(il.quantity AS VARCHAR) AS quantity,
                il.unit_price
           FROM invoice i
           JOIN invoice_line il ON i.id = il.invoice_id
          {where_clause}",
    );
    let mut stmt = conn.prepare(&sql).context("prepare outgoing line SQL")?;
    let from_s = window.from.map(date_str);
    let to_s = window.to.map(date_str);
    let rows = match (has_from, has_to) {
        (true, true) => stmt.query_map(params![from_s.unwrap(), to_s.unwrap()], row_to_outgoing)?,
        (true, false) => stmt.query_map(params![from_s.unwrap()], row_to_outgoing)?,
        (false, true) => stmt.query_map(params![to_s.unwrap()], row_to_outgoing)?,
        (false, false) => stmt.query_map([], row_to_outgoing)?,
    };
    let mut lines = Vec::new();
    for r in rows {
        lines.push(r?);
    }
    fold_outgoing_lines(lines)
}

fn row_to_outgoing(row: &duckdb::Row) -> duckdb::Result<OutgoingLineRow> {
    let quantity_str: String = row.get(6)?;
    let quantity = Decimal::from_str(&quantity_str).map_err(|_| {
        duckdb::Error::FromSqlConversionFailure(
            6,
            duckdb::types::Type::Text,
            format!("invoice_line.quantity is not a decimal: {quantity_str:?}").into(),
        )
    })?;
    Ok(OutgoingLineRow {
        invoice_id: row.get(0)?,
        currency: row.get(1)?,
        issue_date: row.get(2)?,
        payment_deadline: row.get(3)?,
        vat_rate_basis_points: row.get(4)?,
        vat_rate_kind: row.get(5)?,
        quantity,
        unit_price: row.get(7)?,
    })
}

/// ADR-0108 M-2 — fold the per-line rows into `(invoice, vat_rate)` groups
/// **through the same two functions the NAV filing uses**.
///
/// # Why per-line, and what it changes
///
/// Before this fold the report summed the *unrounded* `quantity × unit_price`
/// products per group in SQL and rounded **once**, half-even, at the i64
/// boundary; VAT was then `round_half_even(group_net × bp / 10_000)`. The
/// filing does neither: `nav_xml::write_summary` sums
/// [`line_net_total`] — round-half-even **per line** — and
/// [`line_vat_amount`] — `floor(net × bp / 10_000)`, i.e. **truncated**,
/// per line. Two independent divergences, granularity *and* rounding mode.
///
/// Worked example, and the T-5(c) regression fixture: two 27% lines of 50 Ft
/// net each. Filed VAT is `50 × 2700 / 10_000 = 13` twice → **26 Ft**. The old
/// report computed `round_half_even(100 × 2700 / 10_000) = 27` → **27 Ft**.
/// The report published a figure the tax authority never received.
///
/// Ervin's M-2 ruling is per-line: the ÁFA report shows what was filed. So the
/// arithmetic is not merely *equivalent* to the filing's, it is literally the
/// filing's functions — anything less is a rule-7 fork that survives until the
/// first edit to either copy.
///
/// The storno sign is applied later, by [`aggregate_outgoing`], and that is
/// safe: negation commutes with both roundings. Half-even is symmetric about
/// zero, and Rust's `i64 / i64` truncates *toward zero*, so
/// `-trunc(|x|) == trunc(-|x|)`. The filing negates `quantity`
/// (`nav_xml::negate_line`, S381) and reaches the same magnitudes.
fn fold_outgoing_lines(lines: Vec<OutgoingLineRow>) -> Result<Vec<OutgoingLineGroup>> {
    let mut out: Vec<OutgoingLineGroup> = Vec::new();
    for line in lines {
        // ADR-0101, mirroring `duckdb_store`'s read path exactly: NULL
        // (pre-0101, pre-backfill) → Percent; a present value that does not
        // parse is a corrupt row and fails loud (rule 11) rather than
        // defaulting to Percent, which would invent VAT on an exempt line.
        let kind = match line.vat_rate_kind.as_deref() {
            None => VatRateKind::Percent,
            Some(s) => VatRateKind::from_db_str(s).with_context(|| {
                format!(
                    "invoice {} carries an unknown invoice_line.vat_rate_kind {s:?}",
                    line.invoice_id
                )
            })?,
        };
        let basis_points: u16 = line.vat_rate_basis_points.try_into().with_context(|| {
            format!(
                "invoice {} carries an out-of-range vat_rate_basis_points {}",
                line.invoice_id, line.vat_rate_basis_points
            )
        })?;
        let net = line_net_total(Huf(line.unit_price), line.quantity).with_context(|| {
            format!(
                "net total overflows on a line of invoice {}",
                line.invoice_id
            )
        })?;
        let vat = line_vat_amount(net, kind, basis_points).with_context(|| {
            format!(
                "VAT amount overflows on a line of invoice {}",
                line.invoice_id
            )
        })?;

        let key = (line.invoice_id.as_str(), line.vat_rate_basis_points);
        let idx = match out
            .iter()
            .position(|g| (g.invoice_id.as_str(), g.vat_rate_basis_points) == key)
        {
            Some(idx) => idx,
            None => {
                out.push(OutgoingLineGroup {
                    invoice_id: line.invoice_id.clone(),
                    currency: line.currency,
                    issue_date: line.issue_date,
                    payment_deadline: line.payment_deadline,
                    vat_rate_basis_points: line.vat_rate_basis_points,
                    net_minor: 0,
                    vat_minor: 0,
                });
                out.len() - 1
            }
        };
        let g = &mut out[idx];
        g.net_minor = g
            .net_minor
            .checked_add(net.as_i64())
            .with_context(|| format!("net overflows on invoice {}", g.invoice_id))?;
        g.vat_minor = g
            .vat_minor
            .checked_add(vat.as_i64())
            .with_context(|| format!("VAT overflows on invoice {}", g.invoice_id))?;
    }
    Ok(out)
}

/// S262 / PR-251 — sum of `huf_equivalent_total` (snapshot-rate HUF
/// equivalent of gross, ADR-0037 §1.c) over EUR native invoices in the
/// window. HUF invoices store NULL there (their gross IS the HUF figure),
/// so the `= 'EUR'` predicate is what restricts the sum. Issued basis;
/// see [`CurrencySplitPanel`] for the storno caveat. The window predicate
/// mirrors `query_outgoing_groups` via [`build_date_where`].
fn query_eur_huf_equivalent(
    conn: &Connection,
    window: DateWindow,
    basis: DateBasis,
) -> Result<i64> {
    // `build_date_where` interpolates the teljesites date column; the
    // existing outgoing query relies on the same shape, so the currency
    // split stays consistent with the revenue figure it splits.
    let _ = basis;
    let (date_where, has_from, has_to) = build_date_where(window);
    let currency_pred = "COALESCE(i.currency, 'HUF') = 'EUR'";
    let where_clause = if date_where.is_empty() {
        format!("WHERE {currency_pred}")
    } else {
        format!("{date_where} AND {currency_pred}")
    };
    // ADR-0108 §3.4 site 2 / T-8: no `SUM` in SQL. `huf_equivalent_total` is
    // R1 (`INTEGER` minor units, §3.2 B), where SQL `SUM` is exact but *raises*
    // on i64 overflow — and its former reader swallowed the failure to 0. The
    // column is projected and folded with `checked_add` here instead, so an
    // overflow is a loud error rather than a plausible-looking HUF figure.
    let sql = format!(
        "SELECT i.huf_equivalent_total
           FROM invoice i
          {where_clause}",
    );
    let mut stmt = conn
        .prepare(&sql)
        .context("prepare EUR huf-equivalent SQL")?;
    let from_s = window.from.map(date_str);
    let to_s = window.to.map(date_str);
    // NULL for HUF-native invoices; the `= 'EUR'` predicate already excludes
    // them, so a NULL here contributes nothing rather than failing.
    let read = |row: &duckdb::Row| -> duckdb::Result<Option<i64>> { row.get(0) };
    let rows = match (has_from, has_to) {
        (true, true) => stmt.query_map(params![from_s.unwrap(), to_s.unwrap()], read)?,
        (true, false) => stmt.query_map(params![from_s.unwrap()], read)?,
        (false, true) => stmt.query_map(params![to_s.unwrap()], read)?,
        (false, false) => stmt.query_map([], read)?,
    };
    let mut total: i64 = 0;
    for r in rows {
        let Some(v) = r? else { continue };
        total = total
            .checked_add(v)
            .context("EUR->HUF equivalent total overflows i64")?;
    }
    Ok(total)
}

fn query_ap_rows(
    conn: &Connection,
    tenant: &str,
    window: DateWindow,
    basis: DateBasis,
) -> Result<Vec<ApRow>> {
    let date_col = date_col_sql_ap(basis);
    let mut clauses = vec!["a.tenant_id = ?".to_string()];
    let mut binds: Vec<String> = vec![tenant.to_string()];
    if let Some(from) = window.from {
        clauses.push(format!("{date_col} >= ?"));
        binds.push(date_str(from));
    }
    if let Some(to) = window.to {
        clauses.push(format!("{date_col} <= ?"));
        binds.push(date_str(to));
    }
    // `payment_deadline` gets the same `SUBSTR(CAST(… AS VARCHAR), 1, 10)`
    // treatment as the outgoing projection, and for a sharper reason: this
    // is the side where undated rows are the DOMINANT population, so a
    // column that ever renders wider than a bare date would move the whole
    // payables book into "settled, excluded" in one step. It was the only
    // one of the three date projections with no `CAST` at all, which also
    // left `row.get::<Option<String>>` free to raise `InvalidColumnType`
    // and fail the entire financial report on a type change.
    let sql = format!(
        "SELECT a.id, a.supplier_name,
                SUBSTR(CAST(a.payment_deadline AS VARCHAR), 1, 10) AS payment_deadline,
                a.total_net_minor, a.total_vat_minor, a.total_gross_minor, a.currency,
                a.local_status
           FROM ap_invoice a
          WHERE {}",
        clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql).context("prepare ap_invoice SQL")?;
    let params_dyn: Vec<&dyn duckdb::ToSql> =
        binds.iter().map(|s| s as &dyn duckdb::ToSql).collect();
    let rows = stmt.query_map(params_dyn.as_slice(), |row| {
        Ok(ApRow {
            id: row.get(0)?,
            supplier_name: row.get(1)?,
            payment_deadline: row.get(2)?,
            net_minor: row.get(3)?,
            vat_minor: row.get(4)?,
            gross_minor: row.get(5)?,
            currency: row.get(6)?,
            local_status: row.get(7)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn query_restored_rows(
    conn: &Connection,
    tenant: &str,
    window: DateWindow,
) -> Result<Vec<RestoredRow>> {
    let date_col = date_col_sql_restored();
    let mut clauses = vec!["r.tenant_id = ?".to_string()];
    let mut binds: Vec<String> = vec![tenant.to_string()];
    if let Some(from) = window.from {
        clauses.push(format!("{date_col} >= ?"));
        binds.push(date_str(from));
    }
    if let Some(to) = window.to {
        clauses.push(format!("{date_col} <= ?"));
        binds.push(date_str(to));
    }
    let sql = format!(
        "SELECT r.customer_name,
                r.total_net_minor, r.total_vat_minor, r.total_gross_minor,
                r.currency, r.partner_id
           FROM restored_invoice r
          WHERE {}",
        clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql).context("prepare restored_invoice SQL")?;
    let params_dyn: Vec<&dyn duckdb::ToSql> =
        binds.iter().map(|s| s as &dyn duckdb::ToSql).collect();
    let rows = stmt.query_map(params_dyn.as_slice(), |row| {
        Ok(RestoredRow {
            customer_name: row.get(0)?,
            net_minor: row.get(1)?,
            vat_minor: row.get(2)?,
            gross_minor: row.get(3)?,
            currency: row.get(4)?,
            partner_id: row.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// The shared financial window's date-basis expression, normalised to a
/// date-only `YYYY-MM-DD` VARCHAR.
///
/// Every financial figure rides this one expression — revenue, the ÁFA
/// breakdown, receivables and their aging, the cash-flow projection, the
/// EUR/HUF currency split, the DSO windowing — so it is a single `const`
/// rather than a string repeated per WHERE arm: a fork here is a fork in
/// the money, and it survives until the first edit to one copy.
///
/// The comparison against the operator's bounds is a **string** compare, and
/// the bounds are date-only (`date_str`). The column is not: in production
/// `invoice.issue_date` is `VARCHAR NOT NULL` holding RFC3339 —
/// `draft.issue_date.format(&Rfc3339)` in
/// `modules/billing/src/adapters/duckdb_store.rs`, e.g.
/// `2026-06-30T12:00:00Z`. So the raw column made the period's upper bound
/// read `'2026-06-30T12:00:00Z' <= '2026-06-30'`, which is FALSE — every
/// character up to the bound's length is equal and the column is longer. An
/// invoice issued on the period's LAST day with no `delivery_date` fell out
/// of the period entirely, and being the SHARED predicate it left revenue,
/// VAT and AR together. An under-count with nothing on the tile to show a
/// row went missing (rule 11).
///
/// `SUBSTR(CAST(… AS VARCHAR), 1, 10)` is the same tactic the DSO anchor
/// uses, and it is correct for all three shapes `issue_date` takes in the
/// field: it truncates RFC3339 to its date head, is a no-op on a date-only
/// VARCHAR (hand-seeded / imported rows), and renders a legacy DATE-typed
/// column as `YYYY-MM-DD`. The `CAST` on **both** COALESCE members is
/// load-bearing beyond the truncation: with a DATE-typed `issue_date` the
/// old `COALESCE(CAST(delivery_date AS VARCHAR), issue_date)` could not
/// bind at all ("Cannot mix values of type VARCHAR and DATE in COALESCE"),
/// failing the WHOLE financial report rather than one panel.
///
/// The window semantics are otherwise untouched: same basis column
/// (`delivery_date`, falling back to `issue_date` — `CAST(NULL)` is NULL, so
/// the COALESCE still selects the same member), same inclusive bounds. Only
/// the compare is corrected from lexicographic-on-mixed-shapes to
/// date-vs-date.
const WINDOW_DATE_EXPR: &str =
    "SUBSTR(COALESCE(CAST(i.delivery_date AS VARCHAR), CAST(i.issue_date AS VARCHAR)), 1, 10)";

fn build_date_where(window: DateWindow) -> (String, bool, bool) {
    let date_col = WINDOW_DATE_EXPR;
    // The outgoing query always supplies the column via `date_col_sql_invoice`;
    // here we just emit the WHERE shape and report which bind slots are used.
    // We re-use the `i.issue_date`/`i.delivery_date` reference at the
    // caller's call site via `{where_clause}` interpolation; this helper
    // produces only the canonical default. The outgoing query embeds the
    // date column directly via {date_col} in its format!() — this helper
    // is unused by it. (Kept for ap/restored which build their WHERE
    // dynamically above.)
    let _ = date_col;
    match (window.from, window.to) {
        (Some(_), Some(_)) => (
            format!("WHERE {WINDOW_DATE_EXPR} >= ? AND {WINDOW_DATE_EXPR} <= ?"),
            true,
            true,
        ),
        (Some(_), None) => (format!("WHERE {WINDOW_DATE_EXPR} >= ?"), true, false),
        (None, Some(_)) => (format!("WHERE {WINDOW_DATE_EXPR} <= ?"), false, true),
        (None, None) => ("".into(), false, false),
    }
}

// ──────────────────────────────────────────────────────────────────────
// Aggregation.
// ──────────────────────────────────────────────────────────────────────

/// Per-(currency, vat_rate) accumulator key used while bucketing the
/// outgoing line groups.
type VatBucketKey = (String, i32);

#[derive(Default)]
struct OutgoingAggregate {
    revenue: CurrencyAggregate,
    vat_collected: CurrencyAggregate,
    receivables: CurrencyAggregate,
    receivables_aging: AgingPanel,
    cashflow_forward: CashflowPanel,
    vat_breakdown: BTreeMap<VatBucketKey, (i64, i64)>,
    top_customers: HashMap<(String, String), (i64, u64)>, // (label, currency) -> (gross, count)
    /// (paid_at - issue_date) sample for DSO calculation, by currency.
    dso_huf_samples: Vec<f64>,
    dso_eur_samples: Vec<f64>,
    counted_invoice_ids: HashSet<String>,
    outstanding_past_deadline_count: u64,
    /// Receivables excluded from outstanding as settled because they have
    /// no recorded deadline — see [`aging_placement`].
    aging_settled_undated: SettledUndated,
    rejected_count: u64,
    abandoned_count: u64,
    pending_count: u64,
}

/// Aggregate outgoing native invoices over the windowed line groups,
/// classifying each invoice via the trace map and flipping the sign for
/// storno-self rows.
fn aggregate_outgoing(
    groups: Vec<OutgoingLineGroup>,
    traces: &HashMap<String, ReportTrace>,
    today: Date,
    buyer_names: &HashMap<String, String>,
) -> OutgoingAggregate {
    let mut agg = OutgoingAggregate::default();
    // Per-invoice aggregator: collapse multiple rows-per-invoice (one
    // per VAT rate) into one count + one gross.
    let mut per_invoice: HashMap<String, (String, i64, i64, Option<String>, String, bool)> =
        HashMap::new(); // id -> (currency, net, vat, payment_deadline, issue_date, is_storno_self)
    for group in groups {
        let trace = traces.get(&group.invoice_id).cloned().unwrap_or_default();
        let kind = trace.classify();
        match kind {
            CountedKind::Counted { is_storno_self } => {
                let sign: i64 = if is_storno_self { -1 } else { 1 };
                let net_signed = group.net_minor.saturating_mul(sign);
                // ADR-0108 M-2 — the VAT is the group's per-line sum computed
                // in `fold_outgoing_lines` through the filing's own
                // `line_vat_amount`, signed here. It is NOT re-derived from
                // `net_signed × basis_points`: the group net has already lost
                // the per-line truncation remainders, so re-deriving would
                // reintroduce the very divergence this fold removes.
                let vat_signed = group.vat_minor.saturating_mul(sign);
                let entry = agg
                    .vat_breakdown
                    .entry((group.currency.clone(), group.vat_rate_basis_points))
                    .or_insert((0, 0));
                entry.0 = entry.0.saturating_add(net_signed);
                entry.1 = entry.1.saturating_add(vat_signed);
                let inv_entry = per_invoice.entry(group.invoice_id.clone()).or_insert((
                    group.currency.clone(),
                    0,
                    0,
                    group.payment_deadline.clone(),
                    group.issue_date.clone(),
                    is_storno_self,
                ));
                inv_entry.1 = inv_entry.1.saturating_add(net_signed);
                inv_entry.2 = inv_entry.2.saturating_add(vat_signed);
                agg.counted_invoice_ids.insert(group.invoice_id.clone());
            }
            CountedKind::Rejected => {
                agg.rejected_count = agg.rejected_count.saturating_add(1);
                // Don't double-count per VAT row — collapse into a set.
            }
            CountedKind::Abandoned => {
                agg.abandoned_count = agg.abandoned_count.saturating_add(1);
            }
            CountedKind::PendingDraft => {
                agg.pending_count = agg.pending_count.saturating_add(1);
            }
            CountedKind::Unknown => {}
        }
    }
    // De-duplicate the hygiene counters (per VAT rate produces N rows
    // per invoice; the counter should count invoices, not rows).
    let mut seen_rejected: HashSet<String> = HashSet::new();
    let mut seen_abandoned: HashSet<String> = HashSet::new();
    let mut seen_pending: HashSet<String> = HashSet::new();
    agg.rejected_count = 0;
    agg.abandoned_count = 0;
    agg.pending_count = 0;
    for (id, trace) in traces {
        match trace.classify() {
            CountedKind::Rejected if seen_rejected.insert(id.clone()) => {
                agg.rejected_count += 1;
            }
            CountedKind::Abandoned if seen_abandoned.insert(id.clone()) => {
                agg.abandoned_count += 1;
            }
            CountedKind::PendingDraft if seen_pending.insert(id.clone()) => {
                agg.pending_count += 1;
            }
            _ => {}
        }
    }
    // Materialise per-invoice contributions into the currency aggregate
    // + receivables + DSO + top-customers.
    for (id, (currency, net, vat, deadline, issue_date, is_storno_self)) in &per_invoice {
        let gross = net.saturating_add(*vat);
        let target = match currency.as_str() {
            "EUR" => &mut agg.revenue.eur,
            _ => &mut agg.revenue.huf,
        };
        target.net_minor = target.net_minor.saturating_add(*net);
        target.vat_minor = target.vat_minor.saturating_add(*vat);
        target.gross_minor = target.gross_minor.saturating_add(gross);
        target.count = target.count.saturating_add(1);
        // VAT collected — same totals; in v1 VAT-collected mirrors the
        // VAT line of revenue. (When AAM / reverse-charge sub-bucketing
        // lands, those rows will be excluded from this aggregate.)
        let vat_target = match currency.as_str() {
            "EUR" => &mut agg.vat_collected.eur,
            _ => &mut agg.vat_collected.huf,
        };
        vat_target.gross_minor = vat_target.gross_minor.saturating_add(*vat);
        vat_target.vat_minor = vat_target.vat_minor.saturating_add(*vat);
        vat_target.count = vat_target.count.saturating_add(1);
        // Receivables: counted-but-not-paid. BOTH halves of a LANDED
        // storno pair are excluded — the storno-self row (the −amount credit note, which
        // is self-resolving: the negation IS the payment) AND the cancelled
        // base it reverses. A voided invoice is not *partially* receivable,
        // it is not receivable: nobody owes it. Excluding only the
        // counterpart dropped the −amount while leaving the base's +amount
        // in AR, so the equal-and-opposite pair never netted and the
        // cancelled original leaked into Receivables, its aging bucket, the
        // past-deadline count and (via the `Current` branch below) the
        // cash-flow forward projection. Exclude-both rather than netting via
        // the chain link: netting would still count the pair as 2 open
        // receivables and would depend on both halves landing in the same
        // window.
        //
        // LANDED is load-bearing, and it is why the flag is resolved after
        // the walk rather than at the chain link (see
        // `resolve_landed_stornos`). A storno that was ABORTED at NAV or
        // never submitted cancels nothing: the base is still owed, revenue
        // still carries its +amount, and dropping it from AR would silently
        // erase a real receivable — an under-count nobody can see on the
        // tile, unlike the over-count that got us here (rule 11). This
        // predicate tracks revenue exactly: the pair nets to zero in revenue
        // iff the child classifies as `Counted`, and AR excludes both halves
        // iff the same.
        let trace = traces.get(id).cloned().unwrap_or_default();
        let paid = trace.payment_paid_at.is_some();
        if !paid && !*is_storno_self && !trace.has_landed_storno {
            // The deadline is read FIRST, above the total, and its verdict
            // gates everything below it. A receivable with no recorded
            // deadline is a settled legacy import (see `aging_placement`)
            // and is excluded from the total, from every aging bucket,
            // from the past-deadline counter and from the cash-flow
            // projection — all four together, from this one decision, so
            // no two of them can disagree. Ordering is load-bearing: the
            // previous shape added to the total first and then bucketed
            // unconditionally, and moving only one of the two would
            // reopen the sum(buckets) != total defect in the other
            // direction.
            if let Some((bucket, deadline_d)) = aging_placement(
                today,
                id,
                deadline.as_deref(),
                currency,
                gross,
                &mut agg.aging_settled_undated,
            ) {
                let ar_target = match currency.as_str() {
                    "EUR" => &mut agg.receivables.eur,
                    _ => &mut agg.receivables.huf,
                };
                ar_target.net_minor = ar_target.net_minor.saturating_add(*net);
                ar_target.vat_minor = ar_target.vat_minor.saturating_add(*vat);
                ar_target.gross_minor = ar_target.gross_minor.saturating_add(gross);
                ar_target.count = ar_target.count.saturating_add(1);
                accrue_aging(&mut agg.receivables_aging, bucket, *net, *vat, gross);
                // Reachable only with a deadline we actually read, so the
                // bucket is a measurement and this counter is entitled to
                // assert lateness from it.
                if !matches!(bucket, AgingBucket::Current) {
                    agg.outstanding_past_deadline_count =
                        agg.outstanding_past_deadline_count.saturating_add(1);
                }
                // Forward look only for not-yet-overdue receivables.
                if matches!(bucket, AgingBucket::Current) {
                    let days_out = (deadline_d - today).whole_days();
                    let pair_target = match currency.as_str() {
                        "EUR" => |p: &mut CurrencyPair, v: i64| {
                            p.eur_minor = p.eur_minor.saturating_add(v)
                        },
                        _ => |p: &mut CurrencyPair, v: i64| {
                            p.huf_minor = p.huf_minor.saturating_add(v)
                        },
                    };
                    if days_out <= 30 {
                        pair_target(&mut agg.cashflow_forward.next_30, gross);
                    }
                    if days_out <= 60 {
                        pair_target(&mut agg.cashflow_forward.next_60, gross);
                    }
                    if days_out <= 90 {
                        pair_target(&mut agg.cashflow_forward.next_90, gross);
                    }
                }
            }
        }
        // DSO sample — paid invoice (not storno-self), days between
        // paid_at and the invoice's issue_date.
        if !*is_storno_self {
            if let (Some(paid_at), _) = (&trace.payment_paid_at, &trace.payment_amount_minor) {
                // Use issue_date for DSO: that's the regulatory "sales date"
                // anchor for credit-to-cash timing. It used to read the
                // group's date-basis column instead — under Teljesites that
                // is COALESCE(delivery_date, issue_date), so every advance /
                // prepayment (paid after issue but before fulfillment, a
                // routine arrangement) produced a NEGATIVE days-to-pay and
                // dragged the published DSO below zero.
                if let (Ok(paid_d), Ok(issued_d)) =
                    (parse_iso_date(paid_at), parse_iso_date(issue_date))
                {
                    let days = (paid_d - issued_d).whole_days() as f64;
                    if currency.as_str() == "EUR" {
                        agg.dso_eur_samples.push(days);
                    } else {
                        agg.dso_huf_samples.push(days);
                    }
                }
            }
        }
        // Top customers — keyed by buyer_name lookup (best-effort).
        if let Some(name) = buyer_names.get(id) {
            let key = (name.clone(), currency.clone());
            let entry = agg.top_customers.entry(key).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(gross);
            entry.1 = entry.1.saturating_add(1);
        }
    }
    agg
}

#[derive(Debug, Clone, Copy)]
enum AgingBucket {
    Current,
    Days1To30,
    Days31To60,
    Days61To90,
    Days90Plus,
}

fn aging_bucket_for(today: Date, deadline: Date) -> AgingBucket {
    let overdue_days = (today - deadline).whole_days();
    if overdue_days <= 0 {
        AgingBucket::Current
    } else if overdue_days <= 30 {
        AgingBucket::Days1To30
    } else if overdue_days <= 60 {
        AgingBucket::Days31To60
    } else if overdue_days <= 90 {
        AgingBucket::Days61To90
    } else {
        AgingBucket::Days90Plus
    }
}

/// Per-side tally of invoices EXCLUDED from outstanding as settled
/// because they carry no recorded deadline. Merged onto
/// [`LedgerDiagnostics`] when the report is assembled.
///
/// The gross is kept per currency and never summed across them: HUF minor
/// units are forints and EUR minor units are cents, so one combined
/// number would be arithmetic on unlike things. It exists to give the
/// aggregate warning a magnitude — "42 rows" says nothing about whether
/// this quietly swallowed a 12 M HUF payable.
#[derive(Default)]
struct SettledUndated {
    count: u64,
    ids: Vec<String>,
    huf_gross_minor: i64,
    eur_gross_minor: i64,
}

impl SettledUndated {
    fn record(&mut self, invoice_id: &str, currency: &str, gross_minor: i64) {
        self.count = self.count.saturating_add(1);
        if self.ids.len() < MAX_UNPARSEABLE_ENTRY_IDS {
            self.ids.push(invoice_id.to_string());
        }
        // Same "EUR or else HUF" fallback the amount aggregates use, so a
        // row can never be counted in `count` yet dropped from both
        // amounts.
        let bucket = match currency {
            "EUR" => &mut self.eur_gross_minor,
            _ => &mut self.huf_gross_minor,
        };
        *bucket = bucket.saturating_add(gross_minor);
    }
}

/// Decide whether ONE otherwise-outstanding invoice (receivable or
/// payable) belongs in outstanding at all, and if so which aging bucket
/// it lands in.
///
/// `Some((bucket, deadline))` — the deadline was recorded and readable.
/// This invoice is outstanding: the caller adds it to the receivables /
/// payables total, accrues it into `bucket`, and may assert lateness on
/// it.
///
/// `None` — there is NO recorded deadline (the column is NULL, or holds a
/// string that will not parse). The invoice is treated as SETTLED and the
/// caller must exclude it from the total, from every bucket, and from the
/// past-deadline counters. It is tallied into `settled` instead.
///
/// **The invariant this exists to hold: every invoice counted in the
/// receivables / payables TOTAL lands in exactly one aging bucket, so
/// `sum(buckets) == total`, always.** The two decisions are taken HERE,
/// together, from one reading of the deadline, which is why they cannot
/// disagree. Both aging sites used to read `if let Ok(d) =
/// parse_iso_date(..)` with no `else`, nested inside `if let
/// Some(deadline)` — so a deadline-less invoice fell through BOTH arms
/// into nothing while the lines just above had already added it to the
/// total. The panel's own breakdown summed to less than the panel's own
/// headline, with no signal anywhere: the operator reads "Receivables
/// 4 500 000" over buckets adding to 3 200 000 and has no way to learn
/// which is the lie. Same silent-drop class as the audit entries fixed in
/// PR #67, different code path. The invariant now holds by EXCLUSION from
/// both sides rather than by imputing a bucket.
///
/// **Why deadline-less means settled.** These are legacy invoices
/// issued, and paid, under the prior system, which recorded no payment
/// deadline for them. The operator's ruling is that they are all settled.
///
/// On the AR side that ruling is backed by a MIGRATION TIMELINE, not by
/// input validation — the distinction matters, because validation would
/// only constrain what callers may send, and it is the timeline that
/// constrains what the column can hold:
///
///   * `MIGRATE_PR_84_SQL` (billing `duckdb_store.rs`) added
///     `payment_deadline DATE` to `invoice` with **no backfill** — by
///     design, since pre-PR-84 rows had no operator-chosen date to
///     recover. Every row already in the table got NULL.
///   * Post-PR-84 the column cannot become NULL again:
///     `DraftInvoice::payment_deadline` is a non-`Option` `time::Date`,
///     `issue_invoice` defaults a missing input to the issue date rather
///     than passing nothing through, and the store always formats and
///     binds a canonical `YYYY-MM-DD`.
///   * The migration landed 2026-05-27; the earliest shipped release is
///     PROD_v1.4.1 (2026-05-31). No release predates it.
///
/// So a NULL AR deadline can only be a row written before that
/// migration — genuinely pre-PR-84, genuinely legacy. It is NOT the case
/// that `issue_invoice`'s validation makes this arm unreachable for
/// current-system AR: that check only rejects a malformed deadline a
/// caller *supplied*, and rejecting bad input says nothing about rows
/// that were never given the column at all.
///
/// The consequence is real and was reproduced adversarially: an UNPAID
/// pre-PR-84 receivable leaves the Receivables total silently. That is
/// the operator's ruling working as intended, not a bug — but it is why
/// the count below is not optional.
///
/// **The residual, and why the tally is not optional.** AP is not
/// symmetric: `ap_sync::digest_to_ingestion_input` writes
/// `payment_deadline: None` on every NAV-synced payable (ap_sync.rs:971),
/// and that sync is ONGOING. A genuinely unpaid payable arriving today
/// with no deadline would be swept up by this same treatment and vanish
/// from the payables total — an under-count, the error class that costs
/// money and that nobody can see on a tile. Nothing in this function can
/// tell that row from a settled legacy one. So it is made countable
/// instead: [`LedgerDiagnostics::aging_settled_undated`] on the wire, plus
/// the aggregate `tracing::warn!` in `build_financial_report` carrying the
/// count AND the excluded gross. That warning is the tripwire — if the
/// excluded figure ever moves on a book that is not being migrated,
/// something real got excluded.
///
/// A settled invoice is still an expense and still revenue: this decides
/// only what is OUTSTANDING. The expense / revenue / VAT aggregates are
/// untouched by it.
///
/// Called once per otherwise-outstanding invoice per aggregation run. The
/// comparative windows (MoM / YoY / annual-running) run the same
/// aggregation over their own windows; only the primary window's tally
/// rides out on the wire.
fn aging_placement(
    today: Date,
    invoice_id: &str,
    deadline: Option<&str>,
    currency: &str,
    gross_minor: i64,
    settled: &mut SettledUndated,
) -> Option<(AgingBucket, Date)> {
    match deadline {
        Some(raw) => match parse_iso_date(raw) {
            Ok(parsed) => Some((aging_bucket_for(today, parsed), parsed)),
            Err(error) => {
                // A deadline that is PRESENT but malformed is genuinely
                // wrong data — both writers validate the shape on the way
                // in (`incoming_invoices::validate`, the billing store),
                // so reaching here means something bypassed them. It is
                // ALSO now a reason a row leaves outstanding, which makes
                // it worth an error line each; unlike the `None` arm below
                // it is rare by construction.
                tracing::error!(
                    invoice_id = %invoice_id,
                    payment_deadline = %raw,
                    parse_error = %error,
                    "financial report: outstanding invoice has an UNREADABLE payment_deadline \
                     — treated as a settled legacy import and EXCLUDED from outstanding \
                     (total, aging buckets and past-deadline counters alike); if this invoice \
                     is in fact unpaid, fix the deadline and it returns"
                );
                settled.record(invoice_id, currency, gross_minor);
                None
            }
        },
        None => {
            // An ABSENT deadline is not an error and must not be logged
            // like one: `ap_sync` records `payment_deadline: None` on
            // every NAV-synced payable (ap_sync.rs:971), so at error level
            // this would emit a line per payable per dashboard load and
            // drown the ledger diagnostics that ARE errors. The honest
            // signal is the aggregate — one WARN summary at the end of
            // the report, carrying the count and the excluded gross, plus
            // the machine-countable
            // `LedgerDiagnostics::aging_settled_undated` — with the
            // per-invoice attribution kept at debug for whoever is
            // actually chasing one.
            tracing::debug!(
                invoice_id = %invoice_id,
                "financial report: invoice has NO payment_deadline — treated as a settled \
                 legacy import and excluded from outstanding"
            );
            settled.record(invoice_id, currency, gross_minor);
            None
        }
    }
}

/// The bucket's slot on a panel — the one place the bucket→field mapping
/// lives, so the two aging sites cannot drift apart.
fn aging_slot(panel: &mut AgingPanel, bucket: AgingBucket) -> &mut AmountAggregate {
    match bucket {
        AgingBucket::Current => &mut panel.current,
        AgingBucket::Days1To30 => &mut panel.days_1_30,
        AgingBucket::Days31To60 => &mut panel.days_31_60,
        AgingBucket::Days61To90 => &mut panel.days_61_90,
        AgingBucket::Days90Plus => &mut panel.days_90_plus,
    }
}

/// Add one invoice's amounts to its aging bucket.
fn accrue_aging(panel: &mut AgingPanel, bucket: AgingBucket, net: i64, vat: i64, gross: i64) {
    let dest = aging_slot(panel, bucket);
    dest.net_minor = dest.net_minor.saturating_add(net);
    dest.vat_minor = dest.vat_minor.saturating_add(vat);
    dest.gross_minor = dest.gross_minor.saturating_add(gross);
    dest.count = dest.count.saturating_add(1);
}

// ──────────────────────────────────────────────────────────────────────
// Top-level orchestration.
// ──────────────────────────────────────────────────────────────────────

/// Compute the financial report for the given period + date basis.
///
/// Reads the audit ledger (one walk) + three SQL aggregates against the
/// invoice + restored_invoice + ap_invoice tables; combines into a
/// single JSON snapshot. Computes MoM + YoY deltas by re-running the
/// SQL aggregates against the prior periods (the audit-ledger walk is
/// re-used).
pub fn compute_financial_report(
    db: &aberp_db::Handle,
    tenant: TenantId,
    binary_hash: BinaryHash,
    req: ReportRequest,
) -> Result<FinancialReport> {
    let window = resolve_window(req.period)?;

    // ADR-0110 D8 (GROUP-A sweep): this fn used to open FOUR independent DuckDB
    // instances on the live tenant path — two `Connection::open`, one
    // `DuckDbBillingStore::open`, one `Ledger::open` — each of which folds and
    // TRUNCATES the serve Handle's WAL on close. Opening the Financial Report was
    // therefore enough to make the next invoice-issue commit reach no file. All
    // four now ride the ONE shared Handle.
    //
    // Ensure relevant schemas exist (idempotent; mirrors how the existing
    // list endpoints lazily ensure schema on first read). Billing-side
    // schema is bootstrapped via the typed store so the `invoice` +
    // `invoice_line` tables exist on a fresh DB; the AP and restored
    // mirrors carry their own idempotent CREATE. These are CREATE TABLE IF NOT
    // EXISTS statements, so they take the shared WRITER, in its own scope — the
    // guard MUST drop before the `db.read()` below (a nested read under a held
    // write guard self-deadlocks on the non-reentrant writer mutex).
    {
        use aberp_billing::ports::storage::BillingStore;
        let guard = db
            .write()
            .context("acquire shared writer for financial-report schema bootstrap")?;
        // ADR-0110 D8 / F2 — the audit schema, restored. `Ledger::open` used to
        // `initialise` (⇒ `ensure_schema`) as a side effect of opening; the
        // `Ledger::from_connection` that replaced it does NOT, so the D8 sweep
        // silently dropped this bootstrap and the report errored with "Table
        // audit_ledger does not exist" on a DB that had none (pre-sweep: an empty
        // walk). Latent in-serve because boot ensures it first — but the comment
        // below claimed a bootstrap that was no longer happening, and the three
        // sibling migrations in the same change (DÁP boot, DÁP heartbeat, MES
        // write_one) each kept theirs explicitly. Match them.
        aberp_audit_ledger::ensure_schema(&guard)
            .context("ensure audit-ledger schema for financial report")?;
        let _ = crate::incoming_invoices::ensure_schema(&guard);
        let _ = crate::restore_from_nav_outgoing::ensure_schema(&guard);
        let mut store = aberp_billing::DuckDbBillingStore::from_connection(
            guard
                .try_clone()
                .context("try_clone shared writer for billing schema bootstrap")?,
        );
        store
            .ensure_schema()
            .context("ensure billing-side schema for financial report")?;
    }
    let conn = db
        .read()
        .context("acquire shared reader for financial report")?;

    let tenant_str = tenant.as_str().to_string();
    // The ledger walk rides its own try_clone of the SAME instance, so it sees the
    // WAL-resident audit rows the in-serve writers have committed but not
    // checkpointed — a fresh `Ledger::open` saw only the folded subset, which is
    // exactly how a report could under-report issued invoices.
    let ledger = Ledger::from_connection(
        db.read()
            .context("acquire shared reader for financial-report ledger walk")?,
        tenant.clone(),
        binary_hash,
    );
    let walk = walk_ledger(&ledger, window)?;
    // The walk already error-logged each undecodable entry individually.
    // This is the one operator-facing line that says the SNAPSHOT as a
    // whole is suspect, and it rides out on the report so a caller/UI can
    // say so too.
    if walk.diagnostics.unparseable_entries > 0 {
        tracing::error!(
            unparseable_entries = walk.diagnostics.unparseable_entries,
            "financial report: figures may be INCOMPLETE — some audit entries could not be read"
        );
    }

    // Build a best-effort buyer-name map by reading side-store input.json
    // files for each `InvoiceDraftCreated` entry's `nav_xml_path`. Same
    // posture `serve::list_invoices` takes (S215). Best-effort: missing /
    // unreadable / blank → no entry.
    let buyer_names = build_buyer_names_map(&ledger)?;

    let outgoing_groups = query_outgoing_groups(&conn, window, req.date_basis)?;
    let ap_rows = query_ap_rows(&conn, &tenant_str, window, req.date_basis)?;
    let restored_rows = query_restored_rows(&conn, &tenant_str, window)?;

    let mut outgoing = aggregate_outgoing(outgoing_groups, &walk.traces, req.today, &buyer_names);

    // S262 / PR-251 — capture the NATIVE outgoing revenue (canonical
    // `invoice` table only, storno-adjusted) BEFORE the restored-mirror
    // loop folds digest rows into `outgoing.revenue`. The currency split
    // is snapshot-rate based and restored/AP rows carry no per-invoice
    // snapshot rate, so the split must exclude them.
    let native_revenue = outgoing.revenue.clone();
    let eur_as_huf_minor = query_eur_huf_equivalent(&conn, window, req.date_basis)?;
    let currency_split = CurrencySplitPanel {
        huf_minor: native_revenue.huf.gross_minor,
        huf_count: native_revenue.huf.count,
        eur_native_minor: native_revenue.eur.gross_minor,
        eur_count: native_revenue.eur.count,
        eur_as_huf_minor,
    };

    // Restored rows contribute to revenue + VAT-collected. No line-level
    // breakdown available (digest-only). No storno detection (the
    // restored mirror is read-only).
    for r in &restored_rows {
        let target = match r.currency.as_str() {
            "EUR" => &mut outgoing.revenue.eur,
            _ => &mut outgoing.revenue.huf,
        };
        target.net_minor = target.net_minor.saturating_add(r.net_minor);
        target.vat_minor = target.vat_minor.saturating_add(r.vat_minor);
        target.gross_minor = target.gross_minor.saturating_add(r.gross_minor);
        target.count = target.count.saturating_add(1);
        let vat_target = match r.currency.as_str() {
            "EUR" => &mut outgoing.vat_collected.eur,
            _ => &mut outgoing.vat_collected.huf,
        };
        vat_target.vat_minor = vat_target.vat_minor.saturating_add(r.vat_minor);
        vat_target.gross_minor = vat_target.gross_minor.saturating_add(r.vat_minor);
        vat_target.count = vat_target.count.saturating_add(1);
        // Top customers — restored rows carry buyer_name in-row (S218).
        if let Some(name) = &r.customer_name {
            let key = (name.clone(), r.currency.clone());
            let entry = outgoing.top_customers.entry(key).or_insert((0, 0));
            entry.0 = entry.0.saturating_add(r.gross_minor);
            entry.1 = entry.1.saturating_add(1);
        }
    }

    let ap = aggregate_ap(&ap_rows, req.today);
    let payable_past_deadline = ap.payable_past_deadline;

    let deferred_notes: Vec<String> = vec![
        "Revenue currency split now ships in snapshot-rate HUF (S262); FX-aggregated \
         expenses + a unified all-in-HUF P&L line remain deferred."
            .into(),
        "HIPA base + KATA/KIVA threshold logic deferred to v2.3 (separate ADR).".into(),
        "AAM / reverse-charge / EU-0 VAT sub-buckets deferred — schema does not tag them today."
            .into(),
        "Per-VAT-rate breakdown for incoming + restored deferred (digest-only ingestion in v1)."
            .into(),
    ];

    // Period-over-period deltas — re-run the aggregates over the prior
    // comparable window. Custom + All periods get None.
    let mom = compute_delta(
        &conn,
        &tenant_str,
        &walk.traces,
        &buyer_names,
        req.date_basis,
        req.today,
        previous_period(req.period),
        &outgoing.revenue,
        &ap.expenses,
    )?;
    let yoy = compute_delta(
        &conn,
        &tenant_str,
        &walk.traces,
        &buyer_names,
        req.date_basis,
        req.today,
        yoy_period(req.period),
        &outgoing.revenue,
        &ap.expenses,
    )?;

    // Annual running revenue — YTD up to today for the year that
    // contains `today`. Uses the same date basis as the current request.
    let annual_running = compute_annual_running(
        &conn,
        &tenant_str,
        &walk.traces,
        &buyer_names,
        req.date_basis,
        req.today,
    )?;

    // VAT breakdown → wire shape (sorted by rate DESC for the UI).
    let vat_breakdown_outgoing: Vec<VatRateBreakdownEntry> = outgoing
        .vat_breakdown
        .into_iter()
        .map(|((currency, rate_bp), (net, vat))| VatRateBreakdownEntry {
            rate_basis_points: rate_bp,
            currency,
            net_minor: net,
            vat_minor: vat,
        })
        .collect();

    // Top-N — sort by gross_minor DESC, take the operator-chosen N.
    let top_customers = top_n_from_map(outgoing.top_customers, req.top_n);
    let top_vendors = top_n_from_map(ap.top_vendors, req.top_n);

    // Hygiene panel — combine outgoing + ap + restored signals.
    let restored_no_partner_count = restored_rows
        .iter()
        .filter(|r| r.partner_id.is_none())
        .count() as u64;
    let hygiene = HygienePanel {
        outgoing_rejected_count: outgoing.rejected_count,
        outgoing_abandoned_count: outgoing.abandoned_count,
        outgoing_pending_count: outgoing.pending_count,
        restored_no_partner_count,
        outstanding_past_deadline_count: outgoing.outstanding_past_deadline_count,
        payable_past_deadline_count: payable_past_deadline,
        storno_chain_count: walk.storno_links_in_period,
        modification_chain_count: walk.modification_links_in_period,
    };

    // Gross profit + VAT-to-pay deltas.
    let gross_profit = CurrencyPair {
        huf_minor: outgoing
            .revenue
            .huf
            .gross_minor
            .saturating_sub(ap.expenses.huf.gross_minor),
        eur_minor: outgoing
            .revenue
            .eur
            .gross_minor
            .saturating_sub(ap.expenses.eur.gross_minor),
    };
    let vat_to_pay = CurrencyPair {
        huf_minor: outgoing
            .vat_collected
            .huf
            .vat_minor
            .saturating_sub(ap.vat_paid.huf.vat_minor),
        eur_minor: outgoing
            .vat_collected
            .eur
            .vat_minor
            .saturating_sub(ap.vat_paid.eur.vat_minor),
    };

    // Merge the two settled-undated tallies onto the walk's diagnostics —
    // one integrity object goes out on the wire.
    //
    // The COUNT is exact. The id list is a sample, and honestly so: each
    // side already capped its own ids at `MAX_UNPARSEABLE_ENTRY_IDS` while
    // collecting, so the order here is cap → merge → sort → cap. The
    // published ids are therefore a sorted subset of "the first 50 each
    // side happened to see", NOT the 50 smallest ids overall — and on the
    // AR side "happened to see" is `HashMap` iteration order, which is not
    // stable between runs. The sort buys presentation determinism for a
    // given set, not a deterministic set. That is the same
    // count-exact/ids-are-a-starting-point contract
    // `unparseable_entry_ids` ships under; anyone needing the full list
    // reads the log.
    let mut ledger_diagnostics = walk.diagnostics;
    ledger_diagnostics.aging_settled_undated_receivables = outgoing.aging_settled_undated.count;
    ledger_diagnostics.aging_settled_undated_payables = ap.aging_settled_undated.count;
    ledger_diagnostics.aging_settled_undated = outgoing
        .aging_settled_undated
        .count
        .saturating_add(ap.aging_settled_undated.count);
    let mut settled_ids = outgoing.aging_settled_undated.ids;
    settled_ids.extend(ap.aging_settled_undated.ids.iter().cloned());
    settled_ids.sort();
    settled_ids.truncate(MAX_UNPARSEABLE_ENTRY_IDS);
    ledger_diagnostics.aging_settled_undated_invoice_ids = settled_ids;
    // THE TRIPWIRE. Once per report rather than once per invoice, and
    // carrying the money, not just the row count.
    //
    // Every row named here was DROPPED from the outstanding figures on
    // the strength of a rule — "no recorded deadline means a settled
    // legacy import" — that this code cannot verify per row. On AR the
    // rule is bounded by the PR-84 migration timeline: the column was
    // added without backfill and cannot go NULL again, so a NULL there
    // is necessarily a pre-PR-84 row (see `aging_placement`). On AP it
    // is not bounded at all: `ap_sync` writes `payment_deadline: None` on every
    // NAV-synced payable (ap_sync.rs:971) and that sync is ONGOING, so a
    // genuinely unpaid deadline-less payable arriving tomorrow would be
    // excluded by exactly this rule and would simply not appear in
    // payables. An under-count is the failure nobody sees on a tile,
    // which is why it gets a warning with a magnitude attached rather
    // than a silent count on the wire: a payables figure that moves here
    // on a book that is not being migrated is the signal.
    //
    // WARN rather than ERROR because on a legacy book this condition is
    // expected and universal, and an error per load teaches the operator
    // to ignore the log — which would cost us the very signal above.
    // Amounts are per currency: HUF minor units are forints, EUR minor
    // units are cents, and one summed number would be meaningless.
    if ledger_diagnostics.aging_settled_undated > 0 {
        tracing::warn!(
            aging_settled_undated = ledger_diagnostics.aging_settled_undated,
            receivables = ledger_diagnostics.aging_settled_undated_receivables,
            payables = ledger_diagnostics.aging_settled_undated_payables,
            excluded_huf_gross_minor = outgoing
                .aging_settled_undated
                .huf_gross_minor
                .saturating_add(ap.aging_settled_undated.huf_gross_minor),
            excluded_eur_gross_minor = outgoing
                .aging_settled_undated
                .eur_gross_minor
                .saturating_add(ap.aging_settled_undated.eur_gross_minor),
            "financial report: invoices with NO recorded payment deadline were EXCLUDED from \
             outstanding as settled legacy imports — they are in no receivables/payables total, \
             no aging bucket and no past-deadline counter. Expected for the NAV legacy import; \
             but ap_sync stamps no deadline on ongoing NAV payable syncs too, so if the payables \
             figure here moves on a book that is not being migrated, a genuinely unpaid payable \
             has been excluded — check the ids in ledger_diagnostics"
        );
    }

    let dso_days = DsoPanel {
        huf_days: mean(&outgoing.dso_huf_samples),
        eur_days: mean(&outgoing.dso_eur_samples),
        huf_sample_size: outgoing.dso_huf_samples.len() as u64,
        eur_sample_size: outgoing.dso_eur_samples.len() as u64,
    };

    Ok(FinancialReport {
        period: PeriodMeta {
            kind: period_kind_label(req.period).into(),
            label: period_label(req.period),
            from: window.from.map(date_str),
            to: window.to.map(date_str),
            date_basis: req.date_basis.as_wire_str().into(),
            today: date_str(req.today),
        },
        revenue: outgoing.revenue,
        expenses: ap.expenses,
        gross_profit,
        vat_collected: outgoing.vat_collected,
        vat_paid: ap.vat_paid,
        vat_to_pay,
        receivables: outgoing.receivables,
        payables: ap.payables,
        currency_split,
        receivables_aging: outgoing.receivables_aging,
        payables_aging: ap.payables_aging,
        dso_days,
        cashflow_forward: outgoing.cashflow_forward,
        vat_breakdown_outgoing,
        top_customers,
        top_vendors,
        hygiene,
        deltas: PeriodDeltas { mom, yoy },
        annual_running,
        deferred_notes,
        ledger_diagnostics,
    })
}

#[derive(Default)]
struct ApAggregate {
    expenses: CurrencyAggregate,
    vat_paid: CurrencyAggregate,
    payables: CurrencyAggregate,
    payables_aging: AgingPanel,
    top_vendors: HashMap<(String, String), (i64, u64)>,
    payable_past_deadline: u64,
    /// Payables excluded from outstanding as settled because they have no
    /// recorded deadline — see [`aging_placement`]. The tripwire count:
    /// `ap_sync` is ONGOING and writes no deadline, so this is the side
    /// where a genuinely unpaid row could be swept up.
    aging_settled_undated: SettledUndated,
}

/// AP-side aggregation: expenses + VAT-paid + payables + payable-aging +
/// top vendors. Irrelevant rows are excluded from every bucket per the
/// S177 closed-vocab semantics (operator declared not-our-problem).
///
/// Extracted from `build_financial_report`'s body so the payables aging
/// invariant (`sum(buckets) == payables total`) is unit-testable without
/// standing up a DuckDB fixture — the AR side already was, and only the AR
/// side had pins.
fn aggregate_ap(rows: &[ApRow], today: Date) -> ApAggregate {
    let mut ap = ApAggregate::default();
    for r in rows {
        if r.local_status == "Irrelevant" {
            continue;
        }
        let exp_target = match r.currency.as_str() {
            "EUR" => &mut ap.expenses.eur,
            _ => &mut ap.expenses.huf,
        };
        exp_target.net_minor = exp_target.net_minor.saturating_add(r.net_minor);
        exp_target.vat_minor = exp_target.vat_minor.saturating_add(r.vat_minor);
        exp_target.gross_minor = exp_target.gross_minor.saturating_add(r.gross_minor);
        exp_target.count = exp_target.count.saturating_add(1);
        let vp_target = match r.currency.as_str() {
            "EUR" => &mut ap.vat_paid.eur,
            _ => &mut ap.vat_paid.huf,
        };
        vp_target.vat_minor = vp_target.vat_minor.saturating_add(r.vat_minor);
        vp_target.gross_minor = vp_target.gross_minor.saturating_add(r.vat_minor);
        vp_target.count = vp_target.count.saturating_add(1);
        // Top vendors
        let key = (r.supplier_name.clone(), r.currency.clone());
        let entry = ap.top_vendors.entry(key).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(r.gross_minor);
        entry.1 = entry.1.saturating_add(1);
        // Payables + aging — outstanding only.
        if r.local_status == "Outstanding" {
            // Deadline FIRST, above the total — same ordering and same
            // reason as the AR side. A payable with no recorded deadline
            // is treated as a settled legacy import and is excluded from
            // the payables total, from every bucket, and from the
            // past-deadline counter together.
            //
            // This is the side carrying the residual risk:
            // `ap_sync` stamps `payment_deadline: None` on EVERY
            // NAV-synced payable (ap_sync.rs:971) and that sync is
            // ongoing, so a real unpaid deadline-less payable would be
            // excluded by the same rule. Hence the tally and the
            // aggregate warning — see `aging_placement`.
            if let Some((bucket, _deadline_d)) = aging_placement(
                today,
                &r.id,
                r.payment_deadline.as_deref(),
                &r.currency,
                r.gross_minor,
                &mut ap.aging_settled_undated,
            ) {
                let p_target = match r.currency.as_str() {
                    "EUR" => &mut ap.payables.eur,
                    _ => &mut ap.payables.huf,
                };
                p_target.net_minor = p_target.net_minor.saturating_add(r.net_minor);
                p_target.vat_minor = p_target.vat_minor.saturating_add(r.vat_minor);
                p_target.gross_minor = p_target.gross_minor.saturating_add(r.gross_minor);
                p_target.count = p_target.count.saturating_add(1);
                accrue_aging(
                    &mut ap.payables_aging,
                    bucket,
                    r.net_minor,
                    r.vat_minor,
                    r.gross_minor,
                );
                if !matches!(bucket, AgingBucket::Current) {
                    ap.payable_past_deadline = ap.payable_past_deadline.saturating_add(1);
                }
            }
        }
    }
    ap
}

fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let sum: f64 = xs.iter().copied().sum();
    Some(sum / xs.len() as f64)
}

fn top_n_from_map(map: HashMap<(String, String), (i64, u64)>, n: usize) -> Vec<TopEntry> {
    let mut v: Vec<TopEntry> = map
        .into_iter()
        .map(|((label, currency), (gross, count))| TopEntry {
            label,
            currency,
            gross_minor: gross,
            count,
        })
        .collect();
    // DESC sort by gross_minor (highest first).
    v.sort_by_key(|t| std::cmp::Reverse(t.gross_minor));
    v.truncate(n);
    v
}

#[allow(clippy::too_many_arguments)]
fn compute_delta(
    conn: &Connection,
    tenant: &str,
    traces: &HashMap<String, ReportTrace>,
    buyer_names: &HashMap<String, String>,
    basis: DateBasis,
    today: Date,
    prior_kind: Option<PeriodKind>,
    current_revenue: &CurrencyAggregate,
    current_expenses: &CurrencyAggregate,
) -> Result<Option<DeltaSet>> {
    let Some(prior_kind) = prior_kind else {
        return Ok(None);
    };
    let window = resolve_window(prior_kind)?;
    let groups = query_outgoing_groups(conn, window, basis)?;
    let restored = query_restored_rows(conn, tenant, window)?;
    let ap = query_ap_rows(conn, tenant, window, basis)?;
    let prior_outgoing = aggregate_outgoing(groups, traces, today, buyer_names);
    let mut prior_revenue = prior_outgoing.revenue;
    for r in &restored {
        let target = match r.currency.as_str() {
            "EUR" => &mut prior_revenue.eur,
            _ => &mut prior_revenue.huf,
        };
        target.gross_minor = target.gross_minor.saturating_add(r.gross_minor);
        target.net_minor = target.net_minor.saturating_add(r.net_minor);
        target.vat_minor = target.vat_minor.saturating_add(r.vat_minor);
        target.count = target.count.saturating_add(1);
    }
    let mut prior_expenses = CurrencyAggregate::default();
    for r in &ap {
        if r.local_status == "Irrelevant" {
            continue;
        }
        let target = match r.currency.as_str() {
            "EUR" => &mut prior_expenses.eur,
            _ => &mut prior_expenses.huf,
        };
        target.gross_minor = target.gross_minor.saturating_add(r.gross_minor);
        target.net_minor = target.net_minor.saturating_add(r.net_minor);
        target.vat_minor = target.vat_minor.saturating_add(r.vat_minor);
        target.count = target.count.saturating_add(1);
    }
    let revenue_pct_huf = pct_change(
        prior_revenue.huf.gross_minor,
        current_revenue.huf.gross_minor,
    );
    let revenue_pct_eur = pct_change(
        prior_revenue.eur.gross_minor,
        current_revenue.eur.gross_minor,
    );
    let expenses_pct_huf = pct_change(
        prior_expenses.huf.gross_minor,
        current_expenses.huf.gross_minor,
    );
    let expenses_pct_eur = pct_change(
        prior_expenses.eur.gross_minor,
        current_expenses.eur.gross_minor,
    );
    Ok(Some(DeltaSet {
        period_label: period_label(prior_kind),
        revenue: prior_revenue,
        expenses: prior_expenses,
        revenue_pct_huf,
        revenue_pct_eur,
        expenses_pct_huf,
        expenses_pct_eur,
    }))
}

fn pct_change(prior: i64, current: i64) -> Option<f64> {
    if prior == 0 {
        return None;
    }
    let delta = current as f64 - prior as f64;
    Some((delta / prior.unsigned_abs() as f64) * 100.0)
}

fn compute_annual_running(
    conn: &Connection,
    tenant: &str,
    traces: &HashMap<String, ReportTrace>,
    buyer_names: &HashMap<String, String>,
    basis: DateBasis,
    today: Date,
) -> Result<AnnualRunningPanel> {
    let year = today.year();
    let from = Date::from_calendar_date(year, Month::January, 1)?;
    let window = DateWindow {
        from: Some(from),
        to: Some(today),
    };
    let groups = query_outgoing_groups(conn, window, basis)?;
    let restored = query_restored_rows(conn, tenant, window)?;
    let outgoing = aggregate_outgoing(groups, traces, today, buyer_names);
    let mut revenue = outgoing.revenue;
    for r in &restored {
        let target = match r.currency.as_str() {
            "EUR" => &mut revenue.eur,
            _ => &mut revenue.huf,
        };
        target.gross_minor = target.gross_minor.saturating_add(r.gross_minor);
        target.net_minor = target.net_minor.saturating_add(r.net_minor);
        target.vat_minor = target.vat_minor.saturating_add(r.vat_minor);
        target.count = target.count.saturating_add(1);
    }
    Ok(AnnualRunningPanel { year, revenue })
}

/// Best-effort buyer-name map keyed by invoice id. Mirrors
/// `serve::list_invoices`'s side-store read posture (S215). Missing /
/// unreadable / blank side-store → no entry.
fn build_buyer_names_map(ledger: &Ledger) -> Result<HashMap<String, String>> {
    let entries = ledger
        .entries()
        .context("read audit ledger entries for buyer-name map")?;
    let mut out = HashMap::new();
    for entry in &entries {
        if entry.kind != EventKind::InvoiceDraftCreated {
            continue;
        }
        let Ok(parsed) =
            serde_json::from_slice::<audit_payloads::InvoiceDraftCreatedPayload>(&entry.payload)
        else {
            continue;
        };
        let Some(nav_xml_path) = parsed.nav_xml_path else {
            continue;
        };
        let xml_path = std::path::PathBuf::from(nav_xml_path);
        let input_path = crate::serve::sibling_input_json_path(&xml_path);
        let Ok(bytes) = std::fs::read(&input_path) else {
            continue;
        };
        let Ok(input_json) =
            serde_json::from_slice::<crate::issue_invoice::InvoiceInputJson>(&bytes)
        else {
            continue;
        };
        let trimmed = input_json.customer.name.trim();
        if !trimmed.is_empty() {
            out.insert(parsed.invoice_id, trimmed.to_string());
        }
    }
    Ok(out)
}

/// "Today" anchor for the report. Uses UTC date — the SPA renders the
/// raw ISO string back at the operator so a Budapest-vs-UTC mismatch
/// is visible rather than silently shifted.
pub fn today_local() -> Date {
    OffsetDateTime::now_utc().date()
}

// ──────────────────────────────────────────────────────────────────────
// Tests.
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use aberp_audit_ledger::Actor;
    use aberp_billing::IdempotencyKey;
    use time::Duration;

    #[test]
    fn period_parse_month() {
        assert_eq!(parse_period("2026-06").unwrap(), PeriodKind::Month(2026, 6));
    }

    #[test]
    fn period_parse_quarter() {
        assert_eq!(
            parse_period("2026-Q2").unwrap(),
            PeriodKind::Quarter(2026, 2)
        );
    }

    #[test]
    fn period_parse_year() {
        assert_eq!(parse_period("2026").unwrap(), PeriodKind::Year(2026));
    }

    #[test]
    fn period_parse_all() {
        assert_eq!(parse_period("all").unwrap(), PeriodKind::All);
        assert_eq!(parse_period("All").unwrap(), PeriodKind::All);
    }

    #[test]
    fn period_parse_custom() {
        let kind = parse_period("2026-06-01..2026-06-30").unwrap();
        match kind {
            PeriodKind::Custom { from, to } => {
                assert_eq!(from.year(), 2026);
                assert_eq!(to.month() as u8, 6);
                assert_eq!(to.day(), 30);
            }
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn period_parse_rejects_garbage() {
        assert!(parse_period("nope").is_err());
        assert!(parse_period("2026-13").is_err());
        assert!(parse_period("2026-Q5").is_err());
        assert!(parse_period("2026-06-30..2026-06-01").is_err());
    }

    #[test]
    fn resolve_month_window_inclusive() {
        let w = resolve_window(PeriodKind::Month(2026, 6)).unwrap();
        let fmt = format_description!("[year]-[month]-[day]");
        assert_eq!(w.from.unwrap().format(fmt).unwrap(), "2026-06-01");
        assert_eq!(w.to.unwrap().format(fmt).unwrap(), "2026-06-30");
    }

    #[test]
    fn resolve_year_window_full_year() {
        let w = resolve_window(PeriodKind::Year(2026)).unwrap();
        let fmt = format_description!("[year]-[month]-[day]");
        assert_eq!(w.from.unwrap().format(fmt).unwrap(), "2026-01-01");
        assert_eq!(w.to.unwrap().format(fmt).unwrap(), "2026-12-31");
    }

    #[test]
    fn previous_period_wraps_year() {
        assert_eq!(
            previous_period(PeriodKind::Month(2026, 1)).unwrap(),
            PeriodKind::Month(2025, 12)
        );
        assert_eq!(
            previous_period(PeriodKind::Quarter(2026, 1)).unwrap(),
            PeriodKind::Quarter(2025, 4)
        );
    }

    #[test]
    fn yoy_period_shifts_one_year() {
        assert_eq!(
            yoy_period(PeriodKind::Month(2026, 6)).unwrap(),
            PeriodKind::Month(2025, 6)
        );
        assert!(yoy_period(PeriodKind::All).is_none());
        let custom = PeriodKind::Custom {
            from: Date::from_calendar_date(2026, Month::June, 1).unwrap(),
            to: Date::from_calendar_date(2026, Month::June, 30).unwrap(),
        };
        assert!(yoy_period(custom).is_none());
    }

    #[test]
    fn aging_bucket_classification() {
        let today = Date::from_calendar_date(2026, Month::June, 1).unwrap();
        // Deadline 15 days in the future → Current.
        let future = Date::from_calendar_date(2026, Month::June, 16).unwrap();
        assert!(matches!(
            aging_bucket_for(today, future),
            AgingBucket::Current
        ));
        // Deadline 10 days ago → Days1To30.
        let recent = Date::from_calendar_date(2026, Month::May, 22).unwrap();
        assert!(matches!(
            aging_bucket_for(today, recent),
            AgingBucket::Days1To30
        ));
        // Deadline 100 days ago → Days90Plus.
        let stale = today.checked_sub(Duration::days(100)).unwrap();
        assert!(matches!(
            aging_bucket_for(today, stale),
            AgingBucket::Days90Plus
        ));
    }

    /// ADR-0108 M-2 — the regression fixture, at the unit level. Two 27%
    /// lines of 50 Ft net each: the filing truncates per line to 13 + 13 =
    /// **26 Ft**; the pre-M-2 report rounded the 100 Ft group net once,
    /// half-even, to **27 Ft**. The report now publishes 26.
    ///
    /// This test fails if the fold is ever re-derived from the group net —
    /// `round_half_even(100 × 2700 / 10_000)` is 27, and so is
    /// `floor(100 × 2700 / 10_000)`, so *only* per-line granularity produces
    /// 26. It is the one arithmetic shape that distinguishes them.
    #[test]
    fn two_27pct_lines_fold_to_the_filed_vat_not_the_group_rounded_one() {
        let row = |unit_price: i64| OutgoingLineRow {
            invoice_id: "inv_m2".into(),
            currency: "HUF".into(),
            issue_date: "2026-08-01".into(),
            payment_deadline: None,
            vat_rate_basis_points: 2700,
            vat_rate_kind: Some("Percent".into()),
            quantity: Decimal::ONE,
            unit_price,
        };
        let groups = fold_outgoing_lines(vec![row(50), row(50)]).unwrap();
        assert_eq!(groups.len(), 1, "one (invoice, rate) group");
        assert_eq!(groups[0].net_minor, 100);
        assert_eq!(
            groups[0].vat_minor, 26,
            "per-line truncation: 13 + 13. 27 means the group net was rounded instead"
        );
    }

    /// A non-`Percent` kind contributes ZERO VAT regardless of its stored
    /// basis points (ADR-0103 Invariant V) — the report inherits that from
    /// `line_vat_amount`, the filing's own function. The old report derived
    /// VAT from basis points alone and would have invented 27 Ft of VAT on an
    /// exempt line admitted by a gate-bypassing door.
    #[test]
    fn non_percent_kind_contributes_zero_vat_even_with_a_stored_rate() {
        let groups = fold_outgoing_lines(vec![OutgoingLineRow {
            invoice_id: "inv_aam".into(),
            currency: "HUF".into(),
            issue_date: "2026-08-01".into(),
            payment_deadline: None,
            vat_rate_basis_points: 2700,
            vat_rate_kind: Some("AamExempt".into()),
            quantity: Decimal::ONE,
            unit_price: 100,
        }])
        .unwrap();
        assert_eq!(groups[0].net_minor, 100);
        assert_eq!(groups[0].vat_minor, 0);
    }

    /// Rule 11 — an unparseable `vat_rate_kind` is a corrupt row and fails
    /// loud. Defaulting it to `Percent` would silently invent VAT.
    #[test]
    fn unknown_vat_rate_kind_fails_loud() {
        let err = fold_outgoing_lines(vec![OutgoingLineRow {
            invoice_id: "inv_bad".into(),
            currency: "HUF".into(),
            issue_date: "2026-08-01".into(),
            payment_deadline: None,
            vat_rate_basis_points: 2700,
            vat_rate_kind: Some("NotAKind".into()),
            quantity: Decimal::ONE,
            unit_price: 100,
        }])
        .unwrap_err();
        assert!(format!("{err}").contains("unknown invoice_line.vat_rate_kind"));
    }

    #[test]
    fn pct_change_handles_zero_prior() {
        assert_eq!(pct_change(0, 100), None);
        assert_eq!(pct_change(100, 200).unwrap(), 100.0);
        assert_eq!(pct_change(200, 100).unwrap(), -50.0);
    }

    #[test]
    fn date_basis_round_trip() {
        for basis in [DateBasis::Teljesites, DateBasis::Issued] {
            let s = basis.as_wire_str();
            assert_eq!(DateBasis::parse(s).unwrap(), basis);
        }
        assert!(DateBasis::parse("nope").is_none());
    }

    #[test]
    fn mean_empty_returns_none() {
        let none: Vec<f64> = vec![];
        assert!(mean(&none).is_none());
        assert_eq!(mean(&[1.0, 2.0, 3.0]).unwrap(), 2.0);
    }

    /// S262 / PR-251 — `query_eur_huf_equivalent` sums the snapshot-rate
    /// HUF equivalent ONLY over EUR invoices in the window. HUF invoices
    /// (NULL `huf_equivalent_total`) must contribute nothing — the
    /// currency split is the only consumer and double-counting HUF there
    /// would inflate the EUR bar segment. Also asserts the date window
    /// excludes out-of-period rows and the all-bounds (`All`) path works.
    #[test]
    fn eur_huf_equivalent_sums_only_eur_in_window() {
        let conn = Connection::open_in_memory().expect("in-memory duckdb");
        // Minimal `invoice` shape — only the columns the query reads.
        conn.execute_batch(
            "CREATE TABLE invoice (
                 id VARCHAR,
                 currency VARCHAR,
                 issue_date VARCHAR,
                 delivery_date DATE,
                 huf_equivalent_total DECIMAL(18,0)
             );
             INSERT INTO invoice VALUES
               ('eur-in',  'EUR', '2026-06-10', NULL, 190000),
               ('eur-in2', 'EUR', '2026-06-20', NULL, 10000),
               ('huf-in',  'HUF', '2026-06-12', NULL, NULL),
               ('eur-out', 'EUR', '2026-05-31', NULL, 999999);",
        )
        .expect("seed invoice rows");

        let window = DateWindow {
            from: Some(Date::from_calendar_date(2026, Month::June, 1).unwrap()),
            to: Some(Date::from_calendar_date(2026, Month::June, 30).unwrap()),
        };
        let got = query_eur_huf_equivalent(&conn, window, DateBasis::Teljesites).unwrap();
        assert_eq!(
            got, 200_000,
            "only the two in-window EUR rows (190000 + 10000) contribute; HUF NULL and the May EUR row are excluded"
        );

        // `All` (unbounded) window includes the May EUR row too.
        let got_all =
            query_eur_huf_equivalent(&conn, DateWindow::unbounded(), DateBasis::Teljesites)
                .unwrap();
        assert_eq!(got_all, 1_199_999, "unbounded window sums every EUR row");
    }

    // ──────────────────────────────────────────────────────────────────
    // Receivables / DSO / cash-flow regression fixtures.
    // ──────────────────────────────────────────────────────────────────

    /// A trace that classifies as [`CountedKind::Counted`] — a SAVED ack is
    /// the shortest path through [`ReportTrace::classify`].
    fn saved_trace() -> ReportTrace {
        ReportTrace {
            last_ack_status: Some("SAVED".into()),
            ..Default::default()
        }
    }

    fn group(id: &str, currency: &str, net: i64, deadline: Option<&str>) -> OutgoingLineGroup {
        OutgoingLineGroup {
            invoice_id: id.into(),
            currency: currency.into(),
            issue_date: "2026-07-01".into(),
            payment_deadline: deadline.map(|s| s.into()),
            vat_rate_basis_points: 0,
            net_minor: net,
            vat_minor: 0,
        }
    }

    fn d(y: i32, m: Month, day: u8) -> Date {
        Date::from_calendar_date(y, m, day).unwrap()
    }

    /// Build the trace map the way [`walk_ledger`] does — per-invoice traces
    /// plus the POST-walk storno resolution. Tests go through
    /// [`resolve_landed_stornos`] rather than hand-setting
    /// `has_landed_storno`, so a regression in the resolution itself cannot
    /// hide behind a hand-built flag.
    fn resolved_traces(
        traces: Vec<(&str, ReportTrace)>,
        storno_links: &[(&str, &str)],
    ) -> HashMap<String, ReportTrace> {
        let mut map: HashMap<String, ReportTrace> = traces
            .into_iter()
            .map(|(id, t)| (id.to_string(), t))
            .collect();
        let links: Vec<(String, String)> = storno_links
            .iter()
            .map(|(b, c)| ((*b).to_string(), (*c).to_string()))
            .collect();
        resolve_landed_stornos(&mut map, &links);
        map
    }

    /// A storno child that reached NAV and was SAVED — the reversal landed.
    fn landed_storno_child() -> ReportTrace {
        ReportTrace {
            is_storno_self: true,
            ..saved_trace()
        }
    }

    /// **The AR storno-leak fixture** (the live-prod #3/#4 pair).
    ///
    /// A LANDED storno pair is two equal-and-opposite rows: the cancelled
    /// ORIGINAL (+431,80 €) and the credit note that reverses it
    /// (`is_storno_self`, −431,80 €). The receivables predicate excluded only
    /// the counterpart, so the −amount was dropped while the cancelled
    /// original's +amount stayed in AR: the pair could not net, and prod
    /// published EUR AR = 9 931,40 € (2) where 431,80 € was a voided invoice
    /// nobody owed.
    ///
    /// One predicate gates receivables, its aging panel, the past-deadline
    /// count and the cash-flow-forward projection (they are nested inside
    /// it), so this one fixture pins all four. The control invoice — plain,
    /// unpaid, same currency — must survive: the exclusion is storno-specific,
    /// not a paid-status or date-basis side effect.
    ///
    /// Mutation check: drop `&& !trace.has_landed_storno` from the predicate
    /// and AR goes to 993 140 / count 2, the `Current` aging bucket gains
    /// 43 180, and the cash-flow tiles gain 43 180 — four independent reds.
    #[test]
    fn landed_storno_base_is_excluded_from_receivables_aging_and_cashflow() {
        let today = d(2026, Month::August, 13);
        let traces = resolved_traces(
            vec![
                ("inv3", saved_trace()),
                ("inv4", landed_storno_child()),
                ("inv9", saved_trace()),
            ],
            &[("inv3", "inv4")],
        );
        assert!(
            traces["inv3"].has_landed_storno,
            "a SAVED storno child means the reversal landed"
        );
        let groups = vec![
            // The cancelled original and its credit note — both unpaid, both
            // with a FUTURE deadline, so pre-fix the leak also reached the
            // `Current` bucket and the cash-flow-forward projection.
            group("inv3", "EUR", 43_180, Some("2026-08-23")),
            group("inv4", "EUR", 43_180, Some("2026-08-23")),
            // Control: a plain unpaid EUR receivable, 30 days overdue.
            group("inv9", "EUR", 949_960, Some("2026-07-14")),
        ];
        let agg = aggregate_outgoing(groups, &traces, today, &HashMap::new());

        assert_eq!(
            agg.receivables.eur.gross_minor, 949_960,
            "only the control is receivable; 993 140 means the cancelled original leaked"
        );
        assert_eq!(
            agg.receivables.eur.count, 1,
            "a voided invoice is not an open receivable"
        );
        // The aging panel and the past-deadline count ride the same predicate.
        assert_eq!(agg.receivables_aging.days_1_30.gross_minor, 949_960);
        assert_eq!(
            agg.receivables_aging.current.gross_minor, 0,
            "the storno pair's future deadline must not populate the Current bucket"
        );
        assert_eq!(agg.receivables_aging.current.count, 0);
        assert_eq!(agg.outstanding_past_deadline_count, 1);
        // F3 — the cash-flow-forward leak closes with the same predicate: a
        // cancelled pair with a future deadline projects nothing.
        assert_eq!(agg.cashflow_forward.next_30.eur_minor, 0);
        assert_eq!(agg.cashflow_forward.next_60.eur_minor, 0);
        assert_eq!(agg.cashflow_forward.next_90.eur_minor, 0);
        // Revenue nets the landed pair to zero — the two halves really are
        // equal-and-opposite, which is what makes exclude-both (rather than
        // netting via the chain link) correct. AR must agree with revenue.
        assert_eq!(agg.revenue.eur.gross_minor, 949_960);
    }

    /// **The over-exclusion pin.** `InvoiceStornoIssued` is appended with the
    /// storno DRAFT, in the same transaction, BEFORE the storno is submitted
    /// (`issue_storno.rs:1195-1213`) — so "a storno was issued against this
    /// invoice" does NOT mean the cancellation took effect. If the storno was
    /// ABORTED at NAV, or never submitted, it reverses nothing and the base
    /// is still a live, unpaid, legally outstanding receivable.
    ///
    /// Excluding such a base is the mirror-image defect of the leak above and
    /// strictly worse to operate with: an over-count is visible on the tile
    /// (that is how the leak was caught), an under-count silently erases a
    /// receivable that then never gets chased (rule 11).
    ///
    /// Coherence is the invariant under test: revenue keeps the base's
    /// +amount for a storno that did not land, so receivables must keep it
    /// too. Both bases here must appear in AR, in the right aging bucket,
    /// in the past-deadline count, and in the cash-flow projection.
    ///
    /// Mutation check: flag the base at chain-link time again (the
    /// issuance-time `is_storno_base`) and AR drops to 0 / count 0 while
    /// revenue still reports 63 180 — the incoherence is the tell.
    #[test]
    fn aborted_or_unsubmitted_storno_leaves_the_base_receivable_standing() {
        let today = d(2026, Month::August, 13);
        let traces = resolved_traces(
            vec![
                ("base_past", saved_trace()),
                // Storno submitted, NAV said ABORTED → classifies Rejected.
                (
                    "storno_past",
                    ReportTrace {
                        is_storno_self: true,
                        last_ack_status: Some("ABORTED".into()),
                        ..Default::default()
                    },
                ),
                ("base_future", saved_trace()),
                // Storno drafted but never submitted → classifies
                // PendingDraft. No ack ever arrives.
                (
                    "storno_future",
                    ReportTrace {
                        is_storno_self: true,
                        has_draft: true,
                        ..Default::default()
                    },
                ),
            ],
            &[
                ("base_past", "storno_past"),
                ("base_future", "storno_future"),
            ],
        );
        assert!(
            !traces["base_past"].has_landed_storno,
            "a NAV-ABORTed storno never registered — the base is not cancelled"
        );
        assert!(
            !traces["base_future"].has_landed_storno,
            "an unsubmitted storno cancels nothing"
        );

        let groups = vec![
            group("base_past", "EUR", 43_180, Some("2026-07-14")),
            group("storno_past", "EUR", 43_180, Some("2026-07-14")),
            group("base_future", "EUR", 20_000, Some("2026-08-23")),
            group("storno_future", "EUR", 20_000, Some("2026-08-23")),
        ];
        let agg = aggregate_outgoing(groups, &traces, today, &HashMap::new());

        assert_eq!(
            agg.receivables.eur.gross_minor, 63_180,
            "both bases are still owed in full; 0 means the fix erased live receivables"
        );
        assert_eq!(agg.receivables.eur.count, 2);
        // The tiles nested under the same predicate follow AR.
        assert_eq!(agg.receivables_aging.days_1_30.gross_minor, 43_180);
        assert_eq!(agg.receivables_aging.current.gross_minor, 20_000);
        assert_eq!(agg.outstanding_past_deadline_count, 1);
        assert_eq!(agg.cashflow_forward.next_30.eur_minor, 20_000);
        assert_eq!(agg.cashflow_forward.next_60.eur_minor, 20_000);
        assert_eq!(agg.cashflow_forward.next_90.eur_minor, 20_000);
        // Coherence with revenue: neither storno classifies as Counted, so
        // revenue carries both bases and nothing else. AR reports the same
        // figure. If these two ever disagree, one of them is lying.
        assert_eq!(agg.revenue.eur.gross_minor, 63_180);
        assert_eq!(agg.revenue.eur.gross_minor, agg.receivables.eur.gross_minor);
    }

    /// The resolution must survive the ledger's ORDERING: the chain link is
    /// appended before the storno is submitted, so the child's ack is a
    /// strictly later entry. Anything that decides "did this land?" during
    /// the walk reads the child's trace before it exists.
    ///
    /// Driven through a real in-memory `Ledger` so the entry ordering, the
    /// typed payload decode and the post-walk resolution are all exercised.
    #[test]
    fn walk_ledger_flags_a_base_only_when_its_storno_landed() {
        let tenant = TenantId::new("t1".to_string()).unwrap();
        let mut ledger = Ledger::open_in_memory(tenant, BinaryHash::from_bytes([0u8; 32])).unwrap();
        let actor = Actor::from_local_cli("sess".to_string(), "test-user");

        let link = |ledger: &mut Ledger, base: &str, storno: &str, index: u32| {
            let payload = audit_payloads::InvoiceStornoIssuedPayload::new(
                storno,
                100 + u64::from(index),
                "rsv",
                IdempotencyKey::new(),
                base,
                1,
                index,
            );
            ledger
                .append(
                    EventKind::InvoiceStornoIssued,
                    payload.to_bytes(),
                    actor.clone(),
                    None,
                )
                .unwrap();
        };
        let ack = |ledger: &mut Ledger, invoice: &str, status: &str| {
            let payload = audit_payloads::InvoiceAckStatusPayload::new(
                invoice,
                "tx",
                status,
                b"<a/>".to_vec(),
            );
            ledger
                .append(
                    EventKind::InvoiceAckStatus,
                    payload.to_bytes(),
                    actor.clone(),
                    None,
                )
                .unwrap();
        };

        // Both chain links land in the ledger BEFORE either ack — the real
        // ordering, and the one that breaks a mid-walk decision.
        link(&mut ledger, "base_saved", "storno_saved", 1);
        link(&mut ledger, "base_aborted", "storno_aborted", 1);
        ack(&mut ledger, "storno_saved", "SAVED");
        ack(&mut ledger, "storno_aborted", "ABORTED");

        let walk = walk_ledger(&ledger, DateWindow::unbounded()).unwrap();

        assert!(
            walk.traces["base_saved"].has_landed_storno,
            "SAVED storno child → the base really is cancelled"
        );
        assert!(
            !walk.traces["base_aborted"].has_landed_storno,
            "ABORTED storno child → the base is still outstanding"
        );
        // Both children are storno-self regardless of outcome; that flag IS
        // an issuance-time fact and stays one.
        assert!(walk.traces["storno_saved"].is_storno_self);
        assert!(walk.traces["storno_aborted"].is_storno_self);
        // Hygiene counts chain links, not landings — unchanged by this fix.
        assert_eq!(walk.storno_links_in_period, 2);
        // A ledger of exclusively well-formed payloads must produce an EMPTY
        // diagnostic. Without this, a diagnostic that fires on everything
        // would pass the malformed-payload pins below while crying wolf on
        // every real report.
        assert_eq!(
            walk.diagnostics,
            LedgerDiagnostics::default(),
            "no false positives: every payload here decodes, so nothing is unreadable"
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Unparseable-payload diagnostics.
    //
    // The walk used to swallow a payload that failed to decode (`if let
    // Ok(..)` with no else, `.ok()?`). The entry then contributed to
    // nothing and NOTHING said so — the operator read a clean number that
    // silently omitted a payment / an ack / a storno reversal. These pins
    // hold the two halves of the fix together: the drop is COUNTED and
    // ATTRIBUTED, and the valid rows' arithmetic is untouched.
    //
    // Mutation check: reverting any decode site to its silent form
    // (`if let Ok(parsed) = ..` in `merge`, `.ok()?` in the extractors)
    // drives `unparseable_entries` to 0 and reds every assertion below.
    // ──────────────────────────────────────────────────────────────────

    fn test_ledger() -> (Ledger, Actor) {
        let tenant = TenantId::new("t1".to_string()).unwrap();
        let ledger = Ledger::open_in_memory(tenant, BinaryHash::from_bytes([0u8; 32])).unwrap();
        (
            ledger,
            Actor::from_local_cli("sess".to_string(), "test-user"),
        )
    }

    fn append_raw(ledger: &mut Ledger, actor: &Actor, kind: EventKind, payload: &[u8]) {
        ledger
            .append(kind, payload.to_vec(), actor.clone(), None)
            .unwrap();
    }

    fn append_saved_ack(ledger: &mut Ledger, actor: &Actor, invoice: &str) {
        let payload =
            audit_payloads::InvoiceAckStatusPayload::new(invoice, "tx", "SAVED", b"<a/>".to_vec());
        append_raw(
            ledger,
            actor,
            EventKind::InvoiceAckStatus,
            &payload.to_bytes(),
        );
    }

    /// The headline case. Two SAVED invoices, both with a recorded payment;
    /// one payment payload is malformed (well-formed JSON naming the
    /// invoice, but missing the typed payload's required fields).
    ///
    /// Pre-fix the malformed payment simply evaporated: the invoice looked
    /// unpaid, sat in receivables, and no signal existed anywhere. Post-fix
    /// the NUMBERS ARE THE SAME — deliberately, this fix does not guess at
    /// an amount it could not read — but the run now says one entry was
    /// unreadable, so "receivables" can be presented as possibly-incomplete
    /// instead of authoritative.
    #[test]
    fn malformed_payment_payload_is_counted_not_swallowed() {
        let (mut ledger, actor) = test_ledger();

        append_saved_ack(&mut ledger, &actor, "paid_ok");
        let good_payment = audit_payloads::InvoicePaymentRecordedPayload::new(
            "paid_ok",
            IdempotencyKey::new(),
            "2026-07-20",
            100_000,
            "HUF",
            audit_payloads::PaymentMethod::BankTransfer,
            None,
        );
        append_raw(
            &mut ledger,
            &actor,
            EventKind::InvoicePaymentRecorded,
            &good_payment.to_bytes(),
        );

        append_saved_ack(&mut ledger, &actor, "paid_broken");
        // Valid JSON, carries `invoice_id` (so the entry DOES reach
        // `merge`), but `amount_minor` / `currency` / `method` /
        // `idempotency_key` are absent → the typed decode fails.
        append_raw(
            &mut ledger,
            &actor,
            EventKind::InvoicePaymentRecorded,
            br#"{"invoice_id":"paid_broken","paid_at":"2026-07-20"}"#,
        );

        let walk = walk_ledger(&ledger, DateWindow::unbounded()).unwrap();

        // (a) The drop is visible and attributable.
        assert_eq!(
            walk.diagnostics.unparseable_entries, 1,
            "the malformed payment must be counted; 0 means it vanished silently again"
        );
        assert_eq!(walk.diagnostics.unparseable_entry_ids.len(), 1);
        assert!(
            walk.diagnostics.unparseable_entry_ids[0].starts_with("aud_"),
            "the id must be the operator-facing prefixed form, got {:?}",
            walk.diagnostics.unparseable_entry_ids[0]
        );

        // (b) The valid row is untouched — same payment, same ack.
        assert_eq!(
            walk.traces["paid_ok"].payment_paid_at.as_deref(),
            Some("2026-07-20")
        );
        assert_eq!(walk.traces["paid_ok"].payment_amount_minor, Some(100_000));
        assert_eq!(
            walk.traces["paid_ok"].last_ack_status.as_deref(),
            Some("SAVED")
        );
        // The unreadable payment is NOT invented: the invoice stays as it
        // was before that entry — SAVED, no payment. Preserving current
        // numeric behaviour is the point; only the silence is fixed.
        assert_eq!(walk.traces["paid_broken"].payment_paid_at, None);
        assert_eq!(
            walk.traces["paid_broken"].last_ack_status.as_deref(),
            Some("SAVED"),
            "the malformed payment must not take the invoice's own valid ack down with it"
        );

        // (c) Aggregates over the valid rows are unchanged: `paid_ok` is out
        // of receivables because it is paid, `paid_broken` is in because
        // nothing readable said otherwise.
        let today = d(2026, Month::August, 13);
        let groups = vec![
            group("paid_ok", "HUF", 100_000, Some("2026-07-14")),
            group("paid_broken", "HUF", 250_000, Some("2026-07-14")),
        ];
        let agg = aggregate_outgoing(groups, &walk.traces, today, &HashMap::new());
        assert_eq!(
            agg.receivables.huf.gross_minor, 250_000,
            "only the unpaid one is owed; the valid payment still clears its invoice"
        );
        assert_eq!(agg.revenue.huf.gross_minor, 350_000);
    }

    /// The other two decode surfaces on the same walk, and the
    /// count-each-entry-once rule.
    ///
    /// A malformed ACK is the most damaging of the three: pre-fix it left
    /// `last_ack_status` at `None`, so a genuinely SAVED invoice classified
    /// as `PendingDraft` and dropped out of revenue AND VAT-collected
    /// entirely. A malformed STORNO chain link left the base uncancelled.
    /// A payload that is not JSON at all never even reached `merge`.
    #[test]
    fn malformed_ack_chain_link_and_non_json_payloads_are_each_counted_once() {
        let (mut ledger, actor) = test_ledger();

        // One healthy invoice to prove the valid path survives all of it.
        append_saved_ack(&mut ledger, &actor, "healthy");

        // Malformed ack: has `invoice_id`, missing `transaction_id` +
        // `response_xml`.
        append_raw(
            &mut ledger,
            &actor,
            EventKind::InvoiceAckStatus,
            br#"{"invoice_id":"bad_ack","ack_status":"SAVED"}"#,
        );

        // Malformed storno chain link: no `invoice_id` key at all (normal
        // for this kind — `Ok(None)` from the id extractor), and the typed
        // chain-link decode fails on the missing storno fields.
        append_raw(
            &mut ledger,
            &actor,
            EventKind::InvoiceStornoIssued,
            br#"{"base_invoice_id":"some_base"}"#,
        );

        // Not JSON at all, on a kind that BOTH decode paths look at. Must
        // still be one entry, not two: the count answers "how many entries
        // could not be read", not "how many decode attempts failed".
        append_raw(
            &mut ledger,
            &actor,
            EventKind::InvoiceStornoIssued,
            b"<<< not json >>>",
        );

        let walk = walk_ledger(&ledger, DateWindow::unbounded()).unwrap();

        assert_eq!(
            walk.diagnostics.unparseable_entries, 3,
            "one per unreadable ENTRY — the non-JSON storno fails both decodes but counts once"
        );
        assert_eq!(walk.diagnostics.unparseable_entry_ids.len(), 3);

        // The malformed ack did not fabricate a trace state, and — the
        // point of the fix — did not fabricate silence either.
        assert!(
            walk.traces
                .get("bad_ack")
                .is_none_or(|t| t.last_ack_status.is_none()),
            "an unreadable ack must not be guessed at"
        );
        // The healthy invoice is completely unaffected by its corrupt
        // neighbours.
        assert_eq!(
            walk.traces["healthy"].last_ack_status.as_deref(),
            Some("SAVED")
        );
        assert!(matches!(
            walk.traces["healthy"].classify(),
            CountedKind::Counted {
                is_storno_self: false
            }
        ));
        // The unreadable chain link contributed no hygiene count — pre-fix
        // that was ALSO true, and equally silent. Now it is accounted for.
        assert_eq!(walk.storno_links_in_period, 0);
    }

    /// The id list is capped so a systemically corrupt ledger cannot
    /// balloon the report payload, but the COUNT stays exact — the
    /// difference is what tells a caller "and more".
    #[test]
    fn unparseable_entry_ids_are_capped_while_the_count_stays_exact() {
        let (mut ledger, actor) = test_ledger();
        let corrupt = MAX_UNPARSEABLE_ENTRY_IDS + 7;
        for _ in 0..corrupt {
            append_raw(
                &mut ledger,
                &actor,
                EventKind::InvoicePaymentRecorded,
                b"not json",
            );
        }

        let walk = walk_ledger(&ledger, DateWindow::unbounded()).unwrap();

        assert_eq!(walk.diagnostics.unparseable_entries, corrupt as u64);
        assert_eq!(
            walk.diagnostics.unparseable_entry_ids.len(),
            MAX_UNPARSEABLE_ENTRY_IDS
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // Aging coherence — `sum(buckets) == total`, and the settled-undated
    // exclusion that now holds it.
    //
    // Both aging sites used to read `if let Ok(d) = parse_iso_date(..)`
    // with no `else`, nested in `if let Some(deadline)`. An invoice whose
    // `payment_deadline` was missing or malformed fell out of every bucket
    // — but the lines directly above had already added it to the
    // receivables / payables TOTAL. The panel's breakdown summed to less
    // than the panel's own headline and nothing said so. Same silent-drop
    // class as the audit entries in PR #67, different code path.
    //
    // The invariant is now held by EXCLUSION rather than by imputation: an
    // otherwise-outstanding invoice with no recorded deadline is a settled
    // legacy NAV import (operator ruling) and leaves the total, the
    // buckets and the hygiene counters together. Both sides of the
    // equation lose the same row, so the sum still closes — and the
    // original under-sum is still shut, because the total no longer
    // contains anything the buckets do not.
    //
    // MUTATION PROOFS for the pins below (each verified to red):
    //
    //   1. Restore the pre-#68 shape at either site — total first, then
    //      `if let Some(s) = deadline { if let Ok(d) = ... { .. } }`. The
    //      `sum == total` assertion reds FIRST, buckets short by exactly
    //      the undated row. This is the original defect.
    //   2. Restore the #68 shape — total unconditional, undated imputed
    //      into `Days90Plus`. `sum == total` still holds (that was #68's
    //      point), so the TOTAL assertions red first instead: the excluded
    //      row is back in the headline and in 90+.
    //   3. Drop the exclusion from the hygiene gate only (count every
    //      non-`Current` bucket, undated included). The
    //      `*_past_deadline` assertions red.
    //   4. Stop recording into `SettledUndated`. The diagnostics
    //      assertions red — an exclusion nobody can count is how a real
    //      unpaid payable disappears.
    // ──────────────────────────────────────────────────────────────────

    /// **CLASSIFIER PARITY with the SPA.** `parse_iso_date` and the
    /// SPA's `hasNoRecordedDeadline` (`aging.ts`) decide which invoices
    /// are outstanding AT ALL, on two sides of the wire. A shape they
    /// disagree about is a row one of them counts and the other excludes
    /// — the tile reads 3 and the drill-down shows 2, or a receivable
    /// sits in the total under no bucket anyone can click to.
    ///
    /// This table is duplicated verbatim in `aging.test.ts`
    /// (`deadline classifier parity with reports::parse_iso_date`). Both
    /// must move together; writing it out twice is the point.
    ///
    /// The SPA used to classify with
    /// ``Date.parse(`${d}T00:00:00Z`)``, which disagreed here on three
    /// rows: it rejected the whitespace-padded forms this trims and
    /// accepts, and it SILENTLY ROLLED `2026-02-30` over to 2026-03-02 —
    /// bucketing a receivable from a date that does not exist while this
    /// side had excluded it. None was reachable (writers store canonical
    /// `YYYY-MM-DD`, and the SQL projections now truncate to the date
    /// head), but nothing guarded it either.
    ///
    /// NOTE for anyone loosening `parse_iso_date`: it is shared with the
    /// DSO and window paths. If one of those needs RFC3339 tolerance,
    /// SPLIT the function rather than widening it — this pin reds on
    /// purpose, because widening it silently changes which invoices are
    /// receivable. (The DSO path already gets its tolerance the right
    /// way: `SUBSTR(…, 1, 10)` in the projection, not a laxer parser.)
    #[test]
    fn deadline_classifier_parity_with_the_spa() {
        // (input, is_dated)
        let vocabulary: [(&str, bool); 10] = [
            ("2026-06-30", true),            // canonical
            (" 2026-06-30 ", true),          // `str::trim`; JS said NaN
            ("\t2026-06-30\n", true),        // tabs/newlines trim alike
            ("2026-02-30", false),           // impossible; JS rolled it over
            ("2026-13-45", false),           // out of range both ways
            ("2026-6-3", false),             // unpadded — not canonical
            ("2026-06-30T00:00:00Z", false), // RFC3339 is not a deadline
            ("30/06/2026", false),           // swapped format
            ("not-a-date", false),
            ("", false),
        ];
        for (input, is_dated) in vocabulary {
            assert_eq!(
                parse_iso_date(input).is_ok(),
                is_dated,
                "classifier parity for {input:?} — the SPA's `aging.test.ts` \
                 table must say the same thing"
            );
        }
    }

    /// Sum an aging panel's five buckets — the figure that must equal the
    /// receivables / payables headline the panel sits under.
    fn aging_totals(panel: &AgingPanel) -> (i64, i64, i64, u64) {
        [
            &panel.current,
            &panel.days_1_30,
            &panel.days_31_60,
            &panel.days_61_90,
            &panel.days_90_plus,
        ]
        .into_iter()
        .fold((0, 0, 0, 0), |acc, b| {
            (
                acc.0 + b.net_minor,
                acc.1 + b.vat_minor,
                acc.2 + b.gross_minor,
                acc.3 + b.count,
            )
        })
    }

    /// **RECEIVABLES — PIN 1 + 2 + 3 + 4 on the AR side.** A receivable
    /// with an unparseable `payment_deadline` is treated as a settled
    /// legacy import and leaves outstanding entirely.
    ///
    /// (a) it is out of the receivables TOTAL;
    /// (b) it is out of EVERY bucket — including 90+, which is where PR
    ///     #68 imputed it;
    /// (c) `sum(buckets) == receivables total` still holds, now because
    ///     both sides lost the same row;
    /// (d) it is out of the past-deadline hygiene counter;
    /// (e) the two valid-deadline neighbours are placed BYTE-IDENTICALLY
    ///     — `2026-08-23` still `current`, `2026-07-14` still `days_1_30`,
    ///     to the minor unit. A change that re-bucketed healthy invoices
    ///     would be a worse defect than either behaviour it replaced;
    ///     (f) the excluded row is COUNTED and named in the diagnostic.
    #[test]
    fn receivable_with_an_unparseable_deadline_is_excluded_as_settled() {
        let today = d(2026, Month::August, 13);
        let traces: HashMap<String, ReportTrace> = HashMap::from([
            ("future".into(), saved_trace()),
            ("overdue".into(), saved_trace()),
            ("garbled".into(), saved_trace()),
        ]);
        let groups = vec![
            group("future", "HUF", 20_000, Some("2026-08-23")),
            group("overdue", "HUF", 43_180, Some("2026-07-14")),
            // Real-world shape of a broken deadline: a swapped-format
            // date the ISO parser rejects.
            group("garbled", "HUF", 777_000, Some("14/07/2026")),
        ];
        let agg = aggregate_outgoing(groups, &traces, today, &HashMap::new());

        // (a) Out of the total. Under PR #68 this read 840 180 / 3.
        assert_eq!(
            agg.receivables.huf.gross_minor, 63_180,
            "a receivable with no recorded deadline is a settled legacy import — 840 180 \
             means it is back in the outstanding headline"
        );
        assert_eq!(agg.receivables.huf.count, 2);

        // (b) Out of every bucket, 90+ included.
        assert_eq!(
            agg.receivables_aging.days_90_plus,
            AmountAggregate::default(),
            "the 90+ imputation is gone; 777 000 here is PR #68's behaviour, not this one"
        );

        // (c) The invariant, still. It now closes by exclusion from BOTH
        //     sides rather than by imputing a bucket — which is why (a)
        //     and (b) had to move together.
        let (net, vat, gross, count) = aging_totals(&agg.receivables_aging);
        assert_eq!(
            gross, agg.receivables.huf.gross_minor,
            "the aging buckets must sum to the receivables total the panel is showing"
        );
        assert_eq!(net, agg.receivables.huf.net_minor);
        assert_eq!(vat, agg.receivables.huf.vat_minor);
        assert_eq!(count, agg.receivables.huf.count);

        // (d) Out of the lateness assertion too. `overdue` is the only row
        //     whose deadline we read and which has passed.
        assert_eq!(
            agg.outstanding_past_deadline_count, 1,
            "a settled invoice is not late, and an unreadable deadline is unknown lateness \
             either way"
        );

        // (e) Valid deadlines placed EXACTLY as before — unchanged from
        //     PR #68, and from before it.
        assert_eq!(agg.receivables_aging.current.gross_minor, 20_000);
        assert_eq!(agg.receivables_aging.current.count, 1);
        assert_eq!(agg.receivables_aging.days_1_30.gross_minor, 43_180);
        assert_eq!(agg.receivables_aging.days_1_30.count, 1);
        assert_eq!(agg.receivables_aging.days_31_60, AmountAggregate::default());
        assert_eq!(agg.receivables_aging.days_61_90, AmountAggregate::default());

        // (f) The exclusion is countable and attributable. An exclusion
        //     nobody can see is how a real unpaid invoice disappears.
        assert_eq!(agg.aging_settled_undated.count, 1);
        assert_eq!(agg.aging_settled_undated.ids, vec!["garbled".to_string()]);
        assert_eq!(agg.aging_settled_undated.huf_gross_minor, 777_000);

        // Never promised as incoming cash — as before.
        assert_eq!(agg.cashflow_forward.next_90.huf_minor, 20_000);
    }

    /// The `None` half: a receivable with NO deadline at all. This is the
    /// legacy shape the operator's ruling is actually about — the PR-84
    /// migration added `payment_deadline` without backfilling it and the
    /// column cannot go NULL again, so an undated receivable is a
    /// pre-PR-84 row. See `aging_placement` for why that timeline, and
    /// NOT `issue_invoice`'s input validation, is what bounds this.
    ///
    /// Note what this pin accepts: the invoice is UNPAID and still leaves
    /// Receivables. That is the ruling, applied — and the reason the
    /// exclusion has to stay countable.
    #[test]
    fn receivable_with_no_deadline_at_all_is_excluded_as_settled() {
        let today = d(2026, Month::August, 13);
        let traces: HashMap<String, ReportTrace> =
            HashMap::from([("dateless".into(), saved_trace())]);
        let groups = vec![group("dateless", "EUR", 51_500, None)];
        let agg = aggregate_outgoing(groups, &traces, today, &HashMap::new());

        assert_eq!(
            agg.receivables.eur,
            AmountAggregate::default(),
            "a legacy invoice with no recorded due date is settled — not a receivable"
        );
        assert_eq!(aging_totals(&agg.receivables_aging), (0, 0, 0, 0));
        assert_eq!(agg.aging_settled_undated.count, 1);
        assert_eq!(agg.aging_settled_undated.ids, vec!["dateless".to_string()]);
        // EUR minor units are cents and are tallied apart from forints —
        // one summed "excluded amount" would be arithmetic on unlike
        // things.
        assert_eq!(agg.aging_settled_undated.eur_gross_minor, 51_500);
        assert_eq!(agg.aging_settled_undated.huf_gross_minor, 0);
        assert_eq!(
            agg.outstanding_past_deadline_count, 0,
            "excluded from outstanding means excluded from the hygiene counter too"
        );
    }

    /// A PAID invoice with a garbled deadline is out of AR for the
    /// ORDINARY reason, and must not be double-counted as a settled-undated
    /// exclusion. The two paths to "not outstanding" must not overlap, or
    /// the tripwire count inflates with rows that were never at risk and
    /// stops meaning anything.
    #[test]
    fn paid_invoice_with_an_unparseable_deadline_is_not_also_counted_as_undated() {
        let today = d(2026, Month::August, 13);
        let traces: HashMap<String, ReportTrace> = HashMap::from([(
            "paid".into(),
            ReportTrace {
                payment_paid_at: Some("2026-07-20".into()),
                ..saved_trace()
            },
        )]);
        let groups = vec![group("paid", "HUF", 100_000, Some("garbage"))];
        let agg = aggregate_outgoing(groups, &traces, today, &HashMap::new());

        assert_eq!(agg.receivables.huf, AmountAggregate::default());
        assert_eq!(aging_totals(&agg.receivables_aging), (0, 0, 0, 0));
        assert_eq!(
            agg.aging_settled_undated.count, 0,
            "already settled by payment — do not also report it as an exclusion"
        );
    }

    /// **PIN 2, the cross-product.** `sum(buckets) == total` across every
    /// deadline shape × both sides, in one place, so the invariant is
    /// asserted as a property rather than incidentally per fixture.
    ///
    /// `""` is in the vocabulary deliberately: an empty VARCHAR is
    /// `Some("")`, which takes the *unparseable* arm, not the `None` one —
    /// a distinction easy to lose in a refactor that switches on
    /// `is_empty()` somewhere.
    #[test]
    fn buckets_sum_to_total_for_every_deadline_shape_on_both_sides() {
        let today = d(2026, Month::August, 13);
        // valid / null / unparseable / empty.
        let shapes: [(&str, Option<&str>); 4] = [
            ("valid", Some("2026-07-14")),
            ("null", None),
            ("unparseable", Some("14/07/2026")),
            ("empty", Some("")),
        ];

        for (label, deadline) in shapes {
            // AR.
            let traces: HashMap<String, ReportTrace> =
                HashMap::from([(label.to_string(), saved_trace())]);
            let agg = aggregate_outgoing(
                vec![group(label, "HUF", 250_000, deadline)],
                &traces,
                today,
                &HashMap::new(),
            );
            let (net, vat, gross, count) = aging_totals(&agg.receivables_aging);
            assert_eq!(gross, agg.receivables.huf.gross_minor, "AR gross / {label}");
            assert_eq!(net, agg.receivables.huf.net_minor, "AR net / {label}");
            assert_eq!(vat, agg.receivables.huf.vat_minor, "AR vat / {label}");
            assert_eq!(count, agg.receivables.huf.count, "AR count / {label}");

            // AP.
            let ap = aggregate_ap(&[ap_row(label, deadline, 250_000, "Outstanding")], today);
            let (net, vat, gross, count) = aging_totals(&ap.payables_aging);
            assert_eq!(gross, ap.payables.huf.gross_minor, "AP gross / {label}");
            assert_eq!(net, ap.payables.huf.net_minor, "AP net / {label}");
            assert_eq!(vat, ap.payables.huf.vat_minor, "AP vat / {label}");
            assert_eq!(count, ap.payables.huf.count, "AP count / {label}");

            // …and the invariant must not be satisfied by BOTH sides being
            // empty when the deadline is fine, nor by the row surviving
            // when it is not. Direction, not just equality.
            let outstanding = if label == "valid" { 250_000 } else { 0 };
            assert_eq!(
                agg.receivables.huf.gross_minor, outstanding,
                "AR outstanding / {label}"
            );
            assert_eq!(
                ap.payables.huf.gross_minor, outstanding,
                "AP outstanding / {label}"
            );
            let excluded = u64::from(label != "valid");
            assert_eq!(
                agg.aging_settled_undated.count, excluded,
                "AR settled-undated tally / {label}"
            );
            assert_eq!(
                ap.aging_settled_undated.count, excluded,
                "AP settled-undated tally / {label}"
            );
        }
    }

    fn ap_row(id: &str, deadline: Option<&str>, gross: i64, status: &str) -> ApRow {
        ApRow {
            id: id.into(),
            supplier_name: "Beszállító Kft.".into(),
            payment_deadline: deadline.map(|s| s.into()),
            net_minor: gross,
            vat_minor: 0,
            gross_minor: gross,
            currency: "HUF".into(),
            local_status: status.into(),
        }
    }

    /// **PAYABLES.** The mirror of the receivables pin, on the second
    /// site. Same exclusion, same invariant, same disclosure, same
    /// don't-move-the-healthy-rows requirement.
    #[test]
    fn payable_with_an_unparseable_deadline_is_excluded_as_settled() {
        let today = d(2026, Month::August, 13);
        let rows = vec![
            ap_row("ap_future", Some("2026-08-23"), 20_000, "Outstanding"),
            ap_row("ap_overdue", Some("2026-06-04"), 43_180, "Outstanding"),
            ap_row("ap_garbled", Some("2026-13-45"), 777_000, "Outstanding"),
            ap_row("ap_dateless", None, 5_000, "Outstanding"),
            // Settled + operator-declared-irrelevant rows are not payable
            // and must not be dragged in.
            ap_row("ap_settled", Some("nonsense"), 900_000, "Settled"),
            ap_row("ap_skip", None, 800_000, "Irrelevant"),
        ];
        let ap = aggregate_ap(&rows, today);

        // (a) The two undated rows are out of the total. PR #68 read
        //     845 180 / 4 here.
        assert_eq!(ap.payables.huf.gross_minor, 63_180);
        assert_eq!(ap.payables.huf.count, 2);

        // (b) Out of every bucket — 782 000 in 90+ was PR #68.
        assert_eq!(ap.payables_aging.days_90_plus, AmountAggregate::default());

        // (c) The invariant.
        let (net, vat, gross, count) = aging_totals(&ap.payables_aging);
        assert_eq!(
            gross, ap.payables.huf.gross_minor,
            "the payables buckets must sum to the payables total the panel is showing"
        );
        assert_eq!(net, ap.payables.huf.net_minor);
        assert_eq!(vat, ap.payables.huf.vat_minor);
        assert_eq!(count, ap.payables.huf.count);

        // (d) Only `ap_overdue` is ASSERTED late.
        assert_eq!(
            ap.payable_past_deadline, 1,
            "excluded-as-settled rows reach neither the buckets nor the lateness counter"
        );

        // (e) Valid deadlines unmoved.
        assert_eq!(ap.payables_aging.current.gross_minor, 20_000);
        assert_eq!(ap.payables_aging.days_61_90.gross_minor, 43_180);
        assert_eq!(ap.payables_aging.days_1_30, AmountAggregate::default());
        assert_eq!(ap.payables_aging.days_31_60, AmountAggregate::default());

        // (f) Both exclusions counted, by id, and NOT the non-outstanding
        //     rows — `ap_settled` has an unparseable deadline too but was
        //     never outstanding, and `ap_skip` is operator-declared
        //     irrelevant. Either one leaking in would inflate the tripwire
        //     with rows that were never at risk.
        assert_eq!(ap.aging_settled_undated.count, 2);
        assert_eq!(
            ap.aging_settled_undated.ids,
            vec!["ap_garbled".to_string(), "ap_dateless".to_string()]
        );
        assert_eq!(ap.aging_settled_undated.huf_gross_minor, 782_000);

        // The non-payable rows still reach expenses, and only there. An
        // invoice being settled does not un-spend the money — this change
        // is about what is OUTSTANDING and nothing else.
        assert_eq!(ap.expenses.huf.gross_minor, 1_745_180);
        assert_eq!(ap.expenses.huf.count, 5);
    }

    /// **The production shape, and the residual risk in one fixture.**
    /// `ap_sync::digest_to_ingestion_input` records `payment_deadline:
    /// None` on EVERY NAV-synced payable (ap_sync.rs:971), and NAV sync is
    /// how the payables book is populated — so "all rows undated" is not
    /// an edge case, it is Tuesday.
    ///
    /// Under the operator's ruling that whole book is settled legacy and
    /// the payables panel is empty: total zero, every bucket zero, nothing
    /// asserted late. The panel still adds up — 0 == 0 — and the original
    /// under-sum stays shut, because the headline no longer contains
    /// anything the buckets do not.
    ///
    /// The uncomfortable half is pinned here too: this is EXACTLY the
    /// shape a genuinely unpaid deadline-less payable from an ONGOING sync
    /// would take, and nothing in `aggregate_ap` can tell the two apart.
    /// So the tally must carry all three rows and the full 515 000 — that
    /// count and that amount are the only evidence the exclusion happened,
    /// and they are what the aggregate `tracing::warn!` publishes.
    #[test]
    fn an_all_nav_synced_payables_book_is_excluded_as_settled_but_fully_counted() {
        let today = d(2026, Month::August, 13);
        let rows = vec![
            ap_row("ap_nav_1", None, 120_000, "Outstanding"),
            ap_row("ap_nav_2", None, 340_000, "Outstanding"),
            ap_row("ap_nav_3", None, 55_000, "Outstanding"),
        ];
        let ap = aggregate_ap(&rows, today);

        assert_eq!(
            ap.payables.huf,
            AmountAggregate::default(),
            "an all-legacy payables book is all settled — 515 000 is PR #68's behaviour"
        );
        let (_, _, gross, count) = aging_totals(&ap.payables_aging);
        assert_eq!(gross, ap.payables.huf.gross_minor);
        assert_eq!(count, ap.payables.huf.count);
        assert_eq!(ap.payables_aging.days_90_plus, AmountAggregate::default());
        assert_eq!(ap.payable_past_deadline, 0);

        // The tripwire. Silence here is the failure mode that matters:
        // 515 000 HUF of payables left the report and the count is the
        // only thing that says so.
        assert_eq!(ap.aging_settled_undated.count, 3);
        assert_eq!(ap.aging_settled_undated.huf_gross_minor, 515_000);
    }

    /// A *paid* invoice is out of AR for the ordinary reason, and a plain
    /// unpaid one stays in — asserts the storno exclusion did not widen into
    /// the paid-status path it sits beside.
    #[test]
    fn paid_invoice_leaves_receivables_and_plain_unpaid_one_stays() {
        let today = d(2026, Month::August, 13);
        let traces: HashMap<String, ReportTrace> = HashMap::from([
            (
                "paid".into(),
                ReportTrace {
                    payment_paid_at: Some("2026-07-20".into()),
                    ..saved_trace()
                },
            ),
            ("open".into(), saved_trace()),
        ]);
        let groups = vec![
            group("paid", "HUF", 100_000, Some("2026-07-14")),
            group("open", "HUF", 250_000, Some("2026-07-14")),
        ];
        let agg = aggregate_outgoing(groups, &traces, today, &HashMap::new());
        assert_eq!(agg.receivables.huf.gross_minor, 250_000);
        assert_eq!(agg.receivables.huf.count, 1);
    }

    /// **The DSO anchor fixture.** DSO is `paid_at − ISSUE date`. The sample
    /// used to be anchored on the group's date-basis column, which under
    /// Teljesites is `COALESCE(delivery_date, issue_date)`; any invoice paid
    /// before fulfillment — an advance or prepayment, routine — then produced
    /// a NEGATIVE days-to-pay (live prod showed −5,0 d / −7,0 d).
    ///
    /// Driven end-to-end through `query_outgoing_groups` so the SQL
    /// projection is pinned too, not just the arithmetic. The three invoices
    /// separate the two anchors: with `delivery_date` as the anchor the
    /// prepayment is −12, the ordinary invoice is 20 (not 24), and only the
    /// no-delivery-date row agrees under both.
    ///
    /// The seed uses the PRODUCTION `issue_date` format — RFC3339 VARCHAR —
    /// so the pin also covers the anchor's tolerance of it. The first cut of
    /// this fixture seeded date-only, which the write path never produces;
    /// it passed green while the shipped anchor fed the raw RFC3339 column to
    /// a date-only `parse_iso_date` and dropped EVERY sample (`n=0` on all
    /// real data).
    ///
    /// Mutation checks, both verified red:
    /// - project the date-basis column again instead of `i.issue_date` → the
    ///   prepayment sample goes to −12,0, red on the value and on `>= 0.0`;
    /// - drop the `SUBSTR(CAST(…))` truncation back to a raw `i.issue_date`
    ///   → samples is `[]`, red on the value assertion (this is the bug).
    #[test]
    fn dso_is_anchored_on_issue_date_so_prepayments_are_not_negative() {
        let conn = Connection::open_in_memory().expect("in-memory duckdb");
        conn.execute_batch(
            "CREATE TABLE invoice (
                 id VARCHAR,
                 currency VARCHAR,
                 issue_date VARCHAR,
                 delivery_date DATE,
                 payment_deadline DATE
             );
             CREATE TABLE invoice_line (
                 invoice_id VARCHAR,
                 vat_rate_basis_points INTEGER,
                 vat_rate_kind VARCHAR,
                 quantity DECIMAL(18,6),
                 unit_price INTEGER
             );
             -- `issue_date` is seeded in the PRODUCTION write format: the
               -- billing store declares it `VARCHAR NOT NULL` and writes
               -- `draft.issue_date.format(&Rfc3339)` (see
               -- `modules/billing/src/adapters/duckdb_store.rs`). A date-only
               -- seed here is unfaithful to that write path and lets an
               -- anchor that cannot tolerate RFC3339 pass green.
               INSERT INTO invoice VALUES
               -- Prepayment: paid 07-08, i.e. after issue but 12 days BEFORE
               -- fulfillment. Issue-anchored DSO = +7.
               ('prepaid', 'HUF', '2026-07-01T09:00:00Z', DATE '2026-07-20', DATE '2026-07-31'),
               -- Ordinary: issue 07-01, fulfilled 07-05, paid 07-25.
               -- Issue-anchored = 24; fulfillment-anchored would be 20.
               ('ordinary', 'HUF', '2026-07-01T09:00:00Z', DATE '2026-07-05', DATE '2026-07-31'),
               -- No delivery_date: both anchors coincide at issue → +7.
               ('nodelivery', 'HUF', '2026-07-01T09:00:00Z', NULL, DATE '2026-07-31');
             INSERT INTO invoice_line VALUES
               ('prepaid',    0, 'Percent', 1.0, 100000),
               ('ordinary',   0, 'Percent', 1.0, 100000),
               ('nodelivery', 0, 'Percent', 1.0, 100000);",
        )
        .expect("seed invoice + invoice_line rows");

        let window = DateWindow {
            from: Some(d(2026, Month::July, 1)),
            to: Some(d(2026, Month::July, 31)),
        };
        let groups = query_outgoing_groups(&conn, window, DateBasis::Teljesites).unwrap();
        assert_eq!(groups.len(), 3, "all three invoices fall in the window");

        let paid = |at: &str| ReportTrace {
            payment_paid_at: Some(at.into()),
            payment_amount_minor: Some(100_000),
            ..saved_trace()
        };
        let traces: HashMap<String, ReportTrace> = HashMap::from([
            ("prepaid".into(), paid("2026-07-08")),
            ("ordinary".into(), paid("2026-07-25")),
            ("nodelivery".into(), paid("2026-07-08")),
        ]);
        let agg = aggregate_outgoing(groups, &traces, d(2026, Month::August, 13), &HashMap::new());

        let mut samples = agg.dso_huf_samples.clone();
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            samples,
            vec![7.0, 7.0, 24.0],
            "days-to-pay measured from issue_date: 07-01→07-08 twice, 07-01→07-25 once"
        );
        assert!(
            samples.iter().all(|s| *s >= 0.0),
            "a prepayment is paid before fulfillment but never before issue — DSO cannot go negative"
        );
        assert_eq!(mean(&samples).unwrap(), 38.0 / 3.0);
    }

    /// The DSO anchor's OTHER two input shapes. A legacy install may still
    /// hold `issue_date` as a real DATE column rather than the RFC3339
    /// VARCHAR the current billing store writes; without the `CAST`, the
    /// `row.get::<String>` in [`row_to_outgoing`] raises `InvalidColumnType`
    /// and errors the WHOLE financial report — not merely the DSO panel.
    /// A date-only VARCHAR (hand-seeded / imported rows) must keep working
    /// too, i.e. the `SUBSTR` truncation is a no-op there.
    ///
    /// Driven on the UNBOUNDED window (the operator's `All` period) for a
    /// substantive reason: with bounds, `build_date_where` emits
    /// `COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date)`, which a
    /// DATE-typed `issue_date` cannot bind at all ("Cannot mix values of type
    /// VARCHAR and DATE in COALESCE"). That pre-existing limit of the SHARED
    /// window predicate is out of this fix's scope; `All` is the path on
    /// which a legacy DATE column actually reaches this projection, and it is
    /// exactly there that the `CAST` earns its keep.
    ///
    /// Mutation check: drop the `CAST` and the DATE half is red with an
    /// `Err` out of `query_outgoing_groups`; drop the whole projection
    /// wrapper and the RFC3339 pin above goes to `n=0`.
    #[test]
    fn dso_anchor_tolerates_legacy_date_column_and_date_only_varchar() {
        for (label, issue_col_ddl, issue_value) in [
            ("legacy DATE column", "issue_date DATE", "DATE '2026-07-01'"),
            ("date-only VARCHAR", "issue_date VARCHAR", "'2026-07-01'"),
        ] {
            let conn = Connection::open_in_memory().expect("in-memory duckdb");
            conn.execute_batch(&format!(
                "CREATE TABLE invoice (
                     id VARCHAR,
                     currency VARCHAR,
                     {issue_col_ddl},
                     delivery_date DATE,
                     payment_deadline DATE
                 );
                 CREATE TABLE invoice_line (
                     invoice_id VARCHAR,
                     vat_rate_basis_points INTEGER,
                     vat_rate_kind VARCHAR,
                     quantity DECIMAL(18,6),
                     unit_price INTEGER
                 );
                 INSERT INTO invoice VALUES
                   ('ordinary', 'HUF', {issue_value}, NULL, DATE '2026-07-31');
                 INSERT INTO invoice_line VALUES
                   ('ordinary', 0, 'Percent', 1.0, 100000);"
            ))
            .expect("seed invoice + invoice_line rows");

            let groups =
                query_outgoing_groups(&conn, DateWindow::unbounded(), DateBasis::Teljesites)
                    .unwrap_or_else(|e| {
                        panic!("{label}: the outgoing query must not error: {e:#}")
                    });
            assert_eq!(groups.len(), 1, "{label}: the invoice is in the All window");

            let traces: HashMap<String, ReportTrace> = HashMap::from([(
                "ordinary".to_string(),
                ReportTrace {
                    payment_paid_at: Some("2026-07-25".into()),
                    payment_amount_minor: Some(100_000),
                    ..saved_trace()
                },
            )]);
            let agg =
                aggregate_outgoing(groups, &traces, d(2026, Month::August, 13), &HashMap::new());
            assert_eq!(
                agg.dso_huf_samples,
                vec![24.0],
                "{label}: 07-01→07-25 is a real 24-day sample, not a silently dropped one"
            );
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // The SHARED financial window predicate (`build_date_where`).
    //
    // Every financial figure the operator sees rides this one predicate:
    // revenue, the ÁFA breakdown, receivables and their aging, the
    // cash-flow projection, the EUR/HUF currency split, and the DSO
    // windowing. A row this predicate drops is not a wrong figure on one
    // panel — it is a *smaller* figure on all of them at once, with
    // nothing on screen to say a row went missing.
    // ──────────────────────────────────────────────────────────────────

    /// A production-shaped `invoice` + `invoice_line` fixture for the window
    /// pins below.
    ///
    /// `issue_lit` is a literal-prefix so one fixture covers all three
    /// shapes `issue_date` takes in the field: `""` writes the RFC3339
    /// VARCHAR the billing store actually produces
    /// (`draft.issue_date.format(&Rfc3339)`), and `"DATE "` writes a legacy
    /// DATE-typed column.
    ///
    /// Amounts are powers of two so any total decodes to exactly one subset
    /// of invoices: a wrong figure names the row that went missing instead
    /// of merely being wrong.
    fn seed_window_fixture(issue_col_ddl: &str, issue_lit: &str, time_suffix: &str) -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory duckdb");
        let iss = |day: &str| format!("{issue_lit}'{day}{time_suffix}'");
        conn.execute_batch(&format!(
            "CREATE TABLE invoice (
                 id VARCHAR,
                 currency VARCHAR,
                 {issue_col_ddl},
                 delivery_date DATE,
                 payment_deadline DATE,
                 huf_equivalent_total DECIMAL(18,0)
             );
             CREATE TABLE invoice_line (
                 invoice_id VARCHAR,
                 vat_rate_basis_points INTEGER,
                 vat_rate_kind VARCHAR,
                 quantity DECIMAL(18,6),
                 unit_price INTEGER
             );
             INSERT INTO invoice VALUES
               -- IN: first day of the period, no delivery_date. The low
               -- bound never had the defect ('…-01T12:00:00Z' >= '…-01'
               -- lexicographically) — pinned so a fix cannot trade one
               -- boundary for the other.
               ('firstday',   'HUF', {first},   NULL,             DATE '2026-07-31', NULL),
               -- IN, normal case: an ordinary mid-month invoice.
               ('midmonth',   'HUF', {mid},     NULL,             DATE '2026-07-31', NULL),
               -- IN, normal case: teljesítés inside the period; the basis
               -- column is the DATE delivery_date, not issue_date.
               ('delivered',  'HUF', {deliv},   DATE '2026-06-20', DATE '2026-07-31', NULL),
               -- IN — THE DEFECT. Issued on the period's LAST day with no
               -- delivery_date, so the basis is the raw RFC3339 issue_date:
               -- '2026-06-30T12:00:00Z' <= '2026-06-30' is FALSE as a string
               -- compare, and the invoice fell out of June entirely.
               ('lastday',    'HUF', {last},    NULL,             DATE '2026-07-31', NULL),
               -- IN: last day via a DATE delivery_date. Already correct
               -- pre-fix (CAST of a DATE is date-only); pinned as the
               -- control that isolates the defect to the issue_date half.
               ('deliv_last', 'HUF', {mid},     DATE '2026-06-30', DATE '2026-07-31', NULL),
               -- OUT: issued the day AFTER the period ends. The fix must not
               -- buy inclusion at the price of over-inclusion.
               ('nextday',    'HUF', {next},    NULL,             DATE '2026-07-31', NULL),
               -- OUT: issued the day BEFORE the period starts.
               ('prevmonth',  'HUF', {prev},    NULL,             DATE '2026-07-31', NULL),
               -- OUT, normal case: issued in-period but fulfilled after it.
               -- Teljesítés basis ⇒ it belongs to July, not June.
               ('deliv_next', 'HUF', {mid},     DATE '2026-07-05', DATE '2026-07-31', NULL),
               -- IN, EUR: same last-day shape, for the currency-split
               -- consumer of this predicate.
               ('eur_last',   'EUR', {last},    NULL,             DATE '2026-07-31', 4000000);
             INSERT INTO invoice_line VALUES
               ('firstday',   2700, 'Percent', 1.0,   100000),
               ('midmonth',   2700, 'Percent', 1.0,   200000),
               ('delivered',  2700, 'Percent', 1.0,   400000),
               ('lastday',    2700, 'Percent', 1.0,   800000),
               ('deliv_last', 2700, 'Percent', 1.0,  1600000),
               ('nextday',    2700, 'Percent', 1.0,  3200000),
               ('prevmonth',  2700, 'Percent', 1.0,  6400000),
               ('deliv_next', 2700, 'Percent', 1.0, 12800000),
               ('eur_last',   2700, 'Percent', 1.0,   500000);",
            first = iss("2026-06-01"),
            mid = iss("2026-06-15"),
            deliv = iss("2026-06-10"),
            last = iss("2026-06-30"),
            next = iss("2026-07-01"),
            prev = iss("2026-05-31"),
        ))
        .expect("seed invoice + invoice_line rows");
        conn
    }

    /// June 2026, the way the operator's period selector resolves it.
    fn june_2026() -> DateWindow {
        DateWindow {
            from: Some(d(2026, Month::June, 1)),
            to: Some(d(2026, Month::June, 30)),
        }
    }

    /// **The money pin.** An invoice issued on the LAST day of the period
    /// with no `delivery_date` must land in that period's revenue, ÁFA and
    /// receivables.
    ///
    /// `build_date_where` compares the basis column against the operator's
    /// date-only bounds *as a string*. In production `invoice.issue_date` is
    /// `VARCHAR NOT NULL` holding RFC3339 —
    /// `draft.issue_date.format(&Rfc3339)` in
    /// `modules/billing/src/adapters/duckdb_store.rs` — so the compare that
    /// decided June's contents was `'2026-06-30T12:00:00Z' <= '2026-06-30'`,
    /// which is FALSE: the string is longer and every leading character is
    /// equal. The invoice silently left the period. Because this is the
    /// SHARED window predicate, it left revenue, VAT and AR together, and a
    /// period that is quietly short is exactly the under-count nobody can
    /// see on a tile (rule 11).
    ///
    /// The seed is RFC3339 for that reason: a date-only fixture is unfaithful
    /// to the write path and lets the broken predicate pass green.
    ///
    /// Mutation check (verified red): restore the pre-fix
    /// `COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date)` in
    /// [`build_date_where`] and `lastday` (800 000 net / 216 000 VAT) drops
    /// out — revenue 2 300 000, VAT 621 000, AR gross 2 921 000, four
    /// invoices instead of five.
    #[test]
    fn last_day_rfc3339_invoice_stays_in_revenue_vat_and_receivables() {
        let conn = seed_window_fixture("issue_date VARCHAR", "", "T12:00:00Z");
        let groups = query_outgoing_groups(&conn, june_2026(), DateBasis::Teljesites)
            .expect("the outgoing query must bind and run");

        let mut ids: Vec<&str> = groups.iter().map(|g| g.invoice_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "deliv_last",
                "delivered",
                "eur_last",
                "firstday",
                "lastday",
                "midmonth"
            ],
            "June is the five HUF in-period invoices plus the EUR one; \
             `lastday` missing is the RFC3339 boundary drop, `nextday` / \
             `prevmonth` / `deliv_next` present is over-inclusion"
        );

        // Every invoice unpaid and NAV-SAVED, so receivables mirror revenue
        // exactly and a dropped row is visible on both.
        let traces: HashMap<String, ReportTrace> = ids
            .iter()
            .map(|id| ((*id).to_string(), saved_trace()))
            .collect();
        let agg = aggregate_outgoing(groups, &traces, d(2026, Month::July, 15), &HashMap::new());

        // 100 000 + 200 000 + 400 000 + 800 000 + 1 600 000. Powers of two:
        // 2 300 000 means `lastday` dropped, 5 500 000 means `nextday` leaked.
        assert_eq!(
            agg.revenue.huf.net_minor, 3_100_000,
            "June revenue must carry the last-day invoice's 800 000"
        );
        assert_eq!(
            agg.revenue.huf.vat_minor, 837_000,
            "27% of each line, per-line truncated: 27 000 + 54 000 + 108 000 + 216 000 + 432 000"
        );
        assert_eq!(agg.revenue.huf.gross_minor, 3_937_000);
        assert_eq!(agg.revenue.huf.count, 5);

        // The ÁFA report's filed figure — the money on the tax return.
        assert_eq!(
            agg.vat_collected.huf.vat_minor, 837_000,
            "VAT collected must not be short by the last-day invoice's 216 000"
        );
        assert_eq!(
            agg.vat_breakdown.get(&("HUF".to_string(), 2700)),
            Some(&(3_100_000, 837_000)),
            "the 27% bucket carries the whole period, not the period minus its last day"
        );

        // Receivables ride the same predicate: an invoice that fell out of
        // the window is also an invoice nobody chases.
        assert_eq!(agg.receivables.huf.gross_minor, 3_937_000);
        assert_eq!(agg.receivables.huf.count, 5);
        // Aging is NOT currency-split (`AgingPanel` holds a bare
        // `AmountAggregate`), so this figure is HUF gross + EUR gross in
        // their own minor units: 3 937 000 + 635 000. That mixing is a
        // pre-existing property of the panel and out of this fix's scope —
        // asserted as it actually behaves so the pin measures the window,
        // not a currency question it does not answer.
        assert_eq!(
            agg.receivables_aging.current.gross_minor, 4_572_000,
            "deadline 2026-07-31 against a 2026-07-15 today is Current for all six"
        );
        assert_eq!(agg.receivables_aging.current.count, 6);
        assert_eq!(agg.outstanding_past_deadline_count, 0);

        // The EUR half of the split, same last-day shape.
        assert_eq!(agg.revenue.eur.net_minor, 500_000);
        assert_eq!(agg.revenue.eur.count, 1);
    }

    /// The same predicate seen by the OTHER `build_date_where` consumer —
    /// the EUR→HUF currency split (S262 / PR-251). It reads `invoice`
    /// directly, without the `invoice_line` join, so it is a genuinely
    /// separate binding of the window and would have kept dropping the
    /// last-day EUR invoice even if only the outgoing query were fixed.
    ///
    /// Mutation check (verified red): pre-fix predicate → 0, because the
    /// only EUR row in the fixture is the last-day one.
    #[test]
    fn currency_split_keeps_the_last_day_rfc3339_invoice() {
        let conn = seed_window_fixture("issue_date VARCHAR", "", "T12:00:00Z");
        let got = query_eur_huf_equivalent(&conn, june_2026(), DateBasis::Teljesites)
            .expect("the currency-split query must bind and run");
        assert_eq!(
            got, 4_000_000,
            "the last-day EUR invoice's snapshot-rate HUF equivalent belongs to June"
        );
    }

    /// The window predicate against `issue_date`'s two NON-RFC3339 shapes.
    ///
    /// A legacy install may still hold `issue_date` as a real DATE column
    /// (see `modules/billing/tests/migration_pr73_old_schema.rs`), and
    /// hand-seeded or imported rows carry a date-only VARCHAR. The pre-fix
    /// predicate could not even *bind* the DATE case on a bounded window —
    /// `COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date)` raises
    /// "Cannot mix values of type VARCHAR and DATE in COALESCE" — which
    /// fails the WHOLE financial report, not one panel. Normalising both
    /// COALESCE members to `SUBSTR(CAST(… AS VARCHAR), 1, 10)` makes the
    /// COALESCE type-consistent and the compare date-vs-date in all three
    /// shapes.
    ///
    /// Same fixture, same expected membership as the RFC3339 pin: the point
    /// is that the shape of the stored column does not move the money.
    ///
    /// Mutation checks (both verified red): drop the `CAST` on the
    /// `issue_date` member → the DATE half errors out of
    /// `query_outgoing_groups`; drop the `SUBSTR` → nothing changes here
    /// (these shapes are already date-only), which is precisely why the
    /// RFC3339 pin above has to exist alongside this one.
    #[test]
    fn window_binds_and_filters_legacy_date_and_date_only_varchar() {
        for (label, issue_col_ddl, issue_lit) in [
            ("legacy DATE column", "issue_date DATE", "DATE "),
            ("date-only VARCHAR", "issue_date VARCHAR", ""),
        ] {
            let conn = seed_window_fixture(issue_col_ddl, issue_lit, "");
            let groups = query_outgoing_groups(&conn, june_2026(), DateBasis::Teljesites)
                .unwrap_or_else(|e| panic!("{label}: the outgoing query must not error: {e:#}"));

            let mut ids: Vec<&str> = groups.iter().map(|g| g.invoice_id.as_str()).collect();
            ids.sort_unstable();
            assert_eq!(
                ids,
                vec![
                    "deliv_last",
                    "delivered",
                    "eur_last",
                    "firstday",
                    "lastday",
                    "midmonth"
                ],
                "{label}: the stored shape of issue_date must not change which \
                 invoices belong to June"
            );

            let traces: HashMap<String, ReportTrace> = ids
                .iter()
                .map(|id| ((*id).to_string(), saved_trace()))
                .collect();
            let agg =
                aggregate_outgoing(groups, &traces, d(2026, Month::July, 15), &HashMap::new());
            assert_eq!(
                (
                    agg.revenue.huf.net_minor,
                    agg.revenue.huf.vat_minor,
                    agg.receivables.huf.gross_minor
                ),
                (3_100_000, 837_000, 3_937_000),
                "{label}: revenue / VAT / AR match the RFC3339 fixture exactly"
            );

            assert_eq!(
                query_eur_huf_equivalent(&conn, june_2026(), DateBasis::Teljesites)
                    .unwrap_or_else(|e| panic!("{label}: currency split must not error: {e:#}")),
                4_000_000,
                "{label}: the currency split binds the same normalised predicate"
            );
        }
    }

    /// The half-open shapes of the window. `build_date_where` emits three
    /// different predicates and the fix has to reach all of them — an
    /// operator running "from 2026-06-01" or "up to 2026-06-30" is on a
    /// different SQL string than the bounded period selector.
    ///
    /// Mutation check (verified red): fix only the two-bound arm and the
    /// `to`-only case still drops `lastday`, 2 300 000 instead of 3 100 000.
    #[test]
    fn half_open_windows_share_the_normalised_predicate() {
        let conn = seed_window_fixture("issue_date VARCHAR", "", "T12:00:00Z");
        let net_of = |window: DateWindow| {
            let groups = query_outgoing_groups(&conn, window, DateBasis::Teljesites)
                .expect("the outgoing query must bind and run");
            groups
                .iter()
                .filter(|g| g.currency == "HUF")
                .map(|g| g.net_minor)
                .sum::<i64>()
        };

        // `to`-only: everything up to and including 2026-06-30 — the
        // last-day invoice plus the four earlier HUF ones, and `prevmonth`.
        assert_eq!(
            net_of(DateWindow {
                from: None,
                to: Some(d(2026, Month::June, 30)),
            }),
            9_500_000,
            "3 100 000 in-June + 6 400 000 prevmonth; 8 700 000 means the \
             upper bound still drops the last-day invoice"
        );

        // `from`-only: 2026-06-30 onwards — the last-day invoice, `nextday`,
        // and `deliv_next` (fulfilled 2026-07-05). `deliv_last` is on the
        // bound via its DATE delivery_date, so it is in too.
        assert_eq!(
            net_of(DateWindow {
                from: Some(d(2026, Month::June, 30)),
                to: None,
            }),
            18_400_000,
            "800 000 lastday + 1 600 000 deliv_last + 3 200 000 nextday + \
             12 800 000 deliv_next"
        );

        // Unbounded — every HUF row, the control that says the fixture sums
        // to what the arithmetic above assumes.
        assert_eq!(net_of(DateWindow::unbounded()), 25_500_000);
    }
}
