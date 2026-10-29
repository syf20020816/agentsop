# AgentSOP

> 把 AI 的进度从黑盒变成可视化状态。

AgentSOP 是一个常驻 macOS 菜单栏的 SOP 控制条：不重造编辑器，而是「寄生」在 Codex / Claude Code 之上，用一个 180px 的窄面板管理 `需求 → 编码 → 测试 → 上线` 这样的粗粒度生命周期。你在看板上只关心「走到哪一步」，具体的脏活累活交给 Agent 执行。

核心是两层解耦：

- **宏观（AgentSOP）**：节点、状态机、审批、证据校验。人可理解、可回滚。
- **微观（picop / SKILL）**：一个阶段内部到底要查什么、测什么、怎么思考，编排成 Skill 交给 Agent 调用。

SOP 管阶段，Skill 管怎么做。

## 它解决什么

- **黑盒焦虑**：你问 Agent「写完了吗」，它永远说「写完了，请确认」。AgentSOP 只认完成条件、产物证据、人的审批——三者都满足才推进。
- **上下文割裂**：看板里按一下空格，节点 Prompt 直接填进 Codex / Claude 桌面版输入框，改完按 Enter 执行。不复制、不切换、不粘贴。
- **上下文污染**：人只通过 SOP 写回的 `context.md` 介入主流程，不把临时讨论倒进 Agent 的主上下文。

## 功能

- 180px 无边框悬浮控制条（180×560），`⌥⌘S` 随时呼出，始终置顶
- 节点状态机：`pending` / `ready` / `active` / `waiting_human` / `done` / `blocked` / `skipped` / `stale`
- 事件溯源：每次推进写入 `events.jsonl`，启动读 `snapshot.json` 恢复，状态可审计
- 会话绑定：Codex / Claude 桌面版会话下拉，没开始推进前可改绑，开始后锁定
- 绑定后**不自动执行**：`J`/`K` 选节点，`空格` 填入 Prompt，`Enter` 在 Agent 里发送
- 人工决策：通过 / 打回（备注 ≥ 8 字）/ 重试 / 允许一次 / 中断
- SOP 编辑器：节点增删、类型、Agent、默认 Prompt、完成条件、产物、是否需人工确认
- 默认 Prompt 支持 `/` 调本机 skill（Codex、Claude、共享 skill），保存后对进行中的运行**立即生效**
- 本地 HTTP + Hook 自动上报，MCP server 让 Agent 主动读状态、请求决策

## 快速开始

环境要求：macOS、Rust 1.85+（edition 2024）、Node.js 20.19+、pnpm。

```bash
pnpm install
pnpm tauri dev
```

首次启动后：

1. 菜单栏出现 AgentSOP 图标，按 `⌥⌘S` 呼出面板。
2. 在系统设置 → 隐私与安全性 → **辅助功能** 中，给启动 `pnpm tauri dev` 的终端授权。空格填入、Enter 发送依赖模拟按键。
3. 面板里「创建 SOP」→ 绑定一个 Codex / Claude 会话，即可开始。

打包：

```bash
pnpm tauri build
```

## 使用

| 键 | 作用 |
|---|---|
| `J` / `↓` | 选中下一个节点 |
| `K` / `↑` | 选中上一个节点 |
| `空格` | 把选中节点的默认 Prompt 填进 Agent 输入框 |
| `Enter` | 在 Agent 里发送已填入的 Prompt，执行该节点 |
| `A` | 通过（当前节点等你确认时） |
| `R` | 打回（打开备注浮层） |
| `⌘K` | 打开 / 编辑当前 SOP |
| `P` | 固定 / 取消固定面板 |
| `?` | 快捷键帮助 |
| `Esc` | 收起浮层 / 下拉 |

## CLI

同一个二进制既是 GUI 也是 CLI，所有命令都是本地 HTTP 的薄客户端，避免与界面进程抢写状态。

```bash
agentsop                       # 打开面板（等价 ⌥⌘S）
agentsop status                # 当前运行的人读摘要
agentsop json                  # 当前运行快照（JSON）
agentsop next                  # 当前节点与默认 Prompt
agentsop context               # 渲染给 Agent 的上下文
agentsop runs                  # 历史运行
agentsop start <sop> --repo . --var k=v
agentsop complete [node] --artifact docs/prd.md
agentsop decide approve|reject|retry|allow-once|abort --note "…"
agentsop pause | resume | abort
agentsop sop list|show|validate|install
agentsop hook <source>         # 供 hook 调用，stdin 传 JSON（fail-open）
agentsop hooks install|uninstall|status [claude-code|codex]
agentsop mcp install|uninstall|status [claude-code|codex]
agentsop doctor
```

## 接入 Agent

```bash
agentsop hooks install claude-code   # 合并写入 ~/.claude/settings.json，备份 .agentsop.bak
agentsop mcp install claude-code

agentsop hooks install codex         # 写 ~/.codex/hooks.json，并在 config.toml 打开 codex_hooks
agentsop mcp install codex           # codex mcp add agentsop -- <路径>/agentsop-mcp
```

MCP 工具：`sop_status`、`sop_context`、`sop_next`、`sop_list_nodes`、`sop_complete_node`、`sop_request_decision`、`sop_list_runs`；资源：`agentsop://run/<id>/context`、`agentsop://run/<id>/snapshot`、`agentsop://sop/<name>`。

## 数据与安全

```
~/.agentsop/
├── token              # HTTP Bearer token（0600）
├── run/port           # 实际监听端口
├── sops/*.yaml        # SOP 定义，可入 Git
├── sessions.json      # 跨 Agent 会话登记表
└── runs/<id>/
    ├── events.jsonl   # 事件溯源
    ├── snapshot.json  # 物化快照
    └── context.md     # 写给 Agent 的上下文
```

HTTP 只监听 `127.0.0.1:43117..43127`（被占用时顺延），所有请求必须带 token。Hook 一律 fail-open：AgentSOP 没运行也不阻塞 Agent。

## 项目结构

```
crates/core/     Rust 内核：SOP 解析、状态机、事件溯源、存储、会话发现、skill 扫描
crates/mcp/      stdio MCP server（agentsop-mcp）
src-tauri/       Tauri 壳、HTTP 端点、CLI、系统交互（模拟按键 / 激活 App）
src/             React 19 + Vite 面板
```

## 开发

```bash
cargo check --workspace
cargo test -p agentsop-core
pnpm build
```

## 文档

- [技术架构实现文档.md](技术架构实现文档.md) —— 数据模型、HTTP/Hook/MCP 协议、落盘结构
- [前端设计.md](前端设计.md) —— 面板布局、视觉系统、交互与快捷键
- [任务拆解.md](任务拆解.md)、[checklist.md](checklist.md) —— 里程碑与验收
- [agentsop-demo.html](agentsop-demo.html) —— 基于当前实现的概念演示（reveal.js）

## 状态

已跑通：菜单栏面板、状态机与事件溯源、会话绑定、SOP 编辑与热更新、空格/Enter 交互、Claude Code Hooks、MCP server、本地 HTTP API。

下一步：`/side` 独立上下文窗口；用户直接在 Agent 里执行时自动感知节点状态；picop 导出的 Skill 与默认 Prompt 打通。
