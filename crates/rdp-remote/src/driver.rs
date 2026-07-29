use std::{future::pending, io::Cursor, sync::Arc};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms, run_bounded_blocking,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, Platform, ScreenshotOmissionReason, Viewport,
};
use png::{DecodeOptions, Decoder, Limits, Transformations};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::{sync::Mutex, time};
use uuid::Uuid;

use crate::bridge::{RdpBridge, RdpDesktop, RdpFrame, RdpInput};

const MAX_TEXT_CHARS: usize = 16 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_KEY_CHARS: usize = 128;
const MAX_KEY_BYTES: usize = 512;
const MAX_SCROLL_DELTA: i32 = 1_000_000;
const MAX_SCREENSHOT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 8_192;
const MAX_SCREENSHOT_PIXELS: u64 = 16_000_000;
const MAX_SCREENSHOT_DECODED_BYTES: usize = 64 * 1024 * 1024;
const MAX_METADATA_CHARS: usize = 512;

struct DriverState {
    connected: bool,
    desktop: Option<RdpDesktop>,
}

/// DeviceRail Driver for one stable desktop session owned by an external RDP bridge.
pub struct RdpDriver {
    id: DeviceId,
    name: String,
    target_fingerprint: String,
    bridge: Arc<dyn RdpBridge>,
    state: Mutex<DriverState>,
}

impl std::fmt::Debug for RdpDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RdpDriver")
            .field("id", &self.id)
            .field("target_fingerprint", &self.target_fingerprint)
            .finish_non_exhaustive()
    }
}

impl RdpDriver {
    pub fn new(name: impl Into<String>, bridge: Arc<dyn RdpBridge>) -> DriverResult<Self> {
        let name = name.into();
        let target_fingerprint = bridge.target_fingerprint();
        let valid_fingerprint = target_fingerprint.len() == 64
            && target_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        if name.trim().is_empty()
            || name.chars().count() > MAX_METADATA_CHARS
            || name.chars().any(char::is_control)
            || !valid_fingerprint
        {
            return Err(DriverError::Protocol(
                "invalid RDP driver identity".to_owned(),
            ));
        }
        let target_fingerprint = target_fingerprint.to_ascii_lowercase();
        Ok(Self {
            id: DeviceId::new(format!("rdp:{target_fingerprint}")),
            name,
            target_fingerprint,
            bridge,
            state: Mutex::new(DriverState {
                connected: false,
                desktop: None,
            }),
        })
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let state = self.state.lock().await;
        self.info(&state)
    }

    fn info(&self, state: &DriverState) -> DeviceInfo {
        DeviceInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            platform: Platform::Rdp,
            os_version: state
                .desktop
                .as_ref()
                .and_then(|desktop| desktop.server_version.clone()),
            connected: state.connected,
        }
    }

    async fn capture(
        &self,
        state: &mut DriverState,
        context: &DriverOperationContext,
        omission: Option<ScreenshotOmissionReason>,
        redact_metadata: bool,
    ) -> DeviceOperationResult<Observation> {
        let (desktop, screenshot_bytes) = if omission.is_some() {
            (
                self.bridge.probe(&self.id.0, context.control()).await?,
                None,
            )
        } else {
            let frame = self.bridge.capture(&self.id.0, context.control()).await?;
            let (frame, canonical_png) = run_bounded_blocking(
                context.control(),
                move || {
                    let canonical_png = canonicalize_frame(&frame)?;
                    Ok((frame, canonical_png))
                },
                || platform("invalid_screenshot", false),
            )
            .await?;
            (desktop_from_frame(&frame), Some(canonical_png))
        };
        validate_desktop(&desktop)?;
        state.desktop = Some(desktop.clone());
        let screenshot = match screenshot_bytes {
            Some(bytes) => {
                let size = bytes.len() as u64;
                let stored = context
                    .evidence()
                    .put_with_declared_size("image/png", size, Box::pin(Cursor::new(bytes)))
                    .await?;
                Some(stored.asset_ref())
            }
            None => None,
        };
        let mut metadata = Map::new();
        metadata.insert(
            "targetFingerprint".to_owned(),
            json!(self.target_fingerprint),
        );
        if !redact_metadata {
            if let Some(name) = desktop.desktop_name {
                metadata.insert("desktopName".to_owned(), json!(name));
            }
            if let Some(version) = desktop.server_version {
                metadata.insert("serverVersion".to_owned(), json!(version));
            }
        }
        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.id.clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: desktop.width,
                height: desktop.height,
                scale_factor: desktop.scale_factor,
            },
            screenshot,
            screenshot_omission: omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata,
        })
    }
}

#[async_trait]
impl DeviceDriver for RdpDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        match name {
            "inputSecret" => Some(ActionProtection::Protected),
            "tap" | "pointerMove" | "scroll" | "keyPress" | "typeText" => {
                Some(ActionProtection::Standard)
            }
            _ => None,
        }
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut state = lock_state(&self.state, control).await?;
        if state.connected {
            match self.bridge.probe(&self.id.0, control).await {
                Ok(desktop) => {
                    validate_desktop(&desktop)?;
                    state.desktop = Some(desktop);
                    return Ok(self.info(&state));
                }
                Err(error) => {
                    state.connected = false;
                    state.desktop = None;
                    return Err(error);
                }
            }
        }
        let desktop = self.bridge.connect(&self.id.0, control).await?;
        validate_desktop(&desktop)?;
        state.desktop = Some(desktop);
        state.connected = true;
        Ok(self.info(&state))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
        if !state.connected {
            return Ok(());
        }
        self.bridge.disconnect(&self.id.0, control).await?;
        state.connected = false;
        Ok(())
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(action_definitions())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        let desktop = self.bridge.health(&self.id.0, control).await?;
        validate_desktop(&desktop)
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let mut state = lock_state(&self.state, context.control()).await?;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        self.capture(&mut state, context, omission, false).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let mut state = lock_state(&self.state, context.control()).await?;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let ActionCall {
            id: call_id,
            name,
            arguments,
        } = call;
        let parsed = ParsedAction::parse(&name, arguments)?;
        let protected = parsed.is_protected();
        let omission = if protected {
            Some(ScreenshotOmissionReason::ProtectedAction)
        } else {
            match context.screenshot_policy() {
                ScreenshotPolicy::Capture => None,
                ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
            }
        };
        let before = self
            .capture(&mut state, context, omission, protected)
            .await?;
        let output = parsed.output();
        let input = parsed.into_input(&before.viewport)?;
        let started_at_ms = now_ms();
        self.bridge
            .input(&self.id.0, call_id, input, context.control())
            .await?;
        ensure_active(context.control())?;
        let after = self
            .capture(&mut state, context, omission, protected)
            .await?;
        ensure_active(context.control())?;
        let finished_at_ms = now_ms().max(started_at_ms);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output,
            before: Some(before.clone()),
            after: Some(after.clone()),
            evidence: deduplicated_screenshots(&before, &after),
            execution: None,
        })
    }
}

enum ParsedAction {
    Tap(PointArgs),
    PointerMove(PointArgs),
    Scroll(ScrollArgs),
    KeyPress(KeyArgs),
    TypeText(TextArgs),
    InputSecret(TextArgs),
}

impl ParsedAction {
    fn parse(name: &str, arguments: Value) -> Result<Self, DriverError> {
        match name {
            "tap" => parse_point(name, arguments).map(Self::Tap),
            "pointerMove" => parse_point(name, arguments).map(Self::PointerMove),
            "scroll" => parse_scroll(name, arguments).and_then(|value| {
                if value.delta_x < -MAX_SCROLL_DELTA
                    || value.delta_x > MAX_SCROLL_DELTA
                    || value.delta_y < -MAX_SCROLL_DELTA
                    || value.delta_y > MAX_SCROLL_DELTA
                {
                    Err(invalid(name, "scroll delta exceeds the safe bound"))
                } else if value.delta_x == 0 && value.delta_y == 0 {
                    Err(invalid(name, "scroll delta must not be zero"))
                } else {
                    Ok(Self::Scroll(value))
                }
            }),
            "keyPress" => decode(name, arguments).and_then(|value: KeyArgs| {
                validate_text(name, &value.key, MAX_KEY_CHARS, MAX_KEY_BYTES)?;
                Ok(Self::KeyPress(value))
            }),
            "typeText" => decode(name, arguments).and_then(|value: TextArgs| {
                validate_text(name, &value.text, MAX_TEXT_CHARS, MAX_TEXT_BYTES)?;
                Ok(Self::TypeText(value))
            }),
            "inputSecret" => decode(name, arguments).and_then(|value: TextArgs| {
                validate_text(name, &value.text, MAX_TEXT_CHARS, MAX_TEXT_BYTES)?;
                Ok(Self::InputSecret(value))
            }),
            _ => Err(DriverError::UnknownAction(name.to_owned())),
        }
    }

    fn is_protected(&self) -> bool {
        matches!(self, Self::InputSecret(_))
    }

    fn output(&self) -> Value {
        match self {
            Self::Tap(_) => json!({ "kind": "tap" }),
            Self::PointerMove(_) => json!({ "kind": "pointerMove" }),
            Self::Scroll(_) => json!({ "kind": "scroll" }),
            Self::KeyPress(_) => json!({ "kind": "keyPress" }),
            Self::TypeText(value) => {
                json!({ "kind": "typeText", "characterCount": value.text.chars().count() })
            }
            Self::InputSecret(_) => json!({ "kind": "inputSecret" }),
        }
    }

    fn into_input(self, viewport: &Viewport) -> Result<RdpInput, DriverError> {
        match self {
            Self::Tap(point) => {
                validate_point("tap", &point, viewport)?;
                Ok(RdpInput::Tap {
                    x: point.x,
                    y: point.y,
                })
            }
            Self::PointerMove(point) => {
                validate_point("pointerMove", &point, viewport)?;
                Ok(RdpInput::PointerMove {
                    x: point.x,
                    y: point.y,
                })
            }
            Self::Scroll(value) => Ok(RdpInput::Scroll {
                delta_x: value.delta_x,
                delta_y: value.delta_y,
            }),
            Self::KeyPress(value) => Ok(RdpInput::KeyPress { key: value.key }),
            Self::TypeText(value) => Ok(RdpInput::TypeText { text: value.text }),
            Self::InputSecret(value) => Ok(RdpInput::InputSecret { text: value.text }),
        }
    }
}

struct PointArgs {
    x: u32,
    y: u32,
}

struct ScrollArgs {
    delta_x: i32,
    delta_y: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KeyArgs {
    key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TextArgs {
    text: String,
}

fn decode<T>(action: &str, arguments: Value) -> Result<T, DriverError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).map_err(|_| invalid(action, "arguments do not match schema"))
}

fn parse_point(action: &str, arguments: Value) -> Result<PointArgs, DriverError> {
    let mut fields = exact_object(action, arguments, &["x", "y"])?;
    let x = value_as_u32(fields.remove("x").expect("required field"))
        .ok_or_else(|| invalid(action, "x must be a mathematical u32 integer"))?;
    let y = value_as_u32(fields.remove("y").expect("required field"))
        .ok_or_else(|| invalid(action, "y must be a mathematical u32 integer"))?;
    Ok(PointArgs { x, y })
}

fn parse_scroll(action: &str, arguments: Value) -> Result<ScrollArgs, DriverError> {
    let mut fields = exact_object(action, arguments, &["deltaX", "deltaY"])?;
    let delta_x = value_as_i32(fields.remove("deltaX").expect("required field"))
        .ok_or_else(|| invalid(action, "deltaX must be a mathematical i32 integer"))?;
    let delta_y = value_as_i32(fields.remove("deltaY").expect("required field"))
        .ok_or_else(|| invalid(action, "deltaY must be a mathematical i32 integer"))?;
    Ok(ScrollArgs { delta_x, delta_y })
}

fn exact_object(
    action: &str,
    arguments: Value,
    names: &[&str],
) -> Result<Map<String, Value>, DriverError> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| invalid(action, "arguments must be an object"))?;
    if fields.len() != names.len() || names.iter().any(|name| !fields.contains_key(*name)) {
        return Err(invalid(
            action,
            "arguments contain missing or unknown fields",
        ));
    }
    Ok(fields.clone())
}

fn value_as_u32(value: Value) -> Option<u32> {
    let number = value.as_number()?;
    if let Some(value) = number.as_u64() {
        return u32::try_from(value).ok();
    }
    let value = number.as_f64()?;
    (value.is_finite() && value.fract() == 0.0 && value >= 0.0 && value <= f64::from(u32::MAX))
        .then_some(value as u32)
}

fn value_as_i32(value: Value) -> Option<i32> {
    let number = value.as_number()?;
    if let Some(value) = number.as_i64() {
        return i32::try_from(value).ok();
    }
    if let Some(value) = number.as_u64() {
        return i32::try_from(value).ok();
    }
    let value = number.as_f64()?;
    (value.is_finite()
        && value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX))
    .then_some(value as i32)
}

fn invalid(action: &str, message: &str) -> DriverError {
    DriverError::InvalidArguments {
        action: action.to_owned(),
        message: message.to_owned(),
    }
}

fn validate_text(
    action: &str,
    value: &str,
    max_chars: usize,
    max_bytes: usize,
) -> DriverResult<()> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().count() > max_chars
        || value.contains('\0')
    {
        return Err(invalid(action, "text is empty or exceeds a safe bound"));
    }
    Ok(())
}

fn validate_point(action: &str, point: &PointArgs, viewport: &Viewport) -> DriverResult<()> {
    if point.x >= viewport.width || point.y >= viewport.height {
        return Err(invalid(action, "point is outside the current desktop"));
    }
    Ok(())
}

fn action_definitions() -> Vec<ActionDefinition> {
    const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
    let point = |name: &str, description: &str| ActionDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        protection: ActionProtection::Standard,
        input_schema: json!({
            "$schema": DIALECT,
            "type": "object",
            "additionalProperties": false,
            "required": ["x", "y"],
            "properties": {
                "x": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                "y": { "type": "integer", "minimum": 0, "maximum": u32::MAX }
            }
        }),
    };
    vec![
        point(
            "tap",
            "Click the primary pointer button at one remote desktop point",
        ),
        point("pointerMove", "Move the remote desktop pointer"),
        ActionDefinition {
            name: "scroll".to_owned(),
            description: "Scroll the remote desktop by bounded horizontal and vertical deltas"
                .to_owned(),
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
                "not": { "properties": { "deltaX": { "const": 0 }, "deltaY": { "const": 0 } }, "required": ["deltaX", "deltaY"] }
            }),
        },
        text_definition(
            "keyPress",
            "Press one named remote keyboard key",
            "key",
            MAX_KEY_CHARS,
            ActionProtection::Standard,
        ),
        text_definition(
            "typeText",
            "Type bounded text into the remote desktop",
            "text",
            MAX_TEXT_CHARS,
            ActionProtection::Standard,
        ),
        text_definition(
            "inputSecret",
            "Type protected text without durable screenshots or arguments",
            "text",
            MAX_TEXT_CHARS,
            ActionProtection::Protected,
        ),
    ]
}

fn text_definition(
    name: &str,
    description: &str,
    property: &str,
    max_length: usize,
    protection: ActionProtection,
) -> ActionDefinition {
    ActionDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        protection,
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "required": [property],
            "properties": {
                property: { "type": "string", "minLength": 1, "maxLength": max_length }
            }
        }),
    }
}

fn validate_desktop(desktop: &RdpDesktop) -> DriverResult<()> {
    let pixels = u64::from(desktop.width)
        .checked_mul(u64::from(desktop.height))
        .ok_or_else(|| platform("invalid_desktop", false))?;
    let metadata_valid = [&desktop.desktop_name, &desktop.server_version]
        .into_iter()
        .flatten()
        .all(|value| {
            !value.is_empty()
                && value.chars().count() <= MAX_METADATA_CHARS
                && !value.chars().any(char::is_control)
        });
    if desktop.width == 0
        || desktop.height == 0
        || desktop.width > MAX_SCREENSHOT_DIMENSION
        || desktop.height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
        || !desktop.scale_factor.is_finite()
        || desktop.scale_factor <= 0.0
        || desktop.scale_factor > 16.0
        || !metadata_valid
    {
        return Err(platform("invalid_desktop", false));
    }
    Ok(())
}

fn desktop_from_frame(frame: &RdpFrame) -> RdpDesktop {
    RdpDesktop {
        width: frame.width,
        height: frame.height,
        scale_factor: frame.scale_factor,
        desktop_name: frame.desktop_name.clone(),
        server_version: frame.server_version.clone(),
    }
}

fn canonicalize_frame(frame: &RdpFrame) -> DriverResult<Vec<u8>> {
    let desktop = desktop_from_frame(frame);
    validate_desktop(&desktop)?;
    if frame.png.is_empty() || frame.png.len() > MAX_SCREENSHOT_BYTES {
        return Err(platform("invalid_screenshot", false));
    }
    let mut options = DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = Decoder::new_with_options(Cursor::new(&frame.png), options);
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    decoder.set_limits(Limits {
        bytes: MAX_SCREENSHOT_DECODED_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| platform("invalid_screenshot", false))?;
    let info = reader.info();
    if info.width != frame.width
        || info.height != frame.height
        || info.animation_control.is_some()
        || reader
            .output_buffer_size()
            .is_none_or(|size| size > MAX_SCREENSHOT_DECODED_BYTES)
    {
        return Err(platform("invalid_screenshot", false));
    }
    let output_size = reader
        .output_buffer_size()
        .ok_or_else(|| platform("invalid_screenshot", false))?;
    let mut pixels = vec![0; output_size];
    let output = reader
        .next_frame(&mut pixels)
        .map_err(|_| platform("invalid_screenshot", false))?;
    if output.width != frame.width || output.height != frame.height {
        return Err(platform("invalid_screenshot", false));
    }
    pixels.truncate(output.buffer_size());
    reader
        .finish()
        .map_err(|_| platform("invalid_screenshot", false))?;
    let mut canonical = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut canonical, output.width, output.height);
        encoder.set_color(output.color_type);
        encoder.set_depth(output.bit_depth);
        let mut writer = encoder
            .write_header()
            .map_err(|_| platform("invalid_screenshot", false))?;
        writer
            .write_image_data(&pixels)
            .map_err(|_| platform("invalid_screenshot", false))?;
    }
    if canonical.len() > MAX_SCREENSHOT_BYTES {
        return Err(platform("invalid_screenshot", false));
    }
    Ok(canonical)
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
        guard = state.lock() => Ok(guard),
        _ = control.cancelled() => Err(DriverError::Cancelled),
        _ = deadline => Err(DriverError::TimedOut),
    }
}

fn deduplicated_screenshots(before: &Observation, after: &Observation) -> Vec<AssetRef> {
    let mut evidence = Vec::with_capacity(2);
    for asset in before.screenshot.iter().chain(after.screenshot.iter()) {
        if !evidence.contains(asset) {
            evidence.push(asset.clone());
        }
    }
    evidence
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

fn platform(code: &str, retryable: bool) -> DriverError {
    DriverError::Platform {
        code: code.to_owned(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use async_trait::async_trait;
    use devicerail_core::{
        DeviceDriver, DriverRegistry, EvidenceInput, EvidenceMetadata, EvidenceOutput,
        EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport, MemoryEventStore,
        PutEvidence, ReleaseReport, Sha256Digest, StoredEvidence,
    };
    use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
    use devicerail_protocol::{ActionCall, AssetRef, SessionId};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::{
        ParsedAction, RdpBridge, RdpDesktop, RdpDriver, RdpFrame, RdpInput, canonicalize_frame,
    };

    struct FakeBridge {
        connected: StdMutex<bool>,
        inputs: StdMutex<Vec<RdpInput>>,
        png: Vec<u8>,
    }

    impl FakeBridge {
        fn new() -> Self {
            Self {
                connected: StdMutex::new(false),
                inputs: StdMutex::new(Vec::new()),
                png: fixture_png(),
            }
        }

        fn desktop() -> RdpDesktop {
            RdpDesktop {
                width: 2,
                height: 2,
                scale_factor: 1.0,
                desktop_name: Some("Test Desktop".to_owned()),
                server_version: Some("test-rdp-1".to_owned()),
            }
        }

        fn ensure_connected(&self) -> devicerail_core::DriverResult<()> {
            if *self.connected.lock().expect("connected") {
                Ok(())
            } else {
                Err(devicerail_core::DriverError::Platform {
                    code: "fixture_disconnected".to_owned(),
                    retryable: true,
                })
            }
        }
    }

    #[async_trait]
    impl RdpBridge for FakeBridge {
        fn target_fingerprint(&self) -> String {
            "a".repeat(64)
        }

        async fn health(
            &self,
            _device_id: &str,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<RdpDesktop> {
            Ok(Self::desktop())
        }

        async fn connect(
            &self,
            _device_id: &str,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<RdpDesktop> {
            *self.connected.lock().expect("connected") = true;
            Ok(Self::desktop())
        }

        async fn disconnect(
            &self,
            _device_id: &str,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<()> {
            *self.connected.lock().expect("connected") = false;
            Ok(())
        }

        async fn probe(
            &self,
            _device_id: &str,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<RdpDesktop> {
            self.ensure_connected()?;
            Ok(Self::desktop())
        }

        async fn capture(
            &self,
            _device_id: &str,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<RdpFrame> {
            self.ensure_connected()?;
            let desktop = Self::desktop();
            Ok(RdpFrame {
                width: desktop.width,
                height: desktop.height,
                scale_factor: desktop.scale_factor,
                png: self.png.clone(),
                desktop_name: desktop.desktop_name,
                server_version: desktop.server_version,
            })
        }

        async fn input(
            &self,
            _device_id: &str,
            _call_id: Uuid,
            input: RdpInput,
            _control: &ExecutionControl,
        ) -> devicerail_core::DriverResult<()> {
            self.ensure_connected()?;
            self.inputs.lock().expect("inputs").push(input);
            Ok(())
        }
    }

    fn fixture_driver() -> RdpDriver {
        RdpDriver::new("RDP Fixture", Arc::new(FakeBridge::new())).expect("fixture driver")
    }

    fn conformance_call(
        definition: &devicerail_protocol::ActionDefinition,
    ) -> Result<ActionCall, String> {
        let arguments = match definition.name.as_str() {
            "tap" | "pointerMove" => json!({ "x": 1, "y": 1 }),
            "scroll" => json!({ "deltaX": 0, "deltaY": 1 }),
            "keyPress" => json!({ "key": "Enter" }),
            "typeText" => json!({ "text": "hello" }),
            "inputSecret" => json!({ "text": "secret" }),
            name => panic!("unexpected capability {name}"),
        };
        Ok(ActionCall {
            id: Uuid::new_v4(),
            name: definition.name.clone(),
            arguments,
        })
    }

    struct TemporaryEvidenceStore {
        inner: FileEvidenceStore,
        _root: TempDir,
    }

    impl TemporaryEvidenceStore {
        fn create() -> Arc<dyn EvidenceStore> {
            let root = tempfile::tempdir().expect("temporary Evidence Store root");
            let inner = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("temporary Evidence Store");
            Arc::new(Self { inner, _root: root })
        }
    }

    #[async_trait]
    impl EvidenceStore for TemporaryEvidenceStore {
        async fn put(
            &self,
            request: PutEvidence,
            input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            self.inner.put(request, input).await
        }
        async fn attach(
            &self,
            session_id: &SessionId,
            asset: &AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            self.inner.attach(session_id, asset).await
        }
        async fn verify_session_reference(
            &self,
            session_id: &SessionId,
            asset: &AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            self.inner.verify_session_reference(session_id, asset).await
        }
        async fn open(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            self.inner.open(digest).await
        }
        async fn metadata(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            self.inner.metadata(digest).await
        }
        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            self.inner.referenced_sessions().await
        }
        async fn release_session(
            &self,
            session_id: &SessionId,
            released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            self.inner.release_session(session_id, released_at_ms).await
        }
        async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
            self.inner.gc(policy).await
        }
    }

    devicerail_core::driver_conformance_test!(
        conforms_to_shared_driver_contract,
        fixture_driver,
        conformance_call,
        TemporaryEvidenceStore::create(),
    );

    #[test]
    fn invalid_identity_is_explicit() {
        assert!(RdpDriver::new("", Arc::new(FakeBridge::new())).is_err());
    }

    fn fixture_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer.write_image_data(&[0; 16]).expect("PNG image data");
        }
        bytes
    }

    #[test]
    fn input_debug_does_not_expose_typed_text() {
        let input = RdpInput::TypeText {
            text: "DO-NOT-LOG".to_owned(),
        };
        assert!(!format!("{input:?}").contains("DO-NOT-LOG"));
        let secret = RdpInput::InputSecret {
            text: "DO-NOT-LOG-OR-LENGTH".to_owned(),
        };
        let debug = format!("{secret:?}");
        assert!(!debug.contains("DO-NOT-LOG"));
        assert!(!debug.contains("20"));
    }

    #[test]
    fn action_call_debug_remains_redacted() {
        let call = ActionCall {
            id: Uuid::nil(),
            name: "inputSecret".to_owned(),
            arguments: Value::String("DO-NOT-LOG".to_owned()),
        };
        assert!(!format!("{call:?}").contains("DO-NOT-LOG"));
    }

    #[test]
    fn mathematical_integer_forms_match_the_advertised_schema() {
        assert!(ParsedAction::parse("tap", json!({ "x": 1.0, "y": 1e0 })).is_ok());
        assert!(ParsedAction::parse("scroll", json!({ "deltaX": -1.0, "deltaY": 1e0 })).is_ok());
        assert!(ParsedAction::parse("tap", json!({ "x": 1.5, "y": 1 })).is_err());
    }

    #[test]
    fn screenshot_evidence_is_canonical_and_debug_is_redacted() {
        const SECRET: &str = "RDP-PNG-ANCILLARY-SECRET";
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk("Comment".to_owned(), SECRET.to_owned())
                .expect("text chunk");
            let mut writer = encoder.write_header().expect("PNG header");
            writer.write_image_data(&[0; 16]).expect("PNG image data");
        }
        png.extend_from_slice(SECRET.as_bytes());
        assert!(
            png.windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes())
        );
        let frame = RdpFrame {
            width: 2,
            height: 2,
            scale_factor: 1.0,
            png,
            desktop_name: Some(SECRET.to_owned()),
            server_version: Some(SECRET.to_owned()),
        };
        let debug = format!("{frame:?}");
        assert!(!debug.contains(SECRET));
        let canonical = canonicalize_frame(&frame).expect("canonical PNG");
        assert!(
            !canonical
                .windows(SECRET.len())
                .any(|window| window == SECRET.as_bytes())
        );
    }

    #[test]
    fn bridge_identity_derives_the_stable_device_id() {
        let first = fixture_driver();
        let second = fixture_driver();
        assert_eq!(first.id, second.id);
        assert_eq!(first.id.0, format!("rdp:{}", "a".repeat(64)));
    }

    #[tokio::test]
    async fn registry_rejects_two_aliases_for_the_same_rdp_target() {
        let registry = DriverRegistry::new(Arc::new(MemoryEventStore::default()));
        let first = Arc::new(fixture_driver());
        registry
            .register(first.clone(), first.device_info().await)
            .await
            .expect("first target route");
        let second = Arc::new(fixture_driver());
        assert!(
            registry
                .register(second.clone(), second.device_info().await)
                .await
                .is_err()
        );
    }

    #[test]
    fn protected_input_keeps_a_distinct_wire_kind() {
        let value = serde_json::to_value(RdpInput::InputSecret {
            text: "secret".to_owned(),
        })
        .expect("serialize protected input");
        assert_eq!(value["kind"], "inputSecret");
    }

    #[tokio::test]
    async fn reconnect_probes_and_invalidates_a_lost_bridge_session() {
        let bridge = Arc::new(FakeBridge::new());
        let driver = RdpDriver::new("RDP Fixture", bridge.clone()).expect("driver");
        driver
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("first connect");
        *bridge.connected.lock().expect("connected") = false;
        assert!(
            driver
                .connect(&ExecutionControl::unbounded())
                .await
                .is_err()
        );
        assert!(!driver.device_info().await.connected);
    }
}
