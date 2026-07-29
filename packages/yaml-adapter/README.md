# `@devicerail/yaml-adapter`

Optional DR-045 compatibility adapter. It parses a deliberately bounded,
JSON-shaped YAML plan and compiles it into sequential calls on an already
owned, already-negotiated `@devicerail/client` instance.

The adapter is not a workflow engine. It has no Driver, daemon, Rust, AI,
prompt, retry, branching, interpolation, or filesystem authority. YAML never
enters the DeviceRail kernel.

```yaml
version: devicerail/v1
steps:
  - id: select
    method: device.select
    params:
      deviceId: android-emulator-5554
  - id: connect
    method: device.connect
    params: {}
  - id: tap
    method: device.execute
    params:
      id: 11111111-1111-4111-8111-111111111111
      name: tap
      arguments: {x: 20, y: 40}
```

`device.execute` requires a caller-supplied, device-aware action-protection
classifier and a fixed selected device (from `device.select` or
`initialDeviceId`). Unknown and protected actions are rejected at compile time.
Immediately before execution the adapter re-selects that device, verifies the
connection-local route through `devices.list`, and re-reads
`device.capabilities`; a missing or newly protected action fails before its
arguments are sent. The verified `device.execute` call is started in the same
JavaScript continuation as that final capability check. A client used for YAML
execution must not be shared with non-adapter calls that can race
`device.select`. The caller remains responsible for device and Session lifecycle.

Within the YAML adapter's closed method allowlist, `timeoutMs` is accepted only
for these five request-controlled methods: `device.capabilities`, `device.connect`, `device.disconnect`,
`device.execute`, and `device.observe`. An execution `AbortSignal` is passed only
to those methods; for all other methods it is observed between sequential steps,
matching the public client's cancellation contract.

Parsing uses the `js-yaml` JSON schema and then applies independent byte,
depth, node, collection, key, number, duplicate-ID, method, and timeout bounds.
Custom tags, object aliases/cycles, prototype keys, duplicate YAML keys,
unknown plan fields, `system.hello`, `events.subscribe`, and direct
`request.cancel` calls fail closed. Execution also requires the opaque,
immutable plan returned by `compileYamlPlan` rather than accepting a manually
constructed lookalike. Trust is tracked in a module-private `WeakSet`, so
copying symbols or inheriting from a valid plan does not authorize a new plan.
