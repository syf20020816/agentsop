//! 状态机内核：所有写操作串行经过这里，事件先落盘再改内存。

use crate::context;
use crate::error::{AppError, Result};
use crate::model::*;
use crate::sop::{self, EvalCtx};
use crate::store::Paths;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub struct Kernel {
    paths: Paths,
    runs: HashMap<String, Run>,
    /// 全局会话登记表（session_id → 可读标题/agent），跨运行共享。
    sessions: BTreeMap<String, SessionMeta>,
}

/// 一次 hook 调用的结果：既可能推进状态，也可能只是给 Agent 回一段上下文。
pub struct HookOutcome {
    pub snapshot: Option<Snapshot>,
    pub response: serde_json::Value,
    pub action: String,
}

impl Kernel {
    pub fn boot(paths: Paths) -> Result<Self> {
        paths.ensure()?;
        let mut runs = HashMap::new();
        for run_id in paths.list_runs() {
            if let Ok(Some(run)) = paths.read_run(&run_id) {
                runs.insert(run_id, run);
            }
        }
        let sessions = paths.read_sessions();
        Ok(Kernel { paths, runs, sessions })
    }

    pub fn session_registry(&self) -> &BTreeMap<String, SessionMeta> {
        &self.sessions
    }

    pub fn known_sessions(&self) -> Vec<SessionMeta> {
        let mut all: Vec<SessionMeta> = self.sessions.values().cloned().collect();
        all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        all
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn run_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.runs.keys().cloned().collect();
        ids.sort();
        ids.reverse();
        ids
    }

    pub fn run(&self, run_id: &str) -> Result<&Run> {
        self.runs
            .get(run_id)
            .ok_or_else(|| AppError::not_found(format!("运行「{}」不存在", run_id)))
    }

    pub fn snapshot(&self, run_id: &str) -> Result<Snapshot> {
        Ok(Snapshot::from_run(self.run(run_id)?, &self.sessions))
    }

    pub fn all_snapshots(&self) -> Vec<Snapshot> {
        self.run_ids()
            .iter()
            .filter_map(|id| self.runs.get(id))
            .map(|run| Snapshot::from_run(run, &self.sessions))
            .collect()
    }

    pub fn active_run_id(&self) -> Option<String> {
        let mut ids = self.run_ids();
        ids.retain(|id| {
            self.runs
                .get(id)
                .is_some_and(|r| r.status == RunStatus::Running || r.status == RunStatus::Paused)
        });
        ids.sort_by_key(|id| self.runs.get(id).map(|r| r.created_at.clone()));
        ids.pop()
    }

    pub fn context_md(&self, run_id: &str) -> Result<String> {
        Ok(context::render(self.run(run_id)?))
    }

    // ------------------------------------------------------------ 运行级

    pub fn start_run(
        &mut self,
        sop_name: &str,
        repo: &str,
        vars: BTreeMap<String, String>,
    ) -> Result<Snapshot> {
        self.start_run_bound(sop_name, repo, vars, None, None)
    }

    /// 带上「只用这个 Agent / 这个会话」的启动：面板空态里选定后一次成型。
    pub fn start_run_bound(
        &mut self,
        sop_name: &str,
        repo: &str,
        vars: BTreeMap<String, String>,
        agent: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Snapshot> {
        let sop = self.paths.load_sop(sop_name)?;
        let run_id = format!("run_{}", ulid::Ulid::new().to_string().to_lowercase());
        let ts = now_ts();
        let mut nodes = BTreeMap::new();
        for node in &sop.nodes {
            nodes.insert(node.id.clone(), NodeState::new(&node.id));
        }
        let entry = sop.entry.clone();
        let mut run = Run {
            run_id: run_id.clone(),
            sop,
            repo: repo.to_string(),
            status: RunStatus::Running,
            seq: 0,
            current: None,
            nodes,
            created_at: ts.clone(),
            updated_at: ts.clone(),
            agent: agent.map(|a| a.to_string()),
            sessions: BTreeMap::new(),
            bound_session: session_id.map(|s| s.to_string()),
            last_decision: None,
        };
        if let Some(sid) = session_id {
            run.sessions.insert(sid.to_string(), entry.clone());
        }
        run.sop.vars.extend(vars);

        let created = serde_json::json!({ "sop": run.sop.sop.clone(), "repo": run.repo.clone() });
        emit(&mut run, &self.paths, "run.created", None, created)?;

        enter_node(&mut run, &self.paths, &entry)?;
        self.persist(&mut run)?;
        let snapshot = Snapshot::from_run(&run, &self.sessions);
        self.runs.insert(run_id, run);
        Ok(snapshot)
    }

    /// SOP 编辑保存后调用：把磁盘上的新定义灌进正在跑的运行，面板立刻看到新标题/新 Prompt。
    /// `from` 是运行里记着的名字（改名时是旧文件名），`to` 是刚保存的文件名。
    pub fn reload_sop(&mut self, from: &str, to: &str) -> Result<Vec<String>> {
        let mut fresh = self.paths.load_sop(to)?;
        fresh.sop = to.to_string();
        let paths = self.paths.clone();
        let mut touched = Vec::new();
        for (run_id, run) in self.runs.iter_mut() {
            if !matches!(run.status, RunStatus::Running | RunStatus::Paused) {
                continue;
            }
            if run.sop.sop != from && run.sop.sop != to {
                continue;
            }
            // 运行期写入的变量优先，不被磁盘定义覆盖。
            let vars = std::mem::take(&mut run.sop.vars);
            run.sop = fresh.clone();
            run.sop.vars.extend(vars);

            let ids: Vec<String> = run.sop.nodes.iter().map(|n| n.id.clone()).collect();
            run.nodes.retain(|id, _| ids.contains(id));
            run.sessions.retain(|_, node| ids.contains(node));
            for id in &ids {
                run.nodes.entry(id.clone()).or_insert_with(|| NodeState::new(id));
            }
            // 被删掉的当前节点：退回入口重新开始，避免停在幽灵节点上。
            if let Some(cur) = run.current.clone() {
                if !ids.contains(&cur) {
                    let entry = run.sop.entry.clone();
                    if let Some(state) = run.nodes.get_mut(&entry) {
                        state.status = NodeStatus::Active;
                        state.entered_at = Some(now_ts());
                    }
                    run.current = Some(entry);
                }
            }
            // 正在等人的节点：问题文案跟随最新 Prompt。
            if let Some(cur) = run.current.clone() {
                let question = run.sop.node(&cur).and_then(|s| s.prompt.clone());
                if let Some(state) = run.nodes.get_mut(&cur) {
                    if state.status == NodeStatus::WaitingHuman {
                        state.question = question;
                    }
                }
            }

            let data = serde_json::json!({ "sop": run.sop.sop.clone(), "version": run.sop.version });
            emit(run, &paths, "sop.reloaded", None, data)?;
            persist_run(&paths, run)?;
            touched.push(run_id.clone());
        }
        Ok(touched)
    }

    pub fn pause(&mut self, run_id: &str) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            if run.status != RunStatus::Running {
                return Err(AppError::invalid_transition("当前运行不在 running 状态"));
            }
            run.status = RunStatus::Paused;
            emit(run, paths, "run.paused", None, serde_json::json!({}))
        })
    }

    pub fn resume(&mut self, run_id: &str) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            if run.status != RunStatus::Paused {
                return Err(AppError::invalid_transition("当前运行不在 paused 状态"));
            }
            run.status = RunStatus::Running;
            emit(run, paths, "run.resumed", None, serde_json::json!({}))
        })
    }

    pub fn abort(&mut self, run_id: &str) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            if run.status == RunStatus::Completed || run.status == RunStatus::Aborted {
                return Err(AppError::invalid_transition("运行已结束"));
            }
            run.status = RunStatus::Aborted;
            emit(run, paths, "run.aborted", None, serde_json::json!({}))
        })
    }

    // ------------------------------------------------------------ 节点级

    pub fn complete_node(
        &mut self,
        run_id: &str,
        node_id: &str,
        evidence: Vec<Evidence>,
    ) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            let status = run
                .node_state(node_id)
                .ok_or_else(|| AppError::not_found(format!("节点「{}」不存在", node_id)))?
                .status;
            if !matches!(status, NodeStatus::Active | NodeStatus::WaitingHuman | NodeStatus::Stale)
            {
                return Err(AppError::invalid_transition(format!(
                    "节点「{}」当前状态为 {:?}，不能完成",
                    node_id, status
                )));
            }
            complete_and_advance(run, paths, node_id, evidence, None)
        })
    }

    /// Agent 主动请人决策：把当前节点挂起为 waiting_human，但不推进节点。
    pub fn request_decision(
        &mut self,
        run_id: &str,
        node_id: Option<&str>,
        question: &str,
        options: Option<Vec<String>>,
    ) -> Result<Snapshot> {
        let text = question.trim();
        if text.is_empty() {
            return Err(AppError::new("INVALID_ARGUMENT", "question 不能为空"));
        }
        let question = match options.filter(|o| !o.is_empty()) {
            Some(opts) => format!("{}\n选项：{}", text, opts.join(" / ")),
            None => text.to_string(),
        };
        self.mutate(run_id, |run, paths| {
            if run.status != RunStatus::Running {
                return Err(AppError::invalid_transition("运行不在运行中，无法请求决策"));
            }
            let target = node_id
                .map(|s| s.to_string())
                .or_else(|| run.current.clone())
                .ok_or_else(|| AppError::invalid_transition("没有当前节点"))?;
            let state = run
                .node_state(&target)
                .cloned()
                .ok_or_else(|| AppError::not_found(format!("节点「{}」不存在", target)))?;
            let already_halted = state.halted && state.status == NodeStatus::WaitingHuman;
            if !already_halted && state.status != NodeStatus::Active {
                return Err(AppError::invalid_transition(format!(
                    "节点「{}」当前状态为 {:?}，不能请求决策",
                    target, state.status
                )));
            }
            if let Some(st) = run.nodes.get_mut(&target) {
                st.status = NodeStatus::WaitingHuman;
                st.halted = true;
                st.question = Some(question.clone());
                st.pending_since = Some(now_ts());
            }
            run.current = Some(target.clone());
            emit(
                run,
                paths,
                "decision.requested",
                Some(&target),
                serde_json::json!({ "question": question }),
            )
        })
    }

    pub fn decide(
        &mut self,
        run_id: &str,
        node_id: &str,
        action: DecisionAction,
        note: &str,
    ) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            let state = run
                .node_state(node_id)
                .cloned()
                .ok_or_else(|| AppError::not_found(format!("节点「{}」不存在", node_id)))?;

            if action == DecisionAction::Reject && note.trim().chars().count() < 8 {
                return Err(AppError::new(
                    "INVALID_ARGUMENT",
                    "打回必须写清楚要改什么（至少 8 个字）",
                ));
            }

            match action {
                DecisionAction::Approve => {
                    if state.status != NodeStatus::WaitingHuman {
                        return Err(AppError::invalid_transition("节点不在等待确认状态"));
                    }
                    record_decision(run, paths, node_id, action, note)?;
                    if state.halted {
                        // Agent 主动提问：回答后回到 active，让 Agent 继续本轮工作。
                        return resume_after_halt(run, paths, node_id);
                    }
                    complete_and_advance(run, paths, node_id, Vec::new(), Some(note.to_string()))
                }
                DecisionAction::Reject => {
                    if state.status != NodeStatus::WaitingHuman {
                        return Err(AppError::invalid_transition("节点不在等待确认状态"));
                    }
                    record_decision(run, paths, node_id, action, note)?;
                    if state.halted {
                        return resume_after_halt(run, paths, node_id);
                    }
                    retry_node(run, paths, node_id)
                }
                DecisionAction::Retry => {
                    if !matches!(state.status, NodeStatus::Blocked | NodeStatus::Stale) {
                        return Err(AppError::invalid_transition("只有阻塞或超时的节点可以重试"));
                    }
                    record_decision(run, paths, node_id, action, note)?;
                    retry_node(run, paths, node_id)
                }
                DecisionAction::AllowOnce => {
                    if state.status != NodeStatus::Blocked {
                        return Err(AppError::invalid_transition("只有阻塞节点可以放行一次"));
                    }
                    record_decision(run, paths, node_id, action, note)?;
                    // 放行一次不改变节点语义，只把状态恢复到可继续运行。
                    let target = if spec_kind(run, node_id) == NodeType::Human {
                        NodeStatus::WaitingHuman
                    } else {
                        NodeStatus::Active
                    };
                    set_status(run, node_id, target, paths)
                }
                DecisionAction::Abort => {
                    record_decision(run, paths, node_id, action, note)?;
                    run.status = RunStatus::Aborted;
                    emit(run, paths, "run.aborted", Some(node_id), serde_json::json!({}))
                }
            }
        })
    }

    pub fn attach(&mut self, run_id: &str, session_id: &str, node_id: Option<&str>) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            let target = node_id
                .map(|s| s.to_string())
                .or_else(|| run.current.clone())
                .ok_or_else(|| AppError::invalid_transition("没有可绑定的当前节点"))?;
            run.sessions.insert(session_id.to_string(), target.clone());
            // 首个接入的会话自动成为这次运行的绑定会话。
            if run.bound_session.is_none() {
                run.bound_session = Some(session_id.to_string());
            }
            emit(
                run,
                paths,
                "session.attached",
                Some(&target),
                serde_json::json!({ "session_id": session_id }),
            )
        })
    }

    /// 手动把运行绑定到某个会话，切换后只有该会话的 hook 能驱动。
    /// 只有还没开始推进的运行可以改绑；一旦开始就锁定。
    pub fn bind_session(&mut self, run_id: &str, session_id: &str) -> Result<Snapshot> {
        self.mutate(run_id, |run, paths| {
            if run.sessions_locked() {
                return Err(AppError::invalid_transition(
                    "SOP 已经开始推进，绑定会话不能再改；如需换任务请用「+」新建一次运行",
                ));
            }
            if session_id.is_empty() {
                run.bound_session = None;
                return emit(run, paths, "session.unbound", None, serde_json::json!({}));
            }
            let target = run.current.clone().unwrap_or_default();
            if !target.is_empty() {
                run.sessions.insert(session_id.to_string(), target.clone());
            }
            run.bound_session = Some(session_id.to_string());
            emit(
                run,
                paths,
                "session.bound",
                if target.is_empty() { None } else { Some(&target) },
                serde_json::json!({ "session_id": session_id }),
            )
        })
    }

    /// 标记所有超时未更新的活跃节点。
    pub fn sweep_stale(&mut self, limit_minutes: i64) -> Result<Vec<Snapshot>> {
        let threshold = chrono::Utc::now() - chrono::Duration::minutes(limit_minutes);
        let mut changed = Vec::new();
        let mut candidates: Vec<(String, String, String)> = Vec::new();
        for run_id in self.run_ids() {
            let Some(run) = self.runs.get(&run_id) else { continue };
            let Some(current) = run.current.clone() else { continue };
            let Some(state) = run.node_state(&current) else { continue };
            if !matches!(state.status, NodeStatus::Active | NodeStatus::WaitingHuman) {
                continue;
            }
            let Some(entered) = state.entered_at.clone() else { continue };
            let Ok(entered_at) = chrono::DateTime::parse_from_rfc3339(&entered) else {
                continue;
            };
            if entered_at.with_timezone(&chrono::Utc) > threshold {
                continue;
            }
            candidates.push((run_id, current, entered));
        }

        for (run_id, current, entered) in candidates {
            let snap = self.mutate(&run_id, |run, paths| {
                set_status(run, &current, NodeStatus::Stale, paths)?;
                emit(
                    run,
                    paths,
                    "node.stale",
                    Some(&current),
                    serde_json::json!({ "entered_at": entered }),
                )
            })?;
            changed.push(snap);
        }
        Ok(changed)
    }

    // ------------------------------------------------------------ Hook

    pub fn hook(&mut self, body: serde_json::Value) -> Result<HookOutcome> {
        let source = body
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("generic")
            .to_string();
        // Claude Code 实际字段为 hook_event_name；hook_event 保留给通用/自定义来源。
        let hook_event = body
            .get("hook_event_name")
            .or_else(|| body.get("hook_event"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let session_id = body
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // 任何来源的 hook 都登记会话：这样即使还没启动 SOP，也能在下拉里按标题挑一个绑定。
        if let Some(sid) = session_id.as_deref() {
            self.touch_session(sid, &source, &body);
        }

        let run_id = self
            .resolve_run(body.get("run_id").and_then(|v| v.as_str()), session_id.as_deref())
            .ok_or_else(|| AppError::not_found("没有活跃运行"))?;

        // 运行只认创建时选定的 Agent：别的 Agent 的 hook 一律不推进。
        let agent_mismatch = self
            .runs
            .get(&run_id)
            .and_then(|run| run.agent.as_deref())
            .is_some_and(|wanted| normalize_agent(wanted) != normalize_agent(&source));
        if agent_mismatch {
            return Ok(HookOutcome {
                snapshot: self.snapshot(&run_id).ok(),
                response: serde_json::json!({ "continue": true }),
                action: "agent.ignored".into(),
            });
        }

        // 还没有绑定会话时，第一条带 session_id 的事件直接接管。
        if let Some(sid) = session_id.clone() {
            let unbound = self
                .runs
                .get(&run_id)
                .map(|run| run.bound_session.is_none())
                .unwrap_or(false);
            if unbound {
                let _ = self.attach(&run_id, &sid, None);
            }
        }

        // 只让绑定会话驱动这次运行，避免多个对话互相推进同一个节点。
        if let (Some(sid), Some(bound)) = (
            session_id.as_deref(),
            self.runs
                .get(&run_id)
                .and_then(|run| run.bound_session.as_deref()),
        ) {
            if sid != bound {
                // 不改状态，但把快照带上，让面板刷新会话下拉（新任务才认得出来）。
                return Ok(HookOutcome {
                    snapshot: self.snapshot(&run_id).ok(),
                    response: serde_json::json!({ "continue": true }),
                    action: "session.ignored".into(),
                });
            }
        }

        match hook_event.as_str() {
            "SessionStart" | "UserPromptSubmit" => {
                if let Some(sid) = &session_id {
                    let _ = self.attach(&run_id, sid, None);
                }
                let ctx = self.context_md(&run_id)?;
                let name = if hook_event == "SessionStart" {
                    "SessionStart"
                } else {
                    "UserPromptSubmit"
                };
                Ok(HookOutcome {
                    snapshot: Some(self.snapshot(&run_id)?),
                    response: serde_json::json!({
                        "continue": true,
                        "hookSpecificOutput": {
                            "hookEventName": name,
                            "additionalContext": ctx,
                        }
                    }),
                    action: "context".into(),
                })
            }
            "PreToolUse" => {
                let tool = body
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let command = body
                    .get("tool_input")
                    .and_then(|v| v.get("command"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let verdict = self.guard(&run_id, &tool, &command)?;
                Ok(HookOutcome {
                    snapshot: None,
                    action: verdict.0,
                    response: verdict.1,
                })
            }
            "PostToolUse" => {
                let label = body
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let snap = self.mutate(&run_id, |run, paths| {
                    let node_id = run.current.clone().unwrap_or_default();
                    if let Some(state) = run.nodes.get_mut(&node_id) {
                        state.evidence.push(Evidence {
                            kind: "tool".into(),
                            label,
                            path: None,
                            ts: now_ts(),
                        });
                    }
                    let _ = source;
                    emit(
                        run,
                        paths,
                        "node.evidence",
                        Some(&node_id),
                        serde_json::json!({ "kind": "tool" }),
                    )
                })?;
                Ok(HookOutcome {
                    snapshot: Some(snap),
                    response: serde_json::json!({ "continue": true }),
                    action: "evidence".into(),
                })
            }
            "Stop" | "SubagentStop" => {
                let outcome = self.on_stop(&run_id)?;
                Ok(outcome)
            }
            other => Ok(HookOutcome {
                snapshot: None,
                response: serde_json::json!({ "continue": true }),
                action: format!("ignored:{}", other),
            }),
        }
    }

    fn resolve_run(&self, explicit: Option<&str>, session: Option<&str>) -> Option<String> {
        if let Some(id) = explicit {
            if self.runs.contains_key(id) {
                return Some(id.to_string());
            }
        }
        if let Some(sid) = session {
            if let Some((id, _)) = self
                .runs
                .iter()
                .find(|(_, run)| run.sessions.keys().any(|k| k == sid))
            {
                return Some(id.clone());
            }
        }
        self.active_run_id()
    }

    fn on_stop(&mut self, run_id: &str) -> Result<HookOutcome> {
        let run = self.run(run_id)?;
        let Some(node_id) = run.current.clone() else {
            return Ok(HookOutcome {
                snapshot: None,
                response: serde_json::json!({ "continue": true }),
                action: "no-current".into(),
            });
        };
        let Some(state) = run.node_state(&node_id) else {
            return Ok(HookOutcome {
                snapshot: None,
                response: serde_json::json!({ "continue": true }),
                action: "no-current".into(),
            });
        };
        if state.status != NodeStatus::Active {
            return Ok(HookOutcome {
                snapshot: None,
                response: serde_json::json!({ "continue": true }),
                action: "not-active".into(),
            });
        }
        let spec = run.node_spec(&node_id).cloned();
        let events = spec
            .as_ref()
            .and_then(|s| s.done.as_ref())
            .map(|d| d.events.clone())
            .unwrap_or_default();
        if !events.iter().any(|e| e == "Stop" || e == "SubagentStop") {
            return Ok(HookOutcome {
                snapshot: None,
                response: serde_json::json!({ "continue": true }),
                action: "not-in-whitelist".into(),
            });
        }
        let artifacts = spec
            .as_ref()
            .and_then(|s| s.done.as_ref())
            .map(|d| d.artifacts.clone())
            .unwrap_or_default();
        let repo = Path::new(&run.repo);
        let missing: Vec<String> = artifacts
            .iter()
            .filter(|a| !repo.join(a.as_str()).exists())
            .cloned()
            .collect();
        if !missing.is_empty() {
            let evidence: Vec<Evidence> = missing
                .iter()
                .map(|m| Evidence {
                    kind: "artifact".into(),
                    label: format!("缺少产物 {}", m),
                    path: Some(m.clone()),
                    ts: now_ts(),
                })
                .collect();
            let snap = self.mutate(run_id, |run, paths| {
                if let Some(st) = run.nodes.get_mut(&node_id) {
                    st.evidence.extend(evidence.clone());
                }
                emit(
                    run,
                    paths,
                    "node.evidence",
                    Some(&node_id),
                    serde_json::json!({ "missing": missing }),
                )
            })?;
            return Ok(HookOutcome {
                snapshot: Some(snap),
                response: serde_json::json!({ "continue": true }),
                action: "artifact-missing".into(),
            });
        }
        let snap = self.complete_node(run_id, &node_id, Vec::new())?;
        Ok(HookOutcome {
            snapshot: Some(snap),
            response: serde_json::json!({ "continue": true }),
            action: "completed".into(),
        })
    }

    /// PreToolUse 拦截：当前节点在等人确认且 blocking=true 时拒绝写操作。
    fn guard(
        &self,
        run_id: &str,
        tool_name: &str,
        command: &str,
    ) -> Result<(String, serde_json::Value)> {
        let run = self.run(run_id)?;
        let Some(node_id) = run.current.clone() else {
            return Ok(allow());
        };
        let status = run
            .node_state(&node_id)
            .map(|s| s.status)
            .unwrap_or(NodeStatus::Pending);
        let spec = run.node_spec(&node_id);
        let blocking = spec.map(|s| s.blocking).unwrap_or(false);
        if status != NodeStatus::WaitingHuman || !blocking {
            return Ok(allow());
        }
        let deny = spec
            .and_then(|s| s.guard.as_ref())
            .map(|g| g.deny.clone())
            .unwrap_or_default();
        let hit = deny.iter().any(|pattern| matches_tool(pattern, tool_name, command));
        if !hit {
            return Ok(allow());
        }
        let reason = format!(
            "AgentSOP：节点「{}」正在等待人工确认，已拦截 {}。请在面板确认后再试。",
            spec.map(|s| s.title_or_id()).unwrap_or(&node_id),
            tool_name
        );
        Ok((
            "deny".into(),
            serde_json::json!({
                "continue": true,
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            }),
        ))
    }

    // ------------------------------------------------------------ 内部

    /// 把 hook 上报的会话写进全局登记表；标题取首个 prompt 的第一行。
    fn touch_session(&mut self, session_id: &str, source: &str, body: &serde_json::Value) {
        let text = |key: &str| body.get(key).and_then(|v| v.as_str()).unwrap_or("");
        let prompt = text("prompt");
        let repo = text("cwd");
        // 优先用首个 prompt 当标题；只有 SessionStart 时退化为仓库名。
        let mut title = summarize(&prompt);
        if title.is_empty() && !repo.is_empty() {
            title = repo
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
        }
        let agent = normalize_agent(source);
        let ts = now_ts();
        let entry = self
            .sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionMeta {
                id: session_id.to_string(),
                created_at: ts.clone(),
                updated_at: ts.clone(),
                ..Default::default()
            });
        if entry.title.is_empty() && !title.is_empty() {
            entry.title = title;
        }
        if entry.agent.is_empty() && !agent.is_empty() && agent != "generic" {
            entry.agent = agent.to_string();
        }
        if entry.repo.is_empty() && !repo.is_empty() {
            entry.repo = repo.to_string();
        }
        entry.updated_at = ts;
        let _ = self.paths.write_sessions(&self.sessions);
    }

    fn mutate<F>(&mut self, run_id: &str, f: F) -> Result<Snapshot>
    where
        F: FnOnce(&mut Run, &Paths) -> Result<()>,
    {
        let paths = self.paths.clone();
        let mut run = self
            .runs
            .get(run_id)
            .cloned()
            .ok_or_else(|| AppError::not_found(format!("运行「{}」不存在", run_id)))?;
        f(&mut run, &paths)?;
        self.persist(&mut run)?;
        let snapshot = Snapshot::from_run(&run, &self.sessions);
        self.runs.insert(run_id.to_string(), run);
        Ok(snapshot)
    }

    fn persist(&self, run: &mut Run) -> Result<()> {
        persist_run(&self.paths, run)
    }
}

fn persist_run(paths: &Paths, run: &mut Run) -> Result<()> {
    paths.write_snapshot(run)?;
    let md = context::render(run);
    paths.write_context(&run.run_id, &md)?;
    Ok(())
}

// ---------------------------------------------------------------- 迁移原语

/// hook 上报的 source 与 SOP 里的 agent 名对齐。
fn normalize_agent(name: &str) -> &str {
    match name {
        "claude" => "claude-code",
        other => other,
    }
}

/// 取第一行非空文本、压平空白，最多 40 字符——够在 180px 面板里认出是哪个任务。
fn summarize(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let flat = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 40 {
        flat.chars().take(39).collect::<String>() + "…"
    } else {
        flat
    }
}

fn emit(
    run: &mut Run,
    paths: &Paths,
    kind: &str,
    node_id: Option<&str>,
    data: serde_json::Value,
) -> Result<()> {
    run.seq += 1;
    let ts = now_ts();
    run.updated_at = ts.clone();
    let event = Event {
        seq: run.seq,
        run_id: run.run_id.clone(),
        kind: kind.to_string(),
        node_id: node_id.map(|s| s.to_string()),
        ts,
        data,
    };
    paths.append_event(&event)
}

fn spec_kind(run: &Run, node_id: &str) -> NodeType {
    run.node_spec(node_id)
        .map(|s| s.kind)
        .unwrap_or(NodeType::Agent)
}

fn set_status(run: &mut Run, node_id: &str, status: NodeStatus, paths: &Paths) -> Result<()> {
    if let Some(state) = run.nodes.get_mut(node_id) {
        state.status = status;
        state.halted = false;
        if matches!(status, NodeStatus::Active | NodeStatus::WaitingHuman) {
            state.entered_at = Some(now_ts());
        }
        if status != NodeStatus::WaitingHuman {
            state.pending_since = None;
            state.question = None;
        }
    }
    run.current = Some(node_id.to_string());
    emit(run, paths, "node.entered", Some(node_id), serde_json::json!({ "status": status }))
}

/// Agent 主动提问被回答后，节点回到 active 继续执行，不推进到下一节点。
fn resume_after_halt(run: &mut Run, paths: &Paths, node_id: &str) -> Result<()> {
    if let Some(state) = run.nodes.get_mut(node_id) {
        state.status = NodeStatus::Active;
        state.halted = false;
        state.question = None;
        state.pending_since = None;
    }
    run.current = Some(node_id.to_string());
    emit(run, paths, "node.resumed", Some(node_id), serde_json::json!({}))
}

fn enter_node(run: &mut Run, paths: &Paths, node_id: &str) -> Result<()> {
    let kind = spec_kind(run, node_id);
    let spec = run.node_spec(node_id).cloned();
    let status = if kind == NodeType::Human {
        NodeStatus::WaitingHuman
    } else {
        NodeStatus::Active
    };
    if let Some(state) = run.nodes.get_mut(node_id) {
        state.status = status;
        state.entered_at = Some(now_ts());
        if status == NodeStatus::WaitingHuman {
            state.pending_since = Some(now_ts());
            state.question = spec.as_ref().and_then(|s| s.prompt.clone());
        }
    }
    run.current = Some(node_id.to_string());
    emit(run, paths, "node.entered", Some(node_id), serde_json::json!({ "status": status }))
}

fn complete_and_advance(
    run: &mut Run,
    paths: &Paths,
    node_id: &str,
    evidence: Vec<Evidence>,
    note: Option<String>,
) -> Result<()> {
    let spec = run.node_spec(node_id).cloned();
    let was_active = run
        .node_state(node_id)
        .map(|s| s.status == NodeStatus::Active)
        .unwrap_or(false);

    // review: true 的 agent 节点：AI 干完后先停在等人工确认，不直接进入下一节点。
    if was_active && spec.as_ref().and_then(|s| s.review) == Some(true) {
        if let Some(state) = run.nodes.get_mut(node_id) {
            state.evidence.extend(evidence);
            state.status = NodeStatus::WaitingHuman;
            state.pending_since = Some(now_ts());
            state.question = spec.as_ref().and_then(|s| s.prompt.clone()).or_else(|| {
                Some(format!(
                    "「{}」已完成，是否通过？",
                    spec.as_ref().map(|s| s.title_or_id()).unwrap_or(node_id)
                ))
            });
        }
        emit(
            run,
            paths,
            "node.awaiting_review",
            Some(node_id),
            serde_json::json!({ "note": note }),
        )?;
        return Ok(());
    }

    if let Some(state) = run.nodes.get_mut(node_id) {
        state.evidence.extend(evidence);
        state.status = NodeStatus::Done;
        state.completed_at = Some(now_ts());
        state.pending_since = None;
        state.question = None;
    }
    emit(
        run,
        paths,
        "node.completed",
        Some(node_id),
        serde_json::json!({ "note": note }),
    )?;

    match next_target(run, node_id) {
        Some(next) => enter_node(run, paths, &next),
        None => {
            run.status = RunStatus::Completed;
            run.current = None;
            emit(run, paths, "run.completed", None, serde_json::json!({}))
        }
    }
}

fn retry_node(run: &mut Run, paths: &Paths, node_id: &str) -> Result<()> {
    let retries = run.nodes.get(node_id).map(|s| s.retries).unwrap_or(0) + 1;
    let max = run
        .sop
        .edges
        .iter()
        .find(|e| e.from == node_id && e.kind == EdgeKind::Retry)
        .and_then(|e| e.max)
        .unwrap_or(3);

    let kind = spec_kind(run, node_id);
    if let Some(state) = run.nodes.get_mut(node_id) {
        state.retries = retries;
        state.status = if kind == NodeType::Human {
            NodeStatus::WaitingHuman
        } else {
            NodeStatus::Active
        };
        state.entered_at = Some(now_ts());
        state.pending_since = if state.status == NodeStatus::WaitingHuman {
            Some(now_ts())
        } else {
            None
        };
        state.completed_at = None;
    }
    run.current = Some(node_id.to_string());
    emit(
        run,
        paths,
        "node.retried",
        Some(node_id),
        serde_json::json!({ "retries": retries }),
    )?;

    if retries > max {
        if let Some(state) = run.nodes.get_mut(node_id) {
            state.status = NodeStatus::Blocked;
        }
        emit(
            run,
            paths,
            "node.blocked",
            Some(node_id),
            serde_json::json!({ "reason": "重试次数超限" }),
        )?;
    }
    Ok(())
}

fn record_decision(
    run: &mut Run,
    paths: &Paths,
    node_id: &str,
    action: DecisionAction,
    note: &str,
) -> Result<()> {
    let ts = now_ts();
    run.last_decision = Some(LastDecision {
        node_id: node_id.to_string(),
        action,
        note: note.to_string(),
        ts: ts.clone(),
    });
    emit(
        run,
        paths,
        "decision.recorded",
        Some(node_id),
        serde_json::json!({ "action": action, "note": note }),
    )
}

fn next_target(run: &Run, from: &str) -> Option<String> {
    let ctx = EvalCtx {
        last_decision: run.last_decision.as_ref().map(|d| d.action),
        nodes: &run.nodes,
    };
    run.sop
        .edges
        .iter()
        .filter(|e| e.from == from && e.kind == EdgeKind::Normal)
        .find(|e| sop::eval_when(e.when.as_deref().unwrap_or(""), &ctx))
        .map(|e| e.to.clone())
}

fn allow() -> (String, serde_json::Value) {
    (
        "allow".into(),
        serde_json::json!({
            "continue": true,
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
            }
        }),
    )
}

/// 各 Agent 对 shell 工具的命名不同，Guard 需把它们视为同一类。
fn is_shell_tool(name: &str) -> bool {
    matches!(
        name,
        "Bash" | "RunCommand" | "Shell" | "exec_command" | "bash"
    )
}

fn same_tool(a: &str, b: &str) -> bool {
    a == b || (is_shell_tool(a) && is_shell_tool(b))
}

/// 极简工具匹配：支持 `Bash(git commit:*)` 与 `*` 通配。
fn matches_tool(pattern: &str, tool: &str, command: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some(open) = pattern.find('(') else {
        return same_tool(pattern, tool);
    };
    let tool_part = &pattern[..open];
    let arg_part = pattern[open + 1..].trim_end_matches(')');
    if tool_part != "*" && !same_tool(tool_part, tool) {
        return false;
    }
    let needle = arg_part.trim_end_matches(":*");
    if needle.is_empty() || needle == "*" {
        return true;
    }
    command.contains(needle)
}
