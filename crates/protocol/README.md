# DeviceRail wire protocol

`devicerail-protocol` defines the cross-language DTOs and JSON-RPC wire contract shared by the daemon and language clients. Rust traits remain internal implementation details; this crate's serialized forms are the compatibility boundary.

The 174 generated Protocol 1.5 Draft 2020-12 schemas are checked in under
[`protocol/schema/v1`](https://github.com/wangweiwei/device-rail/tree/main/protocol/schema/v1),
and 89 cross-language examples are enumerated by
[`fixtures/manifest.json`](https://github.com/wangweiwei/device-rail/blob/main/crates/protocol/fixtures/manifest.json).
All 24 public methods have a typed request and success-response baseline.
Regenerate schemas with `cargo run -p devicerail-schema-gen -- write`; use
`--check` to detect missing, changed, or stale files.

The type-only
[`@devicerail/protocol`](https://www.npmjs.com/package/@devicerail/protocol)
package is
generated from those checked-in schemas. Its strict TypeScript contract loads
all Golden Fixtures with `satisfies`, so language clients do not maintain a
second handwritten DTO set.

Rust conformance suites can enable the non-default `fixtures` Cargo feature to
access the exact embedded manifest and fixture catalog without depending on a
repository-relative filesystem path. Normal protocol consumers should leave
that feature disabled.

## Envelope and framing

DeviceRail uses a request/response subset of JSON-RPC 2.0:

- every request and response contains `"jsonrpc": "2.0"`;
- every request has a string or non-negative JavaScript-safe integer `id`;
- after negotiating `request.control.v1`, the five device methods and
  `media.stream.capture` may set
  `timeoutMs` to a positive JavaScript-safe millisecond value;
- `params`, when present, must be an object or array; known no-parameter methods reject non-empty values;
- every response has exactly one of `result` or `error`;
- batch requests are not supported; stdio remains response-only, while a
  negotiated event WebSocket can emit only the two typed stream notifications;
- protocol DTO fields use `camelCase`.

The stdio transport uses NDJSON framing: one complete JSON-RPC message per line,
with a 1 MiB JSON payload limit in both directions and bounded input/output queues. Framing is
transport-specific and is not part of the method contract. Protocol 1.3+
uses one JSON message per WebSocket message for the event data plane.

## Bootstrap with `system.hello`

`system.hello` must be the first successful request on each transport connection.

Example request:

```json
{
  "jsonrpc": "2.0",
  "id": "hello-1",
  "method": "system.hello",
  "params": {
    "client": {
      "name": "example-client",
      "version": "0.1.0"
    },
    "protocol": {
      "ranges": [
        { "major": 1, "minMinor": 0, "maxMinor": 2 },
        { "major": 3, "minMinor": 0, "maxMinor": 0 }
      ]
    },
    "features": {
      "required": [],
      "optional": ["action.protected.v1", "device.routing.v1", "events.snapshot.v1", "request.control.v1"]
    }
  }
}
```

Example response:

```json
{
  "jsonrpc": "2.0",
  "id": "hello-1",
  "result": {
    "connectionId": "019f4b9d-3deb-71b1-ae15-d2fd105269d6",
    "protocol": {
      "selected": { "major": 1, "minor": 2 }
    },
    "server": {
      "name": "devicerail-daemon",
      "version": "0.1.0"
    },
    "transport": {
      "kind": "stdio",
      "framing": "ndjson"
    },
    "features": {
      "enabled": ["action.protected.v1", "device.routing.v1", "events.snapshot.v1", "request.control.v1"]
    }
  }
}
```

The handshake creates a protocol connection and returns a `connectionId`. It does not connect to, select, or lease a device. Those lifecycle operations remain explicit device methods after a successful handshake.

## Protocol version negotiation

A wire version is an explicit `{major, minor}` pair and is independent from the daemon, client, or crate SemVer version.

Each side offers an array of ranges:

```json
{ "major": 3, "minMinor": 1, "maxMinor": 4 }
```

The array supports multiple, non-contiguous majors. For example, offering majors 1 and 3 does not imply support for major 2.

Negotiation follows these rules:

1. Reject an empty offer or any range where `minMinor > maxMinor`.
2. Intersect client and server ranges only when their `major` values match.
3. Select the lexicographically highest compatible `{major, minor}` pair.
4. If no pair is compatible, return `protocol_version_incompatible` and keep the connection in `AwaitingHello` so the client may retry.

An incompatibility includes both offers and one stable reason: `clientTooOld`, `serverTooOld`, or `noCommonVersion`.

## Feature negotiation

Feature negotiation applies only to optional protocol extensions. Feature names are versioned, for example `events.snapshot.v1`. Core device observation and action execution are not feature flags; the driver's action space describes those capabilities after the handshake.

Clients split their offer into two sets:

- `required`: every named feature must be available or the handshake fails with `required_feature_unsupported`;
- `optional`: supported names are enabled and unsupported names are ignored.

The hello result returns the complete deterministic `enabled` set. Transport constraints may further reduce the server's effective feature set.

Methods belonging to an extension are visible only when that feature was negotiated. In v1, `events.list`, `events.clear`, `session.export`, and `sessions.list` require `events.snapshot.v1`; otherwise they return `method_not_found` with `requiredFeature` details.

Protocol 1.4 adds the optional `session.export.page.v1` extension on top of
`events.snapshot.v1`. A legacy `{sessionId?}` request still returns exactly the
complete `{session, events}` result. Supplying `limit` (1–1000), with optional
`afterSequence`, requires the paging Feature and returns an immutable page of
an ended Session. A non-final page includes `nextAfterSequence` equal to its
last event sequence; a final page omits it. `afterSequence` without `limit`, an
active Session, an invalid limit, or a cursor beyond the log fails explicitly.

Protocol 1.5 adds three optional extensions, with one explicit dependency.
`observation.uiSnapshot.v1` lets an Observation reference one bounded,
normalized UI tree stored as typed Evidence; `ui.snapshot.get` resolves only a
reference reachable from the connection's current active Session.
`device.semanticActions.v1` defines the canonical `findElement`, `tapElement`,
`clearElement`, `setElementValue`, and `waitForElement` Action contracts plus
explicit native, web, or coordinate-fallback execution metadata. Drivers must
advertise those Actions before use. It may be enabled only together with
`observation.uiSnapshot.v1`; if the enabled set contains
`device.semanticActions.v1` without that dependency, `system.hello` fails with
JSON-RPC error `-32004` and `data.code = feature_dependency_unsatisfied`.
`verdict.record.v1` remains independently negotiable: it validates that every
Evidence reference is reachable from the active Session and persists the
caller's Verdict; the daemon does not calculate the Verdict.

Protocol 1.3 adds `events.stream.v1`. A negotiated stdio connection calls
`events.stream.open` to obtain a short-lived, single-use, Session-scoped
loopback bearer endpoint; the endpoint is never diagnostic output. The
WebSocket connection performs its own `system.hello`, then exactly one
`events.subscribe`. `EventStreamCursor` binds `streamEpoch`, `sessionId`, and
the last application-confirmed `sequence`, so a cursor from another Session or
daemon lifetime fails explicitly. The server sends `events.stream.event`
notifications followed by one closed, typed `events.stream.terminal` reason.
`lastEmittedCursor` describes only the server's continuous sent prefix and is
not an application acknowledgement.

Protocol 1.4 adds `media.stream.v1`. `mediaStreamStarted`,
`mediaFrameCaptured`, and `mediaStreamEnded` form a closed Session-scoped
lifecycle. A frame carries a one-based index, optional timing/key-frame
metadata, and one canonical `AssetRef`; screenshot/video bytes remain in the
Evidence Store. Stream IDs cannot be reused, frames cannot skip or change
media type, and a Session cannot end with an open stream. The standalone
`ManualRecording` v1 DTO stores ordered human-selected Action templates and an
ActionSpace digest. Protected templates contain only an opaque `secretRef`;
the replay host supplies complete protected arguments transiently.

The production control entry is `media.stream.start` → one or more
`media.stream.capture` calls → `media.stream.end`. Start binds a caller-chosen
stream ID to the active Session and selected leased device. Capture accepts a
one-based `frameIndex` for exact retry and optionally a request timeout; it
internally obtains screenshot Evidence from that device and never accepts
frame bytes, filesystem paths, or caller-provided `AssetRef` values. A `video`
stream is a timed sequence of independent PNG key frames, not an encoded video
container, so every video capture requires a positive `durationMs`.

Protocol 1.2 also adds `action.protected.v1`. Protected capabilities are
omitted and direct protected execution is rejected unless the connection
explicitly negotiated this Feature. Protected and unknown Action events use a
`RecordedActionCall`: `arguments` is `null` and `argumentsRedacted` is true.
Ordinary Action event JSON is unchanged. A screenshot-omitted Observation has
no screenshot and carries a typed `screenshotOmission` reason; an omitted
Action likewise has no screenshot Evidence.

Protocol 1.2 adds `device.routing.v1`. When negotiated, `devices.list` returns
the stable device list plus the connection's nullable `selectedDeviceId`, and
`device.select` accepts `{ "deviceId": <DeviceId> }` and returns the selected
`DeviceInfo`. Selection belongs to one connection. A device request captures
its selected route when accepted, so a later selection cannot redirect work
already in flight. These two routing administration methods do not accept
`timeoutMs`.

When exactly one device is registered, device calls from an unselected legacy
connection route lazily to that sole device. With multiple devices, an
unselected call returns `device_selection_required`; with no registered device,
it returns `device_not_found`. Selecting an unknown ID also returns
`device_not_found` and preserves the connection's previous selection.

Protocol 1.1 adds the `request.control.v1` extension; it is not advertised when
the selected wire version is 1.0. After negotiation, `timeoutMs` is accepted on
`device.connect`, `device.disconnect`, `device.capabilities`, `device.observe`,
and `device.execute`. Protocol 1.4 extends the same request control to
`media.stream.capture`. `system.hello` cannot use it because negotiation has
not completed, and atomic Session/Event administration methods reject it explicitly.

Both `timeoutMs` and `device.execute.params.actionTimeoutMs` are positive
JavaScript-safe integer milliseconds. `timeoutMs` is the absolute budget for
request-scoped device work, including time before the Driver is called;
`actionTimeoutMs` starts after `actionStarted` is durable and covers only Driver
execution. When both apply, the earlier deadline wins. Once an Action has
started, terminal event finalization is shielded from caller cancellation so a
timeout can never leave a half-open Action; that bounded cleanup may finish
after the device-work deadline.

`request.cancel` accepts `{ "requestId": <RpcId> }` and returns the same ID with
one deterministic status: `requested`, `alreadyRequested`, or `notFound`. A
cancel request has its own RPC ID; it never reuses the target request ID.
Cancellation targets the concurrent device requests listed above. A completed
request reports `notFound`, even while its response ID remains reserved until
the response has entered the bounded output queue.

## Sessions and replayable events

Device operations that produce observations or action facts require an active Session:

1. `session.start` creates a new Session and appends sequence 1 (`sessionStarted`).
2. `device.observe` and `device.execute` record events correlated with the RPC request and device.
3. `events.list` accepts optional `sessionId`, `afterSequence`, and `limit` (1–1000); omitting `sessionId` uses the active Session. Bounded consumers advance `afterSequence` to the last returned event until a short or empty page is observed. Omitting `limit` preserves the original full-suffix behavior.
4. `session.end` appends the final `sessionEnded` fact and seals the log.
5. Legacy `session.export` returns `SessionInfo` plus the complete ordered event
   list. With negotiated `session.export.page.v1`, bounded consumers instead
   advance the returned `nextAfterSequence` until it is omitted.

Every event has a globally unique `eventId`, a typed `sessionId`, a one-based JavaScript-safe `sequence`, optional `requestId`/`deviceId`, `atMs`, and a nested `payload`. Action completion uses an explicit `succeeded`, `failed`, `cancelled`, or `timedOut` outcome. Sequence is authoritative for replay; timestamps are informational.

Session logs are append-only. `events.clear` never removes individual facts and rejects active Sessions; for compatibility with the early method name, it deletes one complete ended Session. Clients should prefer thinking of this operation as Session deletion.

## Connection state

```text
AwaitingHello -- successful system.hello --> Ready(negotiated context)
```

- Before `Ready`, any method other than `system.hello` returns `handshake_required`.
- A failed hello leaves the connection in `AwaitingHello` and can be retried.
- Calling `system.hello` again after success returns `handshake_already_completed`.
- Negotiated state belongs to one transport connection. `connectionId` is distinct from future device session or lease identifiers.

## Errors

JSON-RPC failures use a numeric envelope code and structured DeviceRail data:

```json
{
  "jsonrpc": "2.0",
  "id": "hello-1",
  "error": {
    "code": -32003,
    "message": "client and server do not share a protocol version",
    "data": {
      "code": "protocol_version_incompatible",
      "message": "client and server do not share a protocol version",
      "retryable": false,
      "details": {
        "reason": "clientTooOld",
        "clientProtocol": {
          "ranges": [{ "major": 0, "minMinor": 1, "maxMinor": 9 }]
        },
        "serverProtocol": {
            "ranges": [{ "major": 1, "minMinor": 0, "maxMinor": 5 }]
        }
      }
    }
  }
}
```

Stable mappings through DR-008 are:

| JSON-RPC code | DeviceRail code | Meaning |
| ---: | --- | --- |
| `-32700` | `parse_error` | Input is not valid JSON. |
| `-32600` | `invalid_request` | The JSON-RPC envelope is invalid or unsupported. |
| `-32601` | `method_not_found` | The requested method does not exist. |
| `-32602` | `invalid_params` | Method parameters cannot be validated. |
| `-32602` | `feature_not_negotiated` | A control field was used before its Feature was enabled. |
| `-32602` | `request_timeout_not_supported` | `timeoutMs` was used on a method that does not support request deadlines. |
| `-32603` | `internal_error` | The server failed unexpectedly. |
| `-32000` | Driver code | A routed Driver rejected or failed the operation. |
| `-32001` | `handshake_required` | `system.hello` has not completed. |
| `-32002` | `handshake_already_completed` | The connection is already in `Ready`. |
| `-32003` | `protocol_version_incompatible` | Client and server have no compatible wire version. |
| `-32004` | `required_feature_unsupported` | A required protocol feature is unavailable. |
| `-32005` | `session_required` | A recording operation requires an active Session. |
| `-32006` | Session/Event Store code | Session lifecycle or append-only storage rejected the operation. |
| `-32007` | `request_cancelled` | A concurrent device request observed explicit cancellation or shutdown. |
| `-32008` | `request_timed_out` / `action_timed_out` | The request or Driver-only Action budget elapsed. |
| `-32009` | `request_id_in_use` | The same RPC ID is already reserved by an unfinished response. |
| `-32010` | `too_many_requests` | The connection reached its bounded in-flight request limit. |
| `-32011` | `device_selection_required` / `device_not_found` | Device routing cannot resolve a route for this connection. |
| `-32012` | `response_frame_too_large` | The requested result cannot fit in one bounded NDJSON response frame. |

Clients should branch on `error.data.code`, not on human-readable message text. Breaking serialized changes require an explicit wire protocol version bump.

Action terminal events use their own stable nested codes:
`action_cancelled` and `action_timeout`. `session.end` returns the Store code
`session_busy` while an Action is in flight; the Session remains active and the
client may cancel or await the Action, then retry the end request.

## Bootstrap scope

The initial handshake intentionally excluded authentication, reconnection/resume, device discovery and leasing, push-event streams, compression, AI planning, and YAML configuration. Protocol 1.2 added discovery and connection-local selection; Protocol 1.3 added loopback resumable event streaming; Protocol 1.4 added Evidence-referenced media frames; Protocol 1.5 adds Evidence-referenced UI trees, semantic Action contracts, and caller-produced Verdict persistence. Remote authentication, compression, assertion/model execution, AI planning, and YAML remain outside the kernel protocol.
