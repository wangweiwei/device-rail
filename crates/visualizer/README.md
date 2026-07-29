# DeviceRail Offline Visualizer

`devicerail-visualizer` opens a canonical Session Bundle through the Rust
Session Bundle validator and serves a read-only, sequence-ordered timeline on a
temporary loopback capability URL. It does not require a daemon, device, or
network connection and has no Driver, client, Recorder, AI, Prompt, or YAML
dependency.

```sh
cargo run -p devicerail-visualizer -- ./session.bundle
```

The five timeline filters have the same event semantics as the live Viewer.
`observations` contains `observationCaptured` and `mediaFrameCaptured`; media
stream start/end boundaries remain visible only in `all`.

The command prints the URL once and runs until interrupted. The URL includes a
fresh random capability path; anyone who can read it while the process is
running can access the local report, so do not publish or persist it.

## Trust and rendering boundary

- `validate_directory` is the sole authority for Bundle format, event state,
  typed Evidence reachability, and initial size/hash checks. The browser never
  parses `manifest.json`.
- HTML is generated on the server, contains no scripts, escapes and bounds all
  event text/JSON, stops construction at 2 MiB, and pages at most 50 events.
  CSS is a fixed local resource.
- Routes use only a validated lowercase digest. They never resolve
  `AssetRef.uri`, a manifest path, `file://`, or an absolute path.
- Every asset response reopens the digest-derived file without following the
  final component, applies a lower byte limit, and rechecks exact size and
  SHA-256 before returning owned bytes.
- Inline preview is restricted to exact `image/png` assets that pass bounded
  container, CRC/Adler, DEFLATE, static-frame, dimension, pixel, decoded-size,
  and exact-IEND validation. Other media are attachment-only.
- The local HTTP subset is GET-only, checks the exact numeric loopback `Host`,
  disables CORS and caching, and applies CSP, request, concurrency, asset, and
  shutdown limits. Rendering and PNG decoding use separately bounded blocking
  workers whose memory permits remain held through response completion.

Bundle hashes provide internal integrity, not origin authentication. The UI
always displays that the Bundle is unsigned. Another process with the same
filesystem authority must not replace intermediate Bundle directories while
the Viewer is running; this is the same explicitly documented limit as Bundle
v1 validation.

## Static report validation

`devicerail-report validate` does not trust `report.json` as a security policy.
It requires `style.css` to match the built-in stylesheet byte-for-byte, requires
one exact CSP meta policy on every page, accepts only the generator's inert HTML
tag/attribute subset, and resolves every `href`/`src` to a declared local page
or Evidence file. Updating page/CSS hashes in `report.json` cannot authorize
scripts, active markup, external links, a weakened CSP, or modified CSS.
