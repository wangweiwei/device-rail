//! Host-side iOS discovery and WebDriverAgent lifecycle supervision.
//!
//! This crate deliberately sits above the Driver boundary. It invokes host
//! tools, but it contains no DeviceRail wire protocol, recorder, or UI logic.

use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, UNIX_EPOCH},
};

use async_trait::async_trait;
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use url::Url;

const MAX_DISCOVERY_BYTES: usize = 4 * 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_HTTP_BYTES: usize = 64 * 1024;
const DEFAULT_BUILD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_APPIUM_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const APPIUM_AUTO_PORT_ATTEMPTS: usize = 4;
const MAX_APPIUM_STARTUP_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const HEALTH_INTERVAL: Duration = Duration::from_secs(2);
const HEALTH_FAILURE_LIMIT: usize = 3;
const MAX_RECOVERY_BACKOFF: Duration = Duration::from_secs(30);
const RECOVERY_REBUILD_INTERVAL: u32 = 3;
const STAMP_VERSION: u32 = 3;
const MAX_SOURCE_DIFF_BYTES: usize = 16 * 1024 * 1024;

/// The host lifecycle required by an Apple device target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IosDeviceKind {
    #[default]
    Physical,
    Simulator,
}

/// A stable iOS device descriptor discovered from Apple host tools.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosHostDevice {
    pub udid: String,
    pub name: String,
    pub os_version: Option<String>,
    #[serde(default)]
    pub kind: IosDeviceKind,
    pub connected: bool,
    pub paired: Option<bool>,
    pub developer_mode: Option<bool>,
    pub developer_services: Option<bool>,
}

/// Which host API produced a device inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscoverySource {
    DeviceCtl,
    XcdeviceFallback,
    Simctl,
    DeviceCtlAndSimctl,
    XcdeviceFallbackAndSimctl,
}

/// Device discovery plus any bounded fallback diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDiscovery {
    pub source: DiscoverySource,
    pub devices: Vec<IosHostDevice>,
    pub warning_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticCheck {
    pub status: DiagnosticStatus,
    pub code: String,
    pub summary: String,
    pub remediation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDoctorReport {
    pub ready: bool,
    pub checks: Vec<DiagnosticCheck>,
    pub devices: Vec<IosHostDevice>,
}

impl IosDoctorReport {
    pub fn failed(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DiagnosticStatus::Fail)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DoctorOptions {
    pub device_udid: Option<String>,
    pub wda_project: Option<PathBuf>,
    pub iproxy_path: Option<PathBuf>,
    pub wda_endpoint: Option<String>,
    pub skip_iproxy_check: bool,
    pub skip_wda_build_checks: bool,
}

/// Managed WebDriverAgent host settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedIosConfig {
    pub device_udid: Option<String>,
    pub wda_project: PathBuf,
    pub derived_data: PathBuf,
    pub iproxy_path: PathBuf,
    pub local_port: u16,
    pub remote_port: u16,
    pub allow_provisioning_updates: bool,
    pub build_timeout: Duration,
    pub startup_timeout: Duration,
}

impl ManagedIosConfig {
    pub fn new(wda_project: impl Into<PathBuf>) -> Result<Self, IosHostError> {
        let wda_project = wda_project.into();
        if wda_project.as_os_str().is_empty() {
            return Err(IosHostError::new(
                "ios_wda_project_invalid",
                "the WebDriverAgent project path is empty",
            ));
        }
        Ok(Self {
            device_udid: None,
            wda_project,
            derived_data: PathBuf::from(".devicerail/ios/DerivedData"),
            iproxy_path: PathBuf::from("iproxy"),
            local_port: 0,
            remote_port: 8100,
            allow_provisioning_updates: false,
            build_timeout: DEFAULT_BUILD_TIMEOUT,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        })
    }

    /// Builds managed settings from the documented environment variables.
    pub fn from_environment() -> Result<Self, IosHostError> {
        let project = std::env::var_os("DEVICERAIL_IOS_WDA_PROJECT")
            .map(PathBuf::from)
            .or_else(discover_wda_project)
            .ok_or_else(|| {
                IosHostError::new(
                    "ios_wda_project_missing",
                    "set DEVICERAIL_IOS_WDA_PROJECT to WebDriverAgent.xcodeproj",
                )
            })?;
        let mut config = Self::new(project)?;
        config.device_udid = std::env::var("DEVICERAIL_IOS_DEVICE_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        if let Some(path) = std::env::var_os("DEVICERAIL_IOS_DERIVED_DATA") {
            config.derived_data = nonempty_path(path, "ios_derived_data_invalid")?;
        }
        if let Some(path) = std::env::var_os("DEVICERAIL_IOS_IPROXY_PATH") {
            config.iproxy_path = nonempty_path(path, "ios_iproxy_path_invalid")?;
        }
        if let Some(value) = std::env::var_os("DEVICERAIL_IOS_WDA_LOCAL_PORT") {
            config.local_port = parse_port(&value, true, "ios_local_port_invalid")?;
        }
        if let Some(value) = std::env::var_os("DEVICERAIL_IOS_WDA_REMOTE_PORT") {
            config.remote_port = parse_port(&value, false, "ios_remote_port_invalid")?;
        }
        if let Some(value) = std::env::var_os("DEVICERAIL_IOS_ALLOW_PROVISIONING_UPDATES") {
            config.allow_provisioning_updates = parse_bool(&value)?;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), IosHostError> {
        if self.wda_project.as_os_str().is_empty() {
            return Err(IosHostError::new(
                "ios_wda_project_invalid",
                "the WebDriverAgent project path is empty",
            ));
        }
        if self.derived_data.as_os_str().is_empty() {
            return Err(IosHostError::new(
                "ios_derived_data_invalid",
                "the DerivedData path is empty",
            ));
        }
        if self.iproxy_path.as_os_str().is_empty() {
            return Err(IosHostError::new(
                "ios_iproxy_path_invalid",
                "the iproxy path is empty",
            ));
        }
        if self.remote_port == 0 {
            return Err(IosHostError::new(
                "ios_remote_port_invalid",
                "the device WDA port must be non-zero",
            ));
        }
        if self.build_timeout.is_zero() || self.startup_timeout.is_zero() {
            return Err(IosHostError::new(
                "ios_timeout_invalid",
                "managed iOS timeouts must be non-zero",
            ));
        }
        if let Some(udid) = &self.device_udid {
            validate_text(udid, 256, "ios_device_udid_invalid")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedIosDevice {
    pub device: IosHostDevice,
    pub used_cached_build: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedIosEndpoint {
    pub device: IosHostDevice,
    pub wda_url: String,
}

/// A running Direct WDA supervisor with an optional physical-device tunnel.
/// Dropping it kills every owned child process.
pub struct ManagedIosRuntime {
    endpoint: ManagedIosEndpoint,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    _runtime_lock: HostFileLock,
}

impl std::fmt::Debug for ManagedIosRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedIosRuntime")
            .field("endpoint", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl ManagedIosRuntime {
    pub fn endpoint(&self) -> &ManagedIosEndpoint {
        &self.endpoint
    }

    pub async fn shutdown(mut self) -> Result<(), IosHostError> {
        let _ = self.shutdown.send(true);
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match timeout(Duration::from_secs(5), &mut task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(IosHostError::new(
                "ios_supervisor_task_failed",
                "supervisor task failed",
            )),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(IosHostError::new(
                    "ios_supervisor_shutdown_timeout",
                    "supervisor shutdown timed out",
                ))
            }
        }
    }
}

impl Drop for ManagedIosRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Settings for an Appium server process owned by DeviceRail.
///
/// The executable is selected by the operator. DeviceRail never downloads or
/// installs Appium, and it only supplies the fixed address, port, and base-path
/// arguments represented by this type.
#[derive(Clone, PartialEq, Eq)]
pub struct ManagedAppiumConfig {
    executable: PathBuf,
    port: u16,
    base_path: String,
    startup_timeout: Duration,
}

impl std::fmt::Debug for ManagedAppiumConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedAppiumConfig")
            .field("executable", &"[REDACTED]")
            .field("port", &self.port)
            .field("base_path", &self.base_path)
            .field("startup_timeout", &self.startup_timeout)
            .finish()
    }
}

impl ManagedAppiumConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Result<Self, IosHostError> {
        let config = Self {
            executable: executable.into(),
            port: 0,
            base_path: "/".to_owned(),
            startup_timeout: DEFAULT_APPIUM_STARTUP_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn with_port(mut self, port: u16) -> Result<Self, IosHostError> {
        self.port = port;
        self.validate()?;
        Ok(self)
    }

    pub fn with_base_path(mut self, base_path: impl Into<String>) -> Result<Self, IosHostError> {
        self.base_path = base_path.into();
        self.validate()?;
        Ok(self)
    }

    pub fn with_startup_timeout(mut self, timeout: Duration) -> Result<Self, IosHostError> {
        self.startup_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    pub const fn startup_timeout(&self) -> Duration {
        self.startup_timeout
    }

    pub fn validate(&self) -> Result<(), IosHostError> {
        if self.executable.as_os_str().is_empty() {
            return Err(IosHostError::new(
                "ios_appium_path_invalid",
                "the Appium executable path is empty",
            ));
        }
        if self.startup_timeout.is_zero() || self.startup_timeout > MAX_APPIUM_STARTUP_TIMEOUT {
            return Err(IosHostError::new(
                "ios_appium_timeout_invalid",
                "the Appium startup timeout must be between 1 ms and 120 seconds",
            ));
        }
        validate_appium_base_path(&self.base_path)?;
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ManagedAppiumEndpoint {
    url: String,
}

impl std::fmt::Debug for ManagedAppiumEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedAppiumEndpoint")
            .field("url", &"[REDACTED]")
            .finish()
    }
}

impl ManagedAppiumEndpoint {
    pub fn url(&self) -> &str {
        &self.url
    }
}

#[cfg(unix)]
trait ProcessGroupSignaler: Send + Sync {
    fn signal(&self, process_group: i32, signal: i32) -> std::io::Result<()>;
}

#[cfg(unix)]
struct SystemProcessGroupSignaler;

#[cfg(unix)]
impl ProcessGroupSignaler for SystemProcessGroupSignaler {
    fn signal(&self, process_group: i32, signal: i32) -> std::io::Result<()> {
        signal_process_group(process_group, signal)
    }
}

/// Exclusive process-group ownership held by the Appium supervisor task.
///
/// Successful cleanup disarms the numeric PGID before this guard is dropped.
/// Cancellation retains a final SIGKILL fallback while the task still owns the
/// group, without leaving a stale PGID copy in `ManagedAppiumRuntime`.
#[cfg(unix)]
struct OwnedProcessGroup {
    process_group: Option<i32>,
    signaler: Box<dyn ProcessGroupSignaler>,
}

#[cfg(unix)]
impl OwnedProcessGroup {
    fn new(process_group: i32) -> Self {
        Self {
            process_group: Some(process_group),
            signaler: Box::new(SystemProcessGroupSignaler),
        }
    }

    #[cfg(test)]
    fn with_signaler(process_group: i32, signaler: Box<dyn ProcessGroupSignaler>) -> Self {
        Self {
            process_group: Some(process_group),
            signaler,
        }
    }

    fn signal(&self, signal: i32) -> std::io::Result<()> {
        match self.process_group {
            Some(process_group) => self.signaler.signal(process_group, signal),
            None => Ok(()),
        }
    }

    fn kill_and_disarm(&mut self) -> std::io::Result<()> {
        self.signal(libc::SIGKILL)?;
        self.process_group = None;
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        if let Some(process_group) = self.process_group.take() {
            let _ = self.signaler.signal(process_group, libc::SIGKILL);
        }
    }
}

/// A locally owned Appium server. Dropping it requests supervisor-owned cleanup.
pub struct ManagedAppiumRuntime {
    endpoint: ManagedAppiumEndpoint,
    shutdown: watch::Sender<bool>,
    completion: Option<JoinHandle<Result<(), &'static str>>>,
    failure: watch::Receiver<Option<&'static str>>,
}

impl std::fmt::Debug for ManagedAppiumRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedAppiumRuntime")
            .field("endpoint", &"[REDACTED]")
            .field("failure_code", &self.failure_code())
            .finish_non_exhaustive()
    }
}

impl ManagedAppiumRuntime {
    pub fn endpoint(&self) -> &ManagedAppiumEndpoint {
        &self.endpoint
    }

    pub fn failure_code(&self) -> Option<&'static str> {
        *self.failure.borrow()
    }

    pub async fn wait_for_failure(&mut self) -> &'static str {
        loop {
            if let Some(code) = *self.failure.borrow() {
                return code;
            }
            if self.failure.changed().await.is_err() {
                return "ios_appium_task_failed";
            }
        }
    }

    pub async fn shutdown(mut self) -> Result<(), IosHostError> {
        let _ = self.shutdown.send(true);
        let Some(mut completion) = self.completion.take() else {
            return Ok(());
        };
        match timeout(Duration::from_secs(5), &mut completion).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(code))) => Err(IosHostError::new(code, "managed Appium process failed")),
            Ok(Err(_)) => Err(IosHostError::new(
                "ios_appium_task_failed",
                "managed Appium supervisor task failed",
            )),
            Err(_) => {
                // Cancellation drops the still-armed supervisor guard, which
                // applies the SIGKILL fallback exactly once. A converged guard
                // was already disarmed synchronously before the task yielded.
                completion.abort();
                let _ = completion.await;
                Err(IosHostError::new(
                    "ios_appium_shutdown_timeout",
                    "managed Appium shutdown timed out",
                ))
            }
        }
    }
}

impl Drop for ManagedAppiumRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        // The detached supervisor exclusively owns the Child and, on Unix,
        // the process-group guard. It converges cleanup or applies its own
        // fail-safe kill if the task is cancelled with an active group.
        if let Some(completion) = self.completion.take() {
            drop(completion);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAppiumHost;

impl SystemAppiumHost {
    pub async fn start(
        &self,
        config: ManagedAppiumConfig,
    ) -> Result<ManagedAppiumRuntime, IosHostError> {
        config.validate()?;
        let version = run_output(
            config.executable(),
            &[OsString::from("--version")],
            DEFAULT_COMMAND_TIMEOUT,
            64 * 1024,
        )
        .await
        .map_err(|_| {
            IosHostError::new(
                "ios_appium_executable_unavailable",
                "the configured Appium executable could not be started",
            )
        })?;
        if !version.success {
            return Err(IosHostError::new(
                "ios_appium_executable_unavailable",
                "the configured Appium executable rejected --version",
            ));
        }

        let automatic_port = config.port() == 0;
        let attempts = if automatic_port {
            APPIUM_AUTO_PORT_ATTEMPTS
        } else {
            1
        };
        let mut last_error = None;

        for _ in 0..attempts {
            let port = reserve_local_port(config.port()).await.map_err(|_| {
                IosHostError::new(
                    "ios_appium_port_unavailable",
                    "the requested local Appium port is unavailable",
                )
            })?;
            let endpoint = appium_endpoint(port, config.base_path());
            let probe = parse_wda_probe(&endpoint).ok_or_else(|| {
                IosHostError::new(
                    "ios_appium_configuration_invalid",
                    "the managed Appium endpoint could not be constructed",
                )
            })?;

            let mut command = Command::new(config.executable());
            command
                .arg("--address")
                .arg("127.0.0.1")
                .arg("--port")
                .arg(port.to_string())
                .arg("--base-path")
                .arg(config.base_path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(unix)]
            command.process_group(0);
            let mut child = command.spawn().map_err(|_| {
                IosHostError::new(
                    "ios_appium_start_failed",
                    "the managed Appium server could not be started",
                )
            })?;
            #[cfg(unix)]
            let mut process_group = child
                .id()
                .and_then(|id| i32::try_from(id).ok())
                .map(OwnedProcessGroup::new)
                .ok_or_else(|| {
                    IosHostError::new(
                        "ios_appium_process_group_invalid",
                        "the managed Appium process group is unavailable",
                    )
                })?;
            if let Err(error) =
                wait_for_appium_ready(&mut child, &probe, config.startup_timeout()).await
            {
                #[cfg(unix)]
                let _ = process_group.kill_and_disarm();
                let _ = child.kill().await;
                let _ = child.wait().await;
                if automatic_port && error.code() == "ios_appium_exited" {
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }

            let (shutdown, receiver) = watch::channel(false);
            let (failure_sender, failure) = watch::channel(None);
            let completion = tokio::spawn(supervise_appium(
                child,
                receiver,
                failure_sender,
                #[cfg(unix)]
                process_group,
            ));
            return Ok(ManagedAppiumRuntime {
                endpoint: ManagedAppiumEndpoint { url: endpoint },
                shutdown,
                completion: Some(completion),
                failure,
            });
        }

        Err(last_error.unwrap_or_else(|| {
            IosHostError::new(
                "ios_appium_start_failed",
                "the managed Appium server could not be started",
            )
        }))
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct IosHostError {
    code: &'static str,
    message: String,
}

impl IosHostError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded_message(message.into()),
        }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

#[async_trait]
pub trait IosHostBackend: Send + Sync {
    async fn discover(&self) -> Result<IosDiscovery, IosHostError>;
    async fn doctor(&self, options: &DoctorOptions) -> IosDoctorReport;
    async fn prepare(&self, config: &ManagedIosConfig) -> Result<PreparedIosDevice, IosHostError>;
    async fn start(&self, config: ManagedIosConfig) -> Result<ManagedIosRuntime, IosHostError>;
}

#[derive(Clone, Debug)]
pub struct SystemIosHost {
    xcrun: PathBuf,
    xcodebuild: PathBuf,
    security: PathBuf,
    git: PathBuf,
}

impl Default for SystemIosHost {
    fn default() -> Self {
        Self {
            xcrun: PathBuf::from("xcrun"),
            xcodebuild: PathBuf::from("xcodebuild"),
            security: PathBuf::from("security"),
            git: PathBuf::from("git"),
        }
    }
}

#[async_trait]
impl IosHostBackend for SystemIosHost {
    async fn discover(&self) -> Result<IosDiscovery, IosHostError> {
        self.discover_system_devices().await
    }

    async fn doctor(&self, options: &DoctorOptions) -> IosDoctorReport {
        self.doctor_system(options).await
    }

    async fn prepare(&self, config: &ManagedIosConfig) -> Result<PreparedIosDevice, IosHostError> {
        self.prepare_standalone(config).await
    }

    async fn start(&self, config: ManagedIosConfig) -> Result<ManagedIosRuntime, IosHostError> {
        self.start_system(config).await
    }
}

impl SystemIosHost {
    async fn prepare_standalone(
        &self,
        config: &ManagedIosConfig,
    ) -> Result<PreparedIosDevice, IosHostError> {
        config.validate()?;
        if !valid_wda_project(&config.wda_project).await {
            return Err(IosHostError::new(
                "ios_wda_project_missing",
                "WebDriverAgent.xcodeproj or project.pbxproj is missing",
            ));
        }
        ensure_macos_host()?;
        fs::create_dir_all(&config.derived_data)
            .await
            .map_err(|_| {
                IosHostError::new(
                    "ios_derived_data_create_failed",
                    "could not create DerivedData",
                )
            })?;
        let _runtime_lock = acquire_file_lock(
            &config.derived_data.join("devicerail-ios-runtime.lock"),
            Duration::ZERO,
            "ios_managed_runtime_busy",
        )
        .await?;
        self.prepare_system(config, false).await
    }

    async fn discover_system_devices(&self) -> Result<IosDiscovery, IosHostError> {
        let directory = tempdir().map_err(|_| {
            IosHostError::new(
                "ios_device_discovery_failed",
                "could not create discovery workspace",
            )
        })?;
        let output_path = directory.path().join("devices.json");
        let args = vec![
            OsString::from("devicectl"),
            OsString::from("--quiet"),
            OsString::from("--timeout"),
            OsString::from("15"),
            OsString::from("--json-output"),
            output_path.as_os_str().to_owned(),
            OsString::from("list"),
            OsString::from("devices"),
        ];
        let simctl_args = [
            OsString::from("simctl"),
            OsString::from("list"),
            OsString::from("--json"),
        ];
        let (devicectl, simctl) = tokio::join!(
            run_output(
                &self.xcrun,
                &args,
                DEFAULT_COMMAND_TIMEOUT,
                MAX_COMMAND_BYTES,
            ),
            run_output(
                &self.xcrun,
                &simctl_args,
                DEFAULT_COMMAND_TIMEOUT,
                MAX_DISCOVERY_BYTES,
            ),
        );
        let simulator_devices = simctl
            .ok()
            .filter(|output| output.success)
            .and_then(|output| parse_simctl_devices(&output.stdout).ok());
        let physical_devices = match devicectl {
            Ok(output) if output.success => read_bounded_file(&output_path, MAX_DISCOVERY_BYTES)
                .await
                .ok()
                .and_then(|bytes| parse_devicectl_devices(&bytes).ok()),
            _ => None,
        };
        if let Some(mut devices) = physical_devices {
            let (source, warning_code) = if let Some(simulators) = simulator_devices.as_ref() {
                devices.extend(simulators.iter().cloned());
                (DiscoverySource::DeviceCtlAndSimctl, None)
            } else {
                (
                    DiscoverySource::DeviceCtl,
                    Some("ios_simctl_unavailable".to_owned()),
                )
            };
            normalize_devices(&mut devices);
            return Ok(IosDiscovery {
                source,
                devices,
                warning_code,
            });
        }

        let fallback = run_output(
            &self.xcrun,
            &[OsString::from("xcdevice"), OsString::from("list")],
            DEFAULT_COMMAND_TIMEOUT,
            MAX_DISCOVERY_BYTES,
        )
        .await;
        let fallback_devices = fallback
            .ok()
            .filter(|output| output.success)
            .and_then(|output| parse_xcdevice_physical_devices(&output.stdout).ok());
        match (fallback_devices, simulator_devices) {
            (Some(mut devices), Some(simulators)) => {
                devices.extend(simulators);
                normalize_devices(&mut devices);
                Ok(IosDiscovery {
                    source: DiscoverySource::XcdeviceFallbackAndSimctl,
                    devices,
                    warning_code: Some("ios_devicectl_unavailable".to_owned()),
                })
            }
            (Some(mut devices), None) => {
                normalize_devices(&mut devices);
                Ok(IosDiscovery {
                    source: DiscoverySource::XcdeviceFallback,
                    devices,
                    warning_code: Some("ios_simctl_unavailable".to_owned()),
                })
            }
            (None, Some(mut devices)) => {
                normalize_devices(&mut devices);
                Ok(IosDiscovery {
                    source: DiscoverySource::Simctl,
                    devices,
                    warning_code: Some("ios_physical_discovery_unavailable".to_owned()),
                })
            }
            (None, None) => Err(IosHostError::new(
                "ios_device_discovery_failed",
                "devicectl, xcdevice, and simctl discovery failed",
            )),
        }
    }

    async fn doctor_system(&self, options: &DoctorOptions) -> IosDoctorReport {
        let mut checks = Vec::new();
        if !cfg!(target_os = "macos") {
            checks.push(fail(
                "ios_macos_required",
                "managed Xcode WDA requires macOS",
                "use external WDA or an optional cross-platform backend",
            ));
        }

        match run_output(
            &self.xcodebuild,
            &[OsString::from("-version")],
            DEFAULT_COMMAND_TIMEOUT,
            64 * 1024,
        )
        .await
        {
            Ok(output) if output.success => checks.push(pass(
                "ios_xcode_ready",
                "Xcode command-line tools are available",
            )),
            _ => checks.push(fail(
                "ios_xcode_unavailable",
                "xcodebuild is unavailable",
                "install Xcode and select it with xcode-select",
            )),
        }

        match run_output(
            &self.xcrun,
            &[OsString::from("--find"), OsString::from("devicectl")],
            DEFAULT_COMMAND_TIMEOUT,
            64 * 1024,
        )
        .await
        {
            Ok(output) if output.success => {
                checks.push(pass("ios_devicectl_ready", "devicectl is available"))
            }
            _ => checks.push(warn(
                "ios_devicectl_unavailable",
                "devicectl is unavailable; xcdevice fallback may be incomplete",
                "install a current Xcode release and select its developer directory",
            )),
        }

        let discovery = self.discover_system_devices().await;
        let mut devices = Vec::new();
        let mut selected_device = None;
        match discovery {
            Ok(discovery) => {
                if let Some(code) = discovery.warning_code {
                    let (summary, remediation) = match code.as_str() {
                        "ios_simctl_unavailable" => (
                            "Simulator inventory is unavailable",
                            "select a current Xcode release and retry simctl discovery",
                        ),
                        "ios_physical_discovery_unavailable" => (
                            "physical-device inventory is unavailable",
                            "retry after CoreDevice or xcdevice becomes available",
                        ),
                        _ => (
                            "physical-device inventory used the xcdevice fallback",
                            "retry after CoreDevice/devicectl becomes available",
                        ),
                    };
                    checks.push(warn(&code, summary, remediation));
                }
                devices = discovery.devices;
                match select_device(&devices, options.device_udid.as_deref()) {
                    Ok(device) => {
                        selected_device = Some(device.clone());
                        add_device_checks(&mut checks, device);
                    }
                    Err(error) => checks.push(fail(
                        error.code(),
                        &error.message,
                        "connect one physical device or boot one Simulator, or select a UDID explicitly",
                    )),
                }
            }
            Err(error) => checks.push(fail(
                error.code(),
                &error.message,
                "open Xcode Devices and Simulators, connect or boot a target, and retry",
            )),
        }
        let physical_requirements =
            requires_physical_host_support(selected_device.as_ref(), &devices);

        if physical_requirements && !options.skip_iproxy_check && !options.skip_wda_build_checks {
            let iproxy = options
                .iproxy_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("iproxy"));
            match run_output(
                &iproxy,
                &[OsString::from("--version")],
                DEFAULT_COMMAND_TIMEOUT,
                64 * 1024,
            )
            .await
            {
                Ok(output) if output.success => {
                    checks.push(pass("ios_iproxy_ready", "iproxy is available"))
                }
                _ => checks.push(fail(
                    "ios_iproxy_unavailable",
                    "iproxy is unavailable",
                    "install libimobiledevice/usbmuxd iproxy or configure DEVICERAIL_IOS_IPROXY_PATH",
                )),
            }
        }

        if !options.skip_wda_build_checks {
            let project = options.wda_project.clone().or_else(discover_wda_project);
            match project.as_ref() {
                Some(path) if valid_wda_project(path).await => {
                    checks.push(pass(
                        "ios_wda_project_ready",
                        "Appium WebDriverAgent project is available",
                    ));
                    if physical_requirements {
                        match self.wda_signing_settings(path).await {
                            Ok(settings)
                                if settings.development_team.is_some()
                                    && settings.bundle_identifier.is_some() =>
                            {
                                checks.push(pass(
                                    "ios_wda_signing_config_ready",
                                    "WDA resolves a development team and bundle identifier",
                                ));
                            }
                            Ok(_) => checks.push(fail(
                                "ios_wda_signing_config_missing",
                                "WDA does not resolve a development team and bundle identifier",
                                "select the WebDriverAgentRunner target in Xcode and configure Signing & Capabilities",
                            )),
                            Err(error) => checks.push(fail(
                                error.code(),
                                &error.message,
                                "open the Appium WDA project in Xcode and repair its build settings",
                            )),
                        }
                    } else {
                        checks.push(pass(
                            "ios_simulator_signing_not_required",
                            "code signing is not required for the selected Simulator",
                        ));
                    }
                }
                _ => checks.push(fail(
                    "ios_wda_project_missing",
                    "Appium WebDriverAgent.xcodeproj was not found",
                    "run `appium driver install xcuitest`, set DEVICERAIL_IOS_WDA_PROJECT, or pass --wda-project",
                )),
            }
        }

        if cfg!(target_os = "macos") && physical_requirements && !options.skip_wda_build_checks {
            match run_output(
                &self.security,
                &[
                    OsString::from("find-identity"),
                    OsString::from("-v"),
                    OsString::from("-p"),
                    OsString::from("codesigning"),
                ],
                DEFAULT_COMMAND_TIMEOUT,
                256 * 1024,
            )
            .await
            {
                Ok(output)
                    if output.success
                        && !String::from_utf8_lossy(&output.stdout)
                            .contains("0 valid identities found") =>
                {
                    checks.push(pass(
                        "ios_signing_identity_ready",
                        "a code-signing identity is available",
                    ));
                }
                _ => checks.push(fail(
                    "ios_signing_identity_missing",
                    "no usable code-signing identity was found",
                    "sign in to Xcode and create or import an Apple Development identity",
                )),
            }
        }

        if physical_requirements {
            checks.push(warn(
                "ios_ui_automation_confirmation_required",
                "UI Automation cannot be verified reliably from the host",
                "confirm Settings > Developer > Enable UI Automation on the device",
            ));
        }

        if let Some(endpoint) = &options.wda_endpoint {
            match parse_wda_probe(endpoint).ok_or_else(|| {
                IosHostError::new(
                    "ios_wda_endpoint_invalid",
                    "WDA endpoint is not numeric loopback HTTP",
                )
            }) {
                Ok(probe) if wda_ready_at(&probe).await => {
                    checks.push(pass("ios_wda_ready", "WDA is ready"))
                }
                Ok(_) => checks.push(warn(
                    "ios_wda_unreachable",
                    "the configured WDA endpoint is not ready",
                    "run managed prepare/serve or restart the external WDA tunnel",
                )),
                Err(error) => checks.push(fail(
                    error.code(),
                    &error.message,
                    "use a numeric loopback HTTP endpoint",
                )),
            }
        }

        let ready = !checks
            .iter()
            .any(|check| check.status == DiagnosticStatus::Fail);
        IosDoctorReport {
            ready,
            checks,
            devices,
        }
    }

    async fn prepare_system(
        &self,
        config: &ManagedIosConfig,
        force: bool,
    ) -> Result<PreparedIosDevice, IosHostError> {
        config.validate()?;
        if !valid_wda_project(&config.wda_project).await {
            return Err(IosHostError::new(
                "ios_wda_project_missing",
                "WebDriverAgent.xcodeproj or project.pbxproj is missing",
            ));
        }
        let discovery = self.discover_system_devices().await?;
        let device = select_device(&discovery.devices, config.device_udid.as_deref())?.clone();
        ensure_device_ready(&device)?;
        fs::create_dir_all(&config.derived_data)
            .await
            .map_err(|_| {
                IosHostError::new(
                    "ios_derived_data_create_failed",
                    "could not create DerivedData",
                )
            })?;
        let _build_lock = acquire_file_lock(
            &config.derived_data.join("devicerail-wda-build.lock"),
            config.build_timeout,
            "ios_wda_build_busy",
        )
        .await?;

        let fingerprint = self.build_fingerprint(config, &device).await?;
        let stamp_path = config.derived_data.join("devicerail-wda-build.json");
        let products = config.derived_data.join("Build/Products");
        let cached = !force
            && fingerprint.source_state.is_some()
            && fs::try_exists(&products).await.unwrap_or(false)
            && read_build_stamp(&stamp_path).await.as_ref() == Some(&fingerprint);
        if !cached {
            let mut args = xcodebuild_base_args(config, &device);
            if device.kind == IosDeviceKind::Physical && config.allow_provisioning_updates {
                args.push(OsString::from("-allowProvisioningUpdates"));
            }
            args.push(OsString::from("build-for-testing"));
            let output = run_output(
                &self.xcodebuild,
                &args,
                config.build_timeout,
                MAX_COMMAND_BYTES,
            )
            .await?;
            if !output.success {
                return Err(classify_xcodebuild_failure(&output));
            }
            write_build_stamp(&stamp_path, &fingerprint).await?;
        }
        Ok(PreparedIosDevice {
            device,
            used_cached_build: cached,
        })
    }

    async fn wda_signing_settings(
        &self,
        project: &Path,
    ) -> Result<WdaSigningSettings, IosHostError> {
        let output = run_output(
            &self.xcodebuild,
            &[
                OsString::from("-project"),
                project.as_os_str().to_owned(),
                OsString::from("-scheme"),
                OsString::from("WebDriverAgentRunner"),
                OsString::from("-sdk"),
                OsString::from("iphoneos"),
                OsString::from("-showBuildSettings"),
                OsString::from("-json"),
            ],
            DEFAULT_COMMAND_TIMEOUT,
            MAX_COMMAND_BYTES,
        )
        .await
        .map_err(|_| {
            IosHostError::new(
                "ios_wda_signing_config_unavailable",
                "WDA signing build settings could not be inspected",
            )
        })?;
        if !output.success {
            return Err(IosHostError::new(
                "ios_wda_signing_config_unavailable",
                "WDA signing build settings could not be inspected",
            ));
        }
        parse_wda_signing_settings(&output.stdout)
    }

    async fn start_system(
        &self,
        config: ManagedIosConfig,
    ) -> Result<ManagedIosRuntime, IosHostError> {
        config.validate()?;
        if !valid_wda_project(&config.wda_project).await {
            return Err(IosHostError::new(
                "ios_wda_project_missing",
                "WebDriverAgent.xcodeproj or project.pbxproj is missing",
            ));
        }
        ensure_macos_host()?;
        fs::create_dir_all(&config.derived_data)
            .await
            .map_err(|_| {
                IosHostError::new(
                    "ios_derived_data_create_failed",
                    "could not create DerivedData",
                )
            })?;
        let runtime_lock = acquire_file_lock(
            &config.derived_data.join("devicerail-ios-runtime.lock"),
            Duration::ZERO,
            "ios_managed_runtime_busy",
        )
        .await?;
        let mut prepared = self.prepare_system(&config, false).await?;
        let local_port = reserve_local_port(config.local_port).await?;
        let mut bundle = match launch_processes(self, &config, &prepared.device, local_port).await {
            Ok(bundle) => bundle,
            Err(error) if prepared.used_cached_build => {
                prepared = self.prepare_system(&config, true).await?;
                launch_processes(self, &config, &prepared.device, local_port)
                    .await
                    .map_err(|_| error)?
            }
            Err(error) => return Err(error),
        };
        if let Err(first_error) =
            wait_until_ready(&mut bundle, local_port, config.startup_timeout).await
        {
            terminate_bundle(&mut bundle).await;
            if !prepared.used_cached_build {
                return Err(first_error);
            }
            prepared = self.prepare_system(&config, true).await?;
            bundle = launch_processes(self, &config, &prepared.device, local_port).await?;
            wait_until_ready(&mut bundle, local_port, config.startup_timeout).await?;
        }

        let endpoint = ManagedIosEndpoint {
            device: prepared.device.clone(),
            wda_url: format!("http://127.0.0.1:{local_port}"),
        };
        let (shutdown, receiver) = watch::channel(false);
        let host = self.clone();
        let task_config = config.clone();
        let task_device = prepared.device;
        let task = tokio::spawn(async move {
            supervise(host, task_config, task_device, local_port, bundle, receiver).await;
        });
        Ok(ManagedIosRuntime {
            endpoint,
            shutdown,
            task: Some(task),
            _runtime_lock: runtime_lock,
        })
    }

    async fn build_fingerprint(
        &self,
        config: &ManagedIosConfig,
        device: &IosHostDevice,
    ) -> Result<BuildStamp, IosHostError> {
        let xcode = run_output(
            &self.xcodebuild,
            &[OsString::from("-version")],
            DEFAULT_COMMAND_TIMEOUT,
            64 * 1024,
        )
        .await?;
        if !xcode.success {
            return Err(IosHostError::new(
                "ios_xcode_unavailable",
                "xcodebuild -version failed",
            ));
        }
        let project = config
            .wda_project
            .canonicalize()
            .unwrap_or_else(|_| config.wda_project.clone());
        let pbx = config.wda_project.join("project.pbxproj");
        let metadata = fs::metadata(&pbx).await.map_err(|_| {
            IosHostError::new(
                "ios_wda_project_invalid",
                "project.pbxproj metadata is unavailable",
            )
        })?;
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let repository = config.wda_project.parent().unwrap_or(Path::new("."));
        let revision = run_output(
            &self.git,
            &[
                OsString::from("-C"),
                repository.as_os_str().to_owned(),
                OsString::from("rev-parse"),
                OsString::from("--verify"),
                OsString::from("HEAD"),
            ],
            DEFAULT_COMMAND_TIMEOUT,
            64 * 1024,
        )
        .await
        .ok()
        .filter(|output| output.success)
        .and_then(|output| {
            String::from_utf8(output.stdout)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| {
                    (7..=128).contains(&value.len())
                        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        });
        let source_state = self.source_state(repository, revision.as_deref()).await;
        Ok(BuildStamp {
            version: STAMP_VERSION,
            project: project.to_string_lossy().into_owned(),
            device_udid: device.udid.clone(),
            device_kind: device.kind,
            xcode_version: bounded_message(String::from_utf8_lossy(&xcode.stdout).into_owned()),
            project_length: metadata.len(),
            project_modified_ns: modified_ns,
            source_state,
        })
    }

    async fn source_state(&self, repository: &Path, revision: Option<&str>) -> Option<String> {
        let revision = revision?;
        let diff = run_output(
            &self.git,
            &[
                OsString::from("-C"),
                repository.as_os_str().to_owned(),
                OsString::from("diff"),
                OsString::from("--binary"),
                OsString::from("--no-ext-diff"),
                OsString::from("HEAD"),
                OsString::from("--"),
            ],
            DEFAULT_COMMAND_TIMEOUT,
            MAX_SOURCE_DIFF_BYTES,
        )
        .await
        .ok()
        .filter(|output| output.success)?;
        let untracked = run_output(
            &self.git,
            &[
                OsString::from("-C"),
                repository.as_os_str().to_owned(),
                OsString::from("ls-files"),
                OsString::from("--others"),
                OsString::from("--exclude-standard"),
                OsString::from("-z"),
            ],
            DEFAULT_COMMAND_TIMEOUT,
            1024 * 1024,
        )
        .await
        .ok()
        .filter(|output| output.success)?;
        if !untracked.stdout.is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(b"devicerail-ios-wda-source-v1\0");
        digest.update(revision.as_bytes());
        digest.update(b"\0");
        digest.update(&diff.stdout);
        Some(hex::encode(digest.finalize()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildStamp {
    version: u32,
    project: String,
    device_udid: String,
    #[serde(default)]
    device_kind: IosDeviceKind,
    xcode_version: String,
    project_length: u64,
    project_modified_ns: u128,
    source_state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WdaSigningSettings {
    development_team: Option<String>,
    bundle_identifier: Option<String>,
}

struct ProcessBundle {
    iproxy: Option<Child>,
    wda: Child,
}

#[derive(Debug)]
struct HostFileLock(File);

impl Drop for HostFileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

async fn acquire_file_lock(
    path: &Path,
    wait: Duration,
    code: &'static str,
) -> Result<HostFileLock, IosHostError> {
    let path = path.to_owned();
    let file = tokio::task::spawn_blocking(move || {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
    })
    .await
    .map_err(|_| IosHostError::new(code, "iOS lifecycle lock task failed"))?
    .map_err(|_| IosHostError::new(code, "iOS lifecycle lock could not be opened"))?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(HostFileLock(file)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if wait.is_zero() || Instant::now() >= deadline {
                    return Err(IosHostError::new(
                        code,
                        "another process owns the iOS lifecycle lock",
                    ));
                }
                sleep(Duration::from_millis(250)).await;
            }
            Err(_) => {
                return Err(IosHostError::new(
                    code,
                    "iOS lifecycle lock could not be acquired",
                ));
            }
        }
    }
}

async fn launch_processes(
    host: &SystemIosHost,
    config: &ManagedIosConfig,
    device: &IosHostDevice,
    local_port: u16,
) -> Result<ProcessBundle, IosHostError> {
    let iproxy =
        if device.kind == IosDeviceKind::Physical {
            let mapping = format!("{local_port}:{}", config.remote_port);
            let mut command = Command::new(&config.iproxy_path);
            command
                .arg("-u")
                .arg(&device.udid)
                .arg("-s")
                .arg("127.0.0.1")
                .arg(mapping)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            Some(command.spawn().map_err(|_| {
                IosHostError::new("ios_iproxy_start_failed", "could not start iproxy")
            })?)
        } else {
            None
        };

    let mut args = xcodebuild_base_args(config, device);
    if device.kind == IosDeviceKind::Physical && config.allow_provisioning_updates {
        args.push(OsString::from("-allowProvisioningUpdates"));
    }
    args.push(OsString::from("test-without-building"));
    let mut wda = Command::new(&host.xcodebuild);
    wda.args(args)
        .env(
            "USE_PORT",
            wda_listen_port(config, device, local_port).to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let wda = match wda.spawn() {
        Ok(child) => child,
        Err(_) => {
            if let Some(mut iproxy) = iproxy {
                let _ = iproxy.kill().await;
            }
            return Err(IosHostError::new(
                "ios_wda_launch_failed",
                "could not start xcodebuild test-without-building",
            ));
        }
    };
    Ok(ProcessBundle { iproxy, wda })
}

fn wda_listen_port(config: &ManagedIosConfig, device: &IosHostDevice, local_port: u16) -> u16 {
    match device.kind {
        IosDeviceKind::Physical => config.remote_port,
        IosDeviceKind::Simulator => local_port,
    }
}

async fn wait_until_ready(
    bundle: &mut ProcessBundle,
    local_port: u16,
    startup_timeout: Duration,
) -> Result<(), IosHostError> {
    let deadline = Instant::now() + startup_timeout;
    loop {
        if let Some(iproxy) = bundle.iproxy.as_mut() {
            if process_exited(iproxy)? {
                return Err(IosHostError::new(
                    "ios_iproxy_exited",
                    "iproxy exited before WDA became ready",
                ));
            }
        }
        if process_exited(&mut bundle.wda)? {
            return Err(IosHostError::new(
                "ios_wda_launch_failed",
                "xcodebuild exited before WDA became ready",
            ));
        }
        if wda_ready(local_port).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(IosHostError::new(
                "ios_wda_startup_timeout",
                "WDA did not become ready before the startup deadline",
            ));
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn supervise(
    host: SystemIosHost,
    config: ManagedIosConfig,
    device: IosHostDevice,
    local_port: u16,
    mut bundle: ProcessBundle,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut recovery_config = config.clone();
    // Once a route is published, its identity must not drift to a different
    // target that happens to become ready later. Recovery is pinned
    // to the original UDID even when initial selection was automatic.
    recovery_config.device_udid = Some(device.udid.clone());
    let mut health_failures = 0usize;
    let mut restart_attempt = 0u32;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = sleep(HEALTH_INTERVAL) => {
                let iproxy_exited = bundle
                    .iproxy
                    .as_mut()
                    .is_some_and(|iproxy| process_exited(iproxy).unwrap_or(true));
                let exited = iproxy_exited || process_exited(&mut bundle.wda).unwrap_or(true);
                if exited || !wda_ready(local_port).await {
                    health_failures = health_failures.saturating_add(1);
                } else {
                    health_failures = 0;
                    restart_attempt = 0;
                }
                if exited || health_failures >= HEALTH_FAILURE_LIMIT {
                    terminate_bundle(&mut bundle).await;
                    eprintln!("DeviceRail managed iOS route is recovering (ios_wda_recovering)");
                    loop {
                        if *shutdown.borrow() {
                            return;
                        }
                        restart_attempt = restart_attempt.saturating_add(1);
                        let exponent = restart_attempt.min(5);
                        let backoff = Duration::from_secs(1u64 << exponent).min(MAX_RECOVERY_BACKOFF);
                        tokio::select! {
                            changed = shutdown.changed() => {
                                if changed.is_err() || *shutdown.borrow() {
                                    return;
                                }
                            }
                            _ = sleep(backoff) => {}
                        }
                        let force_rebuild = restart_attempt % RECOVERY_REBUILD_INTERVAL == 0;
                        let prepared = match host
                            .prepare_system(&recovery_config, force_rebuild)
                            .await
                        {
                            Ok(prepared) => prepared,
                            Err(_) => continue,
                        };
                        if let Ok(mut candidate) = launch_processes(
                            &host,
                            &recovery_config,
                            &prepared.device,
                            local_port,
                        )
                        .await
                        {
                            match wait_until_ready(
                                &mut candidate,
                                local_port,
                                config.startup_timeout,
                            )
                            .await
                            {
                                Ok(()) => {
                                    bundle = candidate;
                                    health_failures = 0;
                                    eprintln!(
                                        "DeviceRail managed iOS route recovered (ios_wda_recovered)"
                                    );
                                    break;
                                }
                                Err(_) => terminate_bundle(&mut candidate).await,
                            }
                        }
                    }
                }
            }
        }
    }
    terminate_bundle(&mut bundle).await;
}

async fn terminate_bundle(bundle: &mut ProcessBundle) {
    let _ = bundle.wda.kill().await;
    if let Some(iproxy) = bundle.iproxy.as_mut() {
        let _ = iproxy.kill().await;
    }
}

async fn wait_for_appium_ready(
    child: &mut Child,
    probe: &WdaProbe,
    startup_timeout: Duration,
) -> Result<(), IosHostError> {
    let deadline = Instant::now() + startup_timeout;
    loop {
        if process_exited(child)? {
            return Err(IosHostError::new(
                "ios_appium_exited",
                "Appium exited before its status endpoint became ready",
            ));
        }
        if wda_ready_at(probe).await {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(IosHostError::new(
                "ios_appium_startup_timeout",
                "Appium did not become ready before the startup deadline",
            ));
        }
        sleep(Duration::from_millis(100)).await;
    }
}

async fn supervise_appium(
    mut child: Child,
    mut shutdown: watch::Receiver<bool>,
    failure: watch::Sender<Option<&'static str>>,
    #[cfg(unix)] mut process_group: OwnedProcessGroup,
) -> Result<(), &'static str> {
    tokio::select! {
        changed = shutdown.changed() => {
            if changed.is_err() || *shutdown.borrow() {
                terminate_appium(
                    &mut child,
                    #[cfg(unix)]
                    &mut process_group,
                ).await?;
                Ok(())
            } else {
                Err("ios_appium_shutdown_signal_invalid")
            }
        }
        status = child.wait() => {
            let code = match status {
                Ok(_) => "ios_appium_exited",
                Err(_) => "ios_appium_child_status_failed",
            };
            #[cfg(unix)]
            let _ = process_group.kill_and_disarm();
            let _ = failure.send(Some(code));
            eprintln!("DeviceRail managed Appium server stopped ({code})");
            Err(code)
        }
    }
}

async fn terminate_appium(
    child: &mut Child,
    #[cfg(unix)] process_group: &mut OwnedProcessGroup,
) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        process_group
            .signal(libc::SIGTERM)
            .map_err(|_| "ios_appium_shutdown_failed")?;
        match timeout(Duration::from_secs(2), child.wait()).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return Err("ios_appium_shutdown_failed"),
            Err(_) => {
                process_group
                    .signal(libc::SIGKILL)
                    .map_err(|_| "ios_appium_shutdown_failed")?;
                child
                    .wait()
                    .await
                    .map_err(|_| "ios_appium_shutdown_failed")?;
            }
        }
        // Appium may have exited before one of its Xcode/WDA descendants.
        // Remove any remaining members of the isolated process group.
        process_group
            .kill_and_disarm()
            .map_err(|_| "ios_appium_shutdown_failed")?;
        Ok(())
    }

    #[cfg(not(unix))]
    {
        child
            .kill()
            .await
            .map_err(|_| "ios_appium_shutdown_failed")?;
        child
            .wait()
            .await
            .map_err(|_| "ios_appium_shutdown_failed")?;
        Ok(())
    }
}

#[cfg(unix)]
fn signal_process_group(process_group: i32, signal: i32) -> std::io::Result<()> {
    if process_group <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid process group",
        ));
    }
    loop {
        // SAFETY: a negative, validated PGID targets only the isolated child group.
        if unsafe { libc::kill(-process_group, signal) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => return Ok(()),
            Some(libc::EINTR) => continue,
            _ => return Err(error),
        }
    }
}

fn process_exited(child: &mut Child) -> Result<bool, IosHostError> {
    child
        .try_wait()
        .map(|status| status.is_some())
        .map_err(|_| {
            IosHostError::new(
                "ios_child_status_failed",
                "could not inspect managed child process",
            )
        })
}

fn validate_appium_base_path(base_path: &str) -> Result<(), IosHostError> {
    let valid = !base_path.is_empty()
        && base_path.len() <= 256
        && base_path.starts_with('/')
        && (base_path == "/" || !base_path.ends_with('/'))
        && !base_path.contains("//")
        && base_path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
        });
    if valid {
        Ok(())
    } else {
        Err(IosHostError::new(
            "ios_appium_base_path_invalid",
            "the Appium base path is invalid",
        ))
    }
}

fn appium_endpoint(port: u16, base_path: &str) -> String {
    if base_path == "/" {
        format!("http://127.0.0.1:{port}")
    } else {
        format!("http://127.0.0.1:{port}{base_path}")
    }
}

async fn reserve_local_port(requested: u16) -> Result<u16, IosHostError> {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, requested))
        .await
        .map_err(|_| {
            IosHostError::new(
                "ios_local_port_in_use",
                "the requested local WDA port is unavailable",
            )
        })?;
    let port = listener
        .local_addr()
        .map_err(|_| {
            IosHostError::new(
                "ios_local_port_invalid",
                "could not inspect the local WDA port",
            )
        })?
        .port();
    drop(listener);
    Ok(port)
}

async fn wda_ready(port: u16) -> bool {
    wda_ready_at(&WdaProbe {
        address: SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
        path: "/status".to_owned(),
    })
    .await
}

struct WdaProbe {
    address: SocketAddr,
    path: String,
}

async fn wda_ready_at(probe: &WdaProbe) -> bool {
    let address = probe.address;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        probe.path, probe.address
    );
    timeout(Duration::from_secs(2), async move {
        let mut stream = TcpStream::connect(address).await.ok()?;
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer).await.ok()?;
            if count == 0 {
                break;
            }
            if bytes.len().saturating_add(count) > MAX_HTTP_BYTES {
                return None;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        parse_wda_ready_response(&bytes).then_some(())
    })
    .await
    .ok()
    .flatten()
    .is_some()
}

fn parse_wda_ready_response(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = &bytes[..header_end];
    let status_ok = headers
        .split(|byte| *byte == b'\n')
        .next()
        .is_some_and(|line| line.starts_with(b"HTTP/1.1 2") || line.starts_with(b"HTTP/1.0 2"));
    if !status_ok {
        return false;
    }
    let Ok(root) = serde_json::from_slice::<Value>(&bytes[header_end + 4..]) else {
        return false;
    };
    root.pointer("/value/ready")
        .or_else(|| root.get("ready"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn xcodebuild_base_args(config: &ManagedIosConfig, device: &IosHostDevice) -> Vec<OsString> {
    vec![
        OsString::from("-quiet"),
        OsString::from("-project"),
        config.wda_project.as_os_str().to_owned(),
        OsString::from("-scheme"),
        OsString::from("WebDriverAgentRunner"),
        OsString::from("-destination"),
        OsString::from(format!("id={}", device.udid)),
        OsString::from("-destination-timeout"),
        OsString::from("30"),
        OsString::from("-derivedDataPath"),
        config.derived_data.as_os_str().to_owned(),
    ]
}

fn classify_xcodebuild_failure(output: &CommandOutput) -> IosHostError {
    let mut combined = String::from_utf8_lossy(&output.stderr).to_lowercase();
    combined.push_str(&String::from_utf8_lossy(&output.stdout).to_lowercase());
    if combined.contains("provisioning profile")
        || combined.contains("code signing")
        || combined.contains("development team")
        || combined.contains("requires a provisioning profile")
    {
        IosHostError::new(
            "ios_wda_signing_failed",
            "xcodebuild rejected WDA signing or provisioning",
        )
    } else if combined.contains("developer mode") {
        IosHostError::new(
            "ios_developer_mode_required",
            "the device requires Developer Mode",
        )
    } else if combined.contains("device is locked") || combined.contains("unlock") {
        IosHostError::new("ios_device_locked", "the device must be unlocked")
    } else {
        IosHostError::new(
            "ios_wda_build_failed",
            "xcodebuild build-for-testing failed",
        )
    }
}

fn ensure_macos_host() -> Result<(), IosHostError> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(IosHostError::new(
            "ios_macos_required",
            "managed Xcode WDA requires macOS",
        ))
    }
}

fn ensure_device_ready(device: &IosHostDevice) -> Result<(), IosHostError> {
    if !device.connected {
        return Err(IosHostError::new(
            match device.kind {
                IosDeviceKind::Physical => "ios_device_disconnected",
                IosDeviceKind::Simulator => "ios_simulator_not_booted",
            },
            match device.kind {
                IosDeviceKind::Physical => "the selected device is not connected",
                IosDeviceKind::Simulator => "the selected Simulator is not booted",
            },
        ));
    }
    if device.kind == IosDeviceKind::Simulator {
        return Ok(());
    }
    if device.paired == Some(false) {
        return Err(IosHostError::new(
            "ios_pairing_required",
            "the selected device is not paired",
        ));
    }
    if device.developer_mode == Some(false) {
        return Err(IosHostError::new(
            "ios_developer_mode_required",
            "Developer Mode is disabled",
        ));
    }
    if device.developer_services == Some(false) {
        return Err(IosHostError::new(
            "ios_developer_services_unavailable",
            "Xcode developer services are unavailable",
        ));
    }
    Ok(())
}

/// Selects one stable iOS target and verifies its host-visible readiness.
///
/// Physical devices require pairing, Developer Mode, and developer services.
/// A Simulator is ready when CoreSimulator reports it as booted and available.
pub fn select_ready_ios_device(
    devices: &[IosHostDevice],
    requested: Option<&str>,
) -> Result<IosHostDevice, IosHostError> {
    let device = select_device(devices, requested)?.clone();
    ensure_device_ready(&device)?;
    Ok(device)
}

fn select_device<'a>(
    devices: &'a [IosHostDevice],
    requested: Option<&str>,
) -> Result<&'a IosHostDevice, IosHostError> {
    if let Some(requested) = requested {
        return devices
            .iter()
            .find(|device| device.udid == requested)
            .ok_or_else(|| {
                IosHostError::new("ios_device_not_found", "the selected device was not found")
            });
    }
    let mut physical = devices
        .iter()
        .filter(|device| device.connected && device.kind == IosDeviceKind::Physical);
    if let Some(first) = physical.next() {
        if physical.next().is_some() {
            return Err(IosHostError::new(
                "ios_device_selection_required",
                "multiple physical iOS devices are connected",
            ));
        }
        return Ok(first);
    }
    let mut simulators = devices
        .iter()
        .filter(|device| device.connected && device.kind == IosDeviceKind::Simulator);
    let first = simulators.next().ok_or_else(|| {
        IosHostError::new(
            "ios_device_not_found",
            "no connected physical iOS device or booted Simulator was found",
        )
    })?;
    if simulators.next().is_some() {
        return Err(IosHostError::new(
            "ios_device_selection_required",
            "multiple iOS Simulators are booted",
        ));
    }
    Ok(first)
}

fn requires_physical_host_support(
    selected: Option<&IosHostDevice>,
    devices: &[IosHostDevice],
) -> bool {
    selected.map_or_else(
        || {
            devices.is_empty()
                || devices
                    .iter()
                    .any(|device| device.kind == IosDeviceKind::Physical)
        },
        |device| device.kind == IosDeviceKind::Physical,
    )
}

fn parse_devicectl_devices(bytes: &[u8]) -> Result<Vec<IosHostDevice>, IosHostError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| {
        IosHostError::new(
            "ios_devicectl_invalid_json",
            "devicectl returned invalid JSON",
        )
    })?;
    let array = root
        .pointer("/result/devices")
        .or_else(|| root.get("devices"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            IosHostError::new(
                "ios_devicectl_invalid_json",
                "devicectl omitted its device array",
            )
        })?;
    let mut devices = Vec::new();
    for value in array {
        let platform = string_at(
            value,
            &[
                "/hardwareProperties/platform",
                "/deviceProperties/platform",
                "/platform",
            ],
        );
        if !platform.is_some_and(is_ios_platform) {
            continue;
        }
        let Some(udid) = string_at(value, &["/hardwareProperties/udid", "/udid", "/identifier"])
            .and_then(bounded_owned)
        else {
            continue;
        };
        let name = string_at(value, &["/deviceProperties/name", "/name"])
            .and_then(bounded_owned)
            .unwrap_or_else(|| "iOS device".to_owned());
        let os_version = string_at(
            value,
            &[
                "/deviceProperties/osVersionNumber",
                "/deviceProperties/osVersion",
                "/osVersion",
            ],
        )
        .and_then(bounded_owned);
        let tunnel = string_at(value, &["/connectionProperties/tunnelState"]);
        let boot = string_at(value, &["/deviceProperties/bootState"]);
        let connected = !matches!(tunnel, Some("disconnected" | "unavailable"))
            && !matches!(boot, Some("disconnected" | "unavailable" | "shutdown"));
        let paired =
            string_at(value, &["/connectionProperties/pairingState"]).and_then(parse_enabled_state);
        let developer_mode = string_at(value, &["/deviceProperties/developerModeStatus"])
            .and_then(parse_enabled_state);
        let developer_services = bool_at(value, &["/deviceProperties/ddiServicesAvailable"]);
        devices.push(IosHostDevice {
            udid,
            name,
            os_version,
            kind: IosDeviceKind::Physical,
            connected,
            paired,
            developer_mode,
            developer_services,
        });
    }
    devices.sort_by(|left, right| left.udid.cmp(&right.udid));
    devices.dedup_by(|left, right| left.udid == right.udid);
    Ok(devices)
}

fn parse_xcdevice_physical_devices(bytes: &[u8]) -> Result<Vec<IosHostDevice>, IosHostError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| {
        IosHostError::new(
            "ios_xcdevice_invalid_json",
            "xcdevice returned invalid JSON",
        )
    })?;
    let array = root.as_array().ok_or_else(|| {
        IosHostError::new(
            "ios_xcdevice_invalid_json",
            "xcdevice omitted its device array",
        )
    })?;
    let mut devices = Vec::new();
    for value in array {
        if value.get("simulator").and_then(Value::as_bool) != Some(false)
            || !value
                .get("platform")
                .and_then(Value::as_str)
                .is_some_and(is_ios_platform)
        {
            continue;
        }
        let Some(udid) = value
            .get("identifier")
            .and_then(Value::as_str)
            .and_then(bounded_owned)
        else {
            continue;
        };
        let name = value
            .get("name")
            .or_else(|| value.get("modelName"))
            .and_then(Value::as_str)
            .and_then(bounded_owned)
            .unwrap_or_else(|| "iOS device".to_owned());
        let os_version = value
            .get("operatingSystemVersion")
            .and_then(Value::as_str)
            .and_then(bounded_owned);
        let connected = value
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        devices.push(IosHostDevice {
            udid,
            name,
            os_version,
            kind: IosDeviceKind::Physical,
            connected,
            paired: None,
            developer_mode: None,
            developer_services: None,
        });
    }
    devices.sort_by(|left, right| left.udid.cmp(&right.udid));
    devices.dedup_by(|left, right| left.udid == right.udid);
    Ok(devices)
}

fn parse_simctl_devices(bytes: &[u8]) -> Result<Vec<IosHostDevice>, IosHostError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| {
        IosHostError::new("ios_simctl_invalid_json", "simctl returned invalid JSON")
    })?;
    let runtimes = root
        .get("devices")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            IosHostError::new(
                "ios_simctl_invalid_json",
                "simctl omitted its device inventory",
            )
        })?;
    let runtime_versions = root
        .get("runtimes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|runtime| {
            let identifier = runtime
                .get("identifier")?
                .as_str()
                .and_then(bounded_owned)?;
            let version = runtime.get("version")?.as_str()?;
            if !is_numeric_version(version) {
                return None;
            }
            bounded_owned(version).map(|version| (identifier, version))
        })
        .collect::<HashMap<_, _>>();
    let mut devices = Vec::new();
    for (runtime, values) in runtimes {
        let Some(fallback_version) = ios_simulator_runtime_version(runtime) else {
            continue;
        };
        let os_version = runtime_versions
            .get(runtime)
            .and_then(|version| bounded_owned(version))
            .unwrap_or(fallback_version);
        let Some(values) = values.as_array() else {
            continue;
        };
        for value in values {
            let booted = value
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| state.eq_ignore_ascii_case("booted"));
            let available = value.get("isAvailable").and_then(Value::as_bool) == Some(true);
            if !available {
                continue;
            }
            let Some(udid) = value
                .get("udid")
                .and_then(Value::as_str)
                .and_then(bounded_owned)
            else {
                continue;
            };
            let name = value
                .get("name")
                .and_then(Value::as_str)
                .and_then(bounded_owned)
                .unwrap_or_else(|| "iOS Simulator".to_owned());
            devices.push(IosHostDevice {
                udid,
                name,
                os_version: Some(os_version.clone()),
                kind: IosDeviceKind::Simulator,
                connected: booted,
                paired: None,
                developer_mode: None,
                developer_services: None,
            });
        }
    }
    normalize_devices(&mut devices);
    Ok(devices)
}

fn ios_simulator_runtime_version(runtime: &str) -> Option<String> {
    let version = runtime.strip_prefix("com.apple.CoreSimulator.SimRuntime.iOS-")?;
    if version.is_empty()
        || !version.split('-').all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return None;
    }
    bounded_owned(&version.replace('-', "."))
}

fn is_numeric_version(version: &str) -> bool {
    !version.is_empty()
        && version.split('.').all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn normalize_devices(devices: &mut Vec<IosHostDevice>) {
    devices.sort_by(|left, right| left.udid.cmp(&right.udid));
    devices.dedup_by(|left, right| left.udid == right.udid);
}

fn parse_wda_signing_settings(bytes: &[u8]) -> Result<WdaSigningSettings, IosHostError> {
    let root: Value = serde_json::from_slice(bytes).map_err(|_| {
        IosHostError::new(
            "ios_wda_signing_config_invalid",
            "xcodebuild returned invalid WDA build settings",
        )
    })?;
    let entries = root.as_array().ok_or_else(|| {
        IosHostError::new(
            "ios_wda_signing_config_invalid",
            "xcodebuild omitted WDA build settings",
        )
    })?;
    let settings = entries
        .iter()
        .find(|entry| entry.get("target").and_then(Value::as_str) == Some("WebDriverAgentRunner"))
        .or_else(|| entries.first())
        .and_then(|entry| entry.get("buildSettings"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            IosHostError::new(
                "ios_wda_signing_config_invalid",
                "xcodebuild omitted WDA build settings",
            )
        })?;
    let string_setting = |key: &str| {
        settings
            .get(key)
            .and_then(Value::as_str)
            .and_then(bounded_owned)
    };
    Ok(WdaSigningSettings {
        development_team: string_setting("DEVELOPMENT_TEAM"),
        bundle_identifier: string_setting("PRODUCT_BUNDLE_IDENTIFIER"),
    })
}

fn string_at<'a>(root: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers
        .iter()
        .find_map(|pointer| root.pointer(pointer).and_then(Value::as_str))
}

fn bool_at(root: &Value, pointers: &[&str]) -> Option<bool> {
    pointers
        .iter()
        .find_map(|pointer| root.pointer(pointer).and_then(Value::as_bool))
}

fn parse_enabled_state(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "paired" | "enabled" | "available" | "connected" => Some(true),
        "unpaired" | "disabled" | "unavailable" | "disconnected" => Some(false),
        _ => None,
    }
}

fn is_ios_platform(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "ios" | "iphoneos" | "com.apple.platform.iphoneos"
    )
}

fn bounded_owned(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn add_device_checks(checks: &mut Vec<DiagnosticCheck>, device: &IosHostDevice) {
    if device.kind == IosDeviceKind::Simulator {
        if device.connected {
            checks.push(pass(
                "ios_simulator_booted",
                "the selected iOS Simulator is booted and available",
            ));
        } else {
            checks.push(fail(
                "ios_simulator_not_booted",
                "the selected iOS Simulator is not booted",
                "boot the selected Simulator in Xcode or with simctl",
            ));
        }
        return;
    }
    if device.connected {
        checks.push(pass(
            "ios_device_connected",
            "one physical iOS device is connected",
        ));
    } else {
        checks.push(fail(
            "ios_device_disconnected",
            "the selected iOS device is disconnected",
            "unlock and reconnect the device",
        ));
    }
    match device.paired {
        Some(true) => checks.push(pass("ios_device_paired", "host pairing is ready")),
        Some(false) => checks.push(fail(
            "ios_pairing_required",
            "host pairing is not ready",
            "accept Trust This Computer and pair the device in Xcode",
        )),
        None => checks.push(warn(
            "ios_pairing_unknown",
            "host pairing state could not be determined",
            "confirm the device appears in Xcode Devices and Simulators",
        )),
    }
    match device.developer_mode {
        Some(true) => checks.push(pass(
            "ios_developer_mode_ready",
            "Developer Mode is enabled",
        )),
        Some(false) => checks.push(fail(
            "ios_developer_mode_required",
            "Developer Mode is disabled",
            "enable Developer Mode on the device and complete its restart",
        )),
        None => checks.push(warn(
            "ios_developer_mode_unknown",
            "Developer Mode could not be determined",
            "confirm Developer Mode is enabled on the device",
        )),
    }
    match device.developer_services {
        Some(true) => checks.push(pass(
            "ios_developer_services_ready",
            "Xcode developer services are ready",
        )),
        Some(false) => checks.push(fail(
            "ios_developer_services_unavailable",
            "Xcode developer services are unavailable",
            "open Xcode and wait for device preparation to finish",
        )),
        None => checks.push(warn(
            "ios_developer_services_unknown",
            "developer-service readiness could not be determined",
            "open Xcode and wait for device preparation to finish",
        )),
    }
}

fn pass(code: &str, summary: &str) -> DiagnosticCheck {
    check(DiagnosticStatus::Pass, code, summary, None)
}

fn warn(code: &str, summary: &str, remediation: &str) -> DiagnosticCheck {
    check(DiagnosticStatus::Warn, code, summary, Some(remediation))
}

fn fail(code: &str, summary: &str, remediation: &str) -> DiagnosticCheck {
    check(DiagnosticStatus::Fail, code, summary, Some(remediation))
}

fn check(
    status: DiagnosticStatus,
    code: &str,
    summary: &str,
    remediation: Option<&str>,
) -> DiagnosticCheck {
    DiagnosticCheck {
        status,
        code: bounded_message(code.to_owned()),
        summary: bounded_message(summary.to_owned()),
        remediation: remediation.map(|value| bounded_message(value.to_owned())),
    }
}

fn discover_wda_project() -> Option<PathBuf> {
    discover_wda_project_from(
        std::env::var_os("DEVICERAIL_IOS_WDA_PROJECT").map(PathBuf::from),
        std::env::var_os("APPIUM_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::current_dir().ok(),
    )
}

fn discover_wda_project_from(
    explicit: Option<PathBuf>,
    appium_home: Option<PathBuf>,
    home: Option<PathBuf>,
    current: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit.filter(|path| !path.as_os_str().is_empty()) {
        return Some(explicit);
    }
    let relative = Path::new("node_modules")
        .join("appium-xcuitest-driver")
        .join("node_modules")
        .join("appium-webdriveragent")
        .join("WebDriverAgent.xcodeproj");
    let mut candidates = Vec::with_capacity(3);
    if let Some(appium_home) = appium_home.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(appium_home.join(&relative));
    }
    if let Some(home) = home.filter(|path| !path.as_os_str().is_empty()) {
        candidates.push(home.join(".appium").join(&relative));
    }
    if let Some(current) = current {
        candidates.push(current.join(relative));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_dir() && candidate.join("project.pbxproj").is_file())
}

async fn valid_wda_project(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("xcodeproj"))
        && fs::try_exists(path.join("project.pbxproj"))
            .await
            .unwrap_or(false)
}

async fn read_build_stamp(path: &Path) -> Option<BuildStamp> {
    let bytes = read_bounded_file(path, 64 * 1024).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn write_build_stamp(path: &Path, stamp: &BuildStamp) -> Result<(), IosHostError> {
    let bytes = serde_json::to_vec(stamp).map_err(|_| {
        IosHostError::new(
            "ios_build_cache_write_failed",
            "could not serialize WDA build cache",
        )
    })?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).await.map_err(|_| {
        IosHostError::new(
            "ios_build_cache_write_failed",
            "could not write WDA build cache",
        )
    })?;
    fs::rename(&temporary, path).await.map_err(|_| {
        IosHostError::new(
            "ios_build_cache_write_failed",
            "could not publish WDA build cache",
        )
    })?;
    Ok(())
}

async fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, IosHostError> {
    let metadata = fs::metadata(path).await.map_err(|_| {
        IosHostError::new(
            "ios_host_file_read_failed",
            "host tool output is unavailable",
        )
    })?;
    if metadata.len() > limit as u64 {
        return Err(IosHostError::new(
            "ios_host_output_too_large",
            "host tool output exceeded its limit",
        ));
    }
    fs::read(path).await.map_err(|_| {
        IosHostError::new(
            "ios_host_file_read_failed",
            "host tool output could not be read",
        )
    })
}

struct CommandOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_output(
    program: &Path,
    args: &[OsString],
    deadline: Duration,
    limit: usize,
) -> Result<CommandOutput, IosHostError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| {
        IosHostError::new(
            "ios_host_tool_unavailable",
            "could not start a required iOS host tool",
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        IosHostError::new("ios_host_tool_failed", "could not capture host tool stdout")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        IosHostError::new("ios_host_tool_failed", "could not capture host tool stderr")
    })?;
    let stdout_task = tokio::spawn(read_limited(stdout, limit));
    let stderr_task = tokio::spawn(read_limited(stderr, limit));
    let status = match timeout(deadline, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(IosHostError::new(
                "ios_host_tool_failed",
                "iOS host tool execution failed",
            ));
        }
        Err(_) => {
            let _ = child.kill().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(IosHostError::new(
                "ios_host_tool_timeout",
                "iOS host tool execution timed out",
            ));
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|_| IosHostError::new("ios_host_tool_failed", "host stdout task failed"))??;
    let stderr = stderr_task
        .await
        .map_err(|_| IosHostError::new("ios_host_tool_failed", "host stderr task failed"))??;
    Ok(CommandOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}

async fn read_limited<R>(mut reader: R, limit: usize) -> Result<Vec<u8>, IosHostError>
where
    R: AsyncRead + Unpin,
{
    let mut stored = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut too_large = false;
    loop {
        let count = reader.read(&mut buffer).await.map_err(|_| {
            IosHostError::new("ios_host_tool_failed", "could not read host tool output")
        })?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(stored.len());
        stored.extend_from_slice(&buffer[..count.min(remaining)]);
        too_large |= count > remaining;
    }
    if too_large {
        return Err(IosHostError::new(
            "ios_host_output_too_large",
            "host tool output exceeded its limit",
        ));
    }
    Ok(stored)
}

fn parse_wda_probe(endpoint: &str) -> Option<WdaProbe> {
    let url = Url::parse(endpoint).ok()?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let ip = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<IpAddr>()
        .ok()?;
    if !ip.is_loopback() {
        return None;
    }
    let port = url.port()?;
    let base = url.path().trim_end_matches('/');
    let path = if base.is_empty() {
        "/status".to_owned()
    } else {
        format!("{base}/status")
    };
    if path.len() > 4096 || path.chars().any(char::is_control) {
        return None;
    }
    Some(WdaProbe {
        address: SocketAddr::new(ip, port),
        path,
    })
}

fn parse_bool(value: &OsStr) -> Result<bool, IosHostError> {
    match value.to_str() {
        Some("1" | "true" | "yes") => Ok(true),
        Some("0" | "false" | "no") => Ok(false),
        _ => Err(IosHostError::new(
            "ios_provisioning_updates_invalid",
            "DEVICERAIL_IOS_ALLOW_PROVISIONING_UPDATES must be true or false",
        )),
    }
}

fn parse_port(value: &OsStr, allow_zero: bool, code: &'static str) -> Result<u16, IosHostError> {
    let port = value
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| IosHostError::new(code, "iOS port is invalid"))?;
    if !allow_zero && port == 0 {
        return Err(IosHostError::new(code, "iOS port must be non-zero"));
    }
    Ok(port)
}

fn nonempty_path(value: OsString, code: &'static str) -> Result<PathBuf, IosHostError> {
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() {
        Err(IosHostError::new(code, "iOS host path is empty"))
    } else {
        Ok(path)
    }
}

fn validate_text(value: &str, limit: usize, code: &'static str) -> Result<(), IosHostError> {
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        Err(IosHostError::new(code, "iOS host text value is invalid"))
    } else {
        Ok(())
    }
}

fn bounded_message(value: String) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(1024)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(target_os = "macos")]
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[cfg(unix)]
    use std::sync::{Arc, Mutex};

    #[cfg(unix)]
    struct RecordingProcessGroupSignaler {
        signals: Arc<Mutex<Vec<(i32, i32)>>>,
    }

    #[cfg(unix)]
    impl ProcessGroupSignaler for RecordingProcessGroupSignaler {
        fn signal(&self, process_group: i32, signal: i32) -> std::io::Result<()> {
            self.signals
                .lock()
                .expect("record process-group signal")
                .push((process_group, signal));
            Ok(())
        }
    }

    #[cfg(target_os = "macos")]
    fn write_executable(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("write executable");
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).expect("executable permissions");
    }

    #[test]
    fn parses_devicectl_inventory_and_security_states() {
        let fixture = br#"{
          "result": {"devices": [{
            "identifier": "core-id",
            "connectionProperties": {"pairingState": "paired", "tunnelState": "connected"},
            "deviceProperties": {
              "bootState": "booted", "ddiServicesAvailable": true,
              "developerModeStatus": "enabled", "name": "Test iPhone",
              "osVersionNumber": "18.5"
            },
            "hardwareProperties": {"platform": "iOS", "udid": "00008120-TEST"}
          }, {
            "identifier": "sim", "hardwareProperties": {"platform": "macOS"}
          }]}
        }"#;
        let devices = parse_devicectl_devices(fixture).expect("parse");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].udid, "00008120-TEST");
        assert_eq!(devices[0].name, "Test iPhone");
        assert_eq!(devices[0].os_version.as_deref(), Some("18.5"));
        assert_eq!(devices[0].kind, IosDeviceKind::Physical);
        assert!(devices[0].connected);
        assert_eq!(devices[0].paired, Some(true));
        assert_eq!(devices[0].developer_mode, Some(true));
        assert_eq!(devices[0].developer_services, Some(true));
    }

    #[test]
    fn device_kind_is_wire_compatible_and_explicit_for_simulators() {
        let legacy = br#"{
          "udid":"legacy", "name":"Legacy iPhone", "osVersion":"18.5",
          "connected":true, "paired":true, "developerMode":true,
          "developerServices":true
        }"#;
        let device: IosHostDevice = serde_json::from_slice(legacy).expect("legacy device");
        assert_eq!(device.kind, IosDeviceKind::Physical);

        let mut simulator = device;
        simulator.kind = IosDeviceKind::Simulator;
        let json = serde_json::to_value(simulator).expect("Simulator JSON");
        assert_eq!(json.get("kind").and_then(Value::as_str), Some("simulator"));
    }

    #[test]
    fn parses_xcdevice_fallback_without_inventing_security_state() {
        let fixture = br#"[{
          "simulator": false, "available": true,
          "platform": "com.apple.platform.iphoneos",
          "identifier": "device-b", "modelName": "iPhone 17",
          "operatingSystemVersion": "18.5 (22F76)"
        }, {
          "simulator": true, "available": true,
          "platform": "com.apple.platform.iphonesimulator",
          "identifier": "simulator"
        }]"#;
        let devices = parse_xcdevice_physical_devices(fixture).expect("parse");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].kind, IosDeviceKind::Physical);
        assert_eq!(devices[0].paired, None);
        assert_eq!(devices[0].developer_mode, None);
    }

    #[test]
    fn parses_available_ios_simulators_and_tracks_boot_state() {
        let fixture = br#"{
          "runtimes": [{
            "identifier": "com.apple.CoreSimulator.SimRuntime.iOS-26-4",
            "version": "26.4.1", "isAvailable": true
          }],
          "devices": {
            "com.apple.CoreSimulator.SimRuntime.iOS-26-4": [{
              "state": "Booted", "isAvailable": true,
              "name": "iPhone 17", "udid": "booted-simulator"
            }, {
              "state": "Shutdown", "isAvailable": true,
              "name": "iPhone 16", "udid": "shutdown-simulator"
            }, {
              "state": "Booted", "isAvailable": false,
              "name": "Unavailable", "udid": "unavailable-simulator"
            }],
            "com.apple.CoreSimulator.SimRuntime.tvOS-26-4": [{
              "state": "Booted", "isAvailable": true,
              "name": "Apple TV", "udid": "tvos-simulator"
            }]
          }
        }"#;
        let devices = parse_simctl_devices(fixture).expect("parse simctl");
        assert_eq!(devices.len(), 2);
        let booted = devices
            .iter()
            .find(|device| device.udid == "booted-simulator")
            .expect("booted Simulator");
        assert_eq!(booted.name, "iPhone 17");
        assert_eq!(booted.os_version.as_deref(), Some("26.4.1"));
        assert_eq!(booted.kind, IosDeviceKind::Simulator);
        assert!(booted.connected);
        assert_eq!(booted.paired, None);
        let shutdown = devices
            .iter()
            .find(|device| device.udid == "shutdown-simulator")
            .expect("shutdown Simulator");
        assert!(!shutdown.connected);
        assert_eq!(
            select_ready_ios_device(&devices, Some("shutdown-simulator"))
                .expect_err("shutdown Simulator")
                .code(),
            "ios_simulator_not_booted"
        );
    }

    #[test]
    fn simulator_readiness_skips_physical_security_states() {
        let simulator = IosHostDevice {
            udid: "simulator".to_owned(),
            name: "Simulator".to_owned(),
            os_version: Some("26.4".to_owned()),
            kind: IosDeviceKind::Simulator,
            connected: true,
            paired: Some(false),
            developer_mode: Some(false),
            developer_services: Some(false),
        };
        assert_eq!(
            select_ready_ios_device(std::slice::from_ref(&simulator), None)
                .expect("booted Simulator")
                .kind,
            IosDeviceKind::Simulator
        );
        let mut checks = Vec::new();
        add_device_checks(&mut checks, &simulator);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].code, "ios_simulator_booted");
        assert_eq!(checks[0].status, DiagnosticStatus::Pass);
    }

    #[test]
    fn parses_wda_signing_settings_from_the_runner_target() {
        let fixture = br#"[{
          "target":"Other", "buildSettings":{"DEVELOPMENT_TEAM":"wrong"}
        }, {
          "target":"WebDriverAgentRunner", "buildSettings":{
            "DEVELOPMENT_TEAM":"TEAM123", "PRODUCT_BUNDLE_IDENTIFIER":"devicerail.wda"
          }
        }]"#;
        let settings = parse_wda_signing_settings(fixture).expect("signing settings");
        assert_eq!(settings.development_team.as_deref(), Some("TEAM123"));
        assert_eq!(
            settings.bundle_identifier.as_deref(),
            Some("devicerail.wda")
        );
        assert_eq!(
            parse_wda_signing_settings(b"{}")
                .expect_err("invalid settings")
                .code(),
            "ios_wda_signing_config_invalid"
        );
    }

    #[test]
    fn selection_requires_an_explicit_udid_for_multiple_devices() {
        let device = |udid: &str| IosHostDevice {
            udid: udid.to_owned(),
            name: udid.to_owned(),
            os_version: None,
            kind: IosDeviceKind::Physical,
            connected: true,
            paired: Some(true),
            developer_mode: Some(true),
            developer_services: Some(true),
        };
        let devices = vec![device("a"), device("b")];
        assert_eq!(
            select_device(&devices, None).expect_err("ambiguous").code(),
            "ios_device_selection_required"
        );
        assert_eq!(
            select_device(&devices, Some("b")).expect("selected").udid,
            "b"
        );
    }

    #[test]
    fn automatic_selection_preserves_physical_device_priority() {
        let physical = IosHostDevice {
            udid: "physical".to_owned(),
            name: "Physical".to_owned(),
            os_version: None,
            kind: IosDeviceKind::Physical,
            connected: true,
            paired: Some(true),
            developer_mode: Some(true),
            developer_services: Some(true),
        };
        let simulator = IosHostDevice {
            udid: "simulator".to_owned(),
            name: "Simulator".to_owned(),
            os_version: Some("26.4".to_owned()),
            kind: IosDeviceKind::Simulator,
            connected: true,
            paired: None,
            developer_mode: None,
            developer_services: None,
        };
        let devices = vec![simulator.clone(), physical];
        assert_eq!(
            select_device(&devices, None)
                .expect("automatic physical")
                .udid,
            "physical"
        );
        assert_eq!(
            select_device(&devices, Some("simulator"))
                .expect("explicit Simulator")
                .udid,
            "simulator"
        );
        assert_eq!(
            select_device(&[simulator], None)
                .expect("automatic Simulator fallback")
                .udid,
            "simulator"
        );
    }

    #[test]
    fn simulator_only_inventory_never_enables_physical_doctor_requirements() {
        let simulator = |udid: &str, connected: bool| IosHostDevice {
            udid: udid.to_owned(),
            name: udid.to_owned(),
            os_version: Some("26.4".to_owned()),
            kind: IosDeviceKind::Simulator,
            connected,
            paired: None,
            developer_mode: None,
            developer_services: None,
        };
        let simulators = vec![simulator("booted-a", true), simulator("booted-b", true)];
        assert!(!requires_physical_host_support(None, &simulators));
        assert!(!requires_physical_host_support(
            Some(&simulators[0]),
            &simulators
        ));
        assert!(requires_physical_host_support(None, &[]));

        let mut mixed = simulators;
        let mut physical = simulator("physical", false);
        physical.kind = IosDeviceKind::Physical;
        mixed.push(physical);
        assert!(requires_physical_host_support(None, &mixed));
    }

    #[test]
    fn xcodebuild_arguments_are_structured_and_never_shell_joined() {
        let config =
            ManagedIosConfig::new("/tmp/WDA with spaces/WebDriverAgent.xcodeproj").expect("config");
        let device = IosHostDevice {
            udid: "safe-device".to_owned(),
            name: "phone".to_owned(),
            os_version: None,
            kind: IosDeviceKind::Physical,
            connected: true,
            paired: Some(true),
            developer_mode: Some(true),
            developer_services: Some(true),
        };
        let args = xcodebuild_base_args(&config, &device);
        assert!(
            args.iter()
                .any(|arg| arg == "/tmp/WDA with spaces/WebDriverAgent.xcodeproj")
        );
        assert!(args.iter().any(|arg| arg == "id=safe-device"));
    }

    #[test]
    fn simulator_wda_uses_the_reachable_local_port_without_iproxy() {
        let config = ManagedIosConfig::new("WebDriverAgent.xcodeproj").expect("config");
        let simulator = IosHostDevice {
            udid: "simulator".to_owned(),
            name: "Simulator".to_owned(),
            os_version: Some("26.4".to_owned()),
            kind: IosDeviceKind::Simulator,
            connected: true,
            paired: None,
            developer_mode: None,
            developer_services: None,
        };
        assert_eq!(wda_listen_port(&config, &simulator, 49152), 49152);

        let mut physical = simulator;
        physical.kind = IosDeviceKind::Physical;
        assert_eq!(wda_listen_port(&config, &physical, 49152), 8100);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn simulator_launch_does_not_start_iproxy() {
        let directory = tempdir().expect("temporary directory");
        let marker = directory.path().join("simulator-wda-port");
        assert!(!marker.to_string_lossy().contains('\''));
        let xcodebuild = directory.path().join("xcodebuild");
        write_executable(
            &xcodebuild,
            &format!(
                "#!/bin/sh\nprintf '%s' \"$USE_PORT\" > '{}'\nexec sleep 600\n",
                marker.display()
            ),
        );
        let host = SystemIosHost {
            xcrun: directory.path().join("unused-xcrun"),
            xcodebuild,
            security: directory.path().join("unused-security"),
            git: directory.path().join("unused-git"),
        };
        let mut config = ManagedIosConfig::new(directory.path().join("WebDriverAgent.xcodeproj"))
            .expect("config");
        config.iproxy_path = directory.path().join("must-not-start-iproxy");
        let simulator = IosHostDevice {
            udid: "simulator".to_owned(),
            name: "Simulator".to_owned(),
            os_version: Some("26.4".to_owned()),
            kind: IosDeviceKind::Simulator,
            connected: true,
            paired: None,
            developer_mode: None,
            developer_services: None,
        };

        let mut bundle = launch_processes(&host, &config, &simulator, 49152)
            .await
            .expect("Simulator WDA process");
        assert!(bundle.iproxy.is_none());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "WDA port marker timed out");
            sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            std::fs::read_to_string(marker).expect("WDA port marker"),
            "49152"
        );
        terminate_bundle(&mut bundle).await;
    }

    #[test]
    fn parses_only_a_ready_successful_wda_response() {
        assert!(parse_wda_ready_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\n\r\n{\"value\":{\"ready\":true}}"
        ));
        assert!(!parse_wda_ready_response(
            b"HTTP/1.1 500 Error\r\n\r\n{\"value\":{\"ready\":true}}"
        ));
        assert!(!parse_wda_ready_response(
            b"HTTP/1.1 200 OK\r\n\r\n{\"value\":{\"ready\":false}}"
        ));
    }

    #[test]
    fn wda_probe_accepts_numeric_loopback_and_preserves_base_path() {
        let ipv4 = parse_wda_probe("http://127.0.0.1:8100/wda/").expect("IPv4 probe");
        assert_eq!(ipv4.path, "/wda/status");
        assert_eq!(ipv4.address.port(), 8100);

        let ipv6 = parse_wda_probe("http://[::1]:8200").expect("IPv6 probe");
        assert_eq!(ipv6.path, "/status");
        assert!(ipv6.address.ip().is_loopback());

        assert!(parse_wda_probe("https://127.0.0.1:8100").is_none());
        assert!(parse_wda_probe("http://localhost:8100").is_none());
        assert!(parse_wda_probe("http://192.0.2.1:8100").is_none());
    }

    #[tokio::test]
    async fn lifecycle_lock_is_exclusive_and_released_on_drop() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("runtime.lock");
        let first = acquire_file_lock(&path, Duration::ZERO, "ios_managed_runtime_busy")
            .await
            .expect("first lock");
        assert_eq!(
            acquire_file_lock(&path, Duration::ZERO, "ios_managed_runtime_busy")
                .await
                .expect_err("exclusive lock")
                .code(),
            "ios_managed_runtime_busy"
        );
        drop(first);
        acquire_file_lock(&path, Duration::ZERO, "ios_managed_runtime_busy")
            .await
            .expect("released lock");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn managed_lifecycle_builds_launches_probes_and_shuts_down() {
        let directory = tempdir().expect("temporary directory");
        let project = directory.path().join("WebDriverAgent.xcodeproj");
        std::fs::create_dir_all(&project).expect("project directory");
        std::fs::write(project.join("project.pbxproj"), "// fake project")
            .expect("project fixture");

        let marker = directory.path().join("wda-launched");
        assert!(!marker.to_string_lossy().contains('\''));
        let xcrun = directory.path().join("xcrun");
        write_executable(
            &xcrun,
            r#"#!/bin/sh
output=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--json-output' ]; then
    shift
    output="$1"
  fi
  shift
done
test -n "$output" || exit 1
printf '%s' '{"result":{"devices":[{"connectionProperties":{"pairingState":"paired","tunnelState":"connected"},"deviceProperties":{"bootState":"booted","ddiServicesAvailable":true,"developerModeStatus":"enabled","name":"Managed Test iPhone","osVersionNumber":"18.5"},"hardwareProperties":{"platform":"iOS","udid":"managed-test-device"}}]}}' > "$output"
"#,
        );

        let xcodebuild = directory.path().join("xcodebuild");
        write_executable(
            &xcodebuild,
            &format!(
                r#"#!/bin/sh
if [ "$1" = '-version' ]; then
  printf '%s\n' 'Xcode 16.4' 'Build version 16F6'
  exit 0
fi
action=''
derived=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -derivedDataPath) shift; derived="$1" ;;
    build-for-testing) action='build' ;;
    test-without-building) action='test' ;;
  esac
  shift
done
if [ "$action" = 'build' ]; then
  mkdir -p "$derived/Build/Products"
  exit 0
fi
if [ "$action" = 'test' ]; then
  printf '%s' "$USE_PORT" > '{}'
  exec sleep 600
fi
exit 1
"#,
                marker.display()
            ),
        );

        let iproxy = directory.path().join("iproxy");
        write_executable(&iproxy, "#!/bin/sh\nexec sleep 600\n");
        let git = directory.path().join("git");
        write_executable(
            &git,
            r#"#!/bin/sh
case " $* " in
  *' rev-parse '*) printf '%s\n' '0123456789abcdef0123456789abcdef01234567' ;;
  *' diff '*) printf '%s' 'tracked signing configuration' ;;
  *' ls-files '*) : ;;
  *) exit 1 ;;
esac
"#,
        );

        let local_port = reserve_local_port(0).await.expect("free local port");
        let (stop_server, mut stop_receiver) = watch::channel(false);
        let server_marker = marker.clone();
        let server_bound = Arc::new(AtomicBool::new(false));
        let accepted_count = Arc::new(AtomicUsize::new(0));
        let task_bound = Arc::clone(&server_bound);
        let task_accepted = Arc::clone(&accepted_count);
        let server = tokio::spawn(async move {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fs::try_exists(&server_marker).await.unwrap_or(false) {
                if *stop_receiver.borrow() {
                    return;
                }
                assert!(Instant::now() < deadline, "WDA launch marker timed out");
                sleep(Duration::from_millis(20)).await;
            }
            let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, local_port))
                .await
                .expect("fake WDA listener");
            task_bound.store(true, Ordering::SeqCst);
            loop {
                tokio::select! {
                    changed = stop_receiver.changed() => {
                        if changed.is_err() || *stop_receiver.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted.expect("fake WDA accept");
                        task_accepted.fetch_add(1, Ordering::SeqCst);
                        let mut request = [0u8; 1024];
                        let count = stream.read(&mut request).await.expect("fake WDA request");
                        assert!(request[..count].windows(4).any(|window| window == b"\r\n\r\n"));
                        stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\nConnection: close\r\n\r\n{\"value\":{\"ready\":true}}",
                        )
                        .await
                        .expect("fake WDA response");
                        stream.shutdown().await.expect("fake WDA close");
                    }
                }
            }
        });

        let host = SystemIosHost {
            xcrun,
            xcodebuild,
            security: directory.path().join("unused-security"),
            git,
        };
        let mut config = ManagedIosConfig::new(project).expect("managed config");
        config.derived_data = directory.path().join("DerivedData");
        config.iproxy_path = iproxy;
        config.local_port = local_port;
        config.build_timeout = Duration::from_secs(5);
        config.startup_timeout = Duration::from_secs(5);

        let runtime = match host.start(config.clone()).await {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = stop_server.send(true);
                let _ = server.await;
                panic!(
                    "managed runtime failed: {error:?}; marker={}; bound={}; accepted={}",
                    marker.exists(),
                    server_bound.load(Ordering::SeqCst),
                    accepted_count.load(Ordering::SeqCst),
                );
            }
        };
        assert_eq!(
            runtime.endpoint().wda_url,
            format!("http://127.0.0.1:{local_port}")
        );
        assert_eq!(runtime.endpoint().device.udid, "managed-test-device");
        assert_eq!(std::fs::read_to_string(&marker).expect("WDA port"), "8100");
        assert_eq!(
            host.prepare(&config)
                .await
                .expect_err("runtime owns DerivedData")
                .code(),
            "ios_managed_runtime_busy"
        );
        runtime.shutdown().await.expect("managed shutdown");
        let _ = stop_server.send(true);
        server.await.expect("fake WDA server");
        assert!(
            host.prepare(&config)
                .await
                .expect("cached prepare")
                .used_cached_build
        );
    }

    #[test]
    fn managed_config_is_bounded_and_rejects_zero_remote_port() {
        let mut config = ManagedIosConfig::new("WebDriverAgent.xcodeproj").expect("config");
        config.remote_port = 0;
        assert_eq!(
            config.validate().expect_err("invalid").code(),
            "ios_remote_port_invalid"
        );
        config.remote_port = 8100;
        config.device_udid = Some("bad\nvalue".to_owned());
        assert_eq!(
            config.validate().expect_err("invalid").code(),
            "ios_device_udid_invalid"
        );
    }

    #[test]
    fn installed_xcuitest_wda_discovery_has_a_closed_priority_order() {
        let directory = tempdir().expect("temporary directory");
        let explicit = directory.path().join("explicit/WebDriverAgent.xcodeproj");
        let appium_home = directory.path().join("appium-home");
        let home = directory.path().join("home");
        let current = directory.path().join("project");
        let relative = Path::new("node_modules")
            .join("appium-xcuitest-driver")
            .join("node_modules")
            .join("appium-webdriveragent")
            .join("WebDriverAgent.xcodeproj");
        let installed = [
            explicit.clone(),
            appium_home.join(&relative),
            home.join(".appium").join(&relative),
            current.join(&relative),
        ];
        for project in &installed {
            std::fs::create_dir_all(project).expect("create installed WDA project");
            std::fs::write(project.join("project.pbxproj"), "// fixture")
                .expect("write installed WDA project");
        }

        assert_eq!(
            discover_wda_project_from(
                Some(explicit.clone()),
                Some(appium_home.clone()),
                Some(home.clone()),
                Some(current.clone()),
            ),
            Some(explicit)
        );
        assert_eq!(
            discover_wda_project_from(
                None,
                Some(appium_home.clone()),
                Some(home.clone()),
                Some(current.clone()),
            ),
            Some(appium_home.join(&relative))
        );
        assert_eq!(
            discover_wda_project_from(None, None, Some(home.clone()), Some(current.clone())),
            Some(home.join(".appium").join(&relative))
        );
        assert_eq!(
            discover_wda_project_from(None, None, None, Some(current.clone())),
            Some(current.join(relative))
        );
    }

    #[test]
    fn managed_appium_config_is_strict_and_redacts_its_executable() {
        let secret = "/private/tmp/APPIUM-EXECUTABLE-SECRET";
        let config = ManagedAppiumConfig::new(secret)
            .expect("managed Appium config")
            .with_port(0)
            .expect("automatic port")
            .with_base_path("/wd/hub")
            .expect("base path");
        let debug = format!("{config:?}");
        assert!(!debug.contains(secret));
        assert_eq!(config.base_path(), "/wd/hub");
        assert_eq!(config.port(), 0);

        assert_eq!(
            ManagedAppiumConfig::new("appium")
                .and_then(|config| config.with_base_path("wd/hub"))
                .expect_err("relative base path")
                .code(),
            "ios_appium_base_path_invalid"
        );
        assert_eq!(
            ManagedAppiumConfig::new("appium")
                .and_then(|config| config.with_base_path("/wd//hub"))
                .expect_err("ambiguous base path")
                .code(),
            "ios_appium_base_path_invalid"
        );
        assert_eq!(
            ManagedAppiumConfig::new("appium")
                .and_then(|config| config.with_startup_timeout(Duration::ZERO))
                .expect_err("zero timeout")
                .code(),
            "ios_appium_timeout_invalid"
        );
    }

    #[cfg(unix)]
    #[test]
    fn appium_process_group_disarms_after_cleanup_and_retains_drop_fallback() {
        let signals = Arc::new(Mutex::new(Vec::new()));
        {
            let mut process_group = OwnedProcessGroup::with_signaler(
                42_424,
                Box::new(RecordingProcessGroupSignaler {
                    signals: Arc::clone(&signals),
                }),
            );
            process_group
                .kill_and_disarm()
                .expect("converged process-group cleanup");
        }
        assert_eq!(
            *signals.lock().expect("read converged signals"),
            vec![(42_424, libc::SIGKILL)],
            "dropping a disarmed guard must not signal a stale PGID"
        );

        signals.lock().expect("reset recorded signals").clear();
        {
            let _process_group = OwnedProcessGroup::with_signaler(
                51_515,
                Box::new(RecordingProcessGroupSignaler {
                    signals: Arc::clone(&signals),
                }),
            );
        }
        assert_eq!(
            *signals.lock().expect("read fallback signals"),
            vec![(51_515, libc::SIGKILL)],
            "dropping an unconverged supervisor guard must retain fail-safe cleanup"
        );
    }

    #[cfg(target_os = "macos")]
    fn compile_fake_appium(directory: &Path) -> PathBuf {
        let source = directory.join("fake_appium.rs");
        let executable = directory.join("fake-appium");
        let descendant_marker = directory.join("descendant-port");
        let source_text = r###"
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;
use std::time::Duration;

const DESCENDANT_MARKER: &str = __DESCENDANT_MARKER__;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--descendant"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|_| std::process::exit(30));
        fs::write(DESCENDANT_MARKER, listener.local_addr().unwrap().port().to_string())
            .unwrap_or_else(|_| std::process::exit(31));
        loop { thread::sleep(Duration::from_secs(60)); }
    }
    if args.as_slice() == ["--version"] {
        println!("3.0.0-test");
        return;
    }
    let value = |flag: &str| {
        let index = args.iter().position(|value| value == flag).unwrap_or_else(|| std::process::exit(20));
        args.get(index + 1).cloned().unwrap_or_else(|| std::process::exit(21))
    };
    let address = value("--address");
    let port = value("--port");
    let base = value("--base-path");
    if address != "127.0.0.1" || args.len() != 6 {
        std::process::exit(22);
    }
    let _descendant = Command::new(std::env::current_exe().unwrap())
        .arg("--descendant")
        .spawn()
        .unwrap_or_else(|_| std::process::exit(32));
    let expected_path = if base == "/" { "/status".to_owned() } else { format!("{base}/status") };
    let listener = TcpListener::bind(format!("{address}:{port}")).unwrap_or_else(|_| std::process::exit(23));
    for stream in listener.incoming() {
        let mut stream = stream.unwrap_or_else(|_| std::process::exit(24));
        let mut request = [0u8; 4096];
        let count = stream.read(&mut request).unwrap_or_else(|_| std::process::exit(25));
        let first = String::from_utf8_lossy(&request[..count]);
        let ok = first.starts_with(&format!("GET {expected_path} HTTP/1.1"));
        let (status, body) = if ok {
            ("200 OK", r#"{"value":{"ready":true}}"#)
        } else {
            ("404 Not Found", r#"{"value":{"ready":false}}"#)
        };
        let response = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(response.as_bytes()).unwrap_or_else(|_| std::process::exit(26));
    }
}
"###
        .replace(
            "__DESCENDANT_MARKER__",
            &format!("{:?}", descendant_marker.to_string_lossy().as_ref()),
        );
        std::fs::write(&source, source_text).expect("write fake Appium source");
        let status = std::process::Command::new("rustc")
            .arg("--edition=2024")
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("compile fake Appium");
        assert!(status.success(), "fake Appium compilation failed");
        executable
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn managed_appium_launches_fixed_arguments_waits_until_ready_and_shuts_down() {
        let directory = tempdir().expect("temporary directory");
        let executable = compile_fake_appium(directory.path());
        let config = ManagedAppiumConfig::new(executable)
            .expect("managed Appium config")
            .with_base_path("/wd/hub")
            .expect("base path")
            .with_startup_timeout(Duration::from_secs(5))
            .expect("startup timeout");
        let runtime = SystemAppiumHost
            .start(config)
            .await
            .expect("managed Appium startup");
        assert!(runtime.endpoint().url().starts_with("http://127.0.0.1:"));
        assert!(runtime.endpoint().url().ends_with("/wd/hub"));
        assert_eq!(runtime.failure_code(), None);
        let debug = format!("{runtime:?}");
        assert!(!debug.contains(runtime.endpoint().url()));
        let descendant_port = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = std::fs::read_to_string(directory.path().join("descendant-port"))
                    && let Ok(port) = value.parse::<u16>()
                {
                    break port;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fake Appium descendant becomes ready");
        std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, descendant_port))
            .expect("fake Appium descendant is alive");
        runtime.shutdown().await.expect("managed Appium shutdown");
        assert!(
            std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, descendant_port)).is_err(),
            "managed Appium shutdown must terminate its descendant process group"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn managed_appium_detects_an_early_child_exit() {
        let directory = tempdir().expect("temporary directory");
        let executable = directory.path().join("early-exit-appium");
        write_executable(
            &executable,
            "#!/bin/sh\nif [ \"$1\" = '--version' ]; then echo 3.0.0-test; exit 0; fi\nexit 17\n",
        );
        let config = ManagedAppiumConfig::new(executable)
            .expect("managed Appium config")
            .with_startup_timeout(Duration::from_secs(2))
            .expect("startup timeout");
        assert_eq!(
            SystemAppiumHost
                .start(config)
                .await
                .expect_err("early exit must fail")
                .code(),
            "ios_appium_exited"
        );
    }
}
