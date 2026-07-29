use std::{future::Future, time::Duration};

use devicerail_core::{DriverError, DriverResult, ExecutionControl};

pub(crate) fn ensure_active(control: &ExecutionControl) -> DriverResult<()> {
    if control.is_cancelled() {
        Err(DriverError::Cancelled)
    } else if control.is_expired() {
        Err(DriverError::TimedOut)
    } else {
        Ok(())
    }
}

/// Applies both Core's cancellation/deadline and an independent adapter
/// timeout. Dropping `future` must cancel its I/O; Tokio socket operations meet
/// that requirement because this crate never detaches them into tasks.
pub(crate) async fn run_controlled<T, F>(
    control: &ExecutionControl,
    adapter_timeout_ms: u64,
    adapter_timeout_code: &'static str,
    future: F,
) -> DriverResult<T>
where
    F: Future<Output = DriverResult<T>>,
{
    ensure_active(control)?;
    let adapter_timeout = Duration::from_millis(adapter_timeout_ms);
    let caller_remaining = control.remaining();
    let caller_deadline_wins = caller_remaining.is_some_and(|value| value <= adapter_timeout);
    let effective_timeout = caller_remaining
        .map(|value| value.min(adapter_timeout))
        .unwrap_or(adapter_timeout);
    let deadline = tokio::time::sleep(effective_timeout);
    tokio::pin!(deadline);
    tokio::pin!(future);
    tokio::select! {
        biased;
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = &mut deadline => {
            if caller_deadline_wins || control.is_expired() {
                Err(DriverError::TimedOut)
            } else {
                Err(DriverError::Platform {
                    code: adapter_timeout_code.to_owned(),
                    retryable: true,
                })
            }
        }
        result = &mut future => result,
    }
}

pub(crate) fn platform(code: &'static str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}
