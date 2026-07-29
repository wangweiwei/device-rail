# `@devicerail/recorder`

`@devicerail/recorder` records the public DeviceRail Session event stream into
a durable, resumable checkpoint. It orders exclusively by `TestEvent.sequence`,
preserves event and Action correlation, and stores typed Evidence references
rather than screenshot or video bytes.

The package sits above `@devicerail/client` and `@devicerail/protocol`. It has
no Driver, ADB, AI SDK, prompt, YAML, visualizer, or product UI dependency.

## Completion boundary

A checkpoint is an internal recovery artifact, not a second archive format and
not proof that recording completed. Completion requires an ended Session, an
exact `session.export` match, and successful export and independent validation
through the local Rust `devicerail-bundle` CLI. The CLI remains the sole Session
Bundle writer and validator; this package does not reimplement its manifest or
Evidence integrity rules.

The daemon must be stopped before Bundle export so the CLI can acquire the
filesystem Evidence Store's exclusive lock. Until export atomically publishes
the Bundle, callers must not clear the Session or its Evidence and must not
restart the daemon: startup reconciliation may release the stopped process's
orphaned Evidence pins. Once export succeeds, independent validation reads only
the Bundle and may run after a daemon restart. Retain the checkpoint until that
validation succeeds.

The intended lifecycle is `ExecutionRecorder.open` → `captureOnce` or
`captureUntilSealed` → `publishSource` → close the client/daemon → `finalize`.
`ExecutionRecorder.openOffline` resumes a sealed or completed checkpoint after
shutdown without pretending an active in-memory daemon Session can be restored.
Finalization takes an explicit absolute path to the `devicerail-bundle` binary
and invokes it with fixed arguments and `shell: false`; package import never
probes for or starts that binary.

## Recovery limits

Duplicate delivery is idempotent only when the complete event value matches.
Sequence gaps, out-of-order events, cross-Session events, lifecycle violations,
and incomplete Action pairs fail explicitly. Cancellation may retain a durable
checkpoint but never publishes a completed recording.

Protocol 1.4 media start/frame/end events are checkpointed as typed Evidence
references. Stream IDs remain lifetime-unique for the whole Session, including
after a stream has ended; reuse fails before a checkpoint can advance.

The current `events.list` and `session.export` RPC methods do not accept
request-control options. Recorder cancellation is therefore observed before
and after each call and during polling/checkpoint/CLI work; interrupting a
stuck stdio call requires the host to close the client connection.

The current stdio transport limits each `events.list` and `session.export`
response to 1 MiB. Recorder starts at 1000 events per page and, on the typed
`response_frame_too_large` failure, halves that limit until the page fits. The
successful smaller limit is reused for later pages; if one event still cannot
fit, recording fails explicitly as `event_too_large`. Recorder advances
`afterSequence` and durably commits each accepted event page before requesting
the next, so a missed suffix does not need to fit in one response.

When `session.export.page.v1` was negotiated and the configured event source
implements `exportSessionPage`, sealing applies the same bounded retry policy to
the authoritative export. Every page must repeat one stable ended `SessionInfo`
and must exactly match the corresponding durable checkpoint events. Reading
pages never advances the checkpoint; only the fully verified export performs
the single `recording` to `sealed` revision CAS. Without that negotiated
capability Recorder retains the legacy complete `session.export` behavior.
Concurrent seal callers share one in-flight verification and successful CAS,
but keep independent cancellation: an aborted waiter returns immediately
without cancelling the owner, while a non-cancelled waiter may retry after an
owner cancellation leaves the checkpoint recording.
Concurrent seal attempts on one Recorder share a successful verification and
CAS, while each caller retains independent cancellation: aborting a waiter does
not cancel the owner, and a non-cancelled waiter retries after a cancelled
owner operation.

Pagination removes the RPC single-frame bottleneck; it does not remove the
local snapshot limit. The complete Bundle Source must still fit its explicit
8 MiB bound, below the Session Bundle's default 16 MiB manifest budget. The
checkpoint permits a separate fixed 64 KiB envelope/phase headroom so a valid
near-limit Source can advance through sealed and completed without losing room
for its checksum and receipt metadata. Sessions whose Source exceeds 8 MiB
require a future segmented checkpoint and streaming Bundle Source contract. The
current daemon also uses a process-local Event Store, so recovery of an active
Session requires the same daemon and connection to remain alive. A sealed,
terminal checkpoint can be finalized offline before restarting the daemon.

Recording pages are persisted in an explicitly versioned, checksummed sidecar
journal. Each commit appends only the accepted page and atomically advances a
small journal head, so `P` fixed-size pages write `O(N + P)` bytes instead of
rewriting `P` growing prefixes. A checksum chain binds every committed page;
bytes written before a head publication are ignored and truncated on the next
append. Recovery validates the complete chain once, and the `recording` to
`sealed` transition compacts it into one canonical checkpoint snapshot. The
8 MiB Source and checkpoint bounds, per-page fsync, revision CAS, owner-only
metadata checks, and fail-closed corruption behavior remain unchanged.

The in-memory `EventLog` follows the same boundary: batch preparation validates
against overlay indexes without copying the confirmed prefix, and the prefix is
materialized only when a snapshot is requested or sealing begins. Public
speculative `fork()` calls still copy identity indexes to preserve independent
branch mutation and are not used by Recorder page capture.

On Unix, checkpoint and Source files must be owned by the current user with no
group/world permission bits, and directory fsync closes the normal power-loss
window. Node.js exposes neither portable Windows owner-only ACL verification
nor directory fsync. Recorder therefore does not claim or enforce owner-only
access on Windows; deployments must provide a suitably ACL-restricted parent
directory. It still checks regular-file identity and retains file flush plus
atomic replace/no-clobber semantics, with final directory durability left to
the filesystem. Stale writer locks are reclaimed only when the OS confirms
their PID no longer exists; ambiguous or reused PIDs fail closed.
