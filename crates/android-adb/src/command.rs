use std::{
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::ExecutionControl;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

use crate::{AdbSerial, AndroidAdbError, AndroidAdbResult};

const TEXT_STDOUT_LIMIT: usize = 1024 * 1024;
pub(crate) const SCREENSHOT_STDOUT_LIMIT: usize = 32 * 1024 * 1024;
const STDERR_TAIL_LIMIT: usize = 64 * 1024;
// The public swipe contract permits 60 seconds. Keep enough process-level
// headroom for adb setup/teardown so the default runner does not contradict a
// schema-valid action before Core's request/action budget can decide it.
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(65);
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROTECTED_INPUT_SCRIPT: &str =
    "IFS= read -r DEVICERAIL_SECRET && input text \"$DEVICERAIL_SECRET\"";

/// A system property that DeviceRail is allowed to request from Android.
///
/// Keeping this list typed prevents callers from turning the command runner
/// into an arbitrary remote shell boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdbProperty {
    BootCompleted,
    ProductManufacturer,
    ProductModel,
    ReleaseVersion,
}

impl AdbProperty {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BootCompleted => "sys.boot_completed",
            Self::ProductManufacturer => "ro.product.manufacturer",
            Self::ProductModel => "ro.product.model",
            Self::ReleaseVersion => "ro.build.version.release",
        }
    }
}

/// A closed set of host and per-device adb operations.
///
/// Discovery, lifecycle, observation, and each advertised Action use explicit
/// variants. In particular, there is no generic `shell(Vec<String>)` escape
/// hatch. The sole caller-originated string is wrapped by `AdbInputText` after
/// strict remote-shell-safe validation and encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AndroidKey {
    Enter,
    Tab,
    Escape,
    Delete,
    Space,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

impl AndroidKey {
    pub(crate) const VALUES: [&'static str; 9] = [
        "enter",
        "tab",
        "escape",
        "delete",
        "space",
        "arrowUp",
        "arrowDown",
        "arrowLeft",
        "arrowRight",
    ];

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "enter" => Some(Self::Enter),
            "tab" => Some(Self::Tab),
            "escape" => Some(Self::Escape),
            "delete" => Some(Self::Delete),
            "space" => Some(Self::Space),
            "arrowUp" => Some(Self::ArrowUp),
            "arrowDown" => Some(Self::ArrowDown),
            "arrowLeft" => Some(Self::ArrowLeft),
            "arrowRight" => Some(Self::ArrowRight),
            _ => None,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Escape => "escape",
            Self::Delete => "delete",
            Self::Space => "space",
            Self::ArrowUp => "arrowUp",
            Self::ArrowDown => "arrowDown",
            Self::ArrowLeft => "arrowLeft",
            Self::ArrowRight => "arrowRight",
        }
    }

    const fn keycode(self) -> &'static str {
        match self {
            Self::Enter => "KEYCODE_ENTER",
            Self::Tab => "KEYCODE_TAB",
            Self::Escape => "KEYCODE_ESCAPE",
            Self::Delete => "KEYCODE_DEL",
            Self::Space => "KEYCODE_SPACE",
            Self::ArrowUp => "KEYCODE_DPAD_UP",
            Self::ArrowDown => "KEYCODE_DPAD_DOWN",
            Self::ArrowLeft => "KEYCODE_DPAD_LEFT",
            Self::ArrowRight => "KEYCODE_DPAD_RIGHT",
        }
    }
}

/// Pre-encoded input for Android's `input text` command.
///
/// The constructor accepts only a deliberately small remote-shell-safe ASCII
/// alphabet. In particular, caller-provided `%` is rejected and only an
/// allowed space is encoded as the Android input command's `%s` escape.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AdbInputText {
    encoded: String,
    byte_len: usize,
}

/// Validated Android application id used by the two package-scoped commands.
///
/// Android's documented application-id grammar also provides a strict shell
/// token: at least two dot-separated segments, each beginning with an ASCII
/// letter and continuing with ASCII letters, digits, or underscore. Length is
/// bounded independently because ADB itself does not provide a useful wire
/// limit for callers.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AndroidPackageName {
    value: String,
}

impl AndroidPackageName {
    pub(crate) const MIN_BYTES: usize = 3;
    pub(crate) const MAX_BYTES: usize = 255;

    pub(crate) fn parse(value: &str) -> AndroidAdbResult<Self> {
        let valid_length = (Self::MIN_BYTES..=Self::MAX_BYTES).contains(&value.len());
        let mut segments = value.split('.');
        let first = segments.next();
        let mut segment_count = 0_usize;
        let valid_segments = first.into_iter().chain(segments).all(|segment| {
            segment_count += 1;
            let mut bytes = segment.bytes();
            bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        });
        if !valid_length || !value.is_ascii() || segment_count < 2 || !valid_segments {
            return Err(AndroidAdbError::InvalidValue {
                field: "packageName",
                value: "package name does not satisfy the bounded Android application-id grammar"
                    .to_owned(),
            });
        }
        Ok(Self {
            value: value.to_owned(),
        })
    }

    fn as_str(&self) -> &str {
        &self.value
    }

    #[cfg(test)]
    pub(crate) fn byte_len(&self) -> usize {
        self.value.len()
    }
}

impl fmt::Debug for AndroidPackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AndroidPackageName")
            .field("byte_len", &self.value.len())
            .finish_non_exhaustive()
    }
}

/// One operation-scoped protected stdin value.
///
/// The buffer is intentionally not cloneable. Its Debug representation is
/// constant and Drop performs a best-effort overwrite before releasing the
/// allocation. The value is never placed in an ADB argument or error.
pub(crate) struct ProtectedAdbInput {
    bytes: Vec<u8>,
}

impl ProtectedAdbInput {
    pub(crate) const MIN_BYTES: usize = 1;
    pub(crate) const MAX_BYTES: usize = 1024;

    pub(crate) fn parse(bytes: Vec<u8>) -> AndroidAdbResult<Self> {
        let input = Self { bytes };
        let valid_length = (Self::MIN_BYTES..=Self::MAX_BYTES).contains(&input.bytes.len());
        let printable = input.bytes.iter().all(|byte| (0x20..=0x7e).contains(byte));
        let preserves_text = !input.bytes.windows(2).any(|window| window == b"%s");
        if valid_length && printable && preserves_text {
            Ok(input)
        } else {
            Err(AndroidAdbError::InvalidValue {
                field: "inputSecret.secret",
                value: "secret does not satisfy the protected printable-ASCII contract".to_owned(),
            })
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for ProtectedAdbInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl Drop for ProtectedAdbInput {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

impl AdbInputText {
    pub(crate) const MAX_BYTES: usize = 1024;

    pub(crate) fn parse(value: &str) -> AndroidAdbResult<Self> {
        if value.is_empty() || value.len() > Self::MAX_BYTES {
            return Err(AndroidAdbError::InvalidValue {
                field: "inputText.text",
                value: "text length is outside 1..=1024 bytes".to_owned(),
            });
        }
        if !value.bytes().all(is_safe_input_text_byte) {
            return Err(AndroidAdbError::InvalidValue {
                field: "inputText.text",
                value: "text contains a character outside the safe ASCII allowlist".to_owned(),
            });
        }

        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            if byte == b' ' {
                encoded.push_str("%s");
            } else {
                encoded.push(char::from(byte));
            }
        }
        Ok(Self {
            encoded,
            byte_len: value.len(),
        })
    }

    pub(crate) const fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn encoded(&self) -> &str {
        &self.encoded
    }
}

impl fmt::Debug for AdbInputText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdbInputText")
            .field("byte_len", &self.byte_len)
            .finish_non_exhaustive()
    }
}

const fn is_safe_input_text_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b' ' | b'.' | b',' | b'_' | b'@' | b'+' | b'-' | b'=' | b':' | b'/'
        )
}

#[derive(Clone, PartialEq, Eq)]
pub enum AdbOperation {
    DevicesLong,
    GetState,
    Reconnect,
    WaitForDevice,
    GetProperty(AdbProperty),
    CaptureScreenshot,
    WindowSize,
    WindowDensity,
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
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
    },
    InputText(AdbInputText),
    Launch(AndroidPackageName),
    Terminate(AndroidPackageName),
    Back,
    Home,
    RecentApps,
    InputSecret,
}

impl AdbOperation {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DevicesLong => "devices_long",
            Self::GetState => "get_state",
            Self::Reconnect => "reconnect",
            Self::WaitForDevice => "wait_for_device",
            Self::GetProperty(_) => "get_property",
            Self::CaptureScreenshot => "capture_screenshot",
            Self::WindowSize => "window_size",
            Self::WindowDensity => "window_density",
            Self::Tap { .. } => "tap",
            Self::KeyPress(_) => "key_press",
            Self::Swipe { .. } => "swipe",
            Self::Scroll { .. } => "scroll",
            Self::InputText(_) => "input_text",
            Self::Launch(_) => "launch",
            Self::Terminate(_) => "terminate",
            Self::Back => "back",
            Self::Home => "home",
            Self::RecentApps => "recent_apps",
            Self::InputSecret => "input_secret",
        }
    }

    const fn requires_device(&self) -> bool {
        !matches!(self, Self::DevicesLong)
    }

    const fn requires_protected_input(&self) -> bool {
        matches!(self, Self::InputSecret)
    }

    fn append_arguments(&self, arguments: &mut Vec<OsString>) {
        match self {
            Self::DevicesLong => push_args(arguments, &["devices", "-l"]),
            Self::GetState => push_args(arguments, &["get-state"]),
            Self::Reconnect => push_args(arguments, &["reconnect"]),
            Self::WaitForDevice => push_args(arguments, &["wait-for-device"]),
            Self::GetProperty(property) => {
                push_args(arguments, &["shell", "getprop", property.as_str()]);
            }
            Self::CaptureScreenshot => {
                push_args(arguments, &["exec-out", "screencap", "-p"]);
            }
            Self::WindowSize => push_args(arguments, &["shell", "wm", "size"]),
            Self::WindowDensity => push_args(arguments, &["shell", "wm", "density"]),
            Self::Tap { x, y } => {
                push_args(arguments, &["shell", "input", "tap"]);
                arguments.extend([x.to_string().into(), y.to_string().into()]);
            }
            Self::KeyPress(key) => {
                push_args(arguments, &["shell", "input", "keyevent", key.keycode()]);
            }
            Self::Swipe {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            }
            | Self::Scroll {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => {
                push_args(arguments, &["shell", "input", "swipe"]);
                arguments.extend(
                    [start_x, start_y, end_x, end_y, duration_ms]
                        .map(u32::to_string)
                        .map(OsString::from),
                );
            }
            Self::InputText(text) => {
                push_args(arguments, &["shell", "input", "text"]);
                arguments.push(text.encoded().into());
            }
            Self::Launch(package) => {
                push_args(
                    arguments,
                    &[
                        "shell",
                        "am",
                        "start",
                        "-W",
                        "--user",
                        "current",
                        "-a",
                        "android.intent.action.MAIN",
                        "-c",
                        "android.intent.category.LAUNCHER",
                        "-p",
                    ],
                );
                arguments.push(package.as_str().into());
            }
            Self::Terminate(package) => {
                push_args(
                    arguments,
                    &["shell", "am", "force-stop", "--user", "current"],
                );
                arguments.push(package.as_str().into());
            }
            Self::Back => {
                push_args(arguments, &["shell", "input", "keyevent", "KEYCODE_BACK"]);
            }
            Self::Home => {
                push_args(arguments, &["shell", "input", "keyevent", "KEYCODE_HOME"]);
            }
            Self::RecentApps => {
                push_args(
                    arguments,
                    &["shell", "input", "keyevent", "KEYCODE_APP_SWITCH"],
                );
            }
            Self::InputSecret => {
                push_args(arguments, &["shell", "-T", PROTECTED_INPUT_SCRIPT]);
            }
        }
    }

    const fn stdout_limit(&self) -> usize {
        match self {
            Self::CaptureScreenshot => SCREENSHOT_STDOUT_LIMIT,
            _ => TEXT_STDOUT_LIMIT,
        }
    }

    const fn returns_binary(&self) -> bool {
        matches!(self, Self::CaptureScreenshot)
    }
}

impl fmt::Debug for AdbOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DevicesLong => formatter.write_str("DevicesLong"),
            Self::GetState => formatter.write_str("GetState"),
            Self::Reconnect => formatter.write_str("Reconnect"),
            Self::WaitForDevice => formatter.write_str("WaitForDevice"),
            Self::GetProperty(property) => formatter
                .debug_tuple("GetProperty")
                .field(property)
                .finish(),
            Self::CaptureScreenshot => formatter.write_str("CaptureScreenshot"),
            Self::WindowSize => formatter.write_str("WindowSize"),
            Self::WindowDensity => formatter.write_str("WindowDensity"),
            Self::Tap { x, y } => formatter
                .debug_struct("Tap")
                .field("x", x)
                .field("y", y)
                .finish(),
            Self::KeyPress(key) => formatter.debug_tuple("KeyPress").field(key).finish(),
            Self::Swipe {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => formatter
                .debug_struct("Swipe")
                .field("start_x", start_x)
                .field("start_y", start_y)
                .field("end_x", end_x)
                .field("end_y", end_y)
                .field("duration_ms", duration_ms)
                .finish(),
            Self::Scroll {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
            } => formatter
                .debug_struct("Scroll")
                .field("start_x", start_x)
                .field("start_y", start_y)
                .field("end_x", end_x)
                .field("end_y", end_y)
                .field("duration_ms", duration_ms)
                .finish(),
            Self::InputText(text) => formatter.debug_tuple("InputText").field(text).finish(),
            Self::Launch(package) => formatter.debug_tuple("Launch").field(package).finish(),
            Self::Terminate(package) => formatter.debug_tuple("Terminate").field(package).finish(),
            Self::Back => formatter.write_str("Back"),
            Self::Home => formatter.write_str("Home"),
            Self::RecentApps => formatter.write_str("RecentApps"),
            Self::InputSecret => formatter.write_str("InputSecret"),
        }
    }
}

fn push_args(arguments: &mut Vec<OsString>, values: &[&str]) {
    arguments.extend(values.iter().map(OsString::from));
}

/// One typed adb invocation, optionally routed to exactly one serial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdbCommand {
    serial: Option<AdbSerial>,
    operation: AdbOperation,
}

impl AdbCommand {
    pub fn host(operation: AdbOperation) -> Self {
        Self {
            serial: None,
            operation,
        }
    }

    pub fn for_device(serial: AdbSerial, operation: AdbOperation) -> Self {
        Self {
            serial: Some(serial),
            operation,
        }
    }

    #[cfg(test)]
    pub(crate) fn serial(&self) -> Option<&AdbSerial> {
        self.serial.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn operation(&self) -> &AdbOperation {
        &self.operation
    }

    fn validate_scope(&self) -> AndroidAdbResult<()> {
        match (self.operation.requires_device(), self.serial.is_some()) {
            (true, false) => Err(AndroidAdbError::InvalidValue {
                field: "adb command scope",
                value: format!("{} requires a device serial", self.operation.name()),
            }),
            (false, true) => Err(AndroidAdbError::InvalidValue {
                field: "adb command scope",
                value: format!("{} is a host operation", self.operation.name()),
            }),
            _ => Ok(()),
        }
    }

    fn arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        if let Some(serial) = &self.serial {
            arguments.push("-s".into());
            arguments.push(serial.as_str().into());
        }
        self.operation.append_arguments(&mut arguments);
        arguments
    }
}

/// Bounded output from a successful adb process.
#[derive(Clone, PartialEq, Eq)]
pub struct AdbCommandOutput {
    operation: &'static str,
    stdout: Vec<u8>,
    stderr_tail: String,
}

impl AdbCommandOutput {
    /// Builds textual output for a replaceable/fake command runner.
    #[cfg(test)]
    pub(crate) fn text(operation: &'static str, stdout: impl Into<String>) -> Self {
        Self {
            operation,
            stdout: stdout.into().into_bytes(),
            stderr_tail: String::new(),
        }
    }

    /// Builds binary output for a replaceable/fake command runner.
    #[cfg(test)]
    pub(crate) fn binary(operation: &'static str, stdout: Vec<u8>) -> Self {
        Self {
            operation,
            stdout,
            stderr_tail: String::new(),
        }
    }

    /// Consumes bounded binary stdout without cloning screenshot bytes.
    pub(crate) fn into_stdout_bytes(self) -> Vec<u8> {
        self.stdout
    }

    pub(crate) fn stdout_text(&self) -> AndroidAdbResult<&str> {
        std::str::from_utf8(&self.stdout).map_err(|_| AndroidAdbError::InvalidUtf8 {
            operation: self.operation,
            stream: "stdout",
        })
    }

    pub(crate) fn stderr_text(&self) -> &str {
        &self.stderr_tail
    }
}

impl fmt::Debug for AdbCommandOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdbCommandOutput")
            .field("operation", &self.operation)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_tail", &self.stderr_tail)
            .finish()
    }
}

#[async_trait]
pub trait AdbCommandRunner: Send + Sync {
    async fn run(
        &self,
        command: AdbCommand,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<AdbCommandOutput>;

    async fn run_protected(
        &self,
        command: AdbCommand,
        input: ProtectedAdbInput,
        _control: &ExecutionControl,
    ) -> AndroidAdbResult<()> {
        drop(input);
        Err(AndroidAdbError::ProtectedOperationFailed {
            operation: command.operation.name(),
            status: None,
        })
    }
}

/// Configuration for the system adb process boundary.
///
/// Output bounds are intentionally fixed rather than configurable. This
/// prevents an embedding application from accidentally weakening the memory
/// ceiling required by the driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemAdbConfig {
    program: PathBuf,
    command_timeout: Duration,
}

impl SystemAdbConfig {
    pub fn new(program: impl Into<PathBuf>) -> AndroidAdbResult<Self> {
        let config = Self {
            program: program.into(),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    pub fn with_command_timeout(mut self, timeout: Duration) -> AndroidAdbResult<Self> {
        self.command_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> AndroidAdbResult<()> {
        if self.program.as_os_str().is_empty() {
            return Err(AndroidAdbError::InvalidValue {
                field: "adb program",
                value: "path is empty".to_owned(),
            });
        }
        if self.command_timeout.is_zero() || self.command_timeout > MAX_COMMAND_TIMEOUT {
            return Err(AndroidAdbError::InvalidValue {
                field: "adb command timeout",
                value: format!("{:?}", self.command_timeout),
            });
        }
        Ok(())
    }
}

impl Default for SystemAdbConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from("adb"),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemAdbCommandRunner {
    config: SystemAdbConfig,
}

impl SystemAdbCommandRunner {
    pub fn new(config: SystemAdbConfig) -> AndroidAdbResult<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    async fn run_command(
        &self,
        command: AdbCommand,
        protected_input: Option<ProtectedAdbInput>,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<AdbCommandOutput> {
        command.validate_scope()?;
        if control.is_cancelled() {
            return Err(AndroidAdbError::Cancelled);
        }
        if control.is_expired() {
            return Err(AndroidAdbError::TimedOut {
                operation: command.operation.name(),
            });
        }

        let operation = command.operation.name();
        let stdout_limit = command.operation.stdout_limit();
        let returns_binary = command.operation.returns_binary();
        let is_protected = protected_input.is_some();
        let protected_serial = is_protected.then(|| {
            command
                .serial
                .clone()
                .expect("protected command is device scoped")
        });

        let mut process = Command::new(&self.config.program);
        process
            .args(command.arguments())
            .stdin(if is_protected {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = process.spawn().map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                AndroidAdbError::ExecutableNotFound {
                    program: self.config.program.clone(),
                }
            } else {
                AndroidAdbError::Spawn { operation, source }
            }
        })?;
        let stdout = child.stdout.take().ok_or_else(|| AndroidAdbError::Spawn {
            operation,
            source: io::Error::other("adb stdout pipe was not created"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| AndroidAdbError::Spawn {
            operation,
            source: io::Error::other("adb stderr pipe was not created"),
        })?;
        let stdin = if is_protected {
            Some(child.stdin.take().ok_or_else(|| AndroidAdbError::Spawn {
                operation,
                source: io::Error::other("adb stdin pipe was not created"),
            })?)
        } else {
            None
        };

        let collect = async move {
            let write_stdin = async move {
                if let (Some(mut stdin), Some(input)) = (stdin, protected_input) {
                    stdin.write_all(input.as_bytes()).await.map_err(|source| {
                        AndroidAdbError::Write {
                            operation,
                            stream: "stdin",
                            source,
                        }
                    })?;
                    stdin
                        .write_all(b"\n")
                        .await
                        .map_err(|source| AndroidAdbError::Write {
                            operation,
                            stream: "stdin",
                            source,
                        })?;
                    stdin
                        .shutdown()
                        .await
                        .map_err(|source| AndroidAdbError::Write {
                            operation,
                            stream: "stdin",
                            source,
                        })?;
                }
                Ok::<(), AndroidAdbError>(())
            };
            let stdout = async {
                read_limited(stdout, stdout_limit)
                    .await
                    .map_err(|error| match error {
                        BoundedReadError::Io(source) => AndroidAdbError::Read {
                            operation,
                            stream: "stdout",
                            source,
                        },
                        BoundedReadError::TooLarge => AndroidAdbError::OutputTooLarge {
                            operation,
                            stream: "stdout",
                            limit: stdout_limit,
                        },
                    })
            };
            let stderr = async {
                read_tail(stderr, STDERR_TAIL_LIMIT)
                    .await
                    .map_err(|source| AndroidAdbError::Read {
                        operation,
                        stream: "stderr",
                        source,
                    })
            };
            let status = async {
                child.wait().await.map_err(|source| AndroidAdbError::Read {
                    operation,
                    stream: "process status",
                    source,
                })
            };

            match protected_serial {
                Some(serial) => {
                    // Do not let stdin EPIPE short-circuit collection of adb's
                    // transport diagnostic. All four futures remain bounded
                    // by the outer cancellation/deadline select.
                    let (write, stdout, stderr, status) =
                        tokio::join!(write_stdin, stdout, stderr, status);
                    finish_protected_process(
                        &serial,
                        operation,
                        write,
                        stdout,
                        stderr,
                        status.map(|status| status.code()),
                    )
                }
                None => {
                    let (_, stdout, stderr, status) =
                        tokio::try_join!(write_stdin, stdout, stderr, status)?;
                    let stderr_tail = bounded_lossy_text(&stderr);
                    if !status.success() {
                        return Err(AndroidAdbError::ProcessFailed {
                            operation,
                            status: status.code(),
                            stderr_tail: if stderr_tail.is_empty() {
                                "<no stderr>".to_owned()
                            } else {
                                stderr_tail
                            },
                        });
                    }
                    if !returns_binary && std::str::from_utf8(&stdout).is_err() {
                        return Err(AndroidAdbError::InvalidUtf8 {
                            operation,
                            stream: "stdout",
                        });
                    }
                    Ok(AdbCommandOutput {
                        operation,
                        stdout,
                        stderr_tail,
                    })
                }
            }
        };

        tokio::pin!(collect);
        let timeout = control
            .remaining()
            .map_or(self.config.command_timeout, |remaining| {
                remaining.min(self.config.command_timeout)
            });
        let timeout_sleep = tokio::time::sleep(timeout);
        tokio::pin!(timeout_sleep);

        // A fully collected result wins a same-poll boundary race, matching
        // the core runtime's completion/cancellation semantics.
        tokio::select! {
            biased;
            result = &mut collect => result,
            _ = control.cancelled() => Err(AndroidAdbError::Cancelled),
            () = &mut timeout_sleep => Err(AndroidAdbError::TimedOut { operation }),
        }
    }
}

#[async_trait]
impl AdbCommandRunner for SystemAdbCommandRunner {
    async fn run(
        &self,
        command: AdbCommand,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<AdbCommandOutput> {
        if command.operation.requires_protected_input() {
            return Err(AndroidAdbError::ProtectedOperationFailed {
                operation: command.operation.name(),
                status: None,
            });
        }
        self.run_command(command, None, control).await
    }

    async fn run_protected(
        &self,
        command: AdbCommand,
        input: ProtectedAdbInput,
        control: &ExecutionControl,
    ) -> AndroidAdbResult<()> {
        if !command.operation.requires_protected_input() {
            drop(input);
            return Err(AndroidAdbError::ProtectedOperationFailed {
                operation: command.operation.name(),
                status: None,
            });
        }
        self.run_command(command, Some(input), control)
            .await
            .map(|_| ())
    }
}

struct ProtectedOutputBuffers {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ProtectedOutputBuffers {
    fn new(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self { stdout, stderr }
    }

    fn classify_transport(&self, serial: &AdbSerial) -> Option<AndroidAdbError> {
        classify_protected_transport_stderr(serial, &self.stderr)
    }

    fn classify_result(
        &self,
        operation: &'static str,
        status: Option<i32>,
    ) -> Option<AndroidAdbError> {
        let clean_stdout = self.stdout.iter().all(u8::is_ascii_whitespace);
        let clean_stderr = std::str::from_utf8(&self.stderr).is_ok_and(|stderr| {
            stderr
                .lines()
                .all(|line| line.trim().is_empty() || line.trim().starts_with("* daemon "))
        });
        if status == Some(0) && clean_stdout && clean_stderr {
            None
        } else {
            Some(AndroidAdbError::ProtectedOperationFailed { operation, status })
        }
    }
}

fn finish_protected_process(
    serial: &AdbSerial,
    operation: &'static str,
    write: AndroidAdbResult<()>,
    stdout: AndroidAdbResult<Vec<u8>>,
    stderr: AndroidAdbResult<Vec<u8>>,
    status: AndroidAdbResult<Option<i32>>,
) -> AndroidAdbResult<AdbCommandOutput> {
    let (stdout, stdout_error) = match stdout {
        Ok(stdout) => (stdout, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let (stderr, stderr_error) = match stderr {
        Ok(stderr) => (stderr, None),
        Err(error) => (Vec::new(), Some(error)),
    };
    let status_code = status.as_ref().ok().copied().flatten();
    let output = ProtectedOutputBuffers::new(stdout, stderr);

    // Connectivity classification wins over a concurrent stdin EPIPE or
    // process-wait error. The diagnostic bytes remain owned by the zeroing
    // wrapper until this function returns.
    if let Some(error) = output.classify_transport(serial) {
        return Err(error);
    }
    if write.is_err() || stdout_error.is_some() || stderr_error.is_some() || status.is_err() {
        return Err(AndroidAdbError::ProtectedOperationFailed {
            operation,
            status: status_code,
        });
    }
    if let Some(error) = output.classify_result(operation, status_code) {
        return Err(error);
    }
    Ok(AdbCommandOutput {
        operation,
        stdout: Vec::new(),
        stderr_tail: String::new(),
    })
}

pub(crate) fn classify_protected_transport_stderr(
    serial: &AdbSerial,
    stderr: &[u8],
) -> Option<AndroidAdbError> {
    for line in stderr.split(|byte| *byte == b'\n') {
        let line = trim_ascii_whitespace(line);
        let host_diagnostic = starts_with_ascii_case_insensitive(line, b"adb:")
            || starts_with_ascii_case_insensitive(line, b"error: device ")
            || starts_with_ascii_case_insensitive(line, b"error: no devices/emulators found")
            || starts_with_ascii_case_insensitive(
                line,
                b"error: insufficient permissions for device",
            )
            || starts_with_ascii_case_insensitive(line, b"no permissions")
            || starts_with_ascii_case_insensitive(line, b"no devices/emulators found");
        if !host_diagnostic {
            continue;
        }

        let device_id = serial.device_id();
        if contains_ascii_case_insensitive(line, b"unauthorized") {
            return Some(AndroidAdbError::Unauthorized { device_id });
        }
        if contains_ascii_case_insensitive(line, b"no permissions")
            || contains_ascii_case_insensitive(line, b"permission denied")
            || contains_ascii_case_insensitive(line, b"insufficient permissions for device")
        {
            return Some(AndroidAdbError::PermissionDenied { device_id });
        }
        if contains_ascii_case_insensitive(line, b"not found")
            || contains_ascii_case_insensitive(line, b"no devices/emulators found")
        {
            return Some(AndroidAdbError::Missing { device_id });
        }
        if contains_ascii_case_insensitive(line, b"device offline") {
            return Some(AndroidAdbError::OfflineExhausted {
                device_id,
                attempts: 0,
            });
        }
    }
    None
}

impl Drop for ProtectedOutputBuffers {
    fn drop(&mut self) {
        self.stdout.fill(0);
        self.stderr.fill(0);
    }
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn starts_with_ascii_case_insensitive(value: &[u8], prefix: &[u8]) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn trim_ascii_whitespace(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

enum BoundedReadError {
    Io(io::Error),
    TooLarge,
}

async fn read_limited<R>(mut reader: R, limit: usize) -> Result<Vec<u8>, BoundedReadError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(16 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(BoundedReadError::Io)?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > limit {
            return Err(BoundedReadError::TooLarge);
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

async fn read_tail<R>(mut reader: R, limit: usize) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut tail = Vec::with_capacity(limit.min(16 * 1024));
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(tail);
        }
        let chunk = &buffer[..read];
        if chunk.len() >= limit {
            tail.clear();
            tail.extend_from_slice(&chunk[chunk.len() - limit..]);
            continue;
        }
        let overflow = tail.len().saturating_add(chunk.len()).saturating_sub(limit);
        if overflow > 0 {
            tail.drain(..overflow);
        }
        tail.extend_from_slice(chunk);
    }
}

fn bounded_lossy_text(bytes: &[u8]) -> String {
    // Every invalid input byte may become the three-byte UTF-8 replacement
    // character. Bound the encoded String as well as the raw stderr buffer,
    // preserving the most recent diagnostic bytes and a valid char boundary.
    let lossy = String::from_utf8_lossy(bytes);
    let trimmed = lossy.trim();
    let mut start = trimmed.len().saturating_sub(STDERR_TAIL_LIMIT);
    while !trimmed.is_char_boundary(start) {
        start += 1;
    }
    trimmed[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, io, path::Path, time::Duration};

    #[cfg(unix)]
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process::{Command as StdCommand, Stdio as StdStdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use devicerail_core::ExecutionControl;
    #[cfg(unix)]
    use devicerail_core::{CancellationReason, ExecutionController};

    #[cfg(unix)]
    use super::PROTECTED_INPUT_SCRIPT;
    use super::{
        AdbCommand, AdbCommandRunner, AdbInputText, AdbOperation, AdbProperty, AndroidKey,
        AndroidPackageName, MAX_COMMAND_TIMEOUT, ProtectedAdbInput, SCREENSHOT_STDOUT_LIMIT,
        STDERR_TAIL_LIMIT, SystemAdbCommandRunner, SystemAdbConfig, TEXT_STDOUT_LIMIT,
        bounded_lossy_text, finish_protected_process,
    };
    use crate::{AdbSerial, AndroidAdbError};

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn host_and_device_commands_build_distinct_arguments_without_a_shell() {
        let host = AdbCommand::host(AdbOperation::DevicesLong);
        assert_eq!(host.arguments(), strings(&["devices", "-l"]));

        let device = AdbCommand::for_device(
            AdbSerial::parse("emulator-5554").expect("serial"),
            AdbOperation::GetState,
        );
        assert_eq!(
            device.arguments(),
            strings(&["-s", "emulator-5554", "get-state"])
        );

        let property = AdbCommand::for_device(
            AdbSerial::parse("serial-with-punctuation._:-").expect("serial"),
            AdbOperation::GetProperty(AdbProperty::BootCompleted),
        );
        assert_eq!(
            property.arguments(),
            strings(&[
                "-s",
                "serial-with-punctuation._:-",
                "shell",
                "getprop",
                "sys.boot_completed"
            ])
        );

        let serial = AdbSerial::parse("emulator-5554").expect("serial");
        let screenshot = AdbCommand::for_device(serial.clone(), AdbOperation::CaptureScreenshot);
        assert_eq!(
            screenshot.arguments(),
            strings(&["-s", "emulator-5554", "exec-out", "screencap", "-p"])
        );
        let size = AdbCommand::for_device(serial.clone(), AdbOperation::WindowSize);
        assert_eq!(
            size.arguments(),
            strings(&["-s", "emulator-5554", "shell", "wm", "size"])
        );
        let density = AdbCommand::for_device(serial, AdbOperation::WindowDensity);
        assert_eq!(
            density.arguments(),
            strings(&["-s", "emulator-5554", "shell", "wm", "density"])
        );
    }

    #[test]
    fn action_commands_have_exact_serial_scoped_argument_vectors() {
        let serial = AdbSerial::parse("emulator-5554").expect("serial");
        let cases = [
            (
                AdbOperation::Tap { x: 12, y: 34 },
                strings(&["-s", "emulator-5554", "shell", "input", "tap", "12", "34"]),
            ),
            (
                AdbOperation::KeyPress(AndroidKey::ArrowLeft),
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "keyevent",
                    "KEYCODE_DPAD_LEFT",
                ]),
            ),
            (
                AdbOperation::Swipe {
                    start_x: 1,
                    start_y: 2,
                    end_x: 3,
                    end_y: 4,
                    duration_ms: 500,
                },
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "swipe",
                    "1",
                    "2",
                    "3",
                    "4",
                    "500",
                ]),
            ),
            (
                AdbOperation::Scroll {
                    start_x: 75,
                    start_y: 150,
                    end_x: 25,
                    end_y: 50,
                    duration_ms: 300,
                },
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "swipe",
                    "75",
                    "150",
                    "25",
                    "50",
                    "300",
                ]),
            ),
            (
                AdbOperation::InputText(
                    AdbInputText::parse("Device Rail_1@example.com").expect("safe text fixture"),
                ),
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "text",
                    "Device%sRail_1@example.com",
                ]),
            ),
            (
                AdbOperation::Launch(
                    AndroidPackageName::parse("com.example.app").expect("package"),
                ),
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "am",
                    "start",
                    "-W",
                    "--user",
                    "current",
                    "-a",
                    "android.intent.action.MAIN",
                    "-c",
                    "android.intent.category.LAUNCHER",
                    "-p",
                    "com.example.app",
                ]),
            ),
            (
                AdbOperation::Terminate(
                    AndroidPackageName::parse("com.example.app").expect("package"),
                ),
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "am",
                    "force-stop",
                    "--user",
                    "current",
                    "com.example.app",
                ]),
            ),
            (
                AdbOperation::Back,
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "keyevent",
                    "KEYCODE_BACK",
                ]),
            ),
            (
                AdbOperation::Home,
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "keyevent",
                    "KEYCODE_HOME",
                ]),
            ),
            (
                AdbOperation::RecentApps,
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "input",
                    "keyevent",
                    "KEYCODE_APP_SWITCH",
                ]),
            ),
            (
                AdbOperation::InputSecret,
                strings(&[
                    "-s",
                    "emulator-5554",
                    "shell",
                    "-T",
                    "IFS= read -r DEVICERAIL_SECRET && input text \"$DEVICERAIL_SECRET\"",
                ]),
            ),
        ];

        for (operation, expected) in cases {
            let command = AdbCommand::for_device(serial.clone(), operation);
            assert_eq!(command.arguments(), expected);
        }
    }

    #[test]
    fn input_text_rejects_remote_shell_metacharacters_and_redacts_debug() {
        for rejected in [
            "%",
            "unicode-中",
            "line\nbreak",
            "'",
            "\"",
            ";",
            "&",
            "|",
            "$",
            "(",
            ")",
            "<",
            ">",
            "*",
            "?",
            "[",
            "]",
            "{",
            "}",
            "!",
            "#",
            "\\",
        ] {
            assert!(
                AdbInputText::parse(rejected).is_err(),
                "unsafe input unexpectedly accepted: {rejected:?}"
            );
        }
        assert!(AdbInputText::parse("").is_err());
        assert!(AdbInputText::parse(&"a".repeat(AdbInputText::MAX_BYTES + 1)).is_err());

        let secret = "DoNotEchoThis 123";
        let operation = AdbOperation::InputText(AdbInputText::parse(secret).expect("safe text"));
        let debug = format!("{operation:?}");
        assert!(!debug.contains(secret));
        assert!(!debug.contains("DoNotEchoThis"));
        assert!(debug.contains(&secret.len().to_string()));
    }

    #[test]
    fn package_name_enforces_android_grammar_length_and_redacted_errors() {
        let maximum = format!("a.{}", "b".repeat(253));
        assert_eq!(maximum.len(), AndroidPackageName::MAX_BYTES);
        for valid in ["a.b", "Com.Example_1.App2", maximum.as_str()] {
            let package = AndroidPackageName::parse(valid).expect("valid package name");
            assert_eq!(package.byte_len(), valid.len());
            let debug = format!("{package:?}");
            assert!(!debug.contains(valid));
        }

        let overlong = format!("a.{}", "b".repeat(254));
        let private_injection = "com.example;PRIVATE_PAYLOAD";
        for invalid in [
            "",
            "a",
            "a.",
            ".a",
            "a..b",
            "1a.b",
            "a.1b",
            "_a.b",
            "a._b",
            "a-b.c",
            "a/b.c",
            "a.$(id)",
            "a.中",
            "a.b\nnext",
            overlong.as_str(),
        ] {
            AndroidPackageName::parse(invalid).expect_err("invalid package name");
        }
        let error = AndroidPackageName::parse(private_injection).expect_err("injection rejected");
        assert!(!error.to_string().contains(private_injection));
        assert!(!format!("{error:?}").contains("PRIVATE_PAYLOAD"));
    }

    #[test]
    fn protected_input_enforces_printable_ascii_reserved_sequence_and_redacted_debug() {
        for valid in [
            b"A".to_vec(),
            b"printable !@#$^&*()[]{}".to_vec(),
            b"percent%value".to_vec(),
            vec![b'A'; ProtectedAdbInput::MAX_BYTES],
        ] {
            let input = ProtectedAdbInput::parse(valid).expect("valid protected input");
            assert_eq!(format!("{input:?}"), "<redacted>");
        }
        for invalid in [
            Vec::new(),
            b"reserved%svalue".to_vec(),
            b"line\nbreak".to_vec(),
            b"tab\tvalue".to_vec(),
            vec![0x7f],
            vec![b'A'; ProtectedAdbInput::MAX_BYTES + 1],
        ] {
            let error = ProtectedAdbInput::parse(invalid).expect_err("invalid protected input");
            assert!(!format!("{error:?}").contains("reserved%svalue"));
        }
    }

    #[test]
    fn protected_process_prioritizes_closed_transport_diagnostics_over_epipe() {
        let serial = AdbSerial::parse("emulator-5554").expect("serial");
        for (stderr, expected_code) in [
            ("error: device offline\n", "android_device_offline"),
            (
                "error: device unauthorized. Please check the confirmation dialog.\n",
                "android_device_unauthorized",
            ),
            (
                "error: device 'emulator-5554' not found\n",
                "android_device_missing",
            ),
            (
                "error: no devices/emulators found\n",
                "android_device_missing",
            ),
            (
                "no permissions (user is not in the plugdev group)\n",
                "android_device_permission_denied",
            ),
        ] {
            let error = finish_protected_process(
                &serial,
                "input_secret",
                Err(AndroidAdbError::Write {
                    operation: "input_secret",
                    stream: "stdin",
                    source: io::Error::new(io::ErrorKind::BrokenPipe, "fixture EPIPE"),
                }),
                Ok(Vec::new()),
                Ok(stderr.as_bytes().to_vec()),
                Ok(Some(1)),
            )
            .expect_err("transport diagnostic wins over EPIPE");
            assert_eq!(error.code(), expected_code, "stderr={stderr:?}");
            assert!(!error.to_string().contains("fixture EPIPE"));
        }
    }

    #[test]
    fn protected_process_does_not_treat_remote_shell_errors_as_transport_state() {
        let serial = AdbSerial::parse("emulator-5554").expect("serial");
        for stderr in [
            "/system/bin/sh: input: not found\n",
            "input: permission denied\n",
            "Error: permission denied\n",
            "remote output says offline, unauthorized, and no permissions\n",
        ] {
            let error = finish_protected_process(
                &serial,
                "input_secret",
                Ok(()),
                Ok(Vec::new()),
                Ok(stderr.as_bytes().to_vec()),
                Ok(Some(127)),
            )
            .expect_err("remote shell failure is sanitized");
            assert!(matches!(
                error,
                AndroidAdbError::ProtectedOperationFailed {
                    operation: "input_secret",
                    status: Some(127),
                }
            ));
        }
    }

    #[test]
    fn command_scope_rejects_ambiguous_host_device_execution() {
        let host_device_operation = AdbCommand::host(AdbOperation::GetState);
        assert!(host_device_operation.validate_scope().is_err());

        let scoped_host_operation = AdbCommand::for_device(
            AdbSerial::parse("emulator-5554").expect("serial"),
            AdbOperation::DevicesLong,
        );
        assert!(scoped_host_operation.validate_scope().is_err());
    }

    #[test]
    fn configuration_keeps_hard_bounds_and_requires_a_finite_timeout() {
        let default = SystemAdbConfig::default();
        assert_eq!(default.program(), Path::new("adb"));
        assert!(!default.command_timeout().is_zero());
        assert!(default.command_timeout() >= Duration::from_secs(65));
        assert!(default.command_timeout() <= MAX_COMMAND_TIMEOUT);
        assert_eq!(TEXT_STDOUT_LIMIT, 1024 * 1024);
        assert_eq!(SCREENSHOT_STDOUT_LIMIT, 32 * 1024 * 1024);
        assert_eq!(
            AdbOperation::CaptureScreenshot.stdout_limit(),
            SCREENSHOT_STDOUT_LIMIT
        );
        assert!(AdbOperation::CaptureScreenshot.returns_binary());
        assert_eq!(AdbOperation::WindowSize.stdout_limit(), TEXT_STDOUT_LIMIT);
        assert!(!AdbOperation::WindowSize.returns_binary());

        let empty = SystemAdbConfig::new("").expect_err("empty executable is invalid");
        assert!(matches!(empty, AndroidAdbError::InvalidValue { .. }));

        let zero = SystemAdbConfig::default()
            .with_command_timeout(Duration::ZERO)
            .expect_err("zero timeout is invalid");
        assert!(matches!(zero, AndroidAdbError::InvalidValue { .. }));

        let excessive = SystemAdbConfig::default()
            .with_command_timeout(MAX_COMMAND_TIMEOUT + Duration::from_millis(1))
            .expect_err("unbounded timeout is invalid");
        assert!(matches!(excessive, AndroidAdbError::InvalidValue { .. }));
    }

    #[test]
    fn lossy_stderr_conversion_remains_bounded_for_invalid_utf8() {
        let mut bytes = vec![0xff; STDERR_TAIL_LIMIT];
        bytes[STDERR_TAIL_LIMIT - 4..].copy_from_slice(b"TAIL");
        assert!(String::from_utf8_lossy(&bytes).len() > STDERR_TAIL_LIMIT);

        let stderr_tail = bounded_lossy_text(&bytes);

        assert!(stderr_tail.len() <= STDERR_TAIL_LIMIT);
        assert!(stderr_tail.contains('\u{fffd}'));
        assert!(stderr_tail.ends_with("TAIL"));
    }

    #[tokio::test]
    async fn missing_program_has_a_specific_error() {
        let missing = std::env::temp_dir().join(format!(
            "devicerail-adb-command-does-not-exist-{}",
            std::process::id()
        ));
        let runner = SystemAdbCommandRunner::new(
            SystemAdbConfig::new(&missing).expect("valid missing path"),
        )
        .expect("runner");
        let error = runner
            .run(
                AdbCommand::host(AdbOperation::DevicesLong),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect_err("program is absent");
        assert!(matches!(
            error,
            AndroidAdbError::ExecutableNotFound { program } if program == missing
        ));
    }

    #[cfg(unix)]
    static NEXT_TEST_PROGRAM: AtomicU64 = AtomicU64::new(0);
    #[cfg(unix)]
    static SYSTEM_PROCESS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// An executable fixture used only to exercise the real Tokio process
    /// boundary. Production commands still pass an argv vector directly to
    /// `adb` and never concatenate a shell command.
    #[cfg(unix)]
    struct TestProgram {
        directory: PathBuf,
        path: PathBuf,
    }

    #[cfg(unix)]
    impl TestProgram {
        fn new(body: &str) -> Self {
            let unique = format!(
                "devicerail-adb-runner-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after Unix epoch")
                    .as_nanos(),
                NEXT_TEST_PROGRAM.fetch_add(1, Ordering::Relaxed),
            );
            let directory = std::env::temp_dir().join(unique);
            fs::create_dir(&directory).expect("create process fixture directory");
            let path = directory.join("fake-adb");
            fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n"))
                .expect("write process fixture");
            let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&path, permissions).expect("make process fixture executable");
            Self { directory, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn pid_path(&self) -> PathBuf {
            self.sidecar_path(".pid")
        }

        fn sidecar_path(&self, suffix: &str) -> PathBuf {
            let mut path = self.path.as_os_str().to_os_string();
            path.push(suffix);
            path.into()
        }
    }

    #[cfg(unix)]
    impl Drop for TestProgram {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[cfg(unix)]
    fn system_runner(program: &TestProgram, timeout: Duration) -> SystemAdbCommandRunner {
        let config = SystemAdbConfig::new(program.path())
            .expect("valid fixture path")
            .with_command_timeout(timeout)
            .expect("valid fixture timeout");
        SystemAdbCommandRunner::new(config).expect("system runner")
    }

    #[cfg(unix)]
    async fn wait_for_pid(path: &Path) -> u32 {
        // Full-workspace CI can be CPU-saturated while several platform
        // conformance binaries start at once. Keep this below the shortest
        // system-runner deadline but do not fail merely because fork/exec took
        // a little over two seconds.
        for _ in 0..400 {
            if let Ok(value) = fs::read_to_string(path)
                && let Ok(pid) = value.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture did not publish its pid at {}", path.display());
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        StdCommand::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdin(StdStdio::null())
            .stdout(StdStdio::null())
            .stderr(StdStdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    async fn wait_for_process_exit(pid: u32) {
        for _ in 0..100 {
            if !process_exists(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("fixture process {pid} survived runner cancellation");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_runner_rejects_stdout_past_the_hard_limit() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let program =
            TestProgram::new("while :; do\n  printf '0123456789abcdef0123456789abcdef'\ndone");
        let error = system_runner(&program, Duration::from_secs(5))
            .run(
                AdbCommand::host(AdbOperation::DevicesLong),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect_err("unbounded fixture output must be rejected");

        assert!(matches!(
            error,
            AndroidAdbError::OutputTooLarge {
                operation: "devices_long",
                stream: "stdout",
                limit: TEXT_STDOUT_LIMIT,
            }
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_runner_preserves_binary_screenshot_stdout() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let program = TestProgram::new("printf '\\377PNG'");
        let output = system_runner(&program, Duration::from_secs(5))
            .run(
                AdbCommand::for_device(
                    AdbSerial::parse("emulator-5554").expect("serial"),
                    AdbOperation::CaptureScreenshot,
                ),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect("binary stdout must not be treated as UTF-8");

        assert!(matches!(
            output.stdout_text(),
            Err(AndroidAdbError::InvalidUtf8 {
                operation: "capture_screenshot",
                stream: "stdout"
            })
        ));
        assert_eq!(output.into_stdout_bytes(), vec![0xff, b'P', b'N', b'G']);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_runner_keeps_secret_out_of_argv_and_writes_exact_stdin() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let program =
            TestProgram::new("printf '%s\\n' \"$@\" > \"${0}.argv\"\ncat > \"${0}.stdin\"");
        let runner = system_runner(&program, Duration::from_secs(5));
        let sentinel = "SENTINEL $() ; '&\" \\ %value";
        let command = AdbCommand::for_device(
            AdbSerial::parse("emulator-5554").expect("serial"),
            AdbOperation::InputSecret,
        );
        assert!(!format!("{command:?}").contains(sentinel));

        runner
            .run_protected(
                command,
                ProtectedAdbInput::parse(sentinel.as_bytes().to_vec()).expect("protected input"),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect("protected fixture succeeds");

        let arguments = fs::read_to_string(program.sidecar_path(".argv")).expect("recorded argv");
        assert_eq!(
            arguments.lines().collect::<Vec<_>>(),
            ["-s", "emulator-5554", "shell", "-T", PROTECTED_INPUT_SCRIPT,]
        );
        assert!(!arguments.contains(sentinel));
        assert_eq!(
            fs::read(program.sidecar_path(".stdin")).expect("recorded stdin"),
            format!("{sentinel}\n").into_bytes()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_runner_discards_echoed_failure_output_and_keeps_transport_classification() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let sentinel = "SENTINEL_PROTECTED_VALUE";
        for (diagnostic, expect_offline) in [
            ("Error: $value", false),
            ("error: device offline $value", true),
        ] {
            let program = TestProgram::new(&format!(
                "IFS= read -r value\nprintf '%s\\n' \"{diagnostic}\" >&2\nexit 7"
            ));
            let runner = system_runner(&program, Duration::from_secs(5));
            let error = runner
                .run_protected(
                    AdbCommand::for_device(
                        AdbSerial::parse("emulator-5554").expect("serial"),
                        AdbOperation::InputSecret,
                    ),
                    ProtectedAdbInput::parse(sentinel.as_bytes().to_vec())
                        .expect("protected input"),
                    &ExecutionControl::unbounded(),
                )
                .await
                .expect_err("protected fixture fails");
            if expect_offline {
                assert!(matches!(&error, AndroidAdbError::OfflineExhausted { .. }));
            } else {
                assert!(matches!(
                    &error,
                    AndroidAdbError::ProtectedOperationFailed {
                        operation: "input_secret",
                        status: Some(7),
                    }
                ));
            }
            assert!(!error.to_string().contains(sentinel));
            assert!(!format!("{error:?}").contains(sentinel));
        }

        let stdout_sentinel = "offline unauthorized no permissions";
        let program = TestProgram::new("IFS= read -r value\nprintf '%s\n' \"$value\"\nexit 7");
        let error = system_runner(&program, Duration::from_secs(5))
            .run_protected(
                AdbCommand::for_device(
                    AdbSerial::parse("emulator-5554").expect("serial"),
                    AdbOperation::InputSecret,
                ),
                ProtectedAdbInput::parse(stdout_sentinel.as_bytes().to_vec())
                    .expect("protected input"),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect_err("protected stdout is not a transport diagnostic");
        assert!(matches!(
            &error,
            AndroidAdbError::ProtectedOperationFailed {
                operation: "input_secret",
                status: Some(7),
            }
        ));
        assert!(!error.to_string().contains(stdout_sentinel));
        assert!(!format!("{error:?}").contains(stdout_sentinel));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_runner_reports_only_the_bounded_stderr_tail_on_failure() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let block = "x".repeat(1024);
        let body = format!(
            "printf 'HEAD-SENTINEL\\n' >&2\nblock='{block}'\ni=0\nwhile [ \"$i\" -lt 70 ]; do\n  printf '%s' \"$block\" >&2\n  i=$((i + 1))\ndone\nprintf '\\nTAIL-SENTINEL\\n' >&2\nexit 23"
        );
        let program = TestProgram::new(&body);
        let error = system_runner(&program, Duration::from_secs(5))
            .run(
                AdbCommand::host(AdbOperation::DevicesLong),
                &ExecutionControl::unbounded(),
            )
            .await
            .expect_err("non-zero fixture status must fail");

        match error {
            AndroidAdbError::ProcessFailed {
                operation,
                status,
                stderr_tail,
            } => {
                assert_eq!(operation, "devices_long");
                assert_eq!(status, Some(23));
                assert!(stderr_tail.len() <= STDERR_TAIL_LIMIT);
                assert!(!stderr_tail.contains("HEAD-SENTINEL"));
                assert!(stderr_tail.ends_with("TAIL-SENTINEL"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_runner_internal_timeout_kills_the_child() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let program = TestProgram::new("printf '%s' \"$$\" > \"${0}.pid\"\nexec sleep 60");
        let runner = system_runner(&program, Duration::from_secs(3));
        let task = tokio::spawn(async move {
            runner
                .run(
                    AdbCommand::host(AdbOperation::DevicesLong),
                    &ExecutionControl::unbounded(),
                )
                .await
        });
        let pid = tokio::time::timeout(Duration::from_secs(2), wait_for_pid(&program.pid_path()))
            .await
            .expect("fixture published its pid before the internal timeout");
        assert!(process_exists(pid));

        let error = tokio::time::timeout(Duration::from_secs(4), task)
            .await
            .expect("runner reached its internal timeout")
            .expect("runner task did not panic")
            .expect_err("fixture must hit the runner timeout");

        assert!(matches!(
            error,
            AndroidAdbError::TimedOut {
                operation: "devices_long"
            }
        ));
        wait_for_process_exit(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn system_runner_cancellation_kills_the_child() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let program = TestProgram::new("printf '%s' \"$$\" > \"${0}.pid\"\nexec sleep 60");
        let runner = system_runner(&program, Duration::from_secs(5));
        let (controller, control) = ExecutionController::new();
        let task = tokio::spawn(async move {
            runner
                .run(AdbCommand::host(AdbOperation::DevicesLong), &control)
                .await
        });
        let pid = wait_for_pid(&program.pid_path()).await;
        assert!(process_exists(pid));
        assert!(controller.cancel(CancellationReason::Requested));

        let error = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("runner observed cancellation")
            .expect("runner task did not panic")
            .expect_err("cancelled command must fail");
        assert!(matches!(error, AndroidAdbError::Cancelled));
        wait_for_process_exit(pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn protected_join_remains_cancellable_and_kills_the_child() {
        let _guard = SYSTEM_PROCESS_TEST_LOCK.lock().await;
        let program = TestProgram::new("printf '%s' \"$$\" > \"${0}.pid\"\nexec sleep 60");
        let runner = system_runner(&program, Duration::from_secs(15));
        let (controller, control) = ExecutionController::new();
        let task = tokio::spawn(async move {
            runner
                .run_protected(
                    AdbCommand::for_device(
                        AdbSerial::parse("emulator-5554").expect("serial"),
                        AdbOperation::InputSecret,
                    ),
                    ProtectedAdbInput::parse(b"CANCEL_SECRET".to_vec()).expect("protected input"),
                    &control,
                )
                .await
        });
        let pid = wait_for_pid(&program.pid_path()).await;
        assert!(process_exists(pid));
        assert!(controller.cancel(CancellationReason::Requested));

        let error = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("protected runner observed cancellation")
            .expect("protected runner task did not panic")
            .expect_err("cancelled protected command must fail");
        assert!(matches!(error, AndroidAdbError::Cancelled));
        wait_for_process_exit(pid).await;
    }
}
