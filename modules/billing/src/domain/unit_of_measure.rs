//! Unit-of-measure domain types — `NavUnitOfMeasure` + `ProductUnit`.
//!
//! # Why these live in billing (S159)
//!
//! PR-91 first introduced these types in `apps/aberp/src/products.rs` for
//! the products master-data. S159 threads a line's unit-of-measure all the
//! way to the NAV `<unitOfMeasure>` emit, which reads off
//! [`crate::domain::invoice::LineItem`]. `LineItem` is a billing domain
//! type, so the unit type must be reachable from billing without a
//! backwards `billing → app` dependency. The types moved DOWN here; the
//! products module re-exports them (`pub use aberp_billing::{…}`) so its
//! existing API is unchanged.
//!
//! # The unit-of-measure model — load-bearing
//!
//! NAV's v3.0 InvoiceData schema requires every `<line>` to carry a
//! `<unitOfMeasure>` element whose body is one of a closed enum of tokens,
//! OR the literal `OWN` paired with a `<unitOfMeasureOwn>` free-text
//! element. The product's unit maps to that wire shape cleanly so the
//! "pick product → autofill line → NAV emit" path (PR-100 + S159) hands the
//! operator's catalog entry straight to the emitter.

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────
// NavUnitOfMeasure — closed-vocab mirror of the NAV v3.0
// unitOfMeasureType enum (sans OWN — see ProductUnit).
// ──────────────────────────────────────────────────────────────────────

/// NAV v3.0 `unitOfMeasureType` enum mirror. Each variant serialises as
/// the NAV-defined token (`PIECE`, `KILOGRAM`, …) via serde's
/// `rename_all = "SCREAMING_SNAKE_CASE"` — wire body and NAV XML body
/// agree by construction.
///
/// `OWN` is intentionally NOT a variant here. The `OWN` /
/// `unitOfMeasureOwn` pairing on the NAV side is modelled at the
/// outer [`ProductUnit`] level so callers cannot accidentally emit
/// `OWN` without the paired free-text payload — a class of bug that a
/// flat `Nav(OWN)` shape would invite.
///
/// Adding a variant: confirm against NAV's v3.0 unitOfMeasureType
/// schema, extend the enum + the SCREAMING_SNAKE serde mapping, then
/// widen the SPA's dropdown. See ADR-0046.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NavUnitOfMeasure {
    Piece,
    Kilogram,
    Ton,
    Kwh,
    Day,
    Hour,
    Minute,
    Month,
    Liter,
    Kilometer,
    CubicMeter,
    Meter,
    LinearMeter,
    Carton,
    Pack,
}

impl NavUnitOfMeasure {
    /// NAV v3.0 token body — what goes between `<unitOfMeasure>` and
    /// `</unitOfMeasure>` in InvoiceData XML. The NAV XML emitter
    /// (`apps/aberp/src/nav_xml.rs::write_lines`, S159) uses this
    /// directly; serde callers receive the same string via `Serialize`.
    pub fn nav_token(self) -> &'static str {
        match self {
            NavUnitOfMeasure::Piece => "PIECE",
            NavUnitOfMeasure::Kilogram => "KILOGRAM",
            NavUnitOfMeasure::Ton => "TON",
            NavUnitOfMeasure::Kwh => "KWH",
            NavUnitOfMeasure::Day => "DAY",
            NavUnitOfMeasure::Hour => "HOUR",
            NavUnitOfMeasure::Minute => "MINUTE",
            NavUnitOfMeasure::Month => "MONTH",
            NavUnitOfMeasure::Liter => "LITER",
            NavUnitOfMeasure::Kilometer => "KILOMETER",
            NavUnitOfMeasure::CubicMeter => "CUBIC_METER",
            NavUnitOfMeasure::Meter => "METER",
            NavUnitOfMeasure::LinearMeter => "LINEAR_METER",
            NavUnitOfMeasure::Carton => "CARTON",
            NavUnitOfMeasure::Pack => "PACK",
        }
    }

    /// Compact Hungarian operator-facing label for this unit — what the
    /// printed-invoice PDF renders in the "egység" column on each line.
    /// Distinct from the NAV wire token: NAV insists on
    /// `PIECE`/`KILOGRAM`/… as the body of `<unitOfMeasure>`, but the
    /// printed invoice the buyer reads carries the Hungarian short-form
    /// (`db`, `kg`, `nap`) the operator chose. PR-202 closed the
    /// pre-existing bug where the PDF rendered the NAV token verbatim.
    ///
    /// Labels match the SPA `NAV_UNIT_OPTIONS` dropdown's `label_hu`
    /// (`apps/aberp-ui/ui/src/lib/products.ts`) except for compactness:
    /// the SPA shows `db (darab)` / `fm (folyóméter)` to disambiguate
    /// the abbreviation in a selection menu; the printed-invoice column
    /// is tight so the parenthetical long-forms are dropped here.
    pub fn display_label_hu(self) -> &'static str {
        match self {
            NavUnitOfMeasure::Piece => "db",
            NavUnitOfMeasure::Kilogram => "kg",
            NavUnitOfMeasure::Ton => "tonna",
            NavUnitOfMeasure::Kwh => "kWh",
            NavUnitOfMeasure::Day => "nap",
            NavUnitOfMeasure::Hour => "óra",
            NavUnitOfMeasure::Minute => "perc",
            NavUnitOfMeasure::Month => "hónap",
            NavUnitOfMeasure::Liter => "liter",
            NavUnitOfMeasure::Kilometer => "km",
            NavUnitOfMeasure::CubicMeter => "m³",
            NavUnitOfMeasure::Meter => "m",
            NavUnitOfMeasure::LinearMeter => "fm",
            NavUnitOfMeasure::Carton => "karton",
            NavUnitOfMeasure::Pack => "csomag",
        }
    }

    /// Parse a NAV token string back to the enum. Used by the products
    /// module's DB read path (`unit_from_db_columns`). `None` for any
    /// string outside the closed vocab.
    pub fn from_nav_token(token: &str) -> Option<Self> {
        match token {
            "PIECE" => Some(NavUnitOfMeasure::Piece),
            "KILOGRAM" => Some(NavUnitOfMeasure::Kilogram),
            "TON" => Some(NavUnitOfMeasure::Ton),
            "KWH" => Some(NavUnitOfMeasure::Kwh),
            "DAY" => Some(NavUnitOfMeasure::Day),
            "HOUR" => Some(NavUnitOfMeasure::Hour),
            "MINUTE" => Some(NavUnitOfMeasure::Minute),
            "MONTH" => Some(NavUnitOfMeasure::Month),
            "LITER" => Some(NavUnitOfMeasure::Liter),
            "KILOMETER" => Some(NavUnitOfMeasure::Kilometer),
            "CUBIC_METER" => Some(NavUnitOfMeasure::CubicMeter),
            "METER" => Some(NavUnitOfMeasure::Meter),
            "LINEAR_METER" => Some(NavUnitOfMeasure::LinearMeter),
            "CARTON" => Some(NavUnitOfMeasure::Carton),
            "PACK" => Some(NavUnitOfMeasure::Pack),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// ProductUnit — `Nav(enum) | Own(String)` outer sum.
// ──────────────────────────────────────────────────────────────────────

/// A unit of measure: either one of the NAV-defined tokens
/// ([`NavUnitOfMeasure`]) or a free-text label that the NAV emitter
/// renders as `OWN` + `<unitOfMeasureOwn>{label}</...>`.
///
/// Wire shape (internally-tagged serde):
///
/// ```json
/// {"kind": "Nav", "value": "PIECE"}
/// {"kind": "Own", "value": "liter@15C"}
/// ```
///
/// The tagged shape keeps the JSON self-describing for SPA debugging
/// at minimal cost (one extra field name). Pinned by
/// `product_unit_serde_round_trip_pin`.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(tag = "kind", content = "value")]
pub enum ProductUnit {
    /// One of the NAV v3.0 unitOfMeasure tokens.
    Nav(NavUnitOfMeasure),
    /// Operator-typed free-text label. The NAV emitter pairs this with
    /// the literal `OWN` token in the wire `<unitOfMeasure>` element.
    /// `liter@15C` (fuel measure) is the canonical example — no plain
    /// LITER variant covers the temperature correction.
    Own(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR-202 — exhaustive coverage of `display_label_hu` for every
    /// closed-vocab variant. Catches a future variant addition that
    /// extends `NavUnitOfMeasure` + `nav_token` + `from_nav_token`
    /// (the existing F12-style ritual) but forgets the HU label —
    /// without this pin the new variant would silently fall back to
    /// `Piece`'s "db" in `unit_display_from_nav`.
    #[test]
    fn display_label_hu_covers_every_variant() {
        let cases: &[(NavUnitOfMeasure, &str)] = &[
            (NavUnitOfMeasure::Piece, "db"),
            (NavUnitOfMeasure::Kilogram, "kg"),
            (NavUnitOfMeasure::Ton, "tonna"),
            (NavUnitOfMeasure::Kwh, "kWh"),
            (NavUnitOfMeasure::Day, "nap"),
            (NavUnitOfMeasure::Hour, "óra"),
            (NavUnitOfMeasure::Minute, "perc"),
            (NavUnitOfMeasure::Month, "hónap"),
            (NavUnitOfMeasure::Liter, "liter"),
            (NavUnitOfMeasure::Kilometer, "km"),
            (NavUnitOfMeasure::CubicMeter, "m³"),
            (NavUnitOfMeasure::Meter, "m"),
            (NavUnitOfMeasure::LinearMeter, "fm"),
            (NavUnitOfMeasure::Carton, "karton"),
            (NavUnitOfMeasure::Pack, "csomag"),
        ];
        for (variant, expected) in cases {
            assert_eq!(
                variant.display_label_hu(),
                *expected,
                "display_label_hu for {variant:?}"
            );
        }
        // Coverage cross-check: the table length must equal the
        // round-trippable enum cardinality so a future variant
        // addition fails this test rather than silently defaulting.
        // (Lower-bound — `from_nav_token` is the canonical breadth
        // ritual; `display_label_hu` is the load-bearing third leg.)
        let round_trippable = [
            "PIECE",
            "KILOGRAM",
            "TON",
            "KWH",
            "DAY",
            "HOUR",
            "MINUTE",
            "MONTH",
            "LITER",
            "KILOMETER",
            "CUBIC_METER",
            "METER",
            "LINEAR_METER",
            "CARTON",
            "PACK",
        ];
        for token in round_trippable {
            assert!(
                NavUnitOfMeasure::from_nav_token(token).is_some(),
                "from_nav_token should accept {token}"
            );
        }
        assert_eq!(cases.len(), round_trippable.len());
    }
}
