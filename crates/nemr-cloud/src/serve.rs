//! The UI's HTTP surface — the handshake, first (E-11 ruling, 2026-09-06).
//!
//! Served by this commercial process on a loopback port that exists only
//! while `nemr ui` runs. The daemon keeps its Unix socket; this process is
//! its gRPC client for everything that touches the engine.
//!
//! The boundary this is built on: a loopback port is reachable by every
//! process of every local user (and from Windows, through WSL2's forwarding —
//! which is also what makes the UI reachable at all). The daemon's socket is
//! reachable only by its owner, by file mode. The handshake restores that
//! boundary for the browser:
//!
//! 1. A **launch token** — 32 random bytes — is minted at start, written 0600
//!    to the user's state directory (the same file-mode trust as the socket),
//!    and put in the launch URL's *fragment*: `http://127.0.0.1:PORT/#token=…`.
//!    Fragments are never sent in requests or referrers.
//! 2. The page exchanges it **once** (`POST /auth/session`, token in a custom
//!    header, compared in constant time) for a session cookie: `HttpOnly`, so
//!    script cannot read it; `SameSite=Strict`, so no other site's page can
//!    send it; then the page scrubs the fragment from history.
//! 3. Every request must carry the right `Host` (DNS rebinding: a page on an
//!    attacker's name resolving to 127.0.0.1 fails here), and any `Origin` it
//!    carries must be this origin (a cross-site page fails here). No CORS
//!    header is ever emitted.
//! 4. Every `/api/*` request must carry the cookie **and** a custom header
//!    (`X-Nemr-Request`), which a cross-site form cannot add.
//!
//! What this does not protect against, stated: another process of the same
//! user, which can read the token file — the boundary the socket has today.
//!
//! The control that proves it: a request without the cookie is refused.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use subtle::ConstantTimeEq;

const COOKIE: &str = "nemr_session";
const TOKEN_HEADER: &str = "x-nemr-token";
const REQUEST_HEADER: &str = "x-nemr-request";

/// Everything the surface knows. Held in memory for the life of the process;
/// nothing here is written anywhere but the launch URL file.
pub struct UiState {
    port: u16,
    token: [u8; 32],
    token_consumed: AtomicBool,
    sessions: Mutex<HashSet<String>>,
}

impl UiState {
    pub fn new(port: u16, token: [u8; 32]) -> Self {
        Self {
            port,
            token,
            token_consumed: AtomicBool::new(false),
            sessions: Mutex::new(HashSet::new()),
        }
    }

    /// The URL the launcher opens: the token travels in the fragment only.
    pub fn launch_url(&self) -> String {
        format!("http://127.0.0.1:{}/#token={}", self.port, hex(&self.token))
    }

    fn origins(&self) -> [String; 2] {
        [
            format!("http://127.0.0.1:{}", self.port),
            format!("http://localhost:{}", self.port),
        ]
    }

    fn hosts(&self) -> [String; 2] {
        [
            format!("127.0.0.1:{}", self.port),
            format!("localhost:{}", self.port),
        ]
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_bytes() -> [u8; 32] {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// Decode a lowercase-hex token; a malformed one is simply "not the token".
fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

pub fn router(state: Arc<UiState>) -> Router {
    let api = Router::new()
        .route("/ping", get(ping))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .route("/", get(index))
        .route("/auth/session", post(exchange))
        .nest("/api", api)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_this_origin,
        ))
        .with_state(state)
}

/// Host must name this origin (DNS rebinding); an Origin, if sent, must be
/// this origin (cross-site). Applies to everything, before anything else.
async fn require_this_origin(
    State(state): State<Arc<UiState>>,
    req: Request,
    next: Next,
) -> Response {
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| state.hosts().iter().any(|ok| ok == h));
    if !host_ok {
        return (StatusCode::MISDIRECTED_REQUEST, "this is not the UI's host").into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let origin_ok = origin
            .to_str()
            .is_ok_and(|o| state.origins().iter().any(|ok| ok == o));
        if !origin_ok {
            return (StatusCode::FORBIDDEN, "cross-site origin").into_response();
        }
    }
    next.run(req).await
}

/// `/api/*`: the session cookie AND the custom header, or nothing.
async fn require_session(State(state): State<Arc<UiState>>, req: Request, next: Next) -> Response {
    let Some(id) = session_cookie(req.headers()) else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };
    let live = state.sessions.lock().is_ok_and(|s| s.contains(&id));
    if !live {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    }
    if req.headers().get(REQUEST_HEADER).is_none() {
        return (StatusCode::FORBIDDEN, "missing request header").into_response();
    }
    next.run(req).await
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|kv| kv.strip_prefix(COOKIE).and_then(|v| v.strip_prefix('=')))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// The one-shot exchange: launch token in, session cookie out.
async fn exchange(State(state): State<Arc<UiState>>, headers: HeaderMap) -> Response {
    let presented = headers
        .get(TOKEN_HEADER)
        .and_then(|h| h.to_str().ok())
        .and_then(unhex32);
    let Some(presented) = presented else {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    };
    // Constant-time compare, then single use: a token that has been exchanged
    // is spent even if it is right, so a URL that leaked after the fact buys
    // nothing.
    if presented.ct_eq(&state.token).unwrap_u8() != 1 {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }
    if state.token_consumed.swap(true, Ordering::SeqCst) {
        return (StatusCode::UNAUTHORIZED, "token already used").into_response();
    }
    let id = hex(&random_bytes());
    if let Ok(mut s) = state.sessions.lock() {
        s.insert(id.clone());
    }
    let cookie = format!("{COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/");
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("cookie value"),
        )],
    )
        .into_response()
}

async fn ping() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

/// The page: exchange the fragment's token, scrub the fragment, prove the
/// session with one guarded call. No framework, no build step.
async fn index() -> Response {
    const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>nemr</title>
<p id="s">connecting…</p>
<script>
(async () => {
  const s = document.getElementById('s');
  const m = location.hash.match(/token=([0-9a-f]{64})/);
  if (m) {
    const r = await fetch('/auth/session', { method: 'POST', headers: { 'X-Nemr-Token': m[1], 'X-Nemr-Request': '1' } });
    history.replaceState(null, '', '/');
    if (!r.ok) { s.textContent = 'handshake refused (' + r.status + '): start the UI again with nemr ui'; return; }
  }
  const p = await fetch('/api/ping', { headers: { 'X-Nemr-Request': '1' } });
  s.textContent = p.ok ? 'session established' : 'no session (' + p.status + '): start the UI again with nemr ui';
})();
</script>"#;
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PAGE).into_response()
}

/// `nemr ui`: bind loopback, mint the token, print (and optionally open) the
/// launch URL, serve until killed. The port exists only while this runs.
pub fn run(port: Option<u16>, open: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the UI runtime")?;
    rt.block_on(async {
        // The UI is a client of the daemon (E-11 ruling): reach it first, through
        // the API crate — autostart, version handshake — and refuse to open a
        // port for a UI that could not talk to the engine. The daemon's build
        // is printed so the pairing is visible.
        let daemon = nemr_daemon_api::client::connect()
            .await
            .context("the UI needs the daemon; it could not be reached")?;
        drop(daemon);
        println!(
            "nemr ui: daemon reachable at {} (protocol v{})",
            nemr_daemon_api::socket::socket_path()?.display(),
            nemr_daemon_api::proto::PROTOCOL_VERSION
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
            .await
            .context("binding the UI's loopback port")?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(UiState::new(port, random_bytes()));
        let url = state.launch_url();
        crate::state::save_ui_url(&url)?;
        println!("nemr ui: {url}");
        println!("(loopback only; the token in the URL is single-use; Ctrl-C stops the UI and closes the port)");
        if open {
            // Best effort: a missing opener is not an error, the URL is printed.
            let _ = std::process::Command::new("xdg-open")
                .arg(&url)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        axum::serve(listener, router(state))
            .await
            .context("the UI server stopped")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    const PORT: u16 = 4321;
    fn token() -> [u8; 32] {
        [7u8; 32]
    }
    fn app() -> (Router, Arc<UiState>) {
        let state = Arc::new(UiState::new(PORT, token()));
        (router(state.clone()), state)
    }
    fn host() -> String {
        format!("127.0.0.1:{PORT}")
    }
    async fn send(app: &Router, req: HttpRequest<Body>) -> Response {
        app.clone().oneshot(req).await.unwrap()
    }
    fn exchange_req(tok: &str) -> HttpRequest<Body> {
        HttpRequest::post("/auth/session")
            .header("host", host())
            .header(TOKEN_HEADER, tok)
            .header(REQUEST_HEADER, "1")
            .body(Body::empty())
            .unwrap()
    }
    /// Exchange the launch token and return the cookie the page would carry.
    async fn establish(app: &Router) -> String {
        let r = send(app, exchange_req(&hex(&token()))).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        let set = r
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(set.contains("HttpOnly"), "{set}");
        assert!(set.contains("SameSite=Strict"), "{set}");
        set.split(';').next().unwrap().to_string()
    }
    fn ping_req(
        cookie: Option<&str>,
        request_header: bool,
        origin: Option<&str>,
    ) -> HttpRequest<Body> {
        let mut b = HttpRequest::get("/api/ping").header("host", host());
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        if request_header {
            b = b.header(REQUEST_HEADER, "1");
        }
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        b.body(Body::empty()).unwrap()
    }

    /// THE control: a request without the cookie is refused, even with the
    /// custom header and the right host.
    #[tokio::test]
    async fn a_request_without_the_cookie_is_refused() {
        let (app, _) = app();
        let r = send(&app, ping_req(None, true, None)).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        // And a cookie naming a session that was never established is the same.
        let r = send(&app, ping_req(Some("nemr_session=deadbeef"), true, None)).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }

    /// The launch token exchanges for an HttpOnly, SameSite=Strict cookie —
    /// once. A wrong token never does; the right token, spent, does not either.
    #[tokio::test]
    async fn the_launch_token_exchanges_once_for_a_strict_httponly_cookie() {
        let (app, _) = app();
        let wrong = send(&app, exchange_req(&hex(&[8u8; 32]))).await;
        assert_eq!(
            wrong.status(),
            StatusCode::UNAUTHORIZED,
            "a wrong token must not exchange"
        );
        let malformed = send(&app, exchange_req("not-hex")).await;
        assert_eq!(malformed.status(), StatusCode::UNAUTHORIZED);
        let cookie = establish(&app).await;
        let ok = send(&app, ping_req(Some(&cookie), true, None)).await;
        assert_eq!(
            ok.status(),
            StatusCode::OK,
            "the established session must be accepted"
        );
        let again = send(&app, exchange_req(&hex(&token()))).await;
        assert_eq!(
            again.status(),
            StatusCode::UNAUTHORIZED,
            "the token is single-use"
        );
    }

    /// The cookie alone is not enough: the custom header a cross-site form
    /// cannot add is required too.
    #[tokio::test]
    async fn the_cookie_alone_is_refused_without_the_request_header() {
        let (app, _) = app();
        let cookie = establish(&app).await;
        let r = send(&app, ping_req(Some(&cookie), false, None)).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }

    /// A cross-site Origin is refused; this origin and no Origin are accepted.
    #[tokio::test]
    async fn a_cross_site_origin_is_refused_and_this_origin_is_not() {
        let (app, _) = app();
        let cookie = establish(&app).await;
        let evil = send(
            &app,
            ping_req(Some(&cookie), true, Some("http://evil.example")),
        )
        .await;
        assert_eq!(evil.status(), StatusCode::FORBIDDEN);
        let mine = send(
            &app,
            ping_req(
                Some(&cookie),
                true,
                Some(&format!("http://127.0.0.1:{PORT}")),
            ),
        )
        .await;
        assert_eq!(mine.status(), StatusCode::OK);
        let localhost = send(
            &app,
            ping_req(
                Some(&cookie),
                true,
                Some(&format!("http://localhost:{PORT}")),
            ),
        )
        .await;
        assert_eq!(localhost.status(), StatusCode::OK);
    }

    /// DNS rebinding: a request whose Host is not this origin is refused
    /// before anything else, including the exchange.
    #[tokio::test]
    async fn a_wrong_host_is_refused_before_anything_else() {
        let (app, _) = app();
        let r = send(
            &app,
            HttpRequest::post("/auth/session")
                .header("host", format!("attacker.example:{PORT}"))
                .header(TOKEN_HEADER, hex(&token()))
                .header(REQUEST_HEADER, "1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(r.status(), StatusCode::MISDIRECTED_REQUEST);
        let none = send(&app, HttpRequest::get("/").body(Body::empty()).unwrap()).await;
        assert_eq!(
            none.status(),
            StatusCode::MISDIRECTED_REQUEST,
            "no Host at all is refused too"
        );
    }

    /// No response ever carries a CORS header: the surface is same-origin only.
    #[tokio::test]
    async fn no_response_carries_cors_headers() {
        let (app, _) = app();
        let cookie = establish(&app).await;
        for r in [
            send(
                &app,
                HttpRequest::get("/")
                    .header("host", host())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            send(
                &app,
                ping_req(
                    Some(&cookie),
                    true,
                    Some(&format!("http://127.0.0.1:{PORT}")),
                ),
            )
            .await,
            send(&app, ping_req(None, true, Some("http://evil.example"))).await,
        ] {
            assert!(
                r.headers()
                    .keys()
                    .all(|k| !k.as_str().starts_with("access-control-")),
                "a CORS header appeared: {:?}",
                r.headers()
            );
        }
    }

    /// The launch URL carries the token in the fragment and nowhere else.
    #[test]
    fn the_launch_url_puts_the_token_in_the_fragment() {
        let state = UiState::new(PORT, token());
        let url = state.launch_url();
        assert!(
            url.starts_with(&format!("http://127.0.0.1:{PORT}/#token=")),
            "{url}"
        );
        assert!(
            !url.split('#').next().unwrap().contains("token"),
            "no token before the fragment: {url}"
        );
    }
}
