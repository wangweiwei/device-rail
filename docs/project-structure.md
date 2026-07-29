# Project structure

DeviceRail is a Rust, TypeScript, and Python monorepo. The top-level layout
follows the one-way dependency rule:

```text
protocol -> core -> drivers -> daemon
    |
    +-----> official clients -> apps
```

The arrows describe compile-time contract flow. Clients communicate with the
daemon over the public wire protocol; they do not link the daemon or Driver
implementation.

Moving these directories would break public paths, workspace manifests,
release scripts, and generated-code checks. Directory optimization therefore
means clear ownership and navigation rather than flattening stable components.

## Top-level directories

```text
device-rail/
├── .github/       CI, release workflows, and collaboration templates
├── apps/          private end-user or operator applications
├── crates/        Rust protocol, client, runtime, Driver, and daemon crates
├── docs/          cross-cutting architecture and maintenance documentation
├── packages/      TypeScript packages and the Python client
├── packaging/     deterministic release archive and verification tooling
├── protocol/      checked-in generated public JSON Schema
├── scripts/       repository-wide package verification helpers
└── visualizer/    shared visualizer fixtures
```

Generated or local-only directories such as `target/`, `node_modules/`,
`.pnpm-store/`, `.devicerail/`, `.mypy_cache/`, and package `dist/` outputs are
ignored. Applications and tests build their required JavaScript from source;
compiled `dist/` and `.test-dist/` trees are not source artifacts.

## Rust workspace

Rust crates are grouped by responsibility without adding circular dependency
layers:

- **Wire contracts and official Rust client:** `protocol`, `client`. The client
  depends directly on the public protocol DTOs and does not link Core, a
  platform Driver, or the daemon implementation.
- **Runtime contracts:** `core`.
- **Platform Drivers:** `android-adb`, `ios-webdriver`, `harmony-hdc`,
  `desktop-driver`, `rdp-remote`, `playwright-remote`.
- **Platform host lifecycle:** `ios-host` (device doctor, merged physical-device
  and Simulator discovery, transport-specific Direct WDA build/run, and
  optional managed Appium process; kept above the iOS Driver boundary).
- **Extension Drivers:** `plugin-driver`, `distributed-router`, `driver-mock`.
- **Evidence and presentation:** `evidence-fs`, `session-bundle`, `bundle-cli`,
  `visualizer`, `manual-recording`.
- **Transport and process entry points:** `websocket-transport`, `remote-auth`,
  `daemon`.
- **Build-time generation:** `schema-gen`.

Platform Drivers must implement capabilities and the shared conformance suite;
they must not depend on recorder, visualizer, AI, YAML, or product applications.

## Language packages

- `crates/client` owns the official asynchronous Rust NDJSON/JSON-RPC client,
  daemon spawn, stdio/TCP attachment, hello negotiation, and confirmed/resumable
  event-stream behavior.
- `packages/protocol` is generated from `protocol/schema/v1`.
- `packages/client` owns Node.js stdio and event-stream client behavior.
- `packages/python-client` is generated from the same public Schema.
- `packages/tool-adapter` maps capabilities to provider-neutral AI tools.
- `packages/recorder` consumes public events and hands off to Session Bundles.
- `packages/live-visualizer` creates bounded presentation DTOs.
- `packages/yaml-adapter` is an optional public-call compiler above the client.
- `packages/playwright-driver` is a private bridge owned by the Rust Driver.

## Where new work belongs

| Change | Location |
|---|---|
| Public DTO or RPC behavior | `crates/protocol`, then regenerate `protocol/schema/` and language types |
| Runtime contract shared by Drivers | `crates/core` |
| Platform-specific capability | owning Driver crate |
| Transport framing | transport crate or daemon, never core |
| Rust client behavior | `crates/client` |
| TypeScript/Python client behavior | owning package |
| Recorder or visual presentation | package/application above the protocol boundary |
| Release archive/signature behavior | `packaging/` and `.github/workflows/release.yml` |
| Cross-cutting explanation | `docs/` plus links from both root READMEs |
