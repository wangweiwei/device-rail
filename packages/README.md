# Language packages

Language packages consume the public wire protocol and must not import Rust
driver internals. TypeScript and Python types are generated from the same
checked-in Schema boundary.

- `protocol`: generated, type-only Protocol 1.5 models sourced from the
  checked-in JSON Schema and verified against every Golden Fixture.
- `client`: typed Node.js stdio client with handshake and Feature negotiation,
  bounded NDJSON framing and writes, concurrent response correlation,
  cancellation, graceful drain, bounded stderr diagnostics, and an
  explicitly confirmed/resumable loopback WebSocket event stream.
- `tool-adapter`: provider-neutral immutable Tool Catalogs over the typed
  client, with portable Action names, an explicit observation tool, structured
  results/evidence, protected-Action filtering, and host-owned device/Session
  lifecycle.
- `recorder`: sequence-authoritative, resumable `TestEvent` capture with
  durable local checkpoints and a strict handoff to the existing Rust Session
  Bundle CLI. Negotiated Protocol 1.4 export pages allow Sessions larger than
  one stdio response to seal while preserving legacy servers; it never imports
  Drivers or reads Evidence bytes itself.
- `live-visualizer`: bounded, immutable presentation snapshots over public
  `TestEvent` values. It reserves an event before acknowledgement, publishes it
  only after explicit confirmation, and removes every Evidence URI.
- `playwright-driver`: private one-shot bridge for the Rust Playwright Remote
  Driver. It uses pinned `playwright-core` only to connect to an existing
  operator-managed server; it never downloads or launches a browser.
- `python-client`: Python 3.11+ async stdio client whose typed method map,
  models, and packaged runtime schemas are generated from the same public
  protocol manifest and checked against every Golden Fixture.
- `yaml-adapter`: optional, bounded `devicerail/v1` YAML compiler that emits
  sequential calls through the public client. Its fixed method allowlist,
  duplicate-key/alias/prototype checks, resource budgets, and mandatory
  device-bound protection classifier keep YAML outside the kernel and prevent
  protected action arguments from being persisted in a plan. Execution
  reselects the compiled route and refreshes capabilities immediately before
  each `device.execute`; only plans created in the current process are trusted.
- Session Bundle v1 is intentionally a local Rust library/CLI workflow rather
  than a new RPC or TypeScript archive implementation. Recorder and future
  report packages consume its validated event/evidence contract.

`apps/live-visualizer` is a private Node.js host rather than a publishable
protocol package. It attaches to an already-owned, already-negotiated client,
serves a capability-scoped loopback UI, and never owns device or Session
lifecycle.
