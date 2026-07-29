use std::{future::pending, io::Cursor, sync::Arc};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationError, DeviceOperationResult, DriverError, DriverOperationContext,
    DriverResult, ExecutionControl, ScreenshotPolicy, TimeoutScope, now_ms, run_bounded_blocking,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionExecution, ActionProtection, ActionResult,
    CLEAR_ELEMENT_ACTION, ClearElementArguments, DeviceId, DeviceInfo, ElementActionOutput,
    ElementTarget, FIND_ELEMENT_ACTION, FindElementArguments, FindElementResult, Observation,
    Platform, SET_ELEMENT_VALUE_ACTION, ScreenshotOmissionReason, SetElementValueArguments,
    TAP_ELEMENT_ACTION, TapElementArguments, UI_SNAPSHOT_FORMAT_VERSION, UiContextKind,
    UiSnapshotOmissionReason, UiSnapshotRef, WAIT_FOR_ELEMENT_ACTION, WaitForElementArguments,
    WaitForElementCondition, WaitForElementResult, is_semantic_action_name,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::{sync::Mutex, time};
use uuid::Uuid;

use crate::{
    AppiumButton, AppiumDrag, AppiumSession, AppiumSessionRequest, AppiumTransport,
    IosDeviceConfig, IosKey, MjpegFrameSource, WdaAction,
    control::{ensure_active, platform},
    driver::{
        ParsedAction, action_definitions, deduplicated_screenshots, validate_png,
        viewport_with_scale,
    },
    semantic::{
        CachedSnapshot, ResolvedNode, capture_snapshot, capture_snapshot_in_context,
        resolve_selector, resolve_target, select_context, target_context,
        validate_target_provenance,
    },
};

const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const WAIT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 10_000;

struct AppiumDriverState {
    session: AppiumSessionOwnership,
    os_version: Option<String>,
    snapshot: Option<CachedSnapshot>,
    session_generation: u64,
}

#[derive(Clone)]
enum AppiumSessionOwnership {
    Disconnected,
    Live(AppiumSession),
    /// A create request may have reached Appium, but DeviceRail did not receive
    /// a trustworthy Session id. Without an id there is no safe delete or
    /// reconciliation operation, so creating another Session must fail closed.
    OwnershipUnknown,
}

/// iOS Driver whose only automation-Session owner is Appium XCUITest.
///
/// The Driver uses WDA's accessibility snapshot through Appium in native
/// contexts and W3C DOM semantics in Safari/WebView contexts. It never creates
/// a concurrent direct-WDA Session.
pub struct AppiumIosDriver {
    config: IosDeviceConfig,
    transport: Arc<dyn AppiumTransport>,
    session_request: AppiumSessionRequest,
    mjpeg: Option<Arc<dyn MjpegFrameSource>>,
    state: Mutex<AppiumDriverState>,
}

impl std::fmt::Debug for AppiumIosDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppiumIosDriver")
            .field("device", &"[REDACTED]")
            .field("session_capabilities", &"[REDACTED]")
            .field("mjpeg_configured", &self.mjpeg.is_some())
            .finish_non_exhaustive()
    }
}

impl AppiumIosDriver {
    pub fn new(
        config: IosDeviceConfig,
        transport: Arc<dyn AppiumTransport>,
        session_request: AppiumSessionRequest,
    ) -> Self {
        let os_version = config.os_version().map(str::to_owned);
        Self {
            config,
            transport,
            session_request,
            mjpeg: None,
            state: Mutex::new(AppiumDriverState {
                session: AppiumSessionOwnership::Disconnected,
                os_version,
                snapshot: None,
                session_generation: 0,
            }),
        }
    }

    pub fn with_mjpeg(mut self, source: Arc<dyn MjpegFrameSource>) -> Self {
        self.mjpeg = Some(source);
        self
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let state = self.state.lock().await;
        self.info(&state)
    }

    fn info(&self, state: &AppiumDriverState) -> DeviceInfo {
        DeviceInfo {
            id: self.config.id().clone(),
            name: self.config.name().to_owned(),
            platform: Platform::Ios,
            os_version: state.os_version.clone(),
            connected: matches!(state.session, AppiumSessionOwnership::Live(_)),
        }
    }

    async fn create_session(
        &self,
        state: &mut AppiumDriverState,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumSession> {
        if !matches!(state.session, AppiumSessionOwnership::Disconnected) {
            return Err(DriverError::Internal(
                "refusing to create a second Appium session".to_owned(),
            ));
        }
        let next_generation = state.session_generation.checked_add(1).ok_or_else(|| {
            DriverError::Internal("Appium session generation overflow".to_owned())
        })?;
        let status = self.transport.status(control).await?;
        if !status.ready {
            return Err(platform("appium_not_ready", true));
        }
        let session = match self
            .transport
            .create_session(&self.session_request, control)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                return Self::apply_session_creation_failure(state, error);
            }
        };
        if state.os_version.is_none() {
            state.os_version = status.os_version;
        }
        state.snapshot = None;
        state.session_generation = next_generation;
        state.session = AppiumSessionOwnership::Live(session.clone());
        Ok(session)
    }

    fn apply_session_creation_failure<T>(
        state: &mut AppiumDriverState,
        error: DriverError,
    ) -> DriverResult<T> {
        if !is_definitive_session_create_failure(&error) {
            state.session = AppiumSessionOwnership::OwnershipUnknown;
            state.snapshot = None;
        }
        Err(error)
    }

    async fn retire_session(
        &self,
        state: &mut AppiumDriverState,
        session: &AppiumSession,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        Self::apply_session_retirement(state, self.transport.delete_session(session, control).await)
    }

    fn apply_session_retirement(
        state: &mut AppiumDriverState,
        result: DriverResult<()>,
    ) -> DriverResult<()> {
        match result {
            Ok(()) => {
                state.session = AppiumSessionOwnership::Disconnected;
                state.snapshot = None;
                Ok(())
            }
            Err(error) if is_explicit_session_loss(&error) => {
                state.session = AppiumSessionOwnership::Disconnected;
                state.snapshot = None;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    async fn live_session(
        &self,
        state: &mut AppiumDriverState,
        control: &ExecutionControl,
    ) -> DriverResult<AppiumSession> {
        let session = match &state.session {
            AppiumSessionOwnership::Live(session) => session.clone(),
            AppiumSessionOwnership::Disconnected => {
                return Err(DriverError::NotConnected(self.config.id().clone()));
            }
            AppiumSessionOwnership::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
        };
        match self.transport.current_context(&session, control).await {
            Ok(_) => Ok(session),
            Err(error) if is_explicit_session_loss(&error) => {
                state.session = AppiumSessionOwnership::Disconnected;
                state.snapshot = None;
                self.create_session(state, control).await
            }
            Err(error) if is_transport_session_loss(&error) => {
                self.retire_session(state, &session, control).await?;
                self.create_session(state, control).await
            }
            Err(error) => Err(error),
        }
    }

    async fn capture_with_recovery(
        &self,
        state: &mut AppiumDriverState,
        context: &DriverOperationContext,
        force_semantic_snapshot: bool,
    ) -> DeviceOperationResult<Observation> {
        let session = self.live_session(state, context.control()).await?;
        match self
            .capture(
                state,
                &session,
                context,
                context.control(),
                force_semantic_snapshot,
                false,
            )
            .await
        {
            Err(DeviceOperationError::Driver(error)) if is_explicit_session_loss(&error) => {
                state.session = AppiumSessionOwnership::Disconnected;
                state.snapshot = None;
                let session = self.create_session(state, context.control()).await?;
                self.capture(
                    state,
                    &session,
                    context,
                    context.control(),
                    force_semantic_snapshot,
                    false,
                )
                .await
            }
            Err(DeviceOperationError::Driver(error)) if is_transport_session_loss(&error) => {
                self.retire_session(state, &session, context.control())
                    .await?;
                let session = self.create_session(state, context.control()).await?;
                self.capture(
                    state,
                    &session,
                    context,
                    context.control(),
                    force_semantic_snapshot,
                    false,
                )
                .await
            }
            result => result,
        }
    }

    async fn capture(
        &self,
        state: &mut AppiumDriverState,
        session: &AppiumSession,
        context: &DriverOperationContext,
        control: &ExecutionControl,
        force_semantic_snapshot: bool,
        protected_action: bool,
    ) -> DeviceOperationResult<Observation> {
        ensure_active(control)?;
        let observation_id = Uuid::new_v4();
        let screenshot_omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit if protected_action => {
                Some(ScreenshotOmissionReason::ProtectedAction)
            }
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        let should_capture_tree = force_semantic_snapshot
            || (context.ui_snapshots_enabled() && screenshot_omission.is_none());
        // Appium's screenshot geometry depends on the selected context. Keep
        // one context identity across viewport, UI Tree, and screenshot
        // capture so a concurrent or accidental context transition cannot mix
        // native screen bounds with DOM CSS bounds.
        let appium_context = self.transport.current_context(session, control).await?;
        let material = if should_capture_tree {
            Some(
                capture_snapshot_in_context(
                    self.transport.as_ref(),
                    session,
                    &appium_context,
                    observation_id,
                    state.session_generation,
                    control,
                )
                .await?,
            )
        } else {
            None
        };
        let base_viewport = match &material {
            Some(material) => material.viewport.clone(),
            None => self.transport.viewport(session, control).await?,
        };

        let mut screenshot_capture = if screenshot_omission.is_none() {
            if !appium_context.is_native() {
                // `/screenshot` and MJPEG are complete-display captures on
                // iOS. Safari/WebView bounds are CSS viewport coordinates, so
                // those sources include browser chrome and cannot be related
                // by a single scale. XCUITest's viewport screenshot delegates
                // to WebKit and is spatially aligned with the CSS viewport.
                let bytes = self
                    .transport
                    .web_viewport_screenshot_png(session, control)
                    .await?;
                let (bytes, width, height) = run_bounded_blocking(
                    control,
                    move || {
                        let (width, height) = validate_png(&bytes)?;
                        Ok((bytes, width, height))
                    },
                    || platform("appium_invalid_screenshot", false),
                )
                .await?;
                Some((bytes, "image/png", width, height, "appium-web-viewport"))
            } else if let Some(mjpeg) = &self.mjpeg {
                let frame = mjpeg.latest_frame(control).await?;
                let width = frame.width();
                let height = frame.height();
                Some((frame.into_bytes(), "image/jpeg", width, height, "mjpeg"))
            } else {
                let bytes = self.transport.screenshot_png(session, control).await?;
                let (bytes, width, height) = run_bounded_blocking(
                    control,
                    move || {
                        let (width, height) = validate_png(&bytes)?;
                        Ok((bytes, width, height))
                    },
                    || platform("appium_invalid_screenshot", false),
                )
                .await?;
                Some((bytes, "image/png", width, height, "appium"))
            }
        } else {
            None
        };

        let viewport = screenshot_capture
            .as_ref()
            .map(|(_, _, width, height, _)| viewport_with_scale(&base_viewport, *width, *height))
            .transpose()?
            .unwrap_or(base_viewport);
        let screenshot = if let Some((bytes, media_type, _, _, _)) = &mut screenshot_capture {
            if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
                return Err(platform("appium_invalid_screenshot", false).into());
            }
            let size = u64::try_from(bytes.len())
                .map_err(|_| platform("ios_screenshot_too_large", false))?;
            Some(
                context
                    .evidence()
                    .put_with_declared_size(
                        *media_type,
                        size,
                        Box::pin(Cursor::new(std::mem::take(bytes))),
                    )
                    .await?
                    .asset_ref(),
            )
        } else {
            None
        };

        let (ui_snapshot, ui_snapshot_omission) = if context.ui_snapshots_enabled() {
            if screenshot_omission.is_some() {
                (
                    None,
                    Some(if protected_action {
                        UiSnapshotOmissionReason::ProtectedAction
                    } else {
                        UiSnapshotOmissionReason::Policy
                    }),
                )
            } else {
                let material = material.as_ref().ok_or_else(|| {
                    DriverError::Internal("missing Appium UI snapshot".to_owned())
                })?;
                let (stored, byte_length) = context
                    .evidence()
                    .put_ui_snapshot(&material.cached.snapshot)
                    .await?;
                let node_count = u32::try_from(material.cached.snapshot.nodes.len())
                    .map_err(|_| DriverError::Protocol("UI node count overflow".to_owned()))?;
                (
                    Some(UiSnapshotRef {
                        format_version: UI_SNAPSHOT_FORMAT_VERSION,
                        context: material.cached.snapshot.context.clone(),
                        node_count,
                        byte_length,
                        evidence: stored.asset_ref(),
                    }),
                    None,
                )
            }
        } else {
            (None, None)
        };

        ensure_active(control)?;
        let mut metadata = Map::new();
        metadata.insert(
            "automationBackend".to_owned(),
            Value::String("appium-xcuitest".to_owned()),
        );
        if let Some(material) = &material {
            metadata.insert(
                "sourceFormat".to_owned(),
                Value::String(material.source_format.to_owned()),
            );
            metadata.insert(
                "contextId".to_owned(),
                Value::String(material.cached.snapshot.context.context_id.clone()),
            );
            metadata.insert(
                "contextKind".to_owned(),
                serde_json::to_value(material.cached.snapshot.context.context_kind)
                    .map_err(|error| DriverError::Internal(error.to_string()))?,
            );
            state.snapshot = Some(material.cached.clone());
        }
        if let Some((_, _, _, _, source)) = screenshot_capture {
            metadata.insert(
                "screenshotSource".to_owned(),
                Value::String(source.to_owned()),
            );
        }
        Ok(Observation {
            id: observation_id,
            device_id: self.config.id().clone(),
            captured_at_ms: now_ms(),
            viewport,
            screenshot,
            screenshot_omission,
            ui_snapshot,
            ui_snapshot_omission,
            metadata,
        })
    }

    async fn execute_legacy(
        &self,
        state: &mut AppiumDriverState,
        session: &AppiumSession,
        context: &DriverOperationContext,
        call_id: Uuid,
        name: &str,
        arguments: Value,
    ) -> DeviceOperationResult<ActionResult> {
        let parsed = ParsedAction::parse(name, arguments)?;
        let before = self
            .capture(state, session, context, context.control(), false, false)
            .await?;
        let action = parsed.into_wda_action(&before.viewport)?;
        let started_at_ms = now_ms();
        self.perform_legacy(session, action, context.control())
            .await?;
        ensure_active(context.control())?;
        let after = self
            .capture(state, session, context, context.control(), false, false)
            .await?;
        let finished_at_ms = now_ms().max(started_at_ms);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output: json!({ "status": "ok" }),
            evidence: deduplicated_screenshots(&before, &after),
            before: Some(before),
            after: Some(after),
            execution: None,
        })
    }

    async fn perform_legacy(
        &self,
        session: &AppiumSession,
        action: WdaAction,
        control: &ExecutionControl,
    ) -> DriverResult<()> {
        match action {
            WdaAction::Tap { x, y } => self.transport.tap_coordinate(session, x, y, control).await,
            WdaAction::TypeText(text) => self.transport.send_keys(session, &text, control).await,
            WdaAction::Drag {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => {
                let gesture = AppiumDrag::new(start_x, start_y, end_x, end_y, duration_ms)?;
                self.transport.drag(session, gesture, control).await
            }
            WdaAction::PressKey(key) => match key {
                IosKey::Home | IosKey::VolumeUp | IosKey::VolumeDown => {
                    let button = match key {
                        IosKey::Home => AppiumButton::Home,
                        IosKey::VolumeUp => AppiumButton::VolumeUp,
                        IosKey::VolumeDown => AppiumButton::VolumeDown,
                        _ => unreachable!(),
                    };
                    self.transport.press_button(session, button, control).await
                }
                key => {
                    let value = match key {
                        IosKey::Enter => "\u{e007}",
                        IosKey::Tab => "\u{e004}",
                        IosKey::Escape => "\u{e00c}",
                        IosKey::Delete => "\u{e003}",
                        IosKey::Space => " ",
                        _ => unreachable!(),
                    };
                    self.transport.send_keys(session, value, control).await
                }
            },
        }
    }

    async fn execute_semantic(
        &self,
        state: &mut AppiumDriverState,
        session: &AppiumSession,
        context: &DriverOperationContext,
        call_id: Uuid,
        name: &str,
        arguments: Value,
    ) -> DeviceOperationResult<ActionResult> {
        if !context.semantic_actions_enabled() || !context.ui_snapshots_enabled() {
            return Err(DriverError::SemanticChannelUnavailable.into());
        }
        let action = SemanticAction::parse(name, arguments)?;
        let default_wait_control = if action.is_wait()
            && !matches!(context.control().timeout(), Some((TimeoutScope::Action, _)))
        {
            Some(
                context
                    .control()
                    .with_timeout(DEFAULT_WAIT_TIMEOUT_MS, TimeoutScope::Action),
            )
        } else {
            None
        };
        // A shorter parent request/shutdown deadline wins inside
        // `with_timeout`. Only swallow expiration when the effective deadline
        // really is the Driver's default Action deadline; parent budgets and
        // cancellation must propagate unchanged.
        let used_default_wait = default_wait_control.as_ref().is_some_and(|control| {
            matches!(
                control.timeout(),
                Some((TimeoutScope::Action, DEFAULT_WAIT_TIMEOUT_MS))
            )
        });
        let control = default_wait_control.as_ref().unwrap_or(context.control());
        let protected_action = matches!(&action, SemanticAction::SetValue(_));
        if let Some(target) = action.element_target() {
            validate_target_provenance(state.snapshot.as_ref(), target)?;
        }
        let context_selector = action.context_selector();
        select_context(
            self.transport.as_ref(),
            session,
            context_selector.as_ref(),
            control,
        )
        .await?;
        let before = self
            .capture(state, session, context, control, true, protected_action)
            .await?;
        let before_snapshot = state
            .snapshot
            .clone()
            .ok_or_else(|| DriverError::Internal("semantic capture omitted UI state".to_owned()))?;
        let started_at_ms = now_ms();
        let mut semantic_output = self
            .perform_semantic(
                state,
                session,
                action,
                &before_snapshot,
                control,
                used_default_wait,
            )
            .await?;
        let mut reuse_before = used_default_wait && semantic_output.is_wait_not_matched();
        if !reuse_before && wait_default(ensure_active(control), used_default_wait)?.is_none() {
            semantic_output.expire_wait();
            reuse_before = true;
        }
        let (after, after_snapshot) = if reuse_before {
            (before.clone(), before_snapshot.clone())
        } else {
            match wait_default_operation(
                self.capture(state, session, context, control, true, protected_action)
                    .await,
                used_default_wait,
            )? {
                Some(after) => {
                    let snapshot = state.snapshot.clone().ok_or_else(|| {
                        DriverError::Internal("semantic capture omitted UI state".to_owned())
                    })?;
                    (after, snapshot)
                }
                None => {
                    semantic_output.expire_wait();
                    (before.clone(), before_snapshot.clone())
                }
            }
        };
        if let PendingSemanticOutput::Wait { arguments, outcome } = &mut semantic_output
            && !matches!(outcome, WaitOutcome::NotMatched)
        {
            let confirmed = wait_default(
                self.wait_condition_matches(session, &after_snapshot, arguments, control)
                    .await,
                used_default_wait,
            )?
            .flatten();
            *outcome = confirmed.unwrap_or(WaitOutcome::NotMatched);
        }
        let (output, execution_context) = semantic_output.finish(&after_snapshot)?;
        let execution = match execution_context.context_kind {
            UiContextKind::Native => ActionExecution::NativeSemantic {
                context: execution_context,
            },
            UiContextKind::Web => ActionExecution::WebSemantic {
                context: execution_context,
            },
        };
        let finished_at_ms = now_ms().max(started_at_ms);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output,
            evidence: deduplicated_screenshots(&before, &after),
            before: Some(before),
            after: Some(after),
            execution: Some(execution),
        })
    }

    async fn perform_semantic(
        &self,
        state: &mut AppiumDriverState,
        session: &AppiumSession,
        action: SemanticAction,
        before: &CachedSnapshot,
        control: &ExecutionControl,
        used_default_wait: bool,
    ) -> DeviceOperationResult<PendingSemanticOutput> {
        let execution_context = before.snapshot.context.clone();
        match action {
            SemanticAction::Find(arguments) => {
                let resolved = resolve_selector(
                    self.transport.as_ref(),
                    session,
                    before,
                    &arguments.selector,
                    control,
                )
                .await?;
                resolved
                    .find(self.transport.as_ref(), session, control)
                    .await?;
                Ok(PendingSemanticOutput::Element {
                    action: SemanticElementAction::Find,
                    element: resolved.node,
                    execution_context,
                })
            }
            SemanticAction::Tap(arguments) => {
                let resolved = self
                    .resolve_element_target(session, before, &arguments.target, control)
                    .await?;
                let element = resolved
                    .find(self.transport.as_ref(), session, control)
                    .await?;
                if !self
                    .transport
                    .element_displayed(session, &element, control)
                    .await?
                    || !self
                        .transport
                        .element_enabled(session, &element, control)
                        .await?
                {
                    return Err(DriverError::ElementNotInteractable.into());
                }
                self.transport
                    .click_element(session, &element, control)
                    .await?;
                Ok(PendingSemanticOutput::Element {
                    action: SemanticElementAction::Tap,
                    element: resolved.node,
                    execution_context,
                })
            }
            SemanticAction::Clear(arguments) => {
                let resolved = self
                    .resolve_element_target(session, before, &arguments.target, control)
                    .await?;
                let element = resolved
                    .find(self.transport.as_ref(), session, control)
                    .await?;
                if !self
                    .transport
                    .element_enabled(session, &element, control)
                    .await?
                {
                    return Err(DriverError::ElementNotInteractable.into());
                }
                self.transport
                    .clear_element(session, &element, control)
                    .await?;
                Ok(PendingSemanticOutput::Element {
                    action: SemanticElementAction::Clear,
                    element: resolved.node,
                    execution_context,
                })
            }
            SemanticAction::SetValue(arguments) => {
                let resolved = self
                    .resolve_element_target(session, before, &arguments.target, control)
                    .await?;
                let element = resolved
                    .find(self.transport.as_ref(), session, control)
                    .await?;
                if !self
                    .transport
                    .element_enabled(session, &element, control)
                    .await?
                {
                    return Err(DriverError::ElementNotInteractable.into());
                }
                self.transport
                    .set_element_value(session, &element, &arguments.value, control)
                    .await?;
                Ok(PendingSemanticOutput::Element {
                    action: SemanticElementAction::SetValue,
                    element: resolved.node,
                    execution_context,
                })
            }
            SemanticAction::Wait(arguments) => {
                let outcome = self
                    .wait_for_element(
                        state,
                        session,
                        &arguments,
                        before,
                        control,
                        used_default_wait,
                    )
                    .await?;
                Ok(PendingSemanticOutput::Wait { arguments, outcome })
            }
        }
    }

    async fn resolve_element_target(
        &self,
        session: &AppiumSession,
        cached: &CachedSnapshot,
        target: &ElementTarget,
        control: &ExecutionControl,
    ) -> DriverResult<ResolvedNode> {
        if let Some(resolved) = resolve_target(cached, target)? {
            return Ok(resolved);
        }
        let ElementTarget::Selector { selector } = target else {
            unreachable!("node target returned above")
        };
        resolve_selector(self.transport.as_ref(), session, cached, selector, control).await
    }

    async fn wait_for_element(
        &self,
        state: &mut AppiumDriverState,
        session: &AppiumSession,
        arguments: &WaitForElementArguments,
        initial: &CachedSnapshot,
        control: &ExecutionControl,
        used_default: bool,
    ) -> DriverResult<WaitOutcome> {
        let mut snapshot = initial.clone();
        let mut refresh = false;
        loop {
            if refresh {
                if wait_default(
                    sleep_controlled(control, WAIT_POLL_INTERVAL).await,
                    used_default,
                )?
                .is_none()
                {
                    return Ok(WaitOutcome::NotMatched);
                }
                let selected = select_context(
                    self.transport.as_ref(),
                    session,
                    arguments.selector.context.as_ref(),
                    control,
                )
                .await;
                if matches!(selected, Err(DriverError::UiContextNotFound)) {
                    continue;
                }
                if wait_default(selected, used_default)?.is_none() {
                    return Ok(WaitOutcome::NotMatched);
                }
                let captured = wait_default(
                    capture_snapshot(
                        self.transport.as_ref(),
                        session,
                        Uuid::new_v4(),
                        state.session_generation,
                        control,
                    )
                    .await,
                    used_default,
                )?;
                let Some(captured) = captured else {
                    return Ok(WaitOutcome::NotMatched);
                };
                snapshot = captured.cached;
            }
            refresh = true;
            let matched = wait_default(
                self.wait_condition_matches(session, &snapshot, arguments, control)
                    .await,
                used_default,
            )?;
            let Some(matched) = matched else {
                return Ok(WaitOutcome::NotMatched);
            };
            if let Some(outcome) = matched {
                state.snapshot = Some(snapshot);
                return Ok(outcome);
            }
        }
    }

    async fn wait_condition_matches(
        &self,
        session: &AppiumSession,
        snapshot: &CachedSnapshot,
        arguments: &WaitForElementArguments,
        control: &ExecutionControl,
    ) -> DriverResult<Option<WaitOutcome>> {
        let resolved = resolve_selector(
            self.transport.as_ref(),
            session,
            snapshot,
            &arguments.selector,
            control,
        )
        .await;
        let resolved = match resolved {
            Ok(resolved) => Some(resolved),
            Err(DriverError::ElementNotFound) => None,
            Err(error) => return Err(error),
        };
        match arguments.condition {
            WaitForElementCondition::Absent => {
                Ok(resolved.is_none().then_some(WaitOutcome::Absent))
            }
            WaitForElementCondition::Present => {
                Ok(resolved.map(|resolved| WaitOutcome::Node(resolved.node.stable_node_id)))
            }
            WaitForElementCondition::Visible => {
                let Some(resolved) = resolved else {
                    return Ok(None);
                };
                let element = resolved
                    .find(self.transport.as_ref(), session, control)
                    .await?;
                let displayed = self
                    .transport
                    .element_displayed(session, &element, control)
                    .await?;
                Ok(displayed.then_some(WaitOutcome::Node(resolved.node.stable_node_id)))
            }
            WaitForElementCondition::Enabled => {
                let Some(resolved) = resolved else {
                    return Ok(None);
                };
                let element = resolved
                    .find(self.transport.as_ref(), session, control)
                    .await?;
                let enabled = self
                    .transport
                    .element_enabled(session, &element, control)
                    .await?;
                Ok(enabled.then_some(WaitOutcome::Node(resolved.node.stable_node_id)))
            }
        }
    }
}

#[async_trait]
impl DeviceDriver for AppiumIosDriver {
    fn id(&self) -> &DeviceId {
        self.config.id()
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        if name == SET_ELEMENT_VALUE_ACTION {
            // Element values commonly contain credentials. Treat this as a
            // protected boundary so Core redacts arguments before persistence.
            Some(ActionProtection::Protected)
        } else if is_semantic_action_name(name)
            || matches!(name, "tap" | "inputText" | "keyPress" | "swipe" | "scroll")
        {
            Some(ActionProtection::Standard)
        } else {
            None
        }
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut state = lock_state(&self.state, control).await?;
        match &state.session {
            AppiumSessionOwnership::Live(_) => {
                self.live_session(&mut state, control).await?;
                return Ok(self.info(&state));
            }
            AppiumSessionOwnership::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
            AppiumSessionOwnership::Disconnected => {}
        }
        self.create_session(&mut state, control).await?;
        Ok(self.info(&state))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
        let session = match &state.session {
            AppiumSessionOwnership::Disconnected => return Ok(()),
            AppiumSessionOwnership::Live(session) => session.clone(),
            AppiumSessionOwnership::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
        };
        self.retire_session(&mut state, &session, control).await
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        let mut definitions = action_definitions();
        definitions.extend(semantic_action_definitions()?);
        Ok(definitions)
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        let status = self.transport.status(control).await?;
        if !status.ready {
            return Err(platform("appium_not_ready", true));
        }
        let mut state = lock_state(&self.state, control).await?;
        match &state.session {
            AppiumSessionOwnership::Live(_) => {
                self.live_session(&mut state, control).await?;
            }
            AppiumSessionOwnership::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
            AppiumSessionOwnership::Disconnected => {}
        }
        Ok(())
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let mut state = lock_state(&self.state, context.control()).await?;
        self.capture_with_recovery(&mut state, context, false).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let mut state = lock_state(&self.state, context.control()).await?;
        let session = self.live_session(&mut state, context.control()).await?;
        let ActionCall {
            id: call_id,
            name,
            arguments,
        } = call;
        if is_semantic_action_name(&name) {
            self.execute_semantic(&mut state, &session, context, call_id, &name, arguments)
                .await
        } else {
            self.execute_legacy(&mut state, &session, context, call_id, &name, arguments)
                .await
        }
    }
}

enum SemanticAction {
    Find(FindElementArguments),
    Tap(TapElementArguments),
    Clear(ClearElementArguments),
    SetValue(SetElementValueArguments),
    Wait(WaitForElementArguments),
}

impl SemanticAction {
    fn parse(name: &str, arguments: Value) -> DriverResult<Self> {
        match name {
            FIND_ELEMENT_ACTION => parse_arguments(name, arguments).map(Self::Find),
            TAP_ELEMENT_ACTION => parse_arguments(name, arguments).map(Self::Tap),
            CLEAR_ELEMENT_ACTION => parse_arguments(name, arguments).map(Self::Clear),
            SET_ELEMENT_VALUE_ACTION => parse_arguments(name, arguments).map(Self::SetValue),
            WAIT_FOR_ELEMENT_ACTION => parse_arguments(name, arguments).map(Self::Wait),
            _ => Err(DriverError::UnknownAction(name.to_owned())),
        }
    }

    fn context_selector(&self) -> Option<devicerail_protocol::UiContextSelector> {
        match self {
            Self::Find(arguments) => arguments.selector.context.clone(),
            Self::Tap(arguments) => target_context(&arguments.target),
            Self::Clear(arguments) => target_context(&arguments.target),
            Self::SetValue(arguments) => target_context(&arguments.target),
            Self::Wait(arguments) => arguments.selector.context.clone(),
        }
    }

    fn element_target(&self) -> Option<&ElementTarget> {
        match self {
            Self::Tap(arguments) => Some(&arguments.target),
            Self::Clear(arguments) => Some(&arguments.target),
            Self::SetValue(arguments) => Some(&arguments.target),
            Self::Find(_) | Self::Wait(_) => None,
        }
    }

    const fn is_wait(&self) -> bool {
        matches!(self, Self::Wait(_))
    }
}

trait ValidateSemanticArguments {
    fn validate_arguments(&self) -> Result<(), devicerail_protocol::UiContractError>;
}

impl ValidateSemanticArguments for FindElementArguments {
    fn validate_arguments(&self) -> Result<(), devicerail_protocol::UiContractError> {
        self.validate()
    }
}

impl ValidateSemanticArguments for TapElementArguments {
    fn validate_arguments(&self) -> Result<(), devicerail_protocol::UiContractError> {
        self.validate()
    }
}

impl ValidateSemanticArguments for ClearElementArguments {
    fn validate_arguments(&self) -> Result<(), devicerail_protocol::UiContractError> {
        self.validate()
    }
}

impl ValidateSemanticArguments for SetElementValueArguments {
    fn validate_arguments(&self) -> Result<(), devicerail_protocol::UiContractError> {
        self.validate()
    }
}

impl ValidateSemanticArguments for WaitForElementArguments {
    fn validate_arguments(&self) -> Result<(), devicerail_protocol::UiContractError> {
        self.validate()
    }
}

fn parse_arguments<T>(action: &str, arguments: Value) -> DriverResult<T>
where
    T: DeserializeOwned + ValidateSemanticArguments,
{
    let parsed =
        serde_json::from_value::<T>(arguments).map_err(|_| DriverError::InvalidArguments {
            action: action.to_owned(),
            message: "arguments do not match the canonical semantic Action schema".to_owned(),
        })?;
    parsed
        .validate_arguments()
        .map_err(|error| DriverError::InvalidArguments {
            action: action.to_owned(),
            message: error.to_string(),
        })?;
    Ok(parsed)
}

enum SemanticElementAction {
    Find,
    Tap,
    Clear,
    SetValue,
}

enum PendingSemanticOutput {
    Element {
        action: SemanticElementAction,
        element: devicerail_protocol::UiNodeRef,
        execution_context: devicerail_protocol::UiContextRef,
    },
    Wait {
        arguments: WaitForElementArguments,
        outcome: WaitOutcome,
    },
}

enum WaitOutcome {
    NotMatched,
    Absent,
    Node(String),
}

impl PendingSemanticOutput {
    const fn is_wait_not_matched(&self) -> bool {
        matches!(
            self,
            Self::Wait {
                outcome: WaitOutcome::NotMatched,
                ..
            }
        )
    }

    fn expire_wait(&mut self) {
        if let Self::Wait { outcome, .. } = self {
            *outcome = WaitOutcome::NotMatched;
        }
    }

    fn finish(
        self,
        after: &CachedSnapshot,
    ) -> DriverResult<(Value, devicerail_protocol::UiContextRef)> {
        match self {
            Self::Element {
                action,
                element,
                execution_context,
            } => {
                // A successful mutation is allowed to navigate, close a view,
                // or remove its target. Keep the reference tied to the returned
                // before Observation that was actually used to resolve it;
                // retargeting it into `after` can both misidentify a replacement
                // node and turn an acknowledged mutation into a false failure.
                let output = match action {
                    SemanticElementAction::Find => {
                        serde_json::to_value(FindElementResult { element })
                    }
                    SemanticElementAction::Tap
                    | SemanticElementAction::Clear
                    | SemanticElementAction::SetValue => {
                        serde_json::to_value(ElementActionOutput { element })
                    }
                }
                .map_err(|error| DriverError::Internal(error.to_string()))?;
                Ok((output, execution_context))
            }
            Self::Wait { arguments, outcome } => {
                let matched = !matches!(&outcome, WaitOutcome::NotMatched);
                let element = match outcome {
                    WaitOutcome::NotMatched => None,
                    WaitOutcome::Absent => None,
                    WaitOutcome::Node(stable_node_id) => {
                        if !after
                            .snapshot
                            .nodes
                            .iter()
                            .any(|node| node.stable_node_id == stable_node_id)
                        {
                            return Err(DriverError::UiContextChanged);
                        }
                        Some(devicerail_protocol::UiNodeRef {
                            observation_id: after.snapshot.observation_id,
                            context: after.snapshot.context.clone(),
                            stable_node_id,
                        })
                    }
                };
                let result = WaitForElementResult {
                    matched,
                    condition: arguments.condition,
                    element,
                };
                result
                    .validate()
                    .map_err(|error| DriverError::Protocol(error.to_string()))?;
                let output = serde_json::to_value(result)
                    .map_err(|error| DriverError::Internal(error.to_string()))?;
                Ok((output, after.snapshot.context.clone()))
            }
        }
    }
}

fn semantic_action_definitions() -> DriverResult<Vec<ActionDefinition>> {
    let definitions = [
        (
            FIND_ELEMENT_ACTION,
            "Find one element through the active native accessibility or web DOM channel",
            include_str!("../../../protocol/schema/v1/find-element-arguments.schema.json"),
        ),
        (
            TAP_ELEMENT_ACTION,
            "Tap one resolved native accessibility or web DOM element",
            include_str!("../../../protocol/schema/v1/tap-element-arguments.schema.json"),
        ),
        (
            CLEAR_ELEMENT_ACTION,
            "Clear one resolved native accessibility or web DOM element",
            include_str!("../../../protocol/schema/v1/clear-element-arguments.schema.json"),
        ),
        (
            SET_ELEMENT_VALUE_ACTION,
            "Set the value of one resolved native accessibility or web DOM element",
            include_str!("../../../protocol/schema/v1/set-element-value-arguments.schema.json"),
        ),
        (
            WAIT_FOR_ELEMENT_ACTION,
            "Wait for one canonical element condition using semantic UI state",
            include_str!("../../../protocol/schema/v1/wait-for-element-arguments.schema.json"),
        ),
    ];
    definitions
        .into_iter()
        .map(|(name, description, schema)| {
            let input_schema = serde_json::from_str(schema).map_err(|error| {
                DriverError::Internal(format!("invalid embedded semantic Action schema: {error}"))
            })?;
            Ok(ActionDefinition {
                name: name.to_owned(),
                description: description.to_owned(),
                protection: if name == SET_ELEMENT_VALUE_ACTION {
                    ActionProtection::Protected
                } else {
                    ActionProtection::Standard
                },
                input_schema,
            })
        })
        .collect()
}

fn is_explicit_session_loss(error: &DriverError) -> bool {
    matches!(error, DriverError::Platform { code, .. } if code == "appium_invalid_session")
}

fn is_definitive_session_create_failure(error: &DriverError) -> bool {
    matches!(error, DriverError::Cancelled | DriverError::TimedOut)
        || matches!(
            error,
            DriverError::Platform { code, .. } if code == "appium_connect_failed"
        )
        || matches!(
            error,
            DriverError::Platform { code, retryable: true } if code == "appium_session_not_created"
        )
}

fn session_ownership_unknown() -> DriverError {
    platform("appium_session_ownership_unknown", false)
}

fn is_transport_session_loss(error: &DriverError) -> bool {
    matches!(
        error,
        DriverError::Platform { code, .. }
            if matches!(
                code.as_str(),
                "appium_connect_failed" | "appium_read_failed" | "appium_write_failed"
            )
    )
}

async fn lock_state<'a>(
    state: &'a Mutex<AppiumDriverState>,
    control: &ExecutionControl,
) -> DriverResult<tokio::sync::MutexGuard<'a, AppiumDriverState>> {
    ensure_active(control)?;
    let deadline = async {
        match control.remaining() {
            Some(remaining) => time::sleep(remaining).await,
            None => pending::<()>().await,
        }
    };
    tokio::select! {
        biased;
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = deadline => Err(DriverError::TimedOut),
        guard = state.lock() => Ok(guard),
    }
}

async fn sleep_controlled(
    control: &ExecutionControl,
    duration: std::time::Duration,
) -> DriverResult<()> {
    ensure_active(control)?;
    let delay = control
        .remaining()
        .map_or(duration, |remaining| remaining.min(duration));
    tokio::select! {
        biased;
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = time::sleep(delay) => {
            ensure_active(control)
        }
    }
}

fn wait_default<T>(result: DriverResult<T>, used_default: bool) -> DriverResult<Option<T>> {
    match result {
        Err(DriverError::TimedOut) if used_default => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(error) => Err(error),
    }
}

fn wait_default_operation<T>(
    result: DeviceOperationResult<T>,
    used_default: bool,
) -> DeviceOperationResult<Option<T>> {
    match result {
        Err(DeviceOperationError::Driver(DriverError::TimedOut)) if used_default => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_semantic_schemas_are_embedded_without_reconstruction() {
        let definitions = semantic_action_definitions().expect("embedded schemas");
        assert_eq!(
            definitions.len(),
            devicerail_protocol::SEMANTIC_ACTION_NAMES.len()
        );
        for name in devicerail_protocol::SEMANTIC_ACTION_NAMES {
            let definition = definitions
                .iter()
                .find(|definition| definition.name == name)
                .expect("semantic Action definition");
            assert_eq!(
                definition.input_schema["$schema"],
                "https://json-schema.org/draft/2020-12/schema"
            );
        }
        assert_eq!(
            definitions
                .iter()
                .find(|definition| definition.name == SET_ELEMENT_VALUE_ACTION)
                .expect("setElementValue definition")
                .protection,
            ActionProtection::Protected
        );
    }

    #[test]
    fn failed_session_delete_retains_single_owner_for_retry() {
        let session = AppiumSession::parse("owned-session").expect("session");
        let mut state = AppiumDriverState {
            session: AppiumSessionOwnership::Live(session.clone()),
            os_version: None,
            snapshot: None,
            session_generation: 4,
        };
        let failure = DriverError::Platform {
            code: "appium_read_failed".to_owned(),
            retryable: true,
        };
        assert!(AppiumIosDriver::apply_session_retirement(&mut state, Err(failure)).is_err());
        assert!(matches!(
            &state.session,
            AppiumSessionOwnership::Live(owned) if owned.as_str() == "owned-session"
        ));

        AppiumIosDriver::apply_session_retirement(&mut state, Ok(())).expect("confirmed deletion");
        assert!(matches!(
            state.session,
            AppiumSessionOwnership::Disconnected
        ));
        assert_eq!(state.session_generation, 4);
    }

    #[test]
    fn ambiguous_session_creation_failure_poison_ownership_fail_closed() {
        let state = || AppiumDriverState {
            session: AppiumSessionOwnership::Disconnected,
            os_version: None,
            snapshot: None,
            session_generation: 2,
        };

        for error in [
            DriverError::Platform {
                code: "appium_read_failed".to_owned(),
                retryable: true,
            },
            DriverError::Platform {
                code: "appium_command_outcome_unknown".to_owned(),
                retryable: false,
            },
            DriverError::Platform {
                code: "appium_session_not_created".to_owned(),
                retryable: false,
            },
        ] {
            let mut poisoned = state();
            let result: DriverResult<()> =
                AppiumIosDriver::apply_session_creation_failure(&mut poisoned, error);
            assert!(result.is_err());
            assert!(matches!(
                poisoned.session,
                AppiumSessionOwnership::OwnershipUnknown
            ));
            assert!(matches!(
                session_ownership_unknown(),
                DriverError::Platform { code, retryable: false }
                    if code == "appium_session_ownership_unknown"
            ));
        }

        for error in [
            DriverError::Cancelled,
            DriverError::TimedOut,
            DriverError::Platform {
                code: "appium_connect_failed".to_owned(),
                retryable: true,
            },
            DriverError::Platform {
                code: "appium_session_not_created".to_owned(),
                retryable: true,
            },
        ] {
            let mut disconnected = state();
            let result: DriverResult<()> =
                AppiumIosDriver::apply_session_creation_failure(&mut disconnected, error);
            assert!(result.is_err());
            assert!(matches!(
                disconnected.session,
                AppiumSessionOwnership::Disconnected
            ));
        }
    }

    #[test]
    fn acknowledged_element_mutation_keeps_the_before_reference() {
        let before = crate::semantic::normalize_native(
            Uuid::new_v4(),
            9,
            &crate::AppiumContext::native(),
            serde_json::json!({
                "type": "XCUIElementTypeApplication",
                "identifier": "com.example.before",
                "children": [{
                    "type": "XCUIElementTypeButton",
                    "identifier": "dismiss",
                    "label": "Dismiss",
                    "children": []
                }]
            }),
        )
        .expect("before tree");
        let before_node = &before.snapshot.nodes[1];
        let element = devicerail_protocol::UiNodeRef {
            observation_id: before.snapshot.observation_id,
            context: before.snapshot.context.clone(),
            stable_node_id: before_node.stable_node_id.clone(),
        };
        let after = crate::semantic::normalize_native(
            Uuid::new_v4(),
            9,
            &crate::AppiumContext::native(),
            serde_json::json!({
                "type": "XCUIElementTypeApplication",
                "identifier": "com.example.after",
                "children": []
            }),
        )
        .expect("after tree without the mutation target");

        let (output, execution_context) = PendingSemanticOutput::Element {
            action: SemanticElementAction::Tap,
            element: element.clone(),
            execution_context: before.snapshot.context.clone(),
        }
        .finish(&after)
        .expect("successful mutation output");
        let output = serde_json::from_value::<ElementActionOutput>(output)
            .expect("canonical element output");
        assert_eq!(output.element, element);
        assert_eq!(execution_context, before.snapshot.context);
        assert_ne!(execution_context, after.snapshot.context);
    }

    #[test]
    fn default_wait_timeout_becomes_a_non_match_but_parent_timeout_propagates() {
        assert!(matches!(
            wait_default::<()>(Err(DriverError::TimedOut), true),
            Ok(None)
        ));
        assert!(matches!(
            wait_default::<()>(Err(DriverError::TimedOut), false),
            Err(DriverError::TimedOut)
        ));
        let canonical = WaitForElementResult {
            matched: false,
            condition: WaitForElementCondition::Present,
            element: None,
        };
        canonical.validate().expect("canonical non-match");
    }

    #[test]
    fn driver_debug_redacts_device_and_capabilities() {
        struct UnusedTransport;

        #[async_trait]
        impl AppiumTransport for UnusedTransport {
            async fn status(&self, _: &ExecutionControl) -> DriverResult<crate::AppiumStatus> {
                unreachable!()
            }
            async fn create_session(
                &self,
                _: &AppiumSessionRequest,
                _: &ExecutionControl,
            ) -> DriverResult<AppiumSession> {
                unreachable!()
            }
            async fn delete_session(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn contexts(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<Vec<crate::AppiumContext>> {
                unreachable!()
            }
            async fn current_context(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<crate::AppiumContext> {
                unreachable!()
            }
            async fn switch_context(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumContext,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn native_source_json(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<Value> {
                unreachable!()
            }
            async fn page_source(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<String> {
                unreachable!()
            }
            async fn viewport(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<devicerail_protocol::Viewport> {
                unreachable!()
            }
            async fn screenshot_png(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<Vec<u8>> {
                unreachable!()
            }
            async fn web_viewport_screenshot_png(
                &self,
                _: &AppiumSession,
                _: &ExecutionControl,
            ) -> DriverResult<Vec<u8>> {
                unreachable!()
            }
            async fn execute_script(
                &self,
                _: &AppiumSession,
                _: &str,
                _: &[Value],
                _: &ExecutionControl,
            ) -> DriverResult<Value> {
                unreachable!()
            }
            async fn find_element(
                &self,
                _: &AppiumSession,
                _: crate::AppiumLocatorStrategy,
                _: &str,
                _: &ExecutionControl,
            ) -> DriverResult<crate::AppiumElement> {
                unreachable!()
            }
            async fn element_rect(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &ExecutionControl,
            ) -> DriverResult<devicerail_protocol::UiRect> {
                unreachable!()
            }
            async fn element_attribute(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &str,
                _: &ExecutionControl,
            ) -> DriverResult<Option<Value>> {
                unreachable!()
            }
            async fn element_displayed(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &ExecutionControl,
            ) -> DriverResult<bool> {
                unreachable!()
            }
            async fn element_enabled(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &ExecutionControl,
            ) -> DriverResult<bool> {
                unreachable!()
            }
            async fn click_element(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn clear_element(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn set_element_value(
                &self,
                _: &AppiumSession,
                _: &crate::AppiumElement,
                _: &str,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn tap_coordinate(
                &self,
                _: &AppiumSession,
                _: u32,
                _: u32,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn drag(
                &self,
                _: &AppiumSession,
                _: AppiumDrag,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn send_keys(
                &self,
                _: &AppiumSession,
                _: &str,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
            async fn press_button(
                &self,
                _: &AppiumSession,
                _: AppiumButton,
                _: &ExecutionControl,
            ) -> DriverResult<()> {
                unreachable!()
            }
        }

        let device = IosDeviceConfig::new(
            "APPIUM-DEVICE-SECRET",
            "APPIUM-DEVICE-NAME-SECRET",
            Some("APPIUM-OS-SECRET".to_owned()),
        )
        .expect("device config");
        let request = AppiumSessionRequest::new("APPIUM-UDID-SECRET").expect("session request");
        let driver = AppiumIosDriver::new(device, Arc::new(UnusedTransport), request);
        let debug = format!("{driver:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("SECRET"));
    }
}
