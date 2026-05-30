# Cameo — 技术方案（草案 v0.0.1）

> **状态：地基已定，可进入实现规划。** PRD（`prd_0.0.1.md`）锁定需求；本文经两个
> 研究 spike 后，**技术栈（§2）与 Codex runtime（§8）已确定**，依据分别见
> `research_canvas_stack.md` / `research_codex_runtime.md`。剩余「待定」项见 §12，
> 其中 **SP-3（画布性能 spike）是开工前唯一的硬性前置验证**。

---

## 1. 架构脊柱

整个产品建立在两个原则上（见 PRD §4）：

**两个语义域：**

| 谁 | 管什么 |
|---|---|
| **Agent（Codex session）** | 对话/创作语义 + 连续性（stateful，会反问） |
| **App（Cameo）** | 制品 + 空间语义（文件身份、布局、文件夹同步、引用注入） |

**三个真相源：**

1. **Folder（本地文件夹）** — 制品（图片文件）唯一真相，= agent 的 cwd
2. **Board doc** — 文件夹的空间投影（Placement / Annotation / 布局）
3. **Session** — 连续对话，Codex 持有；App 仅存 `session_id` + 消息时间线

**关键判断（已与产品方确认）**：
- **不要**用状态机/文档去重建对话语义；让 Codex session 持有连续性。
- **引用每轮重新喂图** → session 不必记住所有老图，长会话压力被缓解。
- App 唯一拥有的语义是「制品归属 + 空间布局」：agent 写出一张新图，App 负责把它落到「源的右侧」、归入血缘、写进文件夹。

---

## 2. 技术栈（**已定**，依据见 `research_canvas_stack.md`）

**Tauri 2（Rust 壳）+ React chrome + PixiJS v8 画布。** WebGL2 为基线，WebGPU
在支持的平台自动启用。这是 **Figma 的分层模型**：画布交给专门的 GPU 合成器
（PixiJS），周边 chrome（chat / 设置 / 侧栏）走普通 web UI。沿用 Riff 已有的
Tauri 2 + React 19 + 自定义协议 + CLI sidecar 纪律。

为什么不是别的：
- **纯 Rust GPU UI（Vello/egui/GPUI…）被否**：Vello 仍 alpha 且"图片是它最大短板"
  （无界数量图片正是 Cameo 的工作负载）；egui 大图有秒级卡顿；GPUI 基本是单公司
  框架。会被迫同时手搓"图片合成画布"和"富文本 chat UI"，双线踩雷。
- **画布引擎选 PixiJS v8**（WebGL/WebGPU-first，多图场景性能领先），不是
  Konva/Fabric（Canvas2D，几十个形状可以，几百张 2048px 扛不住）或 tldraw/
  Excalidraw（白板 SDK，借其 culling/LOD 思路但不用其渲染器）。

**关键风险 + 必做 spike**：macOS 的 `WKWebView` 历史上**不暴露 `navigator.gpu`**
（即使 Safari 支持），所以**不能假设 WebView 内有 WebGPU**——以 WebGL2 为准。
**承诺前先跑一个一天的 spike**：在**打包后的 Tauri WKWebView**（不是 Safari）里
渲染 **200 张 2048px 纹理 + 平移缩放**，量 FPS / VRAM。过了 → 主路径安全；
不过 → 走 hybrid 兜底（Tauri chrome + 原生 wgpu 画布表面）。

内存策略（几百张大图）：视口剔除（空间索引）+ mipmap/LOD 按缩放 + 显式 eviction
（PixiJS `TextureGCSystem`）+ 大图不进 atlas + 解码放 Rust/Worker 不阻塞主线程。
图片经自定义协议 `cameo://`（沿用 `riff://` 的路径规范化/防穿越）喂给画布，
**Rust 负责解码 / 降采样 / mipmap**。

---

## 3. 数据模型（草案）

### 3.1 Asset（不可变内容）
```ts
type Asset = {
  id: string;            // = content hash (blake3), 内容寻址
  path: string;          // 文件夹内相对路径
  width: number; height: number;
  mime: string;
  createdAt: number;
  origin: 'imported' | 'generated' | 'crop' | 'paste';  // 来源分类（驱动命名 + 未来过滤）
};
```
原图、AI 产出、本地裁切产物都是 Asset。**原图永不被改写**；任何操作铸造新 Asset。

**命名协议（已实现）**：app 铸造的文件用 `<origin>-<YYYYMMDD-HHMMSS>.<ext>`（如 `gen-20260524-141502.png` / `crop-…` / `paste-…`，同秒撞名补 `-N`）——人读、可时序排序、stem 记来源。**用户导入的原图保留原文件名**。关键：Codex `imageGeneration` 的 `savedPath` 产出**重命名**为 `gen-*`，不再沿用 Codex 自己的 `ig_<hash>` 名（见 `assets.rs::import_generated_file`）。`origin` 让"原图 vs 产出"分类进了状态 JSON，**无需 role 子目录**（保住 folder = agent cwd）。

### 3.2 Placement（呈现态，可变、非破坏）
```ts
type Placement = {
  id: string;
  assetId: string;
  x: number; y: number;  // 画布坐标
  scale: number; rotation: number;
  cropFrame?: Rect;      // 非破坏裁切框
  z: number;
  parentId?: string;     // 血缘：派生自哪个 Placement（驱动「平铺右侧」布局）
  fromOpId?: string;     // 由哪个 Op 产生
};
```

### 3.3 Annotation（矢量标记层）
```ts
type Annotation = {
  placementId: string;
  shapes: Shape[];       // rect | ellipse | path(brush) | arrow | text
};
```
dispatch 时渲染成 overlay 图（见 §5）。

### 3.4 Op / Turn（一轮，append-only）
```ts
type Op = {
  id: string;
  sourcePlacementIds: string[];     // 引用了哪些图
  instruction: { presetId?: string; text?: string };
  renderedInputs: { clean: AssetRef[]; overlay?: AssetRef[]; mask?: AssetRef[] };
  outputs: string[];                // 产出 assetId[]
  status: 'running' | 'done' | 'stopped' | 'error';
  startedAt: number; durationMs?: number;
};
```

### 3.5 Session（agent 持有，App 镜像）
```ts
type Session = {
  threadId: string;      // Codex 原生 threadId（thread/start 返回；用于 thread/resume）
  runtime: 'CodexCLI';   // 留可替换
  messages: SessionMessage[];   // UI 渲染用时间线，崩溃恢复回放
};
```

> **OQ-2（已解决，2026-05-24）= 混合**：画布状态（Board doc + 标记 + **血缘** + session）放**文件夹内 `.cameo/` sidecar**——自包含、可移植（拷贝/同步/换机不丢，类比 `.git`），且"位置即血缘"是关于文件的、留 folder 才语义完整。**身份/注册**（boardId、名字、路径、最近列表、app 侧偏好）放**全局 `~/.cameo/workspaces.json`**，按 `boardId` keyed；folder 的 `meta.json.boardId` 当自报标记，移动/改名靠它重绑（不断指针）。**项目资源（未来：风格 prompt 等）**放 folder 内。

---

## 4. 存储布局（已实现，OQ-2 = 混合）

**两层 `.cameo`，别混**（见 `src-tauri/src/paths.rs`）：

```
~/.cameo/                      ← 全局：跨 Board 的 app 状态
├── workspaces.json            ← Board 注册表：[{ boardId, path, name, kind, lastOpened }]，按 boardId keyed
├── config.json                ← app 设置（网络代理）
├── boards.jsonl               ← 最近 Board 索引
└── logs/                      ← 统一日志

<user-folder>/                 ← Board 1:1 对应；agent 的 cwd
├── ChatGPT Image ….png        ← 导入原图：保留原文件名（origin=imported）
├── gen-20260524-141502.png    ← AI 产出（origin=generated；不再用 Codex 的 ig_<hash> 名）
├── crop-20260524-141530.png   ← 本地裁切（origin=crop）
├── paste-….png                ← 粘贴（origin=paste）
└── .cameo/                    ← Board sidecar（agent 看不到 / 不该改）
    ├── board.json             ← Asset / Placement / Annotation / 布局 / 血缘（schema v2）
    ├── meta.json              ← boardId / name / runtime / activeSessionId
    ├── sessions.json          ← 会话索引（threadId 等）
    ├── sessions/<id>.jsonl    ← 每条会话的消息时间线
    ├── thumbs/                ← 缩略图缓存
    └── tmp/                   ← dispatch 临时图
```

借鉴 Riff 的隔离原则：agent cwd = 文件夹本身（读输入图、写输出图），`.cameo/`
sidecar 放 App 内部状态，agent 不该碰（点号前缀 + 系统提示约束）。**画布状态留 folder
→ 可移植**；身份/名字/最近列表归全局注册表（按 boardId 重绑移动的文件夹）。dispatch
overlay 以 `.overlay-*.png` dotfile 写在 root（agent 能读、Finder 隐身、scan 跳过），
开 Board 时清理残留（`assets::sweep_overlays`）。

---

## 5. 标记 → 请求 数据流

```
选中 Placement(s)
  → 其 Annotation(矢量) 渲染成 overlay：
       clean   = 原图
       overlay = 原图上叠半透明标记的合成图（给 vision agent 读「我圈的这块」）
       mask?   = 二值蒙版（留给未来支持结构化 mask 的 runtime）
  → 进入 Composer 作为引用块（§6 引用状态机）
  → instruction = resolve(presetId) + freeText
  → 组装 Op → CodexRuntime.dispatch()
  → agent 在文件夹写出新图 → watcher 发现 → App 按本轮 source 归属 → 落「源右侧」+ 连血缘
```

**关键**：overlay-as-image 让 Cameo 在能力层面 **runtime 无关**——只要 agent 能看图。
代价：产出是**全新整图**（非 patch）→ 一切非破坏 + 血缘。

> **已定（原 OQ-3）**：发 **clean + overlay 两张独立 `localImage` 路径**（Codex 收文件
> 路径不收 base64，确认支持多图输入，见 §8）。`mask` 字段保留给未来支持结构化 mask 的
> runtime，v1 不发。

---

## 6. 引用状态机（画布 ↔ 会话的桥）

```
        hover/选中元素
            │
            ▼
   [GHOST 虚像]  ──选别的──▶ 切换到新图的虚像
            │
       点进输入框
            │
            ▼
   [COMMITTED 实像 tag]  融进文本；再选别的 → 起新 GHOST
```
- 支持单选 / 多选；每个 committed 引用携带 (clean + overlay)
- 发送时所有 committed 引用的图随本轮一起喂给 agent

---

## 7. 候选落点 / 布局

- 产出 Placement 的 `parentId = 源 Placement`；布局算法据此放到**源右侧**
- 批量 N 张 → 源右侧一列**纵向堆叠**
- 位置即血缘：左→右 = 旧→新派生，**无需画连线**

> **开放问题 OQ-4**：批量是否先进「结果托盘」再由用户拖上画布（防废稿淹没），还是直接平铺？

---

## 8. Codex Runtime 适配（**方案已定**，完整版见 `research_codex_runtime.md`）

经调研一套现有的 TS Codex 集成 + 官方文档，**SP-1 的假设全部成立**，方案直接确定：

**驱动方式 = `codex app-server`**：一个长驻的 **JSON-RPC 2.0 over stdio** 进程，
**整个会话保持存活**（不是 Claude Code 的 `claude -p` 逐轮 respawn）。这天然就是
Cameo 想要的"**stateful session，一个 Board 一个**"——比 Riff 的 CC 集成更顺。

协议要点（newline-delimited JSON-RPC）：
- 握手 `initialize`（+ 补发 `initialized` 通知，比该 TS 实现更 spec-correct）
- `thread/start`（新会话，返回 `threadId`）/ `thread/resume`（带 `threadId` 续）
- `turn/start`（发一条用户消息）/ `turn/interrupt`（取消当前 turn）
- 流式回流为 notification：`item/agentMessage/delta`（文本）、`item/reasoning/*Delta`
  （思考）、`item/started`/`item/completed`（工具/产出项）、`turn/completed`（turn 结束）
- **核心是 `Item` 抽象**：一个 turn 产出一串带类型的 item；`imageGeneration` item =
  生图产出
- 未知 item 类型只 warn 不崩（Codex 版本漂移的前向兼容）

**图像 I/O（Cameo 的命脉，已确认双向支持）**：
- **输入 = 文件路径**（不是 base64）：把 clean 原图 + 标记 overlay 写到临时文件，
  `turn/start` 的 `input` = `[{type:'localImage',path:clean},{type:'localImage',path:overlay},{type:'text',text}]`
- **输出 = `imageGeneration` item**：完成项里 `item.savedPath`（Codex v0.117+ 自动落盘，
  零拷贝）**或** `item.result`（base64）——**两者都要处理**；`item.revisedPrompt` 当 caption

**会话续命 / 恢复**：`threadId` 是续聊令牌（Codex 不收客户端 session id，自己存到
`.cameo/meta.json`）。`thread/resume` 失败且匹配 `no rollout found|thread not found`
→ 清掉 threadId、用 `thread/start` 重试一次，别丢用户消息。

**权限**：`thread/start` 带 `{approvalPolicy, sandbox}`；Cameo 默认 **workspace-write +
非阻塞 approval**（编辑只在 Board 文件夹内）。Codex 会以 **server→client request** 形式
发审批/澄清请求 → Cameo **接住并展示**（PRD §7.7 要 agent 能反问），用 `respond(id,…)` 回。

**鉴权**：复用 Codex CLI 自己的凭据 / provider 配置（ChatGPT 登录、API key 登录或自定义
provider）。Cameo 不接收、不保存 API key。`getAuthStatus` 探测：
`requiresLogin = !authMethod && requiresOpenaiAuth===true`；只在这个条件成立时引导用户完成
Codex 认证 / 配置。

**进程纪律**（沿用 Riff `agent.rs`）：绝对路径预解析（每平台都要）；POSIX `detached`
便于 tree-kill；取消 = `turn/interrupt` → 关 stdin → SIGTERM 组 3s → SIGKILL 2s；
始终 drain stderr。

**Rust 端**：把一套 TS `codex.ts` 的 JSON-RPC stdio client + `initialize`→`thread/start`
→`turn/start` 循环 + `buildCodexInput`(localImage) + `imageGeneration` handler **从 TS
端口到 Rust trait**（`ImageAgentRuntime`，产出统一 `UnifiedEvent` 流，保持 runtime 可换）。

---

## 9. 取消 / 错误 / watchdog

沿用 Riff 纪律：`process_group(0)` 树杀、SIGTERM 5s → SIGKILL 2s、drain-before-wait、
strip ANSI、统一日志。被取消的 turn 已写文件保留（非破坏）。错误兜底 UI 见 PRD §7.9。

---

## 10. 网络 / 代理

复用 Riff `proxy.rs`：给 Codex 子进程注入 HTTP/SOCKS5 代理（墙内访问 api.openai.com），
fail-safe 剥离 `ALL_PROXY`，默认注入 `NO_PROXY` 保护 localhost。改个默认 endpoint 即可。

---

## 11. 与 Riff 复用 / 不复用清单

| 复用（≈直接搬） | 不复用（Cameo 自有） |
|---|---|
| CLI 探测 + 绝对路径解析 | Agent 协议层（Codex = app-server JSON-RPC，从一套 TS `codex.ts` 端口，非 Riff 的 NDJSON）|
| 子进程纪律（树杀 / drain / stdin）| 沙盒/隔离层（图片是惰性字节，无需 `riff://`/iframe/CSP）|
| 前端流式 store / chat 面板 / composer | source-of-truth 哲学（图无独立"源"、非确定生成）；session 模型（Codex 持久 app-server vs CC 逐轮 spawn）|
| 流式前端（RAF 批量 / flushSync / 单例 store）| 呈现层（画布 vs iframe）|
| proxy 注入 | 文本锚定（`data-riff-rl`）→ 换成**画布区域标记**|
| 取消 / watchdog / 统一日志 | 画布渲染引擎（全新）|
| Tauri 壳 / 冷启动 / design token / 性能预算 | 引用机制（虚像→tag）|

> **注意一个相反配置**：Riff v0.0.2 故意 deny `AskUserQuestion`；Cameo **要允许并展示**
> agent 反问。

---

## 12. 开放问题汇总（需深聊）

**已解决：**

| # | 结论 |
|---|---|
| ~~SP-1~~ | ✅ Codex `app-server` 支持多图输入（localImage）+ imageGeneration 输出 + 持久 session + 反问，方案见 §8 / `research_codex_runtime.md` |
| ~~OQ-1~~ | ✅ Tauri 2 + React + PixiJS v8，见 §2 / `research_canvas_stack.md` |
| ~~OQ-3~~ | ✅ 发 clean + overlay 两张 `localImage` 路径（Codex 收文件路径不收 base64）|
| ~~SP-2(鉴权)~~ | ✅ 复用本机 Codex CLI 凭据 / provider 配置；Cameo 不接收、不保存 API key |
| ~~OQ-2~~ | ✅ 混合存储（2026-05-24）：画布状态 + 血缘 → folder `.cameo/` sidecar（可移植）；身份/名字/注册表 → 全局 `~/.cameo/workspaces.json`（按 boardId 重绑）；命名 `<origin>-<时间戳>` + `Asset.origin`。见 §3.1 / §4 |

**仍待办：**

| # | 问题 | 影响 |
|---|---|---|
| **SP-3** | **画布 spike**：打包 WKWebView 里 200×2048px 纹理 + 平移缩放，量 FPS/VRAM | **承诺主路径前必做** |
| **SP-4** | 实测观察 Codex 生图的**限流节奏**（快速试很多次会不会被 throttle）| 影响批量/候选数策略 |
| **OQ-4** | 候选落点（托盘 vs 直接平铺右侧）| 画布交互 |
| **OQ-5** | 预设清单：v1 方向 = 去背景 / 扩图 / **胶片·相机风滤镜（可多选批量套用真人照，如 Fuji 胶片）** + 本地裁剪/缩放；具体清单待细化 | 需产品方继续给例子 |
| **OQ-6** | 文件夹↔画布同步语义（外部增删 / 删除 / 命名）| 持久化细节 |

**草案状态：地基（runtime + 画布栈）已定，可进入实现规划；SP-3 是唯一的硬性前置验证。**
