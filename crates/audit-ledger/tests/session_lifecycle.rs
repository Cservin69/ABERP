//! S441 / ADR-0086 + ADR-0087 + ADR-0088 — integration coverage for the
//! timestamp-anchored audit-chain session lifecycle.
//!
//! These exercise the public session API against an in-memory [`Ledger`]:
//! the [[customer-journey-e2e-gate]] full round-trip (boot → login → events
//! → heartbeat → logout → verify green), the chokepoint signed/unsigned
//! split, crash recovery, the service session, heartbeat anchoring, and the
//! tamper-detection negative.

use aberp_audit_ledger::session::crypto::SessionKey;
use aberp_audit_ledger::session::tsa::TimestampAuthority;
use aberp_audit_ledger::session::{
    close_operator_session, endorse_service_session, heartbeat, open_operator_session,
    open_service_session, recover_crashed_sessions, MockTimestampAuthority, OperatorIdentity,
    MOCK_TSA_IDENTIFIER,
};
use aberp_audit_ledger::{verify_chain_signed, Actor, BinaryHash, EventKind, Ledger, TenantId};

fn ledger() -> Ledger {
    Ledger::open_in_memory(
        TenantId::new("s441-test").unwrap(),
        BinaryHash::from_bytes([1u8; 32]),
    )
    .expect("open in-memory ledger")
}

fn actor() -> Actor {
    Actor::from_local_cli("proc-1".to_string(), "ervin")
}

fn operator() -> OperatorIdentity {
    OperatorIdentity {
        dap_subject: "hu-citizen-0001".to_string(),
        display_name: "Áben Ervin".to_string(),
        attested_at_utc: "2026-06-17T10:00:00Z".to_string(),
        identity_source: "dap".to_string(),
    }
}

/// Verify the whole chain (base + signatures + anchors), dispatching the
/// mock authority for mock-anchored rows. `subject_of` returns None for
/// session-lifecycle events — matching the signer.
fn verify_all(ledger: &Ledger) -> aberp_audit_ledger::ChainVerdict {
    let entries = ledger.entries().expect("entries");
    let anchors = ledger.anchors().expect("anchors");
    let tsa = MockTimestampAuthority::new();
    verify_chain_signed(
        ledger.tenant_id(),
        &entries,
        &anchors,
        |_e| None,
        |id| {
            if id == MOCK_TSA_IDENTIFIER {
                Some(&tsa as &dyn TimestampAuthority)
            } else {
                None
            }
        },
    )
    .expect("chain verifies green")
}

// Test 17 + [[customer-journey-e2e-gate]] — boot with dap_enabled=true →
// login → 3 events → heartbeat → logout → verify whole chain green.
#[test]
fn e2e_operator_session_full_lifecycle_verifies_green() {
    let mut l = ledger();
    let tsa = MockTimestampAuthority::new();

    let (ctx, login_anchor) =
        open_operator_session(&mut l, &tsa, actor(), operator()).expect("login");
    assert_eq!(login_anchor.kind.as_str(), "LoginOpen");

    // 3 signed business events under the session.
    for i in 0..3 {
        l.append_signed(
            EventKind::Test,
            "",
            format!("{{\"i\":{i}}}").into_bytes(),
            actor(),
            None,
            Some(&ctx),
        )
        .expect("signed event");
    }

    heartbeat(&mut l, &tsa, actor(), &ctx).expect("heartbeat");
    let logout = close_operator_session(&mut l, &tsa, actor(), &ctx).expect("logout");
    assert_eq!(logout.kind.as_str(), "LogoutClose");

    let verdict = verify_all(&l);
    assert!(verdict.fully_anchored, "every anchor is mock-verified");
    // SessionOpened + 3 events + TimestampAnchorTaken(heartbeat) + SessionClosed = 6 signed.
    assert_eq!(verdict.signatures_verified, 6);
    // LoginOpen + Heartbeat + LogoutClose = 3 anchors.
    assert_eq!(verdict.anchors_anchored, 3);
    assert_eq!(verdict.anchors_pending, 0);
}

// Test 16 — chokepoint: with session ⇒ sig present + verifiable; without
// session ⇒ no sig (back-compat).
#[test]
fn chokepoint_signs_only_with_a_session() {
    let mut l = ledger();
    let tsa = MockTimestampAuthority::new();
    let (ctx, _) = open_operator_session(&mut l, &tsa, actor(), operator()).expect("login");

    // Unsigned legacy append.
    l.append(EventKind::Test, b"legacy".to_vec(), actor(), None)
        .expect("unsigned append");
    // Signed append.
    l.append_signed(
        EventKind::Test,
        "",
        b"signed".to_vec(),
        actor(),
        None,
        Some(&ctx),
    )
    .expect("signed append");

    let entries = l.entries().expect("entries");
    let unsigned = entries.iter().find(|e| e.payload == b"legacy").unwrap();
    let signed = entries.iter().find(|e| e.payload == b"signed").unwrap();

    assert!(unsigned.event_sig.is_none(), "legacy append carries no sig");
    assert!(unsigned.session_id.is_none());
    assert!(signed.event_sig.is_some(), "signed append carries a sig");
    assert_eq!(
        signed.session_pubkey.as_deref(),
        Some(ctx.pubkey_hex().as_str())
    );
}

// Test 18 — crash recovery: an orphan open session (no clean logout) is
// detected on next boot → SessionCrashRecovered fires + a recovery anchor.
#[test]
fn crash_recovery_closes_orphan_sessions() {
    let mut l = ledger();
    let tsa = MockTimestampAuthority::new();

    // Prior run: a session opened but never closed.
    let (orphan, _) = open_operator_session(&mut l, &tsa, actor(), operator()).expect("login");
    let orphan_id = orphan.session_id.clone();
    drop(orphan); // the key is gone, simulating a crash — only the anchor row remains.

    assert_eq!(
        l.open_sessions_without_close().unwrap(),
        vec![orphan_id.clone()],
        "the orphan is detected before recovery"
    );

    // New boot: open a fresh session, run recovery.
    let (boot, _) = open_operator_session(&mut l, &tsa, actor(), operator()).expect("boot login");
    let recovered = recover_crashed_sessions(&mut l, &tsa, actor(), &boot).expect("recover");

    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].orphan_session_id, orphan_id);
    assert_eq!(recovered[0].recovery_anchor.kind.as_str(), "LogoutClose");
    // The orphan is no longer open. (Only `boot` remains open.)
    assert_eq!(
        l.open_sessions_without_close().unwrap(),
        vec![boot.session_id.clone()]
    );

    let kinds: Vec<&str> = l
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert!(kinds.contains(&"auth.session_crash_recovered"));
    verify_all(&l);
}

// Test 19 — service session: dap_enabled=true → service session opens at
// boot + an endorsement event fires on first operator login.
#[test]
fn service_session_opens_and_is_endorsed() {
    let mut l = ledger();
    let tsa = MockTimestampAuthority::new();

    // Boot: open the service session BEFORE any operator.
    let service_key = SessionKey::fresh().unwrap();
    let service_pubkey = service_key.pubkey_hex();
    let (svc, open_anchor) =
        open_service_session(&mut l, &tsa, actor(), service_key).expect("service open");
    assert_eq!(open_anchor.kind.as_str(), "ServiceOpen");
    assert!(svc.session_id.starts_with("svc_"));

    // First operator login endorses the service key.
    let (op, _) = open_operator_session(&mut l, &tsa, actor(), operator()).expect("login");
    let endorse = endorse_service_session(
        &mut l,
        &tsa,
        actor(),
        &op,
        &service_pubkey,
        "hu-citizen-0001",
    )
    .expect("endorse");
    assert_eq!(endorse.kind.as_str(), "ServiceEndorse");

    let kinds: Vec<&str> = l
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert!(kinds.contains(&"auth.service_session_opened"));
    assert!(kinds.contains(&"auth.service_session_endorsed"));
    verify_all(&l);
}

// Test 20 — heartbeat: a heartbeat tick produces an anchor row + event.
#[test]
fn heartbeat_produces_an_anchor_row() {
    let mut l = ledger();
    let tsa = MockTimestampAuthority::new();
    let (ctx, _) = open_operator_session(&mut l, &tsa, actor(), operator()).expect("login");

    let before = l.anchors().unwrap().len();
    let hb = heartbeat(&mut l, &tsa, actor(), &ctx).expect("heartbeat");
    assert_eq!(hb.kind.as_str(), "Heartbeat");
    assert_eq!(l.anchors().unwrap().len(), before + 1);

    let kinds: Vec<&str> = l
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert!(kinds.contains(&"audit.timestamp_anchor_taken"));
}

// A tampered signature is caught by the extended verifier (the base hash
// chain stays intact — event_sig is excluded from the entry_hash preimage —
// so the failure surfaces at the signature layer, ADR-0087).
#[test]
fn tampered_signature_fails_extended_verify() {
    let mut l = ledger();
    let tsa = MockTimestampAuthority::new();
    let (ctx, _) = open_operator_session(&mut l, &tsa, actor(), operator()).expect("login");
    l.append_signed(
        EventKind::Test,
        "",
        b"x".to_vec(),
        actor(),
        None,
        Some(&ctx),
    )
    .expect("signed");

    let mut entries = l.entries().unwrap();
    let anchors = l.anchors().unwrap();
    // Flip a hex nibble of the last entry's signature.
    let last = entries.last_mut().unwrap();
    let mut sig = last.event_sig.clone().unwrap();
    let flipped = if sig.starts_with('a') { 'b' } else { 'a' };
    sig.replace_range(0..1, &flipped.to_string());
    last.event_sig = Some(sig);

    let verdict = verify_chain_signed(
        l.tenant_id(),
        &entries,
        &anchors,
        |_e| None,
        |id| {
            if id == MOCK_TSA_IDENTIFIER {
                Some(&tsa as &dyn TimestampAuthority)
            } else {
                None
            }
        },
    );
    assert!(verdict.is_err(), "a tampered signature must fail verify");
}
