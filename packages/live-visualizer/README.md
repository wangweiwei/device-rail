# `@devicerail/live-visualizer`

Protocol-only bounded presentation model for DeviceRail live Session events.
It stores immutable sanitized presentation DTOs, never raw `TestEvent` values,
daemon capabilities, or Evidence URIs.

```ts
const prepared = timeline.prepare(item.event);
const commit = timeline.commit(prepared);
item.confirm();
const { revision } = timeline.confirm(commit);
```

The split is intentional: a committed but not daemon-confirmed event remains a
single pending reservation. An exact replay is idempotent; different content at
that sequence fails closed. Pages are synchronous snapshots with five filters
and a hard 50-item maximum. `observations` contains captured Observations and
captured media frames; media stream start/end boundaries remain `all`-only even
though every media lifecycle event shares the `media` presentation category.
Prepared and commit tokens are bound by private object identity rather than
copyable fields. The model also rejects duplicate event and Action IDs,
completion without a confirmed start, mismatched result IDs, and Session end
while Actions remain in flight.

Capacity is fail-closed. The event count, stored presentation bytes, per-event
bytes, source-event bytes, text, JSON depth/bytes, and Evidence references all
have validated hard ceilings. A capacity error publishes no partial entry and
sets `state().status` to `viewerCapacityExceeded`; the triggering daemon item
must remain unconfirmed. Evidence presentations are reference-only and contain
no URI, path, binary content, image proxy, or download handle.
