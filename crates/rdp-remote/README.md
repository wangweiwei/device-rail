# DeviceRail RDP Remote Driver

`devicerail-rdp-remote` adapts a long-running RDP bridge to DeviceRail's
`DeviceDriver` contract. The Rust device kernel does not embed an RDP stack or
credentials. A separately managed bridge owns the RDP session and exposes a
small, versioned JSON-over-TCP interface on a loopback socket for probing the
desktop, capturing a PNG frame, and injecting typed input.

The bridge endpoint and the RDP target are validated independently. Endpoint
authentication uses a bounded opaque token sent inside the request body. The
built-in adapter rejects non-loopback bridge addresses because this framing is
not TLS; the token is redacted from `Debug` and never enters observations,
action results, or errors.
Each operation opens a short-lived bridge connection while the bridge keeps the
remote session under the stable DeviceRail device id.

Supported DeviceRail actions are atomic `tap`, `pointerMove`, `scroll`,
`keyPress`, `typeText`, and protected `inputSecret`. There is deliberately no
stateful pointer-down action that could survive a lease boundary. Screenshots are persisted
only through the Session-scoped Evidence Store and observations contain the
resulting canonical evidence reference.

The crate includes an injectable `RdpBridge` boundary, deterministic fake
bridge tests, the shared Driver conformance suite, and protocol framing tests.
It does not download or start an RDP server or bridge.

Bridge protocol v2 keeps the socket open until a terminal response. Closing
the socket is a mandatory cancellation signal: a conforming bridge must stop
uncommitted work before accepting the device for another request. Every
request has an `operationId`; input requests additionally carry the original
DeviceRail `callId`. A bridge must cache the terminal input result by
`(deviceId, callId)` so a retry cannot apply input twice. `inputSecret` is a
distinct wire kind and must not be logged, echoed, or captured by the bridge.
On an ambiguous transport loss the client retries exactly once with the same
`callId`; a second ambiguous loss becomes non-retryable
`rdp_input_indeterminate`, preventing blind duplicate input.
The complete request/response contract is checked in
`protocol/bridge-v2.schema.json`; canonical frames live in
`protocol/fixtures/`.
