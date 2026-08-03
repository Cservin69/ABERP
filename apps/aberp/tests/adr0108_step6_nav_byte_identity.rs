//! ADR-0108 **T-4** — the NAV wire body is byte-identical DuckDB vs SQLite.
//!
//! Step 5 proved the invoice family's *columns* cross without drift. That is
//! not the same claim as "the filed document is unchanged", and the filed
//! document is the regulatory record. This is the missing half: every value
//! is read back out of **both** engines, driven through the **production**
//! `nav_xml` renderer, and the two byte strings are compared.
//!
//! # Why this is the pin that matters, and what it can red on
//!
//! §3.2 C's trailing-zero note says a migrated `"1.500000"` and a fresh
//! `"1.5"` both `.normalize()` to the same emitted bytes — so a *format*
//! difference in an R2 column is invisible at the wire. That is exactly why a
//! column-level equality assert is not sufficient evidence and why this test
//! asserts the emitted bytes instead: what it CAN see is a wrong `quantity`
//! **value**, a wrong `unit_price`, a wrong `huf_equivalent_total` (the R1
//! `DECIMAL(18,0)` → `INTEGER` carry, the HUF figure on the filing), a wrong
//! `exchange_rate` (every `…HUF` element is derived through it), a wrong
//! `vat_rate_basis_points`, and a wrong `vat_rate_kind` (the whole
//! `<lineVatRate>` choice element changes shape). Each of those moves bytes.
//! `mutating_one_ulp_of_a_sqlite_quantity_moves_the_filed_bytes` is the
//! mutation proof, in-suite.
//!
//! # The invoice PDF is covered by this test, transitively — stated, not assumed
//!
//! T-4 names "the NAV `InvoiceData` XML **and** the rendered PDF bytes". The
//! PDF is not re-rendered here because it does not need to be:
//! `print_invoice.rs` builds every money-bearing `PdfLine` field
//! (`unit_price_minor`, `net_minor`, `vat_minor`, `gross_minor`,
//! `vat_rate_percent`, `quantity`) from `parsed` — the **on-disk NAV XML** —
//! not from the database. The only DB-sourced PDF inputs are the buyer-facing
//! notes (`invoice.invoice_note`, `invoice_line.note`, both plain `TEXT`
//! carried verbatim) and the rate metadata. So byte-identical XML plus
//! verbatim notes IS the PDF claim, and both are asserted below. If
//! `print_invoice.rs` ever sources an amount from the DB instead of the XML,
//! that reduction breaks and this comment is what says so.
//!
//! # What this test deliberately does NOT claim
//!
//! `modules/billing` has not crossed the seam (Step 5 landed the migrator
//! half only), so the SQLite-side reader lives **here**, in the test, and
//! mirrors `duckdb_store.rs::load_invoice` +
//! `invoice_currency_metadata::load_invoice_currency_metadata_in_tx`
//! projection-for-projection. The DuckDB side uses the **production** loader.
//! That asymmetry is the point: the claim being tested is "a reader written
//! against the SQLite schema reproduces what production reads today", which is
//! the precondition for the cutover. When billing crosses, this reader is
//! replaced by the real one and the assertions stay.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};
use std::str::FromStr;

use aberp::migrate_to_sqlite::{migrate_families, LedgerSource};
use aberp::nav_xml::{
    self, ChainOperationReference, CustomerAddress, CustomerInfo, CustomerVatStatus, NavParties,
    SupplierInfo,
};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};
use aberp_billing::{
    self as billing, AllocateArgs, BillingStore, Currency, CustomerId, DraftInvoice,
    DuckDbBillingStore, Huf, IdempotencyKey, InvoiceId, InvoiceSeries, LineItem, PaymentMethod,
    RateMetadata, ReadyInvoice, ResetPolicy, SeriesCode, SeriesId, VatRateKind,
};
use rust_decimal::Decimal;
use time::macros::{date, datetime, format_description};
use time::OffsetDateTime;

const TENANT: &str = "test";
const ISSUE_AT: OffsetDateTime = datetime!(2026-07-31 09:00:00 UTC);

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step6-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture: the three invoice shapes Step 5's own fixture does not have
// ---------------------------------------------------------------------------

/// One line of a seeded invoice.
struct SeedLine {
    description: &'static str,
    quantity: &'static str,
    unit_price: i64,
    basis_points: u16,
    kind: VatRateKind,
    note: Option<&'static str>,
}

/// One seeded invoice.
struct SeedInvoice {
    lines: &'static [SeedLine],
    /// `Some((rate, huf_equivalent_total))` → EUR; `None` → HUF.
    rate: Option<(&'static str, i64)>,
    invoice_note: Option<&'static str>,
}

const fn l(
    description: &'static str,
    quantity: &'static str,
    unit_price: i64,
    basis_points: u16,
    kind: VatRateKind,
    note: Option<&'static str>,
) -> SeedLine {
    SeedLine {
        description,
        quantity,
        unit_price,
        basis_points,
        kind,
        note,
    }
}

/// **A — the mixed-rate invoice.** All four legal Hungarian ÁFA rates on one
/// invoice, which is what makes `summaryByVatRate` emit four buckets and what
/// drives `write_vat_rate_choice` through its whole `Percentage` domain
/// (B2: `bp as f64 / 10_000.0` formatted `{:.2}`). Step 5's own fixture puts
/// `2700` on every line, so the 0 / 500 / 1800 rates and the multi-bucket
/// summary were untested across the seam. Fractional six-decimal quantities
/// so the per-line `net_total` rounding is live rather than trivial.
const MIXED_RATE_LINES: &[SeedLine] = &[
    l(
        "27% line",
        "1.333333",
        123_457,
        2700,
        VatRateKind::Percent,
        None,
    ),
    l(
        "18% line",
        "2.500000",
        99_991,
        1800,
        VatRateKind::Percent,
        Some("per-line note, 18%"),
    ),
    l(
        "5% line",
        "0.166667",
        777_777,
        500,
        VatRateKind::Percent,
        None,
    ),
    l("0% line", "10.000000", 1, 0, VatRateKind::Percent, None),
];

/// **B — the uniform non-`Percent` invoice.** ADR-0101's `vat_rate_kind`
/// column carries the whole `<lineVatRate>` choice: an `AamExempt` line emits
/// `<vatExemption><case>AAM</case><reason>…` instead of `<vatPercentage>`, so
/// a kind that failed to cross would change the *element name* on the wire,
/// not a digit. `issue_preflight` rejects MIXED kinds and accepts a uniform
/// non-`Percent` invoice, so this is the shape that is actually reachable.
const EXEMPT_LINES: &[SeedLine] = &[
    l(
        "AAM line 1",
        "3.000000",
        50_000,
        0,
        VatRateKind::AamExempt,
        None,
    ),
    l(
        "AAM line 2",
        "0.750000",
        41_237,
        0,
        VatRateKind::AamExempt,
        None,
    ),
];

/// **C — the EUR invoice.** `exchange_rate` (R2 `TEXT`) and
/// `huf_equivalent_total` (R1 `INTEGER`) are both live: every `…HUF` element
/// on the wire is derived through the rate, so a one-digit drift in either
/// moves several elements at once.
const EUR_LINES: &[SeedLine] = &[
    l(
        "EUR line",
        "1.500000",
        12_345,
        2700,
        VatRateKind::Percent,
        None,
    ),
    l(
        "EUR line 2",
        "0.333333",
        999_999,
        2700,
        VatRateKind::Percent,
        None,
    ),
];

const SEEDS: &[SeedInvoice] = &[
    SeedInvoice {
        lines: MIXED_RATE_LINES,
        rate: None,
        invoice_note: Some("mixed-rate invoice note"),
    },
    SeedInvoice {
        lines: EXEMPT_LINES,
        rate: None,
        invoice_note: None,
    },
    SeedInvoice {
        lines: EUR_LINES,
        rate: Some(("405.230000", 5_065)),
        invoice_note: None,
    },
];

/// Seed a DEV-shaped DuckDB through the **production** writer, plus the audit
/// chain / mirror / tamper-evidence layer the Step-4 gate requires.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");
    let series_id = SeriesId::new();

    {
        let mut store = DuckDbBillingStore::open(&db).unwrap();
        store.ensure_schema().unwrap();
        store
            .create_series(&InvoiceSeries {
                id: series_id,
                code: SeriesCode::new("S6".to_string()).unwrap(),
                reset_policy: ResetPolicy::AnnualOnFiscalYear,
                fiscal_year: None,
                created_at: ISSUE_AT,
            })
            .unwrap();

        for s in SEEDS {
            let rate_metadata = s.rate.map(|(r, huf)| RateMetadata {
                rate: Decimal::from_str(r).unwrap(),
                source: "MNB".to_string(),
                date: date!(2026 - 07 - 30),
                huf_equivalent_total: huf,
            });
            let currency = if rate_metadata.is_some() {
                Currency::Eur
            } else {
                Currency::Huf
            };
            store
                .allocate_and_insert(
                    AllocateArgs {
                        series_id,
                        draft: DraftInvoice {
                            id: InvoiceId::new(),
                            series_id,
                            customer_id: CustomerId::new(),
                            lines: s
                                .lines
                                .iter()
                                .map(|line| LineItem {
                                    description: line.description.to_string(),
                                    quantity: Decimal::from_str(line.quantity).unwrap(),
                                    unit_price: Huf(line.unit_price),
                                    vat_rate_basis_points: line.basis_points,
                                    vat_rate_kind: line.kind,
                                    note: line.note.map(str::to_string),
                                    unit: None,
                                })
                                .collect(),
                            issue_date: ISSUE_AT,
                            payment_deadline: ISSUE_AT.date(),
                            delivery_date: ISSUE_AT.date(),
                        },
                        idempotency_key: IdempotencyKey::new(),
                        currency,
                        rate_metadata,
                        bank_snapshot: None,
                        invoice_note: s.invoice_note.map(str::to_string),
                        email_recipient_override: None,
                        start_value: 1,
                        sequence_floor: None,
                        durable_high_water: None,
                    },
                    ISSUE_AT,
                )
                .unwrap();
        }
    }

    {
        let mut ledger = Ledger::open(
            &db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([6u8; 32]),
        )
        .unwrap();
        for i in 0..4 {
            ledger
                .append(
                    EventKind::DbAutoRecovered,
                    format!(r#"{{"n":{i}}}"#).into_bytes(),
                    Actor::test_only(),
                    None,
                )
                .unwrap();
        }
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(&db))
            .unwrap();
    }
    let conn = duckdb::Connection::open(&db).unwrap();
    conn.execute_batch(
        "UPDATE audit_ledger
            SET session_id = 'sess-6',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-6', ?, 'sess-6', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                 '2026-07-31T00:00:00Z')",
        duckdb::params![TENANT, vec![7u8; 8]],
    )
    .unwrap();
    conn.close().unwrap();
    db
}

// ---------------------------------------------------------------------------
// The two readers
// ---------------------------------------------------------------------------

/// Everything the NAV renderer consumes, from one engine.
#[derive(Debug, PartialEq, Eq)]
struct Emittable {
    invoice: ReadyInvoice,
    currency: Currency,
    rate_metadata: Option<RateMetadata>,
    /// Not on the wire (ADR-0042) — carried so the PDF's only DB-sourced
    /// inputs are compared too. See the module docs.
    invoice_note: Option<String>,
}

fn invoice_ids(db: &Path) -> Vec<String> {
    let conn = duckdb::Connection::open(db).unwrap();
    let mut stmt = conn
        .prepare("SELECT id FROM invoice ORDER BY sequence_number ASC")
        .unwrap();
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(stmt);
    conn.close().unwrap();
    ids
}

/// The **production** DuckDB read path, verbatim.
fn read_duckdb(db: &Path, invoice_id: &str) -> Emittable {
    let mut conn = duckdb::Connection::open(db).unwrap();
    let tx = conn.transaction().unwrap();
    let (invoice, _idem) = billing::load_ready_invoice_by_id(&tx, invoice_id)
        .unwrap()
        .expect("the seeded invoice exists");
    let meta =
        aberp::invoice_currency_metadata::load_invoice_currency_metadata_in_tx(&tx, invoice_id)
            .unwrap();
    let invoice_note = billing::load_invoice_note_in_tx(&tx, invoice_id).unwrap();
    let rate_metadata = rate_metadata_from(
        meta.currency,
        meta.exchange_rate.as_deref(),
        meta.exchange_rate_source.clone(),
        meta.exchange_rate_date.as_deref(),
        meta.huf_equivalent_total,
    );
    tx.rollback().unwrap();
    Emittable {
        invoice,
        currency: meta.currency,
        rate_metadata,
        invoice_note,
    }
}

/// The SQLite read path, mirroring `duckdb_store.rs::load_invoice` +
/// `load_invoice_currency_metadata_in_tx` projection-for-projection. The only
/// intended difference is the absent `CAST(… AS VARCHAR)` / `CAST(… AS
/// BIGINT)` wrappers: under §3.2 the columns already ARE `TEXT` and
/// `INTEGER`, which is the whole representational claim being tested.
fn read_sqlite(lite: &Path, invoice_id: &str) -> Emittable {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();

    #[allow(clippy::type_complexity)]
    let (
        series_id_str,
        customer_id_str,
        issue_date_str,
        seq_number,
        fiscal_year,
        payment_deadline_str,
        delivery_date_str,
        currency_str,
        exchange_rate,
        exchange_rate_source,
        exchange_rate_date,
        huf_equivalent_total,
        invoice_note,
    ): (
        String,
        String,
        String,
        i64,
        i32,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT series_id, customer_id, issue_date, sequence_number, fiscal_year,
                    payment_deadline, delivery_date, currency, exchange_rate,
                    exchange_rate_source, exchange_rate_date, huf_equivalent_total,
                    invoice_note
             FROM invoice WHERE id = ?",
            [invoice_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )
        .unwrap();

    let mut stmt = conn
        .prepare(
            "SELECT description, quantity, unit_price, vat_rate_basis_points, note, vat_rate_kind
             FROM invoice_line WHERE invoice_id = ? ORDER BY ordinal ASC",
        )
        .unwrap();
    #[allow(clippy::type_complexity)]
    let raw: Vec<(String, String, i64, i64, Option<String>, Option<String>)> = stmt
        .query_map([invoice_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let lines: Vec<LineItem> = raw
        .into_iter()
        .map(
            |(description, quantity, unit_price, vat, note, kind)| LineItem {
                description,
                quantity: Decimal::from_str(&quantity)
                    .expect("R2 stores the canonical decimal string and nothing else"),
                unit_price: Huf(unit_price),
                vat_rate_basis_points: vat as u16,
                note,
                // ADR-0101 — NULL means a pre-0101 row; `Percent`, never a silent
                // default for an unknown non-NULL value.
                vat_rate_kind: match kind {
                    None => VatRateKind::Percent,
                    Some(s) => VatRateKind::from_db_str(&s)
                        .expect("stored invoice_line.vat_rate_kind is a known kind"),
                },
                unit: None,
            },
        )
        .collect();

    let issue_date = OffsetDateTime::parse(
        &issue_date_str,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let date_fmt = format_description!("[year]-[month]-[day]");
    let payment_deadline = payment_deadline_str
        .map(|s| time::Date::parse(&s, &date_fmt).unwrap())
        .unwrap_or_else(|| issue_date.date());
    let delivery_date = delivery_date_str
        .map(|s| time::Date::parse(&s, &date_fmt).unwrap())
        .unwrap_or_else(|| issue_date.date());

    let currency = match currency_str.as_deref() {
        None | Some("HUF") => Currency::Huf,
        Some("EUR") => Currency::Eur,
        Some(other) => panic!("unknown invoice.currency {other:?} (ADR-0037 §3)"),
    };

    Emittable {
        invoice: ReadyInvoice {
            id: InvoiceId(unprefix(invoice_id, "inv")),
            series_id: SeriesId(unprefix(&series_id_str, "srs")),
            customer_id: CustomerId(unprefix(&customer_id_str, "cus")),
            lines,
            issue_date,
            payment_deadline,
            delivery_date,
            sequence_number: seq_number as u64,
            fiscal_year,
        },
        currency,
        rate_metadata: rate_metadata_from(
            currency,
            exchange_rate.as_deref(),
            exchange_rate_source,
            exchange_rate_date.as_deref(),
            huf_equivalent_total,
        ),
        invoice_note,
    }
}

fn unprefix(s: &str, prefix: &str) -> ulid::Ulid {
    let bare = s
        .strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('_'))
        .unwrap_or_else(|| panic!("{s:?} is not a {prefix}_-prefixed ULID (ADR-0005)"));
    ulid::Ulid::from_string(bare).unwrap()
}

/// Shared by both readers so a difference between them cannot hide in the
/// assembly rather than in the storage.
fn rate_metadata_from(
    currency: Currency,
    rate: Option<&str>,
    source: Option<String>,
    date: Option<&str>,
    huf_equivalent_total: Option<i64>,
) -> Option<RateMetadata> {
    if currency == Currency::Huf {
        return None;
    }
    let date_fmt = format_description!("[year]-[month]-[day]");
    Some(RateMetadata {
        rate: Decimal::from_str(rate.expect("a non-HUF invoice carries a rate")).unwrap(),
        source: source.expect("a non-HUF invoice carries a rate source"),
        date: time::Date::parse(
            date.expect("a non-HUF invoice carries a rate date"),
            &date_fmt,
        )
        .unwrap(),
        huf_equivalent_total: huf_equivalent_total
            .expect("a non-HUF invoice carries a HUF equivalent"),
    })
}

// ---------------------------------------------------------------------------
// The renderers, driven identically from either side
// ---------------------------------------------------------------------------

fn parties() -> NavParties {
    NavParties {
        supplier: SupplierInfo {
            tax_number: "24904362-2-41".to_string(),
            name: "Aben Consulting Kft".to_string(),
            address_country_code: "HU".to_string(),
            address_postal_code: "1037".to_string(),
            address_city: "Budapest".to_string(),
            address_street: "Visszatero koz 6".to_string(),
        },
        customer: CustomerInfo {
            community_vat_number: None,
            customer_vat_status: CustomerVatStatus::Domestic,
            tax_number: Some("27952890-2-42".to_string()),
            name: "AZ9 Services".to_string(),
            address: Some(CustomerAddress {
                country_code: "HU".to_string(),
                postal_code: "1097".to_string(),
                city: "Budapest".to_string(),
                street: "Ulloi ut 1.".to_string(),
            }),
        },
    }
}

fn series() -> SeriesCode {
    SeriesCode::new("S6".to_string()).unwrap()
}

fn chain_reference(e: &Emittable) -> ChainOperationReference {
    ChainOperationReference {
        base_invoice_number: format!("S6/{:05}", e.invoice.sequence_number),
        modification_index: 1,
        base_line_count: e.invoice.lines.len(),
    }
}

/// The three bodies an invoice can be filed as. Rendered from ONE `Emittable`
/// so the only variable between the two calls is which engine it came from.
fn render_all(e: &Emittable) -> Vec<(&'static str, Vec<u8>)> {
    let r = chain_reference(e);
    vec![
        (
            "invoice",
            nav_xml::render_invoice_data_with_number(
                &e.invoice,
                &series(),
                &parties(),
                e.currency,
                e.rate_metadata.as_ref(),
                PaymentMethod::Transfer,
                None,
            )
            .expect("render the fresh-issuance body"),
        ),
        (
            "storno",
            nav_xml::render_storno_data_with_number(
                &e.invoice,
                &series(),
                &parties(),
                &r,
                e.currency,
                e.rate_metadata.as_ref(),
                PaymentMethod::Transfer,
                None,
                &e.invoice.lines,
            )
            .expect("render the storno body"),
        ),
        (
            "modification",
            nav_xml::render_modification_data_with_number(
                &e.invoice,
                &series(),
                &parties(),
                &r,
                e.currency,
                e.rate_metadata.as_ref(),
                PaymentMethod::Transfer,
                None,
            )
            .expect("render the modification body"),
        ),
    ]
}

fn migrated(tag: &str) -> (PathBuf, PathBuf) {
    let dir = scratch(tag);
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    (db, lite)
}

// ---------------------------------------------------------------------------
// T-4
// ---------------------------------------------------------------------------

/// **T-4.** For every seeded invoice, the fresh-issuance body, the storno body
/// and the modification body are byte-identical across the two engines.
#[test]
fn the_filed_nav_body_is_byte_identical_across_the_engines() {
    let (db, lite) = migrated("t4");
    let ids = invoice_ids(&db);
    assert_eq!(
        ids.len(),
        SEEDS.len(),
        "the fixture must seed every shape it claims to"
    );

    for id in &ids {
        let from_duck = read_duckdb(&db, id);
        let from_lite = read_sqlite(&lite, id);

        // The domain object first: a difference here localises the fault to
        // the carry rather than to the renderer.
        assert_eq!(
            from_duck.invoice, from_lite.invoice,
            "the reconstructed ReadyInvoice differs for {id}"
        );
        assert_eq!(from_duck.currency, from_lite.currency, "currency for {id}");
        assert_eq!(
            from_duck.rate_metadata, from_lite.rate_metadata,
            "rate metadata for {id} — every `…HUF` element on the wire is derived through it"
        );
        // The PDF's only DB-sourced inputs (module docs).
        assert_eq!(
            from_duck.invoice_note, from_lite.invoice_note,
            "invoice.invoice_note for {id} (ADR-0042: PDF-only, never on the wire)"
        );

        // Then the bytes, which is the claim T-4 actually makes.
        for ((what, duck), (_, lite_bytes)) in render_all(&from_duck)
            .into_iter()
            .zip(render_all(&from_lite))
        {
            assert_eq!(
                String::from_utf8_lossy(&duck),
                String::from_utf8_lossy(&lite_bytes),
                "the {what} body for {id} is NOT byte-identical across the engines"
            );
        }
    }
}

/// The mixed-rate invoice really does file four distinct `<vatPercentage>`
/// values, and they are the four legal Hungarian ÁFA rates rendered exactly.
///
/// This is the B2 pin at the wire: `write_vat_rate_choice` renders
/// `vat_rate_basis_points as f64 / 10_000.0` with `{:.2}`. ADR-0108 §3.3 N-2
/// permits that `f64` **only** because the render is value-exact over this
/// closed set. Asserting the emitted substrings is what turns "value-exact"
/// from a claim into a check, and it is what will red if the conversion to
/// `Decimal` (§7 Step 5, not yet landed) ever moves a byte.
#[test]
fn the_four_legal_afa_rates_file_exactly_and_the_summary_buckets_them() {
    let (db, lite) = migrated("rates");
    let id = &invoice_ids(&db)[0]; // the mixed-rate invoice

    for (engine, e) in [
        ("duckdb", read_duckdb(&db, id)),
        ("sqlite", read_sqlite(&lite, id)),
    ] {
        let xml = String::from_utf8(render_all(&e).remove(0).1).unwrap();
        for want in [
            "<vatPercentage>0.27</vatPercentage>",
            "<vatPercentage>0.18</vatPercentage>",
            "<vatPercentage>0.05</vatPercentage>",
            "<vatPercentage>0.00</vatPercentage>",
        ] {
            assert!(
                xml.contains(want),
                "{engine}: the filed body must carry {want} exactly — N-2's `f64` render is \
                 permitted only because it is value-exact over the four legal HU rates"
            );
        }
        // Four rates → four summary buckets (ADR-0103 Invariant S). A single
        // bucket would mean every line's money filed under one rate.
        assert_eq!(
            xml.matches("<summaryByVatRate>").count(),
            4,
            "{engine}: a four-rate invoice must file four summaryByVatRate buckets"
        );
    }
}

/// The `vat_rate_kind` column changes the *element name* on the wire, so a
/// kind that failed to cross is not a rounding difference — it is a different
/// document. Asserted on the uniform `AamExempt` invoice.
#[test]
fn a_non_percent_vat_rate_kind_crosses_as_the_exemption_element() {
    let (db, lite) = migrated("kind");
    let id = &invoice_ids(&db)[1]; // the AamExempt invoice

    for (engine, e) in [
        ("duckdb", read_duckdb(&db, id)),
        ("sqlite", read_sqlite(&lite, id)),
    ] {
        let xml = String::from_utf8(render_all(&e).remove(0).1).unwrap();
        assert!(
            xml.contains("<case>AAM</case>"),
            "{engine}: an AamExempt line files <vatExemption><case>AAM</case>, not a percentage"
        );
        assert!(
            !xml.contains("<vatPercentage>"),
            "{engine}: a uniform non-Percent invoice must carry NO <vatPercentage> at all"
        );
    }
}

// ---------------------------------------------------------------------------
// Mutation proof — the pin can red
// ---------------------------------------------------------------------------

/// **The mutation that proves T-4 has teeth.**
///
/// A pin that cannot go red is not a pin (ADR-0107 §4.1). Rather than asking a
/// future session to hand-mutate the migrator, the mutation is performed
/// in-suite: one unit in the sixth decimal place of one `quantity` on the
/// SQLite side — the smallest change R2 can represent, and precisely the size
/// of drift a float round-trip would introduce.
///
/// Note what this also demonstrates: `STRICT` does **not** refuse the write
/// (R2 is `TEXT`, and the value stays a well-formed decimal string), and a
/// `typeof()` sweep still reads `'text'`. The gate that catches it is the
/// emitted bytes.
#[test]
fn mutating_one_ulp_of_a_sqlite_quantity_moves_the_filed_bytes() {
    let (db, lite) = migrated("mutation");
    let id = &invoice_ids(&db)[0];

    let before = render_all(&read_sqlite(&lite, id)).remove(0).1;
    assert_eq!(
        before,
        render_all(&read_duckdb(&db, id)).remove(0).1,
        "the control: unmutated, the two engines agree"
    );

    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        let n = conn
            .execute(
                "UPDATE invoice_line SET quantity = '1.333334'
                 WHERE invoice_id = ? AND quantity = '1.333333'",
                [id],
            )
            .unwrap();
        assert_eq!(n, 1, "the mutation must hit exactly the line it targets");
        let t: String = conn
            .query_row(
                "SELECT typeof(quantity) FROM invoice_line
                  WHERE invoice_id = ? AND quantity = '1.333334'",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            t, "text",
            "the mutated value is still storage-class TEXT — a typeof() sweep is blind to it, \
             which is why the byte comparison is the gate"
        );
    }

    let after = render_all(&read_sqlite(&lite, id)).remove(0).1;
    assert_ne!(
        before, after,
        "a one-ulp quantity drift MUST move the filed bytes; if it does not, T-4 is asserting \
         nothing"
    );
    assert_ne!(
        after,
        render_all(&read_duckdb(&db, id)).remove(0).1,
        "and the cross-engine comparison must fail on it"
    );
}
