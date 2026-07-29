# Architecture

## Dependency direction

```text
                         +-> official language clients -> tools and apps
                         |
public wire protocol ----+
                         |
                         +-> core -> platform drivers -> daemon
```

Dependencies must never point upward. In particular, `protocol`, `core`, and drivers cannot import AI, YAML, recorder, visualizer, or application code.

The official Rust client depends directly on `devicerail-protocol`; it does not
link Core, a platform Driver, or the daemon implementation. The daemon and
clients meet only through the public JSON-RPC wire boundary. TypeScript and
Python clients consume generated forms of that same protocol contract.

Schema generation is a build-time edge: `schema-gen -> protocol`, and the shared
Driver conformance validator is compiled only for Driver tests. The stock daemon
does not run protocol Schema generation, but its explicitly configured plugin
and distributed adapters do link runtime JSON Schema validation for untrusted
Action contracts.

## Two atomic capability domains

### Device and action contract

- Lifecycle: connect, disconnect, health.
- Observation: optional screenshot/evidence reference, typed omission reason,
  viewport, and device metadata.
- Capability discovery: action name, description, JSON Schema, and protection
  classification.
- Execution: action call, output, before/after observation, and evidence.

### Evidence and event contract

- Execution events are append-only facts.
- Large assets are addressed by reference and hash.
- Recorder is a `TestEvent` consumer and verified Session Bundle producer.
- Visualizer is an event consumer.
- Storage and streaming are replaceable adapters.

## Session and event log

Every recorded fact uses one `TestEvent` envelope with `eventId`, `sessionId`, a one-based JavaScript-safe `sequence`, optional `requestId` and `deviceId`, `atMs`, and a nested typed `payload`. Sequence—not wall-clock time—is the replay order.

The Session Event Store assigns the sequence and appends the event in the same critical section. Sequence 1 is always `sessionStarted`; `sessionEnded` is final. A `SessionId` is lifetime-unique within an Event Store: deleting an ended log retains a used-ID tombstone, so the ID cannot be reclaimed while its Evidence release is still completing or at any later time. Events cannot be cleared or rewritten individually, and an active session cannot be deleted. `events.clear` is therefore a compatibility name for deleting one complete ended session.

An Action has exactly one `actionStarted` and one later `actionCompleted`. The terminal outcome is structurally one of `succeeded`, `failed`, `cancelled`, or `timedOut`; a generic Error event is never used as an Action terminal. The start and terminal facts preserve the same session, request, device, and call IDs.

Core separates `DriverError`, `EventStoreError`, and `RuntimeError`. A storage failure is never mislabeled as a Driver failure. The in-memory Store is the reference implementation for ordering and state-machine semantics; DR-006 adds durable evidence storage, while DR-007 adds cancellation-safe terminal completion and concurrent transport dispatch.

## Protected actions and screenshot policy

Protocol 1.2 adds the optional `action.protected.v1` Feature without changing
the shape of ordinary Action events. A protected or unknown call is persisted
as a `RecordedActionCall`: correlation ID and action name remain available,
while `arguments` is `null` and `argumentsRedacted` is true. Every Driver must
classify each advertised Action through both its capability and the internal
Driver contract; Core rejects mismatches at runtime, and the shared conformance
suite verifies them. Unknown names return no classification and therefore take
the protected recording path by default.

Feature negotiation is an execution boundary, not only a catalog hint. A
connection that did not negotiate `action.protected.v1` neither receives
protected capabilities nor may execute one by name. The Tool Adapter applies a
second boundary: protected tools are absent by default and require an explicit
host opt-in on a client whose handshake enabled the Feature.

`ScreenshotPolicy` is either `Capture` or `Omit`; the daemon maps the closed
`DEVICERAIL_SCREENSHOT_POLICY=capture|omit` setting into every registered
runtime. Omitted observations retain geometry, time, device metadata, and an
explicit `screenshotOmission` reason, but contain no screenshot or Evidence
receipt. Protected Actions override the global policy and always use the
`protectedAction` omission reason for before/after observations. Core rejects a
screenshot, returned Evidence, or persisted receipt on that path.

Android exposes a separate protected `inputSecret`; public `inputText` is not a
secret API. The protected value is removed from durable events before Driver
execution, is absent from host ADB argv, and is written to a fixed `adb shell
-T` command through child stdin. The Android Driver captures only display
geometry around that Action and maps all platform output to stable public
errors before discarding it.

This is a bounded confidentiality claim. It covers DeviceRail events, Session
exports, Evidence, known public diagnostics/Debug representations, host ADB
argv, and DeviceRail screenshot capture. It does not cover transient client or
process memory, stdio/pipes, swap or core dumps, root/ptrace, a malicious
Driver/ADB/device/app/IME, device-side process argv, model-provider retention,
misuse of a public Action, or a later ordinary screenshot of content that
remains visible.

## Request supervision and graceful shutdown

Protocol 1.1 exposes `request.control.v1`. The daemon admits the five device
methods and `media.stream.capture` as concurrent tasks and retains an
`ExecutionController` by RPC ID.
IDs remain reserved until their response enters the bounded writer queue;
duplicates never replace an existing controller. `request.cancel` reports
`requested`, `alreadyRequested`, or `notFound`, and the first cancellation
reason wins races with process shutdown.

`timeoutMs` is an absolute budget for request-scoped device work, including a
media capture's health probe and Observation. The
Action-specific `actionTimeoutMs` begins only after `actionStarted` is durable
and can shorten, never extend, the parent deadline. Once an Action starts, Core
always drives its terminal event to completion: the RPC error distinguishes
request cancellation, request timeout, Action timeout, and Driver failure,
while the event outcome uses `action_cancelled` or `action_timeout`. Durable
terminal cleanup is intentionally shielded and may finish after the device-work
deadline. Arbitrarily aborting a Core `execute` future is outside this contract;
callers must signal its controller and continue polling it. A future persistent
Event Store must likewise define cancellation-safe append/transaction semantics.

`session.end` is serialized by the Event Store state machine. If an Action has
started, it returns `session_busy` without sealing the Session; if end wins the
race first, the later `actionStarted` append is rejected and no partial Action
is recorded. Observation holds a Session-scoped Event Store lease from before
Driver/Evidence work through its final event append, so `session.end` likewise
returns `session_busy` while an observation is in flight. Releasing the exact
Session/lease pair is idempotent until the ended Session log is deleted; an
Event Store must keep either the active token or a released-token tombstone
after an ambiguous release error. Core retries that finalizer a bounded three
times even when operation control is already cancelled, preserving the
original operation result if release eventually succeeds. A permanent release
failure remains an explicit Event Store error and the Session cannot be assumed
safe to end.

The stdio supervisor uses bounded input, response, and in-flight request queues.
Blocking OS stdin/stdout live on detached system threads so a blocked pipe
cannot trap Tokio runtime teardown. On EOF, SIGINT, or SIGTERM the daemon stops
admission, cancels all running controllers with reason `shutdown`, drains tasks
so they can persist terminal events, ends the active Session with outcome
`shutdown`, disconnects all registered Drivers concurrently under independent
fresh bounded controls, then flushes responses up to the shutdown grace period.
Exhausted backpressure and a task that survives cooperative cancellation are
explicit fatal transport errors, not silent success paths.

The loopback TCP supervisor applies that sequence independently to every live
connection. Global shutdown broadcasts one absolute deadline, each connection
cancels and drains its own in-flight requests, and races shutdown ahead of any
inline management RPC so a blocked management future cannot prevent connection
cleanup. Each connection then reserves the final part of that deadline to
append `sessionEnded` and release its owner leases. The parent aborts only
connections that exceed the shared grace period; a final process-wide lease
release remains a safety net before Driver disconnection.

## Driver registry and device routing

Core's `DriverRegistry` owns heterogeneous `DeviceDriver` trait objects without
exposing platform-specific types to the wire protocol. Registration validates
the Driver and `DeviceInfo` identities, rejects blank metadata and duplicate
IDs, and never silently replaces an existing route. Listing is deterministic
by `DeviceId`; a `DriverHandle` is a stable route that does not retain the
registry map lock while device work is in progress.

Each registered device has its own lifecycle gate. `connect` and `disconnect`
take that device's exclusive gate, while capabilities, observation, and Action
execution may share it. Lifecycle changes therefore cannot race ordinary work
on the same device, but unrelated devices have no common gate and can execute
concurrently. A successful connect refreshes the registry-owned `DeviceInfo`
snapshot only after confirming that the Driver returned the registered ID.

Protocol 1.2 exposes discovery and connection-local selection through the
optional `device.routing.v1` extension. `devices.list` returns the stable device
list and nullable `selectedDeviceId`; `device.select` resolves an exact
`DeviceId` before changing the selection. A connection with one registered
device lazily routes legacy device calls to that sole device. With multiple
devices, an unselected call fails explicitly with `device_selection_required`;
with none, it fails with `device_not_found`.

Concurrent device requests capture the connection's selected route when
admitted and execute through its stable `DriverHandle`, so a later
`device.select` cannot redirect in-flight work. Device operation events carry
the routed device's real ID. When `device.routing.v1` is negotiated, Session
lifecycle events retain the leased route's `deviceId`; legacy connections that
did not negotiate routing leave it unset.

## Daemon startup and local configuration

Production startup opens exactly one `FileEvidenceStore` and injects the same
trait object into the registry, every built-in Android, iOS, HarmonyOS, and
host-native Desktop route, the Mock Driver, and the Session cleanup path. The
default root is `.devicerail/evidence`; operators can replace it with
`DEVICERAIL_EVIDENCE_DIR`. Before accepting stdin requests, the daemon
reconciles every Store reference whose Session is absent from the in-memory
Event Store. This intentionally releases pins left by a prior process restart,
because the v1 daemon does not persist Session event logs.

`DEVICERAIL_ANDROID` is a closed `auto`, `off`, or `required` policy and defaults
to `auto`. `off` does not initialize or invoke ADB. `auto` logs bounded local
diagnostic codes and retains the Mock route when ADB initialization or host
discovery fails, or when discovery returns no stable descriptors. `required`
makes those conditions startup errors. `DEVICERAIL_ADB_PATH` selects the host
executable. Startup discovery and registered Drivers use separate system ADB
runners: discovery commands have a five-second upper bound, while runtime
commands have a 65-second hard ceiling so the advertised 60-second swipe can
complete. Request and Action controls remain authoritative and may impose a
shorter deadline. Discovered stable serials are sorted and registered
independently, including offline and unauthorized states so a later explicit
connect can return the precise lifecycle error.

The iOS endpoint depends on the selected backend. Direct WDA always receives
an explicit numeric-loopback WDA endpoint. Legacy external mode obtains it
from `DEVICERAIL_IOS_WDA_ENDPOINT` plus a stable device token, registers
without I/O, and leaves WDA/optional MJPEG and `iproxy` to the operator.
Appium instead receives one numeric-loopback Appium endpoint and only receives
an optional WDA endpoint when the operator explicitly attaches one. Otherwise
XCUITest Driver owns its installed bundled WDA. Optional fields form one atomic
configuration and orphan or non-loopback values fail startup.

The separate Host layer is enabled by `DEVICERAIL_IOS=auto|required`. It always
performs bounded `devicectl` discovery with `xcdevice` fallback and requires an
unambiguous physical device. Direct WDA additionally requires an explicit
`WebDriverAgent.xcodeproj`; the Host fingerprints Xcode/project/device inputs
for DerivedData reuse, runs `build-for-testing`, starts
`test-without-building` and numeric-loopback `iproxy`, and admits the route
only after WDA `/status` is ready. Appium may omit the project: the Host then
limits itself to physical-device discovery while XCUITest Driver owns its
installed bundled WDA. Appium server ownership is separately either an
operator endpoint or a daemon-supervised executable launched with fixed
numeric-loopback arguments.

Owned Direct WDA and Appium children are health-checked and terminated during
daemon shutdown. `auto` logs only a stable code, preserves other routes, and
retains cancellable discovery after an initially missing or unready device; a
later hot-plug dynamically registers the route. A published Direct WDA route
is pinned to its original UDID while WDA, `iproxy`, device readiness, and the
cached build are revalidated before recovery. `required` fails startup.
Provisioning updates are off unless explicitly enabled. No mode stores Apple
credentials or bypasses trust, Developer Mode, UI Automation, or certificate
confirmation. Public Debug and startup diagnostics redact endpoint, project
path, executable path, and device selection; intentional wire inventory still
exposes the stable route identity. Driver requests retain the 65-second
transport ceiling and a configured broken MJPEG never silently falls back.

### iOS semantic automation boundary

Protocol 1.5 defines one platform-neutral semantic surface for native and web
content. An Observation may reference a versioned UI Snapshot Evidence object;
that object contains a bounded, normalized preorder tree with roles, names,
values, logical viewport bounds, enabled/hittable state, and opaque stable node
IDs. Selectors and node references contain no WDA, XCTest, WebKit, Appium, CSS
engine, or platform element-handle type. A node ID is scoped to its automation
Session, context, and document epoch; a reconnect, navigation, or context epoch
change makes the old reference stale instead of silently redirecting it.
Online clients resolve the reference only through `ui.snapshot.get` for an
Observation in their current active Session. The daemon finds the typed
reference in that Session's event log and verifies the Session's Evidence pin
before opening it; callers cannot submit an arbitrary `AssetRef` or use the
method as a cross-Session object-existence probe.

Both UI Snapshot fields and semantic execution metadata are operation-scoped
additions, not unconditional Driver output. The daemon enables them only after
the corresponding Protocol 1.5 Feature is negotiated; Core rejects a Driver
that returns them on an older operation. Both UI fields absent means “no UI
Snapshot claim” and preserves Protocol 1.0–1.4 Observation shapes, including
protected operations from Drivers that do not implement UI capture. A semantic
Action must return valid native, web, or explicit coordinate-fallback execution
metadata, while a non-semantic Action must not return that metadata.
Snapshot reads also preserve this boundary across connections: `events.list`
and complete or paged `session.export` return
`session_protocol_incompatible` when a Session contains additive event fields
newer than the caller's selected protocol. They never strip those fields or
serialize a Protocol 1.5 event to a Protocol 1.0–1.4 connection.

The five canonical semantic Actions (`findElement`, `tapElement`,
`clearElement`, `setElementValue`, and `waitForElement`) continue through the
ordinary `device.execute` boundary. Availability is per Driver and is reported
by `device.capabilities`; the handshake feature only states that both peers
understand the wire contract. Native content is resolved through an
accessibility tree. Safari and WebView content is resolved through a WebDriver
web context and DOM semantics. A coordinate action is a distinct, recorded
compatibility fallback, never an implicit response to not-found, ambiguous, or
stale semantic matches.

One device has exactly one automation-Session owner. For Direct WDA or an
explicitly attached WDA, the iOS Host may own WDA, `xcodebuild`, and `iproxy`
process lifecycles, but never a second WebDriver Session. A configured backend
is exclusively either Direct WDA (legacy, native-only) or Appium XCUITest
(native and web). In Appium mode `AppiumIosDriver` alone creates/deletes the
W3C Session; it either reuses an explicitly attached WDA endpoint or lets
XCUITest Driver manage its bundled WDA. The Appium server itself may be an
operator process or a daemon-supervised executable. The daemon never
constructs a concurrent Direct-WDA Session for that device. Backend changes
require a disconnected device with no Device Pool lease, and native/web
context switches share the same per-device Session gate.

The iOS Appium backend implements this P1 integration. It advertises the five
semantic Actions only after providing normalized native/Web snapshots, typed
selectors, node-reference provenance checks, and shared conformance coverage.
`setElementValue` is Protected and secure field values are redacted before a
tree or event can be persisted. Appium server ownership may be external or a
bounded daemon-supervised child; in the default bundled-WDA path DeviceRail
does not inject `appium:webDriverAgentUrl`. Direct WDA remains a separate,
native-only backend and still requires its explicit endpoint/lifecycle. Real
device/version coverage remains a release validation matrix, not a protocol
or deterministic conformance claim.

HarmonyOS discovery is separately gated by
`DEVICERAIL_HARMONY=auto|off|required` and defaults to `off`; the disabled mode
does not resolve, initialize, or invoke HDC. Once explicitly enabled,
`DEVICERAIL_HDC_PATH` selects the executable and defaults to `hdc`. Discovery
and registered Drivers use distinct HDC runners with five-second and 65-second
command ceilings. Connect-key descriptors are sorted and registered even when
offline or unauthorized so connect reports their precise state. `auto` retains
the other routes after a stable-code discovery or registration failure;
`required` fails startup on initialization, discovery, an empty inventory, or
registration conflict. Raw HDC output, target IDs, and executable paths are
not startup diagnostics.

ADB and HDC discovery are process-startup concerns, not handshake concerns.
`system.hello` only negotiates protocol and features, and never runs discovery
or connects a device. Startup failures and discovery issues stay on the local
diagnostic channel; wire errors expose stable codes rather than executable
paths or tool stderr.

## Host-native Desktop boundary

`DEVICERAIL_DESKTOP=auto|off|required` is a closed, explicit opt-in and defaults
to `off`. A disabled daemon does not resolve a desktop capture or input tool.
When enabled, a process registers at most one native Desktop route, and only for
the operating system it was compiled for: macOS, Windows, or Linux. It never
pretends that one binary can expose all three hosts. Bounded ID, name, optional
OS version, and command timeout settings form the route's startup identity;
host-specific tool, Linux session, input-backend, and Wayland viewport settings
are rejected when incomplete or inconsistent.

`auto` preserves the Mock and every other successfully registered route after
a stable-code native discovery or registration failure. `required` converts
the same failure into a process startup error. Successful registration is
deliberately lazy: it establishes the immutable profile and resolves the
configured host tools, but performs no screenshot or input operation and does
not prove that a GUI session will remain available. Startup discovery has a
five-second control deadline. `device.connect` and health checks perform the
host-specific profile, viewport, and permission probe; observation and Action
execution exercise capture and input tools. Commands use the configured
1–300000 ms ceiling, which request control can shorten. Local paths and raw
command stderr do not become wire errors.

On macOS, `/usr/sbin/screencapture` is the default capture tool and Quartz
provides input. Non-prompting CoreGraphics and Accessibility preflight state is
part of the Driver profile, but the daemon does not open a TCC prompt. Screen
Recording and Accessibility must be granted to the actual daemon executable;
granting them only to a shell or terminal does not authorize a separately
launched binary or service.

On Windows, the adapter uses Windows PowerShell for virtual-desktop probe and
capture and bounded Win32 input APIs for text, keyboard, pointer, and wheel
operations. It controls only the interactive session containing the daemon.
Installing or launching it as a Session 0 service does not grant access to a
logged-in user's desktop, and discovery does not claim otherwise.

Linux keeps X11 and Wayland as distinct profiles. An explicit
`DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=x11|wayland` wins over session
detection; otherwise detection uses `XDG_SESSION_TYPE`, `DISPLAY`, and
`WAYLAND_DISPLAY` and rejects ambiguity. Automatic detection cannot enable a
Wayland route: Wayland requires an explicit
`DEVICERAIL_DESKTOP_LINUX_DISPLAY_SERVER=wayland`, all three configured
physical-pixel viewport fields, `grim`, `WAYLAND_DISPLAY`, and
`XDG_RUNTIME_DIR`. Leaving the display-server setting unset does not bypass the
viewport contract. X11 requires ImageMagick `import` and `xdotool`, plus the
daemon's access to the matching `DISPLAY` and `XAUTHORITY`. Wayland's input
choice is closed as
`auto|ydotool|wtype`: auto prefers an available `ydotool`, then the reduced
`wtype` profile. `ydotool` additionally requires a reachable `ydotoold` with
permission for `/dev/uinput`; discovering an executable does not establish
that runtime service. `wtype` never advertises pointer or wheel Actions.

Desktop registration remains above Core and uses the same `DeviceDriver`,
Device Pool, operation-scoped Evidence Store, screenshot policy, event, and
lease contracts as every other route. The stock adapter does not install host
tools, start a display server or compositor, change TCC permissions, create an
interactive Windows session, or start `ydotoold`.

## Remote TCP admission boundary

The optional `devicerail-remote-auth` gate runs before `system.hello` on the
loopback TCP control plane. A client proves possession of one 32–64 byte key
through a versioned, length-prefixed HMAC-SHA256 challenge. Challenges are
random, single-use, short-lived, attempt-bounded, and indistinguishable for
unknown principals and bad proofs. The complete prelude, including durable
audit writes, has one connection deadline. Credentials are loaded only from a
bounded owner-only regular file; the raw JSON, encoded secret, decoded key,
and error paths use zeroizing storage.

Permissions are closed and hierarchical (`read`, `control`, `admin`). Every
known RPC method has an explicit minimum and unknown future methods are denied.
Before dispatch, the daemon durably appends one canonical hash-chained record
whose fixed stage is `securityAdmission`. A successful audit outcome proves
only that authentication or authorization admission completed; it is not an
RPC terminal-result log. Parameters, nonces, proofs, output, Evidence, and
credentials are never audit fields. Append or sync failure poisons the writer
and closes admission.

This HMAC authenticates the client only. The listener remains numeric
loopback, and HMAC supplies neither server identity, encryption, nor transport
integrity after authentication. Cross-host deployment therefore requires a
separately authenticated SSH or mTLS tunnel. The audit chain detects partial
or local modification but is not a signature against a same-account attacker
who can replace the entire chain.

The official Rust client's built-in `connect_tcp` and `attach` paths currently
begin at NDJSON JSON-RPC and do not implement this length-prefixed HMAC
prelude. They therefore cannot directly attach to a stock listener configured
with `DEVICERAIL_RPC_CREDENTIALS`. Adding a first-party authenticated attach
helper must preserve this admission boundary rather than embedding credentials
in JSON-RPC parameters. `connect_tcp` additionally rejects port zero and every
non-loopback IPv4 or IPv6 address before opening a socket. Generic `attach`
remains the explicit caller-owned boundary for trusted tunnels, authentication
preludes, proxies, or a stricter external transport policy.

The Rust client's public `SpawnConfig` Debug representation exposes counts
rather than argument or environment values and redacts the complete hello
offer. Its bounded child `stderr_tail()` accessor is explicitly sensitive raw
diagnostic data and is never included in ordinary errors or Debug output.

## Distributed peer boundary

`devicerail-distributed-router` remains a Driver-layer adapter above Core. Its
peer-v2 protocol is independent from the public JSON-RPC control plane: an
outbound peer becomes a normal namespaced `DeviceDriver`, while the node-side
`RegistryPeerService` translates authenticated peer operations back into the
same process-wide Registry, Device Pool, Session Event Store, and Evidence
Store. Neither Rust Driver traits nor platform-library types cross this
boundary.

Peer-v2 hello explicitly negotiates UI Snapshot and semantic-Action transport
support. Observe/execute carry both operation Feature gates without widening
them at the remote node. Imported UI Tree bytes pass the same typed body and
reference validation before a local event can be appended; AssetRef,
first-chunk, actual-byte, and reused-id digests must agree. Canonical semantic
Schemas are followed by Arguments DTO validation before dispatch, bounded
remote `invalid_arguments` keeps its Driver taxonomy, and typed results/node
links are revalidated at the receiving Driver boundary. Peer-v1 is closed as
unsupported rather than silently dropping those operation-scoped guarantees.

The stock daemon has two independent, explicitly enabled distributed roles.
`DEVICERAIL_DISTRIBUTED_PEERS` loads mandatory outbound tunnel endpoints.
`DEVICERAIL_DISTRIBUTED_SERVER` loads one inbound peer-server declaration with
the closed fields `schemaVersion`, `nodeId`, `listen`, `securityMode`,
`tunnelId`, `nodeEpoch`, and `inventoryRevision`. Both files are bounded,
owner-only, no-follow inputs. Their socket addresses must be numeric loopback;
the server port must be non-zero and its only accepted security mode is
`externalSshOrMtls`. Platforms where DeviceRail cannot prove the same owner and
ACL contract fail closed rather than accepting environment-only trust claims.

After local route registration, startup constructs the peer service and binds
the configured listener before outbound discovery. Construction snapshots
non-remote routes and rejects `remote:*`, preventing implicit multi-hop export
and route cycles. The service begins behind a starting gate: hello, inventory,
health, and capabilities are discovery-safe, while lease acquisition and every
mutation fail with retryable `node_starting`. This lets two stock daemons
complete mandatory outbound discovery without either side exposing a mutable
route before its own outbound registration converges. Successful outbound
registration marks the service ready. A bind diagnostic only reports the local
socket and is not the ready transition; callers observe readiness through peer
operations and their bounded retry contract. The operator advances `nodeEpoch`
for a new service incarnation and `inventoryRevision` when the exported
snapshot changes.

Listener, connection, service, and Core shutdown form one ordered lifecycle. A
startup failure after bind closes the listener and accepted streams. Normal
stdio EOF, RPC shutdown, or signal shutdown stops admission, cancels peer
requests, drains per-connection staged cleanup, calls service shutdown, and
only then releases the remaining process-wide leases and Drivers. Idle peer
connections are included in that bounded drain; dropping a listener task must
not detach a lease, Session, or Evidence reference. An unexpected peer request
task failure is fail-stop for the stock listener and triggers the same global
shutdown path rather than leaving the daemon available with uncertain state.

`externalSshOrMtls` is an operator attestation, not a cryptographic handshake.
Raw loopback TCP can be opened by another local process and does not become
authenticated merely because DeviceRail attached a tunnel identifier. The
separate JSON-RPC HMAC prelude is not applied to peer-v2. Production cross-host
use therefore requires an independently authenticated SSH or mTLS tunnel whose
local termination and access controls are part of the trusted deployment.
DeviceRail provides no public listener, TLS stack, server identity, tunnel
lifecycle, consensus, cross-node atomic lease, dynamic inventory stream, or
telemetry exporter.

## Process-isolated Driver plugins

`devicerail-plugin-driver` is an adapter above Core, not a native extension
point inside the kernel. The boundary is a published JSON ABI over a supervised
child process:

```text
protocol DTOs -> core DeviceDriver <- plugin adapter -> fixed executable/stdin+stdout
                                                    X  no dylib / Rust ABI / shell
```

Plugins are disabled unless `DEVICERAIL_PLUGIN_DIRS` supplies one or more
explicit directories. Discovery examines at most 16 unique directories, 256
entries per directory, 64 manifests, and 64 KiB per manifest. The directory,
manifest, and every component of the relative executable path must be
non-symlink filesystem objects with a consistent owner; Unix group/world write
bits are rejected and the executable must have an execute bit. Canonical paths
must remain beneath the configured directory. Discovery never searches
`PATH`, user profiles, package registries, or the current directory implicitly.

Manifest v1 declares ABI v1, plugin and device identity, a DeviceRail protocol
range, and the exact action names/protection classes. The first child request
is `hello`; daemon and plugin must agree on the ABI, highest common protocol
version, identity, version, and complete capability set. Every returned action
Schema is bounded, meta-schema checked, compiled locally, object-rooted, and
forbidden from resolving external references. The host derives the public
DeviceId as `plugin:<pluginId>:<deviceKey>`, so a plugin cannot alias a native
route by returning a chosen DeviceId.

Each Driver owns one long-running child started with a fixed
`--devicerail-plugin-abi=1` argument, empty inherited environment, closed
operation enum, and bounded NDJSON stdin/stdout plus lifetime-bounded stderr.
Requests are serialized and correlated by `requestId`, so `connect` state is
the state used by later observations and Actions. Request cancellation,
deadline, timeout, output overflow, or framing ambiguity terminates and poisons
the child through Tokio's supervision; it is never transparently restarted or
replayed. Driver drop also kills the child. Stderr and executable paths never
cross the public error boundary.
The adapter validates dynamic Action arguments against the negotiated Schema,
canonicalizes PNG frames before Evidence storage, strips metadata for protected
actions, and never renders Action arguments in `Debug`. It does not retry an
ambiguous mutating Action.

## Android ADB staging

`devicerail-android-adb` begins as a host-ADB discovery and lifecycle support
layer. Its replaceable command boundary is crate-private: applications receive
typed discovery and device lifecycle APIs, not a generic remote-shell or raw
ADB escape hatch. Every device command is serial-scoped, bounded, cancellable,
and isolated from global ADB server mutation.

Before DR-014, the support crate deliberately did not advertise itself as a
`DeviceDriver` until observation and executable Actions both existed. DR-013
added screenshot and viewport observation with Session-pinned evidence;
DR-014 composes the actual Android Driver, registers it with the daemon, and
invokes the complete shared conformance suite. DR-015 adds closed application
lifecycle and system-navigation Actions; DR-016 adds protected input and the
display-only omission path. This preserves the rule that
every real Driver has non-placeholder capabilities, observations, results,
and evidence from its first trait implementation.

Android screenshot bytes are capped at 32 MiB before parsing. A pinned
pure-Rust PNG decoder consumes every row and the trailer with checksums enabled;
APNG and unbounded text/ICC metadata are excluded. Device-specific ceilings
limit each dimension to 16,384, total pixels to 33,554,432, and decoded frame
data to 128 MiB. `wm size` shares the dimension and pixel limits, while
`wm density` is capped at 10,000 dpi. The actual decoded screenshot dimensions,
not natural-orientation `wm size`, define the captured Observation viewport.
When policy or Action protection omits a screenshot, the same bounded `wm`
parsers provide the viewport and orientation without invoking `screencap`.

An Android device operation gate gives observation a shared lease across the
connected-state check, all three serial-scoped ADB reads, PNG validation, and
the Session evidence pin. Connect, health, disconnect, and mutating Actions use
a control-aware exclusive lease. This prevents lifecycle changes
from crossing an observation without holding the lifecycle state mutex across
ADB or Store I/O; unrelated devices remain independent.

## Evidence storage

The replaceable `EvidenceStore` trait lives in core; the filesystem implementation is isolated in `devicerail-evidence-fs`, so Drivers and protocol DTOs do not depend on filesystem types. Inputs and verified outputs are streamed through `AsyncRead`, which keeps screenshot/video size independent from process memory.

For observation and Action calls, `DeviceRuntime` binds the
`OperationContext` Session to a restricted, non-cloneable
`SessionEvidenceWriter` and supplies it alongside the derived control in a
`DriverOperationContext`. A Driver can put new bytes or attach an existing
canonical asset, but it cannot inspect or choose the Session, access the raw
Store, release references, or run GC. A runtime without an injected Store
rejects writes with `evidence_store_unavailable`; it never silently discards
evidence. Evidence failures remain distinct from Driver and Event Store
failures through runtime events and RPC error data.

An injected Store also enables strict operation-scoped provenance. Each
successful `put` or `attach` adds its canonical `AssetRef` to a private writer
receipt set. Before recording a successful result, Core requires that set to
equal the de-duplicated set returned by the observation screenshot, or by an
Action's `evidence` plus before/after screenshots. This rejects references
from another operation as well as distinct writes omitted from the result;
repeating the same reference across result fields remains valid. Driver
failure or cooperative cancellation still takes precedence when there is no
successful result. A Store pin created before such a failure is retained
conservatively until Session cleanup; v1 has no per-operation rollback marker.
An explicitly screenshot-omitted observation/action is the sole empty-receipt
exception: its typed omission reason must match the effective policy, its
screenshot and Action Evidence must be empty, and the writer must have recorded
no put or attach.

Store-owned assets use SHA-256 as the identity. Their canonical protocol subset is:

```text
id     = sha256:<64 lowercase hex>
uri    = devicerail://assets/sha256/<64 lowercase hex>
sha256 = <the same digest>
```

The filesystem adapter writes an immutable object into same-filesystem staging, syncs data and metadata, atomically renames the object into its hash shard, syncs the parent, then atomically creates a durable Session reference. A returned `AssetRef` therefore always has both bytes and a Session pin. Cancellation before publication removes staging; a failure after object publication can only create an unreferenced orphan, which startup recovery marks for conservative GC.

References are one marker per `(Session, digest)`, not a mutable refcount. Releasing a Session first persists a closed-Session tombstone and then removes its markers. This ordering prevents a concurrent slow upload from recreating a reference after cleanup. Session IDs are globally unique and are not reused within a Store; v1 retains those tombstones intentionally.

Bounded GC later verifies each unreferenced object's metadata, size, and digest, atomically moves it from the live object tree into a trash directory, syncs both sides, removes its marker, and finally deletes the trash entry. Startup either finishes an unreferenced deletion or restores an object that has a durable reference. Malformed markers, unknown entries, dangling live references, symlinks, and corruption stop the operation explicitly.

The deletion order is intentionally the reverse of creation: seal/delete the Session event log first, then release Evidence references. A crash between these steps leaks pinned bytes until reconciliation, but never leaves a retained event pointing at deleted evidence.

## Portable Session Bundle

The Session Bundle is an offline boundary above protocol and core. Its writer
accepts an explicit event protocol version, one ended `SessionExport`, and a
minimal read-only Evidence source. It has no Driver, daemon, recorder,
visualizer, or platform dependency. Bundle v1 is one canonical directory:

```text
manifest.json
assets/sha256/<64 lowercase SHA-256>  # present only when referenced
```

`manifest.json` contains the format magic, Bundle version, event protocol
version, `SessionInfo`, the complete ordered event sequence, and a
digest-sorted asset index. JSON is compact UTF-8 with recursively sorted object
keys and exactly one trailing line feed. Asset paths are derived from their
digest rather than accepted as arbitrary filesystem input. The same Session
and Evidence therefore produce byte-identical internal files; timestamps,
absolute paths, staging IDs, and host metadata never enter the format.

Only typed `AssetRef` positions are reachable: captured Observation
screenshots, successful Action before/after screenshots and result Evidence,
Verdict Evidence, and Protocol 1.4 media-frame Evidence. Duplicate references
are legal and collapse to one file;
one digest with conflicting media types fails. Unreferenced Store pins are not
exported, and a typed screenshot omission never creates a placeholder. In
particular, a protected successful Action must retain its
`protectedAction` omissions and contain no screenshot or Action Evidence.

The writer validates the complete event state machine before I/O, copies each
asset through a bounded streaming hash check, and writes into a private
staging directory beside the target (`0700` on Unix; parent ACL on Windows). It writes the manifest last, validates
the staging tree, syncs it, and atomically renames it without replacing an
existing target. Cancellation or any failure before that rename leaves no
target. The rename is the publication linearization point: cancellation after
it cannot retract the Bundle, and a later parent-directory sync failure is
reported as published with uncertain durability rather than as an unpublished
failure.

The offline validator applies manifest/tree limits before bounded allocation
or asset reads, then rejects
symlinks, special files, extra paths, non-canonical JSON, unsupported versions,
invalid Session/Action correlation, missing or unreferenced assets, and every
size/hash/media mismatch. Successful validation reconstructs the original
`SessionExport` in sequence order. These hashes establish internal integrity,
not origin authenticity: without an external signature or trusted digest, a
party that rewrites the manifest and every asset consistently is outside the
claim.

The local filesystem tree and selected output parent must remain stable for one
operation. Final components are opened without following symlinks and every
Bundle path is derived, but v1 does not claim protection from another process
with the same filesystem authority replacing intermediate directories while a
validation is in progress. A future hostile-concurrency claim requires a
dirfd/openat-style walk held across the full operation. Apple/Linux/Redox use
an atomic rename-without-replacement; Windows uses `MoveFileExW` without the
replace flag and with write-through. Other hosts fail export closed unless a
proven no-clobber primitive is added.

Bundle v1 deliberately has no archive encoding or Bundle-specific public RPC.
A host ends the Session, then saves the negotiated event version plus the final
authoritative `session.export` result in a strict `BundleSource`. Unix enforces
owner-only mode bits, and Recorder also checks current-user ownership; Windows
deployments must provide a suitably ACL-restricted parent directory. The host
stops the daemon before the local CLI opens the exclusively locked File
Evidence Store.
It must finish copying Evidence and atomically publish the Bundle before any
daemon restart. Validation then reads only the self-contained Bundle and may
run after restart; startup reconciles the stopped process's orphaned Evidence
pins, and the stopped daemon cannot accept `events.clear`. Every stdio response
is still capped at 1 MiB, but Protocol 1.4's negotiated
`session.export.page.v1` allows the authoritative ended Session to be assembled
from stable bounded pages. The complete local `BundleSource` is independently
capped at 8 MiB, below the Bundle manifest's default 16 MiB budget; the
checkpoint allows another fixed 64 KiB solely for checksum, phase, Session, and
receipt metadata. Online or remote Bundle streaming and a zip wrapper remain deferred until their
authentication, traversal, duplicate-entry, symlink, and decompression limits
have complete contracts.

## Execution Recorder

The Execution Recorder is a TypeScript boundary above the typed client. It
consumes only public `TestEvent`, `SessionInfo`, and Evidence references, and
hands an ended `SessionExport` to the existing Session Bundle contract. It
does not import a Driver, inspect Evidence bytes, own Session lifecycle, or
reimplement the Rust Bundle manifest and validator.

Sequence is the only replay order. A Recorder accepts the next contiguous
event, treats an exact canonical duplicate at an already confirmed sequence as
idempotent, and rejects a changed duplicate, gap, cross-Session event, reused
event or Action-call identity, terminal append, or mismatched Action
correlation. Concurrent Actions are tracked by call ID rather than a stack.
Protected arguments and screenshot omissions are preserved exactly; the
Recorder never invents an Evidence placeholder.

Every confirmed batch is first applied to a cloned event-log state, then
written to a versioned checkpoint with an exclusive writer lease and revision
compare-and-swap. The checkpoint advances only after its atomic
replacement is durable. It contains typed events and references, not Evidence
binary data, platform objects, or filesystem paths. Corrupt, non-canonical,
unknown-version, stale-writer, and conflicting checkpoints fail closed instead
of restarting from an empty log.

Checkpoint v1 deliberately publishes one canonical snapshot with the complete
confirmed event prefix after every accepted page. Across `P` pages and `N`
events this creates `O(P * N)` aggregate encoding and write amplification,
which is quadratic for fixed-size pages. The implementation structurally
shares already validated in-memory events and encodes a new envelope payload
once, but those optimizations do not change the durable single-file cost or its
revision-CAS boundary. An asymptotic storage improvement requires a future,
explicitly versioned segmented event journal with checksummed records,
compaction, and crash-recovery rules; checkpoint v1 must not acquire an
implicit sidecar or weaker page durability.

On Unix, Recorder requires current-user-owned files with no group/world mode
bits and syncs the containing directory after publication. Node.js has no
portable Windows owner-only ACL verification or directory-fsync API. Recorder
therefore does not claim or enforce owner-only access on Windows; the deployment
must provide a suitably ACL-restricted parent directory. Windows still retains
regular-file identity checks, flushed files, and atomic replace/no-clobber
publication, but final directory durability is filesystem-defined. A stale
writer lock is reclaimed only after the OS reports its PID as nonexistent;
ambiguous or reused PIDs remain locked for manual recovery.

After observing `sessionEnded`, the Recorder compares its log with the daemon's
authoritative final Session export and durably seals an exact `BundleSource`.
When Protocol 1.4 negotiated `session.export.page.v1`, each export page repeats
the same ended `SessionInfo`, carries at most 1000 events, and either supplies a
`nextAfterSequence` exactly equal to its last event or omits the field on the
final page. Under one Event Store lock the daemon clones only Session metadata
and a bounded vector of `Arc<TestEvent>` handles; after releasing the lock it
selects a prefix against the exact serialized response-byte budget and only
then materializes at most 1 MiB of JSON. Its capped counting writer stops serde
as soon as an oversized event crosses the remaining budget and reports an
`actualBytesAtLeast` lower bound instead of scanning the complete value.
Recorder reads these pages without changing its checkpoint, compares every
event with the durable log, rejects metadata or cursor drift, and performs one
final recording-to-sealed revision CAS. Without the Feature it retains the
original complete `session.export` request and JSON response shape.
Concurrent callers share one successful seal operation, while cancellation is
isolated per caller: a cancelled waiter does not stop the owner, and an
uncancelled waiter can retry if the owner cancels before committing.
The host then publishes that canonical Source, closes the client/daemon to
release the Evidence Store lock, and invokes the real offline Bundle CLI for
both export and independent validation. Only matching successful summaries may
mark a fresh export complete. Recovery from an already published target runs
the validator, compares its exact protocol/Session/event identity, and on Unix
re-syncs the output parent before completing the checkpoint. Recorder
cancellation preserves the last durable checkpoint and never creates a
synthetic terminal event or completed recording.

The snapshot RPC methods do not currently accept request-control options, so
an AbortSignal is observed between `events.list`/`session.export` calls rather
than injected into them. A host must close the stdio client to interrupt a call
whose connection itself is stuck.

Recovery is deliberately bounded in v1. While the original daemon remains
alive, a new Recorder can resume after its confirmed sequence. The daemon's
Event Store is currently in-memory, so losing that daemon during an active
Session makes the upstream log unrecoverable; this is an explicit incomplete
recording, not a successful recovery. `events.list` now accepts an optional
1–1000 event page limit while preserving the original full-suffix behavior when
the field is absent. Recorder advances `afterSequence` across bounded pages, so
a large missed suffix no longer has to fit in one 1 MiB response. It starts at
1000 events, halves only on typed `response_frame_too_large`, reuses the first
successful limit, and reports a single still-oversized event explicitly. The
same adaptive policy is used for negotiated authoritative export pages, so the
complete Session no longer has to fit one RPC response. Checkpoint v1 and the
Bundle Source still contain the complete Session. The Source has an explicit
8 MiB hard limit, while the checkpoint adds a fixed 64 KiB metadata headroom so
a valid near-limit Source can progress through sealed and completed; removing
the full-snapshot bound requires a new segmented checkpoint and streaming
Source contract. Protocol 1.3 also provides a resumable event
stream for consumers that opt into the live data plane.
Sealed checkpoints remain independently exportable as long as the daemon has
not been restarted and its Evidence pins are still present.

## Offline Visualizer

The offline Visualizer is a read-only consumer above the Session Bundle
boundary. It calls the Rust `validate_directory` implementation once and keeps
that validated `SessionExport` and asset index as its in-memory snapshot. A
browser never reads or re-validates `manifest.json`, so there is no second,
weaker TypeScript interpretation of the Bundle contract.

The Viewer binds only an ephemeral `127.0.0.1` port and puts a fresh random
capability in every route. It accepts a deliberately small HTTP subset, checks
the exact loopback `Host`, closes each connection, and applies bounded request,
header, concurrency, and shutdown deadlines. The capability is temporary and
must not be shared. Responses disable sniffing, framing, referrers, external
connections, scripts, objects, forms, and cross-origin resource access. HTML
is generated on the server with contextual escaping; CSS is a fixed bundled
resource, and there are no fonts, analytics, CDNs, or runtime network calls.

HTML is written directly into a hard-capped 2 MiB document buffer; event
filtering relies on the validator-confirmed sequence and never collects or
re-sorts the complete log. At most two renders and two asset responses may own
their bounded response memory concurrently. CPU-heavy rendering and complete
PNG decoding run on bounded blocking workers rather than a Tokio reactor
thread, and their permits remain held until the response is written or the
worker ends. Shutdown tracks both connection tasks and those memory permits;
failure to reclaim them inside the configured grace is explicit.

Sequence remains the sole timeline order. The Viewer pages at most 50 events
at a time and treats timestamps, viewport values, messages, action arguments,
outputs, and arbitrary JSON only as bounded display text. It shows all four
Action terminal outcomes, general Error and Verdict events, protected
screenshot omissions, unavailable previews, and an always-visible warning that
Bundle hashes provide integrity rather than author authenticity.

Evidence links are generated only from a digest in the validated asset index.
Neither `AssetRef.uri`, the manifest path field, an absolute path, nor a
`file://` URL reaches HTML or HTTP routing. Each asset request derives its path
again, opens the final component without following links, enforces a lower
Viewer byte budget, and streams size and SHA-256 validation into owned bytes.
Unix uses `O_NOFOLLOW`; Windows opens the handle with
`FILE_FLAG_OPEN_REPARSE_POINT` and checks that opened handle, avoiding a
check-then-open reparse race.
Only exact `image/png` assets that also pass bounded PNG structure, dimensions,
pixel-count, decode, and trailer checks are returned inline. SVG, HTML, XML,
PDF, video, and unknown media remain non-inline octet-stream downloads.

This repeated read closes final-component replacement and post-validation
tamper races. It does not enlarge the Session Bundle's stated filesystem claim:
the Bundle tree must remain stable against another process with the same
authority replacing intermediate directories during the Viewer lifetime. A
future stronger guarantee still requires a held directory-handle-relative
walk. The Viewer imports no Driver, daemon client, Recorder, AI SDK, Prompt, or
YAML runtime.

## Live Visualizer

The Live Visualizer is a separate online consumer, not a live mode inside the
offline Bundle validator. `@devicerail/live-visualizer` depends only on the
public TypeScript protocol package and Node.js built-ins. It turns each typed
`TestEvent` into an immutable, bounded presentation DTO; it never retains the
raw event, an event-stream capability, or an `AssetRef.uri`. The private
`apps/live-visualizer` host is the only layer that also depends on the typed
client.

The host receives an already-owned, already-negotiated client and one explicit
Session ID. It does not select or control a device, start or end a Session, or
close that client. Its acknowledgement boundary is deliberately split into
four steps: prepare a canonical fingerprint and sanitized presentation,
reserve that presentation in the bounded model, explicitly confirm the stream
item, and only then publish a new UI revision. A disconnect between reservation
and confirmation can replay the same sequence and fingerprint idempotently;
different content at that sequence fails closed. Resume always starts after
the last confirmed daemon cursor. Daemon cursors, model revisions, and SSE
event IDs are independent domains. Prepare and commit authority is held in
private object-identity maps, so copying public token fields cannot forge an
accepted presentation. Event IDs and Action IDs are single-use, completions
must match a confirmed start and result ID, and Session end is rejected while
an Action remains in flight.

The in-memory timeline has ceilings for input bytes, JSON depth and bytes,
text, Evidence references, one event, total retained bytes, and event count.
It never evicts confirmed history silently. Reaching a ceiling leaves the
current stream item unconfirmed and transitions to
`viewerCapacityExceeded`, directing the operator to end the Session and use
the offline Bundle Viewer. Pages preserve sequence order, use the same five
filters as the offline Viewer, and contain at most 50 entries. The
`observations` filter contains `observationCaptured` and
`mediaFrameCaptured`; media stream start/end boundaries remain visible only in
`all`. Presentation may still style all three media lifecycle events with the
same media category, but visual styling does not determine filter membership.
Live Evidence is reference-only metadata (ID, media type, and optional digest);
the host offers no asset, filesystem, download, or network proxy route. Binding
also verifies that the HTTP response-byte ceiling can serialize the largest
page admitted by the configured event, total-byte, and event-count limits; an
event is never acknowledged into a model whose pages are permanently
unservable.

The browser is one security boundary farther away from the daemon. It receives
only a 256-bit capability URL on an ephemeral numeric IPv4 loopback listener.
Every HTML, JavaScript, CSS, JSON, and SSE request requires that path and the
exact numeric `Host`; the server accepts a bounded GET/HEAD subset, rejects
bodies and ambiguous targets, applies same-origin checks without CORS, and
ships fixed CSP-constrained assets. Browser code constructs nodes with
`createElement` and `textContent`. SSE carries only small revision
invalidations; state and pages are fetched separately from bounded APIs. Each
tab has its own queue and drain deadline, so a slow tab can only disconnect
itself and never delays daemon acknowledgement.

## Generated cross-language contract

The serialized Rust DTOs are the single source of truth. The opt-in protocol `schema` feature feeds an independent generator that writes versioned Draft 2020-12 documents to `protocol/schema/v1`. Protocol 1.5 currently contains 174 checked-in Schema documents, including typed request and response contracts for all 24 public methods, both event-stream server notifications, manual recording templates, media stream DTOs, UI Snapshot and semantic-action DTOs, Verdict persistence, and bounded Session export parameters/results. A checked-in manifest provides stable schema names and IDs, while `--check` fails for missing, changed, or stale output.

The 89 Golden Fixtures under `crates/protocol/fixtures` lock all 24 method request/success-response pairs, the two event-stream notifications, and representative device, observation, action, semantic UI, result, error, and event payloads. Each fixture maps to a generated schema and must round-trip through its concrete Rust type. This makes the artifacts directly reusable by Rust, TypeScript, and Python clients.

`crates/client` is the official asynchronous Rust boundary. Its typed method
markers use `devicerail-protocol` request and result DTOs directly, so there is
no copied Rust wire model. The control path applies bounded UTF-8 NDJSON
framing, strict JSON-RPC response correlation, negotiated Feature checks,
request cancellation, and bounded shutdown. It can spawn and own a daemon over
stdio, attach to caller-provided asynchronous read/write halves, or connect to the
stock loopback TCP listener; the high-level constructors perform
`system.hello` before returning a ready client. Its consistency suite exercises
the canonical Golden Fixture envelopes and the same typed method mapping used
by live calls.

When `events.stream.v1` is enabled, the Rust client calls
`open_event_stream(EventsSubscribeParams, EventStreamOptions)`. The socket
actor advances the received cursor after validation; `next()` advances only
the delivered cursor, not durable application progress;
`confirm(&cursor)` advances the confirmed cursor only in contiguous order.
`cancel().await` closes local stream work explicitly. Once the stream is finished,
`resume(options)` obtains a fresh single-use capability from the confirmed
cursor; an active or undrained stream cannot be resumed. The stream exposes
received, delivered, and confirmed cursors plus the typed terminal state so
callers do not have to infer acknowledgement or closure from socket behavior.

`packages/protocol` consumes that checked-in manifest and generates one
isolated TypeScript module for every public Schema root. Its generated index
exports the 174 public model names plus a manifest-derived 24-method map, avoiding collisions between repeated
local `$defs`. A byte-for-byte `--check` catches missing, changed, and stale
output, while a generated `satisfies` contract type-checks all 89 Golden
Fixtures without assertions. The package has no Rust or daemon runtime
dependency. TypeScript intentionally approximates constraints such as number
ranges and exact `oneOf`; external JSON still requires runtime Schema
validation.

`packages/python-client` consumes that same manifest into Python 3.11+
`TypedDict`, union, `Literal`, overload, method-map, and packaged runtime
Schema modules. Its async stdio transport validates both directions, reserves
capacity for cancellation, isolates shared response futures from caller task
cancellation, and treats any ambiguous partial write or malformed response as
terminal. Wheel and source-distribution checks import the package in an
isolated interpreter and compare all packaged Schema bytes with the Rust
source set; Python does not maintain handwritten duplicate protocol DTOs.

`packages/client` is the Node.js stdio boundary over those generated method
types. It admits calls only after `system.hello`, enforces negotiated Feature
use, caps both NDJSON directions, serializes writes through bounded
backpressure, and correlates out-of-order responses by their exact typed ID.
Incoming JSON is checked for the JSON-RPC envelope/result-error XOR, strict
error shape, safe JavaScript integers, and a negotiation-consistent hello
result before it reaches the typed API. Shutdown stops admission, drains every
accepted write, closes stdin, lets the daemon finish admitted work, and applies
one deadline to the whole drain-and-exit sequence; abnormal exits retain only
a bounded stderr tail for diagnostics.

`packages/tool-adapter` is the final provider-neutral layer in the
`protocol -> client -> adapter` chain. It snapshots `device.capabilities` into
deeply immutable definitions, assigns provider-safe names without changing the
underlying Driver action name, and adds one explicit observation tool. The
adapter defensively checks the pure-JSON object envelope and common Schema
keyword shapes without compiling or resolving references. It preserves the
Driver's declared dialect and compound-resource structure byte-for-value in a
deeply frozen snapshot. Full meta-schema and reference validation remains the
Driver conformance boundary, so the adapter does not create a second, narrower
wire contract or fetch remote resources.

The adapter generates a fresh Action UUID, keeps any Agent invocation ID as
separate correlation metadata, and returns `ActionResult`, `Observation`, and
evidence references structurally. Request timeout, Action timeout,
`AbortSignal`, and explicit cancellation map directly to the typed client.
RPC and Driver failures remain rejected failures. Device selection, connect,
Session start/end, disconnect, and client close are deliberately absent from
the adapter lifecycle and remain explicit host responsibilities. No AI or
Agent SDK is a runtime or development dependency of this package.

The optional `packages/yaml-adapter` is one level above the typed client. It
parses a bounded `devicerail/v1` document into a process-authenticated,
immutable sequence of public RPC calls. Duplicate keys, aliases, merge keys,
prototype properties, non-finite values, unknown fields/methods, and resource
budget overflow fail closed. Each Action is classified for its compiled
device route before arguments can enter the plan; execution reselects that
route and refreshes capabilities immediately before the call. YAML never
crosses into Rust, the daemon, a Driver, or a protocol DTO.

## Driver conformance boundary

Every Driver supplies a fresh-instance factory and one valid call for each advertised action to the shared conformance suite. The suite checks lifecycle idempotency, stable identity, disconnected errors, capability uniqueness and JSON Schema validity, observations, action results and evidence, unknown/invalid actions, and runtime event ordering. It derives negative calls for missing required fields, wrong types, numeric bounds, and forbidden extra properties, so Drivers must enforce the Action Schema they advertise.

DeviceRail v1 requires each Action to advertise a valid, self-contained JSON
Schema with an object root. The Driver may declare a supported dialect and use
compound resources; the optional full conformance validator checks the
meta-schema, compiles references, and validates the factory call. Successful
actions include an after-observation plus evidence. Conformance should run
against a dedicated disposable device because every advertised action is
exercised. The harness has configurable suite and cleanup timeouts and always
attempts `disconnect`, including after a failure or timeout.

The suite is behind the default-off `conformance` feature; full action-schema compilation is behind `conformance-json-schema`. Production core and daemon builds therefore do not include the test framework or JSON Schema validator.

A Driver crate enables the feature only for tests and supplies construction and valid-call factories:

```rust
devicerail_core::driver_conformance_test!(
    conforms_to_device_driver_contract,
    || MyDriver::new(),
    valid_call_for,
);
```

Evidence-producing Drivers use the macro's fourth argument to inject an
isolated `Arc<dyn EvidenceStore>`; the three-argument form uses an explicit
rejecting Store for Drivers whose fixtures do not persist Store-owned assets.
The four-argument form enables the strict operation receipt check described
above; the compatibility form may continue to return Driver-owned references.

## AI boundary

AI agents live outside DeviceRail. They may use any transport or language client to implement:

```text
observe -> decide -> execute -> verify
```

DeviceRail supplies typed tools and evidence, not planning policy.

## Transport

DeviceRail uses JSON-RPC 2.0 as its language-neutral envelope. The control
plane is NDJSON over stdio by default, or over an explicitly enabled
loopback-only multi-client TCP listener. Every line contains exactly one
complete request or response. NDJSON is transport framing, not a different RPC
protocol. Batch requests and notifications are not supported. All TCP
connections share one Registry/Device Pool but keep independent handshake,
selection, Session, cancellation, and request state. Protocol 1.3 adds a
separate WebSocket data plane whose post-subscription messages are typed server
notifications.

### Connection bootstrap

Every transport connection owns an independent handshake state:

```text
                       successful system.hello
AwaitingHello ----------------------------------------> Ready
      |                                                    |
      | other method: handshake_required                   | system.hello:
      | failed hello: remain AwaitingHello                  | handshake_already_completed
      v                                                    v
AwaitingHello                                             Ready
```

`system.hello` must be the first successful request. It negotiates the wire version and optional protocol features, then assigns a `connectionId`. It must not discover, select, connect to, or lease a device. Device lifecycle remains an explicit operation after the connection reaches `Ready`.

### Version negotiation

Wire versions are `{major, minor}` pairs and are independent from crate or product SemVer versions. Each peer offers one or more ranges shaped as `{major, minMinor, maxMinor}`. Multiple ranges can express non-contiguous major support without implying support for the major versions between them.

The server intersects client and server ranges with the same major, then selects the lexicographically highest common `(major, minor)`. Empty or inverted ranges are invalid. If there is no intersection, the connection remains in `AwaitingHello` and returns `protocol_version_incompatible` with both offers and a stable reason: `clientTooOld`, `serverTooOld`, or `noCommonVersion`.

### Feature negotiation

Handshake features are versioned protocol extensions, such as `events.snapshot.v1`; they are not device actions or driver capabilities. A client sends `required` and `optional` feature sets:

- every required feature must be supported, otherwise negotiation fails with `required_feature_unsupported`;
- supported optional features are enabled and unknown optional features are ignored;
- the response contains the complete enabled feature set for that connection.

Extension methods are available only when their feature was negotiated. `events.list`, `events.clear`, `session.export`, and `sessions.list` require `events.snapshot.v1`. Session start/current/end and base device lifecycle remain available without Feature flags. Observation and execution require an active Session so every produced fact is attributable.

### Stable failures

Failures use JSON-RPC numeric error codes plus structured `data` containing a stable DeviceRail error `code`, human-readable `message`, `retryable`, and optional `details`. Protocol clients should branch on the structured code, not on message text. DR-001 defines stable errors for parse and envelope failures, invalid parameters, unknown methods, handshake ordering, incompatible versions, unsupported required features, and internal failures. DR-007 adds explicit Feature-not-negotiated, timeout-not-supported, cancellation, timeout, duplicate-ID, request-limit, and transport-backpressure paths; DR-008 adds explicit device-selection and missing-device routing failures. DR-010 bounds outbound payloads too: an oversized result is replaced with the same-ID `response_frame_too_large` failure rather than emitting an unbounded frame.

### Resumable event data plane

`events.stream.v1` is an additive Protocol 1.3 Feature. A negotiated stdio
connection calls `events.stream.open` for one Session and receives a redacted,
30-second, single-use capability URL. The adapter binds only `127.0.0.1` and
requires the exact numeric Host, capability path, `devicerail.events.v1`
subprotocol, and absent-or-exact Origin policy. It does not negotiate
compression. Header, frame, message, connection, capability, replay, queue,
write, and shutdown budgets are all explicit. Remote TLS and identity remain
outside this loopback boundary until DR-043.

The WebSocket first performs an independent `system.hello` at the exact protocol
version selected by its control connection, then exactly one `events.subscribe`.
A 1.3 feeder terminates explicitly before a 1.4 media DTO instead of silently
filtering it and breaking sequence continuity. Its cursor binds the
daemon-lifetime stream epoch, Session, and last confirmed event sequence. Core
registers the live broadcast tail and captures the replay snapshot while
holding the same Event Store state lock; the transport therefore emits one
continuous snapshot-to-tail prefix. Epoch mismatch, cursor mismatch/ahead/too
old, Session deletion, sequence gap, Core lag, subscriber queue stall,
oversize event, cancellation, and shutdown are explicit response or terminal
states.

Each subscriber has an event-count and serialized-byte bounded queue feeding a
single socket writer. It never drops, merges, or reorders an event. A slow or
failed subscriber is terminated independently and cannot block Event Store
append or another stream. The Rust and TypeScript clients separately track the
last received, delivered, and application-confirmed cursor; resume starts only
after the last explicit confirmation, never after mere socket receipt.

## Playwright remote boundary

The conformant Rust Web Driver never links a browser runtime into Core. It
spawns one bounded Node helper and supplies the endpoint and operations only
over a private NDJSON stdin/stdout channel. The private helper pins
`playwright-core`, keeps one connection to an operator-owned remote server,
forces selectors through the explicit CSS engine, and returns exactly one
bounded response line per request. It never downloads or launches a browser.
Bridge wire v3 combines each context/page ordinal with a
domain-separated hash of Playwright's server-owned `Page.guid`; URL, title, and
viewport remain presentation state rather than identity. Page reordering or a
same-state replacement fails closed before an admitted operation can be
redirected. The public `browserType.launchServer()` path initially exposes no
connection-owned page, so the helper creates one context and blank page only
when necessary and retains it through the persistent connection. Protected
fills use the ordinary Core protection path and return neither screenshots nor
post-fill URL/title metadata. Read-only `elementExists` and `textContains`
actions return closed boolean result objects rather than unbounded DOM data.
Bridge v4 adds `waitForSelector` (wait for one closed element state on a
strict-CSS target), `clickByText` (click the single visible text match;
ambiguity fails closed), and `readValueNearLabel` (run-time geometric
label→value resolution returning a bounded `{ "value": string }`; the in-page
algorithm is a fixed driver constant — only data crosses the bridge).

## Manual Action recordings

`ManualRecording` is a standalone v1 protocol document, not a new daemon RPC
or a browser-specific event. It binds a source device, a digest of the
advertised ActionSpace, continuous human step sequence, stable call IDs, and
Action argument templates. The replay compiler compares the current
ActionSpace digest and validates each resolved argument object against the
advertised Driver Schema. Standard arguments may be durable; a protected step
stores only a restricted opaque `secretRef`, and the host supplies the full
protected arguments transiently during compilation. No DOM, Playwright, UI,
or platform-library type crosses the protocol boundary.

## Evidence-referenced media streams

Protocol 1.4 `media.stream.v1` adds a closed start/frame/end event lifecycle.
`MediaStreamWriter` is first prepared as a retained recovery token, then
`ensure_started` publishes the exact start event. If that acknowledgement is
lost, the original request ID, timestamp, device, and payload remain available
for an exact retry. The writer serializes producers per stream, attaches each
bounded frame to the Session Evidence Store, then appends only its canonical
`AssetRef` to the Event Store. Evidence attachment starts the irreversible
finalization boundary and is shielded from later request cancellation; the
event append commits the frame. If that append fails, the writer retains that
exact pending frame; idempotent `finish` or `abort` retries it before closing
the accepted prefix, without a second Evidence write. Stream IDs are
lifetime-unique within a Session; frame indexes
are continuous and one-based; media type is fixed at start; the terminal count
must equal the accepted prefix. An active stream blocks Session end. Bundle
validation repeats the state-machine checks and includes reachable frame
assets. WebSocket and Recorder preserve the canonical reference, including its
Store URI, without embedding bytes; offline and live presentation layers remove
that live URI before anything reaches HTML or a browser API.

The daemon exposes the lifecycle through `media.stream.start`,
`media.stream.capture`, and `media.stream.end`. The Feature is advertised only
when screenshot capture and a managed Evidence Store are available. Start
binds a lifetime-unique caller ID to the active Session and selected leased
device without performing unbounded Driver I/O. Capture is the only concurrent
operation: it supports request timeout/cancellation and obtains a screenshot
only through that Driver's normal Observation path. A successful capture
therefore appends `observationCaptured` before `mediaFrameCaptured`. The daemon
verifies `image/png`, then attaches the Store-owned reference. The wire methods
never accept frame bytes,
paths, or client-provided Evidence references. A caller-supplied one-based
`frameIndex` makes an exact lost-response retry idempotent; conflicting or
skipped retry metadata fails explicitly. Screenshot frames omit `durationMs`;
video frames require a positive duration and are independent PNG key frames,
not an encoded container. Start, frame, and explicit end events retain the
request ID of the RPC that produced each event; automatic connection/shutdown
cleanup uses no caller request ID.

Per connection/Session admission allows at most two active streams, eight
stream IDs, 1000 frames per stream, and 20 capture attempts per second per
stream. A second capture for the same stream is rejected instead of queued.
Protected and unknown Actions are mutually exclusive with active or starting
media streams, while remote authorization classifies all three media methods
as Control. Core rejects direct Session closure while a stream is open. The
daemon's explicit Session end, connection cleanup, and global shutdown take
the capture gate and abort every open stream before appending `SessionEnded`.
The real terminal append, not only lock acquisition, is covered by the shared
grace deadline; independent streams close concurrently and retry ambiguous
terminal acknowledgements with bounded backoff. A dead connection releases its
device lease even if Session finalization ultimately reports an error. An
ambiguous frame-event append poisons the stream
for further capture and drives the writer's idempotent abort recovery, so it
cannot be retried as a new frame.
