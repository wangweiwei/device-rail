<p align="center">
  <img src="docs/assets/devicerail-logo.png" alt="DeviceRail device automation infrastructure logo" width="240">
</p>

# DeviceRail

**Open-source, language-neutral device automation and test-evidence infrastructure.**

[简体中文](README.zh-CN.md) · English

![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)
![Rust 1.85+](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)
![Node.js 22+](https://img.shields.io/badge/Node.js-22%2B-339933.svg)
![Python 3.11+](https://img.shields.io/badge/Python-3.11%2B-3776AB.svg)

DeviceRail gives test runners, developer tools, and AI agents one bounded
JSON-RPC interface for Android, iOS, HarmonyOS, macOS, Windows, Linux, RDP,
and Playwright. It records sequence-authoritative events and content-addressed
evidence so the same run can be streamed, replayed, validated, and reviewed
offline.

> **Project status:** `0.1.0` alpha. The protocol and deterministic test suite
> are implemented; production deployments must still validate their exact
> devices, operating systems, permissions, bridges, and signing environment.

**Documentation:** [Get started](#quick-start) ·
[Architecture](docs/architecture.md) · [Project structure](docs/project-structure.md) ·
[Platform setup](docs/platform-support.md) · [Performance](docs/performance.md) ·
[Documentation index](docs/README.md) ·
[Roadmap](ROADMAP.md) · [Contributing](CONTRIBUTING.md)

## Why DeviceRail

- **One protocol across devices:** discover, select, observe, execute, stream,
  and export through versioned, generated wire contracts.
- **Evidence by design:** screenshots and media use SHA-256 references instead
  of embedding large binary payloads in JSON.
- **Driver conformance:** every platform Driver runs the same lifecycle,
  capability, observation, action, error, and evidence contract suite.
- **AI-provider neutral:** the kernel has no model SDK, prompt, planner, YAML,
  recorder UI, or visualizer UI dependency.
- **Fail-closed boundaries:** bounded frames, explicit errors, protected
  actions, cancellation, leases, and loopback-only built-in remote transports.
- **Cross-language contracts:** the Rust client reuses the canonical Rust DTOs;
  the same DTOs generate JSON Schema, TypeScript types, Python types, and
  shared Golden Fixtures.

## Supported targets

| Target | Built-in integration | Host dependency | Validation scope |
|---|---|---|---|
| Android | ADB Driver | Android Platform Tools | Conformance + deterministic daemon E2E |
| iOS | Direct WDA or Appium XCUITest Driver + Host Supervisor | Xcode; Appium/XCUITest Driver for Appium mode; a WDA project for Direct WDA; `iproxy` only for a physical Direct-WDA target | Conformance + deterministic daemon lifecycle E2E; one historical Direct-WDA real-device smoke path; no device/version matrix |
| HarmonyOS | HDC Driver | DevEco/HDC | Conformance + deterministic daemon E2E |
| macOS | Native Desktop Driver | Screen Recording and Accessibility permissions | Conformance + host inventory |
| Windows | Native Desktop Driver | Interactive desktop session | Conformance + CI build/inventory |
| Linux | X11/Wayland Desktop Driver | Explicit capture/input tools | Conformance + fake host tools |
| Web | Playwright Remote Driver | Existing compatible Playwright server | Conformance + bounded bridge tests |
| RDP | RDP Remote Driver | Operator-managed loopback bridge | Conformance + loopback framing tests |

DeviceRail does not install platform SDKs, embed an RDP stack, or download a
browser. Managed Direct WDA builds and supervises an explicitly selected WDA
project; managed Appium supervises an explicitly selected Appium executable
while XCUITest Driver normally owns its installed bundled WDA. Neither mode
stores Apple credentials. Managed discovery supports physical devices and
already-booted iOS Simulators; DeviceRail does not create or boot a Simulator.
See
[platform support](docs/platform-support.md) for exact boundaries.

The Rust kernel deliberately contains **no** AI planner, prompt framework, YAML runtime, recorder UI, or report UI. Optional language-side adapters remain above the public wire boundary. Any AI agent can use the same small protocol to observe a device, inspect its action space, execute actions, and consume evidence.

## Core model

```text
AI / Rust, TypeScript, or Python SDK / CLI
      |
      | JSON-RPC 2.0 control plane over stdio or loopback TCP/NDJSON
      | optional bounded event plane over loopback WebSocket
      v
DeviceRail daemon
      |
      +-- observe()
      +-- capabilities()
      +-- execute(action)
      +-- events()
      |
      +-- built-in Android / iOS / HarmonyOS / native Desktop routes
      +-- built-in RDP / Playwright / plugin / distributed routes
      +-- optional loopback peer-v2 server for exporting local routes
```

## Workspace

- `crates/protocol`: cross-language DTOs and wire protocol.
- `crates/client`: official asynchronous Rust client over bounded NDJSON
  JSON-RPC, with daemon spawn, stdio/TCP attachment, hello negotiation, typed
  calls, and explicitly confirmed/resumable event streams.
- `crates/core`: driver/event contracts, multi-Driver registry, thin execution runtime, and the opt-in Driver conformance suite.
- `crates/android-adb`: conformant host-ADB Android Driver with bounded discovery, lifecycle, Session-pinned observations, and closed typed actions.
- `crates/ios-host`: macOS device doctor, merged physical-device/Simulator
  discovery, transport-specific Direct WDA lifecycle, and optional managed
  Appium process supervisor.
- `crates/ios-webdriver`: conformant Direct WDA/MJPEG and Appium XCUITest iOS
  Drivers, including native/Web semantic channels.
- `crates/harmony-hdc`: conformant typed HDC HarmonyOS Driver and discovery adapter.
- `crates/desktop-driver`: conformant macOS, Windows, X11, and Wayland Drivers with explicit permission/tool detection.
- `crates/rdp-remote`: conformant bridge-backed RDP Driver plus a versioned bridge Schema and Golden Fixtures.
- `crates/plugin-driver`: versioned process-isolated Driver plugin ABI, fail-closed manifest discovery, and supervised executable adapter.
- `crates/remote-auth`: optional HMAC client authentication, closed RPC authorization policy, and durable tamper-evident audit records for the loopback TCP control plane.
- `crates/distributed-router`: opt-in cross-node Driver adapter and peer service with namespaced routes, bounded peer leases, evidence import, telemetry, and a versioned peer protocol.
- `crates/driver-mock`: deterministic Driver that runs the complete shared conformance suite.
- `crates/evidence-fs`: streaming SHA-256 filesystem Evidence Store with durable Session references and conservative GC.
- `crates/session-bundle`: platform-neutral canonical Session Bundle writer and offline validator.
- `crates/bundle-cli`: local `devicerail-bundle` export/validate workflow over a stopped daemon's Evidence Store.
- `crates/visualizer`: read-only offline Bundle viewer with a loopback capability URL and server-rendered timeline.
- `crates/websocket-transport`: capability-scoped, resumable loopback event-stream adapter with bounded per-subscriber backpressure.
- `crates/playwright-remote`: conformant Web Driver proxy over a bounded persistent Node/Playwright bridge.
- `crates/manual-recording`: Driver-neutral validation and deterministic replay compilation for portable human Action recordings.
- `crates/daemon`: newline-delimited request/response RPC server over stdin/stdout or an explicitly enabled loopback TCP listener, plus an independently enabled loopback peer-v2 server.
- `crates/schema-gen`: reproducible JSON Schema generator and stale-output checker.
- `protocol/schema/v1`: generated, versioned JSON Schema for public wire DTOs.
- `crates/protocol/fixtures`: cross-language Golden Fixtures and their manifest.
- `packages/protocol`: generated type-only TypeScript protocol package.
- `packages/client`: typed TypeScript stdio client plus explicitly confirmed/resumable event streams.
- `packages/tool-adapter`: provider-neutral immutable Action/Observation Tool Catalogs over the typed client.
- `packages/recorder`: durable sequence-authoritative Execution Recorder with offline Bundle handoff.
- `packages/live-visualizer`: bounded protocol-only presentation model for live Session events.
- `packages/playwright-driver`: private `playwright-core` bridge that only connects to an operator-provided remote browser.
- `packages/python-client`: Python 3.11+ typed async stdio client generated from the same checked-in protocol Schema.
- `packages/yaml-adapter`: optional bounded YAML-to-public-call compiler; it is never imported by the Rust kernel or daemon.
- `apps/live-visualizer`: private loopback HTTP/SSE host over an already-owned TypeScript client.
- `packaging`: deterministic portable release archives, manifests, SBOM/provenance, signature verification, and installer scripts.
- `apps`: product applications that remain above the client and protocol boundaries.
- `ROADMAP.md`: prioritized feature list and acceptance criteria.

## Quick start

Run the daemon:

```sh
cargo run -p devicerail-daemon
```

The stdio transport accepts one JSON-RPC request per line and writes one response per line. The first successful request on a connection must be `system.hello`:

```jsonl
{"jsonrpc":"2.0","id":"hello-1","method":"system.hello","params":{"client":{"name":"example-client","version":"0.1.0"},"protocol":{"ranges":[{"major":1,"minMinor":0,"maxMinor":5}]},"features":{"required":[],"optional":["action.protected.v1","device.routing.v1","device.semanticActions.v1","events.snapshot.v1","events.stream.v1","media.stream.v1","observation.uiSnapshot.v1","request.control.v1","verdict.record.v1"]}}}
{"jsonrpc":"2.0","id":"devices-1","method":"devices.list","params":{}}
{"jsonrpc":"2.0","id":"select-1","method":"device.select","params":{"deviceId":"mock-1"}}
{"jsonrpc":"2.0","id":"connect-1","method":"device.connect","params":{}}
{"jsonrpc":"2.0","id":"capabilities-1","method":"device.capabilities","params":{}}
{"jsonrpc":"2.0","id":"session-1","method":"session.start","params":{}}
{"jsonrpc":"2.0","id":"observe-1","method":"device.observe","params":{}}
{"jsonrpc":"2.0","id":"execute-1","method":"device.execute","params":{"id":"3d8e56bd-755d-4d79-a78e-1e495f97b2ca","name":"tap","arguments":{"x":320,"y":240}}}
{"jsonrpc":"2.0","id":"events-1","method":"events.list","params":{"limit":1000}}
{"jsonrpc":"2.0","id":"session-2","method":"session.end","params":{"outcome":"completed"}}
```

`system.hello` negotiates the highest common `{major, minor}` protocol version and the connection features before device methods are available. The handshake only establishes the protocol connection; it does not discover, connect to, or select a device. Protocol 1.2 clients that negotiate `device.routing.v1` can list devices and keep an independent selection per connection. A legacy client remains compatible when exactly one device is registered; with multiple devices it must upgrade to protocol 1.2, negotiate routing, and select one explicitly before device calls. Observation and execution require an active Session so their append-only events can be correlated and replayed.

Protected actions are fail-closed behind `action.protected.v1`. Without that negotiated Feature, the daemon hides them from `device.capabilities` and rejects direct execution. The Tool Adapter also hides them by default; a host must explicitly opt in after negotiating the Feature. Android `inputSecret` uses this path, records redacted arguments, sends its value to a fixed ADB command through child stdin rather than the host process argv, and does not capture before/after screenshots.

Protocol 1.5 defines a provider-neutral UI Snapshot and semantic element
contract for native accessibility trees and web contexts. UI Trees are typed,
Session-owned Evidence and are read online only through
`ui.snapshot.get({ observationId })`; the RPC never accepts an arbitrary asset
or Session ID. `findElement`, `tapElement`, `clearElement`,
`setElementValue`, and `waitForElement` remain ordinary advertised Driver
Actions, while `verdict.record` only persists a caller-supplied
`pass|fail|unknown` result and its already Session-owned Evidence. A Driver does
not claim semantic support merely because the connection understands the 1.5
wire types. Core enables the additive UI/execution fields per operation only
after Feature negotiation; older connections retain the Protocol 1.0–1.4 wire
shape instead of receiving unknown 1.5 fields.

The iOS Appium backend implements all five Actions through a normalized WDA
accessibility tree in native context and DOM/W3C element semantics in Safari or
WebView context. Node references are bound to the returned Observation,
context, document epoch, and stable identity; coordinates are never an implicit
semantic fallback. `setElementValue` is Protected so its value, screenshots,
and UI Tree body do not enter the Session event/evidence record.

### Official clients

The official Rust, TypeScript, and Python clients use the same public wire
contract. Add the Rust client from crates.io:

```sh
cargo add devicerail-client
```

```rust
use devicerail_client::{
    CallOptions, DeviceRailClient, SpawnConfig, default_hello, methods,
};

async fn list_devices() -> Result<(), devicerail_client::ClientError> {
    let client = DeviceRailClient::spawn(SpawnConfig::new(
        "devicerail-daemon",
        default_hello(),
    ))
    .await?;
    let devices = client
        .call::<methods::DevicesList>(methods::NoParams, CallOptions::default())
        .await?;
    println!("{:?}", devices.devices);
    client.close().await?;
    Ok(())
}
```

`spawn` owns a daemon child over stdio. `attach` accepts caller-owned
asynchronous read/write halves, while `connect_tcp` connects to an explicitly
enabled non-zero IPv4/IPv6 loopback TCP listener and rejects all remote
addresses before opening a socket. `attach` remains the caller-owned transport
escape hatch; all three complete `system.hello` before returning. The Rust
client directly uses `devicerail-protocol` request/result DTOs instead of
maintaining another wire model. Its built-in TCP path currently does not
implement the optional `remote-auth` HMAC pre-hello exchange, so it cannot
directly attach to a listener configured with
`DEVICERAIL_RPC_CREDENTIALS`.

See the [Rust client](crates/client/README.md),
[TypeScript client](packages/client/README.md), and
[Python client](packages/python-client/README.md) guides.

## Live event streams

Protocol 1.3 adds the optional `events.stream.v1` data plane without changing
the stdio control plane. After negotiation, `events.stream.open` returns a
short-lived, single-use, Session-scoped `ws://127.0.0.1` capability. The socket
performs its own `system.hello` at the exact control-connection protocol version,
then one `events.subscribe`, and
emits typed event and terminal notifications. The daemon never writes stream
traffic to stdout or stderr.

The TypeScript client exposes `openEventStream()` as an async iterator. The
Rust client exposes `open_event_stream()`, `next()`, and an explicit
`confirm(&cursor)` operation; after a finished stream, `resume()` opens a fresh
single-use capability from only its confirmed cursor. Both clients distinguish
socket receipt, application delivery, and application confirmation. Core
registers the bounded live tail at the same linearization point as its replay
snapshot, so every subscriber sees a continuous sequence or an explicit typed
failure. Loopback bind failure leaves older control-plane clients usable and
simply omits the optional stream Feature.

Protocol 1.4's optional `media.stream.v1` adds ordered screenshot/video stream
lifecycle events and three typed control methods. After starting a Session on a
selected, leased device, a client can use the production path directly:

```ts
const streamId = crypto.randomUUID();
await client.call("media.stream.start", { kind: "screenshot", streamId });
const { frame } = await client.call(
  "media.stream.capture",
  { frameIndex: 1, streamId },
  { timeoutMs: 15_000 },
);
await client.call("media.stream.end", { streamId });
```

`media.stream.capture` invokes the selected Driver's Observation path and
accepts no bytes, paths, or client-provided Evidence reference. The explicit
one-based `frameIndex` makes an exact retry idempotent if a response is lost.
For `kind: "video"`, each capture requires a positive `durationMs`; the result
is a timed sequence of independent PNG key frames, not an encoded video
container.

`MediaStreamWriter` is prepared and retained before its start event is
published, so a lost start acknowledgement can be retried with the exact
original correlation. It attaches each frame through the Session's Evidence
Store, then appends only its canonical reference. Once Evidence publication
starts, frame finalization is cancellation-shielded; transient frame or
terminal append failures remain pending for idempotent recovery. Recorder,
Bundle validation, offline reports, and live presentation all reject missing,
out-of-order, media-type-changing, or unterminated streams.

## Daemon configuration

The daemon creates one process-wide filesystem Evidence Store and shares it
across every device route. Its local startup settings are environment
variables:

- `DEVICERAIL_RPC_LISTEN=127.0.0.1:PORT` switches the control plane from stdio
  to one loopback-only multi-client TCP lease authority. Each TCP connection
  keeps independent handshake, selection, Session, cancellation, and request
  state; all connections share the same Driver Registry and Device Pool.
- `DEVICERAIL_RPC_CREDENTIALS` and `DEVICERAIL_RPC_AUDIT_LOG` together enable
  the optional pre-hello HMAC authentication gate and durable authorization
  admission audit. Both paths must satisfy the owner-only file contract; the
  listener remains loopback-only and cross-host use still requires an
  authenticated SSH or mTLS tunnel. See
  [`crates/remote-auth/README.md`](crates/remote-auth/README.md).

- `DEVICERAIL_EVIDENCE_DIR` selects the Store root; the default is
  `.devicerail/evidence` under the current directory.
- `DEVICERAIL_ANDROID=auto|off|required` controls startup ADB discovery. The
  default `auto` keeps the Mock route available when ADB is absent or no
  stable device is found; `off` never starts ADB; `required` fails startup on
  discovery failure or an empty stable-device set.
- `DEVICERAIL_ADB_PATH` selects the ADB executable and defaults to `adb`.
- `DEVICERAIL_IOS_WDA_ENDPOINT=http://127.0.0.1:PORT[/BASE]` explicitly
  enables one iOS route and requires `DEVICERAIL_IOS_DEVICE_TOKEN` as its
  stable identity. `DEVICERAIL_IOS_DEVICE_NAME`,
  `DEVICERAIL_IOS_OS_VERSION`, and `DEVICERAIL_IOS_MJPEG_ENDPOINT` are
  optional. The built-in clear-text WDA and MJPEG transports accept only
  numeric IPv4/IPv6 loopback endpoints; `localhost`, credentials, query
  strings, fragments, TLS, and non-loopback hosts fail closed. Registration
  performs no network I/O; the first lifecycle request verifies the
  operator-managed endpoint. This external mode remains backward compatible.
- `DEVICERAIL_IOS_BACKEND=direct-wda|appium` selects the one Session owner for
  the iOS route and defaults to `direct-wda`. `appium` additionally requires
  exactly one server mode: an external
  `DEVICERAIL_IOS_APPIUM_ENDPOINT=http://127.0.0.1:PORT[/BASE]`, or an
  operator-installed executable selected by `DEVICERAIL_IOS_APPIUM_PATH`.
  Executable mode accepts optional `DEVICERAIL_IOS_APPIUM_PORT` (`0` chooses an
  available port) and `DEVICERAIL_IOS_APPIUM_BASE_PATH` (default `/`). It starts
  Appium with fixed numeric-loopback arguments, waits for bounded readiness,
  detects early exit, retains the child for the daemon lifetime, and performs
  bounded shutdown. An explicitly supplied external or Host-managed WDA
  endpoint is attached through `appium:webDriverAgentUrl`; when it is absent,
  that capability is omitted and XCUITest Driver manages its bundled WDA.
  External Appium inventory remains lazy;
  `device.connect` creates exactly one XCUITest W3C Session and
  `device.disconnect` deletes it. The stock daemon generates the bounded
  capabilities itself (`XCUITest`, UDID, device name, platform version, Safari
  WebView discovery, and the optional WDA URL); it accepts no arbitrary capability JSON,
  process arguments, or credentials. DeviceRail never installs Appium,
  Node.js, XCUITest Driver, or platform packages. Direct WDA and Appium cannot
  own concurrent Sessions for the same route. With this backend selected,
  `devicerail-daemon ios doctor` adds a bounded, redacted external probe or
  temporary managed-process readiness check. `/status` does not prove that the
  extension is installed: run `appium driver install xcuitest`; a missing or
  incompatible extension remains an explicit Session-creation error.
- `DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS` controls the typed
  `appium:newCommandTimeout` capability for every stock-daemon Appium Session.
  It defaults to `600` seconds and accepts only `1..=3600`; zero, malformed,
  and out-of-range values fail startup. This avoids Appium's 60-second default
  deleting an otherwise healthy Session during operator or Agent pauses. An
  explicit `invalid session id` is replaced before the next operation;
  ambiguous mutations are never replayed automatically.
- `DEVICERAIL_IOS_SESSION_TARGET=native|safari` selects the initial Appium
  XCUITest Session target and defaults to `native`. `safari` is rejected with
  `direct-wda`; only the Appium backend owns Safari/WebView contexts.
- `DEVICERAIL_IOS=auto|required` enables managed macOS lifecycle instead of an
  external device inventory. Direct WDA requires `DEVICERAIL_IOS_WDA_PROJECT`
  pointing to `WebDriverAgent.xcodeproj`; Appium can omit it and let XCUITest
  Driver manage its installed bundled WDA on either target kind. Discovery
  merges bounded `devicectl` (with `xcdevice` fallback) physical-device
  inventory with `simctl` Simulator inventory. Only an available Simulator in
  `Booted` state is connected. An explicit `DEVICERAIL_IOS_DEVICE_TOKEN` may
  select either kind. Without one, selection requires exactly one connected
  physical device and prefers it; only when none is connected does it require
  exactly one booted Simulator. DeviceRail does not create or boot a Simulator.
  For Direct WDA or an explicitly attached Appium WDA, the Host also uses
  cached `xcodebuild build-for-testing`, `test-without-building`, WDA readiness
  checks, and supervised recovery. A physical target additionally uses signing,
  trust/Developer Mode checks, and numeric-loopback `iproxy`; a Simulator skips
  those physical-device requirements and makes WDA listen directly on the
  selected host-local port. When `auto` starts without a usable target, the
  daemon stays available and continues bounded discovery; a later USB connect
  or operator-initiated Simulator boot can publish the route without a daemon
  restart. A published route is pinned to that UDID and never changes target
  during an outage. In bundled-WDA Appium mode, XCUITest Driver owns WDA launch
  and recovery. Device-state
  retries use a short interval, while host/build failures use capped backoff.
  `required` still fails startup when no usable route can be established.
  DerivedData, tool/port selection, and explicit provisioning updates are
  controlled by the variables documented in
  [`crates/ios-host/README.md`](crates/ios-host/README.md). `off` is the managed
  default; omitting `DEVICERAIL_IOS` preserves the legacy external settings.
- `DEVICERAIL_HARMONY=auto|off|required` controls startup HDC discovery and
  defaults to `off`, so an ordinary daemon start never invokes HDC.
  `DEVICERAIL_HDC_PATH` optionally selects the executable after `auto` or
  `required` explicitly enables the adapter and defaults to `hdc`; setting the
  path while HarmonyOS is disabled is rejected. `auto` preserves other routes
  on discovery failure or an empty inventory, while `required` makes either
  condition fatal.
- `DEVICERAIL_DESKTOP=auto|off|required` controls discovery of the one native
  desktop route for the daemon's compile-time host and defaults to `off`.
  `off` does not resolve a capture or input tool. `auto` preserves the other
  routes if native discovery or registration fails, while `required` makes the
  failure fatal. Registration is lazy: inventory does not capture the screen,
  inject input, or prove that the interactive desktop remains available;
  `device.connect` and later operations perform the host-specific runtime
  probes.
  `DEVICERAIL_DESKTOP_ID`, `DEVICERAIL_DESKTOP_NAME`, and
  `DEVICERAIL_DESKTOP_OS_VERSION` override the bounded local identity metadata;
  ID and name default to `desktop-local` and `Local desktop`, respectively,
  while `DEVICERAIL_DESKTOP_COMMAND_TIMEOUT_MS` sets the 1–300000 ms local
  command ceiling and defaults to 30000 ms.
- Native tool settings are host-specific:
  `DEVICERAIL_DESKTOP_MACOS_SCREENCAPTURE` selects `screencapture` on macOS;
  `DEVICERAIL_DESKTOP_WINDOWS_POWERSHELL` selects Windows PowerShell; and Linux
  accepts `DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=x11|wayland`,
  `DEVICERAIL_DESKTOP_X11_IMPORT`, `DEVICERAIL_DESKTOP_X11_XDOTOOL`,
  `DEVICERAIL_DESKTOP_WAYLAND_GRIM`, `DEVICERAIL_DESKTOP_WAYLAND_YDOTOOL`, and
  `DEVICERAIL_DESKTOP_WAYLAND_WTYPE`.
  `DEVICERAIL_DESKTOP_WAYLAND_INPUT=auto|ydotool|wtype` defaults to `auto`;
  Wayland must explicitly select
  `DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=wayland` and provide all three
  physical-pixel viewport fields
  `DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_WIDTH`,
  `DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_HEIGHT`, and
  `DEVICERAIL_DESKTOP_WAYLAND_VIEWPORT_SCALE_FACTOR`.
  Leaving the display-server setting unset cannot bypass this requirement.
  The complete names and host requirements are listed in
  [`crates/desktop-driver/README.md`](crates/desktop-driver/README.md).
- `DEVICERAIL_SCREENSHOT_POLICY=capture|omit` controls DeviceRail screenshot
  capture globally and defaults to `capture`. `omit` keeps typed Observation
  geometry and an explicit omission reason, but writes no screenshot Evidence.
  Protected actions always omit screenshots regardless of this setting.
- `DEVICERAIL_PLAYWRIGHT_ENDPOINT=ws://...|wss://...` explicitly enables
  Playwright page discovery. The endpoint must not contain URL credentials or
  a fragment. `DEVICERAIL_PLAYWRIGHT_BROWSER=chromium|firefox|webkit` defaults
  to `chromium`; `DEVICERAIL_PLAYWRIGHT_NODE` defaults to `node`; and
  `DEVICERAIL_PLAYWRIGHT_HELPER` defaults to
  `packages/playwright-driver/dist/helper.js`.
- `DEVICERAIL_RDP_BRIDGE=127.0.0.1:PORT` explicitly enables one RDP route and
  requires `DEVICERAIL_RDP_TARGET=rdp://HOST[:PORT]` plus the secret
  `DEVICERAIL_RDP_TOKEN`. `DEVICERAIL_RDP_NAME` is optional. The built-in
  bridge client is loopback-only, derives the DeviceId from the target
  fingerprint, uses the checked bridge-v2 Schema/fixtures, and never logs the
  target or token.
- `DEVICERAIL_PLUGIN_DIRS` explicitly enables process plugins from a bounded
  platform path list. On Unix, every directory, manifest, and relative
  executable is canonicalized and checked for ownership, writable permission
  bits, symlinks, no-follow opening, and stable file identity before any process
  starts. Other platforms currently fail closed because DeviceRail cannot prove
  an equivalent owner-only ACL contract; it does not use permissive fallback
  checks. `DEVICERAIL_PLUGIN_TIMEOUT_MS` sets the child request/response
  exchange timeout (1–120000 ms; default 30000 ms) and is rejected unless
  plugin directories are enabled. A shorter Core deadline still wins, and time
  spent waiting for the Driver's serialized exchange lock is governed by that
  Core request control rather than this transport timeout.
  Configured discovery is fail-closed: an empty, invalid, incompatible, or
  duplicate plugin set prevents startup.
- `DEVICERAIL_DISTRIBUTED_PEERS` names an owner-only JSON declaration of
  mandatory remote nodes. Every endpoint must be the loopback termination of
  an already established operator-managed SSH/mTLS tunnel; startup fails on a
  missing, stale, or incompatible peer rather than silently registering a
  partial inventory. The peer-file loader enforces Unix owner/mode/no-follow
  semantics and fails closed on platforms where an equivalent owner-only ACL
  cannot be proven.
- `DEVICERAIL_DISTRIBUTED_SERVER` names a separate owner-only JSON declaration
  that exports this daemon's non-remote startup inventory over one numeric
  loopback peer-v2 listener. The strict document contains `schemaVersion`,
  `nodeId`, `listen`, `securityMode`, `tunnelId`, `nodeEpoch`, and
  `inventoryRevision`; `securityMode` must be `externalSshOrMtls`, the port must
  be non-zero, and both wire integers must be in `1..2^53-1`. For example:

  ```json
  {
    "schemaVersion": 1,
    "nodeId": "lab-b",
    "listen": "127.0.0.1:7443",
    "securityMode": "externalSshOrMtls",
    "tunnelId": "ssh-lab-a",
    "nodeEpoch": 17,
    "inventoryRevision": 1
  }
  ```

  The file is all-or-nothing and uses the same Unix owner/mode/no-follow/ACL
  contract as the outbound peer file; platforms where that contract cannot be
  proved fail closed. After registering local routes, the daemon binds the
  listener before outbound peer discovery so two explicitly configured stock
  daemons can start without waiting for the other process to finish discovery.
  While its starting gate is closed, peer hello, inventory, health, and
  capabilities remain available for discovery; lease and mutation operations
  return retryable `node_starting`. Successful outbound registration marks the
  service ready. A later startup failure closes the listener and converges its
  accepted streams. The bind diagnostic reports only socket reservation and is
  not the ready transition. The service snapshots the completed local route
  inventory and never re-exports `remote:*` routes, so this is not transitive
  routing. Operators must advance `nodeEpoch` for a new node incarnation and
  `inventoryRevision` when the exported inventory changes.

  Both distributed files attest an external tunnel; raw loopback TCP is not
  authenticated merely because it is local. The peer listener is distinct from
  the JSON-RPC listener, and `DEVICERAIL_RPC_CREDENTIALS` does not authenticate
  peer-v2. DeviceRail does not open a public peer listener or provide SSH, TLS,
  mTLS, server identity, or tunnel lifecycle management. See
  [`crates/distributed-router/README.md`](crates/distributed-router/README.md).

Driver plugins are executables, not Rust dynamic libraries. The daemon invokes
only the fixed `--devicerail-plugin-abi=1` entry point, clears the inherited
environment, and exchanges one bounded JSON request/response over stdin/stdout.
ABI version, DeviceRail protocol range, immutable device identity, and exact
capability names/protection classes are negotiated before registration. There
is no plugin operation that accepts a shell command, executable path, or argv.
See [`crates/plugin-driver/README.md`](crates/plugin-driver/README.md) for the
manifest and wire contracts.

Build the private helper before enabling the Playwright route:

```sh
pnpm playwright-driver:build
DEVICERAIL_ANDROID=off \
DEVICERAIL_PLAYWRIGHT_ENDPOINT=ws://127.0.0.1:3000/session-token \
cargo run -p devicerail-daemon
```

The daemon never launches or downloads a browser. The remote endpoint must be
started and access-controlled by the operator, and its Playwright server must
have a compatible major/minor version with the pinned `playwright-core`
client. The stock helper works with the public `browserType.launchServer()`
endpoint: it keeps one connection for the daemon lifetime and creates one
blank context/page only when the server exposes no page. Each discovered
context/page is a separate stable DeviceRail route. Selectors are forced
through Playwright's CSS engine. `fillSecret` is a protected Action: its
arguments are redacted and its before/after observations contain no screenshot,
URL, or title. `elementExists` and `textContains` return strict boolean objects
for programmatic page assertions; `waitForSelector`, `clickByText`, and
`readValueNearLabel` (bridge v4) add element-state waits, visible-text clicks,
and run-time geometric label→value reads — all fail-closed, all data-only on
the wire.

Enabled Android, HarmonyOS, and native Desktop discovery each run once during
process startup. Android and HarmonyOS discovery use five-second command
ceilings and their registered routes use separate 65-second runners so
advertised 60-second gestures remain executable. Desktop discovery registers a
lazy host route whose commands use the bounded configured timeout. Request and
Action controls can still shorten those budgets. An explicit external iOS
route is registered without contacting WDA. Managed Direct/attached WDA is
prepared before registration and supervises Xcode plus a physical-device-only
`iproxy`; bundled-WDA Appium performs managed target discovery while XCUITest
Driver owns WDA. All iOS Driver paths use a 65-second transport ceiling.
Desktop registration likewise performs no capture or input; only the
compile-time host platform can be registered. `system.hello` remains
side-effect free and never discovers or connects a device. `events.clear`
deletes an ended Session log before releasing its Evidence pins; startup
reconciliation releases pins left behind if a prior process stopped between
those two steps.

Session start performs a bounded Driver health probe and atomically acquires a
five-minute owner-bound device lease. Core operation guards pin the lease
through Driver I/O, so expiry, release, removal, and another owner cannot race
an admitted operation. Health freshness and lease TTL use monotonic time;
Session end and connection cleanup release the lease only after active guards
finish. Protocol 1.2+ Session lifecycle events retain the leased `deviceId`.

## Offline Session Bundles

Bundle v1 exports an ended Session and every Evidence asset reachable from its
typed events into a deterministic directory. The host first saves exactly the
negotiated `eventProtocolVersion` and authoritative `session.export` result as
a strict JSON `BundleSource`. Unix requires no group/world mode bits, and the
Recorder additionally checks current-user ownership. Windows callers must
provide a suitably ACL-restricted parent directory. Protocol 1.4 clients that negotiated
`session.export.page.v1` assemble large exports from stable ended-Session pages;
legacy clients retain the original complete-response shape. The host then stops
the daemon so the filesystem Evidence Store's exclusive lock is released:

```sh
cargo run -p devicerail-bundle-cli -- export \
  --source ./session-source.json \
  --evidence-dir ./.devicerail/evidence \
  --output ./session.bundle

cargo run -p devicerail-bundle-cli -- validate ./session.bundle
```

Do not restart the daemon between shutdown and successful Bundle export:
startup reconciliation may release the stopped process's orphaned Evidence
pins before the CLI copies them. Once export has atomically published the
Bundle target, independent validation reads only that Bundle, so the daemon may
restart before validation. The stopped daemon cannot accept `events.clear`; a
later startup performs the necessary pin reconciliation.
The Bundle contains `manifest.json` plus optional content-addressed assets; its
hashes detect internal corruption but are not an origin signature. Bundle v1
is deliberately a directory format, not a zip. Each stdio response remains
subject to the 1 MiB frame limit, while bounded `session.export` pages remove
that limit from the complete Session. The local `BundleSource` retains an
independent 8 MiB hard limit; Recorder checkpoint v1 allows a fixed additional
64 KiB for checksum and phase metadata so a valid near-limit Source can still
reach completed. Larger Sessions require a future segmented checkpoint and
streaming Source contract.

## Offline Visualizer

Open a validated Bundle without a daemon, device, or network dependency:

```sh
cargo run -p devicerail-visualizer -- ./session.bundle
```

The command prints one local capability URL and serves until interrupted. The
listener is fixed to `127.0.0.1`; the random path is required on every request
and must be treated as a temporary secret. Pages are generated server-side,
contain no JavaScript, and use only same-origin CSS and digest-derived Evidence
routes. The Viewer displays sequence-ordered Session, Observation, Action,
Error, and Verdict events with bounded pagination.

Bundle hashes detect corruption but do not authenticate who created a Bundle.
The Viewer therefore always marks the input as unsigned. It reopens and
rehashes an asset before each response, previews only a bounded, fully checked
`image/png`, and offers other Evidence only as an explicit octet-stream
download. It never resolves manifest URIs, `file://` URLs, or arbitrary paths.

## Verifiable release archives

The release packager builds deterministic portable installer archives for the
daemon and Session Bundle CLI on Linux, macOS, and Windows. Every archive
includes a strict file manifest, SHA-256 sidecar, SPDX SBOM, DeviceRail-specific
in-toto provenance (without claiming a SLSA level), configuration example,
install script, and licensing inventory.
Unsigned CI artifacts carry `UNSIGNED` in the filename and cannot pass as a
signed release. Signed mode first verifies native payload signatures and then
requires a detached cosign signature over the complete archive; macOS signing
and notarization and Windows Authenticode run only when their explicit
identities are supplied.

See [the release packaging and verification guide](packaging/README.md). Source
and first-party release payloads are licensed under Apache-2.0; third-party
components retain their own license terms and are inventoried in release SBOMs.

## Frequently asked questions

### Is DeviceRail an Appium replacement?

DeviceRail is a smaller device-control and evidence runtime, not a drop-in
Appium implementation. Its optional iOS backend uses an operator-provided
Appium/XCUITest installation, either as an external service or a bounded
daemon-supervised local process, behind DeviceRail's versioned protocol and
Evidence model; it does not expose Appium as DeviceRail's public wire API. Existing
platform automation services such as ADB, Appium/WebDriverAgent, HDC,
Playwright, and an RDP bridge remain outside the kernel.

### Does DeviceRail include an AI agent?

No. DeviceRail exposes provider-neutral capabilities and an optional Tool
Adapter, but model selection, prompts, planning, approval policy, and agent
memory belong to the host application.

### Can one daemon control multiple device types?

Yes. The Driver Registry and Device Pool can expose multiple heterogeneous
routes, with connection-local device selection, owner-bound leases, health
checks, cancellation, and Session-scoped evidence.

### Can it run across machines?

Yes, through the opt-in distributed peer protocol over operator-managed SSH or
mTLS tunnels. Stock listeners bind to numeric loopback addresses; DeviceRail
does not claim to provide a public TLS endpoint or distributed consensus.

### Where are screenshots and recordings stored?

Large media is written to the filesystem Evidence Store and referenced by
SHA-256. Session events retain typed references, and a Session Bundle can be
validated and reviewed offline.

## Development

```sh
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm install --frozen-lockfile
pnpm protocol:types:check
pnpm protocol:types:test
pnpm protocol:types:build
pnpm client:typecheck
pnpm client:test
pnpm client:build
pnpm tool-adapter:typecheck
pnpm tool-adapter:test
pnpm tool-adapter:build
pnpm recorder:typecheck
pnpm recorder:test
pnpm recorder:build
pnpm live-visualizer:typecheck
pnpm live-visualizer:test
pnpm live-visualizer:build
pnpm playwright-driver:typecheck
pnpm playwright-driver:test
pnpm playwright-driver:build
pnpm yaml-adapter:typecheck
pnpm yaml-adapter:test
pnpm yaml-adapter:build
pnpm packages:check
python3 -m pip install -e "packages/python-client[dev]"
python3 packages/python-client/scripts/generate.py --check
python3 -m mypy --config-file packages/python-client/pyproject.toml packages/python-client/typing/contract.py
python3 -W error -m unittest discover -s packages/python-client/tests -v
python3 -m build --outdir packages/python-client/dist packages/python-client
python3 packages/python-client/scripts/check_distribution.py packages/python-client/dist
python3 -W error -m unittest discover -s packaging/tests -v
```

After changing a public protocol DTO, regenerate and verify the checked-in schemas:

```sh
cargo run -p devicerail-schema-gen -- write
cargo run -p devicerail-schema-gen -- --check
pnpm protocol:types:generate
```

See [the documentation index](docs/README.md), [the architecture](docs/architecture.md),
and [the roadmap](ROADMAP.md).

## Community and license

DeviceRail is licensed under the [Apache License 2.0](LICENSE). Contributions
are welcome; read [CONTRIBUTING.md](CONTRIBUTING.md), the
[Code of Conduct](CODE_OF_CONDUCT.md), and [Security Policy](SECURITY.md)
before opening a change or reporting a vulnerability.
