use std::{io::Cursor, sync::Arc};

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
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

use crate::{
    DesktopAction, DesktopActionKind, DesktopBackend, DesktopCapture, DesktopError,
    DesktopIdentity, DesktopProbe, DesktopProfile, DesktopResult, MacOsPermission, PermissionState,
    model::{DesktopKey, validate_viewport},
};

const MAX_COORDINATE: u32 = 100_000;
const MAX_SCROLL_DELTA: i32 = 100_000;
const MAX_TEXT_CHARS: usize = 4_096;
const MAX_TEXT_BYTES: usize = 16 * 1_024;
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 16_384;
const MAX_SCREENSHOT_PIXELS: u64 = 64_000_000;
const MAX_SCREENSHOT_DECODED_BYTES: usize = 256 * 1024 * 1024;

struct DriverState {
    connected: bool,
    viewport: Option<Viewport>,
}

struct DriverEngine {
    identity: DesktopIdentity,
    profile: DesktopProfile,
    backend: Arc<dyn DesktopBackend>,
    state: Mutex<DriverState>,
}

impl std::fmt::Debug for DriverEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DriverEngine")
            .field("identity", &self.identity)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl DriverEngine {
    fn new(
        identity: DesktopIdentity,
        expected_platform: Platform,
        backend: Arc<dyn DesktopBackend>,
    ) -> DesktopResult<Self> {
        identity.validate()?;
        let profile = backend.profile().clone();
        profile.validate()?;
        if profile.platform() != &expected_platform {
            return Err(DesktopError::InvalidProfile(format!(
                "backend platform {:?} does not match driver platform {expected_platform:?}",
                profile.platform()
            )));
        }
        Ok(Self {
            identity,
            profile,
            backend,
            state: Mutex::new(DriverState {
                connected: false,
                viewport: None,
            }),
        })
    }

    fn id(&self) -> &DeviceId {
        &self.identity.id
    }

    fn info(&self, connected: bool) -> DeviceInfo {
        DeviceInfo {
            id: self.identity.id.clone(),
            name: self.identity.name.clone(),
            platform: self.profile.platform().clone(),
            os_version: self.identity.os_version.clone(),
            connected,
        }
    }

    async fn device_info(&self) -> DeviceInfo {
        let state = self.state.lock().await;
        self.info(state.connected)
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        DesktopActionKind::parse(name)
            .filter(|action| self.profile.supports(*action))
            .map(|_| ActionProtection::Standard)
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut state = lock_state(&self.state, control).await?;
        if state.connected {
            return Ok(self.info(true));
        }
        let probe = self
            .backend
            .probe(control)
            .await
            .map_err(map_desktop_error)?;
        self.validate_probe(&probe).map_err(map_desktop_error)?;
        state.viewport = Some(probe.viewport);
        state.connected = true;
        Ok(self.info(true))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
        state.connected = false;
        state.viewport = None;
        Ok(())
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(self.profile.actions().map(action_definition).collect())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        let probe = self
            .backend
            .probe(control)
            .await
            .map_err(map_desktop_error)?;
        self.validate_probe(&probe).map_err(map_desktop_error)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let mut state = lock_state(&self.state, context.control()).await?;
        self.ensure_connected(&state)?;
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        self.capture_locked(&mut state, context, omission).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let mut state = lock_state(&self.state, context.control()).await?;
        self.ensure_connected(&state)?;

        let kind = DesktopActionKind::parse(&call.name)
            .filter(|kind| self.profile.supports(*kind))
            .ok_or_else(|| DriverError::UnknownAction(call.name.clone()))?;
        let action = parse_action(kind, &call.name, call.arguments)?;
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        let before = self.capture_locked(&mut state, context, omission).await?;
        validate_action_viewport(&action, &call.name, Some(&before.viewport))?;
        let started_at_ms = now_ms();
        self.backend
            .execute(action, context.control())
            .await
            .map_err(map_desktop_error)?;
        ensure_active(context.control())?;
        let after = self.capture_locked(&mut state, context, omission).await?;
        ensure_active(context.control())?;
        let finished_at_ms = now_ms().max(started_at_ms);
        let evidence = deduplicated_screenshots(&before, &after);

        Ok(ActionResult {
            call_id: call.id,
            started_at_ms,
            finished_at_ms,
            output: json!({
                "accepted": true,
                "action": call.name,
                "platform": platform_slug(self.profile.platform()),
            }),
            before: Some(before),
            after: Some(after),
            evidence,
            execution: None,
        })
    }

    async fn capture_locked(
        &self,
        state: &mut DriverState,
        context: &DriverOperationContext,
        omission: Option<ScreenshotOmissionReason>,
    ) -> DeviceOperationResult<Observation> {
        let (viewport, screenshot, backend_metadata, live_profile) = if omission.is_some() {
            let probe = self
                .backend
                .probe(context.control())
                .await
                .map_err(map_desktop_error)?;
            self.validate_probe(&probe).map_err(map_desktop_error)?;
            (probe.viewport, None, Map::new(), probe.profile)
        } else {
            let capture = self
                .backend
                .capture(context.control())
                .await
                .map_err(map_desktop_error)?;
            let (capture, dimensions) = run_bounded_blocking(
                context.control(),
                move || {
                    let dimensions = validate_png(&capture.png).map_err(map_desktop_error)?;
                    Ok((capture, dimensions))
                },
                || {
                    map_desktop_error(DesktopError::MalformedPng(
                        "PNG validation task failed".to_owned(),
                    ))
                },
            )
            .await?;
            validate_viewport(&capture.viewport).map_err(map_desktop_error)?;
            if dimensions.0 != capture.viewport.width || dimensions.1 != capture.viewport.height {
                return Err(map_desktop_error(DesktopError::MalformedPng(
                    "PNG dimensions do not match the backend viewport".to_owned(),
                ))
                .into());
            }
            validate_backend_metadata(&capture).map_err(map_desktop_error)?;
            let screenshot = persist_screenshot(context, capture.png).await?;
            (
                capture.viewport,
                Some(screenshot),
                capture.metadata,
                self.profile.clone(),
            )
        };
        state.viewport = Some(viewport.clone());

        let mut metadata = backend_metadata;
        metadata.insert(
            "desktopPlatform".to_owned(),
            json!(platform_slug(self.profile.platform())),
        );
        if let Some(display_server) = self.profile.linux_display_server() {
            metadata.insert(
                "linuxDisplayServer".to_owned(),
                json!(display_server.as_str()),
            );
        }
        if let Some(input_backend) = self.profile.wayland_input_backend() {
            metadata.insert(
                "waylandInputBackend".to_owned(),
                json!(match input_backend {
                    crate::WaylandInputBackend::Ydotool => "ydotool",
                    crate::WaylandInputBackend::Wtype => "wtype",
                }),
            );
        }
        if let Some(permissions) = live_profile.macos_permissions() {
            metadata.insert(
                "macOsPermissions".to_owned(),
                json!({
                    "screenRecording": permissions.screen_recording.as_str(),
                    "accessibility": permissions.accessibility.as_str(),
                }),
            );
        }

        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.identity.id.clone(),
            captured_at_ms: now_ms(),
            viewport,
            screenshot,
            screenshot_omission: omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata,
        })
    }

    fn validate_probe(&self, probe: &DesktopProbe) -> DesktopResult<()> {
        probe.profile.validate()?;
        validate_viewport(&probe.viewport)?;
        if !self.profile.same_contract(&probe.profile) {
            return Err(DesktopError::BackendContractChanged);
        }
        if let Some(permissions) = probe.profile.macos_permissions() {
            if permissions.screen_recording != PermissionState::Granted {
                return Err(DesktopError::MacOsPermissionRequired {
                    permission: MacOsPermission::ScreenRecording,
                    state: permissions.screen_recording,
                });
            }
            if permissions.accessibility != PermissionState::Granted {
                return Err(DesktopError::MacOsPermissionRequired {
                    permission: MacOsPermission::Accessibility,
                    state: permissions.accessibility,
                });
            }
        }
        Ok(())
    }

    fn ensure_connected(&self, state: &DriverState) -> DriverResult<()> {
        if state.connected {
            Ok(())
        } else {
            Err(DriverError::NotConnected(self.identity.id.clone()))
        }
    }
}

macro_rules! desktop_driver {
    ($name:ident, $platform:expr) => {
        #[derive(Debug)]
        pub struct $name {
            inner: DriverEngine,
        }

        impl $name {
            pub fn new(
                identity: DesktopIdentity,
                backend: Arc<dyn DesktopBackend>,
            ) -> DesktopResult<Self> {
                Ok(Self {
                    inner: DriverEngine::new(identity, $platform, backend)?,
                })
            }

            pub async fn device_info(&self) -> DeviceInfo {
                self.inner.device_info().await
            }

            pub fn profile(&self) -> &DesktopProfile {
                &self.inner.profile
            }
        }

        #[async_trait]
        impl DeviceDriver for $name {
            fn id(&self) -> &DeviceId {
                self.inner.id()
            }

            fn action_protection(&self, name: &str) -> Option<ActionProtection> {
                self.inner.action_protection(name)
            }

            async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
                self.inner.connect(control).await
            }

            async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
                self.inner.disconnect(control).await
            }

            async fn capabilities(
                &self,
                control: &ExecutionControl,
            ) -> DriverResult<Vec<ActionDefinition>> {
                self.inner.capabilities(control).await
            }

            async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
                self.inner.health_check(control).await
            }

            async fn observe(
                &self,
                context: &DriverOperationContext,
            ) -> DeviceOperationResult<Observation> {
                self.inner.observe(context).await
            }

            async fn execute(
                &self,
                context: &DriverOperationContext,
                call: ActionCall,
            ) -> DeviceOperationResult<ActionResult> {
                self.inner.execute(context, call).await
            }
        }
    };
}

desktop_driver!(MacOsDriver, Platform::MacOs);
desktop_driver!(WindowsDriver, Platform::Windows);
desktop_driver!(LinuxDriver, Platform::Linux);

async fn lock_state<'a>(
    state: &'a Mutex<DriverState>,
    control: &ExecutionControl,
) -> DriverResult<MutexGuard<'a, DriverState>> {
    ensure_active(control)?;
    tokio::select! {
        biased;
        _ = control.cancelled() => Err(DriverError::Cancelled),
        guard = state.lock() => {
            ensure_active(control)?;
            Ok(guard)
        }
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

fn map_desktop_error(error: DesktopError) -> DriverError {
    match error {
        DesktopError::Cancelled => DriverError::Cancelled,
        DesktopError::TimedOut => DriverError::TimedOut,
        error => DriverError::Platform {
            code: error.code().to_owned(),
            retryable: error.retryable(),
        },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TapArgs {
    x: u32,
    y: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputTextArgs {
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KeyPressArgs {
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScrollArgs {
    delta_x: i32,
    delta_y: i32,
}

fn parse_action(
    kind: DesktopActionKind,
    name: &str,
    arguments: Value,
) -> DriverResult<DesktopAction> {
    let invalid = || DriverError::InvalidArguments {
        action: name.to_owned(),
        message: "arguments do not match the advertised desktop action schema".to_owned(),
    };
    match kind {
        DesktopActionKind::Tap => {
            let args = parse_tap_args(name, arguments)?;
            if args.x > MAX_COORDINATE || args.y > MAX_COORDINATE {
                return Err(invalid());
            }
            Ok(DesktopAction::Tap {
                x: args.x,
                y: args.y,
            })
        }
        DesktopActionKind::InputText => {
            let args: InputTextArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
            let characters = args.text.chars().count();
            if characters == 0
                || characters > MAX_TEXT_CHARS
                || args.text.len() > MAX_TEXT_BYTES
                || args.text.chars().any(|character| character == '\0')
            {
                return Err(invalid());
            }
            Ok(DesktopAction::InputText(args.text))
        }
        DesktopActionKind::KeyPress => {
            let args: KeyPressArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
            DesktopKey::parse(&args.key)
                .map(DesktopAction::KeyPress)
                .ok_or_else(invalid)
        }
        DesktopActionKind::Scroll => {
            let args = parse_scroll_args(name, arguments)?;
            if args.delta_x.unsigned_abs() > MAX_SCROLL_DELTA as u32
                || args.delta_y.unsigned_abs() > MAX_SCROLL_DELTA as u32
            {
                return Err(invalid());
            }
            Ok(DesktopAction::Scroll {
                delta_x: args.delta_x,
                delta_y: args.delta_y,
            })
        }
    }
}

fn parse_tap_args(action: &str, arguments: Value) -> DriverResult<TapArgs> {
    let mut fields = exact_action_object(action, arguments, &["x", "y"])?;
    Ok(TapArgs {
        x: take_mathematical_u32(action, &mut fields, "x")?,
        y: take_mathematical_u32(action, &mut fields, "y")?,
    })
}

fn parse_scroll_args(action: &str, arguments: Value) -> DriverResult<ScrollArgs> {
    let mut fields = exact_action_object(action, arguments, &["deltaX", "deltaY"])?;
    Ok(ScrollArgs {
        delta_x: take_mathematical_i32(action, &mut fields, "deltaX")?,
        delta_y: take_mathematical_i32(action, &mut fields, "deltaY")?,
    })
}

fn exact_action_object(
    action: &str,
    arguments: Value,
    required: &[&str],
) -> DriverResult<Map<String, Value>> {
    arguments
        .as_object()
        .filter(|fields| {
            fields.len() == required.len()
                && required.iter().all(|field| fields.contains_key(*field))
        })
        .cloned()
        .ok_or_else(|| invalid_action_arguments(action))
}

fn take_mathematical_u32(
    action: &str,
    fields: &mut Map<String, Value>,
    field: &str,
) -> DriverResult<u32> {
    fields
        .remove(field)
        .as_ref()
        .and_then(json_integer_as_u32)
        .ok_or_else(|| invalid_action_arguments(action))
}

fn take_mathematical_i32(
    action: &str,
    fields: &mut Map<String, Value>,
    field: &str,
) -> DriverResult<i32> {
    fields
        .remove(field)
        .as_ref()
        .and_then(json_integer_as_i32)
        .ok_or_else(|| invalid_action_arguments(action))
}

fn invalid_action_arguments(action: &str) -> DriverError {
    DriverError::InvalidArguments {
        action: action.to_owned(),
        message: "arguments do not match the advertised desktop action schema".to_owned(),
    }
}

fn validate_action_viewport(
    action: &DesktopAction,
    name: &str,
    viewport: Option<&Viewport>,
) -> DriverResult<()> {
    if let (DesktopAction::Tap { x, y }, Some(viewport)) = (action, viewport)
        && (*x >= viewport.width || *y >= viewport.height)
    {
        return Err(DriverError::InvalidArguments {
            action: name.to_owned(),
            message: "tap coordinate is outside the current desktop viewport".to_owned(),
        });
    }
    Ok(())
}

fn action_definition(kind: DesktopActionKind) -> ActionDefinition {
    let (description, input_schema) = match kind {
        DesktopActionKind::Tap => (
            "Click the primary pointer button at a desktop pixel coordinate.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["x", "y"],
                "properties": {
                    "x": { "type": "integer", "minimum": 0, "maximum": MAX_COORDINATE },
                    "y": { "type": "integer", "minimum": 0, "maximum": MAX_COORDINATE }
                }
            }),
        ),
        DesktopActionKind::InputText => (
            "Type bounded Unicode text into the focused desktop control.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": {
                    "text": { "type": "string", "minLength": 1, "maxLength": MAX_TEXT_CHARS }
                }
            }),
        ),
        DesktopActionKind::KeyPress => (
            "Press one allowlisted desktop navigation key.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["key"],
                "properties": {
                    "key": { "type": "string", "enum": DesktopKey::VALUES }
                }
            }),
        ),
        DesktopActionKind::Scroll => (
            "Send bounded horizontal and vertical desktop scroll deltas.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["deltaX", "deltaY"],
                "properties": {
                    "deltaX": {
                        "type": "integer",
                        "minimum": -MAX_SCROLL_DELTA,
                        "maximum": MAX_SCROLL_DELTA
                    },
                    "deltaY": {
                        "type": "integer",
                        "minimum": -MAX_SCROLL_DELTA,
                        "maximum": MAX_SCROLL_DELTA
                    }
                }
            }),
        ),
    };
    ActionDefinition {
        name: kind.as_str().to_owned(),
        description: description.to_owned(),
        input_schema,
        protection: ActionProtection::Standard,
    }
}

async fn persist_screenshot(
    context: &DriverOperationContext,
    png: Vec<u8>,
) -> DeviceOperationResult<AssetRef> {
    let size = png.len() as u64;
    let stored = context
        .evidence()
        .put_with_declared_size("image/png", size, Box::pin(Cursor::new(png)))
        .await?;
    Ok(stored.asset_ref())
}

fn validate_backend_metadata(capture: &DesktopCapture) -> DesktopResult<()> {
    for reserved in [
        "desktopPlatform",
        "linuxDisplayServer",
        "waylandInputBackend",
        "macOsPermissions",
    ] {
        if capture.metadata.contains_key(reserved) {
            return Err(DesktopError::BackendContractChanged);
        }
    }
    Ok(())
}

fn validate_png(bytes: &[u8]) -> DesktopResult<(u32, u32)> {
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(DesktopError::ScreenshotTooLarge);
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
        .map_err(|error| DesktopError::MalformedPng(error.to_string()))?;
    let info = reader.info();
    let dimensions = (info.width, info.height);
    let pixels = u64::from(info.width)
        .checked_mul(u64::from(info.height))
        .ok_or(DesktopError::ScreenshotTooLarge)?;
    if info.width == 0
        || info.height == 0
        || info.width > MAX_SCREENSHOT_DIMENSION
        || info.height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
        || reader
            .output_buffer_size()
            .is_none_or(|size| size > MAX_SCREENSHOT_DECODED_BYTES)
    {
        return Err(DesktopError::ScreenshotTooLarge);
    }
    while reader
        .next_row()
        .map_err(|error| DesktopError::MalformedPng(error.to_string()))?
        .is_some()
    {}
    reader
        .finish()
        .map_err(|error| DesktopError::MalformedPng(error.to_string()))?;
    Ok(dimensions)
}

fn deduplicated_screenshots(before: &Observation, after: &Observation) -> Vec<AssetRef> {
    let mut evidence = Vec::with_capacity(2);
    for screenshot in [&before.screenshot, &after.screenshot]
        .into_iter()
        .flatten()
    {
        if !evidence.contains(screenshot) {
            evidence.push(screenshot.clone());
        }
    }
    evidence
}

fn platform_slug(platform: &Platform) -> &'static str {
    match platform {
        Platform::MacOs => "macOs",
        Platform::Windows => "windows",
        Platform::Linux => "linux",
        _ => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DesktopActionKind, action_definition, parse_action};

    #[test]
    fn mathematical_integer_representations_match_action_schemas() {
        let cases = [
            (
                DesktopActionKind::Tap,
                serde_json::from_str(r#"{"x":1.0,"y":2e0}"#).expect("tap JSON"),
                true,
            ),
            (DesktopActionKind::Tap, json!({ "x": 1.5, "y": 2 }), false),
            (
                DesktopActionKind::Tap,
                json!({ "x": 100_001.0, "y": 2 }),
                false,
            ),
            (
                DesktopActionKind::Scroll,
                serde_json::from_str(r#"{"deltaX":-1e0,"deltaY":2.0}"#).expect("scroll JSON"),
                true,
            ),
            (
                DesktopActionKind::Scroll,
                json!({ "deltaX": -100_001.0, "deltaY": 2 }),
                false,
            ),
            (
                DesktopActionKind::Scroll,
                json!({ "deltaX": 1.5, "deltaY": 2 }),
                false,
            ),
        ];

        for (kind, arguments, expected) in cases {
            let definition = action_definition(kind);
            let validator = jsonschema::validator_for(&definition.input_schema)
                .expect("compile desktop action schema");
            assert_eq!(
                validator.is_valid(&arguments),
                expected,
                "schema mismatch for {}: {arguments}",
                kind.as_str()
            );
            assert_eq!(
                parse_action(kind, kind.as_str(), arguments.clone()).is_ok(),
                expected,
                "parser mismatch for {}: {arguments}",
                kind.as_str()
            );
        }
    }
}
