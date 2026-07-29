# DeviceRail Session Bundle

`devicerail-session-bundle` writes and validates the platform-neutral,
offline Session Bundle v1 format. It depends only on DeviceRail protocol/core
contracts and has no Driver, daemon, recorder, visualizer, or product runtime
dependency.

## Canonical v1 directory

```text
manifest.json
assets/sha256/<64 lowercase SHA-256>  # omitted when no asset is reachable
```

The canonical manifest embeds the explicit event protocol version, ended
`SessionInfo`, complete sequence-ordered `TestEvent` list, and digest-sorted
asset index. JSON object keys are recursively sorted; encoding is compact
UTF-8 with one trailing LF. Asset paths are fixed derivatives of their digest.

Only typed references are copied: Observation screenshots, successful Action
before/after screenshots and result Evidence, Verdict Evidence, and Protocol
1.4 media-frame Evidence. Repeated references share one file. Unreferenced Store
pins and screenshot omissions do not create Bundle assets.

## APIs

- `export_directory` validates an ended `BundleSource`, reads through the
  minimal `BundleEvidenceSource`, hashes every copied byte, validates staging,
  and atomically publishes without replacing a target.
- `validate_directory` enforces format/resource/state-machine/path/index/hash
  invariants and returns a reconstructed `SessionExport` for offline replay.
- `read_validated_asset` reopens one validator-confirmed digest with the
  no-follow primitive, applies a caller-supplied lower byte ceiling, and
  returns only owned bytes after exact size and SHA-256 checks. It never
  exposes or resolves a manifest path. Unix uses `O_NOFOLLOW`; Windows opens
  the final handle with `FILE_FLAG_OPEN_REPARSE_POINT` before inspecting it.
- Any `EvidenceStore` implements the read-only Bundle source adapter. Callers
  must serialize export with Session cleanup and GC; the CLI achieves this by
  opening the stopped daemon's exclusively locked `FileEvidenceStore`.

Bundle hashes prove internal consistency, not origin authenticity. Bundle v1
does not include signatures or zip. The tree must not be concurrently replaced
by another process with the same filesystem authority during an operation;
that stronger claim requires a future directory-handle-relative implementation.
