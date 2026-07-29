use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use devicerail_protocol::{
    ErrorInfo, EventId, EventSequence, MAX_EVENTS_LIST_PAGE_SIZE, MediaStreamId, Observation,
    RpcId, SessionExport, SessionId, SessionInfo, SessionState, TestEvent, TestEventPayload,
};
use serde_json::json;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, broadcast};
use uuid::Uuid;

use crate::{
    event_stream::{EventStreamConfig, EventStreamError, EventStreamSignal, EventSubscription},
    session::{EndSession, PendingEvent, StartSession},
};

pub type EventStoreResult<T> = Result<T, EventStoreError>;

/// One atomically captured, reference-counted page of an ended Session.
///
/// The Store clones only the bounded vector of [`Arc`] handles while holding
/// its state lock. Transport adapters can size and serialize the referenced
/// events after the lock has been released.
#[derive(Clone, Debug)]
pub struct SessionExportPageSnapshot {
    pub session: SessionInfo,
    pub events: Vec<Arc<TestEvent>>,
}

/// Opaque token for one in-flight observation operation.
///
/// Observation leases are Event Store bookkeeping only. Reserving or
/// releasing one never appends a wire [`TestEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObservationLease(Uuid);

impl ObservationLease {
    /// Creates a fresh lease token for a [`SessionEventStore`]
    /// implementation.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn id(self) -> Uuid {
        self.0
    }
}

impl Default for ObservationLease {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EventStoreError {
    #[error("session already exists: {0}")]
    SessionAlreadyExists(SessionId),
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("session is already ended: {0}")]
    SessionEnded(SessionId),
    #[error("active session cannot be deleted: {0}")]
    SessionActive(SessionId),
    #[error("session {session_id} still has {count} action(s) in flight")]
    ActionsInFlight { session_id: SessionId, count: usize },
    #[error("session {session_id} still has {count} observation(s) in flight")]
    ObservationsInFlight { session_id: SessionId, count: usize },
    #[error("observation lease {lease_id} does not belong to session {session_id}")]
    InvalidObservationLease {
        session_id: SessionId,
        lease_id: Uuid,
    },
    #[error("session lifecycle events can only be written by start_session/end_session")]
    LifecycleEventReserved,
    #[error("action call id is already used in this session: {0}")]
    DuplicateActionCall(Uuid),
    #[error("action was not started in this session: {0}")]
    ActionNotStarted(Uuid),
    #[error("action event correlation changed for call {0}")]
    ActionCorrelationMismatch(Uuid),
    #[error("successful action result call id does not match completed call {0}")]
    ActionResultMismatch(Uuid),
    #[error("media stream id is already used: {0}")]
    DuplicateMediaStream(MediaStreamId),
    #[error("media stream declaration is invalid")]
    InvalidMediaStream,
    #[error("media stream is not active: {0}")]
    MediaStreamNotActive(MediaStreamId),
    #[error("media stream frame is out of order")]
    MediaFrameOutOfOrder,
    #[error("media stream frame does not match its declared stream")]
    MediaFrameMismatch,
    #[error("media stream ended with an inconsistent frame count")]
    MediaFrameCountMismatch,
    #[error("session {session_id} still has {count} media stream(s) in flight")]
    MediaStreamsInFlight { session_id: SessionId, count: usize },
    #[error("event sequence exhausted for session: {0}")]
    SequenceExhausted(SessionId),
    #[error(
        "session {session_id} export cursor {after:?} is ahead of last sequence {last_sequence:?}"
    )]
    CursorAhead {
        session_id: SessionId,
        after: EventSequence,
        last_sequence: EventSequence,
    },
    #[error("event page limit {limit} is outside 1..={maximum}")]
    InvalidPageLimit { limit: usize, maximum: usize },
    #[error("event store is shutting down")]
    ShuttingDown,
    #[error("timestamp exceeds the cross-language safe integer limit")]
    TimestampOutOfRange,
    #[error("event store failed: {0}")]
    Internal(String),
}

impl EventStoreError {
    pub fn to_error_info(&self) -> ErrorInfo {
        let (code, retryable, details) = match self {
            Self::SessionAlreadyExists(session_id) => (
                "session_already_exists",
                false,
                Some(json!({ "sessionId": session_id })),
            ),
            Self::SessionNotFound(session_id) => (
                "session_not_found",
                false,
                Some(json!({ "sessionId": session_id })),
            ),
            Self::SessionEnded(session_id) => (
                "session_ended",
                false,
                Some(json!({ "sessionId": session_id })),
            ),
            Self::SessionActive(session_id) => (
                "session_active",
                false,
                Some(json!({ "sessionId": session_id })),
            ),
            Self::ActionsInFlight { session_id, count } => (
                "session_busy",
                true,
                Some(json!({ "sessionId": session_id, "inFlightActions": count })),
            ),
            Self::ObservationsInFlight { session_id, count } => (
                "session_busy",
                true,
                Some(json!({
                    "sessionId": session_id,
                    "inFlightObservations": count
                })),
            ),
            Self::InvalidObservationLease {
                session_id,
                lease_id,
            } => (
                "invalid_observation_lease",
                false,
                Some(json!({ "sessionId": session_id, "leaseId": lease_id })),
            ),
            Self::LifecycleEventReserved => ("invalid_event", false, None),
            Self::DuplicateActionCall(call_id) => (
                "duplicate_action_call",
                false,
                Some(json!({ "callId": call_id })),
            ),
            Self::ActionNotStarted(call_id) => (
                "action_not_started",
                false,
                Some(json!({ "callId": call_id })),
            ),
            Self::ActionCorrelationMismatch(call_id) => (
                "action_correlation_mismatch",
                false,
                Some(json!({ "callId": call_id })),
            ),
            Self::ActionResultMismatch(call_id) => (
                "action_result_mismatch",
                false,
                Some(json!({ "callId": call_id })),
            ),
            Self::DuplicateMediaStream(stream_id) => (
                "duplicate_media_stream",
                false,
                Some(json!({ "streamId": stream_id })),
            ),
            Self::InvalidMediaStream => ("invalid_media_stream", false, None),
            Self::MediaStreamNotActive(stream_id) => (
                "media_stream_not_active",
                false,
                Some(json!({ "streamId": stream_id })),
            ),
            Self::MediaFrameOutOfOrder => ("media_frame_out_of_order", false, None),
            Self::MediaFrameMismatch => ("media_frame_mismatch", false, None),
            Self::MediaFrameCountMismatch => ("media_frame_count_mismatch", false, None),
            Self::MediaStreamsInFlight { session_id, count } => (
                "session_busy",
                true,
                Some(json!({ "sessionId": session_id, "inFlightMediaStreams": count })),
            ),
            Self::SequenceExhausted(session_id) => (
                "event_sequence_exhausted",
                false,
                Some(json!({ "sessionId": session_id })),
            ),
            Self::CursorAhead {
                session_id,
                after,
                last_sequence,
            } => (
                "event_cursor_ahead",
                false,
                Some(json!({
                    "sessionId": session_id,
                    "afterSequence": after,
                    "lastSequence": last_sequence
                })),
            ),
            Self::InvalidPageLimit { limit, maximum } => (
                "invalid_page_limit",
                false,
                Some(json!({ "limit": limit, "maximum": maximum })),
            ),
            Self::ShuttingDown => ("event_store_shutting_down", false, None),
            Self::TimestampOutOfRange => ("timestamp_out_of_range", false, None),
            Self::Internal(_) => ("event_store_error", true, None),
        };

        ErrorInfo {
            code: code.to_owned(),
            message: self.to_string(),
            retryable,
            details,
        }
    }
}

#[async_trait]
pub trait SessionEventStore: Send + Sync {
    /// Starts a lifetime-unique Session. Implementations must reject every
    /// previously used `SessionId`, even after its ended log was deleted.
    async fn start_session(&self, command: StartSession) -> EventStoreResult<SessionInfo>;

    /// Atomically verifies that `session_id` is active and reserves one
    /// observation operation. This bookkeeping must not append a wire event.
    async fn reserve_observation(
        &self,
        session_id: &SessionId,
    ) -> EventStoreResult<ObservationLease>;

    /// Releases a previously reserved observation. Implementations must
    /// verify both the session and the opaque lease token. Releasing the same
    /// valid `(session_id, lease)` pair is idempotent while that Session record
    /// exists, including after a prior successful release. After an error, the
    /// same valid pair must remain safely retryable: an implementation either
    /// keeps it active or remembers that release took effect so a retry
    /// succeeds. Unknown tokens and tokens paired with the wrong Session return
    /// [`EventStoreError::InvalidObservationLease`].
    async fn release_observation(
        &self,
        session_id: &SessionId,
        lease: ObservationLease,
    ) -> EventStoreResult<()>;

    /// Appends one validated event. An exact retry of a media lifecycle event
    /// whose prior acknowledgement may have been lost must return the original
    /// event without allocating a new sequence or publishing it again.
    async fn append(&self, event: PendingEvent) -> EventStoreResult<TestEvent>;

    async fn end_session(&self, command: EndSession) -> EventStoreResult<SessionInfo>;

    async fn list_after(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
    ) -> EventStoreResult<Vec<TestEvent>>;

    /// Returns at most `limit` events after `after` while preserving sequence
    /// order. Stores should override this method so the bound is applied before
    /// cloning or reading the complete suffix.
    async fn list_page(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
        limit: usize,
    ) -> EventStoreResult<Vec<TestEvent>> {
        let mut events = self.list_after(session_id, after).await?;
        events.truncate(limit);
        Ok(events)
    }

    async fn list_sessions(&self) -> EventStoreResult<Vec<SessionInfo>>;

    async fn export_session(&self, session_id: &SessionId) -> EventStoreResult<SessionExport>;

    /// Returns one immutable page of an ended Session export without deeply
    /// cloning event bodies while the Store is locked.
    async fn export_session_page(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
        limit: usize,
    ) -> EventStoreResult<SessionExportPageSnapshot>;

    /// Deletes one complete ended session. Individual events are never
    /// cleared or rewritten, and the Session's used-ID tombstone remains.
    async fn delete_ended(&self, session_id: &SessionId) -> EventStoreResult<()>;
}

fn export_page_range(
    session: &SessionInfo,
    event_count: usize,
    after: Option<EventSequence>,
    limit: usize,
) -> EventStoreResult<std::ops::Range<usize>> {
    let maximum = MAX_EVENTS_LIST_PAGE_SIZE as usize;
    if !(1..=maximum).contains(&limit) {
        return Err(EventStoreError::InvalidPageLimit { limit, maximum });
    }
    if session.state != SessionState::Ended {
        return Err(EventStoreError::SessionActive(session.id.clone()));
    }
    if let Some(after) = after
        && after > session.last_sequence
    {
        return Err(EventStoreError::CursorAhead {
            session_id: session.id.clone(),
            after,
            last_sequence: session.last_sequence,
        });
    }

    let declared_count = usize::try_from(session.event_count.get()).map_err(|_| {
        EventStoreError::Internal(format!(
            "session {} event count does not fit this platform",
            session.id
        ))
    })?;
    if declared_count != event_count || session.event_count != session.last_sequence {
        return Err(EventStoreError::Internal(format!(
            "session {} metadata does not match its append-only log",
            session.id
        )));
    }
    let start = after.map_or(Ok(0), |sequence| {
        usize::try_from(sequence.get()).map_err(|_| {
            EventStoreError::Internal(format!(
                "session {} export cursor does not fit this platform",
                session.id
            ))
        })
    })?;
    let end = start.saturating_add(limit).min(event_count);
    Ok(start..end)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActionCorrelation {
    request_id: Option<RpcId>,
    device_id: Option<devicerail_protocol::DeviceId>,
}

#[derive(Debug)]
struct SessionLog {
    info: SessionInfo,
    events: Vec<Arc<TestEvent>>,
    stream_tail: broadcast::Sender<EventStreamSignal>,
    seen_calls: HashSet<Uuid>,
    in_flight: HashMap<Uuid, ActionCorrelation>,
    in_flight_observations: HashSet<ObservationLease>,
    seen_media_streams: HashSet<MediaStreamId>,
    media_streams: HashMap<MediaStreamId, MediaStreamState>,
}

#[derive(Debug)]
struct MediaStreamState {
    media_type: String,
    next_frame_index: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ObservationLeaseState {
    session_id: SessionId,
    released: bool,
}

#[derive(Debug, Default)]
struct MemoryEventStoreState {
    sessions: BTreeMap<SessionId, Arc<Mutex<SessionLog>>>,
    used_session_ids: HashSet<SessionId>,
    observation_leases: HashMap<ObservationLease, ObservationLeaseState>,
}

#[derive(Debug)]
pub struct MemoryEventStore {
    state: RwLock<MemoryEventStoreState>,
    stream_shutdown: AtomicBool,
    stream_config: EventStreamConfig,
}

impl Default for MemoryEventStore {
    fn default() -> Self {
        Self::with_event_stream_config(EventStreamConfig::default())
    }
}

impl MemoryEventStore {
    pub fn with_event_stream_config(stream_config: EventStreamConfig) -> Self {
        Self {
            state: RwLock::new(MemoryEventStoreState::default()),
            stream_shutdown: AtomicBool::new(false),
            stream_config,
        }
    }

    /// Looks up one Observation without cloning the Session's complete event
    /// prefix. The lookup remains scoped to the caller-selected Session and
    /// clones only the matched Observation after the per-Session lock is held.
    pub async fn observation_by_id(
        &self,
        session_id: &SessionId,
        observation_id: Uuid,
    ) -> EventStoreResult<Option<Observation>> {
        let log = {
            let state = self.state.read().await;
            state
                .sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?
        };
        let log = log.lock().await;

        let mut matched = None;
        let mut consider = |observation: &Observation| -> EventStoreResult<()> {
            if observation.id != observation_id {
                return Ok(());
            }
            if matched.is_some() {
                return Err(EventStoreError::Internal(format!(
                    "session {session_id} contains a duplicate observation id"
                )));
            }
            matched = Some(observation.clone());
            Ok(())
        };
        for event in &log.events {
            match &event.payload {
                TestEventPayload::ObservationCaptured { observation } => consider(observation)?,
                TestEventPayload::ActionCompleted {
                    outcome: devicerail_protocol::ActionOutcome::Succeeded { result },
                    ..
                } => {
                    if let Some(before) = &result.before {
                        consider(before)?;
                    }
                    if let Some(after) = &result.after {
                        consider(after)?;
                    }
                }
                _ => {}
            }
        }
        Ok(matched)
    }

    /// Returns the first requested Evidence reference that is not already
    /// reachable from this Session's durable event log. The log is scanned
    /// once regardless of the number of Verdict references, event bodies are
    /// not cloned, and input order determines which missing reference wins.
    pub async fn first_unreachable_asset_reference(
        &self,
        session_id: &SessionId,
        assets: &[devicerail_protocol::AssetRef],
    ) -> EventStoreResult<Option<devicerail_protocol::AssetRef>> {
        let log = {
            let state = self.state.read().await;
            state
                .sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?
        };
        let log = log.lock().await;
        let mut missing = assets.iter().cloned().collect::<HashSet<_>>();
        for event in &log.events {
            let mut remove = |reference: &devicerail_protocol::AssetRef| {
                missing.remove(reference);
            };
            match &event.payload {
                TestEventPayload::ObservationCaptured { observation } => {
                    observation.asset_refs().for_each(&mut remove);
                }
                TestEventPayload::ActionCompleted {
                    outcome: devicerail_protocol::ActionOutcome::Succeeded { result },
                    ..
                } => result.asset_refs().for_each(&mut remove),
                TestEventPayload::MediaFrameCaptured { frame } => {
                    remove(&frame.evidence);
                }
                TestEventPayload::VerdictRecorded { verdict } => {
                    verdict.evidence.iter().for_each(&mut remove);
                }
                _ => {}
            }
            if missing.is_empty() {
                return Ok(None);
            }
        }
        Ok(assets
            .iter()
            .find(|asset| missing.contains(*asset))
            .cloned())
    }

    /// Atomically captures the suffix after `after` and registers for every
    /// later event before releasing the Event Store state lock.
    pub async fn subscribe_after(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
    ) -> Result<EventSubscription, EventStreamError> {
        let state = self.state.read().await;
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStreamError::StoreShutdown);
        }
        let Some(log) = state.sessions.get(session_id).cloned() else {
            return if state.used_session_ids.contains(session_id) {
                Err(EventStreamError::SessionDeleted(session_id.clone()))
            } else {
                Err(EventStreamError::SessionNotFound(session_id.clone()))
            };
        };
        // Holding the map read lock until the per-Session lock is acquired
        // prevents delete from overtaking subscription registration.
        let log = log.lock().await;
        drop(state);

        if let Some(after) = after
            && after > log.info.last_sequence
        {
            return Err(EventStreamError::CursorAhead {
                session_id: session_id.clone(),
                after,
                last_sequence: log.info.last_sequence,
            });
        }

        let event_count = usize::try_from(log.info.last_sequence.get()).map_err(|_| {
            EventStreamError::Internal(format!(
                "session {session_id} event count does not fit this platform"
            ))
        })?;
        if event_count != log.events.len() {
            return Err(EventStreamError::Internal(format!(
                "session {session_id} event count does not match its append-only log"
            )));
        }
        let has_terminal_event = log
            .events
            .last()
            .is_some_and(|event| matches!(event.payload, TestEventPayload::SessionEnded { .. }));
        if has_terminal_event != (log.info.state == SessionState::Ended) {
            return Err(EventStreamError::Internal(format!(
                "session {session_id} state does not match its terminal event"
            )));
        }
        let start = after.map_or(Ok(0), |sequence| {
            usize::try_from(sequence.get()).map_err(|_| {
                EventStreamError::Internal(format!(
                    "session {session_id} cursor does not fit this platform"
                ))
            })
        })?;
        let requested = log.events.len().checked_sub(start).ok_or_else(|| {
            EventStreamError::Internal(format!(
                "session {session_id} cursor is inconsistent with its log"
            ))
        })?;
        if requested > self.stream_config.max_replay_events() {
            return Err(EventStreamError::ReplayLimitExceeded {
                session_id: session_id.clone(),
                requested,
                maximum: self.stream_config.max_replay_events(),
            });
        }
        if log.stream_tail.receiver_count() >= self.stream_config.max_subscribers_per_session() {
            return Err(EventStreamError::TooManySubscribers {
                session_id: session_id.clone(),
                maximum: self.stream_config.max_subscribers_per_session(),
            });
        }

        // Receiver registration and snapshot capture share the same state
        // lock as append/end/delete, which is the snapshot-to-tail
        // linearization point.
        let tail = log.stream_tail.subscribe();
        let mut snapshot = VecDeque::new();
        snapshot.try_reserve(requested).map_err(|error| {
            EventStreamError::Internal(format!("failed to reserve event replay snapshot: {error}"))
        })?;
        snapshot.extend(log.events[start..].iter().cloned());
        Ok(EventSubscription::new(
            session_id.clone(),
            snapshot,
            tail,
            after,
            log.info.last_sequence,
            log.info.state,
        ))
    }

    /// Closes the transport-neutral stream hub exactly once.
    ///
    /// Callers must drain runtime operations and end the active Session first.
    /// Subsequent Session mutations and subscriptions fail explicitly, while
    /// reads, lease finalization, and ended-Session deletion remain available.
    pub async fn begin_stream_shutdown(&self) -> bool {
        if self.stream_shutdown.swap(true, Ordering::AcqRel) {
            return false;
        }
        let state = self.state.read().await;
        for log in state.sessions.values() {
            let log = log.lock().await;
            let _ = log.stream_tail.send(EventStreamSignal::StoreShutdown);
        }
        true
    }

    fn next_sequence(log: &SessionLog) -> EventStoreResult<EventSequence> {
        log.info
            .last_sequence
            .checked_next()
            .ok_or_else(|| EventStoreError::SequenceExhausted(log.info.id.clone()))
    }

    fn ensure_timestamp(at_ms: u64) -> EventStoreResult<()> {
        if at_ms <= devicerail_protocol::MAX_SAFE_INTEGER {
            Ok(())
        } else {
            Err(EventStoreError::TimestampOutOfRange)
        }
    }

    fn update_info_after_append(log: &mut SessionLog, sequence: EventSequence) {
        log.info.event_count = sequence;
        log.info.last_sequence = sequence;
    }

    fn publish_event(log: &SessionLog, event: &Arc<TestEvent>) {
        let _ = log
            .stream_tail
            .send(EventStreamSignal::Event(Arc::clone(event)));
    }

    fn exact_media_retry(log: &SessionLog, pending: &PendingEvent) -> Option<TestEvent> {
        log.events
            .iter()
            .rev()
            .find(|event| {
                event.request_id == pending.request_id
                    && event.device_id == pending.device_id
                    && event.at_ms == pending.at_ms
                    && event.payload == pending.payload
            })
            .map(|event| event.as_ref().clone())
    }
}

#[async_trait]
impl SessionEventStore for MemoryEventStore {
    async fn start_session(&self, command: StartSession) -> EventStoreResult<SessionInfo> {
        Self::ensure_timestamp(command.at_ms)?;
        let mut state = self.state.write().await;
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStoreError::ShuttingDown);
        }
        if !state.used_session_ids.insert(command.session_id.clone()) {
            return Err(EventStoreError::SessionAlreadyExists(command.session_id));
        }

        let info = SessionInfo {
            id: command.session_id.clone(),
            state: SessionState::Active,
            started_at_ms: command.at_ms,
            ended_at_ms: None,
            event_count: EventSequence::FIRST,
            last_sequence: EventSequence::FIRST,
        };
        let event = TestEvent {
            event_id: EventId::new(),
            session_id: command.session_id.clone(),
            sequence: EventSequence::FIRST,
            request_id: command.request_id,
            device_id: command.device_id,
            at_ms: command.at_ms,
            payload: TestEventPayload::SessionStarted,
        };
        let (stream_tail, initial_receiver) =
            broadcast::channel(self.stream_config.tail_capacity());
        drop(initial_receiver);
        state.sessions.insert(
            command.session_id,
            Arc::new(Mutex::new(SessionLog {
                info: info.clone(),
                events: vec![Arc::new(event)],
                stream_tail,
                seen_calls: HashSet::new(),
                in_flight: HashMap::new(),
                in_flight_observations: HashSet::new(),
                seen_media_streams: HashSet::new(),
                media_streams: HashMap::new(),
            })),
        );
        Ok(info)
    }

    async fn reserve_observation(
        &self,
        session_id: &SessionId,
    ) -> EventStoreResult<ObservationLease> {
        let mut state = self.state.write().await;
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStoreError::ShuttingDown);
        }
        let log = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?;
        let mut log = log.lock().await;
        if log.info.state == SessionState::Ended {
            return Err(EventStoreError::SessionEnded(session_id.clone()));
        }

        let lease = loop {
            let lease = ObservationLease::new();
            if !state.observation_leases.contains_key(&lease) {
                break lease;
            }
        };
        log.in_flight_observations.insert(lease);
        state.observation_leases.insert(
            lease,
            ObservationLeaseState {
                session_id: session_id.clone(),
                released: false,
            },
        );
        Ok(lease)
    }

    async fn release_observation(
        &self,
        session_id: &SessionId,
        lease: ObservationLease,
    ) -> EventStoreResult<()> {
        let mut state = self.state.write().await;
        let Some(lease_state) = state.observation_leases.get(&lease) else {
            return Err(EventStoreError::InvalidObservationLease {
                session_id: session_id.clone(),
                lease_id: lease.id(),
            });
        };
        if lease_state.session_id != *session_id {
            return Err(EventStoreError::InvalidObservationLease {
                session_id: session_id.clone(),
                lease_id: lease.id(),
            });
        }
        if lease_state.released {
            return Ok(());
        }

        let Some(log) = state.sessions.get(session_id).cloned() else {
            return Err(EventStoreError::Internal(
                "active observation lease has no Session log".to_owned(),
            ));
        };
        let mut log = log.lock().await;
        if !log.in_flight_observations.remove(&lease) {
            return Err(EventStoreError::Internal(
                "active observation lease is missing from Session bookkeeping".to_owned(),
            ));
        }
        let Some(lease_state) = state.observation_leases.get_mut(&lease) else {
            return Err(EventStoreError::Internal(
                "validated observation lease disappeared while the state lock was held".to_owned(),
            ));
        };
        lease_state.released = true;
        Ok(())
    }

    async fn append(&self, pending: PendingEvent) -> EventStoreResult<TestEvent> {
        Self::ensure_timestamp(pending.at_ms)?;
        if matches!(
            pending.payload,
            TestEventPayload::SessionStarted | TestEventPayload::SessionEnded { .. }
        ) {
            return Err(EventStoreError::LifecycleEventReserved);
        }

        let state = self.state.read().await;
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStoreError::ShuttingDown);
        }
        let log = state
            .sessions
            .get(&pending.session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(pending.session_id.clone()))?;
        let mut log = log.lock().await;
        drop(state);
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStoreError::ShuttingDown);
        }
        if log.info.state == SessionState::Ended {
            return Err(EventStoreError::SessionEnded(pending.session_id));
        }
        let sequence = Self::next_sequence(&log)?;

        enum EventTransition {
            None,
            ActionStarted(Uuid, ActionCorrelation),
            ActionCompleted(Uuid),
            MediaStarted(MediaStreamId, String),
            MediaFrame(MediaStreamId),
            MediaEnded(MediaStreamId),
        }

        let transition = match &pending.payload {
            TestEventPayload::ActionStarted { call } => {
                if log.seen_calls.contains(&call.id) {
                    return Err(EventStoreError::DuplicateActionCall(call.id));
                }
                EventTransition::ActionStarted(
                    call.id,
                    ActionCorrelation {
                        request_id: pending.request_id.clone(),
                        device_id: pending.device_id.clone(),
                    },
                )
            }
            TestEventPayload::ActionCompleted { call_id, outcome } => {
                let correlation = log
                    .in_flight
                    .get(call_id)
                    .ok_or(EventStoreError::ActionNotStarted(*call_id))?;
                if correlation.request_id != pending.request_id
                    || correlation.device_id != pending.device_id
                {
                    return Err(EventStoreError::ActionCorrelationMismatch(*call_id));
                }
                if let devicerail_protocol::ActionOutcome::Succeeded { result } = outcome
                    && result.call_id != *call_id
                {
                    return Err(EventStoreError::ActionResultMismatch(*call_id));
                }
                EventTransition::ActionCompleted(*call_id)
            }
            TestEventPayload::MediaStreamStarted { stream } => {
                if log.seen_media_streams.contains(&stream.id) {
                    if log
                        .media_streams
                        .get(&stream.id)
                        .is_some_and(|active| active.next_frame_index == 1)
                        && let Some(existing) = Self::exact_media_retry(&log, &pending)
                    {
                        return Ok(existing);
                    }
                    return Err(EventStoreError::DuplicateMediaStream(stream.id.clone()));
                }
                if crate::PutEvidence::new(log.info.id.clone(), stream.media_type.clone()).is_err()
                    || stream.viewport.as_ref().is_some_and(|viewport| {
                        viewport.width == 0
                            || viewport.height == 0
                            || !viewport.scale_factor.is_finite()
                            || viewport.scale_factor <= 0.0
                    })
                {
                    return Err(EventStoreError::InvalidMediaStream);
                }
                EventTransition::MediaStarted(stream.id.clone(), stream.media_type.clone())
            }
            TestEventPayload::MediaFrameCaptured { frame } => {
                let Some(stream) = log.media_streams.get(&frame.stream_id) else {
                    if let Some(existing) = Self::exact_media_retry(&log, &pending) {
                        return Ok(existing);
                    }
                    return Err(EventStoreError::MediaStreamNotActive(
                        frame.stream_id.clone(),
                    ));
                };
                if frame.frame_index.get() != stream.next_frame_index {
                    if frame.frame_index.get() < stream.next_frame_index
                        && let Some(existing) = Self::exact_media_retry(&log, &pending)
                    {
                        return Ok(existing);
                    }
                    return Err(EventStoreError::MediaFrameOutOfOrder);
                }
                if frame.evidence.media_type != stream.media_type
                    || crate::Sha256Digest::from_asset_ref(&frame.evidence).is_err()
                    || frame
                        .duration_ms
                        .is_some_and(|value| value > devicerail_protocol::MAX_SAFE_INTEGER)
                {
                    return Err(EventStoreError::MediaFrameMismatch);
                }
                EventTransition::MediaFrame(frame.stream_id.clone())
            }
            TestEventPayload::MediaStreamEnded {
                stream_id,
                frame_count,
            } => {
                let Some(stream) = log.media_streams.get(stream_id) else {
                    if let Some(existing) = Self::exact_media_retry(&log, &pending) {
                        return Ok(existing);
                    }
                    return Err(EventStoreError::MediaStreamNotActive(stream_id.clone()));
                };
                if *frame_count != stream.next_frame_index - 1 {
                    return Err(EventStoreError::MediaFrameCountMismatch);
                }
                EventTransition::MediaEnded(stream_id.clone())
            }
            _ => EventTransition::None,
        };

        let event = Arc::new(TestEvent {
            event_id: EventId::new(),
            session_id: pending.session_id,
            sequence,
            request_id: pending.request_id,
            device_id: pending.device_id,
            at_ms: pending.at_ms,
            payload: pending.payload,
        });
        log.events.push(Arc::clone(&event));
        Self::update_info_after_append(&mut log, sequence);
        match transition {
            EventTransition::None => {}
            EventTransition::ActionStarted(call_id, correlation) => {
                log.seen_calls.insert(call_id);
                log.in_flight.insert(call_id, correlation);
            }
            EventTransition::ActionCompleted(call_id) => {
                log.in_flight.remove(&call_id);
            }
            EventTransition::MediaStarted(stream_id, media_type) => {
                log.seen_media_streams.insert(stream_id.clone());
                log.media_streams.insert(
                    stream_id,
                    MediaStreamState {
                        media_type,
                        next_frame_index: 1,
                    },
                );
            }
            EventTransition::MediaFrame(stream_id) => {
                let stream = log
                    .media_streams
                    .get_mut(&stream_id)
                    .expect("validated media stream remains active");
                stream.next_frame_index += 1;
            }
            EventTransition::MediaEnded(stream_id) => {
                log.media_streams.remove(&stream_id);
            }
        }
        Self::publish_event(&log, &event);
        Ok(event.as_ref().clone())
    }

    async fn end_session(&self, command: EndSession) -> EventStoreResult<SessionInfo> {
        Self::ensure_timestamp(command.at_ms)?;
        let state = self.state.read().await;
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStoreError::ShuttingDown);
        }
        let log = state
            .sessions
            .get(&command.session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(command.session_id.clone()))?;
        let mut log = log.lock().await;
        drop(state);
        if self.stream_shutdown.load(Ordering::Acquire) {
            return Err(EventStoreError::ShuttingDown);
        }
        if log.info.state == SessionState::Ended {
            return Err(EventStoreError::SessionEnded(command.session_id));
        }
        if !log.in_flight_observations.is_empty() {
            return Err(EventStoreError::ObservationsInFlight {
                session_id: command.session_id,
                count: log.in_flight_observations.len(),
            });
        }
        if !log.in_flight.is_empty() {
            return Err(EventStoreError::ActionsInFlight {
                session_id: command.session_id,
                count: log.in_flight.len(),
            });
        }
        if !log.media_streams.is_empty() {
            return Err(EventStoreError::MediaStreamsInFlight {
                session_id: command.session_id,
                count: log.media_streams.len(),
            });
        }
        let sequence = Self::next_sequence(&log)?;
        let event = Arc::new(TestEvent {
            event_id: EventId::new(),
            session_id: command.session_id,
            sequence,
            request_id: command.request_id,
            device_id: command.device_id,
            at_ms: command.at_ms,
            payload: TestEventPayload::SessionEnded {
                outcome: command.outcome,
                reason: command.reason,
            },
        });
        log.events.push(Arc::clone(&event));
        log.info.state = SessionState::Ended;
        log.info.ended_at_ms = Some(command.at_ms);
        Self::update_info_after_append(&mut log, sequence);
        Self::publish_event(&log, &event);
        Ok(log.info.clone())
    }

    async fn list_after(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
    ) -> EventStoreResult<Vec<TestEvent>> {
        let state = self.state.read().await;
        let log = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?;
        let log = log.lock().await;
        drop(state);
        let start = usize::try_from(after.map_or(0, EventSequence::get)).unwrap_or(usize::MAX);
        let events = log.events.iter().skip(start).cloned().collect::<Vec<_>>();
        drop(log);
        Ok(events
            .into_iter()
            .map(|event| event.as_ref().clone())
            .collect())
    }

    async fn list_page(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
        limit: usize,
    ) -> EventStoreResult<Vec<TestEvent>> {
        let state = self.state.read().await;
        let log = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?;
        let log = log.lock().await;
        drop(state);
        let start = usize::try_from(after.map_or(0, EventSequence::get)).unwrap_or(usize::MAX);
        let events = log
            .events
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        drop(log);
        Ok(events
            .into_iter()
            .map(|event| event.as_ref().clone())
            .collect())
    }

    async fn list_sessions(&self) -> EventStoreResult<Vec<SessionInfo>> {
        let state = self.state.read().await;
        let mut sessions = Vec::with_capacity(state.sessions.len());
        for log in state.sessions.values() {
            sessions.push(log.lock().await.info.clone());
        }
        Ok(sessions)
    }

    async fn export_session(&self, session_id: &SessionId) -> EventStoreResult<SessionExport> {
        let state = self.state.read().await;
        let log = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?;
        let log = log.lock().await;
        drop(state);
        let session = log.info.clone();
        let events = log.events.clone();
        drop(log);
        Ok(SessionExport {
            session,
            events: events
                .into_iter()
                .map(|event| event.as_ref().clone())
                .collect(),
        })
    }

    async fn export_session_page(
        &self,
        session_id: &SessionId,
        after: Option<EventSequence>,
        limit: usize,
    ) -> EventStoreResult<SessionExportPageSnapshot> {
        let state = self.state.read().await;
        let log = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?;
        let log = log.lock().await;
        drop(state);
        let range = export_page_range(&log.info, log.events.len(), after, limit)?;
        let events = log.events[range].to_vec();
        Ok(SessionExportPageSnapshot {
            session: log.info.clone(),
            events,
        })
    }

    async fn delete_ended(&self, session_id: &SessionId) -> EventStoreResult<()> {
        let mut state = self.state.write().await;
        let log = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| EventStoreError::SessionNotFound(session_id.clone()))?;
        let log = log.lock().await;
        if log.info.state == SessionState::Active {
            return Err(EventStoreError::SessionActive(session_id.clone()));
        }
        let last_sequence = log.info.last_sequence;
        let _ = log.stream_tail.send(EventStreamSignal::SessionDeleted {
            session_id: session_id.clone(),
            last_sequence,
        });
        drop(log);
        state.sessions.remove(session_id);
        state
            .observation_leases
            .retain(|_, lease| lease.session_id != *session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use devicerail_protocol::{
        ActionCall, ActionOutcome, ActionProtection, ErrorInfo, EventId, EventSequence,
        RecordedActionCall, SessionId, SessionOutcome, TestEvent, TestEventPayload,
    };
    use serde_json::json;
    use tokio::sync::Barrier;
    use uuid::Uuid;

    use super::{
        EventStoreError, EventStreamSignal, MAX_EVENTS_LIST_PAGE_SIZE, MemoryEventStore,
        SessionEventStore,
    };
    use crate::{
        EndSession, EventStreamConfig, EventStreamError, EventStreamItem, EventStreamTerminal,
        PendingEvent, StartSession, now_ms,
    };

    fn error_payload(code: &str) -> TestEventPayload {
        TestEventPayload::Error {
            error: ErrorInfo {
                code: code.to_owned(),
                message: code.to_owned(),
                retryable: false,
                details: None,
            },
        }
    }

    fn pending_error(session_id: &SessionId, code: &str) -> PendingEvent {
        PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            payload: error_payload(code),
        }
    }

    fn end_command(session_id: &SessionId) -> EndSession {
        EndSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        }
    }

    #[tokio::test]
    async fn ended_sessions_are_append_only_and_can_be_deleted_whole() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        assert_eq!(
            store
                .start_session(StartSession {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                })
                .await
                .expect_err("an active Session id is already used"),
            EventStoreError::SessionAlreadyExists(session_id.clone())
        );
        assert_eq!(
            store
                .delete_ended(&session_id)
                .await
                .expect_err("active session is protected"),
            EventStoreError::SessionActive(session_id.clone())
        );

        store
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");
        assert_eq!(
            store
                .start_session(StartSession {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                })
                .await
                .expect_err("an ended Session id remains used"),
            EventStoreError::SessionAlreadyExists(session_id.clone())
        );
        let exported = store.export_session(&session_id).await.expect("export");
        assert!(matches!(
            exported.events.last().expect("terminal event").payload,
            TestEventPayload::SessionEnded { .. }
        ));
        store.delete_ended(&session_id).await.expect("delete ended");
        assert!(matches!(
            store.export_session(&session_id).await,
            Err(EventStoreError::SessionNotFound(_))
        ));
        assert_eq!(
            store
                .delete_ended(&session_id)
                .await
                .expect_err("deleting an absent log remains not found"),
            EventStoreError::SessionNotFound(session_id.clone())
        );
        assert_eq!(
            store
                .start_session(StartSession {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                })
                .await
                .expect_err("a deleted Session id remains permanently used"),
            EventStoreError::SessionAlreadyExists(session_id)
        );
    }

    #[tokio::test]
    async fn action_ids_are_single_use_and_sessions_cannot_end_mid_action() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        let call = ActionCall {
            id: Uuid::new_v4(),
            name: "noop".to_owned(),
            arguments: json!({}),
        };
        let started = PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            payload: TestEventPayload::ActionStarted {
                call: RecordedActionCall::from_action_call(&call, Some(ActionProtection::Standard)),
            },
        };
        store.append(started.clone()).await.expect("start action");
        assert_eq!(
            store
                .append(started)
                .await
                .expect_err("call ids are never reused"),
            EventStoreError::DuplicateActionCall(call.id)
        );

        let end = EndSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        };
        assert_eq!(
            store
                .end_session(end.clone())
                .await
                .expect_err("in-flight action protects session"),
            EventStoreError::ActionsInFlight {
                session_id: session_id.clone(),
                count: 1
            }
        );

        store
            .append(PendingEvent {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                payload: TestEventPayload::ActionCompleted {
                    call_id: call.id,
                    outcome: ActionOutcome::Failed {
                        error: ErrorInfo {
                            code: "test_failure".to_owned(),
                            message: "expected".to_owned(),
                            retryable: false,
                            details: None,
                        },
                    },
                },
            })
            .await
            .expect("complete action");
        store.end_session(end).await.expect("end after completion");
        assert!(matches!(
            store
                .append(PendingEvent {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    payload: error_payload("late"),
                })
                .await,
            Err(EventStoreError::SessionEnded(id)) if id == session_id
        ));
    }

    #[tokio::test]
    async fn observation_leases_are_session_scoped_and_do_not_append_events() {
        let store = MemoryEventStore::default();
        let missing = devicerail_protocol::SessionId::new();
        assert_eq!(
            store
                .reserve_observation(&missing)
                .await
                .expect_err("missing session"),
            EventStoreError::SessionNotFound(missing)
        );

        let first = StartSession::new(None, None, now_ms());
        let first_id = first.session_id.clone();
        let second = StartSession::new(None, None, now_ms());
        let second_id = second.session_id.clone();
        store.start_session(first).await.expect("first session");
        store.start_session(second).await.expect("second session");

        let lease = store
            .reserve_observation(&first_id)
            .await
            .expect("reserve observation");
        assert_eq!(
            store
                .list_after(&first_id, None)
                .await
                .expect("lease does not append")
                .len(),
            1
        );
        assert_eq!(
            store
                .release_observation(&second_id, lease)
                .await
                .expect_err("lease is bound to first session"),
            EventStoreError::InvalidObservationLease {
                session_id: second_id,
                lease_id: lease.id(),
            }
        );

        let end = EndSession {
            session_id: first_id.clone(),
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        };
        let busy = store
            .end_session(end.clone())
            .await
            .expect_err("observation protects session");
        assert_eq!(
            busy,
            EventStoreError::ObservationsInFlight {
                session_id: first_id.clone(),
                count: 1,
            }
        );
        let info = busy.to_error_info();
        assert_eq!(info.code, "session_busy");
        assert!(info.retryable);
        assert_eq!(
            info.details,
            Some(json!({
                "sessionId": first_id,
                "inFlightObservations": 1
            }))
        );

        store
            .release_observation(&end.session_id, lease)
            .await
            .expect("release observation");
        assert_eq!(
            store
                .list_after(&end.session_id, None)
                .await
                .expect("release does not append")
                .len(),
            1
        );
        store
            .release_observation(&end.session_id, lease)
            .await
            .expect("releasing the same valid pair is idempotent");
        let unknown = super::ObservationLease::new();
        assert_eq!(
            store
                .release_observation(&end.session_id, unknown)
                .await
                .expect_err("an unknown lease is invalid"),
            EventStoreError::InvalidObservationLease {
                session_id: end.session_id.clone(),
                lease_id: unknown.id(),
            }
        );
        store.end_session(end.clone()).await.expect("end session");
        assert_eq!(
            store
                .reserve_observation(&end.session_id)
                .await
                .expect_err("ended session"),
            EventStoreError::SessionEnded(end.session_id.clone())
        );
        store
            .delete_ended(&end.session_id)
            .await
            .expect("delete ended session");
        assert!(
            !store
                .state
                .read()
                .await
                .observation_leases
                .contains_key(&lease)
        );
        assert_eq!(
            store
                .release_observation(&end.session_id, lease)
                .await
                .expect_err("deletion discards released lease tombstones"),
            EventStoreError::InvalidObservationLease {
                session_id: end.session_id,
                lease_id: lease.id(),
            }
        );
    }

    #[tokio::test]
    async fn sessions_have_independent_sequences_and_cursor_replay() {
        let store = MemoryEventStore::default();
        let first = StartSession::new(None, None, now_ms());
        let second = StartSession::new(None, None, now_ms());
        let first_id = first.session_id.clone();
        let second_id = second.session_id.clone();
        store.start_session(first).await.expect("first session");
        store.start_session(second).await.expect("second session");

        for (session_id, code) in [(&first_id, "first"), (&second_id, "second")] {
            store
                .append(PendingEvent {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    payload: error_payload(code),
                })
                .await
                .expect("append isolated event");
        }

        let first_page = store
            .list_after(&first_id, None)
            .await
            .expect("first replay");
        assert_eq!(
            first_page
                .iter()
                .map(|event| event.sequence.get())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let resumed = store
            .list_after(&first_id, Some(EventSequence::FIRST))
            .await
            .expect("resume replay");
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].sequence.get(), 2);
        let bounded = store
            .list_page(&first_id, None, 1)
            .await
            .expect("bounded first page");
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].sequence.get(), 1);
        let bounded_resume = store
            .list_page(&first_id, Some(bounded[0].sequence), 1)
            .await
            .expect("bounded resumed page");
        assert_eq!(bounded_resume.len(), 1);
        assert_eq!(bounded_resume[0].sequence.get(), 2);
        assert_eq!(
            store
                .list_after(&second_id, None)
                .await
                .expect("second replay")
                .iter()
                .map(|event| event.sequence.get())
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_session_lock_does_not_block_another_session_append() {
        let store = Arc::new(MemoryEventStore::default());
        let first = StartSession::new(None, None, now_ms());
        let second = StartSession::new(None, None, now_ms());
        let first_id = first.session_id.clone();
        let second_id = second.session_id.clone();
        store.start_session(first).await.expect("first session");
        store.start_session(second).await.expect("second session");

        let first_log = store
            .state
            .read()
            .await
            .sessions
            .get(&first_id)
            .cloned()
            .expect("first log");
        let first_guard = first_log.lock().await;
        let blocked = tokio::spawn({
            let store = Arc::clone(&store);
            let first_id = first_id.clone();
            async move { store.append(pending_error(&first_id, "blocked")).await }
        });
        tokio::task::yield_now().await;

        let independent = tokio::time::timeout(
            Duration::from_secs(1),
            store.append(pending_error(&second_id, "independent")),
        )
        .await
        .expect("another Session is not blocked")
        .expect("independent append");
        assert_eq!(independent.sequence.get(), 2);

        drop(first_guard);
        assert_eq!(
            blocked
                .await
                .expect("blocked task joins")
                .expect("first append")
                .sequence
                .get(),
            2
        );
    }

    #[tokio::test]
    async fn ended_session_exports_are_atomically_paged_with_explicit_continuation() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");

        assert_eq!(
            store
                .export_session_page(&session_id, None, 2)
                .await
                .expect_err("active Session cannot produce an immutable export page"),
            EventStoreError::SessionActive(session_id.clone())
        );
        store
            .append(pending_error(&session_id, "first"))
            .await
            .expect("first error");
        store
            .append(pending_error(&session_id, "second"))
            .await
            .expect("second error");
        let ended = store
            .end_session(end_command(&session_id))
            .await
            .expect("end session");
        let stored_log = store
            .state
            .read()
            .await
            .sessions
            .get(&session_id)
            .cloned()
            .expect("stored session");
        let first_stored = Arc::clone(&stored_log.lock().await.events[0]);

        let first = store
            .export_session_page(&session_id, None, 2)
            .await
            .expect("first export page");
        assert_eq!(first.session, ended);
        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.sequence.get())
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(Arc::ptr_eq(&first.events[0], &first_stored));
        let first_cursor = first.events.last().map(|event| event.sequence);
        assert_eq!(first_cursor, EventSequence::new(2));
        assert!(first_cursor.expect("cursor") < first.session.last_sequence);

        let second = store
            .export_session_page(&session_id, first_cursor, 2)
            .await
            .expect("final export page");
        assert_eq!(second.session, first.session);
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.sequence.get())
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert_eq!(
            second.events.last().map(|event| event.sequence),
            Some(second.session.last_sequence)
        );

        let exhausted = store
            .export_session_page(&session_id, EventSequence::new(4), 2)
            .await
            .expect("exhausted export cursor");
        assert!(exhausted.events.is_empty());

        let ahead = EventSequence::new(5).expect("cursor");
        let error = store
            .export_session_page(&session_id, Some(ahead), 2)
            .await
            .expect_err("cursor ahead must fail explicitly");
        assert_eq!(
            error,
            EventStoreError::CursorAhead {
                session_id: session_id.clone(),
                after: ahead,
                last_sequence: ended.last_sequence,
            }
        );
        assert_eq!(error.to_error_info().code, "event_cursor_ahead");

        for limit in [0, MAX_EVENTS_LIST_PAGE_SIZE as usize + 1] {
            assert_eq!(
                store
                    .export_session_page(&session_id, None, limit)
                    .await
                    .expect_err("invalid page limit"),
                EventStoreError::InvalidPageLimit {
                    limit,
                    maximum: MAX_EVENTS_LIST_PAGE_SIZE as usize,
                }
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn subscription_linearizes_snapshot_and_concurrent_tail_without_gaps_or_duplicates() {
        for index in 0..32_u64 {
            let store = Arc::new(MemoryEventStore::default());
            let start = StartSession::new(None, None, now_ms());
            let session_id = start.session_id.clone();
            store.start_session(start).await.expect("start session");

            let barrier = Arc::new(Barrier::new(3));
            let subscribe = tokio::spawn({
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                let session_id = session_id.clone();
                async move {
                    barrier.wait().await;
                    store
                        .subscribe_after(&session_id, Some(EventSequence::FIRST))
                        .await
                }
            });
            let append = tokio::spawn({
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                let session_id = session_id.clone();
                async move {
                    barrier.wait().await;
                    store
                        .append(pending_error(&session_id, &format!("boundary-{index}")))
                        .await
                }
            });
            barrier.wait().await;

            let mut subscription = subscribe.await.expect("join subscribe").expect("subscribe");
            let appended = append.await.expect("join append").expect("append");
            assert_eq!(appended.sequence.get(), 2);
            assert!(matches!(subscription.replay_through().get(), 1 | 2));
            let first = tokio::time::timeout(Duration::from_secs(1), subscription.next())
                .await
                .expect("boundary event arrives")
                .expect("stream item");
            assert!(matches!(
                first,
                EventStreamItem::Event(event) if event.sequence.get() == 2
            ));

            store
                .append(pending_error(&session_id, "after-boundary"))
                .await
                .expect("append after boundary");
            assert!(matches!(
                subscription.next().await,
                Some(EventStreamItem::Event(event)) if event.sequence.get() == 3
            ));
        }
    }

    #[tokio::test]
    async fn subscription_rejects_ahead_and_over_limit_cursors() {
        let config = EventStreamConfig::new(2, 4, 2).expect("test config");
        let store = MemoryEventStore::with_event_stream_config(config);
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        store
            .append(pending_error(&session_id, "second"))
            .await
            .expect("second event");
        store
            .append(pending_error(&session_id, "third"))
            .await
            .expect("third event");

        assert_eq!(
            store
                .subscribe_after(&session_id, None)
                .await
                .expect_err("three event replay exceeds configured limit"),
            EventStreamError::ReplayLimitExceeded {
                session_id: session_id.clone(),
                requested: 3,
                maximum: 2,
            }
        );
        let ahead = EventSequence::new(4).expect("valid cursor");
        assert_eq!(
            store
                .subscribe_after(&session_id, Some(ahead))
                .await
                .expect_err("ahead cursor"),
            EventStreamError::CursorAhead {
                session_id: session_id.clone(),
                after: ahead,
                last_sequence: EventSequence::new(3).expect("last sequence"),
            }
        );

        let mut bounded = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("exactly two replay events");
        assert_eq!(bounded.replay_through().get(), 3);
        for expected in [2, 3] {
            assert!(matches!(
                bounded.next().await,
                Some(EventStreamItem::Event(event)) if event.sequence.get() == expected
            ));
        }
    }

    #[test]
    fn event_stream_config_enforces_nonzero_hard_caps() {
        use crate::{
            MAX_EVENT_STREAM_REPLAY_EVENTS, MAX_EVENT_STREAM_SUBSCRIBERS_PER_SESSION,
            MAX_EVENT_STREAM_TAIL_EVENTS,
        };

        for (field, result) in [
            ("maxReplayEvents", EventStreamConfig::new(0, 1, 1)),
            ("tailCapacity", EventStreamConfig::new(1, 0, 1)),
            ("maxSubscribersPerSession", EventStreamConfig::new(1, 1, 0)),
            (
                "maxReplayEvents",
                EventStreamConfig::new(MAX_EVENT_STREAM_REPLAY_EVENTS + 1, 1, 1),
            ),
            (
                "tailCapacity",
                EventStreamConfig::new(1, MAX_EVENT_STREAM_TAIL_EVENTS + 1, 1),
            ),
            (
                "maxSubscribersPerSession",
                EventStreamConfig::new(1, 1, MAX_EVENT_STREAM_SUBSCRIBERS_PER_SESSION + 1),
            ),
        ] {
            assert!(matches!(
                result,
                Err(EventStreamError::InvalidConfig {
                    field: actual,
                    ..
                }) if actual == field
            ));
        }
        assert_eq!(
            EventStreamConfig::default().max_replay_events(),
            MAX_EVENT_STREAM_REPLAY_EVENTS
        );
    }

    #[tokio::test]
    async fn slow_subscription_lags_explicitly_without_blocking_append() {
        let config = EventStreamConfig::new(8, 2, 4).expect("test config");
        let store = MemoryEventStore::with_event_stream_config(config);
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        let mut subscription = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("subscribe at head");

        tokio::time::timeout(Duration::from_secs(1), async {
            for code in ["second", "third", "fourth"] {
                store
                    .append(pending_error(&session_id, code))
                    .await
                    .expect("append never waits for subscriber");
            }
        })
        .await
        .expect("append remains independent of slow subscriber");

        assert_eq!(
            subscription.next().await,
            Some(EventStreamItem::Terminal(EventStreamTerminal::TailLagged {
                last_delivered: Some(EventSequence::FIRST),
                missed_events: 1,
            }))
        );
        assert_eq!(subscription.next().await, None);
    }

    #[tokio::test]
    async fn ended_and_deleted_sessions_have_ordered_explicit_terminals() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");

        let mut live = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("live subscription");
        store
            .end_session(end_command(&session_id))
            .await
            .expect("end session");
        assert!(matches!(
            live.next().await,
            Some(EventStreamItem::Event(event))
                if event.sequence.get() == 2
                    && matches!(event.payload, TestEventPayload::SessionEnded { .. })
        ));
        assert_eq!(
            live.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::SessionEnded {
                    last_sequence: EventSequence::new(2).expect("terminal sequence"),
                }
            ))
        );
        let ended_log = store
            .state
            .read()
            .await
            .sessions
            .get(&session_id)
            .cloned()
            .expect("ended session");
        assert_eq!(
            ended_log.lock().await.stream_tail.receiver_count(),
            0,
            "a terminal stream releases its subscriber slot immediately"
        );

        let mut at_end = store
            .subscribe_after(
                &session_id,
                Some(EventSequence::new(2).expect("ended cursor")),
            )
            .await
            .expect("subscribe at ended high-water");
        assert_eq!(
            at_end.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::SessionEnded {
                    last_sequence: EventSequence::new(2).expect("terminal sequence"),
                }
            ))
        );

        let mut historical = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("ended session replay");
        assert_eq!(
            historical.session_state(),
            devicerail_protocol::SessionState::Ended
        );
        assert_eq!(historical.replay_through().get(), 2);
        store.delete_ended(&session_id).await.expect("delete ended");
        assert!(matches!(
            historical.next().await,
            Some(EventStreamItem::Event(event)) if event.sequence.get() == 2
        ));
        assert_eq!(
            historical.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::SessionDeleted {
                    last_sequence: EventSequence::new(2).expect("terminal sequence"),
                }
            ))
        );
        assert_eq!(
            store
                .subscribe_after(&session_id, Some(EventSequence::FIRST))
                .await
                .expect_err("deleted tombstone is distinct"),
            EventStreamError::SessionDeleted(session_id)
        );
    }

    #[tokio::test]
    async fn cancelled_wait_is_retryable_and_drop_releases_the_subscriber_slot() {
        let config = EventStreamConfig::new(8, 4, 1).expect("test config");
        let store = MemoryEventStore::with_event_stream_config(config);
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        let mut subscription = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("first subscriber");
        assert_eq!(
            store
                .subscribe_after(&session_id, Some(EventSequence::FIRST))
                .await
                .expect_err("subscriber limit"),
            EventStreamError::TooManySubscribers {
                session_id: session_id.clone(),
                maximum: 1,
            }
        );

        assert!(
            tokio::time::timeout(Duration::from_millis(10), subscription.next())
                .await
                .is_err(),
            "cancelling an empty wait must not consume a cursor"
        );
        store
            .append(pending_error(&session_id, "after-cancel"))
            .await
            .expect("append after cancelled wait");
        assert!(matches!(
            subscription.next().await,
            Some(EventStreamItem::Event(event)) if event.sequence.get() == 2
        ));
        drop(subscription);

        let replacement = store
            .subscribe_after(
                &session_id,
                Some(EventSequence::new(2).expect("replacement cursor")),
            )
            .await
            .expect("receiver Drop releases subscriber slot");
        drop(replacement);
    }

    #[tokio::test]
    async fn stream_shutdown_wakes_waiters_and_rejects_new_writes() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        let mut subscription = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("subscription");

        assert!(store.begin_stream_shutdown().await);
        assert!(!store.begin_stream_shutdown().await);
        assert_eq!(
            subscription.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::StoreShutdown
            ))
        );
        assert_eq!(subscription.next().await, None);
        let shutdown_log = store
            .state
            .read()
            .await
            .sessions
            .get(&session_id)
            .cloned()
            .expect("session retained during shutdown");
        assert_eq!(
            shutdown_log.lock().await.stream_tail.receiver_count(),
            0,
            "shutdown terminal releases the subscriber slot"
        );
        assert_eq!(
            store
                .subscribe_after(&session_id, Some(EventSequence::FIRST))
                .await
                .expect_err("new subscription after shutdown"),
            EventStreamError::StoreShutdown
        );
        assert_eq!(
            store
                .append(pending_error(&session_id, "late"))
                .await
                .expect_err("append after shutdown"),
            EventStoreError::ShuttingDown
        );
        assert_eq!(
            store
                .end_session(end_command(&session_id))
                .await
                .expect_err("end after shutdown"),
            EventStoreError::ShuttingDown
        );
        assert_eq!(
            store
                .reserve_observation(&session_id)
                .await
                .expect_err("new operation after shutdown"),
            EventStoreError::ShuttingDown
        );
        assert_eq!(
            store
                .start_session(StartSession::new(None, None, now_ms()))
                .await
                .expect_err("new session after shutdown"),
            EventStoreError::ShuttingDown
        );
    }

    #[tokio::test]
    async fn subscription_fails_closed_on_tail_session_or_sequence_corruption() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");
        let mut gap = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("gap subscription");
        let sender_log = store
            .state
            .read()
            .await
            .sessions
            .get(&session_id)
            .cloned()
            .expect("session");
        let sender = sender_log.lock().await.stream_tail.clone();
        let actual = EventSequence::new(3).expect("gap sequence");
        let _ = sender.send(EventStreamSignal::Event(Arc::new(TestEvent {
            event_id: EventId::new(),
            session_id: session_id.clone(),
            sequence: actual,
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            payload: error_payload("gap"),
        })));
        assert_eq!(
            gap.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::SequenceGap {
                    expected: 2,
                    actual,
                }
            ))
        );
        drop(gap);

        let mut mismatch = store
            .subscribe_after(&session_id, Some(EventSequence::FIRST))
            .await
            .expect("mismatch subscription");
        let other = SessionId::new();
        let _ = sender.send(EventStreamSignal::Event(Arc::new(TestEvent {
            event_id: EventId::new(),
            session_id: other.clone(),
            sequence: EventSequence::new(2).expect("next sequence"),
            request_id: None,
            device_id: None,
            at_ms: now_ms(),
            payload: error_payload("mismatch"),
        })));
        assert_eq!(
            mismatch.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::SessionMismatch {
                    expected: session_id,
                    actual: other,
                }
            ))
        );
    }

    #[tokio::test]
    async fn sequence_exhaustion_is_explicit_and_never_wraps() {
        let store = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        store.start_session(start).await.expect("start session");

        let almost_exhausted =
            EventSequence::new(devicerail_protocol::MAX_SAFE_INTEGER - 1).expect("safe sequence");
        {
            let state = store.state.write().await;
            let log = state
                .sessions
                .get(&session_id)
                .cloned()
                .expect("session log");
            let mut log = log.lock().await;
            log.info.last_sequence = almost_exhausted;
            log.info.event_count = almost_exhausted;
        }
        let last = store
            .append(PendingEvent {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                payload: error_payload("last"),
            })
            .await
            .expect("maximum sequence is usable");
        assert_eq!(last.sequence.get(), devicerail_protocol::MAX_SAFE_INTEGER);
        assert_eq!(
            store
                .append(PendingEvent {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    payload: error_payload("overflow"),
                })
                .await
                .expect_err("sequence must not wrap"),
            EventStoreError::SequenceExhausted(session_id)
        );
    }
}
