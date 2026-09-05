//! The nemrd daemon (E-09): a gRPC server over a Unix domain socket that is the
//! single writer to containerd and the volume layer.
//!
//! Why single-writer matters: WP A spent nine commits eliminating the class of
//! bug where two processes independently mutate containerd state and mount
//! records and disagree. The daemon makes that structural — the CLI is a gRPC
//! client with no containerd path of its own — rather than conventional.
//!
//! The daemon holds almost no running state: the container's PID 1 is the
//! supervisor (PROC-01), and containerd plus the host filesystem are the source
//! of truth. So this is a thin, mostly-stateless gateway over
//! `nemr_engine::engine::project::*`, holding one containerd connection.

use crate::containerd::client::ContainerdClient;
use crate::proto::nemr_server::Nemr;
use crate::proto::*;
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub mod attach;
pub mod audit;
pub mod client;
pub mod socket;

/// The service implementation. One containerd connection, shared.
pub struct NemrService {
    client: Arc<ContainerdClient>,
    audit: audit::AuditRegistry,
}

impl NemrService {
    pub fn new(client: ContainerdClient, audit: audit::AuditRegistry) -> Self {
        Self {
            client: Arc::new(client),
            audit,
        }
    }
}

/// Map an engine error onto a gRPC status, preserving the full message.
///
/// This is exactly what F-70's typed error taxonomy was built for: the kind
/// becomes the status code, so a caller can branch on the class while the
/// message — including the D-08-standard actionable ones — reaches the user
/// intact. An untyped `bail!` error carries no kind and maps to Internal with
/// its message preserved; tightening those into typed variants is F-70's
/// ongoing work, not a regression here.
pub fn status_from_anyhow(e: anyhow::Error) -> Status {
    use crate::error::ErrorKind::*;
    use tonic::Code;
    let message = format!("{e:#}");
    let code = match e
        .downcast_ref::<crate::error::Error>()
        .map(|err| err.kind())
    {
        Some(InvalidRequest) => Code::InvalidArgument,
        Some(Conflict) => Code::FailedPrecondition,
        Some(HostPrerequisite) => Code::FailedPrecondition,
        Some(CapacityExceeded) => Code::ResourceExhausted,
        Some(DataIntegrity) => Code::DataLoss,
        Some(Incompatible) => Code::FailedPrecondition,
        Some(Transient) => Code::Unavailable,
        Some(Internal) | None => Code::Internal,
    };
    Status::new(code, message)
}

fn status_from_typed(e: crate::error::Error) -> Status {
    status_from_anyhow(e.into())
}

#[tonic::async_trait]
impl Nemr for NemrService {
    async fn handshake(
        &self,
        request: Request<HandshakeRequest>,
    ) -> Result<Response<HandshakeResponse>, Status> {
        let req = request.into_inner();
        // Test seam: force a version so the mismatch-refusal gate is provable
        // end to end without two builds. Production never sets this.
        let expected = std::env::var("NEMR_TEST_DAEMON_PROTOCOL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::proto::PROTOCOL_VERSION);
        if req.protocol_version != expected {
            // Refuse a mismatch cleanly — the hash-gate lesson. Do not try to
            // serve a client speaking a protocol this daemon does not.
            return Err(Status::failed_precondition(format!(
                "protocol version mismatch: the CLI speaks v{}, this daemon speaks v{}. \
                 Reinstall so both come from the same build: ./scripts/install_engine.sh",
                req.protocol_version, expected
            )));
        }
        tracing::debug!(
            "[nemrd] handshake ok: client_build={:?} protocol v{}",
            req.client_build,
            req.protocol_version
        );
        Ok(Response::new(HandshakeResponse {
            protocol_version: expected,
            daemon_build: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    async fn create(
        &self,
        request: Request<CreateRequest>,
    ) -> Result<Response<CreateResponse>, Status> {
        let req = request.into_inner();
        let size = req
            .size
            .parse::<crate::engine::volume::VolumeSize>()
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let agent = req
            .agent
            .parse::<crate::engine::agent::Agent>()
            .map_err(status_from_typed)?;
        let summary = crate::engine::project::create(&self.client, &req.name, size, agent)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(CreateResponse {
            name: summary.name,
            container_id: summary.container_id,
            volume_path: summary.volume_path,
            size: summary.size.to_string(),
            agent: summary.agent.id().to_string(),
        }))
    }

    async fn start(
        &self,
        request: Request<StartRequest>,
    ) -> Result<Response<StartResponse>, Status> {
        let pid = crate::engine::project::start(&self.client, &request.into_inner().name)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(StartResponse {
            supervisor_pid: pid,
        }))
    }

    async fn stop(&self, request: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        let outcome = crate::engine::project::stop(&self.client, &request.into_inner().name)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(StopResponse {
            outcome: outcome.wire_str().to_string(),
        }))
    }

    async fn provision(
        &self,
        request: Request<ProvisionRequest>,
    ) -> Result<Response<ProvisionResponse>, Status> {
        let report = crate::engine::project::provision(&self.client, &request.into_inner().name)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(ProvisionResponse {
            installed: report.installed,
            failed: report
                .failed
                .into_iter()
                .map(|(package, reason)| ProvisionFailure { package, reason })
                .collect(),
        }))
    }

    async fn delete(
        &self,
        request: Request<DeleteRequest>,
    ) -> Result<Response<DeleteResponse>, Status> {
        crate::engine::project::delete(&self.client, &request.into_inner().name)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(DeleteResponse {}))
    }

    async fn list(&self, _request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let projects = crate::engine::project::list(&self.client)
            .await
            .map_err(status_from_anyhow)?;
        let untracked = crate::engine::project::untracked_volumes(&self.client)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(ListResponse {
            projects: projects
                .into_iter()
                .map(|p| ProjectStatus {
                    name: p.name,
                    container_id: p.container_id,
                    quota: p.quota,
                    running: p.running,
                    volume_path: p.volume_path,
                    usage_known: p.usage.is_some(),
                    used_bytes: p.usage.as_ref().map(|u| u.used).unwrap_or(0),
                    used_percent: p.usage.as_ref().map(|u| u.percent()).unwrap_or(0.0),
                    agent: p.agent,
                })
                .collect(),
            untracked_volumes: untracked,
        }))
    }

    async fn port_add(
        &self,
        request: Request<PortAddRequest>,
    ) -> Result<Response<PortListResponse>, Status> {
        let req = request.into_inner();
        let port = crate::engine::ports::parse_port_arg(&req.port, req.expose)
            .map_err(|e| Status::invalid_argument(format!("{e:#}")))?;
        crate::engine::project::add_port(&self.client, &req.name, port)
            .await
            .map_err(status_from_typed)?;
        self.port_list_response(&req.name).await
    }

    async fn port_remove(
        &self,
        request: Request<PortRemoveRequest>,
    ) -> Result<Response<PortListResponse>, Status> {
        let req = request.into_inner();
        crate::engine::project::remove_port(&self.client, &req.name, req.host_port as u16)
            .await
            .map_err(status_from_typed)?;
        self.port_list_response(&req.name).await
    }

    async fn port_list(
        &self,
        request: Request<PortListRequest>,
    ) -> Result<Response<PortListResponse>, Status> {
        self.port_list_response(&request.into_inner().name).await
    }

    async fn status(
        &self,
        request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let name = request.into_inner().name;
        let d = crate::engine::project::status(&self.client, &name)
            .await
            .map_err(status_from_typed)?;
        let mount_check = match d.mount_is_correct() {
            None => -1,
            Some(false) => 0,
            Some(true) => 1,
        };
        let ports = crate::engine::project::list_ports(&self.client, &name)
            .await
            .unwrap_or_default();
        let live_set = crate::engine::ports::list_live().unwrap_or_default();
        Ok(Response::new(StatusResponse {
            ports: ports
                .into_iter()
                .map(|p| PortSpec {
                    live: live_set.iter().any(|f| {
                        f.host_ip == p.host_ip
                            && f.host_port == p.host_port
                            && f.container_port == p.container_port
                    }),
                    host_ip: p.host_ip.clone(),
                    host_port: p.host_port as u32,
                    container_port: p.container_port as u32,
                    url: p.url(),
                })
                .collect(),
            name: d.name,
            agent: d.agent.id().to_string(),
            container_id: d.container_id,
            running: d.running,
            quota: d.quota,
            mount_point: d.mount_point.to_string_lossy().into_owned(),
            image_file: d.image_file.to_string_lossy().into_owned(),
            image_present: d.image_present,
            mounted: d.mounted,
            loop_device: d
                .loop_device
                .map(|n| format!("/dev/loop{n}"))
                .unwrap_or_default(),
            mounted_image: d
                .mounted_image
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            usage_known: d.usage.is_some(),
            used_bytes: d.usage.as_ref().map(|u| u.used).unwrap_or(0),
            used_percent: d.usage.as_ref().map(|u| u.percent()).unwrap_or(0.0),
            base_image: d.base_image,
            base_image_digest: d.base_image_digest.unwrap_or_default(),
            credential_path: d
                .credential
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            credential_modified_secs: d
                .credential_modified
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            credential_expires_at_secs: d.credential_expires_at.unwrap_or(0),
            credential_refresh_expires_at_secs: d.credential_refresh_expires_at.unwrap_or(0),
            credential_blank: d.credential_blank,
            credential_stale: match d.credential_stale {
                None => -1,
                Some(false) => 0,
                Some(true) => 1,
            },
            mount_check,
        }))
    }

    async fn switch_agent(
        &self,
        request: Request<SwitchAgentRequest>,
    ) -> Result<Response<SwitchAgentResponse>, Status> {
        let req = request.into_inner();
        let agent = req
            .agent
            .parse::<crate::engine::agent::Agent>()
            .map_err(status_from_typed)?;
        let (previous, now) = crate::engine::project::set_agent(&self.client, &req.name, agent)
            .await
            .map_err(status_from_typed)?;
        Ok(Response::new(SwitchAgentResponse {
            previous: previous.id().to_string(),
            now: now.id().to_string(),
        }))
    }

    async fn reconcile(
        &self,
        _request: Request<ReconcileRequest>,
    ) -> Result<Response<ReconcileResponse>, Status> {
        let report = crate::engine::project::reconcile_orphans(&self.client)
            .await
            .map_err(status_from_anyhow)?;
        Ok(Response::new(ReconcileResponse {
            released: report.released,
            not_released: report.not_released,
            snapshots_removed: report.snapshots_removed,
            orphan_backing_files: report.orphan_backing_files,
            stale_forwards: report.stale_forwards,
            unattributable_forwards: report.unattributable_forwards,
        }))
    }

    async fn import(
        &self,
        request: Request<ImportRequest>,
    ) -> Result<Response<ImportResponse>, Status> {
        let req = request.into_inner();
        let name = (!req.name.is_empty()).then_some(req.name);
        let size = if req.size.is_empty() {
            None
        } else {
            Some(
                req.size
                    .parse::<crate::engine::volume::VolumeSize>()
                    .map_err(|e| Status::invalid_argument(e.to_string()))?,
            )
        };
        let (name, summary) = crate::engine::project::import_creating(
            &self.client,
            std::path::Path::new(&req.bundle_path),
            name.as_deref(),
            size,
        )
        .await
        .map_err(status_from_typed)?;
        // How many packages the bundle declared, for the CLI's suggestion.
        // Read-only, and a failure to read is zero rather than a failed import:
        // the import already succeeded, and the suggestion is advice, not state.
        let declared_packages = crate::engine::volume::VolumePaths::from_env()
            .ok()
            .map(|paths| paths.mount_point(&name))
            .and_then(|mount| {
                crate::engine::packages::read_declared(&mount)
                    .ok()
                    .flatten()
            })
            .map(|list| list.packages.len() as u64)
            .unwrap_or(0);
        Ok(Response::new(ImportResponse {
            name,
            members: summary.members as u64,
            bytes: summary.bytes,
            declared_packages,
        }))
    }

    async fn export(
        &self,
        request: Request<ExportRequest>,
    ) -> Result<Response<ExportResponse>, Status> {
        let req = request.into_inner();
        let mut policy = crate::bundle::policy::Policy::default();
        if req.include_build_artifacts {
            policy.include_build_artifacts = true;
        }
        let summary = crate::engine::project::export(
            &self.client,
            &req.name,
            std::path::Path::new(&req.destination),
            policy,
        )
        .await
        .map_err(status_from_typed)?;
        Ok(Response::new(ExportResponse {
            path: summary.path.to_string_lossy().into_owned(),
            schema_version: summary.manifest.schema_version,
            members: summary.manifest.members.len() as u64,
            content_bytes: summary.manifest.project.content_bytes,
            bundle_bytes: summary.bundle_bytes,
            unrecognised_fields: summary.unrecognised_fields,
        }))
    }

    type WatchAuditStream =
        tokio_stream::wrappers::UnboundedReceiverStream<Result<AuditEvent, Status>>;

    async fn watch_audit(
        &self,
        request: Request<WatchAuditRequest>,
    ) -> Result<Response<Self::WatchAuditStream>, Status> {
        let request_id = request.into_inner().request_id;
        if request_id.is_empty() {
            return Err(Status::invalid_argument("watch_audit: empty request_id"));
        }

        // Register a raw (non-Result) sender for the tracing Layer to route to,
        // and a task that forwards those events into the response stream and
        // sends the Ready sentinel first. The registration is dropped when the
        // client disconnects (the forwarding task ends), so the map does not
        // grow without bound.
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel::<AuditEvent>();
        {
            let mut reg = self
                .audit
                .lock()
                .map_err(|_| Status::internal("audit registry poisoned"))?;
            reg.insert(request_id.clone(), raw_tx);
        }

        let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<Result<AuditEvent, Status>>();
        // Ready first: the client waits for this before running its command, so
        // it cannot race the events that command produces.
        let _ = out_tx.send(Ok(AuditEvent {
            warning: false,
            ready: true,
            message: String::new(),
            privileged: false,
        }));

        let registry = self.audit.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // The client dropped its stream (command finished). Without
                    // this, the task would block in `recv().await` forever
                    // holding the registry entry — a leak on EVERY command,
                    // since `recv` only returns None once the entry (which holds
                    // the sender) is removed, and it is removed only here.
                    _ = out_tx.closed() => break,
                    ev = raw_rx.recv() => match ev {
                        Some(ev) => {
                            if out_tx.send(Ok(ev)).is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
            if let Ok(mut reg) = registry.lock() {
                reg.remove(&request_id);
            }
        });

        Ok(Response::new(
            tokio_stream::wrappers::UnboundedReceiverStream::new(out_rx),
        ))
    }

    type AttachStream = attach::AttachStream;

    async fn attach(
        &self,
        request: Request<tonic::Streaming<AttachClient>>,
    ) -> Result<Response<Self::AttachStream>, Status> {
        attach::serve(self.client.clone(), request.into_inner()).await
    }
}

impl NemrService {
    /// One project's declared forwards, in the shape the wire uses. Shared by
    /// add/remove/list so every port RPC answers with the same current truth.
    async fn port_list_response(&self, name: &str) -> Result<Response<PortListResponse>, Status> {
        let ports = crate::engine::project::list_ports(&self.client, name)
            .await
            .map_err(status_from_typed)?;
        let live_set = crate::engine::ports::list_live().unwrap_or_default();
        Ok(Response::new(PortListResponse {
            ports: ports
                .into_iter()
                .map(|p| PortSpec {
                    live: live_set.iter().any(|f| {
                        f.host_ip == p.host_ip
                            && f.host_port == p.host_port
                            && f.container_port == p.container_port
                    }),
                    host_ip: p.host_ip.clone(),
                    host_port: p.host_port as u32,
                    container_port: p.container_port as u32,
                    url: p.url(),
                })
                .collect(),
        }))
    }
}
