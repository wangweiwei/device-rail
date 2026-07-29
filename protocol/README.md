# Protocol artifacts

The Rust types in `crates/protocol` are the source of truth for the wire contract. Generated JSON Schema and Golden Fixtures let TypeScript, Python, and other clients consume the same contract without duplicating Rust DTOs manually.

## JSON Schema

Versioned Draft 2020-12 documents live under `protocol/schema/v1/`. Its `manifest.json` enumerates all 174 Protocol 1.5 public schemas with stable names and IDs, including request/response contracts for all 24 methods and the two closed stream notifications.

```sh
cargo run -p devicerail-schema-gen -- write
cargo run -p devicerail-schema-gen -- --check
```

The check fails on missing, changed, or stale generated files and runs in CI. `ActionDefinition.inputSchema` intentionally remains an unconstrained JSON value because each Driver supplies the action-specific JSON Schema.

## TypeScript

`packages/protocol` generates a type-only `@devicerail/protocol` package from
the Schema manifest. It writes one module per public root type and exposes both
the package root and `/v1` entry points. The generated fixture contract checks
all 89 examples with TypeScript `satisfies`; it does not use type assertions.

```sh
pnpm protocol:types:generate
pnpm protocol:types:check
pnpm protocol:types:test
pnpm protocol:types:build
```

## Golden Fixtures

The 89 cross-language fixtures live under `crates/protocol/fixtures/`. Their `manifest.json` declares wire protocol version, model, fixture path, and matching schema. Rust integration tests require lossless typed round trips, complete 24-method request/success-response coverage, safe paths, stream notifications, and all `TestEvent` variants.

Protocol requirements:

- JSON field names use `camelCase`.
- Requests and responses carry caller-provided IDs.
- Protocol 1.1 keeps compatibility with 1.0 and adds positive JavaScript-safe
  device-request/action timeouts plus typed `request.cancel` fixtures under
  `request.control.v1`; Protocol 1.4 extends request timeout/cancellation to
  `media.stream.capture`. `system.hello` and atomic administrative methods do
  not accept request deadlines.
- Protocol 1.2 keeps compatibility with 1.0 and 1.1 and adds strict
  `devices.list` / `device.select` request and response artifacts under
  `device.routing.v1`; device selection is connection-local.
- Protocol 1.2 also gates protected Actions behind `action.protected.v1`,
  records their arguments explicitly redacted, and represents screenshot
  omission with a typed reason.
- Protocol 1.3 adds epoch- and Session-bound resumable cursors, single-use
  loopback stream capabilities, typed event/terminal notifications, and a
  WebSocket hello baseline under `events.stream.v1`.
- Protocol 1.4 adds `media.stream.v1`, the `media.stream.start`,
  `media.stream.capture`, and `media.stream.end` control methods, and closed
  screenshot/video lifecycle events. Capture accepts a client retry index but
  never bytes, paths, or client-supplied Evidence references; frames contain
  only daemon-produced canonical Evidence references. The version also publishes the standalone v1
  `ManualRecording` model; protected arguments remain host-resolved opaque
  references rather than durable values.
- Protocol 1.4 also adds optional `session.export.page.v1`. It preserves the
  legacy complete export shape, while feature-negotiated `{limit,
  afterSequence?}` requests expose stable ended-Session pages with an explicit
  `nextAfterSequence` continuation.
- Protocol 1.5 adds `observation.uiSnapshot.v1`,
  `device.semanticActions.v1`, and `verdict.record.v1`. UI trees are bounded,
  typed Evidence referenced from Observations and readable only from the
  caller's active Session. The five canonical semantic Actions still use
  `device.execute`, while `verdict.record` only validates and persists a
  caller-produced Verdict; it does not run assertions or invoke a model.
- Actions describe their input with JSON Schema.
- Binary evidence is referenced, not embedded in routine JSON messages.
- Errors contain a stable code, human-readable message, retryability, and optional details.
- Events share one Session envelope and a continuous one-based sequence; Action terminal outcomes are explicit.
- Session logs are append-only and can only be deleted as a complete ended Session.
- Breaking changes require a protocol-version bump.
