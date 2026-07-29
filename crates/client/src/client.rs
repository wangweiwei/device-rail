use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    ffi::OsString,
    fmt,
    marker::PhantomData,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::Duration,
};

use devicerail_protocol::{
    FeatureOffer, HelloParams, HelloResult, JsonRpcVersion, PROTOCOL_VERSION, PeerInfo,
    RequestCancelParams, RequestCancelResult, RequestTimeoutMs, RpcId, RpcParams, RpcRequest,
    RpcResponse, TransportInfo, feature, supported_protocol_offer,
};
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    process::{Child, ChildStderr, Command},
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinHandle,
    time,
};
use uuid::Uuid;

use crate::{
    ClientError, DEFAULT_MAX_FRAME_BYTES, NdjsonDecoder,
    methods::{self, ControlMethod, MethodChannel, RpcMethod, validate_dynamic_features},
};

const DEFAULT_MAX_PENDING_REQUESTS: usize = 256;
const DEFAULT_MAX_QUEUED_FRAMES: usize = 256;
const DEFAULT_MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_STDERR_TAIL_BYTES: usize = 64 * 1024;
const DEFAULT_CLOSE_GRACE: Duration = Duration::from_secs(7);
const READ_CHUNK_BYTES: usize = 64 * 1024;
const MAX_JSON_VALUES: usize = 100_000;
const MAX_JSON_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    AwaitingHello,
    HelloInFlight,
    Ready,
    Closing,
    Closed,
    Failed,
}

impl ClientState {
    const fn label(self) -> &'static str {
        match self {
            Self::AwaitingHello => "awaitingHello",
            Self::HelloInFlight => "helloInFlight",
            Self::Ready => "ready",
            Self::Closing => "closing",
            Self::Closed => "closed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlTransportKind {
    Stdio,
    Tcp,
}

impl ControlTransportKind {
    const fn wire_name(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Tcp => "tcp",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClientOptions {
    pub max_frame_bytes: usize,
    pub max_pending_requests: usize,
    pub max_queued_frames: usize,
    pub max_queued_bytes: usize,
    pub stderr_tail_bytes: usize,
    pub close_grace: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_pending_requests: DEFAULT_MAX_PENDING_REQUESTS,
            max_queued_frames: DEFAULT_MAX_QUEUED_FRAMES,
            max_queued_bytes: DEFAULT_MAX_QUEUED_BYTES,
            stderr_tail_bytes: DEFAULT_STDERR_TAIL_BYTES,
            close_grace: DEFAULT_CLOSE_GRACE,
        }
    }
}

impl ClientOptions {
    fn validate(&self) -> Result<(), ClientError> {
        if self.max_frame_bytes == 0 {
            return Err(ClientError::protocol("max_frame_bytes must be positive"));
        }
        if self.max_pending_requests < 2 {
            return Err(ClientError::protocol(
                "max_pending_requests must be at least 2 to reserve cancellation capacity",
            ));
        }
        if self.max_queued_frames == 0 {
            return Err(ClientError::protocol("max_queued_frames must be positive"));
        }
        if self.max_queued_bytes < self.max_frame_bytes.saturating_add(1)
            || self.max_queued_bytes > u32::MAX as usize
        {
            return Err(ClientError::protocol(
                "max_queued_bytes must hold one maximum frame plus its NDJSON delimiter and fit u32",
            ));
        }
        if self.stderr_tail_bytes == 0 {
            return Err(ClientError::protocol("stderr_tail_bytes must be positive"));
        }
        if self.close_grace.is_zero() {
            return Err(ClientError::protocol("close_grace must be positive"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CallOptions {
    pub timeout_ms: Option<RequestTimeoutMs>,
}

#[derive(Clone)]
pub struct SpawnConfig {
    pub command: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<OsString, OsString>,
    pub env_clear: bool,
    pub hello: HelloParams,
    pub client: ClientOptions,
}

impl fmt::Debug for SpawnConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpawnConfig")
            .field("command", &self.command)
            .field("args_len", &self.args.len())
            .field("cwd", &self.cwd)
            .field("env_len", &self.env.len())
            .field("env_clear", &self.env_clear)
            .field("hello", &"<redacted>")
            .field("client", &self.client)
            .finish()
    }
}

impl SpawnConfig {
    pub fn new(command: impl Into<OsString>, hello: HelloParams) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            env_clear: false,
            hello,
            client: ClientOptions::default(),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

pub fn default_hello() -> HelloParams {
    HelloParams {
        client: PeerInfo {
            name: "devicerail-rust".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        protocol: supported_protocol_offer(),
        features: FeatureOffer {
            required: BTreeSet::new(),
            optional: BTreeSet::from([
                feature::ACTION_PROTECTED_V1.to_owned(),
                feature::DEVICE_ROUTING_V1.to_owned(),
                feature::DEVICE_SEMANTIC_ACTIONS_V1.to_owned(),
                feature::EVENTS_SNAPSHOT_V1.to_owned(),
                feature::EVENTS_STREAM_V1.to_owned(),
                feature::MEDIA_STREAM_V1.to_owned(),
                feature::OBSERVATION_UI_SNAPSHOT_V1.to_owned(),
                feature::REQUEST_CONTROL_V1.to_owned(),
                feature::SESSION_EXPORT_PAGE_V1.to_owned(),
                feature::VERDICT_RECORD_V1.to_owned(),
            ]),
        },
    }
}

type ErasedResult = Box<dyn Any + Send>;
type ResultDecoder = fn(Value) -> Result<ErasedResult, ClientError>;

struct PendingRequest {
    application: bool,
    decode: ResultDecoder,
    method: &'static str,
    sender: Option<oneshot::Sender<Result<ErasedResult, ClientError>>>,
}

struct AbandonedRequest {
    decode: ResultDecoder,
    method: &'static str,
}

#[derive(Clone)]
pub(crate) struct NegotiatedConnection {
    pub client: PeerInfo,
    pub result: HelloResult,
}

struct State {
    phase: ClientState,
    pending: HashMap<RpcId, PendingRequest>,
    abandoned: HashMap<RpcId, AbandonedRequest>,
    cancel_backlog: BTreeSet<RpcId>,
    application_pending: usize,
    next_request: u64,
    enabled_features: BTreeSet<String>,
    negotiated: Option<NegotiatedConnection>,
    terminal_error: Option<ClientError>,
}

enum TrackedResponse {
    Pending(PendingRequest),
    Abandoned(AbandonedRequest),
}

enum CancelPumpStep {
    Frame {
        request_id: RpcId,
        target_id: RpcId,
        bytes: Vec<u8>,
    },
    Stop,
    Fail(ClientError),
}

impl State {
    fn new() -> Self {
        Self {
            phase: ClientState::AwaitingHello,
            pending: HashMap::new(),
            abandoned: HashMap::new(),
            cancel_backlog: BTreeSet::new(),
            application_pending: 0,
            next_request: 1,
            enabled_features: BTreeSet::new(),
            negotiated: None,
            terminal_error: None,
        }
    }
}

enum WriterCommand {
    Frame {
        bytes: Vec<u8>,
        _byte_permit: tokio::sync::OwnedSemaphorePermit,
    },
    Close {
        completed: oneshot::Sender<Result<(), ClientError>>,
    },
}

#[derive(Default)]
struct TaskHandles {
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<()>>,
}

struct Inner {
    child: Mutex<Option<Child>>,
    close_lock: tokio::sync::Mutex<()>,
    close_result: watch::Sender<Option<Result<(), ClientError>>>,
    close_started: Mutex<bool>,
    options: ClientOptions,
    expected_transport: ControlTransportKind,
    request_prefix: Uuid,
    shutdown: watch::Sender<bool>,
    state: Mutex<State>,
    stderr_tail: Mutex<VecDeque<u8>>,
    tasks: Mutex<TaskHandles>,
    write_bytes: Arc<Semaphore>,
    writer: mpsc::Sender<WriterCommand>,
}

struct HelloAttemptGuard {
    inner: Arc<Inner>,
    request_sent: bool,
    resolved: bool,
}

impl HelloAttemptGuard {
    fn new(inner: Arc<Inner>) -> Self {
        Self {
            inner,
            request_sent: false,
            resolved: false,
        }
    }
}

impl Drop for HelloAttemptGuard {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        if self.request_sent {
            self.inner.fail(ClientError::transport(
                "system.hello future was dropped after the request was sent",
            ));
            return;
        }
        let mut state = self.inner.state();
        if state.phase == ClientState::HelloInFlight {
            state.phase = ClientState::AwaitingHello;
        }
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn tasks(&self) -> MutexGuard<'_, TaskHandles> {
        self.tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn child(&self) -> MutexGuard<'_, Option<Child>> {
        self.child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn fail(&self, error: ClientError) {
        let pending = {
            let mut state = self.state();
            if matches!(state.phase, ClientState::Closed | ClientState::Failed) {
                return;
            }
            state.phase = ClientState::Failed;
            state.terminal_error = Some(error.clone());
            state.application_pending = 0;
            state.abandoned.clear();
            state.cancel_backlog.clear();
            std::mem::take(&mut state.pending)
        };
        for (_, pending) in pending {
            if let Some(sender) = pending.sender {
                let _ = sender.send(Err(error.clone()));
            }
        }
        let _ = self.shutdown.send(true);
    }

    fn abandon_request(&self, id: &RpcId) {
        let overflow = {
            let mut state = self.state();
            let Some(pending) = state.pending.remove(id) else {
                return;
            };
            if pending.application {
                state.application_pending = state.application_pending.saturating_sub(1);
            }
            if state.abandoned.len() >= self.options.max_pending_requests {
                Some(ClientError::AbandonedRequestLimit(
                    self.options.max_pending_requests,
                ))
            } else {
                let schedule_cancel = state.phase == ClientState::Ready
                    && pending.method != methods::RequestCancel::DESCRIPTOR.name
                    && state.enabled_features.contains(feature::REQUEST_CONTROL_V1);
                state.abandoned.insert(
                    id.clone(),
                    AbandonedRequest {
                        decode: pending.decode,
                        method: pending.method,
                    },
                );
                if schedule_cancel {
                    state.cancel_backlog.insert(id.clone());
                }
                None
            }
        };
        if let Some(error) = overflow {
            self.fail(error);
        } else {
            self.pump_cancel_backlog();
        }
    }

    fn pump_cancel_backlog(&self) {
        loop {
            let step = 'prepare: {
                let mut state = self.state();
                if state.phase != ClientState::Ready
                    || !state.enabled_features.contains(feature::REQUEST_CONTROL_V1)
                    || state.pending.len() >= self.options.max_pending_requests
                {
                    CancelPumpStep::Stop
                } else {
                    let target_id = loop {
                        let Some(target_id) = state.cancel_backlog.iter().next().cloned() else {
                            break None;
                        };
                        if state.abandoned.contains_key(&target_id) {
                            break Some(target_id);
                        }
                        state.cancel_backlog.remove(&target_id);
                    };
                    let Some(target_id) = target_id else {
                        return;
                    };
                    let sequence = state.next_request;
                    let Some(next_request) = state.next_request.checked_add(1) else {
                        drop(state);
                        self.fail(ClientError::Internal(
                            "request id counter overflowed".to_owned(),
                        ));
                        return;
                    };
                    state.next_request = next_request;
                    let request_id = RpcId::String(format!("{}:{sequence}", self.request_prefix));
                    let params = match methods::RequestCancel::encode_params(&RequestCancelParams {
                        request_id: target_id.clone(),
                    }) {
                        Ok(params) => params,
                        Err(error) => {
                            break 'prepare CancelPumpStep::Fail(error);
                        }
                    };
                    let request = RpcRequest {
                        jsonrpc: JsonRpcVersion::V2,
                        id: request_id.clone(),
                        method: methods::RequestCancel::DESCRIPTOR.name.to_owned(),
                        timeout_ms: None,
                        params,
                    };
                    let mut bytes = match serde_json::to_vec(&request) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            break 'prepare CancelPumpStep::Fail(ClientError::serialization(error));
                        }
                    };
                    if bytes.len() > self.options.max_frame_bytes {
                        break 'prepare CancelPumpStep::Fail(ClientError::WriteFrameTooLarge {
                            actual: bytes.len(),
                            limit: self.options.max_frame_bytes,
                        });
                    }
                    bytes.push(b'\n');
                    state.cancel_backlog.remove(&target_id);
                    state.pending.insert(
                        request_id.clone(),
                        PendingRequest {
                            application: false,
                            decode: decode_result::<RequestCancelResult>,
                            method: methods::RequestCancel::DESCRIPTOR.name,
                            sender: None,
                        },
                    );
                    CancelPumpStep::Frame {
                        request_id,
                        target_id,
                        bytes,
                    }
                }
            };

            let CancelPumpStep::Frame {
                request_id,
                target_id,
                bytes,
            } = step
            else {
                if let CancelPumpStep::Fail(error) = step {
                    self.fail(error);
                }
                return;
            };

            let Ok(permit_count) = u32::try_from(bytes.len()) else {
                self.restore_cancel_target(&request_id, &target_id);
                self.fail(ClientError::Internal(
                    "automatic cancellation frame length does not fit u32".to_owned(),
                ));
                return;
            };
            let permit = match Arc::clone(&self.write_bytes).try_acquire_many_owned(permit_count) {
                Ok(permit) => permit,
                Err(_) => {
                    self.restore_cancel_target(&request_id, &target_id);
                    return;
                }
            };
            {
                let state = self.state();
                if state.phase != ClientState::Ready || !state.pending.contains_key(&request_id) {
                    return;
                }
            }
            match self.writer.try_send(WriterCommand::Frame {
                bytes,
                _byte_permit: permit,
            }) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.restore_cancel_target(&request_id, &target_id);
                    return;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    self.restore_cancel_target(&request_id, &target_id);
                    self.fail(ClientError::transport(
                        "request writer closed while scheduling automatic cancellation",
                    ));
                    return;
                }
            }
        }
    }

    fn restore_cancel_target(&self, request_id: &RpcId, target_id: &RpcId) {
        let mut state = self.state();
        state.pending.remove(request_id);
        if state.phase == ClientState::Ready && state.abandoned.contains_key(target_id) {
            state.cancel_backlog.insert(target_id.clone());
        }
    }

    fn accept_response(&self, response: RpcResponse) {
        let (id, result) = match response {
            RpcResponse::Success { id, result, .. } => (id, Ok(result)),
            RpcResponse::Failure {
                id: Some(id),
                error,
                ..
            } => {
                let remote_id = id.clone();
                (
                    id,
                    Err(ClientError::RemoteRpc {
                        request_id: remote_id,
                        error: Box::new(error),
                    }),
                )
            }
            RpcResponse::Failure { id: None, .. } => {
                self.fail(ClientError::protocol(
                    "response for an active client request has a null id",
                ));
                return;
            }
        };

        let tracked = {
            let mut state = self.state();
            if matches!(state.phase, ClientState::Closing | ClientState::Closed) {
                return;
            }
            if let Some(pending) = state.pending.remove(&id) {
                if pending.application {
                    state.application_pending = state.application_pending.saturating_sub(1);
                }
                TrackedResponse::Pending(pending)
            } else if let Some(abandoned) = state.abandoned.remove(&id) {
                state.cancel_backlog.remove(&id);
                TrackedResponse::Abandoned(abandoned)
            } else {
                drop(state);
                self.fail(ClientError::protocol(format!(
                    "response references an unknown or completed id: {id:?}"
                )));
                return;
            }
        };

        match tracked {
            TrackedResponse::Pending(pending) => {
                let decoded = match result {
                    Ok(value) => (pending.decode)(value),
                    Err(error) => Err(error),
                };
                match decoded {
                    Ok(result) => {
                        if let Some(sender) = pending.sender {
                            let _ = sender.send(Ok(result));
                        }
                    }
                    Err(error @ ClientError::RemoteRpc { .. }) => {
                        if let Some(sender) = pending.sender {
                            let _ = sender.send(Err(error));
                        }
                    }
                    Err(error) => {
                        if let Some(sender) = pending.sender {
                            let _ = sender.send(Err(error.clone()));
                        }
                        self.fail(ClientError::protocol(format!(
                            "{} returned an invalid result: {error}",
                            pending.method
                        )));
                        return;
                    }
                }
            }
            TrackedResponse::Abandoned(abandoned) => {
                if let Ok(value) = result
                    && let Err(error) = (abandoned.decode)(value)
                {
                    self.fail(ClientError::protocol(format!(
                        "{} returned an invalid late result: {error}",
                        abandoned.method
                    )));
                    return;
                }
            }
        }
        self.pump_cancel_backlog();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        let tasks = self
            .tasks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(task) = tasks.reader.take() {
            task.abort();
        }
        if let Some(task) = tasks.writer.take() {
            task.abort();
        }
        if let Some(task) = tasks.stderr.take() {
            task.abort();
        }
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(child) = child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

#[derive(Clone)]
pub struct DeviceRailClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for DeviceRailClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceRailClient")
            .field("state", &self.state())
            .field("pending_requests", &self.pending_requests())
            .field("expected_transport", &self.inner.expected_transport)
            .finish_non_exhaustive()
    }
}

impl DeviceRailClient {
    pub async fn spawn(config: SpawnConfig) -> Result<Self, ClientError> {
        config.client.validate()?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &config.cwd {
            command.current_dir(cwd);
        }
        if config.env_clear {
            command.env_clear();
        }
        command.envs(&config.env);

        let mut child = command.spawn().map_err(|error| {
            ClientError::transport(format!(
                "failed to spawn {}: {error}",
                config.command.to_string_lossy()
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::transport("spawned daemon has no piped stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::transport("spawned daemon has no piped stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ClientError::transport("spawned daemon has no piped stderr"))?;
        let client = Self::attach_inner(
            stdout,
            stdin,
            ControlTransportKind::Stdio,
            config.client,
            Some(child),
            Some(stderr),
        )?;
        if let Err(error) = client.hello(config.hello).await {
            let _ = client.close().await;
            return Err(error);
        }
        Ok(client)
    }

    /// Connects to a non-zero IPv4 or IPv6 loopback socket and negotiates hello.
    ///
    /// Use [`Self::attach`] with caller-owned transport halves when a trusted
    /// tunnel, proxy, authentication prelude, or another transport policy is
    /// required.
    pub async fn connect_tcp(
        address: SocketAddr,
        hello: HelloParams,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        validate_loopback_tcp_address(address)?;
        options.validate()?;
        let stream = TcpStream::connect(address).await.map_err(|error| {
            ClientError::transport(format!("failed to connect {address}: {error}"))
        })?;
        let (reader, writer) = stream.into_split();
        Self::attach(reader, writer, ControlTransportKind::Tcp, hello, options).await
    }

    pub async fn attach<R, W>(
        reader: R,
        writer: W,
        transport: ControlTransportKind,
        hello: HelloParams,
        options: ClientOptions,
    ) -> Result<Self, ClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let client = Self::attach_unnegotiated(reader, writer, transport, options)?;
        if let Err(error) = client.hello(hello).await {
            let _ = client.close().await;
            return Err(error);
        }
        Ok(client)
    }

    pub fn attach_unnegotiated<R, W>(
        reader: R,
        writer: W,
        transport: ControlTransportKind,
        options: ClientOptions,
    ) -> Result<Self, ClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::attach_inner(reader, writer, transport, options, None, None)
    }

    fn attach_inner<R, W>(
        reader: R,
        writer: W,
        transport: ControlTransportKind,
        options: ClientOptions,
        child: Option<Child>,
        stderr: Option<ChildStderr>,
    ) -> Result<Self, ClientError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        options.validate()?;
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ClientError::RuntimeUnavailable)?;
        let (writer_tx, writer_rx) = mpsc::channel(options.max_queued_frames);
        let (shutdown, _) = watch::channel(false);
        let (close_result, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            child: Mutex::new(child),
            close_lock: tokio::sync::Mutex::new(()),
            close_result,
            close_started: Mutex::new(false),
            options: options.clone(),
            expected_transport: transport,
            request_prefix: Uuid::new_v4(),
            shutdown,
            state: Mutex::new(State::new()),
            stderr_tail: Mutex::new(VecDeque::with_capacity(options.stderr_tail_bytes)),
            tasks: Mutex::new(TaskHandles::default()),
            write_bytes: Arc::new(Semaphore::new(options.max_queued_bytes)),
            writer: writer_tx,
        });

        let reader_task = runtime.spawn(read_loop(
            Arc::downgrade(&inner),
            reader,
            inner.shutdown.subscribe(),
            options.max_frame_bytes,
        ));
        let writer_task = runtime.spawn(write_loop(
            Arc::downgrade(&inner),
            writer,
            writer_rx,
            inner.shutdown.subscribe(),
        ));
        let stderr_task = stderr.map(|stderr| {
            runtime.spawn(stderr_loop(
                Arc::downgrade(&inner),
                stderr,
                inner.shutdown.subscribe(),
                options.stderr_tail_bytes,
            ))
        });
        *inner.tasks() = TaskHandles {
            reader: Some(reader_task),
            writer: Some(writer_task),
            stderr: stderr_task,
        };
        Ok(Self { inner })
    }

    pub fn state(&self) -> ClientState {
        self.inner.state().phase
    }

    pub fn pending_requests(&self) -> usize {
        self.inner.state().pending.len()
    }

    pub fn enabled_features(&self) -> BTreeSet<String> {
        self.inner.state().enabled_features.clone()
    }

    /// Returns the server-selected connection, protocol, transport, and Feature
    /// result without exposing the client-side hello identity.
    pub fn negotiated_hello(&self) -> Result<HelloResult, ClientError> {
        self.negotiated().map(|connection| connection.result)
    }

    /// Returns a lossily decoded view of the bounded stderr tail captured from
    /// an owned child.
    ///
    /// This is an explicitly sensitive diagnostic interface: daemon stderr may
    /// contain credentials, paths, device data, or other secrets. The client
    /// never injects this value into normal errors or `Debug` output. Callers
    /// should apply their own access control and redaction before persisting or
    /// logging it.
    pub fn stderr_tail(&self) -> String {
        let bytes = self
            .inner
            .stderr_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub async fn hello(&self, params: HelloParams) -> Result<HelloResult, ClientError> {
        validate_hello_offer(&params)?;
        {
            let mut state = self.inner.state();
            if state.phase != ClientState::AwaitingHello {
                return Err(ClientError::HandshakeState(state.phase.label()));
            }
            state.phase = ClientState::HelloInFlight;
        }
        let mut attempt = HelloAttemptGuard::new(Arc::clone(&self.inner));

        let handle = match self.begin_internal::<methods::SystemHello>(
            params.clone(),
            CallOptions::default(),
            true,
        ) {
            Ok(handle) => handle,
            Err(error) => {
                let mut state = self.inner.state();
                if state.phase == ClientState::HelloInFlight {
                    state.phase = ClientState::AwaitingHello;
                }
                attempt.resolved = true;
                return Err(error);
            }
        };
        attempt.request_sent = true;
        let result = match handle.result().await {
            Ok(result) => result,
            Err(error) => {
                let mut state = self.inner.state();
                if state.phase == ClientState::HelloInFlight {
                    state.phase = ClientState::AwaitingHello;
                }
                attempt.resolved = true;
                return Err(error);
            }
        };

        if let Err(error) = validate_hello_result(&params, &result, self.inner.expected_transport) {
            self.inner.fail(error.clone());
            attempt.resolved = true;
            return Err(error);
        }
        {
            let mut state = self.inner.state();
            if state.phase != ClientState::HelloInFlight {
                return Err(state.terminal_error.clone().unwrap_or(ClientError::Closed));
            }
            state.enabled_features = result.features.enabled.clone();
            state.negotiated = Some(NegotiatedConnection {
                client: params.client,
                result: result.clone(),
            });
            state.phase = ClientState::Ready;
        }
        attempt.resolved = true;
        Ok(result)
    }

    pub fn begin_call<M>(
        &self,
        params: M::Params,
        options: CallOptions,
    ) -> Result<RequestHandle<M::Result>, ClientError>
    where
        M: ControlMethod,
    {
        self.begin_internal::<M>(params, options, false)
    }

    pub async fn call<M>(
        &self,
        params: M::Params,
        options: CallOptions,
    ) -> Result<M::Result, ClientError>
    where
        M: ControlMethod,
    {
        self.begin_call::<M>(params, options)?.result().await
    }

    pub async fn cancel(&self, request_id: RpcId) -> Result<RequestCancelResult, ClientError> {
        self.call::<methods::RequestCancel>(
            RequestCancelParams { request_id },
            CallOptions::default(),
        )
        .await
    }

    fn begin_internal<M>(
        &self,
        params: M::Params,
        options: CallOptions,
        bootstrap: bool,
    ) -> Result<RequestHandle<M::Result>, ClientError>
    where
        M: RpcMethod,
    {
        let descriptor = M::DESCRIPTOR;
        if bootstrap != (descriptor.channel == MethodChannel::Bootstrap) {
            return Err(ClientError::protocol(format!(
                "{} is not available on this control call path",
                descriptor.name
            )));
        }
        let params = M::encode_params(&params)?;
        validate_params_json_safety(params.as_ref())?;

        let (id, frame, receiver) = {
            let mut state = self.inner.state();
            let expected_state = if bootstrap {
                ClientState::HelloInFlight
            } else {
                ClientState::Ready
            };
            if state.phase != expected_state {
                return Err(ClientError::HandshakeState(state.phase.label()));
            }
            if !bootstrap {
                validate_dynamic_features(
                    descriptor,
                    params.as_ref(),
                    &state.enabled_features,
                    options.timeout_ms.is_some(),
                )?;
            }
            let cancellation_reserve = usize::min(
                16,
                usize::max(1, self.inner.options.max_pending_requests / 4),
            );
            let max_application = self
                .inner
                .options
                .max_pending_requests
                .saturating_sub(cancellation_reserve);
            let application = !bootstrap && descriptor.name != "request.cancel";
            if state.pending.len() >= self.inner.options.max_pending_requests {
                return Err(ClientError::PendingRequestLimit(
                    self.inner.options.max_pending_requests,
                ));
            }
            if application && state.application_pending >= max_application {
                return Err(ClientError::PendingRequestLimit(max_application));
            }

            let sequence = state.next_request;
            state.next_request = state
                .next_request
                .checked_add(1)
                .ok_or_else(|| ClientError::Internal("request id counter overflowed".to_owned()))?;
            let id = RpcId::String(format!("{}:{sequence}", self.inner.request_prefix));
            let request = RpcRequest {
                jsonrpc: JsonRpcVersion::V2,
                id: id.clone(),
                method: descriptor.name.to_owned(),
                timeout_ms: options.timeout_ms,
                params,
            };
            let mut frame = serde_json::to_vec(&request).map_err(ClientError::serialization)?;
            if frame.len() > self.inner.options.max_frame_bytes {
                return Err(ClientError::WriteFrameTooLarge {
                    actual: frame.len(),
                    limit: self.inner.options.max_frame_bytes,
                });
            }
            frame.push(b'\n');

            let (sender, receiver) = oneshot::channel();
            state.pending.insert(
                id.clone(),
                PendingRequest {
                    application,
                    decode: decode_result::<M::Result>,
                    method: descriptor.name,
                    sender: Some(sender),
                },
            );
            if application {
                state.application_pending += 1;
            }
            (id, frame, receiver)
        };

        let permit_count = u32::try_from(frame.len())
            .map_err(|_| ClientError::Internal("frame length does not fit u32".to_owned()))?;
        let permit = match Arc::clone(&self.inner.write_bytes).try_acquire_many_owned(permit_count)
        {
            Ok(permit) => permit,
            Err(_) => {
                self.remove_pending(&id);
                return Err(ClientError::WriteQueueFull {
                    max_frames: self.inner.options.max_queued_frames,
                    max_bytes: self.inner.options.max_queued_bytes,
                });
            }
        };
        if self
            .inner
            .writer
            .try_send(WriterCommand::Frame {
                bytes: frame,
                _byte_permit: permit,
            })
            .is_err()
        {
            self.remove_pending(&id);
            return Err(
                if matches!(self.state(), ClientState::Failed | ClientState::Closed) {
                    self.inner
                        .state()
                        .terminal_error
                        .clone()
                        .unwrap_or(ClientError::Closed)
                } else {
                    ClientError::WriteQueueFull {
                        max_frames: self.inner.options.max_queued_frames,
                        max_bytes: self.inner.options.max_queued_bytes,
                    }
                },
            );
        }

        Ok(RequestHandle {
            client: self.clone(),
            id,
            receiver: Some(receiver),
            abandon_on_drop: !bootstrap,
            result: PhantomData,
        })
    }

    fn remove_pending(&self, id: &RpcId) {
        let mut state = self.inner.state();
        if let Some(pending) = state.pending.remove(id)
            && pending.application
        {
            state.application_pending = state.application_pending.saturating_sub(1);
        }
    }

    pub async fn close(&self) -> Result<(), ClientError> {
        let mut completion = self.inner.close_result.subscribe();
        {
            let _close_guard = self.inner.close_lock.lock().await;
            if let Some(result) = self.inner.close_result.borrow().clone() {
                return result;
            }
            let start_cleanup = {
                let mut started = self
                    .inner
                    .close_started
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *started {
                    false
                } else {
                    *started = true;
                    true
                }
            };
            if start_cleanup {
                let (failed, pending) = {
                    let mut state = self.inner.state();
                    if state.phase == ClientState::Closed {
                        return Ok(());
                    }
                    let failed = state.phase == ClientState::Failed;
                    if !failed {
                        state.phase = ClientState::Closing;
                    }
                    state.application_pending = 0;
                    state.abandoned.clear();
                    state.cancel_backlog.clear();
                    (failed, std::mem::take(&mut state.pending))
                };
                for (_, pending) in pending {
                    if let Some(sender) = pending.sender {
                        let _ = sender.send(Err(ClientError::Closed));
                    }
                }
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move {
                    let result = close_inner(Arc::clone(&inner), failed).await;
                    {
                        let mut state = inner.state();
                        if let Err(error) = &result
                            && state.terminal_error.is_none()
                        {
                            state.terminal_error = Some(error.clone());
                        }
                        state.phase = ClientState::Closed;
                    }
                    inner.close_result.send_replace(Some(result));
                });
            }
        }

        loop {
            if let Some(result) = completion.borrow().clone() {
                return result;
            }
            completion.changed().await.map_err(|_| {
                ClientError::Internal("client close completion channel ended".to_owned())
            })?;
        }
    }

    pub(crate) fn negotiated(&self) -> Result<NegotiatedConnection, ClientError> {
        let state = self.inner.state();
        if state.phase != ClientState::Ready {
            return Err(ClientError::HandshakeState(state.phase.label()));
        }
        state
            .negotiated
            .clone()
            .ok_or_else(|| ClientError::Internal("ready client has no hello result".to_owned()))
    }
}

async fn close_inner(inner: Arc<Inner>, failed: bool) -> Result<(), ClientError> {
    let deadline = time::Instant::now()
        .checked_add(inner.options.close_grace)
        .ok_or_else(|| ClientError::protocol("close_grace exceeds the Tokio clock range"))?;
    let mut first_error = None;
    let mut shutdown_sent = failed;
    if failed {
        let _ = inner.shutdown.send(true);
    } else if let Err(error) = close_writer_input(&inner, deadline).await {
        first_error = Some(error);
        shutdown_sent = true;
        let _ = inner.shutdown.send(true);
    }

    let mut child = inner.child().take();
    if let Some(child) = child.as_mut() {
        let mut exited = false;
        if time::Instant::now() < deadline {
            match time::timeout_at(deadline, child.wait()).await {
                Ok(Ok(status)) => {
                    exited = true;
                    if !status.success() && first_error.is_none() {
                        first_error = Some(ClientError::transport(format!(
                            "spawned daemon exited unsuccessfully during close: {status}"
                        )));
                    }
                }
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(ClientError::transport(format!(
                            "failed to wait for spawned daemon during close: {error}"
                        )));
                    }
                }
                Err(_) => {}
            }
        }
        if !exited {
            match child.start_kill() {
                Ok(()) if first_error.is_none() => {
                    first_error = Some(ClientError::transport(format!(
                        "spawned daemon did not exit within {:?}",
                        inner.options.close_grace
                    )));
                }
                Err(error) if first_error.is_none() => {
                    first_error = Some(ClientError::transport(format!(
                        "failed to terminate spawned daemon during close: {error}"
                    )));
                }
                Ok(()) | Err(_) => {}
            }
            if time::Instant::now() < deadline {
                let _ = time::timeout_at(deadline, child.wait()).await;
            }
        }
    }

    if !shutdown_sent {
        let _ = inner.shutdown.send(true);
    }
    let tasks = {
        let mut tasks = inner.tasks();
        TaskHandles {
            reader: tasks.reader.take(),
            writer: tasks.writer.take(),
            stderr: tasks.stderr.take(),
        }
    };
    if let Some(task) = tasks.reader {
        task.abort();
    }
    if let Some(task) = tasks.stderr {
        task.abort();
    }
    if let Some(mut task) = tasks.writer {
        if time::Instant::now() < deadline {
            match time::timeout_at(deadline, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(ClientError::Internal(format!(
                            "request writer task failed during close: {error}"
                        )));
                    }
                }
                Err(_) => {
                    task.abort();
                    if first_error.is_none() {
                        first_error = Some(ClientError::transport(format!(
                            "request writer did not stop within {:?}",
                            inner.options.close_grace
                        )));
                    }
                }
            }
        } else {
            task.abort();
            if first_error.is_none() {
                first_error = Some(ClientError::transport(format!(
                    "client close exceeded its {:?} grace period",
                    inner.options.close_grace
                )));
            }
        }
    }

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn close_writer_input(inner: &Inner, deadline: time::Instant) -> Result<(), ClientError> {
    let (completed, completion) = oneshot::channel();
    let close = WriterCommand::Close { completed };
    match inner.writer.try_send(close) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(close)) => {
            time::timeout_at(deadline, inner.writer.send(close))
                .await
                .map_err(|_| {
                    ClientError::transport(format!(
                        "request writer queue did not drain within {:?}",
                        inner.options.close_grace
                    ))
                })?
                .map_err(|_| ClientError::transport("request writer closed during client close"))?;
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            return Err(inner
                .state()
                .terminal_error
                .clone()
                .unwrap_or_else(|| ClientError::transport("request writer is already closed")));
        }
    }
    time::timeout_at(deadline, completion)
        .await
        .map_err(|_| {
            ClientError::transport(format!(
                "request writer did not close within {:?}",
                inner.options.close_grace
            ))
        })?
        .map_err(|_| ClientError::transport("request writer close acknowledgement was dropped"))?
}

struct RequestAbandonGuard {
    inner: Arc<Inner>,
    id: RpcId,
    armed: bool,
}

impl Drop for RequestAbandonGuard {
    fn drop(&mut self) {
        if self.armed {
            self.inner.abandon_request(&self.id);
        }
    }
}

pub struct RequestHandle<T> {
    client: DeviceRailClient,
    id: RpcId,
    receiver: Option<oneshot::Receiver<Result<ErasedResult, ClientError>>>,
    abandon_on_drop: bool,
    result: PhantomData<fn() -> T>,
}

impl<T> fmt::Debug for RequestHandle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<T> Drop for RequestHandle<T> {
    fn drop(&mut self) {
        if self.abandon_on_drop && self.receiver.is_some() {
            self.client.inner.abandon_request(&self.id);
        }
    }
}

impl<T> RequestHandle<T>
where
    T: Send + 'static,
{
    pub fn id(&self) -> &RpcId {
        &self.id
    }

    pub async fn cancel_remote(&self) -> Result<RequestCancelResult, ClientError> {
        self.client.cancel(self.id.clone()).await
    }

    pub async fn result(mut self) -> Result<T, ClientError> {
        let receiver = self.receiver.take().ok_or_else(|| {
            ClientError::Internal("request result was already consumed".to_owned())
        })?;
        let mut abandon = RequestAbandonGuard {
            inner: Arc::clone(&self.client.inner),
            id: self.id.clone(),
            armed: self.abandon_on_drop,
        };
        self.abandon_on_drop = false;
        let received = receiver.await;
        abandon.armed = false;
        let erased =
            received.map_err(|_| ClientError::transport("request response channel closed"))??;
        erased.downcast::<T>().map(|value| *value).map_err(|_| {
            ClientError::Internal("request result type did not match method descriptor".to_owned())
        })
    }
}

fn decode_result<T>(value: Value) -> Result<ErasedResult, ClientError>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    serde_json::from_value::<T>(value)
        .map(|value| Box::new(value) as ErasedResult)
        .map_err(ClientError::serialization)
}

struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(StrictJsonValue)
            .ok_or_else(|| E::custom("JSON numbers must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictJsonValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "JSON object contains duplicate field {key}"
                )));
            }
            let value = object.next_value::<StrictJsonValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(values)))
    }
}

pub(crate) fn parse_strict_json_value(text: &str) -> Result<Value, ClientError> {
    serde_json::from_str::<StrictJsonValue>(text)
        .map(|value| value.0)
        .map_err(ClientError::serialization)
}

pub(crate) fn validate_json_value_safety(root: &Value) -> Result<(), ClientError> {
    validate_json_values(&mut vec![(root, 0)])
}

fn validate_params_json_safety(params: Option<&RpcParams>) -> Result<(), ClientError> {
    let Some(params) = params else {
        return Ok(());
    };
    let mut pending = match params {
        RpcParams::Object(values) => values.values().map(|value| (value, 1)).collect::<Vec<_>>(),
        RpcParams::Array(values) => values.iter().map(|value| (value, 1)).collect::<Vec<_>>(),
    };
    validate_json_values(&mut pending)
}

fn validate_json_values(pending: &mut Vec<(&Value, usize)>) -> Result<(), ClientError> {
    let mut visited = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        visited = visited.saturating_add(1);
        if visited > MAX_JSON_VALUES {
            return Err(ClientError::protocol(format!(
                "JSON value exceeds the {MAX_JSON_VALUES}-node complexity limit"
            )));
        }
        if depth > MAX_JSON_DEPTH {
            return Err(ClientError::protocol(format!(
                "JSON value exceeds the {MAX_JSON_DEPTH}-level nesting limit"
            )));
        }
        match value {
            Value::Null | Value::Bool(_) | Value::String(_) => {}
            Value::Number(number) => {
                if number
                    .as_u64()
                    .is_some_and(|value| value > devicerail_protocol::MAX_SAFE_INTEGER)
                    || number.as_i64().is_some_and(|value| {
                        value.unsigned_abs() > devicerail_protocol::MAX_SAFE_INTEGER
                    })
                    || number.as_f64().is_some_and(|value| {
                        value.is_finite()
                            && value.fract() == 0.0
                            && value.abs() > devicerail_protocol::MAX_SAFE_INTEGER as f64
                    })
                {
                    return Err(ClientError::protocol(
                        "JSON contains an integer outside the cross-language safe range",
                    ));
                }
            }
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                pending.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
        }
    }
    Ok(())
}

fn validate_hello_offer(params: &HelloParams) -> Result<(), ClientError> {
    if params.protocol.ranges.is_empty() {
        return Err(ClientError::protocol(
            "system.hello protocol offer must not be empty",
        ));
    }
    for (index, range) in params.protocol.ranges.iter().enumerate() {
        if !range.is_valid()
            || range.major != PROTOCOL_VERSION.major
            || range.max_minor > PROTOCOL_VERSION.minor
        {
            return Err(ClientError::protocol(format!(
                "system.hello protocol range {index} exceeds Rust client support for 1.0-1.{}",
                PROTOCOL_VERSION.minor
            )));
        }
    }
    Ok(())
}

fn validate_hello_result(
    offer: &HelloParams,
    result: &HelloResult,
    expected_transport: ControlTransportKind,
) -> Result<(), ClientError> {
    let selected = result.protocol.selected;
    if !offer.protocol.ranges.iter().any(|range| {
        range.major == selected.major
            && range.min_minor <= selected.minor
            && selected.minor <= range.max_minor
    }) {
        return Err(ClientError::protocol(
            "system.hello selected a protocol outside the client offer",
        ));
    }
    if selected.major != PROTOCOL_VERSION.major || selected.minor > PROTOCOL_VERSION.minor {
        return Err(ClientError::protocol(
            "system.hello selected a protocol unsupported by this Rust client",
        ));
    }
    for feature in &offer.features.required {
        if !result.features.enabled.contains(feature) {
            return Err(ClientError::protocol(format!(
                "system.hello omitted required feature {feature}"
            )));
        }
    }
    let offered = offer
        .features
        .required
        .union(&offer.features.optional)
        .collect::<BTreeSet<_>>();
    for enabled in &result.features.enabled {
        if !offered.contains(enabled) {
            return Err(ClientError::protocol(format!(
                "system.hello enabled unoffered feature {enabled}"
            )));
        }
        let minimum_minor = match enabled.as_str() {
            feature::REQUEST_CONTROL_V1 => Some(1),
            feature::DEVICE_ROUTING_V1 | feature::ACTION_PROTECTED_V1 => Some(2),
            feature::EVENTS_STREAM_V1 => Some(3),
            feature::MEDIA_STREAM_V1 | feature::SESSION_EXPORT_PAGE_V1 => Some(4),
            feature::OBSERVATION_UI_SNAPSHOT_V1
            | feature::DEVICE_SEMANTIC_ACTIONS_V1
            | feature::VERDICT_RECORD_V1 => Some(5),
            _ => None,
        };
        if minimum_minor.is_some_and(|minimum| selected.minor < minimum) {
            return Err(ClientError::protocol(format!(
                "system.hello enabled {enabled} before protocol 1.{}",
                minimum_minor.expect("minimum is present")
            )));
        }
    }
    if result
        .features
        .enabled
        .contains(feature::SESSION_EXPORT_PAGE_V1)
        && !result
            .features
            .enabled
            .contains(feature::EVENTS_SNAPSHOT_V1)
    {
        return Err(ClientError::protocol(
            "session.export.page.v1 requires events.snapshot.v1",
        ));
    }
    if result
        .features
        .enabled
        .contains(feature::DEVICE_SEMANTIC_ACTIONS_V1)
        && !result
            .features
            .enabled
            .contains(feature::OBSERVATION_UI_SNAPSHOT_V1)
    {
        return Err(ClientError::protocol(
            "device.semanticActions.v1 requires observation.uiSnapshot.v1",
        ));
    }
    let TransportInfo { kind, framing } = &result.transport;
    if kind != expected_transport.wire_name() || framing != "ndjson" {
        return Err(ClientError::protocol(format!(
            "system.hello negotiated {kind}/{framing}, expected {}/ndjson",
            expected_transport.wire_name()
        )));
    }
    Ok(())
}

fn validate_loopback_tcp_address(address: SocketAddr) -> Result<(), ClientError> {
    if address.port() == 0 || !address.ip().is_loopback() {
        return Err(ClientError::protocol(
            "connect_tcp requires a non-zero IPv4 or IPv6 loopback address",
        ));
    }
    Ok(())
}

async fn read_loop<R>(
    inner: Weak<Inner>,
    mut reader: R,
    mut shutdown: watch::Receiver<bool>,
    max_frame_bytes: usize,
) where
    R: AsyncRead + Unpin,
{
    let mut decoder = match NdjsonDecoder::new(max_frame_bytes) {
        Ok(decoder) => decoder,
        Err(error) => {
            if let Some(inner) = inner.upgrade() {
                inner.fail(error.into());
            }
            return;
        }
    };
    let mut buffer = vec![0_u8; READ_CHUNK_BYTES];
    loop {
        let read = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
            read = reader.read(&mut buffer) => read,
        };
        match read {
            Ok(0) => {
                let finish = decoder.finish();
                if let Some(inner) = inner.upgrade() {
                    if let Err(error) = finish {
                        inner.fail(error.into());
                    } else if !matches!(
                        inner.state().phase,
                        ClientState::Closing | ClientState::Closed | ClientState::Failed
                    ) {
                        inner.fail(ClientError::transport(
                            "response stream ended before the client completed",
                        ));
                    }
                }
                return;
            }
            Ok(read) => match decoder.push(&buffer[..read]) {
                Ok(frames) => {
                    for frame in frames {
                        let Some(inner) = inner.upgrade() else {
                            return;
                        };
                        if frame.is_empty() {
                            inner.fail(ClientError::protocol(
                                "response stream contains an empty NDJSON frame",
                            ));
                            return;
                        }
                        match serde_json::from_str::<RpcResponse>(&frame) {
                            Ok(response) => {
                                let safety = parse_strict_json_value(&frame)
                                    .and_then(|value| validate_json_value_safety(&value));
                                if let Err(error) = safety {
                                    inner.fail(ClientError::protocol(format!(
                                        "response frame violates cross-language JSON safety: {error}"
                                    )));
                                    return;
                                }
                                inner.accept_response(response);
                            }
                            Err(error) => {
                                inner.fail(ClientError::protocol(format!(
                                    "response frame is not a strict JSON-RPC response: {error}"
                                )));
                                return;
                            }
                        }
                        if inner.state().phase == ClientState::Failed {
                            return;
                        }
                    }
                }
                Err(error) => {
                    if let Some(inner) = inner.upgrade() {
                        inner.fail(error.into());
                    }
                    return;
                }
            },
            Err(error) => {
                if let Some(inner) = inner.upgrade() {
                    inner.fail(ClientError::transport(format!(
                        "failed to read response stream: {error}"
                    )));
                }
                return;
            }
        }
    }
}

async fn write_loop<W>(
    inner: Weak<Inner>,
    mut writer: W,
    mut commands: mpsc::Receiver<WriterCommand>,
    mut shutdown: watch::Receiver<bool>,
) where
    W: AsyncWrite + Unpin,
{
    loop {
        let command = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
            command = commands.recv() => command,
        };
        match command {
            Some(WriterCommand::Frame { bytes, .. }) => {
                let write = tokio::select! {
                    biased;
                    _ = shutdown.changed() => return,
                    result = writer.write_all(&bytes) => result,
                };
                if let Err(error) = write {
                    if let Some(inner) = inner.upgrade() {
                        inner.fail(ClientError::transport(format!(
                            "failed to write request frame: {error}"
                        )));
                    }
                    return;
                }
                let flush = tokio::select! {
                    biased;
                    _ = shutdown.changed() => return,
                    result = writer.flush() => result,
                };
                if let Err(error) = flush {
                    if let Some(inner) = inner.upgrade() {
                        inner.fail(ClientError::transport(format!(
                            "failed to flush request frame: {error}"
                        )));
                    }
                    return;
                }
                if let Some(inner) = inner.upgrade() {
                    inner.pump_cancel_backlog();
                }
            }
            Some(WriterCommand::Close { completed }) => {
                let result = tokio::select! {
                    biased;
                    _ = shutdown.changed() => Err(ClientError::Closed),
                    result = writer.shutdown() => result.map_err(|error| {
                        ClientError::transport(format!("failed to close request stream: {error}"))
                    }),
                };
                let _ = completed.send(result);
                return;
            }
            None => return,
        }
    }
}

async fn stderr_loop(
    inner: Weak<Inner>,
    mut stderr: ChildStderr,
    mut shutdown: watch::Receiver<bool>,
    max_bytes: usize,
) {
    let mut buffer = vec![0_u8; 16 * 1024];
    loop {
        let read = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
                continue;
            }
            read = stderr.read(&mut buffer) => read,
        };
        match read {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let mut tail = inner
                    .stderr_tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                tail.extend(&buffer[..read]);
                while tail.len() > max_bytes {
                    tail.pop_front();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, net::SocketAddr};

    use devicerail_protocol::{
        FeatureSelection, HelloResult, PeerInfo, ProtocolRange, ProtocolSelection, ProtocolVersion,
        TransportInfo,
    };
    use uuid::Uuid;

    use super::{
        ControlTransportKind, SpawnConfig, default_hello, parse_strict_json_value,
        validate_hello_offer, validate_hello_result, validate_json_value_safety,
        validate_loopback_tcp_address,
    };
    use serde_json::json;

    #[test]
    fn default_hello_offers_every_current_feature() {
        let hello = default_hello();
        assert_eq!(hello.protocol.ranges, vec![ProtocolRange::new(1, 0, 5)]);
        assert_eq!(hello.features.optional.len(), 10);
        validate_hello_offer(&hello).expect("valid default hello");
    }

    #[test]
    fn hello_result_must_match_offer_and_transport() {
        let hello = default_hello();
        let result = HelloResult {
            connection_id: Uuid::nil(),
            protocol: ProtocolSelection {
                selected: ProtocolVersion::new(1, 5),
            },
            server: PeerInfo {
                name: "test".to_owned(),
                version: "0.1.0".to_owned(),
            },
            transport: TransportInfo {
                kind: "stdio".to_owned(),
                framing: "ndjson".to_owned(),
            },
            features: FeatureSelection {
                enabled: BTreeSet::new(),
            },
        };
        validate_hello_result(&hello, &result, ControlTransportKind::Stdio).expect("valid result");
        assert!(validate_hello_result(&hello, &result, ControlTransportKind::Tcp).is_err());
    }

    #[test]
    fn open_json_values_reject_cross_language_unsafe_integers() {
        validate_json_value_safety(&json!({
            "nested": [devicerail_protocol::MAX_SAFE_INTEGER]
        }))
        .expect("maximum safe integer");
        assert!(
            validate_json_value_safety(&json!({
                "nested": [devicerail_protocol::MAX_SAFE_INTEGER + 1]
            }))
            .is_err()
        );
        assert!(
            parse_strict_json_value(r#"{"nested":{"key":1,"key":2}}"#).is_err(),
            "nested duplicate keys must be rejected before Value construction"
        );
    }

    #[test]
    fn connect_tcp_accepts_only_non_zero_ipv4_or_ipv6_loopback_addresses() {
        for address in ["127.0.0.1:1", "[::1]:65535"] {
            validate_loopback_tcp_address(
                address
                    .parse::<SocketAddr>()
                    .expect("valid loopback socket address"),
            )
            .expect("loopback endpoint is allowed");
        }
        for address in ["127.0.0.1:0", "[::1]:0", "192.0.2.1:1", "[2001:db8::1]:1"] {
            let error = validate_loopback_tcp_address(
                address
                    .parse::<SocketAddr>()
                    .expect("valid rejected socket address"),
            )
            .expect_err("non-loopback or zero-port endpoint must be rejected");
            assert!(
                matches!(error, crate::ClientError::ProtocolViolation(_)),
                "unexpected validation error: {error:?}"
            );
        }
    }

    #[test]
    fn spawn_config_debug_redacts_argument_environment_and_hello_values() {
        const ARG_SENTINEL: &str = "ARGUMENT_SECRET_SENTINEL";
        const ENV_SENTINEL: &str = "ENVIRONMENT_SECRET_SENTINEL";
        const HELLO_SENTINEL: &str = "HELLO_SECRET_SENTINEL";

        let mut hello = default_hello();
        hello.client.name = HELLO_SENTINEL.to_owned();
        hello
            .features
            .optional
            .insert(format!("{HELLO_SENTINEL}.feature"));
        let config = SpawnConfig::new("devicerail-daemon", hello)
            .arg(ARG_SENTINEL)
            .env("DEVICERAIL_SECRET", ENV_SENTINEL);

        let rendered = format!("{config:?}");
        assert!(rendered.contains("SpawnConfig"));
        assert!(rendered.contains("args_len: 1"));
        assert!(rendered.contains("env_len: 1"));
        assert!(rendered.contains("hello: \"<redacted>\""));
        for sentinel in [ARG_SENTINEL, ENV_SENTINEL, HELLO_SENTINEL] {
            assert!(
                !rendered.contains(sentinel),
                "SpawnConfig Debug leaked {sentinel}"
            );
        }
    }
}
