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
//! - `sample-paginated-NN-items.pdf` (PR-296) — the pagination eyeball
//!   set at 6 / 7 / 18 / 25 / 42 line items. For THIS fixture's row
//!   shape (full seller bank block, one description in five wrapping to
//!   three lines, notes and performance periods sprinkled in) 6 items is
//!   the last count that fits on one page and 7 the first that breaks,
//!   so the pair brackets the boundary. 18 is the count at which the
//!   pre-PR-296 renderer began painting at NEGATIVE y — off the sheet.
//!   25 and 42 give clean three- and four-page documents.
//!
//!   The break point is a function of content, not a fixed row count:
//!   a leaner invoice (no bank block, short descriptions) fits ~11 rows
//!   on page 1 and ~22 on each continuation page.
//!
//! Run with: `cargo run --example render_samples -p aberp-invoice-pdf`
//! (set `ABERP_SAMPLE_LOGO_PNG` to render with a brand mark).

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

/// PR-296 — pagination samples.
///
/// A realistic multi-item HUF invoice with `n` line items: mixed VAT
/// rates (so the totals block carries several ÁFA rows), a long
/// description that wraps, a per-line Megjegyzés and a performance
/// period on a couple of rows, and an invoice-level note. Everything
/// that makes a row taller than its 28pt base is represented, because
/// those are exactly the rows the page break has to measure correctly.
fn sample_paginated(n: usize) -> InvoiceModel {
    let descriptions = [
        "Marógép-alkatrész, sorozatgyártás",
        "Szerszámacél megmunkálás és hőkezelés — teljes sorozat, \
         NAV-megfelelőségi dokumentációval és mérési jegyzőkönyvvel",
        "CNC esztergálás",
        "Felületkezelés (eloxálás)",
        "Minőségellenőrzés, végátvétel",
    ];
    let rates: [u16; 3] = [27, 27, 5];
    let lines = (0..n)
        .map(|i| {
            let vat = rates[i % rates.len()];
            let unit_price = 12_500 + (i as i64) * 1_450;
            let qty = 1 + (i as i64 % 4);
            let net = unit_price * qty;
            let vat_minor = net * (vat as i64) / 100;
            LineItem {
                description: format!("{}. {}", i + 1, descriptions[i % descriptions.len()]),
                quantity: Decimal::from(qty),
                unit: "db".to_string(),
                unit_price_minor: unit_price,
                net_minor: net,
                vat_rate_percent: vat,
                vat_minor,
                gross_minor: net + vat_minor,
                performance_period: if i % 7 == 3 {
                    Some((date!(2026 - 04 - 01), date!(2026 - 06 - 30)))
                } else {
                    None
                },
                note: if i % 5 == 2 {
                    Some(format!("PO-ref: 2026/Q2-{:03}", i + 1))
                } else {
                    None
                },
            }
        })
        .collect();
    InvoiceModel {
        invoice_number: format!("TEST-ABERPNEW2026/0{:03}", 100 + n),
        issue_date: date!(2026 - 07 - 28),
        fulfillment_date: date!(2026 - 07 - 28),
        payment_due_date: date!(2026 - 08 - 05),
        payment_method: "Átutalás".to_string(),
        currency: Currency::Huf,
        rate_metadata: None,
        supplier: supplier(),
        customer: customer_huf(),
        lines,
        note: Some(
            "Köszönjük a megrendelést. Kérjük az utalásnál tüntessék fel \
             a számla sorszámát a közlemény mezőben."
                .to_string(),
        ),
        tenant_logo: std::env::var("ABERP_SAMPLE_LOGO_PNG").ok().map(|p| {
            let bytes = fs::read(&p).unwrap_or_else(|e| panic!("read {p}: {e}"));
            TenantLogo::from_png_bytes(&bytes).expect("decode sample logo PNG")
        }),
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
    // PR-296 — pagination eyeball set. For this fixture's row shape 6
    // is the last count that fits on one page and 7 the first that
    // breaks; 18 is the line count at which the pre-PR-296 renderer
    // started painting at negative y.
    for n in [6usize, 7, 18, 25, 42] {
        write_sample(
            &format!("sample-paginated-{n:02}-items"),
            &sample_paginated(n),
        );
    }
    println!("\nsamples in: {}", out_dir().display());
    println!("\nrasterize with ghostscript (page 1 → PNG):");
    println!(
        "  gs -dBATCH -dNOPAUSE -sDEVICE=pngalpha -r150 \\\n     \
         -sOutputFile=target/sample-invoices/sample-eur-long.png \\\n     \
         target/sample-invoices/sample-eur-long.pdf"
    );
}
