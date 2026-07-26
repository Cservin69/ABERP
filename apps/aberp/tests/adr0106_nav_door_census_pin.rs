//! ADR-0106 — NAV-emission door count pins, in the `cargo test` loop.
//!
//! WHY THIS EXISTS ALONGSIDE THE CUT-GATE. `tools/cut_gate_nav_emit_door.sh` is
//! the real gate: it censuses the whole reaching set, checks closure, and
//! machine-checks the declared-`direct` door. But it runs in CI only — it is
//! NOT part of `cargo test`. That is the same gap the opener census has, and
//! the same gap that bites: an implementer adds a NAV door, sees a green local
//! test run, and only learns about the census from a red pipeline much later.
//!
//! These pins close the local half of that loop for the two counts that matter
//! most, and they are code-coupled, not file-coupled — the emitter pin reads
//! `nav_xml.rs` itself, so adding an eighth emitter reds `cargo test` on the
//! machine that wrote it.
//!
//! They are DELIBERATELY not a reimplementation of the gate. If one of these
//! reds, the fix is to run the gate, update the registry, and re-freeze — not
//! to bump the number here and move on.

use std::path::{Path, PathBuf};

fn src(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn tools(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools")
        .join(rel)
}

/// The NAV wire-body emitters in `nav_xml.rs`. Seven today:
/// `render_invoice_data{,_with_number}`, `render_storno_data{,_with_number}`,
/// `render_modification_data{,_with_number}`, `render_annulment_data`.
///
/// This is the pin that would have caught `render_annulment_data` being absent
/// from a hand-written list of "the NAV emitters" — which is exactly what
/// happened while ADR-0106 was being built.
#[test]
fn adr0106_nav_xml_has_exactly_seven_wire_body_emitters() {
    let source = std::fs::read_to_string(src("src/nav_xml.rs")).expect("read nav_xml.rs");
    let emitters: Vec<&str> = source
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            // Definition lines only, at item level: `pub fn render_<what>_data(`
            // or `..._data_with_number(`. Doc comments start with `///` and are
            // excluded by the `pub fn` prefix.
            let rest = l.strip_prefix("pub fn render_")?;
            let name = rest.split('(').next()?;
            (name.ends_with("_data") || name.ends_with("_data_with_number")).then_some(l)
        })
        .collect();

    assert_eq!(
        emitters.len(),
        7,
        "nav_xml.rs must expose exactly 7 NAV wire-body emitters — found {}:\n  {}\n\n\
         A NEW emitter is a new way to put bytes on the NAV wire. Register it in \
         tools/adr0106_nav_reach_symbols.txt, re-freeze \
         tools/adr0106_nav_door_fingerprints.txt via \
         tools/cut_gate_nav_emit_door.sh, and only then update this count.",
        emitters.len(),
        emitters.join("\n  ")
    );
}

/// The registered doors — terminal entrypoints that can reach NAV filing.
/// Four today: the issue route (`direct`), the storno and modification routes
/// (`derived`), and the CLI dispatch (`none`).
///
/// A fifth door means a fifth way to reach NAV filing, and the reviewer's
/// question is always the same one: does it pass `validate_invoice_preflight`?
#[test]
fn adr0106_door_registry_declares_exactly_four_doors() {
    let registry = std::fs::read_to_string(tools("adr0106_nav_door_registry.txt"))
        .expect("read door registry");
    let doors: Vec<&str> = registry
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    assert_eq!(
        doors.len(),
        4,
        "the ADR-0106 door registry must declare exactly 4 NAV-reaching doors — found {}:\n  {}",
        doors.len(),
        doors.join("\n  ")
    );

    // Exactly one door may claim to preflight directly, and it is the issue
    // route. If a second appears, Invariant P has started landing and this pin
    // should be updated deliberately, in that change, with its own reasoning.
    let direct: Vec<&&str> = doors
        .iter()
        .filter(|l| l.split_whitespace().nth(1) == Some("direct"))
        .collect();
    assert_eq!(
        direct.len(),
        1,
        "expected exactly 1 `direct` door (serve.rs::handle_issue_invoice), found {}",
        direct.len()
    );
    assert!(
        direct[0].starts_with("serve.rs::handle_issue_invoice"),
        "the sole `direct` door must be serve.rs::handle_issue_invoice, found: {}",
        direct[0]
    );
}

/// `validate_invoice_preflight` must keep exactly one production call site.
///
/// This is the F1 shape reduced to its smallest checkable form: the choke point
/// existing but being called from nowhere is indistinguishable, at the type
/// level, from it not existing. The cut-gate's CHECK N3 asserts the same thing
/// from the other side (the door that *claims* to preflight must be observed
/// doing it); this asserts it from the choke point's side, in the local test
/// loop.
#[test]
fn adr0106_preflight_choke_point_keeps_its_production_call_site() {
    let source = std::fs::read_to_string(src("src/serve.rs")).expect("read serve.rs");
    // serve.rs's `#[cfg(test)]` module is the last item in the file, so
    // truncating at it keeps this pin to production code without needing a
    // full lexer here (the cut-gate's scanner is the lexer-accurate authority).
    let prod = match source.find("\n#[cfg(test)]\n") {
        Some(i) => &source[..i],
        None => &source[..],
    };
    let calls = prod.matches("validate_invoice_preflight(&request)").count();
    assert_eq!(
        calls,
        1,
        "expected exactly 1 production call to validate_invoice_preflight in serve.rs, found {calls}. \
         Zero means the single validation choke point has been orphaned — the defect class ADR-0106 \
         exists for. More than one means the choke point moved or a second door now preflights; if \
         that is Invariant P landing, update this pin and the door registry together."
    );
}
