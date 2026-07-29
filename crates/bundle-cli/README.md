# DeviceRail Session Bundle CLI

`devicerail-bundle` creates and validates a portable, offline DeviceRail
Session Bundle. It does not connect to a device or daemon and it never deletes
Session events or Evidence.

## Safe export workflow

1. End the Session with `session.end`.
2. Retain the selected protocol version returned by `system.hello` (or the
   selection reported by `system.describe`), call `session.export`, and write
   a strict `BundleSource` JSON file with exactly `eventProtocolVersion` and
   `sessionExport`. The version identifies the event DTO encoding; it is not a
   new protocol negotiation. On Unix the file must be owner-only (for example
   mode `0600`). On Windows, where this CLI does not prove an equivalent file
   ACL, deployments must place the Source in a parent directory whose ACL
   restricts access to the intended account before invoking the CLI.

3. Shut down the daemon so it releases the exclusive File Evidence Store lock.
4. Export the Bundle:

   ```text
   devicerail-bundle export --source session-source.json --evidence-dir ./evidence --output ./session.bundle
   ```

5. Validate it without access to the original Evidence Store:

   ```text
   devicerail-bundle validate ./session.bundle
   ```

Do not restart the daemon between step 3 and successful completion of step 4.
A new daemon reconciles the stopped process's orphaned Evidence pins during
startup, which can make those assets unavailable to export. After step 4
succeeds, the published Bundle is self-contained and step 5 no longer reads the
Evidence Store, so validation may run after a daemon restart. The stopped
daemon can no longer accept `events.clear`.

The stdio RPC frame limit is 1 MiB, so large authoritative Session exports must
be assembled from negotiated bounded pages before invoking this offline CLI.
The strict `BundleSource` input is independently capped at 8 MiB, below the
Session Bundle's default 16 MiB manifest budget. A Bundle's hashes prove
internal integrity and detect accidental or partial modification; they are not
a signature and do not prove who created the Bundle.

Both commands write a compact JSON summary to stdout. Errors go to stderr and
never echo the source JSON contents.
