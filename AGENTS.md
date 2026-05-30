# AGENTS.md — Cameo

> 本文件为 Codex（及其它 AI agent）在本仓库工作时的核心指引。**新 session 请先完整
> 读它，再看 [`specs/`](./specs/)**。这里只放：当前现状 + 已锁定决策 + 工作纪律。
> 详细技术架构去 [`specs/ARCHITECTURE.md`](./specs/ARCHITECTURE.md)；视觉规范去
> [`specs/DESIGN.md`](./specs/DESIGN.md)。

---

## 项目：Cameo

> **An image-first canvas for your local Codex agent.**
> 你指着图说话，Codex 来生成和修改，结果在画布上铺开。

- **一句话**：Cameo 是一个 **image-first 原生桌面工具**，把本地 **Codex** agent 的生图 / 改图
  能力，装进一个"指着图说话"的空间画布里。**它是 Codex 的手和眼，不是它的脑子** —— 不做模型、
  不做内容服务、不在 agent 层做编排。
- **仓库**：https://github.com/hAcKlyc/cameo · 主分支 `main` · 协议 **AGPL-3.0**。
- **状态**：**v1 已实现并以 0.1.0 开源发布**。打包跨 macOS（Apple Silicon + Intel）+ Windows；
  设计系统浅色 + 红色（DESIGN.md v1.0.0）已全面落地；Codex sidecar 运行时 + 多会话 + 标注
  overlay + 血缘 + 限流面板 + Gallery + 自动更新 + 托盘都在线。输入框带生成档位选择器（模型 /
  智能 effort / 速度 service tier，每轮 `turn/start` 显式下发）；消息时间线由 Rust runtime 权威
  落盘；产品所有对外网络（cloud / gallery / 埋点）与 Codex 一致统一走 Settings 代理。Codex CLI
  用用户自己已认证或已配置 provider/API 的那份，Cameo 不打包、不卖 token。
- **运行**：`pnpm install && pnpm tauri dev`（需要已完成认证或 provider/API 配置的 Codex CLI）。完整下载 /
  打包步骤见 [`README.md`](./README.md)。

---

## Read first（按此顺序）

1. **本文件** —— 现状 + 决策 + 工作纪律。
2. [`specs/ARCHITECTURE.md`](./specs/ARCHITECTURE.md) —— **系统架构真相源**：模块边界、IPC、
   Codex 运行时、存储布局、构建 + 发布。日常迭代以这份为准。
3. [`specs/CODEX_PROTOCOL.md`](./specs/CODEX_PROTOCOL.md) —— **Codex 协议真相源**：app-server
   JSON-RPC 方法、生成档位参数语义（model/effort/serviceTier/summary/personality 的 sticky +
   null 清除）、model/list 字段（wire camelCase vs cache snake_case）、事件流、图像 I/O。动
   runtime / 加 Codex 参数前必读，别凭记忆。
4. [`specs/DESIGN.md`](./specs/DESIGN.md) —— **视觉真相源**：design token / 字体 / 组件状态
   矩阵 / 画布交互色。新组件请遵守。
5. [`README.md`](./README.md) —— 给开源仓库访问者的入口（quick start、平台、FAQ）。

> 本地 maintainer 文档（**已 gitignore，开源仓库里不存在**）：`specs/prd/`（早期 PRD、决策
> 日志）、`specs/research/`（调研笔记 + Codex runtime file:line 索引）。clone 出去的仓库读
> 不到这些，需要时靠对话补。
>
> **冲突优先级**：本 AGENTS.md > ARCHITECTURE.md（实现细节） > DESIGN.md（视觉）。三份之间
> 真有冲突要么 ARCHITECTURE / DESIGN 该更新了，要么本文档该更新了 —— 不要硬编码 fallback。
> Codex 协议层面的事实（方法名 / 参数语义 / 枚举）以 CODEX_PROTOCOL.md 为准，且最终真相是本机
> `codex app-server generate-json-schema` 生成的 schema —— 与之冲突说明文档过期，重生成核对。

---

## 核心范式（锁定，不再 re-litigate）

1. **不在 agent 层做事。** 生成 / 理解 / 意图澄清交给 Codex。
2. **两个语义域，各管各的**
   - **Agent（Codex session）** 管：对话/创作语义 + 连续性（**stateful，会反问**）。
   - **App（Cameo）** 管：制品 + 空间语义（文件身份、布局、文件夹同步、引用注入）。
3. **三个真相源**：Folder（制品，= agent cwd）/ Board doc（空间投影 + 标记 + 布局）/ Session
   （连续对话，Codex 持有；App 仅镜像 threadId + 消息时间线）。
4. **一切非破坏**：原图永不被改写；每次产出 = 新 Asset + 一条血缘。
5. **标记 = overlay-as-image**：矢量标记渲染成蒙层图，连同原图一起发给 agent（只要 agent
   能看图就行，runtime 无关）。代价：产出是**全新整图**，故必须非破坏 + 血缘。
6. **提供上下文，语义交给 agent —— 不做 workflow**。不用状态机去复刻"对话意图"。

---

## Vocabulary（代码 / UI / 文档 / commit 统一用）

| 术语 | 含义 |
|---|---|
| **Board** | 一块画布 / 工作区，与一个本地文件夹 1:1 |
| **Folder** | Board 背后的本地文件夹；制品真相源；= Codex 的 cwd |
| **Asset** | 不可变图片（blake3 内容寻址）。原图 / 产出 / 裁切产物都是 Asset |
| **Placement** | Asset 在画布上的实例（位置 / 缩放 / 旋转 / 裁切框）。改它不动 Asset |
| **Annotation** | 挂在 Placement 上的矢量标记层；dispatch 时渲染成 overlay 图 |
| **Reference（引用）** | 选中的图作为本轮上下文喂给 agent 的引用块 |
| **Op / Turn** | 一轮：用户消息（文字 + 引用图 + overlay）→ agent → 产出 |
| **Preset** | 一键指令模板（背后一个 prompt 或一个本地操作）|
| **Session** | 一个 Board 一条连续会话，Codex 持有 `threadId` |
| **Runtime** | agent 适配层（v1 = Codex；统一 `UnifiedEvent` 流，保持可换）|

避免在用户面前用："生成器" / "工作流" / "渲染"。

---

## 决策日志（已锁，请勿重开）

| 决策 | 锁定值 |
|---|---|
| 定位 | image-first 的 Codex 前端 |
| 目标用户 | **已有可用本机 Codex CLI 的用户**（ChatGPT 订阅或 Codex 支持的 provider/API 配置）；不卖 token、不收积分 |
| Agent 状态 | **stateful session（Codex app-server 持久进程）**，不是无状态函数 |
| 真相归属 | 三真相源（Folder / Board doc / Session）|
| 标记机制 | **overlay-as-image**（发图，不依赖结构化 mask API）|
| 非破坏 | 原图永不改写；产出皆新 Asset + 血缘；位置（左→右）即血缘 |
| 技术栈 | **Tauri 2 + React + PixiJS v8**（不是纯 Rust GPU）|
| Runtime | v1 仅 Codex，抽象保留可换（Gemini 后续）|
| 反问 | **允许并展示** agent 的 clarifying / approval |
| 对话粒度 | per-Board 一条连续 session（非 per-image chat）|
| 存储 | 注册表 / 全局名 → `~/.cameo/`，画布状态 + 血缘 → folder sidecar `.cameo/` |
| 引用 | v1 走文件路径，agent 自读，不挂传图 |
| 鉴权 | 复用 Codex CLI 自己的凭据 / provider 配置；Cameo **不接收、不保存 API key** |
| 云 | 编译期开关（`VITE_CAMEO_API_*`），开源 fork 默认无云 |
| 生成档位 | model/effort/serviceTier 每轮 `turn/start` 显式下发 + per-Board 持久化（不回落用户 config.toml）；summary=auto / personality=friendly 固定默认 |
| 时间线落盘 | **Rust runtime 权威写入** `.cameo/sessions/<id>.jsonl`（绑定 turn 的 session、不依赖前端聚焦），前端不再 best-effort append |
| 网络代理 | 产品**所有**对外出口（Codex sidecar + cloud + gallery 图片 + 更新）统一走 `net::client` / env，不只是 agent |
| 协议 | AGPL-3.0 |

### 有人（包括未来的 AI）提议以下，请先 push back

- "用结构化 mask API 替代 overlay" → 否，overlay-as-image 是 runtime 无关的关键。
- "让 agent 无状态、用文档/状态机重建上下文" → 否，旧式 workflow 思维（产品方已否）。
- "用 egui / Vello / 纯 Rust GPU 做画布" → 否，见 `specs/research/research_canvas_stack.md`。
- "我们也编排多模型 / 自己接图像模型" → 否，能力交给 Codex，价值在前端空间 UX。
- "做 per-image chat / 工作流编排" → 否，per-Board 连续 session + 提供上下文。
- "保证只改圈选区、其余像素不动" → 不承诺，生成式产出是全新整图。

确有理由重开某条 → 显式跟用户陈述理由，不要默默改。

---

## 工作纪律（写代码前请扫一眼）

### 视觉
- 对照 [`DESIGN.md`](./specs/DESIGN.md) §10 状态矩阵 + §15 token。**不写裸 hex**，一律走
  `--cm-*` 语义别名。新组件四态齐（默认 / hover / pressed / disabled），表单加 focus / 校验态。

### 架构
- 改动属于哪一段：Agent 语义 / App 语义？跨边界请先问。
- 新 Codex 事件：先加到 `UnifiedEvent`（`src-tauri/src/runtime.rs`）+ TS `CodexEvent`
  （`src/types.ts`），handler 走 `src/store/chat.ts::handleEvent`；wire form 用 serde
  camelCase。
- 画布：复用 `src/canvas/scene.ts` 现有原语；不要在 React 里直接画画布。
- 存储：动 `.cameo/` 布局或 `BoardDoc` shape → versioned change + 迁移函数。
- 子进程：清理走 `kill_tree`（unix `nix` / win `taskkill /T /F`）；PATH 用
  `std::env::join_paths`；打开文件管理器 / 揭示文件用 `tauri_plugin_opener`。
- 云：保持编译期短路，开源 fork **默认无云能力**。

### 跨端
- macOS（Apple Silicon + Intel）+ Windows。
- Tauri 不能交叉编译 —— Windows 改完必须在真 Windows 上验证（mac 上 blake3 的 win-only 代码
  `cargo check` 都过不了）。

### 日志
- 用 `tracing::info!(module = "…", …)`，不要 `println!` / `eprintln!`。
- 看日志：设置弹窗里「打开日志文件夹」，或 `tail -f ~/.cameo/logs/cameo.*.log`；要更细 →
  `RUST_LOG=cameo_lib=trace pnpm tauri dev`。

### 完成前自检
```bash
pnpm typecheck && pnpm lint    # 必须过
cargo check                     # Rust 改动时
pnpm tauri dev                  # 有 UI 改动时烟测
```

---

## 提交规范

- **Commit**：祈使句、首字母大写、无句号；有意义的改动写 body 说明 why
- **文档**：specs 用中文（技术名词保留英文），与现有文档一致。
- **schema / 存储**：任何动 `.cameo/` 布局或 Board doc 形状的改动都是 versioned change，加迁移函数。
- **协议**：AGPL-3.0 —— 任何外部依赖 / 引入代码请确认许可证兼容。

---

## When in doubt

1. 重读 [`specs/ARCHITECTURE.md`](./specs/ARCHITECTURE.md) + [`specs/DESIGN.md`](./specs/DESIGN.md)。
2. 找 maintainer 本地的 `specs/prd/` + `specs/research/`（不在仓库里，问 maintainer）。
3. 还不清楚 → 问用户。这些是 living docs，有实质 gap 应靠对话补，不要猜。
