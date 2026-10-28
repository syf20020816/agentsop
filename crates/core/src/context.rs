//! 渲染给 Agent 的上下文摘要（≤1.2KB，UTF-8 安全截断）。

use crate::model::{NodeStatus, Run, RunStatus};

const MAX_BYTES: usize = 1200;

pub fn render(run: &Run) -> String {
    let mut out = String::new();
    out.push_str("# AgentSOP\n\n");
    out.push_str(&format!(
        "运行：{} · SOP：{} · 状态：{}\n",
        run.run_id,
        run.sop.display_title(),
        status_label(run.status)
    ));

    if let Some(current) = &run.current {
        let spec = run.node_spec(current);
        let state = run.node_state(current);
        let idx = run
            .sop
            .node_index(current)
            .map(|i| format!("{:02}", i + 1))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n当前节点：{} {}（{}",
            idx,
            spec.map(|s| s.title_or_id()).unwrap_or(current),
            state.map(|s| node_status_label(s.status)).unwrap_or("pending")
        ));
        if let Some(agent) = spec.and_then(|s| s.agent.as_deref()) {
            out.push_str(&format!(" · {}", agent));
        }
        if let Some(s) = state.filter(|s| s.retries > 0) {
            out.push_str(&format!(" · 第 {} 次", s.retries + 1));
        }
        out.push_str("）\n");

        if let Some(prompt) = spec.and_then(|s| s.default_prompt.as_deref()) {
            out.push_str(&format!("默认 Prompt：{}\n", prompt));
        }

        if let Some(state) = state {
            if state.status == NodeStatus::WaitingHuman {
                let question = state
                    .question
                    .as_deref()
                    .or_else(|| spec.and_then(|s| s.prompt.as_deref()))
                    .unwrap_or("等待人工确认");
                out.push_str(&format!("等待人确认：{}\n", question));
            }
        }

        let artifacts = spec
            .and_then(|s| s.done.as_ref())
            .map(|d| d.artifacts.clone())
            .unwrap_or_default();
        if !artifacts.is_empty() {
            out.push_str(&format!("完成条件：产物 {}\n", artifacts.join("、")));
        }
    } else {
        out.push_str("\n当前节点：无（运行已结束）\n");
    }

    if let Some(decision) = &run.last_decision {
        out.push_str(&format!(
            "\n最近决策：[{}] {} {}\n",
            decision_label(decision.action),
            decision.node_id,
            if decision.note.is_empty() {
                String::new()
            } else {
                format!("— {}", truncate_chars(&decision.note, 120))
            }
        ));
    }

    let progress: Vec<String> = run
        .sop
        .nodes
        .iter()
        .map(|n| {
            let st = run
                .nodes
                .get(&n.id)
                .map(|s| short_status(s.status))
                .unwrap_or("·");
            format!("{} {}", n.id, st)
        })
        .collect();
    out.push_str(&format!("\n进度：{}\n", progress.join(" · ")));

    if run.sop.edges.iter().any(|e| e.kind == crate::model::EdgeKind::Retry) {
        out.push_str("\n规则：被打回时按人的备注修改，不要跳过节点，也不要自行推进状态。\n");
    }

    truncate_bytes(&out, MAX_BYTES)
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Paused => "paused",
        RunStatus::Completed => "completed",
        RunStatus::Aborted => "aborted",
    }
}

fn node_status_label(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Pending => "pending",
        NodeStatus::Ready => "ready",
        NodeStatus::Active => "active",
        NodeStatus::WaitingHuman => "waiting_human",
        NodeStatus::Done => "done",
        NodeStatus::Blocked => "blocked",
        NodeStatus::Skipped => "skipped",
        NodeStatus::Stale => "stale",
    }
}

fn short_status(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Done => "✓",
        NodeStatus::Active => "运行",
        NodeStatus::WaitingHuman => "等你",
        NodeStatus::Blocked => "阻塞",
        NodeStatus::Stale => "超时",
        NodeStatus::Skipped => "跳过",
        _ => "·",
    }
}

fn decision_label(action: crate::model::DecisionAction) -> &'static str {
    match action {
        crate::model::DecisionAction::Approve => "approve",
        crate::model::DecisionAction::Reject => "reject",
        crate::model::DecisionAction::Retry => "retry",
        crate::model::DecisionAction::Abort => "abort",
        crate::model::DecisionAction::AllowOnce => "allow_once",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
