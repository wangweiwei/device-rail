//! Native macOS, Windows, X11, and Wayland adapters for DeviceRail.
//!
//! The crate keeps platform commands and native APIs behind [`DesktopBackend`].
//! This makes permission probes, screenshots, and input independently fakeable
//! without exposing platform-library types at the protocol boundary.

mod backend;
mod driver;
mod error;
mod model;
mod system;

pub use backend::DesktopBackend;
pub use driver::{LinuxDriver, MacOsDriver, WindowsDriver};
pub use error::{DesktopError, DesktopResult};
pub use model::{
    DesktopAction, DesktopActionKind, DesktopCapture, DesktopIdentity, DesktopKey, DesktopProbe,
    DesktopProfile, LinuxDisplayServer, MacOsPermission, MacOsPermissions, PermissionState,
    WaylandInputBackend,
};
pub use system::{
    NativeDesktopDriver, SystemDesktopConfig, detect_linux_display_server, discover_native_driver,
};
