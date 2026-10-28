# AgentSOP 验收 Checklist

> v0.1 · 2026-10-27
> 配套：[技术架构实现文档.md](./技术架构实现文档.md) · [前端设计.md](./前端设计.md) · [任务拆解.md](./任务拆解.md)

使用方式：

- 每条都是**可执行验证**，不是感觉判断。`预期` 列写的是可观察结果。
- 标记含义：`P0` 不通过不得发版；`P1` 不通过需书面记录并排期；`P2` 打磨项。
- 建议在干净账户下执行：`export AGENTSOP_HOME=/tmp/as-test && rm -rf /tmp/as-test`。
- 每条通过后不要删掉，作为**回归清单**长期保留。

---

## A · 构建与工程（P0）

- [ ] **A1** `cargo fmt --all --check` —— 无 diff
- [ ] **A2** `cargo clippy --workspace --all-targets -- -D warnings` —— 0 warning
- [ ] **A3** `cargo test --workspace` —— 全绿，无 ignored 关键用例
- [ ] **A4** `pnpm tsc --noEmit` —— 无类型错误
- [ ] **A5** `pnpm build` —— 产物生成，无 > 500KB 的单 chunk 告警（面板体积应很小）
- [ ] **A6** `pnpm tauri build` —— 生成 `.app` 与 DMG
- [ ] **A7** 仓库中不存在 `TODO:`（除 `KNOWN_ISSUES.md` 引用）与 `console.log`
- [ ] **A8** CI 上跑同样命令全绿，PR 必须过 A1-A5 才可合并

---

## K · 状态机内核（P0）

- [ ] **K1** 加载 `sops/prd-to-ship.yaml` 成功，`agentsop sop validate` 退出码 0
- [ ] **K2** 构造循环依赖（`a→b→a`）→ 校验失败，错误信息含字段与行号
- [ ] **K3** 构造重复 `id` → 校验失败并指出重复项
- [ ] **K4** `artifacts: ["../../etc/passwd"]` → 校验失败（路径 jail）
- [ ] **K5** 迁移表每一行都有单测覆盖：`cargo test -p agentsop-core transitions` 全绿
- [ ] **K6** 非法迁移（对 `done` 节点执行 `approve`）→ 返回 `INVALID_TRANSITION`，状态不变
- [ ] **K7** `agent` 节点声明 `artifacts` 但文件不存在时，`Stop` 事件**不**推进节点，仅记 `node.evidence`
- [ ] **K8** 补齐产物后再来一次 `Stop` → 节点 `done`，且 `events.jsonl` 只有一条 `node.completed`
- [ ] **K9** `when` 表达式：`review.rejected` / `verify.failed` / `build.retry < 3` 均按预期求值；非法表达式被拒绝
- [ ] **K10** 同一 `event_id` 提交 3 次 → 只产生 1 次状态迁移
- [ ] **K11** `paused` 期间收到 `Stop` → 不推进，恢复后按规则补判（或保持 `active`，按设计）
- [ ] **K12** 30 分钟无事件的 `active` 节点 → 变 `stale`，且**不**自动跳过
- [ ] **K13** `default_prompt` 为可选字段：缺省合法；`human` / `auto` 节点配置后被忽略不报错；`agent` 节点保存后能被 `context` / `sop_next` 读到

---

## S · 存储与回放（P0）

- [ ] **S1** `events.jsonl` 行数与 UI 中 `seq` 一致，`seq` 严格单调递增
- [ ] **S2** 随机 `kill -9` 进程后重启 → 终态与不中断执行一致（用 fixture 对比）
- [ ] **S3** 快照写入过程中断电（模拟写临时文件后不 rename）→ 旧快照仍可用，不产生半文件
- [ ] **S4** 手工向 `events.jsonl` 追加一行坏 JSON → 启动不崩溃，坏行被跳过并记录行号
- [ ] **S5** `agentsop doctor --repair` → 生成 `events.jsonl.corrupt.<ts>` 备份，恢复可读状态
- [ ] **S6** 属性测试：任意 1000 条随机合法事件，增量执行与全量重放终态一致
- [ ] **S7** `snapshot.json` 缺失 → 从 `events.jsonl` 全量重放成功
- [ ] **S8** 2000 条事件后 `events.jsonl` < 2MB

---

## H · 本地 HTTP 与安全（P0）

前置：`PORT=$(cat ~/.agentsop/run/port)`、`TOKEN=$(cat ~/.agentsop/token)`

- [ ] **H1** `curl -s http://127.0.0.1:$PORT/v1/health | jq .ok` → `true`
- [ ] **H2** 不带 token 调 `POST /v1/decisions` → `401`，无任何状态写入
- [ ] **H3** 错误 token → `401`，且响应体不泄露 token 提示
- [ ] **H4** `Origin: https://evil.com` 的请求 → 拒绝
- [ ] **H5** 300KB 请求体 → `413`
- [ ] **H6** `python3 -m http.server 43117` 占位后启动 App → `run/port` 内容为 `43118`，CLI 仍可达
- [ ] **H7** `GET /v1/events?since=N` → 断线重连不丢事件、不重复（凭 `id` 去重）
- [ ] **H8** 空闲 30s 后 SSE 连接仍存活（收到心跳）
- [ ] **H9** 非 `127.0.0.1` 地址无法连通（`lsof -iTCP -sTCP:LISTEN` 仅 loopback）
- [ ] **H10** `open` 类端点（`reveal_path`）传入 `/etc` → 拒绝且不弹 Finder

---

## C · CLI（P0）

- [ ] **C1** `agentsop status` 无运行 → 友好提示，退出码 `2`
- [ ] **C2** `agentsop status --json | jq .run_id` → 合法 JSON，无杂字符
- [ ] **C3** `agentsop start prd-to-ship --repo .` → 返回 `run_id`，状态 `running`
- [ ] **C4** `agentsop next` → 输出当前节点 id 与标题
- [ ] **C5** `agentsop decide <node> reject`（无 `--note`）→ 退出码 `3`，明确要求备注
- [ ] **C6** `agentsop decide <node> reject --note "缺竞品对比"`（≥8 字）→ 退出码 0，节点重试 +1
- [ ] **C7** `agentsop complete <node> --evidence docs/prd.md` → 节点 `done`
- [ ] **C8** App 未运行 → `agentsop status` 仍可读历史（只读），写操作返回 `SERVICE_DOWN` 退出码 `4`
- [ ] **C9** `agentsop status` 冷启动耗时 < 30ms（`time` 三次取中位）
- [ ] **C10** 所有子命令 `--help` 结构一致，无未实现占位选项

---

## G · Agent 桥接 / Claude Code（P0，首期核心）

- [ ] **G1** `agentsop hooks install claude --dry-run` → 打印 diff，不写文件
- [ ] **G2** `agentsop hooks install claude` → 备份原文件，写入 6 类 Hook 配置（适配器共支持 8 类映射），路径为绝对路径
- [ ] **G3** 连续安装两次 → 配置字节级不变（幂等）
- [ ] **G4** 原 `settings.json` 已有其它 hooks → 安装后仍然完整保留
- [ ] **G5** `agentsop hooks uninstall claude` → 只删 `_agentsop` 条目，无残留键
- [ ] **G6** `agentsop hooks status` → 打印每个 Hook 的安装状态与实测探测结果
- [ ] **G7** 伪造 8 类 Hook stdin JSON → 每类都有单测，未知字段不 panic
- [ ] **G8** App 停止时执行 `echo '{}' | agentsop hook claude; echo $?` → `0`，stdout 为空（fail-open）
- [ ] **G9** Hook stdout **只有** JSON：`echo '{...}' | agentsop hook claude 2>/dev/null | jq .` → 解析成功
- [ ] **G10** 待决策节点下执行 `git commit` → 被 deny，理由包含人的备注原文
- [ ] **G11** 通过该节点后再执行同一命令 → 放行
- [ ] **G12** 决策后新开一轮 prompt → `additionalContext` 含决策内容；无未读时**不注入**
- [ ] **G13** 两个 Claude 会话操作同一节点 → 后者只记证据，面板提示冲突
- [ ] **G14** Hook 端到端 p95 < 25ms（100 次采样，日志统计）
- [ ] **G15** `agentsop doctor --probe` → 打印当前 Claude Code 版本实测支持的 Hook 能力

---

## M · MCP（P1）

- [ ] **M1** `npx @modelcontextprotocol/inspector` 连接 `agentsop-mcp` → `initialize` 成功
- [ ] **M2** `tools/list` 返回 7 个工具，schema 合法
- [ ] **M3** `sop_status` 实际调用 → 返回当前节点、待决策与 `default_prompt`
- [ ] **M4** `sop_context` 返回值与 `context.md` 内容一致
- [ ] **M5** `sop_complete_node` → 节点推进，无 Stop Hook 时也能完成
- [ ] **M6** `sop_request_decision(blocking=true)` 挂起中，人在面板决策 → 立即返回结果
- [ ] **M7** 同上，90s 无人决策 → 返回 `{status:"pending"}`，不报错
- [ ] **M8** 内核未运行 → 工具返回 `SERVICE_DOWN` 结构化错误，MCP 进程不退出
- [ ] **M9** `resources/read agentsop://run/{id}/context` → `text/markdown` 正确
- [ ] **M10** 进程 stdout 无日志污染（日志全走 stderr）
- [ ] **M11** `sop_next` → 返回当前节点 `{id,title,type,default_prompt}` 与下一步；无 `default_prompt` 时字段为空且不报错

---

## D · 桌面壳（P0）

- [ ] **D1** 启动后 Dock 无图标，菜单栏有图标（`LSUIElement` 生效）
- [ ] **D2** 托盘菜单显示待决策数，决策后 1s 内更新
- [ ] **D3** **非激活验证**：终端为前台 → `⌥⌘S` 呼出面板 → 终端**仍是前台应用**（`lsappinfo front` 不变），可直接继续打字
- [ ] **D4** 快捷键到面板首帧 < 100ms（预热后），无白屏
- [ ] **D5** 面板在副屏呼出 → 出现在副屏光标侧，不越界
- [ ] **D6** 失焦自动隐藏；但输入框有草稿时**不**隐藏
- [ ] **D7** `P` 固定后面板失焦不隐藏；再按取消
- [ ] **D8** 全屏 App（Safari 全屏）下呼出 → 面板浮在其上，不切空间
- [ ] **D9** 重复启动 App → 不产生第二个实例，仅呼出面板
- [ ] **D10** 睡眠唤醒后快捷键仍有效、HTTP 仍可达
- [ ] **D11** 快捷键冲突（占一个已用组合）→ 托盘提示冲突，允许改键
- [ ] **D12** 退出 App → 进程完全退出，无残留 HTTP 监听

---

## U · 面板 UI（P0，对应前端设计 §16）

用 fixture 覆盖 8 种节点状态：

- [ ] **U1 / V1** 180 宽下 20 字节点标题单行截断，36px 行高不破
- [ ] **U2 / V2** 五种状态去掉颜色后仍可区分（形状 + 文案）
- [ ] **U3 / V3** 浅/深色文本对比度 ≥ 4.5:1（取色器实测）
- [ ] **U4 / V4** 同高度下状态更新无白闪、无布局抖动
- [ ] **U5 / V5** 720 高度下编辑器与浮层输入框可见，保存/提交无需二次滚动
- [ ] **U6 / V6** 1440×900 与 2560×1440 下右侧垂直居中（右边距 24），不越界
- [ ] **U7** 轨道当前项有 2px 高亮竖条 + `aria-current="step"`
- [ ] **U8** `J/K` 切换节点，`Enter` 开详情，`A` 通过，`R` 打回，`?` 帮助，`Esc` 关闭浮层/隐藏
- [ ] **U9** 中文输入法拼写中按单键 → 不触发快捷键
- [ ] **U10** 打回备注 < 8 字 → 提交按钮禁用，并说明原因
- [ ] **U11** 连点「通过」第二次 → 400ms 内按钮禁用，无重复事件
- [ ] **U12** 网络/内核失败 → 乐观更新回滚 + toast，状态与真实一致
- [ ] **U13** 断连时轨道置灰但保留最后状态，底栏显示重连提示
- [ ] **U14** 无产物节点不隐藏产物区，显示「暂无产物」
- [ ] **U15** `advisory` 徽标在 Codex/TRAE 运行时出现，tooltip 说明无强制力
- [ ] **U16 / V8** `prefers-reduced-motion` 下无位移/缩放动画
- [ ] **U17 / V9** 等待确认/运行中显示「默认 Prompt」卡片（产物之前、复制可用）；点「通过」后下一节点默认 Prompt 立即可见

---

## E · SOP 编辑器（P1）

- [ ] **E1** 无活跃运行时 `⋯ → 编辑流程` 可进入；有活跃运行时禁用并 tooltip 说明
- [ ] **E2** 修改标题 → YAML 预览出现 `+` 行，保存后文件内容正确
- [ ] **E3** 删除被边引用的节点 → 保存被拒并定位错误
- [ ] **E4** 类型切换（人/AI/自动）时相关字段联动出现/隐藏
- [ ] **E5** Agent 下拉**显示标签**（`Claude Code`）而非内部值
- [ ] **E6** 保存非法 YAML → 后端返回行号错误，前端定位到字段
- [ ] **E7** 保存成功后热重载，无需重启 App
- [ ] **E8** 180 宽下表单所有字段可操作，预览区横向滚动正常
- [ ] **E9** 「默认 Prompt」位于 Agent 之后、完成条件之前，多行输入并写入 YAML `default_prompt`；留空不报错

---

## P · 性能（P0）

- [ ] **P1** App 冷启动到托盘可见 < 600ms
- [ ] **P2** 呼出到首帧 < 100ms（连续 20 次中位数）
- [ ] **P3** 隐藏 < 80ms
- [ ] **P4** Hook RTT p95 < 25ms
- [ ] **P5** 决策点击到 UI 反馈 < 100ms
- [ ] **P6** 空闲常驻内存 < 120MB（Activity Monitor，10 分钟后）
- [ ] **P7** 1000 节点状态更新（模拟事件洪峰）→ 面板掉帧 < 5 帧，CPU 峰值 < 30%
- [ ] **P8** 安装包 < 20MB

---

## X · 隐私与安全（P0）

- [ ] **X1** `token` 文件权限 `-rw-------`
- [ ] **X2** 默认配置下 `events.jsonl` 不含 prompt 全文与工具输出全文（抽查 20 条）
- [ ] **X3** 含 `authorization` / `api_key` 字段的 payload → 落盘已遮蔽
- [ ] **X4** 全量网络抓包（`lsof`/`nettop` 观察 10 分钟）→ 无外联请求
- [ ] **X5** 未声明 `action.shell` 的 SOP 无法触发任何本地命令
- [ ] **X6** 首次运行含 `action.shell` 的 SOP → 必须显式确认
- [ ] **X7** 篡改 `~/.agentsop/runs/*/meta.json` 中的路径 → 无法读取仓外文件
- [ ] **X8** 卸载 App → 不删除用户 `~/.agentsop` 数据（仅提示路径）

---

## R · 打包与发布（P0）

- [ ] **R1** `.app` 通过 `codesign --verify --deep --strict` 与 `spctl -a -vv`
- [ ] **R2** 公证成功（`xcrun stapler validate`）
- [ ] **R3** 干净机器（无 Xcode/Node/Rust）双击安装可运行
- [ ] **R4** `/usr/local/bin/agentsop` shim 可执行，且 `agentsop status` 正常
- [ ] **R5** 登录启动项写入成功，重启后自动常驻
- [ ] **R6** 首次启动 3 分钟内可完成「装 hooks → 跑通一个节点」（新人实测，不口头指导）
- [ ] **R7** 卸载脚本/说明可完整移除 LaunchAgent、shim、hooks
- [ ] **R8** 版本号、`Info.plist`、`Cargo.toml`、`package.json` 一致
- [ ] **R9** 发布说明包含「能力矩阵」与「已知问题」

---

## E2E 端到端验收

### E2E-1 · Claude Code 三节点闭环（P0）

准备：

```bash
export AGENTSOP_HOME=/tmp/as-e2e && rm -rf /tmp/as-e2e
agentsop &                                  # 启动 App
agentsop hooks install claude --scope project --dry-run
agentsop hooks install claude --scope project
agentsop start prd-to-ship --repo /tmp/demo-repo
```

步骤与预期：

| # | 操作 | 预期 |
|---|---|---|
| 1 | 在 `/tmp/demo-repo` 启动 Claude Code，输入「按 SOP 开始」 | `SessionStart` 注入当前节点；面板轨道高亮第 1 个节点 |
| 2 | 人在面板点「通过」澄清节点 | 节点 `done`；`context.md` 更新；下一轮 prompt 可见 |
| 3 | 让 Claude 生成 `docs/prd.md` 后结束 | 500ms 内节点 `draft_prd` 变 `done`（或先变「等你确认」） |
| 4 | 面板「打回」并写备注 | 节点回 `active`/`blocked`，`retry=1` |
| 5 | Claude 尝试 `git commit` | 被 deny，理由含备注原文；Claude 改为继续修改 |
| 6 | 修改完成后再次结束 | 节点 `done`，进入 `build` |
| 7 | 面板切到刚进入的 `build` 节点 | 详情立即显示 `build` 默认 Prompt；下一轮 `context.md` / `sop_next` 携带该 Prompt |
| 8 | 用 MCP 调 `sop_request_decision` | 面板出现决策卡；回答后工具返回 |
| 9 | `kill -9` App 后重启 | 状态恢复到步骤 7 之后，未完成节点标 `stale` |
| 10 | `agentsop status --json` | 与面板显示完全一致 |

全部通过 → 首期核心价值成立。

### E2E-2 · 无 Hook 降级（P1）

| # | 操作 | 预期 |
|---|---|---|
| 1 | 卸载 hooks | `agentsop hooks status` 显示未安装 |
| 2 | 启动 Codex / TRAE 完成一步 | 面板不自动推进（符合预期），节点显示 `advisory` |
| 3 | 面板手动「完成此节点」 | 正常推进，流程不中断 |
| 4 | 读取 `.agentsop/context.md` | 包含最新人的决定 |

### E2E-3 · 双会话冲突（P1）

| # | 操作 | 预期 |
|---|---|---|
| 1 | 两个 Claude 会话同时绑定同一 run | 第二个会话触发冲突提示 |
| 2 | 两个会话都报告完成同一节点 | 只迁移一次，另一条记为证据 |
| 3 | 面板显示冲突提示 | 文案可理解，附「查看会话」入口 |

---

## 回归清单（每次发版必跑）

标记 `@smoke` 的为 10 分钟内可完成的最小集：

- [ ] @smoke A1 A3 A4
- [ ] @smoke H1 H2
- [ ] @smoke C2 C3 C5 C6
- [ ] @smoke G2 G8 G10 G12
- [ ] @smoke D1 D3 D4
- [ ] @smoke U8 U10
- [ ] @smoke E2E-1 步骤 1-6
- [ ] P1 P4 P6
- [ ] X1 X2
- [ ] R1 R3 R8

---

## 发布门禁

| 门禁 | 条件 |
|---|---|
| 可进入内测 | A、K、S、H、C 全绿；D1-D9 全绿；E2E-1 步骤 1-6 通过 |
| 可公开发布 | 全部 P0 条目通过；P1 未通过项 ≤ 3 且已登记 `KNOWN_ISSUES.md`；R7 卸载路径验证过 |
| 紧急热修 | 只需 @smoke + 受影响模块；发布说明标注跳过项 |

## 已知问题记录模板

写入仓库 `KNOWN_ISSUES.md`：

```markdown
### KI-<序号> <一句话标题>
- 影响范围：
- 复现步骤：
- 临时绕过：
- 关联 checklist 条目：
- 计划修复里程碑：
```
