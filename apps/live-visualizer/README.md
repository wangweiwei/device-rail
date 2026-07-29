# DeviceRail live visualizer host

This private Node.js application attaches a bounded live timeline to one
explicit Session on an already-negotiated `DeviceRailClient`.

```ts
const viewer = await bindLiveVisualizer({ client, sessionId });
const localUrl = viewer.endpoint.exposeSecret();

// Open localUrl in a trusted local browser, then detach when finished.
await viewer.close();
```

The endpoint method is deliberately explicit because its 256-bit path is a
temporary capability. Logging, serializing, or sharing that URL grants access
to the local view. Its default string, JSON, and inspection representations are
redacted.

The host does not select a device, start or end a Session, or close the supplied
client. It binds only an ephemeral numeric IPv4 loopback address. Browser
routes expose sanitized presentation pages and SSE revision invalidations;
they never expose the daemon WebSocket capability, Evidence URIs, filesystem
paths, asset bytes, or a download/network proxy.
