//! Cancellation-aware isolation for bounded CPU-heavy Driver work.

use std::sync::{Arc, LazyLock};

use tokio::sync::Semaphore;

use crate::{DriverError, DriverResult, ExecutionControl};

// Screenshot validators may each reserve hundreds of MiB of decoder memory.
// A process-wide gate prevents independently routed Drivers from filling
// Tokio's much larger blocking pool with those allocations.
const MAX_CONCURRENT_BLOCKING_DRIVER_TASKS: usize = 2;

static BLOCKING_DRIVER_GATE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_BLOCKING_DRIVER_TASKS)));

/// Runs pure, CPU-heavy Driver work outside Tokio's async worker pool.
///
/// Waiting for capacity and waiting for the result both honor the supplied
/// cancellation signal and deadline. Tokio cannot stop a blocking closure
/// after it starts, so the acquired permit is owned by that closure and remains
/// held until it actually exits even when the caller has already cancelled.
/// Callers must therefore use this only for bounded work without external side
/// effects.
///
/// `join_error` preserves the caller's stable platform classification if the
/// blocking task cannot return its normal result (for example after a panic).
pub async fn run_bounded_blocking<T, F, J>(
    control: &ExecutionControl,
    work: F,
    join_error: J,
) -> DriverResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> DriverResult<T> + Send + 'static,
    J: Fn() -> DriverError + Send + Sync,
{
    run_bounded_blocking_with_gate(Arc::clone(&BLOCKING_DRIVER_GATE), control, work, join_error)
        .await
}

async fn run_bounded_blocking_with_gate<T, F, J>(
    gate: Arc<Semaphore>,
    control: &ExecutionControl,
    work: F,
    join_error: J,
) -> DriverResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> DriverResult<T> + Send + 'static,
    J: Fn() -> DriverError + Send + Sync,
{
    ensure_active(control)?;
    let permit = tokio::select! {
        biased;
        _ = control.cancelled() => return Err(DriverError::Cancelled),
        _ = control.deadline_elapsed() => return Err(DriverError::TimedOut),
        permit = gate.acquire_owned() => permit.map_err(|_| join_error())?,
    };
    ensure_active(control)?;

    let mut task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    tokio::select! {
        biased;
        _ = control.cancelled() => {
            task.abort();
            Err(DriverError::Cancelled)
        }
        _ = control.deadline_elapsed() => {
            task.abort();
            Err(DriverError::TimedOut)
        }
        result = &mut task => result.unwrap_or_else(|_| Err(join_error())),
    }
}

fn ensure_active(control: &ExecutionControl) -> DriverResult<()> {
    if control.is_cancelled() {
        Err(DriverError::Cancelled)
    } else if control.is_expired() {
        Err(DriverError::TimedOut)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    use tokio::sync::{Semaphore, oneshot};

    use super::run_bounded_blocking_with_gate;
    use crate::{
        CancellationReason, DriverError, ExecutionControl, ExecutionController, TimeoutScope,
    };

    fn join_error() -> DriverError {
        DriverError::Platform {
            code: "stable_blocking_failure".to_owned(),
            retryable: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_work_does_not_stall_the_async_worker() {
        let gate = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let task = tokio::spawn(async move {
            run_bounded_blocking_with_gate(
                gate,
                &ExecutionControl::unbounded(),
                move || {
                    let _ = started_tx.send(());
                    release_rx.recv().expect("release blocking task");
                    Ok(7_u8)
                },
                join_error,
            )
            .await
        });

        started_rx.await.expect("blocking task started");
        tokio::time::timeout(Duration::from_millis(100), tokio::task::yield_now())
            .await
            .expect("async worker remained responsive");
        release_tx.send(()).expect("release blocking task");
        assert_eq!(task.await.expect("task joined").expect("work succeeded"), 7);
    }

    #[tokio::test]
    async fn cancellation_keeps_capacity_reserved_until_blocking_work_exits() {
        let gate = Arc::new(Semaphore::new(1));
        let (controller, control) = ExecutionController::new();
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (first_release_tx, first_release_rx) = mpsc::channel();
        let first_gate = Arc::clone(&gate);
        let first = tokio::spawn(async move {
            run_bounded_blocking_with_gate(
                first_gate,
                &control,
                move || {
                    let _ = first_started_tx.send(());
                    first_release_rx.recv().expect("release first task");
                    Ok(())
                },
                join_error,
            )
            .await
        });
        first_started_rx.await.expect("first task started");
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            first.await.expect("first task joined"),
            Err(DriverError::Cancelled)
        ));

        let (second_started_tx, mut second_started_rx) = oneshot::channel();
        let second = tokio::spawn(async move {
            run_bounded_blocking_with_gate(
                gate,
                &ExecutionControl::unbounded(),
                move || {
                    let _ = second_started_tx.send(());
                    Ok(())
                },
                join_error,
            )
            .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut second_started_rx)
                .await
                .is_err(),
            "detached blocking work must retain its permit"
        );
        first_release_tx.send(()).expect("release first task");
        second_started_rx.await.expect("second task started");
        second
            .await
            .expect("second task joined")
            .expect("second task succeeded");
    }

    #[tokio::test]
    async fn deadline_and_join_failure_keep_stable_error_semantics() {
        let gate = Arc::new(Semaphore::new(1));
        let (_controller, control) = ExecutionController::with_timeout(1, TimeoutScope::Request);
        tokio::time::sleep(Duration::from_millis(2)).await;
        assert!(matches!(
            run_bounded_blocking_with_gate(gate, &control, || Ok(()), join_error).await,
            Err(DriverError::TimedOut)
        ));

        let error = run_bounded_blocking_with_gate(
            Arc::new(Semaphore::new(1)),
            &ExecutionControl::unbounded(),
            || -> Result<(), DriverError> { panic!("test blocking task panic") },
            join_error,
        )
        .await
        .expect_err("join failure");
        assert!(matches!(
            error,
            DriverError::Platform { code, retryable: false }
                if code == "stable_blocking_failure"
        ));
    }
}
