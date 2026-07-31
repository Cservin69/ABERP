//! ADR-0108 §3 — **the money representation rules, measured.**
//!
//! T-5(a) (the `Decimal` ↔ `TEXT` property), T-1 (`STRICT` refuses a float
//! into a money column), and R3's premise (`BLOB` and `TEXT` never compare
//! equal). Every one of them is a claim §3 makes; this file is where they stop
//! being claims.
//!
//! # The correction this file forced
//!
//! §3.1 says `STRICT` "makes a storage-class violation loud". Measured, that
//! is **true for R1 and R3 and false for R2**, and the difference is
//! load-bearing:
//!
//! | declared | given a REAL | result |
//! |---|---|---|
//! | `INTEGER` (R1, money) | `1234.56` | **`SQLITE_CONSTRAINT_DATATYPE`** |
//! | `INTEGER` (R1, money) | `1234.0` | accepted — losslessly an integer |
//! | `TEXT` (R2, quantities/rates) | `1.5` | **ACCEPTED, stringified to `'1.5'`** |
//! | `BLOB` (R3, hashes) | `'abc'` | **`SQLITE_CONSTRAINT_DATATYPE`** |
//!
//! `STRICT` applies the usual affinity conversion and only refuses what cannot
//! convert *losslessly*. REAL → TEXT always converts, so **an R2 column is not
//! protected by `STRICT` at all**. Its guards are the three §3 already names,
//! now known to be the *only* ones: the Rust-side bind (a `Decimal`, never an
//! `f64`), the T-8 cut-gate that keeps arithmetic out of SQL (`TEXT * INTEGER`
//! coerces to `REAL`), and the `typeof()` sweep — which, note, would **not**
//! catch this: a stringified float is still `'text'`.
//!
//! That is the difference between `STRICT` being the R2 mitigation and
//! `STRICT` being incidental to it, and the plan read the first way.

//! Every arm here is `STRICT`-specific, so the whole file is gated: under the
//! default build `aberp_db::engine::Connection` is DuckDB, which has neither
//! `STRICT` nor SQLite's storage classes.

#![cfg(feature = "sqlite-engine")]

use aberp_db::engine::Connection;

/// A STRICT table with one column of each declared type the plan uses.
fn probe() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE m (
             k    INTEGER NOT NULL PRIMARY KEY,
             money INTEGER,
             dec   TEXT,
             hash  BLOB,
             ratio REAL
         ) STRICT;",
    )
    .unwrap();
    c
}

fn extended_code(e: &aberp_db::engine::Error) -> Option<i32> {
    match e {
        aberp_db::engine::Error::SqliteFailure(f, _) => Some(f.extended_code),
        _ => None,
    }
}

/// `SQLITE_CONSTRAINT` (19) with the `DATATYPE` sub-code (12).
const SQLITE_CONSTRAINT_DATATYPE: i32 = 19 | (12 << 8);

// ---------------------------------------------------------------------------
// T-1 / R1 — money is INTEGER and a float cannot get in
// ---------------------------------------------------------------------------

/// **T-1.** A fractional REAL into an R1 money column is
/// `SQLITE_CONSTRAINT_DATATYPE`. This is PR #49 F-6a's closure and the reason
/// every table in this migration carries `STRICT`.
///
/// Mutation-verify: drop the ` STRICT` suffix from the DDL above and the
/// insert succeeds, storing `1234.56` as a float in the column that feeds the
/// NAV filing.
#[test]
fn t1_strict_refuses_a_fractional_float_into_a_money_column() {
    let c = probe();
    let err = c
        .execute("INSERT INTO m (k, money) VALUES (1, 1234.56)", [])
        .expect_err("a fractional REAL must not enter an INTEGER money column");
    assert_eq!(
        extended_code(&err),
        Some(SQLITE_CONSTRAINT_DATATYPE),
        "{err}"
    );

    // The narrowing that matters: STRICT refuses what cannot convert
    // LOSSLESSLY, so a whole-numbered float IS accepted — and lands as an
    // integer, not as a float. That is safe, but it is not the same claim as
    // "a float cannot be written", and a reader who believes the stronger
    // claim will not think to check the Rust bind.
    c.execute("INSERT INTO m (k, money) VALUES (2, 1234.0)", [])
        .expect("a losslessly-integral REAL is accepted and converted");
    let (v, t): (i64, String) = c
        .query_row("SELECT money, typeof(money) FROM m WHERE k = 2", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((v, t.as_str()), (1234, "integer"));
}

/// **The R2 hole, pinned so it cannot be rediscovered as a surprise.**
///
/// `STRICT` does **not** protect a `TEXT` column from a float: REAL → TEXT is
/// a lossless conversion, so the value is accepted and stringified. A
/// `typeof()` sweep sees `'text'` and passes. The only things standing between
/// an R2 column and a float are the Rust-side `Decimal` bind and T-8's
/// no-arithmetic-in-SQL gate.
///
/// The value below is the canonical demonstration: `0.1 + 0.2` in SQL is REAL
/// arithmetic, and what lands in the column is the float's decimal rendering,
/// not `0.3`.
#[test]
fn strict_does_not_protect_an_r2_text_column_from_a_float() {
    let c = probe();
    c.execute("INSERT INTO m (k, dec) VALUES (1, 0.1 + 0.2)", [])
        .expect("REAL → TEXT converts losslessly, so STRICT accepts it");
    let (v, t): (String, String) = c
        .query_row("SELECT dec, typeof(dec) FROM m WHERE k = 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        t, "text",
        "the typeof() sweep is blind to this — the float is stored AS text"
    );
    assert_ne!(
        v, "0.3",
        "if this ever equals 0.3, SQLite changed its REAL→TEXT rendering and the \
         demonstration below needs re-measuring"
    );
    assert!(
        v.starts_with("0.30000"),
        "the stored value is the float's rendering, not the exact decimal: {v:?}"
    );
    // And it no longer round-trips as the value anyone intended.
    assert_ne!(
        rust_decimal::Decimal::from_str_exact(&v).unwrap(),
        rust_decimal::Decimal::from_str_exact("0.3").unwrap()
    );
}

/// **R3.** A `BLOB` column refuses TEXT — there is no lossless TEXT → BLOB
/// conversion, so unlike R2 this one really is enforced by `STRICT`.
///
/// And the premise underneath R3: in SQLite `BLOB` and `TEXT` are distinct
/// storage classes that **never compare equal**, which is why a single `&str`
/// bind where `&[u8]` belongs makes a chain-link lookup return "not found".
#[test]
fn r3_blob_refuses_text_and_the_two_classes_never_compare_equal() {
    let c = probe();
    let err = c
        .execute("INSERT INTO m (k, hash) VALUES (1, 'abc')", [])
        .expect_err("TEXT must not enter a BLOB column");
    assert_eq!(
        extended_code(&err),
        Some(SQLITE_CONSTRAINT_DATATYPE),
        "{err}"
    );

    c.execute(
        "INSERT INTO m (k, hash) VALUES (1, ?)",
        [&b"abc".to_vec() as &dyn aberp_db::engine::ToSql],
    )
    .unwrap();
    let equal: i64 = c
        .query_row("SELECT count(*) FROM m WHERE hash = 'abc'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        equal, 0,
        "x'616263' and 'abc' must NOT compare equal — this is the whole reason hashes are \
         bound as &[u8] and never as &str"
    );
    let found: i64 = c
        .query_row(
            "SELECT count(*) FROM m WHERE hash = ?",
            [&b"abc".to_vec() as &dyn aberp_db::engine::ToSql],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}

// ---------------------------------------------------------------------------
// T-5(a) — the Decimal ↔ TEXT property
// ---------------------------------------------------------------------------

/// A deterministic generator. No `rand` dependency, and a failure is
/// reproducible from the seed alone rather than from a captured corpus.
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state
}

/// **T-5(a).** 100 000 generated `Decimal`s at scales 0–6 — including
/// trailing-zero forms, negatives and the scale extremes — round-trip through
/// a `STRICT TEXT` column **byte-identically**, and re-parse to the same
/// value.
///
/// Byte-identity is the strong form and it is the one the migration needs:
/// §3.2 C carries the DuckDB-rendered string verbatim, so if SQLite ever
/// normalised what it stored, a migrated row and a freshly-written row would
/// disagree on their stored bytes for the same value.
#[test]
fn t5a_one_hundred_thousand_decimals_round_trip_through_text_byte_identically() {
    use rust_decimal::Decimal;

    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE q (k INTEGER NOT NULL PRIMARY KEY, v TEXT NOT NULL) STRICT;")
        .unwrap();

    let mut seed = 0x0108_0005_D0DE_CADEu64;
    let mut cases: Vec<String> = Vec::with_capacity(100_000);
    for _ in 0..100_000 {
        let mantissa = (lcg(&mut seed) % 2_000_000_000_000_000) as i64;
        let signed = if lcg(&mut seed).is_multiple_of(2) {
            mantissa
        } else {
            -mantissa
        };
        let scale = (lcg(&mut seed) % 7) as u32;
        cases.push(Decimal::new(signed, scale).to_string());
    }
    // The forms a generator will not reliably produce, added by hand because
    // they are the ones §3.2 C's trailing-zero note turns on.
    for s in [
        "0",
        "0.0",
        "0.000000",
        "1.5",
        "1.500000",
        "-0.000001",
        "0.000001",
        "310.550000",
        "79228162514264337593543950335", // Decimal::MAX
        "-79228162514264337593543950335",
    ] {
        cases.push(s.to_string());
    }

    {
        let tx = c.unchecked_transaction().unwrap();
        for (i, s) in cases.iter().enumerate() {
            tx.execute(
                "INSERT INTO q (k, v) VALUES (?, ?)",
                aberp_db::engine::params![i as i64, s],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    let mut stmt = c
        .prepare("SELECT k, v, typeof(v) FROM q ORDER BY k")
        .unwrap();
    let rows: Vec<(i64, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(rows.len(), cases.len());
    for (k, got, ty) in &rows {
        let want = &cases[*k as usize];
        assert_eq!(got, want, "stored bytes must be identical for {want:?}");
        assert_eq!(ty, "text", "{want:?} came back as {ty}");
        assert_eq!(
            Decimal::from_str_exact(got).unwrap(),
            Decimal::from_str_exact(want).unwrap(),
            "value drift on {want:?}"
        );
    }
}

/// **R1's round-trip, at the boundaries.** Money crosses as a bare `i64`; the
/// values that would break a float representation are the ones asserted.
///
/// `2^53 + 1` is the first integer an `f64` cannot represent. If any part of
/// the money path ever went through a float, this is the row that would come
/// back wrong — and it comes back exact.
#[test]
fn r1_money_round_trips_through_integer_at_the_i64_boundaries() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE m (k INTEGER NOT NULL PRIMARY KEY, v INTEGER NOT NULL) STRICT;")
        .unwrap();

    let cases: [i64; 9] = [
        0,
        1,
        -1,
        i64::MAX,
        i64::MIN,
        9_007_199_254_740_992,  // 2^53
        9_007_199_254_740_993,  // 2^53 + 1 — not representable as f64
        -9_007_199_254_740_993, // and its negative
        4_611_686_018_427_387_904,
    ];
    for (i, v) in cases.iter().enumerate() {
        c.execute(
            "INSERT INTO m (k, v) VALUES (?, ?)",
            aberp_db::engine::params![i as i64, v],
        )
        .unwrap();
    }
    let mut stmt = c
        .prepare("SELECT k, v, typeof(v) FROM m ORDER BY k")
        .unwrap();
    let rows: Vec<(i64, i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (k, v, ty) in &rows {
        assert_eq!(*v, cases[*k as usize], "money drift at index {k}");
        assert_eq!(ty, "integer");
    }
}
