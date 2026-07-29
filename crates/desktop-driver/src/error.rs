use std::{io, path::PathBuf};

use thiserror::Error;

use crate::{DesktopActionKind, LinuxDisplayServer, MacOsPermission, PermissionState};

pub type DesktopResult<T> = Result<T, DesktopError>;

/// Stable failures produced by the native desktop boundary.
///
/// Driver conversion retains only [`Self::code`] and [`Self::retryable`], so
/// command output and local paths never cross the wire.
#[derive(Debug, Error)]
pub enum DesktopError {
    #[error("desktop operation was cancelled")]
    Cancelled,
    #[error("desktop operation exceeded the request deadline")]
    TimedOut,
    #[error("desktop command `{operation}` exceeded its local timeout")]
    CommandTimedOut { operation: &'static str },
    #[error("desktop platform `{platform}` is not supported by this build")]
    UnsupportedHost { platform: String },
    #[error("requested {requested} driver on a {actual} host")]
    HostPlatformMismatch {
        requested: &'static str,
        actual: &'static str,
    },
    #[error("Linux display server could not be determined without ambiguity")]
    LinuxDisplayServerUnknown,
    #[error("unsupported XDG_SESSION_TYPE value: {value}")]
    UnsupportedLinuxSession { value: String },
    #[error("Wayland viewport must be configured without taking a screenshot")]
    WaylandViewportRequired,
    #[error("required desktop tool was not found: {tool}")]
    ToolNotFound { tool: PathBuf },
    #[error("no supported input tool is available for {display_server:?}")]
    InputToolNotFound { display_server: LinuxDisplayServer },
    #[error("failed to spawn desktop command `{operation}`: {source}")]
    Spawn {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("desktop command `{operation}` failed with status {status:?}: {stderr_tail}")]
    ProcessFailed {
        operation: &'static str,
        status: Option<i32>,
        stderr_tail: String,
    },
    #[error("failed to access {stream} for desktop command `{operation}`: {source}")]
    Io {
        operation: &'static str,
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{stream} from desktop command `{operation}` exceeded {limit} bytes")]
    OutputTooLarge {
        operation: &'static str,
        stream: &'static str,
        limit: usize,
    },
    #[error("desktop command `{operation}` returned invalid UTF-8")]
    InvalidUtf8 { operation: &'static str },
    #[error("desktop command `{operation}` returned malformed output")]
    MalformedOutput { operation: &'static str },
    #[error("desktop screenshot is malformed: {0}")]
    MalformedPng(String),
    #[error("desktop screenshot dimensions exceed the supported limit")]
    ScreenshotTooLarge,
    #[error("desktop profile is invalid: {0}")]
    InvalidProfile(String),
    #[error("desktop backend contract changed after construction")]
    BackendContractChanged,
    #[error("macOS permission {permission:?} is {state:?}")]
    MacOsPermissionRequired {
        permission: MacOsPermission,
        state: PermissionState,
    },
    #[error("desktop backend does not support action {action:?}")]
    UnsupportedAction { action: DesktopActionKind },
    #[error("native macOS input operation failed")]
    MacOsInputFailed,
}

impl DesktopError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "desktop_cancelled",
            Self::TimedOut => "desktop_timed_out",
            Self::CommandTimedOut { .. } => "desktop_command_timed_out",
            Self::UnsupportedHost { .. } => "desktop_host_unsupported",
            Self::HostPlatformMismatch { .. } => "desktop_host_mismatch",
            Self::LinuxDisplayServerUnknown => "desktop_linux_session_unknown",
            Self::UnsupportedLinuxSession { .. } => "desktop_linux_session_unsupported",
            Self::WaylandViewportRequired => "desktop_wayland_viewport_required",
            Self::ToolNotFound { .. } => "desktop_tool_not_found",
            Self::InputToolNotFound { .. } => "desktop_input_tool_not_found",
            Self::Spawn { .. } => "desktop_command_spawn_failed",
            Self::ProcessFailed { .. } => "desktop_command_failed",
            Self::Io { .. } => "desktop_command_io_failed",
            Self::OutputTooLarge { .. } => "desktop_command_output_too_large",
            Self::InvalidUtf8 { .. } => "desktop_command_invalid_utf8",
            Self::MalformedOutput { .. } => "desktop_command_malformed_output",
            Self::MalformedPng(_) => "desktop_malformed_png",
            Self::ScreenshotTooLarge => "desktop_screenshot_too_large",
            Self::InvalidProfile(_) => "desktop_profile_invalid",
            Self::BackendContractChanged => "desktop_backend_contract_changed",
            Self::MacOsPermissionRequired {
                permission: MacOsPermission::ScreenRecording,
                ..
            } => "desktop_macos_screen_recording_required",
            Self::MacOsPermissionRequired {
                permission: MacOsPermission::Accessibility,
                ..
            } => "desktop_macos_accessibility_required",
            Self::UnsupportedAction { .. } => "desktop_action_unsupported",
            Self::MacOsInputFailed => "desktop_macos_input_failed",
        }
    }

    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::TimedOut
                | Self::CommandTimedOut { .. }
                | Self::Spawn { .. }
                | Self::ProcessFailed { .. }
                | Self::Io { .. }
                | Self::MalformedPng(_)
                | Self::MacOsInputFailed
        )
    }
}
