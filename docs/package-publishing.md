# Package publishing

DeviceRail publishes two Rust crates, six public npm packages, and one Python
distribution through `.github/workflows/publish-packages.yml`. Portable native
binaries remain separate artifacts produced by `release.yml`.

The crates.io scope is intentionally closed:

- `devicerail-protocol`
- `devicerail-client`

Every other Rust workspace crate remains `publish = false`. Drivers, Core,
daemon, Evidence, Bundle, visualizer, transport, and build-time crates are
distributed only as source-workspace components or portable native release
artifacts; they are not public crates.io packages.

## GitHub repository setup

Create three protected GitHub environments named `crates-io`, `npm`, and
`pypi`. Configure at least one required reviewer for each environment. Protect
stable tags matching `v*` so that only maintainers can create or move them.

crates.io publishing uses Trusted Publishing and needs no stored secret. The
`crates-publish` job declares `id-token: write` and exchanges its GitHub OIDC
identity for a token that expires after thirty minutes. Configure Trusted
Publishing **once per crate** — the configuration is owned by the crate, not by
the repository — at `https://crates.io/crates/<crate>/settings`, naming
repository owner, repository name, workflow filename `publish-packages.yml`,
and environment `crates-io`. A single exchange mints one token covering every
crate whose configuration matches, so both crates publish from one step.

A crate must already exist before Trusted Publishing can be configured for it;
crates.io has no pending-publisher equivalent. Bootstrap a brand new crate with
a short-lived API token scoped to allow creating new crates, supplied as the
`CARGO_REGISTRY_TOKEN` secret on the `crates-io` environment, then revoke that
token, configure Trusted Publishing, and delete the secret. Both public crates
were bootstrapped this way for `0.3.0`.

The workflow normally starts when a stable `v*.*.*` tag is pushed. A maintainer
can also dispatch it manually for an existing tag and independently select
crates.io, npm, and PyPI with `publish_crates`, `publish_npm`, and
`publish_pypi`. Every public package, the Python distribution, the root package,
and the Cargo workspace must already contain the exact tag version. For
example, tag `v0.1.0` requires version `0.1.0` everywhere.

## crates.io publishing

Publish the Rust crates in dependency order:

1. `devicerail-protocol`
2. `devicerail-client`

`devicerail-client` declares the release-aligned compatible
`devicerail-protocol` registry version while using a workspace path during
development. The minimum protocol version named by the client release must
therefore be visible in the crates.io index before Cargo can verify and publish
the client package. The protected
workflow applies a bounded retry to client package verification while that
index entry propagates. The same job is safe to rerun after a partial
publication: an existing crate is accepted only when its crates.io checksum
matches the archive rebuilt from the release tag.

For a release version already present in both manifests, the package checks and
manual release sequence are:

```bash
cargo package --locked -p devicerail-protocol
cargo publish --locked -p devicerail-protocol

# Wait until the protocol version resolves from crates.io.
cargo package --locked -p devicerail-client
cargo publish --locked -p devicerail-client
```

Run `cargo publish --dry-run` instead of `cargo publish` when validating an
already-indexed dependency chain. Never pass `--allow-dirty` or `--no-verify`
for a release. Before the first protocol version exists in crates.io, the
workflow's unprivileged build job verifies the client archive with a temporary
Cargo `patch.crates-io` that points to the exact workspace protocol source. The
protected publish job then runs ordinary verified `cargo publish` after
publishing the protocol, so the uploaded client must resolve the registry
dependency without that patch. The packaged Rust client must contain its README
and must not link any unpublished workspace implementation. The unprivileged
Rust build also runs the real stock-daemon client E2E before the protected
publish environment can be entered. Its artifact is explicitly named
`rust-crates-preflight-*`; after registry resolution, the protected job archives
the exact final packages separately as `rust-crates-crates-io-*`. Both CI and
the release workflow run the packaged client's embedded Golden/conformance
tests from Cargo's extracted package directory, so those tests cannot
accidentally depend on monorepo-relative fixture paths.

## npmjs.org publishing

The `@devicerail` npm organization must exist and the release maintainer must be
allowed to create its public packages. Create a granular npm access token
with write access to the organization packages and add it as the `NPM_TOKEN`
secret on the protected `npm` environment. Do not store the token in repository
variables, workflow YAML, or `.npmrc`.

The workflow publishes these packages directly to the public npm registry:

- `@devicerail/protocol`
- `@devicerail/client`
- `@devicerail/tool-adapter`
- `@devicerail/recorder`
- `@devicerail/live-visualizer`
- `@devicerail/yaml-adapter`

Every package declares
`publishConfig.registry = "https://registry.npmjs.org/"`. The workflow also
configures `actions/setup-node` and every `npm publish` invocation with that
exact registry, verifies the token with `npm whoami`, and never configures
`npm.pkg.github.com`. Each release tarball receives the exact current GitHub
`repository.url` before it is packed, but GitHub is only the source repository;
it is not the package registry.

## PyPI Trusted Publishing

Configure a pending Trusted Publisher for `devicerail-client` before its first
upload, or configure the publisher from the project settings after a manual
bootstrap upload. Use the repository owner, repository name, workflow filename
`publish-packages.yml`, and environment `pypi`.

No PyPI API token is required. The publish job downloads the wheel and source
distribution built by the unprivileged build job and passes only those verified
artifacts to the pinned PyPA publishing Action with attestations enabled.

## Release order and recovery

Rust crates are published in protocol-then-client order. npm packages are
published independently in dependency order: protocol, client,
live-visualizer, tool-adapter, recorder, then yaml-adapter. Registry versions
are immutable. Do not move a release tag or reuse a version after any upload
has succeeded. If a workflow stops after a partial publication, inspect each
registry before retrying. The Rust publish job is idempotent for the same
release tag: it checksum-verifies an already-published protocol or client and
continues with the missing dependent crate. It fails closed if an existing
version does not match the release archive. Other registry jobs may still
require selecting only the unpublished ecosystem through manual dispatch.

Dispatch such a retry against the release tag, not against a branch. Each
publishing environment may restrict which refs are allowed to deploy to it, and
a tag that already reached the publish step is proven to be allowed while
`main` is not; the web dispatch form defaults to `main`, which is the wrong
ref. The workflow file is identical on both, so nothing is lost:

```bash
gh workflow run publish-packages.yml --ref v0.1.0 \
  -f release_tag=v0.1.0 -f publish_npm=false -f publish_pypi=false \
  -f publish_crates=true
```

Confirm first that the earlier run has finished. The concurrency group is keyed
on the release tag alone, so a retry for the same tag joins the failed run's
group, and `cancel-in-progress` is disabled. If any job of that run is still
waiting on an environment reviewer, the new dispatch queues silently with no
log output rather than reporting an error. Cancel the stalled run first.

Treat each registry's public API as the source of truth when confirming what a
release actually published; a green workflow proves only that no job failed.

```bash
curl -s https://pypi.org/pypi/devicerail-client/json
curl -s https://registry.npmjs.org/@devicerail/protocol
curl -s https://crates.io/api/v1/crates/devicerail-protocol
```

Before pushing a release tag, run the normal CI gates and:

```bash
node scripts/check-release-version.mjs v0.1.0
pnpm packages:check
python -m build --outdir packages/python-client/dist packages/python-client
python packages/python-client/scripts/check_distribution.py packages/python-client/dist
```

## Driving a release

`scripts/release.mjs` performs the mechanical half of a release. It has three
commands and no options.

```bash
node scripts/release.mjs prepare 0.3.1
node scripts/release.mjs publish 0.3.1
node scripts/release.mjs status 0.3.1
```

`prepare` refuses to start unless the working tree is clean, the tag is absent
both locally and on the remote, the requested version is greater than the
current one, and no registry already carries it. It then rewrites every version
touchpoint — nine `package.json` files including the two private ones, the
workspace `Cargo.toml`, both `devicerail-protocol` pins in
`crates/client/Cargo.toml`, `pyproject.toml`, the Python `__version__` and the
`default_hello` client version, and a new `CHANGELOG.md` section — refreshes
`Cargo.lock` through `cargo metadata`, and finishes by running
`check-release-version.mjs`. It leaves the result uncommitted so the diff can be
read. `pnpm-lock.yaml` records no workspace versions and is deliberately left
alone.

Describe the release under the new `CHANGELOG.md` heading, run the gates above,
and commit. `publish` then re-checks the same preconditions against the
committed tree and pushes the annotated tag that starts `publish-packages.yml`.

`status` reports what each registry actually holds for a version and, when
something is missing, prints the exact dispatch command to republish only the
missing ecosystems.

Two touchpoints are outside `check-release-version.mjs`: the private
`playwright-driver` and `live-visualizer` app manifests, whose versions it never
inspects, and the second `devicerail-protocol` pin in
`crates/client/Cargo.toml`, which its non-global regex cannot reach. The release
script rewrites all of them, so drift only appears when a version is bumped by
hand.
