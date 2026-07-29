DeviceRail portable distribution
================================

This archive contains the DeviceRail daemon and offline Session Bundle CLI.
Run the platform install script with an optional absolute destination prefix,
or copy both files from bin/ into a directory on PATH.

Before installation, run the repository verifier described in
packaging/README.md. SHA-256 proves integrity only. A distribution is an
authenticated release only when release-manifest.json says "signed", the
detached archive signature verifies, and every payload signature verifies.

An archive whose name includes UNSIGNED and whose manifest says
"unsigned-test-only" is a CI/test artifact. It must never be represented as a
signed or production release.

The daemon uses stdio by default. See config/devicerail.env.example for the
closed set of optional local settings. The daemon does not automatically load
that file; the host shell, service manager, container runtime, or Windows
service configuration must inject the selected variables.

Android, Playwright, and RDP integrations require their separately installed
host tools or bridges. Direct WDA iOS requires a stable device token and a
numeric-loopback WebDriverAgent (WDA) endpoint, or
DEVICERAIL_IOS=auto|required plus an explicit WebDriverAgent.xcodeproj for
cached xcodebuild, readiness, and recovery. Managed discovery merges physical
devices with available iOS Simulators; only a booted Simulator is connected.
An explicit DEVICERAIL_IOS_DEVICE_TOKEN may select either kind. Otherwise the
unique connected physical device is preferred, falling back to the unique
booted Simulator. DeviceRail does not create or boot Simulators.

A physical Direct-WDA target additionally uses signing/trust/Developer Mode
checks and numeric-loopback iproxy. A Simulator skips those requirements and
shares the selected host-local WDA port directly. The Appium backend instead
requires an operator-installed Appium/XCUITest Driver and either a
numeric-loopback endpoint or an explicitly selected executable; XCUITest
Driver normally manages its bundled WDA for either target kind.
DEVICERAIL_IOS_SESSION_TARGET=native|safari defaults to native; safari requires
the Appium backend. DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS defaults
to 600 and accepts only 1..=3600; it is emitted as appium:newCommandTimeout and
prevents Appium's 60-second default idle cleanup during operator or Agent
pauses. Run `devicerail-daemon ios doctor` first. DeviceRail does
not store Apple credentials or bypass physical-device security boundaries. In
managed `auto`, a later physical-device connection or operator-initiated
Simulator boot may publish a route without restarting the daemon. The route
stays pinned to that target's UDID during recovery.

HarmonyOS HDC discovery is disabled by default and requires a separately
installed HDC executable. DEVICERAIL_HARMONY=auto attempts one startup discovery
but preserves other routes after HDC failure or an empty inventory;
DEVICERAIL_HARMONY=required makes initialization, discovery, an empty inventory,
or registration conflict a daemon startup error. The daemon never installs HDC;
when enabled it invokes only the operator-selected executable.

Native Desktop control is disabled by default. DEVICERAIL_DESKTOP=auto attempts
to register one lazy route for the daemon binary's compile-time host and keeps
the other routes after native discovery or registration failure;
DEVICERAIL_DESKTOP=required makes that failure fatal. A single daemon does not
register macOS, Windows, and Linux routes together. Inventory performs no
screen capture or input. Connect and health checks perform host-specific
profile, permission, and viewport probes; observations and Actions exercise
capture and input tools. The daemon does not install tools, change permissions,
or create a GUI session.

On macOS, Screen Recording and Accessibility (TCC) access must be granted to
the devicerail-daemon executable itself, not only to the terminal that launches
it. The default capture tool is /usr/sbin/screencapture. On Windows, Windows
PowerShell and an interactive user session are required; a daemon running as a
Session 0 service cannot capture or control a logged-in user's desktop.

Linux X11 requires ImageMagick import, xdotool, and access to the matching
DISPLAY and XAUTHORITY. Wayland requires grim, WAYLAND_DISPLAY,
XDG_RUNTIME_DIR, an explicit DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=wayland,
and all three physical-pixel viewport settings. Leaving display-server
selection unset cannot bypass that contract. Input uses either ydotool or the
smaller keyboard/text-only wtype profile. ydotool additionally requires an
operator-managed ydotoold with access to /dev/uinput. DeviceRail does not start
the X server, Wayland compositor, ydotoold, or any of these host tools.

Distributed routing is disabled unless an owner-only JSON path is injected.
DEVICERAIL_DISTRIBUTED_PEERS declares mandatory outbound nodes behind numeric
loopback SSH/mTLS tunnel terminations. DEVICERAIL_DISTRIBUTED_SERVER declares
one numeric-loopback, non-zero-port stock peer-v2 listener with the closed
schemaVersion/nodeId/listen/securityMode/tunnelId/nodeEpoch/inventoryRevision
contract. securityMode must be externalSshOrMtls. Both files use Unix
owner/mode/no-follow/ACL checks. Non-Unix owner-only configuration currently
fails closed because equivalent checks cannot be proved.

After local route registration, the inbound listener is bound before outbound
discovery to avoid two stock daemons waiting on each other's startup. During
that starting gate, hello/inventory/health/capabilities remain available while
lease and mutation operations return retryable node_starting. Successful
outbound registration marks the service ready. Its bind diagnostic reports
only socket reservation, not the ready transition. A later startup failure and
normal EOF both close the listener and converge admitted peer work. It exports
only the daemon's non-remote startup inventory, not imported remote routes.

Raw loopback TCP has no built-in authentication. DEVICERAIL_RPC_CREDENTIALS
protects only the separate JSON-RPC listener and does not authenticate peer-v2.
The operator must establish and isolate the SSH/mTLS tunnel, manage its
identity and lifecycle, and advance nodeEpoch/inventoryRevision when required.
The archive installs no tunnel, TLS stack, certificate, public listener,
firewall rule, consensus service, or telemetry exporter.

The included tests use deterministic backends and bounded host-tool fixtures.
They do not claim real macOS TCC, interactive Windows, X server, Wayland
compositor, ydotoold, /dev/uinput, DPI, multi-monitor laboratory E2E, or real
SSH/mTLS and cross-host network validation.
