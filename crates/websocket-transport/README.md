# DeviceRail WebSocket transport

`devicerail-websocket-transport` is the loopback data-plane adapter for
Protocol 1.3/1.4 `events.stream.v1`. It depends on Core's transport-neutral event
subscription and keeps RFC 6455, HTTP headers, bearer capabilities, queues,
and socket shutdown out of the Rust device kernel.

The stdio control plane calls `events.stream.open` and returns a 30-second,
single-use, Session-scoped capability. The server binds only
`127.0.0.1`, requires the exact numeric `Host`, `/v/<64 lowercase hex>` path,
`devicerail.events.v1` subprotocol, and the capability's absent-or-exact
Origin policy. Compression offers are not negotiated. Handshake headers,
frames, messages, connection count, pending capabilities, replay, queued
events, serialized bytes, queue stall time, writes, and shutdown all have hard
limits.

Each accepted socket performs `system.hello`, then exactly one
`events.subscribe`. A single writer drains one event/byte-bounded queue. Core
lag, queue stall, oversize event, Session deletion, sequence corruption, and
server shutdown become typed terminal notifications; events are never dropped,
merged, or reordered. `lastEmittedCursor` records only writes completed by the
server. Application acknowledgement remains an explicit TypeScript-client
operation.

The selected 1.3/1.4 protocol version is retained by the connection and feeder.
Protocol 1.3 never serializes Protocol 1.4 media lifecycle DTOs: encountering one
terminates the stream explicitly before that sequence. Protocol 1.4 transports
the complete ordered lifecycle.

Run:

```sh
cargo test -p devicerail-websocket-transport
cargo clippy -p devicerail-websocket-transport --all-targets -- -D warnings
```

Hermetic runners that forbid all `AF_INET` binds may explicitly set
`DEVICERAIL_ALLOW_NO_LOOPBACK=1`; tests that require a real loopback listener
then skip only on `PermissionDenied`. Without that acknowledgement a bind
failure is a test failure, so platform CI cannot silently lose WebSocket
coverage. Header authorization, lifecycle, serialization, and queue-budget
tests do not require a network socket.
