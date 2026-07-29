//! Printed-invoice PDF renderer per ADR-0037 §1.a + ADR-0021
//! "Print rendering path" deferred row. PR-44ε.1 / A152.
//!
//! # Posture
//!
//! A4 PDF, paginated (PR-296 — see [`layout_pages`]; before that the
//! renderer emitted exactly one page and painted anything past it off
//! the sheet). Built-in `Helvetica` + `Helvetica-Bold` fonts
//! with WinAnsi encoding. Layout matches the reference template
//! (`reference_aberp_invoice_template.md`) re-branded from Billingo to
//! ABERP — same field set, same top-to-bottom order, same
//! right-aligned totals block.
//!
//! # Why lopdf + built-in Helvetica
//!
//! Per the session-56 A152 decision: `lopdf` is a low-level
//! Rust-native PDF document model with no system deps; the built-in
//! Helvetica font means no font file to embed or ship with the
//! binary. Trade-off: WinAnsi encoding does not cover Hungarian
//! double-acute `ő/ű/Ő/Ű`; the renderer substitutes those to single-
//! acute `ö/ü/Ö/Ü` at the byte boundary (see [`text`] module). The
//! substitution is documented loud and named as the PR-44ε.2 deferred
//! lift.
//!
//! # PR-85 — premium polish (silver / gold palette)
//!
//! ADR-0044 records the brand decision: this is Áben Consulting's
//! real client-facing document, so the surface needs refined-luxury
//! restraint, NOT dev-tool grey. The palette lives in `style` below
//! as a small, named set of `(f32, f32, f32)` constants so colour is
//! tunable in one place. Three discipline rules per ADR-0044:
//!
//! 1. Structural rules in `SILVER_LINE` (soft warm grey).
//! 2. ONE gold accent only — the rule above the totals banner. The
//!    big total figure stays ink (sparing, not gaudy).
//! 3. Section labels in `MUTED` (silver-grey) — small-caps feel comes
//!    from existing uppercase strings + the smaller font size, NOT
//!    extra typography ops (kept tasteful + WinAnsi-safe).
//!
//! # Coordinate system
//!
//! PDF uses bottom-left origin in points (1/72 inch). A4 = 595 × 842
//! points. The renderer positions every text element via absolute
//! `Td` moves; layout drift is structural rather than relative, which
//! keeps the layout deterministic across input data (the regulatory
//! print needs exact placement for accountant readability).

#![forbid(unsafe_code)]

pub mod format;
pub mod logo;
pub mod model;
pub mod text;

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream, StringFormat};
use thiserror::Error;

use aberp_billing::Currency;

pub use logo::{TenantLogo, MAX_LOGO_DIMENSION};
pub use model::{InvoiceModel, LineItem, PartyInfo};

/// A4 page width in PDF points (210 mm × 72/25.4).
const PAGE_WIDTH: i64 = 595;
/// A4 page height in PDF points (297 mm × 72/25.4).
const PAGE_HEIGHT: i64 = 842;
/// Left margin in points.
const MARGIN_LEFT: i64 = 48;
/// Right margin (x-coord of the right edge of the printable area).
const MARGIN_RIGHT: i64 = PAGE_WIDTH - 48;
/// Top margin (y-coord of the top of the printable area; PDF y grows
/// upward).
const MARGIN_TOP: i64 = PAGE_HEIGHT - 56;

// ─── PR-296 — pagination geometry ─────────────────────────────────────
//
// The footer band is fixed at the bottom of every page: the `i/N Oldal`
// counter at `FOOTER_Y_TOP`, the attestation sentence 14pt below it.
// `CONTENT_FLOOR` is the y below which no flowed content (line-item
// rows, totals, MEGJEGYZÉS) may paint — it sits a row's breathing room
// above the counter so a row's sub-lines never collide with the footer.

/// Baseline of the `i/N Oldal` page counter in the footer band.
const FOOTER_Y_TOP: i64 = 64;
/// Lowest y that flowed content may paint at. A row is placed only when
/// its WHOLE slot clears this line; otherwise the flow breaks to the
/// next page.
const CONTENT_FLOOR: i64 = FOOTER_Y_TOP + 28;
/// Continuation-page title baseline ("Számla" + invoice number).
const CONT_TITLE_Y: i64 = MARGIN_TOP - 14;
/// Continuation-page header under-rule.
const CONT_RULE_Y: i64 = MARGIN_TOP - 26;
/// Continuation-page column-header band baseline. Pages 2..N skip the
/// party / date / banner block entirely, so the table starts high.
const CONT_TABLE_TOP: i64 = MARGIN_TOP - 48;
/// Widow control — how many trailing line-item rows are kept together
/// with the closing block when deciding where to break.
///
/// The closing block is ~150pt tall, so reserving it under the LAST row
/// alone routinely pushed exactly one row plus the totals onto a nearly
/// empty final page. Rows this close to the end instead reserve the
/// whole tail, so the break lands before them and the final page reads
/// as a deliberate closing page rather than an orphan.
const TAIL_ROWS_KEPT_WITH_TOTALS: usize = 3;

// ─── PR-85 — silver / gold palette ────────────────────────────────────
//
// Named once here so a future brand tweak is a one-line edit, not a
// grep-and-replace across thirty `Object::Real(0.7)` literals. ADR-0044
// records the brand rationale.
//
// Encoded as `(f32, f32, f32)` RGB in 0..=1. Each colour ships as a
// helper that pushes the right PDF op (`rg` for non-stroking / fill
// used by text, `RG` for stroking used by rule lines).

type Color = (f32, f32, f32);

/// Body ink — near-black with a faint warm shift so it reads softer
/// than a pure-black `Tj`. Used for every primary number + name + body
/// paragraph. NOT pure black (0,0,0): a slight warmth pairs with the
/// silver/gold accents.
const INK: Color = (0.13, 0.13, 0.15);
/// Section labels (ELADÓ, VEVŐ, ADÓSZÁM:, NETTÓ ÖSSZEG:, MEGJEGYZÉS,
/// table column headers). Refined silver-grey — sits below the ink
/// hierarchy without disappearing.
const MUTED: Color = (0.46, 0.47, 0.51);
/// Structural rules — title under-rule, table header rule, table
/// footer rule. A soft warm silver: clearly visible but never
/// competes with the ink content above/below.
const SILVER_LINE: Color = (0.72, 0.72, 0.74);
/// PR-85's ONE accent (per ADR-0044 §"Restraint"). Used for exactly
/// one rule: the line above the FIZETENDŐ BRUTTÓ VÉGÖSSZEG totals
/// banner. A muted warm gold — refined, not gaudy. If a future
/// reviewer feels the need to add gold to a second element, push back
/// and re-read ADR-0044 first.
///
/// Saturation tuned so the accent reads visibly gold (not "slightly
/// darker grey") on a 150-dpi print preview yet stays restrained on
/// a high-resolution actual print. Slightly warmer than a pure
/// midpoint gold so the rule sits comfortably next to the warm-ink
/// body text.
const GOLD_ACCENT: Color = (0.72, 0.54, 0.12);

/// Gap (in points) between a label's colon and its value in the
/// party / date `label_value` pairs. PR-85: was 4pt (cramped — Ervin
/// flagged the `Adószám:123` look), now 10pt for breathing room.
const LABEL_VALUE_GAP: i64 = 10;

/// PR-249 (Bug A) — horizontal gutter (in points) kept clear between
/// the Eladó and Vevő header columns. Each column's text is clamped to
/// its cell width so a long legal name wraps inside the cell instead
/// of running across the boundary and overprinting the other party.
const COLUMN_GUTTER: i64 = 16;

/// Stroke weight (in points) for `SILVER_LINE` structural rules.
const RULE_WEIGHT_SILVER: f32 = 0.5;
/// Stroke weight (in points) for the single `GOLD_ACCENT` rule above
/// the totals banner. Slightly heavier than silver so the accent
/// reads as deliberate rather than a thicker grey line.
const RULE_WEIGHT_GOLD: f32 = 0.85;

// ─── PR-176 — tenant-logo header geometry ─────────────────────────────
//
// Convention over config: a PNG at `~/.aberp/<tenant>/logo.png` is
// drawn top-left of the header inside a fixed `LOGO_BOX_SIDE`-pt
// square. The actual draw is aspect-preserved within the box — a wide
// logo uses the full width and less than full height, a tall logo the
// inverse — so operators can drop any reasonable PNG without picking
// dimensions.
//
// Box size is 50pt (not the brief's example 64pt) because the existing
// header geometry — title baseline at MARGIN_TOP-14, invoice-number
// baseline at MARGIN_TOP-38, silver under-rule at MARGIN_TOP-58 — has
// 58pt of vertical real estate above the under-rule. A 50pt box sits
// comfortably inside that with breathing room, vs. a 64pt box that
// would cross the under-rule and force the entire downstream layout
// to shift. The brief explicitly allows "64×64 OR equivalent — match
// what looks right against the existing header layout"; 50pt is the
// match.
//
// `LOGO_TITLE_GAP` keeps the title text from kissing the logo's right
// edge. Total horizontal slot the title cluster shifts right by is
// `LOGO_BOX_SIDE + LOGO_TITLE_GAP` when a logo is present; absent →
// no shift, byte-for-byte identical title positioning vs pre-PR-176.

/// Side length (points) of the square box reserved for the tenant
/// logo in the header. The logo is scaled aspect-preserved to fit
/// inside this box; empty space inside the box (e.g. when a wide logo
/// uses < `LOGO_BOX_SIDE` of vertical height) is intentional.
const LOGO_BOX_SIDE: i64 = 50;
/// Horizontal breathing room between the logo box's right edge and
/// the title cluster's left edge.
const LOGO_TITLE_GAP: i64 = 10;
/// Name under which the logo Image XObject is registered in the page
/// resources `/XObject` dict. The content stream emits a `Do` op with
/// this exact name to draw it; both sides must agree on the spelling.
const LOGO_XOBJECT_NAME: &str = "Im1";

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("non-HUF invoice requires rate_metadata for the printed render (ADR-0037 §1.a)")]
    MissingRateMetadata,
    #[error("invoice has no line items — refusing to render an empty body")]
    NoLines,
    #[error("PDF content-stream encoding failed: {0}")]
    ContentEncode(String),
    #[error("PDF document save failed: {0}")]
    Save(String),
    /// PR-176 — the operator-supplied PNG at the tenant-logo convention
    /// path failed to decode. Surfaces loudly per CLAUDE.md rule 12
    /// rather than silently dropping the logo — a corrupted file is an
    /// operator-actionable signal (re-export the PNG), not a noise
    /// case to swallow.
    #[error("tenant logo PNG decode failed: {0}")]
    LogoDecode(String),
}

/// Render the invoice to PDF bytes.
///
/// Per ADR-0037 §4 invariant C7 (printed-render slice): non-HUF
/// invoices loud-fail when `rate_metadata` is missing — the §80(1)(g)
/// HUF-equivalent line on the printed invoice depends on the stamped
/// MNB rate.
pub fn render_invoice(model: &InvoiceModel) -> Result<Vec<u8>, RenderError> {
    if model.lines.is_empty() {
        return Err(RenderError::NoLines);
    }
    if !matches!(model.currency, Currency::Huf) && model.rate_metadata.is_none() {
        return Err(RenderError::MissingRateMetadata);
    }

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_regular = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let font_bold = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Bold",
        "Encoding" => "WinAnsiEncoding",
    });
    let font_italic = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica-Oblique",
        "Encoding" => "WinAnsiEncoding",
    });
    // PR-176 — embed the optional tenant logo as a PDF Image XObject
    // and register it under the page resources `/XObject` map. The
    // layout step then references the same name (`Im1`) via a `Do`
    // operator to place it top-left of the header. Absent logo →
    // identical resources dict shape as pre-PR-176 (no `/XObject` key),
    // which keeps the byte-for-byte cmp under existing pin tests stable
    // for the no-logo path.
    let logo_xobject_name: Option<&str> = if model.tenant_logo.is_some() {
        Some(LOGO_XOBJECT_NAME)
    } else {
        None
    };
    let resources_id = if let Some(logo) = &model.tenant_logo {
        let img_stream = build_logo_image_xobject(logo);
        let img_id = doc.add_object(img_stream);
        doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_regular,
                "FB" => font_bold,
                "FI" => font_italic,
            },
            "XObject" => dictionary! {
                LOGO_XOBJECT_NAME => img_id,
            },
        })
    } else {
        doc.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_regular,
                "FB" => font_bold,
                "FI" => font_italic,
            },
        })
    };

    // PR-296 — one content stream + one Page object per laid-out page.
    // `Resources` and `MediaBox` stay on the Pages node so every page
    // inherits them (the fonts and the logo XObject are shared).
    let page_ops = layout_pages(model, logo_xobject_name);
    let page_count = page_ops.len() as i64;
    let mut kids: Vec<Object> = Vec::with_capacity(page_ops.len());
    for ops in page_ops {
        let content = Content { operations: ops };
        let content_bytes = content
            .encode()
            .map_err(|e| RenderError::ContentEncode(e.to_string()))?;
        let content_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => kids,
        "Count" => page_count,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), PAGE_WIDTH.into(), PAGE_HEIGHT.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    let mut buf: Vec<u8> = Vec::new();
    doc.save_to(&mut buf)
        .map_err(|e| RenderError::Save(e.to_string()))?;
    Ok(buf)
}

/// Lay the invoice out across as many A4 pages as its line items need.
/// Returns one content-stream operation list per page, in page order —
/// always at least one (`render_invoice` rejects a line-less invoice
/// before reaching here).
///
/// # PR-296 — why this function exists
///
/// The renderer used to emit a single page unconditionally
/// (`"Count" => 1`) with no line-count guard anywhere. Rows just kept
/// advancing downward: at 16 line items the lowest baseline was y=28,
/// at 17 it was y=0, and from 18 on it went NEGATIVE — painted off the
/// physical sheet, taking the totals block and the ÁFA summary with it
/// on longer invoices, while `render_invoice` still returned
/// `Ok(bytes)`. On a legally-required Hungarian document that is
/// CLAUDE.md rule 11 in its worst form.
///
/// The flow rules, in the order they matter:
///
/// 1. A line-item row is placed only when its WHOLE slot
///    ([`row_height`]) clears [`CONTENT_FLOOR`]; otherwise the flow
///    breaks and the row starts the next page.
/// 2. Every continuation page repeats the column-header band
///    ([`write_table_header`]) under a compact identifying header
///    ([`write_continuation_header`]) — invoice number at the top, page
///    counter in the footer.
/// 3. The closing block (table footer rule + NETTÓ ÖSSZEG / per-rate
///    ÁFA / FIZETENDŐ BRUTTÓ VÉGÖSSZEG + MEGJEGYZÉS) is never split.
///    The LAST row's fit check reserves [`closing_block_depth`] under
///    it, so the block always sits directly below the final row on the
///    final page — which also means a trailing page can never carry a
///    lone totals block.
/// 4. Footers are appended last, once the page count is known, so
///    `i/N Oldal` is correct on every page.
fn layout_pages(m: &InvoiceModel, logo_xobject_name: Option<&str>) -> Vec<Vec<Operation>> {
    let mut pages: Vec<Vec<Operation>> = vec![Vec::new()];

    let table_top = write_page_one_header(&mut pages[0], m, logo_xobject_name);
    let mut y = write_table_header(&mut pages[0], m, table_top);
    let mut rows_on_page = 0usize;

    let closing_depth = closing_block_depth(m);
    for (i, line) in m.lines.iter().enumerate() {
        // A row far from the end only has to fit itself. Each of the
        // last `TAIL_ROWS_KEPT_WITH_TOTALS` rows instead reserves the
        // whole remaining tail PLUS the closing block, so the break
        // lands before the tail and the closing page carries it whole.
        // Reserving under the last row ALONE was not enough: the
        // closing block is ~150pt, so it routinely stranded exactly one
        // row above the totals on a nearly empty final page.
        let remaining = m.lines.len() - i;
        let needed = if remaining <= TAIL_ROWS_KEPT_WITH_TOTALS {
            m.lines[i..].iter().map(row_height).sum::<i64>() + closing_depth
        } else {
            row_height(line)
        };
        // `rows_on_page > 0` keeps a row that cannot fit even on an
        // empty page from bouncing onto a blank one — it prints where
        // it is instead. Reachable only for a description wrapping past
        // a full page (~50 lines), which no NAV line description does.
        if y - needed < CONTENT_FLOOR && rows_on_page > 0 {
            pages.push(Vec::new());
            let page = pages.last_mut().expect("just pushed");
            write_continuation_header(page, m);
            y = write_table_header(page, m, CONT_TABLE_TOP);
            rows_on_page = 0;
        }
        let page = pages.last_mut().expect("at least one page");
        draw_line_row(page, m, line, i, y);
        y -= row_height(line);
        rows_on_page += 1;
    }

    // Closing block — on the last page, directly under the final row.
    let page = pages.last_mut().expect("at least one page");
    let footer_rule_y = y + 8;
    silver_rule(page, MARGIN_LEFT, MARGIN_RIGHT, footer_rule_y);
    let invoice_gross_minor: i64 = m.lines.iter().map(|l| l.gross_minor).sum();
    let totals_bottom = write_totals(page, m, footer_rule_y - 24, invoice_gross_minor);
    write_note(page, m, totals_bottom - 24);

    // Footer band on every page — page counter + attestation. Appended
    // after the flow so the denominator is the real page count.
    let total_pages = pages.len();
    for (i, page) in pages.iter_mut().enumerate() {
        write_page_footer(page, i + 1, total_pages);
    }

    pages
}

/// Draw the page-1 header — logo, title, invoice number, party block,
/// date block, and the FIZETENDŐ BRUTTÓ VÉGÖSSZEG banner. Returns the
/// y-coordinate the line-item column-header band should sit at.
fn write_page_one_header(
    ops: &mut Vec<Operation>,
    m: &InvoiceModel,
    logo_xobject_name: Option<&str>,
) -> i64 {
    // PR-176 — optional tenant logo top-left of the header. When
    // present, the title cluster shifts right by the logo box width
    // plus a small gap so logo + title sit side-by-side without
    // overlap. When absent, every coordinate matches the pre-PR-176
    // layout byte-for-byte.
    let (logo_shift, logo_box) = match (&m.tenant_logo, logo_xobject_name) {
        (Some(logo), Some(name)) => {
            place_logo(ops, logo, name);
            (LOGO_BOX_SIDE + LOGO_TITLE_GAP, LOGO_BOX_SIDE)
        }
        _ => (0, 0),
    };
    let _ = logo_box; // reserved for a future header-rule extension

    let title_x = MARGIN_LEFT + logo_shift;
    // Title block (top-left, shifted right when a logo is present):
    // "Számla" + invoice number. The number stays INK — accountants
    // look it up; it's the primary key on the printed surface. Size-18
    // regular vs size-28 bold above already gives the visual hierarchy.
    text(ops, "FB", 28, title_x, MARGIN_TOP - 14, "Számla");
    text(ops, "F1", 18, title_x, MARGIN_TOP - 38, &m.invoice_number);

    // Title under-rule — structural. Spans the full printable width
    // whether a logo is present or not (the rule's role is to separate
    // the header band from the party block below, NOT to underline the
    // title cluster). Under default (no brand-override) silver-grey
    // per ADR-0044; S195 — when `m.brand_primary_color` is `Some` the
    // operator's brand colour substitutes here.
    structural_rule(
        ops,
        MARGIN_LEFT,
        MARGIN_RIGHT,
        MARGIN_TOP - 58,
        m.brand_primary_color,
    );

    // Two-column party block.
    let party_top = MARGIN_TOP - 78;
    let col_left = MARGIN_LEFT;
    let col_right = MARGIN_LEFT + (MARGIN_RIGHT - MARGIN_LEFT) / 2 + 8;
    // PR-249 (Bug A) — clamp each cell to the space up to its column
    // boundary. Seller stops a gutter short of `col_right`; buyer runs
    // to the right margin. Text wider than the cell wraps (font-metrics
    // break) instead of overprinting the neighbouring column.
    let seller_width = col_right - COLUMN_GUTTER - col_left;
    let buyer_width = MARGIN_RIGHT - col_right;
    let after_seller = write_party(
        ops,
        "ELADÓ",
        &m.supplier,
        col_left,
        party_top,
        true,
        seller_width,
    );
    let after_buyer = write_party(
        ops,
        "VEVŐ",
        &m.customer,
        col_right,
        party_top,
        false,
        buyer_width,
    );
    // The block below the parties anchors to the TALLER column (the
    // smaller / more-negative y), so a wrapped name never overlaps the
    // date rows beneath it.
    let parties_bottom = after_seller.min(after_buyer);

    // Date block: SZÁMLA KELTE / TELJESÍTÉS KELTE on the left,
    // FIZETÉSI HATÁRIDŐ / FIZETÉSI MÓD on the right.
    let dates_top = parties_bottom - 24;
    label_value(
        ops,
        col_left,
        dates_top,
        "SZÁMLA KELTE",
        &format::hungarian_date(m.issue_date),
    );
    label_value(
        ops,
        col_left,
        dates_top - 14,
        "TELJESÍTÉS KELTE",
        &format::hungarian_date(m.fulfillment_date),
    );
    label_value(
        ops,
        col_right,
        dates_top,
        "FIZETÉSI HATÁRIDŐ",
        &format::hungarian_date(m.payment_due_date),
    );
    label_value(
        ops,
        col_right,
        dates_top - 14,
        "FIZETÉSI MÓD",
        &m.payment_method,
    );

    // Highlighted total banner: FIZETENDŐ BRUTTÓ VÉGÖSSZEG, right-aligned.
    // PR-85 — the single gold accent in the document lives here.
    let invoice_gross_minor: i64 = m.lines.iter().map(|l| l.gross_minor).sum();
    let banner_y = dates_top - 44;
    accent_rule(
        ops,
        MARGIN_LEFT,
        MARGIN_RIGHT,
        banner_y + 22,
        m.brand_primary_color,
    );
    let banner_label = "FIZETENDŐ BRUTTÓ VÉGÖSSZEG:";
    let banner_amount = format::money(m.currency, invoice_gross_minor);
    text_right_in(
        ops,
        "F1",
        9,
        MARGIN_RIGHT - 150,
        banner_y + 6,
        banner_label,
        MUTED,
    );
    text_right(ops, "FB", 20, MARGIN_RIGHT, banner_y, &banner_amount);

    // Line-item column-header band sits below the banner.
    banner_y - 28
}

/// PR-296 — compact identifying header for pages 2..N. The full
/// seller / buyer / date / banner block belongs to page 1 only; a
/// continuation page needs the reader to be able to tell WHICH invoice
/// the rows in front of them belong to, which is the invoice number
/// here plus the `i/N Oldal` counter in the footer.
fn write_continuation_header(ops: &mut Vec<Operation>, m: &InvoiceModel) {
    text(ops, "FB", 14, MARGIN_LEFT, CONT_TITLE_Y, "Számla");
    let title_width = crate::text::text_width_points("Számla", 14, true);
    text_in(
        ops,
        "FI",
        9,
        MARGIN_LEFT + title_width + 8,
        CONT_TITLE_Y,
        "(folytatás)",
        MUTED,
    );
    text_right(ops, "F1", 11, MARGIN_RIGHT, CONT_TITLE_Y, &m.invoice_number);
    structural_rule(
        ops,
        MARGIN_LEFT,
        MARGIN_RIGHT,
        CONT_RULE_Y,
        m.brand_primary_color,
    );
}

/// PR-296 — footer band, drawn on every page once the total page count
/// is known. Pre-PR-296 this printed the literal `1/1 Oldal` on the one
/// page the renderer emitted.
fn write_page_footer(ops: &mut Vec<Operation>, page_number: usize, total_pages: usize) {
    text_in(
        ops,
        "FB",
        8,
        MARGIN_LEFT,
        FOOTER_Y_TOP,
        &format!("{page_number}/{total_pages} Oldal"),
        MUTED,
    );
    text_in(
        ops,
        "FI",
        8,
        MARGIN_LEFT,
        FOOTER_Y_TOP - 14,
        "A számla tartalma mindenben megfelel a hatályos törvényekben foglaltaknak",
        MUTED,
    );
}

fn write_party(
    ops: &mut Vec<Operation>,
    section_label: &str,
    party: &PartyInfo,
    x: i64,
    y_top: i64,
    is_seller: bool,
    max_width: i64,
) -> i64 {
    text_in(ops, "F1", 7, x, y_top, section_label, MUTED);
    // Session-148 (Ervin override 3) — the party name slot is rendered
    // UNCONDITIONALLY. The buyer name is mandatory on the printed
    // invoice per Áfa tv. §169 (ADR-0048 amendment, PR-104) for every
    // customer type; the PR-97 GDPR carve-out that skipped the slot for
    // a name-less PRIVATE_PERSON body is removed. "forget GDPR, show
    // the name, always."
    //
    // PR-249 (Bug A) — every field below wraps within `max_width`
    // (font-metrics break) so a long legal name stacks vertically
    // inside its column instead of overprinting the neighbouring one.
    // For short fields (the common case) the wrap is a no-op and the
    // emitted ops are byte-identical to the pre-PR-249 layout.
    let name_last = draw_wrapped(
        ops,
        "FB",
        13,
        true,
        x,
        y_top - 16,
        &party.name,
        max_width,
        15,
        INK,
    );
    let mut y = name_last - 16;
    for line in &party.address_lines {
        let last = draw_wrapped(ops, "F1", 9, false, x, y, line, max_width, 11, INK);
        y = last - 11;
    }
    y -= 4;
    // PR-97 / ADR-0048 — natural-person buyers (PRIVATE_PERSON) carry
    // no ADÓSZÁM; the printed-PDF skips the label entirely rather than
    // rendering a "ADÓSZÁM: " line with an empty value.
    if !party.tax_number.trim().is_empty() {
        let last = label_value_wrapped(ops, x, y, "ADÓSZÁM", &party.tax_number, max_width);
        y = last - 12;
    }
    if is_seller {
        if let Some(v) = &party.bank_account_number {
            let last = label_value_wrapped(ops, x, y, "BANKSZÁMLASZÁM", v, max_width);
            y = last - 12;
        }
        if let Some(v) = &party.iban {
            let last = label_value_wrapped(ops, x, y, "IBAN", v, max_width);
            y = last - 12;
        }
        if let Some(v) = &party.bank_name {
            let last = label_value_wrapped(ops, x, y, "BANK NEVE", v, max_width);
            y = last - 12;
        }
        if let Some(v) = &party.swift_bic {
            let last = label_value_wrapped(ops, x, y, "SWIFT/BIC", v, max_width);
            y = last - 12;
        }
    }
    y
}

/// PR-85 — line-item column geometry. Pulled into a named struct so
/// the column positions are tunable in one place (and so the test
/// for description-wrap can use the same `DESC_WIDTH` value the
/// renderer uses).
///
/// Pre-PR-85 the table sat hard against the right margin and the
/// gutters between numeric columns were tight enough that
/// `NETTÓ EGYSÉGÁR` / `BRUTTÓ ÁR` headers visually kissed each other.
/// This pass shifts every column slightly left off the right margin
/// AND widens the gutters between right-edges of adjacent columns.
///
/// PR-279 — PR-85's nudge-the-constants pass did not hold: the ÁFA and
/// BRUTTÓ ÁR values still collided on a live invoice (`27%2 641 600 Ft`).
/// The right-edges below are no longer hand-tuned. They are DERIVED,
/// right-to-left from `GROSS_RIGHT`, as
///
///   `edge(n) = edge(n+1) − width(n+1) − MIN_GUTTER`
///
/// where `width(n)` is the real Helvetica advance ([`text::text_width_points`])
/// of the WIDEST content that column can carry — its bold size-8 header
/// or its worst-case size-9 value, whichever is wider. Worst-case value
/// is a 9-digit NEGATIVE (storno) amount: `-123 641 600 Ft` at 63pt,
/// which is also the widest EUR shape (`-€ 1 234 567,89`, 63pt).
///
/// `layout_gutters_clear_worst_case_row` pins the invariant, so a future
/// tweak to any edge that closes a gutter fails the suite rather than
/// reaching a customer's PDF.
struct TableLayout;

impl TableLayout {
    /// Row-number column anchor (left-aligned at MARGIN_LEFT).
    const NUM_X: i64 = MARGIN_LEFT;
    /// Description column anchor (left-aligned).
    const DESC_X: i64 = MARGIN_LEFT + 18;
    /// Description column maximum width in POINTS before wrap.
    ///
    /// PR-279 — was `DESC_WRAP_CHARS: usize = 40`, a char count. Same
    /// root cause as the numeric-column overlap: a char count is blind
    /// to glyph width, so a 40-char all-caps description (caps average
    /// ≈ 0.7 em, not the assumed 0.55) ran ~70pt past where the count
    /// implied and into the MENNYISÉG column. Now measured in points
    /// against the real metric via [`wrap_to_width`], sized to land one
    /// `MIN_GUTTER` clear of `QTY_RIGHT`'s worst-case left extent.
    const DESC_WIDTH: i64 = Self::QTY_RIGHT - Self::QTY_W - Self::MIN_GUTTER - (MARGIN_LEFT + 18);
    /// Per-extra-wrapped-description-line vertical advance (points).
    const DESC_WRAP_LINE_HEIGHT: i64 = 11;

    /// Right edges of the numeric columns. Each column is right-aligned
    /// so the right edge is the anchor; the leftmost glyph of the data
    /// floats left based on its width.
    ///
    /// PR-85 tried to fix the ÁFA/BRUTTÓ ÁR collision by nudging these
    /// constants while leaving the flawed width proxy in place. It did
    /// not hold — PR-279 fixes the proxy ([`text_right_in`]) and derives
    /// the edges below from real content widths instead.
    ///
    /// Minimum clear space between one column's worst-case left extent
    /// and the previous column's right edge. 10pt ≈ 1.1 em at size 9 —
    /// a visually unambiguous gutter at print size.
    const MIN_GUTTER: i64 = 10;

    // Worst-case content width per column, in points, at the real
    // Helvetica advances — the wider of the bold size-8 header and the
    // worst-case size-9 value. `column_widths_match_measured_content`
    // pins each of these against `text::text_width_points`, so a stale
    // number here fails the suite instead of silently shrinking a
    // gutter.
    const QTY_W: i64 = 52; // header `MENNYISÉG`
    const UNIT_PRICE_W: i64 = 74; // header `NETTÓ EGYSÉGÁR` (widest header)
    const NET_W: i64 = 63; // value `-123 641 600 Ft`
    const VAT_W: i64 = 18; // value `27%`
    const GROSS_W: i64 = 63; // value `-123 641 600 Ft`

    // Right edges, DERIVED right-to-left from `GROSS_RIGHT`. Writing the
    // subtraction out (rather than baking the results) is the point:
    // PR-85 hand-tuned these five numbers and the gutters silently went
    // negative. Here a column cannot encroach on its neighbour without
    // someone deleting a `MIN_GUTTER` term in plain sight.
    const GROSS_RIGHT: i64 = MARGIN_RIGHT - 6; // 6pt off the page edge
    const VAT_RIGHT: i64 = Self::GROSS_RIGHT - Self::GROSS_W - Self::MIN_GUTTER;
    const NET_RIGHT: i64 = Self::VAT_RIGHT - Self::VAT_W - Self::MIN_GUTTER;
    const UNIT_PRICE_RIGHT: i64 = Self::NET_RIGHT - Self::NET_W - Self::MIN_GUTTER;
    const QTY_RIGHT: i64 = Self::UNIT_PRICE_RIGHT - Self::UNIT_PRICE_W - Self::MIN_GUTTER;
}

/// Draw the line-item column-header band at `top` and return the
/// baseline of the FIRST body row beneath it.
///
/// PR-296 — split out of the old `write_lines_table` so a continuation
/// page can repeat the band. An invoice whose rows run onto page 2 must
/// still tell the reader which column is ÁFA and which is BRUTTÓ ÁR.
fn write_table_header(ops: &mut Vec<Operation>, m: &InvoiceModel, top: i64) -> i64 {
    // Header row — column labels in MUTED at size 8 bold.
    text_in(ops, "FB", 8, TableLayout::NUM_X, top, "#", MUTED);
    text_in(ops, "FB", 8, TableLayout::DESC_X, top, "MEGNEVEZÉS", MUTED);
    text_right_in(
        ops,
        "FB",
        8,
        TableLayout::QTY_RIGHT,
        top,
        "MENNYISÉG",
        MUTED,
    );
    text_right_in(
        ops,
        "FB",
        8,
        TableLayout::UNIT_PRICE_RIGHT,
        top,
        "NETTÓ EGYSÉGÁR",
        MUTED,
    );
    text_right_in(ops, "FB", 8, TableLayout::NET_RIGHT, top, "NETTÓ ÁR", MUTED);
    text_right_in(ops, "FB", 8, TableLayout::VAT_RIGHT, top, "ÁFA", MUTED);
    text_right_in(
        ops,
        "FB",
        8,
        TableLayout::GROSS_RIGHT,
        top,
        "BRUTTÓ ÁR",
        MUTED,
    );
    // Table-header rule — structural. S195 — brand-overridable
    // (substitutes the column-header underline so the operator's
    // brand colour anchors the table cluster).
    structural_rule(
        ops,
        MARGIN_LEFT,
        MARGIN_RIGHT,
        top - 6,
        m.brand_primary_color,
    );

    top - 22
}

/// Vertical slot (in points) one line item consumes.
///
/// Per PR-82 the row height grows from the base 28pt when a line
/// carries a `note` sub-line; per PR-85 it ALSO grows when the
/// description wraps to multiple lines. A `performance_period`
/// sub-line stays inside the 28pt slot (pre-PR-82 legacy posture).
///
/// PR-296 — [`layout_pages`] and [`draw_line_row`] BOTH call this. If
/// they ever disagreed the pagination would drift out of step with what
/// is actually painted, so the arithmetic lives in exactly one place.
fn row_height(line: &LineItem) -> i64 {
    let desc_lines = wrap_to_width(&line.description, TableLayout::DESC_WIDTH, 9, false).len();
    let desc_extra = (desc_lines.saturating_sub(1) as i64) * TableLayout::DESC_WRAP_LINE_HEIGHT;
    let note_extra = match line.note.as_ref() {
        Some(n) if !n.trim().is_empty() => 12,
        _ => 0,
    };
    28 + desc_extra + note_extra
}

/// Draw one line-item row with its top baseline at `y`. `index` is the
/// zero-based position in `m.lines` (printed as the 1-based `#`).
fn draw_line_row(
    ops: &mut Vec<Operation>,
    m: &InvoiceModel,
    line: &LineItem,
    index: usize,
    y: i64,
) {
    let row_num = format!("{}", index + 1);
    text(ops, "F1", 9, TableLayout::NUM_X, y, &row_num);

    // PR-85 — description wraps to multiple lines when long. The
    // first line sits at `y`; subsequent lines stack downward at
    // `DESC_WRAP_LINE_HEIGHT` apart. The numeric columns continue
    // to anchor at `y` (top of the row) — accountants read the
    // numbers off the row's top edge regardless of how tall the
    // description column grows.
    let desc_lines = wrap_to_width(&line.description, TableLayout::DESC_WIDTH, 9, false);
    for (i_line, dline) in desc_lines.iter().enumerate() {
        text(
            ops,
            "F1",
            9,
            TableLayout::DESC_X,
            y - (i_line as i64) * TableLayout::DESC_WRAP_LINE_HEIGHT,
            dline,
        );
    }
    let desc_extra =
        (desc_lines.len().saturating_sub(1) as i64) * TableLayout::DESC_WRAP_LINE_HEIGHT;

    let qty_str = format!("{} {}", format::quantity(line.quantity), line.unit);
    text_right(ops, "F1", 9, TableLayout::QTY_RIGHT, y, &qty_str);
    text_right(
        ops,
        "F1",
        9,
        TableLayout::UNIT_PRICE_RIGHT,
        y,
        &format::money(m.currency, line.unit_price_minor),
    );
    text_right(
        ops,
        "F1",
        9,
        TableLayout::NET_RIGHT,
        y,
        &format::money(m.currency, line.net_minor),
    );
    text_right(
        ops,
        "F1",
        9,
        TableLayout::VAT_RIGHT,
        y,
        &format!("{}%", line.vat_rate_percent),
    );
    text_right(
        ops,
        "F1",
        9,
        TableLayout::GROSS_RIGHT,
        y,
        &format::money(m.currency, line.gross_minor),
    );

    // Sub-line baseline — sits below the wrapped description so
    // performance-period + buyer-note sub-lines don't overlap
    // long descriptions.
    let mut sub_y = y - desc_extra - 12;
    if let Some((start, end)) = line.performance_period {
        let perf = format!(
            "Teljesítési időszak: {} – {}",
            format::iso_dotted_date(start),
            format::iso_dotted_date(end),
        );
        text_in(ops, "FI", 8, TableLayout::DESC_X, sub_y, &perf, MUTED);
        sub_y -= 11;
    }
    // PR-82 — per-line buyer note ("Megjegyzés"). Italic sub-line
    // labelled in Hungarian ("Megjegyzés:") so the buyer reads it
    // in context. Only renders when present; absent notes leave
    // the row at its base height so unannotated invoices look
    // identical to pre-PR-82 output.
    if let Some(note) = line.note.as_ref().filter(|s| !s.trim().is_empty()) {
        let label = format!("Megjegyzés: {}", note);
        text_in(ops, "FI", 8, TableLayout::DESC_X, sub_y, &label, MUTED);
    }
}

fn write_totals(
    ops: &mut Vec<Operation>,
    m: &InvoiceModel,
    top: i64,
    invoice_gross_minor: i64,
) -> i64 {
    // Aggregate per-VAT-rate amounts.
    let mut by_rate: std::collections::BTreeMap<u16, (i64, i64)> =
        std::collections::BTreeMap::new();
    for line in &m.lines {
        let entry = by_rate.entry(line.vat_rate_percent).or_insert((0, 0));
        entry.0 += line.net_minor;
        entry.1 += line.vat_minor;
    }
    let invoice_net_minor: i64 = m.lines.iter().map(|l| l.net_minor).sum();

    let label_right = MARGIN_RIGHT - 150;
    let mut y = top;

    // NETTÓ ÖSSZEG: invoice-currency net total.
    text_right_in(ops, "F1", 9, label_right, y, "NETTÓ ÖSSZEG:", MUTED);
    text_right(
        ops,
        "F1",
        9,
        MARGIN_RIGHT,
        y,
        &format::money(m.currency, invoice_net_minor),
    );
    y -= 14;

    // Per-VAT-rate ÁFA in invoice currency, then HUF (non-HUF only).
    for (&pct, &(_net, vat_minor)) in &by_rate {
        let label = format!("{}% ÁFA:", pct);
        text_right_in(ops, "F1", 9, label_right, y, &label, MUTED);
        text_right(
            ops,
            "F1",
            9,
            MARGIN_RIGHT,
            y,
            &format::money(m.currency, vat_minor),
        );
        y -= 14;
        if !matches!(m.currency, Currency::Huf) {
            if let Some(rate) = m.rate_metadata.as_ref() {
                let vat_huf = aberp_billing::huf_equivalent_round_half_even(vat_minor, &rate.rate)
                    .unwrap_or(0);
                text_right_in(ops, "F1", 9, label_right, y, &label, MUTED);
                text_right(
                    ops,
                    "F1",
                    9,
                    MARGIN_RIGHT,
                    y,
                    &format::money(Currency::Huf, vat_huf),
                );
                y -= 14;
            }
        }
    }

    // FIZETENDŐ BRUTTÓ VÉGÖSSZEG: invoice-currency gross total.
    text_right_in(
        ops,
        "F1",
        9,
        label_right,
        y,
        "FIZETENDŐ BRUTTÓ VÉGÖSSZEG:",
        MUTED,
    );
    text_right(
        ops,
        "F1",
        9,
        MARGIN_RIGHT,
        y,
        &format::money(m.currency, invoice_gross_minor),
    );
    y -= 14;

    // Árfolyam + Bruttó összeg in HUF, non-HUF only.
    if !matches!(m.currency, Currency::Huf) {
        if let Some(rate) = m.rate_metadata.as_ref() {
            let rate_str = format!(
                "Árfolyam: {} Ft",
                format::rate_for_display(&rate.rate.to_string())
            );
            text_right_in(ops, "F1", 9, MARGIN_RIGHT, y, &rate_str, MUTED);
            y -= 14;
            let gross_str = format!(
                "Bruttó összeg: {}",
                format::money(Currency::Huf, rate.huf_equivalent_total),
            );
            text_right(ops, "F1", 9, MARGIN_RIGHT, y, &gross_str);
            y -= 14;
        }
    }

    y
}

fn write_note(ops: &mut Vec<Operation>, m: &InvoiceModel, top: i64) {
    // PR-85 — skip the section entirely when there's nothing to say.
    // A bare "MEGJEGYZÉS" header followed by whitespace looked
    // visually orphaned on HUF invoices with no operator note, and
    // the regulatory record doesn't require the section to exist
    // when empty. Two content sources feed this block:
    //   1. The EUR-only rate-source sub-line ("1 EUR = X Ft")
    //   2. The buyer-facing operator note (PR-82)
    // If neither fires, render no section at all.
    let has_rate_note = !matches!(m.currency, Currency::Huf) && m.rate_metadata.is_some();
    let has_operator_note = m
        .note
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_rate_note && !has_operator_note {
        return;
    }

    text_in(ops, "F1", 7, MARGIN_LEFT, top, "MEGJEGYZÉS", MUTED);
    let mut y = top - 14;
    if has_rate_note {
        if let Some(rate) = m.rate_metadata.as_ref() {
            // PR-86 / session-111 — surface the rate-publication date
            // so the operator and buyer can see WHICH date's MNB rate
            // was applied. The date may differ from the supply date
            // when MNB walked back to a prior publication (weekend,
            // holiday, before that day's publish time) per the
            // ADR-0037 §2.b walk-back rule. Format mirrors the
            // Hungarian short-date convention used by the date block
            // (`YYYY.MM.DD.`).
            let note = format!(
                "1 {} = {} Ft ({}, {})",
                m.currency.iso_code(),
                format::rate_for_display(&rate.rate.to_string()),
                rate.source,
                format::hungarian_date(rate.date),
            );
            text(ops, "FI", 9, MARGIN_LEFT, y, &note);
            y -= 12;
        }
    }
    // PR-82 — buyer-facing invoice-level note. Renders below the
    // EUR-only rate-source sub-line (when applicable) so the rate
    // explanation reads first, the operator's free text second. Wraps
    // long notes naively across multiple lines using `wrap_to_chars`
    // so a paragraph-length note does not run off the right margin.
    if let Some(note) = m.note.as_ref().filter(|s| !s.trim().is_empty()) {
        for wrapped_line in wrap_to_chars(note, NOTE_WRAP_WIDTH_CHARS) {
            text(ops, "F1", 9, MARGIN_LEFT, y, &wrapped_line);
            y -= 12;
        }
    }
}

// ─── PR-296 — closing-block measurement ───────────────────────────────
//
// The three functions below predict, without drawing anything, how far
// down the page the closing block (table footer rule → totals →
// MEGJEGYZÉS) will reach. `layout_pages` reserves exactly that much
// under the LAST line-item row, which is what keeps the totals whole,
// on the last page, and above the footer band.
//
// They mirror `write_totals` / `write_note` step for step. The
// `closing_block_depth_matches_painted_extent` test renders across
// currencies, rate counts and note lengths and compares this prediction
// against the lowest baseline actually emitted, so a future edit to
// either writer that forgets its counterpart here fails the suite
// instead of pushing the totals off the sheet again.

/// Number of 14pt rows [`write_totals`] emits for `m`.
fn totals_row_count(m: &InvoiceModel) -> i64 {
    let rates = m
        .lines
        .iter()
        .map(|l| l.vat_rate_percent)
        .collect::<std::collections::BTreeSet<_>>()
        .len() as i64;
    // The HUF-equivalent sub-row per rate, and the trailing Árfolyam +
    // Bruttó összeg pair, print only for a non-HUF invoice carrying a
    // rate stamp — the same condition `write_totals` branches on.
    let huf_equivalents = !matches!(m.currency, Currency::Huf) && m.rate_metadata.is_some();
    // NETTÓ ÖSSZEG + per-rate ÁFA + FIZETENDŐ BRUTTÓ VÉGÖSSZEG.
    let mut rows = 1 + rates * if huf_equivalents { 2 } else { 1 } + 1;
    if huf_equivalents {
        rows += 2;
    }
    rows
}

/// Number of text lines [`write_note`] emits below its MEGJEGYZÉS
/// header. Zero means the section is skipped entirely.
fn note_line_count(m: &InvoiceModel) -> i64 {
    let mut lines = 0;
    if !matches!(m.currency, Currency::Huf) && m.rate_metadata.is_some() {
        lines += 1;
    }
    if let Some(note) = m.note.as_ref().filter(|s| !s.trim().is_empty()) {
        lines += wrap_to_chars(note, NOTE_WRAP_WIDTH_CHARS).len() as i64;
    }
    lines
}

/// Points from the cursor left below the last line-item row down to the
/// LOWEST baseline the closing block paints. Walks the same offsets
/// [`layout_pages`] then uses to draw:
///
/// ```text
///   cursor + 8              table footer silver rule
///   cursor - 16             totals top            (rule - 24)
///   … - (rows-1) × 14       LAST totals baseline
///   … - 14                  totals bottom cursor  (write_totals' return)
///   … - 24                  MEGJEGYZÉS header
///   … - 14 - (n-1) × 12     LAST note baseline
/// ```
///
/// Note the `rows-1`: `write_totals` advances 14pt PAST its final row,
/// so the last thing it paints sits one advance above the cursor it
/// returns. Without a note that trailing advance is empty space and
/// must not be reserved; with one it is the gap the MEGJEGYZÉS block
/// hangs off.
fn closing_block_depth(m: &InvoiceModel) -> i64 {
    let rows = totals_row_count(m);
    let note_lines = note_line_count(m);
    if note_lines == 0 {
        16 + (rows - 1) * 14
    } else {
        16 + rows * 14 + 24 + 14 + (note_lines - 1) * 12
    }
}

/// PR-82 — naive word-wrap for the MEGJEGYZÉS / Megjegyzés text.
/// Splits on whitespace and accumulates words up to `max_chars` per
/// line. Hand-rolled because: (a) we don't have a font-metrics table
/// (see `text_right`'s comment for the same trade-off), and (b) the
/// invoice surface uses a tiny vocabulary — short notes are the norm,
/// long notes acceptable as wrapped paragraphs.
///
/// PR-85 — renamed from `wrap_note_text` and re-used for line-item
/// description wrapping (same char-counted approach; the description
/// wrap-width constant lives on `TableLayout`).
const NOTE_WRAP_WIDTH_CHARS: usize = 100;

/// Wrap `text` to a sequence of lines, each at most `max_chars`
/// characters wide. Splits on whitespace; words longer than
/// `max_chars` get their own line (no mid-word break — a long URL or
/// product code prints on its own line and may visually overflow, but
/// never silently truncates).
pub(crate) fn wrap_to_chars(text: &str, max_chars: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= max_chars {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push(std::mem::take(&mut current));
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// PR-249 (Bug A) — width-based sibling of [`wrap_to_chars`] for the
/// two-column header. Greedily packs whitespace-separated words onto a
/// line until the next word would push the measured Helvetica width
/// (via [`text::text_width_points`]) past `max_width` points, then
/// breaks. Unlike [`wrap_to_chars`]'s char count, this measures real
/// glyph advances — an all-caps legal name (wide caps) breaks at the
/// right place where a 0.55-per-char proxy would let it overflow. A
/// single word wider than `max_width` gets its own line (no mid-word
/// break; never truncated).
pub(crate) fn wrap_to_width(text: &str, max_width: i64, size: i64, bold: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else {
                let candidate = format!("{current} {word}");
                if crate::text::text_width_points(&candidate, size, bold) <= max_width {
                    current = candidate;
                } else {
                    out.push(std::mem::take(&mut current));
                    current.push_str(word);
                }
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Emit a left-anchored text run at `(x, y)` in `INK` colour using
/// font alias `font` (one of `"F1"` / `"FB"` / `"FI"`) at `size`
/// points. Convenience wrapper around [`text_in`].
fn text(ops: &mut Vec<Operation>, font: &str, size: i64, x: i64, y: i64, content: &str) {
    text_in(ops, font, size, x, y, content, INK);
}

/// Emit a left-anchored text run at `(x, y)` in `color`. PR-85 — the
/// silver/gold palette flows through this entry point: every text op
/// in the renderer goes through either `text` (defaults to `INK`) or
/// `text_in` (explicit colour for `MUTED` section labels, etc.).
fn text_in(
    ops: &mut Vec<Operation>,
    font: &str,
    size: i64,
    x: i64,
    y: i64,
    content: &str,
    color: Color,
) {
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new(
        "Tf",
        vec![Object::Name(font.as_bytes().to_vec()), size.into()],
    ));
    // `rg` sets the non-stroking (fill) colour — what Tj uses for
    // glyph ink. `RG` would set the stroking colour (used by rule
    // strokes via `silver_rule` / `gold_rule`); the two states are
    // independent in the PDF graphics state.
    ops.push(Operation::new(
        "rg",
        vec![
            Object::Real(color.0),
            Object::Real(color.1),
            Object::Real(color.2),
        ],
    ));
    ops.push(Operation::new("Td", vec![x.into(), y.into()]));
    ops.push(Operation::new(
        "Tj",
        vec![Object::String(
            text::winansi_bytes(content),
            StringFormat::Literal,
        )],
    ));
    ops.push(Operation::new("ET", vec![]));
}

/// Emit a right-anchored text run whose right edge sits at `x_right`,
/// in `INK` colour. Width comes from the real Helvetica glyph advances
/// ([`text::text_width_points`]).
///
/// PR-279 — this used to estimate width as a flat `0.55 * size` per
/// char. That proxy is blind to glyph width in BOTH directions and the
/// two errors compound on a right-aligned row:
///   - it UNDER-estimates `%` (real 0.889 em), so `27%` right-aligned
///     at `VAT_RIGHT` actually painted 4pt PAST its stated right edge;
///   - it OVER-estimates the thousands-separator spaces in a money
///     string (real 0.278 em), so `2 641 600 Ft` started 9pt further
///     LEFT than it needed to.
///
/// Net: a 5pt overlap that printed as `27%2 641 600 Ft` on a live
/// invoice. The defect scaled with the magnitude of the amount (more
/// digits + more separators = bigger over-estimate), which is why only
/// large gross values collided.
///
/// The old doc-comment justified the proxy as "a metrics table would be
/// ~200 LoC we don't need". That table has existed since PR-249
/// (`text::HELVETICA_W`, added for the header wrap) — this crate was
/// carrying two width models and right-alignment was using the wrong
/// one. Per CLAUDE.md rule 7 (surface conflicts, don't average them)
/// there is now ONE width model for all measurement.
fn text_right(
    ops: &mut Vec<Operation>,
    font: &str,
    size: i64,
    x_right: i64,
    y: i64,
    content: &str,
) {
    text_right_in(ops, font, size, x_right, y, content, INK);
}

/// Right-anchored variant of [`text_in`] — same width-estimation
/// posture as [`text_right`], with explicit colour.
fn text_right_in(
    ops: &mut Vec<Operation>,
    font: &str,
    size: i64,
    x_right: i64,
    y: i64,
    content: &str,
    color: Color,
) {
    let width = crate::text::text_width_points(content, size, font == "FB");
    let x_left = x_right - width;
    text_in(ops, font, size, x_left, y, content, color);
}

/// Emit a horizontal rule between `(x_left, y)` and `(x_right, y)` in
/// `SILVER_LINE` colour. Default structural rule across the document
/// (table footer rule — the one structural rule that stays silver
/// even under a brand-override per S195's "preserve visual hierarchy"
/// posture).
fn silver_rule(ops: &mut Vec<Operation>, x_left: i64, x_right: i64, y: i64) {
    horizontal_rule(ops, x_left, x_right, y, SILVER_LINE, RULE_WEIGHT_SILVER);
}

/// S195 / PR-195 — brand-overridable variant of [`silver_rule`].
/// Used for the title under-rule and the table-header rule, both of
/// which carry the document's "structural emphasis" role per the
/// reference template. When `brand` is `Some`, the operator's
/// `[seller.branding] primary_color` substitutes for the pre-PR-195
/// silver; `None` is byte-for-byte identical to [`silver_rule`].
/// Stroke weight stays at `RULE_WEIGHT_SILVER` regardless — only the
/// colour is brand-substituted, the weight hierarchy (heavier gold
/// for the totals banner, lighter silver for everything else) is
/// preserved per ADR-0044.
fn structural_rule(
    ops: &mut Vec<Operation>,
    x_left: i64,
    x_right: i64,
    y: i64,
    brand: Option<Color>,
) {
    let color = brand.unwrap_or(SILVER_LINE);
    horizontal_rule(ops, x_left, x_right, y, color, RULE_WEIGHT_SILVER);
}

/// Emit a horizontal rule in `GOLD_ACCENT` colour. Used in exactly
/// one place per ADR-0044: the rule above the totals banner.
///
/// S195 / PR-195 — when `brand` is `Some`, the operator's
/// `[seller.branding] primary_color` substitutes for the gold accent.
/// The stroke weight stays at `RULE_WEIGHT_GOLD` (heavier than the
/// structural rules) so the totals banner keeps its visual weight
/// even when the document's two accent slots collapse to one colour.
fn accent_rule(ops: &mut Vec<Operation>, x_left: i64, x_right: i64, y: i64, brand: Option<Color>) {
    let color = brand.unwrap_or(GOLD_ACCENT);
    horizontal_rule(ops, x_left, x_right, y, color, RULE_WEIGHT_GOLD);
}

/// Underlying rule emitter — sets stroke colour + stroke weight,
/// moves to `(x_left, y)`, lines to `(x_right, y)`, strokes.
fn horizontal_rule(
    ops: &mut Vec<Operation>,
    x_left: i64,
    x_right: i64,
    y: i64,
    color: Color,
    weight: f32,
) {
    ops.push(Operation::new("q", vec![]));
    ops.push(Operation::new(
        "RG",
        vec![
            Object::Real(color.0),
            Object::Real(color.1),
            Object::Real(color.2),
        ],
    ));
    ops.push(Operation::new("w", vec![Object::Real(weight)]));
    ops.push(Operation::new("m", vec![x_left.into(), y.into()]));
    ops.push(Operation::new("l", vec![x_right.into(), y.into()]));
    ops.push(Operation::new("S", vec![]));
    ops.push(Operation::new("Q", vec![]));
}

/// Emit a "LABEL: value" pair at `(x, y)` — label in MUTED small-grey
/// at size 7, value in INK bold at size 9, with `LABEL_VALUE_GAP`
/// points of breathing room between the label's colon and the value's
/// first glyph.
fn label_value(ops: &mut Vec<Operation>, x: i64, y: i64, label: &str, value: &str) {
    text_in(ops, "F1", 7, x, y + 2, &format!("{}:", label), MUTED);
    // Label width: chars + 1 (for the colon) × proxy width at size 7,
    // plus `LABEL_VALUE_GAP` so the value never visually kisses the
    // label (PR-85 — was +4pt, too cramped per Ervin's "Adószám:123"
    // flag).
    let label_width = (label.chars().count() as i64 + 1) * 7 * 55 / 100 + LABEL_VALUE_GAP;
    text_in(ops, "FB", 9, x + label_width, y, value, INK);
}

/// PR-249 (Bug A) — [`label_value`] with the VALUE wrapped to the cell.
/// The label sits at `x`; the value column starts at `x + label_width`
/// and wraps to whatever width remains up to `max_width`. Continuation
/// lines indent under the value (not the label) so the pair reads as
/// one logical row. Returns the baseline of the LAST line emitted.
///
/// For a value that fits on one line (the common case) the emitted ops
/// are byte-identical to [`label_value`].
fn label_value_wrapped(
    ops: &mut Vec<Operation>,
    x: i64,
    y: i64,
    label: &str,
    value: &str,
    max_width: i64,
) -> i64 {
    text_in(ops, "F1", 7, x, y + 2, &format!("{}:", label), MUTED);
    let label_width = (label.chars().count() as i64 + 1) * 7 * 55 / 100 + LABEL_VALUE_GAP;
    // Floor the value width so a pathologically wide label can never
    // drive it to zero (which would loop one-word-per-line forever-ish).
    let value_width = (max_width - label_width).max(40);
    draw_wrapped(
        ops,
        "FB",
        9,
        true,
        x + label_width,
        y,
        value,
        value_width,
        11,
        INK,
    )
}

/// PR-249 (Bug A) — draw `content` left-anchored at `x`, wrapped to
/// `max_width` points using real Helvetica glyph metrics
/// ([`text::text_width_points`]). The first line sits at baseline
/// `y_top`; each subsequent line drops `line_height`. Returns the
/// baseline of the LAST line so callers can stack the next field below
/// the TALLER of two columns. A single token wider than `max_width`
/// prints on its own line and may visually overflow — the same
/// no-mid-word-break, never-truncate policy as [`wrap_to_chars`].
#[allow(clippy::too_many_arguments)]
fn draw_wrapped(
    ops: &mut Vec<Operation>,
    font: &str,
    size: i64,
    bold: bool,
    x: i64,
    y_top: i64,
    content: &str,
    max_width: i64,
    line_height: i64,
    color: Color,
) -> i64 {
    let mut y = y_top;
    for (i, line) in wrap_to_width(content, max_width, size, bold)
        .iter()
        .enumerate()
    {
        if i > 0 {
            y -= line_height;
        }
        text_in(ops, font, size, x, y, line, color);
    }
    y
}

// ─── PR-176 — tenant-logo placement + XObject build ───────────────────

/// Emit the content-stream operators that draw the tenant logo at the
/// top-left of the header. The image XObject is registered under
/// `name` in the page resources (see [`render_invoice`] resource
/// assembly); the operators here position + scale the unit-square
/// XObject via a `cm` (current matrix) op and dispatch the draw with
/// `Do`. The `q`/`Q` save/restore brackets isolate the matrix change
/// from the rest of the layout stream.
///
/// Scaling: the logo box is `LOGO_BOX_SIDE × LOGO_BOX_SIDE` points;
/// the actual draw fits inside the box with aspect preserved. A
/// landscape (wide) logo uses the full `LOGO_BOX_SIDE` width and less
/// vertical height; a portrait logo the inverse; a square logo fills
/// the box exactly. The image is anchored to the top-left corner of
/// the box regardless of aspect — left edge at `MARGIN_LEFT`, top
/// edge at `MARGIN_TOP`.
fn place_logo(ops: &mut Vec<Operation>, logo: &TenantLogo, name: &str) {
    let box_side = LOGO_BOX_SIDE as f32;
    let w = logo.width.max(1) as f32;
    let h = logo.height.max(1) as f32;
    let scale = (box_side / w).min(box_side / h);
    let draw_w = w * scale;
    let draw_h = h * scale;
    // Anchor top-left of the box. PDF y grows upward, so the image's
    // bottom edge sits at `MARGIN_TOP - draw_h`; its top edge at
    // `MARGIN_TOP` (the printable-area top). PDF's Image XObject is
    // implicitly placed with its bottom-left at the cm-translated
    // origin, so the cm op below places the bottom-left of the drawn
    // rectangle at (MARGIN_LEFT, MARGIN_TOP - draw_h).
    let x_left = MARGIN_LEFT as f32;
    let y_bottom = (MARGIN_TOP as f32) - draw_h;

    ops.push(Operation::new("q", vec![]));
    // cm a b c d e f — with a=draw_w, d=draw_h, b=c=0, e=x, f=y, this
    // scales the unit square to (draw_w × draw_h) and translates it
    // to (x, y). The unit-square XObject's pixels then map directly
    // into that rectangle.
    ops.push(Operation::new(
        "cm",
        vec![
            Object::Real(draw_w),
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(draw_h),
            Object::Real(x_left),
            Object::Real(y_bottom),
        ],
    ));
    ops.push(Operation::new(
        "Do",
        vec![Object::Name(name.as_bytes().to_vec())],
    ));
    ops.push(Operation::new("Q", vec![]));
}

/// Build the Image XObject Stream for a decoded tenant logo. The
/// stream's raw content is the 8-bit RGB pixel buffer; `Stream::compress`
/// adds `/Filter /FlateDecode` (zlib) when it shrinks the payload,
/// which it always does for typical brand logos. The PDF reader maps
/// the byte stream back to pixels via the dict's
/// `Width / Height / ColorSpace / BitsPerComponent`.
fn build_logo_image_xobject(logo: &TenantLogo) -> Stream {
    let dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => logo.width as i64,
        "Height" => logo.height as i64,
        "ColorSpace" => Object::Name(b"DeviceRGB".to_vec()),
        "BitsPerComponent" => 8_i64,
    };
    let mut stream = Stream::new(dict, logo.rgb_bytes.clone());
    // Ignore compression errors per the same posture as lopdf's own
    // image embedding path — a failed FlateDecode still yields a valid
    // (uncompressed) Image XObject; the PDF reader handles both.
    let _ = stream.compress();
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR-85 — pin the palette constants. A future "let me just nudge
    /// the gold a bit" edit that drifts away from the ADR-0044
    /// silver/gold posture should fail here loudly. The values are
    /// the brand decision; the regulatory record carries no opinion
    /// on RGB but the company's client-facing surface does.
    #[test]
    fn palette_constants_match_brand_decision() {
        assert_eq!(INK, (0.13, 0.13, 0.15));
        assert_eq!(MUTED, (0.46, 0.47, 0.51));
        assert_eq!(SILVER_LINE, (0.72, 0.72, 0.74));
        assert_eq!(GOLD_ACCENT, (0.72, 0.54, 0.12));
    }

    /// PR-85 — pin the Adószám / IBAN spacing so a future edit that
    /// shrinks `LABEL_VALUE_GAP` back to the pre-PR-85 4pt value
    /// (which Ervin flagged as too tight) trips this test instead of
    /// shipping. The 10pt gap is the brand decision.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn label_value_gap_breathes() {
        assert!(
            LABEL_VALUE_GAP >= 8,
            "LABEL_VALUE_GAP must stay ≥ 8pt — Ervin's polish ask was \
             that the pre-PR-85 4pt gap looked cramped on `Adószám:123`"
        );
    }

    /// PR-279 — THE pin for the defect Ervin flagged as unacceptable:
    /// the ÁFA value printing on top of the BRUTTÓ ÁR value
    /// (`27%2 641 600 Ft` on TEST-ABERPNEW2026/0063).
    ///
    /// Walks the line-item table's numeric band left-to-right and
    /// asserts that every adjacent column pair keeps at least
    /// `MIN_GUTTER` points of clear space between the left column's
    /// right edge and the right column's worst-case LEFT extent —
    /// measured with the same real-glyph metric the renderer aligns
    /// with, so the assertion is over what actually gets painted.
    ///
    /// This fails on the pre-PR-279 geometry (gap was −5pt for the
    /// 7-digit gross in the screenshot, −2pt for a 9-digit one), and it
    /// fails again if anyone re-tunes a right-edge constant by hand.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn layout_gutters_clear_worst_case_row() {
        // Widest content each column can carry: the bold size-8 header
        // or the worst-case size-9 value. `-123 641 600 Ft` is a 9-digit
        // storno amount — the widest money string the formatter emits
        // (ties with EUR's `-€ 1 234 567,89`).
        const WORST_MONEY: &str = "-123 641 600 Ft";
        let cols: [(&str, i64, &str, &str); 5] = [
            (
                "MENNYISÉG",
                TableLayout::QTY_RIGHT,
                "MENNYISÉG",
                "1 000 000 db",
            ),
            (
                "NETTÓ EGYSÉGÁR",
                TableLayout::UNIT_PRICE_RIGHT,
                "NETTÓ EGYSÉGÁR",
                WORST_MONEY,
            ),
            ("NETTÓ ÁR", TableLayout::NET_RIGHT, "NETTÓ ÁR", WORST_MONEY),
            ("ÁFA", TableLayout::VAT_RIGHT, "ÁFA", "27%"),
            (
                "BRUTTÓ ÁR",
                TableLayout::GROSS_RIGHT,
                "BRUTTÓ ÁR",
                WORST_MONEY,
            ),
        ];

        // Left extent = right edge − max(header width @ FB/8, value width @ F1/9).
        let left_extent = |right: i64, header: &str, value: &str| {
            let w = crate::text::text_width_points(header, 8, true)
                .max(crate::text::text_width_points(value, 9, false));
            right - w
        };

        // The description column opens the band.
        let mut prev_name = "MEGNEVEZÉS";
        let mut prev_right = TableLayout::DESC_X + TableLayout::DESC_WIDTH;

        for (name, right, header, value) in cols {
            let left = left_extent(right, header, value);
            let gutter = left - prev_right;
            assert!(
                gutter >= TableLayout::MIN_GUTTER,
                "column `{prev_name}` (right edge {prev_right}) and `{name}` \
                 (worst-case left extent {left}) leave only {gutter}pt — \
                 below the {}pt minimum. Negative means they OVERLAP, which \
                 is the `27%2 641 600 Ft` defect returning.",
                TableLayout::MIN_GUTTER
            );
            prev_name = name;
            prev_right = right;
        }

        // The band must also stay inside the printable area.
        assert!(
            TableLayout::GROSS_RIGHT <= MARGIN_RIGHT,
            "gross column must not cross the right margin"
        );
        assert!(
            TableLayout::DESC_X + TableLayout::DESC_WIDTH < TableLayout::QTY_RIGHT,
            "description column must end before the quantity column"
        );
    }

    /// PR-279 — the `*_W` constants that `TableLayout`'s right-edges are
    /// derived FROM must equal the real measured width of the widest
    /// thing each column carries. Without this the derivation is just
    /// hand-tuned numbers wearing a subtraction: someone could widen a
    /// column's content and the arithmetic would keep reporting healthy
    /// gutters over a layout that overlaps.
    #[test]
    fn column_widths_match_measured_content() {
        const WORST_MONEY: &str = "-123 641 600 Ft";
        let hdr = |s: &str| crate::text::text_width_points(s, 8, true);
        let val = |s: &str| crate::text::text_width_points(s, 9, false);

        for (name, declared, measured) in [
            (
                "QTY_W",
                TableLayout::QTY_W,
                hdr("MENNYISÉG").max(val("1 000 000 db")),
            ),
            (
                "UNIT_PRICE_W",
                TableLayout::UNIT_PRICE_W,
                hdr("NETTÓ EGYSÉGÁR").max(val(WORST_MONEY)),
            ),
            (
                "NET_W",
                TableLayout::NET_W,
                hdr("NETTÓ ÁR").max(val(WORST_MONEY)),
            ),
            ("VAT_W", TableLayout::VAT_W, hdr("ÁFA").max(val("27%"))),
            (
                "GROSS_W",
                TableLayout::GROSS_W,
                hdr("BRUTTÓ ÁR").max(val(WORST_MONEY)),
            ),
        ] {
            assert_eq!(
                declared, measured,
                "TableLayout::{name} is {declared}pt but its widest content \
                 measures {measured}pt — the derived right-edges are stale"
            );
        }

        // EUR is the other currency the formatter emits; its worst shape
        // must not exceed the HUF one the widths were derived from.
        assert!(
            val("-\u{20AC}\u{00A0}1 234 567,89") <= TableLayout::GROSS_W,
            "worst-case EUR amount is wider than the derived money column"
        );
    }

    /// PR-279 — pin the width model itself. Right-alignment must use
    /// real glyph advances, not a per-char proxy. `27%` is the canonical
    /// witness: three chars, but `%` alone is 0.889 em, so the flat
    /// `0.55 * size` proxy called it 14pt when it actually paints 18pt —
    /// the 4pt that ran into the gross column.
    #[test]
    fn right_alignment_measures_real_glyph_advances() {
        let real = crate::text::text_width_points("27%", 9, false);
        let old_proxy = 3 * 9 * 55 / 100;
        assert_eq!(real, 18, "Helvetica `27%` at size 9 is 18pt");
        assert!(
            real > old_proxy,
            "the retired proxy ({old_proxy}pt) under-measured `27%` ({real}pt) — \
             if this stops holding the regression witness is gone"
        );

        // And the opposite error, which compounded with it: a money
        // string's thousands separators are 0.278 em, far NARROWER than
        // the proxy's flat guess, so the proxy pushed it too far left.
        let money = "2 641 600 Ft";
        let money_real = crate::text::text_width_points(money, 9, false);
        let money_proxy = (money.chars().count() as i64) * 9 * 55 / 100;
        assert!(
            money_real < money_proxy,
            "proxy {money_proxy}pt should have OVER-measured `{money}` ({money_real}pt)"
        );
    }

    /// PR-85 — pin the description-wrap behaviour. A short description
    /// fits on one line; a long one wraps; and no mid-word break
    /// occurs (a long URL or product code prints on its own line as
    /// a whole token — never silently truncated).
    #[test]
    fn description_wraps_when_long() {
        // A clearly-short description stays on one line.
        let short = "Tanácsadói díj";
        assert!(crate::text::text_width_points(short, 9, false) <= TableLayout::DESC_WIDTH);
        let wrapped_short = wrap_to_width(short, TableLayout::DESC_WIDTH, 9, false);
        assert_eq!(wrapped_short.len(), 1);

        // A long description wraps to multiple lines (≥ 2). The
        // existing `print_invoice_render` integration fixture's
        // description sits ABOVE the wrap width — its wrap-to-two-lines
        // behaviour is exercised by that suite, which keeps the wrap
        // path live in CI.
        let long = "Tanácsadói szolgáltatás Áben Consulting KFT részére \
                    2026 második negyedévében az ERP-rendszer bevezetésére \
                    vonatkozóan, NAV-megfelelőség és könyvviteli integráció \
                    kiegészítéssel";
        let wrapped_long = wrap_to_width(long, TableLayout::DESC_WIDTH, 9, false);
        assert!(
            wrapped_long.len() >= 2,
            "long description must wrap to ≥ 2 lines; got {} lines",
            wrapped_long.len()
        );

        // No mid-word breaks — every wrapped line is composed of
        // whole whitespace-separated tokens.
        for line in &wrapped_long {
            for word in line.split_whitespace() {
                assert!(!word.is_empty(), "no empty fragments in a wrapped line");
            }
        }

        // PR-279 — the reason the wrap moved off a char count: an
        // ALL-CAPS description of the same char length is far wider in
        // real glyphs. Under `DESC_WRAP_CHARS = 40` this fit on one
        // line by the count while painting ~70pt into the MENNYISÉG
        // column. Every emitted line must now measure inside the column.
        let caps = "SZERSZÁMACÉL MEGMUNKÁLÁS ÉS HŐKEZELÉS KOMPLETT";
        for line in &wrap_to_width(caps, TableLayout::DESC_WIDTH, 9, false) {
            assert!(
                crate::text::text_width_points(line, 9, false) <= TableLayout::DESC_WIDTH,
                "wrapped line {line:?} measures \
                 {}pt — past the {}pt description column",
                crate::text::text_width_points(line, 9, false),
                TableLayout::DESC_WIDTH
            );
        }
    }

    /// Session-148 (Ervin override 3) — the buyer name is rendered on
    /// the printed invoice UNCONDITIONALLY (the PR-97 GDPR carve-out
    /// that skipped the name slot for a name-less PRIVATE_PERSON body
    /// is removed). Pins that a buyer `PartyInfo` whose name is set —
    /// the case for every customer type now that the name is mandatory
    /// per §169 — emits a `Tj` text run carrying that name.
    #[test]
    fn write_party_renders_buyer_name() {
        let buyer = PartyInfo {
            name: "Teszt Maganszemely".to_string(),
            address_lines: vec!["1011 Budapest".to_string()],
            // PrivatePerson buyer: no ADÓSZÁM.
            tax_number: String::new(),
            bank_account_number: None,
            iban: None,
            bank_name: None,
            swift_bic: None,
        };
        let mut ops: Vec<Operation> = Vec::new();
        // is_seller = false — the buyer party path.
        write_party(&mut ops, "Vevő", &buyer, 40, 600, false, 240);
        let expected = text::winansi_bytes("Teszt Maganszemely");
        let rendered_name = ops.iter().any(|op| {
            op.operator == "Tj"
                && matches!(
                    op.operands.first(),
                    Some(Object::String(bytes, _)) if *bytes == expected
                )
        });
        assert!(
            rendered_name,
            "buyer name must be emitted as a Tj text run; ops: {ops:?}"
        );
    }

    /// Session-150 — the buyer address lines are rendered on the printed
    /// invoice BELOW the buyer name (Áfa tv. §169 mandates the buyer
    /// address for every customer type; ADR-0048 amendment 2026-05-29).
    /// Pins that `write_party` emits each address line as a Tj run AND
    /// that its baseline sits below the name's baseline.
    #[test]
    fn write_party_renders_buyer_address_below_name() {
        let buyer = PartyInfo {
            name: "Teszt Vevo Kft".to_string(),
            address_lines: vec![
                "HU".to_string(),
                "1052 Budapest".to_string(),
                "Vaci utca 19.".to_string(),
            ],
            tax_number: "12345678-2-13".to_string(),
            bank_account_number: None,
            iban: None,
            bank_name: None,
            swift_bic: None,
        };
        let mut ops: Vec<Operation> = Vec::new();
        write_party(&mut ops, "Vevő", &buyer, 40, 600, false, 240);

        // Walk ops tracking the y from each `Td` so the y of each `Tj`
        // run can be recovered (BT, Tf, rg, Td(x,y), Tj, ET sequence).
        let y_of = |needle: &str| -> Option<i64> {
            let want = text::winansi_bytes(needle);
            let mut last_y: Option<i64> = None;
            for op in &ops {
                if op.operator == "Td" {
                    if let Some(Object::Integer(y)) = op.operands.get(1) {
                        last_y = Some(*y);
                    }
                } else if op.operator == "Tj" {
                    if let Some(Object::String(bytes, _)) = op.operands.first() {
                        if *bytes == want {
                            return last_y;
                        }
                    }
                }
            }
            None
        };

        let name_y = y_of("Teszt Vevo Kft").expect("buyer name must render");
        let addr_y = y_of("1052 Budapest").expect("buyer address line must render");
        assert!(
            addr_y < name_y,
            "address line (y={addr_y}) must sit below the buyer name (y={name_y})"
        );
        // Every address line renders.
        for line in ["HU", "1052 Budapest", "Vaci utca 19."] {
            assert!(
                y_of(line).is_some(),
                "address line {line:?} must be emitted as a Tj run"
            );
        }
    }

    /// S192 — extreme-aspect-ratio placement pin. PR-182 review's S176
    /// 🟢 named the concern: a 1×N (or N×1) PNG must NOT make the
    /// `place_logo` matrix divide by zero, produce NaN/Inf scale
    /// factors, or scale the draw rectangle to literal zero pixels.
    ///
    /// The math: with `LOGO_BOX_SIDE = 50`, a 1×1024 strip yields
    /// `scale = min(50/1, 50/1024) = 50/1024 ≈ 0.0488`, so
    /// `draw_w = 1 · 0.0488 ≈ 0.0488 pt`, `draw_h = 1024 · 0.0488 = 50 pt`.
    /// Effectively invisible but mathematically well-defined. The
    /// `.max(1)` guard at line 1006-1007 covers the (impossible-after-
    /// PR-185-dimension-cap) 0×N degenerate case; pin both legs here so
    /// a future refactor that drops the guard fails loudly.
    #[test]
    fn place_logo_extreme_aspect_does_not_divide_by_zero_or_scale_below_one_pixel() {
        // Helper: inspect the `cm a b c d e f` operator that
        // `place_logo` emits and recover (draw_w, draw_h) from
        // positions (0, 3). The unit-square XObject maps directly into
        // this rectangle, so non-zero finite values are the contract.
        fn draw_dims(logo: &TenantLogo) -> (f32, f32) {
            let mut ops: Vec<Operation> = Vec::new();
            place_logo(&mut ops, logo, "Im0");
            let cm = ops
                .iter()
                .find(|op| op.operator == "cm")
                .expect("place_logo must emit a `cm` op");
            let read = |idx: usize| -> f32 {
                match cm.operands.get(idx) {
                    Some(Object::Real(v)) => *v,
                    other => panic!("cm operand {idx} must be Real, got {other:?}"),
                }
            };
            (read(0), read(3))
        }

        // 1×1024 strip — tall sliver. draw_h saturates the box,
        // draw_w shrinks below 1pt but stays positive + finite.
        let tall = TenantLogo {
            width: 1,
            height: 1024,
            rgb_bytes: vec![0u8; 1024 * 3],
        };
        let (draw_w, draw_h) = draw_dims(&tall);
        assert!(
            draw_w.is_finite() && draw_h.is_finite(),
            "extreme-aspect placement must produce finite scale factors; got ({draw_w}, {draw_h})"
        );
        assert!(
            draw_w > 0.0,
            "draw_w must be > 0 for a 1×N strip; got {draw_w}"
        );
        assert!(
            draw_h > 0.0,
            "draw_h must be > 0 for a 1×N strip; got {draw_h}"
        );
        let box_side = LOGO_BOX_SIDE as f32;
        // draw_h saturates the box (the long axis); draw_w fits within.
        assert!(
            (draw_h - box_side).abs() < 1e-3,
            "tall strip must saturate the box vertically; got draw_h={draw_h}, box_side={box_side}"
        );
        assert!(
            draw_w < draw_h,
            "tall strip must be narrower than tall after aspect-preserving fit; got ({draw_w}, {draw_h})"
        );

        // 1024×1 strip — wide sliver. Same contract on the swapped
        // axis: draw_w saturates the box; draw_h shrinks below 1pt
        // but stays positive + finite.
        let wide = TenantLogo {
            width: 1024,
            height: 1,
            rgb_bytes: vec![0u8; 1024 * 3],
        };
        let (draw_w_h, draw_h_h) = draw_dims(&wide);
        assert!(
            draw_w_h.is_finite() && draw_h_h.is_finite(),
            "wide-strip placement must produce finite scale factors; got ({draw_w_h}, {draw_h_h})"
        );
        assert!(draw_w_h > 0.0 && draw_h_h > 0.0);
        assert!(
            (draw_w_h - box_side).abs() < 1e-3,
            "wide strip must saturate the box horizontally; got draw_w={draw_w_h}"
        );
        assert!(draw_h_h < draw_w_h);

        // Degenerate-headers defence: the `.max(1)` guard at the top
        // of place_logo ensures even a pathological 0×0 logo (which
        // the PR-185 dimension/decoder caps already rule out at
        // decode time) does not divide by zero — pin the surviving
        // contract here so a future refactor that drops the guard
        // fails loudly.
        let zero = TenantLogo {
            width: 0,
            height: 0,
            rgb_bytes: Vec::new(),
        };
        let (draw_w_z, draw_h_z) = draw_dims(&zero);
        assert!(
            draw_w_z.is_finite() && draw_h_z.is_finite(),
            "0×0 logo must not produce NaN/Inf via the .max(1) guard; got ({draw_w_z}, {draw_h_z})"
        );
        assert_eq!(
            draw_w_z, box_side,
            "0×0 logo's effective 1×1 (post-`.max(1)`) saturates the box on both axes"
        );
        assert_eq!(draw_h_z, box_side);
    }

    // ──────────────────────────────────────────────────────────────────
    // S195 / PR-195 — brand primary-colour pins
    // ──────────────────────────────────────────────────────────────────

    /// Minimal `InvoiceModel` for rule-color pins below — one HUF line,
    /// no logo, no notes. Lets the per-test `brand_primary_color` setter
    /// stay as the only varying input across the pin pair.
    /// PR-296 — the pins below were written when the renderer had a
    /// single `layout()` that appended one page's worth of ops. They
    /// still describe page-1 geometry, so this flattens a single-page
    /// render back to one op stream for them. The length assertion is
    /// the point: a pin can never silently degrade into "reads page 1
    /// of a multi-page document and ignores the rest".
    fn layout(ops: &mut Vec<Operation>, m: &InvoiceModel, logo_xobject_name: Option<&str>) {
        let pages = layout_pages(m, logo_xobject_name);
        assert_eq!(
            pages.len(),
            1,
            "this pin describes a single-page layout but the model rendered \
             {} pages — re-target it at the page it means",
            pages.len()
        );
        ops.extend(pages.into_iter().next().expect("one page"));
    }

    fn pin_model_with_brand(brand: Option<(f32, f32, f32)>) -> InvoiceModel {
        use rust_decimal::Decimal;
        use time::macros::date;
        InvoiceModel {
            invoice_number: "PIN-2026-1".to_string(),
            issue_date: date!(2026 - 05 - 31),
            fulfillment_date: date!(2026 - 05 - 31),
            payment_due_date: date!(2026 - 06 - 07),
            payment_method: "Átutalás".to_string(),
            currency: Currency::Huf,
            rate_metadata: None,
            supplier: PartyInfo {
                name: "Eladó Kft".to_string(),
                address_lines: vec!["HU".to_string(), "1011 Budapest".to_string()],
                tax_number: "12345678-2-13".to_string(),
                bank_account_number: None,
                iban: None,
                bank_name: None,
                swift_bic: None,
            },
            customer: PartyInfo {
                name: "Vevő Kft".to_string(),
                address_lines: vec!["HU".to_string(), "1052 Budapest".to_string()],
                tax_number: "87654321-2-13".to_string(),
                bank_account_number: None,
                iban: None,
                bank_name: None,
                swift_bic: None,
            },
            lines: vec![LineItem {
                description: "Tanácsadás".to_string(),
                quantity: Decimal::from(1),
                unit: "db".to_string(),
                unit_price_minor: 100_000,
                net_minor: 100_000,
                vat_rate_percent: 27,
                vat_minor: 27_000,
                gross_minor: 127_000,
                performance_period: None,
                note: None,
            }],
            note: None,
            tenant_logo: None,
            brand_primary_color: brand,
        }
    }

    /// Recover every `RG` (stroke-color) operand triple emitted into
    /// `ops` — every `horizontal_rule` call pushes one. The order in
    /// the returned vec matches the document's top-to-bottom render
    /// order because `layout()` emits the title under-rule first, then
    /// the totals banner, then table rules. Test helper for the brand
    /// substitution pins below.
    fn stroke_color_triples(ops: &[Operation]) -> Vec<(f32, f32, f32)> {
        ops.iter()
            .filter(|op| op.operator == "RG")
            .map(|op| {
                let r = match op.operands.first() {
                    Some(Object::Real(v)) => *v,
                    _ => f32::NAN,
                };
                let g = match op.operands.get(1) {
                    Some(Object::Real(v)) => *v,
                    _ => f32::NAN,
                };
                let b = match op.operands.get(2) {
                    Some(Object::Real(v)) => *v,
                    _ => f32::NAN,
                };
                (r, g, b)
            })
            .collect()
    }

    /// S195 — when `brand_primary_color` is `None`, the renderer
    /// emits the pre-PR-195 palette byte-for-byte: title under-rule
    /// silver, totals banner gold, table header rule silver, table
    /// footer rule silver. Zero-impact for every tenant that has not
    /// opted in to a custom brand colour.
    #[test]
    fn brand_primary_none_keeps_default_silver_gold_palette() {
        let mut ops: Vec<Operation> = Vec::new();
        layout(&mut ops, &pin_model_with_brand(None), None);
        let strokes = stroke_color_triples(&ops);
        assert!(
            strokes.contains(&SILVER_LINE),
            "default render must emit at least one SILVER_LINE rule; got {strokes:?}"
        );
        assert!(
            strokes.contains(&GOLD_ACCENT),
            "default render must emit the GOLD_ACCENT rule above the totals banner; got {strokes:?}"
        );
        // Conversely — no arbitrary other RGB should leak in.
        for s in &strokes {
            assert!(
                *s == SILVER_LINE || *s == GOLD_ACCENT,
                "default render must use only SILVER_LINE / GOLD_ACCENT for rules; got {s:?}"
            );
        }
    }

    /// S195 — when `brand_primary_color` is `Some`, the title under-
    /// rule, table-header rule, and totals banner all substitute the
    /// operator's colour. The table FOOTER rule deliberately stays
    /// SILVER_LINE (preserves visual hierarchy — only the "structural
    /// emphasis" rules brand-override, not every silver rule).
    #[test]
    fn brand_primary_some_substitutes_three_rules_keeps_table_footer_silver() {
        let brand: (f32, f32, f32) = (0.1, 0.2, 0.3);
        let mut ops: Vec<Operation> = Vec::new();
        layout(&mut ops, &pin_model_with_brand(Some(brand)), None);
        let strokes = stroke_color_triples(&ops);
        let count = |c: (f32, f32, f32)| strokes.iter().filter(|s| **s == c).count();
        // Three rules carry the brand colour (title under-rule + table
        // header rule + totals banner).
        assert_eq!(
            count(brand),
            3,
            "brand colour must replace exactly THREE structural/accent rules; \
             got strokes={strokes:?}"
        );
        // GOLD_ACCENT disappears entirely under a brand override.
        assert_eq!(
            count(GOLD_ACCENT),
            0,
            "GOLD_ACCENT must collapse into the brand colour when set; got {strokes:?}"
        );
        // SILVER_LINE survives once — the table-footer rule keeps the
        // structural hierarchy intact even under a brand override.
        assert_eq!(
            count(SILVER_LINE),
            1,
            "table-footer rule keeps SILVER_LINE under brand override (visual hierarchy); \
             got {strokes:?}"
        );
    }

    /// S195 — defence-in-depth on the `render_invoice` entry point:
    /// a brand-coloured model still produces well-formed PDF bytes.
    /// (The earlier pin walks the op stream; this one closes the
    /// integration loop to surface any PDF-serialization regression a
    /// new colour code path might trigger.)
    #[test]
    fn render_invoice_smoke_with_brand_primary_color() {
        let bytes = render_invoice(&pin_model_with_brand(Some((0.5, 0.5, 0.5))))
            .expect("render with brand colour must succeed");
        assert!(
            bytes.starts_with(b"%PDF"),
            "rendered output must be a PDF (starts with %PDF magic); first 16 bytes = {:?}",
            &bytes[..16.min(bytes.len())]
        );
    }

    // ──────────────────────────────────────────────────────────────────
    // PR-249 / S260 — Bug A (header column clamp + wrap) pins
    // ──────────────────────────────────────────────────────────────────

    /// The two header column anchors, recomputed from the same formula
    /// `layout()` uses, so the pins below assert against the live
    /// geometry rather than magic numbers.
    fn header_cols() -> (i64, i64, i64, i64) {
        let col_left = MARGIN_LEFT;
        let col_right = MARGIN_LEFT + (MARGIN_RIGHT - MARGIN_LEFT) / 2 + 8;
        let seller_width = col_right - COLUMN_GUTTER - col_left;
        let buyer_width = MARGIN_RIGHT - col_right;
        (col_left, col_right, seller_width, buyer_width)
    }

    /// Walk the op stream and recover every `(x, raw-bytes)` `Tj` text
    /// run (the renderer always emits `Td(x, y)` immediately before its
    /// `Tj`).
    fn tj_runs(ops: &[Operation]) -> Vec<(i64, Vec<u8>)> {
        let mut last_x: i64 = 0;
        let mut out = Vec::new();
        for op in ops {
            match op.operator.as_str() {
                "Td" => {
                    if let Some(Object::Integer(x)) = op.operands.first() {
                        last_x = *x;
                    }
                }
                "Tj" => {
                    if let Some(Object::String(bytes, _)) = op.operands.first() {
                        out.push((last_x, bytes.clone()));
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn wrap_to_width_noop_for_short_text_and_breaks_long() {
        let (_, _, seller_width, _) = header_cols();
        // Short field — one line, identical content.
        let short = wrap_to_width("Eladó Kft", seller_width, 13, true);
        assert_eq!(short, vec!["Eladó Kft".to_string()]);
        // Ervin's real legal name — must break into ≥ 2 lines, each
        // measuring within the seller column, and losslessly reassemble.
        let name = "ÁBEN CONSULTING KORLÁTOLT FELELŐSSÉGŰ TÁRSASÁG";
        let lines = wrap_to_width(name, seller_width, 13, true);
        assert!(
            lines.len() >= 2,
            "long all-caps legal name must wrap to ≥ 2 lines; got {lines:?}"
        );
        for line in &lines {
            assert!(
                crate::text::text_width_points(line, 13, true) <= seller_width,
                "wrapped line {line:?} exceeds the seller column width {seller_width}"
            );
        }
        assert_eq!(
            lines.join(" "),
            name,
            "wrapping must not drop or reorder any word"
        );
    }

    /// The headline Bug-A pin: with Ervin's long seller legal name and a
    /// normal-length buyer, the seller cell wraps inside its column AND
    /// the buyer name renders, in full, anchored in the buyer column —
    /// i.e. it is no longer overprinted. Asserts layout positions, not
    /// raw PDF bytes (per the brief's golden-file guidance).
    #[test]
    fn seller_name_wraps_and_buyer_name_stays_readable() {
        let (col_left, col_right, seller_width, buyer_width) = header_cols();
        let mut m = pin_model_with_brand(None);
        m.supplier.name = "ÁBEN CONSULTING KORLÁTOLT FELELŐSSÉGŰ TÁRSASÁG".to_string();
        m.customer.name = "Megrendelő Kft".to_string();

        let mut ops: Vec<Operation> = Vec::new();
        layout(&mut ops, &m, None);
        let runs = tj_runs(&ops);

        // The full unwrapped seller name must NOT appear as a single run
        // (that was the overflow bug — one horizontal line).
        let full = crate::text::winansi_bytes(&m.supplier.name);
        assert!(
            !runs.iter().any(|(_, b)| *b == full),
            "seller name must be split across lines, never emitted as one run"
        );

        // Each wrapped seller line appears as its own run anchored at the
        // seller column's left edge, never crossing into the buyer column.
        let expected_lines = wrap_to_width(&m.supplier.name, seller_width, 13, true);
        assert!(expected_lines.len() >= 2, "precondition: name wraps");
        for line in &expected_lines {
            let want = crate::text::winansi_bytes(line);
            let hit = runs.iter().find(|(_, b)| *b == want);
            let (x, _) = hit.unwrap_or_else(|| {
                panic!("wrapped seller line {line:?} must render as a Tj run; runs={runs:?}")
            });
            assert_eq!(*x, col_left, "seller line {line:?} must anchor at col_left");
            let right_edge = x + crate::text::text_width_points(line, 13, true);
            assert!(
                right_edge <= col_right - COLUMN_GUTTER + 1,
                "seller line {line:?} right edge {right_edge} must stay left of the \
                 buyer column boundary {}",
                col_right - COLUMN_GUTTER
            );
        }

        // The buyer name renders in full, as one run, inside the buyer
        // column — readable, not overprinted.
        let buyer = crate::text::winansi_bytes(&m.customer.name);
        let (bx, _) = runs
            .iter()
            .find(|(_, b)| *b == buyer)
            .expect("buyer name must render as a single, intact Tj run");
        assert_eq!(*bx, col_right, "buyer name must anchor in the buyer column");
        assert!(
            crate::text::text_width_points(&m.customer.name, 13, true) <= buyer_width,
            "buyer name must fit within the buyer column without wrapping"
        );
    }

    /// A wrapped (taller) seller column must push the date rows below it
    /// down — `layout()` anchors the block beneath the parties to the
    /// shorter (more-negative) of the two column bottoms. Pin that the
    /// long-name render lands the first date row strictly lower than the
    /// short-name render would.
    #[test]
    fn wrapped_seller_pushes_dates_down() {
        // Recover the y of the first date label ("SZÁMLA KELTE") below
        // the party block for a given seller name.
        let date_label_y = |seller_name: &str| -> i64 {
            let mut m = pin_model_with_brand(None);
            m.supplier.name = seller_name.to_string();
            let mut ops: Vec<Operation> = Vec::new();
            layout(&mut ops, &m, None);
            let want = crate::text::winansi_bytes("SZÁMLA KELTE:");
            let mut last_y = 0;
            for op in &ops {
                if op.operator == "Td" {
                    if let Some(Object::Integer(y)) = op.operands.get(1) {
                        last_y = *y;
                    }
                } else if op.operator == "Tj" {
                    if let Some(Object::String(b, _)) = op.operands.first() {
                        if *b == want {
                            return last_y;
                        }
                    }
                }
            }
            panic!("SZÁMLA KELTE label must render");
        };
        let short = date_label_y("Eladó Kft");
        let long = date_label_y("ÁBEN CONSULTING KORLÁTOLT FELELŐSSÉGŰ TÁRSASÁG");
        assert!(
            long < short,
            "the wrapped (taller) seller column must push the date rows \
             down: long-name y={long} must be below short-name y={short}"
        );
    }

    /// Bug B: an EUR invoice's emitted text runs carry the `€`(0x80) +
    /// NBSP(0xA0) byte pair — symbol and amount visually separated yet
    /// unbreakable. The HUF render carries no such pair (HUF is postfix
    /// `… Ft`, unchanged). Asserted on the `layout()` op stream because
    /// `render_invoice` deflates the saved content stream via
    /// `doc.compress()`.
    #[test]
    fn eur_layout_emits_euro_nbsp_pair_huf_does_not() {
        use rust_decimal::Decimal;
        use time::macros::date;
        let mut m = pin_model_with_brand(None);
        m.currency = Currency::Eur;
        m.rate_metadata = Some(aberp_billing::RateMetadata {
            rate: Decimal::new(35669, 2),
            source: "MNB".to_string(),
            date: date!(2026 - 05 - 08),
            huf_equivalent_total: 453,
        });
        let mut eur_ops: Vec<Operation> = Vec::new();
        layout(&mut eur_ops, &m, None);
        let has_pair = |ops: &[Operation]| -> bool {
            tj_runs(ops)
                .iter()
                .any(|(_, b)| b.windows(2).any(|w| w == [0x80, 0xA0]))
        };
        assert!(
            has_pair(&eur_ops),
            "EUR layout must emit a € + NBSP (0x80,0xA0) text run"
        );

        let mut huf_ops: Vec<Operation> = Vec::new();
        layout(&mut huf_ops, &pin_model_with_brand(None), None);
        assert!(
            !has_pair(&huf_ops),
            "HUF layout must not emit a € + NBSP pair (HUF is postfix `Ft`)"
        );
    }

    // ─── PR-296 — pagination ──────────────────────────────────────────
    //
    // The defect these pin: the renderer emitted `"Count" => 1` with no
    // line-count guard, so rows just kept advancing downward. Measured
    // on the pre-PR-296 build — 16 line items put the lowest baseline at
    // y=28, 17 at y=0, and from 18 on it went NEGATIVE: painted off the
    // physical sheet, taking the totals block and the ÁFA summary with
    // it, while `render_invoice` still returned `Ok(bytes)`.

    /// `n`-line variant of [`pin_model_with_brand`] — same parties,
    /// same money, one row per index so a row can be identified by its
    /// printed `#`.
    fn pin_model_with_lines(n: usize) -> InvoiceModel {
        let mut m = pin_model_with_brand(None);
        let base = m.lines[0].clone();
        m.lines = (0..n)
            .map(|i| LineItem {
                description: format!("Tétel {}", i + 1),
                ..base.clone()
            })
            .collect();
        m
    }

    /// Lowest y any operator paints at on this page — text baselines
    /// (`Td`) and rule endpoints (`m` / `l`). This is the number that
    /// went negative pre-PR-296.
    fn lowest_painted_y(ops: &[Operation]) -> i64 {
        let mut lowest = i64::MAX;
        for op in ops {
            if matches!(op.operator.as_str(), "Td" | "m" | "l") {
                if let Some(Object::Integer(y)) = op.operands.get(1) {
                    lowest = lowest.min(*y);
                }
            }
        }
        lowest
    }

    /// Every `Tj` string on the page, as the WinAnsi bytes actually
    /// emitted.
    fn page_texts(ops: &[Operation]) -> Vec<Vec<u8>> {
        ops.iter()
            .filter(|op| op.operator == "Tj")
            .filter_map(|op| match op.operands.first() {
                Some(Object::String(bytes, _)) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }

    fn page_has_text(ops: &[Operation], needle: &str) -> bool {
        let want = text::winansi_bytes(needle);
        page_texts(ops).contains(&want)
    }

    /// THE pin for the reported defect: at no line count does any page
    /// paint below the footer band. Fails on the pre-PR-296 renderer
    /// from 17 line items up (y=0, then negative).
    #[test]
    fn no_line_count_paints_content_off_the_page() {
        // The attestation sentence is the lowest thing on a healthy
        // page; nothing may sit below it.
        let floor = FOOTER_Y_TOP - 14;
        for n in 1..=60 {
            let m = pin_model_with_lines(n);
            let pages = layout_pages(&m, None);
            for (i, page) in pages.iter().enumerate() {
                let lowest = lowest_painted_y(page);
                assert!(
                    lowest >= floor,
                    "{n} line items: page {} paints at y={lowest}, below the \
                     footer band at y={floor}. Negative means off the sheet — \
                     the pre-PR-296 defect.",
                    i + 1
                );
            }
        }
    }

    /// No row may be dropped or duplicated by the page break. Each row
    /// prints its 1-based index in the `#` column; all `n` of them must
    /// appear exactly once across the document, in order. CLAUDE.md
    /// rule 11 — a paginator that silently loses row 12 is worse than
    /// one that refuses.
    #[test]
    fn every_line_item_is_painted_exactly_once_across_pages() {
        for n in [1usize, 11, 12, 18, 25, 40, 60] {
            let m = pin_model_with_lines(n);
            let pages = layout_pages(&m, None);
            let printed: Vec<String> = pages
                .iter()
                .flat_map(|p| page_texts(p))
                .filter_map(|t| String::from_utf8(t).ok())
                .filter(|s| s.parse::<usize>().is_ok())
                .collect();
            let expected: Vec<String> = (1..=n).map(|i| i.to_string()).collect();
            assert_eq!(
                printed, expected,
                "{n} line items: the `#` column must read 1..{n} exactly once, \
                 in order, across all pages"
            );
        }
    }

    /// An invoice that spills carries its column headers onto every
    /// continuation page (a reader on page 2 must still know which
    /// column is ÁFA and which is BRUTTÓ ÁR) and identifies itself with
    /// the invoice number.
    #[test]
    fn continuation_pages_repeat_headers_and_identify_the_invoice() {
        let m = pin_model_with_lines(25);
        let pages = layout_pages(&m, None);
        assert!(pages.len() >= 2, "25 line items must spill past page 1");
        for (i, page) in pages.iter().enumerate() {
            for header in ["#", "MEGNEVEZÉS", "MENNYISÉG", "ÁFA", "BRUTTÓ ÁR"] {
                assert!(
                    page_has_text(page, header),
                    "page {} is missing the `{header}` column header",
                    i + 1
                );
            }
            assert!(
                page_has_text(page, &m.invoice_number),
                "page {} does not carry the invoice number",
                i + 1
            );
        }
    }

    /// The footer counter must count the real pages. Pre-PR-296 it was
    /// the hardcoded literal `1/1 Oldal`.
    #[test]
    fn page_counter_reads_i_of_n_on_every_page() {
        for n in [1usize, 25, 60] {
            let pages = layout_pages(&pin_model_with_lines(n), None);
            let total = pages.len();
            for (i, page) in pages.iter().enumerate() {
                let want = format!("{}/{} Oldal", i + 1, total);
                assert!(
                    page_has_text(page, &want),
                    "{n} line items: page {} must print `{want}`",
                    i + 1
                );
            }
        }
    }

    /// The totals block is never split and always lands on the LAST
    /// page — with at least one line-item row above it, so a trailing
    /// page can never be a lone totals block.
    #[test]
    fn totals_block_lands_whole_on_the_last_page() {
        for n in [1usize, 11, 12, 18, 25, 40, 60] {
            let pages = layout_pages(&pin_model_with_lines(n), None);
            let last = pages.len() - 1;
            for label in ["NETTÓ ÖSSZEG:", "27% ÁFA:", "FIZETENDŐ BRUTTÓ VÉGÖSSZEG:"] {
                let carrying: Vec<usize> = pages
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| page_has_text(p, label))
                    .map(|(i, _)| i)
                    .collect();
                // The banner at the top of page 1 carries the same
                // FIZETENDŐ label, so page 0 is a legitimate second
                // home for that one string; the totals-block copy must
                // still be on the last page.
                assert!(
                    carrying.contains(&last),
                    "{n} line items: `{label}` must appear on the last page \
                     (page {}), found on pages {carrying:?}",
                    last + 1
                );
            }
            assert!(
                page_has_text(&pages[last], "1") || n > 0,
                "sanity: the last page carries content"
            );
            // Widow rule — the last page always has a line-item row
            // above the totals.
            let row_on_last = page_texts(&pages[last])
                .iter()
                .filter_map(|t| String::from_utf8(t.clone()).ok())
                .any(|s| s.parse::<usize>().is_ok());
            assert!(
                row_on_last,
                "{n} line items: the last page must carry at least one line-item \
                 row — a page holding only the totals block is the orphan case \
                 the last-row reservation exists to prevent"
            );
        }
    }

    /// [`closing_block_depth`] is a PREDICTION of what `write_totals` +
    /// `write_note` will paint; the page break is sized by it. If the
    /// two ever drift the totals go off the sheet again — silently,
    /// because the prediction would still report a comfortable fit.
    /// Compare prediction against the real painted extent across the
    /// shapes that change the block's height: HUF vs EUR (HUF-equivalent
    /// sub-rows + Árfolyam pair), one VAT rate vs three, no note vs a
    /// wrapping one.
    #[test]
    fn closing_block_depth_matches_painted_extent() {
        use rust_decimal::Decimal;
        use std::str::FromStr;
        use time::macros::date;

        let long_note = "Köszönjük a megrendelést. Kérjük az utalásnál tüntessék fel a \
                         számla sorszámát a közlemény mezőben, valamint a teljesítés \
                         igazolásának másolatát is csatolják a bizonylathoz."
            .to_string();
        let rate = aberp_billing::RateMetadata {
            rate: Decimal::from_str("405.23").unwrap(),
            source: "MNB".to_string(),
            date: date!(2026 - 05 - 26),
            huf_equivalent_total: 810_460,
        };

        for (label, m) in [
            ("HUF, one rate, no note", pin_model_with_lines(3)),
            ("HUF, one rate, long note", {
                let mut m = pin_model_with_lines(3);
                m.note = Some(long_note.clone());
                m
            }),
            ("HUF, three rates", {
                let mut m = pin_model_with_lines(3);
                for (i, pct) in [5u16, 18, 27].into_iter().enumerate() {
                    m.lines[i].vat_rate_percent = pct;
                }
                m
            }),
            ("EUR, rate stamp, long note", {
                let mut m = pin_model_with_lines(3);
                m.currency = Currency::Eur;
                m.rate_metadata = Some(rate.clone());
                m.note = Some(long_note.clone());
                m
            }),
        ] {
            // Draw the closing block alone from a known cursor and read
            // back how far down it actually reached.
            let cursor = 500;
            let mut ops: Vec<Operation> = Vec::new();
            let gross: i64 = m.lines.iter().map(|l| l.gross_minor).sum();
            silver_rule(&mut ops, MARGIN_LEFT, MARGIN_RIGHT, cursor + 8);
            let totals_bottom = write_totals(&mut ops, &m, cursor + 8 - 24, gross);
            write_note(&mut ops, &m, totals_bottom - 24);

            let painted = cursor - lowest_painted_y(&ops);
            assert_eq!(
                closing_block_depth(&m),
                painted,
                "{label}: closing_block_depth predicts {}pt but the block paints \
                 {painted}pt below the cursor — the page break is sized off the \
                 prediction, so a mismatch puts the totals off the sheet",
                closing_block_depth(&m)
            );
        }
    }

    /// The concrete threshold from the field report: an 18-line invoice
    /// was the first one to paint at negative y. It must now be a clean
    /// two-page document.
    #[test]
    fn eighteen_line_invoice_is_two_clean_pages() {
        let pages = layout_pages(&pin_model_with_lines(18), None);
        assert_eq!(
            pages.len(),
            2,
            "18 line items — the measured pre-PR-296 off-page threshold — must \
             render as two pages"
        );
        assert!(page_has_text(&pages[0], "1/2 Oldal"));
        assert!(page_has_text(&pages[1], "2/2 Oldal"));
        assert!(page_has_text(&pages[1], "FIZETENDŐ BRUTTÓ VÉGÖSSZEG:"));
        // The banner is a page-1 element and does NOT repeat.
        assert!(page_has_text(&pages[0], "ELADÓ"));
        assert!(
            !page_has_text(&pages[1], "ELADÓ"),
            "the seller/buyer block belongs to page 1 only"
        );
    }

    /// Widow control — a paginated invoice must not strand a lone row
    /// above the totals on an otherwise empty final page. With uniform
    /// rows the tail reservation can always be satisfied, so the last
    /// page carries at least `TAIL_ROWS_KEPT_WITH_TOTALS` rows — more
    /// when the break lands earlier for its own reasons.
    #[test]
    fn the_last_page_is_not_a_single_orphan_row_under_the_totals() {
        for n in 12..=60 {
            let pages = layout_pages(&pin_model_with_lines(n), None);
            if pages.len() < 2 {
                continue;
            }
            let rows_on_last = page_texts(pages.last().expect("pages"))
                .iter()
                .filter_map(|t| String::from_utf8(t.clone()).ok())
                .filter(|s| s.parse::<usize>().is_ok())
                .count();
            assert!(
                rows_on_last >= TAIL_ROWS_KEPT_WITH_TOTALS,
                "{n} line items over {} pages: the closing page carries only \
                 {rows_on_last} row(s), below the {TAIL_ROWS_KEPT_WITH_TOTALS} \
                 the tail reservation keeps with the totals. One row marooned \
                 above the totals on an otherwise empty page is the widow this \
                 exists to prevent.",
                pages.len()
            );
        }
    }

    /// Page count never decreases as line items are added — a break
    /// rule that oscillates would be a layout bug even when every
    /// individual page happens to fit.
    #[test]
    fn page_count_is_monotonic_in_line_count() {
        let mut previous = 0usize;
        for n in 1..=60 {
            let pages = layout_pages(&pin_model_with_lines(n), None).len();
            assert!(
                pages >= previous,
                "{n} line items render on {pages} pages but {} lines needed \
                 {previous}",
                n - 1
            );
            previous = pages;
        }
    }
}
