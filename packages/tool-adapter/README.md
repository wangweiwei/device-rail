# `@devicerail/tool-adapter`

Provider-neutral DeviceRail Tool Catalogs for Node.js 22 and later.

The package converts the selected device's `device.capabilities` response into
an immutable catalog. It adds one explicit observation tool and maps every
portable tool name back to the original Driver action. It does not import an
AI SDK, translate tools into a provider-specific format, or own device and
Session lifecycle.

```ts
import { DeviceRailClient } from "@devicerail/client";
import {
  actionToolName,
  DeviceRailToolAdapter,
} from "@devicerail/tool-adapter";

const client = await DeviceRailClient.spawn({ command: "devicerail-daemon", hello });

// Lifecycle remains explicit and host-owned.
await client.call("device.select", { deviceId: "android-1" });
await client.call("device.connect");
await client.call("session.start");

const catalog = await new DeviceRailToolAdapter(client).discover();
const result = await catalog.invoke(
  {
    arguments: { x: 320, y: 240 },
    invocationId: "agent-call-1",
    name: actionToolName("tap"),
  },
  { actionTimeoutMs: 5_000, requestTimeoutMs: 10_000 },
);

if (result.kind === "action") {
  console.log(result.action.after, result.action.evidence);
}
```

`beginInvoke()` additionally exposes the exact RPC request ID and a typed
`cancel()` operation. Agent/provider invocation IDs remain metadata; the
adapter always generates a distinct UUID for the DeviceRail Action call.
Remote RPC and Driver failures reject unchanged rather than becoming textual
tool success results.

Results from injected client-compatible implementations cross the same
canonical method-result Schema boundary as the stock client before the
adapter applies its stricter Action, Observation, Evidence, and protected-data
semantics.

Action schemas must be pure JSON with `type: "object"` at the root. During
discovery, the adapter performs defensive shape checks for common JSON Schema
keywords and then deeply freezes an isolated schema snapshot. It accepts Draft
6, Draft 7, Draft 2019-09, Draft 2020-12, and compound schema documents with
nested resources.

The adapter does not fetch schemas, resolve references, select a dialect, or
claim complete meta-schema validation. `$id`, `$ref`, `$dynamicRef`, and
`$recursiveRef` values are preserved for the Driver/daemon validation boundary;
remote and currently unresolved references do not cause network access during
tool discovery.

Protected actions are excluded from catalogs by default. A host that
deliberately wants to expose actions such as Android `inputSecret` must both
negotiate `action.protected.v1` during `system.hello` and opt the adapter in:

```ts
// The client's hello.features.optional must include "action.protected.v1".
const catalog = await new DeviceRailToolAdapter(client, {
  includeProtectedActions: true,
}).discover();
```

Opting in makes the protected argument visible to the Agent/provider and to
the JavaScript process. DeviceRail redacts its durable Action event and omits
the protected before/after screenshots, but the adapter cannot prevent an
upstream model provider, host logger, operating system, or process-memory
inspector from retaining the value.
