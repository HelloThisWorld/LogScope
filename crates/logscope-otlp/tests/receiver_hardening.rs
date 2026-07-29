//! Receiver hardening evidence: disabled by default, loopback-only,
//! bounded bodies, bounded buffering.

use logscope_otlp::*;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use prost::Message;

#[tokio::test(flavor = "multi_thread")]
async fn receiver_is_disabled_by_default() {
    let handle = start(OtlpReceiverConfig::default()).await.unwrap();
    assert!(handle.http_addr.is_none());
    assert!(handle.grpc_addr.is_none());
    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn http_binds_loopback_and_enforces_body_limit() {
    let handle = start(OtlpReceiverConfig {
        http_port: Some(0),
        max_body_bytes: 1024,
        ..Default::default()
    })
    .await
    .unwrap();
    let addr = handle.http_addr.unwrap();
    assert!(addr.ip().is_loopback());

    let client = reqwest::Client::new();

    // Oversized body -> 413.
    let big = vec![0u8; 10_000];
    let r = client
        .post(format!("http://{addr}/v1/logs"))
        .header("content-type", "application/x-protobuf")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);

    // Wrong content type -> 415.
    let r = client
        .post(format!("http://{addr}/v1/logs"))
        .header("content-type", "text/plain")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 415);

    // Garbage protobuf -> 400, never a crash.
    let r = client
        .post(format!("http://{addr}/v1/logs"))
        .header("content-type", "application/x-protobuf")
        .body(vec![0xFFu8; 64])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn full_buffer_answers_429_without_losing_accepted_envelopes() {
    let handle = start(OtlpReceiverConfig {
        http_port: Some(0),
        channel_capacity: 1,
        ..Default::default()
    })
    .await
    .unwrap();
    let addr = handle.http_addr.unwrap();
    let body = ExportLogsServiceRequest::default().encode_to_vec();
    let client = reqwest::Client::new();

    let first = client
        .post(format!("http://{addr}/v1/logs"))
        .header("content-type", "application/x-protobuf")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200);

    // Nothing drained the single-slot channel: next request is rejected.
    let second = client
        .post(format!("http://{addr}/v1/logs"))
        .header("content-type", "application/x-protobuf")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 429);

    let mut handle = handle;
    assert!(handle.envelopes.recv().await.is_some());
    handle.shutdown().await;
}
