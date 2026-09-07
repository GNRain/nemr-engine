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
//!
//! On the handshake, the flow (ruled 2026-09-06: login, session list,
//! pull-and-start, attach, stop-and-push). Steps 1 and 2 live here:
//!
//! - `POST /api/login` / `POST /api/logout` / `GET /api/whoami`, and
//!   `POST /api/register` + `/api/register/confirm` (the recovery code shown
//!   once and typed back through the envelope, exactly the CLI's step) — the
//!   same `core` functions the CLI's `nemr login` and `nemr register` run, so
//!   the browser's login IS the CLI's login: the KDF runs in this process, the password crosses only
//!   the loopback under the cookie-and-header guard, and nothing but the
//!   account (token, public KDF material, sealed envelope) is stored — the
//!   E-16 line unchanged.
//! - `GET /api/sessions` — the server's index merged with the daemon's
//!   project list, asked over the daemon's socket through `nemr-daemon-api`
//!   (the UI is the daemon's gRPC client; it does not link the engine). Each
//!   row says local / remote / both, running or not, and **who holds the
//!   lease right now** — the D-03 state a user must see before pulling.
//!
//! Step 3, pull-and-start: `POST /api/sessions/{name}/pull` (password and
//! take-over in the body) runs the CLI's `pull` through the same core, on a
//! blocking thread, then the daemon's `Start` — as a **job** the page polls
//! (`GET /api/jobs/{id}`), so every line the CLI would have printed is shown
//! as it happens, and an error is the CLI's error. A lease held elsewhere is
//! typed in the reply (`held_by`) so the page can offer the take-over the
//! CLI offers as a flag. `POST /api/sessions/{name}/start` starts a session
//! that is already here. The engine is behind `UiEngine`: the daemon in the
//! binary, a fake in the router's tests.
//!
//! Step 5, stop-and-push: `POST /api/sessions/{name}/push` (password,
//! release and take-over in the body) stops the session first when it is
//! running — the engine's export needs a quiescent volume, and `core::push`
//! refuses a running project outright — then runs the CLI's `push` through
//! the same core, as a job like the pull. Releasing the lease is the
//! default here, because the point of pushing from the browser is to hand
//! the session on.
//!
//! Step 4, attach: the daemon's `Attach` stream bridged to the page over a
//! WebSocket, rendered by xterm.js (pinned, served from this binary — no
//! external resource). A browser cannot put the custom header on a
//! WebSocket handshake, so `/ws/attach/{name}` has its own gate instead:
//! the session cookie, a **single-use ticket** minted by a guarded
//! `POST /api/sessions/{name}/attach-ticket` (bound to that cookie and that
//! session name, expiring in thirty seconds), and an `Origin` that must be
//! present and this origin — every browser sends one on a WebSocket
//! handshake, and a cross-site page's fails. Bytes typed go to the session's
//! stdin as binary frames; the session's stdout and stderr come back as
//! binary frames; resizes and the exit are small text frames.
//!
//! One page, no framework, no build step: the same HTML serves the login form
//! and the list, and picks by asking `whoami`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use nemr_daemon_api::proto::{
    attach_client, attach_server, AttachClient, AttachResize, AttachServer, AttachStart,
};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::core::{self, EngineOps, HeldElsewhere};

const COOKIE: &str = "nemr_session";
const TOKEN_HEADER: &str = "x-nemr-token";
const REQUEST_HEADER: &str = "x-nemr-request";

/// The engine, as the UI needs it: the sync core's three verbs plus start
/// and stop. The daemon over its socket in the binary; a fake in the tests.
pub trait UiEngine: EngineOps {
    fn start(&self, name: &str) -> Result<()>;
    fn stop(&self, name: &str) -> Result<String>;
    /// Open an attach stream: the daemon's, or a fake's in the tests.
    fn attach(
        &self,
        start: AttachStart,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AttachLink>> + Send + '_>>;
}

/// An open attach stream, both directions, as the bridge drives it.
pub struct AttachLink {
    pub to_session: tokio::sync::mpsc::Sender<AttachClient>,
    pub from_session:
        std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<AttachServer>> + Send>>,
    /// Whatever must outlive the stream (the daemon session and its audit
    /// printer).
    pub _keep: Option<Box<dyn std::any::Any + Send>>,
}

/// A ticket for one WebSocket handshake: bound to the page's session and
/// the session name, spent on use, dead after thirty seconds.
struct AttachTicket {
    cookie_session: String,
    name: String,
    expires_at: i64,
}
const TICKET_TTL_SECS: i64 = 30;

/// A long-running action the page watches: the lines the CLI would print,
/// as they happen, and the outcome.
#[derive(Default)]
pub struct Job {
    kind: String,
    session: String,
    lines: Vec<(i64, String)>,
    done: bool,
    error: Option<String>,
    /// Set when the error is a lease held elsewhere, so the page can offer
    /// the take-over.
    held_by: Option<String>,
}

/// Everything the surface knows. Held in memory for the life of the process;
/// nothing here is written anywhere but the launch URL file.
pub struct UiState {
    port: u16,
    token: [u8; 32],
    token_consumed: AtomicBool,
    sessions: Mutex<HashSet<String>>,
    engine: Arc<dyn UiEngine>,
    jobs: Mutex<HashMap<String, Arc<Mutex<Job>>>>,
    attach_tickets: Mutex<HashMap<String, AttachTicket>>,
    /// A registration waiting for its recovery code to be typed back (E-16:
    /// recovery is not deferrable). In memory only, for this process's life:
    /// the same place the CLI keeps it between showing the code and reading
    /// it back.
    pending_registration: Mutex<Option<core::RegistrationPending>>,
}

impl UiState {
    pub fn new(port: u16, token: [u8; 32], engine: Arc<dyn UiEngine>) -> Self {
        Self {
            port,
            token,
            token_consumed: AtomicBool::new(false),
            sessions: Mutex::new(HashSet::new()),
            engine,
            jobs: Mutex::new(HashMap::new()),
            attach_tickets: Mutex::new(HashMap::new()),
            pending_registration: Mutex::new(None),
        }
    }

    /// Start a job on a blocking thread; the page polls it by id. `work`
    /// gets the CLI's `report` callback; its lines land in the job as they
    /// are said.
    fn start_job(
        self: &Arc<Self>,
        kind: &str,
        session: &str,
        work: impl FnOnce(&mut dyn FnMut(&str)) -> Result<()> + Send + 'static,
    ) -> String {
        let id = hex(&random_bytes())[..16].to_string();
        let job = Arc::new(Mutex::new(Job {
            kind: kind.to_string(),
            session: session.to_string(),
            ..Job::default()
        }));
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(id.clone(), job.clone());
        }
        tokio::task::spawn_blocking(move || {
            let mut say = |line: &str| {
                if let Ok(mut j) = job.lock() {
                    j.lines.push((now_unix(), line.to_string()));
                }
            };
            let outcome = work(&mut say);
            if let Ok(mut j) = job.lock() {
                j.done = true;
                if let Err(e) = outcome {
                    j.held_by = e.downcast_ref::<HeldElsewhere>().map(|h| h.holder.clone());
                    j.error = Some(format!("{e:#}"));
                }
            }
        });
        id
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

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
        .route("/whoami", get(whoami))
        .route("/login", post(login))
        .route("/register", post(register_begin))
        .route("/register/confirm", post(register_confirm))
        .route("/logout", post(logout))
        .route("/sessions", get(sessions))
        .route("/sessions/{name}/pull", post(pull))
        .route("/sessions/{name}/start", post(start))
        .route("/sessions/{name}/push", post(push))
        .route("/sessions/{name}/stop", post(stop))
        .route("/jobs/{id}", get(job))
        .route("/sessions/{name}/attach-ticket", post(attach_ticket))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    Router::new()
        .route("/", get(index))
        .route("/assets/xterm.js", get(asset_xterm_js))
        .route("/assets/xterm.css", get(asset_xterm_css))
        .route("/assets/addon-fit.js", get(asset_fit_js))
        .route("/auth/session", post(exchange))
        .route("/ws/attach/{name}", get(attach_ws))
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

/// A failure the page can show: the message, nothing else. `anyhow`'s chain
/// is the same text the CLI prints.
fn failed(status: StatusCode, e: anyhow::Error) -> Response {
    (status, Json(json!({ "error": format!("{e:#}") }))).into_response()
}

/// Who is logged in on this machine (the stored account), and the server the
/// login form should offer.
async fn whoami() -> Json<Value> {
    Json(match core::whoami() {
        Some(a) => json!({ "logged_in": true, "email": a.email, "server": a.server }),
        None => json!({ "logged_in": false, "default_server": core::default_server() }),
    })
}

#[derive(Deserialize)]
struct LoginBody {
    #[serde(default)]
    server: String,
    email: String,
    password: String,
}

/// `nemr login`, from the page. The KDF and the exchange run on a blocking
/// thread (they are the CLI's blocking code); the password lives in this
/// request and nowhere after it.
async fn login(Json(body): Json<LoginBody>) -> Response {
    let server = if body.server.trim().is_empty() {
        core::default_server()
    } else {
        body.server.trim().to_string()
    };
    let email = body.email.trim().to_string();
    if email.is_empty() || body.password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "email and password are required" })),
        )
            .into_response();
    }
    let done = tokio::task::spawn_blocking(move || core::login(&server, &email, &body.password))
        .await
        .map_err(|e| anyhow::anyhow!("the login task failed: {e}"));
    match done {
        Ok(Ok(account)) => Json(json!({
            "logged_in": true, "email": account.email, "server": account.server
        }))
        .into_response(),
        Ok(Err(e)) => failed(StatusCode::UNAUTHORIZED, e),
        Err(e) => failed(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `nemr register`, first half: create the account, hand the page the
/// recovery code to show ONCE. The pending registration stays in this
/// process until confirmed; a page reload loses it, and the response says
/// what that means (the account exists and is not usable until confirmed).
async fn register_begin(
    State(state): State<Arc<UiState>>,
    Json(body): Json<LoginBody>,
) -> Response {
    let server = if body.server.trim().is_empty() {
        core::default_server()
    } else {
        body.server.trim().to_string()
    };
    let email = body.email.trim().to_string();
    if email.is_empty() || body.password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "email and password are required" })),
        )
            .into_response();
    }
    let begun =
        tokio::task::spawn_blocking(move || core::register_begin(&server, &email, &body.password))
            .await;
    match begun {
        Ok(Ok(pending)) => {
            let reply = json!({
                "email": pending.email,
                "server": pending.server,
                "recovery_code": pending.recovery_code.display(),
                "if_abandoned": core::unconfirmed_message(),
            });
            if let Ok(mut slot) = state.pending_registration.lock() {
                *slot = Some(pending);
            }
            Json(reply).into_response()
        }
        Ok(Err(e)) => failed(StatusCode::BAD_REQUEST, e),
        Err(e) => failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("the register task failed: {e}"),
        ),
    }
}

#[derive(Deserialize)]
struct ConfirmBody {
    code: String,
}

/// `nemr register`, second half: the typed code must open the recovery
/// envelope (a real recovery of the master key, not a string compare). A
/// wrong code is refused and the registration stays pending, so the user
/// can try again (F-92); the right one confirms and logs in.
async fn register_confirm(
    State(state): State<Arc<UiState>>,
    Json(body): Json<ConfirmBody>,
) -> Response {
    let pending = match state.pending_registration.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    let Some(pending) = pending else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "no registration is waiting for confirmation here" })),
        )
            .into_response();
    };
    let Some(recovered) = core::register_check_code(&pending, &body.code) else {
        // Put it back: the code was wrong, the registration is intact.
        if let Ok(mut slot) = state.pending_registration.lock() {
            *slot = Some(pending);
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "that code does not open the recovery envelope — try again (hyphens and case do not matter)" })),
        )
            .into_response();
    };
    let done =
        tokio::task::spawn_blocking(move || core::register_confirm(&pending, &recovered)).await;
    match done {
        Ok(Ok(account)) => Json(json!({
            "logged_in": true, "email": account.email, "server": account.server
        }))
        .into_response(),
        Ok(Err(e)) => failed(StatusCode::BAD_GATEWAY, e),
        Err(e) => failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("the confirm task failed: {e}"),
        ),
    }
}

/// `nemr logout`, from the page: revoke server-side, clear the account.
async fn logout() -> Response {
    match tokio::task::spawn_blocking(core::logout).await {
        Ok(Ok(report)) => Json(json!({
            "was_logged_in": report.was_logged_in,
            "revoke_failed": report.revoke_failed,
        }))
        .into_response(),
        Ok(Err(e)) => failed(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("the logout task failed: {e}"),
        ),
    }
}

/// `nemr sessions`, from the page: the server's index merged with the
/// daemon's list. A daemon that cannot be reached is reported in the reply,
/// not hidden — the list still renders from the server alone, and the page
/// says which rows it could not check locally.
async fn sessions(State(state): State<Arc<UiState>>) -> Response {
    let engine = state.engine.clone();
    let rows = tokio::task::spawn_blocking(move || {
        let (local, local_error) = match engine.list() {
            Ok(list) => (Some(list), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        core::sessions(local.as_deref()).map(|rows| (rows, local_error))
    })
    .await;
    match rows {
        Ok(Ok((rows, local_error))) => {
            let rows: Vec<Value> = rows
                .iter()
                .map(|r| {
                    json!({
                        "name": r.name,
                        "agent": r.agent,
                        "where": r.location.as_str(),
                        "running": r.running,
                        "size_bytes": r.size_bytes,
                        "updated_at_unix": r.updated_at_unix,
                        "last_machine": r.last_machine,
                        "has_bundle": r.has_bundle,
                        "held_by": r.held_by,
                        "lease_expires_at_unix": r.lease_expires_at_unix,
                    })
                })
                .collect();
            Json(json!({
                "rows": rows,
                "local_available": local_error.is_none(),
                "local_error": local_error,
                "this_machine": crate::state::holder_identity(),
            }))
            .into_response()
        }
        // "not logged in" is the one the page acts on: it shows the form.
        Ok(Err(e)) if core::whoami().is_none() => failed(StatusCode::UNAUTHORIZED, e),
        Ok(Err(e)) => failed(StatusCode::BAD_GATEWAY, e),
        Err(e) => failed(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("the sessions task failed: {e}"),
        ),
    }
}

#[derive(Deserialize)]
struct PullBody {
    password: String,
    #[serde(default)]
    take_over: bool,
}

/// Step 3: `nemr pull`, then the daemon's `Start`, as a job. The password
/// is used for the master key on the blocking thread and dropped with the
/// request, the CLI's policy (per-device caching is the deferred layer).
async fn pull(
    State(state): State<Arc<UiState>>,
    UrlPath(name): UrlPath<String>,
    Json(body): Json<PullBody>,
) -> Response {
    if core::whoami().is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "not logged in" })),
        )
            .into_response();
    }
    if body.password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the password is required to decrypt the bundle" })),
        )
            .into_response();
    }
    let engine = state.engine.clone();
    let session = name.clone();
    let id = state.start_job("pull", &name, move |say| {
        let pulled = core::pull(&session, &body.password, body.take_over, &*engine, say)?;
        say(&format!(
            "{} of session restored as {:?}, lease held as {}",
            core::human_bytes(pulled.plaintext_bytes as i64),
            pulled.imported,
            pulled.holder
        ));
        say("starting the session");
        engine.start(&session)?;
        say(&format!("{session:?} is running"));
        Ok(())
    });
    Json(json!({ "job": id })).into_response()
}

/// Start a session that is already here, as a job (the daemon's Start can
/// take a while: mounting, the task, provisioning checks).
async fn start(State(state): State<Arc<UiState>>, UrlPath(name): UrlPath<String>) -> Response {
    let engine = state.engine.clone();
    let session = name.clone();
    let id = state.start_job("start", &name, move |say| {
        say("starting the session");
        engine.start(&session)?;
        say(&format!("{session:?} is running"));
        Ok(())
    });
    Json(json!({ "job": id })).into_response()
}

#[derive(Deserialize)]
struct PushBody {
    password: String,
    /// Release the lease after the push — the default from the browser,
    /// because pushing from here is handing the session on. The CLI's
    /// `--release`.
    #[serde(default = "yes")]
    release: bool,
    #[serde(default)]
    take_over: bool,
}
fn yes() -> bool {
    true
}

/// Step 5: stop, then `nemr push`, as one job. The stop comes first
/// because the export needs a quiescent volume — `core::push` refuses a
/// running project, and refusing the user for a state the button could fix
/// would be the CLI's rule enforced without the CLI's remedy. The password
/// is checked before the stop, because a stop a later refusal cannot undo
/// must not be paid for a push that will not happen.
async fn push(
    State(state): State<Arc<UiState>>,
    UrlPath(name): UrlPath<String>,
    Json(body): Json<PushBody>,
) -> Response {
    if core::whoami().is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "not logged in" })),
        )
            .into_response();
    }
    if body.password.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "the password is required to encrypt the bundle" })),
        )
            .into_response();
    }
    let engine = state.engine.clone();
    let session = name.clone();
    let id = state.start_job("push", &name, move |say| {
        // The password FIRST. Stopping is not undoable by a later refusal,
        // and the first form of this handler stopped the session and then
        // failed on a typo — the user paid a running session for a push
        // that never happened. `push` derives the same key again; this only
        // moves the refusal in front of the destructive step.
        core::verify_password(&body.password)?;
        // Stop only what is running: a stop of a stopped session is not an
        // error, but saying "stopping" about one that was never running
        // would be a line the CLI never printed.
        let running = engine
            .list()?
            .into_iter()
            .any(|p| p.name == session && p.running);
        if running {
            say(&format!("stopping {session:?} so the volume is quiescent"));
            let outcome = engine.stop(&session)?;
            say(&format!("stopped ({outcome})"));
        }
        let pushed = core::push(
            &session,
            &body.password,
            body.release,
            body.take_over,
            &*engine,
            say,
        )?;
        if !pushed.released {
            say(&format!("lease still held as {}", pushed.holder));
        }
        Ok(())
    });
    Json(json!({ "job": id })).into_response()
}

/// Stop a session without pushing it — the local half of step 5, for a
/// session the user is not handing on.
async fn stop(State(state): State<Arc<UiState>>, UrlPath(name): UrlPath<String>) -> Response {
    let engine = state.engine.clone();
    let session = name.clone();
    let id = state.start_job("stop", &name, move |say| {
        say(&format!("stopping {session:?}"));
        let outcome = engine.stop(&session)?;
        say(&format!("stopped ({outcome})"));
        Ok(())
    });
    Json(json!({ "job": id })).into_response()
}

/// What a job has said so far, and whether it is done.
async fn job(State(state): State<Arc<UiState>>, UrlPath(id): UrlPath<String>) -> Response {
    let job = state
        .jobs
        .lock()
        .ok()
        .and_then(|jobs| jobs.get(&id).cloned());
    let Some(job) = job else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such job" })),
        )
            .into_response();
    };
    let j = match job.lock() {
        Ok(j) => j,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "job state poisoned" })),
            )
                .into_response()
        }
    };
    Json(json!({
        "kind": j.kind,
        "session": j.session,
        "lines": j.lines.iter().map(|(at, l)| json!({ "at": at, "text": l })).collect::<Vec<_>>(),
        "done": j.done,
        "ok": j.done && j.error.is_none(),
        "error": j.error,
        "held_by": j.held_by,
    }))
    .into_response()
}

async fn asset_xterm_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/xterm.js"),
    )
        .into_response()
}
async fn asset_fit_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../assets/addon-fit.js"),
    )
        .into_response()
}
async fn asset_xterm_css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../assets/xterm.css"),
    )
        .into_response()
}

/// Mint a single-use ticket for one attach handshake, bound to this page's
/// session and to `name`. Guarded like every `/api` route, so the ticket
/// carries the guard's proof into the one handshake a browser cannot put the
/// header on.
async fn attach_ticket(
    State(state): State<Arc<UiState>>,
    UrlPath(name): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let Some(cookie_session) = session_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };
    let ticket = hex(&random_bytes());
    if let Ok(mut t) = state.attach_tickets.lock() {
        // Sweep the dead ones while here; the map never grows past use.
        let now = now_unix();
        t.retain(|_, v| v.expires_at > now);
        t.insert(
            ticket.clone(),
            AttachTicket {
                cookie_session,
                name,
                expires_at: now + TICKET_TTL_SECS,
            },
        );
    }
    Json(json!({ "ticket": ticket, "expires_in_secs": TICKET_TTL_SECS })).into_response()
}

#[derive(Deserialize)]
struct AttachQuery {
    #[serde(default)]
    ticket: String,
}

/// The WebSocket gate: cookie session live, Origin present and this origin
/// (the outer layer already refused a wrong one; a missing one is refused
/// here, because a browser always sends it and this route is for browsers),
/// and a ticket that is unspent, unexpired, this cookie's and this name's.
async fn attach_ws(
    State(state): State<Arc<UiState>>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<AttachQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(cookie_session) = session_cookie(&headers) else {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    };
    if !state
        .sessions
        .lock()
        .is_ok_and(|s| s.contains(&cookie_session))
    {
        return (StatusCode::UNAUTHORIZED, "no session").into_response();
    }
    if headers.get(header::ORIGIN).is_none() {
        return (StatusCode::FORBIDDEN, "no origin").into_response();
    }
    let ticket_ok = state.attach_tickets.lock().is_ok_and(|mut t| {
        // Spent on the way out, right or wrong: a ticket is one handshake.
        match t.remove(&q.ticket) {
            Some(tk) => {
                tk.cookie_session == cookie_session && tk.name == name && tk.expires_at > now_unix()
            }
            None => false,
        }
    });
    if !ticket_ok {
        return (StatusCode::UNAUTHORIZED, "bad ticket").into_response();
    }
    let engine = state.engine.clone();
    ws.on_upgrade(move |socket| bridge(socket, engine, name))
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum PageControl {
    #[serde(rename = "start")]
    Start { rows: u32, cols: u32 },
    #[serde(rename = "resize")]
    Resize { rows: u32, cols: u32 },
}

/// Drive one attach: the page's first text frame says the terminal size;
/// then bytes go down as stdin, bytes come up as stdout/stderr, resizes are
/// text, and the exit (or the daemon's refusal) is the last text frame.
/// One control frame to the page.
async fn tell(socket: &mut WebSocket, v: Value) -> bool {
    socket
        .send(Message::Text(v.to_string().into()))
        .await
        .is_ok()
}

async fn bridge(mut socket: WebSocket, engine: Arc<dyn UiEngine>, name: String) {
    // The page speaks first: its size. Anything else is a protocol error.
    let (rows, cols) = match socket.recv().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<PageControl>(&t) {
            Ok(PageControl::Start { rows, cols }) => (rows, cols),
            _ => {
                tell(
                    &mut socket,
                    json!({"type": "error", "message": "attach: first frame must be start"}),
                )
                .await;
                return;
            }
        },
        _ => return,
    };
    let link = match engine
        .attach(AttachStart {
            name: name.clone(),
            rows,
            cols,
            interactive: true,
        })
        .await
    {
        Ok(link) => link,
        Err(e) => {
            tell(
                &mut socket,
                json!({"type": "error", "message": format!("{e:#}")}),
            )
            .await;
            return;
        }
    };
    let AttachLink {
        to_session,
        mut from_session,
        _keep,
    } = link;
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Page -> session.
    let down = async {
        while let Some(Ok(m)) = ws_rx.next().await {
            let msg = match m {
                Message::Binary(b) => attach_client::Msg::Stdin(b.to_vec()),
                Message::Text(t) => match serde_json::from_str::<PageControl>(&t) {
                    Ok(PageControl::Resize { rows, cols }) => {
                        attach_client::Msg::Resize(AttachResize { rows, cols })
                    }
                    _ => continue,
                },
                Message::Close(_) => break,
                _ => continue,
            };
            if to_session
                .send(AttachClient { msg: Some(msg) })
                .await
                .is_err()
            {
                break;
            }
        }
        // The page went away: the shell inside must be told, or it never exits.
        let _ = to_session
            .send(AttachClient {
                msg: Some(attach_client::Msg::StdinEof(true)),
            })
            .await;
    };
    // Session -> page.
    let up = async {
        while let Some(m) = from_session.next().await {
            let frame = match m {
                Ok(AttachServer { msg: Some(m) }) => match m {
                    attach_server::Msg::Started(_) => continue,
                    attach_server::Msg::Stdout(b) | attach_server::Msg::Stderr(b) => {
                        Message::Binary(b.into())
                    }
                    attach_server::Msg::ExitCode(code) => {
                        let _ = ws_tx
                            .send(Message::Text(
                                json!({"type": "exit", "code": code}).to_string().into(),
                            ))
                            .await;
                        break;
                    }
                    attach_server::Msg::Error(e) => {
                        let _ = ws_tx
                            .send(Message::Text(
                                json!({"type": "error", "message": e}).to_string().into(),
                            ))
                            .await;
                        break;
                    }
                },
                Ok(_) => continue,
                Err(e) => {
                    let _ = ws_tx
                        .send(Message::Text(
                            json!({"type": "error", "message": format!("{e:#}")})
                                .to_string()
                                .into(),
                        ))
                        .await;
                    break;
                }
            };
            if ws_tx.send(frame).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.close().await;
    };
    tokio::select! {
        _ = down => {}
        _ = up => {}
    }
}

/// The page: exchange the fragment's token, scrub the fragment, then show
/// the login form or the session list by asking `whoami`. No framework, no
/// build step, no external resource: everything the browser runs is in this
/// binary.
async fn index() -> Response {
    const PAGE: &str = r##"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>nemr</title>
<link rel="stylesheet" href="/assets/xterm.css">
<script src="/assets/xterm.js"></script>
<script src="/assets/addon-fit.js"></script>
<style>
  body { font: 14px/1.4 system-ui, sans-serif; margin: 0; background: #f6f7f8; color: #1a1a1a; }
  header { display: flex; align-items: baseline; gap: 1rem; padding: .75rem 1.25rem; background: #fff; border-bottom: 1px solid #ddd; }
  header h1 { font-size: 1.1rem; margin: 0; }
  header .who { margin-left: auto; color: #555; }
  main { max-width: 64rem; margin: 1.5rem auto; padding: 0 1.25rem; }
  form.login { max-width: 22rem; display: grid; gap: .6rem; background: #fff; border: 1px solid #ddd; padding: 1.25rem; border-radius: 6px; }
  label { display: grid; gap: .2rem; color: #444; }
  input { font: inherit; padding: .4rem .5rem; border: 1px solid #bbb; border-radius: 4px; }
  button { font: inherit; padding: .4rem .8rem; border: 1px solid #888; border-radius: 4px; background: #fff; cursor: pointer; }
  button.primary { background: #1a1a1a; color: #fff; border-color: #1a1a1a; }
  table { width: 100%; border-collapse: collapse; background: #fff; border: 1px solid #ddd; }
  th, td { text-align: left; padding: .5rem .6rem; border-bottom: 1px solid #eee; white-space: nowrap; }
  th { font-weight: 600; color: #444; background: #fafafa; }
  .muted { color: #777; }
  .warn { color: #8a4b00; }
  .bad { color: #a40000; }
  .pill { display: inline-block; padding: 0 .45rem; border-radius: 999px; border: 1px solid #bbb; font-size: .85em; }
  .pill.both { border-color: #2a7; color: #186; }
  .pill.remote { border-color: #58c; color: #269; }
  .pill.local { border-color: #999; color: #555; }
  .toolbar { display: flex; gap: .6rem; align-items: center; margin: 0 0 .8rem; }
  .toolbar .note { color: #777; margin-left: auto; }
  #status { min-height: 1.4em; margin: .8rem 0; }
  #termwrap { margin-top: 1rem; background: #000; padding: .5rem; border-radius: 6px; }
  #term { height: 60vh; }
</style>
<header><h1>nemr</h1><span class="who" id="who"></span><button id="logout" hidden>log out</button></header>
<main>
  <div id="status">connecting…</div>
  <form class="login" id="login" hidden>
    <label>server <input name="server" autocomplete="url"></label>
    <label>email <input name="email" type="email" autocomplete="username" required></label>
    <label>password <input name="password" type="password" autocomplete="current-password" required></label>
    <button class="primary" type="submit">log in</button>
    <div class="muted">The password stays on this machine: the key is derived here, and only what the CLI's <code>nemr login</code> sends leaves it.</div>
    <div class="muted">No account yet? <a href="#" id="to-register">register</a></div>
  </form>
  <form class="login" id="register" hidden>
    <label>server <input name="server" autocomplete="url"></label>
    <label>email <input name="email" type="email" autocomplete="username" required></label>
    <label>password <input name="password" type="password" autocomplete="new-password" required></label>
    <label>password, again <input name="again" type="password" autocomplete="new-password" required></label>
    <button class="primary" type="submit">register</button>
    <div class="muted">A recovery code is shown next — the only way back in if the password is forgotten. Have somewhere safe to put it. <a href="#" id="to-login">back to log in</a></div>
  </form>
  <section class="login" id="recovery" hidden>
    <div><strong>Your recovery code</strong> — the ONLY way back in if you forget your password:</div>
    <pre id="code" style="font-size:1.2em;user-select:all"></pre>
    <div class="muted">Store it now (password manager, paper — not this machine). A forgotten password with no recovery code means your data is unrecoverable, permanently: the server cannot read it.</div>
    <form id="confirm" style="display:grid;gap:.6rem">
      <label>type the code back to confirm you stored it <input name="code" autocomplete="off" required></label>
      <button class="primary" type="submit">confirm</button>
    </form>
    <div class="warn" id="abandon"></div>
  </section>
  <section id="list" hidden>
    <div class="toolbar"><button id="refresh">refresh</button><span class="note" id="localnote"></span></div>
    <table><thead><tr><th>session</th><th>agent</th><th>where</th><th>state</th><th>size</th><th>updated</th><th>last machine</th><th>open on</th><th></th></tr></thead><tbody id="rows"></tbody></table>
    <section id="job" class="login" style="max-width:40rem;margin-top:1rem" hidden>
      <div><strong id="jobtitle"></strong></div>
      <form id="pullform" style="display:grid;gap:.6rem" hidden>
        <label>password (to decrypt the bundle on this machine) <input name="password" type="password" autocomplete="current-password" required></label>
        <label style="display:flex;gap:.4rem;align-items:center"><input name="take_over" type="checkbox" style="width:auto"> take over the lease if another machine holds it (it will be locked out of writing)</label>
        <div style="display:flex;gap:.6rem"><button class="primary" type="submit">pull &amp; start</button><button type="button" id="jobcancel">cancel</button></div>
      </form>
      <form id="pushform" style="display:grid;gap:.6rem" hidden>
        <label>password (to encrypt the bundle on this machine) <input name="password" type="password" autocomplete="current-password" required></label>
        <label style="display:flex;gap:.4rem;align-items:center"><input name="release" type="checkbox" style="width:auto" checked> release the lease afterwards, so another machine can take it</label>
        <label style="display:flex;gap:.4rem;align-items:center"><input name="take_over" type="checkbox" style="width:auto"> take over the lease if another machine holds it</label>
        <div style="display:flex;gap:.6rem"><button class="primary" type="submit">push</button><button type="button" id="pushcancel">cancel</button></div>
      </form>
      <pre id="joblog" style="margin:0;white-space:pre-wrap"></pre>
      <div id="jobresult"></div>
    </section>
    <section id="attach" hidden>
      <div class="toolbar" style="margin-top:1rem"><strong id="attachtitle"></strong><span class="note" id="attachnote"></span><button id="detach">detach</button></div>
      <div id="termwrap"><div id="term"></div></div>
    </section>
  </section>
</main>
<script>
(() => {
  const $ = id => document.getElementById(id);
  const H = { 'X-Nemr-Request': '1' };
  const api = (path, opts = {}) => fetch('/api' + path, { ...opts, headers: { ...H, ...(opts.headers || {}) } });
  const status = (text, cls) => { const s = $('status'); s.textContent = text; s.className = cls || ''; };
  const esc = t => String(t ?? '').replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const human = n => n == null ? '-' : n >= 2 ** 30 ? (n / 2 ** 30).toFixed(1) + ' GiB' : n >= 2 ** 20 ? (n / 2 ** 20).toFixed(1) + ' MiB' : n >= 1024 ? (n / 1024).toFixed(1) + ' KiB' : n + ' B';
  const ago = u => { if (u == null) return '-'; const d = Math.max(0, Math.floor(Date.now() / 1000) - u); return d < 60 ? d + 's ago' : d < 3600 ? Math.floor(d / 60) + 'm ago' : d < 86400 ? Math.floor(d / 3600) + 'h ago' : Math.floor(d / 86400) + 'd ago'; };

  async function handshake() {
    const m = location.hash.match(/token=([0-9a-f]{64})/);
    if (m) {
      const r = await fetch('/auth/session', { method: 'POST', headers: { ...H, 'X-Nemr-Token': m[1] } });
      history.replaceState(null, '', '/');
      if (!r.ok) { status('handshake refused (' + r.status + '): start the UI again with nemr ui', 'bad'); return false; }
    }
    const p = await api('/ping');
    if (!p.ok) { status('no session (' + p.status + '): start the UI again with nemr ui', 'bad'); return false; }
    return true;
  }

  function showLogin(defaultServer) {
    $('list').hidden = true; $('logout').hidden = true; $('register').hidden = true; $('recovery').hidden = true;
    $('who').textContent = 'not logged in';
    const f = $('login'); f.hidden = false;
    if (!f.server.value) f.server.value = defaultServer || '';
    status('');
    f.email.focus();
  }
  function showRegister() {
    const l = $('login'), f = $('register');
    l.hidden = true; f.hidden = false; $('recovery').hidden = true;
    if (!f.server.value) f.server.value = l.server.value;
    status('');
    f.email.focus();
  }

  async function showList(me) {
    $('login').hidden = true; $('logout').hidden = false;
    $('who').textContent = me.email + ' · ' + me.server;
    $('list').hidden = false;
    await refresh();
  }

  async function refresh() {
    status('loading sessions…');
    const r = await api('/sessions');
    if (r.status === 401) { const w = await api('/whoami').then(x => x.json()); showLogin(w.default_server); return; }
    if (!r.ok) { const e = await r.json().catch(() => ({})); status('could not list sessions: ' + (e.error || r.status), 'bad'); return; }
    const d = await r.json();
    const rows = $('rows'); rows.innerHTML = '';
    if (!d.rows.length) rows.innerHTML = '<tr><td colspan="8" class="muted">no sessions anywhere. Create one with: nemr create &lt;name&gt; --size 2GB</td></tr>';
    for (const s of d.rows) {
      const state = s.where === 'remote' ? '<span class="muted">not here</span>' : s.running ? 'running' : 'stopped';
      let open = '<span class="muted">-</span>';
      if (s.held_by) open = s.held_by === d.this_machine ? 'this machine' : '<span class="warn">' + esc(s.held_by) + '</span>';
      let action = '';
      if (s.where === 'remote' && s.has_bundle) action = '<button data-pull="' + esc(s.name) + '">pull &amp; start</button>';
      else if (s.where === 'remote') action = '<span class="muted">no bundle yet</span>';
      else if (!s.running) action = '<button data-start="' + esc(s.name) + '">start</button> <button data-push="' + esc(s.name) + '">push</button>';
      else action = '<button data-attach="' + esc(s.name) + '">attach</button> <button data-push="' + esc(s.name) + '">stop &amp; push</button>';
      rows.insertAdjacentHTML('beforeend', '<tr><td>' + esc(s.name) + '</td><td>' + esc(s.agent) + '</td><td><span class="pill ' + esc(s.where) + '">' + esc(s.where) + '</span></td><td>' + state + '</td><td>' + human(s.size_bytes) + '</td><td>' + ago(s.updated_at_unix) + '</td><td>' + esc(s.last_machine || '-') + '</td><td>' + open + '</td><td>' + action + '</td></tr>');
    }
    $('localnote').textContent = d.local_available ? '' : 'daemon unreachable: showing the server index only (' + d.local_error + ')';
    $('localnote').className = 'note' + (d.local_available ? '' : ' warn');
    status(d.rows.length + ' session' + (d.rows.length === 1 ? '' : 's'));
  }

  $('login').addEventListener('submit', async ev => {
    ev.preventDefault();
    const f = ev.target;
    status('logging in… (deriving the key takes a moment)');
    f.querySelector('button').disabled = true;
    try {
      const r = await api('/login', { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server: f.server.value, email: f.email.value, password: f.password.value }) });
      const d = await r.json().catch(() => ({}));
      if (!r.ok) { status('login refused: ' + (d.error || r.status), 'bad'); return; }
      f.password.value = '';
      await showList(d);
    } finally { f.querySelector('button').disabled = false; }
  });
  $('to-register').addEventListener('click', ev => { ev.preventDefault(); showRegister(); });
  $('to-login').addEventListener('click', ev => { ev.preventDefault(); showLogin($('register').server.value); });
  $('register').addEventListener('submit', async ev => {
    ev.preventDefault();
    const f = ev.target;
    if (f.password.value !== f.again.value) { status('the two passwords differ', 'bad'); return; }
    status('registering… (deriving the keys takes a moment)');
    f.querySelector('button').disabled = true;
    try {
      const r = await api('/register', { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ server: f.server.value, email: f.email.value, password: f.password.value }) });
      const d = await r.json().catch(() => ({}));
      if (!r.ok) { status('registration refused: ' + (d.error || r.status), 'bad'); return; }
      f.password.value = ''; f.again.value = '';
      f.hidden = true; $('recovery').hidden = false;
      $('code').textContent = d.recovery_code;
      $('abandon').textContent = 'If you leave this page before confirming: ' + d.if_abandoned;
      status('');
      $('confirm').code.focus();
    } finally { f.querySelector('button').disabled = false; }
  });
  $('confirm').addEventListener('submit', async ev => {
    ev.preventDefault();
    const f = ev.target;
    const r = await api('/register/confirm', { method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ code: f.code.value }) });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) { status(d.error || ('confirmation failed (' + r.status + ')'), 'bad'); return; }
    $('recovery').hidden = true; $('code').textContent = '';
    status('recovery confirmed; the account is active');
    await showList(d);
  });
  $('logout').addEventListener('click', async () => {
    const r = await api('/logout', { method: 'POST' });
    const d = await r.json().catch(() => ({}));
    const w = await api('/whoami').then(x => x.json());
    showLogin(w.default_server);
    if (d.revoke_failed) status('logged out here; the server could not be told (' + d.revoke_failed + ')', 'warn');
    else status('logged out');
  });
  $('refresh').addEventListener('click', refresh);

  // --- step 3: pull-and-start, and start, as jobs the page watches ---
  let pulling = null, pushing = null;
  function openJob(title) {
    $('job').hidden = false; $('jobtitle').textContent = title;
    $('joblog').textContent = ''; $('jobresult').textContent = ''; $('jobresult').className = '';
  }
  async function watch(id) {
    for (;;) {
      const r = await api('/jobs/' + id);
      if (!r.ok) { $('jobresult').textContent = 'lost the job (' + r.status + ')'; $('jobresult').className = 'bad'; return null; }
      const j = await r.json();
      $('joblog').textContent = j.lines.map(l => l.text).join('\n');
      if (j.done) return j;
      await new Promise(res => setTimeout(res, 400));
    }
  }
  async function runJob(title, path, body) {
    openJob(title);
    const r = await api(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body || {}) });
    const d = await r.json().catch(() => ({}));
    if (!r.ok) { $('jobresult').textContent = d.error || ('refused (' + r.status + ')'); $('jobresult').className = 'bad'; return null; }
    const j = await watch(d.job);
    if (!j) return null;
    if (j.ok) { $('jobresult').textContent = 'done'; await refresh(); }
    else { $('jobresult').textContent = j.error; $('jobresult').className = 'bad'; }
    return j;
  }
  // --- step 4: attach — the daemon's stream over a WebSocket, drawn by xterm.js ---
  let term = null, fit = null, ws = null;
  async function attach(name) {
    if (ws) { ws.close(); ws = null; }
    $('job').hidden = true;
    $('attach').hidden = false; $('attachtitle').textContent = name; $('attachnote').textContent = 'connecting…';
    if (!term) {
      term = new Terminal({ cursorBlink: true, fontSize: 14, scrollback: 5000 });
      fit = new FitAddon.FitAddon(); term.loadAddon(fit); term.open($('term'));
      term.onData(d => { if (ws && ws.readyState === 1) ws.send(new TextEncoder().encode(d)); });
      term.onResize(({ rows, cols }) => { if (ws && ws.readyState === 1) ws.send(JSON.stringify({ type: 'resize', rows, cols })); });
      window.addEventListener('resize', () => fit && fit.fit());
    }
    term.reset(); fit.fit();
    const t = await api('/sessions/' + encodeURIComponent(name) + '/attach-ticket', { method: 'POST' });
    if (!t.ok) { $('attachnote').textContent = 'refused (' + t.status + ')'; return; }
    const { ticket } = await t.json();
    const proto = location.protocol === 'https:' ? 'wss://' : 'ws://';
    ws = new WebSocket(proto + location.host + '/ws/attach/' + encodeURIComponent(name) + '?ticket=' + ticket);
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => { ws.send(JSON.stringify({ type: 'start', rows: term.rows, cols: term.cols })); $('attachnote').textContent = 'attached — type as in nemr attach; exit the shell or detach'; term.focus(); };
    ws.onmessage = ev => {
      if (typeof ev.data === 'string') { const m = JSON.parse(ev.data); $('attachnote').textContent = m.type === 'exit' ? 'the shell exited (' + m.code + ')' : 'error: ' + m.message; $('attachnote').className = 'note ' + (m.type === 'exit' ? '' : 'bad'); return; }
      term.write(new Uint8Array(ev.data));
    };
    ws.onclose = () => { if ($('attachnote').textContent.startsWith('attached')) $('attachnote').textContent = 'disconnected'; ws = null; refresh(); };
  }
  $('detach').addEventListener('click', () => { if (ws) ws.close(); $('attach').hidden = true; });
  $('rows').addEventListener('click', ev => {
    const b = ev.target.closest('button'); if (!b) return;
    if (b.dataset.attach) attach(b.dataset.attach);
    if (b.dataset.start) runJob('start ' + b.dataset.start, '/sessions/' + encodeURIComponent(b.dataset.start) + '/start');
    if (b.dataset.pull) {
      pulling = b.dataset.pull;
      openJob('pull & start ' + pulling);
      const f = $('pullform'); f.hidden = false; f.take_over.checked = false; f.password.value = ''; f.password.focus();
    }
    if (b.dataset.push) {
      pushing = b.dataset.push;
      if (ws) { ws.close(); $('attach').hidden = true; }
      openJob((b.textContent.startsWith('stop') ? 'stop & push ' : 'push ') + pushing);
      const f = $('pushform'); f.hidden = false; f.release.checked = true; f.take_over.checked = false; f.password.value = ''; f.password.focus();
    }
  });
  $('jobcancel').addEventListener('click', () => { $('pullform').hidden = true; $('job').hidden = true; pulling = null; });
  $('pushcancel').addEventListener('click', () => { $('pushform').hidden = true; $('job').hidden = true; pushing = null; });
  $('pushform').addEventListener('submit', async ev => {
    ev.preventDefault();
    const f = ev.target, name = pushing;
    const body = { password: f.password.value, release: f.release.checked, take_over: f.take_over.checked };
    f.hidden = true; f.password.value = '';
    const j = await runJob('push ' + name, '/sessions/' + encodeURIComponent(name) + '/push', body);
    if (j && !j.ok && j.held_by) {
      $('jobresult').insertAdjacentHTML('beforeend', ' <button id="pushtakeover">take over from ' + esc(j.held_by) + '</button>');
      $('pushtakeover').addEventListener('click', () => { pushing = name; f.hidden = false; f.take_over.checked = true; f.password.focus(); });
    }
  });
  $('pullform').addEventListener('submit', async ev => {
    ev.preventDefault();
    const f = ev.target, name = pulling;
    const body = { password: f.password.value, take_over: f.take_over.checked };
    f.hidden = true; f.password.value = '';
    const j = await runJob('pull & start ' + name, '/sessions/' + encodeURIComponent(name) + '/pull', body);
    if (j && !j.ok && j.held_by) {
      // The CLI's --take-over, offered where the CLI offers it: on refusal, naming the holder.
      $('jobresult').insertAdjacentHTML('beforeend', ' <button id="takeover">take over from ' + esc(j.held_by) + '</button>');
      $('takeover').addEventListener('click', () => { pulling = name; f.hidden = false; f.take_over.checked = true; f.password.focus(); });
    }
  });

  (async () => {
    if (!await handshake()) return;
    const w = await api('/whoami').then(x => x.json());
    if (w.logged_in) await showList(w); else showLogin(w.default_server);
  })();
})();
</script>"##;
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
        let engine: Arc<dyn UiEngine> = Arc::new(crate::daemon::DaemonEngine::new(
            tokio::runtime::Handle::current(),
        ));
        let state = Arc::new(UiState::new(port, random_bytes(), engine));
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
    use crate::engine_cli::LocalProject;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    const PORT: u16 = 4321;
    fn token() -> [u8; 32] {
        [7u8; 32]
    }
    /// The engine the router's tests run against: a list the test states,
    /// and a record of every import and start — with the bundle's bytes and
    /// the mode of the directory it arrived in, the F-92 property.
    #[derive(Default)]
    struct FakeEngine {
        projects: Mutex<Vec<LocalProject>>,
        imported: Mutex<Vec<(String, Vec<u8>, u32)>>,
        started: Mutex<Vec<String>>,
        exported: Mutex<Vec<String>>,
        stopped: Mutex<Vec<String>>,
    }
    impl EngineOps for FakeEngine {
        fn list(&self) -> Result<Vec<LocalProject>> {
            Ok(self.projects.lock().unwrap().clone())
        }
        fn export(&self, name: &str, dest: &std::path::Path) -> Result<()> {
            self.exported.lock().unwrap().push(name.to_string());
            std::fs::write(dest, format!("bundle-of-{name}"))?;
            Ok(())
        }
        fn import(&self, bundle: &std::path::Path, name: &str) -> Result<String> {
            use std::os::unix::fs::PermissionsExt;
            let bytes = std::fs::read(bundle)?;
            let dir_mode = std::fs::metadata(bundle.parent().unwrap())?
                .permissions()
                .mode()
                & 0o777;
            self.imported
                .lock()
                .unwrap()
                .push((name.to_string(), bytes, dir_mode));
            self.projects.lock().unwrap().push(LocalProject {
                name: name.to_string(),
                agent: "claude-code".into(),
                running: false,
                usage_known: false,
                used_bytes: 0,
            });
            Ok(name.to_string())
        }
    }
    impl UiEngine for FakeEngine {
        fn start(&self, name: &str) -> Result<()> {
            self.started.lock().unwrap().push(name.to_string());
            for p in self.projects.lock().unwrap().iter_mut() {
                if p.name == name {
                    p.running = true;
                }
            }
            Ok(())
        }
        fn stop(&self, name: &str) -> Result<String> {
            self.stopped.lock().unwrap().push(name.to_string());
            // The row goes to stopped, as the real engine's does — so a
            // push that follows sees a quiescent volume, and a push that
            // followed a stop which did NOT take is refused here too.
            for p in self.projects.lock().unwrap().iter_mut() {
                if p.name == name {
                    p.running = false;
                }
            }
            Ok("graceful".into())
        }
        /// An echo session: stdin comes back upper-cased on stdout, a resize
        /// is reported on stderr, "exit" ends it with code 7, and a session
        /// named "absent" is refused the way the daemon refuses one that is
        /// not running.
        fn attach(
            &self,
            start: AttachStart,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AttachLink>> + Send + '_>>
        {
            Box::pin(async move {
                if start.name == "absent" {
                    anyhow::bail!("project \"absent\" is not running");
                }
                let (to_session, mut rx) = tokio::sync::mpsc::channel::<AttachClient>(16);
                let (tx, from_session) = tokio::sync::mpsc::channel::<Result<AttachServer>>(16);
                let hello = format!("attached {}x{} to {}\n", start.rows, start.cols, start.name);
                tokio::spawn(async move {
                    let out = |m| AttachServer { msg: Some(m) };
                    tx.send(Ok(out(attach_server::Msg::Started(Default::default()))))
                        .await
                        .ok();
                    tx.send(Ok(out(attach_server::Msg::Stdout(hello.into_bytes()))))
                        .await
                        .ok();
                    while let Some(AttachClient { msg: Some(m) }) = rx.recv().await {
                        match m {
                            attach_client::Msg::Stdin(b) => {
                                if b == b"exit\n" {
                                    tx.send(Ok(out(attach_server::Msg::ExitCode(7)))).await.ok();
                                    return;
                                }
                                let up = String::from_utf8_lossy(&b).to_uppercase();
                                tx.send(Ok(out(attach_server::Msg::Stdout(up.into_bytes()))))
                                    .await
                                    .ok();
                            }
                            attach_client::Msg::Resize(r) => {
                                tx.send(Ok(out(attach_server::Msg::Stderr(
                                    format!("resized {}x{}\n", r.rows, r.cols).into_bytes(),
                                ))))
                                .await
                                .ok();
                            }
                            attach_client::Msg::StdinEof(_) => return,
                            attach_client::Msg::Start(_) => {}
                        }
                    }
                });
                Ok(AttachLink {
                    to_session,
                    from_session: Box::pin(tokio_stream::wrappers::ReceiverStream::new(
                        from_session,
                    )),
                    _keep: None,
                })
            })
        }
    }
    fn fake_engine(list: Vec<LocalProject>) -> Arc<FakeEngine> {
        Arc::new(FakeEngine {
            projects: Mutex::new(list),
            ..FakeEngine::default()
        })
    }
    fn app() -> (Router, Arc<UiState>) {
        app_with(fake_engine(vec![]))
    }
    fn app_with(engine: Arc<FakeEngine>) -> (Router, Arc<UiState>) {
        let state = Arc::new(UiState::new(PORT, token(), engine));
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
        let state = UiState::new(PORT, token(), fake_engine(vec![]));
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

    fn api_req(
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> HttpRequest<Body> {
        let mut b = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header("host", host())
            .header(REQUEST_HEADER, "1");
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        match body {
            Some(v) => b
                .header("content-type", "application/json")
                .body(Body::from(v.to_string()))
                .unwrap(),
            None => b.body(Body::empty()).unwrap(),
        }
    }
    async fn json_of(r: Response) -> Value {
        use http_body_util::BodyExt;
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    /// The flow's routes sit behind the same guard as `/api/ping`: without
    /// the cookie, every one of them is refused — and a login attempt without
    /// a session never reaches the KDF or the server.
    #[tokio::test]
    async fn the_flows_routes_are_behind_the_cookie_guard() {
        let (app, _) = app();
        for (method, path, body) in [
            ("GET", "/api/whoami", None),
            (
                "POST",
                "/api/login",
                Some(json!({"email": "a@b", "password": "x"})),
            ),
            ("POST", "/api/logout", None),
            ("GET", "/api/sessions", None),
            (
                "POST",
                "/api/sessions/x/pull",
                Some(json!({"password": "x"})),
            ),
            ("POST", "/api/sessions/x/start", None),
            (
                "POST",
                "/api/sessions/x/push",
                Some(json!({"password": "x"})),
            ),
            ("POST", "/api/sessions/x/stop", None),
            ("GET", "/api/jobs/abc", None),
            ("POST", "/api/sessions/x/attach-ticket", None),
        ] {
            let r = send(&app, api_req(method, path, None, body)).await;
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{method} {path}");
            // The GUARD's refusal, not the handler's own "not logged in" —
            // the two share a status and only the body tells them apart.
            use http_body_util::BodyExt;
            let body = r.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                std::str::from_utf8(&body).unwrap(),
                "no session",
                "{method} {path} was refused by something other than the guard"
            );
        }
    }

    /// The real sync server, in-process (the same Postgres the nemr-sync suite
    /// uses; `DATABASE_URL` required). The client's state lives in a private
    /// directory for this test, and the KDF runs at test cost.
    fn spawn_sync_server() -> (String, tempfile::TempDir) {
        use nemr_sync::{connect_and_migrate, router, AppState, Config, DynStore, KdfCost};
        let db = std::env::var("DATABASE_URL").expect(
            "DATABASE_URL must be set (scripts/setup_sync_test_db.sh) for the UI surface test",
        );
        let store_dir = tempfile::tempdir().unwrap();
        let store_path = store_dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let pool = connect_and_migrate(&db).await.expect("connect + migrate");
                let store: Arc<dyn DynStore> =
                    Arc::new(nemr_storage::local::LocalStore::new(store_path));
                let state = AppState {
                    pool,
                    store,
                    config: Config {
                        token_ttl: time::Duration::days(1),
                        lease_ttl: time::Duration::seconds(60),
                        server_kdf: KdfCost {
                            m_cost: 8,
                            t_cost: 1,
                            p_cost: 1,
                        },
                        max_login_failures: 100,
                        login_window: time::Duration::minutes(15),
                        bundle_prefix: "ui-surface-test-bundles".into(),
                        auth_pepper: [9u8; 32],
                    },
                };
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, router(state)).await.unwrap();
            });
        });
        let addr = rx.recv().expect("the sync server failed to start");
        (format!("http://{addr}"), store_dir)
    }

    /// The surface tests set process-wide environment (the state directory,
    /// the holder identity) and read the account file it names, so two of
    /// them running at once see each other's login. They hold this for their
    /// whole duration — the F-71 shape: serial by construction, not by a
    /// flag someone must remember.
    static SURFACE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        SURFACE.lock().await
    }

    /// The one file the bundle store holds, read back — the server's own
    /// bytes, not the client's idea of them.
    fn stored_ciphertext(store_dir: &std::path::Path) -> Vec<u8> {
        let mut found = Vec::new();
        let mut stack = vec![store_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    found.push(path);
                }
            }
        }
        assert_eq!(
            found.len(),
            1,
            "expected exactly one stored bundle: {found:?}"
        );
        std::fs::read(&found[0]).unwrap()
    }

    /// Run the CLI's blocking code off the test runtime (its HTTP client is
    /// the blocking one, which refuses to run on an async thread).
    fn blocking<T: Send>(f: impl FnOnce() -> T + Send) -> T {
        std::thread::scope(|s| s.spawn(f).join().unwrap())
    }

    /// Steps 1 and 2 of the flow, end to end against the real server:
    /// registration with the recovery code typed back (wrong code refused,
    /// registration kept; right code confirms and logs in); then a
    /// wrong password is refused and leaves the machine logged out; the right
    /// one logs in, and the stored account is what `nemr login` stores; the
    /// list then merges the server's index with the local view — a local-only
    /// running project, and a remote one that another machine holds right
    /// now, named. Logout clears it and the list is refused again.
    #[tokio::test]
    async fn login_then_the_session_list_names_the_lease_holder() {
        let _serial = serial().await;
        let (server, _store) = spawn_sync_server();
        let state_home = tempfile::tempdir().unwrap();
        // Process-wide, read by this test's own code paths only (the
        // handshake tests never touch the state directory).
        std::env::set_var("XDG_STATE_HOME", state_home.path());
        std::env::set_var("NEMR_CLOUD_KDF_FAST", "1");
        std::env::set_var("NEMR_CLOUD_HOLDER", "this-laptop");
        let email = format!("ui-{}@example.com", &hex(&random_bytes())[..12]);
        const PASSWORD: &str = "correct horse battery staple";

        let engine = fake_engine(vec![LocalProject {
            name: "here-only".into(),
            agent: "codex".into(),
            running: true,
            usage_known: true,
            used_bytes: 700,
        }]);
        let (app, _) = app_with(engine);
        let cookie = establish(&app).await;
        let c = Some(cookie.as_str());

        let who = json_of(send(&app, api_req("GET", "/api/whoami", c, None)).await).await;
        assert_eq!(who["logged_in"], false, "{who}");

        // Register through the surface, the CLI's way: the code is shown
        // once; a wrong code is refused and the registration stays pending;
        // the right one opens the envelope, confirms, and logs in.
        let nothing_pending = send(
            &app,
            api_req(
                "POST",
                "/api/register/confirm",
                c,
                Some(json!({"code": "AAAAA"})),
            ),
        )
        .await;
        assert_eq!(nothing_pending.status(), StatusCode::CONFLICT);
        let begun = send(
            &app,
            api_req(
                "POST",
                "/api/register",
                c,
                Some(json!({"server": server, "email": email, "password": PASSWORD})),
            ),
        )
        .await;
        assert_eq!(begun.status(), StatusCode::OK);
        let begun = json_of(begun).await;
        let code = begun["recovery_code"].as_str().unwrap().to_string();
        assert!(code.contains('-'), "the transcribable form: {code}");
        assert!(begun["if_abandoned"]
            .as_str()
            .unwrap()
            .contains("NOT usable"));
        let wrong_code = send(
            &app,
            api_req(
                "POST",
                "/api/register/confirm",
                c,
                Some(json!({"code": "AAAAA-BBBBB"})),
            ),
        )
        .await;
        assert_eq!(
            wrong_code.status(),
            StatusCode::BAD_REQUEST,
            "a wrong code is refused"
        );
        let who = json_of(send(&app, api_req("GET", "/api/whoami", c, None)).await).await;
        assert_eq!(who["logged_in"], false, "a wrong code logs nobody in");
        // Typed back with the transcription slips the CLI forgives.
        let typed = code.to_lowercase().replace('-', " ");
        let confirmed = send(
            &app,
            api_req(
                "POST",
                "/api/register/confirm",
                c,
                Some(json!({"code": typed})),
            ),
        )
        .await;
        assert_eq!(
            confirmed.status(),
            StatusCode::OK,
            "the shown code confirms"
        );
        let who = json_of(send(&app, api_req("GET", "/api/whoami", c, None)).await).await;
        assert_eq!(who["logged_in"], true, "register ends logged in: {who}");

        // Seed the server with a session another machine holds, then log
        // out so login is exercised from a clean machine.
        let token = blocking(|| {
            let account = crate::state::load_account().unwrap();
            let api = crate::api::Api::new(&server, Some(account.token.clone()));
            api.upsert_session(&json!({
                "name": "shared", "agent": "claude-code", "size_bytes": 4096, "last_machine": "desktop",
            }))
            .unwrap();
            let lease = api.acquire_lease("shared", "desktop").unwrap();
            assert!(lease.granted);
            core::logout().unwrap();
            account.token
        });
        assert!(!token.is_empty());
        let who = json_of(send(&app, api_req("GET", "/api/whoami", c, None)).await).await;
        assert_eq!(who["logged_in"], false, "{who}");
        let r = send(&app, api_req("GET", "/api/sessions", c, None)).await;
        assert_eq!(
            r.status(),
            StatusCode::UNAUTHORIZED,
            "the list needs a login"
        );

        let wrong = send(
            &app,
            api_req(
                "POST",
                "/api/login",
                c,
                Some(json!({"server": server, "email": email, "password": "not it"})),
            ),
        )
        .await;
        assert_eq!(
            wrong.status(),
            StatusCode::UNAUTHORIZED,
            "a wrong password is refused"
        );
        let who = json_of(send(&app, api_req("GET", "/api/whoami", c, None)).await).await;
        assert_eq!(who["logged_in"], false, "still logged out after a refusal");

        let right = send(
            &app,
            api_req(
                "POST",
                "/api/login",
                c,
                Some(json!({"server": server, "email": email, "password": PASSWORD})),
            ),
        )
        .await;
        assert_eq!(right.status(), StatusCode::OK);
        let who = json_of(send(&app, api_req("GET", "/api/whoami", c, None)).await).await;
        assert_eq!(who["logged_in"], true, "{who}");
        assert_eq!(who["email"], email.as_str());
        let stored = crate::state::load_account().expect("the login stored the account");
        assert_eq!(stored.server, server);
        assert!(
            !stored.password_envelope.is_empty(),
            "the sealed envelope is stored; the key is not"
        );

        let list = send(&app, api_req("GET", "/api/sessions", c, None)).await;
        assert_eq!(list.status(), StatusCode::OK);
        let list = json_of(list).await;
        assert_eq!(list["local_available"], true, "{list}");
        assert_eq!(list["this_machine"], "this-laptop");
        let rows = list["rows"].as_array().unwrap();
        let row = |name: &str| {
            rows.iter()
                .find(|r| r["name"] == name)
                .unwrap_or_else(|| panic!("no row {name} in {list}"))
                .clone()
        };
        let here = row("here-only");
        assert_eq!(here["where"], "local");
        assert_eq!(here["running"], true);
        assert_eq!(here["held_by"], Value::Null);
        let shared = row("shared");
        assert_eq!(shared["where"], "remote");
        assert_eq!(shared["running"], false);
        assert_eq!(
            shared["held_by"], "desktop",
            "open on another machine must be named: {shared}"
        );
        assert!(shared["lease_expires_at_unix"].as_i64().unwrap() > 0);

        let out = json_of(send(&app, api_req("POST", "/api/logout", c, None)).await).await;
        assert_eq!(out["was_logged_in"], true);
        assert_eq!(
            out["revoke_failed"],
            Value::Null,
            "the server was told: {out}"
        );
        let r = send(&app, api_req("GET", "/api/sessions", c, None)).await;
        assert_eq!(
            r.status(),
            StatusCode::UNAUTHORIZED,
            "logged out: the list is refused again"
        );
        assert!(
            crate::state::load_account().is_err(),
            "the account is cleared"
        );
    }

    /// Poll a job to completion, the way the page does.
    async fn finish(app: &Router, cookie: &str, started: Response) -> Value {
        assert_eq!(started.status(), StatusCode::OK);
        let id = json_of(started).await["job"].as_str().unwrap().to_string();
        for _ in 0..600 {
            let j = json_of(
                send(
                    app,
                    api_req("GET", &format!("/api/jobs/{id}"), Some(cookie), None),
                )
                .await,
            )
            .await;
            if j["done"] == true {
                return j;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("job {id} never finished");
    }

    /// Step 3, end to end against the real server: a bundle another machine
    /// pushed comes down through the surface — refused while that machine
    /// holds the lease (the holder named, so the page can offer the
    /// take-over), refused with a wrong password before anything is
    /// imported, and with the take-over: downloaded, decrypted to the bytes
    /// that were pushed, imported from a directory only this user can
    /// enter, started, and the lease now held by this machine. Then a
    /// session already here is started on its own.
    #[tokio::test]
    async fn pull_and_start_through_the_surface() {
        let _serial = serial().await;
        let (server, _store) = spawn_sync_server();
        let state_home = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", state_home.path());
        std::env::set_var("NEMR_CLOUD_KDF_FAST", "1");
        std::env::set_var("NEMR_CLOUD_HOLDER", "this-laptop");
        // The heartbeat holder would be spawned as THIS test binary and read
        // its verb as a test filter; the lease's server-side state is what
        // the assertions read, so the holder can be a no-op here.
        std::env::set_var("NEMR_CLOUD_HOLDER_BIN", "/bin/true");
        let email = format!("pull-{}@example.com", &hex(&random_bytes())[..12]);
        const PASSWORD: &str = "correct horse battery staple";
        let plaintext = b"the session, as exported on the desktop".to_vec();

        // Enrol here, then act as "desktop": push a bundle and keep the lease.
        let pushed = plaintext.clone();
        blocking(|| {
            let pending = core::register_begin(&server, &email, PASSWORD).unwrap();
            let mk = core::register_check_code(&pending, &pending.recovery_code.display()).unwrap();
            let account = core::register_confirm(&pending, &mk).unwrap();
            let api = crate::api::Api::new(&server, Some(account.token.clone()));
            api.upsert_session(&json!({
                "name": "shared", "agent": "claude-code", "size_bytes": pushed.len(), "last_machine": "desktop",
            }))
            .unwrap();
            let lease = api.acquire_lease("shared", "desktop").unwrap();
            assert!(lease.granted);
            let mk = crate::keys::master_key(&account, PASSWORD).unwrap();
            api.upload_bundle(
                "shared",
                "desktop",
                lease.fence,
                nemr_crypto::encrypt_bundle(&mk, &pushed),
            )
            .unwrap();
        });

        let engine = fake_engine(vec![LocalProject {
            name: "here-only".into(),
            agent: "codex".into(),
            running: false,
            usage_known: true,
            used_bytes: 700,
        }]);
        let (app, _) = app_with(engine.clone());
        let cookie = establish(&app).await;
        let c = Some(cookie.as_str());
        let pull = |body: Value| api_req("POST", "/api/sessions/shared/pull", c, Some(body));

        // Held by the desktop: refused, holder named, nothing imported.
        let j = finish(
            &app,
            &cookie,
            send(&app, pull(json!({"password": PASSWORD}))).await,
        )
        .await;
        assert_eq!(j["ok"], false, "{j}");
        assert_eq!(
            j["held_by"], "desktop",
            "the page needs the holder to offer the take-over: {j}"
        );
        assert!(
            j["error"].as_str().unwrap().contains("held by desktop"),
            "{j}"
        );
        assert!(
            engine.imported.lock().unwrap().is_empty(),
            "nothing imported under a refused lease"
        );

        // Wrong password: refused before any download is decrypted or imported.
        let j = finish(
            &app,
            &cookie,
            send(&app, pull(json!({"password": "nope", "take_over": true}))).await,
        )
        .await;
        assert_eq!(j["ok"], false);
        assert!(
            j["error"].as_str().unwrap().contains("wrong password"),
            "{j}"
        );
        assert!(engine.imported.lock().unwrap().is_empty());
        assert!(engine.started.lock().unwrap().is_empty());

        // Take over: the bytes the desktop pushed, imported privately, started.
        let j = finish(
            &app,
            &cookie,
            send(&app, pull(json!({"password": PASSWORD, "take_over": true}))).await,
        )
        .await;
        assert_eq!(j["ok"], true, "{j}");
        let lines: Vec<String> = j["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["text"].as_str().unwrap().to_string())
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("took over the lease from desktop")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("decrypting")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("\"shared\" is running")),
            "{lines:?}"
        );
        let imported = engine.imported.lock().unwrap().clone();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].0, "shared");
        assert_eq!(
            imported[0].1, plaintext,
            "the bytes the desktop pushed, decrypted here"
        );
        assert_eq!(
            imported[0].2, 0o700,
            "the plaintext sat in a directory only this user can enter (F-92)"
        );
        assert_eq!(*engine.started.lock().unwrap(), vec!["shared".to_string()]);

        // The list now says: here, running, and the lease is this machine's.
        let list = json_of(send(&app, api_req("GET", "/api/sessions", c, None)).await).await;
        let shared = list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "shared")
            .unwrap()
            .clone();
        assert_eq!(shared["where"], "both");
        assert_eq!(shared["running"], true);
        assert_eq!(shared["held_by"], "this-laptop", "{shared}");

        // A session already here starts on its own, no password involved.
        let j = finish(
            &app,
            &cookie,
            send(
                &app,
                api_req("POST", "/api/sessions/here-only/start", c, None),
            )
            .await,
        )
        .await;
        assert_eq!(j["ok"], true, "{j}");
        assert_eq!(
            *engine.started.lock().unwrap(),
            vec!["shared".to_string(), "here-only".to_string()]
        );
        let missing = send(&app, api_req("GET", "/api/jobs/nope", c, None)).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    /// Serve the router on an ephemeral port for a real WebSocket client.
    async fn serve_for_ws(app: Router) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }
    /// A WebSocket handshake the way a browser makes it, with the headers
    /// the test chooses; `Err(status)` is the server's refusal.
    async fn ws_connect(
        port: u16,
        name: &str,
        ticket: &str,
        cookie: Option<&str>,
        origin: Option<&str>,
    ) -> std::result::Result<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, StatusCode>
    {
        use tokio_tungstenite::tungstenite;
        let mut b = tungstenite::http::Request::builder()
            .uri(format!(
                "ws://127.0.0.1:{port}/ws/attach/{name}?ticket={ticket}"
            ))
            .header("Host", host())
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            );
        if let Some(c) = cookie {
            b = b.header("Cookie", c);
        }
        if let Some(o) = origin {
            b = b.header("Origin", o);
        }
        let req = b.body(()).unwrap();
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        match tokio_tungstenite::client_async(req, tcp).await {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::Error::Http(resp)) => {
                Err(StatusCode::from_u16(resp.status().as_u16()).unwrap())
            }
            Err(e) => panic!("handshake failed oddly: {e}"),
        }
    }
    async fn ticket_for(app: &Router, cookie: &str, name: &str) -> String {
        let r = send(
            app,
            api_req(
                "POST",
                &format!("/api/sessions/{name}/attach-ticket"),
                Some(cookie),
                None,
            ),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        json_of(r).await["ticket"].as_str().unwrap().to_string()
    }

    /// THE gate for the attach WebSocket: without the cookie it is refused;
    /// without an Origin (a non-browser, or a page that stripped it) it is
    /// refused; with a cross-site Origin it is refused; without a ticket, or
    /// with one minted for another session name, or with one already spent,
    /// it is refused; and the one right handshake is accepted.
    #[tokio::test]
    async fn the_attach_websocket_needs_cookie_origin_and_a_fresh_ticket() {
        let (app, _) = app();
        let cookie = establish(&app).await;
        let port = serve_for_ws(app.clone()).await;
        let origin = format!("http://127.0.0.1:{PORT}");

        let t = ticket_for(&app, &cookie, "sess").await;
        assert_eq!(
            ws_connect(port, "sess", &t, None, Some(&origin))
                .await
                .err(),
            Some(StatusCode::UNAUTHORIZED),
            "no cookie"
        );
        assert_eq!(
            ws_connect(port, "sess", &t, Some(&cookie), None)
                .await
                .err(),
            Some(StatusCode::FORBIDDEN),
            "no Origin"
        );
        assert_eq!(
            ws_connect(port, "sess", &t, Some(&cookie), Some("http://evil.example"))
                .await
                .err(),
            Some(StatusCode::FORBIDDEN),
            "cross-site Origin"
        );
        assert_eq!(
            ws_connect(port, "sess", "", Some(&cookie), Some(&origin))
                .await
                .err(),
            Some(StatusCode::UNAUTHORIZED),
            "no ticket"
        );
        assert_eq!(
            ws_connect(port, "other", &t, Some(&cookie), Some(&origin))
                .await
                .err(),
            Some(StatusCode::UNAUTHORIZED),
            "a ticket for another session name"
        );
        // The refusals above spent nothing that was right; the ticket is
        // still unspent, so the right handshake succeeds — and the ticket is
        // then gone.
        let t = ticket_for(&app, &cookie, "sess").await;
        let ws = ws_connect(port, "sess", &t, Some(&cookie), Some(&origin)).await;
        assert!(ws.is_ok(), "the right handshake: {:?}", ws.err());
        assert_eq!(
            ws_connect(port, "sess", &t, Some(&cookie), Some(&origin))
                .await
                .err(),
            Some(StatusCode::UNAUTHORIZED),
            "a spent ticket"
        );
        // And a ticket minted by one page session does not open another's.
        let (app2, _) = super::tests::app();
        let cookie2 = establish(&app2).await;
        let port2 = serve_for_ws(app2.clone()).await;
        let t2 = ticket_for(&app2, &cookie2, "sess").await;
        assert_eq!(
            ws_connect(
                port2,
                "sess",
                &t2,
                Some("nemr_session=deadbeef"),
                Some(&origin)
            )
            .await
            .err(),
            Some(StatusCode::UNAUTHORIZED),
            "a ticket does not stand in for the cookie"
        );
    }

    /// The next binary frame's bytes, as text; a text frame here is a fault.
    async fn next_bytes(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> String {
        use tokio_tungstenite::tungstenite::Message as WsMsg;
        // Bounded: a bridge that drops a frame must FAIL this, not stall it —
        // the first form of this helper hung under exactly that mutation.
        loop {
            match next_frame(ws).await {
                WsMsg::Binary(b) => return String::from_utf8(b.to_vec()).unwrap(),
                WsMsg::Text(t) => panic!("unexpected text frame: {t}"),
                _ => continue,
            }
        }
    }
    /// The next frame within five seconds, or a failed test.
    async fn next_frame(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> tokio_tungstenite::tungstenite::Message {
        tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("no frame arrived within five seconds")
            .expect("the socket closed before the expected frame")
            .unwrap()
    }

    /// The bridge, both ways: the page's size reaches the session's start;
    /// typed bytes come back through the session as stdout; a resize
    /// reaches the session; the shell's exit closes the socket with its
    /// code; and a session the daemon refuses is refused in the first frame.
    #[tokio::test]
    async fn the_attach_bridge_carries_bytes_both_ways_and_the_exit() {
        use tokio_tungstenite::tungstenite::Message as WsMsg;
        let (app, _) = app();
        let cookie = establish(&app).await;
        let port = serve_for_ws(app.clone()).await;
        let origin = format!("http://127.0.0.1:{PORT}");

        let t = ticket_for(&app, &cookie, "sess").await;
        let mut ws = ws_connect(port, "sess", &t, Some(&cookie), Some(&origin))
            .await
            .unwrap();
        ws.send(WsMsg::Text(
            json!({"type": "start", "rows": 24, "cols": 80})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(next_bytes(&mut ws).await, "attached 24x80 to sess\n");
        ws.send(WsMsg::Binary(b"hello".to_vec().into()))
            .await
            .unwrap();
        assert_eq!(
            next_bytes(&mut ws).await,
            "HELLO",
            "typed bytes went down and came back up"
        );
        ws.send(WsMsg::Text(
            json!({"type": "resize", "rows": 50, "cols": 132})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        assert_eq!(
            next_bytes(&mut ws).await,
            "resized 50x132\n",
            "the resize reached the session"
        );
        ws.send(WsMsg::Binary(b"exit\n".to_vec().into()))
            .await
            .unwrap();
        let last = loop {
            match next_frame(&mut ws).await {
                WsMsg::Text(t) => break t.to_string(),
                _ => continue,
            }
        };
        assert_eq!(
            serde_json::from_str::<Value>(&last).unwrap(),
            json!({"type": "exit", "code": 7})
        );
        assert!(
            matches!(
                ws.next().await,
                None | Some(Ok(WsMsg::Close(_))) | Some(Err(_))
            ),
            "the socket closes after the exit"
        );

        // A session the daemon refuses: the refusal is the first frame.
        let t = ticket_for(&app, &cookie, "absent").await;
        let mut ws = ws_connect(port, "absent", &t, Some(&cookie), Some(&origin))
            .await
            .unwrap();
        ws.send(WsMsg::Text(
            json!({"type": "start", "rows": 24, "cols": 80})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let first = loop {
            match next_frame(&mut ws).await {
                WsMsg::Text(t) => break t.to_string(),
                _ => continue,
            }
        };
        let first: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["type"], "error");
        assert!(
            first["message"].as_str().unwrap().contains("not running"),
            "{first}"
        );
    }

    /// Step 5, end to end against the real server: a running session is
    /// stopped first (the export needs a quiescent volume, and `core::push`
    /// refuses a running project), exported, encrypted here, uploaded — and
    /// what the server stores decrypts, with this account's key, to exactly
    /// the bytes the engine exported. A wrong password is refused before
    /// anything is stopped or uploaded; the lease is released by default,
    /// so another machine can take it; and the row the pull side reads then
    /// says the bundle is there.
    #[tokio::test]
    async fn stop_and_push_through_the_surface() {
        let _serial = serial().await;
        let (server, store) = spawn_sync_server();
        let state_home = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", state_home.path());
        std::env::set_var("NEMR_CLOUD_KDF_FAST", "1");
        std::env::set_var("NEMR_CLOUD_HOLDER", "this-laptop");
        std::env::set_var("NEMR_CLOUD_HOLDER_BIN", "/bin/true");
        let email = format!("push-{}@example.com", &hex(&random_bytes())[..12]);
        const PASSWORD: &str = "correct horse battery staple";

        blocking(|| {
            let pending = core::register_begin(&server, &email, PASSWORD).unwrap();
            let mk = core::register_check_code(&pending, &pending.recovery_code.display()).unwrap();
            core::register_confirm(&pending, &mk).unwrap();
        });

        let engine = fake_engine(vec![LocalProject {
            name: "work".into(),
            agent: "claude-code".into(),
            running: true,
            usage_known: true,
            used_bytes: 4096,
        }]);
        let (app, _) = app_with(engine.clone());
        let cookie = establish(&app).await;
        let c = Some(cookie.as_str());
        let push = |body: Value| api_req("POST", "/api/sessions/work/push", c, Some(body));

        // A wrong password: refused before the session is stopped or
        // anything is uploaded — the key is derived before any of it.
        let j = finish(
            &app,
            &cookie,
            send(&app, push(json!({"password": "nope"}))).await,
        )
        .await;
        assert_eq!(j["ok"], false, "{j}");
        assert!(
            j["error"].as_str().unwrap().contains("wrong password"),
            "{j}"
        );
        assert!(
            engine.stopped.lock().unwrap().is_empty(),
            "a refused push must not have stopped the session"
        );
        assert!(engine.exported.lock().unwrap().is_empty());

        // The real one: stopped, exported, encrypted, uploaded, released.
        let j = finish(
            &app,
            &cookie,
            send(&app, push(json!({"password": PASSWORD}))).await,
        )
        .await;
        assert_eq!(j["ok"], true, "{j}");
        let lines: Vec<String> = j["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["text"].as_str().unwrap().to_string())
            .collect();
        assert!(
            lines.iter().any(|l| l.contains("stopping \"work\"")),
            "the running session is stopped first: {lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("encrypting")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("lease released")),
            "the lease is released by default from the browser: {lines:?}"
        );
        assert_eq!(*engine.stopped.lock().unwrap(), vec!["work".to_string()]);
        assert_eq!(*engine.exported.lock().unwrap(), vec!["work".to_string()]);

        // THE property: what the server stores is this session, encrypted —
        // it decrypts with this account's key to exactly what the engine
        // exported, and the ciphertext is not the plaintext.
        let stored = stored_ciphertext(store.path());
        let plaintext = blocking(|| {
            let account = crate::state::load_account().unwrap();
            let mk = crate::keys::master_key(&account, PASSWORD).unwrap();
            nemr_crypto::decrypt_bundle(&mk, &stored).expect("the stored bundle decrypts")
        });
        assert_eq!(
            plaintext,
            b"bundle-of-work".to_vec(),
            "the server holds the bytes the engine exported"
        );
        assert!(
            !stored.windows(6).any(|w| w == b"bundle"),
            "the stored bytes are ciphertext, not the plaintext"
        );

        // And the list now offers it to the other machine: a bundle, and
        // nobody holding it.
        let list = json_of(send(&app, api_req("GET", "/api/sessions", c, None)).await).await;
        let work = list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "work")
            .unwrap()
            .clone();
        assert_eq!(work["has_bundle"], true, "{work}");
        assert_eq!(work["running"], false, "the row shows it stopped: {work}");
        assert_eq!(work["held_by"], Value::Null, "released: {work}");

        // A stop on its own, for a session not being handed on.
        engine.projects.lock().unwrap()[0].running = true;
        let j = finish(
            &app,
            &cookie,
            send(&app, api_req("POST", "/api/sessions/work/stop", c, None)).await,
        )
        .await;
        assert_eq!(j["ok"], true, "{j}");
        assert_eq!(
            *engine.stopped.lock().unwrap(),
            vec!["work".to_string(), "work".to_string()]
        );
    }
}
