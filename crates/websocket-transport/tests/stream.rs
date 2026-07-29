use std::time::Duration;

use devicerail_core::{
    EndSession, MemoryEventStore, PendingEvent, SessionEventStore, StartSession,
};
use devicerail_protocol::{
    AssetRef, DeviceId, ErrorInfo, EventSequence, EventStreamCursor, EventStreamOriginPolicy,
    EventsStreamOpenParams, MediaFrame, MediaStreamId, MediaStreamInfo, MediaStreamKind,
    Observation, RpcResponse, SessionOutcome, TestEventPayload, UiSnapshotOmissionReason, Viewport,
    feature,
};
use devicerail_websocket_transport::{Config, EventStreamServer, TransportError};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    WebSocketStream, client_async,
    tungstenite::{
        Message,
        client::IntoClientRequest as _,
        http::{HeaderValue, header},
    },
};

const SESSION: &str = "33333333-3333-4333-8333-333333333333";

async fn bind_or_skip(events: std::sync::Arc<MemoryEventStore>) -> Option<EventStreamServer> {
    match EventStreamServer::bind(events, Config::default()).await {
        Ok(server) => Some(server),
        Err(TransportError::Bind(error))
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && matches!(
                    std::env::var("DEVICERAIL_ALLOW_NO_LOOPBACK").as_deref(),
                    Ok("1")
                ) =>
        {
            // Hermetic local runners may explicitly acknowledge that they
            // prohibit AF_INET. CI must never hide a loopback regression.
            None
        }
        Err(error) => panic!("bind stream server: {error}"),
    }
}

async fn connect(
    server: &EventStreamServer,
    endpoint: &str,
    origin: Option<&str>,
) -> Result<WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error> {
    let mut request = endpoint.into_client_request()?;
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static("devicerail.events.v1"),
    );
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_EXTENSIONS,
        HeaderValue::from_static("permessage-deflate; client_max_window_bits"),
    );
    if let Some(origin) = origin {
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(origin).expect("test Origin is valid"),
        );
    }
    let tcp = TcpStream::connect(server.local_addr())
        .await
        .expect("connect loopback stream server");
    let (websocket, response) = client_async(request, tcp).await?;
    assert_eq!(
        response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok()),
        Some("devicerail.events.v1")
    );
    assert!(
        response
            .headers()
            .get(header::SEC_WEBSOCKET_EXTENSIONS)
            .is_none(),
        "compression offers must not be negotiated"
    );
    Ok(websocket)
}

async fn send_json(websocket: &mut WebSocketStream<TcpStream>, value: Value) {
    websocket
        .send(Message::text(
            serde_json::to_string(&value).expect("serialize test message"),
        ))
        .await
        .expect("send test message");
}

async fn receive_json(websocket: &mut WebSocketStream<TcpStream>) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(2), websocket.next())
            .await
            .expect("receive is bounded")
            .expect("socket remains open")
            .expect("valid WebSocket message");
        match message {
            Message::Text(text) => {
                return serde_json::from_str(text.as_str()).expect("server text is JSON");
            }
            Message::Ping(payload) => websocket
                .send(Message::Pong(payload))
                .await
                .expect("reply pong"),
            Message::Pong(_) => {}
            Message::Close(frame) => panic!("unexpected early close: {frame:?}"),
            Message::Binary(_) | Message::Frame(_) => panic!("unexpected non-text server data"),
        }
    }
}

async fn hello_version(websocket: &mut WebSocketStream<TcpStream>, minor: u16) {
    send_json(
        websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "hello",
            "method": "system.hello",
            "params": {
                "client": { "name": "transport-test", "version": "0.1.0" },
                "protocol": { "ranges": [{ "major": 1, "minMinor": minor, "maxMinor": minor }] },
                "features": { "required": [feature::EVENTS_STREAM_V1], "optional": [] }
            }
        }),
    )
    .await;
    let response = receive_json(websocket).await;
    let response: RpcResponse = serde_json::from_value(response).expect("typed hello response");
    let result = response.result().expect("hello success");
    assert_eq!(
        result["protocol"]["selected"],
        json!({ "major": 1, "minor": minor })
    );
    assert_eq!(
        result["transport"],
        json!({ "kind": "webSocket", "framing": "jsonMessage" })
    );
}

async fn hello(websocket: &mut WebSocketStream<TcpStream>) {
    hello_version(websocket, 3).await;
}

fn media_stream() -> MediaStreamInfo {
    MediaStreamInfo {
        id: MediaStreamId::new(),
        kind: MediaStreamKind::Video,
        media_type: "video/webm".to_owned(),
        viewport: None,
    }
}

#[tokio::test]
async fn snapshot_tail_end_and_terminal_form_one_continuous_prefix() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let start = StartSession::new(None, None, 1);
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start Session");
    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let opened = server
        .open(EventsStreamOpenParams {
            session_id: session_id.clone(),
            origin_policy: EventStreamOriginPolicy::Absent {},
        })
        .expect("open capability");
    assert!(!format!("{opened:?}").contains(opened.endpoint.expose_secret()));

    let mut websocket = connect(&server, opened.endpoint.expose_secret(), None)
        .await
        .expect("upgrade");
    hello(&mut websocket).await;
    send_json(
        &mut websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "subscribe",
            "method": "events.subscribe",
            "params": { "sessionId": session_id }
        }),
    )
    .await;
    let subscribed = receive_json(&mut websocket).await;
    assert_eq!(subscribed["result"]["replayThrough"]["sequence"], 1);
    let subscription_id = subscribed["result"]["subscriptionId"]
        .as_str()
        .expect("subscription id")
        .to_owned();
    let first = receive_json(&mut websocket).await;
    assert_eq!(first["method"], "events.stream.event");
    assert_eq!(first["params"]["event"]["sequence"], 1);

    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 2,
            payload: TestEventPayload::Error {
                error: ErrorInfo {
                    code: "test".to_owned(),
                    message: "test".to_owned(),
                    retryable: false,
                    details: None,
                },
            },
        })
        .await
        .expect("append live event");
    assert_eq!(
        receive_json(&mut websocket).await["params"]["cursor"]["sequence"],
        2
    );
    events
        .end_session(EndSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 3,
            outcome: SessionOutcome::Completed,
            reason: None,
        })
        .await
        .expect("end Session");
    assert_eq!(
        receive_json(&mut websocket).await["params"]["event"]["sequence"],
        3
    );
    let terminal = receive_json(&mut websocket).await;
    assert_eq!(terminal["method"], "events.stream.terminal");
    assert_eq!(terminal["params"]["subscriptionId"], subscription_id);
    assert_eq!(terminal["params"]["lastEmittedCursor"]["sequence"], 3);
    assert_eq!(terminal["params"]["termination"]["reason"], "sessionEnded");

    server.begin_shutdown();
    server.finish_shutdown().await.expect("clean shutdown");
    assert_eq!(server.stats().active_connections, 0);
}

#[tokio::test]
async fn protocol_13_terminates_before_emitting_protocol_14_media_events() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let start = StartSession::new(None, None, 1);
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start Session");
    let stream = media_stream();
    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 2,
            payload: TestEventPayload::MediaStreamStarted { stream },
        })
        .await
        .expect("append media start");

    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let opened = server
        .open(EventsStreamOpenParams {
            session_id: session_id.clone(),
            origin_policy: EventStreamOriginPolicy::Absent {},
        })
        .expect("open capability");
    let mut websocket = connect(&server, opened.endpoint.expose_secret(), None)
        .await
        .expect("upgrade");
    hello_version(&mut websocket, 3).await;
    send_json(
        &mut websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "subscribe",
            "method": "events.subscribe",
            "params": { "sessionId": session_id }
        }),
    )
    .await;
    let subscribed = receive_json(&mut websocket).await;
    assert_eq!(subscribed["result"]["replayThrough"]["sequence"], 2);
    let first = receive_json(&mut websocket).await;
    assert_eq!(
        first["params"]["event"]["payload"]["type"],
        "sessionStarted"
    );
    let terminal = receive_json(&mut websocket).await;
    assert_eq!(terminal["method"], "events.stream.terminal");
    assert_eq!(
        terminal["params"]["termination"]["error"]["code"],
        "stream_event_protocol_incompatible"
    );
    assert_eq!(terminal["params"]["lastEmittedCursor"]["sequence"], 1);

    server.begin_shutdown();
    server.finish_shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn protocol_14_rejects_protocol_15_observation_fields_before_serialization() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let start = StartSession::new(None, None, 1);
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start Session");
    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 2,
            payload: TestEventPayload::ObservationCaptured {
                observation: Box::new(Observation {
                    id: uuid::Uuid::from_u128(42),
                    device_id: DeviceId::new("mock-1"),
                    captured_at_ms: 2,
                    viewport: Viewport {
                        width: 100,
                        height: 100,
                        scale_factor: 1.0,
                    },
                    screenshot: None,
                    screenshot_omission: None,
                    ui_snapshot: None,
                    ui_snapshot_omission: Some(UiSnapshotOmissionReason::DriverUnsupported),
                    metadata: serde_json::Map::new(),
                }),
            },
        })
        .await
        .expect("append observation");

    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let opened = server
        .open(EventsStreamOpenParams {
            session_id: session_id.clone(),
            origin_policy: EventStreamOriginPolicy::Absent {},
        })
        .expect("open capability");
    let mut websocket = connect(&server, opened.endpoint.expose_secret(), None)
        .await
        .expect("upgrade");
    hello_version(&mut websocket, 4).await;
    send_json(
        &mut websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "subscribe",
            "method": "events.subscribe",
            "params": { "sessionId": session_id }
        }),
    )
    .await;
    let _subscribed = receive_json(&mut websocket).await;
    let first = receive_json(&mut websocket).await;
    assert_eq!(
        first["params"]["event"]["payload"]["type"],
        "sessionStarted"
    );
    let terminal = receive_json(&mut websocket).await;
    assert_eq!(terminal["method"], "events.stream.terminal");
    assert_eq!(
        terminal["params"]["termination"]["error"]["details"]["requiredProtocol"],
        json!({ "major": 1, "minor": 5 })
    );
    assert_eq!(terminal["params"]["lastEmittedCursor"]["sequence"], 1);

    server.begin_shutdown();
    server.finish_shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn protocol_14_emits_the_complete_media_lifecycle() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let start = StartSession::new(None, None, 1);
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start Session");
    let stream = media_stream();
    let stream_id = stream.id.clone();
    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 2,
            payload: TestEventPayload::MediaStreamStarted { stream },
        })
        .await
        .expect("append media start");
    let digest = "a".repeat(64);
    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 3,
            payload: TestEventPayload::MediaFrameCaptured {
                frame: MediaFrame {
                    stream_id: stream_id.clone(),
                    frame_index: EventSequence::FIRST,
                    key_frame: true,
                    duration_ms: Some(20),
                    evidence: AssetRef {
                        id: format!("sha256:{digest}"),
                        media_type: "video/webm".to_owned(),
                        uri: format!("devicerail://assets/sha256/{digest}"),
                        sha256: Some(digest),
                    },
                },
            },
        })
        .await
        .expect("append media frame");
    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 4,
            payload: TestEventPayload::MediaStreamEnded {
                stream_id,
                frame_count: 1,
            },
        })
        .await
        .expect("append media end");
    events
        .end_session(EndSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 5,
            outcome: SessionOutcome::Completed,
            reason: None,
        })
        .await
        .expect("end Session");

    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let opened = server
        .open(EventsStreamOpenParams {
            session_id: session_id.clone(),
            origin_policy: EventStreamOriginPolicy::Absent {},
        })
        .expect("open capability");
    let mut websocket = connect(&server, opened.endpoint.expose_secret(), None)
        .await
        .expect("upgrade");
    hello_version(&mut websocket, 4).await;
    send_json(
        &mut websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "subscribe",
            "method": "events.subscribe",
            "params": { "sessionId": session_id }
        }),
    )
    .await;
    assert_eq!(
        receive_json(&mut websocket).await["result"]["replayThrough"]["sequence"],
        5
    );
    let mut payload_types = Vec::new();
    for _ in 0..5 {
        payload_types.push(
            receive_json(&mut websocket).await["params"]["event"]["payload"]["type"]
                .as_str()
                .expect("payload type")
                .to_owned(),
        );
    }
    assert_eq!(
        payload_types,
        [
            "sessionStarted",
            "mediaStreamStarted",
            "mediaFrameCaptured",
            "mediaStreamEnded",
            "sessionEnded",
        ]
    );
    assert_eq!(
        receive_json(&mut websocket).await["params"]["termination"]["reason"],
        "sessionEnded"
    );

    server.begin_shutdown();
    server.finish_shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn cursor_is_epoch_and_session_bound_and_capability_is_single_use() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let start = StartSession::new(None, None, 1);
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start Session");
    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let opened = server
        .open(EventsStreamOpenParams {
            session_id: session_id.clone(),
            origin_policy: EventStreamOriginPolicy::Absent {},
        })
        .expect("open capability");
    let endpoint = opened.endpoint.expose_secret().to_owned();
    let mut websocket = connect(&server, &endpoint, None).await.expect("first use");
    hello(&mut websocket).await;
    let wrong_epoch = EventStreamCursor {
        stream_epoch: devicerail_protocol::EventStreamEpoch::new(),
        session_id: session_id.clone(),
        sequence: devicerail_protocol::EventSequence::FIRST,
    };
    send_json(
        &mut websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "subscribe",
            "method": "events.subscribe",
            "params": { "sessionId": session_id, "afterCursor": wrong_epoch }
        }),
    )
    .await;
    let response: RpcResponse =
        serde_json::from_value(receive_json(&mut websocket).await).expect("typed failure");
    assert_eq!(
        response.error().expect("cursor rejected").data.code,
        "stream_cursor_epoch_mismatch"
    );

    assert!(
        connect(&server, &endpoint, None).await.is_err(),
        "a consumed bearer capability cannot be replayed"
    );
    server.begin_shutdown();
    server.finish_shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn exact_origin_is_enforced_and_extension_offer_is_not_negotiated() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let session_id = devicerail_protocol::SessionId::from(
        uuid::Uuid::parse_str(SESSION).expect("fixture Session UUID"),
    );
    events
        .start_session(StartSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 1,
        })
        .await
        .expect("start Session");
    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let origin = "http://127.0.0.1:4173";
    let opened = server
        .open(EventsStreamOpenParams {
            session_id,
            origin_policy: EventStreamOriginPolicy::Exact {
                origin: origin.to_owned(),
            },
        })
        .expect("open browser-bound capability");
    let endpoint = opened.endpoint.expose_secret().to_owned();
    assert!(connect(&server, &endpoint, None).await.is_err());
    let websocket = connect(&server, &endpoint, Some(origin))
        .await
        .expect("matching Origin can consume the still-valid capability");
    drop(websocket);
    server.begin_shutdown();
    server.finish_shutdown().await.expect("clean shutdown");
}

#[tokio::test]
async fn daemon_shutdown_does_not_overtake_an_already_written_session_end() {
    let events = std::sync::Arc::new(MemoryEventStore::default());
    let start = StartSession::new(None, None, 1);
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start Session");
    let Some(mut server) = bind_or_skip(std::sync::Arc::clone(&events)).await else {
        return;
    };
    let opened = server
        .open(EventsStreamOpenParams {
            session_id: session_id.clone(),
            origin_policy: EventStreamOriginPolicy::Absent {},
        })
        .expect("open capability");
    let mut websocket = connect(&server, opened.endpoint.expose_secret(), None)
        .await
        .expect("upgrade");
    hello(&mut websocket).await;
    send_json(
        &mut websocket,
        json!({
            "jsonrpc": "2.0",
            "id": "subscribe",
            "method": "events.subscribe",
            "params": { "sessionId": session_id }
        }),
    )
    .await;
    let subscribed = receive_json(&mut websocket).await;
    assert_eq!(subscribed["result"]["replayThrough"]["sequence"], 1);
    assert_eq!(
        receive_json(&mut websocket).await["params"]["event"]["sequence"],
        1
    );

    // This mirrors daemon shutdown ordering: stop new stream admission,
    // finish the active Session, then drain the transport.
    server.begin_shutdown();
    events
        .end_session(EndSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: 2,
            outcome: SessionOutcome::Shutdown,
            reason: Some("daemon shutdown".to_owned()),
        })
        .await
        .expect("write final Session event");
    server
        .finish_shutdown()
        .await
        .expect("natural drain completes");

    let ended = receive_json(&mut websocket).await;
    assert_eq!(ended["method"], "events.stream.event");
    assert_eq!(ended["params"]["event"]["sequence"], 2);
    assert_eq!(ended["params"]["event"]["payload"]["type"], "sessionEnded");
    let terminal = receive_json(&mut websocket).await;
    assert_eq!(terminal["method"], "events.stream.terminal");
    assert_eq!(terminal["params"]["lastEmittedCursor"]["sequence"], 2);
    assert_eq!(terminal["params"]["termination"]["reason"], "sessionEnded");
    assert_eq!(server.stats().active_connections, 0);
    assert_eq!(server.stats().pending_capabilities, 0);
    assert_eq!(
        server.stats().available_connection_permits,
        Config::default().max_connections
    );
}
