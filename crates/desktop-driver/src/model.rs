use std::collections::BTreeSet;

use devicerail_protocol::{DeviceId, Platform, Viewport};
use serde_json::{Map, Value};

use crate::{DesktopError, DesktopResult};

const MAX_DESKTOP_DEVICE_ID_BYTES: usize = 512;
const MAX_DESKTOP_DEVICE_NAME_BYTES: usize = 1_024;
const MAX_DESKTOP_OS_VERSION_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DesktopActionKind {
    Tap,
    InputText,
    KeyPress,
    Scroll,
}

impl DesktopActionKind {
    pub const ALL: [Self; 4] = [Self::Tap, Self::InputText, Self::KeyPress, Self::Scroll];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tap => "tap",
            Self::InputText => "inputText",
            Self::KeyPress => "keyPress",
            Self::Scroll => "scroll",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopKey {
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

impl DesktopKey {
    pub const VALUES: [&'static str; 9] = [
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

    pub fn parse(value: &str) -> Option<Self> {
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

    pub const fn as_str(self) -> &'static str {
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopAction {
    Tap { x: u32, y: u32 },
    InputText(String),
    KeyPress(DesktopKey),
    Scroll { delta_x: i32, delta_y: i32 },
}

impl DesktopAction {
    pub const fn kind(&self) -> DesktopActionKind {
        match self {
            Self::Tap { .. } => DesktopActionKind::Tap,
            Self::InputText(_) => DesktopActionKind::InputText,
            Self::KeyPress(_) => DesktopActionKind::KeyPress,
            Self::Scroll { .. } => DesktopActionKind::Scroll,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxDisplayServer {
    X11,
    Wayland,
}

impl LinuxDisplayServer {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X11 => "x11",
            Self::Wayland => "wayland",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaylandInputBackend {
    Ydotool,
    Wtype,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionState {
    Granted,
    Denied,
    NotRequired,
}

impl PermissionState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotRequired => "notRequired",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacOsPermission {
    ScreenRecording,
    Accessibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacOsPermissions {
    pub screen_recording: PermissionState,
    pub accessibility: PermissionState,
}

impl MacOsPermissions {
    pub const fn granted() -> Self {
        Self {
            screen_recording: PermissionState::Granted,
            accessibility: PermissionState::Granted,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopProfile {
    platform: Platform,
    linux_display_server: Option<LinuxDisplayServer>,
    wayland_input_backend: Option<WaylandInputBackend>,
    macos_permissions: Option<MacOsPermissions>,
    actions: BTreeSet<DesktopActionKind>,
}

impl DesktopProfile {
    pub fn macos(permissions: MacOsPermissions) -> Self {
        Self {
            platform: Platform::MacOs,
            linux_display_server: None,
            wayland_input_backend: None,
            macos_permissions: Some(permissions),
            actions: DesktopActionKind::ALL.into_iter().collect(),
        }
    }

    pub fn windows() -> Self {
        Self {
            platform: Platform::Windows,
            linux_display_server: None,
            wayland_input_backend: None,
            macos_permissions: None,
            actions: DesktopActionKind::ALL.into_iter().collect(),
        }
    }

    pub fn linux_x11() -> Self {
        Self {
            platform: Platform::Linux,
            linux_display_server: Some(LinuxDisplayServer::X11),
            wayland_input_backend: None,
            macos_permissions: None,
            actions: DesktopActionKind::ALL.into_iter().collect(),
        }
    }

    pub fn linux_wayland(input: WaylandInputBackend) -> Self {
        let actions = match input {
            WaylandInputBackend::Ydotool => DesktopActionKind::ALL.into_iter().collect(),
            WaylandInputBackend::Wtype => {
                [DesktopActionKind::InputText, DesktopActionKind::KeyPress]
                    .into_iter()
                    .collect()
            }
        };
        Self {
            platform: Platform::Linux,
            linux_display_server: Some(LinuxDisplayServer::Wayland),
            wayland_input_backend: Some(input),
            macos_permissions: None,
            actions,
        }
    }

    pub fn platform(&self) -> &Platform {
        &self.platform
    }

    pub const fn linux_display_server(&self) -> Option<LinuxDisplayServer> {
        self.linux_display_server
    }

    pub const fn wayland_input_backend(&self) -> Option<WaylandInputBackend> {
        self.wayland_input_backend
    }

    pub const fn macos_permissions(&self) -> Option<MacOsPermissions> {
        self.macos_permissions
    }

    pub fn actions(&self) -> impl Iterator<Item = DesktopActionKind> + '_ {
        self.actions.iter().copied()
    }

    pub fn supports(&self, action: DesktopActionKind) -> bool {
        self.actions.contains(&action)
    }

    pub(crate) fn validate(&self) -> DesktopResult<()> {
        if self.actions.is_empty() {
            return Err(DesktopError::InvalidProfile(
                "at least one input action must be advertised".to_owned(),
            ));
        }
        match self.platform {
            Platform::MacOs
                if self.macos_permissions.is_some()
                    && self.linux_display_server.is_none()
                    && self.wayland_input_backend.is_none() => {}
            Platform::Windows
                if self.macos_permissions.is_none()
                    && self.linux_display_server.is_none()
                    && self.wayland_input_backend.is_none() => {}
            Platform::Linux
                if self.macos_permissions.is_none()
                    && self.linux_display_server.is_some()
                    && (self.linux_display_server != Some(LinuxDisplayServer::Wayland)
                        || self.wayland_input_backend.is_some()) => {}
            _ => {
                return Err(DesktopError::InvalidProfile(
                    "platform-specific profile fields are inconsistent".to_owned(),
                ));
            }
        }
        if self.wayland_input_backend == Some(WaylandInputBackend::Wtype)
            && (self.supports(DesktopActionKind::Tap) || self.supports(DesktopActionKind::Scroll))
        {
            return Err(DesktopError::InvalidProfile(
                "wtype cannot advertise pointer actions".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn same_contract(&self, other: &Self) -> bool {
        self.platform == other.platform
            && self.linux_display_server == other.linux_display_server
            && self.wayland_input_backend == other.wayland_input_backend
            && self.actions == other.actions
    }
}

#[derive(Clone, PartialEq)]
pub struct DesktopIdentity {
    pub id: DeviceId,
    pub name: String,
    pub os_version: Option<String>,
}

impl std::fmt::Debug for DesktopIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopIdentity")
            .field("os_version_configured", &self.os_version.is_some())
            .finish()
    }
}

impl DesktopIdentity {
    pub fn new(id: impl Into<String>, name: impl Into<String>, os_version: Option<String>) -> Self {
        Self {
            id: DeviceId::new(id),
            name: name.into(),
            os_version,
        }
    }

    pub fn validate(&self) -> DesktopResult<()> {
        if !valid_desktop_device_id(&self.id.0) {
            return Err(DesktopError::InvalidProfile(
                "desktop device id must be bounded safe ASCII".to_owned(),
            ));
        }
        if !bounded_text(&self.name, MAX_DESKTOP_DEVICE_NAME_BYTES) {
            return Err(DesktopError::InvalidProfile(
                "desktop device name must be bounded text".to_owned(),
            ));
        }
        if self
            .os_version
            .as_deref()
            .is_some_and(|version| !bounded_text(version, MAX_DESKTOP_OS_VERSION_BYTES))
        {
            return Err(DesktopError::InvalidProfile(
                "desktop OS version must be bounded text when present".to_owned(),
            ));
        }
        Ok(())
    }
}

fn valid_desktop_device_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_DESKTOP_DEVICE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn bounded_text(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopProbe {
    pub profile: DesktopProfile,
    pub viewport: Viewport,
}

impl DesktopProbe {
    pub fn new(profile: DesktopProfile, viewport: Viewport) -> DesktopResult<Self> {
        profile.validate()?;
        validate_viewport(&viewport)?;
        Ok(Self { profile, viewport })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopCapture {
    pub png: Vec<u8>,
    pub viewport: Viewport,
    pub metadata: Map<String, Value>,
}

impl DesktopCapture {
    pub fn new(png: Vec<u8>, viewport: Viewport) -> DesktopResult<Self> {
        validate_viewport(&viewport)?;
        Ok(Self {
            png,
            viewport,
            metadata: Map::new(),
        })
    }

    pub fn with_metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }
}

pub(crate) fn validate_viewport(viewport: &Viewport) -> DesktopResult<()> {
    if viewport.width == 0
        || viewport.height == 0
        || !viewport.scale_factor.is_finite()
        || viewport.scale_factor <= 0.0
    {
        return Err(DesktopError::InvalidProfile(
            "viewport dimensions and scale factor must be positive".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DesktopIdentity, MAX_DESKTOP_DEVICE_ID_BYTES, MAX_DESKTOP_DEVICE_NAME_BYTES,
        MAX_DESKTOP_OS_VERSION_BYTES,
    };

    #[test]
    fn desktop_identity_accepts_exact_byte_boundaries() {
        DesktopIdentity::new(
            "a".repeat(MAX_DESKTOP_DEVICE_ID_BYTES),
            "n".repeat(MAX_DESKTOP_DEVICE_NAME_BYTES),
            Some("v".repeat(MAX_DESKTOP_OS_VERSION_BYTES)),
        )
        .validate()
        .expect("identity at every byte boundary");
    }

    #[test]
    fn desktop_identity_rejects_oversized_fields() {
        for identity in [
            DesktopIdentity::new("a".repeat(MAX_DESKTOP_DEVICE_ID_BYTES + 1), "desktop", None),
            DesktopIdentity::new(
                "desktop-local",
                "n".repeat(MAX_DESKTOP_DEVICE_NAME_BYTES + 1),
                None,
            ),
            DesktopIdentity::new(
                "desktop-local",
                "desktop",
                Some("v".repeat(MAX_DESKTOP_OS_VERSION_BYTES + 1)),
            ),
        ] {
            assert!(identity.validate().is_err());
        }
    }

    #[test]
    fn desktop_identity_rejects_unsafe_ids_and_control_text() {
        for id in [
            "",
            "desktop local",
            "desktop/local",
            "desktop\nlocal",
            "桌面",
        ] {
            assert!(
                DesktopIdentity::new(id, "desktop", None)
                    .validate()
                    .is_err()
            );
        }
        for name in ["   ", "desktop\nname"] {
            assert!(
                DesktopIdentity::new("desktop-local", name, None)
                    .validate()
                    .is_err()
            );
        }
        for version in ["   ", "desktop\rversion"] {
            assert!(
                DesktopIdentity::new("desktop-local", "desktop", Some(version.to_owned()))
                    .validate()
                    .is_err()
            );
        }
    }

    #[test]
    fn desktop_identity_debug_redacts_all_identity_fields() {
        let identity = DesktopIdentity::new(
            "desktop:debug-sentinel-id",
            "debug-sentinel-name",
            Some("debug-sentinel-version".to_owned()),
        );
        let debug = format!("{identity:?}");
        assert!(debug.contains("os_version_configured: true"));
        for sentinel in [
            "debug-sentinel-id",
            "debug-sentinel-name",
            "debug-sentinel-version",
        ] {
            assert!(!debug.contains(sentinel));
        }
    }
}
