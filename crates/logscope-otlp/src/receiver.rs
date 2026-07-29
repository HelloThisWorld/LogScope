//! EXPERIMENTAL loopback-only OTLP receivers (v0.0 spike).
//!
//! - Disabled by default: both ports are `None` in `OtlpReceiverConfig::default()`.
//! - Binds strictly to 127.0.0.1.
//! - Bounded: request bodies are size-limited and decoded envelopes flow
//!   through a bounded channel; overload answers 429 / RESOURCE_EXHAUSTED.
//!
//! Not a production Local OTel Session feature; reliable live ingestion is
//! a v0.7 concern.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use logscope_model::{hash_bytes_hex, SourceProtocol, UnixNanos};
use opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::{
    LogsService, LogsServiceServer,
};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::{
    MetricsService, MetricsServiceServer,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use prost::Message;
use tokio::sync::mpsc;

use crate::error::OtlpError;

#[derive(Debug, Clone)]
pub struct OtlpReceiverConfig {
    /// Loopback HTTP port (`Some(0)` = ephemeral). `None` = disabled.
    pub http_port: Option<u16>,
    /// Loopback gRPC port (`Some(0)` = ephemeral). `None` = disabled.
    pub grpc_port: Option<u16>,
    /// Maximum accepted request body / message size in bytes.
    pub max_body_bytes: usize,
    /// Bounded envelope channel capacity.
    pub channel_capacity: usize,
}

impl Default for OtlpReceiverConfig {
    fn default() -> Self {
        // Disabled by default: an explicit opt-in is required per port.
        OtlpReceiverConfig {
            http_port: None,
            grpc_port: None,
            max_body_bytes: 8 * 1024 * 1024,
            channel_capacity: 64,
        }
    }
}

#[derive(Debug, Clone)]
pub enum EnvelopePayload {
    Logs(ExportLogsServiceRequest),
    Metrics(ExportMetricsServiceRequest),
    Traces(ExportTraceServiceRequest),
}

#[derive(Debug, Clone)]
pub struct EnvelopeMeta {
    pub protocol: SourceProtocol,
    pub content_type: String,
    /// BLAKE3 hex of the raw request body (protobuf or JSON bytes); for
    /// gRPC, of the deterministic prost re-encoding.
    pub raw_hash: String,
    pub received_at: UnixNanos,
}

#[derive(Debug, Clone)]
pub struct ReceivedEnvelope {
    pub payload: EnvelopePayload,
    pub meta: EnvelopeMeta,
}

#[derive(Clone)]
struct Shared {
    tx: mpsc::Sender<ReceivedEnvelope>,
}

impl Shared {
    fn push(&self, envelope: ReceivedEnvelope) -> Result<(), ()> {
        self.tx.try_send(envelope).map_err(|_| ())
    }
}

pub struct OtlpReceiverHandle {
    pub http_addr: Option<SocketAddr>,
    pub grpc_addr: Option<SocketAddr>,
    pub envelopes: mpsc::Receiver<ReceivedEnvelope>,
    shutdown: Vec<tokio::sync::oneshot::Sender<()>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl OtlpReceiverHandle {
    /// Signals both servers to stop and waits for them.
    pub async fn shutdown(mut self) {
        for tx in self.shutdown.drain(..) {
            let _ = tx.send(());
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
    }
}

/// Starts the configured receivers on loopback. With the default (disabled)
/// config this binds nothing and returns a handle with no addresses.
pub async fn start(config: OtlpReceiverConfig) -> Result<OtlpReceiverHandle, OtlpError> {
    let (tx, rx) = mpsc::channel(config.channel_capacity.max(1));
    let shared = Shared { tx };
    let mut handle = OtlpReceiverHandle {
        http_addr: None,
        grpc_addr: None,
        envelopes: rx,
        shutdown: vec![],
        tasks: vec![],
    };

    if let Some(port) = config.http_port {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(OtlpError::Bind)?;
        handle.http_addr = Some(listener.local_addr().map_err(OtlpError::Bind)?);
        let app = Router::new()
            .route("/v1/logs", post(http_logs))
            .route("/v1/metrics", post(http_metrics))
            .route("/v1/traces", post(http_traces))
            .layer(DefaultBodyLimit::max(config.max_body_bytes))
            .with_state(shared.clone());
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        handle.shutdown.push(stop_tx);
        handle.tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stop_rx.await;
                })
                .await;
        }));
    }

    if let Some(port) = config.grpc_port {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(OtlpError::Bind)?;
        handle.grpc_addr = Some(listener.local_addr().map_err(OtlpError::Bind)?);
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        handle.shutdown.push(stop_tx);
        let max = config.max_body_bytes;
        let grpc_shared = shared.clone();
        handle.tasks.push(tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(
                    LogsServiceServer::new(GrpcLogs(grpc_shared.clone()))
                        .max_decoding_message_size(max),
                )
                .add_service(
                    MetricsServiceServer::new(GrpcMetrics(grpc_shared.clone()))
                        .max_decoding_message_size(max),
                )
                .add_service(
                    TraceServiceServer::new(GrpcTraces(grpc_shared)).max_decoding_message_size(max),
                )
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = stop_rx.await;
                })
                .await;
        }));
    }

    Ok(handle)
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

enum HttpKind {
    Protobuf,
    Json,
}

fn classify_content_type(headers: &HeaderMap) -> Option<HttpKind> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mime = ct.split(';').next().unwrap_or("").trim();
    match mime {
        "application/x-protobuf" | "application/protobuf" => Some(HttpKind::Protobuf),
        "application/json" => Some(HttpKind::Json),
        _ => None,
    }
}

fn http_accept<Req, Resp>(
    shared: &Shared,
    headers: &HeaderMap,
    body: &Bytes,
    wrap: impl Fn(Req) -> EnvelopePayload,
    empty_response: Resp,
) -> Response
where
    Req: Message + serde::de::DeserializeOwned + Default,
    Resp: Message,
{
    let kind = match classify_content_type(headers) {
        Some(k) => k,
        None => {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected application/x-protobuf or application/json",
            )
                .into_response()
        }
    };
    let (payload, protocol, content_type): (Req, SourceProtocol, &str) = match kind {
        HttpKind::Protobuf => match Req::decode(body.as_ref()) {
            Ok(req) => (
                req,
                SourceProtocol::OtlpHttpProtobuf,
                "application/x-protobuf",
            ),
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("invalid protobuf: {e}")).into_response()
            }
        },
        HttpKind::Json => match serde_json::from_slice::<Req>(body) {
            Ok(req) => (req, SourceProtocol::OtlpHttpJson, "application/json"),
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("invalid json: {e}")).into_response()
            }
        },
    };

    let envelope = ReceivedEnvelope {
        payload: wrap(payload),
        meta: EnvelopeMeta {
            protocol,
            content_type: content_type.to_string(),
            raw_hash: hash_bytes_hex(body),
            received_at: UnixNanos::now(),
        },
    };
    if shared.push(envelope).is_err() {
        return (StatusCode::TOO_MANY_REQUESTS, "receiver buffer full").into_response();
    }

    match kind {
        HttpKind::Protobuf => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-protobuf")],
            empty_response.encode_to_vec(),
        )
            .into_response(),
        HttpKind::Json => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            "{}",
        )
            .into_response(),
    }
}

async fn http_logs(State(shared): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    http_accept::<ExportLogsServiceRequest, _>(
        &shared,
        &headers,
        &body,
        EnvelopePayload::Logs,
        ExportLogsServiceResponse::default(),
    )
}

async fn http_metrics(State(shared): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    http_accept::<ExportMetricsServiceRequest, _>(
        &shared,
        &headers,
        &body,
        EnvelopePayload::Metrics,
        ExportMetricsServiceResponse::default(),
    )
}

async fn http_traces(State(shared): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    http_accept::<ExportTraceServiceRequest, _>(
        &shared,
        &headers,
        &body,
        EnvelopePayload::Traces,
        ExportTraceServiceResponse::default(),
    )
}

// ---------------------------------------------------------------------------
// gRPC services
// ---------------------------------------------------------------------------

fn grpc_push<Req: Message>(
    shared: &Shared,
    req: Req,
    wrap: impl Fn(Req) -> EnvelopePayload,
) -> Result<(), tonic::Status> {
    // Deterministic prost re-encoding stands in for raw wire bytes.
    let raw_hash = hash_bytes_hex(&req.encode_to_vec());
    let envelope = ReceivedEnvelope {
        payload: wrap(req),
        meta: EnvelopeMeta {
            protocol: SourceProtocol::OtlpGrpc,
            content_type: "application/grpc".to_string(),
            raw_hash,
            received_at: UnixNanos::now(),
        },
    };
    shared
        .push(envelope)
        .map_err(|_| tonic::Status::resource_exhausted("receiver buffer full"))
}

struct GrpcLogs(Shared);
#[tonic::async_trait]
impl LogsService for GrpcLogs {
    async fn export(
        &self,
        request: tonic::Request<ExportLogsServiceRequest>,
    ) -> Result<tonic::Response<ExportLogsServiceResponse>, tonic::Status> {
        grpc_push(&self.0, request.into_inner(), EnvelopePayload::Logs)?;
        Ok(tonic::Response::new(ExportLogsServiceResponse::default()))
    }
}

struct GrpcMetrics(Shared);
#[tonic::async_trait]
impl MetricsService for GrpcMetrics {
    async fn export(
        &self,
        request: tonic::Request<ExportMetricsServiceRequest>,
    ) -> Result<tonic::Response<ExportMetricsServiceResponse>, tonic::Status> {
        grpc_push(&self.0, request.into_inner(), EnvelopePayload::Metrics)?;
        Ok(tonic::Response::new(ExportMetricsServiceResponse::default()))
    }
}

struct GrpcTraces(Shared);
#[tonic::async_trait]
impl TraceService for GrpcTraces {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        grpc_push(&self.0, request.into_inner(), EnvelopePayload::Traces)?;
        Ok(tonic::Response::new(ExportTraceServiceResponse::default()))
    }
}
