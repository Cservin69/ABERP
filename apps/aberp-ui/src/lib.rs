//! Library face of the ABERP Tauri shell (PR-9-2).
//!
//! # What this PR lands
//!
//! - Launches `aberp serve` as a child subprocess on Tauri startup
//!   (`backend::spawn`) per F17 resolution = option 1: parse the
//!   handshake line on stdout, not a persisted port file. The line
//!   shape is locked by `apps/aberp/src/serve.rs` at the `println!`
//!   call site:
//!
//!     `aberp serve: https://127.0.0.1:<port>/ (fingerprint sha256:<hex>)`
//!
//!   The parser in `handshake` rejects anything else loudly; a silent
//!   drift in the format is exactly the CLAUDE.md rule 12 failure
//!   mode.
//!
//! - Builds a `reqwest::Client` with a pin-only TLS trust store: a
//!   custom `rustls::client::danger::ServerCertVerifier` that accepts
//!   the connection iff `SHA-256(leaf cert DER)` equals the
//!   fingerprint parsed off stdout. Per `feedback_reqwest_trust_store`,
//!   the bare `rustls::ClientConfig` is handed to reqwest via
//!   `use_preconfigured_tls`; no `add_root_certificate` builder helper
//!   (those merge with webpki defaults).
//!
//! - Reads the bearer session token from the OS keychain (service
//!   `aberp.nav.<tenant>`, account `session_token`). The Tauri shell
//!   does NOT mint tokens; minting is owned by `aberp serve`'s
//!   `load_or_create_session_token` per A28.
//!
//! - Exposes four `#[tauri::command]` handlers to the Svelte SPA:
//!   `health`, `list_invoices`, `get_invoice`, `get_audit`. Each
//!   forwards to the loopback listener with the bearer header
//!   attached. The SPA never sees the raw URL, the fingerprint, or
//!   the token — capability boundary per ADR-0007 §Tauri allow-list.
//!
//! # What this PR does NOT do
//!
//! - No mutation commands. Mutations stay on the CLI per A29 until
//!   the F16 entropy trigger fires.
//! - No token rotation. One token per tenant until the SPA asks for
//!   one.
//! - No fingerprint-pin persistence on the Tauri side. The shell
//!   verifies against the freshly-parsed fingerprint every launch;
//!   the persistence file (`loopback.fingerprint.sha256`) lives next
//!   to the cert in `~/.aberp/serve/<tenant>/` and exists for
//!   inspection, not for re-use across processes.
//! - No production bundling work. `tauri build` requires real icons
//!   + signing config and is out of scope.

#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tauri::Manager;
use tokio::sync::Mutex;

pub mod backend;
pub mod commands;
pub mod handshake;
pub mod pinned_client;

use backend::BackendHandle;

/// Process-wide state passed to every `#[tauri::command]`.
///
/// `Arc<Mutex<Option<...>>>` shape because the backend is launched
/// asynchronously in `setup` — commands invoked before `setup`
/// completes loud-fail (per rule 12) rather than block.
pub struct AppState {
    pub backend: Arc<Mutex<Option<BackendHandle>>>,
}

/// The single Tauri entry point. Invoked from `main.rs` and from the
/// integration tests (`tests/handshake_parse.rs` does not invoke this
/// — it tests the parser directly; `run()` itself is exercised only
/// at the binary level).
pub fn run() {
    init_tracing();
    install_rustls_crypto_provider();

    let state = AppState {
        backend: Arc::new(Mutex::new(None)),
    };

    tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            let handle = app.handle().clone();
            // Spawn the backend on the Tauri-owned tokio runtime. If the
            // spawn fails we surface it loudly via tracing and exit
            // non-zero: a UI that comes up without a backend would be
            // worse than no UI at all (rule 12).
            tauri::async_runtime::spawn(async move {
                if let Err(e) = boot_backend(&handle).await {
                    tracing::error!(error = %format!("{e:#}"), "backend boot failed — exiting");
                    handle.exit(1);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::health,
            commands::list_invoices,
            commands::get_invoice,
            commands::get_audit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Read the tenant identifier from `ABERP_TENANT`, defaulting to
/// `"default"` — matches every other CLI subcommand's default.
fn read_tenant_env() -> String {
    std::env::var("ABERP_TENANT").unwrap_or_else(|_| "default".to_string())
}

/// Resolve the `aberp` binary path. Three sources, in order:
///   1. `ABERP_BIN` environment variable (operator-explicit).
///   2. Sibling `aberp` (release) next to the running shell binary.
///   3. Sibling `aberp` (debug) — the dev `cargo run` workflow.
///
/// Loud-fails per rule 12 if none of those resolve to an existing
/// file; a Tauri shell that silently falls back to "type 'aberp' and
/// hope the user's PATH has it" is the exact failure mode CLAUDE.md
/// rule 12 names.
fn resolve_aberp_binary() -> Result<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("ABERP_BIN") {
        let p = std::path::PathBuf::from(explicit);
        if p.is_file() {
            return Ok(p);
        }
        return Err(anyhow!(
            "ABERP_BIN points at `{}` but no file exists there",
            p.display()
        ));
    }
    let shell_path = std::env::current_exe().context("read current_exe path")?;
    let shell_dir = shell_path
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent dir"))?;
    let suffix = std::env::consts::EXE_SUFFIX;
    let candidate = shell_dir.join(format!("aberp{suffix}"));
    if candidate.is_file() {
        return Ok(candidate);
    }
    Err(anyhow!(
        "could not locate aberp binary — set ABERP_BIN or place it next to the shell at {}",
        shell_dir.display()
    ))
}

/// Boot the backend: spawn subprocess, parse handshake, load token,
/// build pinned client, store the handle in `AppState`.
async fn boot_backend(handle: &tauri::AppHandle) -> Result<()> {
    let tenant = read_tenant_env();
    let aberp_bin = resolve_aberp_binary()?;
    let db_path = std::env::var("ABERP_DB").unwrap_or_else(|_| "./aberp.duckdb".to_string());

    let started = backend::spawn(&aberp_bin, &tenant, &db_path)
        .await
        .context("spawn aberp serve subprocess")?;
    tracing::info!(
        url = %started.url,
        fingerprint = %started.fingerprint_hex,
        tenant = %tenant,
        "aberp serve handshake parsed"
    );

    let token = load_session_token(&tenant).context("load session token from OS keychain")?;
    let client =
        pinned_client::build(&started.fingerprint_hex).context("build pinned reqwest client")?;
    let backend = BackendHandle::new(started, token, client, tenant);

    let state = handle.state::<AppState>();
    *state.backend.lock().await = Some(backend);
    tracing::info!("backend ready — Tauri commands are live");
    Ok(())
}

/// Look up the session token in the OS keychain — mirrors
/// `apps/aberp/src/serve.rs::load_or_create_session_token` minus the
/// minting branch. The Tauri shell never mints; if the entry is
/// absent we loud-fail and ask the operator to run `aberp serve`
/// once first (which mints the entry as a side effect).
fn load_session_token(tenant: &str) -> Result<String> {
    let service = format!("aberp.nav.{tenant}");
    let entry = keyring::Entry::new(&service, "session_token")
        .context("build keyring::Entry for session_token")?;
    match entry.get_password() {
        Ok(t) if !t.is_empty() => Ok(t),
        Ok(_) => Err(anyhow!(
            "OS keychain entry `{service}` / `session_token` is empty — run `aberp serve --tenant {tenant}` once to mint it"
        )),
        Err(keyring::Error::NoEntry) => Err(anyhow!(
            "OS keychain has no `{service}` / `session_token` entry — run `aberp serve --tenant {tenant}` once to mint it"
        )),
        Err(e) => Err(anyhow!("OS keychain access failed: {e}")),
    }
}

/// rustls 0.23 requires a process-wide crypto provider before any TLS
/// work. Matches `apps/aberp/src/main.rs::install_rustls_crypto_provider`
/// — same try-install discipline (no panic if a transitive crate
/// already installed one).
fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
