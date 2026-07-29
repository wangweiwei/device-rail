# AGENTS.md

Canonical instructions for coding agents in DeviceRail.

## Architecture rules

- Keep the Rust device kernel free of AI SDKs, prompt logic, YAML runtimes, and UI dependencies.
- Treat the wire protocol as the cross-language boundary. Rust traits are internal implementation details.
- Keep dependency direction one-way: `protocol -> core -> drivers -> daemon/clients -> apps`.
- Platform drivers implement capabilities; they must not depend on recorder, visualizer, or product apps.
- Recorder and visualizer communicate through `TestEvent`, `Observation`, `ActionResult`, and evidence references.
- Return explicit errors. Do not silently return empty observations, actions, or evidence.
- Add tests whenever protocol behavior, driver behavior, or serialization changes.
- Every Driver crate must run the shared conformance suite through
  `driver_conformance_test!`; platform-only tests supplement rather than replace it.

## Commands

- Format: `cargo fmt --all`
- Check: `cargo check --workspace`
- Test: `cargo test --workspace`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Generated protocol check: `cargo run -p devicerail-schema-gen -- --check`
- Golden Fixture check: `cargo test -p devicerail-protocol --test golden_fixtures`
- TypeScript workspaces use pnpm only.

## Protocol changes

- Protocol fields use `camelCase` on the wire.
- Every breaking wire change must bump the explicit protocol version.
- After changing a public DTO, regenerate `protocol/schema/`, update the matching
  entry under `crates/protocol/fixtures/`, and run both protocol checks above.
- Do not expose platform-library-specific types in protocol DTOs.
- Prefer evidence references over embedding large binary payloads in JSON.
