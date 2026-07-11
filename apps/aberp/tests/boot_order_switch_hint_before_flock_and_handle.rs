//! ADR-0099 H3 — boot-order regression pin (durability×feature merge).
//!
//! # The ordering this pins
//!
//! The merged `run()` boot MUST perform these three steps in this order:
//!
//!   1. resolve the S433 tenant switch-hint — `resolve_effective_serve_args`
//!      consumes `~/.aberp/next_tenant` and, when a switch is honored,
//!      rebinds `args` to the switched-to tenant + db (`let args = &effective`);
//!   2. acquire the cross-process whole-DB writer flock
//!      (`db_writer_lock::acquire_or_refuse`, ADR-0099 F-E);
//!   3. open the ONE shared `aberp_db::Handle` (`open_tenant_handle`, H3).
//!
//! # Why the ordering is load-bearing
//!
//! Both the flock (step 2) and the Handle (step 3) key off `args.db` /
//! `tenant`. If a refactor moved EITHER ahead of the switch-hint
//! resolution, a boot that honored a tenant switch would flock + open the
//! ORIGINAL `--db`, not the switched-to tenant's DB: the process would
//! single-writer-lock one tenant's file while appending the audit ledger
//! of another — a cross-tenant durability corruption that no later step
//! can undo. The merged tree got this ordering right only by auto-merge
//! luck (the switch-hint override at serve.rs ~:725, the flock at ~:826,
//! the Handle at ~:1494 were untouched by the merge), so nothing pins it.
//! This test is that pin: a future refactor that reorders these steps
//! fails here instead of silently corrupting a switched tenant's ledger.
//!
//! # Why a structural pin (not an end-to-end boot)
//!
//! The ordering lives inside the monolithic `run()` boot fn, which cannot
//! be exercised in a unit test: it binds a loopback TLS listener, reads
//! the OS keychain (which prompts), and spawns the NAV/email daemons. So
//! this scopes the search to `run()`'s source and asserts the first
//! occurrence of each step's anchor appears in the required order.

/// The `serve.rs` source, embedded at test-compile time (a change to it
/// recompiles this pin).
const SERVE_RS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/serve.rs"));

/// First byte index of `needle` at or after `from`.
fn find_after(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay[from..].find(needle).map(|i| from + i)
}

#[test]
fn switch_hint_resolves_before_flock_and_handle_open() {
    let src = SERVE_RS;

    // Scope every anchor to `run()`'s body: `resolve_effective_serve_args`
    // and `open_tenant_handle` also appear at their DEFINITIONS elsewhere in
    // the file, so an unscoped search could match the wrong occurrence.
    let run_start = src
        .find("pub fn run(args: &ServeArgs) -> Result<()> {")
        .expect("locate the run() boot fn in serve.rs");

    let switch_hint = find_after(src, "resolve_effective_serve_args(args);", run_start).expect(
        "run() must call resolve_effective_serve_args(args) — the S433 switch-hint override",
    );
    let rebind = find_after(src, "let args = &effective;", run_start).expect(
        "run() must rebind `args` to the switched-to `effective` args after the switch-hint",
    );
    let flock = find_after(src, "acquire_or_refuse(", run_start).expect(
        "run() must acquire the whole-DB writer flock (db_writer_lock::acquire_or_refuse, F-E)",
    );
    let handle = find_after(src, "open_tenant_handle(", run_start)
        .expect("run() must open the shared aberp_db::Handle via open_tenant_handle (H3)");

    assert!(
        switch_hint < rebind,
        "the switch-hint must be resolved (resolve_effective_serve_args) before `args` is \
         rebound to the switched-to `effective` args"
    );
    assert!(
        rebind < flock,
        "BOOT-ORDER REGRESSION: the tenant switch-hint (resolve_effective_serve_args + \
         `let args = &effective`) MUST be resolved BEFORE the whole-DB writer flock is \
         acquired. A honored tenant switch that flocks AFTER resolving would fence the \
         switched-to DB; one that flocks BEFORE would fence the ORIGINAL --db while later \
         steps run as the switched tenant — cross-tenant single-writer corruption. (ADR-0099 H3)"
    );
    assert!(
        flock < handle,
        "BOOT-ORDER REGRESSION: the whole-DB writer flock MUST be acquired BEFORE the shared \
         aberp_db::Handle opens the live DB — the flock fences a second concurrent writer out \
         before this process opens the file (ADR-0099 F-E → H3)."
    );
    // Transitive closure, stated explicitly so a failure names the full chain.
    assert!(
        switch_hint < flock && rebind < handle,
        "switch-hint → flock → Handle-open ordering violated in run()"
    );
}
