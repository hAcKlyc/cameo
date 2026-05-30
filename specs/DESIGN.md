# Cameo 设计系统 — DESIGN.md

> **状态**：**v1.0.0 已落地**
> **视觉设计真相源** —— token、字体、组件外观、状态样式 / 画布交互色都以这份为准。
> **更新**：2026-05-25 · **品牌基调**：浅色 + 红色（Brand Red `#E53935`）。

---

## 0. 这份文档是什么

- **是**：一套已落地的 design token + 组件外观规范。Cameo 的 React chrome（侧栏 / 对话 / 工具栏 /
  弹层）和 PixiJS 画布交互色都引用这一层，新增组件请按本文 §10 状态矩阵补齐四态。
  token 速查见 §15，实际值与 `src/styles/app.css` 的 `:root` 同步。

### 0.1 相对参考图的设计判断（我改了什么 & 为什么）

> 产品方明确：参考图是参考，需独立判断。以下为本轮主动调整，均为可辩护的工程/设计取舍。

| # | 调整 | 原因 |
|---|---|---|
| J1 | **品牌红与危险红解耦**：参考里 `brand-400` 与 `error` 同为 `#EF4444`。改为品牌红 `#E53935`（暖/鲜）、危险红独立 `#DC2626`（深/冷） | 同值会让"发送/确认"主按钮与"删除/报错"无法区分；解耦后并排可辨，且 `#DC2626` 过 AA 可做文字 |
| J2 | **按钮交互方向修正**：参考 hover 比默认更浅；改为"越按越深"（rest `#E53935`→hover `#D32F2F`→pressed `#C62828`） | 填充按钮 hover/pressed 变浅反直觉；变深是桌面端通行惯例，反馈更明确 |
| J3 | **可访问性兜底**：`#E53935` 白底仅 ~4.2:1、`gray-600 #8E8E93` 白底仅 ~3.3:1，均不过 AA 正文 | 红色**文字**改用 `#D32F2F`(~5:1)；正文次级文字用新增 `gray-700 #5C5C61`(~6.5:1)，`gray-600` 降级为标签/占位 |
| J4 | **补全中性阶**：新增 `gray-500 #A3A3A8` / `gray-700 #5C5C61` | 原 `400→600→800` 断层大，缺"可读的深次级灰" |
| J5 | **阴影升级双层**（接触 + 环境） | 浅色下单层阴影发灰、缺立体感；双层更真实克制 |
| J6 | **补系统级 token**：focus ring / scrim / 禁用规则 / **z-index 层级** | 一个含画布+chrome+多浮层的桌面 app 必须显式定义层叠与焦点 |
| J7 | **新增「画布交互色」**（§3.6，Cameo 专属）：选区/手柄/标注用红 + 白 halo | 画布铺满任意图片，纯红描边会被红/暗图吞掉；加白 halo 保证任何底图上都可辨 |
| J8 | **字体改"系统自带、不打包"**（D2）：Latin→SF Pro，CJK→PingFang SC | 产品方决定本轮不打包思源黑体；PingFang 度量接近，视觉一致，省体积 |

---

## 1. 设计原则

| 原则 | 含义 | 在 Cameo 的体现 |
|---|---|---|
| **清晰层级** | 用层级与留白突出重点信息与操作 | 画布是主角，chrome 退后；强调用品牌红，其余克制 |
| **一致体验** | 统一组件、间距、交互模式，降低学习成本 | 所有按钮/输入/卡片走同一套 token 与状态矩阵 |
| **高效可达** | 减少操作路径，提升任务完成效率 | 预设/快捷键/命令栏直达；少层级、少模态 |
| **简洁克制** | 克制使用色彩与装饰，专注内容与功能 | 大面积中性灰白；红色只用于主操作与强调 |
| **可靠反馈** | 及时、明确的状态反馈，增强信心 | 生成进度、校验态、Toast、空/错/成功状态齐备 |

交互层支撑原则：**清晰反馈 · 一致交互 · 易于发现 · 减少认知负担**。

---

## 2. 品牌与基调

- **品牌色 = 红（Brand Red）**，核心 `#E53935`。只用于：主按钮、重要操作、当前选中/激活、关键强调。
  **不可**用红色铺大面积背景；一屏内"实心红"元素尽量唯一。
- **底色 = 中性灰白**，浅色优先。大面积留白让内容（在 Cameo 里＝图片）成为主角。
- **基调关键词**：简洁、清晰、专注、克制。靠层级与间距分区，而非线条/色块。

---

## 3. 颜色体系（Color）

### 3.1 中性色 Neutral

| Token | 名称 | Hex | 典型用途 |
|---|---|---|---|
| `gray-0` | 白色 | `#FFFFFF` | 卡片/面板表面、输入框底 |
| `gray-50` | 灰 50 | `#FAFAFA` | 页面/app 背景 |
| `gray-100` | 灰 100 | `#F5F5F7` | **画布底色**、嵌套区块、hover 底 |
| `gray-200` | 灰 200 | `#E8E8EA` | 分隔块、轨道底、tertiary 按下 |
| `gray-300` | 灰 300 | `#D8D8DC` | 输入框/控件描边 |
| `gray-400` | 灰 400 | `#B9B9BF` | 占位符、三级文字、禁用文字 |
| `gray-500` | 灰 500 ✚ | `#A3A3A8` | 图标默认、弱化次级（新增，补断层 J4） |
| `gray-600` | 灰 600 | `#8E8E93` | 标签/caption（仅大字或非关键文本，AA-large） |
| `gray-700` | 灰 700 ✚ | `#5C5C61` | **可读次级文字/正文次级**（新增，~6.5:1 过 AA） |
| `gray-800` | 灰 800 | `#2C2C2E` | Tooltip 底、深色反衬 |
| `gray-900` | 灰 900 | `#1A1A1C` | 主文字、标题 |

> ✚ = 相对参考图新增（J4）。`gray-600 #8E8E93` 白底对比仅 ~3.3:1，**不要**用于关键正文，
> 关键次级文字一律用 `gray-700`。

### 3.2 品牌色 Brand Red

| Token | Hex | 用途 |
|---|---|---|
| `brand-50` | `#FFF1F2` | 侧栏激活底、最浅强调底 |
| `brand-100` | `#FEE2E2` | 选中行/卡片底、文字按钮 hover 底、focus ring 基色 |
| `brand-200` | `#FCA5A5` | 禁用态主按钮底、浅强调 |
| `brand-300` | `#F87171` | 浅边/图表辅助 |
| `brand-500` | `#E53935` | **主操作/强调/识别色**（唯一"实心红"主色） |
| `brand-600` | `#D32F2F` ✚ | 主按钮 hover、**红色文字/链接**（~5:1 过 AA） |
| `brand-700` | `#C62828` ✚ | 主按钮 pressed、强红文字 |

> 沿 Material red 同色族加深（600/700 为新增，J2/J3）。**填充按钮越按越深**。
> 红色**文字**在白底用 `brand-600`（非 `brand-500`，后者 4.2:1 不过 AA 正文）。

### 3.3 功能色 Semantic

| Token | Hex | 浅底（chip/校验底） | 用途 |
|---|---|---|---|
| `success` | `#22C55E` | `#ECFDF3` | 成功校验、完成、Toast 成功 |
| `warning` | `#F59E0B` | `#FEF6E7` | 警告校验、需注意 |
| `danger` | `#DC2626` ✚ | `#FEECEC` | 错误校验、危险/删除操作 |
| `danger-strong` | `#B91C1C` ✚ | — | 危险按钮 hover/pressed、强错误文字 |
| `info` | `#64748B` | `#EEF1F4` | 中性信息、提示 |

> **`danger #DC2626` 与 `brand-500 #E53935` 是两种红、刻意区分（J1）**：品牌红更暖/鲜（主操作），
> 危险红更深/冷（删除与报错）。删除按钮因此天然比发送按钮"更沉"，语义即视觉。
> 危险红仅用于：删除类按钮、错误校验/状态、错误 Toast——**不**用于大面积装饰。

### 3.4 语义化别名（组件引用这一层，而非裸 hex）

| 别名 | 取值 | 含义 |
|---|---|---|
| `--cm-bg` | `gray-50 #FAFAFA` | 页面/app 背景 |
| `--cm-surface` | `white #FFFFFF` | 卡片、面板、输入底 |
| `--cm-panel` | `gray-50 #FAFAFA` | 嵌套面板（与 surface 区分层次） |
| `--cm-canvas` | `gray-100 #F5F5F7` | PixiJS 画布底色 |
| `--cm-fg` | `gray-900 #1A1A1C` | 主文字 |
| `--cm-fg-secondary` ✚ | `gray-700 #5C5C61` | 可读次级文字（默认用它，非 muted） |
| `--cm-muted` | `gray-600 #8E8E93` | 标签/caption（仅非关键/大字） |
| `--cm-subtle` | `gray-400 #B9B9BF` | 占位符/三级 |
| `--cm-line` | `#E5E7EB` | 1px 边框 |
| `--cm-divider` | `#F1F1F3` | 分割线 |
| `--cm-accent` | `brand-500 #E53935` | 品牌/主操作 |
| `--cm-accent-hover` | `brand-600 #D32F2F` | 主操作 hover（更深，J2） |
| `--cm-accent-press` | `brand-700 #C62828` | 主操作 pressed |
| `--cm-accent-text` ✚ | `brand-600 #D32F2F` | 白底上的红色文字/链接（过 AA） |
| `--cm-accent-bg` | `brand-50 #FFF1F2` | 选中/激活底 |
| `--cm-on-accent` | `white #FFFFFF` | 红底上的文字（用 ≥14 Semibold） |
| `--cm-danger` ✚ | `#DC2626` | 危险/错误 |
| `--cm-focus-ring` ✚ | `0 0 0 3px rgba(229,57,53,.20)` | 键盘焦点环 |
| `--cm-scrim` ✚ | `rgba(0,0,0,.40)` | 模态/抽屉遮罩 |

### 3.5 用色规则

- 红是**信号色**不是背景色；实心红一屏尽量唯一。
- 文字对比：主 `gray-900`、可读次级 `gray-700`、弱标签 `gray-600`、占位 `gray-400`；正文目标 ≥ 4.5:1。
- 危险操作用 `danger` 红 + "删除"文案 + 图标三重区分，不只靠颜色。

### 3.6 画布交互色（Cameo 专属，✚ 新增 J7）

画布会铺满任意图片，UI 描边可能压在红/暗/花底图上——所有画布交互元素都要**自带对比兜底**。

| 元素 | 规范 |
|---|---|
| 选中描边 Selection | 细 `brand-500` 实线（约 2px，随图缩放、有上限）+ 一点柔光（外发光 alpha~0.15）。**克制——不要粗白 halo band** |
| 框选 Marquee | 边 `brand-500` 1px + 填充 `rgba(229,57,53,.10)` |
| 变换手柄 Handles | 白底 + `brand-500` 1px 边 + `shadow-1`，hover 实心 `brand-500` |
| 标注默认笔触 Annotation | `brand-500`，描边外带细白 halo（保证 overlay 发给 agent 后仍清晰可见）；粗细/色后续可选 |
| 血缘连线 Lineage | `gray-400` 细线；hover/选中升 `brand-300` |

> 标注是 overlay-as-image（见 PRD），渲染成蒙层图随原图发给 Codex——默认高对比红 + 白 halo
> 既利于用户在画布上看清，也利于 agent 在产出整图里识别被指区域。

---

## 4. 字体排版（Typography）

### 4.1 字体家族（D2：系统自带，不打包）

- **拉丁/数字**：SF Pro（macOS 系统字体，经 `-apple-system` / `system-ui` 解析）。
- **中文**：**PingFang SC**（macOS 系统中文字体）。本轮**不打包思源黑体**——PingFang 与思源黑体
  度量接近，视觉一致，省体积（解决 OQ-D2）。
- **等宽**：SF Mono / Menlo（代码、文件 diff、技术信息）。

> macOS WKWebView 下 `-apple-system` 会自动 Latin→SF、CJK→PingFang；显式列 `PingFang SC`
> 作为非系统环境的兜底。字体栈见 §15。

字重 token（思源黑体/SF Pro 都覆盖）：
- `weight-regular: 400` · `weight-medium: 500` · `weight-semibold: 600`（本系统**不用 Bold 700**）。

### 4.2 字阶（Type Scale）

| 级别 | 字号/行高 | 字重 | 用途 |
|---|---|---|---|
| 标题 1 / Display | **28 / 40** | Semibold(600) | 页面主标题、空状态大标题 |
| 标题 2 / Heading | **20 / 28** | Semibold(600) | 区块标题、模态标题 |
| 标题 3 | **16 / 24** | Medium(500) | 卡片标题、分组标题 |
| 正文 / Body | **14 / 22** | Regular(400) | 正文、列表项、输入文字 |
| 辅助 / Caption | **12 / 18** | Regular(400) | 辅助说明、时间戳、helper text |
| 标签 / Overline | **11 / 16** | Regular(400) | 注释、角标、最小标签 |

> 原参考标注 SF Pro Display（大标题）/ SF Pro Text（正文）+ 思源黑体；Cameo 用系统等价（§4.1）。

### 4.3 用法

- 字号阶就这 6 级，不自造中间值；强调靠**字重 + 颜色**，不靠放大零点几号。
- **按钮/标签文字**：14px **Medium(500)**；主按钮文字（红底）用 **Semibold(600)** 提清晰度。
- 标题 `letter-spacing: -0.02em ~ -0.04em`（紧），正文默认。

---

## 5. 间距（Spacing）& 网格

基础单位 **4px**：`4 · 8 · 12 · 16 · 20 · 24 · 32 · 40 · 48 · 64`

| Token | px | 用途 |
|---|---|---|
| `space-1` | 4 | 图标与文字间、最小内距 |
| `space-2` | 8 | 紧凑控件内距、chip 内距 |
| `space-3` | 12 | 按钮内距、列表项间距 |
| `space-4` | 16 | 卡片/区块内距 |
| `space-5` | 20 | 中等区块间距 |
| `space-6` | 24 | 卡片间距、面板内距 |
| `space-8` | 32 | 分区间距 |
| `space-10` | 40 | 大区块间距 |
| `space-12` | 48 | 页面级留白 |
| `space-16` | 64 | 最大留白、空状态 |

控件最小高度：**32px**（小）/ **36px**（中，默认）/ **40px**（大）。卡片内距 `space-4~6`。

---

## 6. 圆角（Radius）

`6 · 8 · 12 · 16 · 20`（+ 派生 pill / full）

| Token | px | 用途 |
|---|---|---|
| `radius-sm` | 6 | 输入框、小按钮、chip |
| `radius-md` | 8 | 标准按钮、下拉、tooltip |
| `radius-lg` | 12 | 卡片、popover、面板 |
| `radius-xl` | 16 | 模态对话框、大面板 |
| `radius-2xl` | 20 | 大容器、抽屉 |
| `radius-pill` | 999 | 徽标、分段控件、开关、状态 chip |
| `radius-full` | 50% | 头像、圆点、圆形图标按钮 |

---

## 7. 阴影（Elevation / Shadow）

升级为**双层**（接触阴影 + 环境阴影，J5），浅色下更真实、不发灰：

| 层级 | Token | 值 | 用途 |
|---|---|---|---|
| 浅层 | `shadow-1` | `0 1px 2px rgba(0,0,0,.04)` | 静置卡片、输入、行 hover |
| 中层 | `shadow-2` | `0 1px 2px rgba(0,0,0,.04), 0 4px 12px rgba(0,0,0,.06)` | popover、下拉、toast、悬浮卡片 |
| 高层 | `shadow-3` | `0 2px 4px rgba(0,0,0,.06), 0 12px 32px rgba(0,0,0,.12)` | 模态、抽屉、侧边面板 |

> 浅色体系优先用**边框 + 极浅阴影**界定层次；环境层的 12px/32px 软散保留参考图原值。

---

## 8. 边框与分割（Border）

| Token | 值 | 用途 |
|---|---|---|
| `--cm-line` | `1px solid #E5E7EB` | 卡片/输入/面板默认描边 |
| `--cm-divider` | `#F1F1F3` | 列表/区块分割线（比边框更隐形） |

描边一律 1px；分割优先用 `divider`。

---

## 9. 图标（Icon）

- **线性图标**：`1.5px` 线宽、**圆角端点**。默认 `gray-500/600`，hover/激活 `gray-900` 或 `brand-500`。
- **状态图标**：实心、走功能色（成功绿/警告橙/危险红/信息蓝）。
- 尺寸 `16 / 20 / 24`px；与文字并排时比字号大 2~4px。
- **推荐 Lucide**（默认 1.5px、圆角端点，契合本规范）或 Phosphor。统一一套，不混用。

---

## 10. 组件规范（Components）

通用：交互组件具备 **默认 / 悬停 / 按下 / 禁用** 四态；表单组件另有 **聚焦** 与 **校验** 态。
键盘焦点一律 `--cm-focus-ring`。

### 10.1 按钮 Buttons

圆角 `radius-md 8px`，中号高 36px，内距 `0 16px`，文字 14 Medium（主按钮 Semibold）：

| 类型 | 默认 | 悬停 | 按下 | 禁用 |
|---|---|---|---|---|
| **主按钮 Primary** | 底 `brand-500` / 白字 | 底 `brand-600` | 底 `brand-700` | 底 `brand-200` / 白字 |
| **次要 Secondary** | 白底 / 边 `gray-300` / `gray-900` | 底 `gray-50` | 底 `gray-100` | 边 `gray-200` / `gray-400` |
| **三次 Tertiary** | 底 `gray-100` / `gray-900` | 底 `gray-200` | 底 `gray-300` | 透明 / `gray-400` |
| **文字 Text** | `brand-600` 字 / 无底 | 底 `brand-50` | 底 `brand-100` | `gray-400` |
| **危险 Destructive** | 底 `danger #DC2626` / 白字 | 底 `danger-strong #B91C1C` | 再加深 | 底 `#FCA5A5` / 白字 |

> 主按钮"越按越深"（J2）。危险用 `danger` 红（≠ 品牌红，J1）。
> **次级/列表内删除**用 outline-danger（边+字 `danger`，透明底），把"实心危险红"留给主危险动作（如模态确认）。

**图标按钮 Icon Button**：圆形或 `radius-md`，三尺寸——大 40（图标 20）/ 中 32（图标 18）/ 小 28（图标 16）。
可承载主色实心、中性描边、危险红三种调性。

### 10.2 输入控件 Inputs

通用：白底、边 `gray-300`、`radius-sm 6px`、高 36px、内距 `0 12px`、文字 `gray-900`、占位 `gray-400`。
- **Focus**：边 `brand-500` + `--cm-focus-ring`。
- **Disabled**：底 `gray-100`、文字 `gray-400`。

覆盖：文本框、密码框（尾 👁 显隐）、搜索框（前导 🔍）、多行 Textarea、选择器 Select（尾 ⌄）、
组合选择 Combobox、数字输入 Number Stepper（`− 1 +`）。

### 10.3 选择控件 Selection

- **Checkbox**：选中＝`brand-500` 填充 + 白勾，`radius-sm`；未选＝白底 + `gray-300` 边；禁用置灰。
- **Radio**：选中＝`brand-500` 环 + 实心点；未选＝`gray-300` 环。
- **Toggle**：开＝轨 `brand-500` + 白柄；关＝轨 `gray-300` + 白柄；`radius-pill`。
- **Segmented Control**：`gray-100` 轨（`radius-pill`/`md`）；选中段＝白底 + `shadow-1` + `gray-900` 字。
  用于"全部/进行中/已完成"互斥切换与视图切换。

### 10.4 反馈控件 Feedback

- **Progress Bar**：轨 `gray-200`、填充 `brand-500`、`radius-pill`，可带 `65%`。
- **Slider**：轨 `gray-200`、已过 `brand-500`、白柄 + `shadow-1`，旁显数值。
- **Spinner**：`brand-500` 圆弧旋转；中性场景 `gray-400`。
- **Badge**：计数（`brand-500` 底 + 白字）、`NEW` 文字徽标、纯红点（未读）。
- **Status Chip**：浅底 + 同色字 + `radius-pill`——成功/警告/危险/信息/默认（中性灰）。
- **Tooltip**：**深底** `gray-800 #2C2C2E` + 白字、`radius-md`、`shadow-2`、小箭头（浅色里故意走深色，更醒目）。

### 10.5 表单模式 Form Patterns + 校验

- **Inline Label / Helper Text**：标签 `gray-700`，helper `gray-600`（如"最多 200 字"）。
- **校验态**（边框 + 尾图标 + 下方同色文案）：
  - 成功：边 `success` / 尾 ✓ / "验证通过"
  - 警告：边 `warning` / 尾 ⚠ / "请检查内容"
  - 错误：边 `danger` / 尾 ✕ / "请输入有效内容"
  - 禁用：底 `gray-100` / `gray-400`

### 10.6 卡片 Cards

| 变体 | 特征 |
|---|---|
| 基础卡片 | 白底、`line` 边、`radius-lg`、内距 `space-4~6`；标题3 + 正文 + 操作行 |
| 悬浮卡片 | + `shadow-2`（hover 抬升）；可点击/拖拽项 |
| 可选卡片 | 选中：边 `brand-300/500` + 角标 ✓ + 底微 `brand-50` |
| 紧凑卡片 | 单行标题 + 副标 + 尾 `›`；密集列表 |
| 列表项卡片 | 头像/缩略 + 标题 + 副信息 + 右上时间戳 |
| 数据/信息卡片 | 大数字（标题1/2）+ 标签 + 涨跌（`success`/`danger` 着色）+ 迷你折线 |
| 空状态卡片 | 居中插画 + "暂无数据" + 说明 + 主操作 |

### 10.7 容器与浮层 Containers & Overlays

| 容器 | 规范 |
|---|---|
| 模态 Modal | `surface`、`radius-xl`、`shadow-3`、`space-6` 内距；遮罩 `--cm-scrim`；标题 + 内容 + 右下操作（取消 次要 / 确认 主） |
| 侧边面板 Side Panel | 右贴边、`radius-lg`、`shadow-3`、标题 + ✕；详情（状态/负责人/时间…） |
| 抽屉 Drawer | 同侧边面板，承载筛选/表单；底部 重置/应用 |
| 气泡 Popover | `radius-lg`、`shadow-2`、小箭头；轻量解释/帮助 |
| 下拉 Dropdown | `radius-md/lg`、`shadow-2`；项＝图标 + 文字，hover `gray-50`；危险项红字 + 上分割线 |
| 轻提示 Toast | `radius-lg`、`shadow-2`；状态图标 + 标题 + 说明 + ✕；短驻 |

### 10.8 导航 Navigation

| 模式 | 规范 |
|---|---|
| 左侧导航 Sidebar | 项＝图标 + 文字；**激活＝底 `brand-50` + 字/图标 `brand-600`**（可加左侧 2px 红条）；hover `gray-50` |
| 顶部标签 Tabs | 激活＝`gray-900` 字 + 底部 `brand-500` 2px 下划线；非激活 `gray-600` |
| 面包屑 Breadcrumb | `gray-600` + `›`，末级 `gray-900` |
| 命令栏 Command Bar | `⌘K` 唤起；搜索 + 分组命令（图标 + 名 + 快捷键） |
| 上下文菜单 Context Menu | 同 Dropdown |
| 分页 Pagination | 当前页＝`brand` 描边/底；其余 `gray-600` |

### 10.9 列表与表格 Lists & Tables

- **简单/分组列表**：图标 + 标题 + 时间戳；分组头 `caption/gray-600`；分隔用 `divider`。
- **行状态**：默认 / hover `gray-50` / 选中 `brand-50`；进行中·已完成·阻塞用状态点着色。
- **可排序表头**：列名 + `⌃⌄`，激活列 `gray-900`。
- **行选择**：行首 checkbox，选中行底 `brand-50`。

---

## 11. 布局模式（Layout Patterns）

| 模式 | 用途 | Cameo 对应 |
|---|---|---|
| 双栏 | 列表 + 详情 | 侧栏（工作区）+ 画布 |
| 三栏 | 导航 + 列表 + 详情 | 工作区栏 + 会话/列表 + 画布/详情 |
| 详情面板 | 选中项右侧详情 | 选中图的元信息/血缘面板 |
| 分栏视图 | 左右对照 | before/after、2-up 比较 |

顶栏高 44px，左侧为 macOS 红绿灯预留 ~92px（见 §14）。

---

## 12. 状态与反馈（Empty / Loading / Error / Success）

| 状态 | 视觉 | 文案 + 操作 |
|---|---|---|
| 空 Empty | 居中线性插画（`gray-400`）+ 标题 | "暂无数据" + 说明 + 主操作 |
| 加载 Loading | `brand-500` spinner + caption | "加载中…" |
| 错误 Error | `danger` 圆形 ✕ | "加载失败" + "重试"（次要按钮） |
| 成功 Success | `success` 圆形 ✓ | "操作成功" + 说明 + "知道了" |

---

## 13. 动效（Motion）

> 原参考未给数值；以下为与"简洁克制"一致的建议默认。

| 场景 | 时长 | 缓动 |
|---|---|---|
| 微交互（hover/按下/勾选） | 120ms | `ease-out` |
| 标准过渡（展开/切换） | 200ms | `cubic-bezier(.2,0,0,1)` |
| 浮层进入（模态/抽屉/popover） | 240–280ms | enter `ease-out` / exit `ease-in` |

位移小（≤ 8px）、以透明度为主；无弹跳。尊重 `prefers-reduced-motion`（仅留淡入淡出）。

---

## 14. 落地到 Cameo（Migration · 已完成）

> **本章是迁移留档**。从首版的**深色 + 蓝色**到当前的**浅色 + 红色**，P1–P3 已落地（commit
> `e3d9301` 起，多 commit 累积）；P4 收尾 + QA 看效果再决定要不要补（OQ-D1 见 §16）。下面这些
> token 对照只作历史留档，**写代码时一律对照 §15 的最终 token，不再走"旧值"**。

### 14.1 Token before → after（`src/styles/app.css` 的 `:root`）

| 旧变量 | 旧值（深） | 新角色 | 新值（浅） |
|---|---|---|---|
| `color-scheme` | `dark` | → | `light` |
| `--cm-bg` | `#0e0e10` | 页面背景 | `#FAFAFA` |
| `--cm-fg` | `#e6e6ea` | 主文字 | `#1A1A1C` |
| `--cm-muted` | `#8a8a93` | 次文字 | 关键次级→`#5C5C61`(`fg-secondary`)，弱标签→`#8E8E93` |
| `--cm-line` | `#2a2a30` | 边框 | `#E5E7EB` |
| `--cm-panel` | `#1a1a1e` | 卡片/面板 | `#FFFFFF`（surface）/ 嵌套 `#FAFAFA` |
| `--cm-accent` | `#6ea8ff`（蓝） | **品牌主色** | `#E53935`（红） |
| —（原 bg 即画布底） | `#0e0e10` | **画布底色** | `#F5F5F7`（新增 `--cm-canvas`） |

散落硬编码色（约 64 处）按语义归并：
- 蓝系强调 `#6ea8ff/#84b6ff/#9bbcff` 及 rgba → `--cm-accent` 家族（红）。
- 成功 `#4ade80`→`#22C55E`；警告 `#fbbf24`→`#F59E0B`；错误 `#ff6b6b/#ff5e5e`→`#DC2626`。
- 深面板 `#121214/#16161a/#1c1c20` → `surface/gray-50/gray-100`。
- 蓝底深字 `#06121f` → 主按钮直接白字 `#FFFFFF`。
- **画布选区/手柄**（原蓝）→ §3.6 的 `brand-500` + 白 halo。

### 14.2 改动落点（已完成的位置 · 写新代码时按这套来）

- **`src/styles/app.css`**：`:root` token = §15 实现版；功能态走语义别名（`--cm-accent`/`--cm-fg`
  / `--cm-line` …），**不再写裸 hex**（新组件请遵守）。
- **`index.html`**：冷启动 splash 底 `#FAFAFA` / 字 `#1A1A1C`。
- **PixiJS 画布**（`src/canvas/scene.ts`）：renderer 背景 `#F5F5F7`；选区描边/手柄/标注用 §3.6 的
  `brand-500` + 白 halo（accent `0xE53935`）。
- **Tauri 窗口**（`src-tauri/tauri.conf.json`）：window background `#FAFAFA`，保留 macOS 红绿灯留白
  ~92px。
- **图标**：统一到 Lucide（1.5px 圆角端点）。

### 14.3 落地阶段记录（P1–P3 ✅ · P4 部分）

1. **P1 地基** ✅ — `:root` token + splash + `color-scheme: light` + 画布底色，一眼变浅。
2. **P2 chrome** ✅ — 顶栏 / 侧栏（激活 `brand-50`）/ 对话面板 / composer 重皮。
3. **P3 组件** ✅ — 按钮 / 输入 / 卡片 / 菜单 / Toast / 状态 chip 按 §10 状态矩阵；散见
   `70f6523` 选区描边、`491df7b` 玻璃统一、`30e0060` settings 对齐。
4. **P4 收尾**（部分 ✅，按需推进）— 双层阴影 edge case、空/错/成功态动效、对比视图 polish。
5. **QA** — 打包 WKWebView 已确认 PixiJS WebGL2 渲染 59fps；OQ-D1（白图/淡图在浅画布上的可辨度）
   留作 living check，看用户反馈。

### 14.4 本轮非目标

- **不做深色主题**。§15 token 分原子层/语义层，未来派生 dark 只需重定义语义层、不动组件 ——
  结构已预留。如要做请保持四态矩阵 + 焦点环 + 画布交互色三组不裸 hex。

---

## 15. 实现 Token 速查（CSS `:root` Drop-in）

> 直接替换 `src/styles/app.css` 顶部 `:root`。保留旧别名以最小化改动，并新增原子层与扩展语义层。

```css
:root {
  color-scheme: light;

  /* ---- 原子层 · 中性 ---- */
  --cm-white:    #FFFFFF;
  --cm-gray-50:  #FAFAFA;
  --cm-gray-100: #F5F5F7;
  --cm-gray-200: #E8E8EA;
  --cm-gray-300: #D8D8DC;
  --cm-gray-400: #B9B9BF;
  --cm-gray-500: #A3A3A8;   /* 新增 */
  --cm-gray-600: #8E8E93;
  --cm-gray-700: #5C5C61;   /* 新增：可读次级文字 */
  --cm-gray-800: #2C2C2E;
  --cm-gray-900: #1A1A1C;

  /* ---- 原子层 · 品牌红 ---- */
  --cm-brand-50:  #FFF1F2;
  --cm-brand-100: #FEE2E2;
  --cm-brand-200: #FCA5A5;
  --cm-brand-300: #F87171;
  --cm-brand-500: #E53935;
  --cm-brand-600: #D32F2F;   /* hover / 红色文字 */
  --cm-brand-700: #C62828;   /* pressed / 强红文字 */

  /* ---- 原子层 · 功能 ---- */
  --cm-success:       #22C55E;  --cm-success-bg: #ECFDF3;
  --cm-warning:       #F59E0B;  --cm-warning-bg: #FEF6E7;
  --cm-danger:        #DC2626;  --cm-danger-bg:  #FEECEC;  /* ≠ 品牌红 */
  --cm-danger-strong: #B91C1C;
  --cm-info:          #64748B;  --cm-info-bg:    #EEF1F4;

  /* ---- 语义层（组件引用这一层）---- */
  --cm-bg:           var(--cm-gray-50);
  --cm-surface:      var(--cm-white);
  --cm-panel:        var(--cm-gray-50);
  --cm-canvas:       var(--cm-gray-100);
  --cm-fg:           var(--cm-gray-900);
  --cm-fg-secondary: var(--cm-gray-700);   /* 默认次级文字 */
  --cm-muted:        var(--cm-gray-600);   /* 仅弱标签/大字 */
  --cm-subtle:       var(--cm-gray-400);
  --cm-line:         #E5E7EB;
  --cm-divider:      #F1F1F3;
  --cm-accent:       var(--cm-brand-500);
  --cm-accent-hover: var(--cm-brand-600);
  --cm-accent-press: var(--cm-brand-700);
  --cm-accent-text:  var(--cm-brand-600);  /* 白底红字过 AA */
  --cm-accent-bg:    var(--cm-brand-50);
  --cm-on-accent:    var(--cm-white);

  /* ---- 焦点 / 遮罩 ---- */
  --cm-focus-ring: 0 0 0 3px rgba(229, 57, 53, .20);
  --cm-scrim:      rgba(0, 0, 0, .40);

  /* ---- 画布交互（Cameo）---- */
  --cm-canvas-select: var(--cm-brand-500);
  --cm-canvas-halo:   rgba(255, 255, 255, .9);
  --cm-canvas-marquee-fill: rgba(229, 57, 53, .10);

  /* ---- 圆角 ---- */
  --cm-radius-sm: 6px; --cm-radius-md: 8px; --cm-radius-lg: 12px;
  --cm-radius-xl: 16px; --cm-radius-2xl: 20px; --cm-radius-pill: 999px;

  /* ---- 间距 ---- */
  --cm-space-1: 4px;  --cm-space-2: 8px;  --cm-space-3: 12px;
  --cm-space-4: 16px; --cm-space-5: 20px; --cm-space-6: 24px;
  --cm-space-8: 32px; --cm-space-10: 40px; --cm-space-12: 48px; --cm-space-16: 64px;

  /* ---- 阴影（双层）---- */
  --cm-shadow-1: 0 1px 2px rgba(0,0,0,.04);
  --cm-shadow-2: 0 1px 2px rgba(0,0,0,.04), 0 4px 12px rgba(0,0,0,.06);
  --cm-shadow-3: 0 2px 4px rgba(0,0,0,.06), 0 12px 32px rgba(0,0,0,.12);

  /* ---- 字体 ---- */
  --cm-font-display: -apple-system, "SF Pro Display", "PingFang SC", system-ui, sans-serif;
  --cm-font-sans:    -apple-system, "SF Pro Text", "PingFang SC", system-ui, sans-serif;
  --cm-font-mono:    ui-monospace, "SF Mono", Menlo, monospace;
  --cm-weight-regular: 400; --cm-weight-medium: 500; --cm-weight-semibold: 600;

  /* ---- z-index 层级（真相源在 src/styles/app.css :root；此处同步）----
     只为「浮在画布之上的系统层」定义 token，升序留空档便于插层。
     画布内部元素用一个保留的本地 band（1–16，raw 小数字），它们的相互
     顺序是画布合成细节、不是系统决策，故不进 token 体系：
       空状态 5 · 图片标题 6 · 工具栏 8 · 顶栏/HUD 10 · 选择条 11 ·
       裁切/Todo 12 · 标记评论 15 · chat/sidebar 面板 16 */
  --cm-z-menu:    30;    /* dropdown / popover / tooltip / 右键菜单 / 生成档位菜单 */
  --cm-z-overlay: 50;    /* 全窗 compare 对比浮层 */
  --cm-z-modal:   60;    /* 模态 scrim + 弹窗（设置等）*/
  --cm-z-gallery: 70;    /* Gallery 浮层（detail backdrop +10，其内菜单 +20）*/
  --cm-z-toast:  100;    /* 应用内 toast — 永远最上层 */
}
```
