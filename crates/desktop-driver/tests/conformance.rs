mod support;

use std::sync::Arc;

use devicerail_desktop_driver::{
    DesktopBackend, DesktopProfile, LinuxDriver, MacOsDriver, MacOsPermissions, WindowsDriver,
};

use support::{FakeBackend, identity, isolated_evidence_store, valid_call};

devicerail_core::driver_conformance_test!(
    macos_driver_conforms_to_device_driver_contract,
    || MacOsDriver::new(
        identity("macos-conformance"),
        Arc::new(FakeBackend::new(DesktopProfile::macos(
            MacOsPermissions::granted(),
        ))) as Arc<dyn DesktopBackend>,
    )
    .expect("macOS test Driver"),
    valid_call,
    isolated_evidence_store(),
);

devicerail_core::driver_conformance_test!(
    windows_driver_conforms_to_device_driver_contract,
    || WindowsDriver::new(
        identity("windows-conformance"),
        Arc::new(FakeBackend::new(DesktopProfile::windows())) as Arc<dyn DesktopBackend>,
    )
    .expect("Windows test Driver"),
    valid_call,
    isolated_evidence_store(),
);

devicerail_core::driver_conformance_test!(
    linux_driver_conforms_to_device_driver_contract,
    || LinuxDriver::new(
        identity("linux-x11-conformance"),
        Arc::new(FakeBackend::new(DesktopProfile::linux_x11())) as Arc<dyn DesktopBackend>,
    )
    .expect("Linux test Driver"),
    valid_call,
    isolated_evidence_store(),
);

devicerail_core::driver_conformance_test!(
    linux_wayland_ydotool_driver_conforms_to_device_driver_contract,
    || LinuxDriver::new(
        identity("linux-wayland-ydotool-conformance"),
        Arc::new(FakeBackend::new(DesktopProfile::linux_wayland(
            devicerail_desktop_driver::WaylandInputBackend::Ydotool,
        ))) as Arc<dyn DesktopBackend>,
    )
    .expect("Linux Wayland ydotool test Driver"),
    valid_call,
    isolated_evidence_store(),
);

devicerail_core::driver_conformance_test!(
    linux_wayland_wtype_driver_conforms_to_device_driver_contract,
    || LinuxDriver::new(
        identity("linux-wayland-wtype-conformance"),
        Arc::new(FakeBackend::new(DesktopProfile::linux_wayland(
            devicerail_desktop_driver::WaylandInputBackend::Wtype,
        ))) as Arc<dyn DesktopBackend>,
    )
    .expect("Linux Wayland wtype test Driver"),
    valid_call,
    isolated_evidence_store(),
);
