# DeviceRail Python Client

`devicerail-client` is the typed Python 3.11+ client for the DeviceRail v1 wire
protocol. Its `TypedDict`, union, `Literal`, method-map, and overload declarations
are generated from the checked-in files under `protocol/schema/v1`; Python does
not maintain a second handwritten DTO model.

The runtime client spawns a daemon without a shell, negotiates `system.hello`,
and exchanges byte-bounded UTF-8 NDJSON over stdio. Every outbound request and
inbound response is checked against the packaged generated Schema. Envelope
fields, JSON-RPC IDs, safe integer bounds, duplicate object keys, frame size,
method features, and camelCase parameter names are rejected explicitly.

When Protocol 1.4 negotiates `session.export.page.v1`, `session.export` accepts
`limit` (1–1000) and optional `afterSequence`; callers follow
`nextAfterSequence` until the final page omits it. The legacy request and full
result remain available, and paged parameters fail locally if the Feature was
not negotiated.

Protocol 1.5 adds the feature-gated `ui.snapshot.get` and `verdict.record`
methods plus canonical semantic element Action names. The client rejects both
methods, and rejects `findElement`, `tapElement`, `clearElement`,
`setElementValue`, or `waitForElement` through `device.execute`, before writing
when the corresponding 1.5 Feature was not negotiated. UI Snapshot lookup is
scoped by the daemon to the active Session and accepts only an Observation ID;
it is not a general Evidence reader.

When `media.stream.v1` is negotiated, call `media.stream.start`, then capture
one-based frames, then end the stream:

```python
stream_id = "00000000-0000-4000-8000-000000000001"
await client.call("media.stream.start", {"kind": "screenshot", "streamId": stream_id})
captured = await client.call(
    "media.stream.capture",
    {"frameIndex": 1, "streamId": stream_id},
    timeout_ms=15_000,
)
await client.call("media.stream.end", {"streamId": stream_id})
```

An identical `frameIndex` retry is for a lost response; conflicting metadata
fails. Video captures require a positive `durationMs` and represent timed PNG
key frames, not an encoded video container. All three methods fail locally if
the Feature was not negotiated.

`stderr_tail` is a bounded view of raw, untrusted child-process diagnostics. It
is never appended to client exceptions because a platform tool can accidentally
write protected input there. Treat it as potentially sensitive and do not log
it automatically; inspect it only through an explicit diagnostic workflow.

```python
import asyncio

from devicerail import DeviceRailClient


async def main() -> None:
    async with await DeviceRailClient.spawn("devicerail-daemon") as client:
        devices = await client.call("devices.list")
        await client.call("device.select", {"deviceId": devices["devices"][0]["id"]})
        await client.call("device.connect", timeout_ms=15_000)
        observation = await client.call("device.observe", timeout_ms=15_000)
        print(observation["deviceId"])


asyncio.run(main())
```

Protocol 1.1's `request.control.v1` feature gates request timeouts and remote
cancellation. Cancelling a Python task sends `request.cancel` for methods whose
request Schema supports `timeoutMs`; `begin_call()` also returns a typed
`RequestHandle` with an explicit `cancel()` operation. Its result is a repeatable,
cancellation-isolated awaitable: cancelling one of several active waiters does
not cancel the shared wire request or the surviving waiters. Cancelling
`close()` delays that cancellation until the bounded subprocess and pipe cleanup
has finished.
`events.subscribe` is
present in the generated public `RpcMethodMap`, but the stdio client rejects it
because it is reserved for the event WebSocket handshake. Use
`events.stream.open` to obtain that endpoint.

Development gates, from this directory:

```console
python -m pip install -e ".[dev]"
python scripts/generate.py --check
python -m mypy --config-file pyproject.toml typing/contract.py
python -W error -m unittest discover -s tests -v
python -m build
python scripts/check_distribution.py dist
```

The distribution check imports the wheel from its ZIP path in an isolated
interpreter, reads every packaged Schema, rebuilds the sdist without build
isolation or downloads, and applies the same runtime import check to that wheel.

Run `python scripts/generate.py` only after the Rust schema generator has
updated `protocol/schema/v1`.
