# @devicerail/client

Typed Node.js client for the DeviceRail public protocol. The stdio/NDJSON
connection is the control plane and supports bounded writes, concurrent
response correlation, request cancellation, and graceful shutdown.

Every inbound method response is validated as a complete JSON-RPC envelope
against its canonical generated response Schema before the pending request is
settled. A Schema violation is terminal and rejects all pending work. The
response Schemas are generated into the published client, and their validators
are compiled once and cached for the process.

The stock stdio/NDJSON and WebSocket receive paths also apply a fixed
pure-JSON defensive complexity budget before Schema validation: at most
100,000 visited JSON values and a maximum nesting depth of 256. This budget is
independent of the transports' byte limits, so an exceptionally complex
message can be rejected before it reaches the configured byte ceiling.

Protocol 1.4's optional `session.export.page.v1` keeps legacy
`session.export({sessionId})` unchanged. When the Feature is enabled, callers
may add `limit` (1–1000) and optional `afterSequence`, then follow the returned
`nextAfterSequence` until it is omitted. Both the Node and Python clients reject
paged parameters locally when the Feature was not negotiated.

Protocol 1.5 exposes three boundaries with one explicit dependency. The client
requires `observation.uiSnapshot.v1` before `ui.snapshot.get` and
`device.semanticActions.v1` before sending any of the five canonical semantic
Action names through `device.execute`. Semantic Actions may be negotiated only
together with UI Snapshot; an enabled set containing
`device.semanticActions.v1` without `observation.uiSnapshot.v1` makes
`system.hello` fail with JSON-RPC error `-32004` and
`data.code = feature_dependency_unsatisfied`. `verdict.record.v1` remains
independently negotiable and is required before `verdict.record`. UI Snapshot
reads are scoped to an Observation in the current active Session.
`verdict.record` persists a caller-produced Verdict; the client and daemon do
not infer pass/fail or run model-based assertions.

When `media.stream.v1` is negotiated, the typed production lifecycle is:

```ts
const streamId = crypto.randomUUID();
await client.call("media.stream.start", { kind: "screenshot", streamId });
const captured = await client.call(
  "media.stream.capture",
  { frameIndex: 1, streamId },
  { timeoutMs: 15_000 },
);
await client.call("media.stream.end", { streamId });
```

Reuse the same `frameIndex` only to retry an identical capture whose response
was lost. A video capture additionally requires a positive `durationMs`; it
produces a timed PNG key frame rather than an encoded video container. The
client rejects all three calls locally unless the Feature was negotiated.

When Protocol 1.3 negotiates `events.stream.v1`, `openEventStream()` obtains a
short-lived loopback capability over stdio and opens the separate WebSocket
data plane:

```ts
const stream = await client.openEventStream({ sessionId });

for await (const item of stream) {
  await persist(item.event);
  const durableCursor = item.confirm();
  await saveCursor(durableCursor);
}
```

Receiving or yielding an event does not confirm it. `item.confirm()` must be
called once, in contiguous sequence order, after the application has durably
accepted the event. If the socket fails, `stream.resume()` requests a fresh
single-use capability from only the last confirmed cursor. Abort, local queue
overflow, cursor mismatch, remote terminal states, and early socket closure
surface as explicit typed errors.

The WebSocket offers exactly the protocol version selected by the control
connection. Protocol 1.3 rejects 1.4 media lifecycle payloads; Protocol 1.4
accepts them without weakening strict event validation.

The default Node WebSocket sends no browser Origin. Browser callers must issue
the capability with one exact canonical `http(s)://127.0.0.1:<non-default-port>`
Origin policy. Remote hosts, ambient credentials, compression, and remote
authentication are not part of this loopback transport.
