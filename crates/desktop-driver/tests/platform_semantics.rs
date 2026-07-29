mod support;

use std::sync::Arc;

use devicerail_core::{
    DeviceDriver, DeviceRuntime, DriverError, ExecutionControl, MemoryEventStore, OperationContext,
    RuntimeError, ScreenshotPolicy, SessionEventStore, StartSession, now_ms,
};
use devicerail_desktop_driver::{
    DesktopBackend, DesktopProfile, LinuxDriver, MacOsDriver, MacOsPermissions,
    NativeDesktopDriver, PermissionState, WaylandInputBackend, detect_linux_display_server,
};
use devicerail_protocol::{ActionCall, Platform, ScreenshotOmissionReason, Viewport};
use serde_json::json;
use uuid::Uuid;

use support::{FakeBackend, identity, isolated_evidence_store};

#[tokio::test]
async fn macos_permissions_fail_closed_with_stable_platform_codes() {
    for (permissions, expected_code) in [
        (
            MacOsPermissions {
                screen_recording: PermissionState::Denied,
                accessibility: PermissionState::Granted,
            },
            "desktop_macos_screen_recording_required",
        ),
        (
            MacOsPermissions {
                screen_recording: PermissionState::Granted,
                accessibility: PermissionState::Denied,
            },
            "desktop_macos_accessibility_required",
        ),
    ] {
        let driver = MacOsDriver::new(
            identity("macos-permission"),
            Arc::new(FakeBackend::new(DesktopProfile::macos(permissions)))
                as Arc<dyn DesktopBackend>,
        )
        .expect("macOS Driver");
        let error = driver
            .connect(&ExecutionControl::unbounded())
            .await
            .expect_err("permission must block connect");
        assert!(matches!(
            error,
            DriverError::Platform { code, retryable: false } if code == expected_code
        ));
    }
}

#[tokio::test]
async fn wayland_wtype_never_advertises_pointer_actions() {
    let driver = LinuxDriver::new(
        identity("wayland-wtype"),
        Arc::new(FakeBackend::new(DesktopProfile::linux_wayland(
            WaylandInputBackend::Wtype,
        ))) as Arc<dyn DesktopBackend>,
    )
    .expect("Wayland wtype Driver");

    let capabilities = driver
        .capabilities(&ExecutionControl::unbounded())
        .await
        .expect("Wayland capabilities");
    assert_eq!(
        capabilities
            .iter()
            .map(|action| action.name.as_str())
            .collect::<Vec<_>>(),
        ["inputText", "keyPress"]
    );
    assert_eq!(driver.action_protection("tap"), None);
    assert_eq!(driver.action_protection("scroll"), None);
    assert_eq!(
        driver.profile().linux_display_server().unwrap().as_str(),
        "wayland"
    );
}

#[test]
fn linux_session_detection_rejects_ambiguity_and_unknown_values() {
    assert_eq!(
        detect_linux_display_server(Some("x11"), Some("wayland-0"), Some(":0"))
            .expect("explicit X11"),
        devicerail_desktop_driver::LinuxDisplayServer::X11
    );
    assert_eq!(
        detect_linux_display_server(None, Some("wayland-0"), None).expect("Wayland display"),
        devicerail_desktop_driver::LinuxDisplayServer::Wayland
    );
    assert!(detect_linux_display_server(None, Some("wayland-0"), Some(":0")).is_err());
    assert!(detect_linux_display_server(Some("tty"), None, None).is_err());
}

#[tokio::test]
async fn omission_uses_probe_without_capturing_screen_pixels() {
    let backend = Arc::new(FakeBackend::new(DesktopProfile::linux_x11()));
    let driver = Arc::new(
        LinuxDriver::new(
            identity("linux-omit"),
            Arc::clone(&backend) as Arc<dyn DesktopBackend>,
        )
        .expect("Linux Driver"),
    );
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let events = Arc::new(MemoryEventStore::default());
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("start Session");
    let context = OperationContext::new(session.id, None);
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), events, isolated_evidence_store())
            .with_screenshot_policy(ScreenshotPolicy::Omit);

    let observation = runtime
        .observe(&context)
        .await
        .expect("omitted Observation");
    assert!(observation.screenshot.is_none());
    assert_eq!(
        observation.screenshot_omission,
        Some(ScreenshotOmissionReason::Policy)
    );
    assert_eq!(observation.metadata["desktopPlatform"], "linux");
    assert_eq!(observation.metadata["linuxDisplayServer"], "x11");
    assert_eq!(backend.capture_count(), 0);
    assert!(backend.probe_count() >= 2, "connect and Observation probe");
}

#[tokio::test]
async fn tap_uses_the_refreshed_before_viewport_after_a_display_shrinks() {
    let large = Viewport {
        width: 100,
        height: 100,
        scale_factor: 1.0,
    };
    let small = Viewport {
        width: 10,
        height: 10,
        scale_factor: 1.0,
    };
    let backend = Arc::new(FakeBackend::with_probe_viewports(
        DesktopProfile::linux_x11(),
        vec![large, small],
    ));
    let driver = Arc::new(
        LinuxDriver::new(
            identity("linux-shrinking-viewport"),
            Arc::clone(&backend) as Arc<dyn DesktopBackend>,
        )
        .expect("Linux Driver"),
    );
    driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect with the original viewport");
    let events = Arc::new(MemoryEventStore::default());
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("start Session");
    let context = OperationContext::new(session.id, None);
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), events, isolated_evidence_store())
            .with_screenshot_policy(ScreenshotPolicy::Omit);

    let error = runtime
        .execute(
            &context,
            ActionCall {
                id: Uuid::new_v4(),
                name: "tap".to_owned(),
                arguments: json!({ "x": 50, "y": 50 }),
            },
        )
        .await
        .expect_err("tap outside the refreshed viewport must be rejected");
    assert!(matches!(
        error,
        RuntimeError::Driver(DriverError::InvalidArguments { ref action, .. }) if action == "tap"
    ));
    assert!(
        backend.actions().is_empty(),
        "tap must not reach the backend"
    );
    assert_eq!(
        backend.capture_count(),
        0,
        "omission must not capture pixels"
    );
    assert_eq!(backend.probe_count(), 2, "connect and before probe");
}

#[tokio::test]
async fn platform_identity_remains_typed_on_device_info() {
    let driver = LinuxDriver::new(
        identity("linux-platform"),
        Arc::new(FakeBackend::new(DesktopProfile::linux_x11())) as Arc<dyn DesktopBackend>,
    )
    .expect("Linux Driver");
    let info = driver
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    assert_eq!(info.platform, Platform::Linux);
    assert!(info.connected);
}

#[tokio::test]
async fn native_wrapper_exposes_initial_device_info_before_registry_consumes_it() {
    let native = NativeDesktopDriver::Linux(
        LinuxDriver::new(
            identity("linux-native-wrapper"),
            Arc::new(FakeBackend::new(DesktopProfile::linux_x11())) as Arc<dyn DesktopBackend>,
        )
        .expect("Linux Driver"),
    );
    let info = native.device_info().await;
    assert_eq!(info.platform, Platform::Linux);
    assert!(!info.connected);
}
