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
}

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
    pub outstanding_past_deadline_count: u64,
    /// Outstanding ap_invoice rows whose payment_deadline has passed.
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
        if let Some(id) = extract_invoice_id_local(entry) {
            walk.traces.entry(id.clone()).or_default().merge(entry, &id);
        }
        if let Some(link) = extract_chain_link_local(entry) {
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
    }
    resolve_landed_stornos(&mut walk.traces, &storno_links);
    Ok(walk)
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
    fn merge(&mut self, entry: &Entry, invoice_id: &str) {
        match entry.kind {
            EventKind::InvoiceDraftCreated => self.has_draft = true,
            EventKind::InvoiceSubmissionAttempt => {
                if let Ok(parsed) = serde_json::from_slice::<
                    audit_payloads::InvoiceSubmissionAttemptPayload,
                >(&entry.payload)
                {
                    if parsed.invoice_id == invoice_id {
                        self.has_attempt = true;
                    }
                }
            }
            EventKind::InvoiceSubmissionResponse => {
                if let Ok(parsed) = serde_json::from_slice::<
                    audit_payloads::InvoiceSubmissionResponsePayload,
                >(&entry.payload)
                {
                    if parsed.invoice_id == invoice_id {
                        self.has_submission_response = true;
                    }
                }
            }
            EventKind::InvoiceAckStatus => {
                if let Ok(parsed) = serde_json::from_slice::<audit_payloads::InvoiceAckStatusPayload>(
                    &entry.payload,
                ) {
                    if parsed.invoice_id == invoice_id {
                        self.last_ack_status = Some(parsed.ack_status);
                    }
                }
            }
            EventKind::InvoiceMarkedAbandoned => {
                self.has_marked_abandoned = true;
            }
            EventKind::InvoicePaymentRecorded => {
                if let Ok(parsed) = serde_json::from_slice::<
                    audit_payloads::InvoicePaymentRecordedPayload,
                >(&entry.payload)
                {
                    if parsed.invoice_id == invoice_id {
                        self.payment_paid_at = Some(parsed.paid_at);
                        self.payment_amount_minor = Some(parsed.amount_minor);
                    }
                }
            }
            _ => {}
        }
    }
}

struct ChainLinkLocal {
    base_invoice_id: String,
    child_invoice_id: String,
    is_storno: bool,
}

fn extract_chain_link_local(entry: &Entry) -> Option<ChainLinkLocal> {
    match entry.kind {
        EventKind::InvoiceStornoIssued => {
            let parsed: audit_payloads::InvoiceStornoIssuedPayload =
                serde_json::from_slice(&entry.payload).ok()?;
            Some(ChainLinkLocal {
                base_invoice_id: parsed.base_invoice_id,
                child_invoice_id: parsed.storno_invoice_id,
                is_storno: true,
            })
        }
        EventKind::InvoiceModificationIssued => {
            let parsed: audit_payloads::InvoiceModificationIssuedPayload =
                serde_json::from_slice(&entry.payload).ok()?;
            Some(ChainLinkLocal {
                base_invoice_id: parsed.base_invoice_id,
                child_invoice_id: parsed.modification_invoice_id,
                is_storno: false,
            })
        }
        _ => None,
    }
}

fn extract_invoice_id_local(entry: &Entry) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&entry.payload).ok()?;
    v.as_object()
        .and_then(|m| m.get("invoice_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
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
                i.issue_date AS issue_date,
                CAST(i.payment_deadline AS VARCHAR) AS payment_deadline,
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
    let sql = format!(
        "SELECT a.supplier_name, a.payment_deadline,
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
            supplier_name: row.get(0)?,
            payment_deadline: row.get(1)?,
            net_minor: row.get(2)?,
            vat_minor: row.get(3)?,
            gross_minor: row.get(4)?,
            currency: row.get(5)?,
            local_status: row.get(6)?,
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

fn build_date_where(window: DateWindow) -> (String, bool, bool) {
    let date_col = "COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date)";
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
            "WHERE COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date) >= ? AND \
                       COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date) <= ?"
                .into(),
            true,
            true,
        ),
        (Some(_), None) => (
            "WHERE COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date) >= ?".into(),
            true,
            false,
        ),
        (None, Some(_)) => (
            "WHERE COALESCE(CAST(i.delivery_date AS VARCHAR), i.issue_date) <= ?".into(),
            false,
            true,
        ),
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
            let ar_target = match currency.as_str() {
                "EUR" => &mut agg.receivables.eur,
                _ => &mut agg.receivables.huf,
            };
            ar_target.net_minor = ar_target.net_minor.saturating_add(*net);
            ar_target.vat_minor = ar_target.vat_minor.saturating_add(*vat);
            ar_target.gross_minor = ar_target.gross_minor.saturating_add(gross);
            ar_target.count = ar_target.count.saturating_add(1);
            // Aging + cashflow forward bucketing.
            if let Some(deadline_str) = deadline {
                if let Ok(deadline_d) = parse_iso_date(deadline_str) {
                    let bucket = aging_bucket_for(today, deadline_d);
                    let panel = &mut agg.receivables_aging;
                    let dest = match bucket {
                        AgingBucket::Current => &mut panel.current,
                        AgingBucket::Days1To30 => &mut panel.days_1_30,
                        AgingBucket::Days31To60 => &mut panel.days_31_60,
                        AgingBucket::Days61To90 => &mut panel.days_61_90,
                        AgingBucket::Days90Plus => &mut panel.days_90_plus,
                    };
                    dest.net_minor = dest.net_minor.saturating_add(*net);
                    dest.vat_minor = dest.vat_minor.saturating_add(*vat);
                    dest.gross_minor = dest.gross_minor.saturating_add(gross);
                    dest.count = dest.count.saturating_add(1);
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

    // AP-side: expenses + VAT-paid + payables + payable-aging + top
    // vendors. Irrelevant rows are excluded from every bucket per the
    // S177 closed-vocab semantics (operator declared not-our-problem).
    let mut ap = ApAggregate::default();
    let mut payable_past_deadline = 0u64;
    for r in &ap_rows {
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
            let p_target = match r.currency.as_str() {
                "EUR" => &mut ap.payables.eur,
                _ => &mut ap.payables.huf,
            };
            p_target.net_minor = p_target.net_minor.saturating_add(r.net_minor);
            p_target.vat_minor = p_target.vat_minor.saturating_add(r.vat_minor);
            p_target.gross_minor = p_target.gross_minor.saturating_add(r.gross_minor);
            p_target.count = p_target.count.saturating_add(1);
            if let Some(deadline_s) = &r.payment_deadline {
                if let Ok(deadline_d) = parse_iso_date(deadline_s) {
                    let bucket = aging_bucket_for(req.today, deadline_d);
                    let dest = match bucket {
                        AgingBucket::Current => &mut ap.payables_aging.current,
                        AgingBucket::Days1To30 => &mut ap.payables_aging.days_1_30,
                        AgingBucket::Days31To60 => &mut ap.payables_aging.days_31_60,
                        AgingBucket::Days61To90 => &mut ap.payables_aging.days_61_90,
                        AgingBucket::Days90Plus => &mut ap.payables_aging.days_90_plus,
                    };
                    dest.net_minor = dest.net_minor.saturating_add(r.net_minor);
                    dest.vat_minor = dest.vat_minor.saturating_add(r.vat_minor);
                    dest.gross_minor = dest.gross_minor.saturating_add(r.gross_minor);
                    dest.count = dest.count.saturating_add(1);
                    if !matches!(bucket, AgingBucket::Current) {
                        payable_past_deadline = payable_past_deadline.saturating_add(1);
                    }
                }
            }
        }
    }

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
    })
}

#[derive(Default)]
struct ApAggregate {
    expenses: CurrencyAggregate,
    vat_paid: CurrencyAggregate,
    payables: CurrencyAggregate,
    payables_aging: AgingPanel,
    top_vendors: HashMap<(String, String), (i64, u64)>,
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
    /// Mutation check: project the date-basis column again instead of
    /// `i.issue_date` and the prepayment sample goes to −12,0 — red on both
    /// the value and the `>= 0.0` assertion.
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
             INSERT INTO invoice VALUES
               -- Prepayment: paid 07-08, i.e. after issue but 12 days BEFORE
               -- fulfillment. Issue-anchored DSO = +7.
               ('prepaid', 'HUF', '2026-07-01', DATE '2026-07-20', DATE '2026-07-31'),
               -- Ordinary: issue 07-01, fulfilled 07-05, paid 07-25.
               -- Issue-anchored = 24; fulfillment-anchored would be 20.
               ('ordinary', 'HUF', '2026-07-01', DATE '2026-07-05', DATE '2026-07-31'),
               -- No delivery_date: both anchors coincide at issue → +7.
               ('nodelivery', 'HUF', '2026-07-01', NULL, DATE '2026-07-31');
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
}
