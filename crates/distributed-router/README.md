# DeviceRail distributed router

`devicerail-distributed-router` is an opt-in Driver-layer adapter for routing
DeviceRail devices through another node. It contains no AI SDK, prompt/YAML
runtime, recorder, visualizer, UI, public listener, or TLS implementation.

## Security boundary

Every `PeerTransport` carries a non-secret `PeerSecurity` attestation. A stream
is accepted only when it was established in one of these ways:

- from an `AuthenticatedPrincipal` returned by `devicerail-remote-auth`, with
  at least `control` permission; or
- through an operator-managed SSH/mTLS tunnel terminating on loopback.

The stock daemon exposes the second form through two independent, opt-in files:
`DEVICERAIL_DISTRIBUTED_PEERS` declares mandatory outbound nodes, and
`DEVICERAIL_DISTRIBUTED_SERVER` declares one inbound stock peer listener. Both
accept only numeric loopback socket addresses and external tunnel attestations.
Each file must be an owner-owned regular file with mode `0600` (or stricter).
Missing or unknown fields, duplicate nodes/endpoints, a public or zero-port
server address, a symlink, unsafe permissions, invalid wire integers, or an
unsupported security mode fail startup. This owner/mode/no-follow file loader
is supported on Unix. On platforms where DeviceRail cannot prove an equivalent
owner-only ACL and stable file identity, daemon peer-file configuration fails
closed with `PermissionsUnsupported`; the transport-neutral service can still
be embedded behind an independently authenticated stream.
On Darwin/macOS, a non-empty extended ACL is unsafe even when the POSIX mode is
`0600`; inability to query that ACL also fails closed.

An outbound peers document has this shape:

```json
{
  "schemaVersion": 1,
  "peers": [
    {
      "nodeId": "lab-a",
      "endpoint": "127.0.0.1:7443",
      "securityMode": "externalSshOrMtls",
      "tunnelId": "ssh-lab-a",
      "ownerId": "ssh-lab-a",
      "leaseTtlMs": 30000,
      "renewBeforeMs": 5000
    }
  ]
}
```

A server document is closed and all-or-nothing:

```json
{
  "schemaVersion": 1,
  "nodeId": "lab-b",
  "listen": "127.0.0.1:7443",
  "securityMode": "externalSshOrMtls",
  "tunnelId": "ssh-lab-a",
  "nodeEpoch": 17,
  "inventoryRevision": 1
}
```

`nodeEpoch` and `inventoryRevision` are positive JavaScript-safe integers. The
operator advances the epoch for a new node incarnation and the revision when
the exported local inventory changes. After local route registration, the
daemon constructs the non-remote inventory snapshot and binds this listener
before outbound discovery. The service starts behind a gate: hello, inventory,
health, and capabilities are admitted, while lease and mutation operations
return retryable `node_starting`. Loopback connection refusal is retried within
the bounded startup deadline so concurrently launched peers can reach a later
listener bind. Outbound registration then marks the service ready. A later
startup failure closes the listener and converges accepted streams. The bind
diagnostic reports socket reservation, not the ready transition. Only
non-remote routes are exported, so importing a route never creates implicit
multi-hop routing.

These configurations attest an external tunnel; they do not prove that a
tunnel exists and do not make raw TCP safe. A raw loopback client can bypass an
incorrectly isolated tunnel termination. The peer listener is separate from
the JSON-RPC listener, so `DEVICERAIL_RPC_CREDENTIALS` and its HMAC prelude do
not authenticate peer-v2. DeviceRail does not claim built-in public-network
TLS, server identity, or tunnel management in this milestone.

For external tunnels, `ownerId` must exactly equal `tunnelId`. The service
binds leases to the authenticated transport subject and rejects a different
wire owner. With `devicerail-remote-auth`, that subject is the returned
principal id.

## Wire and routing contract

The peer protocol is versioned, strict camelCase JSON carried as bounded
NDJSON frames. The current wire version is peer-v2; a peer-v1 frame is rejected
as `UnsupportedVersion`, the stream is closed, and no downgrade is attempted.
It covers hello, inventory, health, capabilities, leases, connect/disconnect,
observe, execute, bounded evidence reads, and cancel.
Frames are capped at 1 MiB; inventories at 256 devices; routers at 64 nodes;
capabilities, metadata, evidence size/chunks, deadlines, and identifiers have
independent limits. Schema and golden fixtures live under `protocol/`.

Peer-v2 hello also negotiates `uiSnapshotsV1` and `semanticActionsV1`.
Observe and execute requests carry the required operation-scoped
`uiSnapshotsEnabled` and `semanticActionsEnabled` booleans unchanged across
the boundary. A peer that cannot guarantee either requested capability fails
closed instead of dropping a flag, manufacturing an empty tree, or executing
a semantic action without durable UI Snapshot support. UI Snapshot Evidence,
`UiContextRef`, `UiNodeRef`, and `ActionExecution` retain their public Protocol
1.5 representation across the peer boundary.

Local ids use `remote:<nodeId>:<deviceKey>`. Node ids and device keys cannot
contain `:`, so the namespace is unambiguous. Inventory epochs and revisions
must be monotonic. Replays, stale snapshots, duplicate keys, platform identity
drift within an epoch, stale health, mismatched response ids, and changed node
epochs fail explicitly.

Each NDJSON connection performs one exchange at a time. Configured peers use up
to four independently authenticated connections, bounded by remote device
count, and pin each device to one connection. This removes cross-device
head-of-line blocking without moving a connection-bound lease or changing
mutation/cancellation outcome semantics. Every shard performs hello, inventory,
and health discovery and must report the same node epoch, revision, identities,
health, and security subject. Client-side cancellation writes a best-effort
cancel frame on that device's shard and then closes/poisons only that stream,
preventing a late response from being consumed as the next request. This crate
exposes `serve_peer_stream` for already-authenticated streams; it deliberately
does not open a listener. The optional stock listener belongs to the daemon
layer and supplies only streams carrying the configured external-tunnel
attestation. An unexpected request-task failure closes that stream explicitly;
the stock daemon treats it as a fatal listener invariant instead of continuing
with potentially inconsistent process state.

Configured peers are discovered concurrently, as are the additional shards for
each peer and independent device health probes. Results are sorted back into
stable device-id order, and simultaneous startup failures are reported in
configuration order, so concurrency does not make inventory or error selection
nondeterministic.

## Lease and mutation semantics

`LeaseTable` provides at-most-one active owner per device inside one node
epoch. Tokens bind device key, owner, epoch, id, and expiry. Expired, released,
replayed, stolen, and previous-epoch tokens are rejected. Disconnect attempts
lease release; expiry is the fallback after a transport failure. Local expiry
uses a monotonic deadline as well as the wire timestamp, so a backwards wall
clock adjustment cannot extend a lease.

Every issued lease owns a lifecycle gate. Connect, observe, execute, evidence
read, renew, disconnect, and release use that same gate, while cancel remains
independently admissible. Cleanup records disconnect, Session end, Evidence
release, and Core lease release as retryable stages; it neither deletes the
binding nor revokes the peer lease until all stages succeed. A failed cleanup
blocks new device operations and can resume without losing ownership state.
EOF teardown is admitted to a service-owned semaphore-bounded task with bounded
retries. The stream server waits a short completion grace, but a grace timeout
does not cancel the admitted cleanup task or discard its staged progress.
Connect publishes a cancellation-safe binding reservation before acquiring the
Core lease, mirrors every replacement lease synchronously before Driver I/O,
and can resume an interrupted pre-Session Connect. Final cleanup sweeps only
the Core owner derived from that unique peer lease, so a stale mirrored lease
id cannot leave a replacement lease behind or affect another tenant.

This is not a distributed consensus or cross-node atomic lease. `execute` is
never automatically retried. If a mutation may have reached the peer and the
transport then fails, the Driver returns `remote_execute_outcome_unknown`.
`RegistryPeerService` uses `CallLedger` for a bounded call-id deduplication
window and retains bounded terminal responses for replay. The same id with a
different request fingerprint is a conflict; a duplicate never executes the
Core Driver again.

## Evidence and observability

Remote observations contain evidence references. The adapter reads evidence in
bounded 256 KiB chunks and writes it through the operation-scoped local
`EvidenceStore`, so Core receipt reconciliation remains intact. The service
keeps at most four cancellation-safe sequential readers per binding; normal
multi-chunk transfer opens and verifies an asset once instead of rescanning from
byte zero for every chunk. Non-sequential offsets safely reopen and seek once.
Digests are verified when supplied. Protected actions omit screenshots, clear
remote metadata, and replace remote output with a non-sensitive acknowledgement.

UI Snapshot Evidence is stricter than a generic binary asset: its advertised
length is checked on the first chunk, its bounded JSON body is decoded, and
`UiSnapshot::validate_against` binds format, Observation, context, node count,
preorder, and stable ids before the Remote Driver can return success. Reusing
one remote evidence id does not skip per-reference validation. AssetRef,
first-chunk, actual-byte, and reused-id digests must all agree. The five
canonical semantic Actions must advertise the generated input Schema; before
dispatch the receiver also decodes the matching Arguments DTO and runs its
typed `validate()` contract. A peer `invalid_arguments` response retains that
bounded Driver taxonomy. Typed output, execution context, Observation, node
reference, and actual tree node are validated again on the receiving node
before an event can be appended.

Telemetry has fixed method/outcome enums, at most 64 node labels plus one
overflow label, and a 1024-record in-memory trace bound. Records contain only
node, method, outcome, duration, and trace id. Device ids, action names,
arguments, credentials, evidence URIs, payloads, and raw errors are absent.

## Verification

```sh
cargo test -p devicerail-distributed-router
cargo clippy -p devicerail-distributed-router --all-targets -- -D warnings
```

Tests exercise the shared `driver_conformance_test!`, strict schema/golden
fixtures, owner-only fail-closed configuration, two-node routing, stale and
drifting inventory, health, lease stealing/replay/expiry, call-id conflicts,
secret-free Debug/telemetry, and evidence import. Real in-process two-node
tests connect `RegistryPeerService` to `serve_peer_stream` over a Tokio duplex
stream, then drive `NdjsonPeerTransport`, `RemoteNode`, `RemoteDeviceDriver`,
the local `DeviceRuntime`, and a separate local `FileEvidenceStore`. They also
cover Session creation, terminal replay, execute/disconnect serialization,
Connect abort/retry and error rollback, cleanup retry, oversized execute
outcome-unknown semantics, incomplete rollback ambiguity, route removal during
cleanup, cancel/transport poisoning, and EOF cleanup without requiring an
external lab. A separate Unix stock-daemon E2E starts two real daemon binaries,
connects their inbound and outbound adapters over a real loopback TCP socket,
and verifies remote inventory, connect, observation/Evidence import, and clean
EOF shutdown. That local framing/lifecycle test does not claim a real SSH/mTLS
tunnel, certificates, cross-host networking, packet loss, or network-partition
coverage.
