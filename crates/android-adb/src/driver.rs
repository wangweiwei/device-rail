use async_trait::async_trait;
use devicerail_core::{
    DeviceDriver, DeviceOperationError, DeviceOperationResult, DriverError, DriverOperationContext,
    DriverResult, ExecutionControl, now_ms,
};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionProtection, ActionResult, AssetRef, DeviceId, DeviceInfo,
    Observation,
};
use serde_json::{Map, Value, json};

use crate::{
    AdbCommandOutput, AdbInputText, AdbOperation, AndroidAdbError, AndroidDevice, AndroidKey,
    AndroidPackageName, ProtectedAdbInput, observation::AndroidObservationError,
};

const MAX_SWIPE_DURATION_MS: u32 = 60_000;
const MAX_SCROLL_DELTA: i32 = 100_000;
const SCROLL_DURATION_MS: u32 = 300;

/// Production Android implementation of DeviceRail's Driver boundary.
///
/// AndroidDevice owns discovery/lifecycle and the closed ADB command surface;
/// this wrapper adds the protocol action contract and linearizes each Action's
/// before snapshot, device mutation, and after snapshot under one device gate.
pub struct AndroidDriver {
    device: AndroidDevice,
}

impl AndroidDriver {
    pub fn new(device: AndroidDevice) -> Self {
        Self { device }
    }

    pub async fn device_info(&self) -> DeviceInfo {
        self.device.device_info().await
    }

    async fn capture_gate_held(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        self.device
            .observe_gate_held(context)
            .await
            .map_err(AndroidObservationError::into_device_operation_error)
    }
}

#[async_trait]
impl DeviceDriver for AndroidDriver {
    fn id(&self) -> &DeviceId {
        self.device.id()
    }

    fn action_protection(&self, name: &str) -> Option<ActionProtection> {
        match name {
            "inputSecret" => Some(ActionProtection::Protected),
            "tap" | "keyPress" | "swipe" | "scroll" | "inputText" | "launch" | "terminate"
            | "back" | "home" | "recentApps" => Some(ActionProtection::Standard),
            _ => None,
        }
    }

    async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
        self.device.connect(control).await.map_err(map_adb_error)
    }

    async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.device.disconnect(control).await.map_err(map_adb_error)
    }

    async fn capabilities(
        &self,
        control: &ExecutionControl,
    ) -> DriverResult<Vec<ActionDefinition>> {
        ensure_active(control)?;
        Ok(action_definitions())
    }

    async fn health_check(&self, control: &ExecutionControl) -> DriverResult<()> {
        self.device.health(control).await.map_err(map_adb_error)?;
        Ok(())
    }

    async fn observe(
        &self,
        context: &DriverOperationContext,
    ) -> DeviceOperationResult<Observation> {
        self.device.capture_observation(context).await
    }

    async fn execute(
        &self,
        context: &DriverOperationContext,
        call: ActionCall,
    ) -> DeviceOperationResult<ActionResult> {
        let control = context.control();
        let _operation = self
            .device
            .lock_operation_write(control, "execute")
            .await
            .map_err(map_adb_operation_error)?;

        // DeviceDriver's contract requires disconnected use to win over both
        // an unknown action name and malformed arguments.
        if !self
            .device
            .connected_gate_held(control, "execute")
            .await
            .map_err(map_adb_operation_error)?
        {
            return Err(DriverError::NotConnected(self.device.id().clone()).into());
        }

        let ActionCall {
            id: call_id,
            name,
            arguments,
        } = call;
        let parsed = if name == "inputSecret" {
            ParsedAction::parse_secret(arguments)?
        } else {
            let parsed = ParsedAction::parse(&name, &arguments)?;
            drop(arguments);
            parsed
        };
        // Do not retain the caller's inputText value for the slower capture
        // and process phases. Parsed input text carries only a redacted-debug,
        // allowlisted command representation.
        drop(name);

        let protected = parsed.is_protected();
        let before = if protected {
            self.device
                .observe_protected_gate_held(context)
                .await
                .map_err(AndroidObservationError::into_device_operation_error)?
        } else {
            self.capture_gate_held(context).await?
        };
        let prepared = parsed.prepare(&before)?;
        let started_at_ms = now_ms();
        match prepared.operation {
            PreparedOperation::Standard(operation) => {
                let command_output = self
                    .device
                    .run_operation_gate_held(operation, control)
                    .await
                    .map_err(map_adb_operation_error)?;
                prepared.validation.validate(&command_output)?;
            }
            PreparedOperation::Protected(input) => {
                self.device
                    .run_protected_operation_gate_held(input, control)
                    .await
                    .map_err(map_adb_operation_error)?;
            }
        }
        ensure_active(control)?;
        let after = if protected {
            self.device
                .observe_protected_gate_held(context)
                .await
                .map_err(AndroidObservationError::into_device_operation_error)?
        } else {
            self.capture_gate_held(context).await?
        };
        ensure_active(control)?;
        let finished_at_ms = now_ms().max(started_at_ms);

        let evidence = deduplicated_screenshots(&before, &after);
        Ok(ActionResult {
            call_id,
            started_at_ms,
            finished_at_ms,
            output: prepared.output,
            before: Some(before),
            after: Some(after),
            evidence,
            execution: None,
        })
    }
}

fn action_definitions() -> Vec<ActionDefinition> {
    const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
    vec![
        ActionDefinition {
            name: "tap".to_owned(),
            description: "Tap one point in the current Android screenshot coordinate space"
                .to_owned(),
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
        },
        ActionDefinition {
            name: "keyPress".to_owned(),
            description: "Press one key from DeviceRail's closed Android key set".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["key"],
                "properties": {
                    "key": { "type": "string", "enum": AndroidKey::VALUES }
                }
            }),
        },
        ActionDefinition {
            name: "swipe".to_owned(),
            description:
                "Swipe between two points in the current Android screenshot coordinate space"
                    .to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["startX", "startY", "endX", "endY", "durationMs"],
                "properties": {
                    "startX": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                    "startY": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                    "endX": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                    "endY": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                    "durationMs": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SWIPE_DURATION_MS
                    }
                }
            }),
        },
        ActionDefinition {
            name: "scroll".to_owned(),
            description: "Scroll in the requested direction using a bounded, viewport-safe gesture"
                .to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
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
                },
                "not": {
                    "required": ["deltaX", "deltaY"],
                    "properties": {
                        "deltaX": { "const": 0 },
                        "deltaY": { "const": 0 }
                    }
                }
            }),
        },
        ActionDefinition {
            name: "inputText".to_owned(),
            description: "Type remote-shell-safe ASCII into the focused Android control".to_owned(),
            protection: ActionProtection::Standard,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": {
                    "text": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": AdbInputText::MAX_BYTES,
                        "pattern": r"^[A-Za-z0-9 .,_@+=:/-]+$"
                    }
                }
            }),
        },
        package_action_definition(
            DIALECT,
            "launch",
            "Launch the package's current-user MAIN/LAUNCHER activity and wait for completion",
        ),
        package_action_definition(
            DIALECT,
            "terminate",
            "Force-stop the package for Android's current user",
        ),
        empty_action_definition(DIALECT, "back", "Invoke Android system Back navigation"),
        empty_action_definition(DIALECT, "home", "Invoke Android system Home navigation"),
        empty_action_definition(
            DIALECT,
            "recentApps",
            "Open Android's system application switcher",
        ),
        ActionDefinition {
            name: "inputSecret".to_owned(),
            description: "Type a protected printable-ASCII value through operation-scoped stdin"
                .to_owned(),
            protection: ActionProtection::Protected,
            input_schema: json!({
                "$schema": DIALECT,
                "type": "object",
                "additionalProperties": false,
                "required": ["secret"],
                "properties": {
                    "secret": {
                        "type": "string",
                        "minLength": ProtectedAdbInput::MIN_BYTES,
                        "maxLength": ProtectedAdbInput::MAX_BYTES,
                        "pattern": r"^[\u0020-\u007e]+$",
                        "not": { "pattern": "%s" }
                    }
                }
            }),
        },
    ]
}

fn package_action_definition(
    dialect: &'static str,
    name: &'static str,
    description: &'static str,
) -> ActionDefinition {
    ActionDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        protection: ActionProtection::Standard,
        input_schema: json!({
            "$schema": dialect,
            "type": "object",
            "additionalProperties": false,
            "required": ["packageName"],
            "properties": {
                "packageName": {
                    "type": "string",
                    "minLength": AndroidPackageName::MIN_BYTES,
                    "maxLength": AndroidPackageName::MAX_BYTES,
                    "pattern": r"^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)+$"
                }
            }
        }),
    }
}

fn empty_action_definition(
    dialect: &'static str,
    name: &'static str,
    description: &'static str,
) -> ActionDefinition {
    ActionDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        protection: ActionProtection::Standard,
        input_schema: json!({
            "$schema": dialect,
            "type": "object",
            "additionalProperties": false,
            "required": [],
            "properties": {}
        }),
    }
}

enum ParsedAction {
    Tap {
        x: u32,
        y: u32,
    },
    KeyPress(AndroidKey),
    Swipe {
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
    },
    Scroll {
        delta_x: i32,
        delta_y: i32,
    },
    InputText(AdbInputText),
    Launch(AndroidPackageName),
    Terminate(AndroidPackageName),
    Back,
    Home,
    RecentApps,
    InputSecret(ProtectedAdbInput),
}

struct PreparedAction {
    operation: PreparedOperation,
    output: Value,
    validation: OperationValidation,
}

enum PreparedOperation {
    Standard(AdbOperation),
    Protected(ProtectedAdbInput),
}

#[derive(Clone, Copy)]
enum OperationValidation {
    EmptyMutation,
    Launch,
}

impl OperationValidation {
    fn validate(self, output: &AdbCommandOutput) -> DeviceOperationResult<()> {
        match self {
            Self::EmptyMutation => validate_empty_mutation_result(
                output.stdout_text().map_err(map_adb_operation_error)?,
                output.stderr_text(),
            )
            .map_err(Into::into),
            Self::Launch => {
                let stdout = output.stdout_text().map_err(map_adb_operation_error)?;
                validate_launch_result(stdout, output.stderr_text()).map_err(Into::into)
            }
        }
    }
}

fn validate_launch_result(stdout: &str, stderr: &str) -> DriverResult<()> {
    if stdout
        .lines()
        .chain(stderr.lines())
        .any(is_launch_remote_error_line)
        || !stderr.lines().all(is_allowed_adb_stderr_line)
    {
        return Err(DriverError::Platform {
            code: "android_app_launch_error".to_owned(),
            retryable: false,
        });
    }

    let mut previous_field = None;
    let mut seen_fields = 0_u16;
    let mut status = None;
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let field = classify_launch_output_line(line, &mut status)?;
        let field_bit = 1_u16 << (field as u8);
        if seen_fields & field_bit != 0 || previous_field.is_some_and(|previous| field <= previous)
        {
            return Err(invalid_launch_result());
        }
        seen_fields |= field_bit;
        previous_field = Some(field);
    }

    if seen_fields & LaunchOutputField::Status.bit() == 0
        || seen_fields & LaunchOutputField::Complete.bit() == 0
    {
        return Err(invalid_launch_result());
    }

    match status.expect("a seen Status field sets launch status") {
        LaunchStatus::Ok => Ok(()),
        LaunchStatus::Timeout => Err(DriverError::Platform {
            code: "android_app_launch_timeout".to_owned(),
            retryable: true,
        }),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchStatus {
    Ok,
    Timeout,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum LaunchOutputField {
    Starting,
    Warning,
    Status,
    LaunchState,
    Activity,
    ThisTime,
    TotalTime,
    WaitTime,
    Complete,
}

impl LaunchOutputField {
    const fn bit(self) -> u16 {
        1_u16 << (self as u8)
    }
}

fn classify_launch_output_line(
    line: &str,
    status: &mut Option<LaunchStatus>,
) -> DriverResult<LaunchOutputField> {
    const DELIVERED_TO_TOP: &str = "Warning: Activity not started, intent has been delivered to currently running top-most instance.";
    const TASK_TO_FRONT: &str =
        "Warning: Activity not started, its current task has been brought to the front";

    if is_aosp_starting_line(line) {
        Ok(LaunchOutputField::Starting)
    } else if line == DELIVERED_TO_TOP || line == TASK_TO_FRONT {
        Ok(LaunchOutputField::Warning)
    } else if let Some(value) = line.strip_prefix("Status:") {
        *status = Some(match value.trim() {
            "ok" => LaunchStatus::Ok,
            "timeout" => LaunchStatus::Timeout,
            _ => return Err(invalid_launch_result()),
        });
        Ok(LaunchOutputField::Status)
    } else if let Some(value) = line.strip_prefix("LaunchState:") {
        if !is_aosp_launch_state(value.trim()) {
            return Err(invalid_launch_result());
        }
        Ok(LaunchOutputField::LaunchState)
    } else if let Some(value) = line.strip_prefix("Activity:") {
        if !is_aosp_component(value.trim()) {
            return Err(invalid_launch_result());
        }
        Ok(LaunchOutputField::Activity)
    } else if let Some(value) = line.strip_prefix("ThisTime:") {
        validate_aosp_duration(value)?;
        Ok(LaunchOutputField::ThisTime)
    } else if let Some(value) = line.strip_prefix("TotalTime:") {
        validate_aosp_duration(value)?;
        Ok(LaunchOutputField::TotalTime)
    } else if let Some(value) = line.strip_prefix("WaitTime:") {
        validate_aosp_duration(value)?;
        Ok(LaunchOutputField::WaitTime)
    } else if line == "Complete" {
        Ok(LaunchOutputField::Complete)
    } else {
        Err(invalid_launch_result())
    }
}

fn is_aosp_starting_line(line: &str) -> bool {
    line.strip_prefix("Starting: Intent {")
        .and_then(|value| value.strip_suffix('}'))
        .is_some_and(|value| !value.trim().is_empty() && !value.chars().any(char::is_control))
}

fn is_aosp_launch_state(value: &str) -> bool {
    matches!(value, "COLD" | "WARM" | "HOT" | "RELAUNCH")
        || value
            .strip_prefix("UNKNOWN (")
            .and_then(|value| value.strip_suffix(')'))
            .is_some_and(|value| value.parse::<i32>().is_ok())
}

fn is_aosp_component(value: &str) -> bool {
    let Some((package, class)) = value.split_once('/') else {
        return false;
    };
    !package.is_empty()
        && !class.is_empty()
        && !class.contains('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b':' | b'\\'))
}

fn validate_aosp_duration(value: &str) -> DriverResult<()> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_launch_result());
    }
    value
        .parse::<u64>()
        .map(|_| ())
        .map_err(|_| invalid_launch_result())
}

fn invalid_launch_result() -> DriverError {
    DriverError::Platform {
        code: "android_app_launch_invalid_result".to_owned(),
        retryable: true,
    }
}

fn validate_empty_mutation_result(stdout: &str, stderr: &str) -> DriverResult<()> {
    if stdout
        .lines()
        .chain(stderr.lines())
        .any(is_remote_error_line)
    {
        Err(DriverError::Platform {
            code: "android_mutation_remote_error".to_owned(),
            retryable: false,
        })
    } else if stdout.trim().is_empty() && stderr.lines().all(is_allowed_adb_stderr_line) {
        Ok(())
    } else {
        Err(DriverError::Platform {
            code: "android_mutation_invalid_result".to_owned(),
            retryable: true,
        })
    }
}

fn is_remote_error_line(line: &str) -> bool {
    let normalized = line.to_ascii_lowercase();
    normalized.contains("error:")
        || normalized.contains("exception")
        || normalized.contains("permission denial")
        || normalized.contains("permission denied")
}

fn is_launch_remote_error_line(line: &str) -> bool {
    if is_remote_error_line(line) {
        return true;
    }
    let normalized = line.trim().to_ascii_lowercase();
    normalized == "failure"
        || normalized.starts_with("failure ")
        || normalized.starts_with("failure:")
        || normalized == "failed"
        || normalized.starts_with("failed ")
        || normalized.starts_with("failed:")
}

fn is_allowed_adb_stderr_line(line: &str) -> bool {
    let line = line.trim();
    line.is_empty() || line.starts_with("* daemon ")
}

impl ParsedAction {
    fn parse_secret(arguments: Value) -> DriverResult<Self> {
        let Value::Object(mut fields) = arguments else {
            return Err(invalid("inputSecret", "arguments must be an object"));
        };
        let secret = fields.remove("secret").ok_or_else(|| {
            invalid(
                "inputSecret",
                "secret must be a protected printable-ASCII string",
            )
        })?;
        let Value::String(secret) = secret else {
            return Err(invalid(
                "inputSecret",
                "secret must be a protected printable-ASCII string",
            ));
        };
        let input = ProtectedAdbInput::parse(secret.into_bytes()).map_err(|_| {
            invalid(
                "inputSecret",
                "secret must be 1..=1024 printable ASCII bytes and must not contain the reserved percent-s sequence",
            )
        })?;
        if !fields.is_empty() {
            return Err(invalid(
                "inputSecret",
                "arguments contain an unexpected property",
            ));
        }
        Ok(Self::InputSecret(input))
    }

    fn is_protected(&self) -> bool {
        matches!(self, Self::InputSecret(_))
    }

    fn parse(name: &str, arguments: &Value) -> DriverResult<Self> {
        match name {
            "tap" => {
                let fields = object_fields(arguments, name, &["x", "y"])?;
                Ok(Self::Tap {
                    x: u32_field(fields, name, "x")?,
                    y: u32_field(fields, name, "y")?,
                })
            }
            "keyPress" => {
                let fields = object_fields(arguments, name, &["key"])?;
                let value = string_field(fields, name, "key")?;
                let key = AndroidKey::parse(value).ok_or_else(|| {
                    invalid(name, "key must be one of the advertised Android key values")
                })?;
                Ok(Self::KeyPress(key))
            }
            "swipe" => {
                let fields = object_fields(
                    arguments,
                    name,
                    &["startX", "startY", "endX", "endY", "durationMs"],
                )?;
                let duration_ms = u32_field(fields, name, "durationMs")?;
                if !(1..=MAX_SWIPE_DURATION_MS).contains(&duration_ms) {
                    return Err(invalid(name, "durationMs must be in 1..=60000"));
                }
                Ok(Self::Swipe {
                    start_x: u32_field(fields, name, "startX")?,
                    start_y: u32_field(fields, name, "startY")?,
                    end_x: u32_field(fields, name, "endX")?,
                    end_y: u32_field(fields, name, "endY")?,
                    duration_ms,
                })
            }
            "scroll" => {
                let fields = object_fields(arguments, name, &["deltaX", "deltaY"])?;
                let delta_x = bounded_i32_field(fields, name, "deltaX")?;
                let delta_y = bounded_i32_field(fields, name, "deltaY")?;
                if delta_x == 0 && delta_y == 0 {
                    return Err(invalid(name, "at least one scroll delta must be non-zero"));
                }
                Ok(Self::Scroll { delta_x, delta_y })
            }
            "inputText" => {
                let fields = object_fields(arguments, name, &["text"])?;
                let text = string_field(fields, name, "text")?;
                let encoded = AdbInputText::parse(text).map_err(|_| {
                    invalid(
                        name,
                        "text must be 1..=1024 bytes of the advertised safe ASCII alphabet",
                    )
                })?;
                Ok(Self::InputText(encoded))
            }
            "launch" | "terminate" => {
                let fields = object_fields(arguments, name, &["packageName"])?;
                let value = string_field(fields, name, "packageName")?;
                let package = AndroidPackageName::parse(value).map_err(|_| {
                    invalid(
                        name,
                        "packageName must satisfy the advertised bounded Android application-id grammar",
                    )
                })?;
                if name == "launch" {
                    Ok(Self::Launch(package))
                } else {
                    Ok(Self::Terminate(package))
                }
            }
            "back" | "home" | "recentApps" => {
                object_fields(arguments, name, &[])?;
                Ok(match name {
                    "back" => Self::Back,
                    "home" => Self::Home,
                    "recentApps" => Self::RecentApps,
                    _ => unreachable!("closed navigation action match"),
                })
            }
            other => Err(DriverError::UnknownAction(other.to_owned())),
        }
    }

    fn prepare(self, before: &Observation) -> DriverResult<PreparedAction> {
        let width = before.viewport.width;
        let height = before.viewport.height;
        match self {
            Self::Tap { x, y } => {
                validate_point("tap", "x/y", x, y, width, height)?;
                Ok(PreparedAction {
                    operation: PreparedOperation::Standard(AdbOperation::Tap { x, y }),
                    output: json!({ "accepted": true, "x": x, "y": y }),
                    validation: OperationValidation::EmptyMutation,
                })
            }
            Self::KeyPress(key) => Ok(PreparedAction {
                operation: PreparedOperation::Standard(AdbOperation::KeyPress(key)),
                output: json!({ "accepted": true, "key": key.as_str() }),
                validation: OperationValidation::EmptyMutation,
            }),
            Self::Swipe {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => {
                validate_point("swipe", "startX/startY", start_x, start_y, width, height)?;
                validate_point("swipe", "endX/endY", end_x, end_y, width, height)?;
                Ok(PreparedAction {
                    operation: PreparedOperation::Standard(AdbOperation::Swipe {
                        start_x,
                        start_y,
                        end_x,
                        end_y,
                        duration_ms,
                    }),
                    output: json!({
                        "accepted": true,
                        "startX": start_x,
                        "startY": start_y,
                        "endX": end_x,
                        "endY": end_y,
                        "durationMs": duration_ms,
                    }),
                    validation: OperationValidation::EmptyMutation,
                })
            }
            Self::Scroll { delta_x, delta_y } => {
                let (start_x, end_x) = scroll_axis(width, delta_x, "deltaX")?;
                let (start_y, end_y) = scroll_axis(height, delta_y, "deltaY")?;
                Ok(PreparedAction {
                    operation: PreparedOperation::Standard(AdbOperation::Scroll {
                        start_x,
                        start_y,
                        end_x,
                        end_y,
                        duration_ms: SCROLL_DURATION_MS,
                    }),
                    output: json!({
                        "accepted": true,
                        "deltaX": delta_x,
                        "deltaY": delta_y,
                    }),
                    validation: OperationValidation::EmptyMutation,
                })
            }
            Self::InputText(text) => {
                let byte_len = text.byte_len();
                Ok(PreparedAction {
                    operation: PreparedOperation::Standard(AdbOperation::InputText(text)),
                    output: json!({ "accepted": true, "byteLength": byte_len }),
                    validation: OperationValidation::EmptyMutation,
                })
            }
            Self::Launch(package) => Ok(PreparedAction {
                operation: PreparedOperation::Standard(AdbOperation::Launch(package)),
                output: json!({ "accepted": true }),
                validation: OperationValidation::Launch,
            }),
            Self::Terminate(package) => Ok(PreparedAction {
                operation: PreparedOperation::Standard(AdbOperation::Terminate(package)),
                output: json!({ "accepted": true }),
                validation: OperationValidation::EmptyMutation,
            }),
            Self::Back => Ok(PreparedAction {
                operation: PreparedOperation::Standard(AdbOperation::Back),
                output: json!({ "accepted": true }),
                validation: OperationValidation::EmptyMutation,
            }),
            Self::Home => Ok(PreparedAction {
                operation: PreparedOperation::Standard(AdbOperation::Home),
                output: json!({ "accepted": true }),
                validation: OperationValidation::EmptyMutation,
            }),
            Self::RecentApps => Ok(PreparedAction {
                operation: PreparedOperation::Standard(AdbOperation::RecentApps),
                output: json!({ "accepted": true }),
                validation: OperationValidation::EmptyMutation,
            }),
            Self::InputSecret(input) => Ok(PreparedAction {
                operation: PreparedOperation::Protected(input),
                output: json!({ "accepted": true }),
                validation: OperationValidation::EmptyMutation,
            }),
        }
    }
}

fn object_fields<'a>(
    arguments: &'a Value,
    action: &str,
    allowed: &[&str],
) -> DriverResult<&'a Map<String, Value>> {
    let fields = arguments
        .as_object()
        .ok_or_else(|| invalid(action, "arguments must be an object"))?;
    if fields
        .keys()
        .any(|field| !allowed.contains(&field.as_str()))
    {
        return Err(invalid(action, "arguments contain an unexpected property"));
    }
    Ok(fields)
}

fn u32_field(fields: &Map<String, Value>, action: &str, field: &str) -> DriverResult<u32> {
    let value = fields.get(field).and_then(value_as_u32).ok_or_else(|| {
        invalid(
            action,
            format!("{field} must be an unsigned 32-bit integer"),
        )
    })?;
    Ok(value)
}

fn bounded_i32_field(fields: &Map<String, Value>, action: &str, field: &str) -> DriverResult<i32> {
    let value = fields
        .get(field)
        .and_then(value_as_i32)
        .filter(|value| (-MAX_SCROLL_DELTA..=MAX_SCROLL_DELTA).contains(value))
        .ok_or_else(|| {
            invalid(
                action,
                format!("{field} must be an integer in -100000..=100000"),
            )
        })?;
    Ok(value)
}

/// JSON Schema's `integer` type is mathematical rather than representation
/// based: `1`, `1.0`, and `1e0` are the same valid integer instance. Preserve
/// that contract while still requiring an exact, finite, in-range value.
fn value_as_u32(value: &Value) -> Option<u32> {
    if let Some(integer) = value.as_u64() {
        return u32::try_from(integer).ok();
    }
    let float = value.as_f64()?;
    if float.is_finite() && float >= 0.0 && float <= f64::from(u32::MAX) && float.fract() == 0.0 {
        let integer = float as u32;
        (f64::from(integer) == float).then_some(integer)
    } else {
        None
    }
}

fn value_as_i32(value: &Value) -> Option<i32> {
    if let Some(integer) = value.as_i64() {
        return i32::try_from(integer).ok();
    }
    let float = value.as_f64()?;
    if float.is_finite()
        && float >= f64::from(i32::MIN)
        && float <= f64::from(i32::MAX)
        && float.fract() == 0.0
    {
        let integer = float as i32;
        (f64::from(integer) == float).then_some(integer)
    } else {
        None
    }
}

fn string_field<'a>(
    fields: &'a Map<String, Value>,
    action: &str,
    field: &str,
) -> DriverResult<&'a str> {
    fields
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(action, format!("{field} must be a string")))
}

fn validate_point(
    action: &str,
    label: &str,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> DriverResult<()> {
    if x < width && y < height {
        Ok(())
    } else {
        Err(invalid(
            action,
            format!("{label} must be inside the current viewport"),
        ))
    }
}

fn scroll_axis(size: u32, delta: i32, field: &str) -> DriverResult<(u32, u32)> {
    let high = size.saturating_sub(1);
    let margin = if size >= 5 { size / 4 } else { 0 };
    let low = margin.min(high);
    let high = high.saturating_sub(margin).max(low);
    let center = low + (high - low) / 2;
    match delta.cmp(&0) {
        std::cmp::Ordering::Equal => Ok((center, center)),
        std::cmp::Ordering::Greater if low < high => Ok((high, low)),
        std::cmp::Ordering::Less if low < high => Ok((low, high)),
        _ => Err(invalid(
            "scroll",
            format!("the current viewport is too small for {field}"),
        )),
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

fn invalid(action: impl Into<String>, message: impl Into<String>) -> DriverError {
    DriverError::InvalidArguments {
        action: action.into(),
        message: message.into(),
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

fn map_adb_operation_error(error: AndroidAdbError) -> DeviceOperationError {
    map_adb_error(error).into()
}

fn map_adb_error(error: AndroidAdbError) -> DriverError {
    match error {
        AndroidAdbError::Cancelled => DriverError::Cancelled,
        AndroidAdbError::TimedOut { .. } => DriverError::TimedOut,
        error => DriverError::Platform {
            code: error.code().to_owned(),
            retryable: error.retryable(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use devicerail_core::{
        CancellationReason, DeviceDriver, DeviceRuntime, DriverError, EvidenceInput,
        EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl,
        ExecutionController, GcPolicy, GcReport, MemoryEventStore, OperationContext, PutEvidence,
        ReleaseReport, RuntimeError, ScreenshotPolicy, SessionEventStore, Sha256Digest,
        StartSession, StoredEvidence, TimeoutScope, now_ms,
    };
    use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
    use devicerail_protocol::{
        ActionCall, ActionDefinition, ActionProtection, ScreenshotOmissionReason, SessionId,
        TestEventPayload,
    };
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::{Notify, Semaphore};
    use uuid::Uuid;

    use super::{
        AndroidDriver, ParsedAction, action_definitions, validate_empty_mutation_result,
        validate_launch_result,
    };
    use crate::{
        AdbCommand, AdbCommandOutput, AdbCommandRunner, AdbDeviceState, AdbInputText, AdbOperation,
        AdbProperty, AdbSerial, AndroidAdbError, AndroidAdbResult, AndroidDevice,
        AndroidDeviceConfig, AndroidKey, AndroidPackageName, DiscoveredAndroidDevice,
        ProtectedAdbInput,
    };

    const TEST_ASYNC_STAGE_TIMEOUT: Duration = Duration::from_secs(10);
    const TEST_BLOCKED_MUTATION_TIMEOUT_MS: u64 = 5_000;

    struct DynamicRunner {
        serial: AdbSerial,
        png: Vec<u8>,
        calls: Mutex<Vec<AdbCommand>>,
        fail_action_once: AtomicBool,
        fail_connectivity_action_once: AtomicBool,
        fail_connectivity_capture_once: AtomicBool,
        fail_after_size_once: AtomicBool,
        launch_stdout_once: Mutex<Option<String>>,
        block_actions: AtomicBool,
        action_started: Notify,
        action_release: Semaphore,
        action_calls: AtomicUsize,
        active_actions: AtomicUsize,
        max_active_actions: AtomicUsize,
    }

    impl DynamicRunner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                serial: fixture_serial(),
                png: fixture_png(100, 200),
                calls: Mutex::new(Vec::new()),
                fail_action_once: AtomicBool::new(false),
                fail_connectivity_action_once: AtomicBool::new(false),
                fail_connectivity_capture_once: AtomicBool::new(false),
                fail_after_size_once: AtomicBool::new(false),
                launch_stdout_once: Mutex::new(None),
                block_actions: AtomicBool::new(false),
                action_started: Notify::new(),
                action_release: Semaphore::new(0),
                action_calls: AtomicUsize::new(0),
                active_actions: AtomicUsize::new(0),
                max_active_actions: AtomicUsize::new(0),
            })
        }

        fn operations(&self) -> Vec<AdbOperation> {
            self.calls
                .lock()
                .expect("calls lock")
                .iter()
                .map(|command| command.operation().clone())
                .collect()
        }

        fn call_count(&self) -> usize {
            self.calls.lock().expect("calls lock").len()
        }

        fn screenshot_calls(&self) -> usize {
            self.operations()
                .iter()
                .filter(|operation| **operation == AdbOperation::CaptureScreenshot)
                .count()
        }

        fn enable_action_blocking(&self) {
            self.block_actions.store(true, Ordering::SeqCst);
        }

        fn disable_action_blocking(&self) {
            self.block_actions.store(false, Ordering::SeqCst);
        }

        async fn wait_for_action_calls(&self, expected: usize) {
            while self.action_calls.load(Ordering::SeqCst) < expected {
                self.action_started.notified().await;
            }
        }

        fn release_one_action(&self) {
            self.action_release.add_permits(1);
        }

        fn record_active_action(&self) -> ActiveAction<'_> {
            let active = self.active_actions.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_actions.fetch_max(active, Ordering::SeqCst);
            ActiveAction { runner: self }
        }
    }

    struct ActiveAction<'a> {
        runner: &'a DynamicRunner,
    }

    impl Drop for ActiveAction<'_> {
        fn drop(&mut self) {
            self.runner.active_actions.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl AdbCommandRunner for DynamicRunner {
        async fn run(
            &self,
            command: AdbCommand,
            control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            assert_eq!(command.serial(), Some(&self.serial));
            let operation = command.operation().clone();
            self.calls.lock().expect("calls lock").push(command);
            let output = match operation {
                AdbOperation::GetState => AdbCommandOutput::text("get_state", "device\n"),
                AdbOperation::GetProperty(property) => {
                    let value = match property {
                        AdbProperty::BootCompleted => "1\n",
                        AdbProperty::ReleaseVersion => "15\n",
                        AdbProperty::ProductManufacturer => "Google\n",
                        AdbProperty::ProductModel => "Pixel\n",
                    };
                    AdbCommandOutput::text("get_property", value)
                }
                AdbOperation::CaptureScreenshot => {
                    if self
                        .fail_connectivity_capture_once
                        .swap(false, Ordering::SeqCst)
                    {
                        return Err(AndroidAdbError::ProcessFailed {
                            operation: "capture_screenshot",
                            status: Some(1),
                            stderr_tail: "error: device offline".to_owned(),
                        });
                    }
                    AdbCommandOutput::binary("capture_screenshot", self.png.clone())
                }
                AdbOperation::WindowSize => {
                    if self.fail_after_size_once.load(Ordering::SeqCst)
                        && self.screenshot_calls() >= 3
                        && self.fail_after_size_once.swap(false, Ordering::SeqCst)
                    {
                        return Err(AndroidAdbError::ProcessFailed {
                            operation: "window_size",
                            status: Some(1),
                            stderr_tail: "PRIVATE after-observation failure".to_owned(),
                        });
                    }
                    AdbCommandOutput::text("window_size", "Physical size: 100x200\n")
                }
                AdbOperation::WindowDensity => {
                    AdbCommandOutput::text("window_density", "Physical density: 320\n")
                }
                action @ (AdbOperation::Tap { .. }
                | AdbOperation::KeyPress(_)
                | AdbOperation::Swipe { .. }
                | AdbOperation::Scroll { .. }
                | AdbOperation::InputText(_)
                | AdbOperation::Launch(_)
                | AdbOperation::Terminate(_)
                | AdbOperation::Back
                | AdbOperation::Home
                | AdbOperation::RecentApps) => {
                    self.action_calls.fetch_add(1, Ordering::SeqCst);
                    // Preserve a permit when the test waiter is between its
                    // atomic count check and registering with `Notify`.
                    self.action_started.notify_one();
                    let _active = self.record_active_action();
                    if self
                        .fail_connectivity_action_once
                        .swap(false, Ordering::SeqCst)
                    {
                        return Err(AndroidAdbError::ProcessFailed {
                            operation: action.name(),
                            status: Some(1),
                            stderr_tail: "error: device offline".to_owned(),
                        });
                    }
                    if self.fail_action_once.swap(false, Ordering::SeqCst) {
                        return Err(AndroidAdbError::ProcessFailed {
                            operation: action.name(),
                            status: Some(1),
                            stderr_tail: "PRIVATE fixture text must not escape".to_owned(),
                        });
                    }
                    if self.block_actions.load(Ordering::SeqCst) {
                        let permit = self.action_release.acquire();
                        tokio::pin!(permit);
                        match control.remaining() {
                            Some(remaining) => {
                                let deadline = tokio::time::sleep(remaining);
                                tokio::pin!(deadline);
                                tokio::select! {
                                    biased;
                                    permit = &mut permit => permit
                                        .expect("action semaphore remains open")
                                        .forget(),
                                    _ = control.cancelled() => {
                                        return Err(AndroidAdbError::Cancelled);
                                    }
                                    () = &mut deadline => {
                                        return Err(AndroidAdbError::TimedOut {
                                            operation: action.name(),
                                        });
                                    }
                                }
                            }
                            None => {
                                tokio::select! {
                                    biased;
                                    permit = &mut permit => permit
                                        .expect("action semaphore remains open")
                                        .forget(),
                                    _ = control.cancelled() => {
                                        return Err(AndroidAdbError::Cancelled);
                                    }
                                }
                            }
                        }
                    }
                    let stdout = if matches!(&action, AdbOperation::Launch(_)) {
                        self.launch_stdout_once
                            .lock()
                            .expect("launch output lock")
                            .take()
                            .unwrap_or_else(|| "Status: ok\nComplete\n".to_owned())
                    } else {
                        String::new()
                    };
                    AdbCommandOutput::text(action.name(), stdout)
                }
                other => panic!("unexpected dynamic runner operation: {other:?}"),
            };
            Ok(output)
        }

        async fn run_protected(
            &self,
            command: AdbCommand,
            input: ProtectedAdbInput,
            control: &ExecutionControl,
        ) -> AndroidAdbResult<()> {
            assert_eq!(command.serial(), Some(&self.serial));
            assert_eq!(command.operation(), &AdbOperation::InputSecret);
            assert_eq!(format!("{input:?}"), "<redacted>");
            self.calls.lock().expect("calls lock").push(command);
            self.action_calls.fetch_add(1, Ordering::SeqCst);
            self.action_started.notify_one();
            let _active = self.record_active_action();
            if self
                .fail_connectivity_action_once
                .swap(false, Ordering::SeqCst)
            {
                return Err(AndroidAdbError::ProcessFailed {
                    operation: "input_secret",
                    status: Some(1),
                    stderr_tail: "error: device offline".to_owned(),
                });
            }
            if self.fail_action_once.swap(false, Ordering::SeqCst) {
                return Err(AndroidAdbError::ProcessFailed {
                    operation: "input_secret",
                    status: Some(1),
                    stderr_tail:
                        "/system/bin/sh: input: not found; input: permission denied; PRIVATE"
                            .to_owned(),
                });
            }
            if self.block_actions.load(Ordering::SeqCst) {
                let permit = self.action_release.acquire();
                tokio::pin!(permit);
                match control.remaining() {
                    Some(remaining) => {
                        let deadline = tokio::time::sleep(remaining);
                        tokio::pin!(deadline);
                        tokio::select! {
                            biased;
                            permit = &mut permit => permit
                                .expect("action semaphore remains open")
                                .forget(),
                            _ = control.cancelled() => {
                                return Err(AndroidAdbError::Cancelled);
                            }
                            () = &mut deadline => {
                                return Err(AndroidAdbError::TimedOut {
                                    operation: "input_secret",
                                });
                            }
                        }
                    }
                    None => {
                        tokio::select! {
                            biased;
                            permit = &mut permit => permit
                                .expect("action semaphore remains open")
                                .forget(),
                            _ = control.cancelled() => {
                                return Err(AndroidAdbError::Cancelled);
                            }
                        }
                    }
                }
            }
            drop(input);
            Ok(())
        }
    }

    struct MultiSerialRunner {
        png: Vec<u8>,
        calls: Mutex<Vec<(AdbSerial, AdbOperation)>>,
    }

    impl MultiSerialRunner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                png: fixture_png(100, 200),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn action_routes(&self) -> Vec<(String, &'static str)> {
            let mut routes = self
                .calls
                .lock()
                .expect("multi-serial calls lock")
                .iter()
                .filter(|(_, operation)| {
                    matches!(
                        operation,
                        AdbOperation::Launch(_)
                            | AdbOperation::Terminate(_)
                            | AdbOperation::Back
                            | AdbOperation::Home
                            | AdbOperation::RecentApps
                            | AdbOperation::InputSecret
                    )
                })
                .map(|(serial, operation)| (serial.as_str().to_owned(), operation.name()))
                .collect::<Vec<_>>();
            routes.sort();
            routes
        }
    }

    #[async_trait]
    impl AdbCommandRunner for MultiSerialRunner {
        async fn run(
            &self,
            command: AdbCommand,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<AdbCommandOutput> {
            let serial = command
                .serial()
                .cloned()
                .expect("multi-serial operations are device scoped");
            let operation = command.operation().clone();
            self.calls
                .lock()
                .expect("multi-serial calls lock")
                .push((serial, operation.clone()));
            Ok(match operation {
                AdbOperation::GetState => AdbCommandOutput::text("get_state", "device\n"),
                AdbOperation::GetProperty(property) => {
                    let value = match property {
                        AdbProperty::BootCompleted => "1\n",
                        AdbProperty::ReleaseVersion => "15\n",
                        AdbProperty::ProductManufacturer => "Google\n",
                        AdbProperty::ProductModel => "Pixel\n",
                    };
                    AdbCommandOutput::text("get_property", value)
                }
                AdbOperation::CaptureScreenshot => {
                    AdbCommandOutput::binary("capture_screenshot", self.png.clone())
                }
                AdbOperation::WindowSize => {
                    AdbCommandOutput::text("window_size", "Physical size: 100x200\n")
                }
                AdbOperation::WindowDensity => {
                    AdbCommandOutput::text("window_density", "Physical density: 320\n")
                }
                AdbOperation::Launch(_) => {
                    AdbCommandOutput::text("launch", "Status: ok\nComplete\n")
                }
                action @ (AdbOperation::Terminate(_)
                | AdbOperation::Back
                | AdbOperation::Home
                | AdbOperation::RecentApps) => AdbCommandOutput::text(action.name(), ""),
                other => panic!("unexpected multi-serial operation: {other:?}"),
            })
        }

        async fn run_protected(
            &self,
            command: AdbCommand,
            input: ProtectedAdbInput,
            _control: &ExecutionControl,
        ) -> AndroidAdbResult<()> {
            let serial = command
                .serial()
                .cloned()
                .expect("protected multi-serial operation is device scoped");
            assert_eq!(command.operation(), &AdbOperation::InputSecret);
            self.calls
                .lock()
                .expect("multi-serial calls lock")
                .push((serial, AdbOperation::InputSecret));
            drop(input);
            Ok(())
        }
    }

    fn fixture_serial() -> AdbSerial {
        AdbSerial::parse("emulator-5554").expect("fixture serial")
    }

    fn fixture_descriptor() -> DiscoveredAndroidDevice {
        descriptor_for(fixture_serial())
    }

    fn descriptor_for(serial: AdbSerial) -> DiscoveredAndroidDevice {
        DiscoveredAndroidDevice {
            serial,
            state: AdbDeviceState::Ready,
            product: Some("fixture".to_owned()),
            model: Some("Pixel".to_owned()),
            device: Some("fixture".to_owned()),
            transport_id: Some(7),
            extensions: BTreeMap::new(),
        }
    }

    fn fixture_driver(runner: &Arc<DynamicRunner>) -> AndroidDriver {
        let runner: Arc<dyn AdbCommandRunner> = runner.clone();
        AndroidDriver::new(
            AndroidDevice::new(fixture_descriptor(), runner, AndroidDeviceConfig::default())
                .expect("fixture Android device"),
        )
    }

    fn fixture_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            writer
                .write_image_data(&vec![0; (width * height) as usize])
                .expect("PNG body");
            writer.finish().expect("PNG trailer");
        }
        bytes
    }

    async fn session_context(
        events: &MemoryEventStore,
        driver: &AndroidDriver,
    ) -> OperationContext {
        let start = StartSession::new(None, Some(driver.id().clone()), now_ms());
        let context = OperationContext::new(start.session_id.clone(), None);
        events.start_session(start).await.expect("start Session");
        context
    }

    fn file_store(root: &TempDir) -> Arc<FileEvidenceStore> {
        Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("fixture Evidence Store"),
        )
    }

    async fn connected_runtime(
        runner: &Arc<DynamicRunner>,
        events: Arc<MemoryEventStore>,
        evidence: Arc<dyn EvidenceStore>,
    ) -> Arc<DeviceRuntime<AndroidDriver, MemoryEventStore>> {
        let driver = Arc::new(fixture_driver(runner));
        driver
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect fixture");
        Arc::new(DeviceRuntime::with_evidence(driver, events, evidence))
    }

    async fn assert_action_event_redacted(
        events: &MemoryEventStore,
        session_id: &SessionId,
        action_name: &str,
        secret: &str,
    ) {
        let recorded = events
            .list_after(session_id, None)
            .await
            .expect("list protected action events");
        let serialized = serde_json::to_string(&recorded).expect("serialize protected events");
        assert!(
            !serialized.contains(secret),
            "protected input escaped into durable events"
        );
        let call = recorded
            .iter()
            .find_map(|event| match &event.payload {
                TestEventPayload::ActionStarted { call } if call.name == action_name => Some(call),
                _ => None,
            })
            .expect("protected ActionStarted event");
        assert!(call.arguments.is_null());
        assert!(call.arguments_redacted);
    }

    fn assert_display_only_observation(
        observation: &devicerail_protocol::Observation,
        omission: ScreenshotOmissionReason,
    ) {
        assert!(observation.screenshot.is_none());
        assert_eq!(observation.screenshot_omission, Some(omission));
        assert_eq!(observation.viewport.width, 100);
        assert_eq!(observation.viewport.height, 200);
        assert_eq!(observation.viewport.scale_factor, 2.0);
        assert_eq!(observation.metadata["android"]["orientation"], "portrait");
    }

    fn call(name: &str, arguments: serde_json::Value) -> ActionCall {
        ActionCall {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            arguments,
        }
    }

    #[test]
    fn schemas_are_closed_bounded_and_keep_system_navigation_out_of_key_press() {
        let definitions = action_definitions();
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            [
                "tap",
                "keyPress",
                "swipe",
                "scroll",
                "inputText",
                "launch",
                "terminate",
                "back",
                "home",
                "recentApps",
                "inputSecret",
            ]
        );
        for definition in &definitions {
            assert_eq!(definition.input_schema["type"], "object");
            assert_eq!(definition.input_schema["additionalProperties"], false);
            assert!(definition.input_schema["required"].is_array());
        }
        let keys = &definitions[1].input_schema["properties"]["key"]["enum"];
        assert_eq!(keys, &json!(AndroidKey::VALUES));
        for system_key in ["back", "home", "recent", "power", "volumeUp"] {
            assert!(
                !keys
                    .as_array()
                    .expect("key enum")
                    .contains(&json!(system_key))
            );
        }
        let text = &definitions[4].input_schema["properties"]["text"];
        assert_eq!(text["maxLength"], AdbInputText::MAX_BYTES);
        assert!(!text["pattern"].as_str().expect("pattern").contains('%'));
        for definition in &definitions[5..=6] {
            let package = &definition.input_schema["properties"]["packageName"];
            assert_eq!(package["minLength"], AndroidPackageName::MIN_BYTES);
            assert_eq!(package["maxLength"], AndroidPackageName::MAX_BYTES);
        }
        assert!(
            definitions[..10]
                .iter()
                .all(|definition| definition.protection == ActionProtection::Standard)
        );
        assert_eq!(definitions[10].protection, ActionProtection::Protected);
    }

    #[test]
    fn parser_accepts_every_json_representation_of_schema_integer() {
        for arguments in [
            serde_json::from_str(r#"{"x":1.0,"y":2e0}"#).expect("decimal integer JSON"),
            serde_json::from_str(r#"{"x":1e0,"y":2.0}"#).expect("exponent integer JSON"),
        ] {
            assert!(ParsedAction::parse("tap", &arguments).is_ok());
        }
        for arguments in [
            serde_json::from_str(r#"{"deltaX":1.0,"deltaY":-2e0}"#)
                .expect("signed decimal integer JSON"),
            serde_json::from_str(r#"{"deltaX":-1e0,"deltaY":2.0}"#)
                .expect("signed exponent integer JSON"),
        ] {
            assert!(ParsedAction::parse("scroll", &arguments).is_ok());
        }

        for (action, arguments) in [
            ("tap", json!({ "x": 1.5, "y": 2 })),
            ("tap", json!({ "x": 4_294_967_296.0_f64, "y": 2 })),
            ("scroll", json!({ "deltaX": 1.5, "deltaY": 2 })),
            ("scroll", json!({ "deltaX": 100_001.0, "deltaY": 2 })),
        ] {
            assert!(
                matches!(
                    ParsedAction::parse(action, &arguments),
                    Err(DriverError::InvalidArguments { .. })
                ),
                "{action} unexpectedly accepted {arguments}"
            );
        }
    }

    #[test]
    fn package_schema_and_parser_accept_exactly_the_same_bounded_language() {
        let definition = action_definitions()
            .into_iter()
            .find(|definition| definition.name == "launch")
            .expect("launch capability");
        let validator = jsonschema::validator_for(&definition.input_schema)
            .expect("compile package action schema");
        let maximum = format!("a.{}", "b".repeat(253));
        let overlong = format!("a.{}", "b".repeat(254));
        let cases = [
            ("a.b".to_owned(), true),
            ("Com.Example_1.App2".to_owned(), true),
            (maximum, true),
            ("a".to_owned(), false),
            ("a.".to_owned(), false),
            ("a..b".to_owned(), false),
            ("1a.b".to_owned(), false),
            ("a.1b".to_owned(), false),
            ("a-b.c".to_owned(), false),
            ("a.$(id)".to_owned(), false),
            ("a.中".to_owned(), false),
            (overlong, false),
        ];
        for (package, expected) in cases {
            let arguments = json!({ "packageName": package });
            assert_eq!(
                validator.is_valid(&arguments),
                expected,
                "schema mismatch for {package:?}"
            );
            assert_eq!(
                ParsedAction::parse("launch", &arguments).is_ok(),
                expected,
                "parser mismatch for {package:?}"
            );
        }
    }

    #[test]
    fn protected_secret_schema_and_consuming_parser_accept_the_same_language() {
        let definition = action_definitions()
            .into_iter()
            .find(|definition| definition.name == "inputSecret")
            .expect("inputSecret capability");
        let validator = jsonschema::validator_for(&definition.input_schema)
            .expect("compile protected secret schema");
        let maximum = "A".repeat(ProtectedAdbInput::MAX_BYTES);
        let overlong = "A".repeat(ProtectedAdbInput::MAX_BYTES + 1);
        let cases = [
            ("A".to_owned(), true),
            ("printable !@#$^&*()[]{}".to_owned(), true),
            ("percent%value".to_owned(), true),
            (maximum, true),
            (String::new(), false),
            ("reserved%svalue".to_owned(), false),
            ("line\nbreak".to_owned(), false),
            ("tab\tvalue".to_owned(), false),
            ("unicode中".to_owned(), false),
            (overlong, false),
        ];
        for (secret, expected) in cases {
            let arguments = json!({ "secret": secret });
            assert_eq!(
                validator.is_valid(&arguments),
                expected,
                "schema mismatch for protected fixture"
            );
            assert_eq!(
                ParsedAction::parse_secret(arguments).is_ok(),
                expected,
                "parser mismatch for protected fixture"
            );
        }
    }

    #[test]
    fn launch_wait_output_accepts_only_completed_success_and_benign_front_warnings() {
        for stdout in [
            "Starting: Intent { act=android.intent.action.MAIN cat=[android.intent.category.LAUNCHER] pkg=com.example }\nStatus: ok\nLaunchState: COLD\nActivity: com.example/.Main\nTotalTime: 11\nWaitTime: 12\nComplete\n",
            "Starting: Intent { act=android.intent.action.MAIN }\nStatus: ok\nActivity: com.example/.Main\nThisTime: 10\nTotalTime: 11\nWaitTime: 12\nComplete\n",
            // The explicit-component form the resolve-then-start launch
            // command produces (command.rs launch_shell_command).
            "Starting: Intent { cmp=com.example/.Main }\nStatus: ok\nLaunchState: COLD\nActivity: com.example/.Main\nTotalTime: 11\nWaitTime: 12\nComplete\n",
            "Status: ok\nLaunchState: UNKNOWN (-1)\nWaitTime: 0\nComplete\n",
            "Warning: Activity not started, intent has been delivered to currently running top-most instance.\r\nStatus: ok\r\nComplete\r\n",
            "Warning: Activity not started, its current task has been brought to the front\nStatus: ok\nComplete\n",
        ] {
            validate_launch_result(stdout, "").expect("valid completed launch output");
        }
        validate_launch_result(
            "Status: ok\nComplete\n",
            "* daemon not running; starting now at tcp:5037\n* daemon started successfully\n",
        )
        .expect("known adb daemon chatter is allowed");

        let failures = [
            (
                "Status: timeout\nComplete\n",
                "",
                "android_app_launch_timeout",
            ),
            (
                "Error: SECRET package missing\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_error",
            ),
            (
                "Failure [SECRET remote failure]\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_error",
            ),
            (
                "Status: ok\nComplete\n",
                "java.lang.SecurityException: SECRET",
                "android_app_launch_error",
            ),
            ("Complete\n", "", "android_app_launch_invalid_result"),
            ("Status: ok\n", "", "android_app_launch_invalid_result"),
            (
                "Complete\nStatus: ok\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nComplete\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nFailure [SECRET remote failure]\nComplete\n",
                "",
                "android_app_launch_error",
            ),
            (
                "Status: ok\nVendorResult: SECRET\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nWaitTime: 12\nActivity: com.example/.Main\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nLaunchState: FUTURE\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nActivity: not-a-component\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Status: ok\nTotalTime: -1\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Starting: arbitrary vendor output\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Warning: Activity not started because the current activity is being kept for the user.\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Warning: Activity not started because intent should be handled by the caller\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
            (
                "Warning: unknown launch condition\nStatus: ok\nComplete\n",
                "",
                "android_app_launch_invalid_result",
            ),
        ];
        for (stdout, stderr, expected_code) in failures {
            let error = validate_launch_result(stdout, stderr).expect_err("launch must fail");
            let DriverError::Platform { code, .. } = &error else {
                panic!("expected sanitized platform failure");
            };
            assert_eq!(code, expected_code);
            assert!(!error.to_string().contains("SECRET"));
            assert!(!format!("{error:?}").contains("SECRET"));
        }
    }

    #[test]
    fn non_launch_mutations_require_empty_output_and_only_known_daemon_chatter() {
        for (stdout, stderr) in [
            ("", ""),
            (" \r\n", ""),
            (
                "",
                "* daemon not running; starting now at tcp:5037\n* daemon started successfully\n",
            ),
        ] {
            validate_empty_mutation_result(stdout, stderr).expect("benign empty mutation output");
        }
        for (stdout, stderr, expected_code, expected_retryable) in [
            ("Error: SECRET", "", "android_mutation_remote_error", false),
            (
                "unexpected success text",
                "",
                "android_mutation_invalid_result",
                true,
            ),
            (
                "",
                "java.lang.SecurityException: SECRET",
                "android_mutation_remote_error",
                false,
            ),
            (
                "",
                "permission denied: SECRET",
                "android_mutation_remote_error",
                false,
            ),
            (
                "",
                "* failed to start daemon: SECRET",
                "android_mutation_invalid_result",
                true,
            ),
        ] {
            let error = validate_empty_mutation_result(stdout, stderr)
                .expect_err("ambiguous legacy-shell output must fail");
            assert!(matches!(
                &error,
                DriverError::Platform { code, retryable }
                    if code == expected_code && *retryable == expected_retryable
            ));
            assert!(!error.to_string().contains("SECRET"));
            assert!(!format!("{error:?}").contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn disconnected_state_wins_over_unknown_action_and_invalid_arguments() {
        let runner = DynamicRunner::new();
        let driver = Arc::new(fixture_driver(&runner));
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let store: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), store);
        let context = session_context(&events, &driver).await;

        for action in [call("unknown", json!(null)), call("tap", json!(null))] {
            assert!(matches!(
                runtime.execute(&context, action).await,
                Err(RuntimeError::Driver(DriverError::NotConnected(id))) if id == *driver.id()
            ));
        }
        assert_eq!(runner.call_count(), 0);
    }

    #[tokio::test]
    async fn input_text_uses_two_evidence_backed_snapshots_without_echoing_text() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let store = file_store(&root);
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        let secret = "Device Rail_1@example.com";

        let result = runtime
            .execute(&context, call("inputText", json!({ "text": secret })))
            .await
            .expect("inputText succeeds");

        assert!(result.before.is_some());
        assert!(result.after.is_some());
        assert_eq!(
            result.evidence.len(),
            1,
            "identical screenshots deduplicate"
        );
        assert_eq!(
            result.output,
            json!({ "accepted": true, "byteLength": secret.len() })
        );
        assert!(!result.output.to_string().contains(secret));
        let action = runner
            .operations()
            .into_iter()
            .find(|operation| matches!(operation, AdbOperation::InputText(_)))
            .expect("inputText adb operation");
        let debug = format!("{action:?}");
        assert!(!debug.contains(secret));
        assert_eq!(
            store.referenced_sessions().await.expect("Session pin"),
            vec![context.session_id]
        );
    }

    #[tokio::test]
    async fn app_lifecycle_and_navigation_actions_return_only_accepted_with_strict_evidence() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        let baseline_screenshots = runner.screenshot_calls();

        for action in [
            call("launch", json!({ "packageName": "com.example.fixture" })),
            call("terminate", json!({ "packageName": "com.example.fixture" })),
            call("back", json!({})),
            call("home", json!({})),
            call("recentApps", json!({})),
        ] {
            let result = runtime
                .execute(&context, action)
                .await
                .expect("new Android action succeeds");
            assert_eq!(result.output, json!({ "accepted": true }));
            assert!(result.before.is_some());
            assert!(result.after.is_some());
            assert!(!result.evidence.is_empty());
        }
        assert_eq!(runner.screenshot_calls() - baseline_screenshots, 10);
    }

    #[tokio::test]
    async fn new_actions_keep_two_device_serial_routes_isolated() {
        let runner = MultiSerialRunner::new();
        let first_serial = AdbSerial::parse("serial-a").expect("first serial");
        let second_serial = AdbSerial::parse("serial-b").expect("second serial");
        let runner_trait: Arc<dyn AdbCommandRunner> = runner.clone();
        let first = Arc::new(AndroidDriver::new(
            AndroidDevice::new(
                descriptor_for(first_serial),
                Arc::clone(&runner_trait),
                AndroidDeviceConfig::default(),
            )
            .expect("first device"),
        ));
        let second = Arc::new(AndroidDriver::new(
            AndroidDevice::new(
                descriptor_for(second_serial),
                runner_trait,
                AndroidDeviceConfig::default(),
            )
            .expect("second device"),
        ));
        let control = ExecutionControl::unbounded();
        let (first_connect, second_connect) =
            tokio::join!(first.connect(&control), second.connect(&control));
        first_connect.expect("connect first");
        second_connect.expect("connect second");

        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let first_runtime = DeviceRuntime::with_evidence(
            Arc::clone(&first),
            Arc::clone(&events),
            Arc::clone(&evidence),
        );
        let second_runtime =
            DeviceRuntime::with_evidence(Arc::clone(&second), Arc::clone(&events), evidence);
        let first_context = session_context(&events, &first).await;
        let second_context = session_context(&events, &second).await;

        for action in [
            call("launch", json!({ "packageName": "com.example.first" })),
            call("back", json!({})),
            call("home", json!({})),
        ] {
            first_runtime
                .execute(&first_context, action)
                .await
                .expect("first device action");
        }
        for action in [
            call("terminate", json!({ "packageName": "com.example.second" })),
            call("recentApps", json!({})),
            call("inputSecret", json!({ "secret": "Serial Secret!" })),
        ] {
            second_runtime
                .execute(&second_context, action)
                .await
                .expect("second device action");
        }

        assert_eq!(
            runner.action_routes(),
            [
                ("serial-a".to_owned(), "back"),
                ("serial-a".to_owned(), "home"),
                ("serial-a".to_owned(), "launch"),
                ("serial-b".to_owned(), "input_secret"),
                ("serial-b".to_owned(), "recent_apps"),
                ("serial-b".to_owned(), "terminate"),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_launch_wait_result_returns_no_action_or_after_payload() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;

        for (stdout, expected_code) in [
            ("Status: timeout\nComplete\n", "android_app_launch_timeout"),
            (
                "Error: SECRET remote failure\nStatus: ok\nComplete\n",
                "android_app_launch_error",
            ),
            (
                "Failure [SECRET remote failure]\nStatus: ok\nComplete\n",
                "android_app_launch_error",
            ),
            (
                "Status: ok\nVendorResult: SECRET\nComplete\n",
                "android_app_launch_invalid_result",
            ),
        ] {
            *runner
                .launch_stdout_once
                .lock()
                .expect("launch output lock") = Some(stdout.to_owned());
            let screenshots = runner.screenshot_calls();
            let error = runtime
                .execute(
                    &context,
                    call("launch", json!({ "packageName": "com.example.fixture" })),
                )
                .await
                .expect_err("invalid wait result must fail the Action");
            let RuntimeError::Driver(ref driver_error @ DriverError::Platform { ref code, .. }) =
                error
            else {
                panic!("expected launch platform failure");
            };
            assert_eq!(code, expected_code);
            assert_eq!(
                runner.screenshot_calls(),
                screenshots + 1,
                "failed launch has a before capture but no after capture"
            );
            assert!(!driver_error.to_string().contains("SECRET"));
            assert!(!format!("{driver_error:?}").contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn unsafe_text_and_zero_scroll_fail_before_any_snapshot_or_adb_action() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        let baseline = runner.call_count();

        for action in [
            call("inputText", json!({ "text": "unsafe;command" })),
            call("inputText", json!({ "text": "caller%value" })),
            call("scroll", json!({ "deltaX": 0, "deltaY": 0 })),
        ] {
            assert!(matches!(
                runtime.execute(&context, action).await,
                Err(RuntimeError::Driver(DriverError::InvalidArguments { .. }))
            ));
        }
        assert_eq!(runner.call_count(), baseline);
    }

    #[tokio::test]
    async fn platform_process_failure_is_classified_without_stderr_or_input_text() {
        let runner = DynamicRunner::new();
        runner.fail_action_once.store(true, Ordering::SeqCst);
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        let secret = "Never Echo This";

        let error = runtime
            .execute(&context, call("inputText", json!({ "text": secret })))
            .await
            .expect_err("fixture action fails");
        let RuntimeError::Driver(ref driver_error @ DriverError::Platform { ref code, .. }) = error
        else {
            panic!("expected platform error");
        };
        assert_eq!(code, "android_adb_process_failed");
        let public = driver_error.to_error_info();
        assert!(!public.message.contains(secret));
        assert!(!public.message.contains("PRIVATE"));
        assert_eq!(
            public.details.expect("platform details")["platformCode"],
            code.as_str()
        );
        assert!(
            runtime.driver().device_info().await.connected,
            "generic process failures must not invalidate transport state"
        );
        let calls = runner.call_count();
        runtime
            .driver()
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("generic process failure preserves idempotent connect");
        assert_eq!(runner.call_count(), calls);
    }

    #[tokio::test]
    async fn action_and_observe_connectivity_failures_force_a_real_connect_recovery() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;

        runner
            .fail_connectivity_action_once
            .store(true, Ordering::SeqCst);
        assert!(matches!(
            runtime
                .execute(&context, call("tap", json!({ "x": 10, "y": 20 })))
                .await,
            Err(RuntimeError::Driver(DriverError::Platform { ref code, .. }))
                if code == "android_device_offline"
        ));
        assert!(!runtime.driver().device_info().await.connected);
        let invalidated_calls = runner.call_count();
        assert!(matches!(
            runtime
                .execute(&context, call("keyPress", json!({ "key": "enter" })))
                .await,
            Err(RuntimeError::Driver(DriverError::NotConnected(_)))
        ));
        assert_eq!(runner.call_count(), invalidated_calls);
        let before_action_recovery = runner.call_count();
        let recovered = runtime
            .driver()
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("action connectivity failure requires a successful probe");
        assert!(recovered.connected);
        assert_eq!(runner.call_count() - before_action_recovery, 5);

        runner
            .fail_connectivity_capture_once
            .store(true, Ordering::SeqCst);
        assert!(matches!(
            runtime.observe(&context).await,
            Err(RuntimeError::Driver(DriverError::Platform { ref code, .. }))
                if code == "android_device_offline"
        ));
        assert!(!runtime.driver().device_info().await.connected);
        let invalidated_calls = runner.call_count();
        assert!(matches!(
            runtime.observe(&context).await,
            Err(RuntimeError::Driver(DriverError::NotConnected(_)))
        ));
        assert_eq!(runner.call_count(), invalidated_calls);
        let before_observe_recovery = runner.call_count();
        let recovered = runtime
            .driver()
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("observation connectivity failure requires a successful probe");
        assert!(recovered.connected);
        assert_eq!(runner.call_count() - before_observe_recovery, 5);
        assert!(runtime.driver().device_info().await.connected);
    }

    #[tokio::test]
    async fn coordinates_use_captured_viewport_and_after_capture_failure_is_not_success() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        let baseline = runner.call_count();

        assert!(matches!(
            runtime
                .execute(&context, call("tap", json!({ "x": 100, "y": 20 })))
                .await,
            Err(RuntimeError::Driver(DriverError::InvalidArguments { .. }))
        ));
        assert_eq!(
            runner.call_count() - baseline,
            3,
            "before snapshot is required"
        );
        assert_eq!(runner.action_calls.load(Ordering::SeqCst), 0);

        runner.fail_after_size_once.store(true, Ordering::SeqCst);
        assert!(matches!(
            runtime
                .execute(&context, call("tap", json!({ "x": 99, "y": 199 })))
                .await,
            Err(RuntimeError::Driver(DriverError::Platform { ref code, .. }))
                if code == "android_adb_process_failed"
        ));
        assert_eq!(runner.action_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exclusive_gate_serializes_actions_and_blocks_observe_until_after_snapshot() {
        let runner = DynamicRunner::new();
        runner.enable_action_blocking();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let first_context = session_context(&events, runtime.driver()).await;
        let second_context = session_context(&events, runtime.driver()).await;
        let observe_context = session_context(&events, runtime.driver()).await;

        let first_runtime = Arc::clone(&runtime);
        let first = tokio::spawn(async move {
            first_runtime
                .execute(&first_context, call("tap", json!({ "x": 10, "y": 20 })))
                .await
        });
        runner.wait_for_action_calls(1).await;
        assert_eq!(
            runner.screenshot_calls(),
            1,
            "first before snapshot completed"
        );

        let second_runtime = Arc::clone(&runtime);
        let second = tokio::spawn(async move {
            second_runtime
                .execute(&second_context, call("keyPress", json!({ "key": "enter" })))
                .await
        });
        let observe_runtime = Arc::clone(&runtime);
        let observe = tokio::spawn(async move { observe_runtime.observe(&observe_context).await });
        tokio::task::yield_now().await;
        assert_eq!(runner.action_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner.screenshot_calls(), 1, "observe cannot cross Action");

        runner.release_one_action();
        first.await.expect("first task").expect("first action");
        runner.wait_for_action_calls(2).await;
        runner.release_one_action();
        second.await.expect("second task").expect("second action");
        observe.await.expect("observe task").expect("observation");
        assert_eq!(runner.max_active_actions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn action_gate_wait_honors_request_cancellation_and_deadline() {
        let runner = DynamicRunner::new();
        runner.enable_action_blocking();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let holder_context = session_context(&events, runtime.driver()).await;

        let holder_runtime = Arc::clone(&runtime);
        let holder = tokio::spawn(async move {
            holder_runtime
                .execute(&holder_context, call("tap", json!({ "x": 10, "y": 20 })))
                .await
        });
        runner.wait_for_action_calls(1).await;

        let cancelled_context = session_context(&events, runtime.driver()).await;
        let (controller, cancelled_control) = ExecutionController::new();
        let cancelled_context = cancelled_context.with_control(cancelled_control);
        let cancelled_runtime = Arc::clone(&runtime);
        let cancelled = tokio::spawn(async move {
            cancelled_runtime
                .execute(
                    &cancelled_context,
                    call("keyPress", json!({ "key": "enter" })),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(controller.cancel(CancellationReason::Requested));
        assert!(matches!(
            cancelled.await.expect("cancelled task"),
            Err(RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            })
        ));

        let timed_context = session_context(&events, runtime.driver())
            .await
            .with_control(ExecutionController::with_timeout(5, TimeoutScope::Request).1);
        assert!(matches!(
            runtime
                .execute(&timed_context, call("keyPress", json!({ "key": "tab" })))
                .await,
            Err(RuntimeError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: 5,
            })
        ));
        assert_eq!(
            runner.action_calls.load(Ordering::SeqCst),
            1,
            "neither waiter may reach adb"
        );

        runner.release_one_action();
        holder.await.expect("holder task").expect("holder action");
    }

    #[tokio::test]
    async fn cancellation_and_deadline_during_mutation_publish_no_after_and_release_gate() {
        let runner = DynamicRunner::new();
        runner.enable_action_blocking();
        let events = Arc::new(MemoryEventStore::default());
        let root = TempDir::new().expect("temporary evidence root");
        let evidence: Arc<dyn EvidenceStore> = file_store(&root);
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;

        let cancelled_context = session_context(&events, runtime.driver()).await;
        let (controller, cancelled_control) = ExecutionController::new();
        let cancelled_context = cancelled_context.with_control(cancelled_control);
        let cancelled_runtime = Arc::clone(&runtime);
        let cancelled = tokio::spawn(async move {
            cancelled_runtime
                .execute(&cancelled_context, call("tap", json!({ "x": 10, "y": 20 })))
                .await
        });
        tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, runner.wait_for_action_calls(1))
            .await
            .expect("cancelled action reaches the runner");
        assert_eq!(runner.screenshot_calls(), 1);
        assert!(controller.cancel(CancellationReason::Requested));
        let cancelled = tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, cancelled)
            .await
            .expect("cancelled mutation finishes")
            .expect("cancelled mutation task");
        assert!(matches!(
            cancelled,
            Err(RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            })
        ));
        assert_eq!(
            runner.screenshot_calls(),
            1,
            "cancelled action has no after"
        );
        assert_eq!(runner.active_actions.load(Ordering::SeqCst), 0);
        assert!(
            tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, runtime.driver().device_info())
                .await
                .expect("cancelled mutation releases the device gate")
                .connected
        );

        let timed_context = session_context(&events, runtime.driver())
            .await
            .with_control(
                ExecutionController::with_timeout(
                    TEST_BLOCKED_MUTATION_TIMEOUT_MS,
                    TimeoutScope::Request,
                )
                .1,
            );
        let timed_runtime = Arc::clone(&runtime);
        let timed = tokio::spawn(async move {
            timed_runtime
                .execute(&timed_context, call("keyPress", json!({ "key": "enter" })))
                .await
        });
        tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, runner.wait_for_action_calls(2))
            .await
            .expect("timed mutation reaches the runner");
        let timed = tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, timed)
            .await
            .expect("timed mutation finishes")
            .expect("timed mutation task");
        assert!(matches!(
            timed,
            Err(RuntimeError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: TEST_BLOCKED_MUTATION_TIMEOUT_MS,
            })
        ));
        assert_eq!(runner.screenshot_calls(), 2, "timed action has only before");
        assert_eq!(runner.active_actions.load(Ordering::SeqCst), 0);
        assert!(
            tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, runtime.driver().device_info())
                .await
                .expect("timed mutation releases the device gate")
                .connected
        );

        runner.disable_action_blocking();
        let recovery_context = session_context(&events, runtime.driver()).await;
        let recovered = tokio::time::timeout(
            TEST_ASYNC_STAGE_TIMEOUT,
            runtime.execute(&recovery_context, call("keyPress", json!({ "key": "tab" }))),
        )
        .await
        .expect("recovery mutation finishes")
        .expect("gate remains reusable after cancellation and timeout");
        assert!(recovered.before.is_some());
        assert!(recovered.after.is_some());
        assert_eq!(runner.screenshot_calls(), 4);
    }

    #[tokio::test]
    async fn protected_input_is_redacted_and_uses_only_display_observations() {
        const SECRET: &str = "SENTINEL_SECRET_$()[]{}";
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let store = TemporaryEvidenceStore::create_concrete();
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        let action = call("inputSecret", json!({ "secret": SECRET }));

        assert!(!format!("{action:?}").contains(SECRET));
        let result = runtime
            .execute(&context, action)
            .await
            .expect("protected input succeeds");

        assert_eq!(result.output, json!({ "accepted": true }));
        assert!(result.evidence.is_empty());
        assert_display_only_observation(
            result.before.as_ref().expect("protected before"),
            ScreenshotOmissionReason::ProtectedAction,
        );
        assert_display_only_observation(
            result.after.as_ref().expect("protected after"),
            ScreenshotOmissionReason::ProtectedAction,
        );
        assert_eq!(runner.screenshot_calls(), 0);
        assert_eq!(store.put_count(), 0);
        assert!(
            store
                .referenced_sessions()
                .await
                .expect("protected evidence sessions")
                .is_empty()
        );
        let retained_commands = format!("{:?}", runner.calls.lock().expect("calls lock"));
        assert!(!retained_commands.contains(SECRET));
        assert_eq!(
            runner
                .operations()
                .iter()
                .filter(|operation| {
                    matches!(
                        operation,
                        AdbOperation::WindowSize | AdbOperation::WindowDensity
                    )
                })
                .count(),
            4
        );
        assert_action_event_redacted(&events, &context.session_id, "inputSecret", SECRET).await;
    }

    #[tokio::test]
    async fn global_screenshot_omission_applies_to_observe_and_standard_actions() {
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let store = TemporaryEvidenceStore::create_concrete();
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let driver = Arc::new(fixture_driver(&runner));
        driver
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect fixture");
        let runtime = DeviceRuntime::with_evidence(driver, Arc::clone(&events), evidence)
            .with_screenshot_policy(ScreenshotPolicy::Omit);
        let context = session_context(&events, runtime.driver()).await;

        let observation = runtime
            .observe(&context)
            .await
            .expect("display-only observe");
        assert_display_only_observation(&observation, ScreenshotOmissionReason::Policy);
        let result = runtime
            .execute(&context, call("tap", json!({ "x": 10, "y": 20 })))
            .await
            .expect("display-only standard action");
        assert!(result.evidence.is_empty());
        assert_display_only_observation(
            result.before.as_ref().expect("policy before"),
            ScreenshotOmissionReason::Policy,
        );
        assert_display_only_observation(
            result.after.as_ref().expect("policy after"),
            ScreenshotOmissionReason::Policy,
        );
        assert_eq!(runner.screenshot_calls(), 0);
        assert_eq!(store.put_count(), 0);
        assert!(
            store
                .referenced_sessions()
                .await
                .expect("policy evidence sessions")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn protected_invalid_unknown_and_platform_failures_never_disclose_arguments() {
        const INVALID_SECRET: &str = "INVALID_SENTINEL%s";
        const UNKNOWN_SECRET: &str = "UNKNOWN_SENTINEL";
        const PLATFORM_SECRET: &str = "PLATFORM_SENTINEL";
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let store = TemporaryEvidenceStore::create_concrete();
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;

        let invalid_context = session_context(&events, runtime.driver()).await;
        let invalid = runtime
            .execute(
                &invalid_context,
                call("inputSecret", json!({ "secret": INVALID_SECRET })),
            )
            .await
            .expect_err("reserved percent-s is invalid");
        assert!(matches!(
            &invalid,
            RuntimeError::Driver(DriverError::InvalidArguments { .. })
        ));
        assert!(!format!("{invalid:?}").contains(INVALID_SECRET));
        assert_action_event_redacted(
            &events,
            &invalid_context.session_id,
            "inputSecret",
            INVALID_SECRET,
        )
        .await;

        let unknown_context = session_context(&events, runtime.driver()).await;
        let unknown = runtime
            .execute(
                &unknown_context,
                call("futureSecret", json!({ "secret": UNKNOWN_SECRET })),
            )
            .await
            .expect_err("unknown action is rejected");
        assert!(matches!(
            &unknown,
            RuntimeError::Driver(DriverError::UnknownAction(name)) if name == "futureSecret"
        ));
        assert!(!format!("{unknown:?}").contains(UNKNOWN_SECRET));
        assert_action_event_redacted(
            &events,
            &unknown_context.session_id,
            "futureSecret",
            UNKNOWN_SECRET,
        )
        .await;

        runner.fail_action_once.store(true, Ordering::SeqCst);
        let platform_context = session_context(&events, runtime.driver()).await;
        let platform = runtime
            .execute(
                &platform_context,
                call("inputSecret", json!({ "secret": PLATFORM_SECRET })),
            )
            .await
            .expect_err("protected platform failure");
        assert!(matches!(
            &platform,
            RuntimeError::Driver(DriverError::Platform { code, .. })
                if code == "android_adb_protected_operation_failed"
        ));
        let platform_debug = format!("{platform:?}");
        assert!(!platform_debug.contains(PLATFORM_SECRET));
        assert!(!platform_debug.contains("/system/bin/sh"));
        assert!(!platform_debug.contains("PRIVATE"));
        assert_action_event_redacted(
            &events,
            &platform_context.session_id,
            "inputSecret",
            PLATFORM_SECRET,
        )
        .await;

        assert_eq!(runner.screenshot_calls(), 0);
        assert_eq!(store.put_count(), 0);
        assert_eq!(runner.action_calls.load(Ordering::SeqCst), 1);
        assert!(runtime.driver().device_info().await.connected);
    }

    #[tokio::test]
    async fn protected_offline_failure_is_classified_before_redaction_and_invalidates_cache() {
        const SECRET: &str = "OFFLINE_SENTINEL";
        let runner = DynamicRunner::new();
        let events = Arc::new(MemoryEventStore::default());
        let store = TemporaryEvidenceStore::create_concrete();
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;
        let context = session_context(&events, runtime.driver()).await;
        runner
            .fail_connectivity_action_once
            .store(true, Ordering::SeqCst);

        let error = runtime
            .execute(&context, call("inputSecret", json!({ "secret": SECRET })))
            .await
            .expect_err("protected offline failure");
        assert!(matches!(
            &error,
            RuntimeError::Driver(DriverError::Platform { code, .. })
                if code == "android_device_offline"
        ));
        assert!(!format!("{error:?}").contains(SECRET));
        assert!(!runtime.driver().device_info().await.connected);
        let invalidated_calls = runner.call_count();
        assert!(matches!(
            runtime
                .execute(
                    &context,
                    call("inputSecret", json!({ "secret": "NEXT_SECRET" }))
                )
                .await,
            Err(RuntimeError::Driver(DriverError::NotConnected(_)))
        ));
        assert_eq!(runner.call_count(), invalidated_calls);
        assert_eq!(runner.screenshot_calls(), 0);
        assert_eq!(store.put_count(), 0);
        assert_action_event_redacted(&events, &context.session_id, "inputSecret", SECRET).await;
    }

    #[tokio::test]
    async fn protected_cancellation_and_timeout_publish_no_after_and_release_gate() {
        const CANCELLED_SECRET: &str = "CANCELLED_SENTINEL";
        const TIMED_SECRET: &str = "TIMED_SENTINEL";
        let runner = DynamicRunner::new();
        runner.enable_action_blocking();
        let events = Arc::new(MemoryEventStore::default());
        let store = TemporaryEvidenceStore::create_concrete();
        let evidence: Arc<dyn EvidenceStore> = store.clone();
        let runtime = connected_runtime(&runner, Arc::clone(&events), evidence).await;

        let cancelled_context = session_context(&events, runtime.driver()).await;
        let cancelled_session = cancelled_context.session_id.clone();
        let (controller, cancelled_control) = ExecutionController::new();
        let cancelled_context = cancelled_context.with_control(cancelled_control);
        let cancelled_runtime = Arc::clone(&runtime);
        let cancelled = tokio::spawn(async move {
            cancelled_runtime
                .execute(
                    &cancelled_context,
                    call("inputSecret", json!({ "secret": CANCELLED_SECRET })),
                )
                .await
        });
        tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, runner.wait_for_action_calls(1))
            .await
            .expect("cancelled protected action reaches the runner");
        assert!(controller.cancel(CancellationReason::Requested));
        let cancelled = tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, cancelled)
            .await
            .expect("cancelled protected mutation finishes")
            .expect("cancelled protected task");
        assert!(matches!(
            cancelled,
            Err(RuntimeError::Cancelled {
                reason: CancellationReason::Requested
            })
        ));
        assert_eq!(runner.active_actions.load(Ordering::SeqCst), 0);

        let timed_context = session_context(&events, runtime.driver())
            .await
            .with_control(
                ExecutionController::with_timeout(
                    TEST_BLOCKED_MUTATION_TIMEOUT_MS,
                    TimeoutScope::Request,
                )
                .1,
            );
        let timed_session = timed_context.session_id.clone();
        let timed_runtime = Arc::clone(&runtime);
        let timed = tokio::spawn(async move {
            timed_runtime
                .execute(
                    &timed_context,
                    call("inputSecret", json!({ "secret": TIMED_SECRET })),
                )
                .await
        });
        tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, runner.wait_for_action_calls(2))
            .await
            .expect("timed protected action reaches the runner");
        let timed = tokio::time::timeout(TEST_ASYNC_STAGE_TIMEOUT, timed)
            .await
            .expect("timed protected mutation finishes")
            .expect("timed protected task");
        assert!(matches!(
            timed,
            Err(RuntimeError::TimedOut {
                scope: TimeoutScope::Request,
                timeout_ms: TEST_BLOCKED_MUTATION_TIMEOUT_MS,
            })
        ));
        assert_eq!(runner.active_actions.load(Ordering::SeqCst), 0);

        runner.disable_action_blocking();
        let recovery_context = session_context(&events, runtime.driver()).await;
        let recovered = tokio::time::timeout(
            TEST_ASYNC_STAGE_TIMEOUT,
            runtime.execute(
                &recovery_context,
                call("inputSecret", json!({ "secret": "RECOVERY_SECRET" })),
            ),
        )
        .await
        .expect("protected recovery mutation finishes")
        .expect("protected gate remains reusable");
        assert_display_only_observation(
            recovered.before.as_ref().expect("recovery before"),
            ScreenshotOmissionReason::ProtectedAction,
        );
        assert_display_only_observation(
            recovered.after.as_ref().expect("recovery after"),
            ScreenshotOmissionReason::ProtectedAction,
        );
        assert_eq!(runner.screenshot_calls(), 0);
        assert_eq!(store.put_count(), 0);
        assert_eq!(runner.action_calls.load(Ordering::SeqCst), 3);
        assert_action_event_redacted(&events, &cancelled_session, "inputSecret", CANCELLED_SECRET)
            .await;
        assert_action_event_redacted(&events, &timed_session, "inputSecret", TIMED_SECRET).await;
    }

    fn conformance_call(action: &ActionDefinition) -> Result<ActionCall, String> {
        let arguments = match action.name.as_str() {
            "tap" => json!({ "x": 10, "y": 20 }),
            "keyPress" => json!({ "key": "enter" }),
            "swipe" => json!({
                "startX": 10,
                "startY": 20,
                "endX": 30,
                "endY": 40,
                "durationMs": 500
            }),
            "scroll" => json!({ "deltaX": 0, "deltaY": 100 }),
            "inputText" => json!({ "text": "Device Rail_1" }),
            "launch" | "terminate" => json!({ "packageName": "com.example.fixture" }),
            "back" | "home" | "recentApps" => json!({}),
            "inputSecret" => json!({ "secret": "Conformance Secret!" }),
            name => return Err(format!("no Android conformance fixture for `{name}`")),
        };
        Ok(ActionCall {
            id: Uuid::new_v4(),
            name: action.name.clone(),
            arguments,
        })
    }

    struct TemporaryEvidenceStore {
        store: FileEvidenceStore,
        _root: TempDir,
        puts: AtomicUsize,
    }

    impl TemporaryEvidenceStore {
        fn create() -> Arc<dyn EvidenceStore> {
            Self::create_concrete()
        }

        fn create_concrete() -> Arc<Self> {
            let root = TempDir::new().expect("temporary conformance Evidence root");
            let store = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("conformance Evidence Store");
            Arc::new(Self {
                store,
                _root: root,
                puts: AtomicUsize::new(0),
            })
        }

        fn put_count(&self) -> usize {
            self.puts.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EvidenceStore for TemporaryEvidenceStore {
        async fn put(
            &self,
            request: PutEvidence,
            input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.store.put(request, input).await
        }

        async fn attach(
            &self,
            session_id: &SessionId,
            asset: &devicerail_protocol::AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            self.store.attach(session_id, asset).await
        }

        async fn verify_session_reference(
            &self,
            session_id: &SessionId,
            asset: &devicerail_protocol::AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            self.store.verify_session_reference(session_id, asset).await
        }

        async fn open(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            self.store.open(digest).await
        }

        async fn metadata(&self, digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            self.store.metadata(digest).await
        }

        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            self.store.referenced_sessions().await
        }

        async fn release_session(
            &self,
            session_id: &SessionId,
            released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            self.store.release_session(session_id, released_at_ms).await
        }

        async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
            self.store.gc(policy).await
        }
    }

    devicerail_core::driver_conformance_test!(
        conforms_to_shared_driver_contract_with_session_evidence,
        || fixture_driver(&DynamicRunner::new()),
        conformance_call,
        TemporaryEvidenceStore::create(),
    );
}
