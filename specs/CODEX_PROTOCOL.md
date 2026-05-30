# CODEX_PROTOCOL.md — Codex app-server 协议真相源

> Cameo 用本机 **Codex CLI 的 `codex app-server`** 作为唯一 agent runtime。本文件是
> **Codex 协议层面的真相源**：连接方式、JSON-RPC 方法、生成参数语义、事件流、图像 I/O、
> 以及这些在 Cameo 里**落在哪、怎么用**。
>
> 与其它文档的分工：
> - [`ARCHITECTURE.md`](./ARCHITECTURE.md) = 系统架构（模块边界 / 存储 / 构建）。
> - **本文件** = Codex 协议细节 + Cameo 的协议级应用（runtime adapter 怎么翻译）。
> - `specs/research/research_codex_runtime.md`（**gitignore，仅本地**）= 早期从 MyAgents 移植的
>   调研笔记，含 MyAgents file:line 锚点。开源仓库读不到。
>
> **所有协议事实均以本机 `codex app-server generate-json-schema` 生成的 v2 schema +
> 官方文档 + 活体探针三方核验为准**（核验环境：`codex-cli 0.134.0`，2026-05）。Codex 升级后，
> 协议可能漂移 —— 改 runtime 前先重新生成 schema 核对（见 §11）。

---

## 0. Headline

- 驱动方式：`codex app-server` —— **长生命周期 JSON-RPC 2.0 over stdio** 进程，整个 session 存活
  （不是 `codex exec` / `codex proto` / `codex mcp`，也不是每轮重启）。一个 Board 一个进程。
- 鉴权：用户的 **ChatGPT 订阅**（`~/.codex`，`codex login` 一次），**无 API key**。
- 会话连续性 = Codex 自己的 **`threadId`**（Cameo 持久化到 `.cameo/sessions.json`）。
- 原生支持图像输入（文本里给路径 / `localImage`）和图像输出（`imageGeneration` item）。

---

## 1. 调用与连接

```
codex app-server          # cwd = Board folder
```
- spawn 一次、保活；stdin/stdout/stderr piped。`codex.rs:962-979`。
- Unix `process_group(0)`（自成进程组，便于 tree-kill）；代理从 `~/.cameo/config.json` 注入。
- **PATH**：必须先把裸 `codex` 解析成绝对路径（GUI 应用 PATH 极简，缺 nvm/homebrew/cargo）。
  `codex.rs:165-186`。
- 短生命周期探针也用 app-server：auth 探测（`probe_codex_auth`）、`codex --version`（detect）。

### 握手
`initialize { clientInfo, capabilities }` → 紧接着发 `initialized` notification（spec 要求，Cameo 发）。

---

## 2. Wire 协议 — newline-delimited JSON-RPC 2.0

一行一个 JSON 对象（`{...}\n`），按 `\n` 切分（**不是** LSP Content-Length framing）。三类消息：

1. **Client→Server 请求**（`id`+`method`）：`call(method, params, timeoutMs)`，按 `id` 匹配 Promise。
2. **Server→Client 通知**（`method`，无 `id`）：流式输出。
3. **Server→Client 请求**（`method`+`id`）：Codex 要客户端做审批/输入决策 → 必须 `respond(id, result)`。

Cameo 的 JSON-RPC 客户端：`codex.rs` 的 `CodexSessionInner`（`call` / `write` / reader loop）。

### Client 请求方法全集（0.134.0，81 个；常用列出）

| 方法 | 用途 |
|---|---|
| `initialize` | 握手 |
| `thread/start` | 新会话 → `{ thread:{id}, model, serviceTier, ... }` |
| `thread/resume` | 恢复线程（需 `threadId`），接受与 start 同样的 override |
| `thread/read` | 读线程（**注意：返回的 Thread 对象不含 serviceTier 等档位字段**） |
| `turn/start` | 发用户消息开跑 → `{ turn:{id} }`，随后流式 |
| `turn/steer` | 向进行中的 turn 追加输入（**不带档位 override**） |
| `turn/interrupt` | 取消进行中的 turn |
| `model/list` | 枚举模型（驱动输入框菜单，见 §6） |
| `thread/list` / `thread/fork` / `thread/rollback` / `thread/compact/start` / `thread/goal/*` / `thread/metadata/update` / `thread/shellCommand` / `thread/inject_items` / … | 其它线程管理（Cameo 暂未用） |

> ⚠️ **不存在 `thread/settings/update` 请求**（只有 `thread/settings/updated` *通知*，§8）。
> 即：会话开始后，改生成档位的**唯一**入口是 `turn/start` 的 override（已逐字核 ClientRequest schema）。

---

## 3. thread/start · thread/resume · turn/start 参数

### thread/start（`ThreadStartParams`，15 字段）
```
cwd, model, approvalPolicy, sandbox, developerInstructions, ephemeral,
personality, serviceTier, serviceName, modelProvider, baseInstructions,
approvalsReviewer, sessionStartSource, threadSource, config(任意 toml override)
```
- `config`（`additionalProperties: true`）是万能逃生口，可塞任意 `~/.codex/config.toml` 键，
  **不污染用户全局配置**。
- 未指定的字段 → 用用户当前 config 设置。

### thread/resume
同 start 的 override 集，外加 `threadId`，去掉 `cwd`/`ephemeral`。

### turn/start（`TurnStartParams`）
```
threadId, input[], summary, effort, model, serviceTier, personality,
cwd, sandboxPolicy, approvalPolicy, approvalsReviewer, outputSchema
```
- **生成档位 override：`model` / `effort` / `serviceTier` / `summary` / `personality`** —— 见 §4。
- `input[]` 见 §7。`outputSchema` 可约束最终 assistant 消息为指定 JSON Schema（Cameo 暂未用）。

Cameo 现状：`turn/start` 发 `{ threadId, input, summary }`（`codex.rs:1420`）；
`thread/start` 发 `{ cwd, model:null, approvalPolicy:"never", sandbox:"workspace-write",
developerInstructions, ephemeral:false }`（`codex.rs:1188`）。

---

## 4. 生成档位参数：语义（**最容易踩坑，重点**）

### sticky（跨轮残留）
`turn/start` 的 override 是 **"for this turn and subsequent turns"** —— 一旦设了就成为该 thread
后续轮的默认值，会跨轮残留。

### null 清除 vs 省略保留（三方实证）
| 传法 | 语义 |
|---|---|
| `serviceTier: "priority"` | 设为快速档 |
| `serviceTier: null` | **主动清除 tier → 回到标准/默认** |
| `serviceTier` 省略不传 | **保留上一轮的 sticky 值（不变）** |

依据：① 官方 app-server README 原文「`serviceTier: null` clears the tier / Omitted fields leave
settings unchanged」；② 本机活体探针：`thread/start` 传 `null`→回显 `"default"`、`"priority"`→
`priority`、省略→`None`（三者各异）；③ openai/codex#15853（VS Code 扩展发 `service_tier:
Some(None)` 把配置 tier 覆盖成 None，证明 null 是主动 override 非 no-op）。

> **推论**：要把"快速 → 标准"必须**显式传 `serviceTier: null`**，不能靠省略 key。同理，凡是
> Cameo 想要"我说了算"的档位，每轮都显式传值（含 null），不要依赖省略。

### 枚举值（v2 schema 权威）
| 参数 | 类型 | 取值 |
|---|---|---|
| `effort` | `ReasoningEffort` | `none` / `minimal` / `low` / `medium` / `high` / `xhigh` |
| `summary` | `ReasoningSummary` | `auto` / `concise` / `detailed` / `none` |
| `personality` | `Personality` | `none` / `friendly` / `pragmatic` |
| `sandbox` | `SandboxMode` | `read-only` / `workspace-write` / `danger-full-access` |
| `approvalPolicy` | `AskForApproval` | `untrusted` / `on-failure` / `on-request` / `never` |
| `serviceTier` | `string`/`null` | 取自 `model/list` 的 `serviceTiers[].id`（实测快速档 = `"priority"`） |

> 副作用注意：
> - `summary` 影响 reasoning 摘要流（`item/reasoning/summaryTextDelta` 等，§5）。某些模型
>   `default_reasoning_summary="none"`，传 `auto` 后摘要可能为空 —— 改前先看思考块是否还渲染。
> - `personality` 是**行为/口吻变更**（friendly / pragmatic 是不同指令块），不是纯参数。

### Cameo 当前默认（坑）
`model:null` + 不传 `effort` → **回落用户 `~/.codex/config.toml`**（典型 `gpt-5.5 + xhigh`），
这是生图"慢"的主因，且体感随每个用户的 Codex 配置而变、不可控。
**0.1.6 起**在 `turn/start` 显式下发档位（见 PRD `prd_0.1.6_generation_controls.md`）。

---

## 5. 流式通知 → UnifiedEvent 映射

**Item 抽象是核心**：一个 turn 产出一串带类型的 item；`item.type` ∈ `commandExecution`(Bash) /
`fileChange`(Edit) / `mcpToolCall` / `webSearch` / `imageView`(Read) / **`imageGeneration`** /
`reasoning` / `agentMessage` / `plan` / `contextCompaction` / …。未知类型只 warn 不 crash（前向兼容）。

| Codex 通知 | 含义 | → UnifiedEvent（`runtime.rs`） |
|---|---|---|
| `thread/started` | 线程建立 | （session_init 已发） |
| `thread/status/changed` | active/idle/systemError | `Status` |
| `turn/started` | turn 运行 | `Status(running)` |
| `turn/completed` | turn 结束（status/error/usage） | `TurnComplete` + `Usage` |
| `turn/plan/updated` | 计划/todo | `PlanUpdated` |
| `item/started` | item 开始 | `ToolStart` / `ThinkingStart` / `GenerationStarted` |
| `item/completed` | item 完成（结果/diff/**图像字节**） | `ToolStop` + `ToolResult` / `ImageGenerated` |
| `item/agentMessage/delta` | assistant 文本 token | `TextDelta` |
| `item/reasoning/summaryTextDelta`、`…/textDelta` | reasoning | `ThinkingDelta` |
| `item/commandExecution/outputDelta` 等 | 工具实时 stdout | `ToolResult`(delta) |
| `account/rateLimits/updated` | 订阅限流 | `RateLimits` |
| `thread/settings/updated` | 线程设置变更（含全 ThreadSettings） | （Cameo 暂未消费） |
| `error` | 错误 | log |

事件处理入口：Rust `codex.rs` 的 reader loop → `UnifiedEvent` → 前端 `src/store/chat.ts::handleEvent`。
加新事件：先加 `UnifiedEvent`（`runtime.rs`）+ TS `CodexEvent`（`src/types.ts`），wire 用 serde camelCase。

> **文本尾补丁 gotcha**：`agentMessage` 完成时，比对累积 delta 与 item 最终 `text`，补回尾部
> —— Codex 偶尔丢最后一个 delta。

---

## 6. model/list — 驱动输入框档位菜单

`ModelListParams { includeHidden?, cursor?, limit? }` → `ModelListResponse { data: Model[],
nextCursor? }`（**有分页**，按 `nextCursor` 兜底翻页）。

**`Model` 是 wire camelCase**（取用其中）：
```
id                          # 模型标识符 —— turn/start 的 `model` 传它（不是 cache 的 slug）
displayName                 # "GPT-5.5"
defaultReasoningEffort      # ReasoningEffort
supportedReasoningEfforts   # [{ reasoningEffort, description }]   ← 智能菜单数据源
serviceTiers                # [{ id, name, description }]          ← 速度菜单数据源（如 priority）
defaultServiceTier          # string | null
hidden, isDefault, supportsPersonality, additionalSpeedTiers(deprecated), inputModalities
```

> ⚠️ **wire vs cache 两张皮**：`~/.codex/models_cache.json` 是 **snake_case 的不同表面**
> （`slug` / `supported_reasoning_levels[].effort` / `service_tiers[].id` /
> `default_reasoning_level` / `default_reasoning_summary`）。**写代码对 `model/list` 必须用 wire
> camelCase**，照搬 cache 字段名会写出"看似对、实则取不到值"的 bug。

官方 Codex app 的档位菜单就是用 `model/list` 这套数据驱动的（智能列表 = 当前模型
`supportedReasoningEfforts`；速度列表 = 标准 + `serviceTiers`）。

---

## 7. 图像 I/O（Cameo 的命脉）

### 输入
`turn/start` 的 `input[]` 支持三类 `UserInput`：
- `{ type:"text", text, text_elements:[] }`
- `{ type:"image", url, detail? }`（远程 URL）
- `{ type:"localImage", path, detail? }`（本地路径，`detail` ∈ low/high/auto）

**Cameo 现状（决策 D4）**：只用 `text`，把引用图/overlay 的**路径内嵌在文本里**，让 Codex 自己
`read`（`codex.rs:1369` + `build_turn_prompt`）。代价是模型要先发一次读图工具调用（多一次往返），
收益是实现简单、不挂字节。**引用图自动 `localImage` 预挂图** = 未来项，需先 push-back D4。

### 输出 — `imageGeneration` item
Codex 包了 OpenAI 的 `image_generation_call`（不是 MCP 工具）。完成的 item 两种形态，**按序都处理**：
1. `item.savedPath`（Codex 自动落盘）→ 直接引用路径（零拷贝）。
2. `item.result` = base64 → 解码落盘。
`item.revisedPrompt` 是 caption。处理入口：`codex.rs:on_image_generation`（约 `2187`）→ mint Asset +
右侧落位（血缘）。

> 图像工具自身参数（size/quality/background/n/partial_images）**未在 app-server 协议暴露为可传参**
> —— 由模型自行决定。客户端只能通过 prompt 文字间接引导，无硬旋钮。

---

## 8. ThreadSettings（读侧）与 settings 通知

`thread/settings/updated` 通知携带完整 `ThreadSettings`，字段含：
`model / effort / serviceTier / summary / personality / approvalPolicy / sandboxPolicy /
collaborationMode / cwd / modelProvider / approvalsReviewer / activePermissionProfile`。

- 这是设置变更的**只读回显通道**（Cameo 暂未消费；将来若要在 UI 反映"实际生效档位"可订阅它）。
- 再次强调：**没有对应的 `update` 请求**。`thread/read` 返回的 Thread 也**不含**这些档位字段。
  所以"当前线程实际是什么档位"在协议上唯一可靠来源是 `thread/start` 响应 + `thread/settings/updated`。

---

## 9. Auth — ChatGPT 订阅，无 API key

- 不传 API key；Codex 读 `~/.codex`（`codex login` 一次，OAuth into ChatGPT，token 在
  `~/.codex/auth.json` / keychain）。
- 探测：`getAuthStatus { includeToken:false, refreshToken:false }` → `{ authMethod,
  requiresOpenaiAuth }`。判定 `requiresLogin = !authMethod && requiresOpenaiAuth === true`。
- `authMethod` ∈ `apikey` / `chatgpt` / `chatgptAuthTokens`（健康订阅用户 = `chatgpt`）。
- Cameo 不存、不展示 token。详见 `prd_codex_setup_status_panel.md` + `probe_codex_auth`。

---

## 10. 进程生命周期

- **PATH**：每平台都先 `which` 解析绝对路径（见 §1）。
- **Cancel**：`turn/interrupt`（3s）→ 关 stdin → kill 升级（SIGTERM 组 → 3s → SIGKILL → 2s）。
  tree-kill = POSIX `kill(-pid)` / Windows `taskkill /F /T /PID`。`kill_tree`。
- stdout EOF → 拒绝所有挂起 RPC（"process exited"）。去重，防 notification-completion 与 process-exit
  双触发 `session_complete`；我们主动 kill 时压掉假的 "exited 143"。
- **务必 drain stderr**（ANSI strip + log + 匹配 auth/401/403），否则啰嗦的 Codex 会堵塞管道。

---

## 11. 重新生成 / 核对 schema

```bash
codex app-server generate-json-schema --out <dir>
# 看 <dir>/v2/TurnStartParams.json / ThreadStartParams.json / ModelListResponse.json
# 枚举在 <dir>/codex_app_server_protocol.v2.schemas.json 的 definitions
```
> ⚠️ 在 read-only sandbox（如 codex exec 审查）里写不了 `/tmp`、起不了 app-server（sqlite 只读）。
> 要在普通 shell 跑。Codex 版本升级后，改 runtime 前先重生成核对本文档的字段/枚举/方法是否漂移。

---

## 12. Cameo 协议级应用速查（file:line 可能随迭代漂移，以代码为准）

| 关注点 | 位置 |
|---|---|
| spawn app-server | `src-tauri/src/codex.rs:962` |
| JSON-RPC client（call/write/reader） | `src-tauri/src/codex.rs` `CodexSessionInner` |
| `thread/start` / `resume`（`ensure_thread`） | `src-tauri/src/codex.rs:1105` |
| `turn/start` 派发（`send_message`） | `src-tauri/src/codex.rs:1339` / 1420 |
| `build_turn_prompt`（D4 路径内嵌） | `src-tauri/src/codex.rs:1445` |
| developer instructions（系统提示） | `src-tauri/src/prompt.rs` |
| `imageGeneration` 处理 | `src-tauri/src/codex.rs:~2187` |
| UnifiedEvent 定义 | `src-tauri/src/runtime.rs` |
| 事件 → 前端 | `src/store/chat.ts::handleEvent` + `src/types.ts` `CodexEvent` |
| auth 探针 | `src-tauri/src/codex.rs:~440` / `probe_codex_auth` |

### 已锁决策（与协议相关）
- runtime 抽象保留可换（v1 仅 Codex），上层只消费 `UnifiedEvent`。
- 引用走文件路径 + agent 自读（D4），不挂传图。
- approval=`never` + sandbox=`workspace-write`（Board folder 内编辑的自然默认）。
- 标记 = overlay-as-image（发图，不依赖结构化 mask API）。

### 已实现（0.1.6）
- 生成档位选择器（`prd_0.1.6_generation_controls.md`）：`turn/start` 显式下发
  model/effort/serviceTier + summary=auto/personality=friendly，输入框暴露模型/智能/速度选择器，
  per-Board 持久化到 meta.json（`store/genSettings.ts` + `codex.rs` `set_gen_settings`）。
- **消息时间线由 runtime 权威落盘**：`codex.rs` 在 turn 开始写 user 记录、turn 结束写 assistant
  记录到 active session 的 `.cameo/sessions/<id>.jsonl`，不依赖前端聚焦（修复静默丢历史）。
- **全产品网络出口统一走代理**：cloud API（`cloud_request` 命令）+ gallery 图片（`cmnet://`
  scheme）经 `net.rs` 的带代理客户端，与 sidecar 一致遵守 Settings → 代理。

---

## Sources
- 本机 `codex app-server generate-json-schema`（v2 schema，0.134.0，真相源）
- [Codex app-server docs](https://developers.openai.com/codex/app-server)
- [openai/codex app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [openai/codex#15853 — service_tier null override](https://github.com/openai/codex/issues/15853)
- [Codex Config Reference](https://developers.openai.com/codex/config-reference)
- 本机活体探针（thread/start serviceTier 回显语义）
- `specs/research/research_codex_runtime.md`（本地，MyAgents 移植笔记）
</content>
