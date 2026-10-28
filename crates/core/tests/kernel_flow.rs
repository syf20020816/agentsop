use agentsop_core::{DecisionAction, Kernel, NodeStatus, Paths, RunStatus};

fn temp_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::new(dir.path().join(".agentsop"));
    // 内核不再预置默认 SOP，测试自己写入夹具。
    paths.ensure().expect("ensure");
    paths
        .save_sop(
            agentsop_core::DEFAULT_SOP_NAME,
            agentsop_core::DEFAULT_SOP_YAML,
        )
        .expect("fixture sop");
    let kernel = Kernel::boot(paths).expect("boot");
    (dir, kernel)
}

#[test]
fn sop_is_validated() {
    let bad = r#"
sop: bad
version: 1
entry: nope
nodes:
  - { id: aa, type: agent }
"#;
    let err = agentsop_core::sop::parse(bad).unwrap_err();
    assert_eq!(err.code, "SOP_INVALID");
    assert!(err.message.contains("entry"));
}

#[test]
fn agent_can_request_decision_and_resume() {
    let (_dir, mut k) = temp_kernel();
    let repo = std::env::temp_dir();
    let snap = k
        .start_run("prd-to-ship", repo.to_str().unwrap(), Default::default())
        .expect("start");

    let halted = k
        .request_decision(
            &snap.run_id,
            None,
            "用户画像按 B 端还是 C 端？",
            Some(vec!["B 端".into(), "C 端".into()]),
        )
        .expect("request");
    assert_eq!(halted.nodes[0].status, NodeStatus::WaitingHuman);
    let question = &halted.nodes[0].pending_decision.as_ref().expect("pending").question;
    assert!(question.contains("B 端"));
    assert!(question.contains("选项"));

    let resumed = k
        .decide(&snap.run_id, "analyze", DecisionAction::Approve, "按 B 端写")
        .expect("decide");
    assert_eq!(resumed.current_node.as_deref(), Some("analyze"));
    assert_eq!(resumed.nodes[0].status, NodeStatus::Active);
    assert!(resumed.nodes[0].pending_decision.is_none());
    let last = resumed.last_decision.expect("last decision");
    assert_eq!(last.action, DecisionAction::Approve);
    assert_eq!(last.note, "按 B 端写");
}

#[test]
fn full_flow_with_review_and_default_prompt() {
    let (_dir, mut k) = temp_kernel();
    let repo = std::env::temp_dir();
    let mut snap = k
        .start_run("prd-to-ship", repo.to_str().unwrap(), Default::default())
        .expect("start");

    assert_eq!(snap.current_node.as_deref(), Some("analyze"));
    let first = &snap.nodes[0];
    assert_eq!(first.status, NodeStatus::Active);
    assert_eq!(first.default_prompt.as_deref(), Some("/analysis 分析 docs/interview.md"));

    // 01 完成 → 02 运行中
    snap = k.complete_node(&snap.run_id, "analyze", vec![]).unwrap();
    assert_eq!(snap.current_node.as_deref(), Some("draft_prd"));
    assert_eq!(snap.nodes[1].status, NodeStatus::Active);

    // 02 review:true → Stop 后停在等人工确认
    snap = k.complete_node(&snap.run_id, "draft_prd", vec![]).unwrap();
    assert_eq!(snap.nodes[1].status, NodeStatus::WaitingHuman);
    assert!(snap.nodes[1].pending_decision.is_some());
    assert!(snap.nodes[1].default_prompt.is_some());

    // 缺少备注的打回必须被拒绝
    let err = k
        .decide(&snap.run_id, "draft_prd", DecisionAction::Reject, "不行")
        .unwrap_err();
    assert_eq!(err.code, "INVALID_ARGUMENT");

    // 通过 → 03 运行中
    snap = k
        .decide(&snap.run_id, "draft_prd", DecisionAction::Approve, "")
        .unwrap();
    assert_eq!(snap.current_node.as_deref(), Some("build"));
    assert_eq!(snap.nodes[1].status, NodeStatus::Done);

    snap = k.complete_node(&snap.run_id, "build", vec![]).unwrap();
    assert_eq!(snap.current_node.as_deref(), Some("release"));
    assert_eq!(snap.nodes[3].status, NodeStatus::WaitingHuman);

    snap = k
        .decide(&snap.run_id, "release", DecisionAction::Approve, "")
        .unwrap();
    assert_eq!(snap.run_status, RunStatus::Completed);
    assert!(snap.current_node.is_none());
}

#[test]
fn reject_retries_and_blocks_after_max() {
    let (_dir, mut k) = temp_kernel();
    let repo = std::env::temp_dir();
    let mut snap = k
        .start_run("prd-to-ship", repo.to_str().unwrap(), Default::default())
        .unwrap();
    snap = k.complete_node(&snap.run_id, "analyze", vec![]).unwrap();
    snap = k.complete_node(&snap.run_id, "draft_prd", vec![]).unwrap();

    for i in 1..=3 {
        snap = k
            .decide(
                &snap.run_id,
                "draft_prd",
                DecisionAction::Reject,
                "定价部分缺少竞品对比，补一张表",
            )
            .unwrap();
        assert_eq!(snap.nodes[1].retries, i);
        assert_eq!(snap.nodes[1].status, NodeStatus::Active);
        snap = k.complete_node(&snap.run_id, "draft_prd", vec![]).unwrap();
    }

    snap = k
        .decide(
            &snap.run_id,
            "draft_prd",
            DecisionAction::Reject,
            "还是缺少竞品对比表，请补齐",
        )
        .unwrap();
    assert_eq!(snap.nodes[1].status, NodeStatus::Blocked);
}

#[test]
fn artifacts_gate_stop_event() {
    let (_dir, mut k) = temp_kernel();
    let repo = tempfile::tempdir().unwrap();
    let mut snap = k
        .start_run("prd-to-ship", repo.path().to_str().unwrap(), Default::default())
        .unwrap();
    snap = k.complete_node(&snap.run_id, "analyze", vec![]).unwrap();

    // 02 需要 docs/prd.md，缺失时 Stop 只记证据、不推进
    let outcome = k
        .hook(serde_json::json!({
            "source": "claude-code",
            "hook_event": "Stop",
            "run_id": snap.run_id,
            "session_id": "cc_test",
        }))
        .unwrap();
    assert_eq!(outcome.action, "artifact-missing");
    let after = k.snapshot(&snap.run_id).unwrap();
    assert_eq!(after.nodes[1].status, NodeStatus::Active);
    assert!(after.nodes[1].evidence.iter().any(|e| e.kind == "artifact"));

    std::fs::create_dir_all(repo.path().join("docs")).unwrap();
    std::fs::write(repo.path().join("docs/prd.md"), "# PRD").unwrap();
    let outcome = k
        .hook(serde_json::json!({
            "source": "claude-code",
            "hook_event": "Stop",
            "run_id": snap.run_id,
            "session_id": "cc_test",
        }))
        .unwrap();
    assert_eq!(outcome.action, "completed");
    let after = k.snapshot(&snap.run_id).unwrap();
    assert_eq!(after.nodes[1].status, NodeStatus::WaitingHuman);
}

#[test]
fn guard_denies_while_a_blocking_node_waits_for_human() {
    let (_dir, mut k) = temp_kernel();
    let mut snap = k
        .start_run("prd-to-ship", ".", Default::default())
        .unwrap();
    snap = k.complete_node(&snap.run_id, "analyze", vec![]).unwrap();
    // 02 有待确认的 review + blocking:true + guard.deny
    snap = k.complete_node(&snap.run_id, "draft_prd", vec![]).unwrap();
    assert_eq!(snap.nodes[1].status, NodeStatus::WaitingHuman);

    let outcome = k
        .hook(serde_json::json!({
            "source": "claude-code",
            "hook_event": "PreToolUse",
            "run_id": snap.run_id,
            "tool_name": "Bash",
            "tool_input": { "command": "git commit -m wip" },
        }))
        .unwrap();
    assert_eq!(outcome.action, "deny");
    assert_eq!(
        outcome.response["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );

    // 读操作不拦
    let ok = k
        .hook(serde_json::json!({
            "source": "claude-code",
            "hook_event": "PreToolUse",
            "run_id": snap.run_id,
            "tool_name": "Read",
            "tool_input": { "file_path": "docs/prd.md" },
        }))
        .unwrap();
    assert_eq!(ok.action, "allow");
}

#[test]
fn replay_from_events_matches_snapshot() {
    let (_dir, mut k) = temp_kernel();
    let mut snap = k
        .start_run("prd-to-ship", ".", Default::default())
        .unwrap();
    snap = k.complete_node(&snap.run_id, "analyze", vec![]).unwrap();
    let run_id = snap.run_id.clone();

    let events = k.paths().read_events(&run_id, None).unwrap();
    assert!(events.len() >= 4);
    assert!(events.windows(2).all(|w| w[0].seq + 1 == w[1].seq));

    // 重启后从磁盘恢复
    let paths = k.paths().clone();
    let k2 = Kernel::boot(paths).unwrap();
    let snap2 = k2.snapshot(&run_id).unwrap();
    assert_eq!(snap2.current_node, snap.current_node);
    assert_eq!(snap2.nodes[1].status, snap.nodes[1].status);
}

#[test]
fn editing_sop_applies_to_running_run_immediately() {
    let (_dir, mut k) = temp_kernel();
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("branch".to_string(), "feat/x".to_string());
    let snap = k
        .start_run("prd-to-ship", ".", vars)
        .expect("start");
    assert_eq!(snap.current_node.as_deref(), Some("analyze"));

    let edited = r#"sop: prd-to-ship
version: 1
title: 从需求到上线（改）
entry: analyze
nodes:
  - id: analyze
    type: agent
    title: 需求梳理
    agent: claude-code
    default_prompt: "/analysis 重新分析"
    done:
      events: [Stop]
  - id: ship
    type: human
    title: 上线确认
    prompt: 确认上线
"#;
    k.paths().save_sop("prd-to-ship", edited).expect("save");
    let touched = k.reload_sop("prd-to-ship", "prd-to-ship").expect("reload");
    assert_eq!(touched, vec![snap.run_id.clone()]);

    let after = k.snapshot(&snap.run_id).expect("snapshot");
    assert_eq!(after.sop.title, "从需求到上线（改）");
    assert_eq!(after.nodes.len(), 2);
    assert_eq!(after.current_node.as_deref(), Some("analyze"));
    let analyze = after.nodes.iter().find(|n| n.id == "analyze").expect("analyze");
    assert_eq!(analyze.title, "需求梳理");
    assert_eq!(analyze.default_prompt.as_deref(), Some("/analysis 重新分析"));
    assert_eq!(analyze.status, NodeStatus::Active);

    // 运行期变量不能被磁盘定义冲掉
    let run = k.run(&snap.run_id).expect("run");
    assert_eq!(run.sop.vars.get("branch").map(String::as_str), Some("feat/x"));

    // 改名后仍然只影响这一条运行，且运行里的 SOP 名同步
    k.paths()
        .save_sop("prd-to-ship-v2", &edited.replace("sop: prd-to-ship", "sop: prd-to-ship-v2"))
        .expect("save renamed");
    k.reload_sop("prd-to-ship", "prd-to-ship-v2").expect("reload renamed");
    assert_eq!(k.run(&snap.run_id).unwrap().sop.sop, "prd-to-ship-v2");
}

#[test]
fn context_contains_default_prompt_and_decision() {
    let (_dir, mut k) = temp_kernel();
    let snap = k
        .start_run("prd-to-ship", ".", Default::default())
        .unwrap();
    let ctx = k.context_md(&snap.run_id).unwrap();
    assert!(ctx.contains("/analysis 分析 docs/interview.md"));
    assert!(ctx.len() <= 1200);
}
