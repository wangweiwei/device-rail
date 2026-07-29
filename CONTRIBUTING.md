# Contributing to DeviceRail

Thank you for helping improve DeviceRail. Contributions can include protocol
tests, Driver implementations, bug fixes, performance work, documentation, and
real-platform validation reports.

## Before you start

- Search existing issues and pull requests before opening a duplicate.
- Use an issue for substantial protocol, architecture, or security-boundary
  changes so the compatibility impact can be discussed first.
- Never post credentials, device identifiers, private endpoints, signing
  material, screenshots containing personal data, or vulnerability details.
- Read [AGENTS.md](AGENTS.md) and [the architecture](docs/architecture.md).

## Development setup

Required toolchains:

- Rust 1.85 or the pinned `rust-toolchain.toml` toolchain;
- Node.js from `.nvmrc` and pnpm 9.3+;
- Python 3.11+ for Python client or release-packaging changes.

```sh
pnpm install --frozen-lockfile
cargo check --workspace
```

Platform SDKs are optional unless the change affects that platform. DeviceRail
does not install ADB, HDC, desktop helpers, Playwright servers, or RDP
bridges for contributors.

## Architecture requirements

- Keep dependencies one-way: `protocol -> core -> drivers -> daemon/clients -> apps`.
- Keep AI SDKs, prompt logic, YAML runtimes, recorder UI, and visualizer UI out
  of the Rust device kernel.
- Treat the wire protocol—not Rust traits—as the cross-language boundary.
- Return explicit errors; never substitute an empty observation, action result,
  or evidence object.
- Every Driver must run `driver_conformance_test!`.
- Add tests for protocol behavior, Driver behavior, and serialization changes.

## Protocol changes

Public wire fields use `camelCase`. Breaking changes require an explicit
protocol version bump. After changing a public DTO:

```sh
cargo run -p devicerail-schema-gen -- write
cargo run -p devicerail-schema-gen -- --check
cargo test -p devicerail-protocol --test golden_fixtures
pnpm protocol:types:generate
pnpm protocol:types:check
```

Update the matching fixtures under `crates/protocol/fixtures/`. Prefer Evidence
references to large binary fields, and never expose platform-library-specific
types in DTOs.

## Validation

Run checks proportional to the change, then run the full relevant gates before
requesting review.

```sh
cargo fmt --all -- --check
cargo run -p devicerail-schema-gen -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

pnpm protocol:types:check
pnpm protocol:types:test
pnpm packages:check

python3 packages/python-client/scripts/generate.py --check
python3 -W error -m unittest discover -s packages/python-client/tests -v
python3 -W error -m unittest discover -s packaging/tests -v
```

TypeScript workspaces use pnpm only. Do not commit `node_modules`, local stores,
test evidence, secrets, or unrelated generated output.

## Documentation and validation claims

- Update both [README.md](README.md) and [README.zh-CN.md](README.zh-CN.md) for
  user-visible requirements or capabilities.
- Keep component-specific instructions next to the component and link them from
  [the documentation index](docs/README.md).
- State whether testing used deterministic fixtures, a fake platform tool, a
  stock daemon, real hardware, or an external signing/network environment.
- Do not describe conformance tests as universal real-device compatibility.

## Pull requests

A pull request should have one coherent purpose and include:

- the problem and chosen boundary;
- tests and exact commands run;
- protocol/schema/fixture impact;
- security and performance impact;
- platform validation scope and limitations;
- documentation changes.

By submitting a contribution, you agree that it is intentionally submitted
under the repository's [Apache License 2.0](LICENSE), as described by section 5
of that license, unless you mark it "Not a Contribution."
