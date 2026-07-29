# DeviceRail documentation

[简体中文 README](../README.zh-CN.md) · [English README](../README.md)

This directory contains durable project documentation. Package- or
Driver-specific operational details stay next to their implementation; this
index provides the stable route to them.

## Start here

| Goal | Document |
|---|---|
| Understand what DeviceRail does | [English README](../README.md) or [中文 README](../README.zh-CN.md) |
| Learn the dependency and security boundaries | [Architecture](architecture.md) |
| Find a component in the monorepo | [Project structure](project-structure.md) |
| Configure a target platform | [Platform support](platform-support.md) |
| Reproduce load, latency, RSS, and profiling checks | [Performance engineering](performance.md) |
| Review implemented scope and external validation gaps | [Roadmap](../ROADMAP.md) |
| Build and submit a change | [Contributing](../CONTRIBUTING.md) |
| Report a vulnerability | [Security policy](../SECURITY.md) |
| Build a release archive | [Release packaging](../packaging/README.md) |
| Publish crates.io, npm, and PyPI packages | [Package publishing](package-publishing.md) |

## Component reference

- [Wire protocol](../protocol/README.md)
- [Rust protocol crate](../crates/protocol/README.md)
- [Rust client](../crates/client/README.md)
- [TypeScript protocol types](../packages/protocol/README.md)
- [TypeScript client](../packages/client/README.md)
- [Python client](../packages/python-client/README.md)
- [AI Tool Adapter](../packages/tool-adapter/README.md)
- [Execution Recorder](../packages/recorder/README.md)
- [Live Visualizer](../packages/live-visualizer/README.md)
- [Session Bundle](../crates/session-bundle/README.md)
- [Evidence Store](../crates/evidence-fs/README.md)
- [WebSocket transport](../crates/websocket-transport/README.md)

## Platform Drivers

- [Android/ADB](../crates/android-adb/README.md)
- [iOS/WebDriverAgent](../crates/ios-webdriver/README.md)
- [iOS Host doctor and managed WDA](../crates/ios-host/README.md)
- [HarmonyOS/HDC](../crates/harmony-hdc/README.md)
- [macOS, Windows, X11, and Wayland](../crates/desktop-driver/README.md)
- [RDP bridge](../crates/rdp-remote/README.md)
- [Playwright bridge](../packages/playwright-driver/README.md)
- [Process plugin ABI](../crates/plugin-driver/README.md)
- [Distributed routing](../crates/distributed-router/README.md)

## Documentation rules

- Describe only behavior implemented by the current tree.
- Distinguish deterministic/conformance tests from real-device or real-network
  validation.
- Link to the owning component instead of copying configuration that can drift.
- Keep protocol field names exactly as they appear on the camelCase wire.
- Update both root READMEs when a user-visible capability or requirement changes.
