use std::{future::pending, io::Cursor, sync::Arc};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms, run_bounded_blocking,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, Platform, ScreenshotOmissionReason, Viewport, json_integer_as_i32,
    json_integer_as_u32,
};
use png::{DecodeOptions, Decoder, Limits};
use serde_json::{Map, Value, json};
use tokio::{sync::Mutex, time};
use uuid::Uuid;

use crate::{
    IosDeviceConfig, IosKey, MjpegFrameSource, WdaAction, WdaPage, WdaSession, WdaTransport,
    control::{ensure_active, platform},
};

const MAX_TEXT_CHARS: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_SWIPE_DURATION_MS: u32 = 60_000;
const MAX_SCROLL_DELTA: i64 = 100_000;
const SCROLL_DURATION_MS: u32 = 300;
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 16_384;
const MAX_SCREENSHOT_PIXELS: u64 = 64_000_000;
const MAX_SCREENSHOT_DECODED_BYTES: usize = 256 * 1024 * 1024;
const MAX_SCREENSHOT_SCALE_FACTOR: f64 = 8.0;
const MAX_SCREENSHOT_RELATIVE_SCALE_ERROR: f64 = 0.005;

struct DriverState {
    session: WdaSessionState,
    os_version: Option<String>,
}

#[derive(Clone)]
enum WdaSessionState {
    Disconnected,
    /// WDA definitively rejected the previous Session id. The stale id has
    /// been discarded and a read/lifecycle operation may safely establish a
    /// replacement without duplicating a device mutation.
    ReconnectPending,
    Live(WdaSession),
    /// A create request may have reached WDA, but no trustworthy Session id
    /// was returned. Creating another Session could duplicate ownership, so
    /// all lifecycle operations fail closed until the Driver is recreated.
    OwnershipUnknown,
}

/// DeviceRail iOS Driver backed by an explicit WebDriverAgent connection.
pub struct IosDriver {
    config: IosDeviceConfig,
    transport: Arc<dyn WdaTransport>,
    mjpeg: Option<Arc<dyn MjpegFrameSource>>,
    state: Mutex<DriverState>,
}

impl std::fmt::Debug for IosDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IosDriver")
            .field("device", &"[REDACTED]")
            .field("mjpeg_configured", &self.mjpeg.is_some())
            .finish_non_exhaustive()
    }
}

impl IosDriver {
    pub fn new(config: IosDeviceConfig, transport: Arc<dyn WdaTransport>) -> Self {
        let os_version = config.os_version().map(str::to_owned);
        Self {
            config,
            transport,
            mjpeg: None,
            state: Mutex::new(DriverState {
                session: WdaSessionState::Disconnected,
                os_version,
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

    fn info(&self, state: &DriverState) -> DeviceInfo {
        DeviceInfo {
            id: self.config.id().clone(),
            name: self.config.name().to_owned(),
            platform: Platform::Ios,
            os_version: state.os_version.clone(),
            connected: matches!(state.session, WdaSessionState::Live(_)),
        }
    }

    async fn establish_session(
        &self,
        state: &mut DriverState,
        control: &ExecutionControl,
    ) -> DriverResult<WdaSession> {
        match state.session {
            WdaSessionState::Live(_) => {
                return Err(DriverError::Internal(
                    "refusing to create a second direct WDA session".to_owned(),
                ));
            }
            WdaSessionState::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
            WdaSessionState::Disconnected | WdaSessionState::ReconnectPending => {}
        }
        let status = self.transport.status(control).await?;
        if !status.ready {
            return Err(platform("wda_not_ready", true));
        }
        let session = match self.transport.create_session(control).await {
            Ok(session) => session,
            Err(error) => {
                if is_ambiguous_session_creation(&error) {
                    state.session = WdaSessionState::OwnershipUnknown;
                }
                return Err(error);
            }
        };
        if state.os_version.is_none() {
            state.os_version = status.os_version;
        }
        state.session = WdaSessionState::Live(session.clone());
        Ok(session)
    }

    async fn session_for_read(
        &self,
        state: &mut DriverState,
        control: &ExecutionControl,
    ) -> DriverResult<WdaSession> {
        match &state.session {
            WdaSessionState::Live(session) => Ok(session.clone()),
            WdaSessionState::ReconnectPending => self.establish_session(state, control).await,
            WdaSessionState::Disconnected => {
                Err(DriverError::NotConnected(self.config.id().clone()))
            }
            WdaSessionState::OwnershipUnknown => Err(session_ownership_unknown()),
        }
    }

    async fn validate_live_session(
        &self,
        state: &mut DriverState,
        control: &ExecutionControl,
    ) -> DriverResult<WdaSession> {
        let session = self.session_for_read(state, control).await?;
        match self.transport.probe_session(&session, control).await {
            Ok(()) => Ok(session),
            Err(error) if is_explicit_session_loss(&error) => {
                state.session = WdaSessionState::ReconnectPending;
                self.establish_session(state, control).await
            }
            Err(error) => Err(error),
        }
    }

    async fn capture_with_recovery(
        &self,
        state: &mut DriverState,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let session = self.session_for_read(state, context.control()).await?;
        match self.capture(&session, context).await {
            Err(devicerail_core::DeviceOperationError::Driver(error))
                if is_explicit_session_loss(&error) =>
            {
                state.session = WdaSessionState::ReconnectPending;
                let session = self.establish_session(state, context.control()).await?;
                let result = self.capture(&session, context).await;
                if matches!(
                    &result,
                    Err(devicerail_core::DeviceOperationError::Driver(error))
                        if is_explicit_session_loss(error)
                ) {
                    state.session = WdaSessionState::ReconnectPending;
                }
                result
            }
            result => result,
        }
    }

    async fn capture(
        &self,
        session: &WdaSession,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let page = self.transport.inspect(session, context.control()).await?;
        validate_page(&page)?;
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        let (screenshot, viewport, screenshot_source) = if omission.is_some() {
            (None, page.viewport.clone(), None)
        } else {
            let capture = if let Some(mjpeg) = &self.mjpeg {
                let frame = mjpeg.latest_frame(context.control()).await?;
                let width = frame.width();
                let height = frame.height();
                ScreenshotCapture {
                    bytes: frame.into_bytes(),
                    media_type: "image/jpeg",
                    width,
                    height,
                    source: "mjpeg",
                }
            } else {
                let bytes = self
                    .transport
                    .screenshot_png(session, context.control())
                    .await?;
                let (bytes, width, height) = run_bounded_blocking(
                    context.control(),
                    move || {
                        let (width, height) = validate_png(&bytes)?;
                        Ok((bytes, width, height))
                    },
                    || platform("wda_invalid_screenshot", false),
                )
                .await?;
                ScreenshotCapture {
                    bytes,
                    media_type: "image/png",
                    width,
                    height,
                    source: "wda",
                }
            };
            let viewport = viewport_with_scale(&page.viewport, capture.width, capture.height)?;
            let size = u64::try_from(capture.bytes.len())
                .map_err(|_| platform("ios_screenshot_too_large", false))?;
            let stored = context
                .evidence()
                .put_with_declared_size(
                    capture.media_type,
                    size,
                    Box::pin(Cursor::new(capture.bytes)),
                )
                .await?;
            (Some(stored.asset_ref()), viewport, Some(capture.source))
        };
        ensure_active(context.control())?;
        let mut metadata = Map::new();
        metadata.insert("pageSource".to_owned(), Value::String(page.source));
        metadata.insert("sourceFormat".to_owned(), Value::String("xml".to_owned()));
        if let Some(source) = screenshot_source {
            metadata.insert(
                "screenshotSource".to_owned(),
                Value::String(source.to_owned()),
            );
        }
        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.config.id().clone(),
            captured_at_ms: now_ms(),
            viewport,
            screenshot,
            screenshot_omission: omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata,
        })
    }
}

#[async_trait]
impl DeviceDriver for IosDriver {
    fn id(&self) -> &DeviceId {
        self.config.id()
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        match name {
            "tap" | "inputText" | "keyPress" | "swipe" | "scroll" => {
                Some(ActionProtection::Standard)
            }
            _ => None,
        }
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut state = lock_state(&self.state, control).await?;
        match state.session {
            WdaSessionState::Live(_) => {
                self.validate_live_session(&mut state, control).await?;
            }
            WdaSessionState::Disconnected | WdaSessionState::ReconnectPending => {
                self.establish_session(&mut state, control).await?;
            }
            WdaSessionState::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
        }
        Ok(self.info(&state))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
        let session = match &state.session {
            WdaSessionState::Disconnected => return Ok(()),
            WdaSessionState::ReconnectPending => {
                state.session = WdaSessionState::Disconnected;
                return Ok(());
            }
            WdaSessionState::Live(session) => session.clone(),
            WdaSessionState::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
        };
        match self.transport.delete_session(&session, control).await {
            Ok(()) => {
                state.session = WdaSessionState::Disconnected;
                Ok(())
            }
            Err(DriverError::Platform { ref code, .. }) if code == "wda_invalid_session" => {
                state.session = WdaSessionState::Disconnected;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(action_definitions())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
        let status = self.transport.status(control).await?;
        if !status.ready {
            return Err(platform("wda_not_ready", true));
        }
        match state.session {
            WdaSessionState::Live(_) => {
                self.validate_live_session(&mut state, control).await?;
            }
            WdaSessionState::ReconnectPending => {
                self.establish_session(&mut state, control).await?;
            }
            WdaSessionState::Disconnected => {}
            WdaSessionState::OwnershipUnknown => {
                return Err(session_ownership_unknown());
            }
        }
        Ok(())
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let mut state = lock_state(&self.state, context.control()).await?;
        self.capture_with_recovery(&mut state, context).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let mut state = lock_state(&self.state, context.control()).await?;
        // Preserve the Driver contract: disconnected state is reported before
        // Action validation, while a known-stale Session can be safely
        // replaced before any device mutation is sent.
        self.session_for_read(&mut state, context.control()).await?;
        let ActionCall {
            id: call_id,
            name,
            arguments,
        } = call;
        let parsed = ParsedAction::parse(&name, arguments)?;
        let before = self.capture_with_recovery(&mut state, context).await?;
        let action = parsed.into_wda_action(&before.viewport)?;
        let session = self.session_for_read(&mut state, context.control()).await?;
        let started_at_ms = now_ms();
        if let Err(error) = self
            .transport
            .perform(&session, action, context.control())
            .await
        {
            if is_explicit_session_loss(&error) {
                // The server definitively rejected the stale Session before
                // executing the command. Mark it recoverable for the next
                // read/lifecycle call, but never replay the mutation here.
                state.session = WdaSessionState::ReconnectPending;
            }
            return Err(error.into());
        }
        ensure_active(context.control())?;
        // Recovering this read-only post-action capture is safe. The
        // acknowledged mutation above is not sent a second time.
        let after = self.capture_with_recovery(&mut state, context).await?;
        ensure_active(context.control())?;
        let finished_at_ms = now_ms().max(started_at_ms);
        let evidence = deduplicated_screenshots(&before, &after);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output: json!({ "status": "ok" }),
            before: Some(before),
            after: Some(after),
            evidence,
            execution: None,
        })
    }
}

struct ScreenshotCapture {
    bytes: Vec<u8>,
    media_type: &'static str,
    width: u32,
    height: u32,
    source: &'static str,
}

pub(crate) enum ParsedAction {
    Tap {
        x: u32,
        y: u32,
    },
    InputText(String),
    KeyPress(IosKey),
    Swipe {
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
    },
    Scroll {
        delta_x: i64,
        delta_y: i64,
    },
}

impl ParsedAction {
    pub(crate) fn parse(name: &str, arguments: Value) -> DriverResult<Self> {
        let mut object = match arguments {
            Value::Object(object) => object,
            _ => return Err(invalid_arguments(name, "arguments must be an object")),
        };
        let parsed = match name {
            "tap" => Self::Tap {
                x: take_u32(name, &mut object, "x")?,
                y: take_u32(name, &mut object, "y")?,
            },
            "inputText" => {
                let text = take_string(name, &mut object, "text")?;
                if text.is_empty()
                    || text.len() > MAX_TEXT_BYTES
                    || text.chars().count() > MAX_TEXT_CHARS
                {
                    return Err(invalid_arguments(
                        name,
                        "text is outside the bounded input contract",
                    ));
                }
                Self::InputText(text)
            }
            "keyPress" => {
                let key = take_string(name, &mut object, "key")?;
                Self::KeyPress(IosKey::parse(&key).ok_or_else(|| {
                    invalid_arguments(name, "key is not in the advertised iOS key set")
                })?)
            }
            "swipe" => {
                let start_x = take_u32(name, &mut object, "startX")?;
                let start_y = take_u32(name, &mut object, "startY")?;
                let end_x = take_u32(name, &mut object, "endX")?;
                let end_y = take_u32(name, &mut object, "endY")?;
                let duration_ms = take_u32(name, &mut object, "durationMs")?;
                if duration_ms == 0 || duration_ms > MAX_SWIPE_DURATION_MS {
                    return Err(invalid_arguments(
                        name,
                        "durationMs is outside the advertised range",
                    ));
                }
                Self::Swipe {
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                    duration_ms,
                }
            }
            "scroll" => {
                let delta_x = take_i64(name, &mut object, "deltaX")?;
                let delta_y = take_i64(name, &mut object, "deltaY")?;
                if delta_x.abs() > MAX_SCROLL_DELTA
                    || delta_y.abs() > MAX_SCROLL_DELTA
                    || (delta_x == 0 && delta_y == 0)
                {
                    return Err(invalid_arguments(
                        name,
                        "scroll delta is outside the advertised range",
                    ));
                }
                Self::Scroll { delta_x, delta_y }
            }
            _ => return Err(DriverError::UnknownAction(name.to_owned())),
        };
        if !object.is_empty() {
            return Err(invalid_arguments(name, "arguments contain unknown fields"));
        }
        Ok(parsed)
    }

    pub(crate) fn into_wda_action(self, viewport: &Viewport) -> DriverResult<WdaAction> {
        match self {
            Self::Tap { x, y } => {
                validate_input_point("tap", x, y, viewport)?;
                Ok(WdaAction::Tap { x, y })
            }
            Self::InputText(text) => Ok(WdaAction::TypeText(text)),
            Self::KeyPress(key) => Ok(WdaAction::PressKey(key)),
            Self::Swipe {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => {
                validate_input_point("swipe", start_x, start_y, viewport)?;
                validate_input_point("swipe", end_x, end_y, viewport)?;
                Ok(WdaAction::Drag {
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                    duration_ms,
                })
            }
            Self::Scroll { delta_x, delta_y } => {
                let max_x = viewport.width.saturating_sub(1);
                let max_y = viewport.height.saturating_sub(1);
                let start_x = max_x / 2;
                let start_y = max_y / 2;
                let end_x = (i64::from(start_x) - delta_x).clamp(0, i64::from(max_x)) as u32;
                let end_y = (i64::from(start_y) - delta_y).clamp(0, i64::from(max_y)) as u32;
                Ok(WdaAction::Drag {
                    start_x,
                    start_y,
                    end_x,
                    end_y,
                    duration_ms: SCROLL_DURATION_MS,
                })
            }
        }
    }
}

fn validate_input_point(action: &str, x: u32, y: u32, viewport: &Viewport) -> DriverResult<()> {
    if x >= viewport.width || y >= viewport.height {
        Err(invalid_arguments(
            action,
            "coordinate is outside the current iOS viewport",
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn action_definitions() -> Vec<ActionDefinition> {
    const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
    let coordinate = || json!({ "type": "integer", "minimum": 0, "maximum": u32::MAX });
    vec![
        ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap one point in the current iOS screenshot coordinate space".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["x", "y"],
                "properties": { "x": coordinate(), "y": coordinate() }
            }),
        },
        ActionDefinition {
            name: "inputText".to_owned(),
            description: "Type bounded Unicode text through WebDriverAgent".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": {
                    "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS }
                }
            }),
        },
        ActionDefinition {
            name: "keyPress".to_owned(),
            description: "Press one key from DeviceRail's closed iOS key set".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["key"],
                "properties": { "key": { "type": "string", "enum": IosKey::VALUES } }
            }),
        },
        ActionDefinition {
            name: "swipe".to_owned(),
            description: "Drag between two iOS screenshot coordinates".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["startX", "startY", "endX", "endY", "durationMs"],
                "properties": {
                    "startX": coordinate(), "startY": coordinate(),
                    "endX": coordinate(), "endY": coordinate(),
                    "durationMs": { "type": "integer", "minimum": 1, "maximum": MAX_SWIPE_DURATION_MS }
                }
            }),
        },
        ActionDefinition {
            name: "scroll".to_owned(),
            description: "Scroll by bounded deltas using a viewport-relative WDA drag".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["deltaX", "deltaY"],
                "properties": {
                    "deltaX": { "type": "integer", "minimum": -MAX_SCROLL_DELTA, "maximum": MAX_SCROLL_DELTA },
                    "deltaY": { "type": "integer", "minimum": -MAX_SCROLL_DELTA, "maximum": MAX_SCROLL_DELTA }
                },
                "anyOf": [
                    { "properties": { "deltaX": { "not": { "const": 0 } } } },
                    { "properties": { "deltaY": { "not": { "const": 0 } } } }
                ]
            }),
        },
    ]
}

fn is_explicit_session_loss(error: &DriverError) -> bool {
    matches!(error, DriverError::Platform { code, .. } if code == "wda_invalid_session")
}

fn is_ambiguous_session_creation(error: &DriverError) -> bool {
    matches!(
        error,
        DriverError::Platform { code, .. }
            if matches!(
                code.as_str(),
                "wda_command_outcome_unknown"
                    | "wda_missing_session"
                    | "wda_invalid_session_response"
            )
    )
}

fn session_ownership_unknown() -> DriverError {
    platform("wda_session_ownership_unknown", false)
}

async fn lock_state<'a>(
    state: &'a Mutex<DriverState>,
    control: &ExecutionControl,
) -> DriverResult<tokio::sync::MutexGuard<'a, DriverState>> {
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

fn validate_page(page: &WdaPage) -> DriverResult<()> {
    if page.source.len() > MAX_SOURCE_BYTES
        || page.viewport.width == 0
        || page.viewport.height == 0
        || page.viewport.width > MAX_SCREENSHOT_DIMENSION
        || page.viewport.height > MAX_SCREENSHOT_DIMENSION
        || !page.viewport.scale_factor.is_finite()
        || page.viewport.scale_factor <= 0.0
        || page.viewport.scale_factor > MAX_SCREENSHOT_SCALE_FACTOR
    {
        return Err(platform("wda_invalid_page", false));
    }
    Ok(())
}

pub(crate) fn validate_png(bytes: &[u8]) -> DriverResult<(u32, u32)> {
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(platform("wda_invalid_screenshot", false));
    }
    let mut options = DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(Limits {
        bytes: MAX_SCREENSHOT_DECODED_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| platform("wda_invalid_screenshot", false))?;
    let info = reader.info();
    let (width, height) = (info.width, info.height);
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| platform("wda_invalid_screenshot", false))?;
    if width == 0
        || height == 0
        || width > MAX_SCREENSHOT_DIMENSION
        || height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
        || reader
            .output_buffer_size()
            .is_none_or(|size| size > MAX_SCREENSHOT_DECODED_BYTES)
    {
        return Err(platform("wda_invalid_screenshot", false));
    }
    while reader
        .next_row()
        .map_err(|_| platform("wda_invalid_screenshot", false))?
        .is_some()
    {}
    reader
        .finish()
        .map_err(|_| platform("wda_invalid_screenshot", false))?;
    Ok((width, height))
}

pub(crate) fn viewport_with_scale(
    viewport: &Viewport,
    image_width: u32,
    image_height: u32,
) -> DriverResult<Viewport> {
    let x_scale = f64::from(image_width) / f64::from(viewport.width);
    let y_scale = f64::from(image_height) / f64::from(viewport.height);
    let relative_scale_error = (x_scale - y_scale).abs() / x_scale.max(y_scale);
    if !x_scale.is_finite()
        || !y_scale.is_finite()
        || !relative_scale_error.is_finite()
        || x_scale <= 0.0
        || y_scale <= 0.0
        || x_scale > MAX_SCREENSHOT_SCALE_FACTOR
        || y_scale > MAX_SCREENSHOT_SCALE_FACTOR
        || relative_scale_error > MAX_SCREENSHOT_RELATIVE_SCALE_ERROR
    {
        return Err(platform("ios_screenshot_viewport_mismatch", false));
    }
    Ok(Viewport {
        width: viewport.width,
        height: viewport.height,
        scale_factor: x_scale,
    })
}

fn take_u32(
    action: &str,
    object: &mut Map<String, Value>,
    field: &'static str,
) -> DriverResult<u32> {
    object
        .remove(field)
        .as_ref()
        .and_then(json_integer_as_u32)
        .ok_or_else(|| {
            invalid_arguments(
                action,
                &format!("{field} must be an unsigned 32-bit integer"),
            )
        })
}

fn take_i64(
    action: &str,
    object: &mut Map<String, Value>,
    field: &'static str,
) -> DriverResult<i64> {
    object
        .remove(field)
        .as_ref()
        .and_then(json_integer_as_i32)
        .map(i64::from)
        .ok_or_else(|| invalid_arguments(action, &format!("{field} must be an integer")))
}

fn take_string(
    action: &str,
    object: &mut Map<String, Value>,
    field: &'static str,
) -> DriverResult<String> {
    object
        .remove(field)
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| invalid_arguments(action, &format!("{field} must be a string")))
}

fn invalid_arguments(action: &str, message: &str) -> DriverError {
    DriverError::InvalidArguments {
        action: action.to_owned(),
        message: message.to_owned(),
    }
}

pub(crate) fn deduplicated_screenshots(before: &Observation, after: &Observation) -> Vec<AssetRef> {
    let mut evidence = Vec::with_capacity(2);
    for asset in before.screenshot.iter().chain(after.screenshot.iter()) {
        if !evidence.contains(asset) {
            evidence.push(asset.clone());
        }
    }
    evidence
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use devicerail_core::DriverError;
    use devicerail_protocol::Viewport;
    use serde_json::{Value, json};

    use super::{IosDriver, ParsedAction, action_definitions, viewport_with_scale};
    use crate::{HttpEndpointConfig, IosDeviceConfig, SystemWdaTransport};

    #[test]
    fn driver_debug_does_not_expose_device_identity() {
        let device = IosDeviceConfig::new(
            "IOS-DRIVER-TOKEN-SENTINEL",
            "IOS-DRIVER-NAME-SENTINEL",
            Some("IOS-DRIVER-OS-SENTINEL".to_owned()),
        )
        .expect("valid device descriptor");
        let transport = Arc::new(SystemWdaTransport::new(
            HttpEndpointConfig::new("http://127.0.0.1:8100").expect("valid endpoint"),
        ));
        let driver = IosDriver::new(device, transport);
        let debug = format!("{driver:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("IOS-DRIVER-TOKEN-SENTINEL"));
        assert!(!debug.contains("IOS-DRIVER-NAME-SENTINEL"));
        assert!(!debug.contains("IOS-DRIVER-OS-SENTINEL"));
    }

    #[test]
    fn mathematical_integer_representations_match_action_schemas() {
        let cases = [
            (
                "tap",
                serde_json::from_str(r#"{"x":1.0,"y":2e0}"#).expect("tap JSON"),
                true,
            ),
            ("tap", json!({ "x": 1.5, "y": 2 }), false),
            (
                "tap",
                serde_json::from_str(r#"{"x":4294967296.0,"y":2}"#).expect("overflowing tap JSON"),
                false,
            ),
            (
                "swipe",
                serde_json::from_str(
                    r#"{"startX":0.0,"startY":1e0,"endX":2.0,"endY":3e0,"durationMs":1e2}"#,
                )
                .expect("swipe JSON"),
                true,
            ),
            (
                "swipe",
                json!({
                    "startX": 0,
                    "startY": 1,
                    "endX": 2,
                    "endY": 3,
                    "durationMs": 60_001.0
                }),
                false,
            ),
            (
                "scroll",
                serde_json::from_str(r#"{"deltaX":-1e0,"deltaY":2.0}"#).expect("scroll JSON"),
                true,
            ),
            (
                "scroll",
                json!({ "deltaX": -100_001.0, "deltaY": 2 }),
                false,
            ),
            ("scroll", json!({ "deltaX": 1.25, "deltaY": 2 }), false),
        ];

        let definitions = action_definitions();
        for (action, arguments, expected) in cases {
            let schema = &definitions
                .iter()
                .find(|definition| definition.name == action)
                .expect("action definition")
                .input_schema;
            let validator = jsonschema::validator_for(schema).expect("compile action schema");
            assert_eq!(
                validator.is_valid(&arguments),
                expected,
                "schema mismatch for {action}: {arguments}"
            );
            assert_eq!(
                ParsedAction::parse(action, arguments.clone()).is_ok(),
                expected,
                "parser mismatch for {action}: {arguments}"
            );
        }
    }

    #[test]
    fn viewport_conversion_rejects_out_of_bounds_tap_and_swipe() {
        let viewport = devicerail_protocol::Viewport {
            width: 10,
            height: 20,
            scale_factor: 1.0,
        };
        for (action, arguments) in [
            ("tap", json!({ "x": 10, "y": 0 })),
            (
                "swipe",
                json!({
                    "startX": 0,
                    "startY": 0,
                    "endX": 9,
                    "endY": 20,
                    "durationMs": 100
                }),
            ),
        ] {
            let parsed = ParsedAction::parse(action, arguments).expect("schema-valid action");
            assert!(parsed.into_wda_action(&viewport).is_err());
        }

        let valid =
            ParsedAction::parse("tap", json!({ "x": 9, "y": 19 })).expect("schema-valid tap");
        assert!(valid.into_wda_action(&viewport).is_ok());
    }

    #[test]
    fn viewport_geometry_accepts_retina_and_webkit_viewport_scaling() {
        let retina = Viewport {
            width: 393,
            height: 852,
            scale_factor: 1.0,
        };
        let normalized = viewport_with_scale(&retina, 1_178, 2_556)
            .expect("one-pixel Simulator rounding remains a uniform Retina scale");
        assert_eq!(normalized.width, 393);
        assert_eq!(normalized.height, 852);
        assert!((normalized.scale_factor - (1_178.0 / 393.0)).abs() < f64::EPSILON);

        // Safari without a meta viewport commonly exposes a 980 CSS-pixel
        // layout viewport. WebKit's viewport screenshot is rendered at the
        // page-to-device scale and excludes browser chrome.
        let css = Viewport {
            width: 980,
            height: 1_733,
            scale_factor: 1.0,
        };
        let normalized = viewport_with_scale(&css, 1_180, 2_085)
            .expect("WebKit viewport screenshot must normalize to CSS coordinates");
        assert_eq!(normalized.width, 980);
        assert_eq!(normalized.height, 1_733);
        assert!((normalized.scale_factor - (1_180.0 / 980.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn viewport_geometry_rejects_full_display_screenshot_for_css_viewport() {
        let css = Viewport {
            width: 980,
            height: 1_733,
            scale_factor: 1.0,
        };
        let error = viewport_with_scale(&css, 1_178, 2_556)
            .expect_err("Safari chrome makes a full-display screenshot non-affine to CSS bounds");
        assert!(matches!(
            error,
            DriverError::Platform { code, retryable: false }
                if code == "ios_screenshot_viewport_mismatch"
        ));
    }

    #[test]
    fn input_values_are_json_numbers() {
        assert!(ParsedAction::parse("tap", json!({ "x": "1", "y": 2 })).is_err());
        assert!(ParsedAction::parse("scroll", Value::Null).is_err());
    }
}
