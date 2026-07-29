use std::{collections::VecDeque, sync::Arc};

use devicerail_protocol::{EventSequence, SessionId, SessionState, TestEvent, TestEventPayload};
use thiserror::Error;
use tokio::sync::broadcast;

pub const MAX_EVENT_STREAM_REPLAY_EVENTS: usize = 100_000;
pub const MAX_EVENT_STREAM_TAIL_EVENTS: usize = 4_096;
pub const MAX_EVENT_STREAM_SUBSCRIBERS_PER_SESSION: usize = 128;

/// Memory and fan-out limits for transport-neutral event subscriptions.
///
/// These limits bound only the Core replay snapshot and tail ring. A concrete
/// transport must additionally bound its serialized byte queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventStreamConfig {
    max_replay_events: usize,
    tail_capacity: usize,
    max_subscribers_per_session: usize,
}

impl EventStreamConfig {
    pub fn new(
        max_replay_events: usize,
        tail_capacity: usize,
        max_subscribers_per_session: usize,
    ) -> Result<Self, EventStreamError> {
        Self::validate_limit(
            "maxReplayEvents",
            max_replay_events,
            MAX_EVENT_STREAM_REPLAY_EVENTS,
        )?;
        Self::validate_limit("tailCapacity", tail_capacity, MAX_EVENT_STREAM_TAIL_EVENTS)?;
        Self::validate_limit(
            "maxSubscribersPerSession",
            max_subscribers_per_session,
            MAX_EVENT_STREAM_SUBSCRIBERS_PER_SESSION,
        )?;
        Ok(Self {
            max_replay_events,
            tail_capacity,
            max_subscribers_per_session,
        })
    }

    fn validate_limit(
        field: &'static str,
        value: usize,
        maximum: usize,
    ) -> Result<(), EventStreamError> {
        if value == 0 || value > maximum {
            return Err(EventStreamError::InvalidConfig {
                field,
                value,
                maximum,
            });
        }
        Ok(())
    }

    pub const fn max_replay_events(self) -> usize {
        self.max_replay_events
    }

    pub const fn tail_capacity(self) -> usize {
        self.tail_capacity
    }

    pub const fn max_subscribers_per_session(self) -> usize {
        self.max_subscribers_per_session
    }
}

impl Default for EventStreamConfig {
    fn default() -> Self {
        Self {
            max_replay_events: MAX_EVENT_STREAM_REPLAY_EVENTS,
            tail_capacity: MAX_EVENT_STREAM_TAIL_EVENTS,
            max_subscribers_per_session: MAX_EVENT_STREAM_SUBSCRIBERS_PER_SESSION,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EventStreamError {
    #[error("invalid event stream config {field}={value}; expected 1..={maximum}")]
    InvalidConfig {
        field: &'static str,
        value: usize,
        maximum: usize,
    },
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("session event log was deleted: {0}")]
    SessionDeleted(SessionId),
    #[error(
        "event cursor {after:?} is ahead of session {session_id} last sequence {last_sequence:?}"
    )]
    CursorAhead {
        session_id: SessionId,
        after: EventSequence,
        last_sequence: EventSequence,
    },
    #[error(
        "event replay for session {session_id} contains {requested} events, exceeding limit {maximum}"
    )]
    ReplayLimitExceeded {
        session_id: SessionId,
        requested: usize,
        maximum: usize,
    },
    #[error("session {session_id} reached its subscriber limit {maximum}")]
    TooManySubscribers {
        session_id: SessionId,
        maximum: usize,
    },
    #[error("event stream store is shutting down")]
    StoreShutdown,
    #[error("event stream failed: {0}")]
    Internal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventStreamTerminal {
    SessionEnded {
        last_sequence: EventSequence,
    },
    SessionDeleted {
        last_sequence: EventSequence,
    },
    TailLagged {
        last_delivered: Option<EventSequence>,
        missed_events: u64,
    },
    StoreShutdown,
    SessionMismatch {
        expected: SessionId,
        actual: SessionId,
    },
    SequenceGap {
        expected: u64,
        actual: EventSequence,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventStreamItem {
    Event(Arc<TestEvent>),
    Terminal(EventStreamTerminal),
}

#[derive(Clone, Debug)]
pub(crate) enum EventStreamSignal {
    Event(Arc<TestEvent>),
    SessionDeleted {
        session_id: SessionId,
        last_sequence: EventSequence,
    },
    StoreShutdown,
}

/// One continuous Session event suffix followed by exactly one terminal.
///
/// Dropping this value immediately drops the broadcast receiver; Core does
/// not own a subscriber task or an asynchronous deregistration path.
#[derive(Debug)]
pub struct EventSubscription {
    session_id: SessionId,
    snapshot: VecDeque<Arc<TestEvent>>,
    tail: Option<broadcast::Receiver<EventStreamSignal>>,
    last_delivered: Option<EventSequence>,
    replay_through: EventSequence,
    session_state: SessionState,
    known_ended: Option<EventSequence>,
    done: bool,
}

impl EventSubscription {
    pub(crate) fn new(
        session_id: SessionId,
        snapshot: VecDeque<Arc<TestEvent>>,
        tail: broadcast::Receiver<EventStreamSignal>,
        after: Option<EventSequence>,
        replay_through: EventSequence,
        session_state: SessionState,
    ) -> Self {
        let known_ended = (session_state == SessionState::Ended).then_some(replay_through);
        Self {
            session_id,
            snapshot,
            tail: Some(tail),
            last_delivered: after,
            replay_through,
            session_state,
            known_ended,
            done: false,
        }
    }

    pub const fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Last sequence covered by this subscription's continuous prefix. It is
    /// initialized from the caller's `after` cursor and then advances for each
    /// emitted event. Transports must not treat it as a new application
    /// acknowledgement.
    pub const fn last_delivered_sequence(&self) -> Option<EventSequence> {
        self.last_delivered
    }

    /// Atomic high-water mark covered by the replay snapshot captured during
    /// subscription registration.
    pub const fn replay_through(&self) -> EventSequence {
        self.replay_through
    }

    /// Session state observed at the same linearization point as
    /// [`Self::replay_through`].
    pub const fn session_state(&self) -> SessionState {
        self.session_state
    }

    pub async fn next(&mut self) -> Option<EventStreamItem> {
        if self.done {
            return None;
        }

        if let Some(event) = self.snapshot.pop_front() {
            return Some(self.validate_event(event));
        }

        let tail = self
            .tail
            .as_mut()
            .expect("an unfinished subscription retains its tail receiver");
        match tail.try_recv() {
            Ok(signal) => return Some(self.handle_signal(signal)),
            Err(broadcast::error::TryRecvError::Lagged(missed_events)) => {
                return Some(self.finish(EventStreamTerminal::TailLagged {
                    last_delivered: self.last_delivered,
                    missed_events,
                }));
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                return Some(self.finish(EventStreamTerminal::StoreShutdown));
            }
            Err(broadcast::error::TryRecvError::Empty) => {}
        }

        if let Some(last_sequence) = self.known_ended {
            return Some(self.finish(EventStreamTerminal::SessionEnded { last_sequence }));
        }

        let tail = self
            .tail
            .as_mut()
            .expect("an unfinished subscription retains its tail receiver");
        match tail.recv().await {
            Ok(signal) => Some(self.handle_signal(signal)),
            Err(broadcast::error::RecvError::Lagged(missed_events)) => {
                Some(self.finish(EventStreamTerminal::TailLagged {
                    last_delivered: self.last_delivered,
                    missed_events,
                }))
            }
            Err(broadcast::error::RecvError::Closed) => {
                Some(self.finish(EventStreamTerminal::StoreShutdown))
            }
        }
    }

    fn handle_signal(&mut self, signal: EventStreamSignal) -> EventStreamItem {
        match signal {
            EventStreamSignal::Event(event) => self.validate_event(event),
            EventStreamSignal::SessionDeleted {
                session_id,
                last_sequence,
            } => {
                if session_id != self.session_id {
                    return self.finish(EventStreamTerminal::SessionMismatch {
                        expected: self.session_id.clone(),
                        actual: session_id,
                    });
                }
                self.finish(EventStreamTerminal::SessionDeleted { last_sequence })
            }
            EventStreamSignal::StoreShutdown => self.finish(EventStreamTerminal::StoreShutdown),
        }
    }

    fn validate_event(&mut self, event: Arc<TestEvent>) -> EventStreamItem {
        if event.session_id != self.session_id {
            return self.finish(EventStreamTerminal::SessionMismatch {
                expected: self.session_id.clone(),
                actual: event.session_id.clone(),
            });
        }

        let expected = self
            .last_delivered
            .map_or(EventSequence::FIRST.get(), |sequence| {
                sequence.get().saturating_add(1)
            });
        if event.sequence.get() != expected {
            return self.finish(EventStreamTerminal::SequenceGap {
                expected,
                actual: event.sequence,
            });
        }

        self.last_delivered = Some(event.sequence);
        if matches!(event.payload, TestEventPayload::SessionEnded { .. }) {
            self.known_ended = Some(event.sequence);
        }
        EventStreamItem::Event(event)
    }

    fn finish(&mut self, terminal: EventStreamTerminal) -> EventStreamItem {
        self.done = true;
        self.snapshot.clear();
        self.tail = None;
        EventStreamItem::Terminal(terminal)
    }
}
