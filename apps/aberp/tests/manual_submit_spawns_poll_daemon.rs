//! Pin test — every successful NAV submit path hands the invoice to the
//! indefinite ack poll daemon, including the manual
//! `POST /invoices/:id/submit` route.
//!
//! # Why this file exists
//!
//! Session-161 moved the post-issue auto-submit onto the two-phase poll
//! daemon so the pictogram flips to its terminal state on its own. The
//! manual submit route (InvoiceDetail's Submit button, used after a
//! NAV-off issue or a failed auto-submit) was left out: it submitted and
//! returned, and the invoice sat at Submitted until an operator poll
//! click or the next app boot's recovery scan.
//!
//! # What this file pins
//!
//! Source-text invariant (A151 source-grep posture, same as
//! `submit_poll_no_nested_runtime.rs`). The in-process test harness has
//! no NAV double, so the spawn is pinned structurally here and verified
//! end to end against the NAV TEST endpoint:
//!
//!   1. `handle_submit_invoice` calls the shared
//!      `spawn_nav_poll_daemon_after_submit` helper on success, guarded
//!      by `!outcome.local_only`.
//!   2. Both post-issue tails use the same helper (one spawn path,
//!      CLAUDE.md rule 8), and the helper actually runs the daemon.

const SERVE_SRC: &str = include_str!("../src/serve.rs");

fn fn_body(src: &str, signature: &str) -> String {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("`{signature}` not found in serve.rs"));
    let rest = &src[start..];
    // Top-level fns end at the first `\n}\n` after their signature.
    let end = rest.find("\n}\n").expect("unterminated fn body");
    rest[..end].to_string()
}

#[test]
fn manual_submit_route_spawns_the_poll_daemon() {
    let body = fn_body(SERVE_SRC, "async fn handle_submit_invoice(");
    let spawn = body.find("spawn_nav_poll_daemon_after_submit(").expect(
        "handle_submit_invoice MUST hand a successful submit to \
             spawn_nav_poll_daemon_after_submit; without it a manually \
             submitted invoice stays at Submitted until a manual poll",
    );
    let guard = body
        .find("if !outcome.local_only")
        .expect("the spawn MUST be skipped for local-only outcomes (no NAV transaction)");
    assert!(guard < spawn, "the local_only guard must wrap the spawn");
    let ok_arm = body.find("Ok(outcome) =>").expect("Ok arm");
    let first_err = body.find("Err(SubmitRouteError::").expect("Err arm");
    assert!(
        ok_arm < spawn && spawn < first_err,
        "the spawn must sit in the success arm only"
    );
}

#[test]
fn every_submit_path_shares_one_spawn_helper() {
    let helper = fn_body(SERVE_SRC, "fn spawn_nav_poll_daemon_after_submit(");
    assert!(helper.contains("build_poll_daemon_inputs("));
    assert!(helper.contains("poll_ack::run_nav_poll_daemon("));
    assert!(helper.contains("tokio::spawn("));
    for origin in [
        "\"post-issue\"",
        "\"post-issue chain\"",
        "\"manual submit\"",
    ] {
        assert!(
            SERVE_SRC.contains(origin),
            "submit path {origin} must route through spawn_nav_poll_daemon_after_submit"
        );
    }
    // Boot recovery spawns per pending invoice from its own fan-out; every
    // other daemon start goes through the helper.
    assert_eq!(
        SERVE_SRC.matches("poll_ack::run_nav_poll_daemon(").count(),
        2,
        "expected exactly two daemon call sites: the shared helper and \
         the boot-recovery fan-out"
    );
}
