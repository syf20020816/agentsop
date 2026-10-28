//! SOP YAML 解析、校验与 `when` 求值。

use crate::error::{AppError, Result};
use crate::model::{DecisionAction, EdgeKind, NodeState, NodeStatus, Sop};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub const DEFAULT_SOP_NAME: &str = "prd-to-ship";

pub fn parse(yaml: &str) -> Result<Sop> {
    let sop: Sop = serde_yaml::from_str(yaml).map_err(|e| {
        let mut err = AppError::sop(format!("YAML 解析失败：{}", e));
        if let Some(loc) = e.location() {
            err.line = Some(loc.line());
        }
        err
    })?;
    validate(&sop)?;
    Ok(sop)
}

pub fn validate(sop: &Sop) -> Result<()> {
    if sop.sop.trim().is_empty() {
        return Err(AppError::sop("缺少 sop 名称"));
    }
    if sop.version == 0 || sop.version > 1 {
        return Err(AppError::sop(format!(
            "不支持的 schema 版本 {}，当前内核支持 1",
            sop.version
        )));
    }
    if sop.nodes.is_empty() {
        return Err(AppError::sop("nodes 不能为空"));
    }

    let mut ids = HashSet::new();
    for node in &sop.nodes {
        if !valid_id(&node.id) {
            return Err(AppError::sop(format!(
                "节点 id「{}」不合法，需满足 ^[a-z][a-z0-9_]{{1,39}}$",
                node.id
            )));
        }
        if !ids.insert(node.id.as_str()) {
            return Err(AppError::sop(format!("节点 id「{}」重复", node.id)));
        }
        if let Some(done) = &node.done {
            for artifact in &done.artifacts {
                if artifact.starts_with('/') || artifact.contains("..") {
                    return Err(AppError::sop(format!(
                        "节点「{}」的产物路径「{}」必须落在仓库内",
                        node.id, artifact
                    )));
                }
            }
        }
    }

    if !ids.contains(sop.entry.as_str()) {
        return Err(AppError::sop(format!("entry「{}」不存在", sop.entry)));
    }

    for edge in &sop.edges {
        if !ids.contains(edge.from.as_str()) {
            return Err(AppError::sop(format!("边起点「{}」不存在", edge.from)));
        }
        if !ids.contains(edge.to.as_str()) {
            return Err(AppError::sop(format!("边终点「{}」不存在", edge.to)));
        }
        if edge.kind == EdgeKind::Retry && edge.max.is_none() {
            return Err(AppError::sop(format!(
                "重试边 {} → {} 必须声明 max",
                edge.from, edge.to
            )));
        }
    }

    detect_cycle(sop)?;
    Ok(())
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 2 || bytes.len() > 40 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// 只对 normal 边做环检测：retry 边允许回到上游。
fn detect_cycle(sop: &Sop) -> Result<()> {
    let mut indegree: HashMap<&str, usize> = sop.nodes.iter().map(|n| (n.id.as_str(), 0)).collect();
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in sop.edges.iter().filter(|e| e.kind == EdgeKind::Normal) {
        out.entry(edge.from.as_str()).or_default().push(edge.to.as_str());
        *indegree.entry(edge.to.as_str()).or_insert(0) += 1;
    }
    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter(|(_, v)| **v == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop_front() {
        visited += 1;
        if let Some(targets) = out.get(id) {
            for t in targets {
                let d = indegree.get_mut(t).expect("target exists");
                *d -= 1;
                if *d == 0 {
                    queue.push_back(t);
                }
            }
        }
    }
    if visited != sop.nodes.len() {
        return Err(AppError::sop("流程存在环（normal 边），请改用 retry 边"));
    }
    Ok(())
}

// ---------------------------------------------------------------- when 求值

pub struct EvalCtx<'a> {
    pub last_decision: Option<DecisionAction>,
    pub nodes: &'a BTreeMap<String, NodeState>,
}

/// 只支持文档里约定的有限文法，无法解析时按 false 处理（宁可停下等人）。
pub fn eval_when(expr: &str, ctx: &EvalCtx<'_>) -> bool {
    let expr = expr.trim();
    if expr.is_empty() {
        return true;
    }
    expr.split(" or ").any(|part| {
        part.split(" and ")
            .all(|term| eval_term(term.trim(), ctx).unwrap_or(false))
    })
}

fn eval_term(term: &str, ctx: &EvalCtx<'_>) -> Option<bool> {
    if term == "review.approved" {
        return Some(ctx.last_decision == Some(DecisionAction::Approve));
    }
    if term == "review.rejected" {
        return Some(ctx.last_decision == Some(DecisionAction::Reject));
    }

    if let Some((node_id, pred)) = term.split_once(".retry") {
        let rest = pred.trim();
        let (op, num) = split_op(rest)?;
        let retries = ctx.nodes.get(node_id)?.retries as i64;
        let n: i64 = num.parse().ok()?;
        return Some(match op {
            "<" => retries < n,
            "<=" => retries <= n,
            ">" => retries > n,
            ">=" => retries >= n,
            "==" => retries == n,
            "!=" => retries != n,
            _ => return None,
        });
    }

    let (node_id, pred) = term.split_once('.')?;
    let status = ctx.nodes.get(node_id)?.status;
    match pred {
        "done" | "completed" => Some(status == NodeStatus::Done),
        "blocked" | "failed" => Some(status == NodeStatus::Blocked),
        "active" => Some(status == NodeStatus::Active),
        "waiting" | "waiting_human" => Some(status == NodeStatus::WaitingHuman),
        _ => None,
    }
}

fn split_op(rest: &str) -> Option<(&'static str, &str)> {
    for op in ["<=", ">=", "==", "!=", "<", ">"] {
        if let Some(rest) = rest.strip_prefix(op) {
            return Some((op, rest.trim()));
        }
    }
    None
}

/// 测试夹具：内核流程测试用的示例 SOP（不再作为默认 SOP 写入用户目录）。
pub const DEFAULT_SOP_YAML: &str = r#"sop: prd-to-ship
version: 1
title: 从需求到上线
entry: analyze
nodes:
  - id: analyze
    type: agent
    title: 需求分析
    agent: claude-code
    default_prompt: "/analysis 分析 docs/interview.md"
    done:
      events: [Stop]
  - id: draft_prd
    type: agent
    title: 起草 PRD
    agent: claude-code
    default_prompt: "/draft-prd 基于 docs/interview.md 起草 PRD"
    done:
      events: [Stop]
      artifacts: ["docs/prd.md"]
    review: true
    blocking: true
    guard:
      deny: ['Bash(git commit:*)', 'Bash(git push:*)']
  - id: build
    type: agent
    title: 实现
    agent: claude-code
    default_prompt: "/implement 按 docs/prd.md 实现定价对比表"
    done:
      events: [Stop]
  - id: release
    type: human
    title: 发布确认
    prompt: 检查变更清单，确认可发布
    blocking: true
edges:
  - { from: analyze, to: draft_prd }
  - { from: draft_prd, to: build, when: "review.approved" }
  - { from: draft_prd, to: draft_prd, when: "review.rejected", kind: retry, max: 3 }
  - { from: build, to: release }
"#;
