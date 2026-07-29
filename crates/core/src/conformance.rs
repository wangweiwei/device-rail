//! Reusable behavioral contract tests for [`DeviceDriver`](crate::DeviceDriver)
//! implementations.
//!
//! A downstream driver supplies two factories: one creates a fresh driver for
//! this test run, and one creates a valid [`ActionCall`] for every advertised
//! capability. Keeping construction behind a factory prevents test instances
//! from sharing connection state when Cargo runs different driver suites in
//! parallel.
//! Run executable action cases against a dedicated disposable test target; the
//! suite intentionally exercises every advertised capability and derived
//! negative argument cases.
//!
//! Enable the `conformance-json-schema` feature in driver test builds to
//! compile every capability schema and validate the factory calls against it.
//! The feature is intentionally off by default so the device kernel does not
//! carry a JSON Schema engine in production.

use std::{collections::HashSet, sync::Arc, time::Duration};

use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionExecution, ActionOutcome, ActionProtection, ActionResult,
    AssetRef, CLEAR_ELEMENT_ACTION, DeviceId, DeviceInfo, ElementActionOutput, ErrorInfo,
    EventSequence, FIND_ELEMENT_ACTION, FindElementResult, Observation, RecordedActionCall,
    SET_ELEMENT_VALUE_ACTION, ScreenshotOmissionReason, SessionId, SessionOutcome, SessionState,
    TAP_ELEMENT_ACTION, TestEvent, TestEventPayload, UiContextRef, UiNodeRef,
    WAIT_FOR_ELEMENT_ACTION, WaitForElementResult, is_semantic_action_name,
};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    DeviceDriver, DeviceRuntime, DriverError, EndSession, EventStoreError, EvidenceError,
    EvidenceStore, ExecutionControl, MemoryEventStore, OperationContext, RuntimeError,
    RuntimeResult, ScreenshotPolicy, SessionEventStore, StartSession, UnavailableEvidenceStore,
    now_ms,
};

/// Builds one valid call for an advertised capability.
///
/// A driver suite should treat a capability without a factory call as missing
/// conformance coverage, rather than silently skipping it.
pub trait ConformanceActionFactory: Send + Sync {
    fn valid_call(&self, action: &ActionDefinition) -> Result<ActionCall, String>;
}

impl<F> ConformanceActionFactory for F
where
    F: Fn(&ActionDefinition) -> Result<ActionCall, String> + Send + Sync,
{
    fn valid_call(&self, action: &ActionDefinition) -> Result<ActionCall, String> {
        self(action)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConformanceReport {
    pub device_id: DeviceId,
    pub capability_count: usize,
    pub executed_action_count: usize,
    /// True when schemas were checked against their meta-schema, compiled, and
    /// used to validate calls by the optional `jsonschema` dependency.
    pub full_json_schema_validation: bool,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("driver conformance check `{check}` failed: {message}")]
pub struct ConformanceFailure {
    pub check: &'static str,
    pub message: String,
}

impl ConformanceFailure {
    fn new(check: &'static str, message: impl Into<String>) -> Self {
        Self {
            check,
            message: message.into(),
        }
    }
}

/// Time limits for the test harness itself.
///
/// The suite timeout prevents a broken Driver from hanging CI forever. Cleanup
/// stages each get a separate budget and are always attempted, including after
/// failure or cancellation of the main suite. Applying the budget separately
/// ensures a hung disconnect cannot prevent Session/Evidence cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConformanceOptions {
    pub suite_timeout: Duration,
    pub cleanup_timeout: Duration,
}

impl Default for ConformanceOptions {
    fn default() -> Self {
        Self {
            suite_timeout: Duration::from_secs(120),
            cleanup_timeout: Duration::from_secs(15),
        }
    }
}

/// Runs the complete driver contract against one freshly constructed driver.
///
/// The contract defines `connect` and `disconnect` as idempotent, permits
/// capability discovery while disconnected, and requires `observe` and
/// `execute` to reject disconnected use with [`DriverError::NotConnected`].
pub async fn run_driver_conformance<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
) -> Result<ConformanceReport, ConformanceFailure>
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    run_driver_conformance_with_options(
        driver_factory,
        action_factory,
        ConformanceOptions::default(),
    )
    .await
}

/// Runs the suite with explicit harness and cleanup time limits.
pub async fn run_driver_conformance_with_options<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
    options: ConformanceOptions,
) -> Result<ConformanceReport, ConformanceFailure>
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    run_driver_conformance_with_policy_and_options(
        driver_factory,
        action_factory,
        Arc::new(UnavailableEvidenceStore),
        options,
        false,
    )
    .await
}

/// Runs the complete contract with an Evidence Store available to Driver
/// operations. This form strictly reconciles each successful result with the
/// receipts issued by its operation writer. Before returning, the harness
/// removes its disposable Session log and releases every Store reference that
/// Session acquired. Existing Drivers that do not produce Store-owned evidence
/// can continue using [`run_driver_conformance`].
pub async fn run_driver_conformance_with_evidence<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
    evidence: Arc<dyn EvidenceStore>,
) -> Result<ConformanceReport, ConformanceFailure>
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    run_driver_conformance_with_evidence_and_options(
        driver_factory,
        action_factory,
        evidence,
        ConformanceOptions::default(),
    )
    .await
}

/// Runs the suite with both an injected Evidence Store and explicit harness
/// time limits.
pub async fn run_driver_conformance_with_evidence_and_options<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
    evidence: Arc<dyn EvidenceStore>,
    options: ConformanceOptions,
) -> Result<ConformanceReport, ConformanceFailure>
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    run_driver_conformance_with_policy_and_options(
        driver_factory,
        action_factory,
        evidence,
        options,
        true,
    )
    .await
}

async fn run_driver_conformance_with_policy_and_options<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
    evidence: Arc<dyn EvidenceStore>,
    options: ConformanceOptions,
    strict_evidence_receipts: bool,
) -> Result<ConformanceReport, ConformanceFailure>
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    let driver = Arc::new(driver_factory());
    let events = Arc::new(MemoryEventStore::default());
    let task_driver = Arc::clone(&driver);
    let task_events = Arc::clone(&events);
    let task_evidence = Arc::clone(&evidence);
    let mut task = tokio::spawn(async move {
        let runtime = if strict_evidence_receipts {
            DeviceRuntime::with_evidence(
                Arc::clone(&task_driver),
                Arc::clone(&task_events),
                Arc::clone(&task_evidence),
            )
        } else {
            DeviceRuntime::new(Arc::clone(&task_driver), Arc::clone(&task_events))
        };
        let omit_runtime = if strict_evidence_receipts {
            DeviceRuntime::with_evidence(task_driver, Arc::clone(&task_events), task_evidence)
        } else {
            DeviceRuntime::new(task_driver, Arc::clone(&task_events))
        }
        .with_screenshot_policy(ScreenshotPolicy::Omit);
        run_driver_conformance_inner(&runtime, &omit_runtime, &task_events, &action_factory).await
    });
    let suite = match tokio::time::timeout(options.suite_timeout, &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(ConformanceFailure::new(
            "suite_panic",
            format!("driver suite task failed: {error}"),
        )),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(ConformanceFailure::new(
                "suite_timeout",
                format!(
                    "driver suite exceeded {} ms",
                    options.suite_timeout.as_millis()
                ),
            ))
        }
    };
    let suite_succeeded = suite.is_ok();

    let cleanup_control = ExecutionControl::unbounded();
    let disconnect_cleanup =
        match tokio::time::timeout(options.cleanup_timeout, driver.disconnect(&cleanup_control))
            .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(driver_failure("cleanup_disconnect", error)),
            Err(_) => Err(ConformanceFailure::new(
                "cleanup_timeout",
                format!(
                    "cleanup disconnect exceeded {} ms",
                    options.cleanup_timeout.as_millis()
                ),
            )),
        };

    // The suite task has completed, panicked, or was explicitly aborted and
    // awaited before this point. It no longer owns a Runtime, Event Store, or
    // operation-scoped Evidence writer. That ordering is essential for the
    // forced-drop path below: a timed-out observation/action may have left
    // in-memory in-flight bookkeeping that cannot be ended normally.
    let store_evidence = strict_evidence_receipts.then_some(evidence);
    let store_cleanup = match tokio::time::timeout(
        options.cleanup_timeout,
        cleanup_harness_sessions(events, store_evidence, suite_succeeded),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(ConformanceFailure::new(
            "cleanup_timeout",
            format!(
                "Session and evidence cleanup exceeded {} ms",
                options.cleanup_timeout.as_millis()
            ),
        )),
    };

    combine_suite_and_cleanup(suite, [disconnect_cleanup, store_cleanup])
}

async fn cleanup_harness_sessions(
    events: Arc<MemoryEventStore>,
    evidence: Option<Arc<dyn EvidenceStore>>,
    suite_succeeded: bool,
) -> Result<(), ConformanceFailure> {
    let sessions = events
        .list_sessions()
        .await
        .map_err(|error| event_store_failure("cleanup_list_sessions", error))?;
    let session_ids = sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<Vec<_>>();
    let mut failures = Vec::new();

    for session in sessions {
        let can_delete = if session.state == SessionState::Ended {
            true
        } else {
            match events
                .end_session(EndSession {
                    session_id: session.id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    outcome: if suite_succeeded {
                        SessionOutcome::Completed
                    } else {
                        SessionOutcome::Failed
                    },
                    reason: None,
                })
                .await
            {
                Ok(_) => true,
                // Aborting a suite drops the operation future before its
                // asynchronous lease/action finalizer can run. The harness
                // owns this MemoryEventStore exclusively, so these states are
                // cleaned by dropping the event log rather than fabricating a
                // SessionEnded event that never occurred.
                Err(
                    EventStoreError::ActionsInFlight { .. }
                    | EventStoreError::ObservationsInFlight { .. },
                ) => false,
                Err(error) => {
                    failures.push(event_store_failure("cleanup_end_session", error));
                    false
                }
            }
        };

        if can_delete && let Err(error) = events.delete_ended(&session.id).await {
            failures.push(event_store_failure("cleanup_delete_session", error));
        }
    }

    // Event references must disappear before their corresponding Evidence
    // pins are released. On the normal path every ended log was deleted
    // above. On abort, dropping the harness's last Event Store ownership is
    // the explicit fallback for a log that still contains in-flight state.
    drop(events);

    if let Some(evidence) = evidence {
        let released_at_ms = now_ms();
        for session_id in session_ids {
            if let Err(error) = evidence.release_session(&session_id, released_at_ms).await {
                failures.push(cleanup_evidence_failure(&session_id, error));
            }
        }
    }

    combine_cleanup_failures(failures)
}

fn cleanup_evidence_failure(session_id: &SessionId, error: EvidenceError) -> ConformanceFailure {
    let public = error.to_error_info();
    ConformanceFailure::new(
        "cleanup_evidence",
        format!(
            "failed to release conformance Session {session_id}: {} ({})",
            public.message, public.code
        ),
    )
}

fn combine_cleanup_failures(failures: Vec<ConformanceFailure>) -> Result<(), ConformanceFailure> {
    let mut failures = failures.into_iter();
    let Some(mut combined) = failures.next() else {
        return Ok(());
    };
    for failure in failures {
        combined
            .message
            .push_str(&format!("; cleanup also failed: {failure}"));
    }
    Err(combined)
}

fn combine_suite_and_cleanup<T, const N: usize>(
    suite: Result<T, ConformanceFailure>,
    cleanups: [Result<(), ConformanceFailure>; N],
) -> Result<T, ConformanceFailure> {
    let cleanup = combine_cleanup_failures(cleanups.into_iter().filter_map(Result::err).collect());
    match (suite, cleanup) {
        (Ok(report), Ok(())) => Ok(report),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(failure), Ok(())) => Err(failure),
        (Err(mut failure), Err(cleanup)) => {
            failure
                .message
                .push_str(&format!("; cleanup also failed: {cleanup}"));
            Err(failure)
        }
    }
}

async fn run_driver_conformance_inner<D, AF>(
    runtime: &DeviceRuntime<D, MemoryEventStore>,
    omit_runtime: &DeviceRuntime<D, MemoryEventStore>,
    events: &MemoryEventStore,
    action_factory: &AF,
) -> Result<ConformanceReport, ConformanceFailure>
where
    D: DeviceDriver + 'static,
    AF: ConformanceActionFactory,
{
    let device_id = runtime.device_id().clone();
    let control = ExecutionControl::unbounded();
    require(
        "stable_id",
        !device_id.0.trim().is_empty(),
        "DeviceDriver::id() must not be empty",
    )?;
    require(
        "stable_id",
        runtime.device_id() == &device_id,
        "DeviceDriver::id() changed between consecutive reads",
    )?;

    runtime
        .disconnect(&control)
        .await
        .map_err(|error| runtime_failure("idempotent_disconnect", error))?;

    runtime
        .health_check(&control)
        .await
        .map_err(|error| runtime_failure("disconnected_health_check", error))?;

    let capabilities = runtime
        .capabilities(&control)
        .await
        .map_err(|error| runtime_failure("disconnected_capabilities", error))?;
    require(
        "capabilities",
        !capabilities.is_empty(),
        "a driver must advertise at least one action",
    )?;

    let calls = prepare_capabilities(runtime, &capabilities, action_factory)?;
    let probe_call = ActionCall {
        id: Uuid::new_v4(),
        ..calls[0].clone()
    };
    let session = events
        .start_session(StartSession::new(None, Some(device_id.clone()), now_ms()))
        .await
        .map_err(|error| event_store_failure("start_session", error))?;
    // Exercise the complete Protocol 1.5 Driver surface. Feature flags remain
    // compatibility-safe by default in production, while conformance must
    // reach advertised semantic Actions instead of being rejected by Core's
    // opt-in gate before the Driver is called.
    let context = OperationContext::new(session.id.clone(), None)
        .with_ui_snapshots_enabled(true)
        .with_semantic_actions_enabled(true);
    let mut cursor = session.last_sequence;

    let error = expect_error(
        "observe_before_connect",
        runtime.observe(&context).await,
        "observe unexpectedly succeeded before connect",
    )?;
    let error_info = require_not_connected("observe_before_connect", &error, &device_id)?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_error_event("observe_before_connect_events", &recorded, &error_info)?;

    let error = expect_error(
        "execute_before_connect",
        runtime.execute(&context, probe_call.clone()).await,
        "execute unexpectedly succeeded before connect",
    )?;
    let error_info = require_not_connected("execute_before_connect", &error, &device_id)?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_failed_action_events(
        "execute_before_connect_events",
        &recorded,
        &probe_call,
        Some(capabilities[0].protection),
        &error_info,
    )?;

    let first_info = runtime
        .connect(&control)
        .await
        .map_err(|error| runtime_failure("connect", error))?;
    validate_connected_info("connect_info", &first_info, &device_id)?;
    require(
        "stable_id",
        runtime.device_id() == &device_id,
        "DeviceDriver::id() changed after connect",
    )?;

    let repeated_info = runtime
        .connect(&control)
        .await
        .map_err(|error| runtime_failure("idempotent_connect", error))?;
    validate_connected_info("idempotent_connect", &repeated_info, &device_id)?;
    require(
        "idempotent_connect",
        repeated_info == first_info,
        format!(
            "repeated connect returned different DeviceInfo: first={first_info:?}, repeated={repeated_info:?}"
        ),
    )?;

    let observation = runtime
        .observe(&context)
        .await
        .map_err(|error| runtime_failure("observe_after_connect", error))?;
    validate_observation("observation", &observation, &device_id, None)?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_observation_event("observe_success_events", &recorded, &observation)?;

    for (definition, call) in capabilities.iter().zip(&calls) {
        for (case, invalid_call) in invalid_argument_calls(definition, call)? {
            let error = expect_error(
                "invalid_arguments",
                runtime.execute(&context, invalid_call.clone()).await,
                format!(
                    "action `{}` accepted invalid arguments ({case})",
                    definition.name
                ),
            )?;
            let error_info =
                require_invalid_arguments("invalid_arguments", &error, definition.name.as_str())?;
            let recorded = events_after(events, &context, &mut cursor).await?;
            require_failed_action_events(
                "invalid_arguments_events",
                &recorded,
                &invalid_call,
                Some(definition.protection),
                &error_info,
            )?;
        }

        let result = runtime
            .execute(&context, call.clone())
            .await
            .map_err(|error| {
                ConformanceFailure::new(
                    "execute_valid_action",
                    format!(
                        "valid factory call for `{}` failed: {error}",
                        definition.name
                    ),
                )
            })?;
        validate_action_result(
            definition,
            call,
            &result,
            &device_id,
            (definition.protection == ActionProtection::Protected)
                .then_some(ScreenshotOmissionReason::ProtectedAction),
        )?;
        let recorded = events_after(events, &context, &mut cursor).await?;
        require_successful_action_events(
            "execute_success_events",
            &recorded,
            call,
            Some(definition.protection),
            &result,
        )?;
    }

    let omitted_observation = omit_runtime
        .observe(&context)
        .await
        .map_err(|error| runtime_failure("observe_omit_policy", error))?;
    validate_observation(
        "observe_omit_policy",
        &omitted_observation,
        &device_id,
        Some(ScreenshotOmissionReason::Policy),
    )?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_observation_event(
        "observe_omit_policy_events",
        &recorded,
        &omitted_observation,
    )?;

    if let Some((definition, call)) = capabilities
        .iter()
        .zip(&calls)
        .find(|(definition, _)| definition.protection == ActionProtection::Standard)
    {
        let omit_call = ActionCall {
            id: Uuid::new_v4(),
            ..call.clone()
        };
        let result = omit_runtime
            .execute(&context, omit_call.clone())
            .await
            .map_err(|error| runtime_failure("execute_omit_policy", error))?;
        validate_action_result(
            definition,
            &omit_call,
            &result,
            &device_id,
            Some(ScreenshotOmissionReason::Policy),
        )?;
        let recorded = events_after(events, &context, &mut cursor).await?;
        require_successful_action_events(
            "execute_omit_policy_events",
            &recorded,
            &omit_call,
            Some(ActionProtection::Standard),
            &result,
        )?;
    }

    let unknown_name = unused_action_name(&capabilities);
    let unknown_call = ActionCall {
        id: Uuid::new_v4(),
        name: unknown_name.clone(),
        arguments: json!({ "secretSentinel": "DEVICERAIL_UNKNOWN_SECRET_SENTINEL" }),
    };
    require(
        "unknown_action_protection",
        runtime.action_protection(&unknown_name).is_none(),
        format!("unknown action `{unknown_name}` must return no protection classification"),
    )?;
    let error = expect_error(
        "unknown_action",
        runtime.execute(&context, unknown_call.clone()).await,
        "an unadvertised action unexpectedly succeeded",
    )?;
    let error_info = require_unknown_action("unknown_action", &error, &unknown_name)?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_failed_action_events(
        "unknown_action_events",
        &recorded,
        &unknown_call,
        None,
        &error_info,
    )?;

    runtime
        .disconnect(&control)
        .await
        .map_err(|error| runtime_failure("disconnect", error))?;
    require(
        "stable_id",
        runtime.device_id() == &device_id,
        "DeviceDriver::id() changed after disconnect",
    )?;

    let error = expect_error(
        "observe_after_disconnect",
        runtime.observe(&context).await,
        "observe unexpectedly succeeded after disconnect",
    )?;
    let error_info = require_not_connected("observe_after_disconnect", &error, &device_id)?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_error_event("observe_after_disconnect_events", &recorded, &error_info)?;

    let probe_call = ActionCall {
        id: Uuid::new_v4(),
        ..calls[0].clone()
    };
    let error = expect_error(
        "execute_after_disconnect",
        runtime.execute(&context, probe_call.clone()).await,
        "execute unexpectedly succeeded after disconnect",
    )?;
    let error_info = require_not_connected("execute_after_disconnect", &error, &device_id)?;
    let recorded = events_after(events, &context, &mut cursor).await?;
    require_failed_action_events(
        "execute_after_disconnect_events",
        &recorded,
        &probe_call,
        Some(capabilities[0].protection),
        &error_info,
    )?;

    runtime
        .disconnect(&control)
        .await
        .map_err(|error| runtime_failure("idempotent_disconnect", error))?;

    Ok(ConformanceReport {
        device_id,
        capability_count: capabilities.len(),
        executed_action_count: calls.len(),
        full_json_schema_validation: cfg!(feature = "conformance-json-schema"),
    })
}

/// Runs the suite and panics with a check-specific message on failure.
pub async fn assert_driver_conformance<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
) -> ConformanceReport
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    run_driver_conformance(driver_factory, action_factory)
        .await
        .unwrap_or_else(|error| panic!("{error}"))
}

/// Runs the suite with an injected Evidence Store and panics with a
/// check-specific message on failure.
pub async fn assert_driver_conformance_with_evidence<D, DF, AF>(
    driver_factory: DF,
    action_factory: AF,
    evidence: Arc<dyn EvidenceStore>,
) -> ConformanceReport
where
    D: DeviceDriver + 'static,
    DF: FnOnce() -> D,
    AF: ConformanceActionFactory + 'static,
{
    run_driver_conformance_with_evidence(driver_factory, action_factory, evidence)
        .await
        .unwrap_or_else(|error| panic!("{error}"))
}

fn prepare_capabilities<D: DeviceDriver + ?Sized, AF: ConformanceActionFactory>(
    runtime: &DeviceRuntime<D, MemoryEventStore>,
    capabilities: &[ActionDefinition],
    action_factory: &AF,
) -> Result<Vec<ActionCall>, ConformanceFailure> {
    let mut names = HashSet::new();
    let mut call_ids = HashSet::new();
    let mut calls = Vec::with_capacity(capabilities.len());

    for action in capabilities {
        require(
            "capability_name",
            !action.name.trim().is_empty(),
            "capability name must not be empty",
        )?;
        require(
            "capability_name",
            names.insert(action.name.as_str()),
            format!("duplicate capability name `{}`", action.name),
        )?;
        require(
            "capability_description",
            !action.description.trim().is_empty(),
            format!("capability `{}` has an empty description", action.name),
        )?;
        validate_schema_shape(action)?;
        validate_canonical_semantic_schema(action)?;
        require(
            "capability_protection",
            runtime.action_protection(&action.name) == Some(action.protection),
            format!(
                "capability `{}` declares {:?}, but DeviceDriver::action_protection returned {:?}",
                action.name,
                action.protection,
                runtime.action_protection(&action.name)
            ),
        )?;

        let call = action_factory.valid_call(action).map_err(|message| {
            ConformanceFailure::new(
                "action_factory",
                format!("no valid call for `{}`: {message}", action.name),
            )
        })?;
        require(
            "action_factory",
            call.name == action.name,
            format!(
                "factory returned action `{}` for capability `{}`",
                call.name, action.name
            ),
        )?;
        require(
            "action_factory",
            !call.id.is_nil(),
            format!("factory returned a nil call id for `{}`", action.name),
        )?;
        require(
            "action_factory",
            call_ids.insert(call.id),
            format!("factory reused call id {}", call.id),
        )?;
        require(
            "action_factory",
            call.arguments.is_object(),
            format!("factory arguments for `{}` must be an object", action.name),
        )?;
        validate_schema_instance(action, &call.arguments)?;
        calls.push(call);
    }

    Ok(calls)
}

fn invalid_argument_calls(
    action: &ActionDefinition,
    valid_call: &ActionCall,
) -> Result<Vec<(String, ActionCall)>, ConformanceFailure> {
    let schema = action
        .input_schema
        .as_object()
        .expect("schema shape was validated before invalid calls are derived");
    let valid = valid_call
        .arguments
        .as_object()
        .expect("factory arguments were validated as an object");
    let mut cases = vec![("non-object root".to_owned(), Value::Null)];

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if valid.contains_key(name) {
                let mut missing = valid.clone();
                missing.remove(name);
                cases.push((
                    format!("missing required property `{name}`"),
                    Value::Object(missing),
                ));
            }
        }
    }

    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        let mut extra = valid.clone();
        let mut name = "__devicerailUnexpected".to_owned();
        while extra.contains_key(&name) {
            name.push('_');
        }
        extra.insert(name.clone(), Value::Bool(true));
        cases.push((
            format!("unexpected property `{name}`"),
            Value::Object(extra),
        ));
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property_schema) in properties {
            if let Some(wrong) = wrong_type_value(property_schema) {
                let mut arguments = valid.clone();
                arguments.insert(name.clone(), wrong);
                cases.push((format!("wrong type for `{name}`"), Value::Object(arguments)));
            }
            for (boundary, invalid) in invalid_numeric_boundaries(property_schema) {
                let mut arguments = valid.clone();
                arguments.insert(name.clone(), invalid);
                cases.push((format!("{boundary} for `{name}`"), Value::Object(arguments)));
            }
        }
    }

    let mut unique = HashSet::new();
    cases.retain(|(_, arguments)| {
        unique.insert(serde_json::to_string(arguments).expect("arguments serialize"))
    });

    #[cfg(feature = "conformance-json-schema")]
    {
        let validator = jsonschema::validator_for(&action.input_schema).map_err(|error| {
            ConformanceFailure::new(
                "invalid_argument_factory",
                format!(
                    "inputSchema for `{}` cannot be compiled: {error}",
                    action.name
                ),
            )
        })?;
        for (case, arguments) in &cases {
            require(
                "invalid_argument_factory",
                !validator.is_valid(arguments),
                format!(
                    "derived negative case `{case}` for `{}` still satisfies inputSchema",
                    action.name
                ),
            )?;
        }
    }

    Ok(cases
        .into_iter()
        .map(|(case, arguments)| {
            (
                case,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: valid_call.name.clone(),
                    arguments,
                },
            )
        })
        .collect())
}

fn wrong_type_value(schema: &Value) -> Option<Value> {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => Some(json!(1)),
        Some("number" | "integer") => Some(json!("not-a-number")),
        Some("boolean") => Some(json!("not-a-boolean")),
        Some("array") => Some(json!({})),
        Some("object") => Some(json!("not-an-object")),
        Some("null") => Some(Value::Bool(true)),
        _ => None,
    }
}

fn invalid_numeric_boundaries(schema: &Value) -> Vec<(&'static str, Value)> {
    let mut cases = Vec::new();
    if let Some(minimum) = schema.get("minimum").and_then(adjacent_number_below) {
        cases.push(("below minimum", minimum));
    }
    if let Some(maximum) = schema.get("maximum").and_then(adjacent_number_above) {
        cases.push(("above maximum", maximum));
    }
    if let Some(minimum) = schema.get("exclusiveMinimum") {
        cases.push(("at exclusive minimum", minimum.clone()));
    }
    if let Some(maximum) = schema.get("exclusiveMaximum") {
        cases.push(("at exclusive maximum", maximum.clone()));
    }
    cases
}

fn adjacent_number_below(value: &Value) -> Option<Value> {
    if let Some(value) = value.as_i64() {
        return value
            .checked_sub(1)
            .map(|value| Value::Number(value.into()));
    }
    if let Some(value) = value.as_u64() {
        return Some(if value == 0 {
            json!(-1)
        } else {
            Value::Number((value - 1).into())
        });
    }
    adjacent_float(value.as_f64()?, -1.0)
}

fn adjacent_number_above(value: &Value) -> Option<Value> {
    if let Some(value) = value.as_i64() {
        return value
            .checked_add(1)
            .map(|value| Value::Number(value.into()));
    }
    if let Some(value) = value.as_u64() {
        return value
            .checked_add(1)
            .map(|value| Value::Number(value.into()));
    }
    adjacent_float(value.as_f64()?, 1.0)
}

fn adjacent_float(value: f64, direction: f64) -> Option<Value> {
    let step = (value.abs() * f64::EPSILON * 2.0).max(f64::MIN_POSITIVE);
    let candidate = value + direction * step;
    serde_json::Number::from_f64(candidate).map(Value::Number)
}

fn validate_schema_shape(action: &ActionDefinition) -> Result<(), ConformanceFailure> {
    let schema = action.input_schema.as_object().ok_or_else(|| {
        ConformanceFailure::new(
            "capability_schema",
            format!("inputSchema for `{}` must be a JSON object", action.name),
        )
    })?;
    require(
        "capability_schema",
        schema.get("type").and_then(Value::as_str) == Some("object"),
        format!(
            "inputSchema for `{}` must declare `type: object`",
            action.name
        ),
    )?;
    if let Some(properties) = schema.get("properties") {
        require(
            "capability_schema",
            properties.is_object(),
            format!("`properties` for `{}` must be an object", action.name),
        )?;
    }
    if let Some(required) = schema.get("required") {
        let entries = required.as_array().ok_or_else(|| {
            ConformanceFailure::new(
                "capability_schema",
                format!("`required` for `{}` must be an array", action.name),
            )
        })?;
        let mut unique = HashSet::new();
        for entry in entries {
            let name = entry.as_str().ok_or_else(|| {
                ConformanceFailure::new(
                    "capability_schema",
                    format!("`required` entries for `{}` must be strings", action.name),
                )
            })?;
            require(
                "capability_schema",
                unique.insert(name),
                format!("duplicate required property `{name}` for `{}`", action.name),
            )?;
        }
    }
    if let Some(additional) = schema.get("additionalProperties") {
        require(
            "capability_schema",
            additional.is_boolean() || additional.is_object(),
            format!(
                "`additionalProperties` for `{}` must be a boolean or schema object",
                action.name
            ),
        )?;
    }
    if let Some(meta_schema) = schema.get("$schema") {
        require(
            "capability_schema",
            meta_schema.is_string(),
            format!("`$schema` for `{}` must be a string", action.name),
        )?;
    }

    #[cfg(feature = "conformance-json-schema")]
    {
        jsonschema::meta::validate(&action.input_schema).map_err(|error| {
            ConformanceFailure::new(
                "capability_schema",
                format!(
                    "inputSchema for `{}` is not a valid JSON Schema: {error}",
                    action.name
                ),
            )
        })?;
        jsonschema::validator_for(&action.input_schema).map_err(|error| {
            ConformanceFailure::new(
                "capability_schema",
                format!(
                    "inputSchema for `{}` cannot be compiled: {error}",
                    action.name
                ),
            )
        })?;
    }

    Ok(())
}

fn validate_schema_instance(
    action: &ActionDefinition,
    arguments: &Value,
) -> Result<(), ConformanceFailure> {
    #[cfg(feature = "conformance-json-schema")]
    {
        let validator = jsonschema::validator_for(&action.input_schema).map_err(|error| {
            ConformanceFailure::new(
                "action_factory",
                format!(
                    "inputSchema for `{}` cannot be compiled: {error}",
                    action.name
                ),
            )
        })?;
        validator.validate(arguments).map_err(|error| {
            ConformanceFailure::new(
                "action_factory",
                format!(
                    "factory arguments for `{}` do not satisfy inputSchema: {error}",
                    action.name
                ),
            )
        })?;
    }

    #[cfg(not(feature = "conformance-json-schema"))]
    let _ = (action, arguments);

    Ok(())
}

fn validate_connected_info(
    check: &'static str,
    info: &DeviceInfo,
    device_id: &DeviceId,
) -> Result<(), ConformanceFailure> {
    require(
        check,
        &info.id == device_id,
        format!("connect returned id {}, expected {device_id}", info.id),
    )?;
    require(check, info.connected, "connect returned connected=false")?;
    require(
        check,
        !info.name.trim().is_empty(),
        "connect returned an empty device name",
    )?;
    if let Some(version) = &info.os_version {
        require(
            check,
            !version.trim().is_empty(),
            "connect returned an empty osVersion",
        )?;
    }
    Ok(())
}

fn validate_action_result(
    definition: &ActionDefinition,
    call: &ActionCall,
    result: &ActionResult,
    device_id: &DeviceId,
    expected_omission: Option<ScreenshotOmissionReason>,
) -> Result<(), ConformanceFailure> {
    require(
        "action_result",
        result.call_id == call.id,
        format!(
            "result for `{}` has callId {}, expected {}",
            definition.name, result.call_id, call.id
        ),
    )?;
    require(
        "action_result",
        result.started_at_ms > 0,
        format!("result for `{}` has a zero start time", definition.name),
    )?;
    require(
        "action_result",
        result.finished_at_ms >= result.started_at_ms,
        format!(
            "result for `{}` finished before it started ({} < {})",
            definition.name, result.finished_at_ms, result.started_at_ms
        ),
    )?;
    if let Some(before) = &result.before {
        validate_observation("action_result_before", before, device_id, expected_omission)?;
    }
    if expected_omission == Some(ScreenshotOmissionReason::ProtectedAction) {
        require(
            "action_result_before",
            result.before.is_some(),
            format!(
                "protected result for `{}` must include a before observation",
                definition.name
            ),
        )?;
    }
    let after = result.after.as_ref().ok_or_else(|| {
        ConformanceFailure::new(
            "action_result_after",
            format!(
                "result for `{}` must include an after observation",
                definition.name
            ),
        )
    })?;
    validate_observation("action_result_after", after, device_id, expected_omission)?;
    require(
        "action_result_evidence",
        if expected_omission.is_some() {
            result.evidence.is_empty()
        } else {
            !result.evidence.is_empty()
        },
        if expected_omission.is_some() {
            format!(
                "screenshot-omitted result for `{}` must not include evidence",
                definition.name
            )
        } else {
            format!("result for `{}` must include evidence", definition.name)
        },
    )?;
    validate_assets("action_result_evidence", &result.evidence)?;
    validate_semantic_action_result(definition, result, expected_omission)?;
    Ok(())
}

fn validate_canonical_semantic_schema(action: &ActionDefinition) -> Result<(), ConformanceFailure> {
    let source = match action.name.as_str() {
        FIND_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/find-element-arguments.schema.json"
        )),
        TAP_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/tap-element-arguments.schema.json"
        )),
        CLEAR_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/clear-element-arguments.schema.json"
        )),
        SET_ELEMENT_VALUE_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/set-element-value-arguments.schema.json"
        )),
        WAIT_FOR_ELEMENT_ACTION => Some(include_str!(
            "../../../protocol/schema/v1/wait-for-element-arguments.schema.json"
        )),
        _ => None,
    };
    let Some(source) = source else {
        return Ok(());
    };
    let canonical = serde_json::from_str::<Value>(source).map_err(|error| {
        ConformanceFailure::new(
            "semantic_action_schema",
            format!("canonical schema for `{}` is invalid: {error}", action.name),
        )
    })?;
    require(
        "semantic_action_schema",
        action.input_schema == canonical,
        format!(
            "semantic capability `{}` must advertise the canonical generated input schema",
            action.name
        ),
    )
}

fn validate_semantic_action_result(
    definition: &ActionDefinition,
    result: &ActionResult,
    expected_omission: Option<ScreenshotOmissionReason>,
) -> Result<(), ConformanceFailure> {
    if !is_semantic_action_name(&definition.name) {
        return require(
            "action_execution",
            result.execution.is_none(),
            format!(
                "non-semantic result for `{}` returned semantic execution metadata",
                definition.name
            ),
        );
    }

    let execution = result.execution.as_ref().ok_or_else(|| {
        ConformanceFailure::new(
            "semantic_action_output",
            format!(
                "semantic result for `{}` omitted execution metadata",
                definition.name
            ),
        )
    })?;
    execution.validate().map_err(|error| {
        ConformanceFailure::new(
            "semantic_action_output",
            format!(
                "semantic result for `{}` has invalid execution metadata: {error}",
                definition.name
            ),
        )
    })?;
    let execution_context = match execution {
        ActionExecution::NativeSemantic { context }
        | ActionExecution::WebSemantic { context }
        | ActionExecution::CoordinateFallback { context, .. } => context,
    };
    let node = match definition.name.as_str() {
        FIND_ELEMENT_ACTION => Some(
            serde_json::from_value::<FindElementResult>(result.output.clone())
                .map_err(|error| semantic_output_error(definition, error))?
                .element,
        ),
        TAP_ELEMENT_ACTION | CLEAR_ELEMENT_ACTION | SET_ELEMENT_VALUE_ACTION => Some(
            serde_json::from_value::<ElementActionOutput>(result.output.clone())
                .map_err(|error| semantic_output_error(definition, error))?
                .element,
        ),
        WAIT_FOR_ELEMENT_ACTION => {
            let output = serde_json::from_value::<WaitForElementResult>(result.output.clone())
                .map_err(|error| semantic_output_error(definition, error))?;
            output.validate().map_err(|error| {
                ConformanceFailure::new(
                    "semantic_action_output",
                    format!(
                        "semantic result for `{}` violates its output contract: {error}",
                        definition.name
                    ),
                )
            })?;
            output.element
        }
        _ => unreachable!("semantic action name was checked above"),
    };

    let observations = result.before.iter().chain(result.after.iter());
    require(
        "semantic_action_output",
        observations.clone().any(|observation| {
            observation
                .ui_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.context == *execution_context)
                || (expected_omission.is_some()
                    && observation.id
                        == node
                            .as_ref()
                            .map_or(observation.id, |node| node.observation_id))
        }),
        format!(
            "semantic result for `{}` has no returned observation for its execution context",
            definition.name
        ),
    )?;

    if let Some(node) = node {
        validate_semantic_node_ref(
            definition,
            &node,
            execution_context,
            result,
            expected_omission,
        )?;
    }
    Ok(())
}

fn semantic_output_error(
    definition: &ActionDefinition,
    error: serde_json::Error,
) -> ConformanceFailure {
    ConformanceFailure::new(
        "semantic_action_output",
        format!(
            "semantic result for `{}` does not match its canonical output DTO: {error}",
            definition.name
        ),
    )
}

fn validate_semantic_node_ref(
    definition: &ActionDefinition,
    node: &UiNodeRef,
    execution_context: &UiContextRef,
    result: &ActionResult,
    expected_omission: Option<ScreenshotOmissionReason>,
) -> Result<(), ConformanceFailure> {
    node.validate().map_err(|error| {
        ConformanceFailure::new(
            "semantic_action_output",
            format!(
                "semantic result for `{}` returned an invalid node reference: {error}",
                definition.name
            ),
        )
    })?;
    require(
        "semantic_action_output",
        &node.context == execution_context,
        format!(
            "semantic result for `{}` returned a node from a different execution context",
            definition.name
        ),
    )?;
    let source = result
        .before
        .iter()
        .chain(result.after.iter())
        .find(|observation| observation.id == node.observation_id)
        .ok_or_else(|| {
            ConformanceFailure::new(
                "semantic_action_output",
                format!(
                    "semantic result for `{}` returned a node from an observation absent from the result",
                    definition.name
                ),
            )
        })?;
    if expected_omission.is_none() {
        require(
            "semantic_action_output",
            source
                .ui_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.context == node.context),
            format!(
                "semantic result for `{}` returned a node without durable UI Snapshot evidence",
                definition.name
            ),
        )?;
    }
    Ok(())
}

fn validate_observation(
    check: &'static str,
    observation: &Observation,
    device_id: &DeviceId,
    expected_omission: Option<ScreenshotOmissionReason>,
) -> Result<(), ConformanceFailure> {
    require(
        check,
        !observation.id.is_nil(),
        "observation id must not be nil",
    )?;
    require(
        check,
        &observation.device_id == device_id,
        format!(
            "observation device id {} does not match {device_id}",
            observation.device_id
        ),
    )?;
    require(
        check,
        observation.captured_at_ms > 0,
        "observation capturedAtMs must be positive",
    )?;
    require(
        check,
        observation.viewport.width > 0 && observation.viewport.height > 0,
        "observation viewport dimensions must be positive",
    )?;
    require(
        check,
        observation.viewport.scale_factor.is_finite() && observation.viewport.scale_factor > 0.0,
        "observation viewport scaleFactor must be finite and positive",
    )?;
    if let Some(screenshot) = &observation.screenshot {
        validate_assets(check, std::slice::from_ref(screenshot))?;
    }
    require(
        check,
        observation.screenshot_omission == expected_omission,
        format!(
            "observation screenshotOmission {:?} does not match expected {expected_omission:?}",
            observation.screenshot_omission
        ),
    )?;
    if expected_omission.is_some() {
        require(
            check,
            observation.screenshot.is_none(),
            "screenshot-omitted observation must not include a screenshot",
        )?;
    }
    Ok(())
}

fn validate_assets(check: &'static str, assets: &[AssetRef]) -> Result<(), ConformanceFailure> {
    let mut ids = HashSet::new();
    for asset in assets {
        require(
            check,
            !asset.id.trim().is_empty(),
            "evidence id must not be empty",
        )?;
        require(
            check,
            ids.insert(asset.id.as_str()),
            format!("duplicate evidence id `{}`", asset.id),
        )?;
        require(
            check,
            asset.media_type.contains('/') && !asset.media_type.contains(char::is_whitespace),
            format!("evidence `{}` has invalid mediaType", asset.id),
        )?;
        require(
            check,
            !asset.uri.trim().is_empty(),
            format!("evidence `{}` has an empty uri", asset.id),
        )?;
        if let Some(digest) = &asset.sha256 {
            require(
                check,
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                format!("evidence `{}` has an invalid sha256", asset.id),
            )?;
        }
    }
    Ok(())
}

fn require_not_connected(
    check: &'static str,
    error: &DriverError,
    device_id: &DeviceId,
) -> Result<ErrorInfo, ConformanceFailure> {
    match error {
        DriverError::NotConnected(actual) if actual == device_id => {}
        DriverError::NotConnected(actual) => {
            return Err(ConformanceFailure::new(
                check,
                format!("NotConnected referenced {actual}, expected {device_id}"),
            ));
        }
        other => {
            return Err(ConformanceFailure::new(
                check,
                format!("expected NotConnected, got {other:?}"),
            ));
        }
    }
    validate_error_info(
        check,
        error,
        "device_not_connected",
        true,
        Some(json!({ "deviceId": device_id })),
    )
}

fn require_invalid_arguments(
    check: &'static str,
    error: &DriverError,
    action: &str,
) -> Result<ErrorInfo, ConformanceFailure> {
    match error {
        DriverError::InvalidArguments {
            action: actual,
            message,
        } if actual == action && !message.trim().is_empty() => {}
        DriverError::InvalidArguments {
            action: actual,
            message,
        } => {
            return Err(ConformanceFailure::new(
                check,
                format!(
                    "InvalidArguments must reference `{action}` with a non-empty message; got action=`{actual}`, message=`{message}`"
                ),
            ));
        }
        other => {
            return Err(ConformanceFailure::new(
                check,
                format!("expected InvalidArguments for `{action}`, got {other:?}"),
            ));
        }
    }
    validate_error_info(
        check,
        error,
        "invalid_arguments",
        false,
        Some(json!({ "action": action })),
    )
}

fn require_unknown_action(
    check: &'static str,
    error: &DriverError,
    action: &str,
) -> Result<ErrorInfo, ConformanceFailure> {
    match error {
        DriverError::UnknownAction(actual) if actual == action => {}
        other => {
            return Err(ConformanceFailure::new(
                check,
                format!("expected UnknownAction for `{action}`, got {other:?}"),
            ));
        }
    }
    validate_error_info(
        check,
        error,
        "unknown_action",
        false,
        Some(json!({ "action": action })),
    )
}

fn validate_error_info(
    check: &'static str,
    error: &DriverError,
    code: &str,
    retryable: bool,
    details: Option<Value>,
) -> Result<ErrorInfo, ConformanceFailure> {
    let info = error.to_error_info();
    require(
        check,
        info.code == code,
        format!("error code `{}` does not match `{code}`", info.code),
    )?;
    require(
        check,
        info.retryable == retryable,
        format!(
            "error `{code}` retryable={} does not match {retryable}",
            info.retryable
        ),
    )?;
    require(
        check,
        info.details == details,
        format!(
            "error `{code}` details {:?} do not match {:?}",
            info.details, details
        ),
    )?;
    require(
        check,
        !info.message.trim().is_empty(),
        format!("error `{code}` has an empty message"),
    )?;
    Ok(info)
}

fn require_error_event(
    check: &'static str,
    events: &[TestEvent],
    expected: &ErrorInfo,
) -> Result<(), ConformanceFailure> {
    match events {
        [
            TestEvent {
                at_ms,
                payload: TestEventPayload::Error { error },
                ..
            },
        ] if error == expected && *at_ms > 0 => Ok(()),
        other => Err(ConformanceFailure::new(
            check,
            format!("expected Error, got {other:?}"),
        )),
    }
}

fn require_observation_event(
    check: &'static str,
    events: &[TestEvent],
    expected: &Observation,
) -> Result<(), ConformanceFailure> {
    match events {
        [
            TestEvent {
                at_ms,
                payload: TestEventPayload::ObservationCaptured { observation },
                ..
            },
        ] if observation.as_ref() == expected && *at_ms > 0 => Ok(()),
        other => Err(ConformanceFailure::new(
            check,
            format!("expected ObservationCaptured, got {other:?}"),
        )),
    }
}

fn require_failed_action_events(
    check: &'static str,
    events: &[TestEvent],
    expected_call: &ActionCall,
    expected_protection: Option<ActionProtection>,
    expected_error: &ErrorInfo,
) -> Result<(), ConformanceFailure> {
    match events {
        [
            TestEvent {
                at_ms,
                payload: TestEventPayload::ActionStarted { call },
                ..
            },
            TestEvent {
                at_ms: completed_at_ms,
                payload:
                    TestEventPayload::ActionCompleted {
                        call_id,
                        outcome: ActionOutcome::Failed { error },
                    },
                ..
            },
        ] if call == &RecordedActionCall::from_action_call(expected_call, expected_protection)
            && *call_id == expected_call.id
            && error == expected_error
            && *at_ms > 0
            && *completed_at_ms >= *at_ms =>
        {
            Ok(())
        }
        other => Err(ConformanceFailure::new(
            check,
            format!("expected ActionStarted -> ActionCompleted(Failed), got {other:?}"),
        )),
    }
}

fn require_successful_action_events(
    check: &'static str,
    events: &[TestEvent],
    expected_call: &ActionCall,
    expected_protection: Option<ActionProtection>,
    expected_result: &ActionResult,
) -> Result<(), ConformanceFailure> {
    match events {
        [
            TestEvent {
                at_ms,
                payload: TestEventPayload::ActionStarted { call },
                ..
            },
            TestEvent {
                at_ms: completed_at_ms,
                payload:
                    TestEventPayload::ActionCompleted {
                        call_id,
                        outcome: ActionOutcome::Succeeded { result },
                    },
                ..
            },
        ] if call == &RecordedActionCall::from_action_call(expected_call, expected_protection)
            && *call_id == expected_call.id
            && result.as_ref() == expected_result
            && *at_ms > 0
            && *completed_at_ms >= *at_ms =>
        {
            Ok(())
        }
        other => Err(ConformanceFailure::new(
            check,
            format!("expected ActionStarted -> ActionCompleted(Succeeded), got {other:?}"),
        )),
    }
}

fn expect_error<T>(
    check: &'static str,
    result: RuntimeResult<T>,
    succeeded_message: impl Into<String>,
) -> Result<DriverError, ConformanceFailure> {
    match result {
        Ok(_) => Err(ConformanceFailure::new(check, succeeded_message)),
        Err(RuntimeError::Driver(error)) => Ok(error),
        Err(RuntimeError::EventStore(error)) => Err(event_store_failure(check, error)),
        Err(RuntimeError::Evidence(error)) => Err(evidence_failure(check, error)),
        Err(error @ (RuntimeError::Cancelled { .. } | RuntimeError::TimedOut { .. })) => {
            Err(runtime_failure(check, error))
        }
    }
}

async fn events_after(
    events: &MemoryEventStore,
    context: &OperationContext,
    cursor: &mut EventSequence,
) -> Result<Vec<TestEvent>, ConformanceFailure> {
    let recorded = events
        .list_after(&context.session_id, Some(*cursor))
        .await
        .map_err(|error| event_store_failure("event_cursor", error))?;
    let mut expected_sequence = cursor.checked_next();
    for event in &recorded {
        require(
            "event_cursor",
            event.session_id == context.session_id,
            format!(
                "event {} belongs to session {}, expected {}",
                event.event_id, event.session_id, context.session_id
            ),
        )?;
        let expected = expected_sequence.ok_or_else(|| {
            ConformanceFailure::new(
                "event_cursor",
                format!("event sequence exhausted after {}", cursor.get()),
            )
        })?;
        require(
            "event_cursor",
            event.sequence == expected,
            format!(
                "event sequence jumped from {} to {}, expected {}",
                cursor.get(),
                event.sequence.get(),
                expected.get()
            ),
        )?;
        *cursor = event.sequence;
        expected_sequence = cursor.checked_next();
    }
    Ok(recorded)
}

fn unused_action_name(capabilities: &[ActionDefinition]) -> String {
    let names = capabilities
        .iter()
        .map(|action| action.name.as_str())
        .collect::<HashSet<_>>();
    let base = "__devicerail_conformance_unknown_action__";
    if !names.contains(base) {
        return base.to_owned();
    }
    (1_u64..)
        .map(|suffix| format!("{base}_{suffix}"))
        .find(|candidate| !names.contains(candidate.as_str()))
        .expect("an infinite name sequence must contain an unused action name")
}

fn require(
    check: &'static str,
    condition: bool,
    message: impl Into<String>,
) -> Result<(), ConformanceFailure> {
    if condition {
        Ok(())
    } else {
        Err(ConformanceFailure::new(check, message))
    }
}

fn driver_failure(check: &'static str, error: DriverError) -> ConformanceFailure {
    ConformanceFailure::new(check, format!("driver returned {error:?}: {error}"))
}

fn event_store_failure(check: &'static str, error: EventStoreError) -> ConformanceFailure {
    ConformanceFailure::new(check, format!("event store returned {error:?}: {error}"))
}

fn evidence_failure(check: &'static str, error: EvidenceError) -> ConformanceFailure {
    ConformanceFailure::new(check, format!("evidence store returned {error:?}: {error}"))
}

fn runtime_failure(check: &'static str, error: RuntimeError) -> ConformanceFailure {
    match error {
        RuntimeError::Driver(error) => driver_failure(check, error),
        RuntimeError::EventStore(error) => event_store_failure(check, error),
        RuntimeError::Evidence(error) => evidence_failure(check, error),
        control_error @ (RuntimeError::Cancelled { .. } | RuntimeError::TimedOut { .. }) => {
            ConformanceFailure::new(check, format!("runtime control failed: {control_error}"))
        }
    }
}

/// Defines one isolated async conformance test in a downstream driver crate.
///
/// The driver factory is evaluated inside the test and therefore must create a
/// new instance. Use a unique device id if the implementation touches shared
/// platform state. This is a Tokio convenience macro; callers using another
/// async test runtime can invoke [`run_driver_conformance`] directly. A fourth
/// expression injects an `Arc<dyn EvidenceStore>` for Drivers that persist
/// Store-owned observations or Action evidence; the original three-argument
/// form retains its explicit rejecting Store.
#[macro_export]
macro_rules! driver_conformance_test {
    ($name:ident, $driver_factory:expr, $action_factory:expr, $evidence:expr $(,)?) => {
        #[::tokio::test]
        async fn $name() {
            let report = $crate::conformance::assert_driver_conformance_with_evidence(
                $driver_factory,
                $action_factory,
                $evidence,
            )
            .await;
            assert_eq!(report.capability_count, report.executed_action_count);
        }
    };
    ($name:ident, $driver_factory:expr, $action_factory:expr $(,)?) => {
        #[::tokio::test]
        async fn $name() {
            let report =
                $crate::conformance::assert_driver_conformance($driver_factory, $action_factory)
                    .await;
            assert_eq!(report.capability_count, report.executed_action_count);
        }
    };
}

#[cfg(test)]
mod cleanup_tests {
    use std::sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use devicerail_protocol::{AssetRef, SessionId};

    use super::cleanup_harness_sessions;
    use crate::{
        EvidenceError, EvidenceInput, EvidenceMetadata, EvidenceOutput, EvidenceResult,
        EvidenceStore, GcPolicy, GcReport, MemoryEventStore, PutEvidence, ReleaseReport,
        SessionEventStore, Sha256Digest, StartSession, StoredEvidence, now_ms,
    };

    struct OrderingEvidenceStore {
        events: Weak<MemoryEventStore>,
        release_saw_events_dropped: AtomicBool,
        releases: Mutex<Vec<SessionId>>,
    }

    impl OrderingEvidenceStore {
        fn unused<T>() -> EvidenceResult<T> {
            Err(EvidenceError::Internal(
                "unused ordering fixture operation".to_owned(),
            ))
        }
    }

    #[async_trait]
    impl EvidenceStore for OrderingEvidenceStore {
        async fn put(
            &self,
            _request: PutEvidence,
            _input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            Self::unused()
        }

        async fn attach(
            &self,
            _session_id: &SessionId,
            _asset: &AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            Self::unused()
        }

        async fn verify_session_reference(
            &self,
            _session_id: &SessionId,
            _asset: &AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            Self::unused()
        }

        async fn open(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            Self::unused()
        }

        async fn metadata(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            Self::unused()
        }

        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            Self::unused()
        }

        async fn release_session(
            &self,
            session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            self.release_saw_events_dropped
                .store(self.events.upgrade().is_none(), Ordering::SeqCst);
            self.releases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(session_id.clone());
            Ok(ReleaseReport {
                session_id: session_id.clone(),
                released_references: 0,
                newly_unreferenced_assets: 0,
                newly_unreferenced_bytes: 0,
            })
        }

        async fn gc(&self, _policy: GcPolicy) -> EvidenceResult<GcReport> {
            Self::unused()
        }
    }

    #[tokio::test]
    async fn event_store_is_dropped_before_evidence_release() {
        let events = Arc::new(MemoryEventStore::default());
        let session = StartSession::new(None, None, now_ms());
        let session_id = session.session_id.clone();
        events.start_session(session).await.expect("start Session");
        let evidence = Arc::new(OrderingEvidenceStore {
            events: Arc::downgrade(&events),
            release_saw_events_dropped: AtomicBool::new(false),
            releases: Mutex::new(Vec::new()),
        });
        let evidence_trait: Arc<dyn EvidenceStore> = evidence.clone();

        cleanup_harness_sessions(events, Some(evidence_trait), true)
            .await
            .expect("normal harness cleanup");

        assert!(
            evidence.release_saw_events_dropped.load(Ordering::SeqCst),
            "event log ownership must be gone before release_session"
        );
        assert_eq!(
            evidence
                .releases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            std::slice::from_ref(&session_id)
        );
    }
}
