# DeviceRail filesystem Evidence Store

`devicerail-evidence-fs` is the durable, zero-AI filesystem adapter for the `EvidenceStore` contract in `devicerail-core`.

## Guarantees

- Streams bytes while enforcing a configurable per-asset limit and computing SHA-256.
- Publishes immutable objects by atomic rename only after syncing data and metadata.
- Returns canonical references shaped as `sha256:<digest>` and `devicerail://assets/sha256/<digest>`.
- Deduplicates identical content and persists one idempotent reference marker per `(Session, digest)`.
- Verifies size and SHA-256 before returning a readable stream.
- Releases Session references separately from bounded, age-aware, dry-run-capable GC.
- Rejects malformed paths, unknown entries, symlinks, corrupt objects, corrupt markers, and a second process opening the same root.
- Cleans cancelled/abandoned staging directories; incomplete operations can leak an orphan safely but cannot publish a partial object or a live reference to missing bytes.

The configured root is application-owned. On Unix its directories/files are set to `0700`/`0600`, and the Store holds an exclusive file lock for its lifetime. Internal symlinks are rejected. This P0 adapter does not claim to resist a privileged process that can mutate an already-open file descriptor; deployments must not share the root with untrusted local writers.

Upload streaming, initial hashing, and staged data/metadata fsync run before
Store coordination. Verification, atomic publication, directory fsync, and
reference writes then use object and Session lock stripes plus a shared GC
gate, so unrelated Sessions can complete concurrently while same-object
deduplication and per-Session limits remain linearizable. Session release,
full reference audits, and GC retain the exclusive gate because their
filesystem and in-memory index transitions span the complete Store.

## Layout

```text
<root>/v1/
  store.json
  locks/store.lock
  staging/.part-<uuid>/
  objects/sha256/ab/cd/<digest>/
    data
    meta.json
  refs/sessions/<session-id>/<digest>.json
  unreferenced/<digest>.json
  released-sessions/<session-id>.json
  trash/<digest>/
```

Session reference markers, rather than a mutable global refcount, are the durable GC mark set. `release_session` removes one Session's markers and records when an object becomes unreferenced. GC validates every candidate before deletion and aborts conservatively on malformed state.

Before removing references, `release_session` durably records a closed-Session tombstone. A concurrent slow upload therefore cannot recreate a reference after cleanup. Session IDs are globally unique and cannot be reused for the lifetime of the Store; tombstones are intentionally retained in the v1 layout. GC first moves an unreferenced object into `trash` atomically, then removes it. Startup recovery either finishes that deletion or restores an object that has become referenced.

## Ordering with the event log

Evidence and events deliberately use a leak-safe order:

```text
put + durable Session reference -> append TestEvent containing AssetRef
delete/seal Session event log   -> release Session references -> GC
```

If event append fails, the reference remains and can be reconciled later. If cleanup fails after the event log is deleted, bytes remain pinned until an idempotent release retry. The Store never guesses from an in-memory event log and never deletes bytes that a durable reference still marks live.
