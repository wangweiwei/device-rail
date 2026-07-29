use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use devicerail_client::{
    CallOptions, ClientError, ClientOptions, ClientState, ControlTransportKind, DeviceRailClient,
    methods::{self, NoParams},
    protocol::{
        FeatureOffer, HelloParams, PeerInfo, RequestTimeoutMs, RpcId, RpcRequest, RpcResponse,
        SessionEndParams, feature, supported_protocol_offer,
    },
};
use devicerail_protocol::fixtures;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{mpsc, oneshot};

const HELLO_REQUEST: &str = fixtures::SYSTEM_HELLO_REQUEST_JSON;
const HELLO_RESPONSE: &str = fixtures::SYSTEM_HELLO_RESPONSE_JSON;
const DEVICES_LIST_REQUEST: &str = fixtures::DEVICES_LIST_REQUEST_JSON;
const DEVICES_LIST_RESPONSE: &str = fixtures::DEVICES_LIST_RESPONSE_JSON;
const DEVICE_CONNECT_REQUEST: &str = fixtures::DEVICE_CONNECT_REQUEST_JSON;
const DEVICE_CONNECT_RESPONSE: &str = fixtures::DEVICE_CONNECT_RESPONSE_JSON;
const RPC_FAILURE: &str = fixtures::RPC_FAILURE_JSON;
const FIXTURE_MANIFEST: &str = fixtures::MANIFEST_JSON;

fn fixture(source: &str) -> Value {
    serde_json::from_str(source).expect("golden fixture must contain valid JSON")
}

fn fixture_hello() -> HelloParams {
    let request: RpcRequest =
        serde_json::from_value(fixture(HELLO_REQUEST)).expect("typed hello request fixture");
    let mut hello: HelloParams = serde_json::from_value(
        request
            .params
            .expect("hello fixture has params")
            .into_value(),
    )
    .expect("typed hello params fixture");

    // The cross-language protocol fixture intentionally demonstrates a
    // multi-major offer. This client may only advertise versions it actually
    // implements, so retain the canonical v1 range for a live negotiation.
    hello
        .protocol
        .ranges
        .retain(|range| range.major == 1 && range.max_minor <= 5);
    hello
}

fn hello_with_features(features: &[&str]) -> HelloParams {
    HelloParams {
        client: PeerInfo {
            name: "rust-conformance-client".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        protocol: supported_protocol_offer(),
        features: FeatureOffer {
            required: BTreeSet::new(),
            optional: features
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
        },
    }
}

fn hello_response(id: RpcId, enabled_features: &[&str]) -> Value {
    let mut response = fixture(HELLO_RESPONSE);
    response["id"] = serde_json::to_value(id).expect("serialize response id");
    response["result"]["features"]["enabled"] = Value::Array(
        enabled_features
            .iter()
            .map(|feature| Value::String((*feature).to_owned()))
            .collect(),
    );
    response
}

async fn read_value<R>(reader: &mut R) -> Value
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = String::new();
    let read = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut frame))
        .await
        .expect("timed out waiting for fake-peer request")
        .expect("read fake-peer request");
    assert!(read > 0, "client request stream ended unexpectedly");
    assert!(frame.ends_with('\n'), "client request was not NDJSON");
    serde_json::from_str(frame.trim_end_matches(['\r', '\n'])).expect("client emitted valid JSON")
}

async fn write_value<W>(writer: &mut W, value: &Value)
where
    W: AsyncWrite + Unpin,
{
    let mut frame = serde_json::to_vec(value).expect("serialize fake-peer response");
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .await
        .expect("write fake-peer response");
    writer.flush().await.expect("flush fake-peer response");
}

async fn write_value_fragmented<W>(writer: &mut W, value: &Value)
where
    W: AsyncWrite + Unpin,
{
    let mut frame = serde_json::to_vec(value).expect("serialize fake-peer response");
    frame.push(b'\n');
    for chunk in frame.chunks(7) {
        writer
            .write_all(chunk)
            .await
            .expect("write fragmented fake-peer response");
        tokio::task::yield_now().await;
    }
    writer
        .flush()
        .await
        .expect("flush fragmented fake-peer response");
}

fn request_id(value: &Value) -> RpcId {
    serde_json::from_value(value["id"].clone()).expect("request has a valid RPC id")
}

async fn attach(client_io: tokio::io::DuplexStream, hello: HelloParams) -> DeviceRailClient {
    let (reader, writer) = tokio::io::split(client_io);
    DeviceRailClient::attach(
        reader,
        writer,
        ControlTransportKind::Stdio,
        hello,
        ClientOptions::default(),
    )
    .await
    .expect("attach and negotiate hello")
}

async fn drain_request_stream<R>(reader: &mut R) -> Vec<u8>
where
    R: AsyncRead + Unpin,
{
    let mut trailing = Vec::new();
    reader
        .read_to_end(&mut trailing)
        .await
        .expect("drain client request stream");
    trailing
}

#[tokio::test]
async fn attach_and_typed_call_consume_canonical_golden_fixtures() {
    let hello = fixture_hello();
    let expected_features = hello.features.optional.clone();
    let expected_hello = {
        let mut expected = fixture(HELLO_REQUEST);
        expected["params"]["protocol"] =
            serde_json::to_value(&hello.protocol).expect("serialize supported protocol offer");
        expected
    };
    let canonical_hello_result: devicerail_client::protocol::HelloResult =
        serde_json::from_value(fixture(HELLO_RESPONSE)["result"].clone())
            .expect("typed canonical hello result");
    let canonical_devices_result: devicerail_client::protocol::DevicesListResult =
        serde_json::from_value(fixture(DEVICES_LIST_RESPONSE)["result"].clone())
            .expect("typed canonical devices result");

    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);

        let actual_hello = read_value(&mut peer_reader).await;
        let mut expected_hello = expected_hello;
        expected_hello["id"] = actual_hello["id"].clone();
        assert_eq!(actual_hello, expected_hello);

        let mut hello_response = fixture(HELLO_RESPONSE);
        hello_response["id"] = actual_hello["id"].clone();
        write_value_fragmented(&mut peer_writer, &hello_response).await;

        let actual_devices = read_value(&mut peer_reader).await;
        let mut expected_devices = fixture(DEVICES_LIST_REQUEST);
        expected_devices["id"] = actual_devices["id"].clone();
        assert_eq!(actual_devices, expected_devices);

        let mut devices_response = fixture(DEVICES_LIST_RESPONSE);
        devices_response["id"] = actual_devices["id"].clone();
        write_value(&mut peer_writer, &devices_response).await;

        assert!(
            drain_request_stream(&mut peer_reader).await.is_empty(),
            "close must not emit an extra RPC frame"
        );
    });

    let client = attach(client_io, hello).await;
    assert_eq!(client.state(), ClientState::Ready);
    assert_eq!(client.enabled_features(), expected_features);
    assert_eq!(
        canonical_hello_result.features.enabled,
        client.enabled_features()
    );
    assert_eq!(
        client
            .negotiated_hello()
            .expect("read negotiated hello result"),
        canonical_hello_result
    );

    let devices = client
        .call::<methods::DevicesList>(NoParams, CallOptions::default())
        .await
        .expect("decode canonical devices.list result");
    assert_eq!(devices, canonical_devices_result);

    client.close().await.expect("close client");
    peer.await.expect("fake peer completed");
}

#[tokio::test]
async fn concurrent_responses_are_correlated_when_the_peer_replies_out_of_order() {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);

        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[]),
        )
        .await;

        let first = read_value(&mut peer_reader).await;
        let second = read_value(&mut peer_reader).await;
        assert_eq!(first["method"], "device.disconnect");
        assert_eq!(second["method"], "device.disconnect");
        assert_ne!(first["id"], second["id"]);

        let second_response = json!({
            "jsonrpc": "2.0",
            "id": second["id"].clone(),
            "result": { "disconnected": true }
        });
        let first_response = json!({
            "jsonrpc": "2.0",
            "id": first["id"].clone(),
            "result": { "disconnected": false }
        });
        let mut coalesced = serde_json::to_vec(&second_response).expect("second response");
        coalesced.push(b'\n');
        coalesced.extend(serde_json::to_vec(&first_response).expect("first response"));
        coalesced.push(b'\n');
        peer_writer
            .write_all(&coalesced)
            .await
            .expect("write coalesced reversed responses");
        peer_writer.flush().await.expect("flush reversed responses");

        drain_request_stream(&mut peer_reader).await
    });

    let client = attach(client_io, hello).await;
    let first = client
        .begin_call::<methods::DeviceDisconnect>(NoParams, CallOptions::default())
        .expect("start first call");
    let second = client
        .begin_call::<methods::DeviceDisconnect>(NoParams, CallOptions::default())
        .expect("start second call");
    assert_eq!(client.pending_requests(), 2);

    let (first_result, second_result) = tokio::join!(first.result(), second.result());
    assert!(!first_result.expect("first result").disconnected);
    assert!(second_result.expect("second result").disconnected);
    assert_eq!(client.pending_requests(), 0);

    client.close().await.expect("close client");
    assert!(peer.await.expect("fake peer completed").is_empty());
}

#[tokio::test]
async fn canonical_remote_rpc_failure_is_non_terminal_and_preserves_the_request_id() {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);

        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[]),
        )
        .await;

        let connect = read_value(&mut peer_reader).await;
        let mut expected = fixture(DEVICE_CONNECT_REQUEST);
        expected["id"] = connect["id"].clone();
        assert_eq!(connect, expected);

        let mut failure = fixture(RPC_FAILURE);
        failure["id"] = connect["id"].clone();
        write_value(&mut peer_writer, &failure).await;

        drain_request_stream(&mut peer_reader).await
    });

    let client = attach(client_io, hello).await;
    let handle = client
        .begin_call::<methods::DeviceConnect>(NoParams, CallOptions::default())
        .expect("start device.connect");
    let expected_id = handle.id().clone();
    let error = handle.result().await.expect_err("remote call must fail");
    match error {
        ClientError::RemoteRpc { request_id, error } => {
            assert_eq!(request_id, expected_id);
            assert_eq!(error.code, -32_000);
            assert_eq!(error.message, "Device unavailable");
            assert_eq!(error.data.code, "device_unavailable");
            assert!(error.data.retryable);
        }
        other => panic!("expected remote RPC error, got {other:?}"),
    }
    assert_eq!(client.state(), ClientState::Ready);
    assert_eq!(client.pending_requests(), 0);

    client.close().await.expect("close client");
    assert!(peer.await.expect("fake peer completed").is_empty());
}

#[derive(Clone, Copy)]
enum TerminalResponse {
    UnknownId,
    NullId,
}

async fn assert_terminal_response_failure(response: TerminalResponse) {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);

        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[]),
        )
        .await;

        let request = read_value(&mut peer_reader).await;
        let invalid = match response {
            TerminalResponse::UnknownId => json!({
                "jsonrpc": "2.0",
                "id": format!("{}-unknown", request["id"].as_str().expect("string id")),
                "result": { "disconnected": true }
            }),
            TerminalResponse::NullId => {
                let mut failure = fixture(RPC_FAILURE);
                failure["id"] = Value::Null;
                failure
            }
        };
        write_value(&mut peer_writer, &invalid).await;
        drain_request_stream(&mut peer_reader).await
    });

    let client = attach(client_io, hello).await;
    let handle = client
        .begin_call::<methods::DeviceDisconnect>(NoParams, CallOptions::default())
        .expect("start pending request");
    let error = tokio::time::timeout(Duration::from_secs(2), handle.result())
        .await
        .expect("pending request must settle after terminal failure")
        .expect_err("invalid response must fail");
    assert!(
        matches!(error, ClientError::ProtocolViolation(_)),
        "unexpected terminal error: {error:?}"
    );
    assert_eq!(client.state(), ClientState::Failed);
    assert_eq!(client.pending_requests(), 0);

    client.close().await.expect("close failed client");
    assert!(peer.await.expect("fake peer completed").is_empty());
}

#[tokio::test]
async fn response_with_unknown_id_fails_the_connection_and_settles_pending_work() {
    assert_terminal_response_failure(TerminalResponse::UnknownId).await;
}

#[tokio::test]
async fn response_with_null_id_fails_the_connection_and_settles_pending_work() {
    assert_terminal_response_failure(TerminalResponse::NullId).await;
}

#[tokio::test]
async fn feature_and_timeout_admission_fail_locally_without_writing_frames() {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);
        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[]),
        )
        .await;
        drain_request_stream(&mut peer_reader).await
    });

    let client = attach(client_io, hello).await;

    let feature_error = client
        .begin_call::<methods::DevicesList>(NoParams, CallOptions::default())
        .expect_err("devices.list requires routing");
    assert!(matches!(
        feature_error,
        ClientError::FeatureNotNegotiated {
            method: "devices.list",
            feature: feature::DEVICE_ROUTING_V1
        }
    ));

    let timeout = RequestTimeoutMs::new(25).expect("positive timeout");
    let method_error = client
        .begin_call::<methods::SystemDescribe>(
            NoParams,
            CallOptions {
                timeout_ms: Some(timeout),
            },
        )
        .expect_err("system.describe does not support timeoutMs");
    assert!(
        matches!(
            method_error,
            ClientError::ProtocolViolation(ref message)
                if message == "system.describe does not support timeoutMs"
        ),
        "unexpected timeout admission error: {method_error:?}"
    );

    let control_error = client
        .begin_call::<methods::DeviceConnect>(
            NoParams,
            CallOptions {
                timeout_ms: Some(timeout),
            },
        )
        .expect_err("timeout requires request.control.v1");
    assert!(matches!(
        control_error,
        ClientError::FeatureNotNegotiated {
            method: "device.connect",
            feature: feature::REQUEST_CONTROL_V1
        }
    ));
    assert_eq!(client.pending_requests(), 0);

    client.close().await.expect("close client");
    assert!(
        peer.await.expect("fake peer completed").is_empty(),
        "locally rejected calls must not reach the transport"
    );
}

#[tokio::test]
async fn timeout_is_serialized_after_request_control_is_negotiated() {
    let hello = hello_with_features(&[feature::REQUEST_CONTROL_V1]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);

        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[feature::REQUEST_CONTROL_V1]),
        )
        .await;

        let connect = read_value(&mut peer_reader).await;
        let mut expected = fixture(DEVICE_CONNECT_REQUEST);
        expected["id"] = connect["id"].clone();
        expected["timeoutMs"] = json!(1_234);
        assert_eq!(connect, expected);

        let mut response = fixture(DEVICE_CONNECT_RESPONSE);
        response["id"] = connect["id"].clone();
        write_value(&mut peer_writer, &response).await;
        drain_request_stream(&mut peer_reader).await
    });

    let client = attach(client_io, hello).await;
    let connected = client
        .call::<methods::DeviceConnect>(
            NoParams,
            CallOptions {
                timeout_ms: RequestTimeoutMs::new(1_234),
            },
        )
        .await
        .expect("device.connect with timeout");
    assert_eq!(connected.id.0, "mock-device-1");
    assert!(connected.connected);

    client.close().await.expect("close client");
    assert!(peer.await.expect("fake peer completed").is_empty());
}

#[tokio::test]
async fn dropping_an_in_flight_hello_is_a_terminal_transport_failure() {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let (reader, writer) = tokio::io::split(client_io);
    let client = DeviceRailClient::attach_unnegotiated(
        reader,
        writer,
        ControlTransportKind::Stdio,
        ClientOptions::default(),
    )
    .expect("attach unnegotiated client");

    let pending_client = client.clone();
    let pending = tokio::spawn(async move { pending_client.hello(hello).await });
    let (peer_reader, _peer_writer) = tokio::io::split(peer_io);
    let mut peer_reader = BufReader::new(peer_reader);
    let request = read_value(&mut peer_reader).await;
    assert_eq!(request["method"], "system.hello");

    pending.abort();
    assert!(
        pending
            .await
            .expect_err("hello task was aborted")
            .is_cancelled()
    );
    tokio::task::yield_now().await;
    assert_eq!(client.state(), ClientState::Failed);
    client.close().await.expect("close failed client");
}

#[tokio::test]
async fn aborting_a_result_future_sends_cancel_and_absorbs_the_late_response() {
    let hello = hello_with_features(&[feature::REQUEST_CONTROL_V1]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let (request_read, request_seen) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);
        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[feature::REQUEST_CONTROL_V1]),
        )
        .await;

        let original = read_value(&mut peer_reader).await;
        assert_eq!(original["method"], "device.disconnect");
        request_read
            .send(request_id(&original))
            .expect("publish original request id");

        let cancel = read_value(&mut peer_reader).await;
        assert_eq!(cancel["method"], "request.cancel");
        assert_eq!(cancel["params"]["requestId"], original["id"]);
        write_value(
            &mut peer_writer,
            &json!({
                "jsonrpc": "2.0",
                "id": cancel["id"].clone(),
                "result": {
                    "requestId": original["id"].clone(),
                    "status": "requested"
                }
            }),
        )
        .await;
        write_value(
            &mut peer_writer,
            &json!({
                "jsonrpc": "2.0",
                "id": original["id"].clone(),
                "result": { "disconnected": true }
            }),
        )
        .await;

        let follow_up = read_value(&mut peer_reader).await;
        assert_eq!(follow_up["method"], "device.disconnect");
        write_value(
            &mut peer_writer,
            &json!({
                "jsonrpc": "2.0",
                "id": follow_up["id"].clone(),
                "result": { "disconnected": false }
            }),
        )
        .await;
        drain_request_stream(&mut peer_reader).await
    });

    let client = attach(client_io, hello).await;
    let handle = client
        .begin_call::<methods::DeviceDisconnect>(NoParams, CallOptions::default())
        .expect("start request that will be abandoned");
    let expected_id = handle.id().clone();
    let result_task = tokio::spawn(handle.result());
    assert_eq!(
        request_seen.await.expect("peer observed original request"),
        expected_id
    );
    result_task.abort();
    assert!(
        result_task
            .await
            .expect_err("result task was aborted")
            .is_cancelled()
    );

    let follow_up = client
        .call::<methods::DeviceDisconnect>(NoParams, CallOptions::default())
        .await
        .expect("connection remains usable after a late abandoned response");
    assert!(!follow_up.disconnected);
    assert_eq!(client.state(), ClientState::Ready);
    assert_eq!(client.pending_requests(), 0);

    client.close().await.expect("close client");
    assert!(peer.await.expect("fake peer completed").is_empty());
}

#[tokio::test]
async fn abandoned_request_budget_fails_closed_without_remote_cancellation() {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(64 * 1024);
    let (observed, mut observations) = mpsc::channel(3);
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);
        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[]),
        )
        .await;
        for _ in 0..3 {
            let request = read_value(&mut peer_reader).await;
            assert_eq!(request["method"], "device.disconnect");
            observed.send(()).await.expect("publish observed request");
        }
        drain_request_stream(&mut peer_reader).await
    });
    let options = ClientOptions {
        max_pending_requests: 2,
        ..ClientOptions::default()
    };
    let (reader, writer) = tokio::io::split(client_io);
    let client =
        DeviceRailClient::attach(reader, writer, ControlTransportKind::Stdio, hello, options)
            .await
            .expect("attach client with a small abandoned budget");

    for index in 0..3 {
        let handle = client
            .begin_call::<methods::DeviceDisconnect>(NoParams, CallOptions::default())
            .expect("abandoned request admission");
        observations
            .recv()
            .await
            .expect("peer observed abandoned request");
        drop(handle);
        assert_eq!(client.pending_requests(), 0);
        if index < 2 {
            assert_eq!(client.state(), ClientState::Ready);
        }
    }
    assert_eq!(client.state(), ClientState::Failed);
    client.close().await.expect("close failed client");
    assert!(peer.await.expect("fake peer completed").is_empty());
}

#[tokio::test]
async fn close_is_bounded_and_survives_cancellation_of_the_first_waiter() {
    let hello = hello_with_features(&[]);
    let (client_io, peer_io) = tokio::io::duplex(128);
    let (blocked, request_blocked) = oneshot::channel();
    let (release_peer, peer_release) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (peer_reader, mut peer_writer) = tokio::io::split(peer_io);
        let mut peer_reader = BufReader::new(peer_reader);
        let hello_request = read_value(&mut peer_reader).await;
        write_value(
            &mut peer_writer,
            &hello_response(request_id(&hello_request), &[]),
        )
        .await;
        let mut prefix = [0_u8; 128];
        peer_reader
            .read_exact(&mut prefix)
            .await
            .expect("read enough request bytes to block the writer");
        blocked.send(()).expect("publish blocked writer state");
        let _ = peer_release.await;
    });
    let options = ClientOptions {
        close_grace: Duration::from_millis(50),
        ..ClientOptions::default()
    };
    let (reader, writer) = tokio::io::split(client_io);
    let client =
        DeviceRailClient::attach(reader, writer, ControlTransportKind::Stdio, hello, options)
            .await
            .expect("attach client");
    let pending = client
        .begin_call::<methods::SessionEnd>(
            Some(SessionEndParams {
                outcome: None,
                reason: Some("x".repeat(32 * 1024)),
            }),
            CallOptions::default(),
        )
        .expect("enqueue a request larger than the duplex capacity");
    request_blocked.await.expect("writer reached backpressure");

    let close_client = client.clone();
    let first_waiter = tokio::spawn(async move { close_client.close().await });
    while client.state() != ClientState::Closing {
        tokio::task::yield_now().await;
    }
    first_waiter.abort();
    assert!(
        first_waiter
            .await
            .expect_err("first close waiter was aborted")
            .is_cancelled()
    );
    let close_error = tokio::time::timeout(Duration::from_secs(1), client.close())
        .await
        .expect("background close cleanup must remain bounded")
        .expect_err("forced writer abort is reported");
    assert!(
        matches!(close_error, ClientError::Transport(_)),
        "unexpected close error: {close_error:?}"
    );
    assert_eq!(client.state(), ClientState::Closed);
    drop(pending);
    let _ = release_peer.send(());
    peer.await.expect("fake peer completed");
}

#[test]
fn attach_unnegotiated_requires_an_active_tokio_runtime() {
    let error = DeviceRailClient::attach_unnegotiated(
        tokio::io::empty(),
        tokio::io::sink(),
        ControlTransportKind::Stdio,
        ClientOptions::default(),
    )
    .expect_err("synchronous attach without a runtime must be explicit");
    assert!(matches!(error, ClientError::RuntimeUnavailable));
}

#[test]
fn canonical_envelopes_round_trip_through_the_protocol_reexport() {
    for source in [HELLO_REQUEST, DEVICES_LIST_REQUEST, DEVICE_CONNECT_REQUEST] {
        let value = fixture(source);
        let request: RpcRequest =
            serde_json::from_value(value.clone()).expect("deserialize canonical request");
        assert_eq!(
            serde_json::to_value(request).expect("serialize canonical request"),
            value
        );
    }

    for source in [
        HELLO_RESPONSE,
        DEVICES_LIST_RESPONSE,
        DEVICE_CONNECT_RESPONSE,
        RPC_FAILURE,
    ] {
        let value = fixture(source);
        let response: RpcResponse =
            serde_json::from_value(value.clone()).expect("deserialize canonical response");
        assert_eq!(
            serde_json::to_value(response).expect("serialize canonical response"),
            value
        );
    }
}

#[test]
fn embedded_fixture_catalog_exactly_matches_the_canonical_manifest() {
    let manifest = fixture(FIXTURE_MANIFEST);
    let entries = manifest["fixtures"]
        .as_array()
        .expect("fixture manifest has fixtures");
    let manifest_paths = entries
        .iter()
        .map(|entry| entry["path"].as_str().expect("fixture path"))
        .collect::<BTreeSet<_>>();
    let embedded_paths = fixtures::CANONICAL_FIXTURES
        .iter()
        .map(|fixture| fixture.path)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        manifest_paths.len(),
        entries.len(),
        "canonical manifest contains duplicate paths"
    );
    assert_eq!(
        embedded_paths.len(),
        fixtures::CANONICAL_FIXTURES.len(),
        "embedded fixture catalog contains duplicate paths"
    );
    assert_eq!(
        embedded_paths, manifest_paths,
        "embedded fixture catalog drifted from the canonical manifest"
    );
}

#[test]
fn method_catalog_has_one_canonical_request_and_success_response_per_method() {
    let manifest = fixture(FIXTURE_MANIFEST);
    let entries = manifest["fixtures"]
        .as_array()
        .expect("fixture manifest has fixtures");
    let mut request_schemas = BTreeMap::<String, Vec<String>>::new();
    let mut success_responses = BTreeMap::<String, usize>::new();
    for entry in entries {
        let kind = entry["kind"].as_str().expect("fixture kind");
        let path = entry["path"].as_str().expect("fixture path");
        if path.starts_with("stream/websocket-system-hello") {
            continue;
        }
        let schema = entry["schema"].as_str().expect("fixture schema");
        let source = fixtures::fixture_json(path)
            .unwrap_or_else(|| panic!("canonical fixture {path} was not embedded"));
        if kind.ends_with("Request") {
            let request: RpcRequest =
                serde_json::from_str(source).expect("canonical request envelope");
            request_schemas
                .entry(request.method)
                .or_default()
                .push(schema.to_owned());
        } else if kind.ends_with("Response") {
            let response: RpcResponse =
                serde_json::from_str(source).expect("canonical response envelope");
            assert!(
                matches!(response, RpcResponse::Success { .. }),
                "{path} must be the success fixture for its method"
            );
            *success_responses.entry(schema.to_owned()).or_default() += 1;
        }
    }

    assert_eq!(methods::METHOD_CATALOG.len(), 24);
    assert_eq!(request_schemas.len(), methods::METHOD_CATALOG.len());
    assert_eq!(success_responses.len(), methods::METHOD_CATALOG.len());

    for descriptor in methods::METHOD_CATALOG {
        let expected_channel = match descriptor.name {
            "system.hello" => methods::MethodChannel::Bootstrap,
            "events.subscribe" => methods::MethodChannel::EventWebSocket,
            _ => methods::MethodChannel::Control,
        };
        assert_eq!(
            descriptor.channel, expected_channel,
            "{} is assigned to the wrong transport channel",
            descriptor.name
        );

        let schemas = request_schemas
            .get(descriptor.name)
            .unwrap_or_else(|| panic!("{} has no canonical request fixture", descriptor.name));
        assert_eq!(
            schemas.len(),
            1,
            "{} must have exactly one canonical request fixture",
            descriptor.name
        );
        let response_schema = schemas[0].replace("-request.schema.json", "-response.schema.json");
        assert_eq!(
            success_responses.get(&response_schema),
            Some(&1),
            "{} must have exactly one canonical success response fixture",
            descriptor.name
        );
    }
}
