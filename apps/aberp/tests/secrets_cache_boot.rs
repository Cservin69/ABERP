//! Session-149 — load-bearing test for the SMTP-keychain-at-boot fix.
//!
//! The bug PR-111 missed: the SMTP password was read from the OS
//! keychain LAZILY (Settings GET, email send, test connection), so on a
//! freshly-rebuilt binary the macOS ACL re-prompt fired AFTER boot, at
//! a surprising time. The fix reads the password ONCE at boot into the
//! in-process [`SecretsCache`] and serves every post-boot consumer from
//! there.
//!
//! This test proves the invariant: after the boot read, NO consumer
//! touches the keychain READ API. It installs a process-global mock
//! keychain whose `get_password` PANICS once a read-guard is armed —
//! any stray post-boot read blows the test up instead of silently
//! re-prompting in production.
//!
//! It lives in its own integration-test binary (not a lib unit test)
//! because `set_default_credential_builder` mutates process-global
//! state; isolating it to one binary matches the repo convention used
//! by `serve_settings_routes.rs` / `serve_setup_nav_credentials_route.rs`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, Once, OnceLock};

use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence};
use keyring::Error as KeyringError;
use ulid::Ulid;
use zeroize::Zeroizing;

use aberp::email_invoice;
use aberp::secrets_cache::SecretsCache;
use aberp::smtp_credentials;

// ── Shared in-process mock keychain with a post-boot read guard ──────

fn shared_store() -> &'static Mutex<HashMap<(String, String), String>> {
    static STORE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// When `true`, any keychain `get_password` panics — the "no read
/// after boot" fence. WRITES are always permitted (operator-initiated).
fn read_guard_armed() -> &'static Mutex<bool> {
    static ARMED: OnceLock<Mutex<bool>> = OnceLock::new();
    ARMED.get_or_init(|| Mutex::new(false))
}

#[derive(Debug)]
struct GuardedMockCredential {
    service: String,
    account: String,
}

impl CredentialApi for GuardedMockCredential {
    fn set_password(&self, password: &str) -> keyring::Result<()> {
        shared_store().lock().expect("store poisoned").insert(
            (self.service.clone(), self.account.clone()),
            password.to_string(),
        );
        Ok(())
    }

    fn get_password(&self) -> keyring::Result<String> {
        if *read_guard_armed().lock().expect("guard poisoned") {
            panic!(
                "POST-BOOT KEYCHAIN READ: get_password called for service `{}` account `{}` \
                 after the read guard was armed — a consumer is still lazy-loading instead of \
                 using the SecretsCache",
                self.service, self.account
            );
        }
        match shared_store()
            .lock()
            .expect("store poisoned")
            .get(&(self.service.clone(), self.account.clone()))
        {
            Some(p) => Ok(p.clone()),
            None => Err(KeyringError::NoEntry),
        }
    }

    fn delete_password(&self) -> keyring::Result<()> {
        let mut store = shared_store().lock().expect("store poisoned");
        if store
            .remove(&(self.service.clone(), self.account.clone()))
            .is_some()
        {
            Ok(())
        } else {
            Err(KeyringError::NoEntry)
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug)]
struct GuardedMockBuilder;

impl CredentialBuilderApi for GuardedMockBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<Credential>> {
        Ok(Box::new(GuardedMockCredential {
            service: service.to_string(),
            account: user.to_string(),
        }))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::ProcessOnly
    }
}

fn install_mock_keyring() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        keyring::set_default_credential_builder(Box::new(GuardedMockBuilder));
    });
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("aberp-secrets-cache")
        .join(format!("{label}-{}", Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_smtp_seller_toml(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("seller.toml");
    let body = r#"[seller]
legal_name = "ABERP Supplier Kft."
tax_number = "12345678-1-42"

[seller.smtp]
host = "smtp.example.com"
port = 587
from_address = "billing@example.com"
username = "billing@example.com"
security = "StartTls"
attach_xml = false
"#;
    std::fs::write(&path, body).expect("write seller.toml");
    path
}

/// The whole Part-E suite is ONE test function so it runs on a single
/// thread within this binary — the read-guard and mock store are
/// process-global, so splitting into parallel `#[test]`s would let one
/// phase's armed guard panic another phase's legitimate boot read.
#[test]
fn keychain_read_only_at_boot() {
    install_mock_keyring();

    // ── Phase 1 — boot reads into cache; post-boot reads are fenced ──
    {
        let dir = temp_dir("no-read-after-boot");
        let seller_toml = write_smtp_seller_toml(&dir);
        let tenant = format!("cache_test_{}", Ulid::new());

        // Seed the keychain (as if configured in a prior session).
        smtp_credentials::write_password(&tenant, "s3cr3t-boot-pw")
            .expect("seed SMTP password for boot read");

        // Boot read → cache populated (guard NOT yet armed).
        let cache = SecretsCache::init_at_boot(&tenant, &seller_toml);
        assert!(
            cache.is_smtp_password_set(),
            "boot should cache the password"
        );

        // Arm the fence: ANY keychain read from here on panics.
        *read_guard_armed().lock().expect("guard poisoned") = true;

        // Post-boot consumers — all answer from cache, none read keychain.
        assert!(cache.is_smtp_password_set(), "is_set served from cache");
        assert_eq!(
            cache.smtp_password().as_deref().map(|z| z.as_str()),
            Some("s3cr3t-boot-pw"),
            "password served from cache"
        );

        // The real previously-keychain-reading helper now takes the
        // cached password and only reads the on-disk TOML config.
        let (cfg, pw) = email_invoice::load_smtp_credentials(cache.smtp_password(), &seller_toml)
            .expect("load_smtp_credentials from cache + TOML");
        assert_eq!(cfg.host, "smtp.example.com");
        assert_eq!(pw.as_str(), "s3cr3t-boot-pw");

        // Operator-initiated rotation: WRITE allowed even with the guard
        // armed; the cache refresh serves the new value with no read.
        smtp_credentials::write_password(&tenant, "rotated-pw").expect("rotate write");
        cache.refresh_smtp_password_after_write(Zeroizing::new("rotated-pw".to_string()));
        assert_eq!(
            cache.smtp_password().as_deref().map(|z| z.as_str()),
            Some("rotated-pw"),
            "cache serves rotated password without a keychain read"
        );

        *read_guard_armed().lock().expect("guard poisoned") = false;
    }

    // ── Phase 2 — `[seller.smtp]` absent: boot reads NOTHING ─────────
    {
        let dir = temp_dir("unconfigured");
        let path = dir.join("seller.toml");
        std::fs::write(
            &path,
            "[seller]\nlegal_name = \"X\"\ntax_number = \"12345678-1-42\"\n",
        )
        .expect("write seller.toml without smtp section");
        let tenant = format!("cache_test_{}", Ulid::new());

        // Guard armed BEFORE init proves the unconfigured branch never reads.
        *read_guard_armed().lock().expect("guard poisoned") = true;
        let cache = SecretsCache::init_at_boot(&tenant, &path);
        *read_guard_armed().lock().expect("guard poisoned") = false;

        assert!(!cache.is_smtp_password_set());
        assert!(cache.smtp_password().is_none());
    }

    // ── Phase 3 — configured but keychain item missing: no hard-fail ─
    {
        let dir = temp_dir("missing-item");
        let seller_toml = write_smtp_seller_toml(&dir);
        let tenant = format!("cache_test_missing_{}", Ulid::new());

        let cache = SecretsCache::init_at_boot(&tenant, &seller_toml);
        assert!(
            !cache.is_smtp_password_set(),
            "missing keychain item leaves the slot empty, not a panic"
        );

        // The send path maps the empty cache to SmtpPasswordMissing.
        match email_invoice::load_smtp_credentials(cache.smtp_password(), &seller_toml) {
            Err(email_invoice::EmailSendError::SmtpPasswordMissing) => {}
            other => panic!("expected SmtpPasswordMissing, got {other:?}"),
        }
    }
}
