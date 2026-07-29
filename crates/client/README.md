# DeviceRail Rust Client

`devicerail-client` is the official asynchronous Rust client for DeviceRail.
It uses the public request, result, event, error, and evidence DTOs from
`devicerail-protocol` directly; the crate does not maintain a second Rust wire
model and does not depend on Core, a platform Driver, or the daemon
implementation.

```sh
cargo add devicerail-client
```

## Control-plane connections

The control plane is bounded UTF-8 NDJSON carrying JSON-RPC 2.0
request/response envelopes. The client correlates concurrent out-of-order
responses by exact RPC ID, reserves request capacity for cancellation, checks
method Feature requirements before writing, and treats malformed framing or an
invalid response as a terminal connection failure.

Three high-level connection paths perform `system.hello` before returning:

- `DeviceRailClient::spawn(SpawnConfig)` starts and owns a daemon child over
  piped stdio. Closing the client drains accepted writes and performs bounded,
  cancellation-safe child and I/O task cleanup.
- `DeviceRailClient::attach(reader, writer, transport, hello, options)` takes
  caller-provided Tokio `AsyncRead` and `AsyncWrite` halves.
- `DeviceRailClient::connect_tcp(address, hello, options)` connects to an
  explicitly enabled stock loopback TCP listener. It rejects port zero and
  every non-loopback IPv4 or IPv6 address before opening a socket.

`attach` is the explicit escape hatch for caller-owned trusted tunnels,
authentication preludes, proxies, or transports with a different admission
policy; `connect_tcp` itself cannot be used to reach a remote network address.

`attach_unnegotiated` exists for a caller that must control the bootstrap
sequence itself. Until `hello()` succeeds, ordinary calls fail locally; a
failed compatible hello leaves the client awaiting another hello, while an
invalid server hello is terminal. It returns `RuntimeUnavailable` instead of
panicking when called without an active Tokio runtime. After negotiation,
`negotiated_hello()` returns the server-selected `HelloResult` without exposing
the client-side hello identity.

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

`default_hello()` offers the protocol versions supported by the linked
`devicerail-protocol` crate and the official client's supported optional
Features. Use an explicit `HelloParams` when a host needs required Features or
a different client identity. The negotiated server, protocol version,
transport, connection ID, and enabled Feature set remain the authoritative
connection context.

### HMAC admission limitation

The built-in TCP and generic attach paths currently begin at NDJSON JSON-RPC.
They do **not** implement the optional `devicerail-remote-auth`
length-prefixed HMAC pre-hello exchange. Consequently, `connect_tcp` cannot
directly attach to a stock listener configured with
`DEVICERAIL_RPC_CREDENTIALS`. Do not send credentials as JSON-RPC parameters or
weaken the daemon's pre-hello admission boundary.

### Sensitive diagnostics

`SpawnConfig` deliberately redacts argument values, environment values, and
the complete hello offer from `Debug`. `DeviceRailClient::stderr_tail()` is a
separate, explicitly sensitive diagnostic interface: it returns a lossily
decoded view of bounded raw child stderr, which may contain credentials, paths,
or device data. The client never adds that tail to ordinary errors or `Debug`;
callers must restrict and redact it before logging or persistence.

## Typed methods and cancellation

`methods` contains one sealed marker for every public RPC method. Each marker
binds its canonical `devicerail-protocol` parameter/result pair, channel,
required Feature, and timeout support:

```rust
use devicerail_client::{CallOptions, DeviceRailClient, methods};
use devicerail_client::protocol::RequestTimeoutMs;

async fn observe(
    client: &DeviceRailClient,
) -> Result<(), devicerail_client::ClientError> {
    let observation = client
        .call::<methods::DeviceObserve>(
            methods::NoParams,
            CallOptions {
                timeout_ms: Some(RequestTimeoutMs::new(15_000).expect("valid timeout")),
            },
        )
        .await?;
    println!("{}", observation.device_id);
    Ok(())
}
```

Use `begin_call()` when the RPC ID or explicit remote cancellation is needed.
`RequestHandle::cancel_remote()` sends `request.cancel` without reusing the
target request ID, and `result()` resolves to the method's concrete result
type. Request timeouts and remote cancellation require negotiated
`request.control.v1`. Dropping a handle or cancelling its result future moves
the request into a bounded late-response tombstone set; when request control is
available, the client also schedules a best-effort remote cancellation. A
validated late response clears the tombstone, while exceeding the bounded
late-response budget fails the connection closed.

## Caller-driven pagination

Pagination is deliberately caller-driven. The client does not automatically
fetch or aggregate pages, so applications retain control over cancellation,
memory use, retry policy, and progress persistence.

- For `events.list`, pass the last returned event's `sequence` as the next
  exclusive `afterSequence`. An empty page completes the current read.
- For paged `session.export`, pass each response's `nextAfterSequence` as the
  next request's `afterSequence` and continue until `nextAfterSequence` is
  absent.

DeviceRail event sequences are one-based, while persisted consumer watermarks
often use `0` to mean “nothing consumed yet.” Use
`after_sequence_from_watermark(watermark)` to convert safely: `0` becomes
`None` (omit `afterSequence`), a positive JavaScript-safe integer becomes an
`EventSequence`, and an out-of-range value returns `WatermarkError` instead of
silently restarting from the beginning.

```rust
use devicerail_client::after_sequence_from_watermark;
use devicerail_client::protocol::EventsListParams;

let params = EventsListParams {
    after_sequence: after_sequence_from_watermark(persisted_watermark)?,
    ..EventsListParams::default()
};
```

## Confirmed and resumable event streams

With `events.stream.v1` negotiated,
`open_event_stream(EventsSubscribeParams, EventStreamOptions)` obtains a
short-lived single-use capability over the control connection and opens the
separate loopback WebSocket data plane. The WebSocket performs its own hello at
the exact control-plane protocol version and then sends one typed
`events.subscribe`.

```rust
use devicerail_client::{
    DeviceRailClient, EventStreamOptions,
    protocol::EventsSubscribeParams,
};

async fn consume(
    client: &DeviceRailClient,
    subscribe: EventsSubscribeParams,
) -> Result<(), devicerail_client::EventStreamError> {
    let mut stream = client
        .open_event_stream(subscribe, EventStreamOptions::default())
        .await?;

    while let Some(item) = stream.next().await? {
        // Persist the event before advancing durable application progress.
        persist(&item.event).await?;
        stream.confirm(&item.cursor)?;
    }

    println!("terminal: {:?}", stream.terminal());
    Ok(())
}
```

Receipt, delivery, and confirmation are separate states:

- the socket actor advances `received_cursor` only after validating a message;
- `next()` advances `delivered_cursor` and yields a typed event item.
- `confirm(&cursor)` advances durable application progress only in contiguous
  order, after the application has accepted the event.
- `cancel().await` terminates local stream work explicitly.
- after a stream is finished and drained, `resume(options)` obtains a new
  single-use capability from only the last confirmed cursor.

`EventStreamOptions` applies one bounded setup deadline across capability
issuance, TCP connect, WebSocket upgrade, hello, and subscription; WebSocket
writes and explicit actor shutdown have separate bounded deadlines. If graceful
shutdown exceeds its allowance, the actor is aborted, and dropping a stream
also aborts any remaining actor instead of detaching it.

When application policy classifies a finished stream as resumable:

```rust
let resumed = stream.resume(EventStreamOptions::default()).await?;
```

The stream exposes its received, delivered, and confirmed cursors and its typed
terminal state. Socket receipt or server `lastEmittedCursor` never counts as
application acknowledgement. Active, undrained, cross-Session, stale-epoch,
ahead, and too-old resume attempts fail explicitly.

## Contract consistency

The client is checked against the canonical Golden Fixture manifest under
`crates/protocol/fixtures`. Its consistency tests cover canonical hello,
devices, connect, success, and failure envelopes; the typed protocol re-export;
attach-time hello; out-of-order response correlation; terminal unknown/null
response IDs; local Feature/timeout admission; and strict fragmented NDJSON,
UTF-8, byte-limit, and EOF behavior. Protocol DTO changes must still update the
generated Schema and Golden Fixtures at their source.

The published `devicerail-client` crate includes these tests. Its development
dependency enables the non-default `devicerail-protocol/fixtures` feature,
which embeds fixture bytes directly from the canonical protocol source rather
than reading a sibling repository path. The consistency suite also requires
the embedded path catalog to exactly match the canonical manifest, so the
workspace and packaged test suite cannot silently drift apart.

From the repository root:

```sh
cargo test -p devicerail-client
cargo test -p devicerail-protocol --test golden_fixtures
cargo run -p devicerail-schema-gen -- --check
```
