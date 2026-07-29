// The event-stream implementation is intentionally kept separate from the
// control-plane client so WebSocket concerns cannot leak into protocol/Core.

use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use devicerail_protocol::{
    EventStreamCursor, EventStreamEpoch, EventStreamOriginPolicy, EventSubscriptionId,
    EventsStreamEventParams, EventsStreamOpenParams, EventsStreamTerminalParams,
    EventsStreamTermination, EventsSubscribeParams, EventsSubscribeResult, FeatureOffer,
    HelloParams, HelloResult, JsonRpcVersion, ProtocolOffer, ProtocolVersion, RpcId, RpcParams,
    RpcRequest, RpcResponse, RpcServerMessage, RpcServerNotification, SessionId, SessionState,
    TestEvent, TestEventPayload, feature,
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Error as WebSocketError, Message,
        client::IntoClientRequest as _,
        http::{HeaderValue, header},
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};
use url::{Host, Url};

use crate::{
    CallOptions, ClientError, DeviceRailClient, EventStreamError,
    client::{parse_strict_json_value, validate_json_value_safety},
    methods,
};

const EVENT_STREAM_SUBPROTOCOL: &str = "devicerail.events.v1";
const HELLO_REQUEST_ID: &str = "devicerail:event-stream:hello";
const SUBSCRIBE_REQUEST_ID: &str = "devicerail:event-stream:subscribe";

pub const DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const DEFAULT_EVENT_STREAM_MAX_QUEUED_EVENTS: usize = 64;
pub const DEFAULT_EVENT_STREAM_MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_EVENT_STREAM_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_EVENT_STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_EVENT_STREAM_CLOSE_GRACE: Duration = Duration::from_secs(3);

const MAX_EVENT_STREAM_SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_EVENT_STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_EVENT_STREAM_CLOSE_GRACE: Duration = Duration::from_secs(30);

/// Hard client-side limits for one event stream.
///
/// Callers may lower these limits to enforce a stricter local budget. They
/// cannot raise them above the public transport contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventStreamOptions {
    pub max_message_bytes: usize,
    pub max_queued_events: usize,
    pub max_queued_bytes: usize,
    /// One deadline for capability issuance, TCP connect, WebSocket upgrade,
    /// WebSocket hello, and subscription.
    pub setup_timeout: Duration,
    /// Per-write deadline for setup messages, Ping replies, and Close frames.
    pub write_timeout: Duration,
    /// Maximum time explicit cancellation or resume waits for the socket actor
    /// before aborting it.
    pub close_grace: Duration,
    /// Native Rust clients normally use [`EventStreamOriginPolicy::Absent`].
    /// `Exact` is provided for a caller intentionally sharing browser-bound
    /// capability issuance semantics.
    pub origin_policy: EventStreamOriginPolicy,
}

impl Default for EventStreamOptions {
    fn default() -> Self {
        Self {
            max_message_bytes: DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES,
            max_queued_events: DEFAULT_EVENT_STREAM_MAX_QUEUED_EVENTS,
            max_queued_bytes: DEFAULT_EVENT_STREAM_MAX_QUEUED_BYTES,
            setup_timeout: DEFAULT_EVENT_STREAM_SETUP_TIMEOUT,
            write_timeout: DEFAULT_EVENT_STREAM_WRITE_TIMEOUT,
            close_grace: DEFAULT_EVENT_STREAM_CLOSE_GRACE,
            origin_policy: EventStreamOriginPolicy::Absent {},
        }
    }
}

impl EventStreamOptions {
    fn validate(&self) -> Result<(), EventStreamError> {
        if self.max_message_bytes == 0
            || self.max_message_bytes > DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES
        {
            return Err(EventStreamError::ProtocolViolation(format!(
                "max_message_bytes must be between 1 and \
                 {DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES}"
            )));
        }
        if self.max_queued_events == 0
            || self.max_queued_events > DEFAULT_EVENT_STREAM_MAX_QUEUED_EVENTS
        {
            return Err(EventStreamError::ProtocolViolation(format!(
                "max_queued_events must be between 1 and \
                 {DEFAULT_EVENT_STREAM_MAX_QUEUED_EVENTS}"
            )));
        }
        if self.max_queued_bytes == 0
            || self.max_queued_bytes > DEFAULT_EVENT_STREAM_MAX_QUEUED_BYTES
        {
            return Err(EventStreamError::ProtocolViolation(format!(
                "max_queued_bytes must be between 1 and \
                {DEFAULT_EVENT_STREAM_MAX_QUEUED_BYTES}"
            )));
        }
        validate_duration(
            self.setup_timeout,
            MAX_EVENT_STREAM_SETUP_TIMEOUT,
            "setup_timeout",
        )?;
        validate_duration(
            self.write_timeout,
            MAX_EVENT_STREAM_WRITE_TIMEOUT,
            "write_timeout",
        )?;
        validate_duration(
            self.close_grace,
            MAX_EVENT_STREAM_CLOSE_GRACE,
            "close_grace",
        )?;
        validate_origin_policy(&self.origin_policy)
    }
}

/// One event delivered to the application. Receipt and delivery do not advance
/// the resumable cursor; call [`DeviceRailEventStream::confirm`] only after the
/// application has durably accepted this event.
#[derive(Clone, Debug, PartialEq)]
pub struct EventStreamItem {
    pub cursor: EventStreamCursor,
    pub event: TestEvent,
}

/// The authoritative typed terminal notification received from the daemon.
pub type EventStreamTerminal = EventsStreamTerminalParams;

struct QueuedEvent {
    bytes: usize,
    item: EventStreamItem,
}

#[derive(Clone, Debug)]
enum Completion {
    Active,
    Remote,
    Failed(EventStreamError),
    Cancelled,
}

#[derive(Debug)]
struct StreamState {
    completion: Completion,
    received_cursor: Option<EventStreamCursor>,
    last_received_was_session_end: bool,
    delivered_cursor: Option<EventStreamCursor>,
    confirmed_cursor: Option<EventStreamCursor>,
    terminal: Option<EventStreamTerminal>,
    queued_bytes: usize,
}

impl StreamState {
    fn new(after_cursor: Option<EventStreamCursor>) -> Self {
        Self {
            completion: Completion::Active,
            received_cursor: None,
            last_received_was_session_end: false,
            delivered_cursor: after_cursor.clone(),
            confirmed_cursor: after_cursor,
            terminal: None,
            queued_bytes: 0,
        }
    }
}

struct OpenedStream {
    websocket: WebSocketStream<TcpStream>,
    stream_epoch: EventStreamEpoch,
    subscription: EventsSubscribeResult,
}

/// A single Session-scoped, explicitly confirmed, resumable event stream.
///
/// The three cursor getters deliberately represent different boundaries:
///
/// - `received_cursor`: validated from the WebSocket;
/// - `delivered_cursor`: returned by [`Self::next`];
/// - `confirmed_cursor`: explicitly acknowledged by [`Self::confirm`].
pub struct DeviceRailEventStream {
    client: DeviceRailClient,
    session_id: SessionId,
    stream_epoch: EventStreamEpoch,
    selected_protocol: ProtocolVersion,
    state: Arc<Mutex<StreamState>>,
    receiver: mpsc::Receiver<QueuedEvent>,
    cancel: watch::Sender<bool>,
    actor: Option<JoinHandle<()>>,
    close_grace: Duration,
}

impl std::fmt::Debug for DeviceRailEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        formatter
            .debug_struct("DeviceRailEventStream")
            .field("session_id", &self.session_id)
            .field("stream_epoch", &self.stream_epoch)
            .field("selected_protocol", &self.selected_protocol)
            .field("received_cursor", &state.received_cursor)
            .field("delivered_cursor", &state.delivered_cursor)
            .field("confirmed_cursor", &state.confirmed_cursor)
            .field("completion", &state.completion)
            .finish_non_exhaustive()
    }
}

impl DeviceRailClient {
    /// Opens the short-lived control-plane capability, performs the WebSocket
    /// hello at the exact negotiated control protocol, and subscribes before
    /// returning.
    pub async fn open_event_stream(
        &self,
        params: EventsSubscribeParams,
        options: EventStreamOptions,
    ) -> Result<DeviceRailEventStream, EventStreamError> {
        options.validate()?;
        validate_after_cursor(&params)?;

        let negotiated = self.negotiated()?;
        let selected_protocol = negotiated.result.protocol.selected;
        if selected_protocol.major != 1 || selected_protocol.minor < 3 {
            return Err(EventStreamError::ProtocolViolation(
                "event streams require a negotiated protocol of at least 1.3".to_owned(),
            ));
        }

        let opened = time::timeout(
            options.setup_timeout,
            open_socket(
                self,
                &negotiated.client,
                selected_protocol,
                &params,
                &options,
            ),
        )
        .await
        .map_err(|_| EventStreamError::Timeout { phase: "setup" })??;
        let state = Arc::new(Mutex::new(StreamState::new(params.after_cursor.clone())));
        let (sender, receiver) = mpsc::channel(options.max_queued_events);
        let (cancel, cancel_receiver) = watch::channel(false);
        let actor = tokio::spawn(stream_actor(
            opened.websocket,
            params.session_id.clone(),
            opened.stream_epoch,
            opened.subscription,
            params.after_cursor,
            selected_protocol,
            options.clone(),
            Arc::clone(&state),
            sender,
            cancel_receiver,
        ));

        Ok(DeviceRailEventStream {
            client: self.clone(),
            session_id: params.session_id,
            stream_epoch: opened.stream_epoch,
            selected_protocol,
            state,
            receiver,
            cancel,
            actor: Some(actor),
            close_grace: options.close_grace,
        })
    }
}

impl DeviceRailEventStream {
    fn state(&self) -> MutexGuard<'_, StreamState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn received_cursor(&self) -> Option<EventStreamCursor> {
        self.state().received_cursor.clone()
    }

    pub fn delivered_cursor(&self) -> Option<EventStreamCursor> {
        self.state().delivered_cursor.clone()
    }

    pub fn confirmed_cursor(&self) -> Option<EventStreamCursor> {
        self.state().confirmed_cursor.clone()
    }

    pub fn terminal(&self) -> Option<EventStreamTerminal> {
        self.state().terminal.clone()
    }

    /// Delivers the next queued event. A clean Session end returns `None` only
    /// after all preceding events have been delivered. Every other ending is
    /// an explicit error.
    pub async fn next(&mut self) -> Result<Option<EventStreamItem>, EventStreamError> {
        if let Some(queued) = self.receiver.recv().await {
            let mut state = self.state();
            state.queued_bytes = state.queued_bytes.saturating_sub(queued.bytes);
            let expected = next_sequence(
                state
                    .delivered_cursor
                    .as_ref()
                    .map(|cursor| cursor.sequence),
            )?;
            if queued.item.cursor.sequence != expected {
                let error = EventStreamError::Cursor(format!(
                    "delivered sequence {} does not follow {}",
                    queued.item.cursor.sequence.get(),
                    expected.get().saturating_sub(1)
                ));
                state.completion = Completion::Failed(error.clone());
                return Err(error);
            }
            state.delivered_cursor = Some(queued.item.cursor.clone());
            return Ok(Some(queued.item));
        }

        let completion = { self.state().completion.clone() };
        match completion {
            Completion::Active => {
                let error = EventStreamError::ProtocolViolation(
                    "event stream queue closed without an explicit terminal".to_owned(),
                );
                self.state().completion = Completion::Failed(error.clone());
                Err(error)
            }
            Completion::Remote => {
                let terminal = self.state().terminal.clone().ok_or_else(|| {
                    EventStreamError::ProtocolViolation(
                        "remote event stream completion omitted its terminal".to_owned(),
                    )
                })?;
                match terminal.termination {
                    EventsStreamTermination::SessionEnded => Ok(None),
                    termination => Err(EventStreamError::RemoteTermination(termination)),
                }
            }
            Completion::Failed(error) => Err(error),
            Completion::Cancelled => Err(EventStreamError::Cancelled),
        }
    }

    /// Advances the resumable cursor by exactly one already-delivered event.
    pub fn confirm(
        &self,
        cursor: &EventStreamCursor,
    ) -> Result<EventStreamCursor, EventStreamError> {
        if cursor.session_id != self.session_id || cursor.stream_epoch != self.stream_epoch {
            return Err(EventStreamError::Cursor(
                "confirmed cursor belongs to another event stream".to_owned(),
            ));
        }
        let mut state = self.state();
        let expected = next_sequence(
            state
                .confirmed_cursor
                .as_ref()
                .map(|confirmed| confirmed.sequence),
        )?;
        if cursor.sequence != expected {
            return Err(EventStreamError::Cursor(format!(
                "confirmed sequence {} is not the contiguous next sequence {}",
                cursor.sequence.get(),
                expected.get()
            )));
        }
        let delivered = state
            .delivered_cursor
            .as_ref()
            .map(|delivered| delivered.sequence);
        if delivered.is_none_or(|delivered| cursor.sequence > delivered) {
            return Err(EventStreamError::Cursor(
                "an event must be delivered before it can be confirmed".to_owned(),
            ));
        }
        state.confirmed_cursor = Some(cursor.clone());
        Ok(cursor.clone())
    }

    /// Cancels this socket without advancing the confirmed application cursor.
    pub async fn cancel(&mut self) {
        self.stop_actor().await;
        while self.receiver.try_recv().is_ok() {}
        let mut state = self.state();
        state.queued_bytes = 0;
        if matches!(state.completion, Completion::Active) {
            state.completion = Completion::Cancelled;
        }
    }

    /// Opens a new single-use capability from only the last explicitly
    /// confirmed cursor. The previous stream must be terminal and drained.
    pub async fn resume(
        &mut self,
        options: EventStreamOptions,
    ) -> Result<DeviceRailEventStream, EventStreamError> {
        if matches!(self.state().completion, Completion::Active) {
            return Err(EventStreamError::Active);
        }
        if !self.receiver.is_empty() {
            return Err(EventStreamError::NotDrained);
        }
        self.stop_actor().await;
        let after_cursor = self.confirmed_cursor();
        self.client
            .open_event_stream(
                EventsSubscribeParams {
                    session_id: self.session_id.clone(),
                    after_cursor,
                },
                options,
            )
            .await
    }

    async fn stop_actor(&mut self) {
        let _ = self.cancel.send(true);
        let Some(mut actor) = self.actor.take() else {
            return;
        };
        if time::timeout(self.close_grace, &mut actor).await.is_err() {
            actor.abort();
            let _ = time::timeout(self.close_grace, &mut actor).await;
        }
    }
}

impl Drop for DeviceRailEventStream {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        if let Some(actor) = self.actor.take() {
            actor.abort();
        }
    }
}

async fn open_socket(
    client: &DeviceRailClient,
    client_peer: &devicerail_protocol::PeerInfo,
    selected_protocol: ProtocolVersion,
    params: &EventsSubscribeParams,
    options: &EventStreamOptions,
) -> Result<OpenedStream, EventStreamError> {
    let capability = client
        .call::<methods::EventsStreamOpen>(
            EventsStreamOpenParams {
                session_id: params.session_id.clone(),
                origin_policy: options.origin_policy.clone(),
            },
            CallOptions::default(),
        )
        .await?;
    // A stale cursor must still complete the one-shot upgrade. The daemon
    // removes the bearer from its pending registry during admission and then
    // rejects the typed subscription, avoiding a TTL-long capability leak.

    let (url, address) = validate_endpoint(capability.endpoint.expose_secret())?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| EventStreamError::InvalidEndpoint("invalid HTTP request target".to_owned()))?;
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(EVENT_STREAM_SUBPROTOCOL),
    );
    if let EventStreamOriginPolicy::Exact { origin } = &options.origin_policy {
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(origin).map_err(|_| {
                EventStreamError::InvalidEndpoint("origin contains invalid header bytes".to_owned())
            })?,
        );
    }

    let tcp = TcpStream::connect(address)
        .await
        .map_err(|error| EventStreamError::Transport(error.to_string()))?;
    let config = WebSocketConfig::default()
        .read_buffer_size(8 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * options.max_message_bytes + 1024)
        .max_message_size(Some(options.max_message_bytes))
        .max_frame_size(Some(options.max_message_bytes))
        .accept_unmasked_frames(false);
    let (mut websocket, response) = client_async_with_config(request, tcp, Some(config))
        .await
        .map_err(map_websocket_error)?;
    let negotiated_subprotocol = response
        .headers()
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .map(|value| value.to_str().ok())
        .collect::<Vec<_>>();
    if negotiated_subprotocol != [Some(EVENT_STREAM_SUBPROTOCOL)] {
        close_protocol(
            &mut websocket,
            "subprotocol required",
            options.write_timeout,
        )
        .await;
        return Err(EventStreamError::ProtocolViolation(
            "event WebSocket did not negotiate devicerail.events.v1".to_owned(),
        ));
    }
    if response
        .headers()
        .get(header::SEC_WEBSOCKET_EXTENSIONS)
        .is_some()
    {
        close_protocol(
            &mut websocket,
            "extensions forbidden",
            options.write_timeout,
        )
        .await;
        return Err(EventStreamError::ProtocolViolation(
            "event WebSocket negotiated an unsupported extension".to_owned(),
        ));
    }

    let hello_params = HelloParams {
        client: client_peer.clone(),
        protocol: ProtocolOffer::exact(selected_protocol),
        features: FeatureOffer {
            required: BTreeSet::from([feature::EVENTS_STREAM_V1.to_owned()]),
            optional: BTreeSet::new(),
        },
    };
    send_rpc_request(
        &mut websocket,
        HELLO_REQUEST_ID,
        "system.hello",
        &hello_params,
        options.max_message_bytes,
        options.write_timeout,
    )
    .await?;
    let hello: HelloResult = receive_rpc_result(
        &mut websocket,
        HELLO_REQUEST_ID,
        options.max_message_bytes,
        options.write_timeout,
    )
    .await?;
    validate_websocket_hello(&hello, selected_protocol)?;

    send_rpc_request(
        &mut websocket,
        SUBSCRIBE_REQUEST_ID,
        "events.subscribe",
        params,
        options.max_message_bytes,
        options.write_timeout,
    )
    .await?;
    let subscription: EventsSubscribeResult = receive_rpc_result(
        &mut websocket,
        SUBSCRIBE_REQUEST_ID,
        options.max_message_bytes,
        options.write_timeout,
    )
    .await
    .map_err(normalize_subscribe_error)?;
    validate_subscription(
        &subscription,
        &params.session_id,
        capability.stream_epoch,
        params.after_cursor.as_ref(),
    )?;

    Ok(OpenedStream {
        websocket,
        stream_epoch: capability.stream_epoch,
        subscription,
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the actor receives one immutable value for each validated stream binding and budget"
)]
async fn stream_actor(
    mut websocket: WebSocketStream<TcpStream>,
    session_id: SessionId,
    stream_epoch: EventStreamEpoch,
    subscription: EventsSubscribeResult,
    after_cursor: Option<EventStreamCursor>,
    selected_protocol: ProtocolVersion,
    options: EventStreamOptions,
    state: Arc<Mutex<StreamState>>,
    sender: mpsc::Sender<QueuedEvent>,
    mut cancel: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    set_completion(&state, Completion::Cancelled);
                    let _ = send_message(
                        &mut websocket,
                        Message::Close(Some(CloseFrame {
                            code: CloseCode::Normal,
                            reason: "client cancelled".into(),
                        })),
                        options.write_timeout,
                        "close write",
                    )
                    .await;
                    break;
                }
            }
            message = websocket.next() => {
                let message = match message {
                    Some(Ok(message)) => message,
                    Some(Err(error)) => {
                        set_completion(
                            &state,
                            Completion::Failed(map_websocket_error(error)),
                        );
                        break;
                    }
                    None => {
                        set_completion(
                            &state,
                            Completion::Failed(EventStreamError::Transport(
                                "event WebSocket ended before a typed terminal".to_owned(),
                            )),
                        );
                        break;
                    }
                };
                match message {
                    Message::Text(text) => {
                        let bytes = text.len();
                        if bytes > options.max_message_bytes {
                            set_completion(
                                &state,
                                Completion::Failed(EventStreamError::ProtocolViolation(format!(
                                    "event WebSocket message is {bytes} bytes; the limit is {}",
                                    options.max_message_bytes
                                ))),
                            );
                            close_protocol(
                                &mut websocket,
                                "message too large",
                                options.write_timeout,
                            )
                            .await;
                            break;
                        }
                        let message = match decode_server_message(text.as_str()) {
                            Ok(message) => message,
                            Err(error) => {
                                set_completion(
                                    &state,
                                    Completion::Failed(EventStreamError::ProtocolViolation(
                                        format!("event WebSocket message is invalid: {error}")
                                    )),
                                );
                                close_protocol(
                                    &mut websocket,
                                    "invalid message",
                                    options.write_timeout,
                                )
                                .await;
                                break;
                            }
                        };
                        let result = match message {
                            RpcServerMessage::Response(_) => Err(EventStreamError::ProtocolViolation(
                                "event WebSocket sent a response after subscription".to_owned(),
                            )),
                            RpcServerMessage::Notification(RpcServerNotification::Event(event)) => {
                                accept_event(
                                    event.params,
                                    bytes,
                                    &session_id,
                                    stream_epoch,
                                    subscription.subscription_id,
                                    after_cursor.as_ref(),
                                    selected_protocol,
                                    &options,
                                    &state,
                                    &sender,
                                )
                            }
                            RpcServerMessage::Notification(RpcServerNotification::Terminal(terminal)) => {
                                accept_terminal(
                                    terminal.params,
                                    &session_id,
                                    stream_epoch,
                                    &subscription,
                                    after_cursor.as_ref(),
                                    &state,
                                ).map(|()| true)
                            }
                        };
                        match result {
                            Ok(true) => {
                                let _ = send_message(
                                    &mut websocket,
                                    Message::Close(Some(CloseFrame {
                                        code: CloseCode::Normal,
                                        reason: "terminal received".into(),
                                    })),
                                    options.write_timeout,
                                    "close write",
                                )
                                .await;
                                break;
                            }
                            Ok(false) => {}
                            Err(error) => {
                                set_completion(&state, Completion::Failed(error));
                                close_protocol(
                                    &mut websocket,
                                    "protocol violation",
                                    options.write_timeout,
                                )
                                .await;
                                break;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if let Err(error) = send_message(
                            &mut websocket,
                            Message::Pong(payload),
                            options.write_timeout,
                            "Pong write",
                        )
                        .await
                        {
                            set_completion(
                                &state,
                                Completion::Failed(error),
                            );
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => {
                        set_completion(
                            &state,
                            Completion::Failed(EventStreamError::Transport(
                                "event WebSocket closed before a typed terminal".to_owned(),
                            )),
                        );
                        break;
                    }
                    Message::Binary(_) | Message::Frame(_) => {
                        set_completion(
                            &state,
                            Completion::Failed(EventStreamError::ProtocolViolation(
                                "event WebSocket application messages must be UTF-8 text".to_owned(),
                            )),
                        );
                        close_protocol(&mut websocket, "text required", options.write_timeout)
                            .await;
                        break;
                    }
                }
            }
        }
    }
    drop(sender);
}

#[allow(
    clippy::too_many_arguments,
    reason = "event acceptance validates every independent wire binding before queueing"
)]
fn accept_event(
    params: EventsStreamEventParams,
    bytes: usize,
    session_id: &SessionId,
    stream_epoch: EventStreamEpoch,
    subscription_id: EventSubscriptionId,
    after_cursor: Option<&EventStreamCursor>,
    selected_protocol: ProtocolVersion,
    options: &EventStreamOptions,
    state: &Arc<Mutex<StreamState>>,
    sender: &mpsc::Sender<QueuedEvent>,
) -> Result<bool, EventStreamError> {
    if params.subscription_id != subscription_id {
        return Err(EventStreamError::ProtocolViolation(
            "event notification identifies another subscription".to_owned(),
        ));
    }
    if params.cursor.session_id != *session_id
        || params.cursor.stream_epoch != stream_epoch
        || params.event.session_id != *session_id
        || params.event.sequence != params.cursor.sequence
    {
        return Err(EventStreamError::Cursor(
            "event notification cursor, event, Session, or epoch do not agree".to_owned(),
        ));
    }
    if params.event.required_protocol_minor() > selected_protocol.minor {
        return Err(EventStreamError::ProtocolViolation(format!(
            "event sequence {} requires protocol 1.{}, but the stream selected {}",
            params.cursor.sequence.get(),
            params.event.required_protocol_minor(),
            selected_protocol
        )));
    }

    let mut shared = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = shared
        .received_cursor
        .as_ref()
        .map(|cursor| cursor.sequence)
        .or_else(|| after_cursor.map(|cursor| cursor.sequence));
    let expected = next_sequence(previous)?;
    if params.cursor.sequence != expected {
        return Err(EventStreamError::Cursor(format!(
            "received sequence {} does not follow {}",
            params.cursor.sequence.get(),
            expected.get().saturating_sub(1)
        )));
    }

    let item = EventStreamItem {
        cursor: params.cursor.clone(),
        event: params.event,
    };
    shared.received_cursor = Some(params.cursor);
    shared.last_received_was_session_end =
        matches!(&item.event.payload, TestEventPayload::SessionEnded { .. });
    let next_bytes =
        shared
            .queued_bytes
            .checked_add(bytes)
            .ok_or(EventStreamError::QueueOverflow {
                max_events: options.max_queued_events,
                max_bytes: options.max_queued_bytes,
            })?;
    if next_bytes > options.max_queued_bytes {
        return Err(EventStreamError::QueueOverflow {
            max_events: options.max_queued_events,
            max_bytes: options.max_queued_bytes,
        });
    }
    match sender.try_send(QueuedEvent { bytes, item }) {
        Ok(()) => {
            shared.queued_bytes = next_bytes;
            Ok(false)
        }
        Err(mpsc::error::TrySendError::Full(_)) => Err(EventStreamError::QueueOverflow {
            max_events: options.max_queued_events,
            max_bytes: options.max_queued_bytes,
        }),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(EventStreamError::Cancelled),
    }
}

fn accept_terminal(
    terminal: EventsStreamTerminalParams,
    session_id: &SessionId,
    stream_epoch: EventStreamEpoch,
    subscription: &EventsSubscribeResult,
    after_cursor: Option<&EventStreamCursor>,
    state: &Arc<Mutex<StreamState>>,
) -> Result<(), EventStreamError> {
    if terminal.subscription_id != subscription.subscription_id
        || terminal.session_id != *session_id
    {
        return Err(EventStreamError::ProtocolViolation(
            "terminal notification identifies another subscription".to_owned(),
        ));
    }
    if terminal.last_emitted_cursor.as_ref().is_some_and(|cursor| {
        cursor.session_id != *session_id || cursor.stream_epoch != stream_epoch
    }) {
        return Err(EventStreamError::Cursor(
            "terminal cursor belongs to another Session or epoch".to_owned(),
        ));
    }

    let mut shared = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if terminal.last_emitted_cursor != shared.received_cursor {
        return Err(EventStreamError::Cursor(
            "terminal cursor does not equal the continuous received prefix".to_owned(),
        ));
    }
    if matches!(terminal.termination, EventsStreamTermination::SessionEnded) {
        let continuous = shared
            .received_cursor
            .as_ref()
            .map(|cursor| cursor.sequence)
            .or_else(|| after_cursor.map(|cursor| cursor.sequence));
        if continuous.is_none_or(|sequence| sequence < subscription.replay_through.sequence) {
            return Err(EventStreamError::Cursor(
                "normal terminal stopped before the subscription replay boundary".to_owned(),
            ));
        }
        let replay_was_already_confirmed = subscription.session_state == SessionState::Ended
            && shared.received_cursor.is_none()
            && after_cursor == Some(&subscription.replay_through);
        if !replay_was_already_confirmed && !shared.last_received_was_session_end {
            return Err(EventStreamError::ProtocolViolation(
                "normal Session termination did not follow a sessionEnded event".to_owned(),
            ));
        }
    }
    shared.terminal = Some(terminal);
    shared.completion = Completion::Remote;
    Ok(())
}

async fn send_rpc_request<T: Serialize>(
    websocket: &mut WebSocketStream<TcpStream>,
    id: &'static str,
    method: &'static str,
    params: &T,
    max_message_bytes: usize,
    write_timeout: Duration,
) -> Result<(), EventStreamError> {
    let value = serde_json::to_value(params)
        .map_err(|error| EventStreamError::Client(ClientError::serialization(error)))?;
    let params = match value {
        serde_json::Value::Object(values) => RpcParams::Object(values),
        serde_json::Value::Array(values) => RpcParams::Array(values),
        _ => {
            return Err(EventStreamError::ProtocolViolation(
                "event WebSocket request params must be an object or array".to_owned(),
            ));
        }
    };
    let request = RpcRequest {
        jsonrpc: JsonRpcVersion::V2,
        id: RpcId::String(id.to_owned()),
        method: method.to_owned(),
        timeout_ms: None,
        params: Some(params),
    };
    let text = serde_json::to_string(&request)
        .map_err(|error| EventStreamError::Client(ClientError::serialization(error)))?;
    if text.len() > max_message_bytes {
        return Err(EventStreamError::ProtocolViolation(format!(
            "{method} request exceeds the event WebSocket message limit"
        )));
    }
    send_message(websocket, Message::text(text), write_timeout, "setup write").await
}

async fn receive_rpc_result<T: DeserializeOwned>(
    websocket: &mut WebSocketStream<TcpStream>,
    expected_id: &'static str,
    max_message_bytes: usize,
    write_timeout: Duration,
) -> Result<T, EventStreamError> {
    loop {
        let message = websocket
            .next()
            .await
            .ok_or_else(|| {
                EventStreamError::Transport(
                    "event WebSocket ended during protocol setup".to_owned(),
                )
            })?
            .map_err(map_websocket_error)?;
        match message {
            Message::Text(text) => {
                if text.len() > max_message_bytes {
                    return Err(EventStreamError::ProtocolViolation(
                        "event WebSocket setup response exceeds the message limit".to_owned(),
                    ));
                }
                let response =
                    serde_json::from_str::<RpcResponse>(text.as_str()).map_err(|error| {
                        EventStreamError::ProtocolViolation(format!(
                            "event WebSocket setup response is invalid: {error}"
                        ))
                    })?;
                validate_websocket_json_safety(text.as_str())?;
                return decode_rpc_result(response, expected_id);
            }
            Message::Ping(payload) => {
                send_message(
                    websocket,
                    Message::Pong(payload),
                    write_timeout,
                    "setup Pong write",
                )
                .await?;
            }
            Message::Pong(_) => {}
            Message::Close(_) => {
                return Err(EventStreamError::Transport(
                    "event WebSocket closed during protocol setup".to_owned(),
                ));
            }
            Message::Binary(_) | Message::Frame(_) => {
                return Err(EventStreamError::ProtocolViolation(
                    "event WebSocket setup messages must be UTF-8 text".to_owned(),
                ));
            }
        }
    }
}

fn decode_server_message(text: &str) -> Result<RpcServerMessage, EventStreamError> {
    let message = serde_json::from_str::<RpcServerMessage>(text).map_err(|error| {
        EventStreamError::ProtocolViolation(format!(
            "event WebSocket message is not a valid DeviceRail server message: {error}"
        ))
    })?;
    validate_websocket_json_safety(text)?;
    Ok(message)
}

fn validate_websocket_json_safety(text: &str) -> Result<(), EventStreamError> {
    let value = parse_strict_json_value(text).map_err(|error| {
        EventStreamError::ProtocolViolation(format!(
            "event WebSocket JSON is not strict cross-language JSON: {error}"
        ))
    })?;
    validate_json_value_safety(&value).map_err(|error| {
        EventStreamError::ProtocolViolation(format!(
            "event WebSocket JSON violates cross-language safety: {error}"
        ))
    })
}

fn decode_rpc_result<T: DeserializeOwned>(
    response: RpcResponse,
    expected_id: &'static str,
) -> Result<T, EventStreamError> {
    match response {
        RpcResponse::Success { id, result, .. } => {
            validate_response_id(&id, expected_id)?;
            serde_json::from_value(result).map_err(|error| {
                EventStreamError::ProtocolViolation(format!(
                    "event WebSocket RPC result is invalid: {error}"
                ))
            })
        }
        RpcResponse::Failure {
            id: Some(id),
            error,
            ..
        } => {
            validate_response_id(&id, expected_id)?;
            Err(EventStreamError::Client(ClientError::RemoteRpc {
                request_id: id,
                error: Box::new(error),
            }))
        }
        RpcResponse::Failure { id: None, .. } => Err(EventStreamError::ProtocolViolation(
            "event WebSocket RPC failure has a null id".to_owned(),
        )),
    }
}

fn validate_response_id(id: &RpcId, expected: &'static str) -> Result<(), EventStreamError> {
    if id == &RpcId::String(expected.to_owned()) {
        Ok(())
    } else {
        Err(EventStreamError::ProtocolViolation(
            "event WebSocket RPC response has an unexpected id".to_owned(),
        ))
    }
}

fn validate_websocket_hello(
    hello: &HelloResult,
    expected_protocol: ProtocolVersion,
) -> Result<(), EventStreamError> {
    if hello.protocol.selected != expected_protocol {
        return Err(EventStreamError::ProtocolViolation(
            "WebSocket hello selected a protocol different from the control connection".to_owned(),
        ));
    }
    if hello.transport.kind != "webSocket" || hello.transport.framing != "jsonMessage" {
        return Err(EventStreamError::ProtocolViolation(
            "WebSocket hello did not negotiate webSocket/jsonMessage".to_owned(),
        ));
    }
    let expected_features = BTreeSet::from([feature::EVENTS_STREAM_V1.to_owned()]);
    if hello.features.enabled != expected_features {
        return Err(EventStreamError::ProtocolViolation(
            "WebSocket hello must enable only events.stream.v1".to_owned(),
        ));
    }
    Ok(())
}

fn validate_subscription(
    subscription: &EventsSubscribeResult,
    session_id: &SessionId,
    stream_epoch: EventStreamEpoch,
    after_cursor: Option<&EventStreamCursor>,
) -> Result<(), EventStreamError> {
    if subscription.session_id != *session_id
        || subscription.replay_through.session_id != *session_id
        || subscription.replay_through.stream_epoch != stream_epoch
    {
        return Err(EventStreamError::Cursor(
            "events.subscribe returned a cursor for another Session or epoch".to_owned(),
        ));
    }
    if after_cursor.is_some_and(|cursor| subscription.replay_through.sequence < cursor.sequence) {
        return Err(EventStreamError::Cursor(
            "events.subscribe replay boundary precedes after_cursor".to_owned(),
        ));
    }
    Ok(())
}

fn validate_after_cursor(params: &EventsSubscribeParams) -> Result<(), EventStreamError> {
    if params
        .after_cursor
        .as_ref()
        .is_some_and(|cursor| cursor.session_id != params.session_id)
    {
        return Err(EventStreamError::Cursor(
            "after_cursor belongs to another Session".to_owned(),
        ));
    }
    Ok(())
}

fn validate_duration(
    value: Duration,
    maximum: Duration,
    name: &'static str,
) -> Result<(), EventStreamError> {
    if value.is_zero() || value > maximum {
        return Err(EventStreamError::ProtocolViolation(format!(
            "{name} must be between 1ns and {}ms",
            maximum.as_millis()
        )));
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<(Url, SocketAddr), EventStreamError> {
    let url = Url::parse(endpoint)
        .map_err(|_| EventStreamError::InvalidEndpoint("URL is malformed".to_owned()))?;
    let token = url.path().strip_prefix("/v/").unwrap_or_default();
    let valid_token = token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let canonical = url
        .port()
        .map(|port| format!("ws://127.0.0.1:{port}/v/{token}"));
    if url.scheme() != "ws"
        || url.host() != Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
        || url.port().is_none()
        || url.port() == Some(0)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !valid_token
        || canonical.as_deref() != Some(endpoint)
    {
        return Err(EventStreamError::InvalidEndpoint(
            "expected ws://127.0.0.1:<port>/v/<64-lowercase-hex-capability>".to_owned(),
        ));
    }
    let address = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        url.port().expect("validated explicit port"),
    );
    Ok((url, address))
}

fn validate_origin_policy(policy: &EventStreamOriginPolicy) -> Result<(), EventStreamError> {
    let EventStreamOriginPolicy::Exact { origin } = policy else {
        return Ok(());
    };
    if origin.len() > 256 || !origin.is_ascii() {
        return Err(EventStreamError::ProtocolViolation(
            "exact Origin must be a short ASCII loopback origin".to_owned(),
        ));
    }
    let url = Url::parse(origin)
        .map_err(|_| EventStreamError::ProtocolViolation("exact Origin is malformed".to_owned()))?;
    let default_port = match url.scheme() {
        "http" => 80,
        "https" => 443,
        _ => {
            return Err(EventStreamError::ProtocolViolation(
                "exact Origin must use http or https".to_owned(),
            ));
        }
    };
    let canonical = url
        .port()
        .map(|port| format!("{}://127.0.0.1:{port}", url.scheme()));
    if url.host() != Some(Host::Ipv4(Ipv4Addr::LOCALHOST))
        || url.port().is_none()
        || url.port() == Some(default_port)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || canonical.as_deref() != Some(origin)
    {
        return Err(EventStreamError::ProtocolViolation(
            "exact Origin must be canonical http(s)://127.0.0.1:<non-default-port>".to_owned(),
        ));
    }
    Ok(())
}

fn next_sequence(
    previous: Option<devicerail_protocol::EventSequence>,
) -> Result<devicerail_protocol::EventSequence, EventStreamError> {
    match previous {
        Some(sequence) => sequence.checked_next().ok_or_else(|| {
            EventStreamError::Cursor("event sequence cannot advance past its wire limit".to_owned())
        }),
        None => Ok(devicerail_protocol::EventSequence::FIRST),
    }
}

fn set_completion(state: &Arc<Mutex<StreamState>>, completion: Completion) {
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(state.completion, Completion::Active) {
        state.completion = completion;
    }
}

fn map_websocket_error(error: WebSocketError) -> EventStreamError {
    match error {
        WebSocketError::Capacity(error) => EventStreamError::ProtocolViolation(format!(
            "event WebSocket exceeded a configured capacity: {error}"
        )),
        WebSocketError::Protocol(error) => EventStreamError::ProtocolViolation(format!(
            "event WebSocket violated RFC 6455: {error}"
        )),
        WebSocketError::Utf8(_) => EventStreamError::ProtocolViolation(
            "event WebSocket contained invalid UTF-8".to_owned(),
        ),
        WebSocketError::AttackAttempt => EventStreamError::ProtocolViolation(
            "event WebSocket transport rejected unsafe input".to_owned(),
        ),
        WebSocketError::Url(_) | WebSocketError::HttpFormat(_) => {
            EventStreamError::InvalidEndpoint(
                "endpoint cannot form a WebSocket upgrade request".to_owned(),
            )
        }
        WebSocketError::Http(response) => EventStreamError::Transport(format!(
            "event WebSocket upgrade was rejected with HTTP {}",
            response.status()
        )),
        WebSocketError::ConnectionClosed => {
            EventStreamError::Transport("event WebSocket connection closed".to_owned())
        }
        WebSocketError::AlreadyClosed => {
            EventStreamError::Transport("event WebSocket is already closed".to_owned())
        }
        WebSocketError::Io(error) => EventStreamError::Transport(error.to_string()),
        WebSocketError::Tls(error) => EventStreamError::Transport(error.to_string()),
        WebSocketError::WriteBufferFull(_) => {
            EventStreamError::Transport("event WebSocket write buffer is full".to_owned())
        }
    }
}

fn normalize_subscribe_error(error: EventStreamError) -> EventStreamError {
    match error {
        EventStreamError::Client(ClientError::RemoteRpc { error, .. })
            if error.data.code == "stream_cursor_epoch_mismatch" =>
        {
            EventStreamError::Cursor(error.data.message.clone())
        }
        error => error,
    }
}

async fn send_message(
    websocket: &mut WebSocketStream<TcpStream>,
    message: Message,
    deadline: Duration,
    phase: &'static str,
) -> Result<(), EventStreamError> {
    time::timeout(deadline, websocket.send(message))
        .await
        .map_err(|_| EventStreamError::Timeout { phase })?
        .map_err(map_websocket_error)
}

async fn close_protocol(
    websocket: &mut WebSocketStream<TcpStream>,
    reason: &'static str,
    write_timeout: Duration,
) {
    let _ = send_message(
        websocket,
        Message::Close(Some(CloseFrame {
            code: CloseCode::Protocol,
            reason: reason.into(),
        })),
        write_timeout,
        "protocol close write",
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use super::{
        Completion, DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES, DeviceRailEventStream,
        EventStreamOptions, StreamState, accept_event, accept_terminal, decode_server_message,
        validate_endpoint, validate_origin_policy,
    };
    use crate::{ClientOptions, ControlTransportKind, DeviceRailClient, EventStreamError};
    use devicerail_protocol::{
        EventId, EventSequence, EventStreamCursor, EventStreamEpoch, EventStreamOriginPolicy,
        EventSubscriptionId, EventsStreamEventParams, EventsStreamTerminalParams,
        EventsStreamTermination, EventsSubscribeResult, ProtocolVersion, SessionId, SessionOutcome,
        SessionState, TestEvent, TestEventPayload,
    };
    use tokio::sync::{mpsc, oneshot, watch};

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn cursor(epoch: EventStreamEpoch, session_id: &SessionId, sequence: u64) -> EventStreamCursor {
        EventStreamCursor {
            stream_epoch: epoch,
            session_id: session_id.clone(),
            sequence: EventSequence::new(sequence).expect("test sequence"),
        }
    }

    fn event(session_id: &SessionId, sequence: u64, payload: TestEventPayload) -> TestEvent {
        TestEvent {
            event_id: EventId::new(),
            session_id: session_id.clone(),
            sequence: EventSequence::new(sequence).expect("test sequence"),
            request_id: None,
            device_id: None,
            at_ms: sequence,
            payload,
        }
    }

    #[test]
    fn endpoint_is_strictly_ipv4_loopback_and_capability_shaped() {
        let token = "a".repeat(64);
        let endpoint = format!("ws://127.0.0.1:4321/v/{token}");
        let (_, address) = validate_endpoint(&endpoint).expect("valid loopback endpoint");
        assert_eq!(address.to_string(), "127.0.0.1:4321");

        for invalid in [
            format!("wss://127.0.0.1:4321/v/{token}"),
            format!("ws://localhost:4321/v/{token}"),
            format!("ws://192.0.2.1:4321/v/{token}"),
            format!("ws://user@127.0.0.1:4321/v/{token}"),
            format!("ws://127.0.0.1:4321/v/{token}?copy=1"),
            "ws://127.0.0.1:4321/v/not-a-capability".to_owned(),
        ] {
            assert!(validate_endpoint(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn browser_origin_is_canonical_and_loopback_only() {
        validate_origin_policy(&EventStreamOriginPolicy::Absent {}).expect("native policy");
        validate_origin_policy(&EventStreamOriginPolicy::Exact {
            origin: "http://127.0.0.1:4173".to_owned(),
        })
        .expect("canonical browser origin");
        for origin in [
            "http://localhost:4173",
            "http://127.0.0.1",
            "http://127.0.0.1:80",
            "https://127.0.0.1:443",
            "http://127.0.0.1:4173/path",
        ] {
            assert!(
                validate_origin_policy(&EventStreamOriginPolicy::Exact {
                    origin: origin.to_owned(),
                })
                .is_err(),
                "{origin}"
            );
        }
    }

    #[test]
    fn options_cannot_relax_the_public_transport_limit() {
        let options = EventStreamOptions {
            max_message_bytes: DEFAULT_EVENT_STREAM_MAX_MESSAGE_BYTES + 1,
            ..EventStreamOptions::default()
        };
        assert!(options.validate().is_err());

        for options in [
            EventStreamOptions {
                setup_timeout: Duration::ZERO,
                ..EventStreamOptions::default()
            },
            EventStreamOptions {
                write_timeout: Duration::ZERO,
                ..EventStreamOptions::default()
            },
            EventStreamOptions {
                close_grace: Duration::ZERO,
                ..EventStreamOptions::default()
            },
        ] {
            assert!(options.validate().is_err());
        }
    }

    #[test]
    fn websocket_json_rejects_unsafe_integers_after_strict_typed_decode() {
        let error = decode_server_message(
            r#"{
                "jsonrpc":"2.0",
                "method":"events.stream.terminal",
                "params":{
                    "subscriptionId":"77777777-7777-4777-8777-777777777777",
                    "sessionId":"33333333-3333-4333-8333-333333333333",
                    "termination":{
                        "reason":"slowConsumer",
                        "error":{
                            "code":"stream_slow_consumer",
                            "message":"slow",
                            "retryable":true,
                            "details":{"future":9007199254740992}
                        }
                    }
                }
            }"#,
        )
        .expect_err("unsafe integer");
        assert!(matches!(error, EventStreamError::ProtocolViolation(_)));
    }

    #[test]
    fn websocket_typed_decode_rejects_duplicate_keys_before_value_scan() {
        let error = decode_server_message(
            r#"{
                "jsonrpc":"2.0",
                "method":"events.stream.terminal",
                "params":{
                    "subscriptionId":"77777777-7777-4777-8777-777777777777",
                    "sessionId":"33333333-3333-4333-8333-333333333333",
                    "sessionId":"44444444-4444-4444-8444-444444444444",
                    "termination":{"reason":"sessionEnded"}
                }
            }"#,
        )
        .expect_err("duplicate Session field");
        assert!(matches!(error, EventStreamError::ProtocolViolation(_)));
    }

    #[test]
    fn received_prefix_and_terminal_are_contiguous_and_subscription_bound() {
        let session_id = SessionId::new();
        let epoch = EventStreamEpoch::new();
        let subscription_id = EventSubscriptionId::new();
        let state = Arc::new(Mutex::new(StreamState::new(None)));
        let (sender, mut receiver) = mpsc::channel(64);
        let options = EventStreamOptions::default();

        assert!(
            !accept_event(
                EventsStreamEventParams {
                    subscription_id,
                    cursor: cursor(epoch, &session_id, 1),
                    event: event(&session_id, 1, TestEventPayload::SessionStarted),
                },
                128,
                &session_id,
                epoch,
                subscription_id,
                None,
                ProtocolVersion::new(1, 5),
                &options,
                &state,
                &sender,
            )
            .expect("first event")
        );
        assert!(matches!(
            accept_event(
                EventsStreamEventParams {
                    subscription_id,
                    cursor: cursor(epoch, &session_id, 3),
                    event: event(
                        &session_id,
                        3,
                        TestEventPayload::SessionEnded {
                            outcome: SessionOutcome::Completed,
                            reason: None,
                        },
                    ),
                },
                128,
                &session_id,
                epoch,
                subscription_id,
                None,
                ProtocolVersion::new(1, 5),
                &options,
                &state,
                &sender,
            ),
            Err(EventStreamError::Cursor(_))
        ));
        assert!(
            !accept_event(
                EventsStreamEventParams {
                    subscription_id,
                    cursor: cursor(epoch, &session_id, 2),
                    event: event(
                        &session_id,
                        2,
                        TestEventPayload::SessionEnded {
                            outcome: SessionOutcome::Completed,
                            reason: None,
                        },
                    ),
                },
                128,
                &session_id,
                epoch,
                subscription_id,
                None,
                ProtocolVersion::new(1, 5),
                &options,
                &state,
                &sender,
            )
            .expect("second event")
        );
        assert_eq!(
            receiver
                .try_recv()
                .expect("first event")
                .item
                .event
                .sequence,
            EventSequence::FIRST
        );
        assert_eq!(
            receiver
                .try_recv()
                .expect("second event")
                .item
                .event
                .sequence,
            EventSequence::new(2).expect("sequence")
        );

        let subscription = EventsSubscribeResult {
            subscription_id,
            session_id: session_id.clone(),
            replay_through: cursor(epoch, &session_id, 2),
            session_state: SessionState::Ended,
        };
        let terminal = EventsStreamTerminalParams {
            subscription_id,
            session_id: session_id.clone(),
            last_emitted_cursor: Some(cursor(epoch, &session_id, 2)),
            termination: EventsStreamTermination::SessionEnded,
        };
        accept_terminal(
            terminal.clone(),
            &session_id,
            epoch,
            &subscription,
            None,
            &state,
        )
        .expect("continuous terminal");
        let shared = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(matches!(shared.completion, Completion::Remote));
        assert_eq!(shared.terminal, Some(terminal));
    }

    #[test]
    fn queue_overflow_advances_only_the_received_boundary() {
        let session_id = SessionId::new();
        let epoch = EventStreamEpoch::new();
        let subscription_id = EventSubscriptionId::new();
        let state = Arc::new(Mutex::new(StreamState::new(None)));
        let (sender, _receiver) = mpsc::channel(1);
        let options = EventStreamOptions {
            max_queued_events: 1,
            ..EventStreamOptions::default()
        };
        accept_event(
            EventsStreamEventParams {
                subscription_id,
                cursor: cursor(epoch, &session_id, 1),
                event: event(&session_id, 1, TestEventPayload::SessionStarted),
            },
            128,
            &session_id,
            epoch,
            subscription_id,
            None,
            ProtocolVersion::new(1, 5),
            &options,
            &state,
            &sender,
        )
        .expect("first event fits");
        assert!(matches!(
            accept_event(
                EventsStreamEventParams {
                    subscription_id,
                    cursor: cursor(epoch, &session_id, 2),
                    event: event(
                        &session_id,
                        2,
                        TestEventPayload::SessionEnded {
                            outcome: SessionOutcome::Completed,
                            reason: None,
                        },
                    ),
                },
                128,
                &session_id,
                epoch,
                subscription_id,
                None,
                ProtocolVersion::new(1, 5),
                &options,
                &state,
                &sender,
            ),
            Err(EventStreamError::QueueOverflow { .. })
        ));
        let shared = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            shared
                .received_cursor
                .as_ref()
                .expect("received boundary")
                .sequence,
            EventSequence::new(2).expect("sequence")
        );
        assert!(shared.delivered_cursor.is_none());
        assert!(shared.confirmed_cursor.is_none());
    }

    #[tokio::test]
    async fn confirmation_requires_the_contiguous_delivered_cursor() {
        let session_id = SessionId::new();
        let epoch = EventStreamEpoch::new();
        let first = cursor(epoch, &session_id, 1);
        let second = cursor(epoch, &session_id, 2);
        let state = Arc::new(Mutex::new(StreamState {
            completion: Completion::Cancelled,
            received_cursor: Some(second.clone()),
            last_received_was_session_end: false,
            delivered_cursor: Some(first.clone()),
            confirmed_cursor: None,
            terminal: None,
            queued_bytes: 0,
        }));
        let (stream_io, peer_io) = tokio::io::duplex(64);
        let (reader, writer) = tokio::io::split(stream_io);
        let client = DeviceRailClient::attach_unnegotiated(
            reader,
            writer,
            ControlTransportKind::Stdio,
            ClientOptions::default(),
        )
        .expect("test client");
        let (_sender, receiver) = mpsc::channel(1);
        let (cancel, _) = watch::channel(false);
        let stream = DeviceRailEventStream {
            client,
            session_id: session_id.clone(),
            stream_epoch: epoch,
            selected_protocol: ProtocolVersion::new(1, 5),
            state,
            receiver,
            cancel,
            actor: None,
            close_grace: std::time::Duration::from_secs(1),
        };

        assert!(matches!(
            stream.confirm(&second),
            Err(EventStreamError::Cursor(_))
        ));
        assert_eq!(
            stream.confirm(&first).expect("confirm first"),
            first.clone()
        );
        assert!(matches!(
            stream.confirm(&first),
            Err(EventStreamError::Cursor(_))
        ));
        drop(peer_io);
    }

    #[tokio::test]
    async fn explicit_cancel_aborts_an_actor_that_ignores_the_cancel_signal() {
        let (stream_io, peer_io) = tokio::io::duplex(64);
        let (reader, writer) = tokio::io::split(stream_io);
        let client = DeviceRailClient::attach_unnegotiated(
            reader,
            writer,
            ControlTransportKind::Stdio,
            ClientOptions::default(),
        )
        .expect("test client");
        let dropped = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = oneshot::channel();
        let actor = tokio::spawn({
            let dropped = Arc::clone(&dropped);
            async move {
                let _flag = DropFlag(dropped);
                let _ = started_tx.send(());
                pending::<()>().await;
            }
        });
        started_rx.await.expect("actor started");
        let (_sender, receiver) = mpsc::channel(1);
        let (cancel, _) = watch::channel(false);
        let state = Arc::new(Mutex::new(StreamState::new(None)));
        let mut stream = DeviceRailEventStream {
            client,
            session_id: SessionId::new(),
            stream_epoch: EventStreamEpoch::new(),
            selected_protocol: ProtocolVersion::new(1, 5),
            state: Arc::clone(&state),
            receiver,
            cancel,
            actor: Some(actor),
            close_grace: Duration::from_millis(10),
        };

        tokio::time::timeout(Duration::from_secs(1), stream.cancel())
            .await
            .expect("cancel must remain bounded");
        assert!(dropped.load(Ordering::SeqCst), "actor was not aborted");
        assert!(matches!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .completion,
            Completion::Cancelled
        ));
        drop(peer_io);
    }

    #[tokio::test]
    async fn dropping_a_stream_aborts_its_actor() {
        let (stream_io, peer_io) = tokio::io::duplex(64);
        let (reader, writer) = tokio::io::split(stream_io);
        let client = DeviceRailClient::attach_unnegotiated(
            reader,
            writer,
            ControlTransportKind::Stdio,
            ClientOptions::default(),
        )
        .expect("test client");
        let dropped = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = oneshot::channel();
        let actor = tokio::spawn({
            let dropped = Arc::clone(&dropped);
            async move {
                let _flag = DropFlag(dropped);
                let _ = started_tx.send(());
                pending::<()>().await;
            }
        });
        started_rx.await.expect("actor started");
        let (_sender, receiver) = mpsc::channel(1);
        let (cancel, _) = watch::channel(false);
        let stream = DeviceRailEventStream {
            client,
            session_id: SessionId::new(),
            stream_epoch: EventStreamEpoch::new(),
            selected_protocol: ProtocolVersion::new(1, 5),
            state: Arc::new(Mutex::new(StreamState::new(None))),
            receiver,
            cancel,
            actor: Some(actor),
            close_grace: Duration::from_millis(10),
        };

        drop(stream);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Drop must abort the actor promptly");
        drop(peer_io);
    }
}
