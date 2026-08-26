//! The D-03 lease: two clients racing one session, takeover, and the losing
//! client refused writes — server-side, not merely by its own good behaviour.

mod common;

use common::{spawn, Enrolled};
use nemr_crypto::encrypt_bundle;
use serde_json::Value;

async fn create_session(app: &common::TestApp, token: &str, name: &str) {
    app.http
        .post(app.url("/v1/sessions"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "agent": "claude" }))
        .send()
        .await
        .unwrap();
}

async fn acquire(app: &common::TestApp, token: &str, name: &str, holder: &str) -> Value {
    app.http
        .post(app.url(&format!("/v1/sessions/{name}/lease")))
        .bearer_auth(token)
        .json(&serde_json::json!({ "holder": holder }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn heartbeat_status(
    app: &common::TestApp,
    token: &str,
    name: &str,
    holder: &str,
    fence: i64,
) -> u16 {
    app.http
        .post(app.url(&format!("/v1/sessions/{name}/lease/heartbeat")))
        .bearer_auth(token)
        .json(&serde_json::json!({ "holder": holder, "fence": fence }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn upload_status(
    app: &common::TestApp,
    token: &str,
    name: &str,
    holder: &str,
    fence: i64,
    body: Vec<u8>,
) -> u16 {
    app.http
        .put(app.url(&format!("/v1/sessions/{name}/bundle")))
        .bearer_auth(token)
        .header("x-nemr-lease-holder", holder)
        .header("x-nemr-lease-fence", fence.to_string())
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn release_status(
    app: &common::TestApp,
    token: &str,
    name: &str,
    holder: &str,
    fence: i64,
) -> u16 {
    app.http
        .post(app.url(&format!("/v1/sessions/{name}/lease/release")))
        .bearer_auth(token)
        .json(&serde_json::json!({ "holder": holder, "fence": fence }))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn release_frees_the_lease_without_waiting_for_the_ttl() {
    // A clean stop releases the lease explicitly (WP-K), so another machine can
    // acquire immediately rather than waiting out the TTL.
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, _mk) = app.enroll(&e).await;
    create_session(&app, &token, "proj").await;

    let a = acquire(&app, &token, "proj", "machine-A").await;
    assert_eq!(a["granted"], true);
    let fence_a = a["fence"].as_i64().unwrap();

    // While held, B cannot acquire (control: release below is what frees it,
    // not the lease never having been held).
    let b = acquire(&app, &token, "proj", "machine-B").await;
    assert_eq!(
        b["granted"], false,
        "held lease must block B before release"
    );

    assert_eq!(
        release_status(&app, &token, "proj", "machine-A", fence_a).await,
        200
    );

    // A retried release — the response was lost, the client sends it again
    // before anyone else acquires — is idempotent success, not an error.
    assert_eq!(
        release_status(&app, &token, "proj", "machine-A", fence_a).await,
        200,
        "a retried clean release is idempotent"
    );

    // No TTL wait: B acquires immediately, outright, no takeover.
    let b = acquire(&app, &token, "proj", "machine-B").await;
    assert_eq!(b["granted"], true, "a released lease is free immediately");

    // The fence stays monotonic ACROSS a release: if release reset it, a zombie
    // holder from the earlier hold could match the fresh lease's credentials.
    let fence_b = b["fence"].as_i64().unwrap();
    assert!(
        fence_b > fence_a,
        "the fence must advance across release ({fence_b} vs {fence_a}), \
         or a stale holder's credentials could come back to life"
    );

    // Once B holds the lease, A's old fence can no longer release anything.
    assert_eq!(
        release_status(&app, &token, "proj", "machine-A", fence_a).await,
        409,
        "A's stale release must not free B's live lease"
    );
    assert_eq!(
        heartbeat_status(&app, &token, "proj", "machine-B", fence_b).await,
        200,
        "B's lease is undisturbed by A's refused release"
    );
}

#[tokio::test]
async fn a_stale_fence_cannot_release_someone_elses_lease() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, _mk) = app.enroll(&e).await;
    create_session(&app, &token, "proj").await;

    let a = acquire(&app, &token, "proj", "machine-A").await;
    let fence_a = a["fence"].as_i64().unwrap();

    // B takes over; A's fence is now stale.
    let t: Value = app
        .http
        .post(app.url("/v1/sessions/proj/lease/takeover"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "holder": "machine-B" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fence_b = t["fence"].as_i64().unwrap();

    // A's release with the stale fence must be refused — releasing a lease you
    // lost would let a stale client free the *winner's* lease.
    assert_eq!(
        release_status(&app, &token, "proj", "machine-A", fence_a).await,
        409,
        "a stale fence must not release the current holder's lease"
    );

    // B is undisturbed.
    assert_eq!(
        heartbeat_status(&app, &token, "proj", "machine-B", fence_b).await,
        200
    );
}

#[tokio::test]
async fn takeover_locks_the_loser_out_of_writing() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, mk) = app.enroll(&e).await;
    create_session(&app, &token, "proj").await;

    // Machine A takes the lease.
    let a = acquire(&app, &token, "proj", "machine-A").await;
    assert_eq!(a["granted"], true);
    let fence_a = a["fence"].as_i64().unwrap();

    // Machine B sees it held — not granted — and learns who holds it.
    let b = acquire(&app, &token, "proj", "machine-B").await;
    assert_eq!(b["granted"], false, "the lease is held; B must not get it");
    assert_eq!(b["holder"], "machine-A");

    // B explicitly takes over. The fence advances.
    let t = app
        .http
        .post(app.url("/v1/sessions/proj/lease/takeover"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "holder": "machine-B" }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(t["granted"], true);
    let fence_b = t["fence"].as_i64().unwrap();
    assert!(fence_b > fence_a, "takeover must advance the fence");

    // A discovers on its next heartbeat that it lost the lease.
    assert_eq!(
        heartbeat_status(&app, &token, "proj", "machine-A", fence_a).await,
        409,
        "the losing client's heartbeat must fail"
    );

    // And — the part that matters — A cannot write. The server refuses the
    // stale-fence upload; it does not rely on A choosing to stop.
    let ct = encrypt_bundle(&mk, b"stale writer's data");
    assert_eq!(
        upload_status(&app, &token, "proj", "machine-A", fence_a, ct).await,
        409,
        "the losing client's write must be refused by the server"
    );

    // B, holding the current fence, can write.
    let ct = encrypt_bundle(&mk, b"new holder's data");
    assert_eq!(
        upload_status(&app, &token, "proj", "machine-B", fence_b, ct).await,
        200,
        "the current holder must be able to write"
    );
}

#[tokio::test]
async fn an_expired_lease_is_reacquired_and_the_stale_holder_is_locked_out() {
    // The silently-restarted-daemon case: a holder that stops heartbeating past
    // the TTL loses the lease to whoever acquires next, and its own stale fence
    // no longer writes — it must re-acquire, and re-acquire is denied if someone
    // else took the gap.
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, mk) = app.enroll(&e).await;
    create_session(&app, &token, "proj").await;

    let a = acquire(&app, &token, "proj", "machine-A").await;
    let fence_a = a["fence"].as_i64().unwrap();

    // Let the 2s test TTL elapse without a heartbeat.
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

    // B acquires the now-expired lease outright (no takeover needed).
    let b = acquire(&app, &token, "proj", "machine-B").await;
    assert_eq!(b["granted"], true, "an expired lease is free to acquire");
    let fence_b = b["fence"].as_i64().unwrap();

    // A's stale fence cannot write, and A's heartbeat fails.
    assert_eq!(
        heartbeat_status(&app, &token, "proj", "machine-A", fence_a).await,
        409
    );
    let ct = encrypt_bundle(&mk, b"data");
    assert_eq!(
        upload_status(&app, &token, "proj", "machine-A", fence_a, ct).await,
        409
    );

    // B writes fine.
    let ct = encrypt_bundle(&mk, b"data");
    assert_eq!(
        upload_status(&app, &token, "proj", "machine-B", fence_b, ct).await,
        200
    );
}
