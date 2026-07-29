# DeviceRail iOS Host

`devicerail-ios-host` owns repeatable macOS-side lifecycle tasks for the iOS
backends. It is intentionally separate from `devicerail-ios-webdriver`: the
Host represents targets as `IosDeviceKind::Physical` or
`IosDeviceKind::Simulator`, discovers both kinds, can build/supervise Direct WDA
with the appropriate transport, and can supervise an operator-selected Appium
executable. The Driver continues to expose the bounded device-operation
contract.

## Doctor

```sh
cargo run -p devicerail-daemon -- ios doctor
cargo run -p devicerail-ios-host --bin devicerail-ios -- doctor
cargo run -p devicerail-ios-host --bin devicerail-ios -- doctor --json
```

The daemon subcommand is the packaged surface; `devicerail-ios` is the
source-workspace helper for preparing or serving WDA directly.

For Direct WDA, doctor checks Xcode, `devicectl`/`xcdevice`, `simctl`, the
selected `WebDriverAgent.xcodeproj`, and an optional configured WDA endpoint.
For a physical target it also checks `iproxy`, code signing, pairing, Developer
Mode, developer services, and the UI Automation confirmation. A booted,
available Simulator skips those physical-device requirements.

When `DEVICERAIL_IOS_BACKEND=appium`, the packaged daemon doctor also checks
the selected Appium server mode. An external
`DEVICERAIL_IOS_APPIUM_ENDPOINT` is probed in place. With
`DEVICERAIL_IOS_APPIUM_PATH`, doctor runs the fixed `--version` command,
starts a temporary numeric-loopback server with fixed arguments, waits for
`/status` under the configured base path, and shuts the child down. Reports
contain stable error codes and never include the executable path or endpoint.
When Appium has no DeviceRail-managed WDA project, doctor intentionally skips
project/signing/`iproxy` checks that DeviceRail does not own; this covers both
XCUITest Driver's bundled WDA and an operator-managed external WDA endpoint.
Appium `/status` readiness does not prove that XCUITest Driver is installed or
can create a device Session; that boundary is exercised by `device.connect`.

## Discovery and selection

Managed discovery merges a bounded `xcrun devicectl` physical-device inventory
with `xcrun simctl list --json`. If `devicectl` is unavailable, `xcdevice` is
the physical-device fallback; Simulator inventory still comes from `simctl`.
Only available iOS runtimes are included. A Simulator is marked `connected`
only while its state is `Booted`; a shutdown Simulator remains inventory, not a
ready target.

`DEVICERAIL_IOS_DEVICE_TOKEN` is an optional explicit UDID and may select either
kind. Without it, selection first requires exactly one connected physical
device. Only when no physical device is connected does it require exactly one
booted Simulator. Multiple eligible targets of the selected class fail with an
explicit selection error. DeviceRail does not create, clone, boot, or shut down
a Simulator; the operator or another tool owns that lifecycle.

## Direct WDA lifecycle

```sh
export DEVICERAIL_IOS_WDA_PROJECT=/path/to/appium-webdriveragent/WebDriverAgent.xcodeproj
cargo run -p devicerail-ios-host --bin devicerail-ios -- prepare
cargo run -p devicerail-ios-host --bin devicerail-ios -- serve
```

The standalone source-workspace helper requires a WDA project. An explicit
`DEVICERAIL_IOS_WDA_PROJECT` always wins; otherwise it performs read-only
lookup in the installed XCUITest Driver: first
`$APPIUM_HOME/node_modules/appium-xcuitest-driver/node_modules/appium-webdriveragent`,
then the same path under `~/.appium`, then the current project's
`node_modules`. Old standalone Git checkout guesses are not used, and lookup
never invokes `open-wda` or launches Xcode. The stock Appium backend does not
require this path unless an operator explicitly chooses to attach Appium to a
DeviceRail-managed WDA; by default, XCUITest Driver manages its bundled WDA.

`prepare` applies the selection policy above and runs a cached `xcodebuild
build-for-testing`. Physical devices reuse the current Xcode project's signing
configuration, while Simulators require no device signing.
`serve` uses numeric-loopback `iproxy` for a physical device. For a Simulator,
WDA and the Host share the selected loopback `DEVICERAIL_IOS_WDA_LOCAL_PORT` and
no tunnel is started. Both paths run `xcodebuild test-without-building`, wait
for WDA `/status`, and restart owned processes after bounded health failures.
The stock daemon's `auto` policy keeps a cancellable discovery supervisor when
no target is initially usable, so a later USB hot-plug or an operator-initiated
Simulator boot can publish the route without restarting the daemon. Once
published, recovery is pinned to the original UDID: temporary unplug,
Simulator shutdown, WDA exit, tunnel loss, or stale build output cannot
silently switch the route to another target. Recovery revalidates the
device and cached build before relaunch and periodically forces a rebuild.
The cache fingerprint includes Xcode, project, device, Git HEAD, and tracked
working-tree changes, so an intentional tracked signing edit can still reuse
its exact build. Untracked source or an unavailable Git fingerprint disables
reuse rather than risking a stale binary. An exclusive DerivedData lock keeps
another `prepare`, `serve`, or daemon process from rebuilding a running WDA.

Optional settings:

- `DEVICERAIL_IOS_DERIVED_DATA`
- `DEVICERAIL_IOS_IPROXY_PATH` (physical targets only)
- `DEVICERAIL_IOS_WDA_LOCAL_PORT` (`0` selects an available loopback port)
- `DEVICERAIL_IOS_WDA_REMOTE_PORT` (physical targets only; default `8100`)
- `DEVICERAIL_IOS_ALLOW_PROVISIONING_UPDATES=true|false` (physical targets only)

The last option is off by default: DeviceRail never silently grants Xcode
permission to modify the developer account or register a device. For physical
targets, first-time host trust, Developer Mode/restart, UI Automation, and
developer-certificate trust remain user-confirmed Apple security boundaries;
none is required for a Simulator.

## Optional managed Appium server

DeviceRail can own the local Appium process while Appium's XCUITest Driver
continues to own the W3C Session:

```sh
appium driver install xcuitest
export DEVICERAIL_IOS_BACKEND=appium
export DEVICERAIL_IOS_APPIUM_PATH=/absolute/path/to/appium
# Optional: 0 selects an available numeric-loopback port.
export DEVICERAIL_IOS_APPIUM_PORT=0
export DEVICERAIL_IOS_APPIUM_BASE_PATH=/
```

`DEVICERAIL_IOS_APPIUM_PATH` and `DEVICERAIL_IOS_APPIUM_ENDPOINT` are mutually
exclusive. The daemon supplies only `--address 127.0.0.1`, `--port`, and
`--base-path`; no shell, arbitrary Appium arguments, capabilities, credentials,
downloads, or installation are accepted. Startup has a 30-second readiness
deadline, detects an early child exit, and fails closed. The daemon retains
the child for its full lifetime. After route publication, an unexpected Appium
exit is a fatal daemon runtime error rather than leaving a dead route
advertised. On Unix Appium runs in a dedicated process group so bounded
TERM-to-KILL cleanup also covers descendants. Shutdown terminates the owned
group before returning.
The optional port defaults to `0` and the base path defaults to `/`.
`/status` readiness proves the Appium server is accepting requests, not that
the XCUITest extension is installed; a missing/incompatible extension remains
an explicit `session not created` error on `device.connect`.

With `DEVICERAIL_IOS=auto|required` and no managed WDA project, the stock Appium
path uses the same physical/Simulator discovery and selection policy. XCUITest
Driver's bundled WDA supports either selected kind; DeviceRail does not add
signing, trust, Developer Mode, or `iproxy` work for a Simulator. Set
`DEVICERAIL_IOS_SESSION_TARGET=native|safari` to select the initial W3C Session
target. It defaults to `native`; `safari` is Appium-only.
