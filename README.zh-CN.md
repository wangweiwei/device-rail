<p align="center">
  <img src="docs/assets/devicerail-logo.png" alt="DeviceRail 设备自动化基础设施 Logo" width="120">
</p>

<h1> <center>DeviceRail</center></h1>

**开源、跨语言的设备自动化与测试证据基础设施。**

简体中文 · [English](README.md)

![许可证：Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)
![Rust 1.85+](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)
![Node.js 22+](https://img.shields.io/badge/Node.js-22%2B-339933.svg)
![Python 3.11+](https://img.shields.io/badge/Python-3.11%2B-3776AB.svg)

DeviceRail 通过一套有明确资源边界的 JSON-RPC 协议，为测试框架、开发者工具和
AI Agent 统一控制 Android、iOS、HarmonyOS、macOS、Windows、Linux、RDP
和 Playwright。每次执行都会生成顺序确定的事件和内容寻址的测试证据，可实时传输、
断线续传、离线回放和独立校验。

> **项目状态：** 当前版本为 `0.1.0` alpha。协议、平台 Driver、生成链和确定性
> 测试已经实现；用于生产环境前，仍需在目标真机、操作系统权限、远程桥接和签名环境中
> 完成独立验收。

**文档导航：** [快速开始](#快速开始) · [架构](docs/architecture.md) ·
[项目目录](docs/project-structure.md) · [平台支持](docs/platform-support.md) ·
[性能工程](docs/performance.md) · [文档中心](docs/README.md) · [功能清单](ROADMAP.md) ·
[参与贡献](CONTRIBUTING.md)

## 为什么选择 DeviceRail

- **统一设备协议：** 使用版本化协议完成设备发现、选择、观察、动作执行、事件流和导出。
- **测试证据优先：** 截图和媒体通过 SHA-256 引用流转，不把大块二进制数据嵌入 JSON。
- **一致的 Driver 契约：** 所有平台 Driver 都运行共享的生命周期、能力、观察、动作、
  错误和 Evidence Conformance Suite。
- **不绑定 AI 厂商：** Rust 内核不包含模型 SDK、Prompt、Planner、YAML、Recorder UI
  或 Visualizer UI。
- **安全失败：** 帧大小、队列、超时和资源均有上限；错误、取消、租约和受保护动作均为
  显式语义。
- **跨语言单一事实源：** Rust Client 直接复用规范 Rust DTO；同一组 DTO 生成 JSON
  Schema、TypeScript 类型、Python 类型和跨语言 Golden Fixtures，避免重复手写协议模型。

## 支持的平台

| 目标      | DeviceRail 集成                         | 宿主依赖                             | 当前验收范围                                     |
| --------- | --------------------------------------- | ------------------------------------ | ------------------------------------------------ |
| Android   | ADB Driver                              | Android Platform Tools               | Conformance + daemon 确定性 E2E                  |
| iOS       | Direct WDA 或 Appium XCUITest Driver + Host Supervisor | Xcode；Appium 模式使用 Appium/XCUITest Driver；Direct WDA 使用 WDA project；只有 Direct-WDA 真机需要 `iproxy` | Conformance + 确定性 daemon 生命周期 E2E；一条历史 Direct-WDA 真机冒烟链路；无设备/版本矩阵 |
| HarmonyOS | HDC Driver                              | DevEco/HDC                           | Conformance + daemon 确定性 E2E                  |
| macOS     | 原生 Desktop Driver                     | 录屏与辅助功能权限                   | Conformance + 宿主 inventory                     |
| Windows   | 原生 Desktop Driver                     | 交互式桌面会话                       | Conformance + CI 编译/inventory                  |
| Linux     | X11/Wayland Desktop Driver              | 显式配置截图和输入工具               | Conformance + fake host tools                    |
| Web       | Playwright Remote Driver                | 已存在且版本兼容的 Playwright Server | Conformance + 有界 bridge 测试                   |
| RDP       | RDP Remote Driver                       | 运维方管理的 loopback bridge         | Conformance + loopback framing 测试              |

DeviceRail 不下载平台 SDK、不内嵌 RDP 协议栈，也不下载浏览器。managed Direct WDA 会
构建并监管显式选择的 WDA project；managed Appium 会监管显式选择的 Appium 可执行文件，
通常由 XCUITest Driver 管理其已安装的 bundled WDA。两种模式都不会保存 Apple 账号凭据。
Managed discovery 同时支持真机和已经 Booted 的 iOS Simulator；DeviceRail 不创建或启动
Simulator。各平台的
准确能力边界和配置入口见[平台支持说明](docs/platform-support.md)。

Web Driver 可直接连接公开的 `browserType.launchServer()` 端点。stock helper 会在 daemon
生命周期内复用同一个连接；仅当 Server 没有暴露页面时，才创建一个空白 Context/Page，
不依赖 Playwright 私有参数。`elementExists` 和 `textContains` 分别返回严格的
`{ "exists": boolean }` 与 `{ "contains": boolean }`，可用于程序化检查页面结果。
bridge v4 新增 `waitForSelector`（等待元素到达指定状态）、`clickByText`（点击唯一
可见文本匹配，歧义即失败）与 `readValueNearLabel`（运行期按几何读取标签旁的数值，
返回有界的 `{ "value": string }`）——全部 fail-closed，线上只传数据、不传代码。

## 核心架构

```text
AI Agent / Rust、TypeScript 或 Python SDK / CLI / 测试框架
                  |
                  | JSON-RPC 2.0：stdio 或 loopback TCP/NDJSON
                  | 可选 loopback WebSocket 事件数据面
                  v
           DeviceRail daemon
                  |
        +---------+----------+
        |         |          |
     observe   execute     events/evidence
        |         |          |
        +---------+----------+
                  |
 Android / iOS / HarmonyOS / Desktop / RDP / Playwright / Plugin / Remote
```

依赖保持单向：

```text
protocol -> core -> drivers -> daemon
    |
    +-----> 官方 Clients -> apps
```

Rust trait 只是进程内实现细节，跨语言边界始终是公开 wire protocol。Recorder 和
Visualizer 只通过 `TestEvent`、`Observation`、`ActionResult` 与 Evidence 引用通信。
Rust Client 直接依赖 `protocol`，通过公开 wire protocol 与 daemon 通信，不链接 Core、
Driver 或 daemon 实现。

## 快速开始

### 环境要求

- Rust `1.85+`；
- Node.js `22+` 与 pnpm `9.3+`（TypeScript 工作区）；
- Python `3.11+`（Python Client）；
- 目标平台所需的 ADB、WDA、HDC 或桌面工具仅在启用相应 Driver 时需要。

iOS 可先运行诊断：

```sh
cargo run -p devicerail-daemon -- ios doctor
```

Managed 模式设置 `DEVICERAIL_IOS=auto|required`。Direct WDA 还需要
`DEVICERAIL_IOS_WDA_PROJECT=/path/to/WebDriverAgent.xcodeproj`；Appium 可以省略该路径，
由 XCUITest Driver 管理其安装目录中的 bundled WDA，并支持真机和 Simulator。DeviceRail 将
`devicectl`（失败时回退 `xcdevice`）的真机 inventory 与 `simctl` 的 Simulator inventory
合并；只有 available 且 `Booted` 的 Simulator 才视为已连接。显式
`DEVICERAIL_IOS_DEVICE_TOKEN` 可以选择任一类型；未指定时优先选择唯一已连接真机，只有没有
真机时才选择唯一 Booted Simulator，多目标会要求明确 UDID。DeviceRail 不会自动创建或启动
Simulator。

Direct WDA 或显式 attach 的 Appium WDA 会缓存 WDA 构建、启动 `xcodebuild` 并等待 WDA
ready。真机还执行签名、信任/Developer Mode 检查并使用 `iproxy`；Simulator 跳过这些真机
步骤，让 WDA 直接监听选定的宿主本地端口。`auto` 启动时没有可用目标也会保持 daemon 运行；
之后接入真机或由操作者启动 Simulator，可在不重启 daemon 的情况下注册路由。已发布路由固定
到原 UDID，不会在故障期间漂移到其他目标；bundled-WDA Appium 由 XCUITest Driver 管理 WDA
启动和恢复。首次“信任此电脑”、Developer Mode、UI Automation 与开发者证书信任只适用于
真机，仍由用户确认；`required` 仍保持启动失败即关闭的语义。

iOS 路由通过 `DEVICERAIL_IOS_BACKEND=direct-wda|appium` 选择唯一的 Session owner，
默认值 `direct-wda` 保持现有行为。选择 `appium` 时必须二选一：配置数字 loopback HTTP
地址，或让 daemon 托管用户已经安装的 Appium 可执行文件：

```sh
export DEVICERAIL_IOS_BACKEND=appium
# 外部服务模式
export DEVICERAIL_IOS_APPIUM_ENDPOINT=http://127.0.0.1:4723

# 托管进程模式（不能与上面的 ENDPOINT 同时设置）
export DEVICERAIL_IOS_APPIUM_PATH=/absolute/path/to/appium
export DEVICERAIL_IOS_APPIUM_PORT=0
export DEVICERAIL_IOS_APPIUM_BASE_PATH=/
```

`DEVICERAIL_IOS_SESSION_TARGET=native|safari` 选择初始 XCUITest Session 目标，默认
`native`。`safari` 只允许 Appium 后端使用；Direct WDA 配置 `safari` 会在启动时被拒绝。

Appium `/status` ready 只表示服务可接收请求，不证明 XCUITest extension 已安装；需先执行
`appium driver install xcuitest`。缺失或版本不兼容会在 `device.connect` 时返回明确的 Session
创建错误。

`DEVICERAIL_IOS_APPIUM_NEW_COMMAND_TIMEOUT_SECONDS` 会为 stock daemon 创建的每个
Appium Session 注入类型安全的 `appium:newCommandTimeout`。默认值为 `600` 秒，只接受
`1..=3600`；零、非数字和越界值会让 daemon 启动失败。它用于避免 Appium 默认 60 秒空闲
回收在人工或 Agent 暂停期间删除健康 Session。若下一次操作前收到明确的
`invalid session id`，Driver 会重建 Session；结果不明确的写操作不会被自动重放。

Appium 后端只在显式提供 external WDA endpoint 或 managed WDA project 时注入
`appium:webDriverAgentUrl`；两者都没有时会省略该 capability，由 XCUITest Driver 管理 bundled WDA。
外部服务模式在 inventory 阶段不会连接 Appium；托管进程模式只使用固定的
`--address 127.0.0.1`、`--port`、`--base-path` 参数启动 Appium，执行有界 readiness/提前
退出检查，并在 daemon 的完整生命周期内持有和有界关闭子进程。`device.connect` 创建一个
XCUITest W3C Session，
`device.disconnect` 删除它，Direct WDA 与 Appium 不会同时持有同一路由的 Session。
stock daemon 只注入受限的 XCUITest、UDID、设备名、系统版本、Safari WebView 发现和可选 WDA URL，
不接受任意 capability JSON、进程参数或凭据。Appium、Node.js、XCUITest Driver 和平台包仍由
用户安装，DeviceRail 不会下载或安装；选择该后端后，`devicerail-daemon ios doctor` 会增加
有界且脱敏的外部 `/status` 检查，或临时启动托管进程完成 readiness 检查后再关闭。当前仓库
包含确定性 transport/daemon E2E，完整 Appium 真机/Simulator 版本矩阵仍需外部验收。

### 启动 daemon

```sh
cargo run -p devicerail-daemon
```

默认 stdio 传输每行接收一个 JSON-RPC 请求。连接的第一个成功请求必须是
`system.hello`：

```jsonl
{"jsonrpc":"2.0","id":"hello-1","method":"system.hello","params":{"client":{"name":"example-client","version":"0.1.0"},"protocol":{"ranges":[{"major":1,"minMinor":0,"maxMinor":5}]},"features":{"required":[],"optional":["device.routing.v1","device.semanticActions.v1","events.stream.v1","media.stream.v1","observation.uiSnapshot.v1","verdict.record.v1"]}}}
{"jsonrpc":"2.0","id":"devices-1","method":"devices.list","params":{}}
{"jsonrpc":"2.0","id":"select-1","method":"device.select","params":{"deviceId":"mock-1"}}
{"jsonrpc":"2.0","id":"connect-1","method":"device.connect","params":{}}
{"jsonrpc":"2.0","id":"session-1","method":"session.start","params":{}}
{"jsonrpc":"2.0","id":"observe-1","method":"device.observe","params":{}}
{"jsonrpc":"2.0","id":"session-2","method":"session.end","params":{"outcome":"completed"}}
```

握手只协商协议版本和 Feature，不会隐式发现、选择或连接设备。Observation 和 Action
必须处于活动 Session 中，保证事件、Evidence 和回放关系完整。

协议 1.5 定义了 Native Accessibility 与 Safari/WebView 共用的 UI Snapshot、Selector、
稳定节点引用和语义 Action 契约。UI Tree 作为当前 Session 拥有的类型化 Evidence 保存，
在线读取只能按当前 Session 的 `observationId` 调用 `ui.snapshot.get`；调用方不能提交任意
AssetRef 或 Session ID。`verdict.record` 只持久化调用方给出的
`pass|fail|unknown`，不会由 DeviceRail 内核替模型做判断。握手支持这些 DTO 不等于当前
Driver 已经广告元素操作能力。Core 只会在对应 Feature 协商成功后为当次操作启用新增的
UI/执行字段；旧连接继续保持 Protocol 1.0～1.4 的 wire 形状，不会收到未知的 1.5 字段。

iOS Appium 后端已实现全部五个 Action：Native context 使用规范化 WDA accessibility tree，
Safari/WebView context 使用 DOM 与 W3C element 语义。节点引用绑定返回的 Observation、
context、document epoch 与稳定身份，坐标不会成为隐式语义降级。`setElementValue` 是
Protected Action，其值、截图和 UI Tree 正文不会进入 Session 事件或 Evidence 记录。

### 官方客户端

官方 Rust、TypeScript 和 Python Client 使用同一套公开 wire contract。Rust Client
可从 crates.io 安装：

```sh
cargo add devicerail-client
```

```rust
use devicerail_client::{
    CallOptions, DeviceRailClient, SpawnConfig, default_hello, methods,
};

async fn list_devices() -> Result<(), devicerail_client::ClientError> {
    let client = DeviceRailClient::spawn(SpawnConfig::new(
        "devicerail-daemon",
        default_hello(),
    ))
    .await?;
    let devices = client
        .call::<methods::DevicesList>(methods::NoParams, CallOptions::default())
        .await?;
    println!("{:?}", devices.devices);
    client.close().await?;
    Ok(())
}
```

`spawn` 通过 stdio 托管 daemon 子进程；`attach` 接收调用方拥有的异步读写端，
`connect_tcp` 只连接显式启用、端口非零的 IPv4/IPv6 loopback TCP listener，并在
打开 socket 前拒绝远程地址；`attach` 保留为调用方自管可信隧道或鉴权前导的逃生口。
三者都在返回前完成 `system.hello`。Rust Client 直接使用
`devicerail-protocol` 的请求/响应 DTO，不维护第二套 wire model。内置 TCP 路径当前
未实现可选 `remote-auth` HMAC pre-hello 交换，因此不能直接连接配置了
`DEVICERAIL_RPC_CREDENTIALS` 的 listener。

事件流通过 `open_event_stream()` 打开。后台 socket actor 负责接收并校验，
`next()` 只推进应用交付边界；应用持久接受事件后必须显式调用
`confirm(&cursor)`。已结束的 stream 才能调用 `resume()`，且只会从最后 confirmed
cursor 获取新的单次 capability，避免把“收到”误当成“已处理”。详见
[Rust Client](crates/client/README.md)、[TypeScript Client](packages/client/README.md)
和 [Python Client](packages/python-client/README.md)。

### 安装 TypeScript 依赖并验证

```sh
pnpm install --frozen-lockfile
pnpm protocol:types:check
pnpm client:typecheck
pnpm client:test
```

### 验证 Rust 工作区

```sh
cargo fmt --all -- --check
cargo run -p devicerail-schema-gen -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

完整命令和贡献流程见[贡献指南](CONTRIBUTING.md)。

## 主要能力

### 协议与多设备路由

- JSON-RPC 2.0 子集与显式版本/Feature 协商；
- 自动生成 Draft 2020-12 JSON Schema；
- Rust、TypeScript、Python 共用 Golden Fixtures；
- 多 Driver Registry、连接级选择、Device Pool、健康检查和 owner-bound lease；
- 请求超时、动作超时、取消和优雅退出。

### Session、Evidence 与可视化

- append-only、sequence-authoritative 的 `TestEvent`；
- SHA-256 内容寻址、去重、原子发布和 Session 引用；
- 可校验的离线 Session Bundle；
- Execution Recorder、离线 Visualizer 与实时 Visualizer；
- WebSocket 事件流、背压、确认和断线续传；
- 截图/媒体帧通过 Evidence Reference 进入统一事件序列。

### SDK 与生态接口

- 官方异步 Rust Client，直接复用协议 DTO，支持 stdio/TCP control plane 与可恢复事件流；
- 类型安全的 Node.js/TypeScript stdio Client；
- Python 3.11+ typed async Client；
- AI Provider 无关的 Tool Adapter；
- 可选、有界的 YAML 兼容适配器；
- 进程隔离的 Driver Plugin ABI；
- loopback RPC 鉴权、授权、审计与分布式设备路由；
- Linux、macOS、Windows 的可验证发行档案、SBOM 和 provenance。

## 项目目录

| 目录         | 职责                                                            |
| ------------ | --------------------------------------------------------------- |
| `crates/`    | Rust 协议、Client、内核、Driver、daemon、Evidence、Bundle 和传输实现 |
| `packages/`  | TypeScript/Python 协议、Client、Recorder、Visualizer 与 Adapter |
| `apps/`      | 位于协议和 Client 之上的产品应用                                |
| `protocol/`  | 生成后的 JSON Schema 与协议说明                                 |
| `docs/`      | 架构、平台支持、目录和维护文档                                  |
| `packaging/` | 确定性发行包、签名验证、SBOM 与安装脚本                         |
| `.github/`   | CI、发行流程、Issue 与 Pull Request 模板                        |

完整职责边界见[项目结构](docs/project-structure.md)。

## 常见问题

### DeviceRail 是 Appium 的替代品吗？

不是直接兼容的 Appium 替代实现。DeviceRail 是更小的设备控制与证据运行时，重点是
版本化跨语言协议、Driver 一致性、明确的资源边界和可移植证据。可选的 iOS 后端使用
运维方安装的 Appium/XCUITest，可连接外部服务，也可由 daemon 有界监管本地进程；它仍只
通过 DeviceRail 协议工作，不会把 Appium API 作为公开 wire 协议。ADB、Appium/WDA、HDC、
Playwright 和 RDP bridge 等平台服务仍在 kernel 之外。

### DeviceRail 是否自带 AI Agent？

不自带。DeviceRail 提供 Provider 无关的能力描述与可选 Tool Adapter；模型、Prompt、
Planner、审批策略和 Agent Memory 由上层宿主负责。

### 一个 daemon 能否同时管理多种设备？

可以。Driver Registry 与 Device Pool 可以暴露异构设备路由，并提供连接级选择、租约、
健康检查、取消和 Session 级 Evidence 隔离。

### 是否支持跨机器控制？

支持可选的分布式 peer 协议，但跨主机身份、加密和传输完整性必须由 SSH 或 mTLS
隧道提供。stock listener 只绑定数字形式的 loopback 地址，不提供公网 TLS endpoint，
也不声称实现分布式共识。

### 截图和录制保存在哪里？

媒体写入文件系统 Evidence Store，并以 SHA-256 引用进入事件。结束后的 Session 可导出
为独立校验、离线查看的 Session Bundle。

## 安全

不要在公开 Issue 中披露漏洞。请按照[安全策略](SECURITY.md)使用 GitHub Private
Vulnerability Reporting 或 Security Advisory。DeviceRail 的 loopback 限制不等于远程
身份认证；跨主机部署必须配置外部 SSH/mTLS。

## 参与贡献

欢迎提交缺陷修复、平台适配、协议测试和文档改进。提交前请阅读：

- [贡献指南](CONTRIBUTING.md)
- [行为准则](CODE_OF_CONDUCT.md)
- [安全策略](SECURITY.md)
- [支持范围](SUPPORT.md)
- [架构约束](docs/architecture.md)

## 许可证

DeviceRail 使用 [Apache License 2.0](LICENSE) 开源。第三方依赖保留各自许可证；发行包
包含第三方许可证清单与 SPDX SBOM。

关键词：设备自动化、移动端自动化测试、Android 测试、iOS 真机测试、iOS Simulator 测试、HarmonyOS
测试、桌面自动化、Playwright、RDP、AI Agent Tool、JSON-RPC、测试证据、Rust、
TypeScript、Python。
