//! Session index and whole-bundle upload/download with client-side encryption.

mod common;

use common::{spawn, Enrolled};
use nemr_crypto::{decrypt_bundle, encrypt_bundle};

#[tokio::test]
async fn the_session_index_lists_only_the_owner_and_needs_auth() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, _mk) = app.enroll(&e).await;

    // No token → unauthorized.
    let r = app.http.get(app.url("/v1/sessions")).send().await.unwrap();
    assert_eq!(r.status(), 401);

    // Create two index entries.
    for name in ["alpha", "beta"] {
        let r = app
            .http
            .post(app.url("/v1/sessions"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "name": name, "agent": "claude",
                "size_bytes": 2_000_000_000i64,
                "description": "a project", "base_image_version": "0.1.0",
                "last_machine": "desktop"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "upsert: {}", r.text().await.unwrap());
    }

    let list: serde_json::Value = app
        .http
        .get(app.url("/v1/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    // Enough fields to draw the list.
    let a = &arr[0];
    assert!(a["name"].is_string());
    assert_eq!(a["agent"], "claude");
    assert_eq!(a["size_bytes"], 2_000_000_000i64);
    assert_eq!(a["base_image_version"], "0.1.0");
    assert_eq!(a["last_machine"], "desktop");
    assert_eq!(a["has_bundle"], false);

    // A different account sees none of them.
    let other = Enrolled::new();
    let (other_token, _) = app.enroll(&other).await;
    let list2: serde_json::Value = app
        .http
        .get(app.url("/v1/sessions"))
        .bearer_auth(&other_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list2.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_bundle_uploads_encrypted_and_downloads_byte_identically() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, mk) = app.enroll(&e).await;

    // Index entry.
    app.http
        .post(app.url("/v1/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "proj", "agent": "claude" }))
        .send()
        .await
        .unwrap();

    // Hold the lease before writing.
    let lease: serde_json::Value = app
        .http
        .post(app.url("/v1/sessions/proj/lease"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "holder": "machine-A" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lease["granted"], true);
    let fence = lease["fence"].as_i64().unwrap();

    // Encrypt client-side; the server must never see the plaintext.
    let plaintext = b"tar.zst bytes of a real session".repeat(500);
    let ciphertext = encrypt_bundle(&mk, &plaintext);

    let r = app
        .http
        .put(app.url("/v1/sessions/proj/bundle"))
        .bearer_auth(&token)
        .header("x-nemr-lease-holder", "machine-A")
        .header("x-nemr-lease-fence", fence.to_string())
        .body(ciphertext.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "upload: {}", r.text().await.unwrap());

    // The stored blob comes back byte-identical...
    let response = app
        .http
        .get(app.url("/v1/sessions/proj/bundle"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    // ...and it is streamed with its length declared. F-16 sends the body as a
    // stream rather than a buffered Vec, and a stream with no Content-Length
    // reaches the client as a chunked body of unknown size — no progress, and
    // no way to tell a truncated download from a complete one.
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok()),
        Some(ciphertext.len()),
        "the streamed download must declare the object's length"
    );
    let got = response.bytes().await.unwrap();
    assert_eq!(got.as_ref(), ciphertext.as_slice(), "ciphertext round-trip");
    // ...and it is ciphertext, not the plaintext.
    assert_ne!(got.as_ref(), plaintext.as_slice());
    // ...and it decrypts back to the original.
    assert_eq!(decrypt_bundle(&mk, &got).unwrap(), plaintext);

    // The index now reports the bundle.
    let list: serde_json::Value = app
        .http
        .get(app.url("/v1/sessions"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list[0]["has_bundle"], true);
}

#[tokio::test]
async fn an_upload_without_holding_the_lease_is_refused() {
    let app = spawn().await;
    let e = Enrolled::new();
    let (token, mk) = app.enroll(&e).await;
    app.http
        .post(app.url("/v1/sessions"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "proj", "agent": "claude" }))
        .send()
        .await
        .unwrap();

    // No lease acquired: a write with a made-up fence is refused server-side.
    let ct = encrypt_bundle(&mk, b"data");
    let r = app
        .http
        .put(app.url("/v1/sessions/proj/bundle"))
        .bearer_auth(&token)
        .header("x-nemr-lease-holder", "machine-A")
        .header("x-nemr-lease-fence", "1")
        .body(ct)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409, "writing without the lease must be refused");
}
