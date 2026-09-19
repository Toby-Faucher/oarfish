//! OTLP logs over gRPC on 4317, via `tonic`.
//!
//! The front door for anything with a collector in front of it. Metrics and
//! traces stay out permanently: OTLP is a log transport here and nothing
//! more, so only `LogsService` is served. OTLP over HTTP/protobuf is deferred
//! to M6, when a real collector is pointed at oarfish.
//!
//! Like syslog TCP, this transport applies backpressure: each record
//! `send().await`s into the channel, so a full pipeline stalls the sender's
//! stream instead of dropping its records.

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;

use bytes::Bytes;
use oarfish_core::{Event, Source};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
    logs_service_server::{LogsService, LogsServiceServer},
};
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value::Value};
use opentelemetry_proto::tonic::logs::v1::LogRecord;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;

use crate::IngestError;

/// Render an OTLP attribute value as text. Scalars stringify; arrays and maps
/// join recursively; bytes stay bytes-shaped only through [`log_record_body`].
fn any_value_to_string(value: &AnyValue) -> String {
    match &value.value {
        None => String::new(),
        Some(Value::StringValue(s)) => s.clone(),
        Some(Value::BoolValue(b)) => b.to_string(),
        Some(Value::IntValue(n)) => n.to_string(),
        Some(Value::DoubleValue(n)) => n.to_string(),
        Some(Value::BytesValue(b)) => String::from_utf8_lossy(b).into_owned(),
        // Profiling-only string-table reference. The spec says to treat it as
        // absent for non-profiling signals, so it renders empty.
        Some(Value::StringValueStrindex(_)) => String::new(),
        Some(Value::ArrayValue(array)) => {
            let items: Vec<String> = array.values.iter().map(any_value_to_string).collect();
            format!("[{}]", items.join(", "))
        }
        Some(Value::KvlistValue(list)) => {
            let items: Vec<String> = list
                .values
                .iter()
                .map(|kv| {
                    format!(
                        "{}={}",
                        kv.key,
                        kv.value
                            .as_ref()
                            .map(any_value_to_string)
                            .unwrap_or_default()
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// The record's body as bytes. String and byte bodies keep their exact bytes —
/// invariant 3 holds for OTLP too — while structured bodies render to text.
fn log_record_body(record: &LogRecord) -> Bytes {
    match &record.body {
        None => Bytes::new(),
        Some(value) => match &value.value {
            Some(Value::StringValue(s)) => Bytes::copy_from_slice(s.as_bytes()),
            Some(Value::BytesValue(b)) => Bytes::copy_from_slice(b),
            _ => Bytes::from(any_value_to_string(value).into_bytes()),
        },
    }
}

fn flatten_attributes(into: &mut BTreeMap<String, String>, prefix: &str, attrs: &[KeyValue]) {
    for kv in attrs {
        let text = kv
            .value
            .as_ref()
            .map(any_value_to_string)
            .unwrap_or_default();
        into.insert(format!("{prefix}{}", kv.key), text);
    }
}

fn nanos_to_timestamp(nanos: u64) -> Option<OffsetDateTime> {
    if nanos == 0 {
        return None;
    }
    OffsetDateTime::from_unix_timestamp_nanos(nanos as i128).ok()
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Turn one OTLP log record into an `Event`. Pure, so tests drive it without
/// standing up a server: same record in, same event out.
///
/// `resource_attrs` are the resource's flattened attributes, `scope` the
/// instrumentation scope name, `peer` the gRPC peer IP as text. The host is
/// `host.name` when the resource names one, else the peer. The source
/// timestamp prefers `time_unix_nano`, then `observed_time_unix_nano`, then
/// nothing — a zero timestamp means unknown, not 1970.
pub fn log_record_to_event(
    record: &LogRecord,
    resource_attrs: &BTreeMap<String, String>,
    scope: &str,
    peer: &str,
    received_at: OffsetDateTime,
) -> Event {
    let mut attrs = BTreeMap::new();
    for (key, value) in resource_attrs {
        attrs.insert(format!("resource.{key}"), value.clone());
    }
    // Record attributes ride under `attr.`, mirroring the `resource.` prefix
    // above: a user attribute named `scope` must never collide with the
    // reserved instrumentation keys inserted below.
    flatten_attributes(&mut attrs, "attr.", &record.attributes);
    if !record.severity_text.is_empty() {
        attrs.insert("severity_text".to_owned(), record.severity_text.clone());
    }
    if record.severity_number != 0 {
        attrs.insert(
            "severity_number".to_owned(),
            record.severity_number.to_string(),
        );
    }
    if !scope.is_empty() {
        attrs.insert("scope".to_owned(), scope.to_owned());
    }
    if !record.trace_id.is_empty() {
        attrs.insert("trace_id".to_owned(), hex_string(&record.trace_id));
    }
    if !record.span_id.is_empty() {
        attrs.insert("span_id".to_owned(), hex_string(&record.span_id));
    }
    Event {
        raw: log_record_body(record),
        received_at,
        timestamp: nanos_to_timestamp(record.time_unix_nano)
            .or_else(|| nanos_to_timestamp(record.observed_time_unix_nano)),
        host: resource_attrs
            .get("host.name")
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| peer.to_owned()),
        source: Source::Otlp,
        attrs,
    }
}

/// Turn a whole export request into events, threading resource and scope
/// context down to each record.
pub fn request_to_events(
    request: &ExportLogsServiceRequest,
    peer: &str,
    received_at: OffsetDateTime,
) -> Vec<Event> {
    let mut events = Vec::new();
    for resource_logs in &request.resource_logs {
        let mut resource_attrs = BTreeMap::new();
        if let Some(resource) = &resource_logs.resource {
            flatten_attributes(&mut resource_attrs, "", &resource.attributes);
        }
        for scope_logs in &resource_logs.scope_logs {
            let scope = scope_logs
                .scope
                .as_ref()
                .map(|scope| scope.name.as_str())
                .unwrap_or("");
            for record in &scope_logs.log_records {
                events.push(log_record_to_event(
                    record,
                    &resource_attrs,
                    scope,
                    peer,
                    received_at,
                ));
            }
        }
    }
    events
}

struct OtlpService {
    tx: mpsc::Sender<Event>,
}

#[tonic::async_trait]
impl LogsService for OtlpService {
    async fn export(
        &self,
        request: tonic::Request<ExportLogsServiceRequest>,
    ) -> Result<tonic::Response<ExportLogsServiceResponse>, tonic::Status> {
        // The peer IP, never host:port: the source port is ephemeral, and the
        // fallback host must be stable for per-host grouping downstream.
        let peer = request
            .remote_addr()
            .map(|addr| addr.ip().to_string())
            .unwrap_or_default();
        let received_at = OffsetDateTime::now_utc();
        let inner = request.into_inner();
        for event in request_to_events(&inner, &peer, received_at) {
            self.tx
                .send(event)
                .await
                .map_err(|_| tonic::Status::unavailable("the pipeline is gone"))?;
        }
        Ok(tonic::Response::new(ExportLogsServiceResponse {
            partial_success: None,
        }))
    }
}

/// OTLP logs over gRPC. Bound in [`Otlp::bind`], served in [`Otlp::run`]:
/// the same two-phase lifecycle as the syslog listeners, so a test binds
/// `127.0.0.1:0` and reads the ephemeral port first.
pub struct Otlp {
    listener: TcpListener,
}

impl Otlp {
    /// Bind the socket. Returns before serving; see [`Otlp::run`].
    pub async fn bind(addr: SocketAddr) -> Result<Self, IngestError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| IngestError::Bind { addr, source })?;
        Ok(Self { listener })
    }

    /// The bound address. A test binds port 0 and reads this back.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve `LogsService` until `cancel` fires, then shut down gracefully.
    /// Drops its `Sender` on exit.
    pub async fn run(self, tx: mpsc::Sender<Event>, cancel: CancellationToken) {
        let service = LogsServiceServer::new(OtlpService { tx });
        let incoming = TcpListenerStream::new(self.listener);
        if let Err(e) = tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming_shutdown(incoming, cancel.cancelled())
            .await
        {
            tracing::error!(error = %e, "otlp server failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::collector::logs::v1::logs_service_client::LogsServiceClient;
    use opentelemetry_proto::tonic::common::v1::InstrumentationScope;
    use opentelemetry_proto::tonic::logs::v1::{ResourceLogs, ScopeLogs};
    use opentelemetry_proto::tonic::resource::v1::Resource;

    fn received_at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_789_812_000).expect("fixed test time")
    }

    fn string_value(s: &str) -> AnyValue {
        AnyValue {
            value: Some(Value::StringValue(s.to_owned())),
        }
    }

    fn record(body: &str) -> LogRecord {
        LogRecord {
            body: Some(string_value(body)),
            time_unix_nano: 1_789_812_000_000_000_000,
            severity_text: "ERROR".to_owned(),
            attributes: vec![KeyValue {
                key: "service.name".to_owned(),
                value: Some(string_value("api")),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_log_record_becomes_an_event() {
        let resource_attrs = BTreeMap::from([("host.name".to_owned(), "web01".to_owned())]);
        let event = log_record_to_event(
            &record("connection refused"),
            &resource_attrs,
            "app",
            "peer",
            received_at(),
        );
        insta::assert_json_snapshot!(event, @r###"
        {
          "raw": "connection refused",
          "received_at": "2026-09-19T10:00:00Z",
          "timestamp": "2026-09-19T10:00:00Z",
          "host": "web01",
          "source": "otlp",
          "attrs": {
            "attr.service.name": "api",
            "resource.host.name": "web01",
            "scope": "app",
            "severity_text": "ERROR"
          }
        }
        "###);
    }

    #[test]
    fn a_missing_host_falls_back_to_the_peer_ip() {
        let event = log_record_to_event(
            &record("hello"),
            &BTreeMap::new(),
            "",
            "10.0.0.5",
            received_at(),
        );
        assert_eq!(event.host, "10.0.0.5");
    }

    #[test]
    fn a_zero_timestamp_means_unknown_not_1970() {
        let mut rec = record("hello");
        rec.time_unix_nano = 0;
        rec.observed_time_unix_nano = 0;
        let event = log_record_to_event(&rec, &BTreeMap::new(), "", "peer", received_at());
        assert_eq!(event.timestamp, None);
    }

    #[test]
    fn a_missing_body_is_still_an_event() {
        let rec = LogRecord::default();
        let event = log_record_to_event(&rec, &BTreeMap::new(), "", "peer", received_at());
        assert!(event.raw.is_empty());
        assert_eq!(event.timestamp, None);
    }

    #[test]
    fn a_user_attribute_named_scope_does_not_overwrite_the_reserved_key() {
        let mut rec = record("hello");
        rec.attributes.push(KeyValue {
            key: "scope".to_owned(),
            value: Some(string_value("checkout")),
            ..Default::default()
        });
        let event = log_record_to_event(&rec, &BTreeMap::new(), "app", "peer", received_at());
        assert_eq!(event.attrs.get("scope").map(String::as_str), Some("app"));
        assert_eq!(
            event.attrs.get("attr.scope").map(String::as_str),
            Some("checkout")
        );
    }

    /// Stand the tonic server up, drive it with the generated client.
    #[tokio::test]
    async fn export_round_trips_through_grpc() {
        let server = Otlp::bind("127.0.0.1:0".parse().expect("addr"))
            .await
            .expect("bind");
        let addr = server.local_addr().expect("local addr");
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(server.run(tx, cancel.clone()));

        let mut client = LogsServiceClient::connect(format!("http://{addr}"))
            .await
            .expect("connect");
        client
            .export(ExportLogsServiceRequest {
                resource_logs: vec![
                    ResourceLogs {
                        resource: Some(Resource {
                            attributes: vec![KeyValue {
                                key: "host.name".to_owned(),
                                value: Some(string_value("web01")),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                        scope_logs: vec![ScopeLogs {
                            scope: Some(InstrumentationScope {
                                name: "app".to_owned(),
                                ..Default::default()
                            }),
                            log_records: vec![record("connection refused")],
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    // No resource at all: the fallback host is the peer IP,
                    // never host:port.
                    ResourceLogs {
                        resource: None,
                        scope_logs: vec![ScopeLogs {
                            scope: None,
                            log_records: vec![record("no resource here")],
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                ],
            })
            .await
            .expect("export");

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("an event");
        assert_eq!(event.raw_lossy(), "connection refused");
        assert_eq!(event.host, "web01");
        assert_eq!(event.source, Source::Otlp);
        assert_eq!(event.attrs.get("scope").map(String::as_str), Some("app"));

        let fallback = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no timeout")
            .expect("a second event");
        assert_eq!(fallback.raw_lossy(), "no resource here");
        assert_eq!(fallback.host, "127.0.0.1");

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("no timeout")
            .expect("join");
    }
}
