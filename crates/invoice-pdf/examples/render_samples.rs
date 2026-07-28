//! PR-85 — visual sample renderer.
//!
//! Emits two reference PDFs into `target/sample-invoices/` so Ervin
//! (and any reviewer) can eyeball the premium-polish pass without
//! standing up the full audit-ledger orchestration:
//!
//! - `target/sample-invoices/sample-huf-short.pdf` — HUF-only invoice
//!   with a short product description. Proves the HUF back-compat
//!   branch still hides Árfolyam + the HUF-equivalent totals.
//! - `target/sample-invoices/sample-eur-long.pdf` — EUR invoice with
//!   a deliberately long product name (60+ chars) so the description-
//!   wrap behaviour from PR-85 is visible; plus the §80(1)(g) HUF
//!   equivalents, the rate-source MEGJEGYZÉS, all three dates (PR-84),
//!   a buyer-facing invoice-level note (PR-82), and a per-line
//!   "Megjegyzés" sub-line.
//!
//! Run with: `cargo run --example render_samples -p aberp-invoice-pdf`

use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use aberp_billing::{Currency, RateMetadata};
use aberp_invoice_pdf::{render_invoice, InvoiceModel, LineItem, PartyInfo, TenantLogo};
use rust_decimal::Decimal;
use time::macros::date;

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/sample-invoices");
    fs::create_dir_all(&dir).expect("create sample-invoices dir");
    dir
}

fn supplier() -> PartyInfo {
    PartyInfo {
        name: "Áben Consulting KFT.".to_string(),
        address_lines: vec![
            "Bartók Béla út 105–113. III. emelet 18.".to_string(),
            "1115 Budapest".to_string(),
            "Magyarország".to_string(),
        ],
        tax_number: "32108410-2-43".to_string(),
        bank_account_number: Some("12100011-19061095-00000000".to_string()),
        iban: Some("HU75 1210 0011 1906 1095 0000 0000".to_string()),
        bank_name: Some("Gránit Bank Zrt.".to_string()),
        swift_bic: Some("GNBAHUHB".to_string()),
    }
}

fn customer_huf() -> PartyInfo {
    PartyInfo {
        name: "Magyar Ügyfél Kft.".to_string(),
        address_lines: vec![
            "Váci utca 19.".to_string(),
            "1052 Budapest".to_string(),
            "Magyarország".to_string(),
        ],
        tax_number: "12345678-2-41".to_string(),
        ..Default::default()
    }
}

fn customer_eu() -> PartyInfo {
    PartyInfo {
        name: "Beispiel Handel GmbH".to_string(),
        address_lines: vec![
            "Friedrichstraße 88".to_string(),
            "10117 Berlin".to_string(),
            "Deutschland".to_string(),
        ],
        tax_number: "DE123456789".to_string(),
        ..Default::default()
    }
}

fn write_sample(name: &str, model: &InvoiceModel) {
    let bytes = render_invoice(model).expect("render");
    let path = out_dir().join(format!("{name}.pdf"));
    fs::write(&path, &bytes).expect("write sample PDF");
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}

fn sample_huf_short() -> InvoiceModel {
    InvoiceModel {
        invoice_number: "INV-2026-000042".to_string(),
        issue_date: date!(2026 - 05 - 27),
        fulfillment_date: date!(2026 - 05 - 27),
        payment_due_date: date!(2026 - 06 - 04),
        payment_method: "Átutalás".to_string(),
        currency: Currency::Huf,
        rate_metadata: None,
        supplier: supplier(),
        customer: customer_huf(),
        lines: vec![LineItem {
            description: "Tanácsadói díj — 2026. május".to_string(),
            quantity: Decimal::from(1),
            unit: "db".to_string(),
            unit_price_minor: 250_000,
            net_minor: 250_000,
            vat_rate_percent: 27,
            vat_minor: 67_500,
            gross_minor: 317_500,
            performance_period: None,
            note: None,
        }],
        note: None,
        tenant_logo: None,
        brand_primary_color: None,
    }
}

fn sample_eur_long() -> InvoiceModel {
    let rate = RateMetadata {
        rate: Decimal::from_str("405.23").unwrap(),
        source: "MNB".to_string(),
        date: date!(2026 - 05 - 26),
        // 200_000c × 4.0523 ≈ 810_460 Ft; toy figure for the sample.
        huf_equivalent_total: 810_460,
    };
    InvoiceModel {
        invoice_number: "INV-2026-000043".to_string(),
        issue_date: date!(2026 - 05 - 27),
        fulfillment_date: date!(2026 - 05 - 20),
        payment_due_date: date!(2026 - 06 - 10),
        payment_method: "Átutalás".to_string(),
        currency: Currency::Eur,
        rate_metadata: Some(rate),
        supplier: supplier(),
        customer: customer_eu(),
        lines: vec![
            LineItem {
                description: "Tanácsadói szolgáltatás Áben Consulting KFT \
                               részére 2026 második negyedévében az ERP-rendszer \
                               bevezetésére vonatkozóan, NAV-megfelelőséggel"
                    .to_string(),
                quantity: Decimal::from(1),
                unit: "db".to_string(),
                unit_price_minor: 150_000,
                net_minor: 150_000,
                vat_rate_percent: 27,
                vat_minor: 40_500,
                gross_minor: 190_500,
                performance_period: Some((date!(2026 - 04 - 01), date!(2026 - 06 - 30))),
                note: Some("PO-ref: 2026/Q2-007".to_string()),
            },
            LineItem {
                description: "Implementáció: telepítés + integráció".to_string(),
                quantity: Decimal::from(1),
                unit: "db".to_string(),
                unit_price_minor: 50_000,
                net_minor: 50_000,
                vat_rate_percent: 27,
                vat_minor: 13_500,
                gross_minor: 63_500,
                performance_period: None,
                note: None,
            },
        ],
        note: Some(
            "Köszönjük a megrendelést. Kérjük az utalásnál tüntessék fel \
             a számla sorszámát a közlemény mezőben."
                .to_string(),
        ),
        tenant_logo: None,
        brand_primary_color: None,
    }
}

/// PR-279 — reproduces the exact shape of the invoice Ervin flagged
/// (TEST-ABERPNEW2026/0063): a single line at 16 × 130 000 Ft net, 27%
/// ÁFA, 2 641 600 Ft gross. On the pre-PR-279 geometry the `27%` and
/// `2 641 600 Ft` values printed on top of each other.
///
/// The second line pushes past the reported defect to a 9-digit gross —
/// the worst case the column band is now derived against — so the
/// eyeball check covers headroom, not just the reported value.
///
/// Renders with the tenant logo when `ABERP_SAMPLE_LOGO_PNG` points at
/// a PNG, so the sample can show the approved brand mark in the header
/// without the example reaching into `~/.aberp`.
fn sample_column_overlap_repro() -> InvoiceModel {
    let tenant_logo = std::env::var("ABERP_SAMPLE_LOGO_PNG").ok().map(|p| {
        let bytes = fs::read(&p).unwrap_or_else(|e| panic!("read {p}: {e}"));
        TenantLogo::from_png_bytes(&bytes).expect("decode sample logo PNG")
    });
    InvoiceModel {
        invoice_number: "TEST-ABERPNEW2026/0063".to_string(),
        issue_date: date!(2026 - 07 - 28),
        fulfillment_date: date!(2026 - 07 - 28),
        payment_due_date: date!(2026 - 08 - 05),
        payment_method: "Átutalás".to_string(),
        currency: Currency::Huf,
        rate_metadata: None,
        supplier: supplier(),
        customer: customer_huf(),
        lines: vec![
            LineItem {
                description: "Erste BA".to_string(),
                quantity: Decimal::from(16),
                unit: "nap".to_string(),
                unit_price_minor: 130_000,
                net_minor: 2_080_000,
                vat_rate_percent: 27,
                vat_minor: 561_600,
                gross_minor: 2_641_600,
                performance_period: None,
                note: None,
            },
            LineItem {
                description: "Szerszámacél megmunkálás és hőkezelés — \
                              teljes sorozat, NAV-megfelelőségi \
                              dokumentációval"
                    .to_string(),
                quantity: Decimal::from(742),
                unit: "db".to_string(),
                unit_price_minor: 131_200,
                net_minor: 97_350_400,
                vat_rate_percent: 27,
                vat_minor: 26_284_608,
                gross_minor: 123_635_008,
                performance_period: None,
                note: None,
            },
        ],
        note: None,
        tenant_logo,
        brand_primary_color: None,
    }
}

fn main() {
    write_sample("sample-huf-short", &sample_huf_short());
    write_sample("sample-eur-long", &sample_eur_long());
    write_sample(
        "sample-column-overlap-repro",
        &sample_column_overlap_repro(),
    );
    println!("\nsamples in: {}", out_dir().display());
    println!("\nrasterize with ghostscript (page 1 → PNG):");
    println!(
        "  gs -dBATCH -dNOPAUSE -sDEVICE=pngalpha -r150 \\\n     \
         -sOutputFile=target/sample-invoices/sample-eur-long.png \\\n     \
         target/sample-invoices/sample-eur-long.pdf"
    );
}
