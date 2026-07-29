use devicerail_protocol::{DeviceId, RpcId, SessionId, SessionOutcome, TestEventPayload};

use crate::{ExecutionControl, SessionEvidenceWriter, TimeoutScope};

/// Controls whether Drivers may capture screenshot evidence for an operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenshotPolicy {
    #[default]
    Capture,
    Omit,
}

/// Restricted context supplied to one Driver observation or Action.
///
/// Core derives the control from the transport-facing [`OperationContext`]
/// and binds evidence writes to that operation's Session. Request IDs,
/// Session IDs, the backing Store, and Session cleanup operations are not
/// exposed across the Driver boundary. This context is borrowed and is not
/// cloneable, keeping its evidence capability scoped to the operation future.
#[derive(Debug)]
pub struct DriverOperationContext {
    control: ExecutionControl,
    evidence: SessionEvidenceWriter,
    screenshot_policy: ScreenshotPolicy,
    ui_snapshots_enabled: bool,
    semantic_actions_enabled: bool,
}

impl DriverOperationContext {
    pub(crate) fn new(
        control: ExecutionControl,
        evidence: SessionEvidenceWriter,
        screenshot_policy: ScreenshotPolicy,
        ui_snapshots_enabled: bool,
        semantic_actions_enabled: bool,
    ) -> Self {
        Self {
            control,
            evidence,
            screenshot_policy,
            ui_snapshots_enabled,
            semantic_actions_enabled,
        }
    }

    pub const fn control(&self) -> &ExecutionControl {
        &self.control
    }

    pub const fn evidence(&self) -> &SessionEvidenceWriter {
        &self.evidence
    }

    pub const fn screenshot_policy(&self) -> ScreenshotPolicy {
        self.screenshot_policy
    }

    /// Whether this operation may return Protocol 1.5 UI Snapshot fields.
    pub const fn ui_snapshots_enabled(&self) -> bool {
        self.ui_snapshots_enabled
    }

    /// Whether this operation may execute Protocol 1.5 semantic Actions and
    /// return their execution-channel metadata.
    pub const fn semantic_actions_enabled(&self) -> bool {
        self.semantic_actions_enabled
    }
}

/// Correlates one runtime operation with its durable session and transport
/// request. The request id is optional for direct Rust callers outside RPC.
#[derive(Clone, Debug)]
pub struct OperationContext {
    pub session_id: SessionId,
    pub request_id: Option<RpcId>,
    /// Absolute control for the parent device-operation budget.
    pub control: ExecutionControl,
    /// Driver-only Action budget. It begins after `ActionStarted` is durable
    /// and can shorten, but never extend, the parent request deadline.
    pub action_timeout_ms: Option<u64>,
    screenshot_policy_override: Option<ScreenshotPolicy>,
    ui_snapshots_enabled: bool,
    semantic_actions_enabled: bool,
}

impl OperationContext {
    pub fn new(session_id: SessionId, request_id: Option<RpcId>) -> Self {
        Self {
            session_id,
            request_id,
            control: ExecutionControl::unbounded(),
            action_timeout_ms: None,
            screenshot_policy_override: None,
            ui_snapshots_enabled: false,
            semantic_actions_enabled: false,
        }
    }

    pub fn with_control(mut self, control: ExecutionControl) -> Self {
        self.control = control;
        self
    }

    pub const fn with_action_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.action_timeout_ms = Some(timeout_ms);
        self
    }

    /// Applies a per-operation screenshot policy without allowing a caller to
    /// relax a stricter runtime-wide policy. This is used by authenticated
    /// distributed routing to preserve an upstream `omit` decision.
    pub const fn with_screenshot_policy(mut self, policy: ScreenshotPolicy) -> Self {
        self.screenshot_policy_override = Some(policy);
        self
    }

    /// Enables the additive Protocol 1.5 UI Snapshot fields for this one
    /// operation. Disabled is the compatibility-safe default.
    pub const fn with_ui_snapshots_enabled(mut self, enabled: bool) -> Self {
        self.ui_snapshots_enabled = enabled;
        self
    }

    /// Enables the additive Protocol 1.5 semantic Action contract for this
    /// one operation. Disabled is the compatibility-safe default.
    pub const fn with_semantic_actions_enabled(mut self, enabled: bool) -> Self {
        self.semantic_actions_enabled = enabled;
        self
    }

    pub(crate) const fn ui_snapshots_enabled(&self) -> bool {
        self.ui_snapshots_enabled
    }

    pub(crate) const fn semantic_actions_enabled(&self) -> bool {
        self.semantic_actions_enabled
    }

    pub(crate) const fn effective_screenshot_policy(
        &self,
        runtime_policy: ScreenshotPolicy,
    ) -> ScreenshotPolicy {
        match (runtime_policy, self.screenshot_policy_override) {
            (ScreenshotPolicy::Omit, _) | (_, Some(ScreenshotPolicy::Omit)) => {
                ScreenshotPolicy::Omit
            }
            _ => ScreenshotPolicy::Capture,
        }
    }

    /// Derives a fresh Driver Action control from the current parent budget.
    /// Calling this method starts the configured Action-specific timeout.
    pub fn execution_control(&self) -> ExecutionControl {
        self.action_timeout_ms.map_or_else(
            || self.control.clone(),
            |timeout_ms| self.control.with_timeout(timeout_ms, TimeoutScope::Action),
        )
    }
}

#[derive(Clone, Debug)]
pub struct StartSession {
    pub session_id: SessionId,
    pub request_id: Option<RpcId>,
    pub device_id: Option<DeviceId>,
    pub at_ms: u64,
}

impl StartSession {
    pub fn new(request_id: Option<RpcId>, device_id: Option<DeviceId>, at_ms: u64) -> Self {
        Self {
            session_id: SessionId::new(),
            request_id,
            device_id,
            at_ms,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EndSession {
    pub session_id: SessionId,
    pub request_id: Option<RpcId>,
    pub device_id: Option<DeviceId>,
    pub at_ms: u64,
    pub outcome: SessionOutcome,
    pub reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PendingEvent {
    pub session_id: SessionId,
    pub request_id: Option<RpcId>,
    pub device_id: Option<DeviceId>,
    pub at_ms: u64,
    pub payload: TestEventPayload,
}

impl PendingEvent {
    pub fn for_operation(
        context: &OperationContext,
        device_id: Option<DeviceId>,
        at_ms: u64,
        payload: TestEventPayload,
    ) -> Self {
        Self {
            session_id: context.session_id.clone(),
            request_id: context.request_id.clone(),
            device_id,
            at_ms,
            payload,
        }
    }
}
