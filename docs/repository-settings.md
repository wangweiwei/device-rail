# Public repository settings

These settings cannot be encoded in Git history. Apply them after the repository
has a public GitHub remote; replace no documentation links with guessed URLs.

## About section

Recommended description:

> Language-neutral device automation and test-evidence runtime for Android,
> iOS, HarmonyOS, desktop, RDP, Playwright, and AI agents.

Recommended topics (GitHub permits up to 20):

```text
device-automation
mobile-testing
test-automation
android
ios
harmonyos
desktop-automation
playwright
rdp
webdriveragent
json-rpc
json-schema
rust
typescript
python
ai-agents
ai-tools
test-evidence
cross-platform
open-source
```

Set the website field only when a canonical project site exists. Until then,
leave it empty rather than linking to an unowned domain.

## Features

Enable:

- Issues and issue templates;
- Discussions for setup/design questions;
- Private vulnerability reporting and Security Advisories;
- branch protection or rulesets on the default branch;
- Dependabot alerts and security updates;
- secret scanning and push protection when available.

Consider disabling the wiki so durable documentation remains reviewable in
`docs/`. Enable Projects only if it becomes the canonical planning system.

## Default branch protection

- Require pull requests and at least one approving review.
- Require the Rust, TypeScript, and Python/release CI checks.
- Require conversations to be resolved.
- Block force pushes and branch deletion.
- Require linear history only if the maintainers intend to enforce it
  consistently.

## Release hygiene

- Publish signed tags and generated release notes.
- Keep package and binary versioning aligned.
- Upload only archives that pass `packaging/devicerail_release.py verify`.
- Never use unsigned CI artifacts as production releases.
- Configure signing, notarization, and package-registry secrets as environment
  secrets with protected reviewers.
- Create protected `crates-io`, `npm`, and `pypi` environments and configure
  their registry authentication as described in
  [package publishing](package-publishing.md).
- Treat crates.io `0.1.0` as a bootstrap: revoke the create-new-crate token
  after publication, add team ownership, and migrate later releases to the
  repository-bound Trusted Publisher.
- Protect stable `v*` tags from creation or movement by untrusted contributors.

## Search and generative-engine discoverability

- Keep the first README paragraph factual and repeat the stable product name,
  supported platforms, protocol, and primary use case in natural language.
- Maintain both READMEs and `llms.txt` when capabilities change.
- Use descriptive release titles and issue labels; avoid keyword stuffing.
- Once the canonical remote exists, add its URL to npm, Python, Cargo, and CFF
  metadata in one reviewed change.
- Do not add CI badges until their repository-specific URLs are known.
