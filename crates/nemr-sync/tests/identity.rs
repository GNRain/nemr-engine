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

/// An ACTIVE account's email is taken: a second registration is refused and
/// the account is untouched.
#[tokio::test]
async fn a_duplicate_email_is_refused_once_the_account_is_active() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (_token, _) = app.enroll(&e).await;

    let mut again = Enrolled::new();
    again.email = e.email.clone();
    let second = app
        .http
        .post(app.url("/v1/register"))
        .json(&again.register_body())
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
    // The original still logs in — nothing about it was replaced.
    let (_token, mk) = app.login(&e).await;
    assert!(
        common::keys_match(&mk, &e.mk),
        "the active account's key survived"
    );
}

/// A registration that never confirmed its recovery holds nothing, and a
/// second registration with the same email REPLACES it — the abandoned first
/// attempt (a closed terminal, a reloaded page) must not take the email with
/// it. The first attempt's recovery code no longer confirms anything; the
/// second's does, and the second's password is the one that logs in.
#[tokio::test]
async fn re_registering_an_unconfirmed_email_replaces_the_pending_account() {
    let app = spawn().await;
    let first = Enrolled::new();
    let r = app
        .http
        .post(app.url("/v1/register"))
        .json(&first.register_body())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    // Abandoned here: no confirmation.

    let mut second = Enrolled::new();
    second.email = first.email.clone();
    let r = app
        .http
        .post(app.url("/v1/register"))
        .json(&second.register_body())
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        201,
        "a pending email is free to register again: {}",
        r.text().await.unwrap()
    );

    // The FIRST attempt's code is dead: its acknowledgement does not activate.
    let stale = app
        .http
        .post(app.url("/v1/recovery/confirm"))
        .json(&serde_json::json!({
            "email": first.email,
            "recovery_ack_hash": first.recovery_confirm_ack_b64(&first.recovery_code),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        stale.status(),
        401,
        "the replaced registration's code must not confirm"
    );

    // The second's does, and its password logs in with its key.
    let ok = app
        .http
        .post(app.url("/v1/recovery/confirm"))
        .json(&serde_json::json!({
            "email": second.email,
            "recovery_ack_hash": second.recovery_confirm_ack_b64(&second.recovery_code),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let (_token, mk) = app.login(&second).await;
    assert!(common::keys_match(&mk, &second.mk));
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

#[tokio::test]
async fn logout_revokes_the_token_server_side() {
    // "Log out" that merely deletes a local file leaves a live token on the
    // server for its whole TTL. Logout must revoke it.
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, _mk) = app.enroll(&e).await;

    // Control: the token works before logout.
    let r = app
        .http
        .get(app.url("/v1/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "token must work before logout");

    let r = app
        .http
        .post(app.url("/v1/logout"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "logout: {}", r.text().await.unwrap());

    // The same token is now dead.
    let r = app
        .http
        .get(app.url("/v1/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "a revoked token must not authenticate");

    // Logout again with the dead token: idempotent, not an oracle.
    let r = app
        .http
        .post(app.url("/v1/logout"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "logout is idempotent");
}

async fn params_for(app: &common::TestApp, email: &str) -> (u16, serde_json::Value) {
    let r = app
        .http
        .post(app.url("/v1/auth/params"))
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
async fn kdf_params_does_not_reveal_whether_an_email_exists() {
    // F-89: the pre-login KDF-params endpoint is unauthenticated. A known and an
    // unknown email must be indistinguishable, or it is an enumeration oracle.
    let app = spawn().await;
    let e = Enrolled::new();
    app.enroll(&e).await;

    let (kstatus, kbody) = params_for(&app, &e.email).await;
    assert_eq!(kstatus, 200);
    assert_eq!(
        common::unb64(kbody["kdf_salt"].as_str().unwrap()).len(),
        16,
        "a real account returns a 16-byte salt"
    );

    let unknown = "definitely-not-registered@example.com";
    let (ustatus, ubody) = params_for(&app, unknown).await;
    // The load-bearing assertion: same status (not a 404), so status does not
    // leak existence. This goes red if the endpoint 404s an unknown email.
    assert_eq!(
        ustatus, 200,
        "an unknown email must not be distinguishable by status code"
    );
    assert_eq!(
        common::unb64(ubody["kdf_salt"].as_str().unwrap()).len(),
        16,
        "the pseudo-salt must have the same shape as a real salt"
    );
    assert!(ubody["kdf_m_cost"].is_number());

    // Deterministic, like a real account's stable salt: two probes match.
    let (_, ubody2) = params_for(&app, unknown).await;
    assert_eq!(
        ubody2["kdf_salt"], ubody["kdf_salt"],
        "the pseudo-salt must be stable across probes"
    );
}
