# `@devicerail/protocol`

Generated TypeScript types for the DeviceRail wire protocol.

The checked-in Draft 2020-12 documents under `protocol/schema/v1/` are the
source of truth. Do not edit `src/generated/` or `test/fixtures.generated.ts`
by hand.

```bash
pnpm protocol:types:generate
pnpm protocol:types:check
pnpm protocol:types:test
pnpm protocol:types:build
```

The package is type-only. JSON Schema remains the runtime authority for
constraints that TypeScript cannot express, including integer ranges,
formats, exact `oneOf` exclusivity (including the `RpcResponse` result/error
XOR), and additional-property rules. Runtime clients must validate those
constraints before trusting external JSON.

Protocol 1.4 includes the `events.stream.v1` bootstrap, subscribe, cursor, and
server-notification models. `RpcResponse` remains response-only;
`RpcServerMessage` is the separate WebSocket response/notification union.
The same generated method map includes `media.stream.start`,
`media.stream.capture`, and `media.stream.end`; capture is the only media
method whose request supports `timeoutMs`.

Protocol 1.5 adds generated contracts for bounded Evidence-backed UI trees,
`ui.snapshot.get`, the five canonical semantic Actions, their explicit
execution channel metadata, and `verdict.record`. The semantic Actions remain
ordinary `device.execute` calls; Action availability still comes from the
selected Driver's advertised capabilities.
