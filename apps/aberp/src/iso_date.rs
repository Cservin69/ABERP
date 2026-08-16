//! The ONE canonical `YYYY-MM-DD` definition.
//!
//! # Why this module exists
//!
//! Three places decide whether a date string is a date, and they must
//! decide it the same way:
//!
//!   * [`incoming_invoices`](crate::incoming_invoices) validates
//!     `issue_date` / `delivery_date` / `payment_deadline` on the AP
//!     ingest write path;
//!   * [`mark_invoice_paid`](crate::mark_invoice_paid) validates
//!     `paid_at`, which is the DSO anchor;
//!   * `reports::parse_iso_date` reads those columns back and decides,
//!     per invoice, whether it is OUTSTANDING at all — an unreadable
//!     payment deadline means "settled legacy import", i.e. dropped from
//!     Receivables / Payables entirely.
//!
//! The last one is why a validator gap is a money bug rather than a
//! cosmetic one: a writer that accepts a shape the reader later rejects
//! books a live invoice as settled.
//!
//! # The gap this closes
//!
//! All three used `time::Date::parse(s, "[year]-[month]-[day]")` on its
//! own. `time`'s `[year]` component has `sign_is_mandatory: false`,
//! which means the sign is OPTIONAL — not absent. So:
//!
//! ```text
//!     Date::parse("+2026-06-15", "[year]-[month]-[day]")  =>  Ok(2026-06-15)
//!     Date::parse("-2026-06-15", "[year]-[month]-[day]")  =>  Ok(-2026-06-15)
//! ```
//!
//! Both were accepted at the writer and stored verbatim. The reader then
//! sees them through `SUBSTR(CAST(… AS VARCHAR), 1, 10)` — eleven
//! characters truncated to ten — so `+2026-06-15` reaches
//! `parse_iso_date` as `+2026-06-1`, which fails. Under the
//! settled-undated rule that failure does not mean "drop from the aging
//! buckets"; it means "treat as settled and remove from the outstanding
//! total". A live, unpaid payable disappears from the book, and the only
//! trace is the `aging_settled_undated` diagnostic counter.
//!
//! # The definition
//!
//! [`parse_canonical_iso_date`] accepts EXACTLY ten bytes:
//! `DDDD-DD-DD`, four ASCII digits of year, two of month, two of day, no
//! sign, no padding, no trailing anything — then hands the string to
//! `time::Date::parse` for the calendar-range check (`2026-02-30` and
//! `2026-13-01` are shape-legal and calendar-illegal, and only a real
//! date library knows which February has 29 days).
//!
//! Whitespace tolerance is NOT part of the canonical form. The two write
//! paths call this directly, so they stay as strict as they were. Only
//! the report reader trims first, and it trims [ASCII whitespace
//! only](trim_ascii_whitespace) — see that function for why the
//! distinction is load-bearing.

use time::macros::format_description;
use time::Date;

/// Parse a strictly canonical ISO `YYYY-MM-DD` date, or `None`.
///
/// Rejects, in this order: any length but 10; dashes not at bytes 4 and
/// 7; a non-ASCII-digit anywhere else (which is what excludes the
/// `+2026-06-15` / `-2026-06-15` signed forms `time` would otherwise
/// accept); and finally a date that does not exist in the calendar.
pub fn parse_canonical_iso_date(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !(b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit))
    {
        return None;
    }
    // Shape is canonical; `time` answers the calendar question (leap
    // years, month lengths, month/day range).
    let fmt = format_description!("[year]-[month]-[day]");
    Date::parse(s, fmt).ok()
}

/// Strict `YYYY-MM-DD` validator for the write paths. Silent acceptance
/// of a non-canonical date string would lock the wrong shape into the
/// audit ledger forever, and — since the financial report reads these
/// columns back through a stricter parser — would book a live invoice as
/// settled.
pub fn is_canonical_iso_date(s: &str) -> bool {
    parse_canonical_iso_date(s).is_some()
}

/// Trim ASCII whitespace — and ONLY ASCII whitespace — from both ends.
///
/// # Why not `str::trim`
///
/// The SPA classifies deadlines too (`aging.ts::parseRecordedDeadline`),
/// and the two classifiers must agree row for row: a shape they disagree
/// about is an invoice the dashboard tile counts and the drill-down list
/// does not, or an invoice that leaves Receivables on one side only.
///
/// `str::trim` trims Unicode `White_Space`; JavaScript's
/// `String.prototype.trim` trims `WhiteSpace` ∪ `LineTerminator`. Those
/// two sets are not the same set, and the differences were live
/// divergences:
///
/// ```text
///     U+FEFF  ZWNBSP   Rust keeps it (not White_Space) → undated
///                      JS strips it                    → DATED
///     U+0085  NEL      Rust strips it (White_Space)    → DATED
///                      JS keeps it (neither category)  → undated
///     U+000B  VT       Rust's is_ascii_whitespace says no
///                      JS's trim says yes
///     U+00A0  NBSP     both strip it — agreeing, but by luck, on a
///                      shape neither writer can produce
/// ```
///
/// ASCII whitespace is the intersection both languages can state
/// exactly, so both sides now trim `\t \n \x0C \r` and space, and
/// nothing else. Every exotic-whitespace form is undated on BOTH sides —
/// which is also the only verdict a value no writer can produce deserves.
pub fn trim_ascii_whitespace(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical happy path, including the two shapes a naive
    /// four-digit-year assumption gets wrong (year < 1000 is a real
    /// date; the SPA's `Date.UTC` remaps 0..99 to 1900+y and needs an
    /// explicit correction to agree — pinned in `aging.test.ts`).
    #[test]
    fn accepts_canonical_dates_including_low_years() {
        for s in [
            "2026-05-26",
            "2026-01-01",
            "2026-12-31",
            "2024-02-29", // leap day
            "0001-01-01",
            "0026-06-15",
            "9999-12-31",
        ] {
            assert!(is_canonical_iso_date(s), "{s:?} is a canonical ISO date");
        }
    }

    /// THE BUG. `time::Date::parse` with `[year]` accepts an optional
    /// sign, so both of these parsed clean at the writer and were stored
    /// verbatim — eleven characters, which the reader's
    /// `SUBSTR(…, 1, 10)` truncates into an unparseable stump, which the
    /// settled-undated rule reads as "paid, drop from the total".
    ///
    /// Mutation check (verified red): drop the length/digit gate and
    /// delegate straight to `Date::parse` — `+2026-06-15` and
    /// `-2026-06-15` both come back accepted.
    #[test]
    fn rejects_a_signed_year() {
        assert!(!is_canonical_iso_date("+2026-06-15"));
        assert!(!is_canonical_iso_date("-2026-06-15"));
        assert!(!is_canonical_iso_date("+002026-06-15"));
        assert!(!is_canonical_iso_date("-0001-01-01"));
    }

    /// Out-of-range and off-shape forms. The month/day range and the
    /// calendar itself (Feb 30, Feb 29 in a common year) are `time`'s
    /// half of the job; the shape gate is this module's half.
    #[test]
    fn rejects_out_of_range_and_malformed() {
        for s in [
            "",
            "2026-13-01",          // month out of range
            "2026-00-01",          // month out of range
            "2026-01-00",          // day out of range
            "2026-02-30",          // impossible calendar day
            "2025-02-29",          // 2025 is not a leap year
            "2026-1-05",           // unpadded month
            "2026-01-5",           // unpadded day
            "2026/05/26",          // wrong separators
            "26-05-2026",          // swapped
            "2026-05-26T00:00:00", // timestamp form
            "2026-05-267",         // too long
            "2026-05-2",           // too short
            "twenty-two-26",       // right length, not digits
            "２026-06-15",         // fullwidth digit
        ] {
            assert!(!is_canonical_iso_date(s), "{s:?} must be rejected");
        }
    }

    /// Whitespace is NOT canonical — the write paths call
    /// [`is_canonical_iso_date`] directly and must stay strict. Only the
    /// report reader trims, and only ASCII.
    #[test]
    fn whitespace_is_not_canonical_but_ascii_trim_removes_it() {
        assert!(!is_canonical_iso_date(" 2026-06-30 "));
        assert!(is_canonical_iso_date(trim_ascii_whitespace(" 2026-06-30 ")));
        assert!(is_canonical_iso_date(trim_ascii_whitespace(
            "\t2026-06-30\n"
        )));
        assert!(is_canonical_iso_date(trim_ascii_whitespace(
            "\r\x0C2026-06-30 "
        )));
    }

    /// The exotic-whitespace vocabulary, from the Rust side. Each of
    /// these is a form `str::trim` and `String.prototype.trim` disagree
    /// about; ASCII-only trimming makes both sides say "not a date".
    /// `aging.test.ts` pins the same five inputs to the same verdict.
    ///
    /// Mutation check (verified red): put `str::trim` back and the NEL
    /// and NBSP rows flip to accepted, re-opening the split with the SPA.
    #[test]
    fn exotic_whitespace_is_not_trimmed_so_both_sides_call_it_undated() {
        for pad in ["\u{feff}", "\u{85}", "\u{a0}", "\u{b}", "\u{2003}"] {
            let prefixed = format!("{pad}2026-06-30");
            let suffixed = format!("2026-06-30{pad}");
            assert!(
                !is_canonical_iso_date(trim_ascii_whitespace(&prefixed)),
                "{prefixed:?} must stay undated"
            );
            assert!(
                !is_canonical_iso_date(trim_ascii_whitespace(&suffixed)),
                "{suffixed:?} must stay undated"
            );
        }
    }
}
