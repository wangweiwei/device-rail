use std::{future::pending, io::Cursor, sync::Arc};

use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationResult, DriverError, DriverOperationContext, DriverResult,
    ExecutionControl, ScreenshotPolicy, now_ms, run_bounded_blocking,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation, ScreenshotOmissionReason, Viewport, json_integer_as_u32,
};
use png::{DecodeOptions, Decoder, Limits};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::{sync::Mutex, time};
use uuid::Uuid;

use crate::{
    DiscoveredHarmonyDevice, HarmonyAbilityName, HarmonyBundleName, HarmonyHdcError,
    HarmonyHdcResult, HarmonyKey, HdcCommand, HdcCommandOutput, HdcCommandRunner, HdcInputText,
    HdcOperation, HdcProperty,
};

const MAX_COORDINATE: u32 = 1_000_000;
const MAX_SWIPE_DURATION_MS: u32 = 60_000;
const MIN_SWIPE_VELOCITY_PPS: u32 = 200;
const MAX_SWIPE_VELOCITY_PPS: u32 = 40_000;
const MAX_LAYOUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_HIERARCHY_OBJECTS: usize = 100_000;
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 16_384;
const MAX_SCREENSHOT_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_DECODED_SCREENSHOT_BYTES: usize = 256 * 1024 * 1024;
const MAX_PROPERTY_BYTES: usize = 4 * 1024;

struct DriverState {
    descriptor: DiscoveredHarmonyDevice,
    connected: bool,
}

pub struct HarmonyHdcDriver {
    id: DeviceId,
    target: crate::HdcTarget,
    runner: Arc<dyn HdcCommandRunner>,
    state: Mutex<DriverState>,
}

impl std::fmt::Debug for HarmonyHdcDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HarmonyHdcDriver")
            .field("id", &self.id)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl HarmonyHdcDriver {
    pub(crate) fn new(
        descriptor: DiscoveredHarmonyDevice,
        runner: Arc<dyn HdcCommandRunner>,
    ) -> Self {
        Self {
            id: descriptor.target.device_id(),
            target: descriptor.target.clone(),
            runner,
            state: Mutex::new(DriverState {
                descriptor,
                connected: false,
            }),
        }
    }

    pub fn id(&self) -> &DeviceId {
        &self.id
    }

    pub async fn device_info(&self) -> DeviceInfo {
        let state = self.state.lock().await;
        state.descriptor.device_info(state.connected)
    }

    async fn run(
        &self,
        operation: HdcOperation,
        control: &ExecutionControl,
    ) -> HarmonyHdcResult<HdcCommandOutput> {
        let command = HdcCommand::for_target(self.target.clone(), operation)?;
        self.runner.run(command, control).await
    }

    async fn capture(
        &self,
        context: &DriverOperationContext,
        omission: Option<ScreenshotOmissionReason>,
    ) -> DeviceOperationResult<Observation> {
        let layout_output = self
            .run(HdcOperation::DumpLayout, context.control())
            .await
            .map_err(|error| error.into_driver_error())?;
        require_empty_stderr(&layout_output, "dump_layout")
            .map_err(|error| error.into_driver_error())?;
        let layout =
            parse_layout(layout_output.stdout()).map_err(|error| error.into_driver_error())?;

        let (screenshot, viewport_width, viewport_height) = if omission.is_none() {
            let screenshot_output = self
                .run(HdcOperation::CaptureScreenshot, context.control())
                .await
                .map_err(|error| error.into_driver_error())?;
            require_empty_stderr(&screenshot_output, "capture_screenshot")
                .map_err(|error| error.into_driver_error())?;
            let bytes = screenshot_output.stdout().to_vec();
            let (bytes, dimensions) = run_bounded_blocking(
                context.control(),
                move || {
                    let dimensions =
                        validate_png(&bytes).map_err(|error| error.into_driver_error())?;
                    Ok((bytes, dimensions))
                },
                || {
                    HarmonyHdcError::InvalidOutput {
                        operation: "capture_screenshot",
                    }
                    .into_driver_error()
                },
            )
            .await?;
            let stored = context
                .evidence()
                .put_with_declared_size(
                    "image/png",
                    bytes.len() as u64,
                    Box::pin(Cursor::new(bytes)),
                )
                .await?;
            (
                Some(stored.asset_ref()),
                dimensions.width,
                dimensions.height,
            )
        } else {
            (None, layout.width, layout.height)
        };

        let mut metadata = Map::new();
        metadata.insert("transport".to_owned(), json!("hdc"));
        metadata.insert("target".to_owned(), json!(self.target.as_str()));
        metadata.insert("hierarchyNodeCount".to_owned(), json!(layout.object_count));
        metadata.insert(
            "layoutViewport".to_owned(),
            json!({ "width": layout.width, "height": layout.height }),
        );
        metadata.insert("hierarchy".to_owned(), layout.value);

        Ok(Observation {
            id: Uuid::new_v4(),
            device_id: self.id.clone(),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: viewport_width,
                height: viewport_height,
                scale_factor: 1.0,
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
impl DeviceDriver for HarmonyHdcDriver {
    fn id(&self) -> &DeviceId {
        &self.id
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        let mut state = lock_state(&self.state, control).await?;
        if state.connected {
            return Ok(state.descriptor.device_info(true));
        }
        if !state.descriptor.state.is_ready() {
            return Err(HarmonyHdcError::TargetUnavailable {
                state: state.descriptor.state.as_str().to_owned(),
            }
            .into_driver_error());
        }

        let probe = self
            .run(HdcOperation::Probe, control)
            .await
            .map_err(HarmonyHdcError::into_driver_error)?;
        require_empty_stderr(&probe, "probe").map_err(HarmonyHdcError::into_driver_error)?;
        if probe
            .stdout_text("probe")
            .map_err(HarmonyHdcError::into_driver_error)?
            .trim()
            != "devicerail"
        {
            return Err(HarmonyHdcError::InvalidOutput { operation: "probe" }.into_driver_error());
        }

        let model = read_property(self, HdcProperty::ProductModel, control)
            .await
            .map_err(HarmonyHdcError::into_driver_error)?;
        let version = read_property(self, HdcProperty::SoftwareVersion, control)
            .await
            .map_err(HarmonyHdcError::into_driver_error)?;
        state.descriptor.name = Some(model);
        state.descriptor.os_version = Some(version);
        state.connected = true;
        Ok(state.descriptor.device_info(true))
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        let mut state = lock_state(&self.state, control).await?;
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
        let state = lock_state(&self.state, control).await?;
        if !state.descriptor.state.is_ready() {
            return Err(HarmonyHdcError::TargetUnavailable {
                state: state.descriptor.state.as_str().to_owned(),
            }
            .into_driver_error());
        }
        let probe = self
            .run(HdcOperation::Probe, control)
            .await
            .map_err(HarmonyHdcError::into_driver_error)?;
        require_empty_stderr(&probe, "probe").map_err(HarmonyHdcError::into_driver_error)?;
        if probe
            .stdout_text("probe")
            .map_err(HarmonyHdcError::into_driver_error)?
            .trim()
            == "devicerail"
        {
            Ok(())
        } else {
            Err(HarmonyHdcError::InvalidOutput { operation: "probe" }.into_driver_error())
        }
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        match name {
            "tap" | "swipe" | "inputText" | "keyPress" | "launch" => {
                Some(ActionProtection::Standard)
            }
            _ => None,
        }
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        let state = lock_state(&self.state, context.control()).await?;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        self.capture(context, omission).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let state = lock_state(&self.state, context.control()).await?;
        if !state.connected {
            return Err(DriverError::NotConnected(self.id.clone()).into());
        }
        let ActionCall {
            id: call_id,
            name,
            arguments,
        } = call;
        let action = ParsedAction::parse(&name, arguments)?;
        let omission = match context.screenshot_policy() {
            ScreenshotPolicy::Capture => None,
            ScreenshotPolicy::Omit => Some(ScreenshotOmissionReason::Policy),
        };
        let before = self.capture(context, omission).await?;
        let started_at_ms = now_ms();
        let operation = action.into_operation(&before.viewport)?;
        let output = self
            .run(operation, context.control())
            .await
            .map_err(|error| error.into_driver_error())?;
        validate_action_ack(&output, if name == "launch" { "launch" } else { "action" })
            .map_err(|error| error.into_driver_error())?;
        let after = self.capture(context, omission).await?;
        ensure_active(context.control())?;
        let finished_at_ms = now_ms().max(started_at_ms);
        let evidence = deduplicated_screenshots(&before, &after);
        drop(state);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output: json!({ "accepted": true, "action": name }),
            before: Some(before),
            after: Some(after),
            evidence,
            execution: None,
        })
    }
}

async fn read_property(
    driver: &HarmonyHdcDriver,
    property: HdcProperty,
    control: &ExecutionControl,
) -> HarmonyHdcResult<String> {
    let output = driver
        .run(HdcOperation::GetProperty(property), control)
        .await?;
    require_empty_stderr(&output, "get_property")?;
    let value = output.stdout_text("get_property")?.trim();
    if value.is_empty()
        || value.len() > MAX_PROPERTY_BYTES
        || value.chars().any(|character| character.is_control())
    {
        return Err(HarmonyHdcError::InvalidOutput {
            operation: "get_property",
        });
    }
    Ok(value.to_owned())
}

fn require_empty_stderr(
    output: &HdcCommandOutput,
    operation: &'static str,
) -> HarmonyHdcResult<()> {
    if output.stderr_text(operation)?.trim().is_empty() {
        Ok(())
    } else {
        Err(HarmonyHdcError::InvalidOutput { operation })
    }
}

fn validate_action_ack(output: &HdcCommandOutput, operation: &'static str) -> HarmonyHdcResult<()> {
    require_empty_stderr(output, operation)?;
    let normalized = output.stdout_text(operation)?.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || matches!(
            normalized.as_str(),
            "ok" | "success" | "start ability successfully" | "start ability successfully."
        )
    {
        Ok(())
    } else {
        Err(HarmonyHdcError::InvalidOutput { operation })
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
struct SwipeArgs {
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
    duration_ms: u32,
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
struct LaunchArgs {
    bundle_name: String,
    ability_name: String,
}

enum ParsedAction {
    Tap(TapArgs),
    Swipe(SwipeArgs),
    InputText(HdcInputText),
    KeyPress(HarmonyKey),
    Launch(HarmonyBundleName, HarmonyAbilityName),
}

impl ParsedAction {
    fn parse(name: &str, arguments: Value) -> DriverResult<Self> {
        let invalid = || DriverError::InvalidArguments {
            action: name.to_owned(),
            message: "arguments do not match the advertised schema".to_owned(),
        };
        match name {
            "tap" => {
                let value = parse_tap_args(name, arguments)?;
                if value.x > MAX_COORDINATE || value.y > MAX_COORDINATE {
                    return Err(invalid());
                }
                Ok(Self::Tap(value))
            }
            "swipe" => {
                let value = parse_swipe_args(name, arguments)?;
                let coordinates = [value.start_x, value.start_y, value.end_x, value.end_y];
                if coordinates.into_iter().any(|value| value > MAX_COORDINATE)
                    || value.duration_ms == 0
                    || value.duration_ms > MAX_SWIPE_DURATION_MS
                    || (value.start_x == value.end_x && value.start_y == value.end_y)
                {
                    return Err(invalid());
                }
                Ok(Self::Swipe(value))
            }
            "inputText" => {
                let value: InputTextArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                Ok(Self::InputText(
                    HdcInputText::parse(value.text).map_err(|_| invalid())?,
                ))
            }
            "keyPress" => {
                let value: KeyPressArgs =
                    serde_json::from_value(arguments).map_err(|_| invalid())?;
                Ok(Self::KeyPress(
                    HarmonyKey::parse(&value.key).ok_or_else(invalid)?,
                ))
            }
            "launch" => {
                let value: LaunchArgs = serde_json::from_value(arguments).map_err(|_| invalid())?;
                Ok(Self::Launch(
                    HarmonyBundleName::parse(value.bundle_name).map_err(|_| invalid())?,
                    HarmonyAbilityName::parse(value.ability_name).map_err(|_| invalid())?,
                ))
            }
            _ => Err(DriverError::UnknownAction(name.to_owned())),
        }
    }

    fn into_operation(self, viewport: &Viewport) -> DriverResult<HdcOperation> {
        match self {
            Self::Tap(value) => {
                validate_input_point("tap", value.x, value.y, viewport)?;
                Ok(HdcOperation::Tap {
                    x: value.x,
                    y: value.y,
                })
            }
            Self::Swipe(value) => {
                validate_input_point("swipe", value.start_x, value.start_y, viewport)?;
                validate_input_point("swipe", value.end_x, value.end_y, viewport)?;
                Ok(HdcOperation::Swipe {
                    start_x: value.start_x,
                    start_y: value.start_y,
                    end_x: value.end_x,
                    end_y: value.end_y,
                    velocity_pps: swipe_velocity(&value),
                })
            }
            Self::InputText(value) => Ok(HdcOperation::InputText(value)),
            Self::KeyPress(value) => Ok(HdcOperation::KeyPress(value)),
            Self::Launch(bundle, ability) => Ok(HdcOperation::Launch { bundle, ability }),
        }
    }
}

fn validate_input_point(action: &str, x: u32, y: u32, viewport: &Viewport) -> DriverResult<()> {
    if x < viewport.width && y < viewport.height {
        Ok(())
    } else {
        Err(DriverError::InvalidArguments {
            action: action.to_owned(),
            message: "coordinate is outside the current HarmonyOS viewport".to_owned(),
        })
    }
}

fn parse_tap_args(action: &str, arguments: Value) -> DriverResult<TapArgs> {
    let mut fields = exact_action_object(action, arguments, &["x", "y"])?;
    Ok(TapArgs {
        x: take_mathematical_u32(action, &mut fields, "x")?,
        y: take_mathematical_u32(action, &mut fields, "y")?,
    })
}

fn parse_swipe_args(action: &str, arguments: Value) -> DriverResult<SwipeArgs> {
    let mut fields = exact_action_object(
        action,
        arguments,
        &["startX", "startY", "endX", "endY", "durationMs"],
    )?;
    Ok(SwipeArgs {
        start_x: take_mathematical_u32(action, &mut fields, "startX")?,
        start_y: take_mathematical_u32(action, &mut fields, "startY")?,
        end_x: take_mathematical_u32(action, &mut fields, "endX")?,
        end_y: take_mathematical_u32(action, &mut fields, "endY")?,
        duration_ms: take_mathematical_u32(action, &mut fields, "durationMs")?,
    })
}

fn exact_action_object(
    action: &str,
    arguments: Value,
    required: &[&str],
) -> DriverResult<Map<String, Value>> {
    let fields = arguments
        .as_object()
        .filter(|fields| {
            fields.len() == required.len()
                && required.iter().all(|field| fields.contains_key(*field))
        })
        .cloned()
        .ok_or_else(|| invalid_action_arguments(action))?;
    Ok(fields)
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

fn invalid_action_arguments(action: &str) -> DriverError {
    DriverError::InvalidArguments {
        action: action.to_owned(),
        message: "arguments do not match the advertised schema".to_owned(),
    }
}

fn swipe_velocity(value: &SwipeArgs) -> u32 {
    let distance = value
        .start_x
        .abs_diff(value.end_x)
        .max(value.start_y.abs_diff(value.end_y));
    let velocity = (u64::from(distance) * 1_000).div_ceil(u64::from(value.duration_ms));
    u32::try_from(velocity)
        .unwrap_or(u32::MAX)
        .clamp(MIN_SWIPE_VELOCITY_PPS, MAX_SWIPE_VELOCITY_PPS)
}

fn action_definitions() -> Vec<ActionDefinition> {
    vec![
        definition(
            "tap",
            "Tap one bounded HarmonyOS screen coordinate.",
            object_schema(
                json!({
                    "x": coordinate_schema(),
                    "y": coordinate_schema()
                }),
                &["x", "y"],
            ),
        ),
        definition(
            "swipe",
            "Swipe between two bounded HarmonyOS screen coordinates.",
            swipe_schema(),
        ),
        definition(
            "inputText",
            "Type bounded shell-safe ASCII text with HarmonyOS uitest.",
            object_schema(
                json!({ "text": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": HdcInputText::MAX_BYTES,
                    "pattern": "^[A-Za-z0-9 .,_@+-]+$"
                }}),
                &["text"],
            ),
        ),
        definition(
            "keyPress",
            "Press one key from the closed HarmonyOS navigation/editing set.",
            object_schema(
                json!({ "key": { "type": "string", "enum": HarmonyKey::VALUES } }),
                &["key"],
            ),
        ),
        definition(
            "launch",
            "Launch one explicitly named HarmonyOS bundle ability.",
            object_schema(
                json!({
                    "bundleName": {
                        "type": "string", "minLength": 3, "maxLength": 255,
                        "pattern": "^[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)+$"
                    },
                    "abilityName": {
                        "type": "string", "minLength": 1, "maxLength": 255,
                        "pattern": "^[A-Za-z][A-Za-z0-9_]*(\\.[A-Za-z][A-Za-z0-9_]*)*$"
                    }
                }),
                &["bundleName", "abilityName"],
            ),
        ),
    ]
}

fn definition(name: &str, description: &str, input_schema: Value) -> ActionDefinition {
    ActionDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        protection: ActionProtection::Standard,
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

fn coordinate_schema() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": MAX_COORDINATE })
}

fn swipe_schema() -> Value {
    object_schema(
        json!({
            "startX": coordinate_schema(),
            "startY": coordinate_schema(),
            "endX": coordinate_schema(),
            "endY": coordinate_schema(),
            "durationMs": {
                "type": "integer", "minimum": 1, "maximum": MAX_SWIPE_DURATION_MS
            }
        }),
        &["startX", "startY", "endX", "endY", "durationMs"],
    )
}

#[derive(Clone, Copy)]
struct PixelSize {
    width: u32,
    height: u32,
}

struct ParsedLayout {
    value: Value,
    width: u32,
    height: u32,
    object_count: usize,
}

fn parse_layout(bytes: &[u8]) -> HarmonyHdcResult<ParsedLayout> {
    if bytes.is_empty() || bytes.len() > MAX_LAYOUT_BYTES {
        return Err(HarmonyHdcError::InvalidOutput {
            operation: "dump_layout",
        });
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| HarmonyHdcError::InvalidOutput {
            operation: "dump_layout",
        })?;
    let mut metrics = LayoutMetrics::default();
    visit_layout(&value, &mut metrics)?;

    if let Some(object) = value.as_object() {
        if let (Some(width), Some(height)) = (
            object.get("width").and_then(Value::as_u64),
            object.get("height").and_then(Value::as_u64),
        ) {
            metrics.max_right = metrics.max_right.max(width);
            metrics.max_bottom = metrics.max_bottom.max(height);
        }
    }
    let width = u32::try_from(metrics.max_right).ok();
    let height = u32::try_from(metrics.max_bottom).ok();
    let valid = width.zip(height).filter(|(width, height)| {
        *width > 0
            && *height > 0
            && *width <= MAX_SCREENSHOT_DIMENSION
            && *height <= MAX_SCREENSHOT_DIMENSION
            && u64::from(*width) * u64::from(*height) <= MAX_SCREENSHOT_PIXELS
    });
    let (width, height) = valid.ok_or(HarmonyHdcError::InvalidOutput {
        operation: "dump_layout",
    })?;
    Ok(ParsedLayout {
        value,
        width,
        height,
        object_count: metrics.object_count,
    })
}

#[derive(Default)]
struct LayoutMetrics {
    max_right: u64,
    max_bottom: u64,
    object_count: usize,
}

fn visit_layout(value: &Value, metrics: &mut LayoutMetrics) -> HarmonyHdcResult<()> {
    match value {
        Value::Object(object) => {
            metrics.object_count += 1;
            if metrics.object_count > MAX_HIERARCHY_OBJECTS {
                return Err(HarmonyHdcError::InvalidOutput {
                    operation: "dump_layout",
                });
            }
            for (key, child) in object {
                if matches!(key.as_str(), "bounds" | "boundsInScreen" | "rect") {
                    apply_bounds(child, metrics);
                }
                visit_layout(child, metrics)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                visit_layout(child, metrics)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn apply_bounds(value: &Value, metrics: &mut LayoutMetrics) {
    let coordinates = match value {
        Value::String(value) => value
            .split(|character: char| !character.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse::<u64>().ok())
            .collect::<Vec<_>>(),
        Value::Array(values) => values.iter().filter_map(Value::as_u64).collect(),
        Value::Object(object) => ["left", "top", "right", "bottom"]
            .into_iter()
            .filter_map(|key| object.get(key).and_then(Value::as_u64))
            .collect(),
        _ => Vec::new(),
    };
    if let [left, top, right, bottom] = coordinates.as_slice() {
        if right > left && bottom > top {
            metrics.max_right = metrics.max_right.max(*right);
            metrics.max_bottom = metrics.max_bottom.max(*bottom);
        }
    }
}

fn validate_png(bytes: &[u8]) -> HarmonyHdcResult<PixelSize> {
    if bytes.is_empty() || bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(HarmonyHdcError::InvalidOutput {
            operation: "capture_screenshot",
        });
    }
    let mut options = DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(Limits {
        bytes: MAX_DECODED_SCREENSHOT_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| HarmonyHdcError::InvalidOutput {
            operation: "capture_screenshot",
        })?;
    let info = reader.info();
    let pixels = u64::from(info.width)
        .checked_mul(u64::from(info.height))
        .ok_or(HarmonyHdcError::InvalidOutput {
            operation: "capture_screenshot",
        })?;
    if info.width == 0
        || info.height == 0
        || info.width > MAX_SCREENSHOT_DIMENSION
        || info.height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
        || reader
            .output_buffer_size()
            .is_none_or(|size| size > MAX_DECODED_SCREENSHOT_BYTES)
    {
        return Err(HarmonyHdcError::InvalidOutput {
            operation: "capture_screenshot",
        });
    }
    let dimensions = PixelSize {
        width: info.width,
        height: info.height,
    };
    while reader
        .next_row()
        .map_err(|_| HarmonyHdcError::InvalidOutput {
            operation: "capture_screenshot",
        })?
        .is_some()
    {}
    reader
        .finish()
        .map_err(|_| HarmonyHdcError::InvalidOutput {
            operation: "capture_screenshot",
        })?;
    Ok(dimensions)
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

fn ensure_active(control: &ExecutionControl) -> DriverResult<()> {
    if control.is_cancelled() {
        Err(DriverError::Cancelled)
    } else if control.is_expired() {
        Err(DriverError::TimedOut)
    } else {
        Ok(())
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use devicerail_protocol::Viewport;

    use super::{ParsedAction, action_definitions, parse_layout, validate_png};

    #[test]
    fn hierarchy_extracts_viewport_from_realistic_bounds() {
        let parsed = parse_layout(
            br#"{"root":{"bounds":"[0,0][1080,1920]","children":[{"bounds":[10,20,100,200]}]}}"#,
        )
        .expect("layout");
        assert_eq!((parsed.width, parsed.height), (1080, 1920));
        assert_eq!(parsed.object_count, 3);
    }

    #[test]
    fn actions_reject_unknown_fields_unsafe_text_and_stationary_swipes() {
        assert!(ParsedAction::parse("tap", json!({"x": 1, "y": 2, "z": 3})).is_err());
        assert!(ParsedAction::parse("inputText", json!({"text": "x; id"})).is_err());
        assert!(
            ParsedAction::parse(
                "swipe",
                json!({"startX": 1, "startY": 2, "endX": 1, "endY": 2, "durationMs": 10})
            )
            .is_err()
        );
    }

    #[test]
    fn tap_and_swipe_are_bounded_by_the_captured_viewport() {
        let viewport = Viewport {
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
            assert!(parsed.into_operation(&viewport).is_err());
        }

        let valid =
            ParsedAction::parse("tap", json!({ "x": 9, "y": 19 })).expect("schema-valid tap");
        assert!(valid.into_operation(&viewport).is_ok());
    }

    #[test]
    fn screenshot_parser_rejects_non_png_bytes() {
        assert!(validate_png(b"not a png").is_err());
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
            ("tap", json!({ "x": 1_000_001.0, "y": 2 }), false),
            (
                "swipe",
                serde_json::from_str(
                    r#"{"startX":0.0,"startY":1e0,"endX":2.0,"endY":3e0,"durationMs":100.0}"#,
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
                "swipe",
                json!({
                    "startX": 0,
                    "startY": 1,
                    "endX": 2,
                    "endY": 3,
                    "durationMs": 1.5
                }),
                false,
            ),
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
}
