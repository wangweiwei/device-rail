# Security policy

DeviceRail controls input devices and captures test evidence. Treat reports
involving authorization bypass, command execution, secret exposure, path
traversal, unsafe remote binding, malformed protocol handling, evidence
integrity, or denial of service as security-sensitive.

## Supported versions

The project is currently alpha. Security fixes are applied to the latest code
on the default branch and to the most recent released `0.1.x` version when a
release exists. Older snapshots are not maintained.

## Reporting a vulnerability

Do not open a public issue or discussion. Use the repository's GitHub
**Private vulnerability reporting** form or open a private draft Security
Advisory. Include, when safe:

- affected version or commit;
- platform and deployment topology;
- minimal reproduction steps;
- expected and observed security boundary;
- impact and required attacker access;
- whether credentials, personal data, or device identifiers were exposed.

Remove real tokens, signing material, Apple/Android account data, UDIDs, serial
numbers, private hostnames, and screenshots containing personal information.
Maintainers will acknowledge a complete report, coordinate validation and a
fix, and credit reporters who want attribution. Response times are best effort
until a formal security team and encrypted contact channel are published.

If private reporting is not enabled on the eventual public repository, do not
post the vulnerability publicly. Ask the repository owner to enable it without
including exploit details.

## Deployment boundary

- Built-in RPC, WebSocket, WDA, RDP bridge, and peer endpoints are loopback
  boundaries; loopback alone does not provide remote identity.
- Cross-host deployments require separately managed SSH or mTLS tunnels.
- RPC HMAC authentication does not add TLS and does not authenticate peer-v2.
- Platform tools, WDA, Playwright servers, RDP bridges, devices, and operating
  system permissions remain external trust boundaries.
- Protected actions reduce DeviceRail persistence and screenshot exposure; they
  do not defend against a malicious host, Driver, device, application, IME,
  debugger, swap, or privileged process.

See [the architecture](docs/architecture.md) and component READMEs for the exact
bounded security claims.
