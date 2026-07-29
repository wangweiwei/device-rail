use std::{
    ffi::OsString,
    fmt,
    future::pending,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::{ExecutionControl, TimeoutScope};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time,
};
use uuid::Uuid;

use crate::{HarmonyHdcError, HarmonyHdcResult, HdcTarget};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(65);
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TEXT_OUTPUT_LIMIT: usize = 1024 * 1024;
const SCREENSHOT_OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
const LAYOUT_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const STDERR_OUTPUT_LIMIT: usize = 64 * 1024;
const MAX_COORDINATE: u32 = 1_000_000;
const MIN_SWIPE_VELOCITY_PPS: u32 = 200;
const MAX_SWIPE_VELOCITY_PPS: u32 = 40_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemHdcConfig {
    executable: PathBuf,
    command_timeout: Duration,
}

impl Default for SystemHdcConfig {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("hdc"),
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }
}

impl SystemHdcConfig {
    pub fn new(
        executable: impl Into<PathBuf>,
        command_timeout: Duration,
    ) -> HarmonyHdcResult<Self> {
        let config = Self {
            executable: executable.into(),
            command_timeout,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    fn validate(&self) -> HarmonyHdcResult<()> {
        if self.executable.as_os_str().is_empty() {
            return Err(HarmonyHdcError::InvalidConfiguration(
                "HDC executable path is empty".to_owned(),
            ));
        }
        if self.command_timeout.is_zero() || self.command_timeout > MAX_COMMAND_TIMEOUT {
            return Err(HarmonyHdcError::InvalidConfiguration(
                "HDC command timeout must be between 1 ns and 300 seconds".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdcProperty {
    ProductModel,
    SoftwareVersion,
}

impl HdcProperty {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProductModel => "const.product.model",
            Self::SoftwareVersion => "const.product.software.version",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HarmonyKey {
    Enter,
    Tab,
    Delete,
    Back,
    Home,
}

impl HarmonyKey {
    pub const VALUES: [&'static str; 5] = ["enter", "tab", "delete", "back", "home"];

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enter" => Some(Self::Enter),
            "tab" => Some(Self::Tab),
            "delete" => Some(Self::Delete),
            "back" => Some(Self::Back),
            "home" => Some(Self::Home),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Delete => "delete",
            Self::Back => "back",
            Self::Home => "home",
        }
    }

    fn key_code(self) -> &'static str {
        match self {
            Self::Enter => "2054",
            Self::Tab => "2049",
            Self::Delete => "2055",
            Self::Back => "Back",
            Self::Home => "Home",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HdcInputText(String);

impl HdcInputText {
    pub const MAX_BYTES: usize = 1024;

    pub fn parse(value: impl Into<String>) -> HarmonyHdcResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= Self::MAX_BYTES
            && value.is_ascii()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b' ' | b'.' | b',' | b'_' | b'@' | b'+' | b'-')
            });
        if !valid {
            return Err(HarmonyHdcError::InvalidValue { field: "text" });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for HdcInputText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HdcInputText")
            .field("byte_len", &self.0.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarmonyBundleName(String);

impl HarmonyBundleName {
    pub fn parse(value: impl Into<String>) -> HarmonyHdcResult<Self> {
        let value = value.into();
        if !valid_qualified_name(&value, 3, 255, true) {
            return Err(HarmonyHdcError::InvalidValue {
                field: "bundleName",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HarmonyAbilityName(String);

impl HarmonyAbilityName {
    pub fn parse(value: impl Into<String>) -> HarmonyHdcResult<Self> {
        let value = value.into();
        if !valid_qualified_name(&value, 1, 255, false) {
            return Err(HarmonyHdcError::InvalidValue {
                field: "abilityName",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_qualified_name(value: &str, min: usize, max: usize, require_dot: bool) -> bool {
    (min..=max).contains(&value.len())
        && value.is_ascii()
        && (!require_dot || value.contains('.'))
        && value.split('.').all(|segment| {
            let mut bytes = segment.bytes();
            bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
                && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

/// Closed, typed HDC command set. There is deliberately no arbitrary shell
/// string or caller-provided argument vector variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HdcOperation {
    ListTargetsVerbose,
    Probe,
    GetProperty(HdcProperty),
    CaptureScreenshot,
    DumpLayout,
    Tap {
        x: u32,
        y: u32,
    },
    Swipe {
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        velocity_pps: u32,
    },
    InputText(HdcInputText),
    KeyPress(HarmonyKey),
    Launch {
        bundle: HarmonyBundleName,
        ability: HarmonyAbilityName,
    },
}

impl HdcOperation {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ListTargetsVerbose => "list_targets",
            Self::Probe => "probe",
            Self::GetProperty(_) => "get_property",
            Self::CaptureScreenshot => "capture_screenshot",
            Self::DumpLayout => "dump_layout",
            Self::Tap { .. } => "tap",
            Self::Swipe { .. } => "swipe",
            Self::InputText(_) => "input_text",
            Self::KeyPress(_) => "key_press",
            Self::Launch { .. } => "launch",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HdcCommand {
    target: Option<HdcTarget>,
    operation: HdcOperation,
}

impl HdcCommand {
    pub fn host(operation: HdcOperation) -> HarmonyHdcResult<Self> {
        if !matches!(operation, HdcOperation::ListTargetsVerbose) {
            return Err(HarmonyHdcError::InvalidConfiguration(
                "device operation requires an HDC target".to_owned(),
            ));
        }
        Ok(Self {
            target: None,
            operation,
        })
    }

    pub fn for_target(target: HdcTarget, operation: HdcOperation) -> HarmonyHdcResult<Self> {
        if matches!(operation, HdcOperation::ListTargetsVerbose) {
            return Err(HarmonyHdcError::InvalidConfiguration(
                "host discovery operation cannot carry an HDC target".to_owned(),
            ));
        }
        validate_device_operation(&operation)?;
        Ok(Self {
            target: Some(target),
            operation,
        })
    }

    pub fn target(&self) -> Option<&HdcTarget> {
        self.target.as_ref()
    }

    pub fn operation(&self) -> &HdcOperation {
        &self.operation
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HdcCommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl HdcCommandOutput {
    pub fn new(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self { stdout, stderr }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub fn stdout_text(&self, operation: &'static str) -> HarmonyHdcResult<&str> {
        std::str::from_utf8(&self.stdout).map_err(|_| HarmonyHdcError::InvalidOutput { operation })
    }

    pub fn stderr_text(&self, operation: &'static str) -> HarmonyHdcResult<&str> {
        std::str::from_utf8(&self.stderr).map_err(|_| HarmonyHdcError::InvalidOutput { operation })
    }
}

#[async_trait]
pub trait HdcCommandRunner: Send + Sync {
    async fn run(
        &self,
        command: HdcCommand,
        control: &ExecutionControl,
    ) -> HarmonyHdcResult<HdcCommandOutput>;
}

pub struct SystemHdcCommandRunner {
    config: SystemHdcConfig,
}

impl SystemHdcCommandRunner {
    pub fn new(config: SystemHdcConfig) -> HarmonyHdcResult<Self> {
        config.validate()?;
        Ok(Self { config })
    }

    async fn run_direct(
        &self,
        args: Vec<OsString>,
        operation: &'static str,
        control: &ExecutionControl,
        stdout_limit: usize,
    ) -> HarmonyHdcResult<HdcCommandOutput> {
        if control.is_cancelled() {
            return Err(HarmonyHdcError::Cancelled { operation });
        }
        if control.is_expired() {
            return Err(HarmonyHdcError::TimedOut { operation });
        }

        let mut command = Command::new(&self.config.executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            HarmonyHdcError::process_io(operation, &self.config.executable, error)
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarmonyHdcError::io(operation, "HDC stdout pipe was not available"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HarmonyHdcError::io(operation, "HDC stderr pipe was not available"))?;
        let process = async {
            let stdout = read_bounded(stdout, operation, "stdout", stdout_limit);
            let stderr = read_bounded(stderr, operation, "stderr", STDERR_OUTPUT_LIMIT);
            let status = async {
                child
                    .wait()
                    .await
                    .map_err(|error| HarmonyHdcError::io(operation, error))
            };
            let (stdout, stderr, status) = tokio::try_join!(stdout, stderr, status)?;
            Ok::<_, HarmonyHdcError>((stdout, stderr, status))
        };
        tokio::pin!(process);

        let process_timeout = time::sleep(self.config.command_timeout);
        tokio::pin!(process_timeout);
        let request_timeout = async {
            match control.remaining() {
                Some(remaining) => time::sleep(remaining).await,
                None => pending::<()>().await,
            }
        };
        tokio::pin!(request_timeout);

        let (stdout, stderr, status) = tokio::select! {
            _ = control.cancelled() => return Err(HarmonyHdcError::Cancelled { operation }),
            _ = &mut process_timeout => return Err(HarmonyHdcError::TimedOut { operation }),
            _ = &mut request_timeout => return Err(HarmonyHdcError::TimedOut { operation }),
            result = &mut process => result?,
        };
        if !status.success() {
            return Err(HarmonyHdcError::NonZeroExit {
                operation,
                status: status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            });
        }
        if reported_hdc_failure(&stdout) || reported_hdc_failure(&stderr) {
            return Err(HarmonyHdcError::ReportedFailure { operation });
        }
        Ok(HdcCommandOutput::new(stdout, stderr))
    }

    async fn capture_remote_file(
        &self,
        target: &HdcTarget,
        kind: RemoteArtifact,
        control: &ExecutionControl,
    ) -> HarmonyHdcResult<HdcCommandOutput> {
        let token = Uuid::new_v4().simple().to_string();
        let remote = format!("/data/local/tmp/devicerail-{token}.{}", kind.extension());
        let local = std::env::temp_dir().join(format!("devicerail-{token}.{}", kind.extension()));
        let mut guard = LocalArtifactGuard(local.clone());

        let create_args = target_args(target, kind.create_args(&remote));
        self.run_direct(create_args, kind.operation(), control, TEXT_OUTPUT_LIMIT)
            .await?;
        let recv_args = target_args(
            target,
            vec![
                OsString::from("file"),
                OsString::from("recv"),
                OsString::from(&remote),
                local.clone().into_os_string(),
            ],
        );
        let recv = self
            .run_direct(recv_args, kind.operation(), control, TEXT_OUTPUT_LIMIT)
            .await;

        // Cleanup gets its own short budget so a request cancellation cannot
        // turn an operation-scoped remote artifact into durable state.
        let cleanup = ExecutionControl::unbounded().with_timeout(2_000, TimeoutScope::Request);
        let cleanup_args = target_args(
            target,
            vec![
                OsString::from("shell"),
                OsString::from("rm"),
                OsString::from("-f"),
                OsString::from(&remote),
            ],
        );
        let cleanup_result = self
            .run_direct(cleanup_args, kind.operation(), &cleanup, TEXT_OUTPUT_LIMIT)
            .await;
        recv?;
        cleanup_result?;

        let metadata = tokio::fs::metadata(&local)
            .await
            .map_err(|error| HarmonyHdcError::io(kind.operation(), error))?;
        if metadata.len() > kind.limit() as u64 {
            return Err(HarmonyHdcError::OutputTooLarge {
                operation: kind.operation(),
                stream: "artifact",
                limit: kind.limit(),
            });
        }
        let bytes = tokio::fs::read(&local)
            .await
            .map_err(|error| HarmonyHdcError::io(kind.operation(), error))?;
        guard.remove();
        Ok(HdcCommandOutput::new(bytes, Vec::new()))
    }
}

async fn read_bounded<R>(
    mut reader: R,
    operation: &'static str,
    stream: &'static str,
    limit: usize,
) -> HarmonyHdcResult<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(|error| HarmonyHdcError::io(operation, error))?;
        if count == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(count) > limit {
            return Err(HarmonyHdcError::OutputTooLarge {
                operation,
                stream,
                limit,
            });
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[async_trait]
impl HdcCommandRunner for SystemHdcCommandRunner {
    async fn run(
        &self,
        command: HdcCommand,
        control: &ExecutionControl,
    ) -> HarmonyHdcResult<HdcCommandOutput> {
        match command.operation {
            HdcOperation::CaptureScreenshot => {
                let target = command.target.as_ref().ok_or_else(|| {
                    HarmonyHdcError::InvalidConfiguration(
                        "capture operation has no HDC target".to_owned(),
                    )
                })?;
                self.capture_remote_file(target, RemoteArtifact::Screenshot, control)
                    .await
            }
            HdcOperation::DumpLayout => {
                let target = command.target.as_ref().ok_or_else(|| {
                    HarmonyHdcError::InvalidConfiguration(
                        "layout operation has no HDC target".to_owned(),
                    )
                })?;
                self.capture_remote_file(target, RemoteArtifact::Layout, control)
                    .await
            }
            operation => {
                let operation_name = operation.name();
                let args = direct_args(command.target.as_ref(), &operation)?;
                self.run_direct(args, operation_name, control, TEXT_OUTPUT_LIMIT)
                    .await
            }
        }
    }
}

fn direct_args(
    target: Option<&HdcTarget>,
    operation: &HdcOperation,
) -> HarmonyHdcResult<Vec<OsString>> {
    let body = match operation {
        HdcOperation::ListTargetsVerbose => {
            return Ok(vec!["list".into(), "targets".into(), "-v".into()]);
        }
        HdcOperation::Probe => vec!["shell".into(), "echo".into(), "devicerail".into()],
        HdcOperation::GetProperty(property) => vec![
            "shell".into(),
            "param".into(),
            "get".into(),
            property.as_str().into(),
        ],
        HdcOperation::Tap { x, y } => vec![
            "shell".into(),
            "uitest".into(),
            "uiInput".into(),
            "click".into(),
            x.to_string().into(),
            y.to_string().into(),
        ],
        HdcOperation::Swipe {
            start_x,
            start_y,
            end_x,
            end_y,
            velocity_pps,
        } => vec![
            "shell".into(),
            "uitest".into(),
            "uiInput".into(),
            "swipe".into(),
            start_x.to_string().into(),
            start_y.to_string().into(),
            end_x.to_string().into(),
            end_y.to_string().into(),
            velocity_pps.to_string().into(),
        ],
        HdcOperation::InputText(text) => vec![
            "shell".into(),
            "uitest".into(),
            "uiInput".into(),
            "text".into(),
            text.as_str().into(),
        ],
        HdcOperation::KeyPress(key) => vec![
            "shell".into(),
            "uitest".into(),
            "uiInput".into(),
            "keyEvent".into(),
            key.key_code().into(),
        ],
        HdcOperation::Launch { bundle, ability } => vec![
            "shell".into(),
            "aa".into(),
            "start".into(),
            "-b".into(),
            bundle.as_str().into(),
            "-a".into(),
            ability.as_str().into(),
        ],
        HdcOperation::CaptureScreenshot | HdcOperation::DumpLayout => {
            return Err(HarmonyHdcError::InvalidConfiguration(
                "artifact operation requires the bounded transfer path".to_owned(),
            ));
        }
    };
    let target = target.ok_or_else(|| {
        HarmonyHdcError::InvalidConfiguration("device operation has no HDC target".to_owned())
    })?;
    Ok(target_args(target, body))
}

fn validate_device_operation(operation: &HdcOperation) -> HarmonyHdcResult<()> {
    let valid = match operation {
        HdcOperation::Tap { x, y } => *x <= MAX_COORDINATE && *y <= MAX_COORDINATE,
        HdcOperation::Swipe {
            start_x,
            start_y,
            end_x,
            end_y,
            velocity_pps,
        } => {
            [*start_x, *start_y, *end_x, *end_y]
                .into_iter()
                .all(|coordinate| coordinate <= MAX_COORDINATE)
                && (*start_x != *end_x || *start_y != *end_y)
                && (MIN_SWIPE_VELOCITY_PPS..=MAX_SWIPE_VELOCITY_PPS).contains(velocity_pps)
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(HarmonyHdcError::InvalidValue { field: "operation" })
    }
}

fn reported_hdc_failure(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|value| {
        value.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("[Fail]") || line.starts_with("Unknown operation command")
        })
    })
}

fn target_args(target: &HdcTarget, body: Vec<OsString>) -> Vec<OsString> {
    let mut args = Vec::with_capacity(body.len() + 2);
    args.push(OsString::from("-t"));
    args.push(OsString::from(target.as_str()));
    args.extend(body);
    args
}

#[derive(Clone, Copy)]
enum RemoteArtifact {
    Screenshot,
    Layout,
}

impl RemoteArtifact {
    const fn operation(self) -> &'static str {
        match self {
            Self::Screenshot => "capture_screenshot",
            Self::Layout => "dump_layout",
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Screenshot => "png",
            Self::Layout => "json",
        }
    }

    const fn limit(self) -> usize {
        match self {
            Self::Screenshot => SCREENSHOT_OUTPUT_LIMIT,
            Self::Layout => LAYOUT_OUTPUT_LIMIT,
        }
    }

    fn create_args(self, remote: &str) -> Vec<OsString> {
        match self {
            Self::Screenshot => vec![
                "shell".into(),
                "uitest".into(),
                "screenCap".into(),
                "-p".into(),
                remote.into(),
            ],
            Self::Layout => vec![
                "shell".into(),
                "uitest".into(),
                "dumpLayout".into(),
                "-p".into(),
                remote.into(),
            ],
        }
    }
}

struct LocalArtifactGuard(PathBuf);

impl LocalArtifactGuard {
    fn remove(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        self.0.clear();
    }
}

impl Drop for LocalArtifactGuard {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, time::Duration};

    use super::{
        HarmonyAbilityName, HarmonyBundleName, HdcInputText, HdcOperation, SystemHdcConfig,
        direct_args, reported_hdc_failure,
    };
    use crate::{HdcCommand, HdcTarget};

    #[test]
    fn typed_operations_build_shell_free_argument_vectors() {
        let target = HdcTarget::parse("192.0.2.1:8710").expect("target");
        let args = direct_args(
            Some(&target),
            &HdcOperation::Launch {
                bundle: HarmonyBundleName::parse("com.example.app").expect("bundle"),
                ability: HarmonyAbilityName::parse("EntryAbility").expect("ability"),
            },
        )
        .expect("arguments");
        assert_eq!(
            args,
            [
                "-t",
                "192.0.2.1:8710",
                "shell",
                "aa",
                "start",
                "-b",
                "com.example.app",
                "-a",
                "EntryAbility"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn caller_strings_cannot_become_remote_shell_fragments() {
        for value in ["", "hello;id", "hello$(id)", "line\nbreak", "你好"] {
            assert!(HdcInputText::parse(value).is_err());
        }
        assert!(HdcInputText::parse("Hello DeviceRail 42").is_ok());
        assert!(HarmonyBundleName::parse("com.example;id").is_err());
        assert!(HarmonyAbilityName::parse("Entry Ability").is_err());
    }

    #[test]
    fn public_typed_commands_still_enforce_runtime_bounds() {
        let target = HdcTarget::parse("FMR022").expect("target");
        assert!(
            HdcCommand::for_target(target.clone(), HdcOperation::Tap { x: 1_000_001, y: 0 },)
                .is_err()
        );
        assert!(
            HdcCommand::for_target(
                target,
                HdcOperation::Swipe {
                    start_x: 0,
                    start_y: 0,
                    end_x: 10,
                    end_y: 10,
                    velocity_pps: 199,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn system_configuration_is_explicit_and_bounded() {
        assert!(SystemHdcConfig::new("", Duration::from_secs(1)).is_err());
        assert!(SystemHdcConfig::new("hdc", Duration::ZERO).is_err());
        let config = SystemHdcConfig::new("/opt/hdc", Duration::from_secs(30)).expect("config");
        assert_eq!(config.executable().to_string_lossy(), "/opt/hdc");
        assert_eq!(config.command_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn zero_exit_hdc_failure_markers_are_not_treated_as_success() {
        assert!(reported_hdc_failure(
            b"[Fail]Device not founded or connected"
        ));
        assert!(reported_hdc_failure(b"Unknown operation command..."));
        assert!(!reported_hdc_failure(b"start ability successfully."));
    }
}
