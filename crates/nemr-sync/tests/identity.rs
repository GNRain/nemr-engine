//! Identity: registration, recovery confirmation, login, rate limiting.

mod common;

use common::{b64, keys_match, spawn, Enrolled};

#[tokio::test]
async fn register_confirm_login_round_trips_the_master_key() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, mk) = app.enroll(&e).await;
    assert!(!token.is_empty());
    // The key recovered at login must be the one wrapped at registration.
    assert!(
        keys_match(&e.mk, &mk),
        "login must recover the same master key that registration wrapped"
    );
}

#[tokio::test]
async fn an_account_cannot_log_in_before_recovery_is_confirmed() {
    let app = spawn().await;
    let e = Enrolled::new();

    let r = app
        .http
        .post(app.url("/v1/register"))
        .json(&e.register_body())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);

    // Correct credentials, but recovery not yet confirmed: usability is gated.
    let r = app
        .http
        .post(app.url("/v1/login"))
        .json(&serde_json::json!({ "email": e.email, "auth_key": e.auth_key_b64() }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        403,
        "login before recovery-confirm must be forbidden"
    );
}

#[tokio::test]
async fn registration_without_a_recovery_envelope_is_refused() {
    let app = spawn().await;
    let e = Enrolled::new();
    let mut body = e.register_body();
    // Recovery is not deferrable (E-16): a registration missing it is refused.
    body["recovery_envelope"] = serde_json::json!("");
    let r = app
        .http
        .post(app.url("/v1/register"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn a_wrong_recovery_acknowledgement_does_not_activate_the_account() {
    let app = spawn().await;
    let e = Enrolled::new();
    app.http
        .post(app.url("/v1/register"))
        .json(&e.register_body())
        .send()
        .await
        .unwrap();

    // An acknowledgement that does not match the stored one (as a wrong recovery
    // code would produce) must be rejected.
    let wrong = b64(&[0x11u8; 32]);
    let r = app
        .http
        .post(app.url("/v1/recovery/confirm"))
        .json(&serde_json::json!({ "email": e.email, "recovery_ack_hash": wrong }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);

    // And the account is still pending: a correct login is still forbidden.
    let r = app
        .http
        .post(app.url("/v1/login"))
        .json(&serde_json::json!({ "email": e.email, "auth_key": e.auth_key_b64() }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn a_duplicate_email_is_refused() {
    let app = spawn().await;
    let e = Enrolled::new();
    let first = app
        .http
        .post(app.url("/v1/register"))
        .json(&e.register_body())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 201);

    let second = app
        .http
        .post(app.url("/v1/register"))
        .json(&e.register_body())
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
}

#[tokio::test]
async fn a_wrong_password_is_rejected_and_uniform() {
    let app = spawn().await;
    let e = Enrolled::new();
    app.enroll(&e).await;

    let r = app
        .http
        .post(app.url("/v1/login"))
        .json(&serde_json::json!({ "email": e.email, "auth_key": b64(&[0x22u8; 32]) }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn repeated_failures_are_rate_limited() {
    let app = spawn().await;
    let e = Enrolled::new();
    app.enroll(&e).await; // active account; failures below are wrong-password

    // Config for tests allows 3 failures per window.
    for _ in 0..3 {
        let r = app
            .http
            .post(app.url("/v1/login"))
            .json(&serde_json::json!({ "email": e.email, "auth_key": b64(&[0x33u8; 32]) }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
    }

    // The next attempt is throttled before the password is even checked — even
    // the correct password now returns 429.
    let r = app
        .http
        .post(app.url("/v1/login"))
        .json(&serde_json::json!({ "email": e.email, "auth_key": e.auth_key_b64() }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429, "the fourth attempt must be rate limited");
}
