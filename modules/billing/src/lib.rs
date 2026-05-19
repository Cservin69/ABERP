//! ABERP billing module.
//!
//! Owns NAV invoice issuing. Domain types, sequence allocator, NAV adapter.
//! Implementation lands in PR-4 per `_handoffs/05-session-5-code-can-start.md`.
//!
//! The module follows the layout in ADR-0006 (`domain/`, `app/`, `ports/`,
//! `adapters/`, `api.rs`). Those subdirectories are added in PR-4 when there
//! is content for them; per CLAUDE.md rule 2, PR-1 does not pre-create
//! empty scaffolding.
//!
//! Design references:
//!
//! - ADR-0009  NAV invoice issuing (lifecycle, sequence allocator, retry,
//!             offline queue, audit-evidence export, certification posture).
//! - ADR-0020  NAV transport and credential correction.
//! - ADR-0006  Module boundaries and contracts.
//! - ADR-0005  Prefixed ULID identifiers.
