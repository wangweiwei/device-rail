# DeviceRail process plugin ABI

`devicerail-plugin-driver` lets an explicitly configured executable implement
the DeviceRail `DeviceDriver` contract without exposing Rust trait or dynamic
library ABI. A plugin is a process speaking versioned JSON on stdin/stdout.
There is no raw command, argv, shell, or library-loading method.

## Installation layout

Each configured absolute directory contains one or more files ending in
`.devicerail-plugin.json` and their relative executables. Example:

```json
{
  "manifestVersion": 1,
  "abiVersion": 1,
  "pluginId": "example-camera",
  "pluginVersion": "1.2.0",
  "executable": "bin/example-camera-plugin",
  "protocol": { "major": 1, "minMinor": 3, "maxMinor": 4 },
  "device": {
    "key": "camera-01",
    "name": "Example camera",
    "platform": { "kind": "other", "value": "camera" },
    "osVersion": "7.1"
  },
  "capabilities": [
    { "name": "capture", "protection": "standard" },
    { "name": "inputSecret", "protection": "protected" }
  ]
}
```

The canonical Schema is
[`protocol/plugin-manifest-v1.schema.json`](protocol/plugin-manifest-v1.schema.json).
On Unix, the directory, manifest, and executable must be owned by the daemon's
effective user, must share that owner, and must not be group/world writable.
On Darwin/macOS, each path and the opened manifest/executable descriptor must
also have no extended ACL entries; an ACL lookup failure is treated as
`plugin_permissions_unsupported` rather than falling back to mode bits.
Manifests are opened with `O_NOFOLLOW` and their opened inode is matched to the
validated directory entry. Symlinks are rejected, including any intermediate
executable path component. The executable must remain inside the configured
directory and be executable. A process already running as the daemon's own
effective user is outside this filesystem trust boundary.

On non-Unix platforms, this crate cannot prove the equivalent owner-only ACL,
no-follow handle, and stable file-identity contract with its current system
boundary. Discovery therefore fails closed with
`plugin_permissions_unsupported` before it reads a plugin manifest or starts a
process. The transport repeats that fail-closed decision during its mandatory
pre-spawn executable revalidation; it does not substitute permissive or
best-effort ACL checks.

## Wire contract

The daemon starts the validated executable with exactly one fixed argument:

```text
--devicerail-plugin-abi=1
```

It keeps one supervised process per Driver and exchanges newline-delimited JSON
request/response frames on stdin/stdout. Requests are serialized and every
response must preserve its `requestId`; `connect` state therefore remains in
the same child used by later observations and actions. The inherited environment
is cleared. The closed operations are `hello`, `health`, `connect`, `disconnect`,
`observe`, and `execute`; the ABI has no generic process or filesystem escape
hatch. The envelope Schema is
[`protocol/plugin-abi-v1.schema.json`](protocol/plugin-abi-v1.schema.json).

`hello` returns the same plugin identity/version/device declaration, the
selected DeviceRail protocol version, and full `ActionDefinition` values. The
host rejects any mismatch with the manifest, duplicate action, invalid or
external-reference Schema, or protection downgrade. Public device identity is
host-derived as `plugin:<pluginId>:<deviceKey>`.

Observation returns a bounded viewport, bounded JSON metadata, and optionally a
base64 PNG. Screenshot capture is explicitly disabled for policy-omitted and
protected observations. The host decodes and canonicalizes PNG pixels before
writing through the Session-scoped Evidence Store. Execute receives a call ID,
one negotiated action name, and Schema-validated arguments. A mutating request
is never retried after ambiguous delivery.

Every frame has a hard input/output limit, stderr has one lifetime ceiling, and
each child request/response exchange has a 30-second default transport timeout,
configurable from 1 ms through 120 seconds. A shorter Core deadline or
cancellation wins; waiting for this Driver's serialized exchange lock is
bounded by Core request control, and the configured transport clock starts
after that lock is acquired. An ambiguous timeout, cancellation, framing
failure, size violation, or process exit kills and permanently poisons that
Driver process; it is not silently restarted or replayed. Dropping the Driver
also kills its child. Diagnostics expose only validated stable codes, never
stderr, paths, or arguments.

## Daemon opt-in

Set `DEVICERAIL_PLUGIN_DIRS` to a platform path list. Optionally set
`DEVICERAIL_PLUGIN_TIMEOUT_MS=1..120000`. A timeout without directories is an
error. Explicit plugin startup is fail-closed if discovery is empty or any
manifest, executable, negotiation, or registration is invalid. Process plugins
are currently executable only on Unix; an explicit configuration on another
platform fails startup with the stable permissions-unsupported code.

The deterministic `devicerail-plugin-fixture` binary runs the shared
`driver_conformance_test!` suite on Unix. Platform-gated contract and security
tests also cover both published Schemas, capability mismatch, ABI mismatch
before execution, symlink and permission rejection, process cancellation, and
protected argument debug redaction. Non-Unix unit tests lock the discovery and
transport revalidation fail-closed boundary.
