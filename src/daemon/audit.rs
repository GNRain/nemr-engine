//! Daemon->client audit event streaming (E-09/NFR-04).
//!
//! The engine already emits its VOL-03/NFR-04 audit trail through `tracing`
//! (`audit`, `audit_elevated`). Rather than thread a channel through every
//! engine function, the daemon installs a tracing `Layer` that captures those
//! events and routes each to the client whose command produced it — so the same
//! single source feeds both the daemon log and the client stream, with no
//! duplication and no drift.
//!
//! Correlation is by request id: a `tower` layer wraps every RPC in a span
//! carrying the `nemr-request-id` the client sent, the tracing Layer reads that
//! id from the span in scope when an engine event fires, and the client's
//! `WatchAudit` stream (opened under the same id) receives it.

use crate::proto::AuditEvent;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;
use tracing::field::{Field, Visit};
use tracing::span;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Maps a request id to the sender feeding that client's WatchAudit stream.
/// Shared between the tracing Layer (which routes events in) and the service
/// (which registers a sender when a client subscribes).
pub type AuditRegistry = Arc<Mutex<HashMap<String, UnboundedSender<AuditEvent>>>>;

pub fn new_registry() -> AuditRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// The span field carrying the client's request id.
const REQUEST_ID_FIELD: &str = "nemr_request_id";

/// A tracing Layer that captures the engine's audit events and routes each to
/// the client whose request produced it.
pub struct AuditLayer {
    registry: AuditRegistry,
}

impl AuditLayer {
    pub fn new(registry: AuditRegistry) -> Self {
        Self { registry }
    }
}

/// Reads the request id out of a span's fields as the span is created.
struct RequestIdVisitor(Option<String>);
impl Visit for RequestIdVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == REQUEST_ID_FIELD {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == REQUEST_ID_FIELD {
            self.0 = Some(value.to_string());
        }
    }
}

/// Reads an audit event's message and category out of its fields.
#[derive(Default)]
struct AuditVisitor {
    message: Option<String>,
    category: Option<String>,
    op: Option<String>,
}
impl Visit for AuditVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "nemr_audit" => self.category = Some(value.to_string()),
            "nemr_op" => self.op = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = Some(format!("{value:?}").trim_matches('"').to_string()),
            "nemr_audit" => {
                self.category = Some(format!("{value:?}").trim_matches('"').to_string())
            }
            "nemr_op" => self.op = Some(format!("{value:?}").trim_matches('"').to_string()),
            _ => {}
        }
    }
}

impl<S> Layer<S> for AuditLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        // Stash the request id on the span so on_event can find it by walking
        // the scope, even across the engine's .await points.
        let mut v = RequestIdVisitor(None);
        attrs.record(&mut v);
        if let Some(request_id) = v.0 {
            if let Some(span) = ctx.span(id) {
                span.extensions_mut().insert(RequestId(request_id));
            }
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        // Only the engine's own audit trail — not tonic/tokio framework noise.
        if !event.metadata().target().starts_with("nemr_engine") {
            return;
        }
        // Find the request id from the enclosing span scope.
        let Some(request_id) = ctx.event_scope(event).and_then(|scope| {
            scope
                .from_root()
                .find_map(|span| span.extensions().get::<RequestId>().map(|r| r.0.clone()))
        }) else {
            return;
        };

        let mut v = AuditVisitor::default();
        event.record(&mut v);
        let privileged = v.category.as_deref() == Some("elevated");
        let warning = v.category.as_deref() == Some("warning");
        let message = v.message.unwrap_or_default();
        // Only surface the engine's audit lines (they set nemr_audit); skip any
        // other nemr_engine event that happens to be in scope.
        if v.category.is_none() {
            return;
        }

        if let Ok(reg) = self.registry.lock() {
            if let Some(tx) = reg.get(&request_id) {
                let _ = tx.send(AuditEvent {
                    ready: false,
                    message,
                    privileged,
                    warning,
                });
            }
        }
    }
}

struct RequestId(String);

// --- the tower layer that wraps each RPC in a request-id span ---

/// Wraps every gRPC call in a span carrying the client's `nemr-request-id`, so
/// the AuditLayer can attribute the engine events that call produces.
#[derive(Clone)]
pub struct RequestSpanLayer;

impl<S> tower::Layer<S> for RequestSpanLayer {
    type Service = RequestSpanService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequestSpanService { inner }
    }
}

#[derive(Clone)]
pub struct RequestSpanService<S> {
    inner: S,
}

impl<S, B> tower::Service<http::Request<B>> for RequestSpanService<S>
where
    S: tower::Service<http::Request<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        use tracing::Instrument;
        let request_id = req
            .headers()
            .get("nemr-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Clone-and-swap so the cloned (ready) inner is the one called — the
        // standard tower correctness pattern.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let span = tracing::info_span!("nemr_request", nemr_request_id = %request_id);
        Box::pin(async move { inner.call(req).await }.instrument(span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// The Layer must route an audit event emitted within a request span to the
    /// sender registered for that request id — the whole correlation mechanism,
    /// tested without a daemon.
    /// Mirror the daemon exactly: a per-layer filter on the AuditLayer, and the
    /// event emitted from inside an `.instrument(span).await`ed future on a
    /// runtime — which is how a real handler runs. If routing works with
    /// `.enter()` but breaks here, the filter or the async span propagation is
    /// the culprit.
    #[test]
    fn routes_through_a_filtered_layer_and_instrumented_future() {
        use tracing_subscriber::Layer;
        let registry = new_registry();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AuditEvent>();
        registry.lock().unwrap().insert("req-2".to_string(), tx);

        let layer = AuditLayer::new(registry.clone())
            .with_filter(tracing_subscriber::EnvFilter::new("nemr_engine=debug"));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(async {
                use tracing::Instrument;
                let span = tracing::info_span!("nemr_request", nemr_request_id = %"req-2");
                async {
                    tracing::info!(nemr_audit = "elevated", "[nemr] elevated: mount");
                }
                .instrument(span)
                .await;
            });
        });

        assert!(
            rx.try_recv().is_ok(),
            "routing must work through a per-layer filter and an instrumented future"
        );
    }

    #[test]
    fn routes_an_audit_event_to_its_request() {
        let registry = new_registry();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AuditEvent>();
        registry.lock().unwrap().insert("req-1".to_string(), tx);

        let subscriber = tracing_subscriber::registry().with(AuditLayer::new(registry.clone()));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("nemr_request", nemr_request_id = %"req-1");
            let _g = span.enter();
            tracing::info!(
                nemr_audit = "elevated",
                nemr_op = "mount",
                "[nemr] elevated: mount"
            );
        });

        let ev = rx
            .try_recv()
            .expect("the elevation event must be routed to req-1");
        assert!(ev.privileged, "an elevated event must be marked privileged");
        assert!(ev.message.contains("elevated"), "message: {:?}", ev.message);

        // CONTROL: an event OUTSIDE any request span must NOT be routed (no
        // sender to attribute it to) — so a clean pass is not vacuous.
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(AuditLayer::new(registry.clone())),
            || {
                tracing::info!(nemr_audit = "elevated", "[nemr] elevated: orphan");
            },
        );
        assert!(
            rx.try_recv().is_err(),
            "an event with no request span must not route"
        );
    }
}
