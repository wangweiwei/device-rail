use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::{BufRead as _, Read as _, Write as _},
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex, mpsc as std_mpsc},
    time::Duration,
};

use devicerail_android_adb::{
    AdbDiscoveryReport, AndroidAdb, AndroidDeviceConfig, DiscoveredAndroidDevice, SystemAdbConfig,
};
use devicerail_core::{
    CancellationReason, DeviceDriver, DeviceLease, DevicePoolError, DriverAccess, DriverHandle,
    DriverRegistry, EndSession, EventStoreError, EvidenceStore, ExecutionControl,
    ExecutionController, LeaseOwnerId, MediaStreamError, MediaStreamWriter, MemoryEventStore,
    OperationContext, PendingEvent, PoolHealth, RegistryError, RuntimeError, RuntimeResult,
    ScreenshotPolicy, SessionCleanupError, SessionEventStore, SessionExportPageSnapshot,
    Sha256Digest, StartSession, TimeoutScope, cleanup_ended_session, now_ms,
    reconcile_missing_session_evidence,
};
use devicerail_desktop_driver::{
    DesktopIdentity, LinuxDisplayServer, SystemDesktopConfig, WaylandInputBackend,
    discover_native_driver,
};
use devicerail_distributed_router::{
    ConfiguredPeerServer as DistributedPeerServer, ConfiguredPeers as DistributedPeers,
    ConnectorError as DistributedConnectorError, PeerSecurity, PeerServerError,
    RegistryPeerService, RouterConfig as DistributedRouterConfig, connect_configured_peers,
    serve_peer_stream_until_cancelled,
};
use devicerail_driver_mock::MockDriver;
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_harmony_hdc::{
    DiscoveredHarmonyDevice, HarmonyDiscoveryReport, HarmonyHdc, SystemHdcConfig,
};
use devicerail_ios_host::{
    DiagnosticCheck, DiagnosticStatus, DoctorOptions, IosHostBackend, IosHostDevice, IosHostError,
    ManagedAppiumConfig, ManagedAppiumRuntime, ManagedIosConfig, ManagedIosRuntime,
    SystemAppiumHost, SystemIosHost, select_ready_ios_device,
};
use devicerail_ios_webdriver::{
    AppiumIosDriver, AppiumSessionRequest, AppiumTransport,
    HttpEndpointConfig as IosHttpEndpointConfig, IosDeviceConfig, IosDriver, SystemAppiumTransport,
    SystemMjpegFrameSource, SystemWdaTransport,
};
use devicerail_playwright_remote::{
    BridgeConfig as PlaywrightBridgeConfig, BrowserKind, discover_playwright_drivers,
};
use devicerail_plugin_driver::{DiscoveryConfig as PluginDiscoveryConfig, discover_plugin_drivers};
use devicerail_protocol::{
    ActionProtection, DeviceDisconnectResult, DeviceExecuteParams, DeviceId, DeviceInfo,
    DeviceSelectParams, DeviceSelectResult, DevicesListResult, ErrorInfo, EventSequence,
    EventsClearResult, EventsListParams, EventsStreamOpenParams, HelloParams, HelloResult,
    MAX_UI_SNAPSHOT_BYTES, MAX_VERDICT_EVIDENCE_REFERENCES, MAX_VERDICT_SUMMARY_LENGTH, MediaFrame,
    MediaStreamCaptureParams, MediaStreamCaptureResult, MediaStreamEndParams, MediaStreamEndResult,
    MediaStreamId, MediaStreamInfo, MediaStreamKind, MediaStreamStartParams,
    MediaStreamStartResult, PeerInfo, ProtocolIncompatibilityReason, ProtocolNegotiationError,
    ProtocolSelection, ProtocolVersion, RequestCancelParams, RequestCancelResult,
    RequestCancelStatus, RequestTimeoutMs, RpcError, RpcId, RpcParams, RpcRequest, RpcResponse,
    SessionCurrentResult, SessionEndParams, SessionExportParams, SessionId, SessionInfo,
    SessionOutcome, SessionTargetParams, SystemDescribeResult, TestEvent, TestEventPayload,
    TransportInfo, UI_SNAPSHOT_MEDIA_TYPE, UiSnapshot, UiSnapshotGetParams, VerdictRecordParams,
    VerdictRecordResult, Viewport, feature, is_semantic_action_name, negotiate_features,
    negotiate_protocol, supported_protocol_offer,
};
use devicerail_rdp_remote::{
    BridgeConfig as RdpBridgeConfig, RdpDriver, RdpTarget, SystemRdpBridge,
};
use devicerail_remote_auth::{
    AuditDecision, AuditEvent, AuditLog, AuditOutcome, AuthChallengeRequest, AuthProofRequest,
    AuthSuccess, AuthenticatedPrincipal, Authenticator, CredentialStore, Permission,
    required_permission,
};
use devicerail_websocket_transport::{EventStreamServer, TransportError as StreamTransportError};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{
        AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _,
        BufReader as TokioBufReader,
    },
    net::{TcpListener, TcpStream},
    sync::{Mutex as TokioMutex, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout_at},
};
use uuid::Uuid;

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;
const DRIVER_ERROR: i32 = -32000;
const HANDSHAKE_REQUIRED: i32 = -32001;
const HANDSHAKE_ALREADY_COMPLETED: i32 = -32002;
const PROTOCOL_VERSION_INCOMPATIBLE: i32 = -32003;
const REQUIRED_FEATURE_UNSUPPORTED: i32 = -32004;
const SESSION_REQUIRED: i32 = -32005;
const SESSION_ERROR: i32 = -32006;
const REQUEST_CANCELLED: i32 = -32007;
const REQUEST_TIMED_OUT: i32 = -32008;
const REQUEST_ID_IN_USE: i32 = -32009;
const TOO_MANY_REQUESTS: i32 = -32010;
const DEVICE_ROUTING_ERROR: i32 = -32011;
const RESPONSE_FRAME_TOO_LARGE: i32 = -32012;
const EVENT_STREAM_ERROR: i32 = -32013;
const DEVICE_POOL_ERROR: i32 = -32014;
const REMOTE_AUTH_ERROR: i32 = -32015;
const MEDIA_STREAM_ERROR: i32 = -32016;
const UI_SNAPSHOT_ERROR: i32 = -32017;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
const CONNECTION_CLEANUP_RESERVE: Duration = Duration::from_millis(250);
const MAX_IN_FLIGHT_REQUESTS: usize = 256;
const INPUT_QUEUE_CAPACITY: usize = 256;
const RESPONSE_QUEUE_CAPACITY: usize = 256;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const MAX_LOOPBACK_CONNECTIONS: usize = 64;
const MAX_PEER_SERVER_CONNECTIONS: usize = 64;
const ANDROID_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const ANDROID_RUNTIME_COMMAND_TIMEOUT: Duration = Duration::from_secs(65);
const HARMONY_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const HARMONY_RUNTIME_COMMAND_TIMEOUT: Duration = Duration::from_secs(65);
const DESKTOP_STARTUP_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_DESKTOP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DESKTOP_COMMAND_TIMEOUT_MS: u64 = 5 * 60_000;
const IOS_RUNTIME_REQUEST_TIMEOUT_MS: u64 = 65_000;
const IOS_APPIUM_DOCTOR_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS: u64 = 600;
const MAX_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS: u64 = 3_600;
const IOS_HOTPLUG_RETRY_MIN: Duration = Duration::from_secs(1);
const IOS_HOTPLUG_RETRY_MAX: Duration = Duration::from_secs(30);
const IOS_SUPERVISOR_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const PLAYWRIGHT_STARTUP_TIMEOUT_MS: u64 = 30_000;
const PLUGIN_STARTUP_TIMEOUT_MS: u64 = 120_000;
const DISTRIBUTED_STARTUP_TIMEOUT_MS: u64 = 30_000;
const REMOTE_AUTH_DEADLINE: Duration = Duration::from_secs(15);
const REMOTE_AUTH_MAX_FRAMES: usize = 8;
const MAX_ACTIVE_MEDIA_STREAMS: usize = 2;
const MAX_MEDIA_STREAMS_PER_SESSION: usize = 8;
const MAX_MEDIA_FRAMES_PER_STREAM: u64 = 1_000;
const MIN_MEDIA_CAPTURE_INTERVAL: Duration = Duration::from_millis(50);
const MEDIA_STREAM_CLOSE_GRACE: Duration = Duration::from_secs(1);
const DEFAULT_EVIDENCE_DIR: &str = ".devicerail/evidence";
const DEFAULT_PLAYWRIGHT_HELPER: &str = "packages/playwright-driver/dist/helper.js";

type Registry = DriverRegistry<MemoryEventStore>;
type DeviceRoute = DriverHandle<MemoryEventStore>;
type DeviceAccess = DriverAccess<MemoryEventStore>;
type RpcResult = Result<Value, RpcError>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AndroidDiscoveryMode {
    #[default]
    Auto,
    Off,
    Required,
}

impl AndroidDiscoveryMode {
    fn parse(value: Option<&OsStr>) -> Result<Self, DaemonStartupError> {
        match value {
            None => Ok(Self::Auto),
            Some(value) => match value.to_str() {
                Some("auto") => Ok(Self::Auto),
                Some("off") => Ok(Self::Off),
                Some("required") => Ok(Self::Required),
                Some(_) | None => Err(DaemonStartupError::InvalidAndroidMode),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HarmonyDiscoveryMode {
    Auto,
    #[default]
    Off,
    Required,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DesktopDiscoveryMode {
    Auto,
    #[default]
    Off,
    Required,
}

impl DesktopDiscoveryMode {
    fn parse(value: Option<&OsStr>) -> Result<Self, DaemonStartupError> {
        match value {
            None => Ok(Self::Off),
            Some(value) => match value.to_str() {
                Some("auto") => Ok(Self::Auto),
                Some("off") => Ok(Self::Off),
                Some("required") => Ok(Self::Required),
                Some(_) | None => Err(DaemonStartupError::InvalidDesktopMode),
            },
        }
    }
}

#[derive(Clone, PartialEq)]
struct DesktopStartupConfig {
    mode: DesktopDiscoveryMode,
    identity: DesktopIdentity,
    system: SystemDesktopConfig,
}

impl std::fmt::Debug for DesktopStartupConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopStartupConfig")
            .field("configuration", &"[REDACTED]")
            .finish()
    }
}

#[derive(Default)]
struct DesktopConfigValues {
    mode: Option<OsString>,
    id: Option<OsString>,
    name: Option<OsString>,
    os_version: Option<OsString>,
    command_timeout_ms: Option<OsString>,
    macos_screencapture: Option<OsString>,
    windows_powershell: Option<OsString>,
    linux_display_server: Option<OsString>,
    x11_import: Option<OsString>,
    x11_xdotool: Option<OsString>,
    wayland_grim: Option<OsString>,
    wayland_input: Option<OsString>,
    wayland_ydotool: Option<OsString>,
    wayland_wtype: Option<OsString>,
    wayland_viewport_width: Option<OsString>,
    wayland_viewport_height: Option<OsString>,
    wayland_viewport_scale_factor: Option<OsString>,
}

impl DesktopConfigValues {
    fn has_auxiliary_setting(&self) -> bool {
        self.id.is_some()
            || self.name.is_some()
            || self.os_version.is_some()
            || self.command_timeout_ms.is_some()
            || self.macos_screencapture.is_some()
            || self.windows_powershell.is_some()
            || self.linux_display_server.is_some()
            || self.x11_import.is_some()
            || self.x11_xdotool.is_some()
            || self.wayland_grim.is_some()
            || self.wayland_input.is_some()
            || self.wayland_ydotool.is_some()
            || self.wayland_wtype.is_some()
            || self.wayland_viewport_width.is_some()
            || self.wayland_viewport_height.is_some()
            || self.wayland_viewport_scale_factor.is_some()
    }

    fn has_linux_setting(&self) -> bool {
        self.linux_display_server.is_some()
            || self.x11_import.is_some()
            || self.x11_xdotool.is_some()
            || self.wayland_grim.is_some()
            || self.wayland_input.is_some()
            || self.wayland_ydotool.is_some()
            || self.wayland_wtype.is_some()
            || self.wayland_viewport_width.is_some()
            || self.wayland_viewport_height.is_some()
            || self.wayland_viewport_scale_factor.is_some()
    }
}

impl HarmonyDiscoveryMode {
    fn parse(value: Option<&OsStr>) -> Result<Self, DaemonStartupError> {
        match value {
            None => Ok(Self::Off),
            Some(value) => match value.to_str() {
                Some("auto") => Ok(Self::Auto),
                Some("off") => Ok(Self::Off),
                Some("required") => Ok(Self::Required),
                Some(_) | None => Err(DaemonStartupError::InvalidHarmonyMode),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IosManagedPolicy {
    Auto,
    Required,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum IosBackendKind {
    #[default]
    DirectWda,
    Appium,
}

impl IosBackendKind {
    fn parse(value: Option<&OsStr>) -> Result<Self, DaemonStartupError> {
        match value {
            None => Ok(Self::DirectWda),
            Some(value) => match value.to_str() {
                Some("direct-wda") => Ok(Self::DirectWda),
                Some("appium") => Ok(Self::Appium),
                Some(_) | None => Err(DaemonStartupError::InvalidIosBackend),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum IosSessionTarget {
    #[default]
    Native,
    Safari,
}

impl IosSessionTarget {
    fn parse(value: Option<&OsStr>) -> Result<Self, DaemonStartupError> {
        match value {
            None => Ok(Self::Native),
            Some(value) => match value.to_str() {
                Some("native") => Ok(Self::Native),
                Some("safari") => Ok(Self::Safari),
                Some(_) | None => Err(DaemonStartupError::InvalidIosSessionTarget),
            },
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum IosDriverBackendConfig {
    DirectWda,
    Appium {
        server: AppiumServerConfig,
        new_command_timeout_seconds: u64,
    },
}

#[derive(Clone, PartialEq, Eq)]
enum AppiumServerConfig {
    External(IosHttpEndpointConfig),
    Managed(ManagedAppiumConfig),
}

impl std::fmt::Debug for IosDriverBackendConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DirectWda => formatter.write_str("DirectWda"),
            Self::Appium {
                server,
                new_command_timeout_seconds,
            } => formatter
                .debug_struct("Appium")
                .field(
                    "server",
                    &match server {
                        AppiumServerConfig::External(_) => "external:[REDACTED]",
                        AppiumServerConfig::Managed(_) => "managed:[REDACTED]",
                    },
                )
                .field("new_command_timeout_seconds", new_command_timeout_seconds)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ExternalIosStartupConfig {
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
    device_udid: String,
    device: IosDeviceConfig,
    wda_endpoint: Option<IosHttpEndpointConfig>,
    mjpeg_endpoint: Option<IosHttpEndpointConfig>,
}

#[derive(Clone, PartialEq, Eq)]
struct ManagedIosStartupConfig {
    policy: IosManagedPolicy,
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
    host: ManagedIosHostConfig,
}

#[derive(Clone, PartialEq, Eq)]
enum ManagedIosHostConfig {
    Wda(ManagedIosConfig),
    AppiumDiscovery { device_udid: Option<String> },
}

#[derive(Clone, PartialEq, Eq)]
enum IosStartupConfig {
    External(ExternalIosStartupConfig),
    Managed(ManagedIosStartupConfig),
}

impl std::fmt::Debug for IosStartupConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::External(config) => formatter
                .debug_struct("ExternalIosStartupConfig")
                .field("backend", &config.backend)
                .field("session_target", &config.session_target)
                .field("device", &"[REDACTED]")
                .field("wda_configured", &config.wda_endpoint.is_some())
                .field("mjpeg_configured", &config.mjpeg_endpoint.is_some())
                .finish(),
            Self::Managed(config) => formatter
                .debug_struct("ManagedIosStartupConfig")
                .field("policy", &config.policy)
                .field("backend", &config.backend)
                .field("session_target", &config.session_target)
                .field("configuration", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Default)]
struct IosConfigValues {
    mode: Option<OsString>,
    backend: Option<OsString>,
    session_target: Option<OsString>,
    appium_endpoint: Option<OsString>,
    appium_path: Option<OsString>,
    appium_port: Option<OsString>,
    appium_base_path: Option<OsString>,
    appium_new_command_timeout_seconds: Option<OsString>,
    wda_endpoint: Option<OsString>,
    device_token: Option<OsString>,
    device_name: Option<OsString>,
    os_version: Option<OsString>,
    mjpeg_endpoint: Option<OsString>,
    wda_project: Option<OsString>,
    derived_data: Option<OsString>,
    iproxy_path: Option<OsString>,
    local_port: Option<OsString>,
    remote_port: Option<OsString>,
    allow_provisioning_updates: Option<OsString>,
}

struct ExternalIosConfigValues {
    session_target: IosSessionTarget,
    session_target_explicit: bool,
    appium_endpoint: Option<OsString>,
    appium_path: Option<OsString>,
    appium_port: Option<OsString>,
    appium_base_path: Option<OsString>,
    appium_new_command_timeout_seconds: u64,
    appium_new_command_timeout_explicit: bool,
    wda_endpoint: Option<OsString>,
    device_token: Option<OsString>,
    device_name: Option<OsString>,
    os_version: Option<OsString>,
    mjpeg_endpoint: Option<OsString>,
}

#[derive(Default)]
struct NativePlatformConfigValues {
    harmony_mode: Option<OsString>,
    hdc_path: Option<OsString>,
    ios: IosConfigValues,
    desktop: DesktopConfigValues,
}

#[derive(Clone, Debug, PartialEq)]
struct DaemonConfig {
    evidence_dir: PathBuf,
    android_mode: AndroidDiscoveryMode,
    adb_path: PathBuf,
    harmony_mode: HarmonyDiscoveryMode,
    hdc_path: PathBuf,
    ios: Option<IosStartupConfig>,
    desktop: DesktopStartupConfig,
    screenshot_policy: ScreenshotPolicy,
    playwright: Option<PlaywrightStartupConfig>,
    rpc_listen: Option<SocketAddr>,
    rdp: Option<RdpStartupConfig>,
    plugins: Option<PluginStartupConfig>,
    distributed_peers: Option<DistributedPeers>,
    distributed_server: Option<DistributedPeerServer>,
    remote_security: Option<RemoteSecurityStartupConfig>,
}

#[derive(Clone, PartialEq, Eq)]
struct RemoteSecurityStartupConfig {
    credential_store: PathBuf,
    audit_log: PathBuf,
}

impl std::fmt::Debug for RemoteSecurityStartupConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteSecurityStartupConfig")
            .field("credential_store", &"[REDACTED]")
            .field("audit_log", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone)]
struct RemoteSecurity {
    authenticator: Arc<Authenticator>,
    audit: Arc<AuditLog>,
}

impl std::fmt::Debug for RemoteSecurity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteSecurity")
            .field("authenticator", &self.authenticator)
            .field("audit", &self.audit)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PluginStartupConfig {
    discovery: PluginDiscoveryConfig,
}

impl std::fmt::Debug for PluginStartupConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginStartupConfig")
            .field("discovery", &self.discovery)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct RdpStartupConfig {
    name: String,
    bridge: RdpBridgeConfig,
}

impl std::fmt::Debug for RdpStartupConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RdpStartupConfig")
            .field("name", &self.name)
            .field("bridge", &self.bridge)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PlaywrightStartupConfig {
    endpoint: String,
    browser: BrowserKind,
    node_path: PathBuf,
    helper_path: PathBuf,
}

#[derive(Default)]
struct PlaywrightConfigValues {
    endpoint: Option<OsString>,
    browser: Option<OsString>,
    node: Option<OsString>,
    helper: Option<OsString>,
}

impl std::fmt::Debug for PlaywrightStartupConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlaywrightStartupConfig")
            .field("endpoint", &"[REDACTED]")
            .field("browser", &self.browser)
            .field("node_path", &self.node_path)
            .field("helper_path", &self.helper_path)
            .finish()
    }
}

fn parse_ios_startup(
    values: IosConfigValues,
) -> Result<Option<IosStartupConfig>, DaemonStartupError> {
    let IosConfigValues {
        mode,
        backend,
        session_target,
        appium_endpoint,
        appium_path,
        appium_port,
        appium_base_path,
        appium_new_command_timeout_seconds,
        wda_endpoint,
        device_token,
        device_name,
        os_version,
        mjpeg_endpoint,
        wda_project,
        derived_data,
        iproxy_path,
        local_port,
        remote_port,
        allow_provisioning_updates,
    } = values;
    let backend_explicit = backend.is_some();
    let session_target_explicit = session_target.is_some();
    let appium_new_command_timeout_explicit = appium_new_command_timeout_seconds.is_some();
    let appium_new_command_timeout_seconds = parse_ios_appium_new_command_timeout_seconds(
        appium_new_command_timeout_seconds.as_deref(),
    )?;
    let has_appium_process_setting =
        appium_path.is_some() || appium_port.is_some() || appium_base_path.is_some();
    let backend = IosBackendKind::parse(backend.as_deref())?;
    let session_target = IosSessionTarget::parse(session_target.as_deref())?;
    let has_managed_setting = wda_project.is_some()
        || derived_data.is_some()
        || iproxy_path.is_some()
        || local_port.is_some()
        || remote_port.is_some()
        || allow_provisioning_updates.is_some();
    let Some(mode) = mode else {
        if has_managed_setting {
            return Err(DaemonStartupError::IosManagedModeRequired);
        }
        return parse_external_ios_startup(
            backend,
            backend_explicit,
            ExternalIosConfigValues {
                session_target,
                session_target_explicit,
                appium_endpoint,
                appium_path,
                appium_port,
                appium_base_path,
                appium_new_command_timeout_seconds,
                appium_new_command_timeout_explicit,
                wda_endpoint,
                device_token,
                device_name,
                os_version,
                mjpeg_endpoint,
            },
        );
    };
    let mode = mode.to_str().ok_or(DaemonStartupError::InvalidIosMode)?;
    if mode == "off" {
        if wda_endpoint.is_some()
            || device_token.is_some()
            || device_name.is_some()
            || os_version.is_some()
            || mjpeg_endpoint.is_some()
            || has_managed_setting
            || backend_explicit
            || session_target_explicit
            || appium_endpoint.is_some()
            || has_appium_process_setting
            || appium_new_command_timeout_explicit
        {
            return Err(DaemonStartupError::IosSettingsWhileDisabled);
        }
        return Ok(None);
    }
    let policy = match mode {
        "auto" => IosManagedPolicy::Auto,
        "required" => IosManagedPolicy::Required,
        _ => return Err(DaemonStartupError::InvalidIosMode),
    };
    validate_ios_session_target(backend, session_target)?;
    if wda_endpoint.is_some()
        || device_name.is_some()
        || os_version.is_some()
        || mjpeg_endpoint.is_some()
    {
        return Err(DaemonStartupError::IosManagedExternalConflict);
    }
    let backend = parse_ios_driver_backend(
        backend,
        appium_endpoint,
        appium_path,
        appium_port,
        appium_base_path,
        appium_new_command_timeout_seconds,
        appium_new_command_timeout_explicit,
    )?;
    let device_udid = device_token
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidManagedIosConfiguration)?;
    let host = if matches!(backend, IosDriverBackendConfig::Appium { .. }) && !has_managed_setting {
        ManagedIosHostConfig::AppiumDiscovery { device_udid }
    } else {
        let project = wda_project.ok_or(DaemonStartupError::IosManagedProjectRequired)?;
        let mut host = ManagedIosConfig::new(PathBuf::from(project))
            .map_err(|_| DaemonStartupError::InvalidManagedIosConfiguration)?;
        host.device_udid = device_udid;
        if let Some(value) = derived_data {
            host.derived_data = parse_ios_path(value)?;
        }
        if let Some(value) = iproxy_path {
            host.iproxy_path = parse_ios_path(value)?;
        }
        if let Some(value) = local_port {
            host.local_port = parse_ios_port(value, true)?;
        }
        if let Some(value) = remote_port {
            host.remote_port = parse_ios_port(value, false)?;
        }
        if let Some(value) = allow_provisioning_updates {
            host.allow_provisioning_updates = match value.to_str() {
                Some("1" | "true" | "yes") => true,
                Some("0" | "false" | "no") => false,
                _ => return Err(DaemonStartupError::InvalidManagedIosConfiguration),
            };
        }
        host.validate()
            .map_err(|_| DaemonStartupError::InvalidManagedIosConfiguration)?;
        ManagedIosHostConfig::Wda(host)
    };
    Ok(Some(IosStartupConfig::Managed(ManagedIosStartupConfig {
        policy,
        backend,
        session_target,
        host,
    })))
}

fn parse_external_ios_startup(
    backend: IosBackendKind,
    backend_explicit: bool,
    values: ExternalIosConfigValues,
) -> Result<Option<IosStartupConfig>, DaemonStartupError> {
    let ExternalIosConfigValues {
        session_target,
        session_target_explicit,
        appium_endpoint,
        appium_path,
        appium_port,
        appium_base_path,
        appium_new_command_timeout_seconds,
        appium_new_command_timeout_explicit,
        wda_endpoint,
        device_token,
        device_name,
        os_version,
        mjpeg_endpoint,
    } = values;
    validate_ios_session_target(backend, session_target)?;
    validate_ios_appium_timeout_backend(backend, appium_new_command_timeout_explicit)?;
    if wda_endpoint.is_none() && backend == IosBackendKind::DirectWda {
        if device_token.is_some()
            || device_name.is_some()
            || os_version.is_some()
            || mjpeg_endpoint.is_some()
            || backend_explicit
            || session_target_explicit
            || appium_endpoint.is_some()
            || appium_path.is_some()
            || appium_port.is_some()
            || appium_base_path.is_some()
        {
            return Err(DaemonStartupError::IosWdaEndpointRequired);
        }
        return Ok(None);
    }
    let device_token = device_token.ok_or(DaemonStartupError::IosDeviceTokenRequired)?;
    let wda_endpoint = wda_endpoint
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?
        .map(|endpoint| {
            IosHttpEndpointConfig::new(endpoint)
                .and_then(|endpoint| {
                    endpoint.with_request_timeout_ms(IOS_RUNTIME_REQUEST_TIMEOUT_MS)
                })
                .map_err(|_| DaemonStartupError::InvalidIosConfiguration)
        })
        .transpose()?;
    let device_token = device_token
        .into_string()
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?;
    let device_name = device_name
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?
        .unwrap_or_else(|| "iOS device".to_owned());
    let os_version = os_version
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?;
    if wda_endpoint
        .as_ref()
        .is_some_and(|endpoint| !endpoint.is_numeric_loopback())
    {
        return Err(DaemonStartupError::InvalidIosConfiguration);
    }
    let mjpeg_endpoint = mjpeg_endpoint
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?
        .map(|endpoint| {
            IosHttpEndpointConfig::new(endpoint)
                .and_then(|endpoint| {
                    endpoint.with_request_timeout_ms(IOS_RUNTIME_REQUEST_TIMEOUT_MS)
                })
                .map_err(|_| DaemonStartupError::InvalidIosConfiguration)
        })
        .transpose()?;
    if mjpeg_endpoint
        .as_ref()
        .is_some_and(|endpoint| !endpoint.is_numeric_loopback())
    {
        return Err(DaemonStartupError::InvalidIosConfiguration);
    }
    let device_udid = device_token.clone();
    let device = IosDeviceConfig::new(device_token, device_name, os_version)
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?;
    let backend = parse_ios_driver_backend(
        backend,
        appium_endpoint,
        appium_path,
        appium_port,
        appium_base_path,
        appium_new_command_timeout_seconds,
        appium_new_command_timeout_explicit,
    )?;
    Ok(Some(IosStartupConfig::External(ExternalIosStartupConfig {
        backend,
        session_target,
        device_udid,
        device,
        wda_endpoint,
        mjpeg_endpoint,
    })))
}

fn validate_ios_session_target(
    backend: IosBackendKind,
    session_target: IosSessionTarget,
) -> Result<(), DaemonStartupError> {
    if session_target == IosSessionTarget::Safari && backend != IosBackendKind::Appium {
        return Err(DaemonStartupError::IosSessionTargetRequiresAppium);
    }
    Ok(())
}

fn validate_ios_appium_timeout_backend(
    backend: IosBackendKind,
    explicit: bool,
) -> Result<(), DaemonStartupError> {
    if explicit && backend != IosBackendKind::Appium {
        return Err(DaemonStartupError::IosAppiumEndpointRequiresBackend);
    }
    Ok(())
}

fn parse_ios_appium_new_command_timeout_seconds(
    value: Option<&OsStr>,
) -> Result<u64, DaemonStartupError> {
    let Some(value) = value else {
        return Ok(DEFAULT_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS);
    };
    value
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1..=MAX_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS).contains(value))
        .ok_or(DaemonStartupError::InvalidIosAppiumNewCommandTimeout)
}

fn parse_ios_driver_backend(
    backend: IosBackendKind,
    appium_endpoint: Option<OsString>,
    appium_path: Option<OsString>,
    appium_port: Option<OsString>,
    appium_base_path: Option<OsString>,
    new_command_timeout_seconds: u64,
    new_command_timeout_explicit: bool,
) -> Result<IosDriverBackendConfig, DaemonStartupError> {
    validate_ios_appium_timeout_backend(backend, new_command_timeout_explicit)?;
    let has_managed_setting =
        appium_path.is_some() || appium_port.is_some() || appium_base_path.is_some();
    match backend {
        IosBackendKind::DirectWda if appium_endpoint.is_none() && !has_managed_setting => {
            Ok(IosDriverBackendConfig::DirectWda)
        }
        IosBackendKind::DirectWda => Err(DaemonStartupError::IosAppiumEndpointRequiresBackend),
        IosBackendKind::Appium if appium_endpoint.is_some() && has_managed_setting => {
            Err(DaemonStartupError::IosAppiumServerConflict)
        }
        IosBackendKind::Appium if appium_endpoint.is_none() && appium_path.is_none() => {
            Err(DaemonStartupError::IosAppiumEndpointRequired)
        }
        IosBackendKind::Appium if appium_endpoint.is_some() => {
            let endpoint = appium_endpoint.expect("guarded Appium endpoint");
            let endpoint = endpoint
                .into_string()
                .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            let endpoint = IosHttpEndpointConfig::new(endpoint)
                .and_then(|endpoint| {
                    endpoint.with_request_timeout_ms(IOS_RUNTIME_REQUEST_TIMEOUT_MS)
                })
                .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            if !endpoint.is_numeric_loopback() {
                return Err(DaemonStartupError::InvalidIosAppiumConfiguration);
            }
            Ok(IosDriverBackendConfig::Appium {
                server: AppiumServerConfig::External(endpoint),
                new_command_timeout_seconds,
            })
        }
        IosBackendKind::Appium => {
            let path = PathBuf::from(appium_path.expect("guarded Appium path"));
            let mut config = ManagedAppiumConfig::new(path)
                .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            if let Some(port) = appium_port {
                let port = parse_ios_port(port, true)
                    .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
                config = config
                    .with_port(port)
                    .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            }
            if let Some(base_path) = appium_base_path {
                let base_path = base_path
                    .into_string()
                    .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
                config = config
                    .with_base_path(base_path)
                    .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            }
            Ok(IosDriverBackendConfig::Appium {
                server: AppiumServerConfig::Managed(config),
                new_command_timeout_seconds,
            })
        }
    }
}

fn parse_ios_path(value: OsString) -> Result<PathBuf, DaemonStartupError> {
    let path = PathBuf::from(value);
    if path.as_os_str().is_empty() {
        Err(DaemonStartupError::InvalidManagedIosConfiguration)
    } else {
        Ok(path)
    }
}

fn parse_ios_port(value: OsString, allow_zero: bool) -> Result<u16, DaemonStartupError> {
    let port = value
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or(DaemonStartupError::InvalidManagedIosConfiguration)?;
    if !allow_zero && port == 0 {
        return Err(DaemonStartupError::InvalidManagedIosConfiguration);
    }
    Ok(port)
}

fn parse_desktop_startup(
    values: DesktopConfigValues,
) -> Result<DesktopStartupConfig, DaemonStartupError> {
    let mode = DesktopDiscoveryMode::parse(values.mode.as_deref())?;
    if mode == DesktopDiscoveryMode::Off {
        if values.has_auxiliary_setting() {
            return Err(DaemonStartupError::DesktopModeRequiredForSettings);
        }
        return Ok(DesktopStartupConfig {
            mode,
            identity: DesktopIdentity::new("desktop-local", "Local desktop", None),
            system: SystemDesktopConfig::default(),
        });
    }

    if (!cfg!(target_os = "macos") && values.macos_screencapture.is_some())
        || (!cfg!(target_os = "windows") && values.windows_powershell.is_some())
        || (!cfg!(target_os = "linux") && values.has_linux_setting())
    {
        return Err(DaemonStartupError::DesktopSettingUnsupportedOnHost);
    }
    let has_x11_override = values.x11_import.is_some() || values.x11_xdotool.is_some();
    let has_wayland_override = values.wayland_grim.is_some()
        || values.wayland_input.is_some()
        || values.wayland_ydotool.is_some()
        || values.wayland_wtype.is_some()
        || values.wayland_viewport_width.is_some()
        || values.wayland_viewport_height.is_some()
        || values.wayland_viewport_scale_factor.is_some();

    let DesktopConfigValues {
        mode: _,
        id,
        name,
        os_version,
        command_timeout_ms,
        macos_screencapture,
        windows_powershell,
        linux_display_server,
        x11_import,
        x11_xdotool,
        wayland_grim,
        wayland_input,
        wayland_ydotool,
        wayland_wtype,
        wayland_viewport_width,
        wayland_viewport_height,
        wayland_viewport_scale_factor,
    } = values;

    let id = desktop_string(id, "desktop-local")?;
    let name = desktop_string(name, "Local desktop")?;
    let os_version = desktop_optional_string(os_version)?;
    let identity = DesktopIdentity::new(id, name, os_version);
    identity
        .validate()
        .map_err(|_| DaemonStartupError::InvalidDesktopConfiguration)?;

    let mut system = SystemDesktopConfig {
        command_timeout: DEFAULT_DESKTOP_COMMAND_TIMEOUT,
        ..SystemDesktopConfig::default()
    };
    if let Some(timeout_ms) = command_timeout_ms {
        let timeout_ms = parse_desktop_u64(timeout_ms)?;
        if timeout_ms == 0 || timeout_ms > MAX_DESKTOP_COMMAND_TIMEOUT_MS {
            return Err(DaemonStartupError::InvalidDesktopConfiguration);
        }
        system.command_timeout = Duration::from_millis(timeout_ms);
    }
    set_desktop_path(&mut system.macos_screencapture, macos_screencapture)?;
    set_desktop_path(&mut system.windows_powershell, windows_powershell)?;
    set_desktop_path(&mut system.x11_import, x11_import)?;
    set_desktop_path(&mut system.x11_xdotool, x11_xdotool)?;
    set_desktop_path(&mut system.wayland_grim, wayland_grim)?;
    set_desktop_path(&mut system.wayland_ydotool, wayland_ydotool)?;
    set_desktop_path(&mut system.wayland_wtype, wayland_wtype)?;

    system.linux_display_server = match linux_display_server.as_deref() {
        None => None,
        Some(value) => match value.to_str() {
            Some("x11") => Some(LinuxDisplayServer::X11),
            Some("wayland") => Some(LinuxDisplayServer::Wayland),
            Some(_) | None => return Err(DaemonStartupError::InvalidDesktopConfiguration),
        },
    };
    match system.linux_display_server {
        None if has_x11_override || has_wayland_override => {
            return Err(DaemonStartupError::InvalidDesktopConfiguration);
        }
        Some(LinuxDisplayServer::X11) if has_wayland_override => {
            return Err(DaemonStartupError::InvalidDesktopConfiguration);
        }
        Some(LinuxDisplayServer::Wayland) if has_x11_override => {
            return Err(DaemonStartupError::InvalidDesktopConfiguration);
        }
        None | Some(_) => {}
    }
    system.wayland_input_backend = match wayland_input.as_deref() {
        None => None,
        Some(value) => match value.to_str() {
            Some("auto") => None,
            Some("ydotool") => Some(WaylandInputBackend::Ydotool),
            Some("wtype") => Some(WaylandInputBackend::Wtype),
            Some(_) | None => return Err(DaemonStartupError::InvalidDesktopConfiguration),
        },
    };

    system.wayland_viewport = match (
        wayland_viewport_width,
        wayland_viewport_height,
        wayland_viewport_scale_factor,
    ) {
        (None, None, None) => None,
        (Some(width), Some(height), Some(scale_factor)) => {
            if system.linux_display_server != Some(LinuxDisplayServer::Wayland) {
                return Err(DaemonStartupError::InvalidDesktopConfiguration);
            }
            Some(Viewport {
                width: parse_desktop_u32(width)?,
                height: parse_desktop_u32(height)?,
                scale_factor: parse_desktop_f64(scale_factor)?,
            })
        }
        _ => return Err(DaemonStartupError::InvalidDesktopConfiguration),
    };
    if system.linux_display_server == Some(LinuxDisplayServer::Wayland)
        && system.wayland_viewport.is_none()
    {
        return Err(DaemonStartupError::InvalidDesktopConfiguration);
    }
    system
        .validate()
        .map_err(|_| DaemonStartupError::InvalidDesktopConfiguration)?;

    Ok(DesktopStartupConfig {
        mode,
        identity,
        system,
    })
}

fn desktop_string(value: Option<OsString>, default: &str) -> Result<String, DaemonStartupError> {
    value
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidDesktopConfiguration)
        .map(|value| value.unwrap_or_else(|| default.to_owned()))
}

fn desktop_optional_string(value: Option<OsString>) -> Result<Option<String>, DaemonStartupError> {
    value
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidDesktopConfiguration)
}

fn set_desktop_path(
    target: &mut PathBuf,
    configured: Option<OsString>,
) -> Result<(), DaemonStartupError> {
    let Some(configured) = configured else {
        return Ok(());
    };
    let configured = PathBuf::from(configured);
    if configured.as_os_str().is_empty() {
        return Err(DaemonStartupError::InvalidDesktopConfiguration);
    }
    *target = configured;
    Ok(())
}

fn parse_desktop_u64(value: OsString) -> Result<u64, DaemonStartupError> {
    value
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
        .ok_or(DaemonStartupError::InvalidDesktopConfiguration)
}

fn parse_desktop_u32(value: OsString) -> Result<u32, DaemonStartupError> {
    value
        .to_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse().ok())
        .ok_or(DaemonStartupError::InvalidDesktopConfiguration)
}

fn parse_desktop_f64(value: OsString) -> Result<f64, DaemonStartupError> {
    value
        .to_str()
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse().ok())
        .ok_or(DaemonStartupError::InvalidDesktopConfiguration)
}

impl DaemonConfig {
    fn from_values(
        evidence_dir: Option<OsString>,
        android_mode: Option<OsString>,
        adb_path: Option<OsString>,
        native_platforms: NativePlatformConfigValues,
        screenshot_policy: Option<OsString>,
        playwright_values: PlaywrightConfigValues,
    ) -> Result<Self, DaemonStartupError> {
        let NativePlatformConfigValues {
            harmony_mode,
            hdc_path,
            ios: ios_values,
            desktop: desktop_values,
        } = native_platforms;
        let evidence_dir = evidence_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_EVIDENCE_DIR));
        if evidence_dir.as_os_str().is_empty() {
            return Err(DaemonStartupError::InvalidEvidenceDirectory);
        }
        let android_mode = AndroidDiscoveryMode::parse(android_mode.as_deref())?;
        let adb_path = adb_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("adb"));
        if adb_path.as_os_str().is_empty() {
            return Err(DaemonStartupError::InvalidAdbPath);
        }
        let harmony_mode = HarmonyDiscoveryMode::parse(harmony_mode.as_deref())?;
        if harmony_mode == HarmonyDiscoveryMode::Off && hdc_path.is_some() {
            return Err(DaemonStartupError::HarmonyModeRequiredForHdcPath);
        }
        let hdc_path = hdc_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("hdc"));
        if hdc_path.as_os_str().is_empty() {
            return Err(DaemonStartupError::InvalidHdcPath);
        }
        let ios = parse_ios_startup(ios_values)?;
        let desktop = parse_desktop_startup(desktop_values)?;
        let screenshot_policy = match screenshot_policy.as_deref() {
            None => ScreenshotPolicy::Capture,
            Some(value) => match value.to_str() {
                Some("capture") => ScreenshotPolicy::Capture,
                Some("omit") => ScreenshotPolicy::Omit,
                Some(_) | None => return Err(DaemonStartupError::InvalidScreenshotPolicy),
            },
        };
        let playwright = match playwright_values.endpoint {
            None => {
                if playwright_values.browser.is_some()
                    || playwright_values.node.is_some()
                    || playwright_values.helper.is_some()
                {
                    return Err(DaemonStartupError::PlaywrightEndpointRequired);
                }
                None
            }
            Some(endpoint) => {
                let endpoint = endpoint
                    .into_string()
                    .map_err(|_| DaemonStartupError::InvalidPlaywrightEndpoint)?;
                let browser = match playwright_values.browser.as_deref().and_then(OsStr::to_str) {
                    None | Some("chromium") => BrowserKind::Chromium,
                    Some("firefox") => BrowserKind::Firefox,
                    Some("webkit") => BrowserKind::Webkit,
                    Some(_) => return Err(DaemonStartupError::InvalidPlaywrightBrowser),
                };
                let node_path = playwright_values
                    .node
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("node"));
                let helper_path = playwright_values
                    .helper
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_PLAYWRIGHT_HELPER));
                if node_path.as_os_str().is_empty() {
                    return Err(DaemonStartupError::InvalidPlaywrightNode);
                }
                if helper_path.as_os_str().is_empty() {
                    return Err(DaemonStartupError::InvalidPlaywrightHelper);
                }
                PlaywrightBridgeConfig::new(&node_path, &helper_path, endpoint.clone(), browser)
                    .map_err(|_| DaemonStartupError::InvalidPlaywrightEndpoint)?;
                Some(PlaywrightStartupConfig {
                    endpoint,
                    browser,
                    node_path,
                    helper_path,
                })
            }
        };
        Ok(Self {
            evidence_dir,
            android_mode,
            adb_path,
            harmony_mode,
            hdc_path,
            ios,
            desktop,
            screenshot_policy,
            playwright,
            rpc_listen: None,
            rdp: None,
            plugins: None,
            distributed_peers: None,
            distributed_server: None,
            remote_security: None,
        })
    }

    fn from_env() -> Result<Self, DaemonStartupError> {
        let mut config = Self::from_values(
            std::env::var_os("DEVICERAIL_EVIDENCE_DIR"),
            std::env::var_os("DEVICERAIL_ANDROID"),
            std::env::var_os("DEVICERAIL_ADB_PATH"),
            NativePlatformConfigValues {
                harmony_mode: std::env::var_os("DEVICERAIL_HARMONY"),
                hdc_path: std::env::var_os("DEVICERAIL_HDC_PATH"),
                ios: IosConfigValues {
                    mode: std::env::var_os("DEVICERAIL_IOS"),
                    backend: std::env::var_os("DEVICERAIL_IOS_BACKEND"),
                    session_target: std::env::var_os("DEVICERAIL_IOS_SESSION_TARGET"),
                    appium_endpoint: std::env::var_os("DEVICERAIL_IOS_APPIUM_ENDPOINT"),
                    appium_path: std::env::var_os("DEVICERAIL_IOS_APPIUM_PATH"),
                    appium_port: std::env::var_os("DEVICERAIL_IOS_APPIUM_PORT"),
                    appium_base_path: std::env::var_os("DEVICERAIL_IOS_APPIUM_BASE_PATH"),
                    appium_new_command_timeout_seconds: std::env::var_os(
                        "DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS",
                    ),
                    wda_endpoint: std::env::var_os("DEVICERAIL_IOS_WDA_ENDPOINT"),
                    device_token: std::env::var_os("DEVICERAIL_IOS_DEVICE_TOKEN"),
                    device_name: std::env::var_os("DEVICERAIL_IOS_DEVICE_NAME"),
                    os_version: std::env::var_os("DEVICERAIL_IOS_OS_VERSION"),
                    mjpeg_endpoint: std::env::var_os("DEVICERAIL_IOS_MJPEG_ENDPOINT"),
                    wda_project: std::env::var_os("DEVICERAIL_IOS_WDA_PROJECT"),
                    derived_data: std::env::var_os("DEVICERAIL_IOS_DERIVED_DATA"),
                    iproxy_path: std::env::var_os("DEVICERAIL_IOS_IPROXY_PATH"),
                    local_port: std::env::var_os("DEVICERAIL_IOS_WDA_LOCAL_PORT"),
                    remote_port: std::env::var_os("DEVICERAIL_IOS_WDA_REMOTE_PORT"),
                    allow_provisioning_updates: std::env::var_os(
                        "DEVICERAIL_IOS_ALLOW_PROVISIONING_UPDATES",
                    ),
                },
                desktop: DesktopConfigValues {
                    mode: std::env::var_os("DEVICERAIL_DESKTOP"),
                    id: std::env::var_os("DEVICERAIL_DESKTOP_ID"),
                    name: std::env::var_os("DEVICERAIL_DESKTOP_NAME"),
                    os_version: std::env::var_os("DEVICERAIL_DESKTOP_OS_VERSION"),
                    command_timeout_ms: std::env::var_os("DEVICERAIL_DESKTOP_COMMAND_TIMEOUT_MS"),
                    macos_screencapture: std::env::var_os("DEVICERAIL_DESKTOP_MACOS_SCREENCAPTURE"),
                    windows_powershell: std::env::var_os("DEVICERAIL_DESKTOP_WINDOWS_POWERSHELL"),
                    linux_display_server: std::env::var_os(
                        "DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER",
                    ),
                    x11_import: std::env::var_os("DEVICERAIL_DESKTOP_X11_IMPORT"),
                    x11_xdotool: std::env::var_os("DEVICERAIL_DESKTOP_X11_XDOTOOL"),
                    wayland_grim: std::env::var_os("DEVICERAIL_DESKTOP_WAYLAND_GRIM"),
                    wayland_input: std::env::var_os("DEVICERAIL_DESKTOP_WAYLAND_INPUT"),
                    wayland_ydotool: std::env::var_os("DEVICERAIL_DESKTOP_WAYLAND_YDOTOOL"),
                    wayland_wtype: std::env::var_os("DEVICERAIL_DESKTOP_WAYLAND_WTYPE"),
                    wayland_viewport_width: std::env::var_os(
                        "DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_WIDTH",
                    ),
                    wayland_viewport_height: std::env::var_os(
                        "DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_HEIGHT",
                    ),
                    wayland_viewport_scale_factor: std::env::var_os(
                        "DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_SCALE_FACTOR",
                    ),
                },
            },
            std::env::var_os("DEVICERAIL_SCREENSHOT_POLICY"),
            PlaywrightConfigValues {
                endpoint: std::env::var_os("DEVICERAIL_PLAYWRIGHT_ENDPOINT"),
                browser: std::env::var_os("DEVICERAIL_PLAYWRIGHT_BROWSER"),
                node: std::env::var_os("DEVICERAIL_PLAYWRIGHT_NODE"),
                helper: std::env::var_os("DEVICERAIL_PLAYWRIGHT_HELPER"),
            },
        )?;
        config.rpc_listen = parse_rpc_listen(std::env::var_os("DEVICERAIL_RPC_LISTEN"))?;
        config.rdp = parse_rdp_startup(
            std::env::var_os("DEVICERAIL_RDP_BRIDGE"),
            std::env::var_os("DEVICERAIL_RDP_TARGET"),
            std::env::var_os("DEVICERAIL_RDP_TOKEN"),
            std::env::var_os("DEVICERAIL_RDP_NAME"),
        )?;
        config.plugins = parse_plugin_startup(
            std::env::var_os("DEVICERAIL_PLUGIN_DIRS"),
            std::env::var_os("DEVICERAIL_PLUGIN_TIMEOUT_MS"),
        )?;
        config.distributed_peers =
            parse_distributed_startup(std::env::var_os("DEVICERAIL_DISTRIBUTED_PEERS"))?;
        config.distributed_server =
            parse_distributed_server_startup(std::env::var_os("DEVICERAIL_DISTRIBUTED_SERVER"))?;
        validate_distributed_topology(
            config.rpc_listen,
            config.distributed_peers.as_ref(),
            config.distributed_server.as_ref(),
        )?;
        config.remote_security = parse_remote_security_startup(
            std::env::var_os("DEVICERAIL_RPC_CREDENTIALS"),
            std::env::var_os("DEVICERAIL_RPC_AUDIT_LOG"),
            config.rpc_listen,
        )?;
        Ok(config)
    }
}

fn parse_distributed_startup(
    config_path: Option<OsString>,
) -> Result<Option<DistributedPeers>, DaemonStartupError> {
    let Some(config_path) = config_path else {
        return Ok(None);
    };
    let config_path = PathBuf::from(config_path);
    if config_path.as_os_str().is_empty() {
        return Err(DaemonStartupError::InvalidDistributedConfiguration);
    }
    DistributedPeers::load(config_path)
        .map(Some)
        .map_err(|_| DaemonStartupError::InvalidDistributedConfiguration)
}

fn parse_distributed_server_startup(
    config_path: Option<OsString>,
) -> Result<Option<DistributedPeerServer>, DaemonStartupError> {
    let Some(config_path) = config_path else {
        return Ok(None);
    };
    let config_path = PathBuf::from(config_path);
    if config_path.as_os_str().is_empty() {
        return Err(DaemonStartupError::InvalidDistributedServerConfiguration);
    }
    DistributedPeerServer::load(config_path)
        .map(Some)
        .map_err(|_| DaemonStartupError::InvalidDistributedServerConfiguration)
}

fn validate_distributed_topology(
    rpc_listen: Option<SocketAddr>,
    peers: Option<&DistributedPeers>,
    server: Option<&DistributedPeerServer>,
) -> Result<(), DaemonStartupError> {
    let Some(server) = server else {
        return Ok(());
    };
    if rpc_listen == Some(server.listen())
        || peers.is_some_and(|peers| {
            peers.peers().iter().any(|peer| {
                peer.endpoint() == server.listen() || peer.node_id() == server.node_id()
            })
        })
    {
        return Err(DaemonStartupError::DistributedServerTopologyConflict);
    }
    Ok(())
}

fn parse_remote_security_startup(
    credentials: Option<OsString>,
    audit_log: Option<OsString>,
    rpc_listen: Option<SocketAddr>,
) -> Result<Option<RemoteSecurityStartupConfig>, DaemonStartupError> {
    match (credentials, audit_log) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(DaemonStartupError::RemoteSecurityIncomplete),
        (Some(credentials), Some(audit_log)) => {
            if rpc_listen.is_none() {
                return Err(DaemonStartupError::RemoteSecurityListenerRequired);
            }
            let credential_store = PathBuf::from(credentials);
            let audit_log = PathBuf::from(audit_log);
            if credential_store.as_os_str().is_empty() || audit_log.as_os_str().is_empty() {
                return Err(DaemonStartupError::RemoteSecurityIncomplete);
            }
            Ok(Some(RemoteSecurityStartupConfig {
                credential_store,
                audit_log,
            }))
        }
    }
}

fn parse_plugin_startup(
    directories: Option<OsString>,
    timeout_ms: Option<OsString>,
) -> Result<Option<PluginStartupConfig>, DaemonStartupError> {
    let Some(directories) = directories else {
        if timeout_ms.is_some() {
            return Err(DaemonStartupError::PluginDirectoryRequired);
        }
        return Ok(None);
    };
    let directories = std::env::split_paths(&directories).collect::<Vec<_>>();
    let mut discovery = PluginDiscoveryConfig::new(directories)
        .map_err(|_| DaemonStartupError::InvalidPluginConfiguration)?;
    if let Some(timeout_ms) = timeout_ms {
        let timeout_ms = timeout_ms
            .to_str()
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(DaemonStartupError::InvalidPluginConfiguration)?;
        discovery = discovery
            .with_command_timeout(Duration::from_millis(timeout_ms))
            .map_err(|_| DaemonStartupError::InvalidPluginConfiguration)?;
    }
    Ok(Some(PluginStartupConfig { discovery }))
}

fn parse_rpc_listen(value: Option<OsString>) -> Result<Option<SocketAddr>, DaemonStartupError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| DaemonStartupError::InvalidRpcListen)?;
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| DaemonStartupError::InvalidRpcListen)?;
    if !address.ip().is_loopback() {
        return Err(DaemonStartupError::InvalidRpcListen);
    }
    Ok(Some(address))
}

fn parse_rdp_startup(
    bridge: Option<OsString>,
    target: Option<OsString>,
    token: Option<OsString>,
    name: Option<OsString>,
) -> Result<Option<RdpStartupConfig>, DaemonStartupError> {
    let Some(bridge) = bridge else {
        if target.is_some() || token.is_some() || name.is_some() {
            return Err(DaemonStartupError::RdpBridgeRequired);
        }
        return Ok(None);
    };
    let target = target.ok_or(DaemonStartupError::RdpTargetRequired)?;
    let token = token.ok_or(DaemonStartupError::RdpTokenRequired)?;
    let bridge = bridge
        .into_string()
        .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?;
    let target = target
        .into_string()
        .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?;
    let token = token
        .into_string()
        .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?;
    let name = name
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?
        .unwrap_or_else(|| "RDP desktop".to_owned());
    let target =
        RdpTarget::parse(target).map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?;
    let bridge = RdpBridgeConfig::new(bridge, target, token)
        .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?;
    RdpDriver::new(name.clone(), Arc::new(SystemRdpBridge::new(bridge.clone())))
        .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?;
    Ok(Some(RdpStartupConfig { name, bridge }))
}

#[derive(Debug, Error, PartialEq, Eq)]
enum DaemonStartupError {
    #[error("DEVICERAIL_ANDROID must be one of: auto, off, required")]
    InvalidAndroidMode,
    #[error("DEVICERAIL_EVIDENCE_DIR must not be empty")]
    InvalidEvidenceDirectory,
    #[error("DEVICERAIL_ADB_PATH must not be empty")]
    InvalidAdbPath,
    #[error("DEVICERAIL_HARMONY must be one of: auto, off, required")]
    InvalidHarmonyMode,
    #[error("DEVICERAIL_HDC_PATH requires DEVICERAIL_HARMONY=auto|required")]
    HarmonyModeRequiredForHdcPath,
    #[error("DEVICERAIL_HDC_PATH must not be empty")]
    InvalidHdcPath,
    #[error("DEVICERAIL_DESKTOP must be one of: auto, off, required")]
    InvalidDesktopMode,
    #[error("desktop settings require DEVICERAIL_DESKTOP=auto|required")]
    DesktopModeRequiredForSettings,
    #[error("desktop platform settings do not match this daemon host")]
    DesktopSettingUnsupportedOnHost,
    #[error("DeviceRail desktop startup configuration is invalid")]
    InvalidDesktopConfiguration,
    #[error("DEVICERAIL_IOS_WDA_ENDPOINT is required when another iOS setting is set")]
    IosWdaEndpointRequired,
    #[error("DEVICERAIL_IOS_DEVICE_TOKEN is required when iOS is enabled")]
    IosDeviceTokenRequired,
    #[error("DEVICERAIL_IOS must be one of: auto, off, required")]
    InvalidIosMode,
    #[error("DEVICERAIL_IOS_BACKEND must be one of: direct-wda, appium")]
    InvalidIosBackend,
    #[error("DEVICERAIL_IOS_SESSION_TARGET must be one of: native, safari")]
    InvalidIosSessionTarget,
    #[error("DEVICERAIL_IOS_SESSION_TARGET=safari requires DEVICERAIL_IOS_BACKEND=appium")]
    IosSessionTargetRequiresAppium,
    #[error(
        "DEVICERAIL_IOS_APPIUM_ENDPOINT or DEVICERAIL_IOS_APPIUM_PATH is required for the Appium iOS backend"
    )]
    IosAppiumEndpointRequired,
    #[error("Appium settings require DEVICERAIL_IOS_BACKEND=appium")]
    IosAppiumEndpointRequiresBackend,
    #[error(
        "DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS must be an integer from 1 through 3600"
    )]
    InvalidIosAppiumNewCommandTimeout,
    #[error("DEVICERAIL_IOS_APPIUM_ENDPOINT conflicts with managed Appium process settings")]
    IosAppiumServerConflict,
    #[error(
        "Appium iOS startup must use a bounded numeric-loopback HTTP endpoint without credentials"
    )]
    InvalidIosAppiumConfiguration,
    #[error("managed Appium startup failed ({code})")]
    IosManagedAppiumStartup { code: &'static str },
    #[error("managed Appium shutdown failed ({code})")]
    IosManagedAppiumShutdown { code: &'static str },
    #[error("managed Appium runtime failed ({code})")]
    IosManagedAppiumRuntime { code: &'static str },
    #[error("managed iOS settings require DEVICERAIL_IOS=auto|required")]
    IosManagedModeRequired,
    #[error("iOS settings are not allowed with DEVICERAIL_IOS=off")]
    IosSettingsWhileDisabled,
    #[error("managed iOS cannot be combined with an external endpoint or static metadata")]
    IosManagedExternalConflict,
    #[error("DEVICERAIL_IOS_WDA_PROJECT is required for managed iOS")]
    IosManagedProjectRequired,
    #[error("managed iOS startup configuration is invalid")]
    InvalidManagedIosConfiguration,
    #[error("required managed iOS startup failed ({code})")]
    IosManagedRequired { code: &'static str },
    #[error("managed iOS supervisor shutdown failed ({code})")]
    IosManagedShutdown { code: &'static str },
    #[error(
        "iOS startup configuration must use bounded numeric-loopback http endpoints and valid device metadata"
    )]
    InvalidIosConfiguration,
    #[error("DEVICERAIL_SCREENSHOT_POLICY must be one of: capture, omit")]
    InvalidScreenshotPolicy,
    #[error("DEVICERAIL_PLAYWRIGHT_ENDPOINT is required when another Playwright setting is set")]
    PlaywrightEndpointRequired,
    #[error(
        "DEVICERAIL_PLAYWRIGHT_ENDPOINT must be a bounded ws:// or wss:// URL without credentials or a fragment"
    )]
    InvalidPlaywrightEndpoint,
    #[error("DEVICERAIL_PLAYWRIGHT_BROWSER must be one of: chromium, firefox, webkit")]
    InvalidPlaywrightBrowser,
    #[error("DEVICERAIL_PLAYWRIGHT_NODE must not be empty")]
    InvalidPlaywrightNode,
    #[error("DEVICERAIL_PLAYWRIGHT_HELPER must not be empty")]
    InvalidPlaywrightHelper,
    #[error("DEVICERAIL_RPC_LISTEN must be a loopback IP socket address")]
    InvalidRpcListen,
    #[error("DEVICERAIL_RPC_CREDENTIALS and DEVICERAIL_RPC_AUDIT_LOG must be configured together")]
    RemoteSecurityIncomplete,
    #[error("remote RPC security requires DEVICERAIL_RPC_LISTEN")]
    RemoteSecurityListenerRequired,
    #[error("failed to initialize the remote credential store ({code})")]
    RemoteCredentialStore { code: &'static str },
    #[error("failed to initialize remote authentication ({code})")]
    RemoteAuthenticator { code: &'static str },
    #[error("failed to initialize the remote audit log ({code})")]
    RemoteAudit { code: &'static str },
    #[error("DEVICERAIL_RDP_BRIDGE is required when another RDP setting is set")]
    RdpBridgeRequired,
    #[error("DEVICERAIL_RDP_TARGET is required when RDP is enabled")]
    RdpTargetRequired,
    #[error("DEVICERAIL_RDP_TOKEN is required when RDP is enabled")]
    RdpTokenRequired,
    #[error("RDP startup configuration is invalid")]
    InvalidRdpConfiguration,
    #[error("DEVICERAIL_PLUGIN_DIRS is required when a plugin timeout is set")]
    PluginDirectoryRequired,
    #[error("DeviceRail plugin startup configuration is invalid")]
    InvalidPluginConfiguration,
    #[error("DeviceRail plugin discovery failed ({code})")]
    PluginDiscovery { code: String },
    #[error("DEVICERAIL_DISTRIBUTED_PEERS must name a complete owner-only loopback tunnel config")]
    InvalidDistributedConfiguration,
    #[error(
        "DEVICERAIL_DISTRIBUTED_SERVER must name a complete owner-only loopback peer-server config"
    )]
    InvalidDistributedServerConfiguration,
    #[error("distributed peer-server configuration conflicts with another daemon route")]
    DistributedServerTopologyConflict,
    #[error("distributed peer-server initialization failed ({code})")]
    DistributedServerStartup { code: &'static str },
    #[error("distributed peer discovery failed ({code})")]
    DistributedDiscovery { code: &'static str },
    #[error("failed to initialize the Evidence Store ({code})")]
    Evidence { code: &'static str },
    #[error("failed to reconcile durable Evidence ({code})")]
    EvidenceReconciliation { code: &'static str },
    #[error("required Android discovery failed ({code})")]
    AndroidRequired { code: &'static str },
    #[error("required Android discovery found no stable devices")]
    AndroidRequiredNoDevices,
    #[error("required HarmonyOS discovery failed ({code})")]
    HarmonyRequired { code: &'static str },
    #[error("required HarmonyOS discovery found no stable devices")]
    HarmonyRequiredNoDevices,
    #[error("required desktop discovery failed ({code})")]
    DesktopRequired { code: &'static str },
    #[error("failed to register a startup device ({code})")]
    DeviceRegistration { code: &'static str },
    #[error("Playwright discovery failed ({code})")]
    PlaywrightDiscovery { code: String },
    #[error("Playwright discovery found no pages")]
    PlaywrightNoPages,
}

#[derive(Clone)]
enum EvidenceCleanup {
    /// Explicitly used only by Registry::new test contexts that cannot write
    /// Evidence. Production always uses Managed.
    #[cfg(test)]
    Disabled,
    Managed(Arc<dyn EvidenceStore>),
}

impl EvidenceCleanup {
    fn managed_store(&self) -> Option<&Arc<dyn EvidenceStore>> {
        match self {
            #[cfg(test)]
            Self::Disabled => None,
            Self::Managed(store) => Some(store),
        }
    }
}

#[derive(Default)]
struct MediaStreamManager {
    state: StdMutex<MediaStreamManagerState>,
}

#[derive(Default)]
struct MediaStreamManagerState {
    streams: BTreeMap<MediaStreamId, Arc<ManagedMediaStream>>,
    pending: BTreeMap<MediaStreamId, PendingMediaStart>,
    sensitive_actions_in_flight: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct PendingMediaStart {
    session_id: SessionId,
    device_id: DeviceId,
    info: MediaStreamInfo,
}

struct ManagedMediaStream {
    session_id: SessionId,
    device_id: DeviceId,
    info: MediaStreamInfo,
    writer: Arc<MediaStreamWriter<MemoryEventStore>>,
    capture_gate: Arc<TokioMutex<()>>,
    state: StdMutex<ManagedMediaStreamState>,
}

#[derive(Default)]
struct ManagedMediaStreamState {
    frame_count: u64,
    last_capture_at: Option<Instant>,
    last_frame: Option<MediaFrame>,
    last_duration_ms: Option<u64>,
    ended_frame_count: Option<u64>,
    poisoned: bool,
}

impl std::fmt::Debug for MediaStreamManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("MediaStreamManager")
            .field("stream_count", &state.streams.len())
            .field("pending_count", &state.pending.len())
            .field(
                "sensitive_actions_in_flight",
                &state.sensitive_actions_in_flight,
            )
            .finish()
    }
}

enum MediaStartAdmission {
    Existing(Arc<ManagedMediaStream>),
    Reserved(MediaStartReservation),
}

struct MediaStartReservation {
    manager: Arc<MediaStreamManager>,
    stream_id: MediaStreamId,
    active: bool,
}

impl MediaStartReservation {
    fn commit(
        mut self,
        session_id: SessionId,
        device_id: DeviceId,
        info: MediaStreamInfo,
        writer: Arc<MediaStreamWriter<MemoryEventStore>>,
    ) -> Arc<ManagedMediaStream> {
        let record = Arc::new(ManagedMediaStream {
            session_id,
            device_id,
            info,
            writer,
            capture_gate: Arc::new(TokioMutex::new(())),
            state: StdMutex::new(ManagedMediaStreamState::default()),
        });
        let mut state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending.remove(&self.stream_id);
        state
            .streams
            .insert(self.stream_id.clone(), Arc::clone(&record));
        self.active = false;
        record
    }
}

impl Drop for MediaStartReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.manager
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&self.stream_id);
    }
}

struct SensitiveActionAdmission {
    manager: Arc<MediaStreamManager>,
}

impl Drop for SensitiveActionAdmission {
    fn drop(&mut self) {
        let mut state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sensitive_actions_in_flight = state.sensitive_actions_in_flight.saturating_sub(1);
    }
}

impl MediaStreamManager {
    fn active_count(state: &MediaStreamManagerState) -> usize {
        state.pending.len()
            + state
                .streams
                .values()
                .filter(|stream| {
                    stream
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .ended_frame_count
                        .is_none()
                })
                .count()
    }

    fn begin_start(
        self: &Arc<Self>,
        session_id: &SessionId,
        device_id: &DeviceId,
        info: &MediaStreamInfo,
    ) -> Result<MediaStartAdmission, RpcError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = state.streams.get(&info.id) {
            return if existing.session_id == *session_id
                && existing.device_id == *device_id
                && existing.info == *info
            {
                Ok(MediaStartAdmission::Existing(Arc::clone(existing)))
            } else {
                Err(media_rpc_error(
                    "media_stream_conflict",
                    "media stream id is already bound to different metadata",
                    false,
                    Some(json!({ "streamId": info.id })),
                ))
            };
        }
        if let Some(pending) = state.pending.get(&info.id) {
            let exact = pending.session_id == *session_id
                && pending.device_id == *device_id
                && pending.info == *info;
            return Err(media_rpc_error(
                if exact {
                    "media_stream_busy"
                } else {
                    "media_stream_conflict"
                },
                "media stream start is already in progress",
                exact,
                Some(json!({ "streamId": info.id })),
            ));
        }
        if state.sensitive_actions_in_flight != 0 {
            return Err(media_rpc_error(
                "media_stream_sensitive_action_in_flight",
                "cannot start media capture while a protected or unknown action is in flight",
                true,
                None,
            ));
        }
        if Self::active_count(&state) >= MAX_ACTIVE_MEDIA_STREAMS {
            return Err(media_rpc_error(
                "media_stream_active_limit",
                "too many media streams are active",
                true,
                Some(json!({ "limit": MAX_ACTIVE_MEDIA_STREAMS })),
            ));
        }
        if state.streams.len() + state.pending.len() >= MAX_MEDIA_STREAMS_PER_SESSION {
            return Err(media_rpc_error(
                "media_stream_session_limit",
                "the Session media stream limit was reached",
                false,
                Some(json!({ "limit": MAX_MEDIA_STREAMS_PER_SESSION })),
            ));
        }
        state.pending.insert(
            info.id.clone(),
            PendingMediaStart {
                session_id: session_id.clone(),
                device_id: device_id.clone(),
                info: info.clone(),
            },
        );
        Ok(MediaStartAdmission::Reserved(MediaStartReservation {
            manager: Arc::clone(self),
            stream_id: info.id.clone(),
            active: true,
        }))
    }

    fn sensitive_action(self: &Arc<Self>) -> Result<SensitiveActionAdmission, RpcError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if Self::active_count(&state) != 0 {
            return Err(media_rpc_error(
                "media_stream_protected_action_blocked",
                "protected or unknown actions are disabled while media capture is active",
                true,
                None,
            ));
        }
        state.sensitive_actions_in_flight += 1;
        Ok(SensitiveActionAdmission {
            manager: Arc::clone(self),
        })
    }

    fn stream(&self, stream_id: &MediaStreamId) -> Result<Arc<ManagedMediaStream>, RpcError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .streams
            .get(stream_id)
            .cloned()
            .ok_or_else(|| {
                media_rpc_error(
                    "media_stream_not_found",
                    "media stream is not owned by this connection",
                    false,
                    Some(json!({ "streamId": stream_id })),
                )
            })
    }

    fn streams_for_session(&self, session_id: &SessionId) -> Vec<Arc<ManagedMediaStream>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .streams
            .values()
            .filter(|stream| &stream.session_id == session_id)
            .cloned()
            .collect()
    }

    fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.streams.clear();
        state.pending.clear();
    }
}

#[derive(Clone, Copy)]
struct DispatchResources<'a> {
    events: &'a MemoryEventStore,
    evidence: &'a EvidenceCleanup,
    streams: Option<&'a EventStreamServer>,
}

#[async_trait::async_trait]
trait AndroidStartupBackend: Send + Sync {
    async fn discover(&self) -> Result<AdbDiscoveryReport, &'static str>;

    async fn build_route(
        &self,
        descriptor: DiscoveredAndroidDevice,
    ) -> Result<(Arc<dyn DeviceDriver>, DeviceInfo), &'static str>;
}

struct SystemAndroidBackend {
    discovery_adb: AndroidAdb,
    runtime_adb: AndroidAdb,
}

#[async_trait::async_trait]
impl AndroidStartupBackend for SystemAndroidBackend {
    async fn discover(&self) -> Result<AdbDiscoveryReport, &'static str> {
        self.discovery_adb
            .discover(&ExecutionControl::unbounded())
            .await
            .map_err(|error| error.code())
    }

    async fn build_route(
        &self,
        descriptor: DiscoveredAndroidDevice,
    ) -> Result<(Arc<dyn DeviceDriver>, DeviceInfo), &'static str> {
        let driver = Arc::new(
            self.runtime_adb
                .driver(descriptor, AndroidDeviceConfig::default())
                .map_err(|error| error.code())?,
        );
        let info = driver.device_info().await;
        Ok((driver, info))
    }
}

#[async_trait::async_trait]
trait HarmonyStartupBackend: Send + Sync {
    async fn discover(&self) -> Result<HarmonyDiscoveryReport, &'static str>;

    async fn build_route(
        &self,
        descriptor: DiscoveredHarmonyDevice,
    ) -> (Arc<dyn DeviceDriver>, DeviceInfo);
}

struct SystemHarmonyBackend {
    discovery_hdc: HarmonyHdc,
    runtime_hdc: HarmonyHdc,
}

#[async_trait::async_trait]
impl HarmonyStartupBackend for SystemHarmonyBackend {
    async fn discover(&self) -> Result<HarmonyDiscoveryReport, &'static str> {
        self.discovery_hdc
            .discover(&ExecutionControl::unbounded())
            .await
            .map_err(|error| error.code())
    }

    async fn build_route(
        &self,
        descriptor: DiscoveredHarmonyDevice,
    ) -> (Arc<dyn DeviceDriver>, DeviceInfo) {
        let driver = Arc::new(self.runtime_hdc.driver(descriptor));
        let info = driver.device_info().await;
        (driver, info)
    }
}

#[async_trait::async_trait]
trait DesktopStartupBackend: Send + Sync {
    async fn discover(
        &self,
        config: &DesktopStartupConfig,
        control: &ExecutionControl,
    ) -> Result<(Arc<dyn DeviceDriver>, DeviceInfo), &'static str>;
}

struct SystemDesktopStartupBackend;

#[async_trait::async_trait]
impl DesktopStartupBackend for SystemDesktopStartupBackend {
    async fn discover(
        &self,
        config: &DesktopStartupConfig,
        control: &ExecutionControl,
    ) -> Result<(Arc<dyn DeviceDriver>, DeviceInfo), &'static str> {
        let native =
            discover_native_driver(config.identity.clone(), config.system.clone(), control)
                .await
                .map_err(|error| error.code())?;
        let info = native.device_info().await;
        Ok((native.into_driver(), info))
    }
}

#[derive(Clone, Debug)]
struct NegotiatedContext {
    client: PeerInfo,
    hello: HelloResult,
    active_session: Option<SessionId>,
    selected_device_id: Option<DeviceId>,
    device_lease: Option<DeviceLease>,
    media_streams: Arc<MediaStreamManager>,
}

#[derive(Clone, Debug)]
enum ConnectionState {
    AwaitingHello { transport_kind: &'static str },
    Ready(Box<NegotiatedContext>),
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self::AwaitingHello {
            transport_kind: "stdio",
        }
    }
}

impl ConnectionState {
    fn loopback_tcp() -> Self {
        Self::AwaitingHello {
            transport_kind: "tcp",
        }
    }

    fn transport_kind(&self) -> &str {
        match self {
            Self::AwaitingHello { transport_kind } => transport_kind,
            Self::Ready(context) => &context.hello.transport.kind,
        }
    }

    fn context(&self) -> Option<&NegotiatedContext> {
        match self {
            Self::AwaitingHello { .. } => None,
            Self::Ready(context) => Some(context),
        }
    }

    fn context_mut(&mut self) -> Option<&mut NegotiatedContext> {
        match self {
            Self::AwaitingHello { .. } => None,
            Self::Ready(context) => Some(context),
        }
    }
}

#[derive(Debug, Default)]
struct RequestRegistry {
    controllers: StdMutex<BTreeMap<RpcId, RegisteredRequest>>,
}

#[derive(Debug)]
enum RegisteredRequest {
    Running(ExecutionController),
    Completed,
}

impl RequestRegistry {
    fn register(&self, request_id: RpcId, controller: ExecutionController) -> bool {
        let mut controllers = self
            .controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if controllers.contains_key(&request_id) {
            return false;
        }
        controllers.insert(request_id, RegisteredRequest::Running(controller));
        true
    }

    fn contains(&self, request_id: &RpcId) -> bool {
        self.controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(request_id)
    }

    fn len(&self) -> usize {
        self.controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn cancel(&self, request_id: &RpcId, reason: CancellationReason) -> RequestCancelStatus {
        let controllers = self
            .controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match controllers.get(request_id) {
            Some(RegisteredRequest::Running(controller)) if controller.cancel(reason) => {
                RequestCancelStatus::Requested
            }
            Some(RegisteredRequest::Running(_)) => RequestCancelStatus::AlreadyRequested,
            Some(RegisteredRequest::Completed) | None => RequestCancelStatus::NotFound,
        }
    }

    fn cancel_all(&self, reason: CancellationReason) {
        let controllers = self
            .controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for request in controllers.values() {
            if let RegisteredRequest::Running(controller) = request {
                controller.cancel(reason);
            }
        }
    }

    fn mark_completed(&self, request_id: &RpcId) {
        if let Some(request) = self
            .controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(request_id)
        {
            *request = RegisteredRequest::Completed;
        }
    }

    fn remove(&self, request_id: &RpcId) {
        self.controllers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(request_id);
    }
}

struct RequestRegistration {
    registry: Arc<RequestRegistry>,
    request_id: RpcId,
}

impl RequestRegistration {
    fn new(registry: Arc<RequestRegistry>, request_id: RpcId) -> Self {
        Self {
            registry,
            request_id,
        }
    }

    fn mark_completed(&self) {
        self.registry.mark_completed(&self.request_id);
    }
}

impl Drop for RequestRegistration {
    fn drop(&mut self) {
        self.registry.remove(&self.request_id);
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
enum DistributedPeerListenerError {
    #[error("distributed peer listener accepted an invalid socket")]
    SocketInvariant,
    #[error("distributed peer listener accept failed")]
    Accept,
    #[error("distributed peer connection task failed")]
    ConnectionTask,
}

impl DistributedPeerListenerError {
    fn code(self) -> &'static str {
        match self {
            Self::SocketInvariant => "distributed_server_socket_invariant",
            Self::Accept => "distributed_server_accept_failed",
            Self::ConnectionTask => "distributed_server_connection_task_failed",
        }
    }
}

struct DistributedPeerServerRuntime {
    controller: ExecutionController,
    service: Arc<RegistryPeerService<MemoryEventStore>>,
    completion: Option<tokio::task::JoinHandle<Result<(), DistributedPeerListenerError>>>,
    failure: watch::Receiver<Option<&'static str>>,
}

impl std::fmt::Debug for DistributedPeerServerRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DistributedPeerServerRuntime")
            .field("ready", &self.service.is_ready())
            .field("running", &self.completion.is_some())
            .finish()
    }
}

impl DistributedPeerServerRuntime {
    fn mark_ready(&self) {
        self.service.mark_ready();
    }

    fn begin_shutdown(&self) {
        self.controller.cancel(CancellationReason::Shutdown);
    }

    fn failure_code(&self) -> Option<&'static str> {
        (*self.failure.borrow()).or_else(|| {
            self.completion
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
                .then_some("distributed_server_task_stopped")
        })
    }

    async fn wait_for_failure(&mut self) -> &'static str {
        loop {
            if let Some(code) = *self.failure.borrow_and_update() {
                return code;
            }
            if self.failure.changed().await.is_err() {
                return "distributed_server_task_stopped";
            }
        }
    }

    async fn shutdown(mut self) -> std::io::Result<()> {
        self.controller.cancel(CancellationReason::Shutdown);
        let listener_result = match self.completion.take() {
            Some(completion) => match completion.await {
                Ok(result) => result.map_err(|error| {
                    std::io::Error::other(format!(
                        "distributed peer server failed ({})",
                        error.code()
                    ))
                }),
                Err(_) => Err(std::io::Error::other(
                    "distributed peer server failed (distributed_server_task_failed)",
                )),
            },
            None => Ok(()),
        };
        let cleanup_errors = self.service.shutdown().await;
        let cleanup_result = if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "distributed peer server failed (distributed_server_cleanup_failed)",
            ))
        };
        listener_result.and(cleanup_result)
    }
}

async fn start_distributed_peer_server(
    config: &DaemonConfig,
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: Arc<dyn EvidenceStore>,
) -> Result<Option<DistributedPeerServerRuntime>, DaemonStartupError> {
    let Some(server) = &config.distributed_server else {
        return Ok(None);
    };
    if !server.listen().ip().is_loopback() || server.listen().port() == 0 {
        return Err(DaemonStartupError::DistributedServerStartup {
            code: "distributed_server_listen_invalid",
        });
    }
    let service = RegistryPeerService::new(
        server.node_id().clone(),
        server.node_epoch(),
        server.inventory_revision(),
        runtime,
        events,
        evidence,
    )
    .await
    .map_err(|_| DaemonStartupError::DistributedServerStartup {
        code: "distributed_server_service_invalid",
    })?;
    service.mark_starting();
    let security = PeerSecurity::external_tunnel(server.tunnel_id()).map_err(|_| {
        DaemonStartupError::DistributedServerStartup {
            code: "distributed_server_security_invalid",
        }
    })?;
    let listener = TcpListener::bind(server.listen()).await.map_err(|_| {
        DaemonStartupError::DistributedServerStartup {
            code: "distributed_server_bind_failed",
        }
    })?;
    let local_address =
        listener
            .local_addr()
            .map_err(|_| DaemonStartupError::DistributedServerStartup {
                code: "distributed_server_local_address_failed",
            })?;
    if local_address != server.listen()
        || !local_address.ip().is_loopback()
        || local_address.port() == 0
    {
        return Err(DaemonStartupError::DistributedServerStartup {
            code: "distributed_server_socket_invariant",
        });
    }

    let (controller, control) = ExecutionController::new();
    let (failure_sender, failure) = watch::channel(None);
    let task_controller = controller.clone();
    let task_service = Arc::clone(&service);
    let completion = tokio::spawn(async move {
        serve_distributed_peer_listener(
            listener,
            security,
            task_service,
            task_controller,
            control,
            failure_sender,
        )
        .await
    });
    eprintln!("DeviceRail peer server listening on {local_address}");
    Ok(Some(DistributedPeerServerRuntime {
        controller,
        service,
        completion: Some(completion),
        failure,
    }))
}

async fn serve_distributed_peer_listener(
    listener: TcpListener,
    security: PeerSecurity,
    service: Arc<RegistryPeerService<MemoryEventStore>>,
    controller: ExecutionController,
    control: ExecutionControl,
    failure: watch::Sender<Option<&'static str>>,
) -> Result<(), DistributedPeerListenerError> {
    let listener_address = listener
        .local_addr()
        .map_err(|_| DistributedPeerListenerError::SocketInvariant)?;
    if !listener_address.ip().is_loopback() || listener_address.port() == 0 {
        return Err(DistributedPeerListenerError::SocketInvariant);
    }
    let mut connections = JoinSet::new();
    let mut listener_error = None;
    loop {
        tokio::select! {
            biased;
            _ = control.cancelled() => break,
            accepted = listener.accept(), if connections.len() < MAX_PEER_SERVER_CONNECTIONS => {
                match accepted {
                    Ok((socket, peer_address)) => {
                        let local_address = match socket.local_addr() {
                            Ok(address) => address,
                            Err(_) => {
                                listener_error = Some(DistributedPeerListenerError::SocketInvariant);
                                break;
                            }
                        };
                        if !peer_address.ip().is_loopback()
                            || !local_address.ip().is_loopback()
                            || local_address != listener_address
                        {
                            listener_error = Some(DistributedPeerListenerError::SocketInvariant);
                            break;
                        }
                        if socket.set_nodelay(true).is_err() {
                            eprintln!(
                                "DeviceRail rejected one peer connection (distributed_server_nodelay_failed)"
                            );
                            continue;
                        }
                        let service = Arc::clone(&service);
                        let security = security.clone();
                        let control = control.clone();
                        connections.spawn(async move {
                            serve_peer_stream_until_cancelled(socket, security, service, control).await
                        });
                    }
                    Err(_) => {
                        listener_error = Some(DistributedPeerListenerError::Accept);
                        break;
                    }
                }
            }
            joined = connections.join_next(), if !connections.is_empty() => {
                match joined {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(PeerServerError::Task))) => {
                        listener_error = Some(DistributedPeerListenerError::ConnectionTask);
                        break;
                    }
                    Some(Ok(Err(error))) => eprintln!(
                        "DeviceRail peer connection closed ({})",
                        distributed_peer_stream_error_code(error)
                    ),
                    Some(Err(_)) => {
                        listener_error = Some(DistributedPeerListenerError::ConnectionTask);
                        break;
                    }
                    None => {}
                }
            }
        }
    }

    drop(listener);
    if let Some(error) = listener_error {
        let _ = failure.send(Some(error.code()));
        controller.cancel(CancellationReason::Shutdown);
    }
    while let Some(joined) = connections.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!(
                "DeviceRail peer connection closed ({})",
                distributed_peer_stream_error_code(error)
            ),
            Err(_) if listener_error.is_none() => {
                let error = DistributedPeerListenerError::ConnectionTask;
                let _ = failure.send(Some(error.code()));
                listener_error = Some(error);
                controller.cancel(CancellationReason::Shutdown);
            }
            Err(_) => {}
        }
    }
    listener_error.map_or(Ok(()), Err)
}

fn distributed_peer_stream_error_code(error: PeerServerError) -> &'static str {
    match error {
        PeerServerError::Security => "peer_security_invalid",
        PeerServerError::Protocol => "peer_protocol_invalid",
        PeerServerError::TimedOut => "peer_frame_timed_out",
        PeerServerError::Io => "peer_io_failed",
        PeerServerError::Task => "peer_request_task_failed",
        PeerServerError::Cleanup => "peer_cleanup_failed",
        PeerServerError::UnsupportedVersion => "peer_protocol_version_unsupported",
    }
}

async fn distributed_peer_server_failure(
    server: &mut Option<DistributedPeerServerRuntime>,
) -> &'static str {
    match server.as_mut() {
        Some(server) => server.wait_for_failure().await,
        None => std::future::pending().await,
    }
}

async fn managed_appium_failure(runtime: &mut Option<ManagedAppiumRuntime>) -> &'static str {
    match runtime.as_mut() {
        Some(runtime) => runtime.wait_for_failure().await,
        None => std::future::pending().await,
    }
}

async fn shutdown_distributed_peer_server(
    server: &mut Option<DistributedPeerServerRuntime>,
) -> std::io::Result<()> {
    match server.take() {
        Some(server) => server.shutdown().await,
        None => Ok(()),
    }
}

fn begin_distributed_peer_server_shutdown(server: &Option<DistributedPeerServerRuntime>) {
    if let Some(server) = server {
        server.begin_shutdown();
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => {}
        [argument] if argument == "--version" => {
            writeln!(
                std::io::stdout().lock(),
                "devicerail-daemon {}",
                env!("CARGO_PKG_VERSION")
            )?;
            return Ok(());
        }
        [ios, doctor, rest @ ..] if ios == "ios" && doctor == "doctor" => {
            return run_ios_doctor(rest).await;
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "usage: devicerail-daemon [--version | ios doctor [--json] [--device UDID] [--wda-project PATH] [--iproxy PATH]]",
            )
            .into());
        }
    }
    let mut config = DaemonConfig::from_env()?;
    let remote_security = initialize_remote_security(&config)?;
    let events = Arc::new(MemoryEventStore::default());
    let evidence = Arc::new(
        FileEvidenceStore::new(&config.evidence_dir, FileEvidenceStoreConfig::default()).map_err(
            |error| DaemonStartupError::Evidence {
                code: evidence_code(&error),
            },
        )?,
    );
    reconcile_missing_session_evidence(events.as_ref(), evidence.as_ref(), now_ms())
        .await
        .map_err(|error| DaemonStartupError::EvidenceReconciliation {
            code: cleanup_error_code(&error),
        })?;
    let evidence_store: Arc<dyn EvidenceStore> = evidence.clone();
    let runtime = Arc::new(
        Registry::with_evidence(Arc::clone(&events), Arc::clone(&evidence_store))
            .with_screenshot_policy(config.screenshot_policy),
    );

    let driver = Arc::new(MockDriver::new("mock-1").with_session_evidence());
    let info = driver.device_info();
    runtime
        .register(driver, info)
        .await
        .map_err(|_| DaemonStartupError::DeviceRegistration {
            code: "mock_registration_failed",
        })?;
    let mut appium_runtime = start_configured_appium(&mut config).await?;
    register_android_devices(runtime.as_ref(), &config).await?;
    let mut ios_runtime = register_ios_device(Arc::clone(&runtime), &config).await?;
    register_harmony_devices(runtime.as_ref(), &config).await?;
    register_desktop_device(runtime.as_ref(), &config).await?;
    register_playwright_devices(runtime.as_ref(), &config).await?;
    register_rdp_device(runtime.as_ref(), &config).await?;
    register_plugin_devices(runtime.as_ref(), &config).await?;
    let mut distributed_server = start_distributed_peer_server(
        &config,
        Arc::clone(&runtime),
        Arc::clone(&events),
        Arc::clone(&evidence_store),
    )
    .await?;
    if let Err(error) = register_distributed_devices(runtime.as_ref(), &config).await {
        if shutdown_distributed_peer_server(&mut distributed_server)
            .await
            .is_err()
        {
            eprintln!(
                "DeviceRail peer server cleanup failed after startup rejection (distributed_server_cleanup_failed)"
            );
        }
        return Err(error.into());
    }

    let streams = match EventStreamServer::bind(Arc::clone(&events), Default::default()).await {
        Ok(streams) => Some(streams),
        Err(StreamTransportError::Bind(error)) => {
            eprintln!(
                "DeviceRail event streaming is unavailable for this run ({})",
                bounded_diagnostic(&error.to_string())
            );
            None
        }
        Err(error) => {
            if shutdown_distributed_peer_server(&mut distributed_server)
                .await
                .is_err()
            {
                eprintln!(
                    "DeviceRail peer server cleanup failed after startup rejection (distributed_server_cleanup_failed)"
                );
            }
            return Err(error.into());
        }
    };
    if let Some(code) = distributed_server
        .as_ref()
        .and_then(DistributedPeerServerRuntime::failure_code)
    {
        let error = DaemonStartupError::DistributedServerStartup { code };
        let _ = shutdown_distributed_peer_server(&mut distributed_server).await;
        return Err(error.into());
    }
    let evidence = EvidenceCleanup::Managed(evidence_store);
    let serve_result = match config.rpc_listen {
        Some(address) => {
            serve_loopback_rpc(
                runtime,
                events,
                evidence,
                streams,
                address,
                LoopbackListenerServices {
                    remote_security,
                    distributed_server,
                    appium_runtime: &mut appium_runtime,
                },
            )
            .await
        }
        None => {
            serve_stdio(
                runtime,
                events,
                evidence,
                streams,
                distributed_server,
                &mut appium_runtime,
            )
            .await
        }
    };
    let ios_shutdown_error = if let Some(managed) = ios_runtime.take() {
        managed.shutdown().await.err()
    } else {
        None
    };
    let appium_runtime_failure = appium_runtime
        .as_ref()
        .and_then(ManagedAppiumRuntime::failure_code);
    let appium_shutdown_error = if let Some(managed) = appium_runtime.take() {
        managed.shutdown().await.err().map(|error| error.code())
    } else {
        None
    };
    if serve_result.is_ok() {
        if let Some(code) = ios_shutdown_error {
            return Err(DaemonStartupError::IosManagedShutdown { code }.into());
        }
        if let Some(code) = appium_shutdown_error {
            return Err(DaemonStartupError::IosManagedAppiumShutdown { code }.into());
        }
    } else {
        if let Some(code) = ios_shutdown_error {
            eprintln!("DeviceRail managed iOS cleanup failed after server shutdown ({code})");
        }
        if let Some(code) =
            appium_shutdown_error.filter(|code| Some(*code) != appium_runtime_failure)
        {
            eprintln!("DeviceRail managed Appium cleanup failed after server shutdown ({code})");
        }
    }
    serve_result
}

async fn run_ios_doctor(arguments: &[OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let mut json_output = false;
    let mut device_udid = std::env::var("DEVICERAIL_IOS_DEVICE_TOKEN").ok();
    let mut wda_project = std::env::var_os("DEVICERAIL_IOS_WDA_PROJECT").map(PathBuf::from);
    let mut iproxy_path = std::env::var_os("DEVICERAIL_IOS_IPROXY_PATH").map(PathBuf::from);
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].to_str() {
            Some("--json") => {
                json_output = true;
                index += 1;
            }
            Some("--device" | "--wda-project" | "--iproxy") => {
                let option = arguments[index]
                    .to_str()
                    .expect("matched UTF-8 doctor option");
                let value = arguments.get(index + 1).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{option} requires a value"),
                    )
                })?;
                match option {
                    "--device" => {
                        device_udid = Some(value.clone().into_string().map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "--device requires UTF-8",
                            )
                        })?);
                    }
                    "--wda-project" => wda_project = Some(PathBuf::from(value)),
                    "--iproxy" => iproxy_path = Some(PathBuf::from(value)),
                    _ => unreachable!("closed doctor option"),
                }
                index += 2;
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid ios doctor option",
                )
                .into());
            }
        }
    }
    let backend = std::env::var_os("DEVICERAIL_IOS_BACKEND");
    let wda_endpoint = std::env::var_os("DEVICERAIL_IOS_WDA_ENDPOINT");
    let appium_skips_wda_host_checks =
        ios_doctor_skips_wda_host_checks(backend.as_deref(), wda_project.is_some());
    let wda_endpoint = wda_endpoint
        .map(OsString::into_string)
        .transpose()
        .map_err(|_| DaemonStartupError::InvalidIosConfiguration)?;
    let mut report = SystemIosHost::default()
        .doctor(&DoctorOptions {
            device_udid,
            wda_project,
            iproxy_path,
            wda_endpoint,
            skip_iproxy_check: appium_skips_wda_host_checks,
            skip_wda_build_checks: appium_skips_wda_host_checks,
        })
        .await;
    if let Some(check) = appium_doctor_check(
        backend,
        std::env::var_os("DEVICERAIL_IOS_APPIUM_ENDPOINT"),
        std::env::var_os("DEVICERAIL_IOS_APPIUM_PATH"),
        std::env::var_os("DEVICERAIL_IOS_APPIUM_PORT"),
        std::env::var_os("DEVICERAIL_IOS_APPIUM_BASE_PATH"),
        std::env::var_os("DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS"),
    )
    .await?
    {
        report.checks.push(check);
        report.ready = !report.failed();
    }
    if json_output {
        writeln!(
            std::io::stdout().lock(),
            "{}",
            serde_json::to_string_pretty(&report)?
        )?;
    } else {
        let mut stdout = std::io::stdout().lock();
        for check in &report.checks {
            let label = match check.status {
                DiagnosticStatus::Pass => "PASS",
                DiagnosticStatus::Warn => "WARN",
                DiagnosticStatus::Fail => "FAIL",
            };
            writeln!(stdout, "{label:4} {:42} {}", check.code, check.summary)?;
            if let Some(remediation) = &check.remediation {
                writeln!(stdout, "     -> {remediation}")?;
            }
        }
    }
    if report.failed() {
        return Err(std::io::Error::other("iOS doctor found blocking checks").into());
    }
    Ok(())
}

fn ios_doctor_skips_wda_host_checks(backend: Option<&OsStr>, wda_project_configured: bool) -> bool {
    matches!(backend.and_then(OsStr::to_str), Some("appium")) && !wda_project_configured
}

async fn appium_doctor_check(
    backend: Option<OsString>,
    appium_endpoint: Option<OsString>,
    appium_path: Option<OsString>,
    appium_port: Option<OsString>,
    appium_base_path: Option<OsString>,
    appium_new_command_timeout_seconds: Option<OsString>,
) -> Result<Option<DiagnosticCheck>, DaemonStartupError> {
    let backend = IosBackendKind::parse(backend.as_deref())?;
    let appium_new_command_timeout_explicit = appium_new_command_timeout_seconds.is_some();
    let appium_new_command_timeout_seconds = parse_ios_appium_new_command_timeout_seconds(
        appium_new_command_timeout_seconds.as_deref(),
    )?;
    let backend = parse_ios_driver_backend(
        backend,
        appium_endpoint,
        appium_path,
        appium_port,
        appium_base_path,
        appium_new_command_timeout_seconds,
        appium_new_command_timeout_explicit,
    )?;
    let IosDriverBackendConfig::Appium { server, .. } = backend else {
        return Ok(None);
    };
    if let AppiumServerConfig::Managed(config) = server {
        return Ok(Some(match SystemAppiumHost.start(config).await {
            Ok(runtime) => match runtime.shutdown().await {
                Ok(()) => DiagnosticCheck {
                    status: DiagnosticStatus::Pass,
                    code: "ios_appium_managed_ready".to_owned(),
                    summary: "managed Appium server is ready; XCUITest availability is unverified"
                        .to_owned(),
                    remediation: None,
                },
                Err(_) => DiagnosticCheck {
                    status: DiagnosticStatus::Fail,
                    code: "ios_appium_managed_shutdown_failed".to_owned(),
                    summary: "managed Appium became ready but did not stop cleanly".to_owned(),
                    remediation: Some(
                        "verify the selected Appium executable responds to process termination"
                            .to_owned(),
                    ),
                },
            },
            Err(error) => DiagnosticCheck {
                status: DiagnosticStatus::Fail,
                code: error.code().to_owned(),
                summary: "managed Appium executable did not become ready".to_owned(),
                remediation: Some(
                    "verify DEVICERAIL_IOS_APPIUM_PATH and the Appium server configuration"
                        .to_owned(),
                ),
            },
        }));
    }
    let AppiumServerConfig::External(endpoint) = server else {
        unreachable!("managed Appium returned above")
    };
    let control =
        ExecutionController::with_timeout(IOS_APPIUM_DOCTOR_TIMEOUT_MS, TimeoutScope::Request).1;
    let transport = SystemAppiumTransport::new(endpoint);
    let (status, code, summary, remediation) = match transport.status(&control).await {
        Ok(status) if status.ready => (
            DiagnosticStatus::Pass,
            "ios_appium_ready",
            "Appium server is ready; XCUITest availability is unverified",
            None,
        ),
        Ok(_) => (
            DiagnosticStatus::Fail,
            "ios_appium_not_ready",
            "Appium server is reachable but not ready",
            Some("start Appium and wait for its status endpoint to become ready"),
        ),
        Err(_) => (
            DiagnosticStatus::Fail,
            "ios_appium_unavailable",
            "Appium server is unavailable",
            Some("start Appium, then verify the numeric-loopback endpoint"),
        ),
    };
    Ok(Some(DiagnosticCheck {
        status,
        code: code.to_owned(),
        summary: summary.to_owned(),
        remediation: remediation.map(str::to_owned),
    }))
}

async fn register_distributed_devices(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    let Some(peers) = &config.distributed_peers else {
        return Ok(());
    };
    let control =
        ExecutionController::with_timeout(DISTRIBUTED_STARTUP_TIMEOUT_MS, TimeoutScope::Request).1;
    let drivers =
        connect_configured_peers(peers, DistributedRouterConfig::default(), None, &control)
            .await
            .map_err(|error| DaemonStartupError::DistributedDiscovery {
                code: distributed_startup_error_code(error),
            })?;
    for driver in drivers {
        let driver = Arc::new(driver);
        let info = driver.device_info().await;
        runtime
            .register(driver as Arc<dyn DeviceDriver>, info)
            .await
            .map_err(|_| DaemonStartupError::DeviceRegistration {
                code: "distributed_registration_failed",
            })?;
    }
    Ok(())
}

fn distributed_startup_error_code(error: DistributedConnectorError) -> &'static str {
    match error {
        DistributedConnectorError::Connect => "distributed_tunnel_connect_failed",
        DistributedConnectorError::Security => "distributed_security_invalid",
        DistributedConnectorError::Discovery => "distributed_discovery_failed",
        DistributedConnectorError::Cancelled => "distributed_startup_cancelled",
        DistributedConnectorError::TimedOut => "distributed_startup_timed_out",
    }
}

fn initialize_remote_security(
    config: &DaemonConfig,
) -> Result<Option<Arc<RemoteSecurity>>, DaemonStartupError> {
    let Some(config) = &config.remote_security else {
        return Ok(None);
    };
    let credentials = CredentialStore::load(&config.credential_store)
        .map_err(|error| DaemonStartupError::RemoteCredentialStore { code: error.code() })?;
    let authenticator = Authenticator::new(credentials)
        .map_err(|error| DaemonStartupError::RemoteAuthenticator { code: error.code() })?;
    let audit = AuditLog::open(&config.audit_log)
        .map_err(|error| DaemonStartupError::RemoteAudit { code: error.code() })?;
    Ok(Some(Arc::new(RemoteSecurity {
        authenticator: Arc::new(authenticator),
        audit: Arc::new(audit),
    })))
}

async fn register_plugin_devices(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    let Some(config) = &config.plugins else {
        return Ok(());
    };
    let control =
        ExecutionController::with_timeout(PLUGIN_STARTUP_TIMEOUT_MS, TimeoutScope::Request).1;
    let drivers = discover_plugin_drivers(&config.discovery, &control)
        .await
        .map_err(|error| DaemonStartupError::PluginDiscovery {
            code: plugin_startup_error_code(&error),
        })?;
    for driver in drivers {
        let info = driver.device_info().await;
        runtime
            .register(driver as Arc<dyn DeviceDriver>, info)
            .await
            .map_err(|_| DaemonStartupError::DeviceRegistration {
                code: "plugin_registration_failed",
            })?;
    }
    Ok(())
}

fn plugin_startup_error_code(error: &devicerail_core::DriverError) -> String {
    match error {
        devicerail_core::DriverError::Platform { code, .. }
            if !code.is_empty()
                && code.len() <= 64
                && code.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }) =>
        {
            code.clone()
        }
        devicerail_core::DriverError::Cancelled => "plugin_startup_cancelled".to_owned(),
        devicerail_core::DriverError::TimedOut => "plugin_startup_timed_out".to_owned(),
        _ => "plugin_startup_failed".to_owned(),
    }
}

async fn register_playwright_devices(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    let Some(config) = &config.playwright else {
        return Ok(());
    };
    let bridge = PlaywrightBridgeConfig::new(
        &config.node_path,
        &config.helper_path,
        config.endpoint.clone(),
        config.browser,
    )
    .map_err(|error| DaemonStartupError::PlaywrightDiscovery {
        code: error.to_error_info().code,
    })?;
    let control =
        ExecutionController::with_timeout(PLAYWRIGHT_STARTUP_TIMEOUT_MS, TimeoutScope::Request).1;
    let mut drivers = discover_playwright_drivers(bridge, &control)
        .await
        .map_err(|error| DaemonStartupError::PlaywrightDiscovery {
            code: error.to_error_info().code,
        })?;
    drivers.sort_by(|left, right| left.id().cmp(right.id()));
    if drivers.is_empty() {
        return Err(DaemonStartupError::PlaywrightNoPages);
    }
    for driver in drivers {
        let info = driver.device_info().await;
        runtime
            .register(driver as Arc<dyn DeviceDriver>, info)
            .await
            .map_err(|_| DaemonStartupError::DeviceRegistration {
                code: "playwright_registration_failed",
            })?;
    }
    Ok(())
}

async fn register_rdp_device(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    let Some(config) = &config.rdp else {
        return Ok(());
    };
    let bridge = Arc::new(SystemRdpBridge::new(config.bridge.clone()));
    let driver = Arc::new(
        RdpDriver::new(config.name.clone(), bridge)
            .map_err(|_| DaemonStartupError::InvalidRdpConfiguration)?,
    );
    let info = driver.device_info().await;
    runtime
        .register(driver as Arc<dyn DeviceDriver>, info)
        .await
        .map_err(|_| DaemonStartupError::DeviceRegistration {
            code: "rdp_registration_failed",
        })?;
    Ok(())
}

async fn start_configured_appium(
    config: &mut DaemonConfig,
) -> Result<Option<ManagedAppiumRuntime>, DaemonStartupError> {
    let managed = match config.ios.as_ref() {
        Some(IosStartupConfig::External(config)) => match &config.backend {
            IosDriverBackendConfig::Appium {
                server: AppiumServerConfig::Managed(config),
                ..
            } => Some(config.clone()),
            _ => None,
        },
        Some(IosStartupConfig::Managed(config)) => match &config.backend {
            IosDriverBackendConfig::Appium {
                server: AppiumServerConfig::Managed(config),
                ..
            } => Some(config.clone()),
            _ => None,
        },
        None => None,
    };
    let Some(managed) = managed else {
        return Ok(None);
    };
    let runtime = SystemAppiumHost
        .start(managed)
        .await
        .map_err(|error| DaemonStartupError::IosManagedAppiumStartup { code: error.code() })?;
    let endpoint = IosHttpEndpointConfig::new(runtime.endpoint().url())
        .and_then(|endpoint| endpoint.with_request_timeout_ms(IOS_RUNTIME_REQUEST_TIMEOUT_MS))
        .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
    if !endpoint.is_numeric_loopback() {
        return Err(DaemonStartupError::InvalidIosAppiumConfiguration);
    }
    let backend = match config.ios.as_mut() {
        Some(IosStartupConfig::External(config)) => &mut config.backend,
        Some(IosStartupConfig::Managed(config)) => &mut config.backend,
        None => return Err(DaemonStartupError::InvalidIosAppiumConfiguration),
    };
    let new_command_timeout_seconds = match backend {
        IosDriverBackendConfig::Appium {
            new_command_timeout_seconds,
            ..
        } => *new_command_timeout_seconds,
        IosDriverBackendConfig::DirectWda => {
            return Err(DaemonStartupError::InvalidIosAppiumConfiguration);
        }
    };
    *backend = IosDriverBackendConfig::Appium {
        server: AppiumServerConfig::External(endpoint),
        new_command_timeout_seconds,
    };
    eprintln!("DeviceRail managed Appium server is ready (ios_appium_ready)");
    Ok(Some(runtime))
}

struct ManagedIosDaemonRuntime {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), &'static str>>>,
}

impl ManagedIosDaemonRuntime {
    fn active(managed: ManagedIosRuntime) -> Self {
        let (shutdown, mut receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            if !*receiver.borrow() {
                let _ = receiver.changed().await;
            }
            managed.shutdown().await.map_err(|error| error.code())
        });
        Self {
            shutdown,
            task: Some(task),
        }
    }

    fn waiting(
        runtime: Arc<Registry>,
        config: ManagedIosConfig,
        backend: IosDriverBackendConfig,
        session_target: IosSessionTarget,
        initial_code: &'static str,
    ) -> Self {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_ios_hotplug_supervisor(
            runtime,
            config,
            backend,
            session_target,
            initial_code,
            receiver,
        ));
        Self {
            shutdown,
            task: Some(task),
        }
    }

    fn waiting_appium_discovery(
        runtime: Arc<Registry>,
        device_udid: Option<String>,
        backend: IosDriverBackendConfig,
        session_target: IosSessionTarget,
        initial_code: &'static str,
    ) -> Self {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_ios_appium_discovery_supervisor(
            runtime,
            device_udid,
            backend,
            session_target,
            initial_code,
            receiver,
        ));
        Self {
            shutdown,
            task: Some(task),
        }
    }

    async fn shutdown(mut self) -> Result<(), &'static str> {
        let _ = self.shutdown.send(true);
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match tokio::time::timeout(IOS_SUPERVISOR_SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("ios_hotplug_task_failed"),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err("ios_supervisor_shutdown_timeout")
            }
        }
    }
}

impl Drop for ManagedIosDaemonRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

async fn register_ios_device(
    runtime: Arc<Registry>,
    config: &DaemonConfig,
) -> Result<Option<ManagedIosDaemonRuntime>, DaemonStartupError> {
    let Some(config) = &config.ios else {
        return Ok(None);
    };
    match config {
        IosStartupConfig::External(config) => {
            register_ios_driver(
                runtime.as_ref(),
                config.backend.clone(),
                config.session_target,
                config.device_udid.clone(),
                config.device.clone(),
                config.wda_endpoint.clone(),
                config.mjpeg_endpoint.clone(),
            )
            .await?;
            Ok(None)
        }
        IosStartupConfig::Managed(config) => match &config.host {
            ManagedIosHostConfig::Wda(host_config) => {
                let managed = match SystemIosHost::default().start(host_config.clone()).await {
                    Ok(managed) => managed,
                    Err(error) if config.policy == IosManagedPolicy::Auto => {
                        eprintln!(
                            "DeviceRail managed iOS discovery is unavailable ({})",
                            error.code()
                        );
                        return Ok(Some(ManagedIosDaemonRuntime::waiting(
                            runtime,
                            host_config.clone(),
                            config.backend.clone(),
                            config.session_target,
                            error.code(),
                        )));
                    }
                    Err(error) => {
                        return Err(DaemonStartupError::IosManagedRequired { code: error.code() });
                    }
                };
                if let Err(error) = register_managed_ios_endpoint(
                    runtime.as_ref(),
                    &managed,
                    config.backend.clone(),
                    config.session_target,
                )
                .await
                {
                    let _ = managed.shutdown().await;
                    return Err(error);
                }
                Ok(Some(ManagedIosDaemonRuntime::active(managed)))
            }
            ManagedIosHostConfig::AppiumDiscovery { device_udid } => {
                let host = SystemIosHost::default();
                let device = match discover_ready_appium_device(&host, device_udid.as_deref()).await
                {
                    Ok(device) => device,
                    Err(error) if config.policy == IosManagedPolicy::Auto => {
                        eprintln!(
                            "DeviceRail Appium iOS discovery is waiting ({})",
                            error.code()
                        );
                        return Ok(Some(ManagedIosDaemonRuntime::waiting_appium_discovery(
                            runtime,
                            device_udid.clone(),
                            config.backend.clone(),
                            config.session_target,
                            error.code(),
                        )));
                    }
                    Err(error) => {
                        return Err(DaemonStartupError::IosManagedRequired { code: error.code() });
                    }
                };
                register_discovered_appium_device(
                    runtime.as_ref(),
                    config.backend.clone(),
                    config.session_target,
                    device,
                )
                .await?;
                Ok(None)
            }
        },
    }
}

async fn discover_ready_appium_device(
    host: &SystemIosHost,
    requested: Option<&str>,
) -> Result<IosHostDevice, IosHostError> {
    let discovery = host.discover().await?;
    select_ready_ios_device(&discovery.devices, requested)
}

async fn register_discovered_appium_device(
    runtime: &Registry,
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
    device: IosHostDevice,
) -> Result<(), DaemonStartupError> {
    let device_udid = device.udid;
    let device = IosDeviceConfig::new(device_udid.clone(), device.name, device.os_version)
        .map_err(|_| DaemonStartupError::InvalidManagedIosConfiguration)?;
    register_ios_driver(
        runtime,
        backend,
        session_target,
        device_udid,
        device,
        None,
        None,
    )
    .await
}

async fn run_ios_appium_discovery_supervisor(
    runtime: Arc<Registry>,
    device_udid: Option<String>,
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
    initial_code: &'static str,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), &'static str> {
    let host = SystemIosHost::default();
    let mut last_code = initial_code;
    let mut attempt = 1u32;
    loop {
        let delay = ios_hotplug_retry_delay(last_code, attempt);
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            _ = tokio::time::sleep(delay) => {}
        }
        let device = match discover_ready_appium_device(&host, device_udid.as_deref()).await {
            Ok(device) => device,
            Err(error) => {
                if error.code() != last_code {
                    eprintln!(
                        "DeviceRail Appium iOS discovery is waiting ({})",
                        error.code()
                    );
                    last_code = error.code();
                    attempt = 1;
                } else {
                    attempt = attempt.saturating_add(1);
                }
                continue;
            }
        };
        if let Err(error) = register_discovered_appium_device(
            runtime.as_ref(),
            backend.clone(),
            session_target,
            device,
        )
        .await
        {
            let code = managed_ios_registration_error_code(&error);
            if code != last_code {
                eprintln!("DeviceRail Appium iOS registration is waiting ({code})");
                last_code = code;
                attempt = 1;
            } else {
                attempt = attempt.saturating_add(1);
            }
            continue;
        }
        eprintln!("DeviceRail Appium iOS route is available (ios_hotplug_ready)");
        if !*shutdown.borrow() {
            let _ = shutdown.changed().await;
        }
        return Ok(());
    }
}

async fn run_ios_hotplug_supervisor(
    runtime: Arc<Registry>,
    config: ManagedIosConfig,
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
    initial_code: &'static str,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), &'static str> {
    let host = SystemIosHost::default();
    let mut last_code = initial_code;
    let mut attempt = 1u32;
    loop {
        let delay = ios_hotplug_retry_delay(last_code, attempt);
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            _ = tokio::time::sleep(delay) => {}
        }
        let started = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
            started = host.start(config.clone()) => started,
        };
        let managed = match started {
            Ok(managed) => managed,
            Err(error) => {
                if error.code() != last_code {
                    eprintln!(
                        "DeviceRail managed iOS hot-plug is waiting ({})",
                        error.code()
                    );
                    last_code = error.code();
                    attempt = 1;
                } else {
                    attempt = attempt.saturating_add(1);
                }
                continue;
            }
        };
        if let Err(error) = register_managed_ios_endpoint(
            runtime.as_ref(),
            &managed,
            backend.clone(),
            session_target,
        )
        .await
        {
            let code = managed_ios_registration_error_code(&error);
            let _ = managed.shutdown().await;
            if code != last_code {
                eprintln!("DeviceRail managed iOS hot-plug registration is waiting ({code})");
                last_code = code;
                attempt = 1;
            } else {
                attempt = attempt.saturating_add(1);
            }
            continue;
        }
        eprintln!("DeviceRail managed iOS route is available (ios_hotplug_ready)");
        if !*shutdown.borrow() {
            let _ = shutdown.changed().await;
        }
        return managed.shutdown().await.map_err(|error| error.code());
    }
}

fn ios_hotplug_retry_delay(code: &str, attempt: u32) -> Duration {
    if matches!(
        code,
        "ios_device_not_found"
            | "ios_device_disconnected"
            | "ios_simulator_not_booted"
            | "ios_device_locked"
            | "ios_pairing_required"
            | "ios_developer_services_unavailable"
    ) {
        return IOS_HOTPLUG_RETRY_MIN;
    }
    let seconds = 1u64 << attempt.min(5);
    Duration::from_secs(seconds).min(IOS_HOTPLUG_RETRY_MAX)
}

fn managed_ios_registration_error_code(error: &DaemonStartupError) -> &'static str {
    match error {
        DaemonStartupError::DeviceRegistration { code } => code,
        DaemonStartupError::InvalidManagedIosConfiguration => "ios_managed_registration_invalid",
        _ => "ios_managed_registration_failed",
    }
}

async fn register_managed_ios_endpoint(
    runtime: &Registry,
    managed: &ManagedIosRuntime,
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
) -> Result<(), DaemonStartupError> {
    let endpoint = managed.endpoint().clone();
    let wda_endpoint = IosHttpEndpointConfig::new(endpoint.wda_url.clone())
        .and_then(|endpoint| endpoint.with_request_timeout_ms(IOS_RUNTIME_REQUEST_TIMEOUT_MS))
        .map_err(|_| DaemonStartupError::InvalidManagedIosConfiguration)?;
    let device_udid = endpoint.device.udid;
    let device = IosDeviceConfig::new(
        device_udid.clone(),
        endpoint.device.name,
        endpoint.device.os_version,
    )
    .map_err(|_| DaemonStartupError::InvalidManagedIosConfiguration)?;
    register_ios_driver(
        runtime,
        backend,
        session_target,
        device_udid,
        device,
        Some(wda_endpoint),
        None,
    )
    .await
}

async fn register_ios_driver(
    runtime: &Registry,
    backend: IosDriverBackendConfig,
    session_target: IosSessionTarget,
    device_udid: String,
    device: IosDeviceConfig,
    wda_endpoint: Option<IosHttpEndpointConfig>,
    mjpeg_endpoint: Option<IosHttpEndpointConfig>,
) -> Result<(), DaemonStartupError> {
    let (driver, info): (Arc<dyn DeviceDriver>, DeviceInfo) = match (backend, session_target) {
        (IosDriverBackendConfig::DirectWda, IosSessionTarget::Safari) => {
            return Err(DaemonStartupError::IosSessionTargetRequiresAppium);
        }
        (IosDriverBackendConfig::DirectWda, IosSessionTarget::Native) => {
            let wda_endpoint = wda_endpoint.ok_or(DaemonStartupError::InvalidIosConfiguration)?;
            let wda = Arc::new(SystemWdaTransport::new(wda_endpoint));
            let mut driver = IosDriver::new(device, wda);
            if let Some(endpoint) = mjpeg_endpoint {
                driver = driver.with_mjpeg(Arc::new(SystemMjpegFrameSource::new(endpoint)));
            }
            let driver = Arc::new(driver);
            let info = driver.device_info().await;
            (driver, info)
        }
        (
            IosDriverBackendConfig::Appium {
                server: AppiumServerConfig::External(endpoint),
                new_command_timeout_seconds,
            },
            session_target,
        ) => {
            let request = match session_target {
                IosSessionTarget::Native => AppiumSessionRequest::new(device_udid),
                IosSessionTarget::Safari => AppiumSessionRequest::safari(device_udid),
            };
            let mut request = request
                .and_then(|request| {
                    request.with_new_command_timeout_seconds(new_command_timeout_seconds)
                })
                .and_then(|request| request.with_device_name(device.name().to_owned()))
                .and_then(|request| {
                    if let Some(version) = device.os_version() {
                        request.with_platform_version(version.to_owned())
                    } else {
                        Ok(request)
                    }
                })
                .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            if let Some(wda_endpoint) = wda_endpoint {
                request = request
                    .with_web_driver_agent_endpoint(wda_endpoint)
                    .map_err(|_| DaemonStartupError::InvalidIosAppiumConfiguration)?;
            }
            let transport = Arc::new(SystemAppiumTransport::new(endpoint));
            let mut driver = AppiumIosDriver::new(device, transport, request);
            if let Some(endpoint) = mjpeg_endpoint {
                driver = driver.with_mjpeg(Arc::new(SystemMjpegFrameSource::new(endpoint)));
            }
            let driver = Arc::new(driver);
            let info = driver.device_info().await;
            (driver, info)
        }
        (
            IosDriverBackendConfig::Appium {
                server: AppiumServerConfig::Managed(_),
                ..
            },
            _,
        ) => return Err(DaemonStartupError::InvalidIosAppiumConfiguration),
    };
    runtime
        .register(driver, info)
        .await
        .map_err(|_| DaemonStartupError::DeviceRegistration {
            code: "ios_registration_failed",
        })?;
    Ok(())
}

async fn register_harmony_devices(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    if config.harmony_mode == HarmonyDiscoveryMode::Off {
        return Ok(());
    }
    let (discovery_config, runtime_config) = match system_harmony_configs(&config.hdc_path) {
        Ok(configs) => configs,
        Err(error) => return handle_harmony_startup_code(config.harmony_mode, error.code()),
    };
    let discovery_hdc = match HarmonyHdc::system(discovery_config) {
        Ok(hdc) => hdc,
        Err(error) => return handle_harmony_startup_code(config.harmony_mode, error.code()),
    };
    let runtime_hdc = match HarmonyHdc::system(runtime_config) {
        Ok(hdc) => hdc,
        Err(error) => return handle_harmony_startup_code(config.harmony_mode, error.code()),
    };
    register_harmony_from_backend(
        runtime,
        config.harmony_mode,
        &SystemHarmonyBackend {
            discovery_hdc,
            runtime_hdc,
        },
    )
    .await
}

fn system_harmony_configs(
    hdc_path: &std::path::Path,
) -> Result<(SystemHdcConfig, SystemHdcConfig), devicerail_harmony_hdc::HarmonyHdcError> {
    let discovery = SystemHdcConfig::new(hdc_path, HARMONY_STARTUP_TIMEOUT)?;
    let runtime = SystemHdcConfig::new(hdc_path, HARMONY_RUNTIME_COMMAND_TIMEOUT)?;
    Ok((discovery, runtime))
}

async fn register_harmony_from_backend<B>(
    runtime: &Registry,
    mode: HarmonyDiscoveryMode,
    backend: &B,
) -> Result<(), DaemonStartupError>
where
    B: HarmonyStartupBackend + ?Sized,
{
    if mode == HarmonyDiscoveryMode::Off {
        return Ok(());
    }
    let report = match backend.discover().await {
        Ok(report) => report,
        Err(code) => return handle_harmony_startup_code(mode, code),
    };
    if !report.ignored_diagnostics.is_empty() {
        eprintln!(
            "DeviceRail HarmonyOS discovery ignored {} bounded diagnostic line(s)",
            report.ignored_diagnostics.len()
        );
    }
    let mut descriptors = report.devices;
    descriptors.sort_by(|left, right| left.target.cmp(&right.target));
    if descriptors.is_empty() {
        return match mode {
            HarmonyDiscoveryMode::Required => Err(DaemonStartupError::HarmonyRequiredNoDevices),
            HarmonyDiscoveryMode::Auto => {
                eprintln!("DeviceRail HarmonyOS discovery found no stable devices");
                Ok(())
            }
            HarmonyDiscoveryMode::Off => Ok(()),
        };
    }
    for descriptor in descriptors {
        let (driver, info) = backend.build_route(descriptor).await;
        if let Err(error) = runtime.register(driver, info).await {
            let code = error.to_error_info().code;
            if mode == HarmonyDiscoveryMode::Required {
                return Err(DaemonStartupError::DeviceRegistration {
                    code: "harmony_registration_failed",
                });
            }
            eprintln!("DeviceRail skipped one HarmonyOS route ({code})");
        }
    }
    Ok(())
}

fn handle_harmony_startup_code(
    mode: HarmonyDiscoveryMode,
    code: &'static str,
) -> Result<(), DaemonStartupError> {
    match mode {
        HarmonyDiscoveryMode::Required => Err(DaemonStartupError::HarmonyRequired { code }),
        HarmonyDiscoveryMode::Auto => {
            eprintln!(
                "DeviceRail HarmonyOS discovery is unavailable for this run ({})",
                code
            );
            Ok(())
        }
        HarmonyDiscoveryMode::Off => Ok(()),
    }
}

async fn register_desktop_device(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    register_desktop_from_backend(runtime, &config.desktop, &SystemDesktopStartupBackend).await
}

async fn register_desktop_from_backend<B>(
    runtime: &Registry,
    config: &DesktopStartupConfig,
    backend: &B,
) -> Result<(), DaemonStartupError>
where
    B: DesktopStartupBackend + ?Sized,
{
    if config.mode == DesktopDiscoveryMode::Off {
        return Ok(());
    }
    let control =
        ExecutionController::with_timeout(DESKTOP_STARTUP_TIMEOUT_MS, TimeoutScope::Request).1;
    let (driver, info) = match backend.discover(config, &control).await {
        Ok(route) => route,
        Err(code) => return handle_desktop_startup_code(config.mode, code),
    };
    if let Err(error) = runtime.register(driver, info).await {
        let code = error.to_error_info().code;
        if config.mode == DesktopDiscoveryMode::Required {
            return Err(DaemonStartupError::DeviceRegistration {
                code: "desktop_registration_failed",
            });
        }
        eprintln!("DeviceRail skipped the native desktop route ({code})");
    }
    Ok(())
}

fn handle_desktop_startup_code(
    mode: DesktopDiscoveryMode,
    code: &'static str,
) -> Result<(), DaemonStartupError> {
    match mode {
        DesktopDiscoveryMode::Required => Err(DaemonStartupError::DesktopRequired { code }),
        DesktopDiscoveryMode::Auto => {
            eprintln!("DeviceRail desktop discovery is unavailable for this run ({code})");
            Ok(())
        }
        DesktopDiscoveryMode::Off => Ok(()),
    }
}

async fn register_android_devices(
    runtime: &Registry,
    config: &DaemonConfig,
) -> Result<(), DaemonStartupError> {
    if config.android_mode == AndroidDiscoveryMode::Off {
        return Ok(());
    }

    let (discovery_config, runtime_config) = match system_android_configs(&config.adb_path) {
        Ok(configs) => configs,
        Err(error) => return handle_android_startup_code(config.android_mode, error.code()),
    };
    let discovery_adb = match AndroidAdb::system(discovery_config) {
        Ok(adb) => adb,
        Err(error) => return handle_android_startup_code(config.android_mode, error.code()),
    };
    let runtime_adb = match AndroidAdb::system(runtime_config) {
        Ok(adb) => adb,
        Err(error) => return handle_android_startup_code(config.android_mode, error.code()),
    };
    register_android_from_backend(
        runtime,
        config.android_mode,
        &SystemAndroidBackend {
            discovery_adb,
            runtime_adb,
        },
    )
    .await
}

fn system_android_configs(
    adb_path: &std::path::Path,
) -> Result<(SystemAdbConfig, SystemAdbConfig), devicerail_android_adb::AndroidAdbError> {
    let base = SystemAdbConfig::new(adb_path)?;
    let discovery = base.clone().with_command_timeout(ANDROID_STARTUP_TIMEOUT)?;
    let runtime = base.with_command_timeout(ANDROID_RUNTIME_COMMAND_TIMEOUT)?;
    Ok((discovery, runtime))
}

async fn register_android_from_backend<B>(
    runtime: &Registry,
    mode: AndroidDiscoveryMode,
    backend: &B,
) -> Result<(), DaemonStartupError>
where
    B: AndroidStartupBackend + ?Sized,
{
    if mode == AndroidDiscoveryMode::Off {
        return Ok(());
    }
    let report = match backend.discover().await {
        Ok(report) => report,
        Err(code) => return handle_android_startup_code(mode, code),
    };

    for (index, issue) in report.issues.iter().take(16).enumerate() {
        eprintln!(
            "DeviceRail Android discovery issue {}: {}",
            index + 1,
            bounded_diagnostic(&issue.message)
        );
    }
    if report.issues.len() > 16 {
        eprintln!(
            "DeviceRail Android discovery omitted {} additional issue(s)",
            report.issues.len() - 16
        );
    }

    let mut descriptors = report.devices;
    descriptors.sort_by(|left, right| left.serial.cmp(&right.serial));
    if descriptors.is_empty() {
        return match mode {
            AndroidDiscoveryMode::Required => Err(DaemonStartupError::AndroidRequiredNoDevices),
            AndroidDiscoveryMode::Auto => {
                eprintln!("DeviceRail Android discovery found no stable devices");
                Ok(())
            }
            AndroidDiscoveryMode::Off => Ok(()),
        };
    }

    for descriptor in descriptors {
        let (driver, info) = match backend.build_route(descriptor).await {
            Ok(route) => route,
            Err(code) => {
                if mode == AndroidDiscoveryMode::Required {
                    return Err(DaemonStartupError::AndroidRequired { code });
                }
                eprintln!(
                    "DeviceRail skipped one Android device during initialization ({})",
                    code
                );
                continue;
            }
        };
        if let Err(error) = runtime.register(driver, info).await {
            let code = error.to_error_info().code;
            if mode == AndroidDiscoveryMode::Required {
                return Err(DaemonStartupError::DeviceRegistration {
                    code: "android_registration_failed",
                });
            }
            eprintln!("DeviceRail skipped one Android route ({code})");
        }
    }
    Ok(())
}

fn handle_android_startup_code(
    mode: AndroidDiscoveryMode,
    code: &'static str,
) -> Result<(), DaemonStartupError> {
    match mode {
        AndroidDiscoveryMode::Required => Err(DaemonStartupError::AndroidRequired { code }),
        AndroidDiscoveryMode::Auto => {
            eprintln!(
                "DeviceRail Android discovery is unavailable for this run ({})",
                code
            );
            Ok(())
        }
        AndroidDiscoveryMode::Off => Ok(()),
    }
}

fn bounded_diagnostic(message: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut output = message.chars().take(MAX_CHARS).collect::<String>();
    if message.chars().count() > MAX_CHARS {
        output.push('…');
    }
    output
}

fn evidence_code(error: &devicerail_core::EvidenceError) -> &'static str {
    match error {
        devicerail_core::EvidenceError::StoreBusy => "evidence_store_busy",
        devicerail_core::EvidenceError::UnsupportedStoreVersion(_) => {
            "unsupported_evidence_store_version"
        }
        devicerail_core::EvidenceError::CorruptStore(_)
        | devicerail_core::EvidenceError::Corrupt { .. }
        | devicerail_core::EvidenceError::CorruptMetadata { .. } => "evidence_corrupt",
        devicerail_core::EvidenceError::UnsafePath(_) => "unsafe_evidence_path",
        devicerail_core::EvidenceError::InvalidConfiguration(_) => {
            "invalid_evidence_store_configuration"
        }
        _ => "evidence_initialization_failed",
    }
}

fn cleanup_error_code(error: &SessionCleanupError) -> &'static str {
    match error {
        SessionCleanupError::Events(_) => "event_store_cleanup_failed",
        SessionCleanupError::Evidence(error) => evidence_code(error),
    }
}

async fn serve_stdio(
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: EvidenceCleanup,
    mut streams: Option<EventStreamServer>,
    mut distributed_server: Option<DistributedPeerServerRuntime>,
    appium_runtime: &mut Option<ManagedAppiumRuntime>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut lines = match stdin_lines() {
        Ok(lines) => lines,
        Err(error) => {
            let _ = shutdown_distributed_peer_server(&mut distributed_server).await;
            return Err(error.into());
        }
    };
    let (responses, mut writer_done) = match stdout_writer() {
        Ok(writer) => writer,
        Err(error) => {
            drop(lines);
            let _ = shutdown_distributed_peer_server(&mut distributed_server).await;
            return Err(error.into());
        }
    };
    if let Some(server) = &distributed_server {
        server.mark_ready();
    }
    let mut connection = ConnectionState::default();
    let registry = Arc::new(RequestRegistry::default());
    let mut requests = JoinSet::new();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let mut writer_finished = false;
    let mut serve_error = None;

    loop {
        tokio::select! {
            line = lines.recv() => {
                let line = match line {
                    Some(Ok(line)) => line,
                    None => break,
                    Some(Err(error)) => {
                        serve_error = Some(error);
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }

                let decoded = decode_request(&line);
                drop(line);
                let request = match decoded {
                    Ok(request) => request,
                    Err(response) => {
                        if let Err(error) = queue_response(&responses, *response) {
                            serve_error = Some(error);
                            break;
                        }
                        continue;
                    }
                };

                if registry.contains(&request.id) {
                    if let Err(error) = queue_response(
                        &responses,
                        RpcResponse::failure(
                            Some(request.id.clone()),
                            request_id_in_use(request.id),
                        ),
                    ) {
                        serve_error = Some(error);
                        break;
                    }
                    continue;
                }

                if method_runs_concurrently(&request.method) && connection.context().is_some() {
                    lazily_select_sole(&mut connection, runtime.as_ref()).await;
                    if registry.len() >= MAX_IN_FLIGHT_REQUESTS {
                        if let Err(error) = queue_response(
                            &responses,
                            RpcResponse::failure(
                                Some(request.id),
                                too_many_requests(),
                            ),
                        ) {
                            serve_error = Some(error);
                            break;
                        }
                        continue;
                    }
                    let (controller, control) = request_control(request.timeout_ms);
                    let admitted_route = selected_route(&connection, runtime.as_ref()).await;
                    if admitted_route.is_err() {
                        let response = dispatch_routed_with_evidence(
                            request,
                            runtime.as_ref(),
                            DispatchResources {
                                events: events.as_ref(),
                                evidence: &evidence,
                                streams: None,
                            },
                            &mut connection,
                            &control,
                            registry.as_ref(),
                            Some(admitted_route),
                        )
                        .await;
                        if let Err(error) = queue_response(&responses, response) {
                            serve_error = Some(error);
                            break;
                        }
                        continue;
                    }
                    if !registry.register(request.id.clone(), controller) {
                        if let Err(error) = queue_response(
                            &responses,
                            RpcResponse::failure(
                                Some(request.id.clone()),
                                request_id_in_use(request.id),
                            ),
                        ) {
                            serve_error = Some(error);
                            break;
                        }
                        continue;
                    }

                    let runtime = Arc::clone(&runtime);
                    let events = Arc::clone(&events);
                    let evidence = evidence.clone();
                    let registry = Arc::clone(&registry);
                    let mut request_connection = connection.clone();
                    let request_id = request.id.clone();
                    requests.spawn(async move {
                        let registration =
                            RequestRegistration::new(registry.clone(), request_id);
                        let response = dispatch_routed_with_evidence(
                            request,
                            runtime.as_ref(),
                            DispatchResources {
                                events: events.as_ref(),
                                evidence: &evidence,
                                streams: None,
                            },
                            &mut request_connection,
                            &control,
                            registry.as_ref(),
                            Some(admitted_route),
                        )
                        .await;
                        registration.mark_completed();
                        (response, registration)
                    });
                } else {
                    let (_, control) = request_control(request.timeout_ms);
                    let response = dispatch_controlled_with_evidence(
                        request,
                        runtime.as_ref(),
                        DispatchResources {
                            events: events.as_ref(),
                            evidence: &evidence,
                            streams: streams.as_ref(),
                        },
                        &mut connection,
                        &control,
                        registry.as_ref(),
                    )
                    .await;
                    if let Err(error) = queue_response(&responses, response) {
                        serve_error = Some(error);
                        break;
                    }
                }
            }
            joined = requests.join_next(), if !requests.is_empty() => {
                match joined {
                    Some(Ok((response, registration))) => {
                        if let Err(error) = queue_response(&responses, response) {
                            drop(registration);
                            serve_error = Some(error);
                            break;
                        }
                        drop(registration);
                    }
                    Some(Err(error)) => {
                        serve_error = Some(std::io::Error::other(format!(
                            "request task failed: {error}"
                        )));
                        break;
                    }
                    None => {}
                }
            }
            result = &mut writer_done => {
                writer_finished = true;
                match result {
                    Ok(Ok(())) => break,
                    Ok(Err(error)) => {
                        serve_error = Some(error);
                        break;
                    }
                    Err(error) => {
                        serve_error = Some(std::io::Error::other(format!(
                            "response writer completion channel failed: {error}"
                        )));
                        break;
                    }
                }
            }
            result = &mut shutdown => {
                if let Err(error) = result {
                    serve_error = Some(error);
                }
                break;
            },
            code = distributed_peer_server_failure(&mut distributed_server) => {
                serve_error = Some(std::io::Error::other(format!(
                    "distributed peer server failed ({code})"
                )));
                break;
            },
            code = managed_appium_failure(appium_runtime) => {
                serve_error = Some(std::io::Error::other(
                    DaemonStartupError::IosManagedAppiumRuntime { code },
                ));
                break;
            },
        }
    }

    drop(lines);
    begin_distributed_peer_server_shutdown(&distributed_server);
    if let Some(streams) = &streams {
        streams.begin_shutdown();
    }
    registry.cancel_all(CancellationReason::Shutdown);
    let drain_result = drain_requests(&mut requests, &responses).await;
    let distributed_result = shutdown_distributed_peer_server(&mut distributed_server).await;
    let shutdown_result =
        shutdown_runtime(runtime.as_ref(), events.as_ref(), &mut connection).await;
    let stream_result = match &mut streams {
        Some(streams) => streams.finish_shutdown().await,
        None => Ok(()),
    };

    drop(responses);
    let writer_result = if !writer_finished {
        match timeout_at(Instant::now() + SHUTDOWN_GRACE, &mut writer_done).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(error)) => Err(std::io::Error::other(format!(
                "response writer completion channel failed: {error}"
            ))),
            Err(_) => Err(std::io::Error::other("response writer shutdown timed out")),
        }
    } else {
        Ok(())
    };

    if let Some(error) = serve_error {
        return Err(error.into());
    }
    drain_result?;
    distributed_result?;
    shutdown_result?;
    stream_result?;
    writer_result?;
    Ok(())
}

/// Serves multiple local RPC clients from one lease authority. The listener
/// remains loopback-only even when optional authentication is enabled; remote
/// clients require an independently secured SSH or mTLS tunnel.
async fn serve_loopback_rpc(
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: EvidenceCleanup,
    streams: Option<EventStreamServer>,
    address: SocketAddr,
    mut services: LoopbackListenerServices<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !address.ip().is_loopback() {
        let _ = shutdown_distributed_peer_server(&mut services.distributed_server).await;
        return Err(DaemonStartupError::InvalidRpcListen.into());
    }
    let listener = match TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = shutdown_distributed_peer_server(&mut services.distributed_server).await;
            return Err(error.into());
        }
    };
    let local_address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => {
            let _ = shutdown_distributed_peer_server(&mut services.distributed_server).await;
            return Err(error.into());
        }
    };
    eprintln!("DeviceRail RPC listening on {local_address}");
    if let Some(server) = &services.distributed_server {
        server.mark_ready();
    }
    serve_loopback_listener_with_security(
        runtime,
        events,
        evidence,
        streams,
        listener,
        services,
        shutdown_signal(),
    )
    .await
}

struct LoopbackListenerServices<'a> {
    remote_security: Option<Arc<RemoteSecurity>>,
    distributed_server: Option<DistributedPeerServerRuntime>,
    appium_runtime: &'a mut Option<ManagedAppiumRuntime>,
}

#[cfg(test)]
async fn serve_loopback_listener<F>(
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: EvidenceCleanup,
    streams: Option<EventStreamServer>,
    listener: TcpListener,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    let mut appium_runtime = None;
    serve_loopback_listener_with_security(
        runtime,
        events,
        evidence,
        streams,
        listener,
        LoopbackListenerServices {
            remote_security: None,
            distributed_server: None,
            appium_runtime: &mut appium_runtime,
        },
        shutdown,
    )
    .await
}

async fn serve_loopback_listener_with_security<F>(
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: EvidenceCleanup,
    mut streams: Option<EventStreamServer>,
    listener: TcpListener,
    services: LoopbackListenerServices<'_>,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: std::future::Future<Output = std::io::Result<()>>,
{
    let LoopbackListenerServices {
        remote_security,
        mut distributed_server,
        appium_runtime,
    } = services;
    tokio::pin!(shutdown);
    let mut connections = JoinSet::new();
    let mut serve_error = None;
    let (connection_shutdown, _) = watch::channel::<Option<Instant>>(None);

    loop {
        tokio::select! {
            accepted = listener.accept(), if connections.len() < MAX_LOOPBACK_CONNECTIONS => match accepted {
                Ok((socket, peer)) if peer.ip().is_loopback() => {
                    let runtime = Arc::clone(&runtime);
                    let events = Arc::clone(&events);
                    let evidence = evidence.clone();
                    let remote_security = remote_security.clone();
                    let shutdown = connection_shutdown.subscribe();
                    connections.spawn(async move {
                        serve_loopback_connection_until_shutdown(
                            socket,
                            runtime,
                            events,
                            evidence,
                            remote_security,
                            shutdown,
                        )
                        .await
                    });
                }
                Ok((_socket, _peer)) => {
                    // A loopback-bound listener should never produce this, but
                    // fail closed if the host network stack violates it.
                    serve_error = Some(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "non-loopback RPC peer rejected",
                    ));
                    break;
                }
                Err(error) => {
                    serve_error = Some(error);
                    break;
                }
            },
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(result) = joined {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => eprintln!(
                            "DeviceRail loopback client closed with an error ({})",
                            bounded_diagnostic(&error.to_string())
                        ),
                        Err(error) => eprintln!(
                            "DeviceRail loopback client task failed ({})",
                            bounded_diagnostic(&error.to_string())
                        ),
                    }
                }
            },
            result = &mut shutdown => {
                if let Err(error) = result {
                    serve_error = Some(error);
                }
                break;
            }
            code = distributed_peer_server_failure(&mut distributed_server) => {
                serve_error = Some(std::io::Error::other(format!(
                    "distributed peer server failed ({code})"
                )));
                break;
            }
            code = managed_appium_failure(appium_runtime) => {
                serve_error = Some(std::io::Error::other(
                    DaemonStartupError::IosManagedAppiumRuntime { code },
                ));
                break;
            }
        }
    }

    drop(listener);
    begin_distributed_peer_server_shutdown(&distributed_server);
    if let Some(streams) = &streams {
        streams.begin_shutdown();
    }
    let connection_deadline = Instant::now() + SHUTDOWN_GRACE;
    // Send one absolute deadline so every connection reserves time for its
    // own session/lease cleanup before the parent is allowed to abort it.
    let _ = connection_shutdown.send(Some(connection_deadline));
    let connection_result = drain_loopback_connections(&mut connections, connection_deadline).await;
    let distributed_result = shutdown_distributed_peer_server(&mut distributed_server).await;
    runtime.release_all_leases(now_ms()).await;
    let mut shutdown_connection = ConnectionState::default();
    let runtime_result =
        shutdown_runtime(runtime.as_ref(), events.as_ref(), &mut shutdown_connection)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()));
    let stream_result = match &mut streams {
        Some(streams) => streams.finish_shutdown().await,
        None => Ok(()),
    };
    if let Some(error) = serve_error {
        return Err(error.into());
    }
    connection_result?;
    distributed_result?;
    runtime_result?;
    stream_result?;
    Ok(())
}

#[cfg(test)]
async fn serve_loopback_connection(
    socket: TcpStream,
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: EvidenceCleanup,
) -> std::io::Result<()> {
    let (_shutdown_sender, shutdown) = watch::channel::<Option<Instant>>(None);
    serve_loopback_connection_until_shutdown(socket, runtime, events, evidence, None, shutdown)
        .await
}

async fn authenticate_loopback_connection<R, W>(
    reader: &mut R,
    writer: &mut W,
    security: &RemoteSecurity,
    connection_id: &str,
    shutdown: &mut watch::Receiver<Option<Instant>>,
) -> std::io::Result<AuthenticatedPrincipal>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let deadline = Instant::now() + REMOTE_AUTH_DEADLINE;
    let mut session = security.authenticator.session();
    let mut frame_count = 0_usize;
    loop {
        let line = tokio::select! {
            biased;
            _ = receive_loopback_shutdown(shutdown) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "remote authentication interrupted by shutdown",
                ));
            }
            line = timeout_at(deadline, read_bounded_async_line(reader, MAX_FRAME_BYTES)) => {
                line.map_err(|_| std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "remote authentication deadline exceeded",
                ))??
            }
        };
        let Some(line) = line else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "connection closed before remote authentication",
            ));
        };
        if line.trim().is_empty() {
            continue;
        }
        frame_count += 1;
        if frame_count > REMOTE_AUTH_MAX_FRAMES {
            append_remote_audit(
                security,
                AuditEvent {
                    at_ms: now_ms(),
                    connection_id: connection_id.to_owned(),
                    principal_id: None,
                    method: "rpc.invalid".to_owned(),
                    required_permission: None,
                    decision: AuditDecision::Denied,
                    outcome: AuditOutcome::Failed,
                    error_code: Some("auth_attempt_limit".to_owned()),
                },
            )
            .await?;
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "remote authentication frame limit exceeded",
            ));
        }
        let request = match decode_request(&line) {
            Ok(request) => request,
            Err(response) => {
                append_remote_audit(
                    security,
                    AuditEvent {
                        at_ms: now_ms(),
                        connection_id: connection_id.to_owned(),
                        principal_id: None,
                        method: "rpc.invalid".to_owned(),
                        required_permission: None,
                        decision: AuditDecision::Denied,
                        outcome: AuditOutcome::Failed,
                        error_code: Some("invalid_request".to_owned()),
                    },
                )
                .await?;
                write_auth_response(writer, *response, deadline, shutdown).await?;
                continue;
            }
        };
        let request_id = request.id.clone();
        if request.timeout_ms.is_some() {
            let error = authentication_error("auth_request_invalid", true);
            append_auth_decision(
                security,
                connection_id,
                None,
                &request.method,
                AuditDecision::Denied,
                Some("auth_request_invalid"),
            )
            .await?;
            write_auth_response(
                writer,
                RpcResponse::failure(Some(request_id), error),
                deadline,
                shutdown,
            )
            .await?;
            continue;
        }
        match request.method.as_str() {
            "auth.challenge" => {
                let decoded = request
                    .params
                    .map(RpcParams::into_value)
                    .ok_or(())
                    .and_then(|value| {
                        serde_json::from_value::<AuthChallengeRequest>(value).map_err(|_| ())
                    });
                let principal_hint = decoded
                    .as_ref()
                    .ok()
                    .map(|value| value.principal_id.clone());
                match decoded
                    .map_err(|_| devicerail_remote_auth::AuthError::InvalidRequest)
                    .and_then(|value| session.begin(value, std::time::Instant::now()))
                {
                    Ok(challenge) => {
                        append_auth_decision(
                            security,
                            connection_id,
                            principal_hint,
                            "auth.challenge",
                            AuditDecision::Allowed,
                            None,
                        )
                        .await?;
                        let value = serde_json::to_value(challenge).map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "authentication response serialization failed",
                            )
                        })?;
                        write_auth_response(
                            writer,
                            RpcResponse::success(request_id, value),
                            deadline,
                            shutdown,
                        )
                        .await?;
                    }
                    Err(error) => {
                        append_auth_decision(
                            security,
                            connection_id,
                            principal_hint,
                            "auth.challenge",
                            AuditDecision::Denied,
                            Some(error.code()),
                        )
                        .await?;
                        let close =
                            matches!(error, devicerail_remote_auth::AuthError::AttemptsExceeded);
                        write_auth_response(
                            writer,
                            RpcResponse::failure(
                                Some(request_id),
                                authentication_error(error.code(), !close),
                            ),
                            deadline,
                            shutdown,
                        )
                        .await?;
                        if close {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "remote authentication attempt limit exceeded",
                            ));
                        }
                    }
                }
            }
            "auth.respond" => {
                let decoded = request
                    .params
                    .map(RpcParams::into_value)
                    .ok_or(())
                    .and_then(|value| {
                        serde_json::from_value::<AuthProofRequest>(value).map_err(|_| ())
                    });
                match decoded
                    .map_err(|_| devicerail_remote_auth::AuthError::InvalidRequest)
                    .and_then(|value| session.finish(value, std::time::Instant::now()))
                {
                    Ok(principal) => {
                        append_auth_decision(
                            security,
                            connection_id,
                            Some(principal.id().to_owned()),
                            "auth.respond",
                            AuditDecision::Allowed,
                            None,
                        )
                        .await?;
                        let value = serde_json::to_value(AuthSuccess::from_principal(&principal))
                            .map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "authentication response serialization failed",
                            )
                        })?;
                        write_auth_response(
                            writer,
                            RpcResponse::success(request_id, value),
                            deadline,
                            shutdown,
                        )
                        .await?;
                        return Ok(principal);
                    }
                    Err(error) => {
                        // Proof failures intentionally do not disclose whether
                        // principal, key, challenge, expiry, or HMAC differed.
                        let code =
                            if matches!(error, devicerail_remote_auth::AuthError::InvalidRequest) {
                                "auth_request_invalid"
                            } else {
                                "authentication_failed"
                            };
                        append_auth_decision(
                            security,
                            connection_id,
                            None,
                            "auth.respond",
                            AuditDecision::Denied,
                            Some(code),
                        )
                        .await?;
                        write_auth_response(
                            writer,
                            RpcResponse::failure(
                                Some(request_id),
                                authentication_error(code, true),
                            ),
                            deadline,
                            shutdown,
                        )
                        .await?;
                    }
                }
            }
            method => {
                let audit_method = if required_permission(method).is_some() {
                    method
                } else {
                    "rpc.unknown"
                };
                append_auth_decision(
                    security,
                    connection_id,
                    None,
                    audit_method,
                    AuditDecision::Denied,
                    Some("authentication_required"),
                )
                .await?;
                write_auth_response(
                    writer,
                    RpcResponse::failure(
                        Some(request_id),
                        authentication_error("authentication_required", true),
                    ),
                    deadline,
                    shutdown,
                )
                .await?;
            }
        }
    }
}

async fn write_auth_response<W>(
    writer: &mut W,
    response: RpcResponse,
    deadline: Instant,
    shutdown: &mut watch::Receiver<Option<Instant>>,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    match timeout_at(
        deadline,
        write_loopback_response(writer, response, shutdown),
    )
    .await
    {
        Ok(Ok(None)) => Ok(()),
        Ok(Ok(Some(_))) => Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "remote authentication interrupted by shutdown",
        )),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "remote authentication response deadline exceeded",
        )),
    }
}

async fn append_auth_decision(
    security: &RemoteSecurity,
    connection_id: &str,
    principal_id: Option<String>,
    method: &str,
    decision: AuditDecision,
    error_code: Option<&str>,
) -> std::io::Result<()> {
    append_remote_audit(
        security,
        AuditEvent {
            at_ms: now_ms(),
            connection_id: connection_id.to_owned(),
            principal_id,
            method: method.to_owned(),
            required_permission: None,
            decision,
            outcome: if decision == AuditDecision::Allowed {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
            error_code: error_code.map(str::to_owned),
        },
    )
    .await
}

async fn authorize_remote_request(
    security: &RemoteSecurity,
    connection_id: &str,
    principal: &AuthenticatedPrincipal,
    request: &RpcRequest,
) -> std::io::Result<Option<RpcResponse>> {
    let required = required_permission(&request.method);
    let allowed = required.is_some_and(|permission| principal.allows(permission));
    let audit_method = if required.is_some() {
        request.method.clone()
    } else {
        "rpc.unknown".to_owned()
    };
    append_remote_audit(
        security,
        AuditEvent {
            at_ms: now_ms(),
            connection_id: connection_id.to_owned(),
            principal_id: Some(principal.id().to_owned()),
            method: audit_method,
            required_permission: required,
            decision: if allowed {
                AuditDecision::Allowed
            } else {
                AuditDecision::Denied
            },
            outcome: if allowed {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
            error_code: (!allowed).then(|| "permission_denied".to_owned()),
        },
    )
    .await?;
    if allowed {
        Ok(None)
    } else {
        Ok(Some(RpcResponse::failure(
            Some(request.id.clone()),
            permission_denied(required),
        )))
    }
}

async fn append_remote_audit(security: &RemoteSecurity, event: AuditEvent) -> std::io::Result<()> {
    let audit = Arc::clone(&security.audit);
    tokio::task::spawn_blocking(move || audit.append(event))
        .await
        .map_err(|_| std::io::Error::other("remote audit task failed"))?
        .map(|_| ())
        .map_err(|error| std::io::Error::other(format!("remote audit failed ({})", error.code())))
}

fn authentication_error(code: &str, retryable: bool) -> RpcError {
    rpc_error(
        REMOTE_AUTH_ERROR,
        code,
        "remote authentication failed",
        retryable,
        Some(json!({ "authProtocolVersion": "1" })),
    )
}

fn permission_denied(required: Option<Permission>) -> RpcError {
    rpc_error(
        REMOTE_AUTH_ERROR,
        "permission_denied",
        "the authenticated principal is not permitted to call this method",
        false,
        required.map(|permission| json!({ "requiredPermission": permission })),
    )
}

async fn serve_loopback_connection_until_shutdown(
    socket: TcpStream,
    runtime: Arc<Registry>,
    events: Arc<MemoryEventStore>,
    evidence: EvidenceCleanup,
    remote_security: Option<Arc<RemoteSecurity>>,
    mut shutdown: watch::Receiver<Option<Instant>>,
) -> std::io::Result<()> {
    let (read, mut write) = socket.into_split();
    let mut reader = TokioBufReader::new(read);
    let security_connection_id = remote_security.as_ref().map(|_| Uuid::new_v4().to_string());
    let authenticated_principal = match (&remote_security, &security_connection_id) {
        (Some(security), Some(connection_id)) => Some(
            timeout_at(
                Instant::now() + REMOTE_AUTH_DEADLINE,
                authenticate_loopback_connection(
                    &mut reader,
                    &mut write,
                    security,
                    connection_id,
                    &mut shutdown,
                ),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "remote authentication deadline exceeded",
                )
            })??,
        ),
        _ => None,
    };
    let mut connection = ConnectionState::loopback_tcp();
    let requests = Arc::new(RequestRegistry::default());
    let mut tasks = JoinSet::new();
    let mut result = Ok(());
    let mut global_shutdown_deadline = None;

    loop {
        tokio::select! {
            line = read_bounded_async_line(&mut reader, MAX_FRAME_BYTES) => {
                let line = match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let request = match decode_request(&line) {
                    Ok(request) => request,
                    Err(response) => {
                        if let (Some(security), Some(connection_id), Some(principal)) = (
                            &remote_security,
                            &security_connection_id,
                            &authenticated_principal,
                        ) {
                            append_remote_audit(
                                security,
                                AuditEvent {
                                    at_ms: now_ms(),
                                    connection_id: connection_id.clone(),
                                    principal_id: Some(principal.id().to_owned()),
                                    method: "rpc.invalid".to_owned(),
                                    required_permission: None,
                                    decision: AuditDecision::Denied,
                                    outcome: AuditOutcome::Failed,
                                    error_code: Some("invalid_request".to_owned()),
                                },
                            )
                            .await?;
                        }
                        match write_loopback_response(&mut write, *response, &mut shutdown).await {
                            Ok(None) => {}
                            Ok(Some(deadline)) => {
                                global_shutdown_deadline = Some(deadline);
                                break;
                            }
                            Err(error) => {
                                result = Err(error);
                                break;
                            }
                        }
                        continue;
                    }
                };
                if let (Some(security), Some(connection_id), Some(principal)) = (
                    &remote_security,
                    &security_connection_id,
                    &authenticated_principal,
                ) {
                    match authorize_remote_request(
                        security,
                        connection_id,
                        principal,
                        &request,
                    )
                    .await?
                    {
                        None => {}
                        Some(response) => {
                            match write_loopback_response(&mut write, response, &mut shutdown).await {
                                Ok(None) => {}
                                Ok(Some(deadline)) => {
                                    global_shutdown_deadline = Some(deadline);
                                    break;
                                }
                                Err(error) => {
                                    result = Err(error);
                                    break;
                                }
                            }
                            continue;
                        }
                    }
                }
                if requests.contains(&request.id) {
                    let response = RpcResponse::failure(
                        Some(request.id.clone()),
                        request_id_in_use(request.id),
                    );
                    match write_loopback_response(&mut write, response, &mut shutdown).await {
                        Ok(None) => {}
                        Ok(Some(deadline)) => {
                            global_shutdown_deadline = Some(deadline);
                            break;
                        }
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                    continue;
                }

                if method_runs_concurrently(&request.method) && connection.context().is_some() {
                    lazily_select_sole(&mut connection, runtime.as_ref()).await;
                    if requests.len() >= MAX_IN_FLIGHT_REQUESTS {
                        let response = RpcResponse::failure(Some(request.id), too_many_requests());
                        match write_loopback_response(&mut write, response, &mut shutdown).await {
                            Ok(None) => {}
                            Ok(Some(deadline)) => {
                                global_shutdown_deadline = Some(deadline);
                                break;
                            }
                            Err(error) => {
                                result = Err(error);
                                break;
                            }
                        }
                        continue;
                    }
                    let (controller, control) = request_control(request.timeout_ms);
                    let admitted_route = selected_route(&connection, runtime.as_ref()).await;
                    if !requests.register(request.id.clone(), controller) {
                        let response = RpcResponse::failure(
                            Some(request.id.clone()),
                            request_id_in_use(request.id),
                        );
                        match write_loopback_response(&mut write, response, &mut shutdown).await {
                            Ok(None) => {}
                            Ok(Some(deadline)) => {
                                global_shutdown_deadline = Some(deadline);
                                break;
                            }
                            Err(error) => {
                                result = Err(error);
                                break;
                            }
                        }
                        continue;
                    }
                    let runtime = Arc::clone(&runtime);
                    let events = Arc::clone(&events);
                    let evidence = evidence.clone();
                    let requests = Arc::clone(&requests);
                    let mut request_connection = connection.clone();
                    let request_id = request.id.clone();
                    tasks.spawn(async move {
                        let registration = RequestRegistration::new(requests.clone(), request_id);
                        let response = dispatch_routed_with_evidence(
                            request,
                            runtime.as_ref(),
                            DispatchResources {
                                events: events.as_ref(),
                                evidence: &evidence,
                                streams: None,
                            },
                            &mut request_connection,
                            &control,
                            requests.as_ref(),
                            Some(admitted_route),
                        )
                        .await;
                        registration.mark_completed();
                        (response, registration)
                    });
                } else {
                    let (controller, control) = request_control(request.timeout_ms);
                    let dispatch = dispatch_controlled_with_evidence(
                        request,
                        runtime.as_ref(),
                        DispatchResources {
                            events: events.as_ref(),
                            evidence: &evidence,
                            streams: None,
                        },
                        &mut connection,
                        &control,
                        requests.as_ref(),
                    );
                    let response = match dispatch_inline_until_shutdown(
                        dispatch,
                        &controller,
                        &mut shutdown,
                    )
                    .await
                    {
                        InlineDispatchOutcome::Response(response) => response,
                        InlineDispatchOutcome::Shutdown(deadline) => {
                            global_shutdown_deadline = Some(deadline);
                            break;
                        }
                    };
                    match write_loopback_response(&mut write, response, &mut shutdown).await {
                        Ok(None) => {}
                        Ok(Some(deadline)) => {
                            global_shutdown_deadline = Some(deadline);
                            break;
                        }
                        Err(error) => {
                            result = Err(error);
                            break;
                        }
                    }
                }
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok((response, registration))) => {
                        let write_result =
                            write_loopback_response(&mut write, response, &mut shutdown).await;
                        drop(registration);
                        match write_result {
                            Ok(None) => {}
                            Ok(Some(deadline)) => {
                                global_shutdown_deadline = Some(deadline);
                                break;
                            }
                            Err(error) => {
                                result = Err(error);
                                break;
                            }
                        }
                    }
                    Some(Err(error)) => {
                        result = Err(std::io::Error::other(format!("request task failed: {error}")));
                        break;
                    }
                    None => {}
                }
            }
            deadline = receive_loopback_shutdown(&mut shutdown) => {
                global_shutdown_deadline = Some(deadline);
                break;
            }
        }
    }

    requests.cancel_all(CancellationReason::Shutdown);
    let request_deadline = global_shutdown_deadline.map_or_else(
        || Instant::now() + SHUTDOWN_GRACE,
        |deadline| {
            deadline
                .checked_sub(CONNECTION_CLEANUP_RESERVE)
                .unwrap_or(deadline)
        },
    );
    let drain_result = drain_loopback_requests(
        &mut tasks,
        &mut write,
        request_deadline,
        global_shutdown_deadline.is_some(),
    )
    .await;
    let cleanup_result = cleanup_connection(
        runtime.as_ref(),
        events.as_ref(),
        &mut connection,
        if global_shutdown_deadline.is_some() {
            "daemon shutdown"
        } else {
            "RPC connection closed"
        },
    )
    .await;
    if result.is_ok() {
        result = drain_result;
    }
    if result.is_ok() {
        result = cleanup_result;
    }
    result
}

enum InlineDispatchOutcome {
    Response(RpcResponse),
    Shutdown(Instant),
}

async fn dispatch_inline_until_shutdown<F>(
    dispatch: F,
    controller: &ExecutionController,
    shutdown: &mut watch::Receiver<Option<Instant>>,
) -> InlineDispatchOutcome
where
    F: std::future::Future<Output = RpcResponse>,
{
    tokio::pin!(dispatch);
    tokio::select! {
        biased;
        deadline = receive_loopback_shutdown(shutdown) => {
            controller.cancel(CancellationReason::Shutdown);
            InlineDispatchOutcome::Shutdown(deadline)
        }
        response = &mut dispatch => InlineDispatchOutcome::Response(response),
    }
}

async fn receive_loopback_shutdown(shutdown: &mut watch::Receiver<Option<Instant>>) -> Instant {
    loop {
        if let Some(deadline) = *shutdown.borrow_and_update() {
            return deadline;
        }
        if shutdown.changed().await.is_err() {
            // A dropped sender is also a shutdown request. This branch is a
            // fail-safe; the listener normally broadcasts an absolute deadline.
            return Instant::now() + SHUTDOWN_GRACE;
        }
    }
}

async fn write_loopback_response<W>(
    write: &mut W,
    response: RpcResponse,
    shutdown: &mut watch::Receiver<Option<Instant>>,
) -> std::io::Result<Option<Instant>>
where
    W: AsyncWrite + Unpin,
{
    tokio::select! {
        result = write_rpc_response(write, response) => result.map(|()| None),
        deadline = receive_loopback_shutdown(shutdown) => Ok(Some(deadline)),
    }
}

async fn drain_loopback_requests<W>(
    tasks: &mut JoinSet<(RpcResponse, RequestRegistration)>,
    write: &mut W,
    deadline: Instant,
    mut write_responses: bool,
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut first_error = None;
    while !tasks.is_empty() {
        match timeout_at(deadline, tasks.join_next()).await {
            Ok(Some(Ok((response, registration)))) => {
                if write_responses {
                    match timeout_at(deadline, write_rpc_response(write, response)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                            write_responses = false;
                        }
                        Err(_) => {
                            if first_error.is_none() {
                                first_error = Some(std::io::Error::other(
                                    "response write timed out during connection shutdown",
                                ));
                            }
                            write_responses = false;
                        }
                    }
                }
                drop(registration);
            }
            Ok(Some(Err(error))) => {
                if first_error.is_none() {
                    first_error = Some(std::io::Error::other(format!(
                        "request task failed during connection shutdown: {error}"
                    )));
                }
            }
            Ok(None) => break,
            Err(_) => {
                tasks.abort_all();
                // Dropping an aborted JoinSet is non-blocking. Waiting for a
                // non-cooperative task here would consume the cleanup reserve
                // and prevent this connection from releasing its owner lease.
                drop(std::mem::replace(tasks, JoinSet::new()));
                if first_error.is_none() {
                    first_error = Some(std::io::Error::other(
                        "request shutdown timed out after cancellation",
                    ));
                }
                break;
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn drain_loopback_connections(
    connections: &mut JoinSet<std::io::Result<()>>,
    deadline: Instant,
) -> std::io::Result<()> {
    let mut first_error = None;
    while !connections.is_empty() {
        match timeout_at(deadline, connections.join_next()).await {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => {
                eprintln!(
                    "DeviceRail loopback client closed with an error ({})",
                    bounded_diagnostic(&error.to_string())
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Ok(Some(Err(error))) => {
                eprintln!(
                    "DeviceRail loopback client task failed ({})",
                    bounded_diagnostic(&error.to_string())
                );
                if first_error.is_none() {
                    first_error = Some(std::io::Error::other(format!(
                        "loopback client task failed: {error}"
                    )));
                }
            }
            Ok(None) => break,
            Err(_) => {
                connections.abort_all();
                // The global deadline is authoritative: abort and detach
                // stragglers instead of turning the bounded grace into an
                // unbounded JoinHandle wait.
                drop(std::mem::replace(connections, JoinSet::new()));
                return Err(std::io::Error::other(
                    "loopback client shutdown timed out after cancellation",
                ));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn cleanup_connection(
    runtime: &Registry,
    events: &MemoryEventStore,
    connection: &mut ConnectionState,
    reason: &str,
) -> std::io::Result<()> {
    let Some(context) = connection.context() else {
        return Ok(());
    };
    let owner_id = LeaseOwnerId::new(context.hello.connection_id);
    let active_session = context.active_session.clone();
    let event_device_id = session_event_device_id(connection);
    let finalization = if let Some(session_id) = active_session {
        match abort_media_streams(connection, MEDIA_STREAM_CLOSE_GRACE, None).await {
            Ok(()) => events
                .end_session(EndSession {
                    session_id,
                    request_id: None,
                    device_id: event_device_id,
                    at_ms: now_ms(),
                    outcome: SessionOutcome::Shutdown,
                    reason: Some(reason.to_owned()),
                })
                .await
                .map(|_| ())
                .map_err(|error| {
                    std::io::Error::other(format!("failed to end RPC connection session: {error}"))
                }),
            Err(error) => Err(std::io::Error::other(format!(
                "failed to close RPC connection media streams: {}",
                error.data.code
            ))),
        }
    } else {
        Ok(())
    };
    // The transport connection is gone even when Session finalization fails;
    // never strand its device lease behind a failed media append.
    runtime.release_owner_leases(owner_id, now_ms()).await;
    finalization?;
    if let Some(context) = connection.context_mut() {
        context.active_session = None;
        context.device_lease = None;
        context.media_streams.clear();
    }
    Ok(())
}

async fn read_bounded_async_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::new();
    let read = reader
        .take((max_bytes as u64).saturating_add(2))
        .read_until(b'\n', &mut bytes)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("NDJSON frame exceeds the {max_bytes}-byte limit"),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("NDJSON frame is not valid UTF-8: {error}"),
        )
    })
}

async fn write_rpc_response<W>(writer: &mut W, response: RpcResponse) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let frame = bounded_response_frame(response, MAX_FRAME_BYTES)?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

fn stdin_lines() -> std::io::Result<mpsc::Receiver<std::io::Result<String>>> {
    let (lines, receiver) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("devicerail-stdin".to_owned())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            loop {
                match read_bounded_line(&mut reader, MAX_FRAME_BYTES) {
                    Ok(Some(line)) => {
                        if lines.blocking_send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = lines.blocking_send(Err(error));
                        break;
                    }
                }
            }
        })
        .map(|_| receiver)
}

fn read_bounded_line<R>(reader: &mut R, max_bytes: usize) -> std::io::Result<Option<String>>
where
    R: std::io::BufRead,
{
    let mut bytes = Vec::new();
    let read = reader
        .take((max_bytes as u64).saturating_add(2))
        .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("NDJSON frame exceeds the {max_bytes}-byte limit"),
        ));
    }
    String::from_utf8(bytes).map(Some).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("NDJSON frame is not valid UTF-8: {error}"),
        )
    })
}

type ResponseFrame = Vec<u8>;
type ResponseSender = std_mpsc::SyncSender<ResponseFrame>;

fn stdout_writer() -> std::io::Result<(ResponseSender, oneshot::Receiver<std::io::Result<()>>)> {
    let (responses, queue): (ResponseSender, std_mpsc::Receiver<ResponseFrame>) =
        std_mpsc::sync_channel(RESPONSE_QUEUE_CAPACITY);
    let (completed, completion) = oneshot::channel();
    std::thread::Builder::new()
        .name("devicerail-stdout".to_owned())
        .spawn(move || {
            let result = (|| {
                let stdout = std::io::stdout();
                let mut writer = stdout.lock();
                for frame in queue {
                    writer.write_all(&frame)?;
                    writer.flush()?;
                }
                writer.flush()
            })();
            let _ = completed.send(result);
        })?;
    Ok((responses, completion))
}

fn queue_response(responses: &ResponseSender, response: RpcResponse) -> std::io::Result<()> {
    let frame = bounded_response_frame(response, MAX_FRAME_BYTES)?;
    responses.try_send(frame).map_err(|error| match error {
        std_mpsc::TrySendError::Full(_) => std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "response queue is full; the client must continuously read daemon output",
        ),
        std_mpsc::TrySendError::Disconnected(_) => {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "response writer stopped")
        }
    })
}

fn bounded_response_frame(
    response: RpcResponse,
    max_bytes: usize,
) -> std::io::Result<ResponseFrame> {
    let response_id = match &response {
        RpcResponse::Success { id, .. } => Some(id.clone()),
        RpcResponse::Failure { id, .. } => id.clone(),
    };
    let (encoded, actual_bytes) = serialize_response_bounded(&response, max_bytes)?;
    if actual_bytes <= max_bytes {
        return append_ndjson_delimiter(encoded);
    }

    let replacement = RpcResponse::failure(
        response_id,
        response_frame_too_large(actual_bytes, max_bytes),
    );
    let (encoded, replacement_bytes) = serialize_response_bounded(&replacement, max_bytes)?;
    if replacement_bytes > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "response_frame_too_large replacement is {} bytes and exceeds the {max_bytes}-byte limit",
                replacement_bytes
            ),
        ));
    }
    append_ndjson_delimiter(encoded)
}

struct BoundedResponseWriter {
    bytes: Vec<u8>,
    limit: usize,
    total: usize,
}

impl BoundedResponseWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            total: 0,
        }
    }
}

impl std::io::Write for BoundedResponseWriter {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.total = self.total.checked_add(input.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized response length overflowed usize",
            )
        })?;
        let retained = self.limit.saturating_sub(self.bytes.len()).min(input.len());
        if retained > 0 {
            self.bytes.try_reserve(retained).map_err(|error| {
                std::io::Error::other(format!(
                    "failed to allocate bounded response frame: {error}"
                ))
            })?;
            self.bytes.extend_from_slice(&input[..retained]);
        }
        // Bytes beyond the cap are intentionally counted and discarded. This
        // preserves exact error details without materializing an oversized
        // response before applying the wire limit.
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize_response_bounded(
    response: &RpcResponse,
    max_bytes: usize,
) -> std::io::Result<(ResponseFrame, usize)> {
    let mut writer = BoundedResponseWriter::new(max_bytes);
    serde_json::to_writer(&mut writer, response).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("response serialization failed: {error}"),
        )
    })?;
    Ok((writer.bytes, writer.total))
}

fn append_ndjson_delimiter(mut encoded: ResponseFrame) -> std::io::Result<ResponseFrame> {
    encoded.try_reserve(1).map_err(|error| {
        std::io::Error::other(format!(
            "failed to allocate NDJSON response delimiter: {error}"
        ))
    })?;
    encoded.push(b'\n');
    Ok(encoded)
}

async fn drain_requests(
    requests: &mut JoinSet<(RpcResponse, RequestRegistration)>,
    responses: &ResponseSender,
) -> std::io::Result<()> {
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    let mut first_error = None;
    while !requests.is_empty() {
        match timeout_at(deadline, requests.join_next()).await {
            Ok(Some(Ok((response, registration)))) => {
                if let Err(error) = queue_response(responses, response)
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
                drop(registration);
            }
            Ok(Some(Err(error))) => {
                if first_error.is_none() {
                    first_error = Some(std::io::Error::other(format!(
                        "request task failed during shutdown: {error}"
                    )));
                }
            }
            Ok(None) => break,
            Err(_) => {
                requests.abort_all();
                while requests.join_next().await.is_some() {}
                return Err(std::io::Error::other(
                    "request shutdown timed out after cancellation",
                ));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn shutdown_runtime(
    runtime: &Registry,
    events: &MemoryEventStore,
    connection: &mut ConnectionState,
) -> Result<(), Box<dyn std::error::Error>> {
    shutdown_runtime_with_grace(runtime, events, connection, SHUTDOWN_GRACE).await
}

async fn shutdown_runtime_with_grace(
    runtime: &Registry,
    events: &MemoryEventStore,
    connection: &mut ConnectionState,
    grace: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let active_session = connection
        .context()
        .and_then(|context| context.active_session.clone());
    let shutdown_device_id = session_event_device_id(connection);
    let session_result = if let Some(session_id) = active_session {
        match abort_media_streams(connection, grace, None).await {
            Ok(()) => events
                .end_session(EndSession {
                    session_id,
                    request_id: None,
                    device_id: shutdown_device_id,
                    at_ms: now_ms(),
                    outcome: SessionOutcome::Shutdown,
                    reason: Some("daemon shutdown".to_owned()),
                })
                .await
                .map(|_| ())
                .map_err(|error| error.to_string()),
            Err(error) => Err(format!("media streams: {}", error.data.code)),
        }
    } else {
        Ok(())
    };
    let disconnect_deadline = Instant::now() + grace;
    let mut disconnects = JoinSet::new();
    let mut disconnect_errors = Vec::new();
    let shutdown_owner = connection
        .context()
        .map(|context| LeaseOwnerId::new(context.hello.connection_id))
        .unwrap_or_else(|| LeaseOwnerId::new(Uuid::nil()));
    for handle in runtime.handles().await {
        match runtime
            .access_available_to(handle, shutdown_owner, now_ms())
            .await
        {
            Ok(access) => {
                disconnects.spawn(async move {
                    let device_id = access.id().clone();
                    let (_, control) = ExecutionController::with_timeout(
                        grace.as_millis() as u64,
                        TimeoutScope::Shutdown,
                    );
                    (device_id, access.disconnect(&control).await)
                });
            }
            Err(error) => disconnect_errors.push(format!("device pool: {error}")),
        }
    }
    while !disconnects.is_empty() {
        match timeout_at(disconnect_deadline, disconnects.join_next()).await {
            Ok(Some(Ok((_, Ok(()))))) => {}
            Ok(Some(Ok((device_id, Err(error))))) => {
                disconnect_errors.push(format!("{device_id}: {error}"));
            }
            Ok(Some(Err(error))) => {
                disconnect_errors.push(format!("disconnect task: {error}"));
            }
            Ok(None) => break,
            Err(_) => {
                disconnects.abort_all();
                while disconnects.join_next().await.is_some() {}
                disconnect_errors.push(format!(
                    "disconnect phase timed out after {} ms",
                    grace.as_millis()
                ));
                break;
            }
        }
    }
    if session_result.is_ok() {
        runtime.release_owner_leases(shutdown_owner, now_ms()).await;
        if let Some(context) = connection.context_mut() {
            context.device_lease = None;
        }
    }
    match session_result {
        Ok(()) => {
            if let Some(context) = connection.context_mut() {
                context.active_session = None;
                context.media_streams.clear();
            }
        }
        Err(error) => disconnect_errors.push(format!("session end: {error}")),
    }
    disconnect_errors.sort();
    if !disconnect_errors.is_empty() {
        return Err(std::io::Error::other(format!(
            "one or more shutdown operations failed: {}",
            disconnect_errors.join("; ")
        ))
        .into());
    }
    Ok(())
}

fn request_control(
    timeout_ms: Option<RequestTimeoutMs>,
) -> (ExecutionController, ExecutionControl) {
    timeout_ms.map_or_else(ExecutionController::new, |timeout_ms| {
        ExecutionController::with_timeout(timeout_ms.get(), TimeoutScope::Request)
    })
}

async fn lazily_select_sole(connection: &mut ConnectionState, registry: &Registry) {
    let needs_selection = connection
        .context()
        .is_some_and(|context| context.selected_device_id.is_none());
    if !needs_selection {
        return;
    }
    if let Ok(handle) = registry.sole().await
        && let Some(context) = connection.context_mut()
    {
        context.selected_device_id = Some(handle.id().clone());
    }
}

async fn selected_route(
    connection: &ConnectionState,
    registry: &Registry,
) -> Result<DeviceRoute, RpcError> {
    if let Some(device_id) = connection
        .context()
        .and_then(|context| context.selected_device_id.as_ref())
    {
        return registry.resolve(device_id).await.map_err(registry_error);
    }

    let handles = registry.handles().await;
    match handles.as_slice() {
        [] => Err(rpc_error(
            DEVICE_ROUTING_ERROR,
            "device_not_found",
            "no devices are registered",
            true,
            Some(json!({ "reason": "noRegisteredDevices" })),
        )),
        [handle] => Ok(handle.clone()),
        _ => Err(device_selection_required(&handles)),
    }
}

async fn request_route(
    admitted_route: &mut Option<Result<DeviceRoute, RpcError>>,
    connection: &ConnectionState,
    registry: &Registry,
) -> Result<DeviceRoute, RpcError> {
    match admitted_route.take() {
        Some(route) => route,
        None => selected_route(connection, registry).await,
    }
}

async fn authorize_leased_route(
    connection: &ConnectionState,
    registry: &Registry,
    route: DeviceRoute,
    control: &ExecutionControl,
) -> Result<DeviceAccess, RpcError> {
    let context = connection.context().ok_or_else(|| {
        rpc_error(
            HANDSHAKE_REQUIRED,
            "handshake_required",
            "system.hello must succeed before any device operation",
            true,
            None,
        )
    })?;
    let owner_id = LeaseOwnerId::new(context.hello.connection_id);
    let lease = context.device_lease.as_ref().ok_or_else(session_required)?;
    probe_and_record_health(registry, &route, control).await?;
    registry
        .access_with_lease(route, lease.id, owner_id, now_ms())
        .await
        .map_err(device_pool_error)
}

async fn probe_and_record_health(
    registry: &Registry,
    route: &DeviceRoute,
    control: &ExecutionControl,
) -> Result<(), RpcError> {
    let checked_at_ms = now_ms();
    match route.health_check(control).await {
        Ok(()) => registry
            .record_health(route, PoolHealth::healthy(checked_at_ms), checked_at_ms)
            .await
            .map(|_| ())
            .map_err(device_pool_error),
        Err(error) => {
            let code = error.to_error_info().code;
            let health = PoolHealth::unhealthy(checked_at_ms, code).map_err(device_pool_error)?;
            registry
                .record_health(route, health, checked_at_ms)
                .await
                .map_err(device_pool_error)?;
            Err(runtime_error(error))
        }
    }
}

async fn connect_route(
    connection: &ConnectionState,
    registry: &Registry,
    route: DeviceRoute,
    control: &ExecutionControl,
) -> RpcResult {
    probe_and_record_health(registry, &route, control).await?;
    let context = connection
        .context()
        .expect("handshake checked before dispatch");
    let owner_id = LeaseOwnerId::new(context.hello.connection_id);
    match &context.device_lease {
        Some(lease) => {
            let access = registry
                .access_with_lease(route, lease.id, owner_id, now_ms())
                .await
                .map_err(device_pool_error)?;
            serialize_runtime_result(access.connect(control).await)
        }
        None => {
            let access = registry
                .access_available_to(route, owner_id, now_ms())
                .await
                .map_err(device_pool_error)?;
            serialize_runtime_result(access.connect(control).await)
        }
    }
}

async fn disconnect_route(
    connection: &ConnectionState,
    registry: &Registry,
    route: DeviceRoute,
    control: &ExecutionControl,
) -> RpcResult {
    let context = connection
        .context()
        .expect("handshake checked before dispatch");
    let owner_id = LeaseOwnerId::new(context.hello.connection_id);
    let result = match &context.device_lease {
        Some(lease) => {
            let access = registry
                .cleanup_access_with_lease(route, lease.id, owner_id, now_ms())
                .await
                .map_err(device_pool_error)?;
            access.disconnect(control).await
        }
        None => {
            let access = registry
                .access_available_to(route, owner_id, now_ms())
                .await
                .map_err(device_pool_error)?;
            access.disconnect(control).await
        }
    };
    serialize_runtime_result(result.map(|_| DeviceDisconnectResult { disconnected: true }))
}

fn connection_owner(connection: &ConnectionState) -> Result<LeaseOwnerId, RpcError> {
    connection
        .context()
        .map(|context| LeaseOwnerId::new(context.hello.connection_id))
        .ok_or_else(|| {
            rpc_error(
                HANDSHAKE_REQUIRED,
                "handshake_required",
                "system.hello must succeed before acquiring a device lease",
                true,
                None,
            )
        })
}

fn session_event_device_id(connection: &ConnectionState) -> Option<DeviceId> {
    connection.context().and_then(|context| {
        feature_enabled(&context.hello, feature::DEVICE_ROUTING_V1).then(|| {
            context
                .device_lease
                .as_ref()
                .map(|lease| lease.device_id.clone())
                .or_else(|| context.selected_device_id.clone())
        })
    })?
}

fn device_selection_required(handles: &[DeviceRoute]) -> RpcError {
    rpc_error(
        DEVICE_ROUTING_ERROR,
        "device_selection_required",
        "select a device before calling device methods",
        true,
        Some(json!({
            "deviceCount": handles.len(),
            "availableDeviceIds": handles.iter().map(DriverHandle::id).collect::<Vec<_>>(),
            "requiredMethod": "device.select",
            "requiredFeature": feature::DEVICE_ROUTING_V1
        })),
    )
}

fn method_runs_concurrently(method: &str) -> bool {
    matches!(
        method,
        "device.connect"
            | "device.disconnect"
            | "device.capabilities"
            | "device.observe"
            | "device.execute"
            | "media.stream.capture"
    )
}

#[cfg(unix)]
async fn shutdown_signal() -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        signal = terminate.recv() => signal.map_or_else(
            || Err(std::io::Error::other("SIGTERM stream closed")),
            |_| Ok(())
        ),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

fn decode_request(line: &str) -> Result<RpcRequest, Box<RpcResponse>> {
    serde_json::from_str::<RpcRequest>(line).map_err(|error| {
        let (numeric_code, code, message, details) = match error.classify() {
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof => (
                PARSE_ERROR,
                "parse_error",
                "request is not valid JSON",
                Some(json!({ "line": error.line(), "column": error.column() })),
            ),
            serde_json::error::Category::Data | serde_json::error::Category::Io => (
                INVALID_REQUEST,
                "invalid_request",
                "request does not match the DeviceRail JSON-RPC subset",
                None,
            ),
        };
        Box::new(RpcResponse::failure(
            None,
            rpc_error(numeric_code, code, message, false, details),
        ))
    })
}

#[cfg(test)]
async fn dispatch(
    request: RpcRequest,
    runtime: &Registry,
    events: &MemoryEventStore,
    connection: &mut ConnectionState,
) -> RpcResponse {
    let (_, control) = request_control(request.timeout_ms);
    dispatch_controlled(
        request,
        runtime,
        events,
        connection,
        &control,
        &RequestRegistry::default(),
    )
    .await
}

#[cfg(test)]
async fn dispatch_controlled(
    request: RpcRequest,
    runtime: &Registry,
    events: &MemoryEventStore,
    connection: &mut ConnectionState,
    control: &ExecutionControl,
    registry: &RequestRegistry,
) -> RpcResponse {
    dispatch_controlled_with_evidence(
        request,
        runtime,
        DispatchResources {
            events,
            evidence: &EvidenceCleanup::Disabled,
            streams: None,
        },
        connection,
        control,
        registry,
    )
    .await
}

async fn dispatch_controlled_with_evidence(
    request: RpcRequest,
    runtime: &Registry,
    resources: DispatchResources<'_>,
    connection: &mut ConnectionState,
    control: &ExecutionControl,
    registry: &RequestRegistry,
) -> RpcResponse {
    dispatch_routed_with_evidence(
        request, runtime, resources, connection, control, registry, None,
    )
    .await
}

#[cfg(test)]
async fn dispatch_routed(
    request: RpcRequest,
    runtime: &Registry,
    events: &MemoryEventStore,
    connection: &mut ConnectionState,
    control: &ExecutionControl,
    registry: &RequestRegistry,
    admitted_route: Option<Result<DeviceRoute, RpcError>>,
) -> RpcResponse {
    dispatch_routed_with_evidence(
        request,
        runtime,
        DispatchResources {
            events,
            evidence: &EvidenceCleanup::Disabled,
            streams: None,
        },
        connection,
        control,
        registry,
        admitted_route,
    )
    .await
}

async fn dispatch_routed_with_evidence(
    request: RpcRequest,
    runtime: &Registry,
    resources: DispatchResources<'_>,
    connection: &mut ConnectionState,
    control: &ExecutionControl,
    registry: &RequestRegistry,
    mut admitted_route: Option<Result<DeviceRoute, RpcError>>,
) -> RpcResponse {
    let DispatchResources {
        events,
        evidence,
        streams,
    } = resources;
    let RpcRequest {
        id,
        method,
        timeout_ms,
        params,
        ..
    } = request;
    let request_id = id.clone();

    if method == "system.hello" {
        if timeout_ms.is_some() {
            return RpcResponse::failure(Some(id), request_control_not_negotiated("timeoutMs"));
        }
        let event_stream_available = streams.is_some_and(EventStreamServer::is_accepting);
        let evidence_store_available = evidence.managed_store().is_some();
        let media_producer_available =
            evidence_store_available && runtime.screenshot_policy() == ScreenshotPolicy::Capture;
        return match negotiate_connection(
            params,
            connection,
            event_stream_available,
            media_producer_available,
            evidence_store_available,
        ) {
            Ok(value) => RpcResponse::success(id, value),
            Err(error) => RpcResponse::failure(Some(id), error),
        };
    }

    if connection.context().is_none() {
        return RpcResponse::failure(
            Some(id),
            rpc_error(
                HANDSHAKE_REQUIRED,
                "handshake_required",
                "system.hello must succeed before any other method",
                true,
                Some(json!({ "requiredMethod": "system.hello" })),
            ),
        );
    }

    if (admitted_route.is_none() && method_runs_concurrently(&method))
        || method == "system.describe"
    {
        lazily_select_sole(connection, runtime).await;
    }
    let context = connection
        .context()
        .expect("handshake state was checked before dispatch");

    if method == "request.cancel" && !feature_enabled(&context.hello, feature::REQUEST_CONTROL_V1) {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::REQUEST_CONTROL_V1)),
        );
    }

    if matches!(
        method.as_str(),
        "events.list" | "events.clear" | "session.export" | "sessions.list"
    ) && !feature_enabled(&context.hello, feature::EVENTS_SNAPSHOT_V1)
    {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::EVENTS_SNAPSHOT_V1)),
        );
    }

    if method == "events.stream.open" && !feature_enabled(&context.hello, feature::EVENTS_STREAM_V1)
    {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::EVENTS_STREAM_V1)),
        );
    }

    if matches!(
        method.as_str(),
        "media.stream.start" | "media.stream.capture" | "media.stream.end"
    ) && !feature_enabled(&context.hello, feature::MEDIA_STREAM_V1)
    {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::MEDIA_STREAM_V1)),
        );
    }

    if matches!(method.as_str(), "devices.list" | "device.select")
        && !feature_enabled(&context.hello, feature::DEVICE_ROUTING_V1)
    {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::DEVICE_ROUTING_V1)),
        );
    }

    if method == "ui.snapshot.get"
        && !feature_enabled(&context.hello, feature::OBSERVATION_UI_SNAPSHOT_V1)
    {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::OBSERVATION_UI_SNAPSHOT_V1)),
        );
    }

    if method == "verdict.record" && !feature_enabled(&context.hello, feature::VERDICT_RECORD_V1) {
        return RpcResponse::failure(
            Some(id),
            method_unavailable(&method, Some(feature::VERDICT_RECORD_V1)),
        );
    }

    if !known_method(&method) {
        return RpcResponse::failure(Some(id), method_unavailable(&method, None));
    }

    if timeout_ms.is_some() && !feature_enabled(&context.hello, feature::REQUEST_CONTROL_V1) {
        return RpcResponse::failure(Some(id), request_control_not_negotiated("timeoutMs"));
    }

    if timeout_ms.is_some() && !method_runs_concurrently(&method) {
        return RpcResponse::failure(Some(id), request_timeout_not_supported(&method));
    }

    if let Some(error) = inactive_request_error(control) {
        return RpcResponse::failure(Some(id), error);
    }

    if method_takes_no_params(&method) {
        if let Err(error) = validate_no_params(params.as_ref(), &method) {
            return RpcResponse::failure(Some(id), error);
        }
    }

    let result = match method.as_str() {
        "system.describe" => to_json_value(SystemDescribeResult {
            connection: context.hello.clone(),
            client: context.client.clone(),
            device_id: context.selected_device_id.clone(),
            active_session_id: context.active_session.clone(),
        }),
        "devices.list" => to_json_value(DevicesListResult {
            devices: runtime.list().await,
            selected_device_id: context.selected_device_id.clone(),
        }),
        "device.select" => select_device(params, connection, runtime).await,
        "device.connect" => match request_route(&mut admitted_route, connection, runtime).await {
            Ok(route) => connect_route(connection, runtime, route, control).await,
            Err(error) => Err(error),
        },
        "device.disconnect" => {
            match request_route(&mut admitted_route, connection, runtime).await {
                Ok(route) => disconnect_route(connection, runtime, route, control).await,
                Err(error) => Err(error),
            }
        }
        "device.capabilities" => {
            match request_route(&mut admitted_route, connection, runtime).await {
                Ok(route) => serialize_runtime_result(route.capabilities(control).await.map(
                    |mut capabilities| {
                        if !feature_enabled(&context.hello, feature::ACTION_PROTECTED_V1) {
                            capabilities.retain(|definition| {
                                definition.protection != ActionProtection::Protected
                            });
                        }
                        if !feature_enabled(&context.hello, feature::DEVICE_SEMANTIC_ACTIONS_V1) {
                            capabilities
                                .retain(|definition| !is_semantic_action_name(&definition.name));
                        }
                        capabilities
                    },
                )),
                Err(error) => Err(error),
            }
        }
        "device.observe" => match request_route(&mut admitted_route, connection, runtime).await {
            Ok(route) => match authorize_leased_route(connection, runtime, route, control).await {
                Ok(access) => {
                    match active_operation_context(connection, request_id.clone(), control.clone())
                    {
                        Ok(operation) => serialize_runtime_result(access.observe(&operation).await),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        },
        "device.execute" => match serde_json::from_value::<DeviceExecuteParams>(
            params.map(RpcParams::into_value).unwrap_or(Value::Null),
        ) {
            Ok(execute) => {
                if execute.action_timeout_ms.is_some()
                    && !feature_enabled(&context.hello, feature::REQUEST_CONTROL_V1)
                {
                    Err(request_control_not_negotiated("actionTimeoutMs"))
                } else if is_semantic_action_name(&execute.name)
                    && !feature_enabled(&context.hello, feature::DEVICE_SEMANTIC_ACTIONS_V1)
                {
                    Err(semantic_action_not_negotiated(&execute.name))
                } else {
                    let action_timeout_ms = execute.action_timeout_ms.map(RequestTimeoutMs::get);
                    match request_route(&mut admitted_route, connection, runtime).await {
                        Ok(route)
                            if route.action_protection(&execute.name)
                                == Some(ActionProtection::Protected)
                                && !feature_enabled(
                                    &context.hello,
                                    feature::ACTION_PROTECTED_V1,
                                ) =>
                        {
                            Err(protected_action_not_negotiated(&execute.name))
                        }
                        Ok(route) => {
                            let sensitive_admission = if route.action_protection(&execute.name)
                                != Some(ActionProtection::Standard)
                            {
                                context.media_streams.sensitive_action().map(Some)
                            } else {
                                Ok(None)
                            };
                            match sensitive_admission {
                                Err(error) => Err(error),
                                Ok(_sensitive_admission) => {
                                    match authorize_leased_route(
                                        connection, runtime, route, control,
                                    )
                                    .await
                                    {
                                        Ok(access) => {
                                            match active_operation_context(
                                                connection,
                                                request_id.clone(),
                                                control.clone(),
                                            ) {
                                                Ok(operation) => {
                                                    let operation = action_timeout_ms.map_or(
                                                        operation.clone(),
                                                        |timeout_ms| {
                                                            operation
                                                                .with_action_timeout_ms(timeout_ms)
                                                        },
                                                    );
                                                    serialize_runtime_result(
                                                        access
                                                            .execute(
                                                                &operation,
                                                                execute.into_action_call(),
                                                            )
                                                            .await,
                                                    )
                                                }
                                                Err(error) => Err(error),
                                            }
                                        }
                                        Err(error) => Err(error),
                                    }
                                }
                            }
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            Err(_) => Err(rpc_error(
                INVALID_PARAMS,
                "invalid_params",
                "device.execute params are invalid",
                false,
                Some(json!({ "method": "device.execute" })),
            )),
        },
        "media.stream.start" => {
            start_media_stream(params, connection, runtime, evidence, request_id).await
        }
        "media.stream.capture" => {
            capture_media_stream(
                params,
                connection,
                runtime,
                request_id,
                control,
                &mut admitted_route,
            )
            .await
        }
        "media.stream.end" => end_media_stream(params, connection, request_id).await,
        "request.cancel" => cancel_request(params, registry),
        "events.stream.open" => match serde_json::from_value::<EventsStreamOpenParams>(
            params.map(RpcParams::into_value).unwrap_or(Value::Null),
        ) {
            Ok(params) => match streams {
                Some(streams) => streams
                    .open(params)
                    .map_err(stream_transport_error)
                    .and_then(to_json_value),
                None => Err(method_unavailable(
                    "events.stream.open",
                    Some(feature::EVENTS_STREAM_V1),
                )),
            },
            Err(_) => Err(rpc_error(
                INVALID_PARAMS,
                "invalid_params",
                "events.stream.open params are invalid",
                false,
                Some(json!({ "method": "events.stream.open" })),
            )),
        },
        "session.start" => start_session(connection, runtime, events, request_id, control).await,
        "session.current" => current_session(connection),
        "session.end" => end_session(connection, runtime, events, request_id, params).await,
        "sessions.list" => serialize_event_store_result(events.list_sessions().await),
        "session.export" => {
            match decode_params_or_default::<SessionExportParams>(params, "session.export") {
                Ok(query) if query.after_sequence.is_some() && query.limit.is_none() => {
                    Err(rpc_error(
                        INVALID_PARAMS,
                        "invalid_params",
                        "session.export afterSequence requires limit",
                        false,
                        Some(json!({ "method": "session.export" })),
                    ))
                }
                Ok(query) => {
                    if query.limit.is_some()
                        && !feature_enabled(&context.hello, feature::SESSION_EXPORT_PAGE_V1)
                    {
                        Err(method_unavailable(
                            "session.export",
                            Some(feature::SESSION_EXPORT_PAGE_V1),
                        ))
                    } else {
                        let session_id = query.session_id.or_else(|| {
                            connection
                                .context()
                                .and_then(|value| value.active_session.clone())
                        });
                        match session_id {
                            None => Err(session_required()),
                            Some(session_id) => match query.limit {
                                Some(limit) => match events
                                    .export_session_page(
                                        &session_id,
                                        query.after_sequence,
                                        limit as usize,
                                    )
                                    .await
                                {
                                    Ok(snapshot) => {
                                        match ensure_events_protocol_compatible(
                                            snapshot.events.iter().map(AsRef::as_ref),
                                            context.hello.protocol.selected,
                                            "session.export",
                                            &session_id,
                                        ) {
                                            Ok(()) => bounded_session_export_page_value(
                                                &id,
                                                snapshot,
                                                MAX_FRAME_BYTES,
                                            ),
                                            Err(error) => Err(error),
                                        }
                                    }
                                    Err(error) => Err(event_store_error(error)),
                                },
                                None => match events.export_session(&session_id).await {
                                    Ok(export) => {
                                        match ensure_events_protocol_compatible(
                                            &export.events,
                                            context.hello.protocol.selected,
                                            "session.export",
                                            &session_id,
                                        ) {
                                            Ok(()) => to_json_value(export),
                                            Err(error) => Err(error),
                                        }
                                    }
                                    Err(error) => Err(event_store_error(error)),
                                },
                            },
                        }
                    }
                }
                Err(error) => Err(error),
            }
        }
        "events.list" => {
            match decode_params_or_default::<EventsListParams>(params, "events.list") {
                Ok(query) => {
                    let session_id = query.session_id.or_else(|| {
                        connection
                            .context()
                            .and_then(|value| value.active_session.clone())
                    });
                    match session_id {
                        Some(session_id) => {
                            let result = match query.limit {
                                Some(limit) => {
                                    events
                                        .list_page(
                                            &session_id,
                                            query.after_sequence,
                                            limit as usize,
                                        )
                                        .await
                                }
                                None => events.list_after(&session_id, query.after_sequence).await,
                            };
                            match result {
                                Ok(events) => {
                                    match ensure_events_protocol_compatible(
                                        &events,
                                        context.hello.protocol.selected,
                                        "events.list",
                                        &session_id,
                                    ) {
                                        Ok(()) => to_json_value(events),
                                        Err(error) => Err(error),
                                    }
                                }
                                Err(error) => Err(event_store_error(error)),
                            }
                        }
                        None => Err(session_required()),
                    }
                }
                Err(error) => Err(error),
            }
        }
        "events.clear" => match session_target(params, connection, "events.clear") {
            Ok(session_id) => clear_ended_session(events, evidence, session_id).await,
            Err(error) => Err(error),
        },
        "ui.snapshot.get" => get_ui_snapshot(params, connection, events, evidence).await,
        "verdict.record" => record_verdict(params, connection, events, evidence, request_id).await,
        _ => Err(method_unavailable(&method, None)),
    };

    match result {
        Ok(value) => RpcResponse::success(id, value),
        Err(error) => RpcResponse::failure(Some(id), error),
    }
}

async fn get_ui_snapshot(
    params: Option<RpcParams>,
    connection: &ConnectionState,
    events: &MemoryEventStore,
    evidence: &EvidenceCleanup,
) -> RpcResult {
    let params = decode_required_params::<UiSnapshotGetParams>(params, "ui.snapshot.get")?;
    let session_id = connection
        .context()
        .and_then(|context| context.active_session.clone())
        .ok_or_else(session_required)?;
    let observation = events
        .observation_by_id(&session_id, params.observation_id)
        .await
        .map_err(event_store_error)?
        .ok_or_else(|| {
            ui_snapshot_error(
                "ui_snapshot_not_found",
                "the Observation does not exist in the active Session",
                false,
                Some(json!({ "observationId": params.observation_id })),
            )
        })?;
    let reference = observation.ui_snapshot.clone().ok_or_else(|| {
        ui_snapshot_error(
            "ui_snapshot_unavailable",
            "the Observation does not contain a UI snapshot",
            false,
            Some(json!({
                "observationId": params.observation_id,
                "omissionReason": observation.ui_snapshot_omission
            })),
        )
    })?;
    if reference.validate().is_err() {
        return Err(invalid_ui_snapshot(params.observation_id));
    }

    let store = evidence.managed_store().ok_or_else(|| {
        method_unavailable("ui.snapshot.get", Some(feature::OBSERVATION_UI_SNAPSHOT_V1))
    })?;
    let metadata = store
        .verify_session_reference(&session_id, &reference.evidence)
        .await
        .map_err(evidence_error)?;
    if metadata.media_type() != UI_SNAPSHOT_MEDIA_TYPE
        || metadata.byte_length() != reference.byte_length
        || metadata.byte_length() > MAX_UI_SNAPSHOT_BYTES
    {
        return Err(invalid_ui_snapshot(params.observation_id));
    }

    let declared_bytes = usize::try_from(reference.byte_length)
        .map_err(|_| invalid_ui_snapshot(params.observation_id))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(declared_bytes)
        .map_err(|_| invalid_ui_snapshot(params.observation_id))?;
    let reader = store
        .open(metadata.digest())
        .await
        .map_err(evidence_error)?;
    reader
        .take(MAX_UI_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| {
            ui_snapshot_error(
                "ui_snapshot_read_failed",
                "the UI snapshot could not be read",
                true,
                Some(json!({ "observationId": params.observation_id })),
            )
        })?;
    if bytes.len() != declared_bytes || bytes.len() > MAX_UI_SNAPSHOT_BYTES as usize {
        return Err(invalid_ui_snapshot(params.observation_id));
    }
    let snapshot = serde_json::from_slice::<UiSnapshot>(&bytes)
        .map_err(|_| invalid_ui_snapshot(params.observation_id))?;
    snapshot
        .validate_against(params.observation_id, &reference)
        .map_err(|_| invalid_ui_snapshot(params.observation_id))?;
    to_json_value(snapshot)
}

async fn record_verdict(
    params: Option<RpcParams>,
    connection: &ConnectionState,
    events: &MemoryEventStore,
    evidence: &EvidenceCleanup,
    request_id: RpcId,
) -> RpcResult {
    let params = decode_required_params::<VerdictRecordParams>(params, "verdict.record")?;
    let session_id = connection
        .context()
        .and_then(|context| context.active_session.clone())
        .ok_or_else(session_required)?;
    if params.verdict.validate().is_err() {
        return Err(rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            "verdict.record params exceed the supported bounds",
            false,
            Some(json!({
                "method": "verdict.record",
                "maximumSummaryLength": MAX_VERDICT_SUMMARY_LENGTH,
                "maximumEvidenceReferences": MAX_VERDICT_EVIDENCE_REFERENCES
            })),
        ));
    }

    let mut digests = BTreeSet::new();
    for asset in &params.verdict.evidence {
        let digest = Sha256Digest::from_asset_ref(asset).map_err(evidence_error)?;
        if !digests.insert(digest.clone()) {
            return Err(rpc_error(
                INVALID_PARAMS,
                "duplicate_evidence_reference",
                "verdict evidence references must be unique",
                false,
                Some(json!({ "digest": digest.as_str() })),
            ));
        }
    }
    if !params.verdict.evidence.is_empty() {
        let store = evidence.managed_store().ok_or_else(|| {
            method_unavailable("verdict.record", Some(feature::VERDICT_RECORD_V1))
        })?;
        if let Some(asset) = events
            .first_unreachable_asset_reference(&session_id, &params.verdict.evidence)
            .await
            .map_err(event_store_error)?
        {
            return Err(rpc_error(
                INVALID_PARAMS,
                "evidence_not_reachable",
                "verdict evidence must already be reachable from the active Session log",
                false,
                asset
                    .sha256
                    .as_deref()
                    .map(|digest| json!({ "digest": digest })),
            ));
        }
        for asset in &params.verdict.evidence {
            store
                .verify_session_reference(&session_id, asset)
                .await
                .map_err(evidence_error)?;
        }
    }

    let event = events
        .append(PendingEvent {
            session_id,
            request_id: Some(request_id),
            device_id: session_event_device_id(connection),
            at_ms: now_ms(),
            payload: TestEventPayload::VerdictRecorded {
                verdict: params.verdict,
            },
        })
        .await
        .map_err(event_store_error)?;
    to_json_value(VerdictRecordResult { event })
}

fn invalid_ui_snapshot(observation_id: Uuid) -> RpcError {
    ui_snapshot_error(
        "invalid_ui_snapshot",
        "the stored UI snapshot is invalid",
        false,
        Some(json!({ "observationId": observation_id })),
    )
}

fn ui_snapshot_error(
    code: &'static str,
    message: &'static str,
    retryable: bool,
    details: Option<Value>,
) -> RpcError {
    rpc_error(UI_SNAPSHOT_ERROR, code, message, retryable, details)
}

async fn clear_ended_session(
    events: &MemoryEventStore,
    evidence: &EvidenceCleanup,
    session_id: SessionId,
) -> RpcResult {
    match evidence {
        #[cfg(test)]
        EvidenceCleanup::Disabled => events
            .delete_ended(&session_id)
            .await
            .map_err(event_store_error)?,
        EvidenceCleanup::Managed(store) => {
            cleanup_ended_session(events, store.as_ref(), &session_id, now_ms())
                .await
                .map_err(session_cleanup_error)?;
        }
    }
    to_json_value(EventsClearResult {
        deleted: true,
        session_id,
    })
}

fn active_operation_context(
    connection: &ConnectionState,
    request_id: devicerail_protocol::RpcId,
    control: ExecutionControl,
) -> Result<OperationContext, RpcError> {
    let context = connection.context().ok_or_else(session_required)?;
    let session_id = context
        .active_session
        .clone()
        .ok_or_else(session_required)?;
    let ui_snapshots_enabled = feature_enabled(&context.hello, feature::OBSERVATION_UI_SNAPSHOT_V1);
    let semantic_actions_enabled =
        feature_enabled(&context.hello, feature::DEVICE_SEMANTIC_ACTIONS_V1);
    if semantic_actions_enabled && !ui_snapshots_enabled {
        return Err(semantic_snapshot_dependency_unsatisfied());
    }
    Ok(OperationContext::new(session_id, Some(request_id))
        .with_control(control)
        .with_ui_snapshots_enabled(ui_snapshots_enabled)
        .with_semantic_actions_enabled(semantic_actions_enabled))
}

fn cancel_request(params: Option<RpcParams>, registry: &RequestRegistry) -> RpcResult {
    let params = serde_json::from_value::<RequestCancelParams>(
        params.map(RpcParams::into_value).unwrap_or(Value::Null),
    )
    .map_err(|_| {
        rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            "request.cancel params are invalid",
            false,
            Some(json!({ "method": "request.cancel" })),
        )
    })?;
    let status = registry.cancel(&params.request_id, CancellationReason::Requested);
    to_json_value(RequestCancelResult {
        request_id: params.request_id,
        status,
    })
}

async fn start_media_stream(
    params: Option<RpcParams>,
    connection: &ConnectionState,
    runtime: &Registry,
    evidence: &EvidenceCleanup,
    request_id: RpcId,
) -> RpcResult {
    let params = decode_required_params::<MediaStreamStartParams>(params, "media.stream.start")?;
    if runtime.screenshot_policy() != ScreenshotPolicy::Capture {
        return Err(method_unavailable(
            "media.stream.start",
            Some(feature::MEDIA_STREAM_V1),
        ));
    }
    let store = evidence
        .managed_store()
        .cloned()
        .ok_or_else(|| method_unavailable("media.stream.start", Some(feature::MEDIA_STREAM_V1)))?;
    let context = connection.context().ok_or_else(session_required)?;
    let session_id = context
        .active_session
        .clone()
        .ok_or_else(session_required)?;
    let manager = Arc::clone(&context.media_streams);
    let route = selected_route(connection, runtime).await?;
    let device_id = route.id().clone();
    // Start performs no Driver I/O. Capture owns the cancellable health probe
    // and observation; start only proves that the selected route is still
    // leased by this connection.
    let lease = context.device_lease.as_ref().ok_or_else(session_required)?;
    let owner_id = LeaseOwnerId::new(context.hello.connection_id);
    let access = runtime
        .access_with_lease(route, lease.id, owner_id, now_ms())
        .await
        .map_err(device_pool_error)?;
    if access.id() != &device_id {
        return Err(media_rpc_error(
            "media_stream_device_changed",
            "selected media producer changed during admission",
            true,
            None,
        ));
    }
    drop(access);
    let info = MediaStreamInfo {
        id: params.stream_id,
        kind: params.kind,
        media_type: "image/png".to_owned(),
        viewport: None,
    };
    match manager.begin_start(&session_id, &device_id, &info)? {
        MediaStartAdmission::Existing(existing) => {
            if existing.session_id != session_id || existing.device_id != device_id {
                return Err(media_rpc_error(
                    "media_stream_owner_conflict",
                    "media stream id belongs to another Session or device",
                    false,
                    Some(json!({ "streamId": info.id })),
                ));
            }
            existing
                .writer
                .ensure_started()
                .await
                .map_err(media_stream_error)?;
            to_json_value(MediaStreamStartResult {
                stream: existing.info.clone(),
            })
        }
        MediaStartAdmission::Reserved(reservation) => {
            let writer = Arc::new(MediaStreamWriter::prepare(
                runtime.event_store(),
                store,
                session_id.clone(),
                Some(request_id),
                session_event_device_id(connection),
                info.clone(),
                now_ms(),
            ));
            let record = reservation.commit(session_id, device_id, info.clone(), writer);
            record
                .writer
                .ensure_started()
                .await
                .map_err(media_stream_error)?;
            to_json_value(MediaStreamStartResult { stream: info })
        }
    }
}

async fn capture_media_stream(
    params: Option<RpcParams>,
    connection: &ConnectionState,
    runtime: &Registry,
    request_id: RpcId,
    control: &ExecutionControl,
    admitted_route: &mut Option<Result<DeviceRoute, RpcError>>,
) -> RpcResult {
    let params =
        decode_required_params::<MediaStreamCaptureParams>(params, "media.stream.capture")?;
    let context = connection.context().ok_or_else(session_required)?;
    let session_id = context
        .active_session
        .clone()
        .ok_or_else(session_required)?;
    let stream = context.media_streams.stream(&params.stream_id)?;
    if stream.session_id != session_id {
        return Err(media_rpc_error(
            "media_stream_session_mismatch",
            "media stream does not belong to the active Session",
            false,
            Some(json!({ "streamId": params.stream_id })),
        ));
    }
    match stream.info.kind {
        MediaStreamKind::Screenshot if params.duration_ms.is_some() => {
            return Err(media_rpc_error(
                "invalid_media_frame",
                "screenshot stream frames cannot declare durationMs",
                false,
                Some(json!({ "streamId": params.stream_id })),
            ));
        }
        MediaStreamKind::Video if params.duration_ms.is_none_or(|duration| duration == 0) => {
            return Err(media_rpc_error(
                "invalid_media_frame",
                "video stream frames require a positive durationMs",
                false,
                Some(json!({ "streamId": params.stream_id })),
            ));
        }
        _ => {}
    }
    let _capture = Arc::clone(&stream.capture_gate)
        .try_lock_owned()
        .map_err(|_| {
            media_rpc_error(
                "media_stream_busy",
                "another capture is already in flight for this stream",
                true,
                Some(json!({ "streamId": params.stream_id })),
            )
        })?;

    let requested_index = params.frame_index.get();
    {
        let mut state = stream
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if requested_index == state.frame_count {
            if let Some(frame) = state.last_frame.clone() {
                if state.last_duration_ms == params.duration_ms {
                    return to_json_value(MediaStreamCaptureResult { frame });
                }
                return Err(media_rpc_error(
                    "media_frame_retry_conflict",
                    "frame retry metadata differs from the committed frame",
                    false,
                    Some(json!({
                        "streamId": params.stream_id,
                        "frameIndex": requested_index
                    })),
                ));
            }
        }
        if state.ended_frame_count.is_some() {
            return Err(media_rpc_error(
                "media_stream_ended",
                "media stream is already ended",
                false,
                Some(json!({ "streamId": params.stream_id })),
            ));
        }
        if state.poisoned {
            return Err(media_rpc_error(
                "media_stream_poisoned",
                "media stream encountered an ambiguous frame append and must be ended",
                false,
                Some(json!({ "streamId": params.stream_id })),
            ));
        }
        let expected_index = state.frame_count.saturating_add(1);
        if requested_index != expected_index {
            return Err(media_rpc_error(
                "media_frame_out_of_order",
                "media frame index is not the next expected index",
                false,
                Some(json!({
                    "streamId": params.stream_id,
                    "frameIndex": requested_index,
                    "expectedFrameIndex": expected_index
                })),
            ));
        }
        if state.frame_count >= MAX_MEDIA_FRAMES_PER_STREAM {
            return Err(media_rpc_error(
                "media_stream_frame_limit",
                "media stream frame limit was reached",
                false,
                Some(json!({ "limit": MAX_MEDIA_FRAMES_PER_STREAM })),
            ));
        }
        let now = Instant::now();
        if let Some(last) = state.last_capture_at {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < MIN_MEDIA_CAPTURE_INTERVAL {
                let remaining = MIN_MEDIA_CAPTURE_INTERVAL - elapsed;
                let retry_after_ms = remaining.as_millis().max(1) as u64;
                return Err(media_rpc_error(
                    "media_capture_rate_limited",
                    "media capture rate exceeds the per-stream limit",
                    true,
                    Some(json!({
                        "streamId": params.stream_id,
                        "retryAfterMs": retry_after_ms
                    })),
                ));
            }
        }
        state.last_capture_at = Some(now);
    }

    let route = request_route(admitted_route, connection, runtime).await?;
    if route.id() != &stream.device_id {
        return Err(media_rpc_error(
            "media_stream_device_changed",
            "selected media producer differs from the stream owner",
            false,
            Some(json!({ "streamId": params.stream_id })),
        ));
    }
    let access = authorize_leased_route(connection, runtime, route, control).await?;
    let operation = active_operation_context(connection, request_id.clone(), control.clone())?;
    let observation = access.observe(&operation).await.map_err(runtime_error)?;
    let screenshot = observation.screenshot.ok_or_else(|| {
        media_rpc_error(
            "media_capture_unavailable",
            "selected device did not return screenshot Evidence",
            true,
            Some(json!({ "streamId": params.stream_id })),
        )
    })?;
    if screenshot.media_type != "image/png" {
        return Err(media_rpc_error(
            "media_capture_format_unsupported",
            "selected device screenshot is not image/png",
            false,
            Some(json!({
                "streamId": params.stream_id,
                "mediaType": screenshot.media_type
            })),
        ));
    }
    let frame = match stream
        .writer
        .push_asset_frame_with_request_id(
            control,
            Some(request_id.clone()),
            now_ms(),
            true,
            params.duration_ms,
            &screenshot,
        )
        .await
    {
        Ok(frame) => frame,
        Err(error @ MediaStreamError::Event(_)) => {
            let _ = poison_and_abort_media_stream(&stream, Some(request_id)).await;
            return Err(media_stream_error(error));
        }
        Err(error) => return Err(media_stream_error(error)),
    };
    if frame.frame_index != params.frame_index {
        return Err(media_rpc_error(
            "media_frame_state_mismatch",
            "media writer frame index differs from daemon admission state",
            false,
            Some(json!({ "streamId": params.stream_id })),
        ));
    }
    {
        let mut state = stream
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.frame_count = requested_index;
        state.last_duration_ms = params.duration_ms;
        state.last_frame = Some(frame.clone());
    }
    if let Some(error) = inactive_request_error(control) {
        return Err(error);
    }
    to_json_value(MediaStreamCaptureResult { frame })
}

async fn end_media_stream(
    params: Option<RpcParams>,
    connection: &ConnectionState,
    request_id: RpcId,
) -> RpcResult {
    let params = decode_required_params::<MediaStreamEndParams>(params, "media.stream.end")?;
    let context = connection.context().ok_or_else(session_required)?;
    let session_id = context
        .active_session
        .clone()
        .ok_or_else(session_required)?;
    let stream = context.media_streams.stream(&params.stream_id)?;
    if stream.session_id != session_id {
        return Err(media_rpc_error(
            "media_stream_session_mismatch",
            "media stream does not belong to the active Session",
            false,
            Some(json!({ "streamId": params.stream_id })),
        ));
    }
    let _capture = Arc::clone(&stream.capture_gate)
        .try_lock_owned()
        .map_err(|_| {
            media_rpc_error(
                "media_stream_busy",
                "media stream capture is still in flight",
                true,
                Some(json!({ "streamId": params.stream_id })),
            )
        })?;
    if let Some(frame_count) = stream
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ended_frame_count
    {
        return to_json_value(MediaStreamEndResult {
            stream_id: params.stream_id,
            frame_count,
        });
    }
    let poisoned = stream
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .poisoned;
    let frame_count = if poisoned {
        stream
            .writer
            .abort_with_request_id(Some(request_id), now_ms())
            .await
    } else {
        stream
            .writer
            .finish_with_request_id(Some(request_id), now_ms())
            .await
    }
    .map_err(media_stream_error)?;
    stream
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ended_frame_count = Some(frame_count);
    to_json_value(MediaStreamEndResult {
        stream_id: params.stream_id,
        frame_count,
    })
}

async fn abort_media_streams(
    connection: &ConnectionState,
    grace: Duration,
    request_id: Option<RpcId>,
) -> Result<(), RpcError> {
    let Some(context) = connection.context() else {
        return Ok(());
    };
    let Some(session_id) = context.active_session.clone() else {
        return Ok(());
    };
    let streams = context.media_streams.streams_for_session(&session_id);
    let deadline = Instant::now() + grace;
    let mut closes = JoinSet::new();
    for stream in streams {
        if stream
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ended_frame_count
            .is_some()
        {
            continue;
        }
        let request_id = request_id.clone();
        closes.spawn(async move { abort_media_stream_until(stream, request_id, deadline).await });
    }
    let mut first_error = None;
    while let Some(result) = closes.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
            Ok(Err(_)) => {}
            Err(error) if first_error.is_none() => {
                first_error = Some(media_rpc_error(
                    "media_stream_close_failed",
                    "media stream close task failed",
                    true,
                    Some(json!({ "reason": bounded_diagnostic(&error.to_string()) })),
                ));
            }
            Err(_) => {}
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn abort_media_stream_until(
    stream: Arc<ManagedMediaStream>,
    request_id: Option<RpcId>,
    deadline: Instant,
) -> Result<(), RpcError> {
    let stream_id = stream.info.id.clone();
    let _capture = timeout_at(deadline, Arc::clone(&stream.capture_gate).lock_owned())
        .await
        .map_err(|_| {
            media_rpc_error(
                "media_stream_close_timed_out",
                "timed out waiting for media capture to finish",
                true,
                Some(json!({ "streamId": stream_id })),
            )
        })?;
    let mut retryable_failures = 0_u32;
    loop {
        let result = timeout_at(
            deadline,
            stream
                .writer
                .abort_with_request_id(request_id.clone(), now_ms()),
        )
        .await;
        match result {
            Ok(Ok(frame_count)) => {
                stream
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .ended_frame_count = Some(frame_count);
                return Ok(());
            }
            Ok(Err(MediaStreamError::Event(error)))
                if error.to_error_info().retryable && Instant::now() < deadline =>
            {
                retryable_failures = retryable_failures.saturating_add(1);
                if retryable_failures > 1 {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    tokio::time::sleep(remaining.min(Duration::from_millis(5))).await;
                } else {
                    tokio::task::yield_now().await;
                }
            }
            Ok(Err(error)) => return Err(media_stream_error(error)),
            Err(_) => {
                return Err(media_rpc_error(
                    "media_stream_close_timed_out",
                    "timed out while closing media stream",
                    true,
                    Some(json!({ "streamId": stream_id })),
                ));
            }
        }
    }
}

async fn poison_and_abort_media_stream(
    stream: &ManagedMediaStream,
    request_id: Option<RpcId>,
) -> Result<u64, MediaStreamError> {
    stream
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .poisoned = true;
    let frame_count = stream
        .writer
        .abort_with_request_id(request_id, now_ms())
        .await?;
    let mut state = stream
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.frame_count = frame_count;
    state.ended_frame_count = Some(frame_count);
    Ok(frame_count)
}

async fn select_device(
    params: Option<RpcParams>,
    connection: &mut ConnectionState,
    registry: &Registry,
) -> RpcResult {
    if let Some(context) = connection.context()
        && context.active_session.is_some()
    {
        return Err(rpc_error(
            DEVICE_POOL_ERROR,
            "device_lease_active",
            "end the active session before selecting another device",
            false,
            context
                .active_session
                .as_ref()
                .map(|session_id| json!({ "sessionId": session_id })),
        ));
    }
    let params = serde_json::from_value::<DeviceSelectParams>(
        params.map(RpcParams::into_value).unwrap_or(Value::Null),
    )
    .map_err(|_| {
        rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            "device.select params are invalid",
            false,
            Some(json!({ "method": "device.select" })),
        )
    })?;
    let handle = registry
        .resolve(&params.device_id)
        .await
        .map_err(registry_error)?;
    let device = handle.info().await;
    connection
        .context_mut()
        .expect("handshake was checked before device.select")
        .selected_device_id = Some(params.device_id);
    to_json_value(DeviceSelectResult { device })
}

async fn start_session(
    connection: &mut ConnectionState,
    runtime: &Registry,
    events: &MemoryEventStore,
    request_id: devicerail_protocol::RpcId,
    control: &ExecutionControl,
) -> RpcResult {
    if let Some(active) = connection
        .context()
        .and_then(|context| context.active_session.clone())
    {
        return Err(rpc_error(
            SESSION_ERROR,
            "session_already_active",
            "this connection already has an active session",
            false,
            Some(json!({ "sessionId": active })),
        ));
    }

    let route = selected_route(connection, runtime).await?;
    let device_id = route.id().clone();
    let checked_at_ms = now_ms();
    match route.health_check(control).await {
        Ok(()) => {
            runtime
                .record_health(&route, PoolHealth::healthy(checked_at_ms), checked_at_ms)
                .await
                .map_err(device_pool_error)?;
        }
        Err(error) => {
            let code = error.to_error_info().code;
            let health = PoolHealth::unhealthy(checked_at_ms, &code).map_err(device_pool_error)?;
            runtime
                .record_health(&route, health, checked_at_ms)
                .await
                .map_err(device_pool_error)?;
            return Err(runtime_error(error));
        }
    }
    let owner_id = connection_owner(connection)?;
    let lease = runtime
        .acquire_lease(&route, owner_id, now_ms())
        .await
        .map_err(device_pool_error)?;
    let event_device_id = connection
        .context()
        .is_some_and(|context| feature_enabled(&context.hello, feature::DEVICE_ROUTING_V1))
        .then_some(device_id.clone());
    let command = StartSession::new(Some(request_id), event_device_id, now_ms());
    let session_id = command.session_id.clone();
    let info = match events.start_session(command).await {
        Ok(info) => info,
        Err(error) => {
            let _ = runtime.release_lease(lease.id, owner_id, now_ms()).await;
            return Err(event_store_error(error));
        }
    };
    let context = connection
        .context_mut()
        .expect("handshake was checked before dispatch");
    context.selected_device_id = Some(device_id);
    context.active_session = Some(session_id);
    context.device_lease = Some(lease);
    to_json_value(info)
}

fn current_session(connection: &ConnectionState) -> RpcResult {
    match connection
        .context()
        .and_then(|context| context.active_session.clone())
    {
        Some(session_id) => to_json_value(SessionCurrentResult { session_id }),
        None => Err(session_required()),
    }
}

async fn end_session(
    connection: &mut ConnectionState,
    runtime: &Registry,
    events: &MemoryEventStore,
    request_id: devicerail_protocol::RpcId,
    params: Option<RpcParams>,
) -> RpcResult {
    let session_id = connection
        .context()
        .and_then(|context| context.active_session.clone())
        .ok_or_else(session_required)?;
    let params = decode_params_or_default::<SessionEndParams>(params, "session.end")?;
    abort_media_streams(
        connection,
        MEDIA_STREAM_CLOSE_GRACE,
        Some(request_id.clone()),
    )
    .await?;
    let event_device_id = session_event_device_id(connection);
    let info = events
        .end_session(EndSession {
            session_id: session_id.clone(),
            request_id: Some(request_id),
            device_id: event_device_id,
            at_ms: now_ms(),
            outcome: params.outcome.unwrap_or(SessionOutcome::Completed),
            reason: params.reason,
        })
        .await
        .map_err(event_store_error)?;
    let context = connection
        .context()
        .expect("handshake was checked before dispatch");
    let lease = context.device_lease.clone();
    let owner_id = LeaseOwnerId::new(context.hello.connection_id);
    if let Some(lease) = lease {
        match runtime.release_lease(lease.id, owner_id, now_ms()).await {
            Ok(_) | Err(DevicePoolError::LeaseNotFound | DevicePoolError::LeaseExpired) => {}
            Err(error) => return Err(device_pool_error(error)),
        }
    }
    let context = connection
        .context_mut()
        .expect("handshake was checked before dispatch");
    if context.active_session.as_ref() == Some(&session_id) {
        context.active_session = None;
        context.device_lease = None;
        context.media_streams.clear();
    }
    to_json_value(info)
}

fn session_target(
    params: Option<RpcParams>,
    connection: &ConnectionState,
    method: &str,
) -> Result<SessionId, RpcError> {
    let params = decode_params_or_default::<SessionTargetParams>(params, method)?;
    params
        .session_id
        .or_else(|| {
            connection
                .context()
                .and_then(|context| context.active_session.clone())
        })
        .ok_or_else(session_required)
}

fn decode_params_or_default<T>(params: Option<RpcParams>, method: &str) -> Result<T, RpcError>
where
    T: DeserializeOwned + Default,
{
    match params {
        None => Ok(T::default()),
        Some(params) => serde_json::from_value(params.into_value()).map_err(|_| {
            rpc_error(
                INVALID_PARAMS,
                "invalid_params",
                format!("{method} params are invalid"),
                false,
                Some(json!({ "method": method })),
            )
        }),
    }
}

fn decode_required_params<T>(params: Option<RpcParams>, method: &str) -> Result<T, RpcError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(params.map(RpcParams::into_value).unwrap_or(Value::Null)).map_err(|_| {
        rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            format!("{method} params are invalid"),
            false,
            Some(json!({ "method": method })),
        )
    })
}

fn session_required() -> RpcError {
    rpc_error(
        SESSION_REQUIRED,
        "session_required",
        "start a session before recording device operations",
        true,
        Some(json!({ "requiredMethod": "session.start" })),
    )
}

fn negotiate_connection(
    params: Option<RpcParams>,
    connection: &mut ConnectionState,
    event_stream_available: bool,
    media_producer_available: bool,
    evidence_store_available: bool,
) -> RpcResult {
    let transport_kind = connection.transport_kind().to_owned();
    if connection.context().is_some() {
        return Err(rpc_error(
            HANDSHAKE_ALREADY_COMPLETED,
            "handshake_already_completed",
            "this transport connection has already completed system.hello",
            false,
            None,
        ));
    }

    let request = serde_json::from_value::<HelloParams>(
        params.map(RpcParams::into_value).unwrap_or(Value::Null),
    )
    .map_err(|_| {
        rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            "system.hello params are invalid",
            true,
            Some(json!({ "method": "system.hello" })),
        )
    })?;

    let server_protocol = supported_protocol_offer();
    let selected = negotiate_protocol(&request.protocol, &server_protocol)
        .map_err(|error| protocol_negotiation_error(error, &request.protocol, &server_protocol))?;

    let mut available_features = server_features(
        selected,
        event_stream_available,
        media_producer_available,
        evidence_store_available,
    );
    let snapshot_offered = request
        .features
        .required
        .contains(feature::EVENTS_SNAPSHOT_V1)
        || request
            .features
            .optional
            .contains(feature::EVENTS_SNAPSHOT_V1);
    if !snapshot_offered {
        available_features.remove(feature::SESSION_EXPORT_PAGE_V1);
    }
    let features = negotiate_features(&request.features, &available_features).map_err(|error| {
        rpc_error(
            REQUIRED_FEATURE_UNSUPPORTED,
            "required_feature_unsupported",
            "one or more required protocol features are unsupported",
            true,
            Some(json!({
                "unsupportedRequired": error.unsupported_required,
                "available": available_features
            })),
        )
    })?;
    if features
        .enabled
        .contains(feature::DEVICE_SEMANTIC_ACTIONS_V1)
        && !features
            .enabled
            .contains(feature::OBSERVATION_UI_SNAPSHOT_V1)
    {
        return Err(semantic_snapshot_dependency_unsatisfied());
    }

    let hello = HelloResult {
        connection_id: Uuid::new_v4(),
        protocol: ProtocolSelection { selected },
        server: PeerInfo {
            name: "devicerail-daemon".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        transport: TransportInfo {
            kind: transport_kind,
            framing: "ndjson".to_owned(),
        },
        features,
    };
    let value = to_json_value(&hello)?;
    *connection = ConnectionState::Ready(Box::new(NegotiatedContext {
        client: request.client,
        hello,
        active_session: None,
        selected_device_id: None,
        device_lease: None,
        media_streams: Arc::new(MediaStreamManager::default()),
    }));
    Ok(value)
}

fn protocol_negotiation_error(
    error: ProtocolNegotiationError,
    client: &devicerail_protocol::ProtocolOffer,
    server: &devicerail_protocol::ProtocolOffer,
) -> RpcError {
    match error {
        ProtocolNegotiationError::EmptyClientOffer => rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            "client protocol offer must contain at least one range",
            true,
            Some(json!({ "reason": "emptyProtocolOffer" })),
        ),
        ProtocolNegotiationError::InvalidClientRange => rpc_error(
            INVALID_PARAMS,
            "invalid_params",
            "client protocol offer contains an inverted minor range",
            true,
            Some(json!({ "reason": "invalidProtocolRange", "clientProtocol": client })),
        ),
        ProtocolNegotiationError::EmptyServerOffer
        | ProtocolNegotiationError::InvalidServerRange => rpc_error(
            INTERNAL_ERROR,
            "internal_error",
            "server protocol offer is invalid",
            false,
            None,
        ),
        ProtocolNegotiationError::Incompatible(reason) => {
            incompatible_protocol_error(reason, client, server)
        }
    }
}

fn incompatible_protocol_error(
    reason: ProtocolIncompatibilityReason,
    client: &devicerail_protocol::ProtocolOffer,
    server: &devicerail_protocol::ProtocolOffer,
) -> RpcError {
    rpc_error(
        PROTOCOL_VERSION_INCOMPATIBLE,
        "protocol_version_incompatible",
        "client and server do not share a protocol version",
        false,
        Some(json!({
            "reason": reason,
            "clientProtocol": client,
            "serverProtocol": server
        })),
    )
}

fn server_features(
    selected: ProtocolVersion,
    event_stream_available: bool,
    media_producer_available: bool,
    evidence_store_available: bool,
) -> BTreeSet<String> {
    let mut features = BTreeSet::from([feature::EVENTS_SNAPSHOT_V1.to_owned()]);
    if selected.major == 1 && selected.minor >= 1 {
        features.insert(feature::REQUEST_CONTROL_V1.to_owned());
    }
    if selected.major == 1 && selected.minor >= 2 {
        features.insert(feature::DEVICE_ROUTING_V1.to_owned());
        features.insert(feature::ACTION_PROTECTED_V1.to_owned());
    }
    if event_stream_available && selected.major == 1 && selected.minor >= 3 {
        features.insert(feature::EVENTS_STREAM_V1.to_owned());
    }
    if media_producer_available && selected.major == 1 && selected.minor >= 4 {
        features.insert(feature::MEDIA_STREAM_V1.to_owned());
    }
    if selected.major == 1 && selected.minor >= 4 {
        features.insert(feature::SESSION_EXPORT_PAGE_V1.to_owned());
    }
    if selected.major == 1 && selected.minor >= 5 && evidence_store_available {
        features.insert(feature::DEVICE_SEMANTIC_ACTIONS_V1.to_owned());
        features.insert(feature::OBSERVATION_UI_SNAPSHOT_V1.to_owned());
        features.insert(feature::VERDICT_RECORD_V1.to_owned());
    }
    features
}

fn feature_enabled(context: &HelloResult, feature: &str) -> bool {
    context.features.enabled.contains(feature)
}

fn known_method(method: &str) -> bool {
    matches!(
        method,
        "system.describe"
            | "devices.list"
            | "device.select"
            | "device.connect"
            | "device.disconnect"
            | "device.capabilities"
            | "device.observe"
            | "device.execute"
            | "media.stream.start"
            | "media.stream.capture"
            | "media.stream.end"
            | "request.cancel"
            | "session.start"
            | "session.current"
            | "session.end"
            | "sessions.list"
            | "session.export"
            | "events.list"
            | "events.clear"
            | "events.stream.open"
            | "ui.snapshot.get"
            | "verdict.record"
    )
}

fn method_takes_no_params(method: &str) -> bool {
    matches!(
        method,
        "system.describe"
            | "devices.list"
            | "device.connect"
            | "device.disconnect"
            | "device.capabilities"
            | "device.observe"
            | "session.start"
            | "session.current"
            | "sessions.list"
    )
}

fn validate_no_params(params: Option<&RpcParams>, method: &str) -> Result<(), RpcError> {
    if params.is_none_or(RpcParams::is_empty) {
        return Ok(());
    }

    Err(rpc_error(
        INVALID_PARAMS,
        "invalid_params",
        format!("{method} does not accept params"),
        false,
        Some(json!({ "method": method })),
    ))
}

fn method_unavailable(method: &str, required_feature: Option<&str>) -> RpcError {
    let details = required_feature.map_or_else(
        || json!({ "method": method }),
        |feature| json!({ "method": method, "requiredFeature": feature }),
    );
    rpc_error(
        METHOD_NOT_FOUND,
        "method_not_found",
        format!("method is not available: {method}"),
        false,
        Some(details),
    )
}

fn protected_action_not_negotiated(action: &str) -> RpcError {
    rpc_error(
        METHOD_NOT_FOUND,
        "protected_action_not_negotiated",
        "protected action requires an explicitly negotiated feature",
        false,
        Some(json!({
            "action": action,
            "requiredFeature": feature::ACTION_PROTECTED_V1
        })),
    )
}

fn semantic_action_not_negotiated(action: &str) -> RpcError {
    rpc_error(
        METHOD_NOT_FOUND,
        "semantic_action_not_negotiated",
        "semantic action requires an explicitly negotiated feature",
        false,
        Some(json!({
            "action": action,
            "requiredFeature": feature::DEVICE_SEMANTIC_ACTIONS_V1
        })),
    )
}

fn semantic_snapshot_dependency_unsatisfied() -> RpcError {
    rpc_error(
        REQUIRED_FEATURE_UNSUPPORTED,
        "feature_dependency_unsatisfied",
        "semantic Actions require negotiated UI snapshot evidence",
        false,
        Some(json!({
            "feature": feature::DEVICE_SEMANTIC_ACTIONS_V1,
            "requiredFeature": feature::OBSERVATION_UI_SNAPSHOT_V1
        })),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionExportPageWire<'a> {
    session: &'a SessionInfo,
    events: &'a [&'a TestEvent],
    #[serde(skip_serializing_if = "Option::is_none")]
    next_after_sequence: Option<EventSequence>,
}

#[derive(Serialize)]
struct RpcSuccessWire<'a, T>
where
    T: Serialize,
{
    jsonrpc: &'static str,
    id: &'a RpcId,
    result: T,
}

#[derive(Default)]
struct SerializedLength {
    bytes: usize,
}

impl std::io::Write for SerializedLength {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.checked_add(input.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized response length overflowed usize",
            )
        })?;
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_length<T>(value: &T) -> Result<usize, RpcError>
where
    T: Serialize + ?Sized,
{
    let mut counter = SerializedLength::default();
    serde_json::to_writer(&mut counter, value).map_err(|error| {
        rpc_error(
            INTERNAL_ERROR,
            "internal_error",
            "response serialization failed",
            false,
            Some(json!({ "cause": error.to_string() })),
        )
    })?;
    Ok(counter.bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CappedJsonLength {
    Exact(usize),
    Exceeded {
        counted_bytes: usize,
        actual_bytes_at_least: usize,
    },
}

struct CappedSerializedLength {
    bytes: usize,
    limit: usize,
    actual_bytes_at_least: Option<usize>,
}

impl CappedSerializedLength {
    fn new(limit: usize) -> Self {
        Self {
            bytes: 0,
            limit,
            actual_bytes_at_least: None,
        }
    }
}

impl std::io::Write for CappedSerializedLength {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let next = self.bytes.checked_add(input.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized response length overflowed usize",
            )
        })?;
        if next > self.limit {
            self.actual_bytes_at_least = Some(next);
            return Err(std::io::Error::other(
                "serialized JSON exceeded its byte budget",
            ));
        }
        self.bytes = next;
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capped_serialized_json_length<T>(value: &T, limit: usize) -> Result<CappedJsonLength, RpcError>
where
    T: Serialize + ?Sized,
{
    let mut counter = CappedSerializedLength::new(limit);
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(CappedJsonLength::Exact(counter.bytes)),
        Err(_) if counter.actual_bytes_at_least.is_some() => Ok(CappedJsonLength::Exceeded {
            counted_bytes: counter.bytes,
            actual_bytes_at_least: counter
                .actual_bytes_at_least
                .expect("budget failure records its lower bound"),
        }),
        Err(error) => Err(rpc_error(
            INTERNAL_ERROR,
            "internal_error",
            "response serialization failed",
            false,
            Some(json!({ "cause": error.to_string() })),
        )),
    }
}

fn session_export_page_response_length(
    id: &RpcId,
    session: &SessionInfo,
    events: &[&TestEvent],
    next_after_sequence: Option<EventSequence>,
) -> Result<usize, RpcError> {
    serialized_json_length(&RpcSuccessWire {
        jsonrpc: "2.0",
        id,
        result: SessionExportPageWire {
            session,
            events,
            next_after_sequence,
        },
    })
}

fn decimal_digits(value: u64) -> usize {
    debug_assert!(value > 0);
    value.ilog10() as usize + 1
}

/// Selects a byte-bounded prefix after the Store lock has been released.
///
/// The candidate vector contains only `Arc` handles. Event bodies are measured
/// without allocation, and only the fitting prefix is materialized as a JSON
/// value. If one event cannot fit, return the same typed size failure as the
/// global response writer without constructing the oversized value.
fn bounded_session_export_page_value(
    id: &RpcId,
    snapshot: SessionExportPageSnapshot,
    max_bytes: usize,
) -> RpcResult {
    let empty: [&TestEvent; 0] = [];
    let base_without_continuation =
        session_export_page_response_length(id, &snapshot.session, &empty, None)?;
    let base_with_one_digit_continuation = session_export_page_response_length(
        id,
        &snapshot.session,
        &empty,
        Some(EventSequence::FIRST),
    )?;
    let mut selected = 0_usize;
    let mut selected_event_bytes = 0_usize;
    let mut selected_response_bytes = base_without_continuation;

    for event in &snapshot.events {
        let separator_bytes = usize::from(selected > 0);
        let next = (event.sequence < snapshot.session.last_sequence).then_some(event.sequence);
        let base_bytes = match next {
            Some(sequence) => base_with_one_digit_continuation
                .checked_add(decimal_digits(sequence.get()).saturating_sub(1))
                .ok_or_else(|| {
                    rpc_error(
                        INTERNAL_ERROR,
                        "internal_error",
                        "serialized response length overflowed usize",
                        false,
                        None,
                    )
                })?,
            None => base_without_continuation,
        };
        let fixed_bytes = base_bytes
            .checked_add(selected_event_bytes)
            .and_then(|value| value.checked_add(separator_bytes))
            .ok_or_else(|| {
                rpc_error(
                    INTERNAL_ERROR,
                    "internal_error",
                    "serialized response length overflowed usize",
                    false,
                    None,
                )
            })?;
        let Some(remaining) = max_bytes.checked_sub(fixed_bytes) else {
            if selected == 0 {
                return Err(response_frame_too_large_lower_bound(
                    max_bytes.saturating_add(1),
                    max_bytes,
                ));
            }
            break;
        };
        match capped_serialized_json_length(event.as_ref(), remaining)? {
            CappedJsonLength::Exact(event_bytes) => {
                let candidate_event_bytes = selected_event_bytes
                    .checked_add(separator_bytes)
                    .and_then(|value| value.checked_add(event_bytes))
                    .ok_or_else(|| {
                        rpc_error(
                            INTERNAL_ERROR,
                            "internal_error",
                            "serialized response length overflowed usize",
                            false,
                            None,
                        )
                    })?;
                let candidate_response_bytes = base_bytes
                    .checked_add(candidate_event_bytes)
                    .ok_or_else(|| {
                        rpc_error(
                            INTERNAL_ERROR,
                            "internal_error",
                            "serialized response length overflowed usize",
                            false,
                            None,
                        )
                    })?;
                if candidate_response_bytes > max_bytes {
                    if selected == 0 {
                        return Err(response_frame_too_large(
                            candidate_response_bytes,
                            max_bytes,
                        ));
                    }
                    break;
                }
                selected_event_bytes = candidate_event_bytes;
                selected_response_bytes = candidate_response_bytes;
                selected += 1;
            }
            CappedJsonLength::Exceeded {
                actual_bytes_at_least,
                ..
            } => {
                if selected == 0 {
                    let response_bytes_at_least = fixed_bytes.saturating_add(actual_bytes_at_least);
                    return Err(response_frame_too_large_lower_bound(
                        response_bytes_at_least,
                        max_bytes,
                    ));
                }
                break;
            }
        }
    }

    let events = snapshot.events[..selected]
        .iter()
        .map(|event| event.as_ref())
        .collect::<Vec<_>>();
    let next_after_sequence = events.last().and_then(|event| {
        (event.sequence < snapshot.session.last_sequence).then_some(event.sequence)
    });
    if selected_response_bytes > max_bytes {
        return Err(response_frame_too_large(selected_response_bytes, max_bytes));
    }
    to_json_value(SessionExportPageWire {
        session: &snapshot.session,
        events: &events,
        next_after_sequence,
    })
}

fn serialize_runtime_result<T>(result: RuntimeResult<T>) -> RpcResult
where
    T: Serialize,
{
    result.map_err(runtime_error).and_then(to_json_value)
}

fn serialize_event_store_result<T>(result: Result<T, EventStoreError>) -> RpcResult
where
    T: Serialize,
{
    result.map_err(event_store_error).and_then(to_json_value)
}

fn ensure_events_protocol_compatible<'a>(
    events: impl IntoIterator<Item = &'a TestEvent>,
    selected: ProtocolVersion,
    method: &'static str,
    session_id: &SessionId,
) -> Result<(), RpcError> {
    for event in events {
        let required_minor = event.required_protocol_minor();
        if selected.major == 1 && selected.minor >= required_minor {
            continue;
        }
        return Err(rpc_error(
            SESSION_ERROR,
            "session_protocol_incompatible",
            "the Session contains an event that requires a newer protocol",
            false,
            Some(json!({
                "eventId": event.event_id,
                "eventSequence": event.sequence,
                "method": method,
                "requiredProtocol": ProtocolVersion::new(1, required_minor),
                "selectedProtocol": selected,
                "sessionId": session_id,
            })),
        ));
    }
    Ok(())
}

fn driver_error(error: devicerail_core::DriverError) -> RpcError {
    let data = error.to_error_info();
    RpcError {
        code: DRIVER_ERROR,
        message: data.message.clone(),
        data,
    }
}

fn evidence_error(error: devicerail_core::EvidenceError) -> RpcError {
    let data = error.to_error_info();
    RpcError {
        code: INTERNAL_ERROR,
        message: data.message.clone(),
        data,
    }
}

fn runtime_error(error: RuntimeError) -> RpcError {
    match error {
        RuntimeError::Driver(error) => driver_error(error),
        RuntimeError::EventStore(error) => event_store_error(error),
        RuntimeError::Evidence(error) => evidence_error(error),
        error @ RuntimeError::Cancelled { .. } => {
            let data = error.to_error_info();
            RpcError {
                code: REQUEST_CANCELLED,
                message: data.message.clone(),
                data,
            }
        }
        error @ RuntimeError::TimedOut { .. } => {
            let data = error.to_error_info();
            RpcError {
                code: REQUEST_TIMED_OUT,
                message: data.message.clone(),
                data,
            }
        }
    }
}

fn inactive_request_error(control: &ExecutionControl) -> Option<RpcError> {
    control
        .cancellation_reason()
        .map(|reason| runtime_error(RuntimeError::Cancelled { reason }))
        .or_else(|| {
            control.is_expired().then(|| {
                let (scope, timeout_ms) = control.timeout().unwrap_or((TimeoutScope::Request, 0));
                runtime_error(RuntimeError::TimedOut { scope, timeout_ms })
            })
        })
}

fn request_control_not_negotiated(field: &str) -> RpcError {
    rpc_error(
        INVALID_PARAMS,
        "feature_not_negotiated",
        format!("{field} requires negotiated request control"),
        false,
        Some(json!({
            "field": field,
            "requiredFeature": feature::REQUEST_CONTROL_V1
        })),
    )
}

fn request_timeout_not_supported(method: &str) -> RpcError {
    rpc_error(
        INVALID_PARAMS,
        "request_timeout_not_supported",
        format!("timeoutMs is not supported by {method}"),
        false,
        Some(json!({
            "field": "timeoutMs",
            "method": method,
            "supportedMethods": [
                "device.connect",
                "device.disconnect",
                "device.capabilities",
                "device.observe",
                "device.execute",
                "media.stream.capture"
            ]
        })),
    )
}

fn request_id_in_use(request_id: RpcId) -> RpcError {
    rpc_error(
        REQUEST_ID_IN_USE,
        "request_id_in_use",
        "request id is already active on this connection",
        false,
        Some(json!({ "requestId": request_id })),
    )
}

fn too_many_requests() -> RpcError {
    rpc_error(
        TOO_MANY_REQUESTS,
        "too_many_requests",
        "this connection has reached its in-flight request limit",
        true,
        Some(json!({ "limit": MAX_IN_FLIGHT_REQUESTS })),
    )
}

fn response_frame_too_large(actual_bytes: usize, limit_bytes: usize) -> RpcError {
    rpc_error(
        RESPONSE_FRAME_TOO_LARGE,
        "response_frame_too_large",
        format!("response frame exceeds the {limit_bytes}-byte limit"),
        false,
        Some(json!({
            "actualBytes": actual_bytes,
            "limitBytes": limit_bytes
        })),
    )
}

fn response_frame_too_large_lower_bound(
    actual_bytes_at_least: usize,
    limit_bytes: usize,
) -> RpcError {
    rpc_error(
        RESPONSE_FRAME_TOO_LARGE,
        "response_frame_too_large",
        format!("response frame exceeds the {limit_bytes}-byte limit"),
        false,
        Some(json!({
            "actualBytesAtLeast": actual_bytes_at_least,
            "limitBytes": limit_bytes
        })),
    )
}

fn event_store_error(error: EventStoreError) -> RpcError {
    let data = error.to_error_info();
    RpcError {
        code: SESSION_ERROR,
        message: data.message.clone(),
        data,
    }
}

fn media_rpc_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
    details: Option<Value>,
) -> RpcError {
    rpc_error(MEDIA_STREAM_ERROR, code, message, retryable, details)
}

fn media_stream_error(error: MediaStreamError) -> RpcError {
    match error {
        MediaStreamError::Event(error) => event_store_error(error),
        MediaStreamError::Evidence(error) => evidence_error(error),
        MediaStreamError::Cancelled => rpc_error(
            REQUEST_CANCELLED,
            "request_cancelled",
            "media capture was cancelled",
            true,
            None,
        ),
        MediaStreamError::TimedOut => rpc_error(
            REQUEST_TIMED_OUT,
            "request_timed_out",
            "media capture timed out",
            true,
            None,
        ),
        MediaStreamError::Ended => media_rpc_error(
            "media_stream_ended",
            "media stream is already ended",
            false,
            None,
        ),
        MediaStreamError::InvalidFrame => media_rpc_error(
            "invalid_media_frame",
            "media frame metadata is invalid",
            false,
            None,
        ),
    }
}

fn stream_transport_error(error: StreamTransportError) -> RpcError {
    let (numeric_code, code, message, retryable) = match error {
        StreamTransportError::InvalidOriginPolicy => (
            INVALID_PARAMS,
            "invalid_stream_origin",
            "event stream Origin policy is invalid",
            false,
        ),
        StreamTransportError::CapabilityLimit => (
            EVENT_STREAM_ERROR,
            "stream_capability_limit",
            "event stream capability limit reached",
            true,
        ),
        StreamTransportError::ShuttingDown => (
            EVENT_STREAM_ERROR,
            "stream_server_shutdown",
            "event stream server is shutting down",
            true,
        ),
        _ => (
            EVENT_STREAM_ERROR,
            "stream_transport_error",
            "event stream transport failed",
            false,
        ),
    };
    rpc_error(numeric_code, code, message, retryable, None)
}

fn session_cleanup_error(error: SessionCleanupError) -> RpcError {
    match error {
        SessionCleanupError::Events(error) => event_store_error(error),
        SessionCleanupError::Evidence(error) => {
            let cause = error.to_error_info();
            rpc_error(
                INTERNAL_ERROR,
                "evidence_cleanup_failed",
                "session event log was deleted but Evidence release did not complete",
                cause.retryable,
                Some(json!({ "causeCode": cause.code })),
            )
        }
    }
}

fn registry_error(error: RegistryError) -> RpcError {
    let data = error.to_error_info();
    RpcError {
        code: DEVICE_ROUTING_ERROR,
        message: data.message.clone(),
        data,
    }
}

fn device_pool_error(error: DevicePoolError) -> RpcError {
    let (code, retryable, details) = match &error {
        DevicePoolError::HealthUnknown(device_id) => (
            "device_health_unknown",
            true,
            Some(json!({ "deviceId": device_id })),
        ),
        DevicePoolError::HealthStale(device_id) => (
            "device_health_stale",
            true,
            Some(json!({ "deviceId": device_id })),
        ),
        DevicePoolError::DeviceUnhealthy { device_id, code } => (
            "device_unhealthy",
            true,
            Some(json!({ "deviceId": device_id, "healthCode": code })),
        ),
        DevicePoolError::DeviceInUse(device_id) | DevicePoolError::DeviceLeased(device_id) => (
            "device_in_use",
            true,
            Some(json!({ "deviceId": device_id })),
        ),
        DevicePoolError::LeaseExpired => ("device_lease_expired", true, None),
        DevicePoolError::LeaseNotFound => ("device_lease_not_found", true, None),
        DevicePoolError::LeaseMismatch => ("device_lease_mismatch", false, None),
        DevicePoolError::DeviceNotFound(device_id) => (
            "device_not_found",
            false,
            Some(json!({ "deviceId": device_id })),
        ),
        DevicePoolError::DeviceRegistrationChanged(device_id) => (
            "device_registration_changed",
            true,
            Some(json!({ "deviceId": device_id })),
        ),
        DevicePoolError::InvalidConfiguration
        | DevicePoolError::InvalidHealthCode
        | DevicePoolError::InvalidDeviceId
        | DevicePoolError::DeviceAlreadyRegistered(_) => ("device_pool_error", false, None),
    };
    rpc_error(
        DEVICE_POOL_ERROR,
        code,
        error.to_string(),
        retryable,
        details,
    )
}

fn to_json_value<T>(value: T) -> RpcResult
where
    T: Serialize,
{
    serde_json::to_value(value).map_err(|error| {
        rpc_error(
            INTERNAL_ERROR,
            "internal_error",
            "response serialization failed",
            false,
            Some(json!({ "cause": error.to_string() })),
        )
    })
}

fn rpc_error(
    numeric_code: i32,
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
    details: Option<Value>,
) -> RpcError {
    let message = message.into();
    RpcError {
        code: numeric_code,
        message: message.clone(),
        data: ErrorInfo {
            code: code.into(),
            message,
            retryable,
            details,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        ffi::{OsStr, OsString},
        io::Cursor,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    #[cfg(unix)]
    use std::fs;

    #[cfg(unix)]
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use devicerail_android_adb::{
        AdbDeviceState, AdbDiscoveryReport, AdbSerial, DiscoveredAndroidDevice,
    };
    use devicerail_core::{
        CancellationReason, DeviceDriver, DeviceOperationResult, DriverError,
        DriverOperationContext, DriverResult, EndSession, EvidenceError, EvidenceInput,
        EvidenceMetadata, EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl,
        ExecutionController, GcPolicy, GcReport, LeaseOwnerId, MemoryEventStore, PendingEvent,
        PutEvidence, ReleaseReport, RuntimeError, ScreenshotPolicy, SessionEventStore,
        Sha256Digest, StartSession, StoredEvidence, TimeoutScope, UnavailableEvidenceStore, now_ms,
        reconcile_missing_session_evidence,
    };
    #[cfg(unix)]
    use devicerail_distributed_router::{
        NdjsonPeerTransport, PeerOperation, PeerRequest, PeerResult, PeerSecurity, PeerTransport,
    };
    use devicerail_driver_mock::MockDriver;
    use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
    use devicerail_harmony_hdc::{
        DiscoveredHarmonyDevice, HarmonyDiscoveryReport, HdcTarget, HdcTargetState,
    };
    use devicerail_protocol::{
        ActionCall, ActionDefinition, ActionOutcome, ActionProtection, ActionResult, AssetRef,
        DeviceId, DeviceInfo, ErrorInfo, EventSequence, EventsStreamOpenResult, FeatureOffer,
        HelloParams, JsonRpcVersion, Observation, PeerInfo, Platform, ProtocolOffer, ProtocolRange,
        ProtocolVersion, RequestTimeoutMs, RpcId, RpcParams, RpcRequest, RpcResponse,
        ScreenshotOmissionReason, SessionId, SessionOutcome, TestEventPayload,
        UI_SNAPSHOT_FORMAT_VERSION, UI_SNAPSHOT_MEDIA_TYPE, UiContextKind, UiContextRef, UiNode,
        UiRect, UiSnapshot, UiSnapshotRef, Viewport, feature,
    };
    #[cfg(unix)]
    use devicerail_remote_auth::{
        AuditDecision, AuditLog, AuthChallenge, AuthChallengeRequest, AuthProofRequest,
        Authenticator, CredentialStore, compute_proof,
    };
    use devicerail_websocket_transport::{Config as StreamConfig, EventStreamServer};
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::{
        io::{
            AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _,
            BufReader as TokioBufReader,
        },
        net::{TcpListener, TcpStream},
        sync::Notify,
        task::JoinSet,
        time::Instant,
    };
    use uuid::Uuid;

    use super::{
        AndroidDiscoveryMode, AndroidStartupBackend, AppiumServerConfig, BrowserKind,
        CappedJsonLength, ConnectionState, DEFAULT_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS,
        DaemonConfig, DaemonStartupError, DesktopConfigValues, DesktopDiscoveryMode,
        DesktopStartupBackend, DesktopStartupConfig, DiagnosticStatus, DispatchResources,
        EvidenceCleanup, HarmonyDiscoveryMode, HarmonyStartupBackend, InlineDispatchOutcome,
        IosConfigValues, IosDriverBackendConfig, IosManagedPolicy, IosSessionTarget,
        IosStartupConfig, MAX_FRAME_BYTES, ManagedIosHostConfig, NativePlatformConfigValues,
        PlaywrightConfigValues, Registry, RequestRegistration, RequestRegistry,
        appium_doctor_check, bounded_response_frame, bounded_session_export_page_value,
        capped_serialized_json_length, cleanup_connection, clear_ended_session, connection_owner,
        decode_request, dispatch, dispatch_controlled, dispatch_controlled_with_evidence,
        dispatch_inline_until_shutdown, dispatch_routed, ios_doctor_skips_wda_host_checks,
        ios_hotplug_retry_delay, parse_plugin_startup, parse_rdp_startup,
        parse_remote_security_startup, parse_rpc_listen, queue_response, read_bounded_line,
        register_android_from_backend, register_desktop_from_backend,
        register_harmony_from_backend, register_ios_device, runtime_error,
        serve_loopback_connection, serve_loopback_listener, server_features,
        session_export_page_response_length, shutdown_runtime, shutdown_runtime_with_grace,
        supported_protocol_offer, system_android_configs, system_harmony_configs,
    };

    #[cfg(unix)]
    use super::{
        RemoteSecurity, authorize_remote_request, parse_distributed_server_startup,
        parse_distributed_startup, serve_loopback_connection_until_shutdown,
        shutdown_distributed_peer_server, start_distributed_peer_server,
        validate_distributed_topology,
    };

    #[cfg(target_os = "linux")]
    use super::{LinuxDisplayServer, WaylandInputBackend};

    struct FakeAndroidBackend {
        report: Result<AdbDiscoveryReport, &'static str>,
        discovery_calls: AtomicUsize,
        build_order: Mutex<Vec<String>>,
    }

    impl FakeAndroidBackend {
        fn new(report: Result<AdbDiscoveryReport, &'static str>) -> Self {
            Self {
                report,
                discovery_calls: AtomicUsize::new(0),
                build_order: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl AndroidStartupBackend for FakeAndroidBackend {
        async fn discover(&self) -> Result<AdbDiscoveryReport, &'static str> {
            self.discovery_calls.fetch_add(1, Ordering::SeqCst);
            self.report.clone()
        }

        async fn build_route(
            &self,
            descriptor: DiscoveredAndroidDevice,
        ) -> Result<(Arc<dyn DeviceDriver>, DeviceInfo), &'static str> {
            self.build_order
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(descriptor.serial.as_str().to_owned());
            let info = descriptor.device_info();
            let driver: Arc<dyn DeviceDriver> = Arc::new(MockDriver::new(info.id.0.clone()));
            Ok((driver, info))
        }
    }

    fn android_descriptor(serial: &str) -> DiscoveredAndroidDevice {
        DiscoveredAndroidDevice {
            serial: AdbSerial::parse(serial).expect("test serial"),
            state: AdbDeviceState::Ready,
            product: Some("fixture".to_owned()),
            model: Some(format!("model_{serial}")),
            device: None,
            transport_id: None,
            extensions: Default::default(),
        }
    }

    struct FakeHarmonyBackend {
        report: Result<HarmonyDiscoveryReport, &'static str>,
        discovery_calls: AtomicUsize,
        build_order: Mutex<Vec<String>>,
    }

    impl FakeHarmonyBackend {
        fn new(report: Result<HarmonyDiscoveryReport, &'static str>) -> Self {
            Self {
                report,
                discovery_calls: AtomicUsize::new(0),
                build_order: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl HarmonyStartupBackend for FakeHarmonyBackend {
        async fn discover(&self) -> Result<HarmonyDiscoveryReport, &'static str> {
            self.discovery_calls.fetch_add(1, Ordering::SeqCst);
            self.report.clone()
        }

        async fn build_route(
            &self,
            descriptor: DiscoveredHarmonyDevice,
        ) -> (Arc<dyn DeviceDriver>, DeviceInfo) {
            self.build_order
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(descriptor.target.as_str().to_owned());
            let info = descriptor.device_info(false);
            let driver: Arc<dyn DeviceDriver> = Arc::new(MockDriver::new(info.id.0.clone()));
            (driver, info)
        }
    }

    fn harmony_descriptor(target: &str, state: HdcTargetState) -> DiscoveredHarmonyDevice {
        DiscoveredHarmonyDevice {
            target: HdcTarget::parse(target).expect("test HDC target"),
            state,
            name: Some(format!("Harmony {target}")),
            os_version: Some("5.0".to_owned()),
            extensions: Default::default(),
        }
    }

    struct FakeDesktopBackend {
        route: Result<DeviceInfo, &'static str>,
        discovery_calls: AtomicUsize,
        timeout_budgets_ms: Mutex<Vec<u64>>,
    }

    impl FakeDesktopBackend {
        fn new(route: Result<DeviceInfo, &'static str>) -> Self {
            Self {
                route,
                discovery_calls: AtomicUsize::new(0),
                timeout_budgets_ms: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl DesktopStartupBackend for FakeDesktopBackend {
        async fn discover(
            &self,
            _config: &DesktopStartupConfig,
            control: &ExecutionControl,
        ) -> Result<(Arc<dyn DeviceDriver>, DeviceInfo), &'static str> {
            self.discovery_calls.fetch_add(1, Ordering::SeqCst);
            self.timeout_budgets_ms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(control.timeout().map_or(0, |(_, timeout_ms)| timeout_ms));
            let info = self.route.clone()?;
            let driver: Arc<dyn DeviceDriver> = Arc::new(MockDriver::new(info.id.0.clone()));
            Ok((driver, info))
        }
    }

    fn desktop_info(id: &str, platform: Platform) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: "Local desktop".to_owned(),
            platform,
            os_version: None,
            connected: false,
        }
    }

    struct ProtectedTestDriver {
        inner: MockDriver,
        connected: AtomicBool,
    }

    impl ProtectedTestDriver {
        fn new(id: &str) -> Self {
            Self {
                inner: MockDriver::new(id),
                connected: AtomicBool::new(false),
            }
        }

        fn device_info(&self) -> DeviceInfo {
            self.inner.device_info()
        }

        fn protected_observation(&self) -> Observation {
            Observation {
                id: Uuid::new_v4(),
                device_id: self.inner.id().clone(),
                captured_at_ms: now_ms(),
                viewport: Viewport {
                    width: 320,
                    height: 640,
                    scale_factor: 1.0,
                },
                screenshot: None,
                screenshot_omission: Some(ScreenshotOmissionReason::ProtectedAction),
                ui_snapshot: None,
                ui_snapshot_omission: None,
                metadata: Default::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl DeviceDriver for ProtectedTestDriver {
        fn id(&self) -> &DeviceId {
            self.inner.id()
        }

        fn action_protection(&self, name: &str) -> Option<ActionProtection> {
            if name == "inputSecret" {
                Some(ActionProtection::Protected)
            } else {
                self.inner.action_protection(name)
            }
        }

        async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            let info = self.inner.connect(control).await?;
            self.connected.store(true, Ordering::SeqCst);
            Ok(info)
        }

        async fn disconnect(&self, control: &ExecutionControl) -> DriverResult<()> {
            self.inner.disconnect(control).await?;
            self.connected.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn capabilities(
            &self,
            control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            let mut capabilities = self.inner.capabilities(control).await?;
            capabilities.push(ActionDefinition {
                name: "inputSecret".to_owned(),
                description: "Type one protected test secret".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["secret"],
                    "properties": {
                        "secret": { "type": "string", "minLength": 1, "maxLength": 1024 }
                    }
                }),
                protection: ActionProtection::Protected,
            });
            Ok(capabilities)
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
            if call.name != "inputSecret" {
                return self.inner.execute(context, call).await;
            }
            if !self.connected.load(Ordering::SeqCst) {
                return Err(DriverError::NotConnected(self.id().clone()).into());
            }
            if context.screenshot_policy() != ScreenshotPolicy::Omit {
                return Err(DriverError::Protocol(
                    "protected test action did not receive screenshot omission policy".to_owned(),
                )
                .into());
            }
            let fields =
                call.arguments
                    .as_object()
                    .ok_or_else(|| DriverError::InvalidArguments {
                        action: "inputSecret".to_owned(),
                        message: "arguments must be an object".to_owned(),
                    })?;
            if fields.len() != 1 {
                return Err(DriverError::InvalidArguments {
                    action: "inputSecret".to_owned(),
                    message: "arguments must contain only secret".to_owned(),
                }
                .into());
            }
            let byte_length = fields
                .get("secret")
                .and_then(Value::as_str)
                .filter(|secret| !secret.is_empty())
                .map(str::len)
                .ok_or_else(|| DriverError::InvalidArguments {
                    action: "inputSecret".to_owned(),
                    message: "secret must be a non-empty string".to_owned(),
                })?;
            let before = self.protected_observation();
            let started_at_ms = now_ms();
            let after = self.protected_observation();
            Ok(ActionResult {
                call_id: call.id,
                started_at_ms,
                finished_at_ms: now_ms().max(started_at_ms),
                output: json!({ "accepted": true, "byteLength": byte_length }),
                before: Some(before),
                after: Some(after),
                evidence: Vec::new(),
                execution: None,
            })
        }
    }

    struct MismatchedProtectionTestDriver {
        inner: MockDriver,
        capability_calls: AtomicUsize,
        execute_calls: AtomicUsize,
    }

    impl MismatchedProtectionTestDriver {
        fn new(id: &str) -> Self {
            Self {
                inner: MockDriver::new(id),
                capability_calls: AtomicUsize::new(0),
                execute_calls: AtomicUsize::new(0),
            }
        }

        fn device_info(&self) -> DeviceInfo {
            self.inner.device_info()
        }
    }

    #[async_trait::async_trait]
    impl DeviceDriver for MismatchedProtectionTestDriver {
        fn id(&self) -> &DeviceId {
            self.inner.id()
        }

        fn action_protection(&self, name: &str) -> Option<ActionProtection> {
            if name == "misclassifiedSecret" {
                Some(ActionProtection::Standard)
            } else {
                self.inner.action_protection(name)
            }
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
            self.capability_calls.fetch_add(1, Ordering::SeqCst);
            let mut capabilities = self.inner.capabilities(control).await?;
            capabilities.push(ActionDefinition {
                name: "misclassifiedSecret".to_owned(),
                description: "Deliberately mismatched protected action".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["secret"],
                    "properties": { "secret": { "type": "string" } }
                }),
                protection: ActionProtection::Protected,
            });
            Ok(capabilities)
        }

        async fn health_check(&self, _control: &ExecutionControl) -> DriverResult<()> {
            // Keep daemon admission from implicitly discovering capabilities;
            // the regression must exercise DeviceRuntime::execute directly.
            Ok(())
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
            if call.name == "misclassifiedSecret" {
                self.execute_calls.fetch_add(1, Ordering::SeqCst);
                return Err(DriverError::Internal(
                    "misclassified protected action reached Driver I/O".to_owned(),
                )
                .into());
            }
            self.inner.execute(context, call).await
        }
    }

    struct FlakyReleaseStore {
        inner: Arc<FileEvidenceStore>,
        events: Arc<MemoryEventStore>,
        session_id: SessionId,
        failures_remaining: AtomicUsize,
        release_saw_deleted_log: AtomicBool,
    }

    struct PendingReleaseGuard {
        dropped: Arc<AtomicBool>,
    }

    impl Drop for PendingReleaseGuard {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct BlockingReleaseStore {
        inner: Arc<FileEvidenceStore>,
        release_started: Arc<Notify>,
        release_dropped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl EvidenceStore for BlockingReleaseStore {
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
            _session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            let _guard = PendingReleaseGuard {
                dropped: Arc::clone(&self.release_dropped),
            };
            self.release_started.notify_one();
            std::future::pending().await
        }

        async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
            self.inner.gc(policy).await
        }
    }

    #[async_trait::async_trait]
    impl EvidenceStore for FlakyReleaseStore {
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
            if matches!(
                self.events.export_session(&self.session_id).await,
                Err(devicerail_core::EventStoreError::SessionNotFound(_))
            ) {
                self.release_saw_deleted_log.store(true, Ordering::SeqCst);
            }
            if self
                .failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(EvidenceError::Internal(
                    "injected release failure".to_owned(),
                ));
            }
            self.inner.release_session(session_id, released_at_ms).await
        }

        async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
            self.inner.gc(policy).await
        }
    }

    struct CancelOnAttachStore {
        inner: Arc<FileEvidenceStore>,
        controller: ExecutionController,
        attaches: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl EvidenceStore for CancelOnAttachStore {
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
            let stored = self.inner.attach(session_id, asset).await?;
            self.attaches.fetch_add(1, Ordering::SeqCst);
            self.controller.cancel(CancellationReason::Requested);
            Ok(stored)
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

    async fn test_context() -> (Registry, Arc<MemoryEventStore>) {
        test_context_with_drivers(vec![MockDriver::new("mock-test")]).await
    }

    async fn delayed_test_context(delay: Duration) -> (Registry, Arc<MemoryEventStore>) {
        test_context_with_drivers(vec![MockDriver::new("mock-test").with_action_delay(delay)]).await
    }

    async fn test_context_with_drivers(
        drivers: Vec<MockDriver>,
    ) -> (Registry, Arc<MemoryEventStore>) {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events));
        for driver in drivers {
            let driver = Arc::new(driver);
            let device = driver.device_info();
            runtime
                .register(driver, device)
                .await
                .expect("register test Driver");
        }
        (runtime, events)
    }

    async fn dispatch_managed(
        request: RpcRequest,
        runtime: &Registry,
        events: &MemoryEventStore,
        evidence: &EvidenceCleanup,
        connection: &mut ConnectionState,
    ) -> RpcResponse {
        let (_, control) = super::request_control(request.timeout_ms);
        dispatch_controlled_with_evidence(
            request,
            runtime,
            DispatchResources {
                events,
                evidence,
                streams: None,
            },
            connection,
            &control,
            &RequestRegistry::default(),
        )
        .await
    }

    #[derive(Clone, Copy)]
    enum DisconnectBehavior {
        Fail,
        Pending,
    }

    struct ShutdownTestDriver {
        inner: MockDriver,
        behavior: DisconnectBehavior,
        attempted: Arc<AtomicBool>,
    }

    impl ShutdownTestDriver {
        fn new(id: &str, behavior: DisconnectBehavior, attempted: Arc<AtomicBool>) -> Self {
            Self {
                inner: MockDriver::new(id),
                behavior,
                attempted,
            }
        }

        fn device_info(&self) -> DeviceInfo {
            self.inner.device_info()
        }
    }

    #[async_trait::async_trait]
    impl DeviceDriver for ShutdownTestDriver {
        fn id(&self) -> &DeviceId {
            self.inner.id()
        }

        fn action_protection(&self, name: &str) -> Option<ActionProtection> {
            self.inner.action_protection(name)
        }

        async fn connect(&self, control: &ExecutionControl) -> DriverResult<DeviceInfo> {
            self.inner.connect(control).await
        }

        async fn disconnect(&self, _control: &ExecutionControl) -> DriverResult<()> {
            self.attempted.store(true, Ordering::SeqCst);
            match self.behavior {
                DisconnectBehavior::Fail => Err(DriverError::Internal(
                    "forced disconnect failure".to_owned(),
                )),
                DisconnectBehavior::Pending => std::future::pending().await,
            }
        }

        async fn capabilities(
            &self,
            control: &ExecutionControl,
        ) -> DriverResult<Vec<ActionDefinition>> {
            self.inner.capabilities(control).await
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

    struct BlockingHealthMediaDriver {
        inner: MockDriver,
        block_health: AtomicBool,
        health_calls: AtomicUsize,
    }

    impl BlockingHealthMediaDriver {
        fn new(id: &str) -> Self {
            Self {
                inner: MockDriver::new(id).with_session_evidence(),
                block_health: AtomicBool::new(false),
                health_calls: AtomicUsize::new(0),
            }
        }

        fn device_info(&self) -> DeviceInfo {
            self.inner.device_info()
        }
    }

    #[async_trait::async_trait]
    impl DeviceDriver for BlockingHealthMediaDriver {
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
            self.health_calls.fetch_add(1, Ordering::SeqCst);
            if self.block_health.load(Ordering::SeqCst) {
                std::future::pending().await
            } else {
                self.inner.health_check(control).await
            }
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

    fn request(id: u64, method: &str, params: serde_json::Value) -> RpcRequest {
        RpcRequest {
            jsonrpc: JsonRpcVersion::V2,
            id: RpcId::Number(id),
            method: method.to_owned(),
            timeout_ms: None,
            params: Some(
                serde_json::from_value::<RpcParams>(params)
                    .expect("test request params must be structured"),
            ),
        }
    }

    fn hello_request(protocol: ProtocolOffer, required: &[&str], optional: &[&str]) -> RpcRequest {
        let params = HelloParams {
            client: PeerInfo {
                name: "test-client".to_owned(),
                version: "0.1.0".to_owned(),
            },
            protocol,
            features: FeatureOffer {
                required: required.iter().map(|value| (*value).to_owned()).collect(),
                optional: optional.iter().map(|value| (*value).to_owned()).collect(),
            },
        };
        request(
            1,
            "system.hello",
            serde_json::to_value(params).expect("serialize hello params"),
        )
    }

    fn daemon_config_without_playwright(
        evidence_dir: Option<OsString>,
        android_mode: Option<OsString>,
        adb_path: Option<OsString>,
        screenshot_policy: Option<OsString>,
    ) -> Result<DaemonConfig, DaemonStartupError> {
        DaemonConfig::from_values(
            evidence_dir,
            android_mode,
            adb_path,
            NativePlatformConfigValues::default(),
            screenshot_policy,
            PlaywrightConfigValues::default(),
        )
    }

    fn daemon_config_with_platforms(
        harmony_mode: Option<OsString>,
        hdc_path: Option<OsString>,
        ios_values: IosConfigValues,
    ) -> Result<DaemonConfig, DaemonStartupError> {
        DaemonConfig::from_values(
            None,
            Some(OsString::from("off")),
            None,
            NativePlatformConfigValues {
                harmony_mode,
                hdc_path,
                ios: ios_values,
                desktop: DesktopConfigValues::default(),
            },
            None,
            PlaywrightConfigValues::default(),
        )
    }

    fn daemon_config_with_desktop(
        desktop: DesktopConfigValues,
    ) -> Result<DaemonConfig, DaemonStartupError> {
        DaemonConfig::from_values(
            None,
            Some(OsString::from("off")),
            None,
            NativePlatformConfigValues {
                harmony_mode: None,
                hdc_path: None,
                ios: IosConfigValues::default(),
                desktop,
            },
            None,
            PlaywrightConfigValues::default(),
        )
    }

    #[test]
    fn daemon_config_parses_android_mode_without_mutating_process_environment() {
        let default =
            daemon_config_without_playwright(None, None, None, None).expect("default config");
        assert_eq!(default.android_mode, AndroidDiscoveryMode::Auto);
        assert_eq!(
            default.evidence_dir,
            std::path::PathBuf::from(".devicerail/evidence")
        );
        assert_eq!(default.adb_path, std::path::PathBuf::from("adb"));
        assert_eq!(default.harmony_mode, HarmonyDiscoveryMode::Off);
        assert_eq!(default.hdc_path, std::path::PathBuf::from("hdc"));
        assert_eq!(default.ios, None);
        assert_eq!(default.desktop.mode, DesktopDiscoveryMode::Off);
        assert_eq!(default.desktop.identity.id.0, "desktop-local");
        assert_eq!(default.desktop.identity.name, "Local desktop");
        assert_eq!(
            default.desktop.system.command_timeout,
            Duration::from_secs(30)
        );
        assert_eq!(default.screenshot_policy, ScreenshotPolicy::Capture);

        for (value, expected) in [
            ("auto", AndroidDiscoveryMode::Auto),
            ("off", AndroidDiscoveryMode::Off),
            ("required", AndroidDiscoveryMode::Required),
        ] {
            let parsed = daemon_config_without_playwright(
                Some(OsString::from("evidence")),
                Some(OsString::from(value)),
                Some(OsString::from("custom-adb")),
                Some(OsString::from("omit")),
            )
            .expect("valid config");
            assert_eq!(parsed.android_mode, expected);
            assert_eq!(parsed.screenshot_policy, ScreenshotPolicy::Omit);
        }

        assert_eq!(
            daemon_config_without_playwright(None, Some(OsString::from("sometimes")), None, None),
            Err(DaemonStartupError::InvalidAndroidMode)
        );
        assert_eq!(
            daemon_config_without_playwright(None, Some(OsString::new()), None, None),
            Err(DaemonStartupError::InvalidAndroidMode)
        );
        assert_eq!(
            daemon_config_without_playwright(Some(OsString::new()), None, None, None),
            Err(DaemonStartupError::InvalidEvidenceDirectory)
        );
        assert_eq!(
            daemon_config_without_playwright(None, None, Some(OsString::new()), None),
            Err(DaemonStartupError::InvalidAdbPath)
        );
        assert_eq!(
            daemon_config_without_playwright(None, None, None, Some(OsString::from("sometimes")),),
            Err(DaemonStartupError::InvalidScreenshotPolicy)
        );
    }

    #[test]
    fn daemon_config_desktop_is_explicit_closed_bounded_and_redacted() {
        for (value, expected) in [
            ("auto", DesktopDiscoveryMode::Auto),
            ("required", DesktopDiscoveryMode::Required),
        ] {
            let parsed = daemon_config_with_desktop(DesktopConfigValues {
                mode: Some(OsString::from(value)),
                ..DesktopConfigValues::default()
            })
            .expect("enabled desktop config");
            assert_eq!(parsed.desktop.mode, expected);
            assert_eq!(parsed.desktop.identity.id.0, "desktop-local");
            assert_eq!(parsed.desktop.identity.name, "Local desktop");
            assert_eq!(
                parsed.desktop.system.command_timeout,
                Duration::from_secs(30)
            );
        }
        for value in ["sometimes", "", "AUTO"] {
            assert_eq!(
                daemon_config_with_desktop(DesktopConfigValues {
                    mode: Some(OsString::from(value)),
                    ..DesktopConfigValues::default()
                }),
                Err(DaemonStartupError::InvalidDesktopMode)
            );
        }

        let orphan_settings = [
            DesktopConfigValues {
                id: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                name: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                os_version: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                command_timeout_ms: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                macos_screencapture: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                windows_powershell: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                linux_display_server: Some(OsString::from("x11")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                x11_import: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                x11_xdotool: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_grim: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_input: Some(OsString::from("auto")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_ydotool: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_wtype: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_viewport_width: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_viewport_height: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                wayland_viewport_scale_factor: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
        ];
        for orphan in orphan_settings {
            assert_eq!(
                daemon_config_with_desktop(orphan),
                Err(DaemonStartupError::DesktopModeRequiredForSettings)
            );
        }
        assert_eq!(
            daemon_config_with_desktop(DesktopConfigValues {
                mode: Some(OsString::from("off")),
                id: Some(OsString::from("orphan")),
                ..DesktopConfigValues::default()
            }),
            Err(DaemonStartupError::DesktopModeRequiredForSettings)
        );

        for value in ["1", "300000"] {
            let parsed = daemon_config_with_desktop(DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                command_timeout_ms: Some(OsString::from(value)),
                ..DesktopConfigValues::default()
            })
            .expect("bounded desktop timeout");
            assert_eq!(
                parsed.desktop.system.command_timeout,
                Duration::from_millis(value.parse().expect("test timeout"))
            );
        }
        for value in ["", "0", "300001", "-1", "1.5"] {
            assert_eq!(
                daemon_config_with_desktop(DesktopConfigValues {
                    mode: Some(OsString::from("auto")),
                    command_timeout_ms: Some(OsString::from(value)),
                    ..DesktopConfigValues::default()
                }),
                Err(DaemonStartupError::InvalidDesktopConfiguration)
            );
        }
        for (id, name, os_version) in [
            ("", "desktop", None),
            ("desktop", "\n", None),
            ("desktop", "desktop", Some("")),
        ] {
            assert_eq!(
                daemon_config_with_desktop(DesktopConfigValues {
                    mode: Some(OsString::from("auto")),
                    id: Some(OsString::from(id)),
                    name: Some(OsString::from(name)),
                    os_version: os_version.map(OsString::from),
                    ..DesktopConfigValues::default()
                }),
                Err(DaemonStartupError::InvalidDesktopConfiguration)
            );
        }

        let secret = "DESKTOP-STARTUP-SECRET-SENTINEL";
        let mut values = DesktopConfigValues {
            mode: Some(OsString::from("auto")),
            id: Some(OsString::from("desktop-secret")),
            name: Some(OsString::from(secret)),
            os_version: Some(OsString::from(secret)),
            ..DesktopConfigValues::default()
        };
        if cfg!(target_os = "macos") {
            values.macos_screencapture = Some(OsString::from(format!("/{secret}")));
        } else if cfg!(target_os = "windows") {
            values.windows_powershell = Some(OsString::from(format!(r"C:\{secret}.exe")));
        } else if cfg!(target_os = "linux") {
            values.linux_display_server = Some(OsString::from("x11"));
            values.x11_import = Some(OsString::from(format!("/{secret}")));
        }
        let config = daemon_config_with_desktop(values).expect("redacted desktop config");
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(secret));
        assert!(!debug.contains("desktop-secret"));
    }

    #[test]
    fn daemon_config_desktop_rejects_wrong_host_settings_and_empty_host_paths() {
        let mut wrong_host_settings = Vec::new();
        if !cfg!(target_os = "macos") {
            wrong_host_settings.push(DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                macos_screencapture: Some(OsString::from("/usr/sbin/screencapture")),
                ..DesktopConfigValues::default()
            });
        }
        if !cfg!(target_os = "windows") {
            wrong_host_settings.push(DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                windows_powershell: Some(OsString::from("powershell.exe")),
                ..DesktopConfigValues::default()
            });
        }
        if !cfg!(target_os = "linux") {
            wrong_host_settings.push(DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("x11")),
                ..DesktopConfigValues::default()
            });
        }
        for wrong_host in wrong_host_settings {
            assert_eq!(
                daemon_config_with_desktop(wrong_host),
                Err(DaemonStartupError::DesktopSettingUnsupportedOnHost)
            );
        }

        let mut empty_paths = Vec::new();
        if cfg!(target_os = "macos") {
            empty_paths.push(DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                macos_screencapture: Some(OsString::new()),
                ..DesktopConfigValues::default()
            });
        } else if cfg!(target_os = "windows") {
            empty_paths.push(DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                windows_powershell: Some(OsString::new()),
                ..DesktopConfigValues::default()
            });
        } else if cfg!(target_os = "linux") {
            empty_paths.extend([
                DesktopConfigValues {
                    mode: Some(OsString::from("auto")),
                    linux_display_server: Some(OsString::from("x11")),
                    x11_import: Some(OsString::new()),
                    ..DesktopConfigValues::default()
                },
                DesktopConfigValues {
                    mode: Some(OsString::from("auto")),
                    linux_display_server: Some(OsString::from("x11")),
                    x11_xdotool: Some(OsString::new()),
                    ..DesktopConfigValues::default()
                },
            ]);
            for field in ["grim", "ydotool", "wtype"] {
                let mut values = DesktopConfigValues {
                    mode: Some(OsString::from("auto")),
                    linux_display_server: Some(OsString::from("wayland")),
                    wayland_viewport_width: Some(OsString::from("1")),
                    wayland_viewport_height: Some(OsString::from("1")),
                    wayland_viewport_scale_factor: Some(OsString::from("1")),
                    ..DesktopConfigValues::default()
                };
                match field {
                    "grim" => values.wayland_grim = Some(OsString::new()),
                    "ydotool" => values.wayland_ydotool = Some(OsString::new()),
                    "wtype" => values.wayland_wtype = Some(OsString::new()),
                    _ => unreachable!("closed test field"),
                }
                empty_paths.push(values);
            }
        }
        for empty_path in empty_paths {
            assert_eq!(
                daemon_config_with_desktop(empty_path),
                Err(DaemonStartupError::InvalidDesktopConfiguration)
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn daemon_config_desktop_linux_profiles_and_wayland_viewport_are_closed() {
        for (value, expected) in [
            ("auto", None),
            ("ydotool", Some(WaylandInputBackend::Ydotool)),
            ("wtype", Some(WaylandInputBackend::Wtype)),
        ] {
            let config = daemon_config_with_desktop(DesktopConfigValues {
                mode: Some(OsString::from("required")),
                linux_display_server: Some(OsString::from("wayland")),
                wayland_input: Some(OsString::from(value)),
                wayland_viewport_width: Some(OsString::from("1920")),
                wayland_viewport_height: Some(OsString::from("1080")),
                wayland_viewport_scale_factor: Some(OsString::from("1.5")),
                ..DesktopConfigValues::default()
            })
            .expect("valid explicit Wayland config");
            assert_eq!(
                config.desktop.system.linux_display_server,
                Some(LinuxDisplayServer::Wayland)
            );
            assert_eq!(config.desktop.system.wayland_input_backend, expected);
            assert_eq!(
                config.desktop.system.wayland_viewport,
                Some(Viewport {
                    width: 1920,
                    height: 1080,
                    scale_factor: 1.5,
                })
            );
        }

        let invalid = [
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                x11_import: Some(OsString::from("import")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("wayland")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("x11")),
                wayland_viewport_width: Some(OsString::from("1")),
                wayland_viewport_height: Some(OsString::from("1")),
                wayland_viewport_scale_factor: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                wayland_viewport_width: Some(OsString::from("1")),
                wayland_viewport_height: Some(OsString::from("1")),
                wayland_viewport_scale_factor: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("x11")),
                wayland_grim: Some(OsString::from("grim")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("wayland")),
                x11_xdotool: Some(OsString::from("xdotool")),
                wayland_viewport_width: Some(OsString::from("1")),
                wayland_viewport_height: Some(OsString::from("1")),
                wayland_viewport_scale_factor: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("wayland")),
                wayland_viewport_width: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
            DesktopConfigValues {
                mode: Some(OsString::from("auto")),
                linux_display_server: Some(OsString::from("wayland")),
                wayland_input: Some(OsString::from("fallback")),
                wayland_viewport_width: Some(OsString::from("1")),
                wayland_viewport_height: Some(OsString::from("1")),
                wayland_viewport_scale_factor: Some(OsString::from("1")),
                ..DesktopConfigValues::default()
            },
        ];
        for values in invalid {
            assert_eq!(
                daemon_config_with_desktop(values),
                Err(DaemonStartupError::InvalidDesktopConfiguration)
            );
        }
    }

    #[test]
    fn daemon_config_harmony_is_explicit_closed_and_bounded() {
        let disabled = daemon_config_with_platforms(None, None, IosConfigValues::default())
            .expect("HarmonyOS disabled by default");
        assert_eq!(disabled.harmony_mode, HarmonyDiscoveryMode::Off);
        assert_eq!(disabled.hdc_path, PathBuf::from("hdc"));

        for (value, expected) in [
            ("auto", HarmonyDiscoveryMode::Auto),
            ("required", HarmonyDiscoveryMode::Required),
        ] {
            let parsed = daemon_config_with_platforms(
                Some(OsString::from(value)),
                None,
                IosConfigValues::default(),
            )
            .expect("enabled HarmonyOS config");
            assert_eq!(parsed.harmony_mode, expected);
            assert_eq!(parsed.hdc_path, PathBuf::from("hdc"));
        }
        assert_eq!(
            daemon_config_with_platforms(
                Some(OsString::from("sometimes")),
                None,
                IosConfigValues::default(),
            ),
            Err(DaemonStartupError::InvalidHarmonyMode)
        );
        assert_eq!(
            daemon_config_with_platforms(
                None,
                Some(OsString::from("custom-hdc")),
                IosConfigValues::default(),
            ),
            Err(DaemonStartupError::HarmonyModeRequiredForHdcPath)
        );
        assert_eq!(
            daemon_config_with_platforms(
                Some(OsString::from("off")),
                Some(OsString::from("custom-hdc")),
                IosConfigValues::default(),
            ),
            Err(DaemonStartupError::HarmonyModeRequiredForHdcPath)
        );
        assert_eq!(
            daemon_config_with_platforms(
                Some(OsString::from("auto")),
                Some(OsString::new()),
                IosConfigValues::default(),
            ),
            Err(DaemonStartupError::InvalidHdcPath)
        );
    }

    #[test]
    fn daemon_config_ios_is_explicit_loopback_only_and_redacted() {
        let endpoint_secret = "WDA-ENDPOINT-SECRET";
        let device_secret = "IOS-DEVICE-TOKEN-SECRET";
        let config = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                wda_endpoint: Some(OsString::from(format!(
                    "http://127.0.0.1:8100/{endpoint_secret}"
                ))),
                device_token: Some(OsString::from(device_secret)),
                device_name: Some(OsString::from("Test iPhone")),
                os_version: Some(OsString::from("18.0")),
                mjpeg_endpoint: Some(OsString::from("http://[::1]:9100/mjpeg")),
                ..IosConfigValues::default()
            },
        )
        .expect("valid explicit iOS route");
        let IosStartupConfig::External(ios) = config.ios.as_ref().expect("iOS enabled") else {
            panic!("expected external iOS route");
        };
        assert_eq!(ios.backend, IosDriverBackendConfig::DirectWda);
        assert_eq!(ios.session_target, IosSessionTarget::Native);
        assert_eq!(ios.device.id().0, format!("ios-wda:{device_secret}"));
        assert_eq!(ios.device.name(), "Test iPhone");
        assert_eq!(ios.device.os_version(), Some("18.0"));
        assert!(ios.mjpeg_endpoint.is_some());
        assert!(format!("{:?}", ios.wda_endpoint).contains("65000"));
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(endpoint_secret));
        assert!(!debug.contains(device_secret));

        for orphan in [
            IosConfigValues {
                device_token: Some(OsString::from("orphan")),
                ..IosConfigValues::default()
            },
            IosConfigValues {
                device_name: Some(OsString::from("orphan")),
                ..IosConfigValues::default()
            },
            IosConfigValues {
                os_version: Some(OsString::from("18.0")),
                ..IosConfigValues::default()
            },
            IosConfigValues {
                mjpeg_endpoint: Some(OsString::from("http://127.0.0.1:9100")),
                ..IosConfigValues::default()
            },
        ] {
            assert_eq!(
                daemon_config_with_platforms(None, None, orphan),
                Err(DaemonStartupError::IosWdaEndpointRequired)
            );
        }
        assert_eq!(
            daemon_config_with_platforms(
                None,
                None,
                IosConfigValues {
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    ..IosConfigValues::default()
                },
            ),
            Err(DaemonStartupError::IosDeviceTokenRequired)
        );

        for endpoint in [
            "http://localhost:8100",
            "http://192.0.2.1:8100",
            "https://127.0.0.1:8100",
            "http://user:secret@127.0.0.1:8100",
            "http://127.0.0.1:8100?token=secret",
        ] {
            assert_eq!(
                daemon_config_with_platforms(
                    None,
                    None,
                    IosConfigValues {
                        wda_endpoint: Some(OsString::from(endpoint)),
                        device_token: Some(OsString::from("valid-token")),
                        ..IosConfigValues::default()
                    },
                ),
                Err(DaemonStartupError::InvalidIosConfiguration),
                "endpoint should fail closed: {endpoint}"
            );
        }
        assert_eq!(
            daemon_config_with_platforms(
                None,
                None,
                IosConfigValues {
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    device_token: Some(OsString::from("valid-token")),
                    mjpeg_endpoint: Some(OsString::from("http://192.0.2.2:9100")),
                    ..IosConfigValues::default()
                },
            ),
            Err(DaemonStartupError::InvalidIosConfiguration)
        );
        for (device_token, device_name) in [("../phone", "phone"), ("phone", "\n")] {
            assert_eq!(
                daemon_config_with_platforms(
                    None,
                    None,
                    IosConfigValues {
                        wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                        device_token: Some(OsString::from(device_token)),
                        device_name: Some(OsString::from(device_name)),
                        ..IosConfigValues::default()
                    },
                ),
                Err(DaemonStartupError::InvalidIosConfiguration)
            );
        }
    }

    #[test]
    fn daemon_config_ios_appium_backend_is_closed_exclusive_loopback_and_redacted() {
        let appium_secret = "APPIUM-ENDPOINT-SECRET";
        let device_secret = "APPIUM-IOS-DEVICE-SECRET";
        let external = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                backend: Some(OsString::from("appium")),
                session_target: Some(OsString::from("safari")),
                appium_endpoint: Some(OsString::from(format!(
                    "http://127.0.0.1:4723/{appium_secret}"
                ))),
                appium_new_command_timeout_seconds: Some(OsString::from("601")),
                wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                device_token: Some(OsString::from(device_secret)),
                device_name: Some(OsString::from("Appium iPhone")),
                os_version: Some(OsString::from("18.0")),
                ..IosConfigValues::default()
            },
        )
        .expect("valid external Appium route");
        let IosStartupConfig::External(external_ios) =
            external.ios.as_ref().expect("external Appium enabled")
        else {
            panic!("expected external Appium route");
        };
        assert!(matches!(
            external_ios.backend,
            IosDriverBackendConfig::Appium { .. }
        ));
        assert_eq!(external_ios.session_target, IosSessionTarget::Safari);
        assert!(matches!(
            &external_ios.backend,
            IosDriverBackendConfig::Appium {
                new_command_timeout_seconds: 601,
                ..
            }
        ));
        let debug = format!("{external:?}");
        assert!(debug.contains("Appium"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(appium_secret));
        assert!(!debug.contains(device_secret));

        let bundled_wda = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                backend: Some(OsString::from("appium")),
                appium_endpoint: Some(OsString::from("http://127.0.0.1:4723")),
                device_token: Some(OsString::from("bundled-wda-device")),
                ..IosConfigValues::default()
            },
        )
        .expect("Appium may manage its bundled WDA");
        let IosStartupConfig::External(bundled_wda) = bundled_wda.ios.expect("iOS enabled") else {
            panic!("expected external Appium route");
        };
        assert!(bundled_wda.wda_endpoint.is_none());
        assert_eq!(bundled_wda.session_target, IosSessionTarget::Native);
        let IosDriverBackendConfig::Appium {
            new_command_timeout_seconds,
            ..
        } = bundled_wda.backend
        else {
            panic!("expected bundled Appium backend");
        };
        assert_eq!(
            new_command_timeout_seconds,
            DEFAULT_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS
        );

        let managed = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                mode: Some(OsString::from("required")),
                backend: Some(OsString::from("appium")),
                appium_endpoint: Some(OsString::from("http://[::1]:4723/wd/hub")),
                appium_new_command_timeout_seconds: Some(OsString::from("602")),
                wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                ..IosConfigValues::default()
            },
        )
        .expect("valid managed Appium route");
        let IosStartupConfig::Managed(managed_ios) = managed.ios.expect("managed Appium enabled")
        else {
            panic!("expected managed Appium route");
        };
        assert_eq!(managed_ios.policy, IosManagedPolicy::Required);
        assert!(matches!(
            &managed_ios.backend,
            IosDriverBackendConfig::Appium { .. }
        ));
        assert_eq!(managed_ios.session_target, IosSessionTarget::Native);
        assert!(matches!(
            &managed_ios.backend,
            IosDriverBackendConfig::Appium {
                new_command_timeout_seconds: 602,
                ..
            }
        ));

        let discovery_only = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                mode: Some(OsString::from("auto")),
                backend: Some(OsString::from("appium")),
                appium_endpoint: Some(OsString::from("http://127.0.0.1:4723")),
                appium_new_command_timeout_seconds: Some(OsString::from("603")),
                device_token: Some(OsString::from("discovery-only-device")),
                ..IosConfigValues::default()
            },
        )
        .expect("Appium managed mode does not require a standalone WDA checkout");
        let IosStartupConfig::Managed(discovery_only) = discovery_only.ios.expect("iOS enabled")
        else {
            panic!("expected managed Appium discovery");
        };
        assert!(matches!(
            discovery_only.host,
            ManagedIosHostConfig::AppiumDiscovery {
                device_udid: Some(ref value)
            } if value == "discovery-only-device"
        ));
        assert!(matches!(
            discovery_only.backend,
            IosDriverBackendConfig::Appium {
                new_command_timeout_seconds: 603,
                ..
            }
        ));

        let executable_secret = "/tmp/APPIUM-EXECUTABLE-SECRET/appium";
        let local = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                backend: Some(OsString::from("appium")),
                appium_path: Some(OsString::from(executable_secret)),
                appium_port: Some(OsString::from("0")),
                appium_base_path: Some(OsString::from("/wd/hub")),
                appium_new_command_timeout_seconds: Some(OsString::from("604")),
                wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                device_token: Some(OsString::from("managed-appium-device")),
                ..IosConfigValues::default()
            },
        )
        .expect("valid managed Appium process route");
        let IosStartupConfig::External(local_ios) = local.ios.as_ref().expect("iOS enabled") else {
            panic!("expected external WDA route");
        };
        let IosDriverBackendConfig::Appium {
            server: AppiumServerConfig::Managed(server),
            new_command_timeout_seconds,
        } = &local_ios.backend
        else {
            panic!("expected managed Appium server");
        };
        assert_eq!(server.port(), 0);
        assert_eq!(server.base_path(), "/wd/hub");
        assert_eq!(*new_command_timeout_seconds, 604);
        assert!(!format!("{local:?}").contains(executable_secret));

        for (values, expected) in [
            (
                IosConfigValues {
                    backend: Some(OsString::from("webdriver")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::InvalidIosBackend,
            ),
            (
                IosConfigValues {
                    backend: Some(OsString::from("direct-wda")),
                    appium_endpoint: Some(OsString::from("http://127.0.0.1:4723")),
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    device_token: Some(OsString::from("valid-token")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosAppiumEndpointRequiresBackend,
            ),
            (
                IosConfigValues {
                    backend: Some(OsString::from("appium")),
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    device_token: Some(OsString::from("valid-token")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosAppiumEndpointRequired,
            ),
            (
                IosConfigValues {
                    mode: Some(OsString::from("off")),
                    backend: Some(OsString::from("appium")),
                    appium_endpoint: Some(OsString::from("http://127.0.0.1:4723")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosSettingsWhileDisabled,
            ),
            (
                IosConfigValues {
                    backend: Some(OsString::from("appium")),
                    appium_endpoint: Some(OsString::from("http://127.0.0.1:4723")),
                    appium_path: Some(OsString::from("appium")),
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    device_token: Some(OsString::from("valid-token")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosAppiumServerConflict,
            ),
        ] {
            assert_eq!(
                daemon_config_with_platforms(None, None, values),
                Err(expected)
            );
        }
        assert_eq!(
            daemon_config_with_platforms(
                None,
                None,
                IosConfigValues {
                    session_target: Some(OsString::from("browser")),
                    ..IosConfigValues::default()
                },
            ),
            Err(DaemonStartupError::InvalidIosSessionTarget)
        );
        assert_eq!(
            daemon_config_with_platforms(
                None,
                None,
                IosConfigValues {
                    backend: Some(OsString::from("direct-wda")),
                    session_target: Some(OsString::from("safari")),
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    device_token: Some(OsString::from("valid-token")),
                    ..IosConfigValues::default()
                },
            ),
            Err(DaemonStartupError::IosSessionTargetRequiresAppium)
        );
        for value in ["", "0", "3601", "abc", "1.5", " 600"] {
            assert_eq!(
                daemon_config_with_platforms(
                    None,
                    None,
                    IosConfigValues {
                        backend: Some(OsString::from("appium")),
                        appium_new_command_timeout_seconds: Some(OsString::from(value)),
                        ..IosConfigValues::default()
                    },
                ),
                Err(DaemonStartupError::InvalidIosAppiumNewCommandTimeout),
                "invalid Appium new-command timeout should fail closed: {value:?}"
            );
        }
        assert_eq!(
            daemon_config_with_platforms(
                None,
                None,
                IosConfigValues {
                    appium_new_command_timeout_seconds: Some(OsString::from("600")),
                    ..IosConfigValues::default()
                },
            ),
            Err(DaemonStartupError::IosAppiumEndpointRequiresBackend)
        );
        for (path, port, base_path) in [
            ("", "0", "/"),
            ("appium", "65536", "/"),
            ("appium", "0", "wd/hub"),
            ("appium", "0", "/wd//hub"),
        ] {
            assert_eq!(
                daemon_config_with_platforms(
                    None,
                    None,
                    IosConfigValues {
                        backend: Some(OsString::from("appium")),
                        appium_path: Some(OsString::from(path)),
                        appium_port: Some(OsString::from(port)),
                        appium_base_path: Some(OsString::from(base_path)),
                        wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                        device_token: Some(OsString::from("valid-token")),
                        ..IosConfigValues::default()
                    },
                ),
                Err(DaemonStartupError::InvalidIosAppiumConfiguration)
            );
        }

        for endpoint in [
            "http://localhost:4723",
            "http://192.0.2.1:4723",
            "https://127.0.0.1:4723",
            "http://user:secret@127.0.0.1:4723",
            "http://127.0.0.1:4723?token=secret",
        ] {
            assert_eq!(
                daemon_config_with_platforms(
                    None,
                    None,
                    IosConfigValues {
                        backend: Some(OsString::from("appium")),
                        appium_endpoint: Some(OsString::from(endpoint)),
                        wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                        device_token: Some(OsString::from("valid-token")),
                        ..IosConfigValues::default()
                    },
                ),
                Err(DaemonStartupError::InvalidIosAppiumConfiguration),
                "Appium endpoint should fail closed: {endpoint}"
            );
        }
    }

    #[tokio::test]
    async fn ios_appium_doctor_probes_status_without_exposing_the_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Appium doctor endpoint");
        let address = listener.local_addr().expect("fake Appium doctor address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept Appium doctor probe");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.expect("read doctor probe");
                assert!(read > 0, "doctor probe ended before its headers");
                request.extend_from_slice(&buffer[..read]);
                assert!(request.len() <= 16 * 1024, "doctor probe is bounded");
            }
            assert!(request.starts_with(b"GET /doctor-secret-path/status HTTP/1.1\r\n"));
            let body = br#"{"value":{"ready":true,"build":{"version":"2.0.0"}}}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("write doctor response head");
            stream
                .write_all(body)
                .await
                .expect("write doctor response body");
        });

        let check = appium_doctor_check(
            Some(OsString::from("appium")),
            Some(OsString::from(format!(
                "http://{address}/doctor-secret-path"
            ))),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("valid Appium doctor config")
        .expect("Appium diagnostic");
        assert_eq!(check.status, DiagnosticStatus::Pass);
        assert_eq!(check.code, "ios_appium_ready");
        assert_eq!(
            check.summary,
            "Appium server is ready; XCUITest availability is unverified"
        );
        assert_eq!(check.remediation, None);
        assert!(!format!("{check:?}").contains("doctor-secret-path"));
        server.await.expect("join fake Appium doctor endpoint");

        assert_eq!(
            appium_doctor_check(None, None, None, None, None, None)
                .await
                .expect("default direct-WDA doctor"),
            None
        );
    }

    #[test]
    fn ios_doctor_skips_wda_host_checks_for_operator_owned_appium_wda() {
        assert!(ios_doctor_skips_wda_host_checks(
            Some(OsStr::new("appium")),
            false
        ));
        assert!(!ios_doctor_skips_wda_host_checks(
            Some(OsStr::new("appium")),
            true
        ));
        assert!(!ios_doctor_skips_wda_host_checks(None, false));
    }

    #[test]
    fn daemon_config_ios_managed_mode_is_explicit_bounded_and_redacted() {
        let project_secret = "/tmp/WDA-PROJECT-SECRET/WebDriverAgent.xcodeproj";
        let device_secret = "MANAGED-IOS-DEVICE-SECRET";
        let config = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                mode: Some(OsString::from("auto")),
                device_token: Some(OsString::from(device_secret)),
                wda_project: Some(OsString::from(project_secret)),
                derived_data: Some(OsString::from("managed-derived-data")),
                iproxy_path: Some(OsString::from("custom-iproxy")),
                local_port: Some(OsString::from("0")),
                remote_port: Some(OsString::from("8100")),
                allow_provisioning_updates: Some(OsString::from("false")),
                ..IosConfigValues::default()
            },
        )
        .expect("valid managed iOS route");
        let IosStartupConfig::Managed(ios) = config.ios.as_ref().expect("iOS enabled") else {
            panic!("expected managed iOS route");
        };
        assert_eq!(ios.policy, IosManagedPolicy::Auto);
        assert_eq!(ios.backend, IosDriverBackendConfig::DirectWda);
        let ManagedIosHostConfig::Wda(host) = &ios.host else {
            panic!("explicit WDA project must select managed WDA lifecycle");
        };
        assert_eq!(host.device_udid.as_deref(), Some(device_secret));
        assert_eq!(host.wda_project, PathBuf::from(project_secret));
        assert_eq!(host.derived_data, PathBuf::from("managed-derived-data"));
        assert_eq!(host.iproxy_path, PathBuf::from("custom-iproxy"));
        assert_eq!(host.local_port, 0);
        assert_eq!(host.remote_port, 8100);
        assert!(!host.allow_provisioning_updates);
        let debug = format!("{config:?}");
        assert!(!debug.contains(project_secret));
        assert!(!debug.contains(device_secret));

        let required = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                mode: Some(OsString::from("required")),
                wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                ..IosConfigValues::default()
            },
        )
        .expect("required managed route");
        let IosStartupConfig::Managed(required) = required.ios.expect("enabled") else {
            panic!("expected managed iOS route");
        };
        assert_eq!(required.policy, IosManagedPolicy::Required);

        for (values, expected) in [
            (
                IosConfigValues {
                    mode: Some(OsString::from("sometimes")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::InvalidIosMode,
            ),
            (
                IosConfigValues {
                    mode: Some(OsString::from("off")),
                    wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosSettingsWhileDisabled,
            ),
            (
                IosConfigValues {
                    wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosManagedModeRequired,
            ),
            (
                IosConfigValues {
                    mode: Some(OsString::from("auto")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosManagedProjectRequired,
            ),
            (
                IosConfigValues {
                    mode: Some(OsString::from("auto")),
                    wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                    wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::IosManagedExternalConflict,
            ),
            (
                IosConfigValues {
                    mode: Some(OsString::from("auto")),
                    wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                    remote_port: Some(OsString::from("0")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::InvalidManagedIosConfiguration,
            ),
            (
                IosConfigValues {
                    mode: Some(OsString::from("auto")),
                    wda_project: Some(OsString::from("WebDriverAgent.xcodeproj")),
                    allow_provisioning_updates: Some(OsString::from("maybe")),
                    ..IosConfigValues::default()
                },
                DaemonStartupError::InvalidManagedIosConfiguration,
            ),
        ] {
            assert_eq!(
                daemon_config_with_platforms(None, None, values),
                Err(expected)
            );
        }
    }

    #[test]
    fn daemon_config_playwright_is_explicit_and_redacts_endpoint_debug() {
        let config = DaemonConfig::from_values(
            None,
            Some(OsString::from("off")),
            None,
            NativePlatformConfigValues::default(),
            None,
            PlaywrightConfigValues {
                endpoint: Some(OsString::from("wss://127.0.0.1:9443/session-token")),
                browser: Some(OsString::from("firefox")),
                node: Some(OsString::from("custom-node")),
                helper: Some(OsString::from("custom-helper.js")),
            },
        )
        .expect("valid Playwright config");
        let playwright = config.playwright.as_ref().expect("Playwright enabled");
        assert_eq!(playwright.browser, BrowserKind::Firefox);
        assert_eq!(playwright.node_path, PathBuf::from("custom-node"));
        assert_eq!(playwright.helper_path, PathBuf::from("custom-helper.js"));
        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("session-token"));

        assert_eq!(
            DaemonConfig::from_values(
                None,
                None,
                None,
                NativePlatformConfigValues::default(),
                None,
                PlaywrightConfigValues {
                    browser: Some(OsString::from("chromium")),
                    ..PlaywrightConfigValues::default()
                },
            ),
            Err(DaemonStartupError::PlaywrightEndpointRequired)
        );
        assert_eq!(
            DaemonConfig::from_values(
                None,
                None,
                None,
                NativePlatformConfigValues::default(),
                None,
                PlaywrightConfigValues {
                    endpoint: Some(OsString::from("ws://user:secret@127.0.0.1/")),
                    ..PlaywrightConfigValues::default()
                },
            ),
            Err(DaemonStartupError::InvalidPlaywrightEndpoint)
        );
    }

    #[test]
    fn rpc_listener_is_explicitly_loopback_only() {
        assert_eq!(parse_rpc_listen(None).expect("absent listener"), None);
        assert_eq!(
            parse_rpc_listen(Some(OsString::from("127.0.0.1:0")))
                .expect("loopback listener")
                .expect("configured listener")
                .port(),
            0
        );
        for value in ["0.0.0.0:9000", "192.0.2.1:9000", "not-an-address"] {
            assert_eq!(
                parse_rpc_listen(Some(OsString::from(value))),
                Err(DaemonStartupError::InvalidRpcListen)
            );
        }
    }

    #[test]
    fn plugin_startup_is_explicit_bounded_and_redacts_directories() {
        let secret_directory = std::env::temp_dir().join("PLUGIN-DIRECTORY-SECRET-SENTINEL");
        let secret_path_list =
            std::env::join_paths([&secret_directory]).expect("join plugin directory path");
        assert_eq!(
            parse_plugin_startup(None, None).expect("plugins disabled"),
            None
        );
        assert_eq!(
            parse_plugin_startup(None, Some(OsString::from("1000"))),
            Err(DaemonStartupError::PluginDirectoryRequired)
        );
        for value in ["0", "120001", "not-a-number"] {
            assert_eq!(
                parse_plugin_startup(Some(secret_path_list.clone()), Some(OsString::from(value)),),
                Err(DaemonStartupError::InvalidPluginConfiguration)
            );
        }
        assert_eq!(
            parse_plugin_startup(Some(OsString::from("relative-plugin-dir")), None),
            Err(DaemonStartupError::InvalidPluginConfiguration)
        );
        let configured = parse_plugin_startup(Some(secret_path_list), Some(OsString::from("5000")))
            .expect("valid plugin config")
            .expect("plugins enabled");
        let debug = format!("{configured:?}");
        assert!(!debug.contains("PLUGIN-DIRECTORY-SECRET-SENTINEL"));
        assert!(debug.contains("directory_count"));
    }

    #[cfg(unix)]
    #[test]
    fn distributed_startup_is_owner_only_loopback_and_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        assert_eq!(parse_distributed_startup(None).expect("disabled"), None);
        assert_eq!(
            parse_distributed_startup(Some(OsString::new())),
            Err(DaemonStartupError::InvalidDistributedConfiguration)
        );
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("distributed.json");
        fs::write(
            &path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"lab-a","endpoint":"127.0.0.1:7443","securityMode":"externalSshOrMtls","tunnelId":"ssh-lab","ownerId":"ssh-lab","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        )
        .expect("write config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        let configured = parse_distributed_startup(Some(path.clone().into_os_string()))
            .expect("valid config")
            .expect("enabled");
        assert_eq!(configured.peers().len(), 1);
        assert!(configured.peers()[0].endpoint().ip().is_loopback());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("unsafe mode");
        assert_eq!(
            parse_distributed_startup(Some(path.into_os_string())),
            Err(DaemonStartupError::InvalidDistributedConfiguration)
        );
    }

    #[cfg(unix)]
    #[test]
    fn distributed_server_startup_is_owner_only_redacted_and_conflict_free() {
        use std::os::unix::fs::PermissionsExt as _;

        fn write_owner_only(path: &Path, body: &str) {
            fs::write(path, body).expect("write distributed config");
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .expect("owner-only distributed config");
        }

        assert_eq!(
            parse_distributed_server_startup(None).expect("disabled"),
            None
        );
        assert_eq!(
            parse_distributed_server_startup(Some(OsString::new())),
            Err(DaemonStartupError::InvalidDistributedServerConfiguration)
        );

        let root = tempfile::tempdir().expect("tempdir");
        let server_path = root.path().join("SERVER-PATH-SECRET.json");
        write_owner_only(
            &server_path,
            r#"{"schemaVersion":1,"nodeId":"stock-node","listen":"127.0.0.1:7444","securityMode":"externalSshOrMtls","tunnelId":"TUNNEL-SECRET-SENTINEL","nodeEpoch":7,"inventoryRevision":3}"#,
        );
        let server = parse_distributed_server_startup(Some(server_path.clone().into_os_string()))
            .expect("valid server config")
            .expect("server enabled");
        assert_eq!(server.listen(), "127.0.0.1:7444".parse().expect("address"));
        let debug = format!("{server:?}");
        assert!(!debug.contains("TUNNEL-SECRET-SENTINEL"));
        assert!(!debug.contains("SERVER-PATH-SECRET"));
        validate_distributed_topology(None, None, Some(&server)).expect("isolated server");
        assert_eq!(
            validate_distributed_topology(Some(server.listen()), None, Some(&server)),
            Err(DaemonStartupError::DistributedServerTopologyConflict)
        );

        let peers_path = root.path().join("peers.json");
        write_owner_only(
            &peers_path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"remote-node","endpoint":"127.0.0.1:7445","securityMode":"externalSshOrMtls","tunnelId":"remote-tunnel","ownerId":"remote-tunnel","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        );
        let peers = parse_distributed_startup(Some(peers_path.clone().into_os_string()))
            .expect("valid peers")
            .expect("peers enabled");
        validate_distributed_topology(None, Some(&peers), Some(&server))
            .expect("unrelated outbound peer");

        write_owner_only(
            &peers_path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"remote-node","endpoint":"127.0.0.1:7444","securityMode":"externalSshOrMtls","tunnelId":"remote-tunnel","ownerId":"remote-tunnel","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        );
        let endpoint_conflict =
            parse_distributed_startup(Some(peers_path.clone().into_os_string()))
                .expect("valid conflict fixture")
                .expect("peer enabled");
        assert_eq!(
            validate_distributed_topology(None, Some(&endpoint_conflict), Some(&server)),
            Err(DaemonStartupError::DistributedServerTopologyConflict)
        );

        write_owner_only(
            &peers_path,
            r#"{"schemaVersion":1,"peers":[{"nodeId":"stock-node","endpoint":"127.0.0.1:7445","securityMode":"externalSshOrMtls","tunnelId":"remote-tunnel","ownerId":"remote-tunnel","leaseTtlMs":30000,"renewBeforeMs":5000}]}"#,
        );
        let node_conflict = parse_distributed_startup(Some(peers_path.into_os_string()))
            .expect("valid conflict fixture")
            .expect("peer enabled");
        assert_eq!(
            validate_distributed_topology(None, Some(&node_conflict), Some(&server)),
            Err(DaemonStartupError::DistributedServerTopologyConflict)
        );

        fs::set_permissions(&server_path, fs::Permissions::from_mode(0o644))
            .expect("unsafe permissions");
        let error = parse_distributed_server_startup(Some(server_path.into_os_string()))
            .expect_err("unsafe server config");
        let diagnostic = error.to_string();
        assert!(!diagnostic.contains("TUNNEL-SECRET-SENTINEL"));
        assert!(!diagnostic.contains("SERVER-PATH-SECRET"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn distributed_server_starting_gate_and_shutdown_clean_shared_runtime() {
        use std::os::unix::fs::PermissionsExt as _;

        let reserved =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve peer listener address");
        let address = reserved.local_addr().expect("reserved address");
        drop(reserved);

        let root = tempfile::tempdir().expect("tempdir");
        let server_path = root.path().join("server.json");
        fs::write(
            &server_path,
            format!(
                r#"{{"schemaVersion":1,"nodeId":"stock-node","listen":"{address}","securityMode":"externalSshOrMtls","tunnelId":"stock-tunnel","nodeEpoch":7,"inventoryRevision":1}}"#
            ),
        )
        .expect("write peer server config");
        fs::set_permissions(&server_path, fs::Permissions::from_mode(0o600))
            .expect("owner-only peer server config");

        let mut config = DaemonConfig::from_values(
            Some(root.path().join("evidence").into_os_string()),
            Some(OsString::from("off")),
            None,
            NativePlatformConfigValues::default(),
            None,
            PlaywrightConfigValues::default(),
        )
        .expect("base daemon config");
        config.distributed_server =
            parse_distributed_server_startup(Some(server_path.into_os_string()))
                .expect("peer server config");

        let events = Arc::new(MemoryEventStore::default());
        let concrete_evidence = Arc::new(
            FileEvidenceStore::new(root.path().join("evidence"), Default::default())
                .expect("Evidence Store"),
        );
        let evidence: Arc<dyn EvidenceStore> = concrete_evidence.clone();
        let runtime = Arc::new(Registry::with_evidence(
            Arc::clone(&events),
            Arc::clone(&evidence),
        ));
        let driver = Arc::new(MockDriver::new("peer-mock").with_session_evidence());
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register peer fixture Driver");

        let mut server = start_distributed_peer_server(
            &config,
            Arc::clone(&runtime),
            Arc::clone(&events),
            evidence,
        )
        .await
        .expect("start peer server");
        let service = Arc::clone(&server.as_ref().expect("server enabled").service);
        assert!(!service.is_ready());

        let stream = TcpStream::connect(address)
            .await
            .expect("connect peer client");
        let security = PeerSecurity::external_tunnel("stock-tunnel").expect("peer security");
        let transport = NdjsonPeerTransport::new(
            stream,
            config
                .distributed_server
                .as_ref()
                .expect("server config")
                .node_id()
                .clone(),
            security.clone(),
        );
        let control = ExecutionControl::unbounded();
        let node_id = config
            .distributed_server
            .as_ref()
            .expect("server config")
            .node_id()
            .clone();

        let hello = transport
            .request(
                PeerRequest::new(node_id.clone(), None, PeerOperation::Hello),
                &control,
            )
            .await
            .expect("starting Hello");
        assert!(hello.ok);
        let inventory = transport
            .request(
                PeerRequest::new(node_id.clone(), None, PeerOperation::Inventory),
                &control,
            )
            .await
            .expect("starting Inventory");
        let Some(PeerResult::Inventory { inventory }) = inventory.result else {
            panic!("inventory response expected");
        };
        let device_key = inventory.devices[0].device_key.clone();
        let rejected = transport
            .request(
                PeerRequest::new(
                    node_id.clone(),
                    Some(7),
                    PeerOperation::LeaseAcquire {
                        device_key: device_key.clone(),
                        owner_id: security.subject().to_owned(),
                        ttl_ms: 30_000,
                    },
                ),
                &control,
            )
            .await
            .expect("starting lease response");
        assert_eq!(
            rejected.error.as_ref().map(|error| error.code.as_str()),
            Some("node_starting")
        );
        assert!(rejected.error.expect("starting error").retryable);

        server.as_ref().expect("server enabled").mark_ready();
        let lease_response = transport
            .request(
                PeerRequest::new(
                    node_id.clone(),
                    Some(7),
                    PeerOperation::LeaseAcquire {
                        device_key: device_key.clone(),
                        owner_id: security.subject().to_owned(),
                        ttl_ms: 30_000,
                    },
                ),
                &control,
            )
            .await
            .expect("lease response");
        let Some(PeerResult::Lease { lease }) = lease_response.result else {
            panic!("lease response expected");
        };
        let mut connect = PeerRequest::new(
            node_id.clone(),
            Some(7),
            PeerOperation::Connect {
                device_key: device_key.clone(),
            },
        );
        connect.lease = Some(lease.clone());
        assert!(
            transport
                .request(connect, &control)
                .await
                .expect("connect response")
                .ok
        );
        let mut observe = PeerRequest::new(
            node_id,
            Some(7),
            PeerOperation::Observe {
                device_key,
                screenshot_omission: None,
                ui_snapshots_enabled: false,
                semantic_actions_enabled: false,
            },
        );
        observe.lease = Some(lease);
        assert!(
            transport
                .request(observe, &control)
                .await
                .expect("observe response")
                .ok
        );
        assert!(
            runtime
                .pool_entries(now_ms())
                .await
                .iter()
                .any(|entry| entry.lease.is_some())
        );
        assert!(
            !concrete_evidence
                .referenced_sessions()
                .await
                .expect("Evidence references")
                .is_empty()
        );

        server.as_ref().expect("server enabled").begin_shutdown();
        assert!(
            !server
                .as_ref()
                .expect("server enabled")
                .controller
                .cancel(CancellationReason::Shutdown),
            "the pre-drain shutdown signal must be idempotently visible"
        );
        tokio::time::timeout(
            Duration::from_secs(10),
            shutdown_distributed_peer_server(&mut server),
        )
        .await
        .expect("peer shutdown remains bounded")
        .expect("peer shutdown succeeds");
        assert!(
            runtime
                .pool_entries(now_ms())
                .await
                .iter()
                .all(|entry| entry.lease.is_none())
        );
        assert!(
            events
                .list_sessions()
                .await
                .expect("peer Sessions")
                .is_empty()
        );
        assert!(
            concrete_evidence
                .referenced_sessions()
                .await
                .expect("released Evidence references")
                .is_empty()
        );
    }

    #[test]
    fn rdp_startup_requires_a_complete_redacted_loopback_configuration() {
        assert_eq!(
            parse_rdp_startup(None, None, None, None).expect("RDP disabled"),
            None
        );
        assert_eq!(
            parse_rdp_startup(None, Some(OsString::from("rdp://host")), None, None,),
            Err(DaemonStartupError::RdpBridgeRequired)
        );
        assert_eq!(
            parse_rdp_startup(
                Some(OsString::from("127.0.0.1:7766")),
                None,
                Some(OsString::from("secret")),
                None,
            ),
            Err(DaemonStartupError::RdpTargetRequired)
        );
        let config = parse_rdp_startup(
            Some(OsString::from("127.0.0.1:7766")),
            Some(OsString::from("rdp://private-host.example")),
            Some(OsString::from("DO-NOT-LOG-RDP-TOKEN")),
            Some(OsString::from("Lab RDP")),
        )
        .expect("valid RDP config")
        .expect("RDP enabled");
        let debug = format!("{config:?}");
        assert!(debug.contains("Lab RDP"));
        assert!(!debug.contains("DO-NOT-LOG"));
        assert!(!debug.contains("private-host"));
    }

    async fn protected_test_context(policy: ScreenshotPolicy) -> (Registry, Arc<MemoryEventStore>) {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events)).with_screenshot_policy(policy);
        let driver = Arc::new(ProtectedTestDriver::new("protected-test"));
        runtime
            .register(
                Arc::clone(&driver) as Arc<dyn DeviceDriver>,
                driver.device_info(),
            )
            .await
            .expect("register protected test Driver");
        (runtime, events)
    }

    #[tokio::test]
    async fn direct_execute_rejects_misclassified_protected_action_before_driver_io() {
        const SENTINEL: &str = "daemon-misclassified-protected-sentinel";
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events));
        let driver = Arc::new(MismatchedProtectionTestDriver::new(
            "mismatched-protection-test",
        ));
        runtime
            .register(
                Arc::clone(&driver) as Arc<dyn DeviceDriver>,
                driver.device_info(),
            )
            .await
            .expect("register mismatched protection Driver");
        let mut connection = ConnectionState::default();

        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await
        .result()
        .expect("hello without protected action feature");
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await
        .result()
        .expect("connect mismatched protection Driver");
        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("Session")["id"].clone())
                .expect("Session id");

        let rejected = dispatch(
            request(
                4,
                "device.execute",
                json!({
                    "id": "10000000-0000-4000-8000-000000000007",
                    "name": "misclassifiedSecret",
                    "arguments": { "secret": SENTINEL }
                }),
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert_eq!(
            rejected.error().expect("protection mismatch").data.code,
            "protocol_error"
        );
        assert_eq!(driver.capability_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            driver.execute_calls.load(Ordering::SeqCst),
            0,
            "mismatched action must not receive a screenshot-capable Driver context"
        );

        let exported = events
            .export_session(&session_id)
            .await
            .expect("export active Session");
        assert!(
            matches!(
                exported.events.as_slice(),
                [event] if matches!(event.payload, TestEventPayload::SessionStarted)
            ),
            "only SessionStarted may be durable"
        );
        assert!(
            !serde_json::to_string(&exported)
                .expect("serialize Session export")
                .contains(SENTINEL)
        );
    }

    #[tokio::test]
    async fn protected_actions_are_hidden_and_rejected_until_the_feature_is_negotiated() {
        const REJECTED_SENTINEL: &str = "dr016-rejected-secret-sentinel";
        const INVALID_SENTINEL: &str = "dr016-invalid-params-sentinel";
        const UNKNOWN_SENTINEL: &str = "dr016-unknown-action-sentinel";
        let (runtime, events) = protected_test_context(ScreenshotPolicy::Capture).await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let capabilities = dispatch(
            request(3, "device.capabilities", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert!(
            capabilities
                .result()
                .expect("filtered capabilities")
                .as_array()
                .expect("capability array")
                .iter()
                .all(|definition| definition["name"] != "inputSecret")
        );

        let started = dispatch(
            request(4, "session.start", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("session")["id"].clone())
                .expect("session id");
        let invalid = dispatch(
            request(
                5,
                "device.execute",
                json!({
                    "id": "11111111-1111-4111-8111-111111111111",
                    "name": "inputSecret",
                    "arguments": { "secret": INVALID_SENTINEL },
                    "unexpected": INVALID_SENTINEL
                }),
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert_eq!(
            invalid.error().expect("invalid params").data.code,
            "invalid_params"
        );
        assert!(
            !serde_json::to_string(&invalid)
                .unwrap()
                .contains(INVALID_SENTINEL)
        );

        let rejected = dispatch(
            request(
                6,
                "device.execute",
                json!({
                    "id": "11111111-1111-4111-8111-111111111111",
                    "name": "inputSecret",
                    "arguments": { "secret": REJECTED_SENTINEL }
                }),
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let error = rejected.error().expect("protected feature gate");
        assert_eq!(error.data.code, "protected_action_not_negotiated");
        assert_eq!(
            error.data.details.as_ref().expect("feature details")["requiredFeature"],
            feature::ACTION_PROTECTED_V1
        );
        assert!(
            !serde_json::to_string(&rejected)
                .unwrap()
                .contains(REJECTED_SENTINEL)
        );

        let unknown = dispatch(
            request(
                7,
                "device.execute",
                json!({
                    "id": "22222222-2222-4222-8222-222222222222",
                    "name": "unknownProtectedCandidate",
                    "arguments": { "secret": UNKNOWN_SENTINEL }
                }),
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert_eq!(
            unknown.error().expect("unknown reaches Core").data.code,
            "unknown_action"
        );
        let exported = events
            .export_session(&session_id)
            .await
            .expect("export active test Session");
        let encoded = serde_json::to_string(&exported).expect("serialize exported Session");
        assert!(!encoded.contains(REJECTED_SENTINEL));
        assert!(!encoded.contains(UNKNOWN_SENTINEL));
        assert!(encoded.contains("argumentsRedacted"));
    }

    #[tokio::test]
    async fn negotiated_protected_action_is_visible_and_session_exports_stay_redacted() {
        const SECRET_SENTINEL: &str = "dr016-negotiated-secret-sentinel";
        let (runtime, events) = protected_test_context(ScreenshotPolicy::Capture).await;
        let mut connection = ConnectionState::default();
        let hello = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::ACTION_PROTECTED_V1, feature::EVENTS_SNAPSHOT_V1],
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert!(
            hello.result().expect("hello")["features"]["enabled"]
                .as_array()
                .expect("enabled features")
                .iter()
                .any(|value| value == feature::ACTION_PROTECTED_V1)
        );
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let capabilities = dispatch(
            request(3, "device.capabilities", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let protected = capabilities
            .result()
            .expect("capabilities")
            .as_array()
            .expect("capability array")
            .iter()
            .find(|definition| definition["name"] == "inputSecret")
            .expect("protected capability is visible");
        assert_eq!(protected["protection"], "protected");

        let started = dispatch(
            request(4, "session.start", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let session_id = started.result().expect("session")["id"].clone();
        let executed = dispatch(
            request(
                5,
                "device.execute",
                json!({
                    "id": "33333333-3333-4333-8333-333333333333",
                    "name": "inputSecret",
                    "arguments": { "secret": SECRET_SENTINEL }
                }),
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert!(
            executed.error().is_none(),
            "protected execute succeeds: {executed:?}"
        );
        assert!(
            !serde_json::to_string(&executed)
                .unwrap()
                .contains(SECRET_SENTINEL)
        );

        let listed = dispatch(
            request(6, "events.list", json!({ "sessionId": session_id.clone() })),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let listed_json = serde_json::to_string(&listed).expect("serialize events.list");
        assert!(!listed_json.contains(SECRET_SENTINEL));
        assert!(listed_json.contains("argumentsRedacted"));
        assert!(listed_json.contains("protectedAction"));

        dispatch(
            request(7, "session.end", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let exported = dispatch(
            request(8, "session.export", json!({ "sessionId": session_id })),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let exported_json = serde_json::to_string(&exported).expect("serialize session.export");
        assert!(!exported_json.contains(SECRET_SENTINEL));
        assert!(exported_json.contains("argumentsRedacted"));
    }

    #[tokio::test]
    async fn global_screenshot_omit_policy_reaches_registered_mock_routes() {
        let events = Arc::new(MemoryEventStore::default());
        let runtime =
            Registry::new(Arc::clone(&events)).with_screenshot_policy(ScreenshotPolicy::Omit);
        let driver = Arc::new(MockDriver::new("omit-policy-mock"));
        runtime
            .register(
                Arc::clone(&driver) as Arc<dyn DeviceDriver>,
                driver.device_info(),
            )
            .await
            .expect("register Mock");
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let observed = dispatch(
            request(4, "device.observe", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let observation = observed.result().expect("omitted observation");
        assert!(observation["screenshot"].is_null());
        assert_eq!(observation["screenshotOmission"], "policy");
        assert_eq!(runtime.screenshot_policy(), ScreenshotPolicy::Omit);
    }

    #[tokio::test]
    async fn explicit_ios_route_registers_without_touching_the_network() {
        let config = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                device_token: Some(OsString::from("00008030-001")),
                device_name: Some(OsString::from("Stock daemon iPhone")),
                os_version: Some(OsString::from("18.0")),
                mjpeg_endpoint: None,
                ..IosConfigValues::default()
            },
        )
        .expect("valid iOS startup config");
        let runtime = Arc::new(Registry::new(Arc::new(MemoryEventStore::default())));
        register_ios_device(Arc::clone(&runtime), &config)
            .await
            .expect("register iOS route without connecting WDA");
        let devices = runtime.list().await;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::new("ios-wda:00008030-001"));
        assert_eq!(devices[0].name, "Stock daemon iPhone");
        assert_eq!(devices[0].platform, Platform::Ios);
        assert_eq!(devices[0].os_version.as_deref(), Some("18.0"));
        assert!(!devices[0].connected);

        assert!(matches!(
            register_ios_device(Arc::clone(&runtime), &config).await,
            Err(DaemonStartupError::DeviceRegistration {
                code: "ios_registration_failed"
            })
        ));
    }

    #[tokio::test]
    async fn explicit_appium_ios_route_registers_without_creating_a_w3c_session() {
        let config = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                backend: Some(OsString::from("appium")),
                appium_endpoint: Some(OsString::from("http://127.0.0.1:4723")),
                wda_endpoint: Some(OsString::from("http://127.0.0.1:8100")),
                device_token: Some(OsString::from("00008030-appium")),
                device_name: Some(OsString::from("Stock daemon Appium iPhone")),
                os_version: Some(OsString::from("18.0")),
                ..IosConfigValues::default()
            },
        )
        .expect("valid Appium iOS startup config");
        let runtime = Arc::new(Registry::new(Arc::new(MemoryEventStore::default())));
        register_ios_device(Arc::clone(&runtime), &config)
            .await
            .expect("register Appium route without contacting Appium or WDA");
        let devices = runtime.list().await;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::new("ios-wda:00008030-appium"));
        assert_eq!(devices[0].name, "Stock daemon Appium iPhone");
        assert_eq!(devices[0].platform, Platform::Ios);
        assert_eq!(devices[0].os_version.as_deref(), Some("18.0"));
        assert!(!devices[0].connected);
    }

    #[tokio::test]
    async fn managed_ios_auto_keeps_a_cancellable_hotplug_supervisor_after_initial_failure() {
        let directory = TempDir::new().expect("temporary iOS directory");
        let config = daemon_config_with_platforms(
            None,
            None,
            IosConfigValues {
                mode: Some(OsString::from("auto")),
                wda_project: Some(
                    directory
                        .path()
                        .join("missing/WebDriverAgent.xcodeproj")
                        .into_os_string(),
                ),
                ..IosConfigValues::default()
            },
        )
        .expect("managed auto config");
        let runtime = Arc::new(Registry::new(Arc::new(MemoryEventStore::default())));
        let managed = register_ios_device(Arc::clone(&runtime), &config)
            .await
            .expect("auto preserves startup")
            .expect("hot-plug supervisor remains active");
        assert!(runtime.list().await.is_empty());
        managed.shutdown().await.expect("hot-plug shutdown");
    }

    #[test]
    fn ios_hotplug_retry_is_fast_for_device_state_and_bounded_for_host_failures() {
        assert_eq!(
            ios_hotplug_retry_delay("ios_device_not_found", 20),
            Duration::from_secs(1)
        );
        assert_eq!(
            ios_hotplug_retry_delay("ios_simulator_not_booted", 20),
            Duration::from_secs(1)
        );
        assert_eq!(
            ios_hotplug_retry_delay("ios_wda_signing_failed", 1),
            Duration::from_secs(2)
        );
        assert_eq!(
            ios_hotplug_retry_delay("ios_wda_signing_failed", 100),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn desktop_off_never_discovers_and_auto_failures_preserve_other_routes() {
        let runtime = Registry::new(Arc::new(MemoryEventStore::default()));
        let mock = Arc::new(MockDriver::new("mock-test"));
        runtime
            .register(
                Arc::clone(&mock) as Arc<dyn DeviceDriver>,
                mock.device_info(),
            )
            .await
            .expect("register Mock");
        let backend = FakeDesktopBackend::new(Err("desktop_tool_not_found"));
        let off =
            daemon_config_with_desktop(DesktopConfigValues::default()).expect("desktop off config");
        register_desktop_from_backend(&runtime, &off.desktop, &backend)
            .await
            .expect("off succeeds");
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 0);

        let auto = daemon_config_with_desktop(DesktopConfigValues {
            mode: Some(OsString::from("auto")),
            ..DesktopConfigValues::default()
        })
        .expect("desktop auto config");
        register_desktop_from_backend(&runtime, &auto.desktop, &backend)
            .await
            .expect("auto host failure is optional");
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.list().await.len(), 1);
        assert_eq!(runtime.list().await[0].id, DeviceId::new("mock-test"));

        let required = daemon_config_with_desktop(DesktopConfigValues {
            mode: Some(OsString::from("required")),
            ..DesktopConfigValues::default()
        })
        .expect("desktop required config");
        assert_eq!(
            register_desktop_from_backend(&runtime, &required.desktop, &backend).await,
            Err(DaemonStartupError::DesktopRequired {
                code: "desktop_tool_not_found"
            })
        );
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn desktop_registers_one_disconnected_route_under_a_five_second_startup_control() {
        let config = daemon_config_with_desktop(DesktopConfigValues {
            mode: Some(OsString::from("required")),
            id: Some(OsString::from("desktop-stock")),
            name: Some(OsString::from("Stock desktop")),
            ..DesktopConfigValues::default()
        })
        .expect("desktop required config");
        let backend = FakeDesktopBackend::new(Ok(DeviceInfo {
            name: "Stock desktop".to_owned(),
            ..desktop_info("desktop-stock", Platform::MacOs)
        }));
        let runtime = Registry::new(Arc::new(MemoryEventStore::default()));
        register_desktop_from_backend(&runtime, &config.desktop, &backend)
            .await
            .expect("register native desktop route");

        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *backend
                .timeout_budgets_ms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            [5_000]
        );
        let devices = runtime.list().await;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, DeviceId::new("desktop-stock"));
        assert_eq!(devices[0].name, "Stock desktop");
        assert_eq!(devices[0].platform, Platform::MacOs);
        assert!(!devices[0].connected);
    }

    #[tokio::test]
    async fn desktop_registration_conflicts_are_optional_only_in_auto_mode() {
        let runtime = Registry::new(Arc::new(MemoryEventStore::default()));
        let duplicate = Arc::new(MockDriver::new("desktop-local"));
        runtime
            .register(
                Arc::clone(&duplicate) as Arc<dyn DeviceDriver>,
                duplicate.device_info(),
            )
            .await
            .expect("register conflicting route");
        let backend = FakeDesktopBackend::new(Ok(desktop_info("desktop-local", Platform::Windows)));
        let auto = daemon_config_with_desktop(DesktopConfigValues {
            mode: Some(OsString::from("auto")),
            ..DesktopConfigValues::default()
        })
        .expect("desktop auto config");
        register_desktop_from_backend(&runtime, &auto.desktop, &backend)
            .await
            .expect("auto skips a conflicting desktop route");
        assert_eq!(runtime.list().await.len(), 1);

        let required = daemon_config_with_desktop(DesktopConfigValues {
            mode: Some(OsString::from("required")),
            ..DesktopConfigValues::default()
        })
        .expect("desktop required config");
        assert_eq!(
            register_desktop_from_backend(&runtime, &required.desktop, &backend).await,
            Err(DaemonStartupError::DeviceRegistration {
                code: "desktop_registration_failed"
            })
        );
    }

    #[test]
    fn system_harmony_uses_distinct_discovery_and_runtime_command_budgets() {
        let (discovery, runtime) =
            system_harmony_configs(Path::new("custom-hdc")).expect("valid system HDC configs");
        assert_eq!(discovery.executable(), Path::new("custom-hdc"));
        assert_eq!(runtime.executable(), Path::new("custom-hdc"));
        assert_eq!(discovery.command_timeout(), Duration::from_secs(5));
        assert_eq!(runtime.command_timeout(), Duration::from_secs(65));
        assert!(runtime.command_timeout() > Duration::from_secs(60));
    }

    #[tokio::test]
    async fn harmony_off_never_discovers_and_auto_failures_preserve_other_routes() {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(events);
        let mock = Arc::new(MockDriver::new("mock-test"));
        runtime
            .register(
                Arc::clone(&mock) as Arc<dyn DeviceDriver>,
                mock.device_info(),
            )
            .await
            .expect("register Mock");
        let backend = FakeHarmonyBackend::new(Err("hdc_executable_not_found"));

        register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Off, &backend)
            .await
            .expect("off succeeds");
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 0);

        register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Auto, &backend)
            .await
            .expect("auto host failure is optional");
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.list().await.len(), 1);
        assert_eq!(runtime.list().await[0].id, DeviceId::new("mock-test"));
        assert_eq!(
            register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Required, &backend).await,
            Err(DaemonStartupError::HarmonyRequired {
                code: "hdc_executable_not_found"
            })
        );
    }

    #[tokio::test]
    async fn harmony_auto_accepts_no_devices_while_required_rejects_an_empty_inventory() {
        let runtime = Registry::new(Arc::new(MemoryEventStore::default()));
        let empty = FakeHarmonyBackend::new(Ok(HarmonyDiscoveryReport::default()));
        register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Auto, &empty)
            .await
            .expect("auto permits no HarmonyOS devices");
        assert_eq!(runtime.list().await, Vec::<DeviceInfo>::new());
        assert_eq!(
            register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Required, &empty).await,
            Err(DaemonStartupError::HarmonyRequiredNoDevices)
        );
    }

    #[tokio::test]
    async fn harmony_routes_register_in_stable_order_and_preserve_unavailable_states() {
        let runtime = Registry::new(Arc::new(MemoryEventStore::default()));
        let mock = Arc::new(MockDriver::new("mock-test"));
        runtime
            .register(
                Arc::clone(&mock) as Arc<dyn DeviceDriver>,
                mock.device_info(),
            )
            .await
            .expect("register Mock");
        let backend = FakeHarmonyBackend::new(Ok(HarmonyDiscoveryReport {
            devices: vec![
                harmony_descriptor("target-b", HdcTargetState::Offline),
                harmony_descriptor("target-a", HdcTargetState::Unauthorized),
            ],
            ignored_diagnostics: vec!["bounded diagnostic".to_owned()],
        }));

        register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Required, &backend)
            .await
            .expect("register HarmonyOS routes");
        assert_eq!(
            *backend
                .build_order
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["target-a", "target-b"]
        );
        let devices = runtime.list().await;
        assert_eq!(
            devices
                .iter()
                .map(|info| info.id.0.as_str())
                .collect::<Vec<_>>(),
            ["harmony-hdc:target-a", "harmony-hdc:target-b", "mock-test"]
        );
        for info in &devices[..2] {
            assert_eq!(info.platform, Platform::HarmonyOs);
            assert!(!info.connected);
        }
    }

    #[tokio::test]
    async fn harmony_registration_conflicts_are_optional_only_in_auto_mode() {
        let runtime = Registry::new(Arc::new(MemoryEventStore::default()));
        let duplicate = Arc::new(MockDriver::new("harmony-hdc:duplicate"));
        runtime
            .register(
                Arc::clone(&duplicate) as Arc<dyn DeviceDriver>,
                duplicate.device_info(),
            )
            .await
            .expect("register conflicting route");
        let backend = FakeHarmonyBackend::new(Ok(HarmonyDiscoveryReport {
            devices: vec![harmony_descriptor("duplicate", HdcTargetState::Ready)],
            ignored_diagnostics: Vec::new(),
        }));
        register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Auto, &backend)
            .await
            .expect("auto skips a conflicting HarmonyOS route");
        assert_eq!(runtime.list().await.len(), 1);
        assert_eq!(
            register_harmony_from_backend(&runtime, HarmonyDiscoveryMode::Required, &backend).await,
            Err(DaemonStartupError::DeviceRegistration {
                code: "harmony_registration_failed"
            })
        );
    }

    #[test]
    fn system_android_uses_distinct_discovery_and_runtime_command_budgets() {
        let (discovery, runtime) = system_android_configs(std::path::Path::new("custom-adb"))
            .expect("valid system ADB configs");
        assert_eq!(discovery.program(), std::path::Path::new("custom-adb"));
        assert_eq!(runtime.program(), std::path::Path::new("custom-adb"));
        assert_eq!(discovery.command_timeout(), Duration::from_secs(5));
        assert_eq!(runtime.command_timeout(), Duration::from_secs(65));
        assert!(runtime.command_timeout() > Duration::from_secs(60));
    }

    #[tokio::test]
    async fn android_off_does_not_touch_discovery_and_auto_keeps_mock_on_host_failure() {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events));
        let mock = Arc::new(MockDriver::new("mock-test"));
        runtime
            .register(
                Arc::clone(&mock) as Arc<dyn DeviceDriver>,
                mock.device_info(),
            )
            .await
            .expect("register Mock");
        let backend = FakeAndroidBackend::new(Err("android_adb_not_found"));

        register_android_from_backend(&runtime, AndroidDiscoveryMode::Off, &backend)
            .await
            .expect("off succeeds");
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 0);

        register_android_from_backend(&runtime, AndroidDiscoveryMode::Auto, &backend)
            .await
            .expect("auto host failure is optional");
        assert_eq!(backend.discovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.list().await.len(), 1);
        assert_eq!(runtime.list().await[0].id, DeviceId::new("mock-test"));
    }

    #[tokio::test]
    async fn android_auto_accepts_no_devices_while_required_rejects_failures() {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(events);
        let empty = FakeAndroidBackend::new(Ok(AdbDiscoveryReport::default()));
        register_android_from_backend(&runtime, AndroidDiscoveryMode::Auto, &empty)
            .await
            .expect("auto permits no devices");
        assert_eq!(runtime.list().await, Vec::<DeviceInfo>::new());
        assert_eq!(
            register_android_from_backend(&runtime, AndroidDiscoveryMode::Required, &empty).await,
            Err(DaemonStartupError::AndroidRequiredNoDevices)
        );

        let failed = FakeAndroidBackend::new(Err("android_adb_process_failed"));
        assert_eq!(
            register_android_from_backend(&runtime, AndroidDiscoveryMode::Required, &failed).await,
            Err(DaemonStartupError::AndroidRequired {
                code: "android_adb_process_failed"
            })
        );
    }

    #[tokio::test]
    async fn android_routes_register_in_stable_order_and_preserve_mock() {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events));
        let mock = Arc::new(MockDriver::new("mock-test"));
        runtime
            .register(
                Arc::clone(&mock) as Arc<dyn DeviceDriver>,
                mock.device_info(),
            )
            .await
            .expect("register Mock");
        let backend = FakeAndroidBackend::new(Ok(AdbDiscoveryReport {
            devices: vec![
                android_descriptor("serial-b"),
                android_descriptor("serial-a"),
            ],
            issues: Vec::new(),
        }));

        register_android_from_backend(&runtime, AndroidDiscoveryMode::Required, &backend)
            .await
            .expect("register Android routes");
        assert_eq!(
            *backend
                .build_order
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ["serial-a", "serial-b"]
        );
        assert_eq!(
            runtime
                .list()
                .await
                .into_iter()
                .map(|info| info.id.0)
                .collect::<Vec<_>>(),
            ["android-adb:serial-a", "android-adb:serial-b", "mock-test"]
        );
    }

    #[tokio::test]
    async fn events_clear_deletes_log_before_file_evidence_release_and_is_retryable() {
        let root = TempDir::new().expect("temporary Evidence root");
        let inner = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let events = Arc::new(MemoryEventStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        inner
            .put(
                PutEvidence::new(session_id.clone(), "text/plain").expect("put request"),
                Box::pin(Cursor::new(b"durable fixture".to_vec())),
            )
            .await
            .expect("seed Evidence");
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end Session");

        let flaky = Arc::new(FlakyReleaseStore {
            inner: Arc::clone(&inner),
            events: Arc::clone(&events),
            session_id: session_id.clone(),
            failures_remaining: AtomicUsize::new(1),
            release_saw_deleted_log: AtomicBool::new(false),
        });
        let store: Arc<dyn EvidenceStore> = flaky.clone();
        let first = clear_ended_session(
            events.as_ref(),
            &EvidenceCleanup::Managed(Arc::clone(&store)),
            session_id.clone(),
        )
        .await
        .expect_err("first release is injected to fail");
        assert_eq!(first.data.code, "evidence_cleanup_failed");
        assert!(flaky.release_saw_deleted_log.load(Ordering::SeqCst));
        assert_eq!(
            inner
                .referenced_sessions()
                .await
                .expect("references remain"),
            std::slice::from_ref(&session_id)
        );

        let retry = clear_ended_session(
            events.as_ref(),
            &EvidenceCleanup::Managed(store),
            session_id.clone(),
        )
        .await
        .expect("retry releases Evidence after already-deleted log");
        assert_eq!(retry["deleted"], true);
        assert_eq!(retry["sessionId"], session_id.to_string());
        assert!(
            inner
                .referenced_sessions()
                .await
                .expect("released references")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn startup_reconciliation_releases_orphaned_file_evidence_pins() {
        let root = TempDir::new().expect("temporary Evidence root");
        let evidence = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
            .expect("File Evidence Store");
        let orphan = SessionId::new();
        evidence
            .put(
                PutEvidence::new(orphan.clone(), "text/plain").expect("put request"),
                Box::pin(Cursor::new(b"orphan fixture".to_vec())),
            )
            .await
            .expect("seed orphan Evidence");
        let events = MemoryEventStore::default();

        let reports = reconcile_missing_session_evidence(&events, &evidence, now_ms())
            .await
            .expect("startup reconciliation");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].session_id, orphan);
        assert!(
            evidence
                .referenced_sessions()
                .await
                .expect("orphan released")
                .is_empty()
        );
    }

    #[test]
    fn distinguishes_parse_errors_from_invalid_requests() {
        let parse = decode_request("{").expect_err("malformed JSON");
        assert_eq!(parse.error().expect("parse error").code, -32700);
        assert_eq!(parse.error().expect("parse error").data.code, "parse_error");

        let invalid = decode_request(r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#)
            .expect_err("null ids are outside the supported subset");
        assert_eq!(invalid.error().expect("invalid request").code, -32600);

        let scalar_params =
            decode_request(r#"{"jsonrpc":"2.0","id":1,"method":"device.connect","params":42}"#)
                .expect_err("params must be structured");
        assert_eq!(scalar_params.error().expect("invalid request").code, -32600);

        const SENTINEL: &str = "dr016-request-debug-sentinel";
        let sensitive = decode_request(&format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"device.execute","params":{{"id":"44444444-4444-4444-8444-444444444444","name":"inputSecret","arguments":{{"secret":"{SENTINEL}"}}}}}}"#
        ))
        .expect("valid protected request");
        assert!(!format!("{sensitive:?}").contains(SENTINEL));
        assert!(
            serde_json::to_string(&sensitive)
                .expect("serialize request wire")
                .contains(SENTINEL),
            "the secret necessarily remains present on the request wire"
        );
    }

    #[tokio::test]
    async fn requires_hello_before_every_other_method() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let response = dispatch(
            request(1, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;

        let error = response.error().expect("handshake error");
        assert_eq!(error.code, -32001);
        assert_eq!(error.data.code, "handshake_required");
    }

    #[tokio::test]
    async fn hello_unlocks_methods_without_connecting_the_device() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let hello = dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;

        assert_eq!(
            hello.result().expect("hello result")["protocol"]["selected"],
            json!({ "major": 1, "minor": 5 })
        );
        assert!(
            connection
                .context()
                .is_some_and(|context| context.selected_device_id.is_none()),
            "hello must not select a device"
        );

        let observe = dispatch(
            request(2, "device.observe", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            observe.error().expect("session is required").data.code,
            "session_required"
        );

        let session = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(session.result().expect("session starts")["state"], "active");

        let observe = dispatch(
            request(4, "device.observe", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            observe
                .error()
                .expect("device remains disconnected")
                .data
                .code,
            "device_not_connected"
        );

        let connect = dispatch(
            request(5, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(connect.result().expect("device info")["connected"], true);

        let describe = dispatch(
            request(6, "system.describe", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            describe.result().expect("connection description")["client"]["name"],
            "test-client"
        );
    }

    #[tokio::test]
    async fn protocol_14_observations_do_not_leak_protocol_15_ui_fields() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let hello = dispatch(
            hello_request(ProtocolOffer::exact(ProtocolVersion::new(1, 4)), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            hello.result().expect("1.4 hello")["protocol"]["selected"],
            json!({ "major": 1, "minor": 4 })
        );
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await
        .result()
        .expect("connect legacy client");
        dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await
        .result()
        .expect("start legacy Session");
        let observation = dispatch(
            request(4, "device.observe", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let observation = observation
            .result()
            .and_then(Value::as_object)
            .expect("legacy Observation object");
        assert!(!observation.contains_key("uiSnapshot"));
        assert!(!observation.contains_key("uiSnapshotOmission"));
    }

    #[tokio::test]
    async fn legacy_snapshot_methods_reject_sessions_with_newer_event_fields() {
        let (runtime, events) = test_context().await;
        let mut producer = ConnectionState::default();
        dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 5)),
                &[],
                &[
                    feature::EVENTS_SNAPSHOT_V1,
                    feature::OBSERVATION_UI_SNAPSHOT_V1,
                ],
            ),
            &runtime,
            &events,
            &mut producer,
        )
        .await
        .result()
        .expect("Protocol 1.5 producer hello");
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &mut producer,
        )
        .await
        .result()
        .expect("connect producer device");
        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut producer,
        )
        .await;
        let session_id: SessionId = serde_json::from_value(
            started.result().expect("producer Session starts")["id"].clone(),
        )
        .expect("producer Session id");
        let context = UiContextRef {
            context_kind: UiContextKind::Native,
            context_id: "NATIVE_APP".to_owned(),
            document_epoch: "mixed-version-fixture".to_owned(),
        };
        events
            .append(PendingEvent {
                session_id: session_id.clone(),
                request_id: Some(RpcId::Number(4)),
                device_id: Some(DeviceId::new("mock-1")),
                at_ms: now_ms(),
                payload: TestEventPayload::ObservationCaptured {
                    observation: Box::new(Observation {
                        id: Uuid::new_v4(),
                        device_id: DeviceId::new("mock-1"),
                        captured_at_ms: now_ms(),
                        viewport: Viewport {
                            width: 100,
                            height: 100,
                            scale_factor: 1.0,
                        },
                        screenshot: None,
                        screenshot_omission: None,
                        ui_snapshot: Some(UiSnapshotRef {
                            format_version: UI_SNAPSHOT_FORMAT_VERSION,
                            context,
                            node_count: 1,
                            byte_length: 2,
                            evidence: AssetRef {
                                id: "sha256:mixed-version".to_owned(),
                                media_type: UI_SNAPSHOT_MEDIA_TYPE.to_owned(),
                                uri: "devicerail://assets/sha256/mixed-version".to_owned(),
                                sha256: Some("a".repeat(64)),
                            },
                        }),
                        ui_snapshot_omission: None,
                        metadata: Default::default(),
                    }),
                },
            })
            .await
            .expect("append Protocol 1.5 event");
        dispatch(
            request(5, "session.end", json!({})),
            &runtime,
            &events,
            &mut producer,
        )
        .await
        .result()
        .expect("end producer Session");

        let mut legacy = ConnectionState::default();
        dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[feature::EVENTS_SNAPSHOT_V1, feature::SESSION_EXPORT_PAGE_V1],
                &[],
            ),
            &runtime,
            &events,
            &mut legacy,
        )
        .await
        .result()
        .expect("Protocol 1.4 consumer hello");

        let responses = [
            dispatch(
                request(2, "events.list", json!({ "sessionId": session_id.clone() })),
                &runtime,
                &events,
                &mut legacy,
            )
            .await,
            dispatch(
                request(
                    3,
                    "session.export",
                    json!({ "sessionId": session_id.clone() }),
                ),
                &runtime,
                &events,
                &mut legacy,
            )
            .await,
            dispatch(
                request(
                    4,
                    "session.export",
                    json!({ "sessionId": session_id, "limit": 100 }),
                ),
                &runtime,
                &events,
                &mut legacy,
            )
            .await,
        ];
        for response in responses {
            let error = response
                .error()
                .expect("legacy snapshot method fails closed");
            assert_eq!(error.data.code, "session_protocol_incompatible");
            assert_eq!(
                error.data.details.as_ref().expect("compatibility details")["requiredProtocol"],
                json!({ "major": 1, "minor": 5 })
            );
        }
    }

    #[tokio::test]
    async fn ui_snapshot_get_returns_only_a_bounded_snapshot_from_the_active_session_log() {
        let root = TempDir::new().expect("temporary UI Snapshot Evidence root");
        let store = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let store_trait: Arc<dyn EvidenceStore> = store.clone();
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store_trait));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store_trait);
        let driver = Arc::new(MockDriver::new("snapshot-mock"));
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register UI Snapshot Mock Driver");
        let mut connection = ConnectionState::default();

        dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 5)),
                &[feature::OBSERVATION_UI_SNAPSHOT_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("UI Snapshot hello");
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect UI Snapshot Driver");
        let started = dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let session_id: SessionId = serde_json::from_value(
            started.result().expect("start UI Snapshot Session")["id"].clone(),
        )
        .expect("UI Snapshot Session id");

        let observation_id = Uuid::new_v4();
        let context = UiContextRef {
            context_kind: UiContextKind::Native,
            context_id: "NATIVE_APP".to_owned(),
            document_epoch: "wda-session-1".to_owned(),
        };
        let snapshot = UiSnapshot {
            format_version: UI_SNAPSHOT_FORMAT_VERSION,
            observation_id,
            context: context.clone(),
            root_stable_node_ids: vec!["root".to_owned()],
            nodes: vec![UiNode {
                stable_node_id: "root".to_owned(),
                parent_stable_node_id: None,
                role: "application".to_owned(),
                name: Some("Fixture".to_owned()),
                value: None,
                identifier: None,
                text: None,
                bounds: Some(UiRect {
                    x: 0.0,
                    y: 0.0,
                    width: 390.0,
                    height: 844.0,
                }),
                enabled: Some(true),
                hittable: Some(true),
            }],
        };
        snapshot.validate().expect("valid UI Snapshot fixture");
        let bytes = serde_json::to_vec(&snapshot).expect("serialize UI Snapshot fixture");
        let stored = store
            .put(
                PutEvidence::new(session_id.clone(), UI_SNAPSHOT_MEDIA_TYPE)
                    .expect("UI Snapshot put request")
                    .with_declared_size_bytes(bytes.len() as u64),
                Box::pin(Cursor::new(bytes.clone())),
            )
            .await
            .expect("store UI Snapshot fixture");
        let observation = Observation {
            id: observation_id,
            device_id: DeviceId::new("snapshot-mock"),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: 390,
                height: 844,
                scale_factor: 3.0,
            },
            screenshot: None,
            screenshot_omission: None,
            ui_snapshot: Some(UiSnapshotRef {
                format_version: UI_SNAPSHOT_FORMAT_VERSION,
                context,
                node_count: 1,
                byte_length: bytes.len() as u64,
                evidence: stored.asset_ref(),
            }),
            ui_snapshot_omission: None,
            metadata: Default::default(),
        };
        events
            .append(PendingEvent {
                session_id,
                request_id: Some(RpcId::Number(4)),
                device_id: Some(DeviceId::new("snapshot-mock")),
                at_ms: now_ms(),
                payload: TestEventPayload::ObservationCaptured {
                    observation: Box::new(observation),
                },
            })
            .await
            .expect("append UI Snapshot Observation");

        let fetched = dispatch_managed(
            request(
                5,
                "ui.snapshot.get",
                json!({ "observationId": observation_id }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let fetched: UiSnapshot = serde_json::from_value(
            fetched
                .result()
                .expect("bounded UI Snapshot response")
                .clone(),
        )
        .expect("typed UI Snapshot response");
        assert_eq!(fetched, snapshot);

        let missing = dispatch_managed(
            request(
                6,
                "ui.snapshot.get",
                json!({ "observationId": Uuid::new_v4() }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            missing.error().expect("Session-scoped lookup").data.code,
            "ui_snapshot_not_found"
        );
    }

    #[tokio::test]
    async fn verdict_record_accepts_only_evidence_reachable_from_the_active_session_log() {
        let root = TempDir::new().expect("temporary Verdict Evidence root");
        let store = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let store_trait: Arc<dyn EvidenceStore> = store.clone();
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store_trait));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store_trait);
        let driver = Arc::new(MockDriver::new("verdict-mock").with_session_evidence());
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register Verdict Mock Driver");
        let mut connection = ConnectionState::default();

        dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 5)),
                &[feature::VERDICT_RECORD_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("Verdict hello");
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect Verdict Driver");
        let started = dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("start Verdict Session")["id"].clone())
                .expect("Verdict Session id");

        // A valid Session pin is insufficient: failed operations may leave a
        // pin that was never committed into the append-only event log.
        let orphan = store
            .put(
                PutEvidence::new(session_id.clone(), "text/plain").expect("orphan put request"),
                Box::pin(Cursor::new(b"uncommitted pin".to_vec())),
            )
            .await
            .expect("seed uncommitted Session pin")
            .asset_ref();
        let rejected = dispatch_managed(
            request(
                4,
                "verdict.record",
                json!({
                    "verdict": {
                        "status": "unknown",
                        "summary": "must not promote an orphan pin",
                        "evidence": [orphan.clone()]
                    }
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            rejected.error().expect("orphan pin rejection").data.code,
            "evidence_not_reachable"
        );

        let observed = dispatch_managed(
            request(5, "device.observe", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let screenshot = observed.result().expect("durable Observation")["screenshot"].clone();
        let mixed = dispatch_managed(
            request(
                6,
                "verdict.record",
                json!({
                    "verdict": {
                        "status": "unknown",
                        "summary": "all references must already be reachable",
                        "evidence": [screenshot.clone(), orphan]
                    }
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            mixed
                .error()
                .expect("mixed reachable and orphan rejection")
                .data
                .code,
            "evidence_not_reachable"
        );

        let recorded = dispatch_managed(
            request(
                7,
                "verdict.record",
                json!({
                    "verdict": {
                        "status": "pass",
                        "summary": "caller supplied result",
                        "evidence": [screenshot]
                    }
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let result = recorded.result().expect("durable Verdict event");
        assert_eq!(result["event"]["payload"]["type"], "verdictRecorded");
        assert_eq!(result["event"]["payload"]["verdict"]["status"], "pass");

        let durable = events
            .list_after(&session_id, None)
            .await
            .expect("Verdict Session events");
        assert_eq!(
            durable
                .iter()
                .filter(|event| matches!(event.payload, TestEventPayload::VerdictRecorded { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn media_stream_rpc_produces_file_evidence_events_and_is_retry_safe() {
        let root = TempDir::new().expect("temporary media Evidence root");
        let store: Arc<dyn EvidenceStore> = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store);
        let driver = Arc::new(MockDriver::new("media-mock").with_session_evidence());
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register media Mock Driver");
        let mut connection = ConnectionState::default();

        let hello = dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[feature::MEDIA_STREAM_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            hello.result().expect("media hello")["features"]["enabled"],
            json!([feature::MEDIA_STREAM_V1])
        );
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect media producer");
        let started = dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("start media Session")["id"].clone())
                .expect("media Session id");
        let first_stream = "10000000-0000-4000-8000-000000000001";
        let start_params = json!({ "streamId": first_stream, "kind": "screenshot" });
        let first_start = dispatch_managed(
            request(4, "media.stream.start", start_params.clone()),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            first_start.result().expect("start media stream")["stream"]["mediaType"],
            "image/png"
        );
        let retried_start = dispatch_managed(
            request(5, "media.stream.start", start_params),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(retried_start.result(), first_start.result());
        let conflict = dispatch_managed(
            request(
                6,
                "media.stream.start",
                json!({ "streamId": first_stream, "kind": "video" }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            conflict
                .error()
                .expect("stream metadata conflict")
                .data
                .code,
            "media_stream_conflict"
        );

        let capture_params = json!({ "streamId": first_stream, "frameIndex": 1 });
        let captured = dispatch_managed(
            request(7, "media.stream.capture", capture_params.clone()),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let captured_frame = captured.result().expect("capture frame")["frame"].clone();
        assert_eq!(captured_frame["frameIndex"], 1);
        assert_eq!(captured_frame["keyFrame"], true);
        assert_eq!(captured_frame["evidence"]["mediaType"], "image/png");
        assert!(
            captured_frame["evidence"]["uri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("devicerail://assets/sha256/"))
        );
        let capture_retry = dispatch_managed(
            request(8, "media.stream.capture", capture_params),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            capture_retry.result().expect("idempotent capture retry")["frame"],
            captured_frame
        );
        let ended = dispatch_managed(
            request(9, "media.stream.end", json!({ "streamId": first_stream })),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(ended.result().expect("end stream")["frameCount"], 1);
        let event_count_after_end = events
            .list_after(&session_id, None)
            .await
            .expect("events after explicit media end")
            .len();
        let capture_after_end = dispatch_managed(
            request(
                80,
                "media.stream.capture",
                json!({ "streamId": first_stream, "frameIndex": 1 }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            capture_after_end
                .result()
                .expect("exact capture retry remains cached after end")["frame"],
            captured_frame
        );
        assert_eq!(
            events
                .list_after(&session_id, None)
                .await
                .expect("events after cached post-end retry")
                .len(),
            event_count_after_end
        );
        let rejected_after_end = dispatch_managed(
            request(
                81,
                "media.stream.capture",
                json!({ "streamId": first_stream, "frameIndex": 2 }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            rejected_after_end
                .error()
                .expect("new frame after end is rejected")
                .data
                .code,
            "media_stream_ended"
        );
        let end_retry = dispatch_managed(
            request(10, "media.stream.end", json!({ "streamId": first_stream })),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(end_retry.result(), ended.result());

        let second_stream = "10000000-0000-4000-8000-000000000002";
        dispatch_managed(
            request(
                11,
                "media.stream.start",
                json!({ "streamId": second_stream, "kind": "video" }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start stream for automatic abort");
        let third_stream = "10000000-0000-4000-8000-000000000008";
        dispatch_managed(
            request(
                110,
                "media.stream.start",
                json!({ "streamId": third_stream, "kind": "screenshot" }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start second active stream");
        let active_limit = dispatch_managed(
            request(
                109,
                "media.stream.start",
                json!({
                    "streamId": "10000000-0000-4000-8000-000000000009",
                    "kind": "screenshot"
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            active_limit.error().expect("active stream limit").data.code,
            "media_stream_active_limit"
        );
        let bounded_second_id = super::MediaStreamId(
            Uuid::parse_str(second_stream).expect("bounded second stream UUID"),
        );
        let bounded_third_id =
            super::MediaStreamId(Uuid::parse_str(third_stream).expect("bounded third stream UUID"));
        let media_manager = &connection
            .context()
            .expect("bounded media context")
            .media_streams;
        let bounded_second = media_manager
            .stream(&bounded_second_id)
            .expect("bounded second stream");
        let bounded_third = media_manager
            .stream(&bounded_third_id)
            .expect("bounded third stream");
        let held_capture = Arc::clone(&bounded_second.capture_gate)
            .try_lock_owned()
            .expect("hold one capture gate");
        let close_started = tokio::time::Instant::now();
        let close_error = super::abort_media_streams(
            &connection,
            Duration::from_millis(10),
            Some(RpcId::Number(112)),
        )
        .await
        .expect_err("one held stream exhausts the aggregate close deadline");
        assert_eq!(close_error.data.code, "media_stream_close_timed_out");
        assert!(close_started.elapsed() < Duration::from_millis(250));
        assert!(
            bounded_second
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ended_frame_count
                .is_none()
        );
        assert_eq!(
            bounded_third
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ended_frame_count,
            Some(0)
        );
        drop(held_capture);
        let unknown_action = dispatch_managed(
            request(
                111,
                "device.execute",
                json!({
                    "id": "10000000-0000-4000-8000-000000000099",
                    "name": "futureUnknownAction",
                    "arguments": {}
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            unknown_action
                .error()
                .expect("unknown action blocked during capture")
                .data
                .code,
            "media_stream_protected_action_blocked"
        );
        let missing_duration = dispatch_managed(
            request(
                12,
                "media.stream.capture",
                json!({ "streamId": second_stream, "frameIndex": 1 }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            missing_duration
                .error()
                .expect("video duration required")
                .data
                .code,
            "invalid_media_frame"
        );
        dispatch_managed(
            request(
                13,
                "media.stream.capture",
                json!({
                    "streamId": second_stream,
                    "frameIndex": 1,
                    "durationMs": 16
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("capture timed PNG video frame");
        let conflicting_retry = dispatch_managed(
            request(
                14,
                "media.stream.capture",
                json!({
                    "streamId": second_stream,
                    "frameIndex": 1,
                    "durationMs": 17
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            conflicting_retry
                .error()
                .expect("conflicting frame retry")
                .data
                .code,
            "media_frame_retry_conflict"
        );
        let second_id = devicerail_protocol::MediaStreamId(
            Uuid::parse_str(second_stream).expect("second stream UUID"),
        );
        let second_record = connection
            .context()
            .expect("negotiated media context")
            .media_streams
            .stream(&second_id)
            .expect("owned second stream");
        second_record
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_capture_at = Some(Instant::now());
        let rate_limited = dispatch_managed(
            request(
                15,
                "media.stream.capture",
                json!({
                    "streamId": second_stream,
                    "frameIndex": 2,
                    "durationMs": 16
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            rate_limited.error().expect("capture rate limit").data.code,
            "media_capture_rate_limited"
        );
        let capture_gate = Arc::clone(&second_record.capture_gate)
            .try_lock_owned()
            .expect("capture gate available");
        let busy = dispatch_managed(
            request(
                16,
                "media.stream.capture",
                json!({
                    "streamId": second_stream,
                    "frameIndex": 2,
                    "durationMs": 16
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            busy.error().expect("capture queue rejected").data.code,
            "media_stream_busy"
        );
        drop(capture_gate);
        {
            let mut state = second_record
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.frame_count = super::MAX_MEDIA_FRAMES_PER_STREAM;
        }
        let frame_limit = dispatch_managed(
            request(
                18,
                "media.stream.capture",
                json!({
                    "streamId": second_stream,
                    "frameIndex": 1001,
                    "durationMs": 16
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            frame_limit.error().expect("media frame limit").data.code,
            "media_stream_frame_limit"
        );
        second_record
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .frame_count = 1;
        dispatch_managed(
            request(17, "session.end", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("Session end aborts open streams");

        let export = events
            .export_session(&session_id)
            .await
            .expect("export media Session");
        let media_payloads = export
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    TestEventPayload::MediaStreamStarted { .. }
                        | TestEventPayload::MediaFrameCaptured { .. }
                        | TestEventPayload::MediaStreamEnded { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(media_payloads.len(), 8);
        assert_eq!(
            media_payloads
                .iter()
                .map(|event| event.request_id.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(RpcId::Number(4)),
                Some(RpcId::Number(7)),
                Some(RpcId::Number(9)),
                Some(RpcId::Number(11)),
                Some(RpcId::Number(110)),
                Some(RpcId::Number(112)),
                Some(RpcId::Number(13)),
                Some(RpcId::Number(17)),
            ]
        );
        assert!(matches!(
            media_payloads.last().map(|event| &event.payload),
            Some(TestEventPayload::MediaStreamEnded { frame_count: 1, .. })
        ));
    }

    #[tokio::test]
    async fn media_methods_are_hidden_without_a_managed_capture_producer() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let hello = dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[],
                &[feature::MEDIA_STREAM_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert!(
            hello.result().expect("legacy hello")["features"]["enabled"]
                .as_array()
                .is_some_and(|features| features.is_empty())
        );
        let unavailable = dispatch(
            request(
                2,
                "media.stream.start",
                json!({
                    "streamId": "10000000-0000-4000-8000-000000000003",
                    "kind": "screenshot"
                }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            unavailable.error().expect("media method hidden").data.code,
            "method_not_found"
        );
    }

    #[tokio::test]
    async fn media_feature_is_hidden_by_global_screenshot_omit_policy() {
        let root = TempDir::new().expect("temporary omitted media Evidence root");
        let store: Arc<dyn EvidenceStore> = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store)
            .with_screenshot_policy(ScreenshotPolicy::Omit);
        let mut connection = ConnectionState::default();
        let hello = dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[],
                &[feature::MEDIA_STREAM_V1],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert!(
            hello.result().expect("omit-policy hello")["features"]["enabled"]
                .as_array()
                .is_some_and(|features| features.is_empty())
        );
    }

    #[tokio::test]
    async fn media_stream_start_never_calls_blocking_driver_health() {
        let root = TempDir::new().expect("temporary blocking-health Evidence root");
        let store: Arc<dyn EvidenceStore> = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store);
        let driver = Arc::new(BlockingHealthMediaDriver::new("blocking-media-health"));
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register blocking health Driver");
        let mut connection = ConnectionState::default();
        dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[feature::MEDIA_STREAM_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("media hello");
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect blocking health Driver");
        dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start blocking health Session");
        let health_before = driver.health_calls.load(Ordering::SeqCst);
        driver.block_health.store(true, Ordering::SeqCst);
        let start = tokio::time::timeout(
            Duration::from_millis(100),
            dispatch_managed(
                request(
                    4,
                    "media.stream.start",
                    json!({
                        "streamId": "10000000-0000-4000-8000-000000000004",
                        "kind": "screenshot"
                    }),
                ),
                &runtime,
                &events,
                &evidence,
                &mut connection,
            ),
        )
        .await
        .expect("media start cannot wait for Driver health");
        start
            .result()
            .expect("media start uses lease-only admission");
        assert_eq!(driver.health_calls.load(Ordering::SeqCst), health_before);
    }

    #[test]
    fn media_and_sensitive_action_admission_are_atomic() {
        let manager = Arc::new(super::MediaStreamManager::default());
        let session_id = SessionId::new();
        let device_id = DeviceId::new("atomic-media-device");
        let info = devicerail_protocol::MediaStreamInfo {
            id: devicerail_protocol::MediaStreamId::new(),
            kind: devicerail_protocol::MediaStreamKind::Screenshot,
            media_type: "image/png".to_owned(),
            viewport: None,
        };
        let sensitive = manager
            .sensitive_action()
            .expect("sensitive action admitted first");
        let error = match manager.begin_start(&session_id, &device_id, &info) {
            Ok(_) => panic!("media start must be blocked by sensitive action"),
            Err(error) => error,
        };
        assert_eq!(error.data.code, "media_stream_sensitive_action_in_flight");
        drop(sensitive);
        let reservation = manager
            .begin_start(&session_id, &device_id, &info)
            .expect("media reservation admitted");
        let error = match manager.sensitive_action() {
            Ok(_) => panic!("sensitive action must be blocked by media reservation"),
            Err(error) => error,
        };
        assert_eq!(error.data.code, "media_stream_protected_action_blocked");
        drop(reservation);
    }

    #[tokio::test]
    async fn media_poison_abort_uses_capture_request_id() {
        let events = Arc::new(MemoryEventStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events
            .start_session(start)
            .await
            .expect("start poison Session");
        let device_id = DeviceId::new("poison-media-device");
        let info = super::MediaStreamInfo {
            id: super::MediaStreamId::new(),
            kind: super::MediaStreamKind::Screenshot,
            media_type: "image/png".to_owned(),
            viewport: None,
        };
        let writer = Arc::new(super::MediaStreamWriter::prepare(
            Arc::clone(&events),
            Arc::new(UnavailableEvidenceStore),
            session_id.clone(),
            Some(RpcId::Number(4)),
            Some(device_id.clone()),
            info.clone(),
            now_ms(),
        ));
        writer.ensure_started().await.expect("start poison stream");
        let stream = super::ManagedMediaStream {
            session_id: session_id.clone(),
            device_id,
            info,
            writer,
            capture_gate: Arc::new(tokio::sync::Mutex::new(())),
            state: Mutex::new(super::ManagedMediaStreamState::default()),
        };

        assert_eq!(
            super::poison_and_abort_media_stream(&stream, Some(RpcId::Number(5)))
                .await
                .expect("capture poison closes stream"),
            0
        );
        let recorded = events
            .list_after(&session_id, None)
            .await
            .expect("poison lifecycle");
        let terminal = recorded
            .iter()
            .find(|event| matches!(event.payload, TestEventPayload::MediaStreamEnded { .. }))
            .expect("poison terminal event");
        assert_eq!(terminal.request_id, Some(RpcId::Number(5)));
    }

    #[tokio::test]
    async fn media_manager_enforces_total_stream_limit_after_streams_end() {
        let manager = Arc::new(super::MediaStreamManager::default());
        let events = Arc::new(MemoryEventStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events
            .start_session(start)
            .await
            .expect("start bounded Session");
        let device_id = DeviceId::new("bounded-media-device");
        for index in 0..super::MAX_MEDIA_STREAMS_PER_SESSION {
            let info = devicerail_protocol::MediaStreamInfo {
                id: devicerail_protocol::MediaStreamId::from(Uuid::from_u128(
                    0x10000000_0000_4000_8000_000000001000_u128 + index as u128,
                )),
                kind: devicerail_protocol::MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            };
            let writer = Arc::new(super::MediaStreamWriter::prepare(
                Arc::clone(&events),
                Arc::new(UnavailableEvidenceStore),
                session_id.clone(),
                None,
                Some(device_id.clone()),
                info.clone(),
                now_ms(),
            ));
            writer.ensure_started().await.expect("start bounded stream");
            writer.finish(now_ms()).await.expect("end bounded stream");
            manager
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .streams
                .insert(
                    info.id.clone(),
                    Arc::new(super::ManagedMediaStream {
                        session_id: session_id.clone(),
                        device_id: device_id.clone(),
                        info,
                        writer,
                        capture_gate: Arc::new(tokio::sync::Mutex::new(())),
                        state: Mutex::new(super::ManagedMediaStreamState {
                            ended_frame_count: Some(0),
                            ..super::ManagedMediaStreamState::default()
                        }),
                    }),
                );
        }
        let ninth = devicerail_protocol::MediaStreamInfo {
            id: devicerail_protocol::MediaStreamId::from(Uuid::from_u128(
                0x10000000_0000_4000_8000_000000001100_u128,
            )),
            kind: devicerail_protocol::MediaStreamKind::Screenshot,
            media_type: "image/png".to_owned(),
            viewport: None,
        };
        let error = match manager.begin_start(&session_id, &device_id, &ninth) {
            Ok(_) => panic!("ninth media stream must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.data.code, "media_stream_session_limit");
    }

    #[tokio::test]
    async fn active_media_stream_blocks_protected_action_before_driver_io() {
        let root = TempDir::new().expect("temporary protected-media Evidence root");
        let store: Arc<dyn EvidenceStore> = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store);
        let driver = Arc::new(ProtectedTestDriver::new("protected-media"));
        runtime
            .register(
                driver.clone() as Arc<dyn DeviceDriver>,
                driver.device_info(),
            )
            .await
            .expect("register protected media Driver");
        let mut connection = ConnectionState::default();
        dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[feature::MEDIA_STREAM_V1, feature::ACTION_PROTECTED_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("protected media hello");
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect protected media Driver");
        dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start protected media Session");
        dispatch_managed(
            request(
                4,
                "media.stream.start",
                json!({
                    "streamId": "10000000-0000-4000-8000-000000000005",
                    "kind": "screenshot"
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start protected media stream");
        let blocked = dispatch_managed(
            request(
                5,
                "device.execute",
                json!({
                    "id": "10000000-0000-4000-8000-000000000006",
                    "name": "inputSecret",
                    "arguments": { "secret": "must-not-reach-driver" }
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            blocked.error().expect("protected action blocked").data.code,
            "media_stream_protected_action_blocked"
        );
    }

    #[tokio::test]
    async fn connection_cleanup_aborts_media_before_ending_session() {
        let root = TempDir::new().expect("temporary cleanup-media Evidence root");
        let store: Arc<dyn EvidenceStore> = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store);
        let driver = Arc::new(MockDriver::new("cleanup-media").with_session_evidence());
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register cleanup media Driver");
        let mut connection = ConnectionState::default();
        dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[feature::MEDIA_STREAM_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("cleanup media hello");
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect cleanup media Driver");
        let started = dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("cleanup Session")["id"].clone())
                .expect("cleanup Session id");
        dispatch_managed(
            request(
                4,
                "media.stream.start",
                json!({
                    "streamId": "10000000-0000-4000-8000-000000000007",
                    "kind": "screenshot"
                }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start cleanup media stream");
        let cleanup_stream_id = super::MediaStreamId(
            Uuid::parse_str("10000000-0000-4000-8000-000000000007").expect("cleanup stream UUID"),
        );
        let cleanup_stream = connection
            .context()
            .expect("cleanup media context")
            .media_streams
            .stream(&cleanup_stream_id)
            .expect("cleanup media stream");
        let held_capture = Arc::clone(&cleanup_stream.capture_gate)
            .try_lock_owned()
            .expect("hold cleanup capture gate");
        let context = connection.context().expect("cleanup lease context");
        let owner_id = LeaseOwnerId::new(context.hello.connection_id);
        let lease = context.device_lease.clone().expect("cleanup device lease");
        let cleanup_error =
            cleanup_connection(&runtime, &events, &mut connection, "injected media timeout")
                .await
                .expect_err("held media capture blocks first cleanup");
        assert!(
            cleanup_error
                .to_string()
                .contains("media_stream_close_timed_out")
        );
        assert!(matches!(
            runtime.release_lease(lease.id, owner_id, now_ms()).await,
            Err(devicerail_core::DevicePoolError::LeaseNotFound)
        ));
        assert!(
            connection
                .context()
                .is_some_and(|context| context.active_session.as_ref() == Some(&session_id))
        );
        drop(held_capture);
        cleanup_connection(&runtime, &events, &mut connection, "test disconnect")
            .await
            .expect("connection cleanup");
        assert!(
            connection
                .context()
                .is_some_and(|context| context.active_session.is_none())
        );
        let export = events
            .export_session(&session_id)
            .await
            .expect("export cleaned media Session");
        assert!(matches!(
            export.events[export.events.len() - 2].payload,
            TestEventPayload::MediaStreamEnded { frame_count: 0, .. }
        ));
        assert_eq!(export.events[export.events.len() - 2].request_id, None);
        assert!(matches!(
            export.events.last().map(|event| &event.payload),
            Some(TestEventPayload::SessionEnded {
                outcome: SessionOutcome::Shutdown,
                ..
            })
        ));
        assert_eq!(
            export.events.last().expect("cleanup terminal").request_id,
            None
        );
    }

    #[tokio::test]
    async fn media_capture_reports_post_commit_cancel_and_exact_retry_is_cached() {
        let root = TempDir::new().expect("temporary cancelled-media Evidence root");
        let inner = Arc::new(
            FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let (controller, capture_control) = ExecutionController::new();
        let store = Arc::new(CancelOnAttachStore {
            inner,
            controller,
            attaches: AtomicUsize::new(0),
        });
        let store_trait: Arc<dyn EvidenceStore> = store.clone();
        let evidence = EvidenceCleanup::Managed(Arc::clone(&store_trait));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::with_evidence(Arc::clone(&events), store_trait);
        let driver = Arc::new(MockDriver::new("cancelled-media").with_session_evidence());
        runtime
            .register(driver.clone(), driver.device_info())
            .await
            .expect("register cancelled media Driver");
        let mut connection = ConnectionState::default();
        dispatch_managed(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 4)),
                &[feature::MEDIA_STREAM_V1, feature::REQUEST_CONTROL_V1],
                &[],
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("cancelled media hello");
        dispatch_managed(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("connect cancelled media Driver");
        dispatch_managed(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start cancelled media Session");
        let stream_id = "10000000-0000-4000-8000-000000000010";
        dispatch_managed(
            request(
                4,
                "media.stream.start",
                json!({ "streamId": stream_id, "kind": "screenshot" }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("start cancelled media stream");

        let cancelled = dispatch_controlled_with_evidence(
            request(
                5,
                "media.stream.capture",
                json!({ "streamId": stream_id, "frameIndex": 1 }),
            ),
            &runtime,
            DispatchResources {
                events: events.as_ref(),
                evidence: &evidence,
                streams: None,
            },
            &mut connection,
            &capture_control,
            &RequestRegistry::default(),
        )
        .await;
        assert_eq!(
            cancelled
                .error()
                .expect("post-commit cancellation reported")
                .data
                .code,
            "request_cancelled"
        );
        assert_eq!(store.attaches.load(Ordering::SeqCst), 1);
        let retry = dispatch_managed(
            request(
                6,
                "media.stream.capture",
                json!({ "streamId": stream_id, "frameIndex": 1 }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            retry.result().expect("exact cancelled capture retry")["frame"]["frameIndex"],
            1
        );
        assert_eq!(store.attaches.load(Ordering::SeqCst), 1);
        let session_id = connection
            .context()
            .and_then(|context| context.active_session.clone())
            .expect("active cancelled media Session");
        dispatch_managed(
            request(7, "media.stream.end", json!({ "streamId": stream_id })),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await
        .result()
        .expect("end cancelled media stream");
        let events_after_end = events
            .list_after(&session_id, None)
            .await
            .expect("events after cancelled media end")
            .len();
        let post_end_retry = dispatch_managed(
            request(
                8,
                "media.stream.capture",
                json!({ "streamId": stream_id, "frameIndex": 1 }),
            ),
            &runtime,
            &events,
            &evidence,
            &mut connection,
        )
        .await;
        assert_eq!(
            post_end_retry
                .result()
                .expect("post-end exact capture retry")["frame"]["frameIndex"],
            1
        );
        assert_eq!(store.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(
            events
                .list_after(&session_id, None)
                .await
                .expect("events after post-end cached retry")
                .len(),
            events_after_end
        );
        let events = events
            .list_after(&session_id, None)
            .await
            .expect("cancelled media events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    TestEventPayload::MediaFrameCaptured { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event.payload,
                        TestEventPayload::MediaStreamStarted { .. }
                            | TestEventPayload::MediaFrameCaptured { .. }
                            | TestEventPayload::MediaStreamEnded { .. }
                    )
                })
                .map(|event| event.request_id.clone())
                .collect::<Vec<_>>(),
            vec![
                Some(RpcId::Number(4)),
                Some(RpcId::Number(5)),
                Some(RpcId::Number(7)),
            ]
        );
    }

    #[tokio::test]
    async fn stream_feature_bootstraps_one_redacted_loopback_capability() {
        let (runtime, events) = test_context().await;
        let mut streams =
            match EventStreamServer::bind(Arc::clone(&events), StreamConfig::default()).await {
                Ok(streams) => streams,
                Err(devicerail_websocket_transport::TransportError::Bind(error))
                    if error.kind() == std::io::ErrorKind::PermissionDenied
                        && matches!(
                            std::env::var("DEVICERAIL_ALLOW_NO_LOOPBACK").as_deref(),
                            Ok("1")
                        ) =>
                {
                    return;
                }
                Err(error) => panic!("bind event stream server: {error}"),
            };
        let mut connection = ConnectionState::default();
        let control = ExecutionControl::unbounded();
        let registry = RequestRegistry::default();
        let hello = dispatch_controlled_with_evidence(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 3)),
                &[feature::EVENTS_STREAM_V1],
                &[],
            ),
            &runtime,
            DispatchResources {
                events: events.as_ref(),
                evidence: &EvidenceCleanup::Disabled,
                streams: Some(&streams),
            },
            &mut connection,
            &control,
            &registry,
        )
        .await;
        assert_eq!(
            hello.result().expect("stream hello success")["features"]["enabled"],
            json!([feature::EVENTS_STREAM_V1])
        );

        let session_id = SessionId::new();
        let opened = dispatch_controlled_with_evidence(
            request(
                2,
                "events.stream.open",
                json!({
                    "sessionId": session_id.clone(),
                    "originPolicy": { "kind": "absent" }
                }),
            ),
            &runtime,
            DispatchResources {
                events: events.as_ref(),
                evidence: &EvidenceCleanup::Disabled,
                streams: Some(&streams),
            },
            &mut connection,
            &control,
            &registry,
        )
        .await;
        let result: EventsStreamOpenResult =
            serde_json::from_value(opened.result().expect("stream open success").clone())
                .expect("typed stream open result");
        assert!(
            result
                .endpoint
                .expose_secret()
                .starts_with("ws://127.0.0.1:")
        );
        assert!(!format!("{opened:?}").contains(result.endpoint.expose_secret()));
        assert_eq!(streams.stats().pending_capabilities, 1);
        streams.begin_shutdown();
        streams.finish_shutdown().await.expect("stream shutdown");
    }

    #[tokio::test]
    async fn reports_old_and_new_clients_with_stable_reasons() {
        for (offer, reason) in [
            (
                ProtocolOffer::new(vec![ProtocolRange::new(0, 1, 9)]),
                "clientTooOld",
            ),
            (
                ProtocolOffer::new(vec![ProtocolRange::new(2, 0, 3)]),
                "serverTooOld",
            ),
        ] {
            let (runtime, events) = test_context().await;
            let mut connection = ConnectionState::default();
            let response = dispatch(
                hello_request(offer, &[], &[]),
                &runtime,
                &events,
                &mut connection,
            )
            .await;

            let error = response.error().expect("incompatible protocol");
            assert_eq!(error.code, -32003);
            assert_eq!(error.data.code, "protocol_version_incompatible");
            let details = error.data.details.as_ref().expect("offer details");
            assert_eq!(details["reason"], reason);
            assert!(details.get("clientProtocol").is_some());
            assert!(details.get("serverProtocol").is_some());
        }
    }

    #[tokio::test]
    async fn invalid_offer_leaves_connection_open_for_retry() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let invalid = dispatch(
            hello_request(
                ProtocolOffer::new(vec![ProtocolRange::new(1, 2, 1)]),
                &[],
                &[],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(invalid.error().expect("invalid offer").code, -32602);

        let retry = dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert!(retry.error().is_none());
    }

    #[tokio::test]
    async fn hello_rejects_request_control_before_it_is_negotiated() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let mut hello = hello_request(
            supported_protocol_offer(),
            &[],
            &[feature::REQUEST_CONTROL_V1],
        );
        hello.timeout_ms = RequestTimeoutMs::new(100);
        let response = dispatch(hello, &runtime, &events, &mut connection).await;
        let error = response.error().expect("pre-negotiation timeout rejected");
        assert_eq!(error.code, -32602);
        assert_eq!(error.data.code, "feature_not_negotiated");
        assert!(connection.context().is_none());

        let retry = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::REQUEST_CONTROL_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert!(retry.error().is_none());
    }

    #[tokio::test]
    async fn required_feature_failure_is_retryable_and_optional_unknown_is_ignored() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let missing = dispatch(
            hello_request(supported_protocol_offer(), &["events.push.v1"], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let error = missing.error().expect("unsupported required feature");
        assert_eq!(error.code, -32004);
        assert_eq!(error.data.code, "required_feature_unsupported");

        let retry = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::EVENTS_SNAPSHOT_V1, "events.push.v1"],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            retry.result().expect("retry succeeds")["features"]["enabled"],
            json!([feature::EVENTS_SNAPSHOT_V1])
        );

        let list = dispatch(
            request(2, "events.list", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            list.error().expect("session is required").data.code,
            "session_required"
        );

        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let session_id = started.result().expect("session starts")["id"].clone();

        let list = dispatch(
            request(4, "events.list", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let listed = list.result().expect("snapshot feature enables list");
        assert_eq!(listed.as_array().expect("event list").len(), 1);
        assert_eq!(listed[0]["payload"]["type"], "sessionStarted");

        let clear = dispatch(
            request(
                5,
                "events.clear",
                json!({ "sessionId": session_id.clone() }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            clear
                .error()
                .expect("active logs are append-only")
                .data
                .code,
            "session_active"
        );

        let ended = dispatch(
            request(6, "session.end", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(ended.result().expect("session ends")["state"], "ended");

        let clear = dispatch(
            request(7, "events.clear", json!({ "sessionId": session_id })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            clear.result().expect("ended session can be deleted")["deleted"],
            true
        );
    }

    #[tokio::test]
    async fn request_control_is_not_advertised_to_protocol_one_minor_zero() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let response = dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 0)),
                &[],
                &[feature::REQUEST_CONTROL_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let hello = response.result().expect("1.0 hello");
        assert_eq!(
            hello["protocol"]["selected"],
            json!({ "major": 1, "minor": 0 })
        );
        assert_eq!(hello["features"]["enabled"], json!([]));

        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let response = dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 0)),
                &[feature::REQUEST_CONTROL_V1],
                &[],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            response
                .error()
                .expect("1.0 cannot require 1.1 feature")
                .data
                .code,
            "required_feature_unsupported"
        );
    }

    #[tokio::test]
    async fn device_routing_feature_requires_protocol_one_minor_two() {
        for version in [ProtocolVersion::new(1, 0), ProtocolVersion::new(1, 1)] {
            let (runtime, events) = test_context().await;
            let mut connection = ConnectionState::default();
            let response = dispatch(
                hello_request(
                    ProtocolOffer::exact(version),
                    &[],
                    &[feature::DEVICE_ROUTING_V1],
                ),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            assert_eq!(
                response.result().expect("older hello succeeds")["features"]["enabled"],
                json!([])
            );

            let (runtime, events) = test_context().await;
            let mut connection = ConnectionState::default();
            let response = dispatch(
                hello_request(
                    ProtocolOffer::exact(version),
                    &[feature::DEVICE_ROUTING_V1],
                    &[],
                ),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            assert_eq!(
                response
                    .error()
                    .expect("older protocol cannot require routing")
                    .data
                    .code,
                "required_feature_unsupported"
            );
        }

        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let response = dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 2)),
                &[feature::DEVICE_ROUTING_V1],
                &[],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            response.result().expect("1.2 routing hello")["features"]["enabled"],
            json!([feature::DEVICE_ROUTING_V1])
        );
    }

    #[tokio::test]
    async fn protected_action_feature_requires_protocol_one_minor_two() {
        for version in [ProtocolVersion::new(1, 0), ProtocolVersion::new(1, 1)] {
            let (runtime, events) = test_context().await;
            let mut connection = ConnectionState::default();
            let optional = dispatch(
                hello_request(
                    ProtocolOffer::exact(version),
                    &[],
                    &[feature::ACTION_PROTECTED_V1],
                ),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            assert_eq!(
                optional.result().expect("older hello succeeds")["features"]["enabled"],
                json!([])
            );

            let (runtime, events) = test_context().await;
            let mut connection = ConnectionState::default();
            let required = dispatch(
                hello_request(
                    ProtocolOffer::exact(version),
                    &[feature::ACTION_PROTECTED_V1],
                    &[],
                ),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            assert_eq!(
                required
                    .error()
                    .expect("older protocol cannot require protected Actions")
                    .data
                    .code,
                "required_feature_unsupported"
            );
        }

        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let response = dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 2)),
                &[feature::ACTION_PROTECTED_V1],
                &[],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            response.result().expect("1.2 protected Action hello")["features"]["enabled"],
            json!([feature::ACTION_PROTECTED_V1])
        );
    }

    #[tokio::test]
    async fn routing_methods_are_gated_and_list_select_are_stable() {
        let (runtime, events) =
            test_context_with_drivers(vec![MockDriver::new("mock-b"), MockDriver::new("mock-a")])
                .await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        for method in ["devices.list", "device.select"] {
            let params = if method == "device.select" {
                json!({ "deviceId": "mock-a" })
            } else {
                json!({})
            };
            let response = dispatch(
                request(2, method, params),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            let error = response.error().expect("routing method is feature gated");
            assert_eq!(error.code, -32601);
            assert_eq!(
                error.data.details.as_ref().expect("feature details")["requiredFeature"],
                feature::DEVICE_ROUTING_V1
            );
        }

        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::DEVICE_ROUTING_V1, feature::REQUEST_CONTROL_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let listed = dispatch(
            request(3, "devices.list", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let listed = listed.result().expect("devices.list succeeds");
        assert_eq!(listed["selectedDeviceId"], Value::Null);
        assert_eq!(listed["devices"][0]["id"], "mock-a");
        assert_eq!(listed["devices"][1]["id"], "mock-b");

        for (id, method, params) in [
            (4, "devices.list", json!({})),
            (5, "device.select", json!({ "deviceId": "mock-a" })),
        ] {
            let mut request = request(id, method, params);
            request.timeout_ms = RequestTimeoutMs::new(10);
            let response = dispatch(request, &runtime, &events, &mut connection).await;
            assert_eq!(
                response
                    .error()
                    .expect("routing admin timeout is rejected")
                    .data
                    .code,
                "request_timeout_not_supported"
            );
        }

        for id in [6, 7] {
            let selected = dispatch(
                request(id, "device.select", json!({ "deviceId": "mock-b" })),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            assert_eq!(
                selected.result().expect("idempotent selection")["device"]["id"],
                "mock-b"
            );
        }

        let missing = dispatch(
            request(8, "device.select", json!({ "deviceId": "missing" })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let error = missing.error().expect("unknown device is explicit");
        assert_eq!(error.code, -32011);
        assert_eq!(error.data.code, "device_not_found");
        assert!(!error.data.retryable);
        assert_eq!(
            error.data.details.as_ref().expect("missing id")["deviceId"],
            "missing"
        );

        let listed = dispatch(
            request(9, "devices.list", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            listed.result().expect("old selection is retained")["selectedDeviceId"],
            "mock-b"
        );
    }

    #[tokio::test]
    async fn multi_and_zero_device_routes_fail_explicitly() {
        let (runtime, events) =
            test_context_with_drivers(vec![MockDriver::new("mock-b"), MockDriver::new("mock-a")])
                .await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::DEVICE_ROUTING_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        for (id, method, params) in [
            (2, "device.connect", json!({})),
            (3, "device.disconnect", json!({})),
            (4, "device.capabilities", json!({})),
            (5, "device.observe", json!({})),
            (
                6,
                "device.execute",
                json!({
                    "id": "66666666-6666-4666-8666-666666666666",
                    "name": "tap",
                    "arguments": { "x": 1, "y": 2 }
                }),
            ),
        ] {
            let response = dispatch(
                request(id, method, params),
                &runtime,
                &events,
                &mut connection,
            )
            .await;
            let error = response
                .error()
                .expect("multi-device route needs selection");
            assert_eq!(error.code, -32011, "{method}");
            assert_eq!(error.data.code, "device_selection_required", "{method}");
            assert!(error.data.retryable, "{method}");
            let details = error.data.details.as_ref().expect("selection details");
            assert_eq!(details["deviceCount"], 2);
            assert_eq!(details["availableDeviceIds"], json!(["mock-a", "mock-b"]));
        }

        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events));
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let response = dispatch(
            request(7, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let error = response.error().expect("empty registry is explicit");
        assert_eq!(error.code, -32011);
        assert_eq!(error.data.code, "device_not_found");
        assert!(error.data.retryable);
        assert_eq!(
            error.data.details.as_ref().expect("empty reason")["reason"],
            "noRegisteredDevices"
        );
    }

    #[tokio::test]
    async fn protocol_one_minor_zero_single_device_flow_remains_compatible() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let hello = dispatch(
            hello_request(ProtocolOffer::exact(ProtocolVersion::new(1, 0)), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            hello.result().expect("legacy hello")["protocol"]["selected"],
            json!({ "major": 1, "minor": 0 })
        );
        assert!(
            connection
                .context()
                .is_some_and(|context| context.selected_device_id.is_none())
        );

        let connected = dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            connected.result().expect("legacy connect")["id"],
            "mock-test"
        );
        assert_eq!(
            connection
                .context()
                .and_then(|context| context.selected_device_id.as_ref()),
            Some(&DeviceId::new("mock-test"))
        );

        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("legacy session")["id"].clone())
                .expect("session id");

        let observed = dispatch(
            request(4, "device.observe", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            observed.result().expect("legacy observation")["deviceId"],
            "mock-test"
        );
        let executed = dispatch(
            request(
                5,
                "device.execute",
                json!({
                    "id": "55555555-5555-4555-8555-555555555555",
                    "name": "tap",
                    "arguments": { "x": 3, "y": 4 }
                }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            executed.result().expect("legacy action")["callId"],
            "55555555-5555-4555-8555-555555555555"
        );
        dispatch(
            request(6, "session.end", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await
        .result()
        .expect("legacy session ends");

        let replay = events
            .export_session(&session_id)
            .await
            .expect("legacy session replay");
        assert_eq!(replay.events.first().expect("start").device_id, None);
        assert_eq!(replay.events.last().expect("end").device_id, None);
        assert!(
            replay.events[1..replay.events.len() - 1]
                .iter()
                .all(|event| event.device_id.as_ref() == Some(&DeviceId::new("mock-test")))
        );
    }

    #[tokio::test]
    async fn device_selection_is_scoped_to_each_connection() {
        let (runtime, events) =
            test_context_with_drivers(vec![MockDriver::new("mock-a"), MockDriver::new("mock-b")])
                .await;
        let mut first = ConnectionState::default();
        let mut second = ConnectionState::default();
        for connection in [&mut first, &mut second] {
            dispatch(
                hello_request(
                    supported_protocol_offer(),
                    &[],
                    &[feature::DEVICE_ROUTING_V1],
                ),
                &runtime,
                &events,
                connection,
            )
            .await;
        }

        dispatch(
            request(2, "device.select", json!({ "deviceId": "mock-a" })),
            &runtime,
            &events,
            &mut first,
        )
        .await;
        dispatch(
            request(3, "device.select", json!({ "deviceId": "mock-b" })),
            &runtime,
            &events,
            &mut second,
        )
        .await;

        let first_list = dispatch(
            request(4, "devices.list", json!({})),
            &runtime,
            &events,
            &mut first,
        )
        .await;
        let second_list = dispatch(
            request(5, "devices.list", json!({})),
            &runtime,
            &events,
            &mut second,
        )
        .await;
        assert_eq!(
            first_list.result().expect("first selection")["selectedDeviceId"],
            "mock-a"
        );
        assert_eq!(
            second_list.result().expect("second selection")["selectedDeviceId"],
            "mock-b"
        );
    }

    #[tokio::test]
    async fn device_pool_excludes_other_clients_until_session_release() {
        let (runtime, events) = test_context().await;
        let mut first = ConnectionState::default();
        let mut second = ConnectionState::default();
        for connection in [&mut first, &mut second] {
            dispatch(
                hello_request(supported_protocol_offer(), &[], &[]),
                &runtime,
                &events,
                connection,
            )
            .await
            .result()
            .expect("handshake");
        }

        dispatch(
            request(2, "session.start", json!({})),
            &runtime,
            &events,
            &mut first,
        )
        .await
        .result()
        .expect("first owner acquires lease");
        let rejected = dispatch(
            request(2, "session.start", json!({})),
            &runtime,
            &events,
            &mut second,
        )
        .await;
        assert_eq!(
            rejected.error().expect("second owner rejected").data.code,
            "device_in_use"
        );

        dispatch(
            request(3, "session.end", json!({})),
            &runtime,
            &events,
            &mut first,
        )
        .await
        .result()
        .expect("first owner releases lease");
        dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut second,
        )
        .await
        .result()
        .expect("second owner acquires released device");

        let entries = runtime.pool_entries(now_ms()).await;
        let lease = entries[0].lease.as_ref().expect("active second lease");
        assert_eq!(
            lease.owner_id,
            connection_owner(&second).expect("second owner")
        );
    }

    #[tokio::test]
    async fn loopback_rpc_clients_share_one_real_lease_authority() {
        async fn rpc(client: &mut TokioBufReader<TcpStream>, request: RpcRequest) -> RpcResponse {
            let mut frame = serde_json::to_vec(&request).expect("request JSON");
            frame.push(b'\n');
            client
                .get_mut()
                .write_all(&frame)
                .await
                .expect("write request");
            let mut response = String::new();
            client
                .read_line(&mut response)
                .await
                .expect("read response");
            serde_json::from_str(&response).expect("response JSON")
        }

        let (runtime, events) = test_context().await;
        let runtime = Arc::new(runtime);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let server_runtime = Arc::clone(&runtime);
        let server_events = Arc::clone(&events);
        let server = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            for _ in 0..2 {
                let (socket, _) = listener.accept().await.expect("accept client");
                let runtime = Arc::clone(&server_runtime);
                let events = Arc::clone(&server_events);
                clients.spawn(async move {
                    serve_loopback_connection(socket, runtime, events, EvidenceCleanup::Disabled)
                        .await
                });
            }
            while let Some(result) = clients.join_next().await {
                result.expect("client task").expect("client service");
            }
        });

        let mut first =
            TokioBufReader::new(TcpStream::connect(address).await.expect("first TCP client"));
        let mut second = TokioBufReader::new(
            TcpStream::connect(address)
                .await
                .expect("second TCP client"),
        );
        for client in [&mut first, &mut second] {
            let hello = rpc(client, hello_request(supported_protocol_offer(), &[], &[])).await;
            assert_eq!(
                hello.result().expect("TCP handshake")["transport"]["kind"],
                "tcp"
            );
        }

        rpc(&mut first, request(2, "session.start", json!({})))
            .await
            .result()
            .expect("first TCP client acquires lease");
        let rejected = rpc(&mut second, request(2, "session.start", json!({}))).await;
        assert_eq!(
            rejected
                .error()
                .expect("second TCP client rejected")
                .data
                .code,
            "device_in_use"
        );
        rpc(&mut first, request(3, "session.end", json!({})))
            .await
            .result()
            .expect("first TCP client releases lease");
        rpc(&mut second, request(3, "session.start", json!({})))
            .await
            .result()
            .expect("second TCP client acquires released lease");

        first
            .get_mut()
            .shutdown()
            .await
            .expect("close first client");
        second
            .get_mut()
            .shutdown()
            .await
            .expect("close second client");
        server.await.expect("loopback server");
        assert!(
            runtime
                .pool_entries(now_ms())
                .await
                .iter()
                .all(|entry| entry.lease.is_none())
        );
    }

    #[tokio::test]
    async fn global_loopback_shutdown_drains_requests_and_cleans_every_connection() {
        async fn rpc(client: &mut TokioBufReader<TcpStream>, request: RpcRequest) -> RpcResponse {
            send_request(client, request).await;
            read_response(client).await
        }

        async fn send_request(client: &mut TokioBufReader<TcpStream>, request: RpcRequest) {
            let mut frame = serde_json::to_vec(&request).expect("request JSON");
            frame.push(b'\n');
            client
                .get_mut()
                .write_all(&frame)
                .await
                .expect("write request");
        }

        async fn read_response(client: &mut TokioBufReader<TcpStream>) -> RpcResponse {
            let mut response = String::new();
            client
                .read_line(&mut response)
                .await
                .expect("read response");
            serde_json::from_str(&response).expect("response JSON")
        }

        let (runtime, events) = test_context_with_drivers(vec![
            MockDriver::new("mock-a").with_action_delay(Duration::from_secs(30)),
            MockDriver::new("mock-b").with_action_delay(Duration::from_secs(30)),
        ])
        .await;
        let runtime = Arc::new(runtime);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel();
        let server_runtime = Arc::clone(&runtime);
        let server_events = Arc::clone(&events);
        let server = tokio::spawn(async move {
            serve_loopback_listener(
                server_runtime,
                server_events,
                EvidenceCleanup::Disabled,
                None,
                listener,
                async move {
                    shutdown_receiver
                        .await
                        .map_err(|_| std::io::Error::other("test shutdown sender was dropped"))
                },
            )
            .await
            .map_err(|error| error.to_string())
        });

        let mut first =
            TokioBufReader::new(TcpStream::connect(address).await.expect("first TCP client"));
        let mut second = TokioBufReader::new(
            TcpStream::connect(address)
                .await
                .expect("second TCP client"),
        );
        let mut session_ids = Vec::new();
        for (client, device_id) in [(&mut first, "mock-a"), (&mut second, "mock-b")] {
            rpc(
                client,
                hello_request(
                    supported_protocol_offer(),
                    &[],
                    &[feature::DEVICE_ROUTING_V1],
                ),
            )
            .await
            .result()
            .expect("TCP handshake");
            rpc(
                client,
                request(2, "device.select", json!({ "deviceId": device_id })),
            )
            .await
            .result()
            .expect("select device");
            rpc(client, request(3, "device.connect", json!({})))
                .await
                .result()
                .expect("connect device");
            let started = rpc(client, request(4, "session.start", json!({}))).await;
            session_ids.push(
                serde_json::from_value::<SessionId>(
                    started.result().expect("start session")["id"].clone(),
                )
                .expect("session id"),
            );
        }

        for (client, call_id) in [
            (&mut first, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
            (&mut second, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        ] {
            send_request(
                client,
                request(
                    5,
                    "device.execute",
                    json!({
                        "id": call_id,
                        "name": "tap",
                        "arguments": { "x": 10, "y": 20 }
                    }),
                ),
            )
            .await;
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let mut all_started = true;
                for session_id in &session_ids {
                    let replay = events
                        .list_after(session_id, None)
                        .await
                        .expect("session events");
                    all_started &= replay.iter().any(|event| {
                        matches!(event.payload, TestEventPayload::ActionStarted { .. })
                    });
                }
                if all_started {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both actions become durable before shutdown");

        shutdown_sender.send(()).expect("signal global shutdown");
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("global shutdown must not hang")
            .expect("server task")
            .expect("graceful server shutdown");

        for client in [&mut first, &mut second] {
            let cancelled = tokio::time::timeout(Duration::from_secs(1), read_response(client))
                .await
                .expect("cancelled request response");
            assert_eq!(
                cancelled.error().expect("cancelled action").data.code,
                "request_cancelled"
            );
        }

        for (session_id, device_id) in session_ids.iter().zip(["mock-a", "mock-b"]) {
            let replay = events
                .export_session(session_id)
                .await
                .expect("ended session");
            let last = replay.events.last().expect("terminal session event");
            assert_eq!(last.device_id.as_ref(), Some(&DeviceId::new(device_id)));
            assert!(matches!(
                &last.payload,
                TestEventPayload::SessionEnded {
                    outcome: SessionOutcome::Shutdown,
                    reason: Some(reason),
                } if reason == "daemon shutdown"
            ));
        }
        assert!(
            runtime
                .pool_entries(now_ms())
                .await
                .iter()
                .all(|entry| entry.lease.is_none())
        );
    }

    #[tokio::test]
    async fn global_shutdown_cancels_inline_management_rpc_before_connection_cleanup() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::loopback_tcp();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::DEVICE_ROUTING_V1, feature::EVENTS_SNAPSHOT_V1],
            ),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await
        .result()
        .expect("handshake");
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await
        .result()
        .expect("connect device");
        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            events.as_ref(),
            &mut connection,
        )
        .await;
        let active_session_id = serde_json::from_value::<SessionId>(
            started.result().expect("start active Session")["id"].clone(),
        )
        .expect("active Session id");

        let ended = StartSession::new(None, None, now_ms());
        let ended_session_id = ended.session_id.clone();
        events
            .start_session(ended)
            .await
            .expect("seed ended Session");
        events
            .end_session(EndSession {
                session_id: ended_session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end cleanup target Session");

        let evidence_root = TempDir::new().expect("temporary Evidence root");
        let inner = Arc::new(
            FileEvidenceStore::new(evidence_root.path(), FileEvidenceStoreConfig::default())
                .expect("File Evidence Store"),
        );
        let release_started = Arc::new(Notify::new());
        let release_dropped = Arc::new(AtomicBool::new(false));
        let store: Arc<dyn EvidenceStore> = Arc::new(BlockingReleaseStore {
            inner,
            release_started: Arc::clone(&release_started),
            release_dropped: Arc::clone(&release_dropped),
        });
        let evidence = EvidenceCleanup::Managed(store);
        let requests = RequestRegistry::default();
        let (controller, control) = ExecutionController::new();
        let dispatch = dispatch_controlled_with_evidence(
            request(4, "events.clear", json!({ "sessionId": ended_session_id })),
            &runtime,
            DispatchResources {
                events: events.as_ref(),
                evidence: &evidence,
                streams: None,
            },
            &mut connection,
            &control,
            &requests,
        );
        let (shutdown_sender, mut shutdown_receiver) =
            tokio::sync::watch::channel::<Option<tokio::time::Instant>>(None);
        let trigger_shutdown = async {
            tokio::time::timeout(Duration::from_secs(1), release_started.notified())
                .await
                .expect("inline Evidence release starts");
            shutdown_sender
                .send(Some(tokio::time::Instant::now() + Duration::from_secs(1)))
                .expect("signal global shutdown");
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                dispatch_inline_until_shutdown(dispatch, &controller, &mut shutdown_receiver),
                trigger_shutdown,
            )
        })
        .await
        .expect("inline management RPC must not delay connection cleanup");
        assert!(matches!(outcome, InlineDispatchOutcome::Shutdown(_)));
        assert!(
            release_dropped.load(Ordering::SeqCst),
            "shutdown must drop the blocked inline dispatch future"
        );

        cleanup_connection(
            &runtime,
            events.as_ref(),
            &mut connection,
            "daemon shutdown",
        )
        .await
        .expect("connection cleanup");

        let replay = events
            .export_session(&active_session_id)
            .await
            .expect("active Session is ended during connection cleanup");
        let last = replay.events.last().expect("terminal active Session event");
        assert_eq!(last.device_id.as_ref(), Some(&DeviceId::new("mock-test")));
        assert!(matches!(
            &last.payload,
            TestEventPayload::SessionEnded {
                outcome: SessionOutcome::Shutdown,
                reason: Some(reason),
            } if reason == "daemon shutdown"
        ));
        assert!(
            runtime
                .pool_entries(now_ms())
                .await
                .iter()
                .all(|entry| entry.lease.is_none())
        );
    }

    #[tokio::test]
    async fn active_session_lease_rejects_selection_changes_and_preserves_admitted_route() {
        let (runtime, events) = test_context_with_drivers(vec![
            MockDriver::new("mock-a").with_action_delay(Duration::from_millis(100)),
            MockDriver::new("mock-b"),
        ])
        .await;
        let runtime = Arc::new(runtime);
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::DEVICE_ROUTING_V1],
            ),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        for (id, device_id) in [(2, "mock-a"), (4, "mock-b")] {
            dispatch(
                request(id, "device.select", json!({ "deviceId": device_id })),
                runtime.as_ref(),
                events.as_ref(),
                &mut connection,
            )
            .await;
            dispatch(
                request(id + 1, "device.connect", json!({})),
                runtime.as_ref(),
                events.as_ref(),
                &mut connection,
            )
            .await;
        }
        dispatch(
            request(6, "device.select", json!({ "deviceId": "mock-a" })),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        let started = dispatch(
            request(7, "session.start", json!({})),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("session")["id"].clone())
                .expect("session id");

        let admitted = runtime
            .resolve(&DeviceId::new("mock-a"))
            .await
            .expect("admit A route");
        let mut task_connection = connection.clone();
        let task_runtime = Arc::clone(&runtime);
        let task_events = Arc::clone(&events);
        let action_a = tokio::spawn(async move {
            dispatch_routed(
                request(
                    8,
                    "device.execute",
                    json!({
                        "id": "88888888-8888-4888-8888-888888888888",
                        "name": "tap",
                        "arguments": { "x": 8, "y": 8 }
                    }),
                ),
                task_runtime.as_ref(),
                task_events.as_ref(),
                &mut task_connection,
                &ExecutionControl::unbounded(),
                &RequestRegistry::default(),
                Some(Ok(admitted)),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let replay = events.list_after(&session_id, None).await.expect("events");
                if replay.iter().any(|event| {
                    event.device_id.as_ref() == Some(&DeviceId::new("mock-a"))
                        && matches!(event.payload, TestEventPayload::ActionStarted { .. })
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("A action starts");

        let rejected_selection = dispatch(
            request(9, "device.select", json!({ "deviceId": "mock-b" })),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert_eq!(
            rejected_selection
                .error()
                .expect("selection rejected")
                .data
                .code,
            "device_lease_active"
        );
        let second_action_a = dispatch(
            request(
                10,
                "device.execute",
                json!({
                    "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                    "name": "tap",
                    "arguments": { "x": 10, "y": 10 }
                }),
            ),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert_eq!(
            second_action_a.result().expect("second A action")["callId"],
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        let action_a = action_a.await.expect("A task");
        assert_eq!(
            action_a.result().expect("admitted A action")["callId"],
            "88888888-8888-4888-8888-888888888888"
        );

        dispatch(
            request(11, "session.end", json!({})),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await
        .result()
        .expect("session ends");
        let replay = events
            .export_session(&session_id)
            .await
            .expect("multi-device replay");
        assert_eq!(
            replay.events.first().expect("start").device_id,
            Some(DeviceId::new("mock-a"))
        );
        assert_eq!(
            replay.events.last().expect("end").device_id,
            Some(DeviceId::new("mock-a"))
        );
        let action_devices = replay
            .events
            .iter()
            .filter_map(|event| match event.payload {
                TestEventPayload::ActionStarted { .. }
                | TestEventPayload::ActionCompleted { .. } => event.device_id.clone(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            action_devices,
            vec![
                DeviceId::new("mock-a"),
                DeviceId::new("mock-a"),
                DeviceId::new("mock-a"),
                DeviceId::new("mock-a")
            ]
        );
    }

    #[tokio::test]
    async fn session_rpc_records_correlated_replayable_action_events() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::EVENTS_SNAPSHOT_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let session_id = started.result().expect("session starts")["id"].clone();

        let call_id = "22222222-2222-4222-8222-222222222222";
        let execute = dispatch(
            request(
                4,
                "device.execute",
                json!({
                    "id": call_id,
                    "name": "tap",
                    "arguments": { "x": 10, "y": 20 }
                }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            execute.result().expect("action succeeds")["callId"],
            call_id
        );

        let list = dispatch(
            request(5, "events.list", json!({ "afterSequence": 1 })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let action_events = list
            .result()
            .expect("list action events")
            .as_array()
            .expect("event array");
        assert_eq!(action_events.len(), 2);
        assert_eq!(action_events[0]["sequence"], 2);
        assert_eq!(action_events[1]["sequence"], 3);
        assert_eq!(action_events[0]["requestId"], 4);
        assert_eq!(action_events[1]["requestId"], 4);
        assert_eq!(action_events[0]["deviceId"], "mock-test");
        assert_eq!(action_events[0]["payload"]["type"], "actionStarted");
        assert_eq!(action_events[1]["payload"]["type"], "actionCompleted");
        assert_eq!(
            action_events[1]["payload"]["outcome"]["outcome"],
            "succeeded"
        );

        let first_page = dispatch(
            request(50, "events.list", json!({ "afterSequence": 1, "limit": 1 })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let first_page = first_page
            .result()
            .expect("bounded first page")
            .as_array()
            .expect("bounded first page array");
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0]["sequence"], 2);

        let second_page = dispatch(
            request(51, "events.list", json!({ "afterSequence": 2, "limit": 1 })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let second_page = second_page
            .result()
            .expect("bounded second page")
            .as_array()
            .expect("bounded second page array");
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0]["sequence"], 3);

        let exhausted = dispatch(
            request(52, "events.list", json!({ "afterSequence": 3, "limit": 1 })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert!(
            exhausted
                .result()
                .expect("exhausted page")
                .as_array()
                .expect("exhausted page array")
                .is_empty()
        );
        let invalid_limit = dispatch(
            request(53, "events.list", json!({ "limit": 0 })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            invalid_limit.error().expect("invalid page limit").data.code,
            "invalid_params"
        );

        let ended = dispatch(
            request(
                6,
                "session.end",
                json!({ "outcome": "completed", "reason": "test complete" }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(ended.result().expect("end session")["lastSequence"], 4);

        let exported = dispatch(
            request(7, "session.export", json!({ "sessionId": session_id })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let export = exported.result().expect("export ended session");
        assert_eq!(export["events"].as_array().expect("events").len(), 4);
        assert_eq!(export["events"][3]["payload"]["type"], "sessionEnded");
        assert!(export.get("nextAfterSequence").is_none());

        let page_without_feature = dispatch(
            request(
                70,
                "session.export",
                json!({ "sessionId": session_id.clone(), "limit": 1 }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let page_error = page_without_feature
            .error()
            .expect("pagination feature is required");
        assert_eq!(page_error.data.code, "method_not_found");
        assert_eq!(
            page_error.data.details.as_ref().expect("feature details")["requiredFeature"],
            feature::SESSION_EXPORT_PAGE_V1
        );

        let cursor_without_limit = dispatch(
            request(
                71,
                "session.export",
                json!({ "sessionId": session_id.clone(), "afterSequence": 1 }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            cursor_without_limit
                .error()
                .expect("cursor without limit is invalid")
                .data
                .code,
            "invalid_params"
        );

        let invalid_export_limit = dispatch(
            request(
                74,
                "session.export",
                json!({ "sessionId": session_id.clone(), "limit": 0 }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            invalid_export_limit
                .error()
                .expect("invalid export limit")
                .data
                .code,
            "invalid_params"
        );
        let excessive_export_limit = dispatch(
            request(
                75,
                "session.export",
                json!({ "sessionId": session_id.clone(), "limit": 1001 }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            excessive_export_limit
                .error()
                .expect("excessive export limit")
                .data
                .code,
            "invalid_params"
        );

        let mut paging_connection = ConnectionState::default();
        let paging_hello = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::EVENTS_SNAPSHOT_V1, feature::SESSION_EXPORT_PAGE_V1],
            ),
            &runtime,
            &events,
            &mut paging_connection,
        )
        .await;
        assert!(
            paging_hello.result().expect("paging hello")["features"]["enabled"]
                .as_array()
                .expect("enabled features")
                .iter()
                .any(|value| value == feature::SESSION_EXPORT_PAGE_V1)
        );

        let first_export_page = dispatch(
            request(
                72,
                "session.export",
                json!({ "sessionId": session_id.clone(), "limit": 2 }),
            ),
            &runtime,
            &events,
            &mut paging_connection,
        )
        .await;
        let first_export_page = first_export_page.result().expect("first export page");
        assert_eq!(
            first_export_page["events"]
                .as_array()
                .expect("first page events")
                .len(),
            2
        );
        assert_eq!(first_export_page["events"][0]["sequence"], 1);
        assert_eq!(first_export_page["events"][1]["sequence"], 2);
        assert_eq!(first_export_page["nextAfterSequence"], 2);
        assert_eq!(first_export_page["session"]["eventCount"], 4);

        let final_export_page = dispatch(
            request(
                73,
                "session.export",
                json!({
                    "sessionId": session_id,
                    "afterSequence": 2,
                    "limit": 2
                }),
            ),
            &runtime,
            &events,
            &mut paging_connection,
        )
        .await;
        let final_export_page = final_export_page.result().expect("final export page");
        assert_eq!(
            final_export_page["events"]
                .as_array()
                .expect("final page events")
                .len(),
            2
        );
        assert_eq!(final_export_page["events"][0]["sequence"], 3);
        assert_eq!(final_export_page["events"][1]["sequence"], 4);
        assert!(final_export_page.get("nextAfterSequence").is_none());

        let exhausted_export_page = dispatch(
            request(
                76,
                "session.export",
                json!({
                    "sessionId": session_id.clone(),
                    "afterSequence": 4,
                    "limit": 2
                }),
            ),
            &runtime,
            &events,
            &mut paging_connection,
        )
        .await;
        let exhausted_export_page = exhausted_export_page
            .result()
            .expect("exhausted export page");
        assert!(
            exhausted_export_page["events"]
                .as_array()
                .expect("exhausted events")
                .is_empty()
        );
        assert!(exhausted_export_page.get("nextAfterSequence").is_none());

        let cursor_ahead = dispatch(
            request(
                77,
                "session.export",
                json!({
                    "sessionId": session_id,
                    "afterSequence": 5,
                    "limit": 2
                }),
            ),
            &runtime,
            &events,
            &mut paging_connection,
        )
        .await;
        assert_eq!(
            cursor_ahead.error().expect("cursor ahead").data.code,
            "event_cursor_ahead"
        );

        let observe = dispatch(
            request(8, "device.observe", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            observe.error().expect("session ended").data.code,
            "session_required"
        );
    }

    #[tokio::test]
    async fn export_paging_closes_feature_dependencies_and_preserves_protocol_one_three() {
        let (runtime, events) = test_context().await;

        let mut optional_page_only = ConnectionState::default();
        let optional_hello = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::SESSION_EXPORT_PAGE_V1],
            ),
            &runtime,
            &events,
            &mut optional_page_only,
        )
        .await;
        assert!(
            !optional_hello.result().expect("optional hello")["features"]["enabled"]
                .as_array()
                .expect("enabled features")
                .iter()
                .any(|value| value == feature::SESSION_EXPORT_PAGE_V1)
        );

        let mut required_page_only = ConnectionState::default();
        let required_hello = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[feature::SESSION_EXPORT_PAGE_V1],
                &[],
            ),
            &runtime,
            &events,
            &mut required_page_only,
        )
        .await;
        assert_eq!(
            required_hello
                .error()
                .expect("missing base feature dependency")
                .data
                .code,
            "required_feature_unsupported"
        );

        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start session");

        let mut paging = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::EVENTS_SNAPSHOT_V1, feature::SESSION_EXPORT_PAGE_V1],
            ),
            &runtime,
            &events,
            &mut paging,
        )
        .await
        .result()
        .expect("paging hello");
        let active = dispatch(
            request(
                2,
                "session.export",
                json!({ "sessionId": session_id.clone(), "limit": 1 }),
            ),
            &runtime,
            &events,
            &mut paging,
        )
        .await;
        assert_eq!(
            active.error().expect("active export page").data.code,
            "session_active"
        );

        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");

        let mut protocol_one_three = ConnectionState::default();
        let hello = dispatch(
            hello_request(
                ProtocolOffer::exact(ProtocolVersion::new(1, 3)),
                &[feature::EVENTS_SNAPSHOT_V1],
                &[feature::SESSION_EXPORT_PAGE_V1],
            ),
            &runtime,
            &events,
            &mut protocol_one_three,
        )
        .await;
        let hello = hello.result().expect("Protocol 1.3 hello");
        assert_eq!(
            hello["protocol"]["selected"],
            json!({ "major": 1, "minor": 3 })
        );
        assert!(
            !hello["features"]["enabled"]
                .as_array()
                .expect("enabled features")
                .iter()
                .any(|value| value == feature::SESSION_EXPORT_PAGE_V1)
        );

        let legacy = dispatch(
            request(
                3,
                "session.export",
                json!({ "sessionId": session_id.clone() }),
            ),
            &runtime,
            &events,
            &mut protocol_one_three,
        )
        .await;
        let legacy = legacy.result().expect("Protocol 1.3 legacy export");
        assert_eq!(legacy["events"].as_array().expect("legacy events").len(), 2);
        assert!(legacy.get("nextAfterSequence").is_none());

        let paged = dispatch(
            request(
                4,
                "session.export",
                json!({ "sessionId": session_id, "limit": 1 }),
            ),
            &runtime,
            &events,
            &mut protocol_one_three,
        )
        .await;
        assert_eq!(
            paged.error().expect("Protocol 1.3 paged export").data.code,
            "method_not_found"
        );
    }

    #[tokio::test]
    async fn export_page_applies_the_wire_budget_before_materializing_event_values() {
        let (runtime, events) = test_context().await;
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start session");
        for index in 0..40 {
            events
                .append(PendingEvent {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    payload: TestEventPayload::Error {
                        error: ErrorInfo {
                            code: format!("large-{index}"),
                            message: "x".repeat(64 * 1024),
                            retryable: false,
                            details: None,
                        },
                    },
                })
                .await
                .expect("append large event");
        }
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");

        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::EVENTS_SNAPSHOT_V1, feature::SESSION_EXPORT_PAGE_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await
        .result()
        .expect("paging hello");
        let page = dispatch(
            request(
                2,
                "session.export",
                json!({ "sessionId": session_id, "limit": 1000 }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let bytes = serde_json::to_vec(&page).expect("serialize bounded page");
        assert!(bytes.len() <= MAX_FRAME_BYTES);
        let result = page
            .result()
            .expect("large page succeeds with a bounded prefix");
        let page_events = result["events"].as_array().expect("page events");
        assert!(!page_events.is_empty());
        assert!(page_events.len() < 42);
        assert_eq!(
            result["nextAfterSequence"],
            page_events.last().expect("last event")["sequence"]
        );
    }

    #[tokio::test]
    async fn final_single_export_event_fits_an_exact_wire_budget_without_continuation() {
        let events = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start session");
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");
        let snapshot = events
            .export_session_page(&session_id, Some(EventSequence::FIRST), 1)
            .await
            .expect("terminal page snapshot");
        let id = RpcId::Number(99);
        let single = [snapshot.events[0].as_ref()];
        let exact = session_export_page_response_length(&id, &snapshot.session, &single, None)
            .expect("exact response length");

        let value = bounded_session_export_page_value(&id, snapshot.clone(), exact)
            .expect("exact budget must fit");
        assert_eq!(value["events"].as_array().expect("events").len(), 1);
        assert!(value.get("nextAfterSequence").is_none());

        let error = bounded_session_export_page_value(&id, snapshot, exact - 1)
            .expect_err("one byte below exact budget must fail");
        assert_eq!(error.data.code, "response_frame_too_large");
        let lower_bound = error.data.details.expect("size details")["actualBytesAtLeast"]
            .as_u64()
            .expect("lower bound") as usize;
        assert!(lower_bound > exact - 1);
        assert!(lower_bound <= exact);
    }

    #[tokio::test]
    async fn continuation_digit_growth_keeps_exact_export_page_sizing() {
        let events = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start session");
        for index in 0..9 {
            events
                .append(PendingEvent {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    payload: TestEventPayload::Error {
                        error: ErrorInfo {
                            code: format!("sizing-{index}"),
                            message: "sizing".to_owned(),
                            retryable: false,
                            details: None,
                        },
                    },
                })
                .await
                .expect("append event");
        }
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");
        let after = EventSequence::new(9).expect("sequence");
        let snapshot = events
            .export_session_page(&session_id, Some(after), 1)
            .await
            .expect("continuation page snapshot");
        let id = RpcId::Number(100);
        let single = [snapshot.events[0].as_ref()];
        let next = Some(EventSequence::new(10).expect("continuation"));
        let exact = session_export_page_response_length(&id, &snapshot.session, &single, next)
            .expect("exact response length");

        let value = bounded_session_export_page_value(&id, snapshot.clone(), exact)
            .expect("exact two-digit continuation budget must fit");
        assert_eq!(value["nextAfterSequence"], 10);
        let error = bounded_session_export_page_value(&id, snapshot, exact - 1)
            .expect_err("one byte below the exact continuation budget must fail");
        assert_eq!(error.data.code, "response_frame_too_large");
    }

    #[tokio::test]
    async fn oversized_real_event_stops_counting_at_the_wire_budget() {
        let events = MemoryEventStore::default();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start session");
        let details = Value::Array((0..4096).map(|_| Value::String("x".repeat(1024))).collect());
        events
            .append(PendingEvent {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                payload: TestEventPayload::Error {
                    error: ErrorInfo {
                        code: "oversized".to_owned(),
                        message: "oversized".to_owned(),
                        retryable: false,
                        details: Some(details),
                    },
                },
            })
            .await
            .expect("append oversized event");
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end session");
        let snapshot = events
            .export_session_page(&session_id, Some(EventSequence::FIRST), 1)
            .await
            .expect("oversized event snapshot");

        match capped_serialized_json_length(snapshot.events[0].as_ref(), MAX_FRAME_BYTES)
            .expect("capped measurement")
        {
            CappedJsonLength::Exceeded {
                counted_bytes,
                actual_bytes_at_least,
            } => {
                assert!(counted_bytes <= MAX_FRAME_BYTES);
                assert!(actual_bytes_at_least > MAX_FRAME_BYTES);
            }
            CappedJsonLength::Exact(_) => panic!("real event must exceed the frame budget"),
        }
    }

    #[tokio::test]
    async fn second_hello_is_rejected() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let first = dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert!(first.error().is_none());

        let second = dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(second.error().expect("second hello").code, -32002);
    }

    #[tokio::test]
    async fn method_visibility_depends_on_state_and_negotiated_features() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let before = dispatch(
            request(1, "unknown", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(before.error().expect("hello first").code, -32001);

        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let mut unknown = request(2, "unknown", json!({}));
        unknown.timeout_ms = RequestTimeoutMs::new(100);
        let after = dispatch(unknown, &runtime, &events, &mut connection).await;
        assert_eq!(after.error().expect("unknown after hello").code, -32601);

        let mut hidden = request(3, "events.list", json!({ "mustRemainHidden": true }));
        hidden.timeout_ms = RequestTimeoutMs::new(100);
        let events_list = dispatch(hidden, &runtime, &events, &mut connection).await;
        assert_eq!(
            events_list.error().expect("feature not enabled").code,
            -32601
        );
    }

    #[tokio::test]
    async fn no_param_methods_reject_nonempty_params() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;

        let response = dispatch(
            request(2, "device.connect", json!({ "unexpected": true })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let error = response.error().expect("invalid params");
        assert_eq!(error.code, -32602);
        assert_eq!(error.data.code, "invalid_params");
    }

    #[test]
    fn request_registry_rejects_duplicates_and_preserves_cancel_status() {
        let registry = RequestRegistry::default();
        let request_id = RpcId::String("slow-action".to_owned());
        let (controller, control) = ExecutionController::new();
        assert!(registry.register(request_id.clone(), controller));
        assert!(!registry.register(request_id.clone(), ExecutionController::new().0));
        assert_eq!(
            registry.cancel(&request_id, CancellationReason::Requested),
            devicerail_protocol::RequestCancelStatus::Requested
        );
        assert_eq!(
            control.cancellation_reason(),
            Some(CancellationReason::Requested)
        );
        assert_eq!(
            registry.cancel(&request_id, CancellationReason::Requested),
            devicerail_protocol::RequestCancelStatus::AlreadyRequested
        );
        registry.mark_completed(&request_id);
        assert!(registry.contains(&request_id));
        assert_eq!(
            registry.cancel(&request_id, CancellationReason::Requested),
            devicerail_protocol::RequestCancelStatus::NotFound
        );
        registry.remove(&request_id);
        assert_eq!(
            registry.cancel(&request_id, CancellationReason::Requested),
            devicerail_protocol::RequestCancelStatus::NotFound
        );
    }

    #[test]
    fn response_backpressure_is_bounded_and_explicit() {
        let (responses, _queue) = std::sync::mpsc::sync_channel(1);
        queue_response(
            &responses,
            RpcResponse::success(RpcId::Number(1), json!({})),
        )
        .expect("first response fits");
        let error = queue_response(
            &responses,
            RpcResponse::success(RpcId::Number(2), json!({})),
        )
        .expect_err("bounded queue rejects overflow");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn response_frames_accept_the_exact_limit_and_replace_oversized_payloads() {
        let response = |text: String| RpcResponse::success(RpcId::Number(7), Value::String(text));
        let empty_bytes = serde_json::to_vec(&response(String::new()))
            .expect("empty response serializes")
            .len();
        assert!(empty_bytes < MAX_FRAME_BYTES);

        let exact = response("x".repeat(MAX_FRAME_BYTES - empty_bytes));
        assert_eq!(
            serde_json::to_vec(&exact)
                .expect("exact response serializes")
                .len(),
            MAX_FRAME_BYTES
        );
        let (responses, queue) = std::sync::mpsc::sync_channel(1);
        queue_response(&responses, exact).expect("exact-limit response is queued");
        let frame = queue.recv().expect("exact-limit frame");
        assert_eq!(frame.len(), MAX_FRAME_BYTES + 1);
        assert_eq!(frame.last(), Some(&b'\n'));

        let oversized = response("x".repeat(MAX_FRAME_BYTES - empty_bytes + 1));
        let actual_bytes = serde_json::to_vec(&oversized)
            .expect("oversized response serializes")
            .len();
        assert_eq!(actual_bytes, MAX_FRAME_BYTES + 1);
        let (responses, queue) = std::sync::mpsc::sync_channel(1);
        queue_response(&responses, oversized).expect("oversized response is replaced");
        let frame = queue.recv().expect("replacement frame");
        assert_eq!(frame.last(), Some(&b'\n'));
        assert!(frame.len() - 1 <= MAX_FRAME_BYTES);
        let replacement: RpcResponse = serde_json::from_slice(&frame[..frame.len() - 1])
            .expect("replacement is valid JSON-RPC");
        match replacement {
            RpcResponse::Failure {
                id: Some(RpcId::Number(7)),
                error,
                ..
            } => {
                assert_eq!(error.code, -32012);
                assert_eq!(error.data.code, "response_frame_too_large");
                assert!(!error.data.retryable);
                let details = error.data.details.expect("size details");
                assert_eq!(details["actualBytes"], actual_bytes);
                assert_eq!(details["limitBytes"], MAX_FRAME_BYTES);
            }
            other => panic!("unexpected replacement response: {other:?}"),
        }
    }

    #[test]
    fn response_serialization_and_unrepresentable_replacements_fail_explicitly() {
        let invalid_id = RpcResponse::success(
            RpcId::Number(devicerail_protocol::MAX_SAFE_INTEGER_ID + 1),
            json!({}),
        );
        let (responses, queue) = std::sync::mpsc::sync_channel(1);
        let error = queue_response(&responses, invalid_id)
            .expect_err("invalid response serialization is explicit");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(matches!(
            queue.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        let error = bounded_response_frame(RpcResponse::success(RpcId::Number(1), json!({})), 1)
            .expect_err("replacement that cannot fit is explicit");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains("response_frame_too_large replacement")
        );
    }

    #[test]
    fn ndjson_input_frames_are_size_bounded_and_utf8_checked() {
        let mut exact = Cursor::new(b"12345\nnext\n".to_vec());
        assert_eq!(
            read_bounded_line(&mut exact, 5).expect("exact limit"),
            Some("12345".to_owned())
        );
        assert_eq!(
            read_bounded_line(&mut exact, 5).expect("next frame"),
            Some("next".to_owned())
        );

        let mut oversized = Cursor::new(b"123456\n".to_vec());
        assert_eq!(
            read_bounded_line(&mut oversized, 5)
                .expect_err("oversized frame")
                .kind(),
            std::io::ErrorKind::InvalidData
        );

        let mut invalid_utf8 = Cursor::new(vec![0xff, b'\n']);
        assert_eq!(
            read_bounded_line(&mut invalid_utf8, 5)
                .expect_err("invalid UTF-8")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn request_control_requires_negotiation_and_cancel_is_observable() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;

        let unavailable = dispatch(
            request(2, "request.cancel", json!({ "requestId": "slow-action" })),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let error = unavailable.error().expect("feature-gated method");
        assert_eq!(error.code, -32601);
        assert_eq!(
            error.data.details.as_ref().expect("feature details")["requiredFeature"],
            feature::REQUEST_CONTROL_V1
        );

        let mut timed = request(3, "device.connect", json!({}));
        timed.timeout_ms = RequestTimeoutMs::new(50);
        let unavailable = dispatch(timed, &runtime, &events, &mut connection).await;
        assert_eq!(
            unavailable.error().expect("timeout feature gate").data.code,
            "feature_not_negotiated"
        );

        let action_timeout = dispatch(
            request(
                4,
                "device.execute",
                json!({
                    "id": "22222222-2222-4222-8222-222222222222",
                    "name": "tap",
                    "arguments": { "x": 1, "y": 1 },
                    "actionTimeoutMs": 50
                }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            action_timeout
                .error()
                .expect("action timeout feature gate")
                .data
                .code,
            "feature_not_negotiated"
        );

        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        let hello = dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::REQUEST_CONTROL_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(
            hello.result().expect("hello")["features"]["enabled"],
            json!([feature::REQUEST_CONTROL_V1])
        );

        let mut describe = request(4, "system.describe", json!({}));
        describe.timeout_ms = RequestTimeoutMs::new(50);
        let unsupported = dispatch(describe, &runtime, &events, &mut connection).await;
        let error = unsupported.error().expect("admin timeout is rejected");
        assert_eq!(error.code, -32602);
        assert_eq!(error.data.code, "request_timeout_not_supported");

        let registry = RequestRegistry::default();
        let target = RpcId::String("slow-action".to_owned());
        let (controller, target_control) = ExecutionController::new();
        assert!(registry.register(target.clone(), controller));
        for (request_id, expected) in [(2, "requested"), (3, "alreadyRequested")] {
            let control = ExecutionControl::unbounded();
            let response = dispatch_controlled(
                request(request_id, "request.cancel", json!({ "requestId": target })),
                &runtime,
                &events,
                &mut connection,
                &control,
                &registry,
            )
            .await;
            assert_eq!(
                response.result().expect("cancel result")["status"],
                expected
            );
        }
        assert_eq!(
            target_control.cancellation_reason(),
            Some(CancellationReason::Requested)
        );
    }

    #[tokio::test]
    async fn running_action_cancel_writes_one_terminal_before_session_end() {
        let (runtime, events) = delayed_test_context(Duration::from_secs(30)).await;
        let runtime = Arc::new(runtime);
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::REQUEST_CONTROL_V1],
            ),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        dispatch(
            request(2, "device.connect", json!({})),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        let started = dispatch(
            request(3, "session.start", json!({})),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("session started")["id"].clone())
                .expect("session id");

        let target = RpcId::String("slow-action".to_owned());
        let mut execute = request(
            4,
            "device.execute",
            json!({
                "id": "33333333-3333-4333-8333-333333333333",
                "name": "tap",
                "arguments": { "x": 10, "y": 20 }
            }),
        );
        execute.id = target.clone();

        let registry = Arc::new(RequestRegistry::default());
        let (controller, control) = ExecutionController::new();
        assert!(registry.register(target.clone(), controller));
        let task_runtime = Arc::clone(&runtime);
        let task_events = Arc::clone(&events);
        let task_registry = Arc::clone(&registry);
        let mut task_connection = connection.clone();
        let task_target = target.clone();
        let action = tokio::spawn(async move {
            let _registration = RequestRegistration::new(task_registry.clone(), task_target);
            dispatch_controlled(
                execute,
                task_runtime.as_ref(),
                task_events.as_ref(),
                &mut task_connection,
                &control,
                task_registry.as_ref(),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let events = events
                    .list_after(&session_id, None)
                    .await
                    .expect("event list");
                if events
                    .iter()
                    .any(|event| matches!(event.payload, TestEventPayload::ActionStarted { .. }))
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ActionStarted becomes durable");

        let busy = dispatch(
            request(5, "session.end", json!({})),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        let error = busy.error().expect("in-flight Action keeps Session open");
        assert_eq!(error.code, -32006);
        assert_eq!(error.data.code, "session_busy");
        assert_eq!(
            error.data.details.as_ref().expect("busy details")["inFlightActions"],
            1
        );
        assert!(
            connection
                .context()
                .is_some_and(|context| context.active_session.as_ref() == Some(&session_id))
        );

        let cancel = dispatch_controlled(
            request(6, "request.cancel", json!({ "requestId": target })),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
            &ExecutionControl::unbounded(),
            registry.as_ref(),
        )
        .await;
        assert_eq!(
            cancel.result().expect("cancel result")["status"],
            "requested"
        );

        let execute = tokio::time::timeout(Duration::from_secs(1), action)
            .await
            .expect("cancelled action completed")
            .expect("action task");
        let error = execute.error().expect("cancelled response");
        assert_eq!(error.code, -32007);
        assert_eq!(error.data.code, "request_cancelled");

        let replay = events
            .export_session(&session_id)
            .await
            .expect("session replay");
        let terminals = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                TestEventPayload::ActionCompleted { outcome, .. } => Some(outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(terminals.len(), 1);
        assert!(matches!(terminals[0], ActionOutcome::Cancelled { .. }));

        let ended = dispatch(
            request(7, "session.end", json!({})),
            runtime.as_ref(),
            events.as_ref(),
            &mut connection,
        )
        .await;
        assert_eq!(ended.result().expect("session ends")["state"], "ended");
    }

    #[tokio::test]
    async fn request_and_action_timeouts_keep_rpc_and_event_scopes_distinct() {
        let (runtime, events) = delayed_test_context(Duration::from_secs(30)).await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(
                supported_protocol_offer(),
                &[],
                &[feature::REQUEST_CONTROL_V1],
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("session started")["id"].clone())
                .expect("session id");

        let mut request_timeout = request(
            4,
            "device.execute",
            json!({
                "id": "44444444-4444-4444-8444-444444444444",
                "name": "tap",
                "arguments": { "x": 10, "y": 20 },
                "actionTimeoutMs": 1000
            }),
        );
        request_timeout.timeout_ms = RequestTimeoutMs::new(10);
        let response = dispatch(request_timeout, &runtime, &events, &mut connection).await;
        let error = response.error().expect("request timeout");
        assert_eq!(error.code, -32008);
        assert_eq!(error.data.code, "request_timed_out");
        assert_eq!(
            error.data.details.as_ref().expect("scope")["scope"],
            "request"
        );

        let response = dispatch(
            request(
                5,
                "device.execute",
                json!({
                    "id": "55555555-5555-4555-8555-555555555555",
                    "name": "tap",
                    "arguments": { "x": 10, "y": 20 },
                    "actionTimeoutMs": 10
                }),
            ),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let error = response.error().expect("action timeout");
        assert_eq!(error.code, -32008);
        assert_eq!(error.data.code, "action_timed_out");
        assert_eq!(
            error.data.details.as_ref().expect("scope")["scope"],
            "action"
        );

        let replay = events
            .export_session(&session_id)
            .await
            .expect("session replay");
        let timeout_events = replay
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                TestEventPayload::ActionCompleted {
                    outcome: ActionOutcome::TimedOut { error, timeout_ms },
                    ..
                } => Some((error, *timeout_ms)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(timeout_events.len(), 2);
        assert_eq!(timeout_events[0].0.code, "action_timeout");
        assert_eq!(
            timeout_events[0].0.details.as_ref().expect("scope")["scope"],
            "request"
        );
        assert_eq!(timeout_events[0].1, 10);
        assert_eq!(timeout_events[1].0.code, "action_timeout");
        assert_eq!(
            timeout_events[1].0.details.as_ref().expect("scope")["scope"],
            "action"
        );
        assert_eq!(timeout_events[1].1, 10);

        let ended = dispatch(
            request(6, "session.end", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        assert_eq!(ended.result().expect("session ends")["state"], "ended");
    }

    #[test]
    fn runtime_control_errors_have_distinct_rpc_codes() {
        let cancelled = runtime_error(RuntimeError::Cancelled {
            reason: CancellationReason::Requested,
        });
        assert_eq!(cancelled.code, -32007);
        assert_eq!(cancelled.data.code, "request_cancelled");

        let timed_out = runtime_error(RuntimeError::TimedOut {
            scope: TimeoutScope::Action,
            timeout_ms: 25,
        });
        assert_eq!(timed_out.code, -32008);
        assert_eq!(timed_out.data.code, "action_timed_out");

        let driver = runtime_error(DriverError::Internal("failed".to_owned()).into());
        assert_eq!(driver.code, -32000);
        assert_eq!(driver.data.code, "internal_error");

        let evidence = runtime_error(RuntimeError::Evidence(
            devicerail_core::EvidenceError::Unavailable,
        ));
        assert_eq!(evidence.code, super::INTERNAL_ERROR);
        assert_eq!(evidence.data.code, "evidence_store_unavailable");
    }

    #[tokio::test]
    async fn graceful_shutdown_ends_the_session_then_disconnects() {
        let (runtime, events) = test_context().await;
        let mut connection = ConnectionState::default();
        dispatch(
            hello_request(supported_protocol_offer(), &[], &[]),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        dispatch(
            request(2, "device.connect", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let started = dispatch(
            request(3, "session.start", json!({})),
            &runtime,
            &events,
            &mut connection,
        )
        .await;
        let session_id: SessionId =
            serde_json::from_value(started.result().expect("session started")["id"].clone())
                .expect("session id");

        shutdown_runtime(&runtime, &events, &mut connection)
            .await
            .expect("graceful shutdown");
        assert!(
            connection
                .context()
                .is_some_and(|context| context.active_session.is_none())
        );

        let exported = events
            .export_session(&session_id)
            .await
            .expect("ended session");
        assert!(matches!(
            exported.events.last().map(|event| &event.payload),
            Some(TestEventPayload::SessionEnded {
                outcome: SessionOutcome::Shutdown,
                ..
            })
        ));

        let handle = runtime.sole().await.expect("sole test Driver");
        assert!(!handle.info().await.connected);
    }

    #[tokio::test]
    async fn shutdown_disconnects_all_drivers_with_one_global_grace() {
        let events = Arc::new(MemoryEventStore::default());
        let runtime = Registry::new(Arc::clone(&events));

        let good = Arc::new(MockDriver::new("mock-good"));
        let good_info = good.device_info();
        let good = runtime
            .register(good, good_info)
            .await
            .expect("register good Driver");

        let failed = Arc::new(AtomicBool::new(false));
        let driver = Arc::new(ShutdownTestDriver::new(
            "mock-fail",
            DisconnectBehavior::Fail,
            Arc::clone(&failed),
        ));
        let info = driver.device_info();
        runtime
            .register(driver, info)
            .await
            .expect("register failing Driver");

        let pending = Arc::new(AtomicBool::new(false));
        let driver = Arc::new(ShutdownTestDriver::new(
            "mock-pending",
            DisconnectBehavior::Pending,
            Arc::clone(&pending),
        ));
        let info = driver.device_info();
        runtime
            .register(driver, info)
            .await
            .expect("register pending Driver");

        for handle in runtime.handles().await {
            runtime
                .access_available_to(handle, LeaseOwnerId::new(Uuid::nil()), now_ms())
                .await
                .expect("lifecycle access")
                .connect(&ExecutionControl::unbounded())
                .await
                .expect("connect shutdown test Driver");
        }
        assert!(good.info().await.connected);

        let mut connection = ConnectionState::default();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            shutdown_runtime_with_grace(
                &runtime,
                &events,
                &mut connection,
                Duration::from_millis(20),
            ),
        )
        .await
        .expect("shutdown uses one bounded phase")
        .expect_err("failing and pending Drivers are reported");
        let message = result.to_string();
        assert!(message.contains("mock-fail"), "{message}");
        assert!(message.contains("disconnect phase timed out"), "{message}");
        assert!(failed.load(Ordering::SeqCst));
        assert!(pending.load(Ordering::SeqCst));
        assert!(
            !good.info().await.connected,
            "one Driver failure must not prevent another disconnect"
        );
    }

    #[test]
    fn remote_security_configuration_is_complete_listener_bound_and_redacted() {
        let listen = Some("127.0.0.1:47831".parse().expect("loopback address"));
        assert_eq!(
            parse_remote_security_startup(None, None, listen).expect("disabled"),
            None
        );
        assert_eq!(
            parse_remote_security_startup(Some(OsString::from("credentials.json")), None, listen,),
            Err(DaemonStartupError::RemoteSecurityIncomplete)
        );
        assert_eq!(
            parse_remote_security_startup(
                Some(OsString::from("credentials.json")),
                Some(OsString::from("audit.jsonl")),
                None,
            ),
            Err(DaemonStartupError::RemoteSecurityListenerRequired)
        );
        let config = parse_remote_security_startup(
            Some(OsString::from("/private/credentials.json")),
            Some(OsString::from("/private/audit.jsonl")),
            listen,
        )
        .expect("complete config")
        .expect("enabled");
        let debug = format!("{config:?}");
        assert!(!debug.contains("credentials.json"));
        assert!(!debug.contains("audit.jsonl"));
        assert!(debug.contains("REDACTED"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn in_memory_remote_authorization_is_fail_closed_and_durably_audited() {
        use std::os::unix::fs::PermissionsExt as _;

        let files = TempDir::new().expect("security tempdir");
        fs::set_permissions(files.path(), fs::Permissions::from_mode(0o700))
            .expect("owner-only audit parent");
        let credentials_path = files.path().join("credentials.json");
        let audit_path = files.path().join("audit.jsonl");
        let secret = [0x2a_u8; 32];
        fs::write(
            &credentials_path,
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "principals": [{
                    "principalId": "memory-reader",
                    "keyId": "key-1",
                    "secretBase64": URL_SAFE_NO_PAD.encode(secret),
                    "permissions": ["read"]
                }]
            }))
            .expect("credentials JSON"),
        )
        .expect("write credentials");
        fs::set_permissions(&credentials_path, fs::Permissions::from_mode(0o600))
            .expect("owner-only credentials");
        let authenticator = Arc::new(
            Authenticator::new(CredentialStore::load(&credentials_path).expect("credential store"))
                .expect("authenticator"),
        );
        let security = RemoteSecurity {
            authenticator: Arc::clone(&authenticator),
            audit: Arc::new(AuditLog::open(&audit_path).expect("audit log")),
        };
        let mut auth = authenticator.session();
        let client_nonce = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let challenge = auth
            .begin(
                AuthChallengeRequest {
                    auth_protocol_version: "1".into(),
                    principal_id: "memory-reader".into(),
                    key_id: "key-1".into(),
                    client_nonce: client_nonce.clone(),
                },
                std::time::Instant::now(),
            )
            .expect("challenge");
        let principal = auth
            .finish(
                AuthProofRequest {
                    auth_protocol_version: "1".into(),
                    challenge_id: challenge.challenge_id.clone(),
                    proof: compute_proof(
                        &secret,
                        "memory-reader",
                        "key-1",
                        &client_nonce,
                        &challenge,
                    )
                    .expect("proof"),
                },
                std::time::Instant::now(),
            )
            .expect("principal");
        assert!(
            authorize_remote_request(
                &security,
                "memory-connection",
                &principal,
                &request(1, "system.describe", json!({})),
            )
            .await
            .expect("read authorization")
            .is_none()
        );
        let denied = authorize_remote_request(
            &security,
            "memory-connection",
            &principal,
            &request(2, "device.execute", json!({ "never": "audited" })),
        )
        .await
        .expect("control authorization")
        .expect("read-only principal denied");
        assert_eq!(
            denied.error().expect("permission error").data.code,
            "permission_denied"
        );
        let records = AuditLog::verify(&audit_path).expect("audit chain");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].decision, AuditDecision::Allowed);
        assert_eq!(records[1].decision, AuditDecision::Denied);
        assert!(
            !fs::read(&audit_path)
                .expect("audit bytes")
                .windows(7)
                .any(|window| window == b"audited")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn authenticated_loopback_rpc_enforces_read_scope_and_audits_without_params() {
        use std::os::unix::fs::PermissionsExt as _;

        async fn rpc(client: &mut TokioBufReader<TcpStream>, request: RpcRequest) -> RpcResponse {
            let mut frame = serde_json::to_vec(&request).expect("request JSON");
            frame.push(b'\n');
            client
                .get_mut()
                .write_all(&frame)
                .await
                .expect("write request");
            let mut response = String::new();
            client
                .read_line(&mut response)
                .await
                .expect("read response");
            serde_json::from_str(&response).expect("response JSON")
        }

        fn owner_only(path: &Path) {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("owner-only mode");
        }

        let files = TempDir::new().expect("security tempdir");
        fs::set_permissions(files.path(), fs::Permissions::from_mode(0o700))
            .expect("owner-only audit parent");
        let credentials_path = files.path().join("credentials.json");
        let audit_path = files.path().join("audit.jsonl");
        let secret = [0x2a_u8; 32];
        fs::write(
            &credentials_path,
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "principals": [{
                    "principalId": "tcp-reader",
                    "keyId": "key-1",
                    "secretBase64": URL_SAFE_NO_PAD.encode(secret),
                    "permissions": ["read"]
                }]
            }))
            .expect("credentials JSON"),
        )
        .expect("write credentials");
        owner_only(&credentials_path);
        let security = Arc::new(RemoteSecurity {
            authenticator: Arc::new(
                Authenticator::new(
                    CredentialStore::load(&credentials_path).expect("credential store"),
                )
                .expect("authenticator"),
            ),
            audit: Arc::new(AuditLog::open(&audit_path).expect("audit log")),
        });
        let (runtime, events) = test_context().await;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let server_security = Arc::clone(&security);
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("accept client");
            let (_shutdown_sender, shutdown) = tokio::sync::watch::channel(None);
            serve_loopback_connection_until_shutdown(
                socket,
                Arc::new(runtime),
                events,
                EvidenceCleanup::Disabled,
                Some(server_security),
                shutdown,
            )
            .await
        });
        let mut client =
            TokioBufReader::new(TcpStream::connect(address).await.expect("TCP client"));

        let before_auth = rpc(
            &mut client,
            hello_request(supported_protocol_offer(), &[], &[]),
        )
        .await;
        assert_eq!(
            before_auth
                .error()
                .expect("authentication required")
                .data
                .code,
            "authentication_required"
        );

        let client_nonce = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let challenge_response = rpc(
            &mut client,
            request(
                2,
                "auth.challenge",
                serde_json::to_value(AuthChallengeRequest {
                    auth_protocol_version: "1".into(),
                    principal_id: "tcp-reader".into(),
                    key_id: "key-1".into(),
                    client_nonce: client_nonce.clone(),
                })
                .expect("challenge request"),
            ),
        )
        .await;
        let challenge: AuthChallenge = serde_json::from_value(
            challenge_response
                .result()
                .expect("challenge result")
                .clone(),
        )
        .expect("challenge");
        let proof = compute_proof(&secret, "tcp-reader", "key-1", &client_nonce, &challenge)
            .expect("proof");
        let authenticated = rpc(
            &mut client,
            request(
                3,
                "auth.respond",
                serde_json::to_value(AuthProofRequest {
                    auth_protocol_version: "1".into(),
                    challenge_id: challenge.challenge_id,
                    proof,
                })
                .expect("proof request"),
            ),
        )
        .await;
        assert_eq!(
            authenticated.result().expect("authenticated")["principalId"],
            "tcp-reader"
        );

        let hello = rpc(
            &mut client,
            hello_request(supported_protocol_offer(), &[], &[]),
        )
        .await;
        assert_eq!(
            hello.result().expect("authorized hello")["transport"]["kind"],
            "tcp"
        );
        let denied = rpc(
            &mut client,
            request(
                4,
                "device.execute",
                json!({
                    "id": "44444444-4444-4444-8444-444444444444",
                    "name": "tap",
                    "arguments": { "x": 1, "y": 2 },
                }),
            ),
        )
        .await;
        assert_eq!(
            denied.error().expect("read principal denied").data.code,
            "permission_denied"
        );
        drop(client);
        server.await.expect("server task").expect("server service");

        let records = AuditLog::verify(&audit_path).expect("valid audit chain");
        assert_eq!(
            records
                .iter()
                .map(|record| record.method.as_str())
                .collect::<Vec<_>>(),
            [
                "system.hello",
                "auth.challenge",
                "auth.respond",
                "system.hello",
                "device.execute"
            ]
        );
        assert_eq!(
            records.last().expect("denial").decision,
            AuditDecision::Denied
        );
        let audit_bytes = fs::read(&audit_path).expect("audit bytes");
        assert!(!audit_bytes.windows(6).any(|window| window == b"params"));
        assert!(!audit_bytes.windows(6).any(|window| window == b"secret"));
        assert!(!audit_bytes.windows(5).any(|window| window == b"proof"));
    }

    #[test]
    fn feature_sets_are_deterministic() {
        let set = BTreeSet::from(["z.v1".to_owned(), "a.v1".to_owned()]);
        assert_eq!(set.into_iter().collect::<Vec<_>>(), ["a.v1", "z.v1"]);
    }

    #[test]
    fn protocol_one_minor_four_features_are_not_advertised_early() {
        assert!(
            !server_features(ProtocolVersion::new(1, 3), true, true, true)
                .contains(feature::MEDIA_STREAM_V1)
        );
        assert!(
            !server_features(ProtocolVersion::new(1, 3), true, true, true)
                .contains(feature::SESSION_EXPORT_PAGE_V1)
        );
        assert!(
            server_features(ProtocolVersion::new(1, 4), true, true, true)
                .contains(feature::MEDIA_STREAM_V1)
        );
        assert!(
            !server_features(ProtocolVersion::new(1, 4), true, false, true)
                .contains(feature::MEDIA_STREAM_V1)
        );
        assert!(
            server_features(ProtocolVersion::new(1, 4), true, true, true)
                .contains(feature::SESSION_EXPORT_PAGE_V1)
        );
    }

    #[test]
    fn protocol_one_minor_five_features_require_their_runtime_dependencies() {
        let without_evidence = server_features(ProtocolVersion::new(1, 5), true, true, false);
        assert!(!without_evidence.contains(feature::DEVICE_SEMANTIC_ACTIONS_V1));
        assert!(!without_evidence.contains(feature::OBSERVATION_UI_SNAPSHOT_V1));
        assert!(!without_evidence.contains(feature::VERDICT_RECORD_V1));

        let with_evidence = server_features(ProtocolVersion::new(1, 5), true, true, true);
        assert!(with_evidence.contains(feature::DEVICE_SEMANTIC_ACTIONS_V1));
        assert!(with_evidence.contains(feature::OBSERVATION_UI_SNAPSHOT_V1));
        assert!(with_evidence.contains(feature::VERDICT_RECORD_V1));
    }

    #[test]
    fn semantic_actions_require_ui_snapshots_at_handshake_and_operation_boundaries() {
        let semantic_only = hello_request(
            ProtocolOffer::exact(ProtocolVersion::new(1, 5)),
            &[],
            &[feature::DEVICE_SEMANTIC_ACTIONS_V1],
        );
        let mut connection = ConnectionState::default();
        let error =
            super::negotiate_connection(semantic_only.params, &mut connection, false, false, true)
                .expect_err("semantic-only negotiation must fail closed");
        assert_eq!(error.data.code, "feature_dependency_unsatisfied");
        assert_eq!(
            error.data.details.as_ref().expect("dependency details")["requiredFeature"],
            feature::OBSERVATION_UI_SNAPSHOT_V1
        );
        assert!(connection.context().is_none());

        let complete = hello_request(
            ProtocolOffer::exact(ProtocolVersion::new(1, 5)),
            &[],
            &[
                feature::DEVICE_SEMANTIC_ACTIONS_V1,
                feature::OBSERVATION_UI_SNAPSHOT_V1,
            ],
        );
        super::negotiate_connection(complete.params, &mut connection, false, false, true)
            .expect("complete semantic negotiation");
        let ConnectionState::Ready(context) = &mut connection else {
            panic!("successful hello must make the connection ready");
        };
        context.active_session = Some(SessionId::new());
        context
            .hello
            .features
            .enabled
            .remove(feature::OBSERVATION_UI_SNAPSHOT_V1);
        let error = super::active_operation_context(
            &connection,
            RpcId::Number(9),
            ExecutionControl::unbounded(),
        )
        .expect_err("operation must defend the feature dependency invariant");
        assert_eq!(error.data.code, "feature_dependency_unsatisfied");
    }

    #[test]
    fn offer_helper_covers_protocol_one_minor_zero_through_five() {
        assert_eq!(
            supported_protocol_offer(),
            ProtocolOffer::new(vec![ProtocolRange::new(1, 0, 5)])
        );
    }
}
