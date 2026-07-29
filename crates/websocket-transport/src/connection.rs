use std::{
    collections::BTreeSet,
    io::{self, Cursor},
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    task::{Context, Poll},
};

use devicerail_core::{EventStreamError, EventStreamItem, EventStreamTerminal, EventSubscription};
use devicerail_protocol::{
    ErrorInfo, EventStreamCursor, EventStreamOriginPolicy, EventSubscriptionId,
    EventsStreamTerminalNotification, EventsStreamTerminalParams, EventsStreamTermination,
    EventsSubscribeParams, EventsSubscribeResult, FeatureSelection, HelloParams, HelloResult,
    JsonRpcVersion, PeerInfo, ProtocolOffer, ProtocolSelection, ProtocolVersion, RpcError,
    RpcParams, RpcRequest, RpcResponse, SessionId, TestEvent, TransportInfo, feature,
    negotiate_features, negotiate_protocol,
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf},
    net::TcpStream,
    sync::{oneshot, watch},
    task::JoinHandle,
    time,
};
use tokio_tungstenite::{
    WebSocketStream, accept_hdr_async_with_config,
    tungstenite::{
        Message,
        handshake::server::{ErrorResponse, Request, Response},
        http::{HeaderValue, StatusCode, header},
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};

use crate::{
    Capability, MAX_FRAME_BYTES, MAX_HEADER_BYTES, MAX_HEADER_COUNT, MAX_MESSAGE_BYTES, State,
    queue::{QueueError, event_queue},
    unix_time_ms,
};

const SUBPROTOCOL: &str = "devicerail.events.v1";
const INVALID_REQUEST: i32 = -32600;
const INVALID_PARAMS: i32 = -32602;
const METHOD_NOT_FOUND: i32 = -32601;
const PROTOCOL_INCOMPATIBLE: i32 = -32003;
const REQUIRED_FEATURE_UNSUPPORTED: i32 = -32004;
const SESSION_ERROR: i32 = -32006;
const STREAM_ERROR: i32 = -32013;

#[allow(
    clippy::result_large_err,
    reason = "tokio-tungstenite fixes the handshake callback error type as ErrorResponse by value"
)]
pub(crate) async fn handle_connection(
    stream: TcpStream,
    state: Arc<State>,
    shutdown: watch::Receiver<bool>,
) {
    let prefix =
        match time::timeout(state.config.handshake_timeout, read_header_prefix(stream)).await {
            Ok(Ok(prefix)) => prefix,
            _ => return,
        };
    let captured = Arc::new(StdMutex::new(None));
    let callback_state = Arc::clone(&state);
    let callback_captured = Arc::clone(&captured);
    let config = WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MAX_MESSAGE_BYTES + 1024)
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES))
        .accept_unmasked_frames(false);
    let websocket = time::timeout(
        state.config.handshake_timeout,
        accept_hdr_async_with_config(
            prefix,
            move |request: &Request, response: Response| {
                authorize_upgrade(request, response, &callback_state, &callback_captured)
            },
            Some(config),
        ),
    )
    .await;
    let Ok(Ok(mut websocket)) = websocket else {
        return;
    };
    let capability = captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(capability) = capability else {
        let _ = send_close(
            &mut websocket,
            CloseCode::Policy,
            "unauthorized",
            state.config.write_timeout,
        )
        .await;
        return;
    };
    run_protocol(&mut websocket, state, capability, shutdown).await;
}

struct PrefixStream {
    prefix: Cursor<Vec<u8>>,
    inner: TcpStream,
}

async fn read_header_prefix(mut inner: TcpStream) -> std::io::Result<PrefixStream> {
    let mut prefix = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = inner.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "WebSocket handshake ended before its headers",
            ));
        }
        prefix.extend_from_slice(&chunk[..read]);
        if prefix.len() > MAX_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "WebSocket handshake headers exceed the limit",
            ));
        }
        if prefix.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(PrefixStream {
                prefix: Cursor::new(prefix),
                inner,
            });
        }
    }
}

impl AsyncRead for PrefixStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let position = usize::try_from(self.prefix.position()).unwrap_or(usize::MAX);
        let bytes = self.prefix.get_ref();
        if position < bytes.len() && buffer.remaining() > 0 {
            let count = buffer.remaining().min(bytes.len() - position);
            buffer.put_slice(&bytes[position..position + count]);
            self.prefix.set_position((position + count) as u64);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for PrefixStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[allow(clippy::result_large_err)] // tungstenite's callback API fixes this exact error type.
fn authorize_upgrade(
    request: &Request,
    mut response: Response,
    state: &State,
    captured: &StdMutex<Option<Capability>>,
) -> Result<Response, ErrorResponse> {
    if request.headers().len() > MAX_HEADER_COUNT
        || request.uri().query().is_some()
        || request.headers().get_all(header::HOST).iter().count() != 1
        || request.headers().get_all(header::CONNECTION).iter().count() != 1
        || request.headers().get_all(header::UPGRADE).iter().count() != 1
        || request
            .headers()
            .get_all(header::SEC_WEBSOCKET_KEY)
            .iter()
            .count()
            != 1
        || request
            .headers()
            .get_all(header::SEC_WEBSOCKET_VERSION)
            .iter()
            .count()
            != 1
        || request
            .headers()
            .get_all(header::SEC_WEBSOCKET_EXTENSIONS)
            .iter()
            .count()
            > 1
        || request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            != Some(state.expected_host.as_str())
        || request
            .headers()
            .get_all(header::SEC_WEBSOCKET_PROTOCOL)
            .iter()
            .count()
            != 1
        || request
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok())
            != Some(SUBPROTOCOL)
    {
        return Err(forbidden());
    }
    let Some(token) = request.uri().path().strip_prefix("/v/") else {
        return Err(forbidden());
    };
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(forbidden());
    }
    let now = unix_time_ms().map_err(|_| forbidden())?;
    let mut admission = state
        .admission
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if admission.shutting_down {
        return Err(forbidden());
    }
    admission
        .capabilities
        .retain(|_, capability| capability.expires_at_ms > now);
    let Some(capability) = admission.capabilities.get(token).cloned() else {
        return Err(forbidden());
    };
    if !origin_matches(request, &capability.origin_policy) {
        return Err(forbidden());
    }
    let Some(capability) = admission.capabilities.remove(token) else {
        return Err(forbidden());
    };
    *captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(capability);
    response.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(SUBPROTOCOL),
    );
    // Extension offers are deliberately ignored. No compression extension is
    // enabled, so a later RSV1 frame is rejected by tungstenite.
    Ok(response)
}

fn origin_matches(request: &Request, policy: &EventStreamOriginPolicy) -> bool {
    let origins = request.headers().get_all(header::ORIGIN);
    match policy {
        EventStreamOriginPolicy::Absent {} => origins.iter().next().is_none(),
        EventStreamOriginPolicy::Exact { origin } => {
            let mut values = origins.iter();
            values.next().and_then(|value| value.to_str().ok()) == Some(origin.as_str())
                && values.next().is_none()
        }
    }
}

fn forbidden() -> ErrorResponse {
    tokio_tungstenite::tungstenite::http::Response::builder()
        .status(StatusCode::FORBIDDEN)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Some("forbidden".to_owned()))
        .expect("fixed rejection response is valid")
}

async fn run_protocol(
    websocket: &mut WebSocketStream<PrefixStream>,
    state: Arc<State>,
    capability: Capability,
    mut shutdown: watch::Receiver<bool>,
) {
    let hello_request =
        match receive_request(websocket, &mut shutdown, state.config.handshake_timeout).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(response) => {
                let _ = send_response(websocket, response, state.config.write_timeout).await;
                let _ = send_close(
                    websocket,
                    CloseCode::Protocol,
                    "invalid hello",
                    state.config.write_timeout,
                )
                .await;
                return;
            }
        };
    let hello_id = hello_request.id.clone();
    let hello = match negotiate_hello(hello_request) {
        Ok(hello) => hello,
        Err(error) => {
            let _ = send_response(
                websocket,
                RpcResponse::failure(Some(hello_id), error),
                state.config.write_timeout,
            )
            .await;
            let _ = send_close(
                websocket,
                CloseCode::Protocol,
                "hello rejected",
                state.config.write_timeout,
            )
            .await;
            return;
        }
    };
    let selected_protocol = hello.protocol.selected;
    let hello_value = match serde_json::to_value(hello) {
        Ok(value) => value,
        Err(_) => return,
    };
    if !send_response(
        websocket,
        RpcResponse::success(hello_id, hello_value),
        state.config.write_timeout,
    )
    .await
    {
        return;
    }

    let subscribe_request =
        match receive_request(websocket, &mut shutdown, state.config.handshake_timeout).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(response) => {
                let _ = send_response(websocket, response, state.config.write_timeout).await;
                let _ = send_close(
                    websocket,
                    CloseCode::Protocol,
                    "invalid subscribe",
                    state.config.write_timeout,
                )
                .await;
                return;
            }
        };
    let subscribe_id = subscribe_request.id.clone();
    let (subscription_id, subscription) =
        match subscribe(subscribe_request, &state, &capability.session_id).await {
            Ok(value) => value,
            Err(error) => {
                let _ = send_response(
                    websocket,
                    RpcResponse::failure(Some(subscribe_id), error),
                    state.config.write_timeout,
                )
                .await;
                let _ = send_close(
                    websocket,
                    CloseCode::Policy,
                    "subscribe rejected",
                    state.config.write_timeout,
                )
                .await;
                return;
            }
        };
    let result = EventsSubscribeResult {
        subscription_id,
        session_id: capability.session_id.clone(),
        replay_through: EventStreamCursor {
            stream_epoch: state.stream_epoch,
            session_id: capability.session_id.clone(),
            sequence: subscription.replay_through(),
        },
        session_state: subscription.session_state(),
    };
    let result = match serde_json::to_value(result) {
        Ok(value) => value,
        Err(_) => return,
    };
    if !send_response(
        websocket,
        RpcResponse::success(subscribe_id, result),
        state.config.write_timeout,
    )
    .await
    {
        return;
    }
    stream_events(
        websocket,
        state,
        capability.session_id,
        subscription_id,
        subscription,
        selected_protocol,
        shutdown,
    )
    .await;
}

async fn receive_request(
    websocket: &mut WebSocketStream<PrefixStream>,
    shutdown: &mut watch::Receiver<bool>,
    deadline: std::time::Duration,
) -> Result<Option<RpcRequest>, RpcResponse> {
    if *shutdown.borrow() {
        return Ok(None);
    }
    let received = time::timeout(deadline, async {
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(None);
                    }
                }
                message = websocket.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            return serde_json::from_str::<RpcRequest>(text.as_str())
                                .map(Some)
                                .map_err(|_| RpcResponse::failure(None, rpc_error(
                                    INVALID_REQUEST,
                                    "invalid_request",
                                    "WebSocket message is not a DeviceRail request",
                                    false,
                                    None,
                                )));
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if websocket.send(Message::Pong(payload)).await.is_err() {
                                return Ok(None);
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Ok(None),
                        Some(Ok(Message::Binary(_))) | Some(Ok(Message::Frame(_))) => {
                            return Err(RpcResponse::failure(None, rpc_error(
                                INVALID_REQUEST,
                                "invalid_request",
                                "WebSocket application messages must be text JSON",
                                false,
                                None,
                            )));
                        }
                    }
                }
            }
        }
    })
    .await;
    received.unwrap_or(Ok(None))
}

fn negotiate_hello(request: RpcRequest) -> Result<HelloResult, RpcError> {
    if request.method != "system.hello" || request.timeout_ms.is_some() {
        return Err(rpc_error(
            METHOD_NOT_FOUND,
            "handshake_required",
            "system.hello must be the first WebSocket message",
            false,
            Some(json!({ "requiredMethod": "system.hello" })),
        ));
    }
    let params = decode_params::<HelloParams>(request.params, "system.hello")?;
    let server_offer = ProtocolOffer::new(vec![devicerail_protocol::ProtocolRange::new(
        1,
        3,
        devicerail_protocol::PROTOCOL_VERSION.minor,
    )]);
    let selected = negotiate_protocol(&params.protocol, &server_offer).map_err(|_| {
        rpc_error(
            PROTOCOL_INCOMPATIBLE,
            "protocol_version_incompatible",
            "event WebSocket requires protocol 1.3 or newer",
            false,
            Some(json!({ "clientProtocol": params.protocol, "serverProtocol": server_offer })),
        )
    })?;
    let available = BTreeSet::from([feature::EVENTS_STREAM_V1.to_owned()]);
    let features = negotiate_features(&params.features, &available).map_err(|error| {
        rpc_error(
            REQUIRED_FEATURE_UNSUPPORTED,
            "required_feature_unsupported",
            "event WebSocket requires events.stream.v1",
            false,
            Some(json!({ "unsupportedRequired": error.unsupported_required })),
        )
    })?;
    if !features.enabled.contains(feature::EVENTS_STREAM_V1) {
        return Err(rpc_error(
            REQUIRED_FEATURE_UNSUPPORTED,
            "required_feature_unsupported",
            "event WebSocket requires events.stream.v1",
            false,
            Some(json!({ "requiredFeature": feature::EVENTS_STREAM_V1 })),
        ));
    }
    Ok(HelloResult {
        connection_id: uuid::Uuid::new_v4(),
        protocol: ProtocolSelection { selected },
        server: PeerInfo {
            name: "devicerail-daemon".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        transport: TransportInfo {
            kind: "webSocket".to_owned(),
            framing: "jsonMessage".to_owned(),
        },
        features: FeatureSelection {
            enabled: features.enabled,
        },
    })
}

async fn subscribe(
    request: RpcRequest,
    state: &State,
    granted_session: &SessionId,
) -> Result<(EventSubscriptionId, EventSubscription), RpcError> {
    if request.method != "events.subscribe" {
        return Err(rpc_error(
            METHOD_NOT_FOUND,
            "method_not_found",
            "the event WebSocket accepts exactly one events.subscribe request",
            false,
            Some(json!({ "method": request.method })),
        ));
    }
    if request.timeout_ms.is_some() {
        return Err(rpc_error(
            INVALID_PARAMS,
            "request_timeout_not_supported",
            "events.subscribe does not accept timeoutMs",
            false,
            None,
        ));
    }
    let params = decode_params::<EventsSubscribeParams>(request.params, "events.subscribe")?;
    if params.session_id != *granted_session {
        return Err(stream_rpc_error(
            "stream_cursor_session_mismatch",
            "stream capability and subscription identify different Sessions",
            false,
            None,
        ));
    }
    let after = match params.after_cursor {
        Some(cursor)
            if cursor.session_id == *granted_session
                && cursor.stream_epoch == state.stream_epoch =>
        {
            Some(cursor.sequence)
        }
        Some(cursor) if cursor.session_id != *granted_session => {
            return Err(stream_rpc_error(
                "stream_cursor_session_mismatch",
                "event cursor belongs to another Session",
                false,
                None,
            ));
        }
        Some(_) => {
            return Err(stream_rpc_error(
                "stream_cursor_epoch_mismatch",
                "event cursor belongs to another daemon lifetime",
                true,
                None,
            ));
        }
        None => None,
    };
    let subscription = state
        .events
        .subscribe_after(granted_session, after)
        .await
        .map_err(core_stream_error)?;
    Ok((EventSubscriptionId::new(), subscription))
}

fn decode_params<T>(params: Option<RpcParams>, method: &str) -> Result<T, RpcError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(params.map(RpcParams::into_value).unwrap_or(Value::Null)).map_err(|_| {
        rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            format!("{method} params are invalid"),
            false,
            Some(json!({ "method": method })),
        )
    })
}

async fn stream_events(
    websocket: &mut WebSocketStream<PrefixStream>,
    state: Arc<State>,
    session_id: SessionId,
    subscription_id: EventSubscriptionId,
    subscription: EventSubscription,
    selected_protocol: ProtocolVersion,
    mut shutdown: watch::Receiver<bool>,
) {
    let (sender, mut receiver) = event_queue(
        state.config.max_queued_events,
        state.config.max_queued_bytes,
        state.config.queue_stall_timeout,
    );
    let (terminal_sender, mut terminal_receiver) = oneshot::channel();
    let feeder = AbortOnDropTask::new(spawn_feeder(
        subscription,
        sender,
        terminal_sender,
        state.stream_epoch,
        session_id.clone(),
        subscription_id,
        selected_protocol,
    ));
    let mut last_emitted = None;
    let mut terminal = None;
    loop {
        if terminal.is_some() {
            match receiver.recv().await {
                Some(event) => {
                    if !send_text_bytes(websocket, event.bytes, state.config.write_timeout).await {
                        break;
                    }
                    last_emitted = Some(event.cursor);
                }
                None => {
                    send_terminal_and_close(
                        websocket,
                        subscription_id,
                        session_id.clone(),
                        last_emitted,
                        terminal.take().expect("terminal is present"),
                        state.config.write_timeout,
                    )
                    .await;
                    break;
                }
            }
            continue;
        }
        tokio::select! {
            biased;
            result = &mut terminal_receiver => {
                terminal = Some(result.unwrap_or_else(|_| error_termination(
                    "internalError",
                    "stream_internal_error",
                    "event stream feeder stopped unexpectedly",
                    false,
                    None,
                )));
            }
            event = receiver.recv() => {
                match event {
                    Some(event) => {
                        if !send_text_bytes(websocket, event.bytes, state.config.write_timeout).await {
                            break;
                        }
                        last_emitted = Some(event.cursor);
                    }
                    None => {
                        terminal = Some(error_termination(
                            "internalError",
                            "stream_internal_error",
                            "event stream queue closed without a terminal",
                            false,
                            None,
                        ));
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    receiver.close();
                    terminal = Some(error_termination(
                        "serverShutdown",
                        "stream_server_shutdown",
                        "event stream server is shutting down",
                        true,
                        None,
                    ));
                }
            }
            message = websocket.next() => {
                match message {
                    Some(Ok(Message::Ping(payload))) => {
                        if !matches!(
                            time::timeout(
                                state.config.write_timeout,
                                websocket.send(Message::Pong(payload)),
                            ).await,
                            Ok(Ok(()))
                        ) {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_)))
                    | Some(Ok(Message::Frame(_))) => {
                        let _ = send_close(
                            websocket,
                            CloseCode::Protocol,
                            "unexpected message",
                            state.config.write_timeout,
                        )
                        .await;
                        break;
                    }
                }
            }
        }
    }
    feeder.abort_and_wait().await;
}

struct AbortOnDropTask {
    handle: Option<JoinHandle<()>>,
}

impl AbortOnDropTask {
    fn new(handle: JoinHandle<()>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn abort_and_wait(mut self) {
        if let Some(handle) = self.handle.as_mut() {
            handle.abort();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for AbortOnDropTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedEventParams<'a> {
    subscription_id: EventSubscriptionId,
    cursor: &'a EventStreamCursor,
    event: &'a TestEvent,
}

#[derive(Serialize)]
struct BorrowedEventNotification<'a> {
    jsonrpc: JsonRpcVersion,
    method: &'static str,
    params: BorrowedEventParams<'a>,
}

#[derive(Debug, PartialEq, Eq)]
enum EventSerializationError {
    TooLarge { observed_bytes_at_least: usize },
    Failed,
}

struct CappedWriter {
    bytes: Vec<u8>,
    limit: usize,
    overflow_at: Option<usize>,
}

impl CappedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(8 * 1024)),
            limit,
            overflow_at: None,
        }
    }

    fn finish(self) -> Result<Vec<u8>, EventSerializationError> {
        match self.overflow_at {
            Some(observed_bytes_at_least) => Err(EventSerializationError::TooLarge {
                observed_bytes_at_least,
            }),
            None => Ok(self.bytes),
        }
    }
}

impl io::Write for CappedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next_len = self.bytes.len().saturating_add(buffer.len());
        if next_len > self.limit {
            self.overflow_at = Some(next_len);
            return Err(io::Error::other("serialized event exceeds message limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_event_notification(
    subscription_id: EventSubscriptionId,
    cursor: &EventStreamCursor,
    event: &TestEvent,
    limit: usize,
) -> Result<Vec<u8>, EventSerializationError> {
    let notification = BorrowedEventNotification {
        jsonrpc: JsonRpcVersion::V2,
        method: "events.stream.event",
        params: BorrowedEventParams {
            subscription_id,
            cursor,
            event,
        },
    };
    let mut writer = CappedWriter::new(limit);
    if serde_json::to_writer(&mut writer, &notification).is_err() {
        return match writer.finish() {
            Err(error) => Err(error),
            Ok(_) => Err(EventSerializationError::Failed),
        };
    }
    writer.finish()
}

fn spawn_feeder(
    mut subscription: EventSubscription,
    sender: crate::queue::EventQueueSender,
    terminal: oneshot::Sender<EventsStreamTermination>,
    stream_epoch: devicerail_protocol::EventStreamEpoch,
    session_id: SessionId,
    subscription_id: EventSubscriptionId,
    selected_protocol: ProtocolVersion,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let termination = loop {
            match subscription.next().await {
                Some(EventStreamItem::Event(event)) => {
                    let required_minor = event.required_protocol_minor();
                    if selected_protocol.major != 1 || selected_protocol.minor < required_minor {
                        break error_termination(
                            "internalError",
                            "stream_event_protocol_incompatible",
                            "an event requires a newer negotiated protocol version",
                            false,
                            Some(json!({
                                "selectedProtocol": selected_protocol,
                                "requiredProtocol": { "major": 1, "minor": required_minor },
                                "sequence": event.sequence,
                            })),
                        );
                    }
                    let cursor = EventStreamCursor {
                        stream_epoch,
                        session_id: session_id.clone(),
                        sequence: event.sequence,
                    };
                    let bytes = match serialize_event_notification(
                        subscription_id,
                        &cursor,
                        event.as_ref(),
                        MAX_MESSAGE_BYTES,
                    ) {
                        Ok(bytes) => bytes,
                        Err(EventSerializationError::TooLarge {
                            observed_bytes_at_least,
                        }) => {
                            break error_termination(
                                "eventTooLarge",
                                "stream_event_too_large",
                                "one event exceeds the WebSocket message limit",
                                false,
                                Some(json!({
                                    "observedBytesAtLeast": observed_bytes_at_least,
                                    "limitBytes": MAX_MESSAGE_BYTES,
                                })),
                            );
                        }
                        Err(EventSerializationError::Failed) => {
                            break error_termination(
                                "internalError",
                                "stream_internal_error",
                                "event serialization failed",
                                false,
                                None,
                            );
                        }
                    };
                    match sender.send(bytes, cursor).await {
                        Ok(()) => {}
                        Err(QueueError::SlowConsumer) => {
                            break error_termination(
                                "slowConsumer",
                                "stream_slow_consumer",
                                "subscriber exceeded its bounded queue",
                                true,
                                None,
                            );
                        }
                        Err(QueueError::Closed) => return,
                    }
                }
                Some(EventStreamItem::Terminal(terminal)) => break map_core_terminal(terminal),
                None => {
                    break error_termination(
                        "internalError",
                        "stream_internal_error",
                        "Core event subscription ended without a terminal",
                        false,
                        None,
                    );
                }
            }
        };
        // Publish the authoritative terminal before closing the event queue.
        // Otherwise the socket actor can observe `recv() == None` in the tiny
        // gap between sender drop and oneshot delivery and misreport an
        // ordinary Session end as an internal error.
        let _ = terminal.send(termination);
        drop(sender);
    })
}

fn map_core_terminal(terminal: EventStreamTerminal) -> EventsStreamTermination {
    match terminal {
        EventStreamTerminal::SessionEnded { .. } => EventsStreamTermination::SessionEnded,
        EventStreamTerminal::SessionDeleted { last_sequence } => error_termination(
            "sessionDeleted",
            "stream_session_deleted",
            "the streamed Session was deleted",
            false,
            Some(json!({ "lastSequence": last_sequence })),
        ),
        EventStreamTerminal::TailLagged {
            last_delivered,
            missed_events,
        } => error_termination(
            "slowConsumer",
            "stream_slow_consumer",
            "subscriber fell behind the bounded Core tail",
            true,
            Some(json!({ "lastDelivered": last_delivered, "missedEvents": missed_events })),
        ),
        EventStreamTerminal::StoreShutdown => error_termination(
            "serverShutdown",
            "stream_server_shutdown",
            "event stream server is shutting down",
            true,
            None,
        ),
        EventStreamTerminal::SessionMismatch { .. } | EventStreamTerminal::SequenceGap { .. } => {
            error_termination(
                "sequenceGap",
                "stream_sequence_gap",
                "Core event stream lost its continuous sequence invariant",
                false,
                None,
            )
        }
    }
}

fn error_termination(
    reason: &str,
    code: &str,
    message: &str,
    retryable: bool,
    details: Option<Value>,
) -> EventsStreamTermination {
    let error = ErrorInfo {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable,
        details,
    };
    match reason {
        "slowConsumer" => EventsStreamTermination::SlowConsumer { error },
        "sessionDeleted" => EventsStreamTermination::SessionDeleted { error },
        "serverShutdown" => EventsStreamTermination::ServerShutdown { error },
        "sequenceGap" => EventsStreamTermination::SequenceGap { error },
        "eventTooLarge" => EventsStreamTermination::EventTooLarge { error },
        _ => EventsStreamTermination::InternalError { error },
    }
}

async fn send_terminal_and_close(
    websocket: &mut WebSocketStream<PrefixStream>,
    subscription_id: EventSubscriptionId,
    session_id: SessionId,
    last_emitted_cursor: Option<EventStreamCursor>,
    termination: EventsStreamTermination,
    write_timeout: std::time::Duration,
) {
    let notification = EventsStreamTerminalNotification::new(EventsStreamTerminalParams {
        subscription_id,
        session_id,
        last_emitted_cursor,
        termination,
    });
    if let Ok(text) = serde_json::to_string(&notification) {
        let _ = time::timeout(write_timeout, websocket.send(Message::text(text))).await;
    }
    let _ = send_close(
        websocket,
        CloseCode::Normal,
        "stream terminal",
        write_timeout,
    )
    .await;
}

async fn send_response(
    websocket: &mut WebSocketStream<PrefixStream>,
    response: RpcResponse,
    write_timeout: std::time::Duration,
) -> bool {
    let Ok(text) = serde_json::to_string(&response) else {
        return false;
    };
    matches!(
        time::timeout(write_timeout, websocket.send(Message::text(text))).await,
        Ok(Ok(()))
    )
}

async fn send_text_bytes(
    websocket: &mut WebSocketStream<PrefixStream>,
    bytes: Vec<u8>,
    write_timeout: std::time::Duration,
) -> bool {
    let Ok(text) = String::from_utf8(bytes) else {
        return false;
    };
    matches!(
        time::timeout(write_timeout, websocket.send(Message::text(text))).await,
        Ok(Ok(()))
    )
}

async fn send_close(
    websocket: &mut WebSocketStream<PrefixStream>,
    code: CloseCode,
    reason: &'static str,
    write_timeout: std::time::Duration,
) -> Result<(), ()> {
    match time::timeout(
        write_timeout,
        websocket.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(()),
    }
}

fn core_stream_error(error: EventStreamError) -> RpcError {
    match error {
        EventStreamError::SessionNotFound(session_id) => rpc_error(
            SESSION_ERROR,
            "session_not_found",
            "event stream Session was not found",
            false,
            Some(json!({ "sessionId": session_id })),
        ),
        EventStreamError::SessionDeleted(session_id) => stream_rpc_error(
            "stream_session_deleted",
            "event stream Session was already deleted",
            false,
            Some(json!({ "sessionId": session_id })),
        ),
        EventStreamError::CursorAhead {
            after,
            last_sequence,
            ..
        } => stream_rpc_error(
            "stream_cursor_ahead",
            "event cursor is ahead of the Session head",
            false,
            Some(json!({ "afterSequence": after, "lastSequence": last_sequence })),
        ),
        EventStreamError::ReplayLimitExceeded {
            requested, maximum, ..
        } => stream_rpc_error(
            "stream_cursor_too_old",
            "event replay suffix exceeds the bounded replay limit",
            true,
            Some(json!({ "requestedEvents": requested, "maximumEvents": maximum })),
        ),
        EventStreamError::TooManySubscribers { maximum, .. } => stream_rpc_error(
            "stream_subscription_limit",
            "Session reached its event subscriber limit",
            true,
            Some(json!({ "maximumSubscribers": maximum })),
        ),
        EventStreamError::StoreShutdown => stream_rpc_error(
            "stream_server_shutdown",
            "event stream server is shutting down",
            true,
            None,
        ),
        EventStreamError::InvalidConfig { .. } | EventStreamError::Internal(_) => stream_rpc_error(
            "stream_internal_error",
            "event stream Core failed",
            false,
            None,
        ),
    }
}

fn stream_rpc_error(
    code: &str,
    message: &str,
    retryable: bool,
    details: Option<Value>,
) -> RpcError {
    rpc_error(STREAM_ERROR, code, message, retryable, details)
}

fn rpc_error(
    numeric_code: i32,
    code: &str,
    message: impl Into<String>,
    retryable: bool,
    details: Option<Value>,
) -> RpcError {
    let message = message.into();
    RpcError {
        code: numeric_code,
        message: message.clone(),
        data: ErrorInfo {
            code: code.to_owned(),
            message,
            retryable,
            details,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::Write as _,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use devicerail_core::MemoryEventStore;
    use devicerail_protocol::{
        DeviceId, ErrorInfo, EventId, EventSequence, EventStreamCursor, EventStreamEpoch,
        EventStreamOriginPolicy, EventSubscriptionId, Observation, SessionId, TestEvent,
        TestEventPayload, UiSnapshotOmissionReason, Viewport,
    };
    use tokio::sync::Semaphore;
    use tokio_tungstenite::tungstenite::{
        handshake::server::{Request, Response},
        http::{HeaderValue, StatusCode, header},
    };

    use super::{
        AbortOnDropTask, CappedWriter, EventSerializationError, MAX_MESSAGE_BYTES, SUBPROTOCOL,
        authorize_upgrade, serialize_event_notification,
    };
    use crate::{AdmissionState, Capability, Config, State, validate_origin_policy};

    const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn state(policy: EventStreamOriginPolicy) -> State {
        let config = Config::default();
        State {
            events: Arc::new(MemoryEventStore::default()),
            stream_epoch: EventStreamEpoch::new(),
            expected_host: "127.0.0.1:43123".to_owned(),
            admission: Mutex::new(AdmissionState {
                shutting_down: false,
                capabilities: HashMap::from([(
                    TOKEN.to_owned(),
                    Capability {
                        session_id: SessionId::new(),
                        origin_policy: policy,
                        expires_at_ms: crate::unix_time_ms().expect("clock") + 10_000,
                    },
                )]),
            }),
            config,
            active_connections: AtomicUsize::new(0),
            connections: Arc::new(Semaphore::new(config.max_connections)),
        }
    }

    fn request(origin: Option<&str>) -> Request {
        let mut request = Request::builder()
            .uri(format!("/v/{TOKEN}"))
            .header(header::HOST, "127.0.0.1:43123")
            .header(header::CONNECTION, "Upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_PROTOCOL, SUBPROTOCOL)
            .header(
                header::SEC_WEBSOCKET_EXTENSIONS,
                "permessage-deflate; client_max_window_bits",
            )
            .body(())
            .expect("fixed request");
        if let Some(origin) = origin {
            request.headers_mut().insert(
                header::ORIGIN,
                HeaderValue::from_str(origin).expect("test Origin"),
            );
        }
        request
    }

    fn response() -> Response {
        Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .body(())
            .expect("fixed response")
    }

    #[test]
    fn nested_ui_fields_raise_the_event_protocol_floor_to_15() {
        let session_id = SessionId::new();
        let base = |payload| TestEvent {
            event_id: EventId::new(),
            session_id: session_id.clone(),
            sequence: EventSequence::FIRST,
            request_id: None,
            device_id: None,
            at_ms: 1,
            payload,
        };
        assert_eq!(
            base(TestEventPayload::SessionStarted).required_protocol_minor(),
            0
        );
        assert_eq!(
            base(TestEventPayload::ObservationCaptured {
                observation: Box::new(Observation {
                    id: uuid::Uuid::new_v4(),
                    device_id: DeviceId::new("ios-1"),
                    captured_at_ms: 1,
                    viewport: Viewport {
                        width: 390,
                        height: 844,
                        scale_factor: 3.0,
                    },
                    screenshot: None,
                    screenshot_omission: None,
                    ui_snapshot: None,
                    ui_snapshot_omission: Some(UiSnapshotOmissionReason::DriverUnsupported),
                    metadata: serde_json::Map::new(),
                }),
            })
            .required_protocol_minor(),
            5
        );
    }

    #[test]
    fn upgrade_consumes_one_capability_and_never_negotiates_compression() {
        let state = state(EventStreamOriginPolicy::Absent {});
        let captured = Mutex::new(None);
        let upgraded = authorize_upgrade(&request(None), response(), &state, &captured)
            .expect("valid upgrade");
        assert_eq!(
            upgraded
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok()),
            Some(SUBPROTOCOL)
        );
        assert!(
            upgraded
                .headers()
                .get(header::SEC_WEBSOCKET_EXTENSIONS)
                .is_none()
        );
        assert!(captured.lock().expect("captured lock").is_some());
        assert!(
            authorize_upgrade(&request(None), response(), &state, &Mutex::new(None)).is_err(),
            "the same token cannot be consumed twice"
        );
    }

    #[test]
    fn origin_mismatch_does_not_burn_the_capability() {
        let origin = "http://127.0.0.1:4173";
        let state = state(EventStreamOriginPolicy::Exact {
            origin: origin.to_owned(),
        });
        assert!(authorize_upgrade(&request(None), response(), &state, &Mutex::new(None)).is_err());
        let captured = Mutex::new(None);
        authorize_upgrade(&request(Some(origin)), response(), &state, &captured)
            .expect("matching exact Origin");
        assert!(captured.lock().expect("captured lock").is_some());
    }

    #[test]
    fn host_path_and_subprotocol_are_exact() {
        for mutate in ["host", "path", "protocol"] {
            let state = state(EventStreamOriginPolicy::Absent {});
            let mut request = request(None);
            match mutate {
                "host" => {
                    request
                        .headers_mut()
                        .insert(header::HOST, HeaderValue::from_static("localhost:43123"));
                }
                "path" => *request.uri_mut() = format!("/v/{TOKEN}/").parse().expect("URI"),
                "protocol" => {
                    request.headers_mut().insert(
                        header::SEC_WEBSOCKET_PROTOCOL,
                        HeaderValue::from_static("other"),
                    );
                }
                _ => unreachable!(),
            }
            assert!(
                authorize_upgrade(&request, response(), &state, &Mutex::new(None)).is_err(),
                "{mutate} mismatch must be rejected"
            );
        }
    }

    #[test]
    fn exact_browser_origin_rejects_default_ports_that_browsers_normalize_away() {
        for origin in ["http://127.0.0.1:80", "https://127.0.0.1:443"] {
            assert!(
                validate_origin_policy(&EventStreamOriginPolicy::Exact {
                    origin: origin.to_owned(),
                })
                .is_err(),
                "{origin} cannot survive browser Origin normalization"
            );
        }
        validate_origin_policy(&EventStreamOriginPolicy::Exact {
            origin: "http://127.0.0.1:4173".to_owned(),
        })
        .expect("non-default explicit loopback port");
    }

    fn event_with_message(message: String) -> TestEvent {
        TestEvent {
            event_id: EventId::new(),
            session_id: SessionId::new(),
            sequence: EventSequence::FIRST,
            request_id: None,
            device_id: None,
            at_ms: 1,
            payload: TestEventPayload::Error {
                error: ErrorInfo {
                    code: "test".to_owned(),
                    message,
                    retryable: false,
                    details: None,
                },
            },
        }
    }

    #[test]
    fn borrowed_event_serialization_stops_at_the_message_cap() {
        let mut capped = CappedWriter::new(4);
        capped.write_all(b"1234").expect("exact cap fits");
        assert!(capped.write_all(b"5").is_err());
        assert_eq!(capped.bytes.len(), 4, "overflow never grows the buffer");

        let event = event_with_message("x".repeat(MAX_MESSAGE_BYTES));
        let cursor = EventStreamCursor {
            stream_epoch: EventStreamEpoch::new(),
            session_id: event.session_id.clone(),
            sequence: event.sequence,
        };
        assert!(matches!(
            serialize_event_notification(
                EventSubscriptionId::new(),
                &cursor,
                &event,
                MAX_MESSAGE_BYTES,
            ),
            Err(EventSerializationError::TooLarge {
                observed_bytes_at_least
            }) if observed_bytes_at_least > MAX_MESSAGE_BYTES
        ));

        let small = event_with_message("small".to_owned());
        let small_cursor = EventStreamCursor {
            stream_epoch: cursor.stream_epoch,
            session_id: small.session_id.clone(),
            sequence: small.sequence,
        };
        let bytes = serialize_event_notification(
            EventSubscriptionId::new(),
            &small_cursor,
            &small,
            MAX_MESSAGE_BYTES,
        )
        .expect("small borrowed event serializes");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("valid JSON");
        assert_eq!(value["method"], "events.stream.event");
        assert_eq!(
            value["params"]["event"]["payload"]["error"]["message"],
            "small"
        );
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn dropping_socket_actor_aborts_its_feeder_task() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (started, started_receiver) = tokio::sync::oneshot::channel();
        let task_dropped = Arc::clone(&dropped);
        let feeder = AbortOnDropTask::new(tokio::spawn(async move {
            let _drop_flag = DropFlag(task_dropped);
            let _ = started.send(());
            std::future::pending::<()>().await;
        }));
        started_receiver.await.expect("feeder started");
        drop(feeder);
        for _ in 0..10 {
            if dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            dropped.load(Ordering::SeqCst),
            "dropping the connection-owned guard must cancel the detached feeder"
        );
    }
}
