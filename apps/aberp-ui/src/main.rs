//! ABERP Tauri shell — operator UI entry point (PR-9-2).
//!
//! Thin shim around `aberp_ui::run()` matching the apps/aberp pattern.
//! The orchestration (subprocess launch + handshake parse + pinned
//! reqwest client + Tauri command surface) lives in `lib.rs` so the
//! integration tests under `tests/` can import it.
//!
//! See `lib.rs` for the per-module commentary.

#![forbid(unsafe_code)]
// Hide the Windows console window in release builds. Dev builds keep it
// open so the operator can see the embedded `aberp serve` log stream.
#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

fn main() {
    aberp_ui::run();
}
