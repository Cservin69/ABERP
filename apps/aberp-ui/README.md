# aberp-ui — operator UI shell

PR-9-2 landing. Tauri 2 shell + Svelte 5 SPA over the loopback
HTTPS+JSON wire protocol from PR-9-1.

## Layout

```
apps/aberp-ui/
  Cargo.toml             ← workspace member (Tauri 2 Rust crate)
  build.rs               ← tauri-build entry
  tauri.conf.json        ← Tauri 2 config (frontendDist = ui/dist)
  capabilities/
    default.json         ← Tauri allow-list per ADR-0007 (no fs::all)
  icons/
    icon.png             ← placeholder warm-charcoal square; replace
                            before any `tauri build` production bundle
  src/                   ← Tauri Rust shell
    main.rs              ← entry shim
    lib.rs               ← Tauri Builder + boot orchestration
    handshake.rs         ← stdout handshake parser (F17 contract)
    backend.rs           ← `aberp serve` subprocess lifecycle
    pinned_client.rs     ← reqwest client with SHA-256 fingerprint pin
    commands.rs          ← four #[tauri::command] read-only handlers
  tests/
    handshake_parse.rs   ← round-trip with serve.rs's println format
  ui/                    ← Svelte 5 SPA
    package.json
    tsconfig.json
    vite.config.ts
    svelte.config.js
    index.html
    src/
      main.ts            ← Svelte 5 mount
      App.svelte         ← root component (health probe + invoice list)
      app.d.ts           ← ambient .css module declaration
      lib/
        api.ts           ← Tauri `invoke()` wrappers
        tokens.css       ← ADR-0017 design tokens
      routes/
        InvoiceList.svelte ← first dense-table screen
```

## Quickstart

Prerequisites:

  - Rust toolchain at the workspace MSRV (currently 1.88) — see
    `rust-toolchain.toml`.
  - Node 20+ and npm.
  - The `aberp` backend binary built (`cargo build --bin aberp`) and
    a tenant DuckDB populated with at least one issued invoice; the
    SPA will otherwise render the empty-state row.
  - The OS keychain must already have the session-token entry
    `aberp.nav.<tenant>` / `session_token`. Run
    `aberp serve --tenant <tenant>` once locally to mint it; the
    Tauri shell never mints tokens itself (per A28).

Dev loop:

```sh
# from apps/aberp-ui/
cd ui
npm install
cd ..
cargo run --bin aberp-ui
```

`cargo run` invokes `beforeDevCommand` from `tauri.conf.json`, which
runs `npm --prefix ui run dev` for the Vite dev server. The Tauri
shell launches `aberp serve --tenant default --db ./aberp.duckdb
--port 0` as a subprocess and parses the handshake line off stdout
(see `src/handshake.rs`).

Environment overrides:

  - `ABERP_BIN`  — explicit path to the `aberp` binary (defaults to a
                   sibling next to the running `aberp-ui` binary).
  - `ABERP_TENANT` — tenant identifier (defaults to `default`).
  - `ABERP_DB`    — DuckDB path (defaults to `./aberp.duckdb`).

## What this PR does NOT do

  - No mutation routes — those stay on the CLI until F16 fires.
  - No production bundling (`tauri build`). The icon is a placeholder;
    real artwork + signing configuration is a separate PR.
  - No token rotation — `aberp rotate-session-token` lands when the
    SPA needs it.
  - No fingerprint-pin persistence on the Tauri side. The shell
    re-parses the handshake every launch; the persistence file
    `~/.aberp/serve/<tenant>/loopback.fingerprint.sha256` exists for
    operator inspection, not cross-process re-use.

See the PR-9-2 commit message under `_handoffs/` for the full
decision log.
