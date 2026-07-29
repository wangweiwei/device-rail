# DeviceRail remote authentication and audit

This crate is the optional security gate for DeviceRail's loopback TCP RPC
listener. It does not add TLS and does not make the daemon safe to bind to a
public or LAN address. The daemon continues to require a numeric loopback
address. A client on another host must connect through a separately configured,
authenticated SSH or mTLS tunnel that supplies server identity, encryption,
and transport integrity.

## Daemon configuration

Authentication is disabled when both settings are absent, preserving the
existing loopback TCP behavior. Enabling it requires all three settings; a
partial configuration fails startup:

```text
DEVICERAIL_RPC_LISTEN=127.0.0.1:47831
DEVICERAIL_RPC_CREDENTIALS=/owner-only/devicerail-credentials.json
DEVICERAIL_RPC_AUDIT_LOG=/owner-only/devicerail-audit.jsonl
```

The credential file follows `protocol/credential-store-v1.schema.json`, must be
a real owner-owned regular file with no group/other permission bits, and is
bounded to 128 KiB and 64 declarations. Secrets are canonical unpadded base64url
and decode to 32–64 bytes. Duplicate principal/key pairs and inconsistent
permissions across a principal's rotating keys are rejected. Paths and secrets
are redacted from `Debug` and startup errors.

Darwin/macOS additionally requires the credential file, audit file, and audit
parent directory to have no extended ACL entries. ACL retrieval failure cannot
be distinguished from a safe owner-only object and therefore fails closed with
the existing `*_permissions_unsupported` classification.

Windows configuration currently fails closed because the standard-library
implementation cannot prove an owner-only ACL. Unconfigured Windows TCP remains
compatible. This is an explicit capability limit, not an owner-only claim.

## Challenge-response v1

Before `system.hello`, the client sends exactly `auth.challenge`, then
`auth.respond`. Both are JSON-RPC requests whose params follow
`protocol/auth-v1.schema.json`.

1. The client creates a fresh 32-byte nonce and names a principal and key ID.
2. The server returns a random 16-byte single-use challenge ID, random 32-byte
   nonce, `HMAC-SHA256`, and a 10-second relative expiry.
3. The client computes HMAC-SHA256 over the versioned, length-prefixed protocol
   context using the credential secret. `compute_proof` is the reference client
   implementation.
4. The server consumes the challenge on every proof attempt and compares the
   fixed-size proof in constant time. Replays, wrong IDs, expiry, unknown
   principals, unknown keys, and wrong proofs share a generic failure.

Authentication has a 15-second connection deadline, at most eight non-empty
prelude frames and three generated challenges, 1 MiB framing inherited from
the daemon, strict DTOs, canonical
nonce encoding, and no secret on the wire. An unknown principal still receives
the same challenge shape and runs an HMAC comparison against a random dummy
key.

HMAC authenticates the client but neither encrypts the post-authentication
connection nor authenticates the server. Do not expose raw loopback TCP through
port forwarding that lacks SSH/mTLS peer authentication.

## Authorization

Permissions are hierarchical: `admin` includes `control` and `read`; `control`
includes `read`. Every current application RPC method has an explicit minimum
permission. Unknown and future methods have no default and are denied.

- `read`: handshake/describe, device inventory/capabilities, Session/Event read
  methods, and event stream opening.
- `control`: device selection/lifecycle/observe/execute, media stream
  start/capture/end, cancellation, and Session start/end.
- `admin`: destructive `events.clear`.

Authorization and its durable audit admission happen before method dispatch.
Every v1 record carries the fixed wire stage `securityAdmission`; therefore
`outcome: succeeded` means that this security admission completed, not that the
subsequently dispatched RPC reached a successful terminal result. An audit
failure closes the connection without admitting the operation.

## Audit log

The audit file and its parent directory are owner-only, the file is exclusively
locked by the daemon, opened with no-follow semantics and inode matching,
appended with `O_APPEND`, and synced per record. Any append or sync failure
permanently poisons the writer for that process. Each
canonical JSONL record contains only sequence/time, connection ID, principal,
the fixed security-admission stage, method, required permission, decision,
bounded error code, and the previous and current SHA-256 hashes. It never stores
RPC params, nonces, proofs, credentials, device output, or secrets.

Startup verifies the entire chain and resumes its sequence/hash. Truncation,
partial crash records, reordered lines, unknown fields, sequence gaps, and hash
or content changes fail closed as corruption. The 64 MiB limit requires log
rotation while the daemon is stopped.

The hash chain is tamper-evident, not a signature. A privileged or same-account
attacker who can rewrite the whole file and recompute every hash is outside this
claim. Preserve signed/off-host checkpoints when stronger audit non-repudiation
is required.

Run the deterministic crate gates with:

```sh
cargo test -p devicerail-remote-auth
cargo clippy -p devicerail-remote-auth --all-targets -- -D warnings
```
