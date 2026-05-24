# ADR-0001 — Backend language: Rust

- **Status:** Accepted (cornerstone — pre-decided)
- **Date:** 2026-05-19
- **Deciders:** Ervin

## Context

ABERP must run as a long-lived process per tenant, hold sensitive financial and
inventory data, talk to external systems (NAV, Billingo, label printers, eventually
robotics), and ship as a single desktop binary inside Tauri today and a server
binary on cloud later. Memory-safety bugs in this surface are unacceptable: a
use-after-free in the NAV submission path is a tax incident. The language also
needs a strong type system (we'll use it to keep modules honest) and good
ecosystem support for crypto, async, and embedded databases.

Alternatives considered: Go (chosen against because the type system is weaker
and error handling is by convention rather than enforced), Kotlin/JVM (chosen
against because of the JVM footprint inside Tauri and a softer story for FFI
to native printer/robotics SDKs), Node/TypeScript (chosen against for the
backend because of single-threaded model and historically weak supply-chain
hygiene; TS is fine for the cloud UI).

## Decision

All backend code is written in Rust, stable channel, with the toolchain version
pinned in `rust-toolchain.toml`. Edition 2021 (or later when stable). No `unsafe`
in business modules; `unsafe` is permitted only in well-isolated adapters (e.g.,
binding to a native printer SDK) and must be reviewed.

## Consequences

- Hiring is harder than for TypeScript or Go. Accepted — the team is small and the safety bar is high.
- Compile times will hurt as the workspace grows. We commit to module crate splits (ADR-0006) which keep incremental builds fast.
- FFI to label-printer and robotics SDKs (often C or C++) is easier than from a JVM language, which is a deliberate side benefit.
- Tauri integration is native — Tauri itself is Rust, so the desktop shell and the backend share a runtime.
- We can publish a single static binary per platform with reproducible builds. Important for audit evidence: "the binary the auditor sees is the binary that signed the invoice".

## Adversarial review

- *"Rust is overkill for a CRUD app."* — ABERP is not a CRUD app. It is a long-running, multi-tenant system that handles tax-authority submissions, label printers, and eventually robotics. The safety floor matters.
- *"What about async ecosystem churn?"* — We commit to `tokio` and the maintained set around it. Drift is real but manageable with pinned versions and `cargo-deny`.
- *"Solo developer can't move fast in Rust."* — Slower at first; faster later because refactors don't break silently. We accept the early-phase cost.
- *"You'll want a quick prototype in Python."* — Prototyping in Python in this project is forbidden for code that handles invoices or tenant data. Prototypes that touch real data must be written in the same language as production.

## Alternatives considered

- **Go** — weaker type system, error handling by convention, less safety upside per hour of effort.
- **Kotlin/JVM** — JVM in Tauri is heavyweight; FFI to native SDKs is rougher.
- **TypeScript end-to-end** — fine for cloud UI; not for invoice-issuing backend.
- **C# / .NET** — viable, but ecosystem story for embedded DBs and Tauri is weaker.

## Open questions

None at this layer. Async runtime, error-handling crate, logging crate, and CLI
crate are picked in a later "stack baseline" ADR before commit #1.
