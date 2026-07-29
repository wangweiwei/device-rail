//! Bounded loopback WebSocket transport for DeviceRail event streams.
//!
//! This crate is deliberately above Core: Core owns durable event ordering,
//! while this crate owns bearer capabilities, RFC 6455, serialized queue
//! budgets, and connection cleanup.

mod connection;
mod queue;

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use devicerail_core::MemoryEventStore;
use devicerail_protocol::{
    EventStreamEndpoint, EventStreamEpoch, EventStreamOriginPolicy, EventsStreamOpenParams,
    EventsStreamOpenResult, MAX_SAFE_INTEGER, SessionId,
};
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time::{self, Instant},
};
use uuid::Uuid;

const CAPABILITY_HEX_LENGTH: usize = 64;
const DEFAULT_CAPABILITY_TTL: Duration = Duration::from_secs(30);
const MAX_CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);
const DEFAULT_MAX_PENDING_CAPABILITIES: usize = 256;
const MAX_PENDING_CAPABILITIES: usize = 4_096;
const DEFAULT_MAX_CONNECTIONS: usize = 32;
const MAX_CONNECTIONS: usize = 256;
const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_QUEUE_STALL_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_QUEUE_STALL_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_HEADER_COUNT: usize = 32;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const DEFAULT_MAX_QUEUED_EVENTS: usize = 64;
const MAX_QUEUED_EVENTS: usize = 1_024;
const DEFAULT_MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;

/// Hard limits for one loopback listener and its subscribers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
    pub capability_ttl: Duration,
    pub max_pending_capabilities: usize,
    pub max_connections: usize,
    pub handshake_timeout: Duration,
    pub write_timeout: Duration,
    pub queue_stall_timeout: Duration,
    pub shutdown_grace: Duration,
    pub max_queued_events: usize,
    pub max_queued_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 0,
            capability_ttl: DEFAULT_CAPABILITY_TTL,
            max_pending_capabilities: DEFAULT_MAX_PENDING_CAPABILITIES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            write_timeout: DEFAULT_WRITE_TIMEOUT,
            queue_stall_timeout: DEFAULT_QUEUE_STALL_TIMEOUT,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            max_queued_events: DEFAULT_MAX_QUEUED_EVENTS,
            max_queued_bytes: DEFAULT_MAX_QUEUED_BYTES,
        }
    }
}

impl Config {
    fn validate(self) -> Result<Self, TransportError> {
        if self.capability_ttl.is_zero()
            || self.capability_ttl > MAX_CAPABILITY_TTL
            || self.max_pending_capabilities == 0
            || self.max_pending_capabilities > MAX_PENDING_CAPABILITIES
            || self.max_connections == 0
            || self.max_connections > MAX_CONNECTIONS
            || self.handshake_timeout.is_zero()
            || self.handshake_timeout > MAX_HANDSHAKE_TIMEOUT
            || self.write_timeout.is_zero()
            || self.write_timeout > MAX_WRITE_TIMEOUT
            || self.queue_stall_timeout.is_zero()
            || self.queue_stall_timeout > MAX_QUEUE_STALL_TIMEOUT
            || self.shutdown_grace.is_zero()
            || self.shutdown_grace > MAX_SHUTDOWN_GRACE
            || self.max_queued_events == 0
            || self.max_queued_events > MAX_QUEUED_EVENTS
            || self.max_queued_bytes < MAX_MESSAGE_BYTES
            || self.max_queued_bytes > MAX_QUEUED_BYTES
            || self.max_queued_bytes > u32::MAX as usize
        {
            return Err(TransportError::InvalidConfig);
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("WebSocket transport configuration is invalid")]
    InvalidConfig,
    #[error("event stream Origin policy is invalid")]
    InvalidOriginPolicy,
    #[error("failed to bind the loopback WebSocket listener")]
    Bind(#[source] std::io::Error),
    #[error("failed to inspect the loopback WebSocket listener")]
    LocalAddress(#[source] std::io::Error),
    #[error("the loopback WebSocket listener failed")]
    Accept(#[source] std::io::Error),
    #[error("the WebSocket capability registry is full")]
    CapabilityLimit,
    #[error("the WebSocket transport is shutting down")]
    ShuttingDown,
    #[error("the system clock cannot represent a safe capability expiry")]
    Clock,
    #[error("the WebSocket endpoint could not be represented safely")]
    Endpoint,
    #[error("the WebSocket accept task failed")]
    Task,
    #[error("the WebSocket transport did not shut down in time")]
    ShutdownTimedOut,
}

#[derive(Clone)]
pub(crate) struct Capability {
    pub(crate) session_id: SessionId,
    pub(crate) origin_policy: EventStreamOriginPolicy,
    pub(crate) expires_at_ms: u64,
}

pub(crate) struct AdmissionState {
    shutting_down: bool,
    capabilities: HashMap<String, Capability>,
}

pub(crate) struct State {
    pub(crate) events: Arc<MemoryEventStore>,
    pub(crate) stream_epoch: EventStreamEpoch,
    pub(crate) expected_host: String,
    pub(crate) admission: StdMutex<AdmissionState>,
    pub(crate) config: Config,
    pub(crate) active_connections: AtomicUsize,
    connections: Arc<Semaphore>,
}

/// Observable resource counts used by shutdown and leak tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportStats {
    pub active_connections: usize,
    pub pending_capabilities: usize,
    pub available_connection_permits: usize,
}

/// Running loopback event-stream listener.
pub struct EventStreamServer {
    address: SocketAddr,
    state: Arc<State>,
    admission_shutdown: watch::Sender<bool>,
    connection_shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), TransportError>>>,
}

impl EventStreamServer {
    /// Bind an IPv4 loopback listener and create a fresh daemon stream epoch.
    pub async fn bind(
        events: Arc<MemoryEventStore>,
        config: Config,
    ) -> Result<Self, TransportError> {
        let config = config.validate()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
            .await
            .map_err(TransportError::Bind)?;
        let address = listener
            .local_addr()
            .map_err(TransportError::LocalAddress)?;
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
            return Err(TransportError::Bind(std::io::Error::other(
                "listener did not bind IPv4 loopback",
            )));
        }

        let state = Arc::new(State {
            events,
            stream_epoch: EventStreamEpoch::new(),
            expected_host: format!("127.0.0.1:{}", address.port()),
            admission: StdMutex::new(AdmissionState {
                shutting_down: false,
                capabilities: HashMap::new(),
            }),
            config,
            active_connections: AtomicUsize::new(0),
            connections: Arc::new(Semaphore::new(config.max_connections)),
        });
        let (admission_shutdown, admission_receiver) = watch::channel(false);
        let (connection_shutdown, _) = watch::channel(false);
        let task = tokio::spawn(accept_loop(
            listener,
            Arc::clone(&state),
            admission_receiver,
            connection_shutdown.clone(),
        ));
        Ok(Self {
            address,
            state,
            admission_shutdown,
            connection_shutdown,
            task: Some(task),
        })
    }

    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn stream_epoch(&self) -> EventStreamEpoch {
        self.state.stream_epoch
    }

    /// Whether new connections can still be advertised and admitted.
    pub fn is_accepting(&self) -> bool {
        let admission = self
            .state
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !admission.shutting_down && self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Issue a short-lived, one-shot, Session-scoped bearer endpoint.
    pub fn open(
        &self,
        params: EventsStreamOpenParams,
    ) -> Result<EventsStreamOpenResult, TransportError> {
        validate_origin_policy(&params.origin_policy)?;
        let now_ms = unix_time_ms()?;
        let expires_at_ms = now_ms
            .checked_add(
                u64::try_from(self.state.config.capability_ttl.as_millis())
                    .map_err(|_| TransportError::Clock)?,
            )
            .filter(|value| *value <= MAX_SAFE_INTEGER)
            .ok_or(TransportError::Clock)?;
        let mut admission = self
            .state
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if admission.shutting_down {
            return Err(TransportError::ShuttingDown);
        }
        admission
            .capabilities
            .retain(|_, capability| capability.expires_at_ms > now_ms);
        if admission.capabilities.len() >= self.state.config.max_pending_capabilities {
            return Err(TransportError::CapabilityLimit);
        }

        let token = loop {
            // Two v4 UUIDs carry 244 random bits and never enter Debug output.
            let candidate = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            if !admission.capabilities.contains_key(&candidate) {
                break candidate;
            }
        };
        debug_assert_eq!(token.len(), CAPABILITY_HEX_LENGTH);
        admission.capabilities.insert(
            token.clone(),
            Capability {
                session_id: params.session_id,
                origin_policy: params.origin_policy,
                expires_at_ms,
            },
        );
        let endpoint =
            EventStreamEndpoint::new(format!("ws://{}/v/{token}", self.state.expected_host))
                .ok_or(TransportError::Endpoint)?;
        Ok(EventsStreamOpenResult {
            endpoint,
            stream_epoch: self.state.stream_epoch,
            expires_at_ms,
        })
    }

    pub fn stats(&self) -> TransportStats {
        let pending_capabilities = self
            .state
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .capabilities
            .len();
        TransportStats {
            active_connections: self.state.active_connections.load(Ordering::SeqCst),
            pending_capabilities,
            available_connection_permits: self.state.connections.available_permits(),
        }
    }

    /// Stop new admission. This method is idempotent and non-blocking.
    pub fn begin_shutdown(&self) {
        close_admission(&self.state);
        self.admission_shutdown.send_replace(true);
    }

    /// Signal Core and active sockets, then wait for every accepted resource.
    /// Callers first invoke [`Self::begin_shutdown`], drain runtime operations,
    /// and only then call this method so final Session events remain visible.
    pub async fn finish_shutdown(&mut self) -> Result<(), TransportError> {
        let started = Instant::now();
        let shutdown_deadline = started + self.state.config.shutdown_grace;
        let natural_drain_deadline = started + self.state.config.shutdown_grace / 2;
        self.begin_shutdown();
        if self.task.is_none() {
            return shutdown_core_until(&self.state.events, shutdown_deadline).await;
        }

        let natural = {
            let task = self.task.as_mut().expect("task presence was checked");
            time::timeout_at(natural_drain_deadline, task).await
        };
        match natural {
            Ok(result) => {
                self.task.take();
                shutdown_core_until(&self.state.events, shutdown_deadline).await?;
                accept_task_result(result)
            }
            Err(_) => {
                // Keep Core open during the natural phase: an ended Session's
                // ordered event must be followed by `sessionEnded`, not raced
                // by a later global StoreShutdown signal. After that bounded
                // window, force all remaining resources within the same
                // overall deadline.
                if let Err(error) = shutdown_core_until(&self.state.events, shutdown_deadline).await
                {
                    abort_accept_task(self.task.as_mut().expect("task remains owned")).await;
                    self.task.take();
                    return Err(error);
                }
                self.connection_shutdown.send_replace(true);
                let result = force_accept_task(
                    self.task.as_mut().expect("task remains owned"),
                    shutdown_deadline,
                )
                .await;
                self.task.take();
                result
            }
        }
    }
}

async fn shutdown_core_until(
    events: &MemoryEventStore,
    shutdown_deadline: Instant,
) -> Result<(), TransportError> {
    time::timeout_at(shutdown_deadline, events.begin_stream_shutdown())
        .await
        .map(|_| ())
        .map_err(|_| TransportError::ShutdownTimedOut)
}

async fn force_accept_task(
    task: &mut JoinHandle<Result<(), TransportError>>,
    shutdown_deadline: Instant,
) -> Result<(), TransportError> {
    match time::timeout_at(shutdown_deadline, &mut *task).await {
        Ok(result) => accept_task_result(result),
        Err(_) => {
            abort_accept_task(task).await;
            Err(TransportError::ShutdownTimedOut)
        }
    }
}

async fn abort_accept_task(task: &mut JoinHandle<Result<(), TransportError>>) {
    task.abort();
    let _ = task.await;
}

fn accept_task_result(
    result: Result<Result<(), TransportError>, tokio::task::JoinError>,
) -> Result<(), TransportError> {
    match result {
        Ok(result) => result,
        Err(_) => Err(TransportError::Task),
    }
}

impl Drop for EventStreamServer {
    fn drop(&mut self) {
        self.begin_shutdown();
        self.connection_shutdown.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct ConnectionGuard(Arc<State>);

impl ConnectionGuard {
    fn new(state: Arc<State>) -> Self {
        state.active_connections.fetch_add(1, Ordering::SeqCst);
        Self(state)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.active_connections.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<State>,
    mut admission_shutdown: watch::Receiver<bool>,
    connection_shutdown: watch::Sender<bool>,
) -> Result<(), TransportError> {
    let mut tasks = JoinSet::new();
    let mut terminal_error = None;
    loop {
        tokio::select! {
            biased;
            changed = admission_shutdown.changed() => {
                if changed.is_err() || *admission_shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        close_admission(&state);
                        connection_shutdown.send_replace(true);
                        terminal_error = Some(TransportError::Accept(error));
                        break;
                    }
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&state.connections).try_acquire_owned() else {
                    continue;
                };
                let state = Arc::clone(&state);
                let receiver = connection_shutdown.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    let _guard = ConnectionGuard::new(Arc::clone(&state));
                    connection::handle_connection(stream, state, receiver).await;
                });
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    drop(listener);
    while tasks.join_next().await.is_some() {}
    terminal_error.map_or(Ok(()), Err)
}

fn close_admission(state: &State) {
    let mut admission = state
        .admission
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    admission.shutting_down = true;
    admission.capabilities.clear();
}

fn validate_origin_policy(policy: &EventStreamOriginPolicy) -> Result<(), TransportError> {
    match policy {
        EventStreamOriginPolicy::Absent {} => Ok(()),
        EventStreamOriginPolicy::Exact { origin } => {
            let prefix = if origin.starts_with("http://127.0.0.1:") {
                "http://127.0.0.1:"
            } else if origin.starts_with("https://127.0.0.1:") {
                "https://127.0.0.1:"
            } else {
                return Err(TransportError::InvalidOriginPolicy);
            };
            let port = &origin[prefix.len()..];
            let parsed_port = port.parse::<u16>().ok().filter(|value| *value != 0);
            if origin.len() > 256
                || !origin.is_ascii()
                || port.is_empty()
                || (port.len() > 1 && port.starts_with('0'))
                || !port.bytes().all(|byte| byte.is_ascii_digit())
                || parsed_port.is_none()
                || (prefix == "http://127.0.0.1:" && parsed_port == Some(80))
                || (prefix == "https://127.0.0.1:" && parsed_port == Some(443))
            {
                return Err(TransportError::InvalidOriginPolicy);
            }
            Ok(())
        }
    }
}

pub(crate) fn unix_time_ms() -> Result<u64, TransportError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TransportError::Clock)?
        .as_millis();
    u64::try_from(millis)
        .ok()
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or(TransportError::Clock)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

    use devicerail_core::MemoryEventStore;
    use devicerail_protocol::{
        EventStreamEpoch, EventStreamOriginPolicy, EventsStreamOpenParams, SessionId,
    };
    use tokio::sync::{Semaphore, watch};

    use super::{
        AdmissionState, Config, EventStreamServer, State, TransportError, close_admission,
        force_accept_task,
    };

    fn state(config: Config) -> Arc<State> {
        Arc::new(State {
            events: Arc::new(MemoryEventStore::default()),
            stream_epoch: EventStreamEpoch::new(),
            expected_host: "127.0.0.1:43123".to_owned(),
            admission: std::sync::Mutex::new(AdmissionState {
                shutting_down: false,
                capabilities: HashMap::new(),
            }),
            config,
            active_connections: std::sync::atomic::AtomicUsize::new(0),
            connections: Arc::new(Semaphore::new(config.max_connections)),
        })
    }

    fn server_without_listener(config: Config) -> EventStreamServer {
        let (admission_shutdown, _) = watch::channel(false);
        let (connection_shutdown, _) = watch::channel(false);
        EventStreamServer {
            address: "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("fixed socket address"),
            state: state(config),
            admission_shutdown,
            connection_shutdown,
            task: None,
        }
    }

    #[test]
    fn begin_shutdown_linearizes_with_capability_admission() {
        let server = server_without_listener(Config::default());
        server
            .open(EventsStreamOpenParams {
                session_id: SessionId::new(),
                origin_policy: EventStreamOriginPolicy::Absent {},
            })
            .expect("admission is initially open");
        assert_eq!(server.stats().pending_capabilities, 1);

        server.begin_shutdown();
        assert_eq!(server.stats().pending_capabilities, 0);
        assert!(matches!(
            server.open(EventsStreamOpenParams {
                session_id: SessionId::new(),
                origin_policy: EventStreamOriginPolicy::Absent {},
            }),
            Err(TransportError::ShuttingDown)
        ));
    }

    #[tokio::test]
    async fn shutdown_uses_natural_phase_before_forcing_remaining_tasks() {
        let config = Config {
            shutdown_grace: Duration::from_millis(40),
            ..Config::default()
        };
        let (admission_shutdown, _) = watch::channel(false);
        let (connection_shutdown, mut connection_receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            connection_receiver
                .changed()
                .await
                .expect("force sender remains alive");
            assert!(*connection_receiver.borrow());
            Ok(())
        });
        let mut server = EventStreamServer {
            address: "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("fixed socket address"),
            state: state(config),
            admission_shutdown,
            connection_shutdown,
            task: Some(task),
        };

        server
            .finish_shutdown()
            .await
            .expect("forced phase completes within the same grace");
        assert!(*server.connection_shutdown.borrow());
    }

    #[tokio::test]
    async fn final_force_deadline_aborts_an_unresponsive_task() {
        let mut task = tokio::spawn(async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });
        assert!(matches!(
            force_accept_task(
                &mut task,
                tokio::time::Instant::now() + Duration::from_millis(10)
            )
            .await,
            Err(TransportError::ShutdownTimedOut)
        ));
    }

    #[tokio::test]
    async fn cancelled_finish_retains_ownership_of_the_accept_task() {
        let config = Config {
            shutdown_grace: Duration::from_millis(100),
            ..Config::default()
        };
        let (admission_shutdown, _) = watch::channel(false);
        let (connection_shutdown, mut connection_receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            connection_receiver
                .changed()
                .await
                .expect("connection shutdown sender remains alive");
            Ok(())
        });
        let mut server = EventStreamServer {
            address: "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("fixed socket address"),
            state: state(config),
            admission_shutdown,
            connection_shutdown,
            task: Some(task),
        };

        assert!(
            tokio::time::timeout(Duration::from_millis(5), server.finish_shutdown())
                .await
                .is_err(),
            "the first graceful wait is deliberately cancelled"
        );
        assert!(
            server.task.is_some(),
            "cancelling finish must not detach the accept task"
        );

        server.connection_shutdown.send_replace(true);
        server
            .finish_shutdown()
            .await
            .expect("a second finish reclaims the still-owned task");
        assert!(server.task.is_none());
    }

    #[tokio::test]
    async fn fatal_accept_state_stops_capabilities_and_persists_connection_shutdown() {
        let config = Config::default();
        let state = state(config);
        let (admission_shutdown, _) = watch::channel(false);
        let (connection_shutdown, _) = watch::channel(false);
        let task = tokio::spawn(async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });
        let server = EventStreamServer {
            address: "127.0.0.1:43123"
                .parse::<SocketAddr>()
                .expect("fixed socket address"),
            state: Arc::clone(&state),
            admission_shutdown,
            connection_shutdown,
            task: Some(task),
        };
        server
            .open(EventsStreamOpenParams {
                session_id: SessionId::new(),
                origin_policy: EventStreamOriginPolicy::Absent {},
            })
            .expect("pre-failure capability");

        close_admission(&state);
        server.connection_shutdown.send_replace(true);
        let receiver = server.connection_shutdown.subscribe();
        assert!(
            *receiver.borrow(),
            "late subscribers observe the fatal state"
        );
        assert!(!server.is_accepting());
        assert_eq!(server.stats().pending_capabilities, 0);
        assert!(matches!(
            server.open(EventsStreamOpenParams {
                session_id: SessionId::new(),
                origin_policy: EventStreamOriginPolicy::Absent {},
            }),
            Err(TransportError::ShuttingDown)
        ));
    }
}
