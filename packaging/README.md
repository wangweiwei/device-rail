# DeviceRail release packaging

This directory builds verifiable portable installer archives for the
`devicerail-daemon` and `devicerail-bundle` binaries. Linux produces a
deterministic `.tar.gz`; macOS and Windows produce deterministic stored `.zip`
files. Each archive has one root and contains only regular files with canonical
permissions. It includes both binaries, a platform install script, a closed
configuration example, the Apache-2.0 license and NOTICE, a generated
third-party license inventory, an SPDX 2.3 SBOM, a DeviceRail-specific in-toto
provenance statement, a binary target/version
contract, a signing declaration, and the v1 release manifest. The provenance
uses `https://devicerail.dev/provenance/v1`; it does not claim a SLSA build
level or a SLSA provenance predicate. The SPDX document namespace is a
deterministic content identity derived from the source and binary digests, so
different release payloads do not reuse a document namespace.

These are portable archive installers, not native `.pkg`, `.msi`, or distro
packages. macOS CI can submit its signed ZIP to Apple notarization, but there is
no stapling operation for ZIP archives.

## Unsigned reproducible test artifact

Build the two native binaries first. Packaging invokes each with `--version`
and rejects a version that differs from the Cargo workspace, root
`package.json`, or Cargo metadata.

```sh
cargo build --locked --release \
  -p devicerail-daemon -p devicerail-bundle-cli

mkdir -p dist
python3 packaging/devicerail_release.py package \
  --platform linux \
  --architecture x86_64 \
  --daemon target/release/devicerail-daemon \
  --bundle target/release/devicerail-bundle \
  --output-dir dist \
  --source-date-epoch 1700000000 \
  --source-uri git+https://github.com/example/device-rail.git \
  --release-status unsigned-test-only

python3 packaging/devicerail_release.py verify \
  dist/devicerail-0.1.0-linux-x86_64-UNSIGNED.tar.gz
```

The unsigned path uses only Python's standard library, `cargo metadata
--locked --offline --no-deps`, and the checked-in `Cargo.lock`; it does not
download release tools. Workspace package metadata is combined with every
locked dependency to form the inventory. Because `Cargo.lock` does not carry
license expressions, external entries are conservatively marked `NOASSERTION`
and require redistribution review. With identical input binaries, Cargo
metadata, packaging files, Python/zlib toolchain, platform,
architecture, and `SOURCE_DATE_EPOCH`, it emits identical archive, manifest,
SBOM, provenance, and checksum bytes. ZIP timestamps earlier than 1980 are
canonically clamped to the ZIP epoch.

Normal packaging records the current Git commit as a source material. A dirty
unsigned workspace is explicitly marked `dirty-uncommitted` and
`sourceMaterialComplete: false`; signed packaging rejects it. Signed packaging
also requires an explicit credential-free `--source-uri`. The Cargo metadata
fixture hook exists only in the Python test API, is not a command-line option,
sets `cargoLocked: false`, and is labeled `test-fixture` rather than being
presented as production source provenance.

Every unsigned filename contains `UNSIGNED`, its manifest says
`unsigned-test-only`, and its included README says it is unauthenticated. A
checksum proves integrity, not origin.

## Signed release contract

A release can say `signed` only after the packager successfully verifies both
native payload signatures:

- macOS: `codesign --verify --strict` with an explicit requirement for each
  hardened-runtime binary, followed by exact Team ID and leaf Authority checks.
- Windows: `signtool verify /pa /all` for each Authenticode binary, followed by
  an exact leaf-certificate Subject and SHA-256 thumbprint check through
  `Get-AuthenticodeSignature`.
- Linux: `cosign verify-blob` for each binary and its checked-in public-key
  payload.

The signed package additionally declares a required detached cosign signature
over the complete archive. After `package`, sign the exact artifact without
modifying it:

```sh
cosign sign-blob --yes \
  --key env://RELEASE_COSIGN_PRIVATE_KEY \
  --output-signature "${artifact}.sig" \
  "${artifact}"

python3 packaging/devicerail_release.py verify "${artifact}" \
  --trusted-artifact-public-key /trusted/out-of-band/devicerail-release.pub
```

Pass `--artifact-public-key` when packaging; the packager copies it to
`<artifact>.cosign.pub` and binds its SHA-256 in the internal signing
declaration. Verification still requires the same public key from a trusted,
out-of-band location; the adjacent copy is never accepted as its own trust
anchor. Linux signed packaging also requires `--linux-public-key`,
`--linux-daemon-signature`, and `--linux-bundle-signature`. The verifier fails
closed if a required tool, key, signature, archive checksum, file inventory,
SBOM identity, provenance subject, or native platform signature is missing or
invalid. It never exposes a flag that skips signature verification.

Signed macOS packaging requires all of `--macos-team-id`,
`--macos-designated-requirement`, and `--macos-signing-identity`; verification
requires the corresponding `--expected-macos-*` values from an out-of-band
configuration. The signing identity is the exact leaf certificate
`Authority`/Common Name reported by `codesign -d --verbose=4`, not a certificate
hash. The verifier also requires hardened-runtime flags and a non-ad-hoc
designated requirement. An incomplete macOS identity configuration fails.

Signed Windows packaging similarly requires both
`--windows-publisher-subject` and `--windows-publisher-sha256`; verification
requires the corresponding `--expected-windows-*` values from a separate
trusted configuration. A valid Authenticode chain alone is insufficient when
its leaf publisher identity differs from that out-of-band expectation.

Verification always checks executable headers against the manifest target.
For a signed artifact on the target operating system it first authenticates
the archive and native payload signatures, then executes each binary with
`--version` and compares the exact result. Cross-operating-system signed
verification fails closed because that final execution cannot be performed
safely. Unsigned verification never executes archive-provided code and reports
only the package-time version contract.

Signatures and timestamp/notarization services can make production artifacts
non-reproducible. Reproducibility is asserted for unsigned test artifacts; a
signed release instead preserves its exact archive digest, detached signature,
SBOM, and provenance.

## CI release modes

`.github/workflows/release.yml` defaults to `unsigned-test-only`. Selecting
`signed` is a separate explicit manual action. It requires all relevant
identity material and fails rather than silently downgrading a requested signed
release:

- all platforms: `RELEASE_COSIGN_PRIVATE_KEY`,
  `RELEASE_COSIGN_PUBLIC_KEY`, and `RELEASE_COSIGN_PASSWORD`;
- macOS: `MACOS_CERTIFICATE_BASE64`, `MACOS_CERTIFICATE_PASSWORD`,
  `APPLE_ID`, `APPLE_TEAM_ID`, and `APPLE_APP_PASSWORD`, plus explicit
  non-secret workflow inputs for the expected Team ID and exact signing
  identity;
- Windows: `WINDOWS_PFX_BASE64` and `WINDOWS_PFX_PASSWORD`.

Signed Windows dispatches additionally require explicit non-secret workflow
inputs for the expected publisher Subject and SHA-256 certificate thumbprint.
Every third-party Action used by the release workflow is pinned to a complete
commit SHA; the adjacent comments retain the reviewed upstream tag for update
tracking.

Unsigned jobs do not import identities, invoke signing tools, contact Apple,
or claim notarization. Signed macOS CI verifies its code signatures before
packaging and submits the final ZIP to `notarytool`. The workflow uploads only
artifacts that pass the same local verifier.

## Security and licensing limits

Verification reads the archive into bounded memory after rejecting traversal,
absolute/non-ASCII paths, duplicate and case-colliding names, encrypted ZIP
members, symlinks, hard links, sparse files, devices, unlisted content, and
non-canonical modes. ZIP entry count and central-directory size are checked
before Python allocates ZIP member objects; ZIP64 is deliberately unsupported.
Tar files are inspected as a bounded stream without building an unbounded
member list, and the complete decompressed tar byte stream is capped so GNU or
PAX metadata cannot expand without limit. Native signature verification materializes only those
already validated bytes in a new private temporary directory.

DeviceRail source and first-party payloads are licensed under Apache-2.0. Every
archive includes the repository's `LICENSE` and `NOTICE`. The generated SPDX
and third-party inventory record dependency license metadata, but do not
replace upstream license texts, required notices, or redistribution review.

Run the local security suite without network access:

```sh
python3 -m unittest discover -s packaging/tests -v
```
