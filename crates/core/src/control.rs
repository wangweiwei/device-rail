use std::{
    fmt,
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};

const MAX_TIMER_SLICE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationReason {
    Requested,
    Shutdown,
}

impl CancellationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Shutdown => "shutdown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutScope {
    Request,
    Action,
    Shutdown,
}

impl TimeoutScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Action => "action",
            Self::Shutdown => "shutdown",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ExecutionDeadline {
    started_at: Instant,
    duration: Duration,
    timeout_ms: u64,
    scope: TimeoutScope,
}

impl ExecutionDeadline {
    fn remaining_at(self, now: Instant) -> Duration {
        let elapsed = now
            .checked_duration_since(self.started_at)
            .unwrap_or_default();
        self.duration.saturating_sub(elapsed)
    }
}

#[derive(Debug, Default)]
struct CancellationState {
    reason: Mutex<Option<CancellationReason>>,
    changed: Notify,
}

/// Read-only request control passed into Drivers.
///
/// Clones share the same cancellation signal. A derived clone may shorten the
/// deadline (for example with an Action-specific timeout) without extending
/// the parent request budget.
#[derive(Clone)]
pub struct ExecutionControl {
    cancellation: Arc<CancellationState>,
    deadline: Option<ExecutionDeadline>,
}

impl fmt::Debug for ExecutionControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutionControl")
            .field("cancellation_reason", &self.cancellation_reason())
            .field("deadline", &self.deadline)
            .finish()
    }
}

/// The cancellation authority retained by the request supervisor.
#[derive(Clone, Debug)]
pub struct ExecutionController {
    cancellation: Arc<CancellationState>,
}

impl ExecutionController {
    pub fn new() -> (Self, ExecutionControl) {
        let cancellation = Arc::new(CancellationState::default());
        (
            Self {
                cancellation: Arc::clone(&cancellation),
            },
            ExecutionControl {
                cancellation,
                deadline: None,
            },
        )
    }

    pub fn with_timeout(timeout_ms: u64, scope: TimeoutScope) -> (Self, ExecutionControl) {
        let (controller, control) = Self::new();
        (controller, control.with_timeout(timeout_ms, scope))
    }

    /// Signals cancellation once. The first reason is durable and wins races
    /// between an explicit cancel and shutdown.
    pub fn cancel(&self, reason: CancellationReason) -> bool {
        let mut current = self
            .cancellation
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.is_some() {
            return false;
        }
        *current = Some(reason);
        drop(current);
        self.cancellation.changed.notify_waiters();
        true
    }
}

impl ExecutionControl {
    pub fn unbounded() -> Self {
        ExecutionController::new().1
    }

    pub fn with_timeout(&self, timeout_ms: u64, scope: TimeoutScope) -> Self {
        let now = Instant::now();
        let candidate = ExecutionDeadline {
            started_at: now,
            duration: Duration::from_millis(timeout_ms),
            timeout_ms,
            scope,
        };
        let deadline = match self.deadline {
            Some(existing) if candidate.duration < existing.remaining_at(now) => Some(candidate),
            Some(existing) => Some(existing),
            None => Some(candidate),
        };
        Self {
            cancellation: Arc::clone(&self.cancellation),
            deadline,
        }
    }

    pub fn cancellation_reason(&self) -> Option<CancellationReason> {
        *self
            .cancellation
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation_reason().is_some()
    }

    pub fn is_expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| deadline.remaining_at(Instant::now()).is_zero())
    }

    pub fn timeout(&self) -> Option<(TimeoutScope, u64)> {
        self.deadline
            .map(|deadline| (deadline.scope, deadline.timeout_ms))
    }

    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.remaining_at(Instant::now()))
    }

    pub async fn cancelled(&self) -> CancellationReason {
        loop {
            let changed = self.cancellation.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(reason) = self.cancellation_reason() {
                return reason;
            }
            changed.await;
        }
    }

    pub(crate) async fn deadline_elapsed(&self) {
        match self.deadline {
            Some(deadline) => loop {
                let remaining = deadline.remaining_at(Instant::now());
                if remaining.is_zero() {
                    return;
                }
                tokio::time::sleep(remaining.min(MAX_TIMER_SLICE)).await;
            },
            None => pending::<()>().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{CancellationReason, ExecutionController, TimeoutScope};

    #[tokio::test]
    async fn first_cancellation_reason_wins_and_wakes_clones() {
        let (controller, control) = ExecutionController::new();
        let waiter = tokio::spawn({
            let control = control.clone();
            async move { control.cancelled().await }
        });
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(!controller.cancel(CancellationReason::Shutdown));
        assert_eq!(waiter.await.expect("waiter"), CancellationReason::Requested);
        assert_eq!(
            control.cancellation_reason(),
            Some(CancellationReason::Requested)
        );
    }

    #[tokio::test]
    async fn derived_action_timeout_can_only_shorten_a_request() {
        let (_, request) = ExecutionController::with_timeout(5_000, TimeoutScope::Request);
        let action = request.with_timeout(10, TimeoutScope::Action);
        assert_eq!(action.timeout(), Some((TimeoutScope::Action, 10)));
        assert!(action.remaining().expect("deadline") <= Duration::from_millis(10));

        let longer = action.with_timeout(60_000, TimeoutScope::Request);
        assert_eq!(longer.timeout(), Some((TimeoutScope::Action, 10)));
    }

    #[tokio::test]
    async fn very_large_timeout_remains_bounded_and_can_be_shortened() {
        let (_, request) = ExecutionController::with_timeout(u64::MAX, TimeoutScope::Request);
        assert_eq!(request.timeout(), Some((TimeoutScope::Request, u64::MAX)));
        assert!(!request.is_expired());
        assert!(
            request
                .remaining()
                .is_some_and(|value| value > Duration::from_secs(60))
        );

        let action = request.with_timeout(10, TimeoutScope::Action);
        assert_eq!(action.timeout(), Some((TimeoutScope::Action, 10)));
    }
}
