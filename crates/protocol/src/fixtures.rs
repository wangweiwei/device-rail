//! Canonical cross-language Golden Fixtures embedded for consistency suites.
//!
//! This module is available only with the non-default `fixtures` feature.
//! Every value is embedded directly from `crates/protocol/fixtures`, so users
//! do not need repository-relative filesystem paths and there is no copied
//! fixture body that can drift from the canonical source.

/// One canonical fixture embedded in the protocol crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalFixture {
    /// Path relative to the canonical fixture manifest.
    pub path: &'static str,
    /// Exact UTF-8 JSON source.
    pub json: &'static str,
}

impl CanonicalFixture {
    const fn new(path: &'static str, json: &'static str) -> Self {
        Self { path, json }
    }
}

macro_rules! fixture {
    ($path:literal) => {
        CanonicalFixture::new($path, include_str!(concat!("../fixtures/", $path)))
    };
}

/// Exact canonical fixture manifest JSON.
pub const MANIFEST_JSON: &str = include_str!("../fixtures/manifest.json");
/// Canonical `system.hello` request fixture.
pub const SYSTEM_HELLO_REQUEST_JSON: &str =
    include_str!("../fixtures/system-hello-v1.request.json");
/// Canonical `system.hello` response fixture.
pub const SYSTEM_HELLO_RESPONSE_JSON: &str =
    include_str!("../fixtures/system-hello-v1.response.json");
/// Canonical `devices.list` request fixture.
pub const DEVICES_LIST_REQUEST_JSON: &str =
    include_str!("../fixtures/rpc/devices-list-v1.request.json");
/// Canonical `devices.list` response fixture.
pub const DEVICES_LIST_RESPONSE_JSON: &str =
    include_str!("../fixtures/rpc/devices-list-v1.response.json");
/// Canonical `device.connect` request fixture.
pub const DEVICE_CONNECT_REQUEST_JSON: &str =
    include_str!("../fixtures/rpc/device-connect-v1.request.json");
/// Canonical `device.connect` response fixture.
pub const DEVICE_CONNECT_RESPONSE_JSON: &str =
    include_str!("../fixtures/rpc/device-connect-v1.response.json");
/// Canonical JSON-RPC failure fixture.
pub const RPC_FAILURE_JSON: &str = include_str!("../fixtures/rpc/failure-v1.json");
/// Canonical event-stream event notification fixture.
pub const EVENTS_STREAM_EVENT_NOTIFICATION_JSON: &str =
    include_str!("../fixtures/stream/events-stream-event-v1.notification.json");
/// Canonical event-stream terminal notification fixture.
pub const EVENTS_STREAM_TERMINAL_NOTIFICATION_JSON: &str =
    include_str!("../fixtures/stream/events-stream-terminal-v1.notification.json");

/// Every JSON fixture named by [`MANIFEST_JSON`], in stable path order.
pub const CANONICAL_FIXTURES: &[CanonicalFixture] = &[
    fixture!("events/action-completed-cancelled-v1.json"),
    fixture!("events/action-completed-failed-v1.json"),
    fixture!("events/action-completed-succeeded-v1.json"),
    fixture!("events/action-completed-timed-out-v1.json"),
    fixture!("events/action-started-cancelled-v1.json"),
    fixture!("events/action-started-failed-v1.json"),
    fixture!("events/action-started-succeeded-v1.json"),
    fixture!("events/action-started-timed-out-v1.json"),
    fixture!("events/error-v1.json"),
    fixture!("events/media-frame-captured-v1.json"),
    fixture!("events/media-stream-ended-v1.json"),
    fixture!("events/media-stream-started-v1.json"),
    fixture!("events/observation-captured-v1.json"),
    fixture!("events/session-ended-v1.json"),
    fixture!("events/session-started-v1.json"),
    fixture!("events/verdict-recorded-v1.json"),
    fixture!("models/action-call-v1.json"),
    fixture!("models/action-definition-protected-v1.json"),
    fixture!("models/action-definition-v1.json"),
    fixture!("models/action-result-v1.json"),
    fixture!("models/clear-element-arguments-v1.json"),
    fixture!("models/clear-element-result-v1.json"),
    fixture!("models/device-info-v1.json"),
    fixture!("models/error-info-v1.json"),
    fixture!("models/find-element-arguments-v1.json"),
    fixture!("models/find-element-result-v1.json"),
    fixture!("models/manual-recording-v1.json"),
    fixture!("models/observation-omitted-v1.json"),
    fixture!("models/observation-v1.json"),
    fixture!("models/set-element-value-arguments-v1.json"),
    fixture!("models/set-element-value-result-v1.json"),
    fixture!("models/tap-element-arguments-v1.json"),
    fixture!("models/tap-element-result-v1.json"),
    fixture!("models/ui-snapshot-v1.json"),
    fixture!("models/wait-for-element-arguments-v1.json"),
    fixture!("models/wait-for-element-result-v1.json"),
    fixture!("rpc/device-capabilities-v1.request.json"),
    fixture!("rpc/device-capabilities-v1.response.json"),
    fixture!("rpc/device-connect-v1.request.json"),
    fixture!("rpc/device-connect-v1.response.json"),
    fixture!("rpc/device-disconnect-v1.request.json"),
    fixture!("rpc/device-disconnect-v1.response.json"),
    fixture!("rpc/device-execute-timeout-v1.json"),
    fixture!("rpc/device-execute-timeout-v1.response.json"),
    fixture!("rpc/device-observe-v1.request.json"),
    fixture!("rpc/device-observe-v1.response.json"),
    fixture!("rpc/device-select-v1.request.json"),
    fixture!("rpc/device-select-v1.response.json"),
    fixture!("rpc/devices-list-v1.request.json"),
    fixture!("rpc/devices-list-v1.response.json"),
    fixture!("rpc/events-clear-v1.request.json"),
    fixture!("rpc/events-clear-v1.response.json"),
    fixture!("rpc/events-list-v1.request.json"),
    fixture!("rpc/events-list-v1.response.json"),
    fixture!("rpc/failure-v1.json"),
    fixture!("rpc/media-stream-capture-v1.request.json"),
    fixture!("rpc/media-stream-capture-v1.response.json"),
    fixture!("rpc/media-stream-end-v1.request.json"),
    fixture!("rpc/media-stream-end-v1.response.json"),
    fixture!("rpc/media-stream-start-v1.request.json"),
    fixture!("rpc/media-stream-start-v1.response.json"),
    fixture!("rpc/request-cancel-v1.request.json"),
    fixture!("rpc/request-cancel-v1.response.json"),
    fixture!("rpc/session-current-v1.request.json"),
    fixture!("rpc/session-current-v1.response.json"),
    fixture!("rpc/session-end-v1.request.json"),
    fixture!("rpc/session-end-v1.response.json"),
    fixture!("rpc/session-export-v1.request.json"),
    fixture!("rpc/session-export-v1.response.json"),
    fixture!("rpc/session-start-v1.request.json"),
    fixture!("rpc/session-start-v1.response.json"),
    fixture!("rpc/sessions-list-v1.request.json"),
    fixture!("rpc/sessions-list-v1.response.json"),
    fixture!("rpc/system-describe-v1.request.json"),
    fixture!("rpc/system-describe-v1.response.json"),
    fixture!("rpc/ui-snapshot-get-v1.request.json"),
    fixture!("rpc/ui-snapshot-get-v1.response.json"),
    fixture!("rpc/verdict-record-v1.request.json"),
    fixture!("rpc/verdict-record-v1.response.json"),
    fixture!("stream/events-stream-event-v1.notification.json"),
    fixture!("stream/events-stream-open-v1.request.json"),
    fixture!("stream/events-stream-open-v1.response.json"),
    fixture!("stream/events-stream-terminal-v1.notification.json"),
    fixture!("stream/events-subscribe-v1.request.json"),
    fixture!("stream/events-subscribe-v1.response.json"),
    fixture!("stream/websocket-system-hello-v1.request.json"),
    fixture!("stream/websocket-system-hello-v1.response.json"),
    fixture!("system-hello-v1.request.json"),
    fixture!("system-hello-v1.response.json"),
];

/// Returns the exact canonical JSON for a manifest-relative fixture path.
#[must_use]
pub fn fixture_json(path: &str) -> Option<&'static str> {
    CANONICAL_FIXTURES
        .iter()
        .find(|fixture| fixture.path == path)
        .map(|fixture| fixture.json)
}
