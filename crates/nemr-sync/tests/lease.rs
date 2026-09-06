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

/// A fenced-out write is refused **and publishes nothing** — the stored bundle
/// and its recorded digest are exactly what the rightful holder left.
///
/// **What this test does and does not prove.** It proves the outcome: after a
/// takeover, the loser's upload is refused and the good bundle still stands. It
/// does NOT isolate the in-`UPDATE` fence re-check added for F-92, because the
/// pre-flight `require_held` refuses the stale fence first and the two use
/// identical conditions — removing the in-`UPDATE` guard leaves this test green
/// (verified). That guard closes a genuine TOCTOU (a takeover landing between
/// the pre-check and the write, a window a long body transfer widens), but
/// triggering it from outside would need the server to pause mid-request, and a
/// test-only hook in the request path is a worse thing to own than an unproven
/// line of defence-in-depth. Recorded as a known coverage gap rather than
/// claimed as a guard — see F-92 in docs/CONFORMANCE.md.
#[tokio::test]
async fn a_fenced_out_write_is_refused_and_publishes_nothing() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, mk) = app.enroll(&e).await;
    create_session(&app, &token, "proj").await;

    // A holds and publishes a first bundle: this is the content that must
    // survive.
    let a = acquire(&app, &token, "proj", "machine-A").await;
    let fence_a = a["fence"].as_i64().unwrap();
    let good = encrypt_bundle(&mk, b"the content that must survive");
    assert_eq!(
        upload_status(&app, &token, "proj", "machine-A", fence_a, good).await,
        200
    );
    let published_before: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as("SELECT ciphertext_sha256 FROM sessions WHERE name = 'proj'")
            .fetch_optional(&app.pool)
            .await
            .unwrap();
    let digest_before = published_before.unwrap().0;

    // B takes over. A's fence is now stale — but imagine A's upload was already
    // in flight, having passed its pre-check a moment earlier.
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
    assert_eq!(t["granted"], true);

    // A's write with the stale fence must be refused AND must not publish.
    let stale = encrypt_bundle(&mk, b"the stale machine's divergent work");
    assert_eq!(
        upload_status(&app, &token, "proj", "machine-A", fence_a, stale).await,
        409,
        "a fenced-out write must be refused"
    );

    let published_after: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as("SELECT ciphertext_sha256 FROM sessions WHERE name = 'proj'")
            .fetch_optional(&app.pool)
            .await
            .unwrap();
    assert_eq!(
        published_after.unwrap().0,
        digest_before,
        "the refused write must not have published its bundle over the good one"
    );
}

/// The session list says who holds the lease — "open on <machine>", the state
/// D-03's takeover UX is built around — and stops saying so once it is
/// released. Read from the same endpoint the UI's list reads.
#[tokio::test]
async fn the_session_list_names_the_live_lease_holder() {
    let app = spawn().await;
    let (token, _) = app.enroll(&Enrolled::new()).await;
    create_session(&app, &token, "proj").await;

    let list = |app: &common::TestApp, token: &str| {
        let http = app.http.clone();
        let url = app.url("/v1/sessions");
        let token = token.to_string();
        async move {
            http.get(url)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json::<Vec<Value>>()
                .await
                .unwrap()
        }
    };
    let before = list(&app, &token).await;
    assert_eq!(
        before[0]["held_by"],
        Value::Null,
        "nobody holds a fresh session"
    );

    let lease = acquire(&app, &token, "proj", "laptop").await;
    assert_eq!(lease["granted"], true);
    let during = list(&app, &token).await;
    assert_eq!(during[0]["held_by"], "laptop", "{during:?}");
    assert!(during[0]["lease_expires_at_unix"].as_i64().unwrap() > 0);

    let released = release_status(
        &app,
        &token,
        "proj",
        "laptop",
        lease["fence"].as_i64().unwrap(),
    )
    .await;
    assert!((200..300).contains(&released), "release: {released}");
    let after = list(&app, &token).await;
    assert_eq!(
        after[0]["held_by"],
        Value::Null,
        "a released lease is nobody: {after:?}"
    );
}
