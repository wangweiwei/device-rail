use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::{DeviceDriver, ExecutionControl};
use devicerail_protocol::{DeviceInfo, Platform, Viewport};
use serde_json::{Map, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    time,
};

use crate::{
    DesktopAction, DesktopBackend, DesktopCapture, DesktopError, DesktopIdentity, DesktopProbe,
    DesktopProfile, DesktopResult, LinuxDisplayServer, LinuxDriver, MacOsDriver, MacOsPermission,
    PermissionState, WaylandInputBackend, WindowsDriver, model::DesktopKey,
};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SCREENSHOT_STDOUT_LIMIT: usize = 32 * 1024 * 1024;
const TEXT_STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;

/// Explicit tool and session configuration for the native desktop adapter.
///
/// Wayland deliberately requires a viewport because querying it portably can
/// require a screen capture. Requiring the caller to supply the compositor's
/// capture coordinate space preserves screenshot-omission policy.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemDesktopConfig {
    pub command_timeout: Duration,
    pub macos_screencapture: PathBuf,
    pub windows_powershell: PathBuf,
    pub x11_import: PathBuf,
    pub x11_xdotool: PathBuf,
    pub wayland_grim: PathBuf,
    pub wayland_ydotool: PathBuf,
    pub wayland_wtype: PathBuf,
    pub linux_display_server: Option<LinuxDisplayServer>,
    /// Selects a required Wayland input backend. `None` discovers `ydotool`
    /// first and falls back to `wtype` only when `ydotool` is absent.
    pub wayland_input_backend: Option<WaylandInputBackend>,
    pub wayland_viewport: Option<Viewport>,
}

impl Default for SystemDesktopConfig {
    fn default() -> Self {
        Self {
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            macos_screencapture: PathBuf::from("/usr/sbin/screencapture"),
            windows_powershell: PathBuf::from("powershell.exe"),
            x11_import: PathBuf::from("import"),
            x11_xdotool: PathBuf::from("xdotool"),
            wayland_grim: PathBuf::from("grim"),
            wayland_ydotool: PathBuf::from("ydotool"),
            wayland_wtype: PathBuf::from("wtype"),
            linux_display_server: None,
            wayland_input_backend: None,
            wayland_viewport: None,
        }
    }
}

impl SystemDesktopConfig {
    pub fn validate(&self) -> DesktopResult<()> {
        if self.command_timeout.is_zero() || self.command_timeout > MAX_COMMAND_TIMEOUT {
            return Err(DesktopError::InvalidProfile(format!(
                "command timeout must be between 1 ms and {} ms",
                MAX_COMMAND_TIMEOUT.as_millis()
            )));
        }
        if let Some(viewport) = &self.wayland_viewport {
            crate::model::validate_viewport(viewport)?;
        }
        Ok(())
    }
}

/// The platform-specific Driver returned by native discovery.
pub enum NativeDesktopDriver {
    MacOs(MacOsDriver),
    Windows(WindowsDriver),
    Linux(LinuxDriver),
}

impl NativeDesktopDriver {
    pub async fn device_info(&self) -> DeviceInfo {
        match self {
            Self::MacOs(driver) => driver.device_info().await,
            Self::Windows(driver) => driver.device_info().await,
            Self::Linux(driver) => driver.device_info().await,
        }
    }

    pub fn into_driver(self) -> Arc<dyn DeviceDriver> {
        match self {
            Self::MacOs(driver) => Arc::new(driver),
            Self::Windows(driver) => Arc::new(driver),
            Self::Linux(driver) => Arc::new(driver),
        }
    }
}

/// Discovers only tools for the compile-time host, then constructs the matching
/// Driver. Calling this function never downloads or installs platform tools.
pub async fn discover_native_driver(
    identity: DesktopIdentity,
    config: SystemDesktopConfig,
    control: &ExecutionControl,
) -> DesktopResult<NativeDesktopDriver> {
    ensure_active(control)?;
    config.validate()?;
    let backend = Arc::new(SystemDesktopBackend::discover(config, control).await?);
    let platform = backend.profile().platform().clone();
    let backend: Arc<dyn DesktopBackend> = backend;
    match platform {
        Platform::MacOs => MacOsDriver::new(identity, backend).map(NativeDesktopDriver::MacOs),
        Platform::Windows => {
            WindowsDriver::new(identity, backend).map(NativeDesktopDriver::Windows)
        }
        Platform::Linux => LinuxDriver::new(identity, backend).map(NativeDesktopDriver::Linux),
        _ => Err(DesktopError::UnsupportedHost {
            platform: format!("{platform:?}"),
        }),
    }
}

/// Determines X11 versus Wayland without treating one as a fallback for the
/// other. An unsupported explicit session value and an ambiguous pair of
/// display variables both fail closed.
pub fn detect_linux_display_server(
    xdg_session_type: Option<&str>,
    wayland_display: Option<&str>,
    display: Option<&str>,
) -> DesktopResult<LinuxDisplayServer> {
    if let Some(session) = xdg_session_type
        .map(str::trim)
        .filter(|session| !session.is_empty())
    {
        return match session.to_ascii_lowercase().as_str() {
            "x11" => Ok(LinuxDisplayServer::X11),
            "wayland" => Ok(LinuxDisplayServer::Wayland),
            _ => Err(DesktopError::UnsupportedLinuxSession {
                value: session.to_owned(),
            }),
        };
    }

    let has_wayland = wayland_display.is_some_and(|value| !value.trim().is_empty());
    let has_x11 = display.is_some_and(|value| !value.trim().is_empty());
    match (has_wayland, has_x11) {
        (true, false) => Ok(LinuxDisplayServer::Wayland),
        (false, true) => Ok(LinuxDisplayServer::X11),
        _ => Err(DesktopError::LinuxDisplayServerUnknown),
    }
}

struct SystemDesktopBackend {
    profile: DesktopProfile,
    kind: SystemKind,
    runner: SystemCommandRunner,
}

// Non-host variants stay compiled so cross-platform command construction is
// type-checked on every target even though only one is discoverable at runtime.
#[allow(dead_code)]
enum SystemKind {
    MacOs {
        screencapture: PathBuf,
    },
    Windows {
        powershell: PathBuf,
    },
    X11 {
        import: PathBuf,
        xdotool: PathBuf,
    },
    Wayland {
        grim: PathBuf,
        input: WaylandTool,
        viewport: Viewport,
    },
}

#[allow(dead_code)]
enum WaylandTool {
    Ydotool(PathBuf),
    Wtype(PathBuf),
}

impl SystemDesktopBackend {
    async fn discover(
        config: SystemDesktopConfig,
        control: &ExecutionControl,
    ) -> DesktopResult<Self> {
        ensure_active(control)?;
        let runner = SystemCommandRunner {
            timeout: config.command_timeout,
        };

        #[cfg(target_os = "macos")]
        {
            let screencapture = resolve_required(&config.macos_screencapture)?;
            let profile = DesktopProfile::macos(macos_native::permissions());
            return Ok(Self {
                profile,
                kind: SystemKind::MacOs { screencapture },
                runner,
            });
        }

        #[cfg(target_os = "windows")]
        {
            let powershell = resolve_required(&config.windows_powershell)?;
            return Ok(Self {
                profile: DesktopProfile::windows(),
                kind: SystemKind::Windows { powershell },
                runner,
            });
        }

        #[cfg(target_os = "linux")]
        {
            let display_server = match config.linux_display_server {
                Some(display_server) => display_server,
                None => detect_linux_display_server(
                    std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
                    std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
                    std::env::var("DISPLAY").ok().as_deref(),
                )?,
            };
            return match display_server {
                LinuxDisplayServer::X11 => {
                    let import = resolve_required(&config.x11_import)?;
                    let xdotool = resolve_required(&config.x11_xdotool)?;
                    Ok(Self {
                        profile: DesktopProfile::linux_x11(),
                        kind: SystemKind::X11 { import, xdotool },
                        runner,
                    })
                }
                LinuxDisplayServer::Wayland => {
                    let grim = resolve_required(&config.wayland_grim)?;
                    let (input, profile) = discover_wayland_input(&config)?;
                    let viewport = config
                        .wayland_viewport
                        .ok_or(DesktopError::WaylandViewportRequired)?;
                    Ok(Self {
                        profile,
                        kind: SystemKind::Wayland {
                            grim,
                            input,
                            viewport,
                        },
                        runner,
                    })
                }
            };
        }

        #[allow(unreachable_code)]
        Err(DesktopError::UnsupportedHost {
            platform: std::env::consts::OS.to_owned(),
        })
    }

    async fn probe_windows(
        &self,
        powershell: &Path,
        control: &ExecutionControl,
    ) -> DesktopResult<Viewport> {
        let output = self
            .runner
            .run(
                powershell_command(
                    powershell,
                    "desktop_windows_probe",
                    WINDOWS_PROBE,
                    vec![],
                    None,
                ),
                control,
            )
            .await?;
        let text = output.text("desktop_windows_probe")?;
        parse_dimensions(text.trim(), "desktop_windows_probe", 1.0)
    }

    async fn probe_x11(
        &self,
        xdotool: &Path,
        control: &ExecutionControl,
    ) -> DesktopResult<Viewport> {
        let output = self
            .runner
            .run(
                CommandSpec::new(
                    xdotool,
                    "desktop_x11_probe",
                    ["getdisplaygeometry", "--shell"],
                ),
                control,
            )
            .await?;
        let text = output.text("desktop_x11_probe")?;
        let mut width = None;
        let mut height = None;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("WIDTH=") {
                width = value.parse::<u32>().ok();
            } else if let Some(value) = line.strip_prefix("HEIGHT=") {
                height = value.parse::<u32>().ok();
            } else if !line.trim().is_empty() {
                return Err(DesktopError::MalformedOutput {
                    operation: "desktop_x11_probe",
                });
            }
        }
        viewport(width, height, 1.0, "desktop_x11_probe")
    }

    async fn execute_windows(
        &self,
        powershell: &Path,
        action: DesktopAction,
        control: &ExecutionControl,
    ) -> DesktopResult<()> {
        let (suffix, args, stdin) = match action {
            DesktopAction::Tap { x, y } => (
                "[DeviceRailInput]::Tap([int]$args[0], [int]$args[1])",
                vec![OsString::from(x.to_string()), OsString::from(y.to_string())],
                None,
            ),
            DesktopAction::InputText(text) => (
                "$utf8 = New-Object System.Text.UTF8Encoding($false, $true); $reader = New-Object System.IO.StreamReader([Console]::OpenStandardInput(), $utf8); try { [DeviceRailInput]::Text($reader.ReadToEnd()) } finally { $reader.Dispose() }",
                Vec::new(),
                Some(text.into_bytes()),
            ),
            DesktopAction::KeyPress(key) => (
                "[DeviceRailInput]::Key([byte]$args[0])",
                vec![OsString::from(windows_virtual_key(key).to_string())],
                None,
            ),
            DesktopAction::Scroll { delta_x, delta_y } => (
                "[DeviceRailInput]::Scroll([int]$args[0], [int]$args[1])",
                vec![
                    OsString::from(delta_x.to_string()),
                    OsString::from(delta_y.to_string()),
                ],
                None,
            ),
        };
        let script = format!("{WINDOWS_INPUT_TYPE}\n{suffix}\n");
        self.runner
            .run(
                powershell_command(powershell, "desktop_windows_input", script, args, stdin),
                control,
            )
            .await?;
        Ok(())
    }

    async fn execute_x11(
        &self,
        xdotool: &Path,
        action: DesktopAction,
        control: &ExecutionControl,
    ) -> DesktopResult<()> {
        let spec = match action {
            DesktopAction::Tap { x, y } => CommandSpec::new(
                xdotool,
                "desktop_x11_tap",
                vec![
                    OsString::from("mousemove"),
                    OsString::from("--sync"),
                    OsString::from(x.to_string()),
                    OsString::from(y.to_string()),
                    OsString::from("click"),
                    OsString::from("1"),
                ],
            ),
            DesktopAction::InputText(text) => CommandSpec::new(
                xdotool,
                "desktop_x11_text",
                ["type", "--clearmodifiers", "--delay", "0", "--file", "-"],
            )
            .with_stdin(text.into_bytes()),
            DesktopAction::KeyPress(key) => CommandSpec::new(
                xdotool,
                "desktop_x11_key",
                ["key", "--clearmodifiers", x11_key(key)],
            ),
            DesktopAction::Scroll { delta_x, delta_y } => {
                let mut args = Vec::<OsString>::new();
                append_x11_scroll(&mut args, delta_y, "4", "5");
                append_x11_scroll(&mut args, delta_x, "6", "7");
                if args.is_empty() {
                    args.extend([
                        "mousemove_relative".into(),
                        "--".into(),
                        "0".into(),
                        "0".into(),
                    ]);
                }
                CommandSpec::new(xdotool, "desktop_x11_scroll", args)
            }
        };
        self.runner.run(spec, control).await?;
        Ok(())
    }

    async fn execute_wayland(
        &self,
        input: &WaylandTool,
        action: DesktopAction,
        control: &ExecutionControl,
    ) -> DesktopResult<()> {
        let specs = match (input, action) {
            (WaylandTool::Ydotool(program), DesktopAction::Tap { x, y }) => vec![
                CommandSpec::new(
                    program,
                    "desktop_wayland_pointer_move",
                    vec![
                        OsString::from("mousemove"),
                        OsString::from("--absolute"),
                        OsString::from("-x"),
                        OsString::from(x.to_string()),
                        OsString::from("-y"),
                        OsString::from(y.to_string()),
                    ],
                ),
                CommandSpec::new(program, "desktop_wayland_tap", ["click", "0xC0"]),
            ],
            (WaylandTool::Ydotool(program), DesktopAction::InputText(text)) => {
                vec![CommandSpec::new(
                    program,
                    "desktop_wayland_text",
                    vec![
                        OsString::from("type"),
                        OsString::from("--"),
                        OsString::from(text),
                    ],
                )]
            }
            (WaylandTool::Ydotool(program), DesktopAction::KeyPress(key)) => {
                let code = linux_input_key(key);
                vec![CommandSpec::new(
                    program,
                    "desktop_wayland_key",
                    vec![
                        OsString::from("key"),
                        OsString::from(format!("{code}:1")),
                        OsString::from(format!("{code}:0")),
                    ],
                )]
            }
            (WaylandTool::Ydotool(program), DesktopAction::Scroll { delta_x, delta_y }) => {
                vec![CommandSpec::new(
                    program,
                    "desktop_wayland_scroll",
                    vec![
                        OsString::from("mousemove"),
                        OsString::from("--wheel"),
                        OsString::from("-x"),
                        OsString::from(delta_x.to_string()),
                        OsString::from("-y"),
                        OsString::from(delta_y.saturating_neg().to_string()),
                    ],
                )]
            }
            (WaylandTool::Wtype(program), DesktopAction::InputText(text)) => {
                vec![CommandSpec::new(
                    program,
                    "desktop_wayland_text",
                    vec![OsString::from("--"), OsString::from(text)],
                )]
            }
            (WaylandTool::Wtype(program), DesktopAction::KeyPress(key)) => vec![CommandSpec::new(
                program,
                "desktop_wayland_key",
                vec![OsString::from("-k"), OsString::from(wayland_key(key))],
            )],
            (WaylandTool::Wtype(_), action) => {
                return Err(DesktopError::UnsupportedAction {
                    action: action.kind(),
                });
            }
        };
        for spec in specs {
            self.runner.run(spec, control).await?;
        }
        Ok(())
    }
}

// Kept compiled on every target so explicit/automatic Wayland selection is
// type-checked and testable even when the current host is not Linux.
#[allow(dead_code)]
fn discover_wayland_input(
    config: &SystemDesktopConfig,
) -> DesktopResult<(WaylandTool, DesktopProfile)> {
    match config.wayland_input_backend {
        Some(WaylandInputBackend::Ydotool) => {
            let ydotool = resolve_required(&config.wayland_ydotool)?;
            Ok((
                WaylandTool::Ydotool(ydotool),
                DesktopProfile::linux_wayland(WaylandInputBackend::Ydotool),
            ))
        }
        Some(WaylandInputBackend::Wtype) => {
            let wtype = resolve_required(&config.wayland_wtype)?;
            Ok((
                WaylandTool::Wtype(wtype),
                DesktopProfile::linux_wayland(WaylandInputBackend::Wtype),
            ))
        }
        None => {
            if let Some(ydotool) = resolve_optional(&config.wayland_ydotool) {
                Ok((
                    WaylandTool::Ydotool(ydotool),
                    DesktopProfile::linux_wayland(WaylandInputBackend::Ydotool),
                ))
            } else if let Some(wtype) = resolve_optional(&config.wayland_wtype) {
                Ok((
                    WaylandTool::Wtype(wtype),
                    DesktopProfile::linux_wayland(WaylandInputBackend::Wtype),
                ))
            } else {
                Err(DesktopError::InputToolNotFound {
                    display_server: LinuxDisplayServer::Wayland,
                })
            }
        }
    }
}

#[async_trait]
impl DesktopBackend for SystemDesktopBackend {
    fn profile(&self) -> &DesktopProfile {
        &self.profile
    }

    async fn probe(&self, control: &ExecutionControl) -> DesktopResult<DesktopProbe> {
        ensure_active(control)?;
        let (profile, viewport) = match &self.kind {
            SystemKind::MacOs { .. } => (
                DesktopProfile::macos(macos_native::permissions()),
                macos_native::viewport()?,
            ),
            SystemKind::Windows { powershell } => (
                self.profile.clone(),
                self.probe_windows(powershell, control).await?,
            ),
            SystemKind::X11 { xdotool, .. } => (
                self.profile.clone(),
                self.probe_x11(xdotool, control).await?,
            ),
            SystemKind::Wayland { viewport, .. } => (self.profile.clone(), viewport.clone()),
        };
        DesktopProbe::new(profile, viewport)
    }

    async fn capture(&self, control: &ExecutionControl) -> DesktopResult<DesktopCapture> {
        ensure_active(control)?;
        let probe = self.probe(control).await?;
        ensure_macos_permissions(&probe.profile)?;
        let (png, tool) = match &self.kind {
            SystemKind::MacOs { screencapture } => {
                let directory = tempfile::tempdir().map_err(|source| DesktopError::Io {
                    operation: "desktop_macos_capture",
                    stream: "temporary directory",
                    source,
                })?;
                let path = directory.path().join("capture.png");
                self.runner
                    .run(
                        CommandSpec::new(
                            screencapture,
                            "desktop_macos_capture",
                            [
                                "-x".into(),
                                "-m".into(),
                                "-t".into(),
                                "png".into(),
                                path.clone().into_os_string(),
                            ],
                        )
                        .with_stdout_limit(TEXT_STDOUT_LIMIT),
                        control,
                    )
                    .await?;
                let bytes = tokio::fs::read(path)
                    .await
                    .map_err(|source| DesktopError::Io {
                        operation: "desktop_macos_capture",
                        stream: "capture file",
                        source,
                    })?;
                if bytes.len() > SCREENSHOT_STDOUT_LIMIT {
                    return Err(DesktopError::OutputTooLarge {
                        operation: "desktop_macos_capture",
                        stream: "capture file",
                        limit: SCREENSHOT_STDOUT_LIMIT,
                    });
                }
                (bytes, "screencapture")
            }
            SystemKind::Windows { powershell } => {
                let output = self
                    .runner
                    .run(
                        powershell_command(
                            powershell,
                            "desktop_windows_capture",
                            WINDOWS_CAPTURE,
                            vec![],
                            None,
                        )
                        .with_stdout_limit(SCREENSHOT_STDOUT_LIMIT),
                        control,
                    )
                    .await?;
                (output.stdout, "powershell")
            }
            SystemKind::X11 { import, .. } => {
                let output = self
                    .runner
                    .run(
                        CommandSpec::new(
                            import,
                            "desktop_x11_capture",
                            ["-window", "root", "png:-"],
                        )
                        .with_stdout_limit(SCREENSHOT_STDOUT_LIMIT),
                        control,
                    )
                    .await?;
                (output.stdout, "import")
            }
            SystemKind::Wayland { grim, .. } => {
                let output = self
                    .runner
                    .run(
                        CommandSpec::new(grim, "desktop_wayland_capture", ["-t", "png", "-"])
                            .with_stdout_limit(SCREENSHOT_STDOUT_LIMIT),
                        control,
                    )
                    .await?;
                (output.stdout, "grim")
            }
        };
        let metadata = Map::from_iter([("captureTool".to_owned(), json!(tool))]);
        DesktopCapture::new(png, probe.viewport).map(|capture| capture.with_metadata(metadata))
    }

    async fn execute(
        &self,
        action: DesktopAction,
        control: &ExecutionControl,
    ) -> DesktopResult<()> {
        ensure_active(control)?;
        if !self.profile.supports(action.kind()) {
            return Err(DesktopError::UnsupportedAction {
                action: action.kind(),
            });
        }
        if matches!(&self.kind, SystemKind::MacOs { .. }) {
            ensure_macos_permissions(&DesktopProfile::macos(macos_native::permissions()))?;
        }
        match &self.kind {
            SystemKind::MacOs { .. } => macos_native::execute(action),
            SystemKind::Windows { powershell } => {
                self.execute_windows(powershell, action, control).await
            }
            SystemKind::X11 { xdotool, .. } => self.execute_x11(xdotool, action, control).await,
            SystemKind::Wayland { input, .. } => self.execute_wayland(input, action, control).await,
        }
    }
}

fn ensure_active(control: &ExecutionControl) -> DesktopResult<()> {
    if control.is_cancelled() {
        Err(DesktopError::Cancelled)
    } else if control.is_expired() {
        Err(DesktopError::TimedOut)
    } else {
        Ok(())
    }
}

fn ensure_macos_permissions(profile: &DesktopProfile) -> DesktopResult<()> {
    let Some(permissions) = profile.macos_permissions() else {
        return Ok(());
    };
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
    Ok(())
}

fn viewport(
    width: Option<u32>,
    height: Option<u32>,
    scale_factor: f64,
    operation: &'static str,
) -> DesktopResult<Viewport> {
    let viewport = Viewport {
        width: width.ok_or(DesktopError::MalformedOutput { operation })?,
        height: height.ok_or(DesktopError::MalformedOutput { operation })?,
        scale_factor,
    };
    crate::model::validate_viewport(&viewport)?;
    Ok(viewport)
}

fn parse_dimensions(
    value: &str,
    operation: &'static str,
    scale_factor: f64,
) -> DesktopResult<Viewport> {
    let (width, height) = value
        .split_once('\t')
        .ok_or(DesktopError::MalformedOutput { operation })?;
    viewport(
        width.parse::<u32>().ok(),
        height.parse::<u32>().ok(),
        scale_factor,
        operation,
    )
}

fn resolve_required(configured: &Path) -> DesktopResult<PathBuf> {
    resolve_optional(configured).ok_or_else(|| DesktopError::ToolNotFound {
        tool: configured.to_owned(),
    })
}

fn resolve_optional(configured: &Path) -> Option<PathBuf> {
    if configured.is_absolute() || configured.components().count() > 1 {
        return configured.is_file().then(|| configured.to_owned());
    }
    let path = std::env::var_os("PATH")?;
    let directories = std::env::split_paths(&path).collect::<Vec<_>>();
    if let Some(found) = directories
        .iter()
        .map(|directory| directory.join(configured))
        .find(|candidate| candidate.is_file())
    {
        return Some(found);
    }
    #[cfg(target_os = "windows")]
    if configured.extension().is_none() {
        let extensions =
            std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        for extension in extensions
            .to_string_lossy()
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            let name = PathBuf::from(format!("{}{}", configured.display(), extension));
            if let Some(found) = directories
                .iter()
                .map(|directory| directory.join(&name))
                .find(|candidate| candidate.is_file())
            {
                return Some(found);
            }
        }
    }
    None
}

struct CommandSpec {
    program: PathBuf,
    operation: &'static str,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    stdout_limit: usize,
}

impl CommandSpec {
    fn new<I, A>(program: impl AsRef<Path>, operation: &'static str, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        Self {
            program: program.as_ref().to_owned(),
            operation,
            args: args.into_iter().map(Into::into).collect(),
            stdin: None,
            stdout_limit: TEXT_STDOUT_LIMIT,
        }
    }

    fn with_stdin(mut self, stdin: Vec<u8>) -> Self {
        self.stdin = Some(stdin);
        self
    }

    const fn with_stdout_limit(mut self, limit: usize) -> Self {
        self.stdout_limit = limit;
        self
    }
}

struct CommandOutput {
    stdout: Vec<u8>,
}

impl CommandOutput {
    fn text(&self, operation: &'static str) -> DesktopResult<&str> {
        std::str::from_utf8(&self.stdout).map_err(|_| DesktopError::InvalidUtf8 { operation })
    }
}

struct SystemCommandRunner {
    timeout: Duration,
}

impl SystemCommandRunner {
    async fn run(
        &self,
        spec: CommandSpec,
        control: &ExecutionControl,
    ) -> DesktopResult<CommandOutput> {
        ensure_active(control)?;
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(if spec.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|source| DesktopError::Spawn {
            operation: spec.operation,
            source,
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| DesktopError::Io {
            operation: spec.operation,
            stream: "stdout",
            source: std::io::Error::other("stdout pipe missing"),
        })?;
        let stderr = child.stderr.take().ok_or_else(|| DesktopError::Io {
            operation: spec.operation,
            stream: "stderr",
            source: std::io::Error::other("stderr pipe missing"),
        })?;
        let operation = spec.operation;
        let process = async move {
            let write = write_stdin(stdin, spec.stdin, operation);
            let read_stdout = read_limited(stdout, spec.stdout_limit, operation, "stdout");
            let read_stderr = read_limited(stderr, STDERR_LIMIT, operation, "stderr");
            let wait = async {
                child.wait().await.map_err(|source| DesktopError::Io {
                    operation,
                    stream: "process status",
                    source,
                })
            };
            let (write, stdout, stderr, status) =
                tokio::join!(write, read_stdout, read_stderr, wait);
            write?;
            let stdout = stdout?;
            let stderr = stderr?;
            let status = status?;
            if !status.success() {
                return Err(DesktopError::ProcessFailed {
                    operation,
                    status: status.code(),
                    stderr_tail: sanitize_stderr(&stderr),
                });
            }
            Ok(CommandOutput { stdout })
        };

        let remaining = control.remaining();
        let timeout = remaining.map_or(self.timeout, |remaining| remaining.min(self.timeout));
        let request_deadline_first = remaining.is_some_and(|remaining| remaining <= self.timeout);
        tokio::pin!(process);
        tokio::select! {
            biased;
            _ = control.cancelled() => Err(DesktopError::Cancelled),
            result = &mut process => result,
            _ = time::sleep(timeout) => {
                if request_deadline_first || control.is_expired() {
                    Err(DesktopError::TimedOut)
                } else {
                    Err(DesktopError::CommandTimedOut { operation })
                }
            }
        }
    }
}

async fn write_stdin(
    stdin: Option<tokio::process::ChildStdin>,
    bytes: Option<Vec<u8>>,
    operation: &'static str,
) -> DesktopResult<()> {
    if let (Some(mut stdin), Some(bytes)) = (stdin, bytes) {
        stdin
            .write_all(&bytes)
            .await
            .map_err(|source| DesktopError::Io {
                operation,
                stream: "stdin",
                source,
            })?;
        stdin.shutdown().await.map_err(|source| DesktopError::Io {
            operation,
            stream: "stdin",
            source,
        })?;
    }
    Ok(())
}

async fn read_limited<R: AsyncRead + Unpin>(
    reader: R,
    limit: usize,
    operation: &'static str,
    stream: &'static str,
) -> DesktopResult<Vec<u8>> {
    let take_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut reader = reader.take(take_limit);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| DesktopError::Io {
            operation,
            stream,
            source,
        })?;
    if bytes.len() > limit {
        return Err(DesktopError::OutputTooLarge {
            operation,
            stream,
            limit,
        });
    }
    Ok(bytes)
}

fn sanitize_stderr(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn powershell_command(
    powershell: &Path,
    operation: &'static str,
    script: impl Into<OsString>,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
) -> CommandSpec {
    let mut command_args = vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-Command".into(),
        script.into(),
    ];
    command_args.extend(args);
    let mut spec = CommandSpec::new(powershell, operation, command_args);
    spec.stdin = stdin;
    spec
}

fn append_x11_scroll(args: &mut Vec<OsString>, delta: i32, negative: &str, positive: &str) {
    if delta == 0 {
        return;
    }
    let count = delta.unsigned_abs().div_ceil(120).max(1);
    args.extend([
        "click".into(),
        "--repeat".into(),
        count.to_string().into(),
        if delta < 0 { negative } else { positive }.into(),
    ]);
}

const fn windows_virtual_key(key: DesktopKey) -> u16 {
    match key {
        DesktopKey::Enter => 0x0D,
        DesktopKey::Tab => 0x09,
        DesktopKey::Escape => 0x1B,
        DesktopKey::Delete => 0x2E,
        DesktopKey::Space => 0x20,
        DesktopKey::ArrowUp => 0x26,
        DesktopKey::ArrowDown => 0x28,
        DesktopKey::ArrowLeft => 0x25,
        DesktopKey::ArrowRight => 0x27,
    }
}

const fn linux_input_key(key: DesktopKey) -> u16 {
    match key {
        DesktopKey::Enter => 28,
        DesktopKey::Tab => 15,
        DesktopKey::Escape => 1,
        DesktopKey::Delete => 111,
        DesktopKey::Space => 57,
        DesktopKey::ArrowUp => 103,
        DesktopKey::ArrowDown => 108,
        DesktopKey::ArrowLeft => 105,
        DesktopKey::ArrowRight => 106,
    }
}

const fn x11_key(key: DesktopKey) -> &'static str {
    match key {
        DesktopKey::Enter => "Return",
        DesktopKey::Tab => "Tab",
        DesktopKey::Escape => "Escape",
        DesktopKey::Delete => "Delete",
        DesktopKey::Space => "space",
        DesktopKey::ArrowUp => "Up",
        DesktopKey::ArrowDown => "Down",
        DesktopKey::ArrowLeft => "Left",
        DesktopKey::ArrowRight => "Right",
    }
}

const fn wayland_key(key: DesktopKey) -> &'static str {
    x11_key(key)
}

const WINDOWS_PROBE: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
[Console]::Write("{0}`t{1}", $bounds.Width, $bounds.Height)
"#;

const WINDOWS_CAPTURE: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$bounds = [System.Windows.Forms.SystemInformation]::VirtualScreen
$bitmap = New-Object System.Drawing.Bitmap($bounds.Width, $bounds.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
try {
  $graphics.CopyFromScreen($bounds.X, $bounds.Y, 0, 0, $bitmap.Size)
  $stream = New-Object System.IO.MemoryStream
  try {
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $stream.ToArray()
    [Console]::OpenStandardOutput().Write($bytes, 0, $bytes.Length)
  } finally { $stream.Dispose() }
} finally {
  $graphics.Dispose()
  $bitmap.Dispose()
}
"#;

const WINDOWS_INPUT_TYPE: &str = r#"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class DeviceRailInput {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION U; }
  [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION {
    [FieldOffset(0)] public KEYBDINPUT ki;
    [FieldOffset(0)] public MOUSEINPUT mi;
  }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT {
    public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT {
    public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo;
  }
  [DllImport("user32.dll", SetLastError=true)] static extern uint SendInput(uint n, INPUT[] p, int size);
  [DllImport("user32.dll", SetLastError=true)] static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public static void Text(string value) {
    foreach (char c in value) {
      INPUT[] input = new INPUT[2]; input[0].type = 1; input[0].U.ki.wScan = c; input[0].U.ki.dwFlags = 4;
      input[1] = input[0]; input[1].U.ki.dwFlags = 6;
      if (SendInput(2, input, Marshal.SizeOf(typeof(INPUT))) != 2) throw new System.ComponentModel.Win32Exception();
    }
  }
  public static void Key(byte vk) { keybd_event(vk, 0, 0, UIntPtr.Zero); keybd_event(vk, 0, 2, UIntPtr.Zero); }
  public static void Tap(int x, int y) {
    var b = System.Windows.Forms.SystemInformation.VirtualScreen;
    if (!SetCursorPos(b.X + x, b.Y + y)) throw new System.ComponentModel.Win32Exception();
    mouse_event(2, 0, 0, 0, UIntPtr.Zero); mouse_event(4, 0, 0, 0, UIntPtr.Zero);
  }
  public static void Scroll(int x, int y) {
    if (y != 0) mouse_event(0x0800, 0, 0, unchecked((uint)-y), UIntPtr.Zero);
    if (x != 0) mouse_event(0x1000, 0, 0, unchecked((uint)x), UIntPtr.Zero);
  }
}
'@ -ReferencedAssemblies System.Windows.Forms
"#;

#[cfg(target_os = "macos")]
mod macos_native {
    use std::ffi::c_void;

    use devicerail_protocol::Viewport;

    use crate::{
        DesktopAction, DesktopError, DesktopResult, MacOsPermissions, PermissionState,
        model::DesktopKey,
    };

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    type CGEventRef = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGMainDisplayID() -> u32;
        fn CGDisplayPixelsWide(display: u32) -> usize;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
        fn CGDisplayBounds(display: u32) -> CGRect;
        fn CGEventCreateMouseEvent(
            source: *mut c_void,
            event_type: u32,
            position: CGPoint,
            button: u32,
        ) -> CGEventRef;
        fn CGEventCreateKeyboardEvent(
            source: *mut c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> CGEventRef;
        fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, text: *const u16);
        fn CGEventCreateScrollWheelEvent(
            source: *mut c_void,
            units: u32,
            wheel_count: u32,
            ...
        ) -> CGEventRef;
        fn CGEventPost(tap: u32, event: CGEventRef);
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const c_void);
    }

    pub(super) fn permissions() -> MacOsPermissions {
        // Both APIs are non-prompting preflight checks for this process.
        let screen_recording = unsafe { CGPreflightScreenCaptureAccess() };
        let accessibility = unsafe { AXIsProcessTrusted() };
        MacOsPermissions {
            screen_recording: state(screen_recording),
            accessibility: state(accessibility),
        }
    }

    pub(super) fn viewport() -> DesktopResult<Viewport> {
        // CoreGraphics returns immutable scalar display data for the main display.
        let display = unsafe { CGMainDisplayID() };
        let width = u32::try_from(unsafe { CGDisplayPixelsWide(display) })
            .map_err(|_| DesktopError::ScreenshotTooLarge)?;
        let height = u32::try_from(unsafe { CGDisplayPixelsHigh(display) })
            .map_err(|_| DesktopError::ScreenshotTooLarge)?;
        let bounds = unsafe { CGDisplayBounds(display) };
        let scale_factor = if bounds.size.width > 0.0 {
            f64::from(width) / bounds.size.width
        } else {
            return Err(DesktopError::MalformedOutput {
                operation: "desktop_macos_probe",
            });
        };
        let viewport = Viewport {
            width,
            height,
            scale_factor,
        };
        crate::model::validate_viewport(&viewport)?;
        Ok(viewport)
    }

    pub(super) fn execute(action: DesktopAction) -> DesktopResult<()> {
        let viewport = viewport()?;
        match action {
            DesktopAction::Tap { x, y } => {
                let point = CGPoint {
                    x: f64::from(x) / viewport.scale_factor,
                    y: f64::from(y) / viewport.scale_factor,
                };
                post_mouse(1, point)?;
                post_mouse(2, point)
            }
            DesktopAction::InputText(text) => {
                let utf16 = text.encode_utf16().collect::<Vec<_>>();
                post_keyboard(0, true, Some(&utf16))?;
                post_keyboard(0, false, Some(&utf16))
            }
            DesktopAction::KeyPress(key) => {
                let code = macos_key(key);
                post_keyboard(code, true, None)?;
                post_keyboard(code, false, None)
            }
            DesktopAction::Scroll { delta_x, delta_y } => {
                // kCGScrollEventUnitPixel = 0; Quartz vertical wheel direction is
                // the inverse of DeviceRail's positive-down coordinate contract.
                let event = unsafe {
                    CGEventCreateScrollWheelEvent(
                        std::ptr::null_mut(),
                        0,
                        2,
                        delta_y.saturating_neg(),
                        delta_x,
                    )
                };
                post_and_release(event)
            }
        }
    }

    const fn state(granted: bool) -> PermissionState {
        if granted {
            PermissionState::Granted
        } else {
            PermissionState::Denied
        }
    }

    fn post_mouse(event_type: u32, position: CGPoint) -> DesktopResult<()> {
        let event =
            unsafe { CGEventCreateMouseEvent(std::ptr::null_mut(), event_type, position, 0) };
        post_and_release(event)
    }

    fn post_keyboard(key: u16, down: bool, text: Option<&[u16]>) -> DesktopResult<()> {
        let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null_mut(), key, down) };
        if event.is_null() {
            return Err(DesktopError::MacOsInputFailed);
        }
        if let Some(text) = text {
            unsafe { CGEventKeyboardSetUnicodeString(event, text.len(), text.as_ptr()) };
        }
        post_and_release(event)
    }

    fn post_and_release(event: CGEventRef) -> DesktopResult<()> {
        if event.is_null() {
            return Err(DesktopError::MacOsInputFailed);
        }
        // The event is posted synchronously, then its Create-owned reference is released.
        unsafe {
            CGEventPost(0, event);
            CFRelease(event.cast_const());
        }
        Ok(())
    }

    const fn macos_key(key: DesktopKey) -> u16 {
        match key {
            DesktopKey::Enter => 36,
            DesktopKey::Tab => 48,
            DesktopKey::Escape => 53,
            DesktopKey::Delete => 51,
            DesktopKey::Space => 49,
            DesktopKey::ArrowUp => 126,
            DesktopKey::ArrowDown => 125,
            DesktopKey::ArrowLeft => 123,
            DesktopKey::ArrowRight => 124,
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod macos_native {
    use devicerail_protocol::Viewport;

    use crate::{DesktopAction, DesktopError, DesktopResult, MacOsPermissions, PermissionState};

    pub(super) const fn permissions() -> MacOsPermissions {
        MacOsPermissions {
            screen_recording: PermissionState::Denied,
            accessibility: PermissionState::Denied,
        }
    }

    pub(super) fn viewport() -> DesktopResult<Viewport> {
        Err(DesktopError::HostPlatformMismatch {
            requested: "macos",
            actual: std::env::consts::OS,
        })
    }

    pub(super) fn execute(_action: DesktopAction) -> DesktopResult<()> {
        Err(DesktopError::HostPlatformMismatch {
            requested: "macos",
            actual: std::env::consts::OS,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs::File, path::Path, time::Duration};

    use crate::{DesktopError, WaylandInputBackend};

    use super::{MAX_COMMAND_TIMEOUT, SystemDesktopConfig, WaylandTool, discover_wayland_input};

    fn create_tool(path: &Path) {
        File::create(path).expect("create fake desktop tool");
    }

    #[test]
    fn system_config_validation_is_public_and_bounded() {
        let mut config = SystemDesktopConfig::default();
        config.validate().expect("default desktop System config");

        config.command_timeout = MAX_COMMAND_TIMEOUT;
        config.validate().expect("maximum command timeout");
        config.command_timeout = MAX_COMMAND_TIMEOUT + Duration::from_millis(1);
        assert!(config.validate().is_err());

        config.command_timeout = Duration::ZERO;
        assert!(config.validate().is_err());
    }

    #[test]
    fn automatic_wayland_input_prefers_ydotool_then_wtype() {
        let directory = tempfile::tempdir().expect("temporary tool directory");
        let ydotool = directory.path().join("ydotool");
        let wtype = directory.path().join("wtype");
        create_tool(&ydotool);
        create_tool(&wtype);

        let config = SystemDesktopConfig {
            wayland_ydotool: ydotool.clone(),
            wayland_wtype: wtype.clone(),
            ..SystemDesktopConfig::default()
        };
        let (tool, profile) = discover_wayland_input(&config).expect("automatic Wayland input");
        assert!(matches!(tool, WaylandTool::Ydotool(path) if path == ydotool));
        assert_eq!(
            profile.wayland_input_backend(),
            Some(WaylandInputBackend::Ydotool)
        );

        std::fs::remove_file(&config.wayland_ydotool).expect("remove fake ydotool");
        let (tool, profile) = discover_wayland_input(&config).expect("automatic wtype fallback");
        assert!(matches!(tool, WaylandTool::Wtype(path) if path == wtype));
        assert_eq!(
            profile.wayland_input_backend(),
            Some(WaylandInputBackend::Wtype)
        );
    }

    #[test]
    fn explicit_wayland_backend_uses_only_the_selected_tool() {
        let directory = tempfile::tempdir().expect("temporary tool directory");
        let ydotool = directory.path().join("ydotool");
        let wtype = directory.path().join("wtype");
        create_tool(&ydotool);
        create_tool(&wtype);

        for (backend, expected_path) in [
            (WaylandInputBackend::Ydotool, ydotool.clone()),
            (WaylandInputBackend::Wtype, wtype.clone()),
        ] {
            let config = SystemDesktopConfig {
                wayland_ydotool: ydotool.clone(),
                wayland_wtype: wtype.clone(),
                wayland_input_backend: Some(backend),
                ..SystemDesktopConfig::default()
            };
            let (tool, profile) = discover_wayland_input(&config).expect("explicit Wayland input");
            match backend {
                WaylandInputBackend::Ydotool => {
                    assert!(matches!(tool, WaylandTool::Ydotool(path) if path == expected_path));
                }
                WaylandInputBackend::Wtype => {
                    assert!(matches!(tool, WaylandTool::Wtype(path) if path == expected_path));
                }
            }
            assert_eq!(profile.wayland_input_backend(), Some(backend));
        }
    }

    #[test]
    fn explicit_wayland_backend_never_falls_back() {
        let directory = tempfile::tempdir().expect("temporary tool directory");
        let ydotool = directory.path().join("ydotool");
        let wtype = directory.path().join("wtype");
        create_tool(&ydotool);
        create_tool(&wtype);

        for (backend, missing) in [
            (
                WaylandInputBackend::Ydotool,
                directory.path().join("missing-ydotool"),
            ),
            (
                WaylandInputBackend::Wtype,
                directory.path().join("missing-wtype"),
            ),
        ] {
            let mut config = SystemDesktopConfig {
                wayland_ydotool: ydotool.clone(),
                wayland_wtype: wtype.clone(),
                wayland_input_backend: Some(backend),
                ..SystemDesktopConfig::default()
            };
            match backend {
                WaylandInputBackend::Ydotool => config.wayland_ydotool = missing.clone(),
                WaylandInputBackend::Wtype => config.wayland_wtype = missing.clone(),
            }
            let error = match discover_wayland_input(&config) {
                Ok(_) => panic!("explicit backend must not use the other available tool"),
                Err(error) => error,
            };
            assert!(matches!(error, DesktopError::ToolNotFound { tool } if tool == missing));
        }
    }
}
