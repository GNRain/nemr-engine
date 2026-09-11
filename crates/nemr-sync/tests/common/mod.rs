//! Integration-test harness: a real server on a random port against a real
//! Postgres, driven by a client that performs the actual E-16 crypto flow.
//!
//! These tests require `DATABASE_URL` to point at a Postgres. They are NOT
//! `#[ignore]`d and do not skip: without a database they fail loudly, the same
//! discipline as the host-requiring regression suite. CI provides Postgres via a
//! service container; locally, run one (e.g. `podman run -p 127.0.0.1:5433:5432
//! -e POSTGRES_PASSWORD=nemr postgres:16`) and export DATABASE_URL.

#![allow(dead_code)]

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nemr_crypto::{
    decrypt_bundle, derive_root, encrypt_bundle, recovery_acknowledgement, Envelope, KdfParams,
    MasterKey, RecoveryCode,
};
use nemr_storage::local::LocalStore;
use nemr_sync::{connect_and_migrate, router, AppState, Config, DynStore, KdfCost};
use sqlx::PgPool;
use time::Duration;

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn unb64(s: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(s).expect("valid base64")
}

/// Cheap KDF cost so the suite is fast. Production uses the OWASP defaults.
pub fn test_params() -> KdfParams {
    KdfParams {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    }
}

pub struct TestApp {
    pub base: String,
    pub http: reqwest::Client,
    pub pool: PgPool,
    _tmp: tempfile::TempDir,
}

pub async fn spawn() -> TestApp {
    // The environment first, then the server's own settings file — the same
    // order the server uses, so a machine that can run a server can run its
    // tests with nothing exported.
    let db = nemr_sync::settings::setting("DATABASE_URL")
        .unwrap_or_else(|e| panic!("{e}\n  (scripts/setup_sync_test_db.sh starts one)"));
    let pool = connect_and_migrate(&db).await.expect("connect + migrate");

    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn DynStore> = Arc::new(LocalStore::new(tmp.path()));

    let config = Config {
        token_ttl: Duration::days(1),
        lease_ttl: Duration::seconds(2),
        server_kdf: KdfCost {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        },
        max_login_failures: 3,
        login_window: Duration::minutes(15),
        bundle_prefix: "test-bundles".into(),
        // Fixed pepper so the F-89 pseudo-salt is deterministic within a test.
        auth_pepper: [7u8; 32],
    };

    let state = AppState {
        pool: pool.clone(),
        store,
        config,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });

    TestApp {
        base: format!("http://{addr}"),
        http: reqwest::Client::new(),
        pool,
        _tmp: tmp,
    }
}

/// A client's local key material, mirroring what a real client holds.
pub struct Enrolled {
    pub email: String,
    pub password: String,
    pub salt: [u8; 16],
    pub params: KdfParams,
    pub mk: MasterKey,
    pub recovery_code: String,
    pub recovery_salt: [u8; 16],
    pub recovery_envelope: Vec<u8>,
}

impl Enrolled {
    /// Build fresh crypto material for a new unique account (client-side only;
    /// nothing sent yet).
    pub fn new() -> Self {
        let params = test_params();
        let salt: [u8; 16] = rand::random();
        let root = derive_root(b"correct horse battery staple", &salt, params).unwrap();
        let mk = MasterKey::generate();
        let _password_envelope = root.wrap_key().seal(&mk);

        let recovery_code = RecoveryCode::generate();
        let recovery_salt: [u8; 16] = rand::random();
        let rroot = derive_root(recovery_code.as_secret(), &recovery_salt, params).unwrap();
        let recovery_envelope = rroot.recovery_wrap_key().seal(&mk).to_bytes();

        let n: u64 = rand::random();
        Enrolled {
            email: format!("u{n:x}@example.com"),
            password: "correct horse battery staple".into(),
            salt,
            params,
            mk,
            recovery_code: recovery_code.display(),
            recovery_salt,
            recovery_envelope,
        }
    }

    fn root(&self) -> nemr_crypto::RootKey {
        derive_root(self.password.as_bytes(), &self.salt, self.params).unwrap()
    }

    pub fn auth_key_b64(&self) -> String {
        b64(self.root().auth_key().as_bytes())
    }

    fn password_envelope_b64(&self) -> String {
        b64(&self.root().wrap_key().seal(&self.mk).to_bytes())
    }

    fn recovery_ack_b64(&self) -> String {
        b64(&recovery_acknowledgement(&self.mk))
    }

    pub fn register_body(&self) -> serde_json::Value {
        serde_json::json!({
            "email": self.email,
            "kdf_salt": b64(&self.salt),
            "kdf_m_cost": self.params.m_cost,
            "kdf_t_cost": self.params.t_cost,
            "kdf_p_cost": self.params.p_cost,
            "auth_key": self.auth_key_b64(),
            "password_envelope": self.password_envelope_b64(),
            "recovery_salt": b64(&self.recovery_salt),
            "recovery_m_cost": self.params.m_cost,
            "recovery_t_cost": self.params.t_cost,
            "recovery_p_cost": self.params.p_cost,
            "recovery_envelope": b64(&self.recovery_envelope),
            "recovery_ack_hash": self.recovery_ack_b64(),
        })
    }

    /// Recover the master key through the recovery envelope with the re-entered
    /// code, then compute the acknowledgement — exactly what a client does to
    /// confirm recovery works before the account is usable.
    pub fn recovery_confirm_ack_b64(&self, code: &str) -> String {
        let parsed = RecoveryCode::parse(code).unwrap();
        let rroot = derive_root(parsed.as_secret(), &self.recovery_salt, self.params).unwrap();
        let env = Envelope::from_bytes(&self.recovery_envelope).unwrap();
        let mk = rroot.recovery_wrap_key().open(&env).unwrap();
        b64(&recovery_acknowledgement(&mk))
    }
}

impl TestApp {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Full enrolment: register, confirm recovery, log in. Returns the bearer
    /// token and the recovered master key (proving login round-trips the key).
    pub async fn enroll(&self, e: &Enrolled) -> (String, MasterKey) {
        let r = self
            .http
            .post(self.url("/v1/register"))
            .json(&e.register_body())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201, "register: {}", r.text().await.unwrap());

        let ack = e.recovery_confirm_ack_b64(&e.recovery_code);
        let r = self
            .http
            .post(self.url("/v1/recovery/confirm"))
            .json(&serde_json::json!({ "email": e.email, "recovery_ack_hash": ack }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "confirm: {}", r.text().await.unwrap());

        self.login(e).await
    }

    pub async fn login(&self, e: &Enrolled) -> (String, MasterKey) {
        let r = self
            .http
            .post(self.url("/v1/login"))
            .json(&serde_json::json!({ "email": e.email, "auth_key": e.auth_key_b64() }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "login: {}", r.text().await.unwrap());
        let body: serde_json::Value = r.json().await.unwrap();
        let token = body["token"].as_str().unwrap().to_string();
        let env =
            Envelope::from_bytes(&unb64(body["password_envelope"].as_str().unwrap())).unwrap();
        let mk = e.root().wrap_key().open(&env).unwrap();
        (token, mk)
    }
}

/// Prove two master keys are equal by exercising them, since MasterKey exposes
/// no bytes: a bundle encrypted under one decrypts under the other.
pub fn keys_match(a: &MasterKey, b: &MasterKey) -> bool {
    let ct = encrypt_bundle(a, b"probe");
    decrypt_bundle(b, &ct)
        .map(|p| p == b"probe")
        .unwrap_or(false)
}
