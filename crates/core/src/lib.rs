use std::{
    future::Future,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionOutcome, ActionProtection, ActionResult, DeviceId,
    DeviceInfo, ErrorInfo, Observation, RecordedActionCall, ScreenshotOmissionReason,
    TestEventPayload, UiSnapshotOmissionReason, is_semantic_action_name,
};
use serde_json::json;
use thiserror::Error;

mod blocking;
mod cleanup;
mod control;
pub mod event_store;
mod event_stream;
mod evidence;
mod media;
mod pool;
mod registry;
mod session;

#[cfg(feature = "conformance")]
pub mod conformance;

pub use blocking::run_bounded_blocking;
pub use cleanup::{
    SessionCleanupError, SessionCleanupReport, cleanup_ended_session,
    reconcile_missing_session_evidence,
};
pub use control::{CancellationReason, ExecutionControl, ExecutionController, TimeoutScope};
pub use event_store::{
    EventStoreError, EventStoreResult, MemoryEventStore, ObservationLease, SessionEventStore,
    SessionExportPageSnapshot,
};
pub use event_stream::{
    EventStreamConfig, EventStreamError, EventStreamItem, EventStreamTerminal, EventSubscription,
    MAX_EVENT_STREAM_REPLAY_EVENTS, MAX_EVENT_STREAM_SUBSCRIBERS_PER_SESSION,
    MAX_EVENT_STREAM_TAIL_EVENTS,
};
pub use evidence::{
    EvidenceError, EvidenceInput, EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore,
    GcPolicy, GcReport, PutEvidence, ReleaseReport, SessionEvidenceWriter, Sha256Digest,
    StoredEvidence, UnavailableEvidenceStore,
};
pub use media::{MediaStreamError, MediaStreamWriter};
pub use pool::{
    DeviceAccessGuard, DeviceLease, DevicePool, DevicePoolConfig, DevicePoolEntry, DevicePoolError,
    DevicePoolResult, DeviceRegistrationToken, LeaseId, LeaseOwnerId, PoolHealth, PoolHealthState,
};
pub use registry::{
    DriverAccess, DriverHandle, DriverLifecycleAccess, DriverRegistry, RegistryError,
    RegistryResult,
};
pub use session::{
    DriverOperationContext, EndSession, OperationContext, PendingEvent, ScreenshotPolicy,
    StartSession,
};

pub type DriverResult<T> = Result<T, DriverError>;
pub type DeviceOperationResult<T> = Result<T, DeviceOperationError>;
pub type RuntimeResult<T> = Result<T, RuntimeError>;

const OBSERVATION_RELEASE_MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("device {0} is not connected")]
    NotConnected(DeviceId),
    #[error("unknown action: {0}")]
    UnknownAction(String),
    #[error("invalid arguments for action {action}: {message}")]
    InvalidArguments { action: String, message: String },
    #[error("element was not found")]
    ElementNotFound,
    #[error("element selector matched more than one element")]
    ElementAmbiguous,
    #[error("element reference is stale")]
    ElementStale,
    #[error("element is not interactable")]
    ElementNotInteractable,
    #[error("UI context was not found")]
    UiContextNotFound,
    #[error("UI context selector matched more than one context")]
    UiContextAmbiguous,
    #[error("UI context changed")]
    UiContextChanged,
    #[error("semantic UI channel is unavailable")]
    SemanticChannelUnavailable,
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The Driver stopped because it observed the supplied control's
    /// cancellation signal. Without a matching Core cancellation signal this
    /// remains a Driver failure rather than a request cancellation.
    #[error("driver operation was cancelled")]
    Cancelled,
    /// The Driver stopped because it observed the supplied control's deadline.
    /// An independent platform timeout remains a Driver failure unless the
    /// Core deadline has also elapsed.
    #[error("driver operation timed out")]
    TimedOut,
    /// A platform adapter failed with a stable, non-sensitive classification.
    /// The public message remains generic; only a validated code and the
    /// adapter-selected retryability cross the wire.
    #[error("platform operation failed")]
    Platform { code: String, retryable: bool },
    #[error("internal driver error: {0}")]
    Internal(String),
}

impl DriverError {
    pub fn to_error_info(&self) -> ErrorInfo {
        let (code, message, retryable, details) = match self {
            Self::NotConnected(device_id) => (
                "device_not_connected",
                self.to_string(),
                true,
                Some(json!({ "deviceId": device_id })),
            ),
            Self::UnknownAction(action) => (
                "unknown_action",
                self.to_string(),
                false,
                Some(json!({ "action": action })),
            ),
            Self::InvalidArguments { action, .. } => (
                "invalid_arguments",
                format!("invalid arguments for action {action}"),
                false,
                Some(json!({ "action": action })),
            ),
            Self::ElementNotFound => ("element_not_found", self.to_string(), false, None),
            Self::ElementAmbiguous => ("element_ambiguous", self.to_string(), false, None),
            Self::ElementStale => ("element_stale", self.to_string(), true, None),
            Self::ElementNotInteractable => {
                ("element_not_interactable", self.to_string(), false, None)
            }
            Self::UiContextNotFound => ("ui_context_not_found", self.to_string(), false, None),
            Self::UiContextAmbiguous => ("ui_context_ambiguous", self.to_string(), false, None),
            Self::UiContextChanged => ("ui_context_changed", self.to_string(), true, None),
            Self::SemanticChannelUnavailable => (
                "semantic_channel_unavailable",
                self.to_string(),
                false,
                None,
            ),
            Self::Protocol(_) => (
                "protocol_error",
                "driver protocol error".to_owned(),
                false,
                None,
            ),
            Self::Cancelled => ("driver_cancelled", self.to_string(), false, None),
            Self::TimedOut => ("driver_timed_out", self.to_string(), true, None),
            Self::Platform { code, retryable } => (
                "platform_error",
                self.to_string(),
                *retryable,
                Some(json!({ "platformCode": public_platform_code(code) })),
            ),
            Self::Internal(_) => (
                "internal_error",
                "internal driver error".to_owned(),
                true,
                None,
            ),
        };

        ErrorInfo {
            code: code.to_owned(),
            message,
            retryable,
            details,
        }
    }
}

fn public_platform_code(code: &str) -> &str {
    let is_stable = !code.is_empty()
        && code.len() <= 64
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        });
    if is_stable { code } else { "unknown" }
}

/// Failures produced while a Driver is allowed to use Session-scoped runtime
/// capabilities. Evidence failures remain distinct from platform failures all
/// the way through Core and onto the wire.
#[derive(Debug, Error)]
pub enum DeviceOperationError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error(transparent)]
    EventStore(#[from] EventStoreError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error("request was cancelled: {reason:?}")]
    Cancelled { reason: CancellationReason },
    #[error("{scope:?} timed out after {timeout_ms} ms")]
    TimedOut {
        scope: TimeoutScope,
        timeout_ms: u64,
    },
}

impl RuntimeError {
    pub fn to_error_info(&self) -> ErrorInfo {
        match self {
            Self::Driver(error) => error.to_error_info(),
            Self::EventStore(error) => error.to_error_info(),
            Self::Evidence(error) => error.to_error_info(),
            Self::Cancelled { reason } => ErrorInfo {
                code: "request_cancelled".to_owned(),
                message: self.to_string(),
                retryable: false,
                details: Some(json!({ "reason": reason.as_str() })),
            },
            Self::TimedOut { scope, timeout_ms } => ErrorInfo {
                code: match scope {
                    TimeoutScope::Action => "action_timed_out",
                    TimeoutScope::Request | TimeoutScope::Shutdown => "request_timed_out",
                }
                .to_owned(),
                message: self.to_string(),
                retryable: true,
                details: Some(json!({
                    "scope": scope.as_str(),
                    "timeoutMs": timeout_ms
                })),
            },
        }
    }

    fn to_action_error_info(&self) -> ErrorInfo {
        match self {
            Self::Cancelled { reason } => ErrorInfo {
                code: "action_cancelled".to_owned(),
                message: self.to_string(),
                retryable: false,
                details: Some(json!({ "reason": reason.as_str() })),
            },
            Self::TimedOut { scope, timeout_ms } => ErrorInfo {
                code: "action_timeout".to_owned(),
                message: self.to_string(),
                retryable: true,
                details: Some(json!({
                    "scope": scope.as_str(),
                    "timeoutMs": timeout_ms
                })),
            },
            _ => self.to_error_info(),
        }
    }
}

/// Platform implementation boundary for device operations.
///
/// The runtime may stop polling and drop any method future as soon as the
/// supplied control is cancelled or expires. Implementations must therefore
/// be cancellation-safe and must not leave detached, untracked device work
/// running after their future is dropped.
#[async_trait]
pub trait DeviceDriver: Send + Sync {
    /// Returns the immutable identity of this driver instance.
    ///
    /// The value must remain stable before, during, and after connections.
    fn id(&self) -> &DeviceId;

    /// Connects the device and returns its connected identity.
    ///
    /// Connecting an already connected driver is idempotent: it succeeds and
    /// returns information equivalent to the first successful call.
    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo>;

    /// Disconnects the device.
    ///
    /// Disconnecting an already disconnected driver is an idempotent success.
    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()>;

    /// Describes the driver's action space without requiring a connection.
    ///
    /// Action names must be unique. Each `input_schema` is a valid,
    /// self-contained JSON Schema whose root type is `object`.
    async fn capabilities(&self, control: &ExecutionControl)
    -> DriverResult<Vec<ActionDefinition>>;

    /// Performs a bounded, non-mutating platform liveness probe.
    ///
    /// The default verifies only that the Driver can answer its static
    /// capability contract. Platform Drivers should override this with the
    /// cheapest real transport/device probe that does not connect, capture a
    /// screenshot, prompt for permission, or mutate input state.
    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.capabilities(control).await.map(drop)
    }

    /// Returns the protection class for one advertised action without I/O.
    /// Unknown action names must return `None` and are redacted by Core.
    fn action_protection(&self, name: &str) -> Option<ActionProtection>;

    /// Captures the device state, or returns [`DriverError::NotConnected`].
    /// Successful observations identify this driver and contain valid viewport
    /// and evidence references.
    async fn observe(&self, context: &DriverOperationContext)
    -> DeviceOperationResult<Observation>;

    /// Executes an action, or returns [`DriverError::NotConnected`] before
    /// validating the action when the device is disconnected.
    ///
    /// A connected driver returns [`DriverError::UnknownAction`] for an
    /// unadvertised name and [`DriverError::InvalidArguments`] for arguments
    /// that do not satisfy the advertised object schema. Successful results
    /// preserve the call id, have ordered timestamps, and include an `after`
    /// observation and evidence.
    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult>;
}

pub struct DeviceRuntime<D: ?Sized, S: ?Sized> {
    driver: Arc<D>,
    events: Arc<S>,
    evidence: Arc<dyn EvidenceStore>,
    strict_evidence_receipts: bool,
    screenshot_policy: ScreenshotPolicy,
}

impl<D, S> DeviceRuntime<D, S>
where
    D: DeviceDriver + ?Sized,
    S: SessionEventStore + ?Sized,
{
    /// Creates a runtime whose Driver operations explicitly reject evidence
    /// writes. Use [`Self::with_evidence`] for evidence-producing Drivers.
    pub fn new(driver: Arc<D>, events: Arc<S>) -> Self {
        Self {
            driver,
            events,
            evidence: Arc::new(UnavailableEvidenceStore),
            // Preserve compatibility for Drivers such as the mock Driver that
            // expose non-Store evidence references. Injecting a Store opts in
            // to operation-scoped provenance enforcement.
            strict_evidence_receipts: false,
            screenshot_policy: ScreenshotPolicy::Capture,
        }
    }

    /// Creates a runtime that binds every Driver evidence capability to the
    /// Session in the corresponding [`OperationContext`]. Successful results
    /// must return exactly the de-duplicated evidence receipt set issued by
    /// that operation's writer.
    pub fn with_evidence(driver: Arc<D>, events: Arc<S>, evidence: Arc<dyn EvidenceStore>) -> Self {
        Self {
            driver,
            events,
            evidence,
            strict_evidence_receipts: true,
            screenshot_policy: ScreenshotPolicy::Capture,
        }
    }

    pub const fn with_screenshot_policy(mut self, screenshot_policy: ScreenshotPolicy) -> Self {
        self.screenshot_policy = screenshot_policy;
        self
    }

    pub const fn screenshot_policy(&self) -> ScreenshotPolicy {
        self.screenshot_policy
    }

    pub fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        self.driver.action_protection(name)
    }

    pub fn device_id(&self) -> &DeviceId {
        self.driver.id()
    }

    pub fn driver(&self) -> &Arc<D> {
        &self.driver
    }

    pub fn event_store(&self) -> &Arc<S> {
        &self.events
    }

    pub async fn connect(&self, control: &ExecutionControl) -> RuntimeResult<DeviceInfo> {
        await_driver(control, self.driver.connect(control)).await
    }

    pub async fn disconnect(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        await_driver(control, self.driver.disconnect(control)).await
    }

    pub async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> RuntimeResult<Vec<ActionDefinition>> {
        let definitions = await_driver(control, self.driver.capabilities(control)).await?;
        self.validate_capability_protections(&definitions)?;
        Ok(definitions)
    }

    fn validate_capability_protections(
        &self,
        definitions: &[ActionDefinition],
    ) -> RuntimeResult<()> {
        for definition in definitions {
            let declared = self.driver.action_protection(&definition.name);
            if declared != Some(definition.protection) {
                return Err(DriverError::Protocol(format!(
                    "action protection for `{}` is {:?}, expected {:?}",
                    definition.name, declared, definition.protection
                ))
                .into());
            }
        }
        Ok(())
    }

    async fn validated_action_protection(
        &self,
        control: &ExecutionControl,
        name: &str,
    ) -> RuntimeResult<Option<ActionProtection>> {
        let definitions = await_driver(control, self.driver.capabilities(control)).await?;
        self.validate_capability_protections(&definitions)?;
        if let Some(definition) = definitions
            .iter()
            .find(|definition| definition.name == name)
        {
            return Ok(Some(definition.protection));
        }

        // Unknown-action classification remains the Driver's responsibility
        // and is covered by conformance. Preserve the legacy classifier for
        // unadvertised names; Core still treats `None` as protected, while an
        // advertised definition is authoritative above.
        Ok(self.driver.action_protection(name))
    }

    pub async fn health_check(&self, control: &ExecutionControl) -> RuntimeResult<()> {
        await_driver(control, self.driver.health_check(control)).await
    }

    /// Captures and durably records one observation while holding a
    /// Session-scoped operation lease.
    ///
    /// All returned outcomes, including cooperative cancellation and event
    /// append failures, explicitly release the lease. Arbitrarily dropping or
    /// aborting this future bypasses that asynchronous cleanup and is outside
    /// the runtime's cooperative cancellation contract.
    pub async fn observe(&self, context: &OperationContext) -> RuntimeResult<Observation> {
        let lease = self.events.reserve_observation(&context.session_id).await?;
        let screenshot_policy = context.effective_screenshot_policy(self.screenshot_policy);
        let driver_context =
            self.driver_context(context.control.clone(), context, screenshot_policy);
        let operation = await_device_operation(
            driver_context.control(),
            self.driver.observe(&driver_context),
        )
        .await
        .and_then(|observation| {
            self.validate_observation_evidence(
                &driver_context,
                &observation,
                match screenshot_policy {
                    ScreenshotPolicy::Capture => None,
                    ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
                },
            )?;
            Ok(observation)
        });
        let result = match operation {
            Ok(observation) => {
                let append = self
                    .events
                    .append(PendingEvent::for_operation(
                        context,
                        Some(self.driver.id().clone()),
                        now_ms(),
                        TestEventPayload::ObservationCaptured {
                            observation: Box::new(observation.clone()),
                        },
                    ))
                    .await;
                append.map(|_| observation).map_err(RuntimeError::from)
            }
            Err(error) => match self.emit_error(context, error.to_error_info()).await {
                Ok(()) => Err(error),
                Err(append_error) => Err(append_error.into()),
            },
        };

        match self
            .release_observation_lease(&context.session_id, lease)
            .await
        {
            Ok(()) => result,
            Err(release_error) => Err(release_error.into()),
        }
    }

    /// Runs the observation finalizer independently of the operation control:
    /// cancellation has already selected the operation result, but must not
    /// strand the Session lease. A bounded retry covers transient/ambiguous
    /// Event Store failures without making cleanup unbounded.
    async fn release_observation_lease(
        &self,
        session_id: &devicerail_protocol::SessionId,
        lease: ObservationLease,
    ) -> EventStoreResult<()> {
        let mut last_error = None;
        for attempt in 1..=OBSERVATION_RELEASE_MAX_ATTEMPTS {
            match self.events.release_observation(session_id, lease).await {
                Ok(()) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
            if attempt < OBSERVATION_RELEASE_MAX_ATTEMPTS {
                tokio::task::yield_now().await;
            }
        }
        Err(last_error.expect("at least one release attempt is configured"))
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        call: ActionCall,
    ) -> RuntimeResult<ActionResult> {
        if let Some(error) = inactive_control_error(&context.control) {
            return Err(error);
        }
        let semantic_action = is_semantic_action_name(&call.name);
        if semantic_action
            && (!context.semantic_actions_enabled() || !context.ui_snapshots_enabled())
        {
            return Err(DriverError::SemanticChannelUnavailable.into());
        }
        let call_id = call.id;
        // The synchronous classifier is a redaction and screenshot safety
        // boundary, not an independent source of truth. Reconcile it with the
        // advertised capability contract before persisting ActionStarted or
        // giving the Driver an evidence-capable operation context.
        let protection = self
            .validated_action_protection(&context.control, &call.name)
            .await?;
        let recorded_call = RecordedActionCall::from_action_call(&call, protection);
        self.events
            .append(PendingEvent::for_operation(
                context,
                Some(self.driver.id().clone()),
                now_ms(),
                TestEventPayload::ActionStarted {
                    call: recorded_call,
                },
            ))
            .await?;

        // The action-specific budget covers only Driver execution. The parent
        // request deadline remains absolute and therefore still includes the
        // durable ActionStarted append above.
        let control = context.execution_control();
        let screenshot_policy = context.effective_screenshot_policy(self.screenshot_policy);
        let expected_omission = match protection {
            Some(ActionProtection::Standard) => match screenshot_policy {
                ScreenshotPolicy::Capture => None,
                ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
            },
            Some(ActionProtection::Protected) | None => {
                Some(ScreenshotOmissionReason::ProtectedAction)
            }
        };
        let action_screenshot_policy = if expected_omission.is_some() {
            ScreenshotPolicy::Omit
        } else {
            ScreenshotPolicy::Capture
        };
        let driver_context = self.driver_context(control, context, action_screenshot_policy);
        let result = await_device_operation(
            driver_context.control(),
            self.driver.execute(&driver_context, call),
        )
        .await
        .and_then(|result| validate_action_result(call_id, semantic_action, result))
        .and_then(|result| {
            self.validate_action_evidence(&driver_context, &result, expected_omission)?;
            Ok(result)
        });
        let outcome = match &result {
            Ok(result) => ActionOutcome::Succeeded {
                result: Box::new(result.clone()),
            },
            Err(RuntimeError::Cancelled { .. }) => ActionOutcome::Cancelled {
                error: result
                    .as_ref()
                    .expect_err("cancel error")
                    .to_action_error_info(),
            },
            Err(RuntimeError::TimedOut { timeout_ms, .. }) => ActionOutcome::TimedOut {
                error: result
                    .as_ref()
                    .expect_err("timeout error")
                    .to_action_error_info(),
                timeout_ms: *timeout_ms,
            },
            Err(error) => ActionOutcome::Failed {
                error: error.to_error_info(),
            },
        };

        self.events
            .append(PendingEvent::for_operation(
                context,
                Some(self.driver.id().clone()),
                now_ms(),
                TestEventPayload::ActionCompleted { call_id, outcome },
            ))
            .await?;
        result
    }

    fn driver_context(
        &self,
        control: ExecutionControl,
        operation: &OperationContext,
        screenshot_policy: ScreenshotPolicy,
    ) -> DriverOperationContext {
        let evidence: Arc<dyn EvidenceStore> = match screenshot_policy {
            ScreenshotPolicy::Capture => Arc::clone(&self.evidence),
            ScreenshotPolicy::Omit => Arc::new(UnavailableEvidenceStore),
        };
        DriverOperationContext::new(
            control,
            SessionEvidenceWriter::new(operation.session_id.clone(), evidence),
            screenshot_policy,
            operation.ui_snapshots_enabled(),
            operation.semantic_actions_enabled(),
        )
    }

    fn validate_observation_evidence(
        &self,
        context: &DriverOperationContext,
        observation: &Observation,
        expected_omission: Option<ScreenshotOmissionReason>,
    ) -> RuntimeResult<()> {
        validate_observation_omission(
            observation,
            expected_omission,
            context.ui_snapshots_enabled(),
        )?;
        if expected_omission.is_some() && !context.evidence().receipts_match(std::iter::empty()) {
            return Err(DriverError::Protocol(
                "screenshot-omitted observation persisted evidence".to_owned(),
            )
            .into());
        }
        if !self.strict_evidence_receipts
            || context.evidence().receipts_match(observation.asset_refs())
        {
            return Ok(());
        }

        Err(DriverError::Protocol(
            "observation evidence does not match this operation's receipts".to_owned(),
        )
        .into())
    }

    fn validate_action_evidence(
        &self,
        context: &DriverOperationContext,
        result: &ActionResult,
        expected_omission: Option<ScreenshotOmissionReason>,
    ) -> RuntimeResult<()> {
        if expected_omission.is_some() && result.after.is_none() {
            return Err(DriverError::Protocol(
                "screenshot-omitted action did not return an after observation".to_owned(),
            )
            .into());
        }
        if expected_omission == Some(ScreenshotOmissionReason::ProtectedAction)
            && result.before.is_none()
        {
            return Err(DriverError::Protocol(
                "protected action did not return a before observation".to_owned(),
            )
            .into());
        }
        for observation in result.before.iter().chain(result.after.iter()) {
            validate_observation_omission(
                observation,
                expected_omission,
                context.ui_snapshots_enabled(),
            )?;
        }
        if expected_omission.is_some() {
            if !result.evidence.is_empty() {
                return Err(DriverError::Protocol(
                    "screenshot-omitted action returned evidence".to_owned(),
                )
                .into());
            }
            if !context.evidence().receipts_match(std::iter::empty()) {
                return Err(DriverError::Protocol(
                    "screenshot-omitted action persisted evidence".to_owned(),
                )
                .into());
            }
            return Ok(());
        }
        let returned = result.asset_refs();
        if !self.strict_evidence_receipts || context.evidence().receipts_match(returned) {
            return Ok(());
        }

        Err(DriverError::Protocol(
            "action evidence does not match this operation's receipts".to_owned(),
        )
        .into())
    }

    async fn emit_error(
        &self,
        context: &OperationContext,
        error: ErrorInfo,
    ) -> EventStoreResult<()> {
        self.events
            .append(PendingEvent::for_operation(
                context,
                Some(self.driver.id().clone()),
                now_ms(),
                TestEventPayload::Error { error },
            ))
            .await
            .map(|_| ())
    }
}

fn validate_observation_omission(
    observation: &Observation,
    expected: Option<ScreenshotOmissionReason>,
    ui_snapshots_enabled: bool,
) -> RuntimeResult<()> {
    if observation.screenshot_omission != expected {
        return Err(DriverError::Protocol(format!(
            "observation screenshot omission {:?} does not match expected {expected:?}",
            observation.screenshot_omission
        ))
        .into());
    }
    if expected.is_some() && observation.screenshot.is_some() {
        return Err(DriverError::Protocol(
            "screenshot-omitted observation returned a screenshot".to_owned(),
        )
        .into());
    }
    if !ui_snapshots_enabled
        && (observation.ui_snapshot.is_some() || observation.ui_snapshot_omission.is_some())
    {
        return Err(DriverError::Protocol(
            "observation returned Protocol 1.5 UI Snapshot fields without operation support"
                .to_owned(),
        )
        .into());
    }
    if !observation.ui_snapshot_state_is_valid() {
        return Err(DriverError::Protocol(
            "observation returned both a UI snapshot and an omission reason".to_owned(),
        )
        .into());
    }
    let expected_ui_omission = match expected {
        Some(ScreenshotOmissionReason::Policy) => Some(UiSnapshotOmissionReason::Policy),
        Some(ScreenshotOmissionReason::ProtectedAction) => {
            Some(UiSnapshotOmissionReason::ProtectedAction)
        }
        None => None,
    };
    match (expected_ui_omission, observation.ui_snapshot_omission) {
        (Some(expected_ui_omission), Some(actual)) if actual != expected_ui_omission => {
            return Err(DriverError::Protocol(format!(
                "observation UI snapshot omission {actual:?} does not match expected {expected_ui_omission:?}"
            ))
            .into());
        }
        (None, Some(UiSnapshotOmissionReason::ProtectedAction)) => {
            return Err(DriverError::Protocol(
                "observation reported a protected UI snapshot omission unexpectedly".to_owned(),
            )
            .into());
        }
        _ => {}
    }
    if expected_ui_omission.is_some() && observation.ui_snapshot.is_some() {
        return Err(DriverError::Protocol(
            "UI-snapshot-omitted observation returned a UI snapshot".to_owned(),
        )
        .into());
    }
    if let Some(snapshot) = &observation.ui_snapshot
        && snapshot.validate().is_err()
    {
        return Err(DriverError::Protocol(
            "observation returned an invalid UI snapshot reference".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn validate_action_result(
    call_id: uuid::Uuid,
    semantic_action: bool,
    result: ActionResult,
) -> RuntimeResult<ActionResult> {
    if result.call_id != call_id {
        return Err(DriverError::Protocol(format!(
            "action result call id {} does not match request {call_id}",
            result.call_id
        ))
        .into());
    }
    match (&result.execution, semantic_action) {
        (Some(_), false) => Err(DriverError::Protocol(
            "non-semantic action returned semantic execution metadata".to_owned(),
        )
        .into()),
        (None, true) => Err(DriverError::Protocol(
            "semantic action omitted execution metadata".to_owned(),
        )
        .into()),
        (Some(execution), true) if execution.validate().is_err() => Err(DriverError::Protocol(
            "semantic action returned invalid execution metadata".to_owned(),
        )
        .into()),
        _ => Ok(result),
    }
}

async fn await_driver<T, F>(control: &ExecutionControl, future: F) -> RuntimeResult<T>
where
    F: Future<Output = DriverResult<T>>,
{
    if let Some(reason) = control.cancellation_reason() {
        return Err(RuntimeError::Cancelled { reason });
    }
    if control.is_expired() {
        return Err(timeout_error(control));
    }

    tokio::pin!(future);
    // A Driver result that is already ready wins a same-poll boundary race.
    // This avoids relabelling completed device work as cancelled or timed out;
    // otherwise the active cancellation/deadline branch stops polling it.
    tokio::select! {
        biased;
        result = &mut future => normalize_driver_result(control, result),
        reason = control.cancelled() => Err(RuntimeError::Cancelled { reason }),
        () = control.deadline_elapsed() => Err(timeout_error(control)),
    }
}

async fn await_device_operation<T, F>(control: &ExecutionControl, future: F) -> RuntimeResult<T>
where
    F: Future<Output = DeviceOperationResult<T>>,
{
    if let Some(reason) = control.cancellation_reason() {
        return Err(RuntimeError::Cancelled { reason });
    }
    if control.is_expired() {
        return Err(timeout_error(control));
    }

    tokio::pin!(future);
    tokio::select! {
        biased;
        result = &mut future => normalize_device_operation_result(control, result),
        reason = control.cancelled() => Err(RuntimeError::Cancelled { reason }),
        () = control.deadline_elapsed() => Err(timeout_error(control)),
    }
}

fn normalize_device_operation_result<T>(
    control: &ExecutionControl,
    result: DeviceOperationResult<T>,
) -> RuntimeResult<T> {
    match result {
        Ok(value) => Ok(value),
        Err(DeviceOperationError::Driver(error)) => normalize_driver_result(control, Err(error)),
        Err(DeviceOperationError::Evidence(error)) => Err(RuntimeError::Evidence(error)),
    }
}

fn normalize_driver_result<T>(
    control: &ExecutionControl,
    result: DriverResult<T>,
) -> RuntimeResult<T> {
    match result {
        Err(DriverError::Cancelled) if control.cancellation_reason().is_some() => {
            Err(RuntimeError::Cancelled {
                reason: control
                    .cancellation_reason()
                    .expect("cancellation reason was checked"),
            })
        }
        Err(DriverError::TimedOut) if control.is_expired() => Err(timeout_error(control)),
        other => other.map_err(RuntimeError::Driver),
    }
}

fn timeout_error(control: &ExecutionControl) -> RuntimeError {
    let (scope, timeout_ms) = control.timeout().unwrap_or((TimeoutScope::Request, 0));
    RuntimeError::TimedOut { scope, timeout_ms }
}

fn inactive_control_error(control: &ExecutionControl) -> Option<RuntimeError> {
    control
        .cancellation_reason()
        .map(|reason| RuntimeError::Cancelled { reason })
        .or_else(|| control.is_expired().then(|| timeout_error(control)))
}

pub fn now_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    millis.min(u128::from(devicerail_protocol::MAX_SAFE_INTEGER)) as u64
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        future::pending,
        io::Cursor,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use devicerail_protocol::{
        ActionCall, ActionDefinition, ActionExecution, ActionOutcome, ActionProtection,
        ActionResult, AssetRef, DeviceId, DeviceInfo, EventSequence, FIND_ELEMENT_ACTION,
        Observation, Platform, ScreenshotOmissionReason, SessionExport, SessionId, SessionInfo,
        SessionOutcome, TestEvent, TestEventPayload, UiContextKind, UiContextRef,
        UiSnapshotOmissionReason, Viewport,
    };
    use serde_json::{Map, Value, json};
    use uuid::Uuid;

    use super::{
        CancellationReason, DeviceDriver, DeviceOperationResult, DeviceRuntime, DriverError,
        DriverOperationContext, DriverResult, EndSession, EventStoreError, EventStoreResult,
        EventStreamItem, EventStreamTerminal, EvidenceError, EvidenceInput, EvidenceMetadata,
        EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl, ExecutionController,
        GcPolicy, GcReport, MemoryEventStore, OBSERVATION_RELEASE_MAX_ATTEMPTS, ObservationLease,
        OperationContext, PendingEvent, PutEvidence, ReleaseReport, RuntimeError, ScreenshotPolicy,
        SessionEventStore, SessionExportPageSnapshot, Sha256Digest, StartSession, StoredEvidence,
        TimeoutScope, now_ms, validate_action_result, validate_observation_omission,
    };

    struct TestDriver {
        id: DeviceId,
    }

    struct EvidenceDriver {
        id: DeviceId,
    }

    #[derive(Clone, Copy)]
    enum ReceiptScenario {
        ExternalObservation,
        ExtraObservation,
        ReusePreviousObservation,
        DuplicateActionEvidence,
        AttachedActionEvidence,
        ExternalActionEvidence,
        WriteThenActionError,
        WriteThenActionHang,
    }

    struct ReceiptDriver {
        id: DeviceId,
        scenario: ReceiptScenario,
        observation_count: AtomicUsize,
        previous: Mutex<Option<AssetRef>>,
    }

    impl ReceiptDriver {
        fn new(id: impl Into<String>, scenario: ReceiptScenario) -> Self {
            Self {
                id: DeviceId::new(id),
                scenario,
                observation_count: AtomicUsize::new(0),
                previous: Mutex::new(None),
            }
        }

        async fn put_evidence(context: &DriverOperationContext) -> EvidenceResult<AssetRef> {
            let bytes = b"png".to_vec();
            context
                .evidence()
                .put_with_declared_size(
                    "image/png",
                    bytes.len() as u64,
                    Box::pin(Cursor::new(bytes)),
                )
                .await
                .map(|stored| stored.asset_ref())
        }

        fn action_result(call_id: Uuid) -> ActionResult {
            ActionResult {
                call_id,
                started_at_ms: now_ms(),
                finished_at_ms: now_ms(),
                output: Value::Null,
                before: None,
                after: None,
                evidence: Vec::new(),
                execution: None,
            }
        }
    }

    #[async_trait]
    impl DeviceDriver for ReceiptDriver {
        fn id(&self) -> &DeviceId {
            &self.id
        }

        async fn connect(&self, _control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            Ok(DeviceInfo {
                id: self.id.clone(),
                name: "receipt test".to_owned(),
                platform: Platform::Mock,
                os_version: None,
                connected: true,
            })
        }

        async fn disconnect(&self, _control: &ExecutionControl) -> DriverResult<()> {
            Ok(())
        }

        async fn capabilities(
            &self,
            _control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            Ok(Vec::new())
        }

        fn action_protection(&self, name: &str) -> Option<ActionProtection> {
            matches!(
                name,
                "duplicate-evidence"
                    | "attach-evidence"
                    | "external-evidence"
                    | "write-then-error"
                    | "write-then-hang"
            )
            .then_some(ActionProtection::Standard)
            .or_else(|| {
                (name == "protected-write-then-error").then_some(ActionProtection::Protected)
            })
        }

        async fn observe(
            &self,
            context: &DriverOperationContext,
        ) -> DeviceOperationResult<Observation> {
            let mut captured = observation(&self.id);
            match self.scenario {
                ReceiptScenario::ExternalObservation => {
                    captured.screenshot = Some(test_evidence_ref());
                }
                ReceiptScenario::ExtraObservation => {
                    let _unused = Self::put_evidence(context).await?;
                }
                ReceiptScenario::ReusePreviousObservation => {
                    let count = self.observation_count.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        let reference = Self::put_evidence(context).await?;
                        *self
                            .previous
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(reference.clone());
                        captured.screenshot = Some(reference);
                    } else {
                        captured.screenshot = self
                            .previous
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                    }
                }
                ReceiptScenario::DuplicateActionEvidence
                | ReceiptScenario::AttachedActionEvidence
                | ReceiptScenario::ExternalActionEvidence
                | ReceiptScenario::WriteThenActionError
                | ReceiptScenario::WriteThenActionHang => {}
            }
            Ok(captured)
        }

        async fn execute(
            &self,
            context: &DriverOperationContext,
            call: ActionCall,
        ) -> DeviceOperationResult<ActionResult> {
            match self.scenario {
                ReceiptScenario::DuplicateActionEvidence => {
                    let reference = Self::put_evidence(context).await?;
                    let mut before = observation(&self.id);
                    before.screenshot = Some(reference.clone());
                    let mut after = observation(&self.id);
                    after.screenshot = Some(reference.clone());
                    let mut result = Self::action_result(call.id);
                    result.before = Some(before);
                    result.after = Some(after);
                    result.evidence = vec![reference.clone(), reference];
                    Ok(result)
                }
                ReceiptScenario::AttachedActionEvidence => {
                    let stored = context.evidence().attach(&test_evidence_ref()).await?;
                    let mut result = Self::action_result(call.id);
                    result.evidence = vec![stored.asset_ref()];
                    Ok(result)
                }
                ReceiptScenario::ExternalActionEvidence => {
                    let reference = test_evidence_ref();
                    let mut after = observation(&self.id);
                    after.screenshot = Some(reference.clone());
                    let mut result = Self::action_result(call.id);
                    result.after = Some(after);
                    result.evidence = vec![reference];
                    Ok(result)
                }
                ReceiptScenario::WriteThenActionError => {
                    let _receipt = Self::put_evidence(context).await?;
                    Err(DriverError::Internal("expected action failure".to_owned()).into())
                }
                ReceiptScenario::WriteThenActionHang => {
                    let _receipt = Self::put_evidence(context).await?;
                    pending().await
                }
                ReceiptScenario::ExternalObservation
                | ReceiptScenario::ExtraObservation
                | ReceiptScenario::ReusePreviousObservation => {
                    Err(DriverError::UnknownAction(call.name).into())
                }
            }
        }
    }

    #[async_trait]
    impl DeviceDriver for EvidenceDriver {
        fn id(&self) -> &DeviceId {
            &self.id
        }

        async fn connect(&self, _control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            Ok(DeviceInfo {
                id: self.id.clone(),
                name: "evidence test".to_owned(),
                platform: Platform::Mock,
                os_version: None,
                connected: true,
            })
        }

        async fn disconnect(&self, _control: &ExecutionControl) -> DriverResult<()> {
            Ok(())
        }

        async fn capabilities(
            &self,
            _control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            Ok(Vec::new())
        }

        fn action_protection(&self, _name: &str) -> Option<ActionProtection> {
            None
        }

        async fn observe(
            &self,
            context: &DriverOperationContext,
        ) -> DeviceOperationResult<Observation> {
            let bytes = b"png".to_vec();
            let stored = context
                .evidence()
                .put_with_declared_size(
                    "image/png",
                    bytes.len() as u64,
                    Box::pin(Cursor::new(bytes)),
                )
                .await?;
            let mut captured = observation(&self.id);
            captured.screenshot = Some(stored.asset_ref());
            Ok(captured)
        }

        async fn execute(
            &self,
            _context: &DriverOperationContext,
            _call: ActionCall,
        ) -> DeviceOperationResult<ActionResult> {
            Err(DriverError::UnknownAction("test".to_owned()).into())
        }
    }

    #[derive(Clone, Copy)]
    enum EvidencePutBehavior {
        Succeed,
        Block,
    }

    struct TestEvidenceStore {
        sessions: Mutex<Vec<SessionId>>,
        behavior: EvidencePutBehavior,
        started: tokio::sync::Notify,
        pending_put_dropped: Arc<AtomicBool>,
    }

    impl TestEvidenceStore {
        fn new(behavior: EvidencePutBehavior) -> Self {
            Self {
                sessions: Mutex::new(Vec::new()),
                behavior,
                started: tokio::sync::Notify::new(),
                pending_put_dropped: Arc::new(AtomicBool::new(false)),
            }
        }

        fn sessions(&self) -> Vec<SessionId> {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    struct PendingPutGuard(Arc<AtomicBool>);

    impl Drop for PendingPutGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl EvidenceStore for TestEvidenceStore {
        async fn put(
            &self,
            request: PutEvidence,
            _input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.session_id().clone());
            self.started.notify_one();
            if matches!(self.behavior, EvidencePutBehavior::Block) {
                let _guard = PendingPutGuard(Arc::clone(&self.pending_put_dropped));
                return pending().await;
            }
            let digest = Sha256Digest::parse(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            )?;
            let metadata = EvidenceMetadata::new(digest, "image/png", 3, now_ms(), 1)?;
            Ok(StoredEvidence::new(metadata, false))
        }

        async fn attach(
            &self,
            session_id: &SessionId,
            asset: &AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(session_id.clone());
            let digest = Sha256Digest::from_asset_ref(asset)?;
            let metadata = EvidenceMetadata::new(digest, asset.media_type.clone(), 3, now_ms(), 1)?;
            Ok(StoredEvidence::new(metadata, true))
        }

        async fn verify_session_reference(
            &self,
            _session_id: &SessionId,
            _asset: &AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            Err(EvidenceError::Internal(
                "unused test session reference verification".to_owned(),
            ))
        }

        async fn open(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            Err(EvidenceError::Internal("unused test open".to_owned()))
        }

        async fn metadata(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            Err(EvidenceError::Internal("unused test metadata".to_owned()))
        }

        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            Ok(self.sessions())
        }

        async fn release_session(
            &self,
            _session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            Err(EvidenceError::Internal("unused test release".to_owned()))
        }

        async fn gc(&self, _policy: GcPolicy) -> EvidenceResult<GcReport> {
            Err(EvidenceError::Internal("unused test gc".to_owned()))
        }
    }

    #[async_trait]
    impl DeviceDriver for TestDriver {
        fn id(&self) -> &DeviceId {
            &self.id
        }

        async fn connect(&self, _control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            Ok(DeviceInfo {
                id: self.id.clone(),
                name: "test".to_owned(),
                platform: Platform::Mock,
                os_version: None,
                connected: true,
            })
        }

        async fn disconnect(&self, _control: &ExecutionControl) -> DriverResult<()> {
            Ok(())
        }

        async fn capabilities(
            &self,
            _control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            match self.id.0.as_str() {
                "mismatched-protection" => Ok(vec![ActionDefinition {
                    name: "declared-protected".to_owned(),
                    description: "fixture".to_owned(),
                    input_schema: json!({ "type": "object" }),
                    protection: ActionProtection::Protected,
                }]),
                "reverse-mismatched-protection" => Ok(vec![ActionDefinition {
                    name: "declared-standard".to_owned(),
                    description: "fixture".to_owned(),
                    input_schema: json!({ "type": "object" }),
                    protection: ActionProtection::Standard,
                }]),
                _ => Ok(Vec::new()),
            }
        }

        fn action_protection(&self, name: &str) -> Option<ActionProtection> {
            if matches!(
                name,
                "protected-success"
                    | "protected-invalid"
                    | "protected-fail"
                    | "protected-hang"
                    | "declared-standard"
            ) {
                Some(ActionProtection::Protected)
            } else if matches!(
                name,
                "noop"
                    | "fail"
                    | "hang"
                    | "driver-cancelled"
                    | "driver-timed-out"
                    | "wrong-call-id"
                    | "declared-protected"
            ) {
                Some(ActionProtection::Standard)
            } else {
                None
            }
        }

        async fn observe(
            &self,
            context: &DriverOperationContext,
        ) -> DeviceOperationResult<Observation> {
            let omission = (context.screenshot_policy() == ScreenshotPolicy::Omit)
                .then_some(ScreenshotOmissionReason::Policy);
            Ok(observation_with_omission(&self.id, omission))
        }

        async fn execute(
            &self,
            context: &DriverOperationContext,
            call: ActionCall,
        ) -> DeviceOperationResult<ActionResult> {
            if call.name == "fail" {
                return Err(DriverError::Internal("expected failure".to_owned()).into());
            }
            if call.name == "protected-fail" {
                return Err(DriverError::Internal(
                    "DEVICERAIL_PROTECTED_SECRET_SENTINEL".to_owned(),
                )
                .into());
            }
            if call.name == "protected-invalid" {
                return Err(DriverError::InvalidArguments {
                    action: call.name,
                    message: "DEVICERAIL_PROTECTED_SECRET_SENTINEL".to_owned(),
                }
                .into());
            }
            if matches!(call.name.as_str(), "hang" | "protected-hang") {
                return pending().await;
            }
            if call.name == "driver-cancelled" {
                return Err(DriverError::Cancelled.into());
            }
            if call.name == "driver-timed-out" {
                return Err(DriverError::TimedOut.into());
            }
            let protection = self.action_protection(&call.name);
            if protection.is_none() {
                return Err(DriverError::UnknownAction(call.name).into());
            }
            let omission = match protection {
                Some(ActionProtection::Protected) => {
                    assert_eq!(context.screenshot_policy(), ScreenshotPolicy::Omit);
                    Some(ScreenshotOmissionReason::ProtectedAction)
                }
                Some(ActionProtection::Standard)
                    if context.screenshot_policy() == ScreenshotPolicy::Omit =>
                {
                    Some(ScreenshotOmissionReason::Policy)
                }
                Some(ActionProtection::Standard) => None,
                None => unreachable!("unknown actions returned above"),
            };
            Ok(ActionResult {
                call_id: if call.name == "wrong-call-id" {
                    Uuid::new_v4()
                } else {
                    call.id
                },
                started_at_ms: now_ms(),
                finished_at_ms: now_ms(),
                output: Value::Null,
                before: (protection == Some(ActionProtection::Protected))
                    .then(|| observation_with_omission(&self.id, omission)),
                after: Some(observation_with_omission(&self.id, omission)),
                evidence: Vec::new(),
                execution: None,
            })
        }
    }

    struct DelayedActionStartStore {
        inner: MemoryEventStore,
        delay: Duration,
    }

    struct FailObservationAppendStore {
        inner: MemoryEventStore,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ReleaseFailureTiming {
        BeforeApply,
        AfterApply,
    }

    struct FlakyObservationReleaseStore {
        inner: MemoryEventStore,
        failures_before_success: usize,
        failure_timing: ReleaseFailureTiming,
        release_attempts: AtomicUsize,
    }

    impl FlakyObservationReleaseStore {
        fn new(failures_before_success: usize, failure_timing: ReleaseFailureTiming) -> Self {
            Self {
                inner: MemoryEventStore::default(),
                failures_before_success,
                failure_timing,
                release_attempts: AtomicUsize::new(0),
            }
        }

        fn release_attempts(&self) -> usize {
            self.release_attempts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl SessionEventStore for DelayedActionStartStore {
        async fn start_session(&self, command: StartSession) -> EventStoreResult<SessionInfo> {
            self.inner.start_session(command).await
        }

        async fn reserve_observation(
            &self,
            session_id: &SessionId,
        ) -> EventStoreResult<ObservationLease> {
            self.inner.reserve_observation(session_id).await
        }

        async fn release_observation(
            &self,
            session_id: &SessionId,
            lease: ObservationLease,
        ) -> EventStoreResult<()> {
            self.inner.release_observation(session_id, lease).await
        }

        async fn append(&self, event: PendingEvent) -> EventStoreResult<TestEvent> {
            if matches!(&event.payload, TestEventPayload::ActionStarted { .. }) {
                tokio::time::sleep(self.delay).await;
            }
            self.inner.append(event).await
        }

        async fn end_session(&self, command: EndSession) -> EventStoreResult<SessionInfo> {
            self.inner.end_session(command).await
        }

        async fn list_after(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
        ) -> EventStoreResult<Vec<TestEvent>> {
            self.inner.list_after(session_id, after).await
        }

        async fn list_sessions(&self) -> EventStoreResult<Vec<SessionInfo>> {
            self.inner.list_sessions().await
        }

        async fn export_session(&self, session_id: &SessionId) -> EventStoreResult<SessionExport> {
            self.inner.export_session(session_id).await
        }

        async fn export_session_page(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
            limit: usize,
        ) -> EventStoreResult<SessionExportPageSnapshot> {
            self.inner
                .export_session_page(session_id, after, limit)
                .await
        }

        async fn delete_ended(&self, session_id: &SessionId) -> EventStoreResult<()> {
            self.inner.delete_ended(session_id).await
        }
    }

    #[async_trait]
    impl SessionEventStore for FailObservationAppendStore {
        async fn start_session(&self, command: StartSession) -> EventStoreResult<SessionInfo> {
            self.inner.start_session(command).await
        }

        async fn reserve_observation(
            &self,
            session_id: &SessionId,
        ) -> EventStoreResult<ObservationLease> {
            self.inner.reserve_observation(session_id).await
        }

        async fn release_observation(
            &self,
            session_id: &SessionId,
            lease: ObservationLease,
        ) -> EventStoreResult<()> {
            self.inner.release_observation(session_id, lease).await
        }

        async fn append(&self, event: PendingEvent) -> EventStoreResult<TestEvent> {
            if matches!(&event.payload, TestEventPayload::ObservationCaptured { .. }) {
                return Err(EventStoreError::Internal(
                    "expected observation append failure".to_owned(),
                ));
            }
            self.inner.append(event).await
        }

        async fn end_session(&self, command: EndSession) -> EventStoreResult<SessionInfo> {
            self.inner.end_session(command).await
        }

        async fn list_after(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
        ) -> EventStoreResult<Vec<TestEvent>> {
            self.inner.list_after(session_id, after).await
        }

        async fn list_sessions(&self) -> EventStoreResult<Vec<SessionInfo>> {
            self.inner.list_sessions().await
        }

        async fn export_session(&self, session_id: &SessionId) -> EventStoreResult<SessionExport> {
            self.inner.export_session(session_id).await
        }

        async fn export_session_page(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
            limit: usize,
        ) -> EventStoreResult<SessionExportPageSnapshot> {
            self.inner
                .export_session_page(session_id, after, limit)
                .await
        }

        async fn delete_ended(&self, session_id: &SessionId) -> EventStoreResult<()> {
            self.inner.delete_ended(session_id).await
        }
    }

    #[async_trait]
    impl SessionEventStore for FlakyObservationReleaseStore {
        async fn start_session(&self, command: StartSession) -> EventStoreResult<SessionInfo> {
            self.inner.start_session(command).await
        }

        async fn reserve_observation(
            &self,
            session_id: &SessionId,
        ) -> EventStoreResult<ObservationLease> {
            self.inner.reserve_observation(session_id).await
        }

        async fn release_observation(
            &self,
            session_id: &SessionId,
            lease: ObservationLease,
        ) -> EventStoreResult<()> {
            let attempt = self.release_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let should_fail = attempt <= self.failures_before_success;
            if should_fail && self.failure_timing == ReleaseFailureTiming::BeforeApply {
                return Err(EventStoreError::Internal(format!(
                    "expected observation release failure {attempt}"
                )));
            }

            self.inner.release_observation(session_id, lease).await?;
            if should_fail {
                return Err(EventStoreError::Internal(format!(
                    "expected ambiguous observation release failure {attempt}"
                )));
            }
            Ok(())
        }

        async fn append(&self, event: PendingEvent) -> EventStoreResult<TestEvent> {
            self.inner.append(event).await
        }

        async fn end_session(&self, command: EndSession) -> EventStoreResult<SessionInfo> {
            self.inner.end_session(command).await
        }

        async fn list_after(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
        ) -> EventStoreResult<Vec<TestEvent>> {
            self.inner.list_after(session_id, after).await
        }

        async fn list_sessions(&self) -> EventStoreResult<Vec<SessionInfo>> {
            self.inner.list_sessions().await
        }

        async fn export_session(&self, session_id: &SessionId) -> EventStoreResult<SessionExport> {
            self.inner.export_session(session_id).await
        }

        async fn export_session_page(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
            limit: usize,
        ) -> EventStoreResult<SessionExportPageSnapshot> {
            self.inner
                .export_session_page(session_id, after, limit)
                .await
        }

        async fn delete_ended(&self, session_id: &SessionId) -> EventStoreResult<()> {
            self.inner.delete_ended(session_id).await
        }
    }

    fn observation(device_id: &DeviceId) -> Observation {
        observation_with_omission(device_id, None)
    }

    fn observation_with_omission(
        device_id: &DeviceId,
        screenshot_omission: Option<ScreenshotOmissionReason>,
    ) -> Observation {
        Observation {
            id: Uuid::new_v4(),
            device_id: device_id.clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
            screenshot: None,
            screenshot_omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata: Map::new(),
        }
    }

    fn test_evidence_ref() -> AssetRef {
        const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        AssetRef {
            id: format!("sha256:{DIGEST}"),
            media_type: "image/png".to_owned(),
            uri: format!("devicerail://assets/sha256/{DIGEST}"),
            sha256: Some(DIGEST.to_owned()),
        }
    }

    fn action_call(name: &str) -> ActionCall {
        ActionCall {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            arguments: json!({}),
        }
    }

    async fn session_context<S>(events: &Arc<S>, device_id: &DeviceId) -> OperationContext
    where
        S: SessionEventStore + ?Sized,
    {
        let start = StartSession::new(None, Some(device_id.clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start session");
        context
    }

    #[tokio::test]
    async fn runtime_rejects_capability_and_protection_classifier_mismatches() {
        for driver_id in ["mismatched-protection", "reverse-mismatched-protection"] {
            let driver = Arc::new(TestDriver {
                id: DeviceId::new(driver_id),
            });
            let events = Arc::new(MemoryEventStore::default());
            let runtime = DeviceRuntime::new(driver, events);

            let error = runtime
                .capabilities(&ExecutionControl::unbounded())
                .await
                .expect_err("mismatched protection must fail closed");
            assert!(matches!(
                error,
                RuntimeError::Driver(DriverError::Protocol(_))
            ));
        }
    }

    #[tokio::test]
    async fn direct_execute_rejects_protection_mismatch_before_action_start() {
        const SENTINEL: &str = "DEVICERAIL_MISCLASSIFIED_PROTECTED_SENTINEL";
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("mismatched-protection"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "declared-protected".to_owned(),
                    arguments: json!({ "secret": SENTINEL }),
                },
            )
            .await
            .expect_err("direct execute must validate protection before ActionStarted");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::Protocol(_))
        ));

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert!(
            recorded.is_empty(),
            "contract failure must precede ActionStarted"
        );
        let exported = events
            .export_session(&context.session_id)
            .await
            .expect("export active Session");
        assert!(
            !serde_json::to_string(&exported)
                .expect("serialize Session export")
                .contains(SENTINEL)
        );
    }

    #[tokio::test]
    async fn runtime_emits_correlated_observation_and_action_events() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("test-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        runtime.observe(&context).await.expect("observe");
        runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::nil(),
                    name: "noop".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect("execute");

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert!(matches!(
            recorded[0].payload,
            TestEventPayload::ObservationCaptured { .. }
        ));
        assert!(matches!(
            recorded[1].payload,
            TestEventPayload::ActionStarted { .. }
        ));
        assert!(matches!(
            recorded[2].payload,
            TestEventPayload::ActionCompleted {
                outcome: ActionOutcome::Succeeded { .. },
                ..
            }
        ));
        events
            .end_session(EndSession {
                session_id: context.session_id,
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("successful observation releases lease");
    }

    #[tokio::test]
    async fn session_evidence_writer_forces_runtime_session_attribution() {
        let driver = Arc::new(EvidenceDriver {
            id: DeviceId::new("evidence-session-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
        let evidence_store: Arc<dyn EvidenceStore> = evidence.clone();
        let runtime =
            DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store);
        let context = session_context(&events, runtime.device_id()).await;

        let captured = runtime.observe(&context).await.expect("observe");

        assert!(captured.screenshot.is_some());
        assert_eq!(evidence.sessions(), vec![context.session_id.clone()]);
    }

    #[tokio::test]
    async fn default_runtime_preserves_non_store_evidence_compatibility() {
        let driver = Arc::new(ReceiptDriver::new(
            "receipt-compatibility-1",
            ReceiptScenario::ExternalObservation,
        ));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        let captured = runtime
            .observe(&context)
            .await
            .expect("compatibility runtime accepts Driver-owned evidence");
        assert_eq!(captured.screenshot, Some(test_evidence_ref()));
    }

    #[tokio::test]
    async fn strict_runtime_rejects_external_and_unreturned_observation_evidence() {
        for (scenario, expected_message) in [
            (
                ReceiptScenario::ExternalObservation,
                "driver protocol error",
            ),
            (ReceiptScenario::ExtraObservation, "driver protocol error"),
        ] {
            let driver = Arc::new(ReceiptDriver::new("receipt-observation-1", scenario));
            let events = Arc::new(MemoryEventStore::default());
            let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
            let evidence_store: Arc<dyn EvidenceStore> = evidence;
            let runtime = DeviceRuntime::with_evidence(
                Arc::clone(&driver),
                Arc::clone(&events),
                evidence_store,
            );
            let context = session_context(&events, runtime.device_id()).await;

            let error = runtime
                .observe(&context)
                .await
                .expect_err("strict runtime rejects mismatched receipts");
            assert!(matches!(
                error,
                RuntimeError::Driver(DriverError::Protocol(_))
            ));
            let error_info = error.to_error_info();
            assert_eq!(error_info.code, "protocol_error");
            assert_eq!(error_info.message, expected_message);
            assert_eq!(error_info.details, None);
            assert!(!error_info.message.contains("sha256:"));
        }
    }

    #[tokio::test]
    async fn strict_runtime_rejects_a_receipt_from_an_earlier_operation() {
        let driver = Arc::new(ReceiptDriver::new(
            "receipt-reuse-1",
            ReceiptScenario::ReusePreviousObservation,
        ));
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
        let evidence_store: Arc<dyn EvidenceStore> = evidence;
        let runtime =
            DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store);
        let context = session_context(&events, runtime.device_id()).await;

        runtime
            .observe(&context)
            .await
            .expect("first operation returns its own receipt");
        let error = runtime
            .observe(&context)
            .await
            .expect_err("second operation cannot reuse the first receipt");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::Protocol(ref message))
                if message == "observation evidence does not match this operation's receipts"
        ));
    }

    #[tokio::test]
    async fn strict_action_receipts_use_deduplicated_result_set_semantics() {
        let driver = Arc::new(ReceiptDriver::new(
            "receipt-action-duplicates-1",
            ReceiptScenario::DuplicateActionEvidence,
        ));
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
        let evidence_store: Arc<dyn EvidenceStore> = evidence;
        let runtime =
            DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store);
        let context = session_context(&events, runtime.device_id()).await;

        let result = runtime
            .execute(&context, action_call("duplicate-evidence"))
            .await
            .expect("one issued receipt may appear in every result evidence field");
        assert_eq!(result.evidence.len(), 2);
        assert_eq!(
            result.before.and_then(|value| value.screenshot),
            Some(test_evidence_ref())
        );
        assert_eq!(
            result.after.and_then(|value| value.screenshot),
            Some(test_evidence_ref())
        );
    }

    #[tokio::test]
    async fn strict_action_accepts_a_receipt_issued_by_attach() {
        let driver = Arc::new(ReceiptDriver::new(
            "receipt-action-attach-1",
            ReceiptScenario::AttachedActionEvidence,
        ));
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
        let evidence_store: Arc<dyn EvidenceStore> = evidence.clone();
        let runtime =
            DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store);
        let context = session_context(&events, runtime.device_id()).await;

        let result = runtime
            .execute(&context, action_call("attach-evidence"))
            .await
            .expect("attach issues an operation-scoped receipt");
        assert_eq!(result.evidence, vec![test_evidence_ref()]);
        assert_eq!(evidence.sessions(), vec![context.session_id]);
    }

    #[tokio::test]
    async fn strict_runtime_rejects_external_action_evidence() {
        let driver = Arc::new(ReceiptDriver::new(
            "receipt-action-external-1",
            ReceiptScenario::ExternalActionEvidence,
        ));
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
        let evidence_store: Arc<dyn EvidenceStore> = evidence;
        let runtime =
            DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store);
        let context = session_context(&events, runtime.device_id()).await;

        let error = runtime
            .execute(&context, action_call("external-evidence"))
            .await
            .expect_err("action cannot return evidence without this operation's receipt");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::Protocol(ref message))
                if message == "action evidence does not match this operation's receipts"
        ));
    }

    #[tokio::test]
    async fn receipts_do_not_replace_driver_errors_or_cooperative_cancellation() {
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));

        let error_driver = Arc::new(ReceiptDriver::new(
            "receipt-driver-error-1",
            ReceiptScenario::WriteThenActionError,
        ));
        let error_store: Arc<dyn EvidenceStore> = evidence.clone();
        let error_runtime = DeviceRuntime::with_evidence(
            Arc::clone(&error_driver),
            Arc::clone(&events),
            error_store,
        );
        let error_context = session_context(&events, error_runtime.device_id()).await;
        let error = error_runtime
            .execute(&error_context, action_call("write-then-error"))
            .await
            .expect_err("Driver error remains authoritative without a successful result");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::Internal(ref message))
                if message == "expected action failure"
        ));

        let hang_driver = Arc::new(ReceiptDriver::new(
            "receipt-cancel-1",
            ReceiptScenario::WriteThenActionHang,
        ));
        let hang_store: Arc<dyn EvidenceStore> = evidence.clone();
        let hang_runtime = Arc::new(DeviceRuntime::with_evidence(
            Arc::clone(&hang_driver),
            Arc::clone(&events),
            hang_store,
        ));
        let (controller, control) = ExecutionController::new();
        let hang_context = session_context(&events, hang_runtime.device_id())
            .await
            .with_control(control);
        let task = tokio::spawn({
            let runtime = Arc::clone(&hang_runtime);
            let context = hang_context.clone();
            async move {
                runtime
                    .execute(&context, action_call("write-then-hang"))
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while evidence.sessions().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second operation stores its receipt before cancellation");
        assert!(controller.cancel(CancellationReason::Requested));

        let cancelled = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("action observes cancellation")
            .expect("action task")
            .expect_err("cancelled action");
        assert!(matches!(
            cancelled,
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));
    }

    #[tokio::test]
    async fn protected_operations_cannot_write_to_the_configured_evidence_store() {
        let driver = Arc::new(ReceiptDriver::new(
            "protected-no-evidence-1",
            ReceiptScenario::WriteThenActionError,
        ));
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Succeed));
        let evidence_store: Arc<dyn EvidenceStore> = evidence.clone();
        let runtime =
            DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store);
        let context = session_context(&events, runtime.device_id()).await;

        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "protected-write-then-error".to_owned(),
                    arguments: json!({ "text": "DEVICERAIL_EVIDENCE_SECRET_SENTINEL" }),
                },
            )
            .await
            .expect_err("protected evidence write is rejected");
        assert!(matches!(
            error,
            RuntimeError::Evidence(EvidenceError::Unavailable)
        ));
        assert!(
            evidence.sessions().is_empty(),
            "the configured Store must never see an omitted operation write"
        );
        let serialized = serde_json::to_string(
            &events
                .export_session(&context.session_id)
                .await
                .expect("export active Session"),
        )
        .expect("serialize Session");
        assert!(!serialized.contains("DEVICERAIL_EVIDENCE_SECRET_SENTINEL"));
    }

    #[tokio::test]
    async fn unavailable_evidence_store_is_explicit_and_not_a_driver_failure() {
        let driver = Arc::new(EvidenceDriver {
            id: DeviceId::new("evidence-unavailable-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        let error = runtime
            .observe(&context)
            .await
            .expect_err("default Store rejects evidence");
        assert!(matches!(
            error,
            RuntimeError::Evidence(EvidenceError::Unavailable)
        ));

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert!(matches!(
            recorded.as_slice(),
            [TestEvent {
                payload: TestEventPayload::Error { error },
                ..
            }] if error.code == "evidence_store_unavailable"
        ));
        events
            .end_session(EndSession {
                session_id: context.session_id,
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Failed,
                reason: Some("expected evidence failure".to_owned()),
            })
            .await
            .expect("failed observation releases lease");
    }

    #[tokio::test]
    async fn in_flight_observation_blocks_end_and_cancellation_releases_lease() {
        let driver = Arc::new(EvidenceDriver {
            id: DeviceId::new("evidence-cancel-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::new(EvidencePutBehavior::Block));
        let evidence_store: Arc<dyn EvidenceStore> = evidence.clone();
        let runtime = Arc::new(DeviceRuntime::with_evidence(
            Arc::clone(&driver),
            Arc::clone(&events),
            evidence_store,
        ));
        let (controller, control) = ExecutionController::new();
        let context = session_context(&events, runtime.device_id())
            .await
            .with_control(control);
        let task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let context = context.clone();
            async move { runtime.observe(&context).await }
        });

        tokio::time::timeout(Duration::from_secs(1), evidence.started.notified())
            .await
            .expect("evidence write starts");
        let end = EndSession {
            session_id: context.session_id.clone(),
            request_id: None,
            device_id: Some(driver.id().clone()),
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        };
        let busy = events
            .end_session(end.clone())
            .await
            .expect_err("observation lease protects evidence and event append");
        assert_eq!(
            busy,
            EventStoreError::ObservationsInFlight {
                session_id: context.session_id.clone(),
                count: 1,
            }
        );
        assert_eq!(
            busy.to_error_info().details,
            Some(json!({
                "sessionId": context.session_id.clone(),
                "inFlightObservations": 1
            }))
        );

        assert!(controller.cancel(CancellationReason::Requested));
        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("observe stops")
            .expect("observe task")
            .expect_err("cancelled observe");
        assert!(matches!(
            error,
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));
        assert!(evidence.pending_put_dropped.load(Ordering::SeqCst));

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert!(matches!(
            recorded.as_slice(),
            [TestEvent {
                payload: TestEventPayload::Error { error },
                ..
            }] if error.code == "request_cancelled"
        ));
        events
            .end_session(end)
            .await
            .expect("cooperative cancellation releases observation lease");
    }

    #[tokio::test]
    async fn observation_append_failure_still_releases_lease() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("observation-append-failure-1"),
        });
        let events = Arc::new(FailObservationAppendStore {
            inner: MemoryEventStore::default(),
        });
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        let error = runtime
            .observe(&context)
            .await
            .expect_err("observation event append fails");
        assert!(matches!(
            error,
            RuntimeError::EventStore(EventStoreError::Internal(ref message))
                if message == "expected observation append failure"
        ));

        events
            .end_session(EndSession {
                session_id: context.session_id,
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Failed,
                reason: Some("expected append failure".to_owned()),
            })
            .await
            .expect("append failure releases observation lease");
    }

    #[tokio::test]
    async fn observation_release_retries_an_ambiguous_failure_and_preserves_success() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("observation-release-retry-1"),
        });
        let events = Arc::new(FlakyObservationReleaseStore::new(
            1,
            ReleaseFailureTiming::AfterApply,
        ));
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        let observed = runtime
            .observe(&context)
            .await
            .expect("retry restores the successful observation result");
        assert_eq!(observed.device_id, *runtime.device_id());
        assert_eq!(events.release_attempts(), 2);
        events
            .end_session(EndSession {
                session_id: context.session_id,
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("the retried release allows Session end");
    }

    #[tokio::test]
    async fn observation_release_retries_after_cancellation_and_preserves_cancellation() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("observation-release-cancel-1"),
        });
        let events = Arc::new(FlakyObservationReleaseStore::new(
            2,
            ReleaseFailureTiming::AfterApply,
        ));
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let (controller, control) = ExecutionController::new();
        let context = session_context(&events, runtime.device_id())
            .await
            .with_control(control);
        assert!(controller.cancel(CancellationReason::Requested));

        let error = runtime
            .observe(&context)
            .await
            .expect_err("the original cancellation remains authoritative");
        assert!(matches!(
            error,
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));
        assert_eq!(events.release_attempts(), OBSERVATION_RELEASE_MAX_ATTEMPTS);
        events
            .end_session(EndSession {
                session_id: context.session_id,
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Cancelled,
                reason: Some("expected cancellation".to_owned()),
            })
            .await
            .expect("cancelled control does not suppress lease finalization");
    }

    #[tokio::test]
    async fn permanent_observation_release_failure_is_explicit_and_keeps_session_busy() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("observation-release-permanent-1"),
        });
        let events = Arc::new(FlakyObservationReleaseStore::new(
            usize::MAX,
            ReleaseFailureTiming::BeforeApply,
        ));
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        let error = runtime
            .observe(&context)
            .await
            .expect_err("bounded retries cannot hide a permanent Store failure");
        assert!(matches!(
            error,
            RuntimeError::EventStore(EventStoreError::Internal(ref message))
                if message == "expected observation release failure 3"
        ));
        assert_eq!(events.release_attempts(), OBSERVATION_RELEASE_MAX_ATTEMPTS);
        assert_eq!(
            events
                .end_session(EndSession {
                    session_id: context.session_id.clone(),
                    request_id: None,
                    device_id: Some(driver.id().clone()),
                    at_ms: now_ms(),
                    outcome: SessionOutcome::Failed,
                    reason: Some("expected release failure".to_owned()),
                })
                .await
                .expect_err("Core cannot claim the Session is safe to end"),
            EventStoreError::ObservationsInFlight {
                session_id: context.session_id,
                count: 1,
            }
        );
    }

    #[tokio::test]
    async fn failed_actions_have_one_typed_terminal_event() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("failure-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;
        let call_id = Uuid::new_v4();

        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: call_id,
                    name: "fail".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect_err("driver failure");
        assert!(matches!(error, super::RuntimeError::Driver(_)));

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert_eq!(recorded.len(), 2);
        assert!(matches!(
            recorded[0].payload,
            TestEventPayload::ActionStarted { .. }
        ));
        assert!(matches!(
            &recorded[1].payload,
            TestEventPayload::ActionCompleted {
                call_id: completed,
                outcome: ActionOutcome::Failed { .. }
            } if *completed == call_id
        ));
        assert!(
            !recorded
                .iter()
                .any(|event| matches!(&event.payload, TestEventPayload::Error { .. }))
        );
    }

    #[tokio::test]
    async fn cancellation_before_action_start_records_no_partial_action() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("pre-cancel-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let (controller, control) = ExecutionController::new();
        assert!(controller.cancel(CancellationReason::Requested));
        let context = session_context(&events, runtime.device_id())
            .await
            .with_control(control);

        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "noop".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect_err("pre-cancelled action");
        assert!(matches!(
            error,
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));
        assert!(
            events
                .list_after(&context.session_id, Some(EventSequence::FIRST))
                .await
                .expect("events")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn in_flight_cancellation_records_exactly_one_cancelled_terminal() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("cancel-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Arc::new(DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events)));
        let (controller, control) = ExecutionController::new();
        let context = session_context(&events, runtime.device_id())
            .await
            .with_control(control);
        let call_id = Uuid::new_v4();
        let task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let context = context.clone();
            async move {
                runtime
                    .execute(
                        &context,
                        ActionCall {
                            id: call_id,
                            name: "hang".to_owned(),
                            arguments: json!({}),
                        },
                    )
                    .await
            }
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let events = events
                    .list_after(&context.session_id, Some(EventSequence::FIRST))
                    .await
                    .expect("events");
                if events.iter().any(|event| {
                    matches!(
                        &event.payload,
                        TestEventPayload::ActionStarted { call } if call.id == call_id
                    )
                }) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("action starts");

        assert!(controller.cancel(CancellationReason::Requested));
        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("action observes cancellation")
            .expect("action task")
            .expect_err("cancelled action");
        assert!(matches!(
            error,
            RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            }
        ));

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert_eq!(recorded.len(), 2);
        assert!(matches!(
            &recorded[1].payload,
            TestEventPayload::ActionCompleted {
                call_id: completed,
                outcome: ActionOutcome::Cancelled { error }
            } if *completed == call_id && error.code == "action_cancelled"
        ));
    }

    #[tokio::test]
    async fn request_and_action_timeouts_keep_distinct_scopes_and_terminals() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("timeout-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let base = session_context(&events, runtime.device_id()).await;

        let (_, request_control) = ExecutionController::with_timeout(10, TimeoutScope::Request);
        let request_context = OperationContext::new(base.session_id.clone(), None)
            .with_control(request_control)
            .with_action_timeout_ms(1_000);
        let request_call_id = Uuid::new_v4();
        let request_error = runtime
            .execute(
                &request_context,
                ActionCall {
                    id: request_call_id,
                    name: "hang".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect_err("request timeout");
        assert!(matches!(
            request_error,
            RuntimeError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: 10
            }
        ));

        let action_context =
            OperationContext::new(base.session_id.clone(), None).with_action_timeout_ms(10);
        let action_call_id = Uuid::new_v4();
        let action_error = runtime
            .execute(
                &action_context,
                ActionCall {
                    id: action_call_id,
                    name: "hang".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect_err("action timeout");
        assert!(matches!(
            action_error,
            RuntimeError::TimedOut {
                scope: TimeoutScope::Action,
                timeout_ms: 10
            }
        ));

        let recorded = events
            .list_after(&base.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert_eq!(recorded.len(), 4);
        for (call_id, expected_timeout) in [(request_call_id, 10), (action_call_id, 10)] {
            assert_eq!(
                recorded
                    .iter()
                    .filter(|event| matches!(
                        &event.payload,
                        TestEventPayload::ActionCompleted {
                            call_id: completed,
                            outcome: ActionOutcome::TimedOut { error, timeout_ms }
                        } if *completed == call_id
                            && *timeout_ms == expected_timeout
                            && error.code == "action_timeout"
                    ))
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn protected_and_unknown_arguments_never_enter_events_or_session_export() {
        const SENTINEL: &str = "DEVICERAIL_PROTECTED_SECRET_SENTINEL";

        let driver = Arc::new(TestDriver {
            id: DeviceId::new("protected-events-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Arc::new(DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events)));
        let base = session_context(&events, runtime.device_id()).await;

        let protected_call = |name: &str| ActionCall {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            arguments: json!({ "text": SENTINEL, "extra": SENTINEL }),
        };

        runtime
            .execute(&base, protected_call("protected-success"))
            .await
            .expect("protected success");

        for name in [
            "protected-invalid",
            "protected-fail",
            "unknown-protected-probe",
        ] {
            runtime
                .execute(&base, protected_call(name))
                .await
                .expect_err("expected protected failure");
        }

        let timeout_context =
            OperationContext::new(base.session_id.clone(), None).with_action_timeout_ms(10);
        let timeout_error = runtime
            .execute(&timeout_context, protected_call("protected-hang"))
            .await
            .expect_err("protected timeout");
        assert!(matches!(timeout_error, RuntimeError::TimedOut { .. }));

        let (controller, control) = ExecutionController::new();
        let cancellation_context =
            OperationContext::new(base.session_id.clone(), None).with_control(control);
        let cancellation_call = protected_call("protected-hang");
        let cancellation_id = cancellation_call.id;
        let task = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            async move {
                runtime
                    .execute(&cancellation_context, cancellation_call)
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let recorded = events
                    .list_after(&base.session_id, Some(EventSequence::FIRST))
                    .await
                    .expect("events");
                if recorded.iter().any(|event| {
                    matches!(
                        &event.payload,
                        TestEventPayload::ActionStarted { call } if call.id == cancellation_id
                    )
                }) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("protected cancellation starts");
        assert!(controller.cancel(CancellationReason::Requested));
        let cancellation_error = task
            .await
            .expect("cancellation task")
            .expect_err("protected cancellation");
        assert!(matches!(cancellation_error, RuntimeError::Cancelled { .. }));

        events
            .end_session(EndSession {
                session_id: base.session_id.clone(),
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end protected Session");
        let export = events
            .export_session(&base.session_id)
            .await
            .expect("export protected Session");
        let serialized = serde_json::to_string(&export).expect("serialize export");
        assert!(!serialized.contains(SENTINEL));
        assert!(!format!("{export:?}").contains(SENTINEL));

        let mut starts = 0;
        for event in &export.events {
            if let TestEventPayload::ActionStarted { call } = &event.payload {
                starts += 1;
                assert!(call.arguments.is_null());
                assert!(call.arguments_redacted);
            }
        }
        assert_eq!(starts, 6);
    }

    #[tokio::test]
    async fn action_timeout_starts_after_started_event_but_request_deadline_does_not_reset() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("deadline-layering-1"),
        });
        let events = Arc::new(DelayedActionStartStore {
            inner: MemoryEventStore::default(),
            delay: Duration::from_millis(30),
        });
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let base = session_context(&events, runtime.device_id()).await;

        let action_context =
            OperationContext::new(base.session_id.clone(), None).with_action_timeout_ms(5);
        runtime
            .execute(
                &action_context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "noop".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect("event-store latency is outside action timeout");

        let (_, request_control) = ExecutionController::with_timeout(5, TimeoutScope::Request);
        let request_context = OperationContext::new(base.session_id.clone(), None)
            .with_control(request_control)
            .with_action_timeout_ms(1_000);
        let error = runtime
            .execute(
                &request_context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: "noop".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect_err("request deadline includes event persistence");
        assert!(matches!(
            error,
            RuntimeError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: 5
            }
        ));
    }

    #[tokio::test]
    async fn mismatched_driver_call_id_becomes_failed_terminal_instead_of_stuck_action() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("mismatch-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;
        let call_id = Uuid::new_v4();

        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: call_id,
                    name: "wrong-call-id".to_owned(),
                    arguments: json!({}),
                },
            )
            .await
            .expect_err("invalid driver result");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::Protocol(_))
        ));

        let recorded = events
            .list_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("events");
        assert_eq!(recorded.len(), 2);
        assert!(matches!(
            &recorded[1].payload,
            TestEventPayload::ActionCompleted {
                call_id: completed,
                outcome: ActionOutcome::Failed { error }
            } if *completed == call_id && error.code == "protocol_error"
        ));

        events
            .end_session(EndSession {
                session_id: context.session_id,
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Failed,
                reason: Some("driver contract violation".to_owned()),
            })
            .await
            .expect("failed terminal clears in-flight action");
    }

    #[tokio::test]
    async fn independent_driver_stop_is_failed_not_request_cancelled_or_timed_out() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("driver-stop-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events));
        let context = session_context(&events, runtime.device_id()).await;

        for (name, expected_code) in [
            ("driver-cancelled", "driver_cancelled"),
            ("driver-timed-out", "driver_timed_out"),
        ] {
            let call_id = Uuid::new_v4();
            let error = runtime
                .execute(
                    &context,
                    ActionCall {
                        id: call_id,
                        name: name.to_owned(),
                        arguments: json!({}),
                    },
                )
                .await
                .expect_err("driver stop");
            assert!(matches!(error, RuntimeError::Driver(_)));

            let recorded = events
                .list_after(&context.session_id, Some(EventSequence::FIRST))
                .await
                .expect("events");
            assert!(matches!(
                &recorded.last().expect("terminal").payload,
                TestEventPayload::ActionCompleted {
                    call_id: completed,
                    outcome: ActionOutcome::Failed { error }
                } if *completed == call_id && error.code == expected_code
            ));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_hundred_concurrent_actions_are_replayable() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("parallel-1"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Arc::new(DeviceRuntime::new(Arc::clone(&driver), Arc::clone(&events)));
        let context = session_context(&events, runtime.device_id()).await;
        let mut subscription = events
            .subscribe_after(&context.session_id, Some(EventSequence::FIRST))
            .await
            .expect("subscribe before concurrent actions");

        let mut tasks = Vec::new();
        for index in 0..100_u64 {
            let runtime = Arc::clone(&runtime);
            let context = OperationContext::new(
                context.session_id.clone(),
                Some(devicerail_protocol::RpcId::Number(index)),
            );
            tasks.push(tokio::spawn(async move {
                let call = ActionCall {
                    id: Uuid::new_v4(),
                    name: "noop".to_owned(),
                    arguments: json!({ "index": index }),
                };
                let call_id = call.id;
                runtime.execute(&context, call).await.expect("execute");
                call_id
            }));
        }

        let mut call_ids = Vec::new();
        for task in tasks {
            call_ids.push(task.await.expect("join action"));
        }
        events
            .end_session(EndSession {
                session_id: context.session_id.clone(),
                request_id: None,
                device_id: Some(driver.id().clone()),
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");

        let replay = events
            .export_session(&context.session_id)
            .await
            .expect("export session");
        assert_eq!(replay.events.len(), 202);
        for (index, event) in replay.events.iter().enumerate() {
            assert_eq!(event.sequence.get(), index as u64 + 1);
        }
        for expected in 2..=202_u64 {
            let item = tokio::time::timeout(Duration::from_secs(1), subscription.next())
                .await
                .expect("stream delivery is bounded")
                .expect("stream event");
            assert!(matches!(
                item,
                EventStreamItem::Event(event) if event.sequence.get() == expected
            ));
        }
        assert_eq!(
            subscription.next().await,
            Some(EventStreamItem::Terminal(
                EventStreamTerminal::SessionEnded {
                    last_sequence: EventSequence::new(202).expect("terminal sequence"),
                }
            ))
        );

        let unique_events = replay
            .events
            .iter()
            .map(|event| event.event_id.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_events.len(), replay.events.len());

        type Correlation = (
            Option<u64>,
            Option<u64>,
            Option<devicerail_protocol::RpcId>,
            Option<DeviceId>,
            Option<devicerail_protocol::RpcId>,
            Option<DeviceId>,
        );
        let mut ranges = HashMap::<Uuid, Correlation>::new();
        for event in &replay.events {
            match &event.payload {
                TestEventPayload::ActionStarted { call } => {
                    ranges.entry(call.id).or_default().0 = Some(event.sequence.get());
                    ranges.entry(call.id).or_default().2 = event.request_id.clone();
                    ranges.entry(call.id).or_default().3 = event.device_id.clone();
                }
                TestEventPayload::ActionCompleted { call_id, .. } => {
                    ranges.entry(*call_id).or_default().1 = Some(event.sequence.get());
                    ranges.entry(*call_id).or_default().4 = event.request_id.clone();
                    ranges.entry(*call_id).or_default().5 = event.device_id.clone();
                }
                _ => {}
            }
        }
        assert_eq!(ranges.len(), call_ids.len());
        for call_id in call_ids {
            let (started, completed, start_request, start_device, end_request, end_device) =
                ranges[&call_id].clone();
            assert!(started.expect("start") < completed.expect("completion"));
            assert_eq!(start_request, end_request);
            assert_eq!(start_device, end_device);
        }
    }

    #[test]
    fn driver_error_wire_classification_is_stable() {
        let cases = [
            (
                DriverError::NotConnected(DeviceId::new("test-1")),
                "device_not_connected",
                true,
            ),
            (
                DriverError::UnknownAction("missing".to_owned()),
                "unknown_action",
                false,
            ),
            (
                DriverError::InvalidArguments {
                    action: "tap".to_owned(),
                    message: "x is required".to_owned(),
                },
                "invalid_arguments",
                false,
            ),
            (DriverError::ElementNotFound, "element_not_found", false),
            (DriverError::ElementAmbiguous, "element_ambiguous", false),
            (DriverError::ElementStale, "element_stale", true),
            (
                DriverError::ElementNotInteractable,
                "element_not_interactable",
                false,
            ),
            (
                DriverError::UiContextNotFound,
                "ui_context_not_found",
                false,
            ),
            (
                DriverError::UiContextAmbiguous,
                "ui_context_ambiguous",
                false,
            ),
            (DriverError::UiContextChanged, "ui_context_changed", true),
            (
                DriverError::SemanticChannelUnavailable,
                "semantic_channel_unavailable",
                false,
            ),
            (
                DriverError::Protocol("bad envelope".to_owned()),
                "protocol_error",
                false,
            ),
            (
                DriverError::Platform {
                    code: "adb_device_offline".to_owned(),
                    retryable: true,
                },
                "platform_error",
                true,
            ),
            (
                DriverError::Internal("transport closed".to_owned()),
                "internal_error",
                true,
            ),
        ];

        for (error, code, retryable) in cases {
            let info = error.to_error_info();
            assert_eq!(info.code, code);
            assert_eq!(info.retryable, retryable);
            assert!(!info.message.is_empty());
        }

        let platform = DriverError::Platform {
            code: "/Users/private/device.log".to_owned(),
            retryable: false,
        }
        .to_error_info();
        assert_eq!(platform.message, "platform operation failed");
        assert_eq!(platform.details, Some(json!({ "platformCode": "unknown" })));
        assert!(!platform.message.contains("/Users/private"));

        for error in [
            DriverError::InvalidArguments {
                action: "inputSecret".to_owned(),
                message: "DEVICERAIL_ERROR_SECRET_SENTINEL".to_owned(),
            },
            DriverError::Protocol("DEVICERAIL_ERROR_SECRET_SENTINEL".to_owned()),
            DriverError::Internal("DEVICERAIL_ERROR_SECRET_SENTINEL".to_owned()),
        ] {
            assert!(
                !error
                    .to_error_info()
                    .message
                    .contains("DEVICERAIL_ERROR_SECRET_SENTINEL")
            );
        }
    }

    #[test]
    fn protocol_15_operation_fields_are_opt_in_and_legacy_omission_stays_valid() {
        let device_id = DeviceId::new("compatibility-driver");
        let mut unsupported = observation(&device_id);
        unsupported.ui_snapshot_omission = Some(UiSnapshotOmissionReason::DriverUnsupported);
        assert!(validate_observation_omission(&unsupported, None, false).is_err());
        validate_observation_omission(&unsupported, None, true)
            .expect("negotiated UI Snapshot omission");

        let protected =
            observation_with_omission(&device_id, Some(ScreenshotOmissionReason::ProtectedAction));
        validate_observation_omission(
            &protected,
            Some(ScreenshotOmissionReason::ProtectedAction),
            false,
        )
        .expect("legacy protected Observation makes no UI Snapshot claim");

        let call_id = Uuid::new_v4();
        let semantic_result = ActionResult {
            call_id,
            started_at_ms: 1,
            finished_at_ms: 2,
            output: Value::Null,
            before: None,
            after: None,
            evidence: Vec::new(),
            execution: Some(ActionExecution::NativeSemantic {
                context: UiContextRef {
                    context_kind: UiContextKind::Native,
                    context_id: "NATIVE_APP".to_owned(),
                    document_epoch: "epoch-1".to_owned(),
                },
            }),
        };
        assert!(validate_action_result(call_id, false, semantic_result.clone()).is_err());
        validate_action_result(call_id, true, semantic_result)
            .expect("semantic execution metadata is valid only for a semantic Action");

        let missing_execution = ActionResult {
            call_id,
            started_at_ms: 1,
            finished_at_ms: 2,
            output: Value::Null,
            before: None,
            after: None,
            evidence: Vec::new(),
            execution: None,
        };
        assert!(validate_action_result(call_id, true, missing_execution).is_err());
    }

    #[tokio::test]
    async fn semantic_actions_require_durable_ui_snapshot_support() {
        let driver = Arc::new(TestDriver {
            id: DeviceId::new("semantic-feature-dependency"),
        });
        let events = Arc::new(MemoryEventStore::default());
        let session = events
            .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
            .await
            .expect("start Session");
        let runtime = DeviceRuntime::new(driver, Arc::clone(&events));
        let context =
            OperationContext::new(session.id.clone(), None).with_semantic_actions_enabled(true);

        let error = runtime
            .execute(
                &context,
                ActionCall {
                    id: Uuid::new_v4(),
                    name: FIND_ELEMENT_ACTION.to_owned(),
                    arguments: json!({"selector": {"identifier": "query"}}),
                },
            )
            .await
            .expect_err("semantic-only negotiation must fail closed");
        assert!(matches!(
            error,
            RuntimeError::Driver(DriverError::SemanticChannelUnavailable)
        ));
        let recorded = events
            .list_after(&session.id, None)
            .await
            .expect("list events");
        assert!(
            recorded.iter().all(|event| !matches!(
                event.payload,
                TestEventPayload::ActionStarted { .. }
                    | TestEventPayload::ActionCompleted { .. }
                    | TestEventPayload::Error { .. }
            )),
            "feature-gate rejection must happen before operation events are durable"
        );
    }
}
