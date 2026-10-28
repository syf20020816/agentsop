//! SOP 定义与运行期数据模型。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    Human,
    Agent,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending,
    Ready,
    Active,
    WaitingHuman,
    Done,
    Blocked,
    Skipped,
    Stale,
}

impl NodeStatus {
    pub fn is_open(self) -> bool {
        matches!(
            self,
            NodeStatus::Ready | NodeStatus::Active | NodeStatus::WaitingHuman | NodeStatus::Stale
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Paused,
    Completed,
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    Approve,
    Reject,
    Retry,
    Abort,
    AllowOnce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Normal,
    Retry,
}

impl Default for EdgeKind {
    fn default() -> Self {
        EdgeKind::Normal
    }
}

// ---------------------------------------------------------------- SOP 定义

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sop {
    pub sop: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    pub entry: String,
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
}

impl Sop {
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.sop)
    }

    pub fn node(&self, id: &str) -> Option<&NodeSpec> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_index(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSpec {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: NodeType,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// 人力节点的说明文字（也兼容旧字段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// 节点轮转到此时自动填入的 Prompt。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<DoneRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<GuardRule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextSpec>,
}

impl NodeSpec {
    pub fn title_or_id(&self) -> &str {
        if self.title.is_empty() {
            &self.id
        } else {
            &self.title
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoneRule {
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuardRule {
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionSpec {
    #[serde(default)]
    pub shell: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextSpec {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub extra: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeSpec {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default)]
    pub kind: EdgeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
}

// ---------------------------------------------------------------- 运行期

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeState {
    pub id: String,
    pub status: NodeStatus,
    #[serde(default)]
    pub retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entered_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Agent 主动请求人决策时置 true；通过/打回后回到 active，不推进节点。
    #[serde(default)]
    pub halted: bool,
}

impl NodeState {
    pub fn new(id: &str) -> Self {
        NodeState {
            id: id.to_string(),
            status: NodeStatus::Pending,
            retries: 0,
            entered_at: None,
            completed_at: None,
            evidence: Vec::new(),
            pending_since: None,
            question: None,
            halted: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub run_id: String,
    /// 完整 SOP 快照，保证回放时不依赖磁盘上的最新定义。
    pub sop: Sop,
    pub repo: String,
    pub status: RunStatus,
    pub seq: u64,
    pub current: Option<String>,
    pub nodes: BTreeMap<String, NodeState>,
    pub created_at: String,
    pub updated_at: String,
    /// 本次运行只在这个 Agent 里跑（创建时选择，覆盖节点自带的 agent）。
    #[serde(default)]
    pub agent: Option<String>,
    /// session_id → node_id
    #[serde(default)]
    pub sessions: BTreeMap<String, String>,
    /// 绑定的会话：只有该 session 的 hook 能驱动这次运行；为空时首个会话自动绑定。
    #[serde(default)]
    pub bound_session: Option<String>,
    #[serde(default)]
    pub last_decision: Option<LastDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastDecision {
    pub node_id: String,
    pub action: DecisionAction,
    #[serde(default)]
    pub note: String,
    pub ts: String,
}

impl Run {
    pub fn node_state(&self, id: &str) -> Option<&NodeState> {
        self.nodes.get(id)
    }

    pub fn node_spec(&self, id: &str) -> Option<&NodeSpec> {
        self.sop.node(id)
    }

    /// 已经开始推进：当前节点不再是入口，或已有节点完成/等人/被拦/超时/留下证据。
    /// 一旦开始，绑定会话就锁定。
    pub fn sessions_locked(&self) -> bool {
        if self.current.as_deref() != Some(self.sop.entry.as_str()) {
            return true;
        }
        self.nodes.values().any(|s| {
            !s.evidence.is_empty()
                || matches!(
                    s.status,
                    NodeStatus::Done
                        | NodeStatus::WaitingHuman
                        | NodeStatus::Blocked
                        | NodeStatus::Stale
                )
        })
    }
}

// ---------------------------------------------------------------- 前端快照

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopInfo {
    pub name: String,
    pub title: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingDecision {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    pub since: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub idx: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: NodeType,
    pub status: NodeStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entered_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub evidence: Vec<Evidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_decision: Option<PendingDecision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub blocking: bool,
    pub blocking_capable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeView {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub run_id: String,
    pub sop: SopInfo,
    pub run_status: RunStatus,
    pub seq: u64,
    pub current_node: Option<String>,
    pub nodes: Vec<NodeView>,
    pub edges: Vec<EdgeView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_decision: Option<LastDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_session: Option<String>,
    #[serde(default)]
    pub sessions: Vec<SessionView>,
    /// 本次运行选定的 Agent（覆盖节点自带的 agent）。
    #[serde(default)]
    pub agent: Option<String>,
    /// 全局见过的会话（跨运行），供“还没开始”时挑一个绑定。
    #[serde(default)]
    pub known_sessions: Vec<SessionMeta>,
    /// 运行已经开始推进时，绑定会话锁定、不能再改。
    #[serde(default)]
    pub session_locked: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    pub node_id: String,
    #[serde(default)]
    pub bound: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent: String,
}

/// 全局会话登记：hook 上报过的会话，用首个 prompt 做可读标题。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub repo: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Snapshot {
    pub fn from_run(run: &Run, known: &std::collections::BTreeMap<String, SessionMeta>) -> Self {
        let nodes = run
            .sop
            .nodes
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                let st = run.nodes.get(&spec.id).cloned().unwrap_or_else(|| NodeState::new(&spec.id));
                let pending = if st.status == NodeStatus::WaitingHuman {
                    Some(PendingDecision {
                        question: st
                            .question
                            .clone()
                            .or_else(|| spec.prompt.clone())
                            .unwrap_or_else(|| "确认是否通过".to_string()),
                        options: None,
                        since: st.pending_since.clone().unwrap_or_else(|| run.updated_at.clone()),
                    })
                } else {
                    None
                };
                let agent = run.agent.clone().or_else(|| spec.agent.clone());
                NodeView {
                    id: spec.id.clone(),
                    idx: format!("{:02}", i + 1),
                    title: spec.title_or_id().to_string(),
                    kind: spec.kind,
                    status: st.status,
                    agent: agent.clone(),
                    retries: st.retries,
                    entered_at: st.entered_at.clone(),
                    completed_at: st.completed_at.clone(),
                    evidence: st.evidence.clone(),
                    pending_decision: pending,
                    default_prompt: spec.default_prompt.clone(),
                    prompt: spec.prompt.clone(),
                    blocking: spec.blocking,
                    blocking_capable: matches!(
                        agent.as_deref(),
                        Some("claude-code") | Some("codex")
                    ),
                }
            })
            .collect();

        Snapshot {
            run_id: run.run_id.clone(),
            sop: SopInfo {
                name: run.sop.sop.clone(),
                title: run.sop.display_title().to_string(),
                version: run.sop.version,
            },
            run_status: run.status,
            seq: run.seq,
            current_node: run.current.clone(),
            nodes,
            edges: run
                .sop
                .edges
                .iter()
                .map(|e| EdgeView {
                    from: e.from.clone(),
                    to: e.to.clone(),
                    kind: e.kind,
                })
                .collect(),
            last_decision: run.last_decision.clone(),
            bound_session: run.bound_session.clone(),
            sessions: run
                .sessions
                .iter()
                .map(|(id, node_id)| SessionView {
                    id: id.clone(),
                    node_id: node_id.clone(),
                    bound: run.bound_session.as_deref() == Some(id.as_str()),
                    title: known.get(id).map(|m| m.title.clone()).unwrap_or_default(),
                    agent: known.get(id).map(|m| m.agent.clone()).unwrap_or_default(),
                })
                .collect(),
            agent: run.agent.clone(),
            known_sessions: {
                let mut all: Vec<SessionMeta> = known.values().cloned().collect();
                all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                all.truncate(12);
                all
            },
            session_locked: run.sessions_locked(),
            updated_at: run.updated_at.clone(),
        }
    }
}

// ---------------------------------------------------------------- 事件

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub run_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub ts: String,
    #[serde(default)]
    pub data: serde_json::Value,
}
