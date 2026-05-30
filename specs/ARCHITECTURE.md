# Cameo 技术架构 — ARCHITECTURE.md

> **本文是 Cameo 的系统架构真相源**。后续迭代请先读这里再下手 —— 模块边界、IPC 形状、运行时
> 模型、存储布局都以这份为准。状态：**v1 已实现并随 0.1.0 开源发布**（AGPL-3.0）。
>
> **与其它文档的关系**
> - 项目根 [`CLAUDE.md`](../CLAUDE.md) —— 当前状态 + 已锁定决策 + "未来 AI 别再开这些问题"。
> - [`DESIGN.md`](./DESIGN.md) —— 视觉真相（token / 字体 / 组件外观 / 画布交互色）。
> - 本地过程文档 `prd/` / `research/` —— 不在开源仓库；早期 PRD、调研笔记。
>
> **更新**：2026-05-25

---

## 0. 你只看一段的话

Cameo 是一个**桌面 app**（Tauri 2 + React + PixiJS），把 OpenAI 的 **Codex CLI**（用户本机已登录的
那份）当作 stateful 的生图 / 改图 agent。Cameo 自己不做模型、不做编排、不卖 token —— 它只做
**"指着图说话"** 的空间画布：一个文件夹 = 一个 Board，画布上每张图是一个 Placement，AI 出的图
落在源图右侧 + 一条血缘线。Codex 跑作子进程，通过 JSON-RPC over stdio 跟它对话；流出来的事件经
一个 `UnifiedEvent` 适配层（runtime-agnostic）打到前端 store。所有图片永远是本地文件，App 唯一对
外的网络通讯是 Codex sidecar 自己访问模型 —— 跟用户平时用 CLI 时一模一样。

---

## 1. 架构脊柱（语义域 + 真相源）

### 1.1 两个语义域，各管各的

| 谁 | 管什么 |
|---|---|
| **Agent（Codex session）** | 对话/创作语义 + 连续性（stateful，会反问）|
| **App（Cameo）** | 制品 + 空间语义（文件身份、布局、文件夹同步、引用注入）|

**铁律**：App 不在 agent 层做事 —— 生成 / 理解 / 意图澄清都交给 Codex。不要用状态机或文档
去重建对话语义；连续性由 Codex session 持有。

### 1.2 三个真相源

1. **Folder（本地文件夹）** —— 制品（图片文件）的唯一真相，**也是 Codex 的 cwd**。
2. **Board doc**（`<folder>/.cameo/board.json`）—— 文件夹的空间投影：Placement / Annotation /
   布局。可重新构造，不持有制品。
3. **Session**（`<folder>/.cameo/sessions/`）—— 连续对话；Codex 持有 `threadId`，App 仅镜像它
   + 消息时间线 JSONL。

**衍生原则**
- **一切非破坏**：原图永不被改写；每次产出 = 新 Asset + 新 Placement + 一条血缘（`parentId` /
  `fromOpId`）。
- **位置即血缘**：产出落在源图右侧，让"空间"直接表达"派生关系"。
- **标记 = overlay-as-image**：用户在画布上的矢量标记（点 / 框 / 椭圆 / 涂抹）渲染成蒙层 PNG，
  与原图**两张一起**发给 Codex（不依赖结构化 mask API）。代价 = 产出是全新整图，所以非破坏 +
  血缘是必须的。

---

## 2. 技术栈（已定，不再 re-litigate）

| 层 | 选择 | 备选拒绝的原因（速记）|
|---|---|---|
| 桌面壳 | **Tauri 2** | 轻、Rust 后端、原生菜单/托盘/更新器都有，跨 mac/win 一份代码 |
| UI chrome | **React 19** + Zustand v5 | 团队熟、生态全；状态库选 Zustand 是因为不想要 Redux 模板 |
| 画布渲染 | **PixiJS v8**（WebGL2 基线，WebGPU 自动启用）| 不选 Vello（alpha + 大图弱）、egui（大图卡）、GPUI（单公司）|
| 图片协议 | 自定义 Cameo 图片协议 | 前端只通过 `src/lib/cameo-url.ts` 生成 URL；macOS/Linux/iOS 为 `cameo://localhost/<boardId>/…`，Windows/Android 为 Tauri/WebView 需要的 `http://cameo.localhost/<boardId>/…`；boardId 放 path + 路径规范化 + 防穿越 |
| Agent | **Codex `app-server`**（JSON-RPC over stdio）| 长驻进程 = stateful session，跟"每轮 respawn"完全不同 |
| 鉴权 | 复用 Codex CLI 凭据 / provider 配置 | Cameo 不接收、不保存 API key；能力来自用户本机可用的 Codex |
| 国际化 | 自研 `i18n/messages.ts`（key catalog）| 简单到不需要 i18next；en 是 source of truth |
| 后端日志 | `tracing` + `tracing-appender` | stderr + 文件 daily roll，前端 / Codex stderr 一起汇 |

### 2.1 "不要做"清单（来自决策日志）

- ❌ **结构化 mask API** 替代 overlay → overlay-as-image 是 runtime 无关的关键。
- ❌ **让 agent 无状态、用文档/状态机重建上下文** → 那是旧式 workflow 思维。
- ❌ **纯 Rust GPU 画布**（Vello / egui / GPUI）→ 见 `research/research_canvas_stack.md`。
- ❌ **自己接图像模型 / 编排多模型** → 能力交给 Codex，Cameo 的价值在空间 UX。
- ❌ **per-image chat / 工作流编排** → per-Board 一条连续 session + 提供上下文。
- ❌ **承诺只改圈选区** → 生成式产出是全新整图，无法承诺像素级保留。

确有理由重开任何一条 → 显式跟 user 陈述理由，不要默默改。

---

## 3. 进程拓扑

```
┌────────────────────────────────────────────────────────────────────┐
│  Cameo.app (Tauri host process)                                    │
│                                                                    │
│  ┌──────────────────────┐    ┌──────────────────────────────────┐  │
│  │  Rust backend        │    │  WKWebView / WebView2            │  │
│  │  (src-tauri/)        │    │  React + PixiJS                  │  │
│  │                      │◄──►│  (src/)                          │  │
│  │  • Tauri commands    │IPC │                                  │  │
│  │  • Board registry    │    │  • Stores (Zustand)              │  │
│  │  • Codex sidecar     │    │  • Canvas scene (PixiJS)         │  │
│  │    driver            │    │  • Chat / Composer / Gallery     │  │
│  │  • Cameo image proto │    │                                  │  │
│  │  • Logging / Config  │    │                                  │  │
│  │  • Updater / Tray    │    │                                  │  │
│  └──────────┬───────────┘    └──────────────────────────────────┘  │
│             │                                                      │
└─────────────┼──────────────────────────────────────────────────────┘
              │ spawn (one per Board)
              ▼
   ┌──────────────────────────────┐
   │  codex app-server             │
   │  (user's local Codex CLI)     │
   │                               │
   │  • Persistent JSON-RPC 2.0    │
   │    over stdio                 │
   │  • Holds threadId per Board   │
   │  • Reads ~/.codex auth        │
   │  • Reaches OpenAI on its own  │
   └───────────────────────────────┘
```

- **所有对外网络都从 Rust 出**：Codex sidecar（自带网络）+ 一切产品请求经 `net.rs` 的带代理
  `reqwest` 客户端（cloud telemetry / gallery JSON + 图片，见 §7；自动更新见 `updater.rs`）。
  WebView 不直接访问外部服务 —— 它只读本地 `cameo://` 图片协议、以及把远程图片经 `cmnet://`
  转回 Rust。这样 Settings 里的代理对全产品生效（WebView fetch/`<img>` 本身不认 app 代理）。
  另有一条轻量网络诊断 probe（Google `generate_204` 连通性检查；不带账号 / prompt / 图片内容）。
- **进程清理纪律**：unix 走 `nix` 进程组 SIGTERM→SIGKILL，win 走 `taskkill /T /F`（`codex.rs`
  里 `kill_tree` 两个 `#[cfg]` 版本）—— 关 app / 换 Board / interrupt turn 都不能留僵尸。

---

## 4. 后端模块（`src-tauri/src/`）

### 4.1 模块清单（按职责分组）

**Tauri 入口 + IPC**
| 文件 | 行数 | 职责 |
|---|---:|---|
| `main.rs` | 5 | 二进制入口，调 `cameo_lib::run()` |
| `lib.rs` | 181 | **Tauri Builder**：plugin（opener/dialog/notification/updater/process）+ 全局 state（BoardRegistry / CodexRegistry）+ ~54 个 command 注册 + 本地图片协议（`cameo://`）+ 代理远程图片协议（`cmnet://`）+ 窗口关闭→托盘逻辑 |
| `commands.rs` | 1393 | **IPC 桥**：workspace / board / asset / session / codex / 生成档位（gen settings / model 列表）/ cloud 传输 / config / device / updater / clipboard 的所有命令实现。前端的每个 `ipc.xxx` 在这里有 1:1 对应 |

**Board 和文档真相**
| 文件 | 行数 | 职责 |
|---|---:|---|
| `board.rs` | 339 | 内存中的 `BoardRegistry`，每个 Board 一个 entry（folder / doc / 名字）。Board id = blake3(folder)。所有 Placement / Annotation 突变汇集于此 |
| `model.rs` | 188 | 可序列化类型：`BoardDoc` / `Placement` / `Asset` / `Annotation` / `Shape` / `BoardMeta`（含 per-Board 生成档位 `gen_model/gen_effort/gen_service_tier`）/ `GenSettings` |
| `storage.rs` | 57 | 原子读写 `board.json` / `meta.json`（temp + rename），meta 解析失败会 warn（不静默默认化）|
| `workspace.rs` | 117 | 全局工作区注册表（`~/.cameo/boards.jsonl`）|
| `session.rs` | 176 | 每个 Board 多会话：`sessions.json`（索引 + threadId）+ `sessions/<id>.jsonl`（消息时间线）。**时间线由 runtime（codex.rs）权威写入**（见 §5）；append 失败会 warn |

**Codex 集成（详见 §5）**
| 文件 | 行数 | 职责 |
|---|---:|---|
| `codex.rs` | 2737 | **核心**：spawn `codex app-server` → JSON-RPC `initialize` / `thread/start` / `turn/start`（显式下发 model/effort/serviceTier/summary/personality）+ active-turn `turn/steer` → drain items → 转 `UnifiedEvent`。还负责 `model/list`、per-Board 生成档位、以及**消息时间线权威落盘**（user 记录在 turn 开始、assistant 在 turn 结束写入 active session）。协议细节见 [`CODEX_PROTOCOL.md`](./CODEX_PROTOCOL.md) |
| `process.rs` | 31 | 子进程小工具；Windows 下给 Codex / taskkill 等 console 子进程加 `CREATE_NO_WINDOW` |
| `runtime.rs` | 89 | runtime-agnostic 事件枚举：`UnifiedEvent`（SessionInit / TextDelta / Thinking* / Tool* / GenerationStarted / ImageGenerated / TurnComplete / RateLimits / …）|
| `prompt.rs` | 32 | 系统提示 + 引用图块格式 |

**资源 + 协议**
| 文件 | 行数 | 职责 |
|---|---:|---|
| `assets.rs` | 184 | Asset 内容寻址（blake3）+ 缩略图生成（image crate → JPEG）|
| `protocol.rs` | 248 | 两个 URI scheme：`cameo://`（本地 Board 图片，路由到磁盘 + 防穿越）和 `cmnet://`（**代理远程图片** —— 经 `net.rs` 拉取，SSRF 白名单只允许 public https）。均兼容 Windows WebView2 `http://<scheme>.localhost/...` 形态 |
| `paths.rs` | 100 | 文件系统布局：全局 `~/.cameo/` + 每 Board `.cameo/`，`CAMEO_HOME` 可覆盖（测试/便携）|

**系统服务**
| 文件 | 行数 | 职责 |
|---|---:|---|
| `config.rs` | 61 | `AppConfig`（proxy / close_to_tray / telemetry_opt_out / last_telemetry_date）+ `~/.cameo/config.json` 原子 IO |
| `proxy.rs` | 819 | `ProxySettings` + HTTP/SOCKS5 代理注入 —— 给 Codex sidecar spawn 时的 env 设 `HTTP(S)_PROXY` / `ALL_PROXY` / `NO_PROXY`；同时提供 Settings / AI 面板的轻量网络诊断 probe |
| `net.rs` | 59 | **产品所有对外网络出口的唯一带代理客户端**（cloud API / gallery 远程图片）。按 `ProxySettings` 构建一个缓存的 `reqwest::Client`、代理变更时重建。WebView 的 `fetch`/`<img>` 不认 app 代理，故一切需走代理的请求都经此（pit of success：新功能联网用它就自动继承代理）|
| `logging.rs` | 56 | `tracing` 双 sink：stderr + 日滚文件（`~/.cameo/logs/cameo.YYYY-MM-DD.log`，保留 14 天）。Rust / 前端 `front_log` / Codex stderr 全部带 `module=` 标签汇在一处 |
| `device.rs` | 94 | 匿名 device id（UUID v4 持久化到 `~/.cameo/device_id`），cloud telemetry 的锚 |
| `tray.rs` | 68 | 系统托盘图标 + 菜单。关窗口默认隐藏到托盘（设置可关）；macOS dock reopen 处理 |
| `updater.rs` | 422 | Tauri updater plugin：启动 60s 延迟探一次 `r.cameo.ink/update/`；签名校验由 plugin + `tauri.conf.json` 的 pubkey 做 |

### 4.2 关键不变量

- **Board 突变只走 BoardRegistry**：任何写 placement / asset 的 command 都通过 `board.rs` 的
  `with_doc_mut`-类入口；持锁期间不做 IO（IO 在锁外做，再回锁里 commit）。
- **跨平台 PATH**：`std::env::join_paths`（平台分隔符），**永不硬编码 `:`**。
- **打开文件管理器 / 揭示文件**：统一 `tauri_plugin_opener`，别再 shell out `open`。
- **win-only 代码改完必须在真 Windows 上验证** —— mac 上 `cargo check` 也过不了（blake3 build
  需要 `ml64.exe`）。

---

## 5. Codex 集成（运行时 + 事件流）

### 5.1 时序

```
launch
  └─ spawn `codex app-server`（PATH 已 augment：brew / cargo / npm/pnpm / nvm/fnm/asdf/mise/volta / scoop；GUI 启动兜底读 shell PATH）
  └─ JSON-RPC initialize（含 `initialized` 回握手）
  └─ thread/start  ──►  threadId（存进 SessionMeta）
                    ◄──  item/* stream（textDelta / thinking* / tool* / imageGeneration / …）

user 发第一条
  ├─ App 渲染选中的图为 clean PNG + overlay PNG（写到 `<folder>/.cameo/tmp/`）
  ├─ turn/start { content: [text, localImage(clean), localImage(overlay), …] }
  ├─ Codex 自己读图、推理、出图
  ├─ item.imageGeneration 来了：
  │    • 有 savedPath → 直接 import_generated_file
  │    • 否则 base64 → import_bytes
  │    → 新 Asset + 新 Placement（在源右侧）+ 一条血缘
  └─ turn/completed

active turn 中继续发
  ├─ UI 立即追加用户消息，不把“正在生成”当成本地错误
  ├─ 已拿到 active turn id → turn/steer { threadId, input, expectedTurnId }
  ├─ turn/start 已发出但 turn id 尚未返回 → 本地 pending steer 队列暂存
  └─ 收到 turn id 后按顺序 flush 到 Codex；是否接受由 Codex 的 active-turn steer 规则决定
```

### 5.2 事件模型 —— `UnifiedEvent`

事件经 `runtime.rs::UnifiedEvent` 抽象（runtime-agnostic），目前只有 Codex 一个适配。类型在
TS 侧镜像于 `src/types.ts::CodexEvent`（serde camelCase wire form）。

关键事件 + 处理边界：

| Event | 谁处理 | 怎么处理 |
|---|---|---|
| `sessionInit` | chat store | 记 `threadId` + `model` |
| `textDelta` / `thinkingDelta` | chat store | 追加到当前 assistant 消息的对应 block |
| `tool*` | chat store | 渲染工具调用行（detail 摘要 + 状态点）|
| `generationStarted` | **board store** + chat store | board 加占位（让用户看到「位置已锁定」）；chat 加 generating block，**key = `placeholderId`** |
| `imageGenerated` | **board store** + chat store | board apply 新 asset+placement；chat 用 `placeholderId` 匹配 generating block → promote 到 done（**`placement.id` ≠ `placeholderId`**，要用 placeholder 配对）|
| `turnComplete` / `sessionComplete` / fatal `error` | chat store + watchdog | turnStatus → idle；watchdog stop |
| `log` | chat store | 运行时诊断 note；**不**改变 turnStatus，不算 watchdog 进度 |
| `rateLimits` | chat store | primary（5h）+ secondary（weekly）两窗口都更新 |
| `permissionRequest` | Cameo 自动 accept | workspace-write 沙箱兜底（与 Riff 故意 deny 相反）|

### 5.3 健壮性三件套

1. **Inactivity watchdog**（`src/lib/watchdog.ts`）—— 长时间没有"进度类"事件就认定卡死，
   触发自动重启。`PROGRESS_EVENT_KINDS` 白名单避免被 log/rateLimits 这种"心跳"误骗。
   Codex 的 `"error"` notification 可能只是可恢复的 transport/runtime 诊断，适配层默认转成
   `log`；真正结束一轮只信 `turn/completed`、`turn/start` RPC 失败、fatal runtime error 或
   sidecar 退出。
   同一个 active turn 里的后续用户输入走 Codex `turn/steer`，不用本地屏蔽；只有 Codex 明确拒绝
   steering（例如 active turn 不可 steer）时才把该次发送标记失败。
2. **Stop button 升级**（`stopTurn` in `chat.ts`）—— 3s 软取消窗口失败后 escalate 到 tree-kill；
   `stopEpoch` 计数器防止 orphan timer 打到下一轮 turn。
3. **stderr 浮现**（`codex.rs::STDERR_SURFACE`）—— 已知失败行（401/403/网络/超时/限流/transport）
   会自动变成聊天里的红色 note，turn 失败不再是空白 error。

### 5.4 鉴权 + 网络

- Codex 使用用户本机已配置的凭据 / provider（ChatGPT 登录、API key 登录或自定义 provider）。
  **Cameo 完全不接收、不保存 API key / token**，只驱动 Codex CLI。
- Agent 状态面板会做本机 setup/auth 探测：先找 Codex CLI，再用 `codex app-server`
  的 `getAuthStatus` 检查是否需要 `codex login`。探测只返回 auth method / 是否需要登录，
  **不请求 token、不读取 token**；真正运行中的账号切换 / token 刷新仍由 Codex sidecar 自己处理。
- **代理覆盖产品所有对外网络出口**，不止 Codex：
  - **Codex sidecar** —— spawn 时注入 `HTTP(S)_PROXY`/`ALL_PROXY`/`NO_PROXY` env（`proxy.rs`）。保存设置后**自动重启当前 session**（`settings.restartNonce` → `App.tsx` 会话 effect）让新代理生效。
  - **cloud API（gallery + telemetry）** —— 前端 `cloudFetch` 不再用 WebView `fetch`，改走 `cloud_request` 命令 → `net.rs` 的带代理 `reqwest` 客户端（见 §7）。
  - **gallery 远程图片** —— `<img>` 经 `cmnet://` scheme → `net.rs` 代理拉取（`proxiedImg()` 包装）。
  - **自动更新** —— `updater.rs::build_updater_with_proxy` 把代理应用到 updater 的 reqwest builder。
  - 设计理由：WebView 自己的 `fetch`/`<img>` 只认系统代理、不认 app 内代理，所以一切需走代理的请求都下沉到 Rust 的 `net::client`（唯一出口，pit of success）。
- 代理开关开启且 host / port 有效时，Settings 会触发 `proxy.rs::probe_connectivity`：先连本地
  代理端口，再按所选协议访问 `http://connectivitycheck.gstatic.com/generate_204`，预期 HTTP 204。
  结果只用于设置面板的文字反馈，帮助用户发现端口、协议、认证或代理节点问题；不参与 agent 语义。
- AI 面板启动后会静默触发 `proxy.rs::probe_codex_connectivity`：代理开启时走代理，关闭时直连同一个
  Google 204 轻量检测。它只是一条网络 / 代理诊断 hint，不等价于 OpenAI/Codex 服务可达性；
  只有失败才在 Chat 底部露出“设置代理”入口。

详细的 file:line 索引见 `research/research_codex_runtime.md`（maintainer 本地）。

---

## 6. 前端（`src/`）

### 6.1 入口 + 顶层布局

- `main.tsx` —— React DOM root，挂到 `#root`。
- `App.tsx` —— 顶层 layout：TopBar（标题 / 工作区开关 / AI 面板开关 / 设置 / 更新提示）+
  ToolBar（mark 工具 / 导入 / 选择 / 撤销 / 重做）+ CameoCanvas（PixiJS）+ ChatPanel +
  SettingsModal。所有 store 的订阅都在 App / 各组件内做。

### 6.2 状态（Zustand 单例 store）

每个 store 是**单例**（不是 React Context）。这样 PixiJS 的 imperative 代码也能 `getState()` /
`subscribe()` 跟 React 共享真相。

| Store | 行 | 管什么 |
|---|---:|---|
| `store/board.ts` | 360 | Placement / Asset 的内存镜像；选中、移动、撤销、导入、剪贴板 |
| `store/chat.ts` | 681 | 消息时间线 + Codex 事件 handler + streaming phrases + watchdog；多会话切换；rate-limit |
| `store/ui.ts` | 72 | 画布 stats / 面板可见性 / mark 工具状态 / 选中 / 上下文菜单 |
| `store/composer.ts` | 39 | 输入草稿 + 引用 pill |
| `store/genSettings.ts` | 130 | per-Board 生成档位（模型 / 智能 effort / 速度 service tier）；`model/list` 缓存 + 切模型时 clamp effort/tier + 失效模型兜底；改动经 `ipc.setGenSettings` 持久化（Rust 落 meta.json 并推到 live session）|
| `store/history.ts` | 49 | undo / redo 时间线 |
| `store/settings.ts` | 122 | proxy / close-to-tray / telemetry opt-out / 更新 nonce |
| `store/workspace.ts` | 74 | 最近工作区 / 当前激活 / sidebar 可见 |

**纪律**：subscribe 用细粒度 selector（`useChatStore(s => s.messages)` 而不是 `useChatStore()`），
不然 PixiJS 每帧都会 trigger React render。

### 6.3 画布（`src/canvas/`）—— PixiJS scene

| 文件 | 行 | 职责 |
|---|---:|---|
| `CameoCanvas.tsx` | 233 | React mount + 桥接 board↔scene；SelectionBar / CropOverlay / CanvasContextMenu 子组件 |
| `scene.ts` | 1969 | **PixiJS 引擎**：sprite/text 渲染、选区描边、移动手柄、手势（pointer/touch）、crop 模式、标注（点/框/椭圆/涂抹）、纹理缓存、stats（FPS / vertices）、context menu dispatch |

**性能纪律**
- 视口剔除 + LOD / mipmap + TextureGC eviction + 大图不进 atlas + 解码离主线程。
- 画布 accent 用红色 `0xE53935`，选区描边 + 白 halo（DESIGN.md §3.6）。
- chrome 走 React，画布走 PixiJS —— Figma 分层模型。

### 6.4 组件（按区域）

- **聊天**：`ChatPanel.tsx` / `ChatInlineImage.tsx` / `Composer.tsx` / `GenSettingsMenu.tsx` /
  `StreamingStatus.tsx` —— 消息列表、流式指示器、composer 栏 + 生成档位选择器（模型 / 智能 /
  速度，对照官方 Codex app 的输入框菜单）；inline image 检测（markdown / link / plain path）+ 右键
  菜单（复制 / 引用 / 加到画布 / 在 Finder 显示）。
- **画布 chrome**：`SelectionBar.tsx` / `CropOverlay.tsx` / `CanvasContextMenu.tsx` —— 选中浮动条、
  crop 编辑 UI、右键菜单。
- **Gallery**：`gallery/GalleryButton.tsx` / `GalleryCard.tsx` / `GalleryDetail.tsx` /
  `GalleryOverlay.tsx` —— 云端 prompt 画廊（受 `CLOUD_ENABLED` 编译期开关，见 §7）。
- **UI 杂项**：`Sidebar.tsx` / `SettingsModal.tsx` / `CompareModal.tsx` / `UpdateIndicator.tsx`。

### 6.5 IPC + utilities（`src/lib/`）

| 文件 | 职责 |
|---|---|
| `ipc.ts` | Tauri `invoke` 的薄包装，1:1 映射到 `commands.rs` |
| `cameo-url.ts` | Cameo 图片 URL 单一出口；通过 Tauri `convertFileSrc` 推导平台协议 base，再按 `<boardId>/<rel-path>` 逐段编码 |
| `useCodexEvents.ts` | listen `codex-event` channel，按 `event.kind` 路由到 board / chat / 通知 |
| `useFileImport.ts` | drag-drop + 文件 picker，导入到当前 Board |
| `overlay.ts` | 把 canvas + 标注合成成 PNG（clean + overlay 两张）|
| `chatImageDetect.ts` | 在 AI 文本里识别图片引用（`![alt](p)` / `[name](p.png)` / plain path）|
| `watchdog.ts` | inactivity 看门狗（见 §5.3）|
| `streamingPhrases.ts` | "agent is working" 的画图主题文案池 |
| `notify.ts` | OS 通知（turn complete）|

---

## 7. 云集成（可选，编译期开关）

云能力**完全可选**：环境变量 `VITE_CAMEO_API_BASE` + `VITE_CAMEO_API_KEY` 没设的话整个 `src/services/cloud/`
模块短路，Gallery 入口都不渲染。开源 fork 默认编译时**无云能力** —— 自托管 / 商业部署可自填这两个变量。

`src/services/cloud/`
| 文件 | 职责 |
|---|---|
| `index.ts` | `cloudFetch` 包装（注入 `X-API-Key` / `X-Device-Id` / `X-App-Version` header + 错误码归一化：401 → unauthorized、403 → banned、429 → quota）。**传输经 `cloud_request` 命令 → Rust `net.rs` 带代理客户端**（不用 WebView `fetch`，以走 Settings 代理）。还导出 `proxiedImg()`：把远程 https 图片 URL 包成 `cmnet://`，让 `<img>` 也走代理 |
| `telemetry.ts` | 匿名 daily ping（一天一条 `app_open`），UTC 日期去重；用户可在设置里关闭 |
| `gallery.ts` | Prompt 画廊 API：`GET /api/v1/gallery/items`（列表+详情数据一次给全，点开不需要二跳）、`/facets`、`/random`、`/items/:id`（SEO / 直链）|

> 所有云请求（gallery JSON、telemetry）和 gallery 远程图片都经 Rust 带代理客户端出口（见 §5.4），
> 与 Codex sidecar 一致地遵守 Settings → 代理。

**Server 端**是一个独立的云服务（Cloudflare Workers + D1 + R2 + KV 实现），不在本仓库范围；开源自建版无需它。

**隐私边界**
- 图片永远是本地文件，**绝不上传**。只有 Codex sidecar 自己访问模型，跟用户平时用 CLI 一样。
- Telemetry 只发：随机 device id + 平台 + 版本号 + 启动事件。不发 prompt / 文件名 / 路径 / 任何
  文件内容。
- 用户可一键 reset device id（`device.rs::reset_device_id`）。

---

## 8. 存储布局

### 8.1 全局（`~/.cameo/`，由 `paths.rs::global_dir` 决定）

| 路径 | 内容 | 所有者 |
|---|---|---|
| `config.json` | `AppConfig`（proxy / close_to_tray / telemetry_opt_out / last_telemetry_date）| `config.rs` |
| `boards.jsonl` | 最近 Board 注册表（每行一条 JSON）| `workspace.rs` |
| `device_id` | 匿名 UUID v4（单行）| `device.rs` |
| `logs/cameo.YYYY-MM-DD.log` | 日滚日志，保留 14 天 | `logging.rs` |

### 8.2 每 Board sidecar（`<folder>/.cameo/`）

> 决策日志 OQ-2：**注册表 / 全局名 → 全局**，**画布状态 + 血缘 → folder 内 sidecar**。
> 这样 folder 可移动 / 同步（板子跟着图走），全局配置不污染用户的内容文件夹。

| 路径 | 内容 | 所有者 |
|---|---|---|
| `board.json` | `BoardDoc`：Placement[] / Annotation[] / 布局 | `board.rs` + `storage.rs` |
| `meta.json` | `BoardMeta`：threadId / 显示名 / active session / **per-Board 生成档位（gen_model/gen_effort/gen_service_tier，additive Option）** | `board.rs` + `storage.rs` |
| `sessions.json` | active session id + `SessionMeta[]` | `session.rs` |
| `sessions/<id>.jsonl` | 消息时间线（`ChatMessage` JSON）。**由 runtime（codex.rs）权威写入**，绑定 turn 的 session、不依赖前端聚焦（修复了旧版前端 best-effort 落盘可能静默丢历史的问题）| `codex.rs` 写 / `session.rs` IO |
| `thumbs/` | blake3-named 缩略图缓存 | `assets.rs` |
| `tmp/` | dispatch 临时图（clean + overlay）—— 放在 workspace 内是为了让 Codex sandbox 能读 | `codex.rs` |

### 8.3 文件命名（Asset on disk）

- **imported**：保留原文件名（用户已经按习惯命名了）。
- **generated / crop / paste**：`<kind>-<timestamp>.png`（如 `generated-20260524-221736.png`）。
- 加 hash 后缀（blake3 前 8 位）做 collision dedup。

### 8.4 schema 迁移

任何动 `.cameo/` 布局或 `BoardDoc` 形状的改动都是 **versioned change**：
- 涨 `BoardDoc.version`；
- 加迁移函数（`storage.rs::load_board_doc`）；
- 老版本读得出来，新版本读得出来。

---

## 9. 构建 + 发布

### 9.1 跨端目标

**Windows + macOS Apple Silicon + macOS Intel**。Cameo 不打包任何 sidecar / runtime（codex
用用户自己装、已登录的那份，运行时 PATH 上找），所以打包很轻 —— 无 Node runtime、无 per-arch 原生
模块、无二进制下载。

### 9.2 脚本（仓库根）

`.sh` = mac / `.ps1` = win，同名不同后缀对应平台。

| 脚本 | 用途 |
|---|---|
| `setup.{sh,ps1}` | 一次性：查工具链 + 加 `rustup target` + 装依赖 |
| `build_dev.{sh,ps1}` | unsigned 调试包：mac 出 `Cameo.app`，win 出 `cameo.exe` |
| `build_release.{sh,ps1}` | mac 默认出 **universal** `.dmg`（arm64 + Intel 一份），可 `--arm` / `--intel` / `--both`；win 出 NSIS 安装器 |
| `publish_release.{sh,ps1}` | R2 发布（updater payload/manifest + 官网 `latest*.json` + 安装包）并把安装包镜像到 GitHub Release |

### 9.3 签名

可选，从 `.env` 读（见 `.env.example`）。
- **macOS**：`APPLE_SIGNING_IDENTITY` + 公证凭据齐了 → Tauri 自动签 + 公证；缺省走 ad-hoc
  （本机能跑，他机被 Gatekeeper 拦）。
- **Windows**：走 `tauri.conf.json` 的 `bundle.windows.certificateThumbprint`，缺省不签。
- **Updater 签名**：`TAURI_SIGNING_PRIVATE_KEY` ed25519，pubkey 嵌在 `tauri.conf.json` 的
  updater 段，下发的 `darwin-*` / `windows-*` updater manifest 带签名，installer 端校验。
- **下载源分工**：R2 `https://r.cameo.ink` 是官网 `latest*.json`、官网安装包下载和 app updater 的
  canonical source；GitHub Release 只保留一份安装包镜像给开源访问者手动下载。
- **发布后验证**：`publish_release.*` 上传 R2 后会可选使用 `CF_ZONE_ID` / `CF_API_TOKEN` 清 CDN
  缓存，并对刚上传的 `r.cameo.ink` URL 做 HEAD 验证。

### 9.4 跨端不能交叉

Tauri 不能 mac↔win 交叉编译 —— Windows 的 `build_release.ps1` 必须在 Windows 上跑（需 MSVC +
WebView2；脚本用 vswhere 探测并 init vcvars64）。

---

## 10. 给新 contributor 的核对清单

迭代 / 加功能前请确认：

- [ ] 读过 [`CLAUDE.md`](../CLAUDE.md) 的「核心范式」+「决策日志」—— 别 re-litigate 已锁的决定。
- [ ] 知道你的改动属于哪一段：Agent 语义 / App 语义？跨边界请先问。
- [ ] 涉及视觉：对照 [`DESIGN.md`](./DESIGN.md) §10 状态矩阵 + §15 token，**不写裸 hex**。
- [ ] 涉及 Codex 事件：新事件先加到 `UnifiedEvent`（`runtime.rs`）+ TS `CodexEvent`
      （`types.ts`），handler 走 `chat.ts::handleEvent`；前后端 wire form 用 serde camelCase。
- [ ] 涉及画布：复用 `scene.ts` 的现有原语；不要在 React 里直接画画布。
- [ ] 涉及存储：动 `.cameo/` 布局或 `BoardDoc` shape → versioned change + 迁移函数。
- [ ] 涉及子进程：进程清理走 `kill_tree`（unix `nix` / win `taskkill /T /F`）；PATH 用
      `std::env::join_paths`；打开文件管理器用 `tauri_plugin_opener`。
- [ ] 涉及云：保持**编译期开关**短路（`CLOUD_ENABLED`），开源 fork 默认无云。
- [ ] 涉及日志：用 `tracing::info!(module = "…", …)`，不要 println / eprintln。
- [ ] 改完跑：`pnpm typecheck && pnpm lint && cargo check`，Rust 改动加 `cargo build`，前端涉
      界面的烟测一下 `pnpm tauri dev`。

---

## 附录 A · 词汇表

| 术语 | 含义 |
|---|---|
| **Board** | 一块画布 / 工作区，与一个本地文件夹 1:1 |
| **Folder** | Board 背后的本地文件夹；制品真相源；= Codex cwd |
| **Asset** | 不可变图片（blake3 内容寻址）。原图 / 产出 / 裁切产物都是 Asset |
| **Placement** | Asset 在画布上的实例（位置 / 缩放 / 旋转 / 裁切框）。改它不动 Asset |
| **Annotation** | 挂在 Placement 上的矢量标记层；dispatch 时渲染成 overlay 图 |
| **Reference（引用）** | 选中的图作为本轮上下文喂给 agent 的引用块 |
| **Op / Turn** | 一轮：用户消息（文字 + 引用图 + overlay）→ agent → 产出 |
| **Preset** | 一键指令模板（背后一个 prompt 或一个本地操作）|
| **Session** | 一个 Board 一条连续会话，Codex 持有 `threadId` |
| **Runtime** | agent 适配层（v1 = Codex；统一 `UnifiedEvent` 流，保持可换）|

UI / 代码 / 文档 / commit message 一律用这套词，**不要**说"生成器" / "工作流" / "渲染"。

---

## 附录 B · 已锁定的决策（速查）

| 决策 | 锁定值 |
|---|---|
| 定位 | image-first 的 Codex 前端 |
| 目标用户 | 已有可用本机 Codex CLI 的用户（ChatGPT 订阅或 Codex 支持的 provider/API 配置） |
| Agent 状态 | stateful session（Codex app-server 持久进程）|
| 真相归属 | 三真相源（Folder / Board doc / Session）|
| 标记机制 | overlay-as-image（不依赖结构化 mask API）|
| 非破坏 | 原图永不改写；产出皆新 Asset + 血缘 |
| 技术栈 | Tauri 2 + React + PixiJS v8 |
| Runtime | v1 仅 Codex，抽象保留可换 |
| 反问 | 允许并展示 Codex 的 clarifying / approval |
| 对话粒度 | per-Board 一条连续 session |
| 存储 | 注册表 / 全局名走 `~/.cameo/`，画布 + 血缘走 folder sidecar |
| 鉴权 | 复用 Codex CLI 凭据 / provider 配置；Cameo 不接收、不保存 API key |
| 引用 | v1 走文件路径，agent 自读，不挂传图 |
| 云 | 编译期开关，开源 fork 默认无云 |
| 协议 | AGPL-3.0（仓库：https://github.com/hAcKlyc/cameo）|

确有理由重开任何一条 → push back，与 user 显式对齐，再动手。
