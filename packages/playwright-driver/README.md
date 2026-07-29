# Playwright Remote Driver bridge

This private package is the bounded Node side of DeviceRail's conformant
Playwright Remote Driver. The public DeviceRail Driver, evidence writes,
cancellation, action schemas, and lifecycle remain in Rust. This helper reads
bounded newline-delimited JSON requests from stdin, keeps one Playwright
connection for its process lifetime, and writes exactly one bounded response
line per request. The Rust Driver reuses that helper and connection across
operations, then terminates and recreates both after an I/O failure, timeout,
or cancellation.

It intentionally uses `playwright-core`: no browser is downloaded or launched.
The operator owns the remote server and its authentication boundary. Endpoint
URLs accept only `ws://` or `wss://` without URL credentials or fragments, and
the endpoint is never placed in child-process arguments or public diagnostics.
Playwright requires compatible client/server major and minor versions; this
package pins its exact client version.

Bridge wire v3 derives the page fingerprint from Playwright's server-owned
`Page.guid`, not from mutable URL/title/viewport state. The raw GUID is never
returned: the helper sends a domain-separated SHA-256 fingerprint. A missing
GUID, an older helper, page reordering, or same-state page replacement fails
closed before `fillSecret` or any other action reaches a locator.

A standard public `browserType.launchServer()` endpoint starts without a
connection-owned context. On its first connection, the helper creates one
context and one blank page when the endpoint exposes none; if contexts exist
but all are empty, it creates a page in the first context. It does not use
Playwright private connection options. The created page remains available
because the helper keeps that same connection alive.

Build and validate:

```sh
pnpm --filter @devicerail/playwright-driver typecheck
pnpm --filter @devicerail/playwright-driver test
pnpm --filter @devicerail/playwright-driver build
```

The driver accepts one CSS selector dialect and the system port prefixes it
with Playwright's explicit `css=` engine marker. `fillSecret` is protected: no
screenshot is captured and no URL/title derived after the fill is returned.
`elementExists` returns `{ "exists": boolean }`; `textContains` requires
exactly one CSS match and returns `{ "contains": boolean }`, with optional
`caseSensitive` defaulting to `true`. These are read-only, bounded Driver
actions intended for machine-checkable assertions.

Bridge v4 adds three actions. `waitForSelector{selector, state?}` waits until
the strict-CSS target reaches one closed element state (`attached` | `detached`
| `visible` | `hidden`, default `visible`). `clickByText{text, exact?}` clicks
the single VISIBLE element whose text matches (`exact` defaults to `true`;
hidden duplicates are ignored; ambiguity fails closed). `readValueNearLabel
{label, direction?, exact?}` resolves a label→value layout relation by
GEOMETRY at run time — the fixed in-page algorithm finds the unique visible
label, then the nearest visible text element in the requested direction
(`right` | `below`, default `right`), polling until the relation resolves —
and returns `{ "value": string }` (control characters stripped, capped at
4096 code points; absent or ambiguous labels, and distance ties, fail closed).
Only `{label, direction, exact}` data ever crosses the bridge, never code.
Wait-shaped actions answer within the request budget minus a fixed headroom so
their fail-closed responses beat the daemon's hard exchange deadline.
Real-browser acceptance requires an explicitly provisioned remote Playwright
server; unit and conformance tests do not claim that external environment.
