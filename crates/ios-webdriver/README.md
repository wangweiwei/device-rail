# DeviceRail iOS WebDriver Driver

`devicerail-ios-webdriver` implements the DeviceRail `DeviceDriver` boundary
for iOS through one of two mutually exclusive automation backends:

- direct WebDriverAgent (WDA), with an optional WDA MJPEG screenshot stream;
- Appium XCUITest Driver, with native accessibility and Safari/WebView DOM
  semantic channels under one W3C Session.

The Driver crate deliberately does **not** launch WDA or Appium, pair a device,
create an `iproxy` tunnel, or discover physical devices/Simulators. Its host
application owns that lifecycle and supplies either:

- a stable `IosDeviceConfig` identity;
- an explicit `http://` WDA endpoint through `HttpEndpointConfig`;
- optionally, an explicit MJPEG endpoint; and
- injectable `WdaTransport` / `MjpegFrameSource` implementations when the
  built-in clear-text local HTTP adapters are not appropriate.

or a validated clear-text HTTP Appium endpoint, a bounded
`AppiumSessionRequest`, and an injectable `AppiumTransport`. Embedding
applications choose their own authenticated tunnel boundary; the stock daemon
separately restricts this endpoint to numeric loopback. Direct WDA and Appium
are separate Driver types; they never share or concurrently own an automation
Session for one route.

The built-in transport exposes a closed typed WDA surface: status, session
create/delete, source and viewport inspection, PNG screenshot capture, tap,
text/key input, and coordinate drag. It has no arbitrary URL or JSON escape
hatch. HTTP headers, bodies, source XML, screenshots, MJPEG boundaries, JPEG
dimensions, device identifiers, and timeouts are bounded. Core cancellation
drops socket I/O directly; no detached network task is created.

`AppiumIosDriver` itself uses a fixed bounded operation set: status, Session
lifecycle, context discovery/switching, native source, DOM extraction, W3C
element interaction, viewport, screenshot, coordinate gestures, key input,
and typed button presses. The public crate-level injection API also retains
bounded `execute_script` and additional-capability extension points for trusted
embedding applications; these are not DeviceRail wire operations. The stock
daemon does not expose either escape hatch and constructs its capability set
only through typed builders.

Protocol 1.5's five canonical semantic Actions (`findElement`, `tapElement`,
`clearElement`, `setElementValue`, and `waitForElement`) are advertised only by
`AppiumIosDriver`. Native contexts use a normalized WDA accessibility tree;
Safari/WebView contexts use DOM and W3C element semantics. Both emit bounded
UI Snapshot Evidence with stable node ids, context/epoch provenance, and an
explicit `NativeSemantic` or `WebSemantic` execution channel. A stale
`UiNodeRef` fails explicitly. Semantic actions require operation-scoped UI
Snapshot support, and never silently guess a coordinate. `waitForElement`
inherits a caller deadline or uses a bounded 10-second default.

Safari/WebView observations capture XCUITest's typed WebKit viewport screenshot
instead of Appium's full-display screenshot or MJPEG stream. The latter include
Safari chrome and cannot be mapped to DOM CSS bounds with one scale factor. The
Driver preserves CSS viewport dimensions, derives the screenshot scale only
after strict uniform-geometry validation, and fails explicitly on a mismatch.

Password, secure, and one-time-code fields and their sensitive subtrees are
redacted from native and Web trees.
`setElementValue` is a Protected Action: its value, screenshots, UI Snapshot
body, and raw output are not persisted. A Session is recreated only after
explicit invalid-session evidence or a confirmed delete; an ambiguous delete
failure retains ownership instead of creating a second Session.

## Example wiring

```rust,no_run
use std::sync::Arc;
use devicerail_ios_webdriver::{
    HttpEndpointConfig, IosDeviceConfig, IosDriver, SystemMjpegFrameSource,
    SystemWdaTransport,
};

let device = IosDeviceConfig::new("00008030-001", "Test iPhone", None)?;
let wda = Arc::new(SystemWdaTransport::new(
    HttpEndpointConfig::new("http://127.0.0.1:8100")?,
));
let mjpeg = Arc::new(SystemMjpegFrameSource::new(
    HttpEndpointConfig::new("http://127.0.0.1:9100")?,
));
let driver = IosDriver::new(device, wda).with_mjpeg(mjpeg);
# Ok::<(), devicerail_core::DriverError>(())
```

Appium XCUITest with its installed bundled WDA does not require a separate WDA
endpoint:

```rust,no_run
use std::sync::Arc;
use devicerail_ios_webdriver::{
    AppiumIosDriver, AppiumSessionRequest, HttpEndpointConfig, IosDeviceConfig,
    SystemAppiumTransport,
};

let device = IosDeviceConfig::new("00008030-001", "Test iPhone", None)?;
let transport = Arc::new(SystemAppiumTransport::new(
    HttpEndpointConfig::new("http://127.0.0.1:4723")?,
));
let request = AppiumSessionRequest::safari("00008030-001")?;
let driver = AppiumIosDriver::new(device, transport, request);
# Ok::<(), devicerail_core::DriverError>(())
```

When MJPEG is configured, a malformed/unavailable stream is an explicit
Driver failure; the driver does not silently fall back to WDA screenshots.
Without MJPEG, observations use WDA's `/screenshot` PNG response. Page source
is captured through `/source` and attached to Observation metadata as bounded
XML. Screenshot bytes are persisted only through Core's operation-scoped
Evidence Store capability.

## Stock daemon wiring

External direct-WDA mode registers one lazy iOS route only when both
`DEVICERAIL_IOS_WDA_ENDPOINT` and `DEVICERAIL_IOS_DEVICE_TOKEN` are present.
`DEVICERAIL_IOS_DEVICE_NAME`, `DEVICERAIL_IOS_OS_VERSION`, and
`DEVICERAIL_IOS_MJPEG_ENDPOINT` are optional. Any orphan auxiliary setting is
a startup error. The built-in daemon path restricts WDA and MJPEG to numeric
loopback HTTP endpoints, redacts endpoints and stable device identity from the
iOS adapter's public `Debug` implementations and stock daemon startup
configuration diagnostics, and raises
the transport ceiling to 65 seconds so the advertised 60-second drag remains
executable. This diagnostic boundary does not hide the route identity from the
intentional wire inventory. Registration performs no HTTP request; WDA
readiness is checked by the ordinary Driver lifecycle.

```sh
DEVICERAIL_ANDROID=off \
DEVICERAIL_IOS_WDA_ENDPOINT=http://127.0.0.1:8100 \
DEVICERAIL_IOS_DEVICE_TOKEN=00008030-001 \
DEVICERAIL_IOS_DEVICE_NAME="Test iPhone" \
cargo run -p devicerail-daemon
```

The operator starts WDA and any `iproxy` tunnel in external mode. The daemon
does not silently use remote clear-text HTTP; applications that require a
different authenticated transport continue to inject the transport traits.

For Appium, select `DEVICERAIL_IOS_BACKEND=appium` and either supply an
operator-started numeric-loopback `DEVICERAIL_IOS_APPIUM_ENDPOINT` or a
caller-selected `DEVICERAIL_IOS_APPIUM_PATH` for the daemon to supervise. The
WDA endpoint is optional; when absent, DeviceRail omits
`appium:webDriverAgentUrl` and XCUITest Driver manages its installed bundled
WDA for either a physical device or an iOS Simulator. The daemon accepts no
arbitrary Appium argv or capability JSON.

```sh
DEVICERAIL_ANDROID=off \
DEVICERAIL_IOS_BACKEND=appium \
DEVICERAIL_IOS_SESSION_TARGET=safari \
DEVICERAIL_IOS_APPIUM_ENDPOINT=http://127.0.0.1:4723 \
DEVICERAIL_IOS_DEVICE_TOKEN=00008030-001 \
cargo run -p devicerail-daemon
```

`DEVICERAIL_IOS_SESSION_TARGET=native|safari` defaults to `native`. `safari`
selects an Appium XCUITest Safari Session and is rejected for Direct WDA.
Creating the first Appium Session has an independent 300-second adapter timeout
because XCUITest may need to build and install WDA. All later Appium commands
retain the endpoint request timeout. A shorter caller deadline or cancellation
still wins; after a Session request reaches the socket, an interruption is
reported as an unknown command outcome rather than being retried automatically.

On macOS, managed Direct WDA moves the repeatable WDA lifecycle to the separate
`devicerail-ios-host` crate while retaining this exact Driver boundary:

```sh
DEVICERAIL_ANDROID=off \
DEVICERAIL_IOS=required \
DEVICERAIL_IOS_WDA_PROJECT=/path/to/appium-webdriveragent/WebDriverAgent.xcodeproj \
cargo run -p devicerail-daemon
```

Managed discovery merges physical devices from `devicectl`/`xcdevice` with
available Simulators from `simctl`; only a `Booted` Simulator is connected.
Without an explicit UDID, the unique connected physical device is preferred;
only when none is connected does the unique booted Simulator become eligible.
DeviceRail does not boot Simulators.

Managed Direct WDA performs a fingerprinted cached `build-for-testing`, launches
`test-without-building`, and waits for `/status`. A physical target uses
signing/trust/Developer Mode gates and loopback `iproxy`. A Simulator skips
those physical-device steps and makes WDA listen directly on the selected Host
port. `auto` preserves other routes after a Host failure; `required` rejects
daemon startup. Run
`cargo run -p devicerail-daemon -- ios doctor` first for structured
remediation. Apple trust, Developer Mode/restart, UI Automation, and
certificate trust remain user-confirmed physical-device security boundaries.

With `DEVICERAIL_IOS_BACKEND=appium`, the same `auto|required` switch can
perform physical-device/Simulator discovery without a WDA project. XCUITest
Driver then owns its bundled WDA for either target kind. Appium server ownership
remains independently external through `DEVICERAIL_IOS_APPIUM_ENDPOINT` or
daemon-supervised through `DEVICERAIL_IOS_APPIUM_PATH`; neither path implies a
DeviceRail WDA build or `iproxy` process.

## Verification

After registering this crate in the root workspace:

```text
cargo test -p devicerail-ios-webdriver
cargo clippy -p devicerail-ios-webdriver --all-targets -- -D warnings
```

The test targets invoke `driver_conformance_test!` for both backends with
injectable fake WDA/Appium transports and a temporary real Evidence Store.
They cover both native and Web semantic paths, all five canonical Actions,
sensitive-value redaction, provenance/session recovery, and bounded waits;
they never access a real iOS device or an external/operator Appium/WDA service.
System-transport tests use only bounded loopback fake endpoints.
