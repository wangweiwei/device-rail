# DeviceRail 功能清单

目标：先完成一个最小、稳定、零 AI 依赖的设备控制与测试证据内核，再逐步增加真实平台、录制和可视化能力。

## 实施规则

- 严格按编号顺序推进；确需调整顺序时先更新本文件。
- 每个编号应能独立实现、测试和审查，不把多个能力塞进一个改动。
- 完成标准统一包括：代码、单元/契约测试、必要文档、`cargo fmt`、`cargo check`、`cargo test` 和 Clippy。
- 协议变更必须提供序列化 fixture；Driver 变更必须通过统一 conformance suite。
- AI、Prompt、YAML、Recorder UI、Visualizer UI 永远不进入 Rust kernel。

## 已完成基线

- [x] **DR-000 Repository bootstrap**：Rust/pnpm Monorepo、协议 DTO、`DeviceDriver`、事件运行时、Mock Driver、stdio NDJSON RPC daemon。
- [x] **DR-000A Baseline validation**：协议序列化、事件、Mock Driver 共 6 个测试，Clippy 零警告，stdio 冒烟链路通过。
- [x] **DR-001 协议握手与版本协商**：以 `system.hello` 作为连接首包；按多个非连续 major range 协商最高公共 `{major, minor}`；独立协商 required/optional Feature；落实 `AwaitingHello -> Ready` 状态机、JSON-RPC 2.0 子集和稳定错误。握手不会发现、选择或连接设备。
- [x] **DR-002 协议 JSON Schema 自动生成**：初始公开 DTO 通过默认关闭的 feature 生成 41 个 Draft 2020-12 Schema，随 DR-007 扩展为 49 个、随 DR-008 扩展为 56 个、随 DR-010 的完整方法契约扩展为 97 个；独立生成器支持写入和 `--check`，CI 拒绝缺失、变化或残留文件；默认 daemon 不引入生成/验证依赖。
- [x] **DR-003 跨语言 Golden Fixtures**：初始 22 个固定 Fixture 覆盖握手、设备、Observation、Action、Result、Error、全部事件 payload 和四种 Action 终态，随 DR-007 扩展为 25 个、随 DR-008 扩展为 29 个、随 DR-010 扩展为覆盖 17 个方法请求/成功响应的 54 个；manifest 声明协议版本、模型、路径和 Schema，Rust 强制 typed round-trip、序列关联与完整性检查。
- [x] **DR-004 Driver Conformance Suite**：默认关闭的通用测试套件通过工厂复用于任意 Driver，覆盖生命周期、Capabilities、Action Schema、Observation、Result/Evidence、错误分类和事件顺序；Mock Driver 已通过完整套件。
- [x] **DR-005 Session 与事件序列**：统一事件 envelope、Session 生命周期和 append-only 内存 Store；sequence 在单一临界区内分配且为 JS-safe 连续整数；Action 具有 success/failed/cancelled/timedOut 四类显式终态，RPC 支持启动、结束、以可选 1～1000 条 `limit` 和 `afterSequence` 分页列出、导出和整 Session 删除。100 个并发 Action 可稳定关联并重放。
- [x] **DR-006 Evidence Store**：零 AI 的 object-safe 流式 Store 契约与文件系统实现；SHA-256 内容寻址、原子发布、跨 Session 去重和持久引用；Session 关闭墓碑防止慢上传重新挂载；GC 通过可恢复 trash 事务删除，并覆盖损坏、缺失、越界路径、取消、并发和崩溃中间态。
- [x] **DR-007 Timeout、取消与优雅退出**：协议 1.1 的 `request.control.v1` 提供 Request/Action 分层超时和 typed `request.cancel`；Core 在取消、超时和 Driver failure 下写入唯一 Action 终态；daemon 以有界队列并发调度设备请求，拒绝重复 ID，并在 EOF/SIGINT/SIGTERM 后按“停止接收、取消、drain、结束 Session、断开 Driver、限时 flush”顺序退出。
- [x] **DR-008 Driver Registry 与多设备路由**：协议 1.2 的 `device.routing.v1` 提供 typed `devices.list` 和 `device.select`；Core 通过稳定 `DriverHandle` 注册、排序和解析多个异构 Driver，并以逐设备 lifecycle gate 隔离生命周期与普通操作；daemon 按连接保存选择并在请求接纳时固定路由，保留单设备旧客户端兼容性，对多设备未选择和设备不存在显式失败，操作事件记录实际设备 ID，退出时并发断开全部 Driver。
- [x] **DR-009 TypeScript 协议类型生成**：`@devicerail/protocol` 从 checked-in Protocol Schema 根可重复生成隔离的 type-only 模块与 root/`v1` 入口，现随 Protocol 1.5 完整方法契约覆盖 174 个 Schema 根、自动 24 方法 `RpcMethodMap` 和 89 个 Golden Fixture；严格 TypeScript 契约检查 discriminator、空参数、`unknown`、camelCase、nullable/optional 和包导出；pnpm 9 lockfile、missing/changed/stale 检查及 Linux/Windows CI 保持 `Rust DTO -> Schema -> TS` 单向生成链。
- [x] **DR-010 TypeScript stdio Client**：`@devicerail/client` 现覆盖全部 24 个公开方法的 typed Params/Result、Schema、Fixture 与 `RpcMethodMap`，提供 Protocol 1.0～1.5 握手/Feature 状态机、双向 1 MiB NDJSON、有界串行背压、乱序 ID 关联、取消 reserve、全阶段关闭 deadline 和有界 stderr；严格校验 envelope/XOR/ID/安全整数及 hello 一致性，全部 24 个 method response/result 还会按发布包内嵌的 canonical generated Schema 执行运行时校验。Protocol 1.3 的独立事件 WebSocket 数据面、Protocol 1.4 的媒体事件与 Protocol 1.5 的 UI Snapshot、语义动作及 Verdict 持久化均按协商 Feature 校验。Mock daemon 全流程、协议 1.0 fallback、50 并发逆序、队列饱和取消、异常退出和 tarball 隔离消费者均进入 Linux/Windows CI。
- [x] **DR-011 通用 AI Tool Adapter**：`@devicerail/tool-adapter` 仅依赖 typed client/protocol，生成深度不可变、供应商无关的 Observation/Action Catalog；可移植名称固定映射原始 Action，并原样保留 Driver 已通过 conformance 的 Schema dialect/compound resources，不在 Adapter 中建立更窄的第二套协议合同或执行网络解析。调用独立关联 Agent invocation、RPC request 与自动生成的 Action UUID，严格验证并保留 Observation、ActionResult、after-observation 和 Evidence，同时透传两级 timeout、AbortSignal、取消及 RPC/Driver failure。单元/Mock daemon 测试、Linux/Windows CI 和三包 tarball 隔离消费者锁定边界，设备与 Session 生命周期仍由宿主显式管理。
- [x] **DR-012 Android ADB 发现与生命周期支撑层**：仅调用宿主已安装的 `adb`，以 crate-private、无通用 shell escape hatch 的可替换命令边界完成 `devices -l` 解析、稳定 serial/`DeviceInfo` 映射、严格状态与错误分类、boot polling、幂等生命周期及有限重连。所有设备命令显式 `-s` 路由；锁等待、backoff、`wait-for-device` 和真实子进程均受 timeout/cancel，stdout/stderr 有硬上限。确定性 fake/fixture 覆盖 malformed、unauthorized、offline、missing、permission、boot、瞬时 transport 丢失和双设备并发隔离；Unix 另验证异常退出及进程终止。为避免伪造 Observation/Action，本阶段保持 support crate，实际 `DeviceDriver`、daemon 注册与共享 conformance 在 DR-014 首次具备完整能力时一次完成。
- [x] **DR-013 Android Observation**：Core 以不可克隆的 `DriverOperationContext` 将 Session、control 与受限 Evidence writer 绑定，注入 Store 时严格核对本次 operation 的 `put`/`attach` receipt，拒绝外部、旧 operation 和未返回的 Evidence；Observation lease 阻止 Session 在 pin 与事件落盘之间结束，释放具备幂等 tombstone 与有界重试，Session ID 删除后仍不可复用。Android 通过同一 serial 的 `screencap`、`wm size`、`wm density` 生成 Observation；完整 PNG 解码验证 CRC、Adler、DEFLATE、scanline、palette 和 trailer，并对输入、chunk、尺寸、像素与解码内存设硬上限。逐设备 operation gate 让截图、几何查询和 Session pin 与 lifecycle 变更线性化；取消、超时、错误脱敏、双 Session 归属及 File Evidence Store 隔离均有确定性回归。wire 1.2 未改变，Android 继续保持支撑层，直到 DR-014 具备真实 Action 后再实现 `DeviceDriver`。
- [x] **DR-014 Android Driver 与基础动作**：首个真实 `AndroidDriver` 组合 discovery、lifecycle、Observation 与逐设备独占 Action gate，提供 `tap`、`keyPress`、`swipe`、`scroll`、`inputText` 五个闭合 typed ADB Action；坐标基于当次截图 viewport，Schema 与 parser 对 JSON 数学整数保持一致，文本仅允许 1～1024 字节安全 ASCII 且不进入 Debug、output 或公开错误。每次成功动作持久化 before/after Session Evidence 并严格核对 operation receipt；取消、超时、平台失败及 after capture 失败不伪造成功。明确 transport 错误会失效连接缓存并要求真实重连。daemon 共享单一 File Evidence Store，启动对账 orphan pin，支持 Android `auto/off|required`、稳定多路由及 delete→release 的 `events.clear`；discovery/runtime ADB runner 分离为 5/65 秒上限。Android 与 Store 运行四参数共享 conformance，Mock 也提供严格 Session Evidence 模式；Rust、TypeScript daemon E2E 与独立审查均通过。wire 保持 1.2，Secret 输入仍明确不属于本阶段的 `inputText`。
- [x] **DR-015 Android App/System 动作**：在不改变 Protocol 1.2 wire 的前提下增加 `launch`、`terminate`、`back`、`home`、`recentApps`，Android Driver 共发布 10 个 capability。`AndroidPackageName` 的闭合 Schema 与 parser 对 3～255 字节 application ID 保持等价；所有 ADB argv 均由 serial-scoped typed operation 固定生成，不开放 shell/flags/component/intent。`launch -W` 只接受唯一成功状态、允许的前台 warning 和后续完成标记；全部其他 mutation 即使宿主 `adb` 退出码为 0，也必须满足闭合 stdout/stderr 合同，阻断 legacy shell 远端失败被误报成功。五个新动作继续走 before→mutation→after 独占 gate、严格 Evidence receipt、取消/超时和 transport 失效恢复；双 serial、注入、输出解析及 10-capability conformance 均有确定性回归。Android 76 个测试、check、Clippy、格式、diff check 与独立安全审查通过；无真机 CI 仅验证命令合同，不冒充真机验收。
- [x] **DR-016 安全与脱敏策略**：保持 Protocol 1.2，以向后兼容的 `ActionProtection`、`RecordedActionCall`、`ScreenshotOmissionReason` 和 `action.protected.v1` 建立 fail-closed 边界；普通 Action/Observation wire 不变，Schema/TS 根扩展为 100 个，Golden Fixture 扩展为 56 个。Core 在 `ActionStarted` 前脱敏 protected/unknown arguments，运行时拒绝 capability/classifier 不一致，并以 `capture|omit` policy、不可写 Evidence context 和 typed omission 强制省略合同。Android 新增第 11 个 `inputSecret`，Secret 只经固定 `adb shell -T` 子进程 stdin 传输，不进入宿主 argv、事件、导出、Evidence、Action output 或已知公开诊断；before/after 仅查询 display geometry。daemon 未协商时同时隐藏和拒绝 protected action，Tool Adapter 默认隐藏并要求 Feature+显式 opt-in；EPIPE/transport 分类与远端 shell 误判经独立隐私审查修复。Rust workspace 285 项、Client 58 项、Tool Adapter 47 项、生成/三包隔离检查、Clippy 与两轮独立 adversarial review 全部通过；可证明边界及不覆盖的瞬时内存、特权/恶意组件、设备端与后续截图风险记录于架构文档。
- [x] **DR-017 Session Bundle 与可移植导出**：新增平台无关 `devicerail-session-bundle` 与离线 `devicerail-bundle` CLI，以单一 canonical `manifest.json` 和可选 `assets/sha256/<digest>` 目录固化显式事件协议版本、结束的 Session、完整有序事件及 typed reachable Evidence 去重索引。writer 在任何 Evidence I/O 前执行严格 Source/资源/state-machine 校验，逐资产流式重算大小/hash，manifest 最后写入并整包回读；Apple/Linux/Redox 使用原子 NOREPLACE，Windows 使用无替换且 write-through 的 `MoveFileExW`，取消/超时、并发目标竞争和发布后 durability-unknown 均有显式语义。validator 拒绝非 canonical JSON、额外/缺失/重复索引、symlink/reparse、篡改/截断和 protected omission 破坏；真实 File Evidence Store 覆盖五处重复引用、release+GC 后独立重放，另有 checked-in protected 零资产 fixture。v1 不增加 RPC/Feature、zip 或真实性签名；同权限 hostile 并发目录替换明确不在声明内。Rust workspace 327 项、Bundle/CLI 42 项、macOS/Windows Clippy、100 Schema/56 Fixture、Client 58、Tool Adapter 47、三包隔离检查及最终 adversarial review 全部通过。
- [x] **DR-018 Execution Recorder**：新增 `@devicerail/recorder`，以公开 `events.list`/`session.export` 为唯一在线输入，按 sequence 原子接纳完整 canonical `TestEvent`，精确重复幂等而冲突、gap、跨 Session、event/call 重用、Action correlation 与终态违规均 fail closed。Unix owner-only canonical checkpoint 采用 checksum、revision CAS、原子替换、父目录同步与可保守恢复的 PID/token writer lock，阶段只由 recording 经 ended Session 精确比对进入 sealed，再由真实离线 Bundle CLI export+validate 和 manifest identity 收敛为 completed；Source no-clobber 发布、取消线性化、预存目标恢复、损坏 Evidence 与 protected 零资产 omission 均有闭环。Protocol 1.4 的 `session.export.page.v1` 以 ended Session 原子分页、严格 continuation 和单次最终 seal CAS 消除整 Session 的 1 MiB RPC 单帧瓶颈，未协商时保留 legacy 完整响应；Bundle Source v1 仍有独立 8 MiB 上限，checkpoint 额外保留固定 64 KiB checksum/phase headroom，内存 Event Store 导致的 active-daemon-loss 与 RPC 调用间取消均显式失败；Windows 的 directory-fsync/ACL 限制被明确记录为部署前提，而非可移植的实现保证。Recorder 契约/E2E 测试覆盖约 2.23 MiB 常规大 Session、近 8 MiB recording→分页 seal→Source→真实 Bundle→completed、并发 seal 单次 CAS、取消隔离及真实 daemon 分页协商；Client、Tool Adapter、隔离发布、Schema/Fixture、Clippy 和独立 review 全部通过；无 Driver、AI、Prompt、YAML、Visualizer 或产品 UI 依赖。
- [x] **DR-019 离线 Visualizer**：新增 Rust `devicerail-visualizer`，以 `validate_directory` 为唯一 Bundle/事件/Evidence 权威并持有不可变 snapshot；GET-only Viewer 固定 `127.0.0.1`、双 UUID capability path、精确 numeric Host、严格 8 KiB HTTP/1.1 子集和无脚本/CSP/无外联的服务端分页 HTML。Timeline 按 validator-confirmed sequence 展示 Session、Observation、Action 四终态、Error、Verdict、protected omission 与 unsigned 警告；C0/C1/bidi、prototype key、长文本/JSON、Evidence 大列表、safe integer、极端 viewport/PNG ratio 均有显式有界呈现。资源路由只查 validated digest index，每次以 Unix `O_NOFOLLOW` 或 Windows `FILE_FLAG_OPEN_REPARSE_POINT` 重开同一 handle、限量读、重算大小/hash 后返回 owned bytes；仅 exact `image/png` 经完整静态 PNG container/CRC/Adler/DEFLATE/IEND、8192 单边/1600 万像素/64 MiB decoded 预算后内联，活动媒体只作 octet-stream attachment。HTML 构造硬限 2 MiB，render/asset 各最多 2 个并发内存预算，CPU 重活在 bounded blocking worker 执行且 permit 持有到写完，shutdown 等待连接与 worker 资源。Visualizer 41 项、资产二次读取 6 项、Windows cross-check、Schema/Fixture、隔离发布、Clippy、CLI+curl 冒烟和两轮独立 P0–P2 review 全部通过；无 Driver、client、recorder、AI、Prompt 或 YAML 依赖。
- [x] **DR-020 WebSocket 实时事件传输**：以 additive Protocol 1.3 `events.stream.v1` 增加 `events.stream.open`、独立 WebSocket `system.hello`/`events.subscribe`、epoch+Session+sequence cursor 及闭合 event/terminal 通知，Schema 扩展到 120、Golden Fixture 扩展到 64、公开方法扩展到 19。Core 在同一 Event Store 临界区注册 bounded tail 并捕获 `Arc<TestEvent>` replay，过旧/超前/跨 Session/跨 daemon、删除、lag、gap 与 shutdown 均显式失败；独立 transport crate 只绑定 numeric IPv4 loopback，使用 244-bit 单次短期 capability、精确 Host/path/Origin/subprotocol、无压缩、严格 header/frame/message/连接/queue/write/grace 上限，借用+capped serialization、单写者及 abort-on-drop 阻止放大、静默丢失和 task 泄漏。listener fatal error 会原子关闭 admission、清除 capability 并显式终止已有连接；可取消的 `finish_shutdown` 始终保留 accept task 所有权。daemon 仅在健康 listener 可用时协商 Feature，stdio 仍完全兼容，并按 stop admission→drain runtime→写 `SessionEnded`→自然排空 stream→bounded force 顺序退出。TypeScript client 区分 received/delivered/confirmed cursor，只有连续显式 `confirm()` 才能 resume；严格验证 replay boundary、正常终态、Schema 开放/封闭字段、Abort、late response、本地 event/byte queue 和 Node/browser Origin。完整 Rust workspace、WebSocket loopback 6 项和 Client 75 项测试合同均通过；协议生成/fixture、包隔离、Clippy、格式和修复后 P0–P2 review 均通过。仅在明确禁止 AF_INET 的 hermetic runner 中，transport crate 的 6 条真 socket/bind E2E 及其他显式标注的 loopback 集成测试才允许通过 `DEVICERAIL_ALLOW_NO_LOOPBACK=1` 显式跳过；默认 Linux/macOS/Windows CI 不设置该变量并会把任何 loopback 回归判为失败。Rust kernel、Driver、Recorder 与离线 Visualizer 均不依赖 WebSocket 实现。
- [x] **DR-021 Live Visualizer**：新增只依赖 `@devicerail/protocol` 的可发布 `@devicerail/live-visualizer` 与 private Node host；宿主注入已拥有的 typed client 和 Session，不转移 client、设备或 Session 生命周期所有权。事件严格按 prepare→bounded commit→daemon confirm→model confirm→revision publish 处理，断线只从 confirmed cursor 有界恢复；timeline 仅保存不可变、已消毒且删除 Evidence URI 的 presentation DTO，容量耗尽不确认当前事件也不淘汰历史。浏览器经 256-bit capability、exact Host/Origin、GET/HEAD-only 的 loopback HTTP 与 bounded SSE invalidation 读取分页状态；CSP、固定外部脚本、无 HTML sink、慢标签隔离、键盘/读屏/reduced-motion 与 shared DR-019 presentation fixture 锁定安全和语义。14 项 timeline 测试、12 项 app/HTTP/SSE/真实 daemon WebSocket E2E、TypeScript typecheck/build、包隔离消费检查及允许 loopback 的完整 Rust workspace 测试全部通过；Rust kernel、Driver、Recorder 和离线 Bundle validator 均不依赖 Live UI。
- [x] **DR-022 独立报告导出**：在 `devicerail-visualizer` 上新增只接收 validator-confirmed Bundle 的静态报告 exporter 与 `devicerail-report export|validate` CLI；固定 Viewer 资产、分页数据和 reachable Evidence 以 no-clobber staging 原子发布，断网可打开且 CSP 禁止外联。导出与复核都会重新验证 digest/size/PNG，protected omission 保持零资产，恶意文本有界转义，取消、篡改、目标竞争和发布失败不留下伪完成产物；实现不读取 daemon、不依赖 Driver/Recorder，也不进入 kernel。
- [x] **DR-023 Playwright Remote Driver**：新增 conformant Rust `devicerail-playwright-remote` 与 private `@devicerail/playwright-driver` one-shot Node bridge。daemon 仅在显式 `DEVICERAIL_PLAYWRIGHT_ENDPOINT` 配置时发现并注册远端 context/page，不下载或启动浏览器；固定 helper argv，endpoint/selector/text 只走 stdin，响应与诊断有硬上限并可取消。bridge v2 用 context/page ordinal 与 Playwright server-owned `Page.guid` 的域分离 SHA-256 建立稳定页面身份，重连取不到相同 GUID、页面重排或同状态替换均在动作前 fail closed；选择器强制 Playwright `css=` 引擎；八个闭合 Action 覆盖导航、点击、输入、按键、选择、滚动和等待，`fillSecret` 不截图、不返回 URL/title、不持久化参数。Rust Driver 运行共享 Evidence conformance，Node 契约/typecheck/test/build 与 daemon 配置脱敏测试通过；真实浏览器验收仍要求操作者显式提供版本兼容的远端 Playwright server，不以缺失外部环境冒充通过。
- [x] **DR-024 人工操作录制协议**：新增公共 `ManualRecording` v1 DTO、Golden Fixture、Schema/生成 TS 类型与独立 `devicerail-manual-recording` replay compiler。记录按连续 sequence、时间、唯一 call ID 和 ActionSpace SHA-256 固化人类选择的 Action 模板；编译时重新核对当前 capability、完整 JSON Schema 和 protection。标准参数可持久化，protected 参数只保存受限 opaque `secretRef`，由宿主在回放瞬时提供完整参数；ActionSpace 漂移、序列断裂、ID 重用、保护不匹配、缺失 Secret 和越界输入全部显式失败。协议保持 Driver-neutral，不暴露 Playwright/DOM 私有类型，也不把 Recorder UI 引入 kernel。
- [x] **DR-025 Screenshot/Video Stream**：以 additive Protocol 1.4 `media.stream.v1` 增加 `media.stream.start|capture|end` 生产入口以及 `mediaStreamStarted`、`mediaFrameCaptured`、`mediaStreamEnded` 闭合事件和 `MediaStreamWriter`。daemon 将流绑定到当前 Session、selected leased device 和内部 Observation producer；wire 不接受 bytes、path 或 caller `AssetRef`，只返回经 Session Evidence Store attach 的 canonical 引用。一基 `frameIndex`、caller `streamId` 和 terminal result 支持 exact lost-response retry；start/frame/terminal 各保留真实 producing request correlation，start/terminal 丢 ACK 继续使用冻结的原事件恢复。capture 支持 timeout/cancel，video 明确定义为带正 duration 的独立 PNG key-frame 序列。Feature 仅在 capture policy 与 managed Store 可用时宣称；2 active/8 per Session/1000 frames/20 fps/no queued capture 设定资源上限，protected/unknown Action 与采集原子互斥，Session end/connection cleanup/shutdown 对实际 terminal append 执行共享 deadline、并发关闭和有界 backoff，断连即使终态失败也释放 owner lease。模糊 frame append 会 poison 并恢复关闭。Core 继续拒绝 ID 重用、跳帧、media type 漂移和错误终止计数；Bundle、WebSocket client、Recorder、离线/Live Visualizer 全部验证或安全呈现媒体事件。当前 174 Schema、89 Golden Fixture、24 方法的 Rust/TypeScript/Python 生成合同、真实 daemon 生命周期及引用去重、顺序、清理、鉴权和脱敏回归共同锁定边界。
- [x] **DR-030 iOS Driver**：`devicerail-ios-webdriver` 提供互斥的 Direct WDA 与 Appium XCUITest 两个 conformant backend。Direct WDA 保留闭合 status/session/source/viewport、PNG/MJPEG observation、坐标/文本/按键/拖动兼容能力；Appium 在单一 W3C Session 内增加 Native accessibility 与 Safari/WebView DOM 双通道、规范化 UI Tree、稳定 node/context/document epoch、`findElement`/`tapElement`/`clearElement`/`setElementValue`/`waitForElement` 五个 canonical Action，以及 Protected 值与敏感子树脱敏。Session 创建/删除和变更类请求对模糊网络结果 fail closed，不能安全重试。stock daemon 可连接 numeric-loopback Appium endpoint，或用固定参数托管显式 Appium executable；托管进程有有界 readiness、意外退出监管、Unix process-group TERM→KILL 清理和完整 daemon shutdown 收敛。未提供 WDA endpoint 时不注入 `appium:webDriverAgentUrl`，由 XCUITest Driver 管理其 bundled WDA；Direct WDA 或显式 attach WDA 才使用指纹化 DerivedData、`xcodebuild`、`iproxy` 与恢复 supervisor。`devicerail-ios-host` 继续负责有界真机发现、`ios doctor`、auto/required、热插拔和原 UDID 固定，不保存 Apple 凭据，也不绕过首次信任、Developer Mode/restart、UI Automation 或开发者证书确认。验收覆盖共享 conformance、fake WDA/Appium transport、external/managed stock daemon E2E、managed 进程/配置/脱敏/auto-required/热插拔策略，以及一条历史 Direct WDA 真机链路；真实 Appium/XCUITest、多机型、多 iOS/Xcode/签名团队与长时间稳定性矩阵仍是外部发布验收。
- [x] **DR-031 HarmonyOS Driver**：新增 conformant `devicerail-harmony-hdc`，通过 typed HDC runner 完成 target discovery/state/error、PNG 截图与 bounded hierarchy observation，并提供 tap、swipe、inputText、keyPress、launch 五个闭合 Action、健康探测及 Evidence 持久化；不暴露通用 shell escape hatch。stock daemon 默认关闭 HDC，只有显式 `auto|required` 才执行一次启动发现；独立 5 秒 discovery 与 65 秒 runtime runner、稳定 target 排序、offline/unauthorized 路由保留以及 auto/required 失败策略已进入 daemon 回归。系统适配器仍要求宿主安装 HDC；当前验收除 deterministic fake/fixture 外，还覆盖 stock daemon→真实 `SystemHdcCommandRunner`→fake executable 的二进制 E2E，但不声称 DevEco、真实 HarmonyOS 设备或真正 HDC 安装环境 E2E。
- [x] **DR-032 macOS Driver**：`devicerail-desktop-driver` 提供 non-prompting Screen Recording/Accessibility 权限预检、系统 `screencapture` 截图和原生 Quartz 键盘/指针/滚轮输入，权限不足返回稳定显式错误，Observation 通过 Core Evidence Store；macOS Driver 已运行共享 conformance。stock daemon 以默认关闭的 `DEVICERAIL_DESKTOP=auto|off|required` 显式注册编译宿主唯一 native Desktop route，inventory 保持 lazy，首次 connect 才验证当前 TCC/桌面状态。验收覆盖 fake backend、daemon 配置/注册与真实二进制 inventory 路径，不声称已在真实 macOS 桌面、TCC 授权和显示器组合上完成 OS E2E。
- [x] **DR-033 Windows Driver**：同一 Desktop crate 提供 Windows virtual-desktop capture、Win32 键盘/指针/滚轮输入、显式 viewport/health 及 Evidence，Windows Driver 已运行共享 conformance。stock daemon 通过同一默认关闭的 Desktop 合同在 Windows build 中注册一条 lazy native route，并由 Linux/macOS/Windows CI matrix 编译与运行宿主适配的 inventory 回归。当前不声称真实 interactive Windows Session、Session 0 服务隔离、DPI/多显示器或输入 OS E2E。
- [x] **DR-034 Linux Driver**：Desktop crate 将 X11 与 Wayland 建模为不同 profile：X11 使用 `import`/`xdotool`，Wayland 使用 `grim` 配合完整 `ydotool` 或缩减为仅键盘/文本的 `wtype`；工具缺失、session 歧义、权限和 Wayland viewport 不匹配均显式失败，能力不会静默夸大，X11、Wayland/ydotool 与 Wayland/wtype 均进入共享 conformance。stock daemon 可显式选择 display server、input backend、工具路径和物理像素 viewport；真实 daemon 二进制覆盖配置→inventory 并以 fake host tools 验证有界 system runner。当前不声称真实 X server、Wayland compositor、`ydotoold`/`uinput` OS E2E。
- [x] **DR-035 RDP Driver**：新增 conformant `devicerail-rdp-remote` 与 checked-in bridge v2 Schema/Golden Fixtures；loopback-only adapter 以目标 fingerprint 派生稳定 DeviceId，对截图执行 bounded PNG 解码与 canonical 重编码，并提供 atomic pointer/key/text/scroll 及 protected `inputSecret`。`operationId`、原始 `callId`、断连取消、终态去重和一次同 ID 重试避免跨租约悬挂或盲目重复输入；daemon 只在显式 bridge/target/token 配置时注册路由。DeviceRail 不嵌入或启动 RDP stack/server/bridge；当前验收覆盖 fake bridge、真实 loopback framing 与 conformance，不声称真实远端桌面 E2E。
- [x] **DR-036 Device Pool、租约与健康检查**：Core 新增进程级 `DevicePool`，以 owner-bound lease、monotonic TTL/health freshness、逐设备 operation guard 和受限 access handle 阻止 raw handle 绕过、过期交接与 Driver I/O 的 TOCTOU；Registry 注册/移除与库存保持一致。daemon 在 Session 启动及 leased operation 前执行 bounded Driver health probe，Session 结束、连接断开和全局 shutdown 按 guard 顺序回收；可选 loopback TCP transport 让多个真实 socket client 共享同一 pool/lease authority，并覆盖争用与清理 E2E。此阶段只保证单 daemon 进程内的多客户端租约，不包含跨进程锁、远程鉴权或分布式租约，后两者分别留给 DR-043/044。
- [x] **DR-040 Python Client**：新增 Python 3.11+ typed async stdio client，从同一 174 个公开 Schema 根生成 353 个协议资源文件，其中包含 959 个可解析 forward reference 的 `TypedDict`，以及 union、`Literal`、overload、24-method map 和 packaged runtime Schema；transport 对双向 NDJSON、UTF-8、JSON-RPC envelope、ID、安全整数、Feature、pending/write 容量、取消与关闭均有闭合上限，模糊 partial write 会永久 poison connection。生成检查、strict mypy contract、单测、全部 Golden Fixture runtime validation，以及 wheel/sdist 隔离导入和 Schema 字节一致性检查均通过；Python 不维护第二套手写 DTO。
- [x] **DR-041 Driver Plugin ABI**：新增进程隔离、版本化的 plugin manifest/ABI v1 和 conformant `DeviceDriver` adapter。Unix daemon 只在显式 owner-only 目录配置时发现插件，以 no-follow/inode/owner/permission/revalidation 约束 manifest 与 executable，固定 argv、清空环境并保持一个 supervised child 承载完整 lifecycle；无法证明等价 ACL/no-follow/file-identity 的 non-Unix 平台在 discovery 与 transport pre-spawn revalidation 两层以 `plugin_permissions_unsupported` 明确 fail closed，不使用恒真 owner/permission fallback。ABI、协议范围、身份、capability、protection、Schema、frame/diagnostic/timeout 和 response kind 任一漂移均 fail closed，模糊 mutation 不重试。Unix 真实 fixture 子进程运行共享 conformance 和平台门控的契约/安全测试，non-Unix 单测锁定拒绝边界；Rust trait 和动态库 ABI 不成为 wire 边界。
- [x] **DR-042 签名二进制与安装包**：新增 Linux deterministic tar.gz、macOS/Windows deterministic ZIP portable installer，绑定 daemon/Bundle CLI 版本、target/header、闭合文件清单、SHA-256、SPDX 2.3 SBOM、DeviceRail-specific in-toto provenance、签名声明和安装配置。unsigned test artifact 明确不可认证；signed 模式要求 clean production Git source、先验 archive/native payload signature、out-of-band macOS/Windows/Linux identity 与完整 archive cosign signature，跨 OS 或缺少工具/身份时拒绝验证。archive parser 对 ZIP64、central directory、tar decompressed stream、entry、path、link、mode 和资源量 fail closed；27 项 `-W error` 安全测试通过，CI/release workflow Action 固定完整 SHA。真实证书签名、Apple notarization 和目标平台验证只在持有密钥的 release CI 中执行，不以本地 fixture 冒充。
- [x] **DR-043 远程鉴权、授权与审计**：可选 loopback TCP security gate 以一次性 HMAC-SHA256 challenge-response 认证 client，闭合 read/control/admin 权限表默认拒绝未知方法，并在 dispatch 前同步写入 owner-only、no-follow、逐条 fsync、SHA-256 链式的 canonical audit admission record；credential、proof、params、设备输出与 Secret 不进入日志或公开错误。deadline 覆盖完整认证和 audit，challenge/replay/expiry/attempt/frame/credential/audit 大小均有硬上限，10 项 crate 安全测试及真实 daemon loopback 鉴权/授权/持久审计 E2E 通过。它不提供 TLS 或 server identity；跨主机仍要求外部 SSH/mTLS，无法证明 owner-only ACL 的 Windows 配置明确 fail closed。
- [x] **DR-044 分布式设备路由与观测**：新增 opt-in `devicerail-distributed-router`，以严格 camelCase peer v2、稳定 `remote:<node>:<key>` namespace、epoch/revision inventory、health、owner-bound monotonic lease、connect/Session/observe/execute/evidence/cancel 和 fixed-cardinality telemetry 将远端设备实现为普通 conformant Driver。peer-v2 显式协商 UI Snapshot/semantic capability，并原样透传两个 operation Feature gate；peer-v1 fail closed，不做静默降级。`RegistryPeerService` 与 bounded `serve_peer_stream` 提供可嵌入服务端；stock daemon 另以显式 owner-only `DEVICERAIL_DISTRIBUTED_SERVER` 文档开放单一 numeric-loopback peer listener，与全局 Registry、Event Store 及 Evidence Store 共享资源，只导出本地 route 快照而排除 `remote:*`。本地 route 注册后、outbound discovery 前即绑定 listener 以消除两个 stock daemon 的启动互等；starting gate 期间 hello/inventory/health/capabilities 仍可完成发现，lease/mutation 则返回可重试 `node_starting`，outbound 注册完成后才 mark ready。bind 日志仅表示 socket 已保留而不是 ready 信号，后续启动或关闭失败会有界取消连接并收敛 lease、Session 和 Evidence。Evidence 分块读回本地 operation Store，execute 从不自动重试，同 call ID terminal replay 不重复 mutation，per-lease gate、取消安全的 Core lease 镜像、owner-scoped 回收与可重试 staged cleanup 阻止生命周期竞态和资源状态丢失。验收包含共享 conformance、内存 duplex 故障/竞态回归，以及两个真实 stock daemon 二进制进程通过真实 loopback TCP 完成 remote inventory、connect、observe/Evidence 与 EOF 清理。该本地 E2E 不代表真实 SSH/mTLS 或跨主机网络验收；raw loopback TCP、RPC HMAC 均不会自动鉴权 peer-v2。本阶段不是 consensus、公共 listener、多跳路由或内建 TLS，无法证明 owner-only 配置 ACL 的 non-Unix 平台明确 fail closed。
- [x] **DR-045 可选 YAML 兼容适配器**：新增依赖 typed client 与专用 `js-yaml` parser 的 `@devicerail/yaml-adapter`，把 bounded、JSON-shaped `devicerail/v1` YAML 编译为不可伪造、深度不可变的公共 RPC call plan。custom tag、alias/merge、duplicate/prototype key、unknown field/method、非有限数、资源越界和持久化 protected/unknown Action 全部 fail closed；Action protection 绑定 compile-time device，并在同一 execution continuation 中重新选择 route、核对 inventory/capability 后调用。typecheck/build、测试和 isolated package consumer 检查通过；YAML、workflow policy、重试、AI 和 Driver 生命周期不进入 Rust kernel 或 daemon。

## 下一执行队列

P4 的 DR-040～045 已完成，当前没有尚未实现的已编号条目。完成范围是代码、生成物、契约/安全测试、真实本机 loopback/duplex E2E、stock daemon 宿主 Desktop 注册/inventory、单台真实 iPhone 的 operator-managed WDA 自动化链路，以及双 daemon peer-v2 入站/出站路径、文档与本地总门禁；真实 Android/HarmonyOS/macOS/Windows/Linux/RDP 实验室环境、扩展 iOS 机型/版本矩阵、真实 Appium/XCUITest 与 Playwright server、真实 SSH/mTLS 跨主机网络，持有生产密钥的签名/notarization，以及 GitHub Actions Linux/macOS/Windows matrix 仍是独立的外部发布验收，不以 fake tool、本地 fixture 或单一 macOS 主机代替真实 OS/网络验收。

Protocol 1.5 的非编号 P1 集成已进入确定性完成态：Appium XCUITest process/W3C Session 管理、Safari/WebView DOM resolver、Native accessibility resolver、五个 canonical Action 的 DTO/Schema/output 等价校验、UI Tree Evidence 落盘前正文校验，以及 distributed peer-v2 的 operation-scoped UI/semantic Feature 透传与 round-trip E2E 均已实现。只有完成这些门禁的 Appium iOS Driver 才广告 semantic Action；Direct WDA 不广告，坐标始终是显式、可审计的兼容降级。尚未完成的是依赖外部环境的真实 Appium/XCUITest 多版本真机矩阵，它是发布验收，不是以 fake transport 代替的代码实现项。

## 完整功能列表

| 编号 | 优先级 | 功能 | 依赖 | 核心验收结果 |
|---|---|---|---|---|
| DR-001 | P0 | 协议握手与版本协商 | DR-000 | 客户端与 daemon 显式协商版本 |
| DR-002 | P0 | JSON Schema 生成 | DR-001 | 可重复生成公共协议 Schema |
| DR-003 | P0 | Golden fixtures | DR-002 | Rust/TS 共用稳定 JSON 基线 |
| DR-004 | P0 | Driver conformance suite | DR-003 | 所有 Driver 运行同一套契约测试 |
| DR-005 | P0 | Session 与事件序列 | DR-003 | 事件可稳定关联、排序和重放 |
| DR-006 | P0 | Evidence Store | DR-005 | 截图等资产哈希寻址、去重和校验 |
| DR-007 | P0 | Timeout、取消、优雅退出 | DR-005 | 无悬挂任务和半写入产物 |
| DR-008 | P1 | Driver Registry 与多设备路由 | DR-004、DR-007 | daemon 可注册、发现、选择多个 Driver |
| DR-009 | P1 | TypeScript 协议类型生成 | DR-002、DR-003 | TS 不手写重复 DTO |
| DR-010 | P1 | TypeScript stdio Client | DR-001、DR-009 | TS 可完成全部 Mock 端到端调用 |
| DR-011 | P1 | 通用 AI Tool Adapter | DR-010 | 任意 Agent 可发现并调用 ActionSpace |
| DR-012 | P1 | Android ADB 发现与生命周期支撑层 | DR-004、DR-008 | 通过系统 `adb` 枚举、连接和有限重连 |
| DR-013 | P1 | Android Observation | DR-006、DR-012 | 截图、viewport、方向和设备信息正确 |
| DR-014 | P1 | Android Driver 与基础动作 | DR-004、DR-013 | 注册实际 Driver，conformance 覆盖 tap、inputText、keyPress、swipe、scroll |
| DR-015 | P1 | Android App/System 动作 | DR-014 | launch、terminate、back、home、recent apps |
| DR-016 | P1 | 安全与脱敏策略 | DR-006、DR-014 | Secret 不进入 DeviceRail 持久产物/公开诊断/宿主 adb argv，protected action 不截图且默认隐藏 |
| DR-017 | P2 | Session Bundle 与可移植导出 | DR-005、DR-006 | 一个 canonical 目录包含协议、事件和证据 |
| DR-018 | P2 | Execution Recorder | DR-017 | 仅消费 TestEvent，不依赖具体 Driver |
| DR-019 | P2 | 离线 Visualizer | DR-017 | 展示 Timeline、截图、Action、Error、Verdict |
| DR-020 | P2 | WebSocket 实时事件传输 | DR-007、DR-017 | 支持背压、断线和续传 |
| DR-021 | P2 | Live Visualizer | DR-019、DR-020 | 可实时跟随 daemon 执行事件 |
| DR-022 | P2 | 独立报告导出 | DR-019 | Viewer 与数据打包但不注入 kernel |
| DR-023 | P2 | Playwright Remote Driver | DR-004、DR-009 | Web 通过同一协议提供 Observation/Action |
| DR-024 | P2 | 人工操作录制协议 | DR-017、DR-023 | 人工事件可转换为可回放 Action 流 |
| DR-025 | P2 | Screenshot/Video Stream | DR-006、DR-020 | 流帧以 evidence reference 进入事件系统 |
| DR-030 | P3 | iOS Driver | DR-004、DR-008 | WDA/WebDriver/MJPEG 通过 conformance suite |
| DR-031 | P3 | HarmonyOS Driver | DR-004、DR-008 | HDC Driver 通过 conformance suite |
| DR-032 | P3 | macOS Driver | DR-004、DR-008 | 截图、输入和权限检测完整 |
| DR-033 | P3 | Windows Driver | DR-004、DR-008 | 截图和输入动作完整 |
| DR-034 | P3 | Linux Driver | DR-004、DR-008 | X11/Wayland 能力明确区分 |
| DR-035 | P3 | RDP Driver | DR-004、DR-008 | 远程截图与输入符合统一协议 |
| DR-036 | P3 | Device Pool、租约与健康检查 | DR-008 | 多客户端安全共享设备资源 |
| DR-040 | P4 | Python Client | DR-002、DR-003 | 从协议生成并通过 golden fixtures |
| DR-041 | P4 | Driver Plugin ABI | DR-004、DR-008 | 插件发现和版本兼容检查 |
| DR-042 | P4 | 签名二进制与安装包 | P1 稳定 | macOS/Windows/Linux 可验证发行 |
| DR-043 | P4 | 远程鉴权、授权与审计 | DR-020 | 远程 daemon 具备安全边界 |
| DR-044 | P4 | 分布式设备路由与观测 | DR-036、DR-043 | 跨节点路由、指标和追踪 |
| DR-045 | P4 | 可选 YAML 兼容适配器 | DR-010 | 只编译为公共调用，不进入 kernel |

## 阶段完成标准

### P0 完成

- 协议可生成 Schema，并有稳定 golden fixtures。
- 任意 Driver 都能运行统一 conformance suite。
- 执行事件和 evidence 可以可靠保存、取消和关闭。
- Kernel 继续保持零 AI、零 YAML、零 UI 依赖。

### P1 完成

- TypeScript 和通用 AI Agent 可通过公共协议控制真实 Android 设备。
- Android Driver 不向上层泄漏 ADB 类型或命令细节。
- 每次执行都产生安全、可移植的 session/evidence。

### P2 完成

- Recorder 和 Visualizer 只依赖协议及 Session Bundle。
- Android、Mock、Web 产生的产物可由同一个 Visualizer 打开。
- Viewer 构建与 Rust kernel 完全解耦。

### P3 完成

- iOS、HarmonyOS、macOS、Windows、Linux 和 RDP 通过统一 Driver 协议与共享 conformance，不向 wire DTO 暴露平台库类型。
- 单 daemon 进程内的多客户端通过同一 Device Pool、健康状态、租约与 operation guard 安全共享设备。
- 平台 CI 验收是确定性合同验收，不等同于真实硬件、桌面会话、权限组合或远端 RDP 环境的实验室 E2E。

### P4 完成

- Python Client 与 TypeScript Client 从同一 checked-in wire Schema/Golden Fixture 单向生成，不把语言 SDK 类型引入 Rust。
- Plugin、remote auth、distributed router 和 YAML adapter 都是显式 opt-in 的上层适配器；默认依赖方向和零 AI/Prompt/YAML/UI kernel 边界保持不变。
- 安装包的来源、内容、SBOM、provenance、native identity 与 archive signature 可 fail-closed 验证；unsigned artifact 永不伪装成 signed release。
- 跨节点身份依赖独立 SSH/mTLS transport，stock peer listener 只绑定 numeric loopback，RPC HMAC 不隐式扩展到 peer-v2，分布式租约不声称 consensus；真实平台、证书和网络环境继续作为显式外部验收矩阵。

## Kernel 明确不做

- AI 模型或 Provider 集成。
- Prompt 模板或自主 Planning。
- YAML 执行引擎。
- 产品级 Recorder/Visualizer UI。
- 在协议 DTO 中暴露平台库类型。
