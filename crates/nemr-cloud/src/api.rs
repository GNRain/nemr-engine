//! Blocking HTTP client for the sync server's API.
//!
//! Thin by intent: every method maps to one endpoint, errors carry the server's
//! own message (the server's errors are deliberately terse — surfacing them
//! verbatim leaks nothing), and lease conflicts are a distinct error variant so
//! callers can react to "you lost the lease" differently from "network down".

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

pub struct Api {
    http: reqwest::blocking::Client,
    server: String,
    token: Option<String>,
}

/// A 409 from a lease-gated endpoint: the caller does not hold the lease.
/// Separate from other failures because the required reaction — stop writing —
/// is different from a retry.
#[derive(Debug)]
pub struct LeaseLost(pub String);

impl std::fmt::Display for LeaseLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for LeaseLost {}

#[derive(Deserialize)]
pub struct KdfParamsResponse {
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
}

#[derive(Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub password_envelope: String,
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
}

/// A server-index row. Only what the client renders; serde skips the rest of
/// the server's fields, so the server may grow its response freely.
#[derive(Deserialize, Clone)]
pub struct SessionEntry {
    pub name: String,
    pub agent: String,
    pub size_bytes: i64,
    pub last_machine: Option<String>,
    pub has_bundle: bool,
    pub ciphertext_bytes: Option<i64>,
    pub updated_at_unix: i64,
    /// The live lease holder, if any (server ≥ SPEC 1.108); absent from an
    /// older server, which serde reads as `None`.
    #[serde(default)]
    pub held_by: Option<String>,
    #[serde(default)]
    pub lease_expires_at_unix: Option<i64>,
}

#[derive(Deserialize)]
pub struct LeaseResponse {
    pub granted: bool,
    pub holder: String,
    pub fence: i64,
    pub expires_at_unix: i64,
    /// The lease policy's full TTL, reported by the server so the heartbeat is
    /// paced off policy rather than off a partly-elapsed lease (F-92).
    #[serde(default)]
    pub ttl_seconds: i64,
}

impl Api {
    pub fn new(server: &str, token: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("constructing an HTTP client cannot fail with static config");
        Api {
            http,
            server: server.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server)
    }

    fn auth(&self, req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// Decode a response: 2xx parses, anything else becomes an error carrying
    /// the server's own message; a 409 is [`LeaseLost`] when `lease_gated`.
    fn decode<T: serde::de::DeserializeOwned>(
        resp: reqwest::blocking::Response,
        what: &str,
        lease_gated: bool,
    ) -> Result<T> {
        let status = resp.status();
        if status.is_success() {
            return resp.json().with_context(|| format!("decoding {what}"));
        }
        let msg = resp
            .json::<serde_json::Value>()
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("HTTP {status}"));
        if lease_gated && status == reqwest::StatusCode::CONFLICT {
            return Err(anyhow!(LeaseLost(msg)));
        }
        Err(anyhow!("{what}: {msg}"))
    }

    pub fn kdf_params(&self, email: &str) -> Result<KdfParamsResponse> {
        let resp = self
            .http
            .post(self.url("/v1/auth/params"))
            .json(&json!({ "email": email }))
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "fetching KDF parameters", false)
    }

    pub fn register(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .http
            .post(self.url("/v1/register"))
            .json(body)
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "registering", false)
    }

    pub fn confirm_recovery(&self, email: &str, ack_b64: &str) -> Result<serde_json::Value> {
        let resp = self
            .http
            .post(self.url("/v1/recovery/confirm"))
            .json(&json!({ "email": email, "recovery_ack_hash": ack_b64 }))
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "confirming recovery", false)
    }

    pub fn login(&self, email: &str, auth_key_b64: &str) -> Result<LoginResponse> {
        let resp = self
            .http
            .post(self.url("/v1/login"))
            .json(&json!({ "email": email, "auth_key": auth_key_b64 }))
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "logging in", false)
    }

    pub fn logout(&self) -> Result<()> {
        let resp = self
            .auth(self.http.post(self.url("/v1/logout")))
            .send()
            .context("reaching the server")?;
        Self::decode::<serde_json::Value>(resp, "logging out", false).map(|_| ())
    }

    pub fn sessions(&self) -> Result<Vec<SessionEntry>> {
        let resp = self
            .auth(self.http.get(self.url("/v1/sessions")))
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "listing sessions", false)
    }

    pub fn upsert_session(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let resp = self
            .auth(self.http.post(self.url("/v1/sessions")))
            .json(body)
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "updating the session index", false)
    }

    pub fn acquire_lease(&self, name: &str, holder: &str) -> Result<LeaseResponse> {
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/v1/sessions/{name}/lease"))),
            )
            .json(&json!({ "holder": holder }))
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "acquiring the lease", false)
    }

    pub fn takeover_lease(&self, name: &str, holder: &str) -> Result<LeaseResponse> {
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/v1/sessions/{name}/lease/takeover"))),
            )
            .json(&json!({ "holder": holder }))
            .send()
            .context("reaching the server")?;
        Self::decode(resp, "taking over the lease", false)
    }

    pub fn heartbeat(&self, name: &str, holder: &str, fence: i64) -> Result<LeaseResponse> {
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/v1/sessions/{name}/lease/heartbeat"))),
            )
            .json(&json!({ "holder": holder, "fence": fence }))
            .send()
            .context("reaching the server")?;
        // Heartbeat's response has no `granted`/`holder`; map into the common
        // shape so callers track one struct.
        #[derive(Deserialize)]
        struct Hb {
            fence: i64,
            expires_at_unix: i64,
            #[serde(default)]
            ttl_seconds: i64,
        }
        let hb: Hb = Self::decode(resp, "renewing the lease", true)?;
        Ok(LeaseResponse {
            granted: true,
            holder: holder.to_string(),
            fence: hb.fence,
            expires_at_unix: hb.expires_at_unix,
            ttl_seconds: hb.ttl_seconds,
        })
    }

    pub fn release_lease(&self, name: &str, holder: &str, fence: i64) -> Result<()> {
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/v1/sessions/{name}/lease/release"))),
            )
            .json(&json!({ "holder": holder, "fence": fence }))
            .send()
            .context("reaching the server")?;
        Self::decode::<serde_json::Value>(resp, "releasing the lease", true).map(|_| ())
    }

    pub fn upload_bundle(
        &self,
        name: &str,
        holder: &str,
        fence: i64,
        ciphertext: Vec<u8>,
    ) -> Result<serde_json::Value> {
        let bytes = ciphertext.len();
        let resp = self
            .auth(
                self.http
                    .put(self.url(&format!("/v1/sessions/{name}/bundle"))),
            )
            .header("x-nemr-lease-holder", holder)
            .header("x-nemr-lease-fence", fence.to_string())
            .body(ciphertext)
            .send()
            // F-16: a server that refuses the size mid-stream closes the
            // connection, and reqwest reports that as a body-write error
            // ("Broken pipe") that names nothing. Say what it almost always
            // means, with the size, so the user is not left guessing.
            .map_err(|e| {
                if e.is_body() || e.is_request() {
                    anyhow!(
                        "the server closed the connection while this {} bundle was being \
                         uploaded ({e}).\n\
                         That is what a server refusing the size looks like from here: it stops \
                         reading before it can answer. Check the server's bundle ceiling \
                         (NEMR_MAX_BUNDLE_BYTES) and its log.",
                        crate::core::human_bytes(bytes as i64)
                    )
                } else {
                    anyhow::Error::from(e).context("reaching the server")
                }
            })?;
        if resp.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            let said = resp
                .json::<serde_json::Value>()
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_default();
            bail!(
                "the server refused this bundle as too large ({}){}",
                crate::core::human_bytes(bytes as i64),
                if said.is_empty() {
                    String::new()
                } else {
                    format!(": {said}")
                }
            );
        }
        Self::decode(resp, "uploading the bundle", true)
    }

    /// E-22: delete a session's cloud copy — the object and the index row.
    /// Sends this machine's holder id so the server can allow the delete when
    /// this machine holds the lease (or none is held) and refuse, naming the
    /// holder, when another machine does. A 409 is a lease conflict.
    pub fn delete_cloud(&self, name: &str, holder: &str) -> Result<()> {
        let resp = self
            .auth(self.http.delete(self.url(&format!("/v1/sessions/{name}"))))
            .header("x-nemr-lease-holder", holder)
            .send()
            .context("reaching the server")?;
        Self::decode::<serde_json::Value>(resp, "deleting the cloud copy", true).map(|_| ())
    }

    pub fn download_bundle(&self, name: &str) -> Result<Vec<u8>> {
        let resp = self
            .auth(
                self.http
                    .get(self.url(&format!("/v1/sessions/{name}/bundle"))),
            )
            .send()
            .context("reaching the server")?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.bytes().context("reading the bundle body")?.to_vec());
        }
        let msg = resp
            .json::<serde_json::Value>()
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(anyhow!("downloading the bundle: {msg}"))
    }
}
