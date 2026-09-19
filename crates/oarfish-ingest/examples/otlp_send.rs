//! Smoke probe: send one OTLP log record to a running daemon.
//!
//! ```sh
//! cargo run -p oarfish -- --syslog 127.0.0.1:5514 --otlp 127.0.0.1:44317 &
//! cargo run -p oarfish-ingest --example otlp_send -- 127.0.0.1:44317
//! ```
//!
//! The daemon should log the record's `TemplateId`.

use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, logs_service_client::LogsServiceClient,
};
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value::Value};
use opentelemetry_proto::tonic::logs::v1::{ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;

fn string_value(s: &str) -> AnyValue {
    AnyValue {
        value: Some(Value::StringValue(s.to_owned())),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:4317".to_owned());
    let mut client = LogsServiceClient::connect(format!("http://{addr}")).await?;
    let response = client
        .export(ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "host.name".to_owned(),
                        value: Some(string_value("probe")),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![opentelemetry_proto::tonic::logs::v1::LogRecord {
                        body: Some(string_value("probe connection refused from 10.0.0.9")),
                        time_unix_nano: 1_789_812_000_000_000_000,
                        severity_text: "ERROR".to_owned(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        })
        .await?;
    println!("export ok: {:?}", response.into_inner());
    Ok(())
}
