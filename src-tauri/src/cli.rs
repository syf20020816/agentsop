//! CLI：按文档约定，CLI 只做 HTTP 薄客户端，避免与界面进程抢写状态。

use agentsop_core::store::Paths;
use agentsop_core::{AppError, Result};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpStream;

pub fn run(args: Vec<String>) -> i32 {
    match dispatch(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{}", err);
            1
        }
    }
}

fn dispatch(args: &[String]) -> Result<i32> {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("status");
    match cmd {
        "status" => {
            let snap = call("GET", "/v1/snapshot", None)?;
            print_status(&snap);
            Ok(0)
        }
        "json" => {
            let snap = call("GET", "/v1/snapshot", None)?;
            println!("{}", serde_json::to_string_pretty(&snap).unwrap_or_default());
            Ok(0)
        }
        "runs" => {
            let out = call("GET", "/v1/runs", None)?;
            let runs = out["runs"].as_array().cloned().unwrap_or_default();
            if runs.is_empty() {
                println!("（还没有运行）");
            }
            for run in runs {
                let current = run["current_node"].as_str().unwrap_or("—");
                println!(
                    "{}  {:<16} {:?}  当前 {}",
                    run["run_id"].as_str().unwrap_or(""),
                    run["sop"]["title"].as_str().unwrap_or(""),
                    run["run_status"].as_str().unwrap_or(""),
                    current
                );
            }
            Ok(0)
        }
        "start" => {
            let sop = args
                .get(1)
                .ok_or_else(|| AppError::new("INVALID_ARGUMENT", "用法：agentsop start <sop> [--repo <路径>]"))?;
            let mut repo = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".into());
            let mut vars = serde_json::Map::new();
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--repo" => {
                        repo = args.get(i + 1).cloned().unwrap_or(repo);
                        i += 2;
                    }
                    "--var" => {
                        if let Some(kv) = args.get(i + 1) {
                            if let Some((k, v)) = kv.split_once('=') {
                                vars.insert(k.to_string(), Value::String(v.to_string()));
                            }
                        }
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            let snap = call(
                "POST",
                "/v1/runs",
                Some(json!({ "sop": sop, "repo": repo, "vars": vars })),
            )?;
            println!(
                "已启动 {}（{}）",
                snap["sop"]["title"].as_str().unwrap_or(sop.as_str()),
                snap["run_id"].as_str().unwrap_or("")
            );
            print_status(&snap);
            Ok(0)
        }
        "next" => {
            let snap = call("GET", "/v1/snapshot", None)?;
            println!("{}", serde_json::to_string_pretty(&next_payload(&snap)?).unwrap_or_default());
            Ok(0)
        }
        "context" => {
            let out = call("GET", "/v1/context", None)?;
            println!("{}", out["markdown"].as_str().unwrap_or(""));
            Ok(0)
        }
        "complete" => {
            let node_id = args.get(1).cloned().unwrap_or_else(|| {
                let snap = call("GET", "/v1/snapshot", None).unwrap_or(Value::Null);
                snap["current_node"].as_str().unwrap_or("").to_string()
            });
            if node_id.is_empty() {
                return Err(AppError::new("INVALID_ARGUMENT", "没有当前节点可完成"));
            }
            let mut evidence = Vec::new();
            let mut i = 2;
            while i < args.len() {
                if args[i] == "--artifact" {
                    if let Some(p) = args.get(i + 1) {
                        evidence.push(json!({ "kind": "file", "label": p, "path": p }));
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            let snap = call(
                "POST",
                &format!("/v1/nodes/{}/complete", node_id),
                Some(json!({ "evidence": evidence })),
            )?;
            print_status(&snap);
            Ok(0)
        }
        "decide" => {
            let action = args.get(1).ok_or_else(|| {
                AppError::new("INVALID_ARGUMENT", "用法：agentsop decide <approve|reject|retry|allow-once|abort>")
            })?;
            let mut node_id = String::new();
            let mut note = String::new();
            let mut run_id: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--node" => {
                        node_id = args.get(i + 1).cloned().unwrap_or_default();
                        i += 2;
                    }
                    "--note" => {
                        note = args.get(i + 1).cloned().unwrap_or_default();
                        i += 2;
                    }
                    "--run" => {
                        run_id = args.get(i + 1).cloned();
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            if node_id.is_empty() {
                let snap = call("GET", "/v1/snapshot", None)?;
                node_id = snap["current_node"].as_str().unwrap_or("").to_string();
            }
            let action = match action.as_str() {
                "approve" => "approve",
                "reject" => "reject",
                "retry" => "retry",
                "allow-once" | "allow_once" => "allow_once",
                "abort" => "abort",
                other => {
                    return Err(AppError::new(
                        "INVALID_ARGUMENT",
                        format!("未知动作 {}", other),
                    ));
                }
            };
            let mut body = json!({ "node_id": node_id, "action": action, "note": note });
            if let Some(id) = run_id {
                body["run_id"] = Value::String(id);
            }
            let snap = call("POST", "/v1/decisions", Some(body))?;
            print_status(&snap);
            Ok(0)
        }
        "pause" => {
            let id = current_run_id()?;
            let snap = call("POST", &format!("/v1/runs/{}/pause", id), Some(json!({})))?;
            print_status(&snap);
            Ok(0)
        }
        "resume" => {
            let id = current_run_id()?;
            let snap = call("POST", &format!("/v1/runs/{}/resume", id), Some(json!({})))?;
            print_status(&snap);
            Ok(0)
        }
        "abort" => {
            let id = current_run_id()?;
            let snap = call("POST", &format!("/v1/runs/{}/abort", id), Some(json!({})))?;
            print_status(&snap);
            Ok(0)
        }
        "hook" => {
            // fail-open：任何异常都返回 continue:true，绝不阻塞开发者工作流。
            let source = args.get(1).map(|s| s.as_str()).unwrap_or("generic");
            let mut raw = String::new();
            let _ = std::io::stdin().read_to_string(&mut raw);
            let payload: Value = serde_json::from_str(if raw.trim().is_empty() { "{}" } else { &raw })
                .unwrap_or_else(|_| json!({}));
            match call("POST", &format!("/v1/hooks/{}", source), Some(payload)) {
                Ok(resp) => {
                    println!("{}", serde_json::to_string(&resp).unwrap_or_else(|_| r#"{"continue":true}"#.into()));
                }
                Err(err) => {
                    eprintln!("AgentSOP hook 降级放行：{}", err);
                    println!(r#"{{"continue":true}}"#);
                }
            }
            Ok(0)
        }
        "sop" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("list");
            match sub {
                "list" => {
                    let out = call("GET", "/v1/sops", None)?;
                    for sop in out["sops"].as_array().cloned().unwrap_or_default() {
                        println!(
                            "{:<20} {:<16} v{} · {} 个节点",
                            sop["name"].as_str().unwrap_or(""),
                            sop["title"].as_str().unwrap_or(""),
                            sop["version"].as_u64().unwrap_or(0),
                            sop["nodes"].as_u64().unwrap_or(0)
                        );
                    }
                    Ok(0)
                }
                "show" => {
                    let name = args.get(2).ok_or_else(|| AppError::new("INVALID_ARGUMENT", "缺少 SOP 名称"))?;
                    let out = call("GET", &format!("/v1/sops/{}", name), None)?;
                    println!("{}", out["yaml"].as_str().unwrap_or(""));
                    Ok(0)
                }
                "validate" => {
                    let file = args
                        .get(2)
                        .ok_or_else(|| AppError::new("INVALID_ARGUMENT", "缺少文件路径"))?;
                    let yaml = std::fs::read_to_string(file)?;
                    let out = call("POST", "/v1/sops/_/validate", Some(json!({ "yaml": yaml })))?;
                    println!("SOP 合法：{} · {} 个节点", out["sop"].as_str().unwrap_or(""), out["nodes"]);
                    Ok(0)
                }
                "install" => {
                    let file = args
                        .get(2)
                        .ok_or_else(|| AppError::new("INVALID_ARGUMENT", "缺少文件路径"))?;
                    let yaml = std::fs::read_to_string(file)?;
                    let sop = agentsop_core::sop::parse(&yaml)?;
                    let out = call(
                        "POST",
                        &format!("/v1/sops/{}", sop.sop),
                        Some(json!({ "yaml": yaml })),
                    )?;
                    println!("已安装 → {}", out["name"].as_str().unwrap_or(&sop.sop));
                    Ok(0)
                }
                other => Err(AppError::new("INVALID_ARGUMENT", format!("未知子命令 sop {}", other))),
            }
        }
        "doctor" => doctor(),
        "hooks" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            let dry_run = args.iter().any(|a| a == "--dry-run");
            let targets = parse_agents(args.get(2..).unwrap_or(&[]))?;
            match sub {
                "install" => hooks_install(&targets, dry_run),
                "uninstall" => hooks_uninstall(&targets, dry_run),
                "status" => hooks_status(&targets),
                other => Err(AppError::new(
                    "INVALID_ARGUMENT",
                    format!("未知子命令 hooks {}", other),
                )),
            }
        }
        "mcp" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("status");
            let dry_run = args.iter().any(|a| a == "--dry-run");
            let targets = parse_agents(args.get(2..).unwrap_or(&[]))?;
            match sub {
                "install" => mcp_install(&targets, dry_run),
                "uninstall" => mcp_uninstall(&targets, dry_run),
                "status" => mcp_status(&targets),
                other => Err(AppError::new(
                    "INVALID_ARGUMENT",
                    format!("未知子命令 mcp {}", other),
                )),
            }
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(0)
        }
        other => Err(AppError::new(
            "INVALID_ARGUMENT",
            format!("未知命令「{}」，试试 agentsop help", other),
        )),
    }
}

fn current_run_id() -> Result<String> {
    let snap = call("GET", "/v1/snapshot", None)?;
    snap["run_id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::not_found("没有进行中的运行"))
}

fn next_payload(snap: &Value) -> Result<Value> {
    let current = snap["current_node"].as_str().unwrap_or("");
    if current.is_empty() {
        return Err(AppError::new("NO_CURRENT_NODE", "运行已结束或尚未开始"));
    }
    let node = snap["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|n| n["id"] == current))
        .cloned()
        .ok_or_else(|| AppError::not_found("当前节点不在快照中"))?;
    Ok(json!({
        "node_id": node["id"],
        "idx": node["idx"],
        "title": node["title"],
        "type": node["type"],
        "status": node["status"],
        "agent": node["agent"],
        "default_prompt": node["default_prompt"],
        "prompt": node["prompt"],
        "pending_decision": node["pending_decision"],
        "evidence": node["evidence"],
        "run_id": snap["run_id"],
    }))
}

fn print_status(snap: &Value) {
    let title = snap["sop"]["title"].as_str().unwrap_or("未命名 SOP");
    let run_id = snap["run_id"].as_str().unwrap_or("");
    let status = snap["run_status"].as_str().unwrap_or("");
    println!("AgentSOP · {}  {}  [{}]", title, run_id, status);
    if let Some(nodes) = snap["nodes"].as_array() {
        for n in nodes {
            let mark = match n["status"].as_str().unwrap_or("") {
                "done" => "✓",
                "active" => "▶",
                "waiting_human" => "◉",
                "blocked" => "✕",
                "stale" => "!",
                _ => "·",
            };
            let current = snap["current_node"].as_str() == n["id"].as_str();
            println!(
                "  {} {} {:<12} {:<14}{}",
                mark,
                n["idx"].as_str().unwrap_or(""),
                n["title"].as_str().unwrap_or(""),
                n["status"].as_str().unwrap_or(""),
                if current { "  ←" } else { "" }
            );
        }
    }
    if let Some(decision) = snap.get("last_decision").and_then(|d| d.as_object()) {
        let _ = decision;
    }
}

fn doctor() -> Result<i32> {
    let paths = Paths::default_handle();
    println!("状态目录：{}", paths.root().display());

    let mut ok = true;
    match paths.token() {
        Ok(token) => {
            println!("token：已就绪（{}…）", &token[..6.min(token.len())]);
            let mode = std::fs::metadata(paths.token_path())
                .map(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o777
                })
                .unwrap_or(0);
            if mode != 0o600 {
                println!("  ⚠ token 权限为 {:o}，应为 600", mode);
                ok = false;
            }
        }
        Err(err) => {
            println!("token：缺失（{}）", err);
            ok = false;
        }
    }

    match active_port() {
        Some(port) => println!("HTTP：运行中，监听 127.0.0.1:{}", port),
        None => {
            println!("HTTP：未运行（打开 AgentSOP 或按 ⌥⌘S）");
            ok = false;
        }
    }

    let sops = paths.list_sops();
    println!("SOP：{} 个（{}）", sops.len(), sops.join("、"));
    let runs = paths.list_runs();
    println!("历史运行：{} 个", runs.len());

    for agent in [AgentKind::Claude, AgentKind::Codex, AgentKind::Trae] {
        let path = agent.hooks_path();
        if !path.exists() {
            println!("{} hooks：未配置（{} 不存在）", agent.label(), path.display());
        } else {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            if raw.contains("agentsop") {
                println!("{} hooks：已配置（{}）", agent.label(), path.display());
            } else {
                println!(
                    "{} hooks：未配置（运行 agentsop hooks install {}）",
                    agent.label(),
                    agent.slug()
                );
            }
        }
        println!(
            "{} MCP：{}",
            agent.label(),
            if mcp_configured(agent) { "已配置" } else { "未配置" }
        );
    }

    Ok(if ok { 0 } else { 2 })
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

// ---------------------------------------------------------------- Agent 集成（hooks / MCP）

#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentKind {
    Claude,
    Codex,
    Trae,
}

impl AgentKind {
    fn slug(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude-code",
            AgentKind::Codex => "codex",
            AgentKind::Trae => "trae",
        }
    }

    fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Trae => "TRAE",
        }
    }

    fn home_dir(self) -> std::path::PathBuf {
        dirs_home().join(match self {
            AgentKind::Claude => ".claude",
            AgentKind::Codex => ".codex",
            AgentKind::Trae => ".trae-cn",
        })
    }

    fn hooks_path(self) -> std::path::PathBuf {
        match self {
            AgentKind::Claude => self.home_dir().join("settings.json"),
            AgentKind::Codex => self.home_dir().join("hooks.json"),
            AgentKind::Trae => self.home_dir().join("hooks.json"),
        }
    }

    fn events(self) -> &'static [&'static str] {
        match self {
            AgentKind::Claude => &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
                "SubagentStop",
            ],
            AgentKind::Codex => &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
            ],
            AgentKind::Trae => &[
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "Stop",
            ],
        }
    }

    /// TRAE 的 hooks.json 顶层需要 schema 版本。
    fn needs_schema_version(self) -> bool {
        matches!(self, AgentKind::Trae)
    }
}

/// 解析 `hooks install codex trae` 里的 Agent 列表；空列表表示稍后自动探测。
fn parse_agents(args: &[String]) -> Result<Vec<AgentKind>> {
    let mut out: Vec<AgentKind> = Vec::new();
    for arg in args {
        let agent = match arg.as_str() {
            "claude" | "claude-code" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "trae" | "trae-cn" => AgentKind::Trae,
            "all" => {
                for agent in [AgentKind::Claude, AgentKind::Codex, AgentKind::Trae] {
                    if !out.contains(&agent) {
                        out.push(agent);
                    }
                }
                continue;
            }
            other if other.starts_with("--") => continue,
            other => {
                return Err(AppError::new(
                    "INVALID_ARGUMENT",
                    format!("未知 Agent「{}」，可选：claude-code / codex / trae / all", other),
                ));
            }
        };
        if !out.contains(&agent) {
            out.push(agent);
        }
    }
    Ok(out)
}

fn which(name: &str) -> bool {
    std::env::var("PATH")
        .map(|path| {
            path.split(':')
                .any(|dir| !dir.is_empty() && std::path::Path::new(dir).join(name).exists())
        })
        .unwrap_or(false)
}

/// 显式指定优先；未指定时按本机实际安装情况探测。
fn resolve_agents(explicit: &[AgentKind]) -> Vec<AgentKind> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }
    let mut detected = Vec::new();
    if dirs_home().join(".claude/settings.json").exists() || which("claude") {
        detected.push(AgentKind::Claude);
    }
    if dirs_home().join(".codex").exists() {
        detected.push(AgentKind::Codex);
    }
    if dirs_home().join(".trae-cn").exists() {
        detected.push(AgentKind::Trae);
    }
    if detected.is_empty() {
        println!("未检测到已安装的 Agent，可用 agentsop hooks install codex trae 显式指定");
    }
    detected
}

fn hook_command(agent: AgentKind) -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("'{}' hook {}", exe.display(), agent.slug())
}

fn is_agentsop_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("agentsop") && c.contains("hook"))
            })
        })
        .unwrap_or(false)
}

fn load_settings(path: &std::path::Path) -> Value {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return json!({});
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

fn backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "config".into());
    path.with_file_name(format!("{}.agentsop.bak", name))
}

fn write_settings(path: &std::path::Path, settings: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        let _ = std::fs::copy(path, backup_path(path));
    }
    let text = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, format!("{}\n", text))?;
    Ok(())
}

fn hooks_install(agents: &[AgentKind], dry_run: bool) -> Result<i32> {
    let agents = resolve_agents(agents);
    if agents.is_empty() {
        return Ok(2);
    }
    for agent in agents {
        let path = agent.hooks_path();
        let command = hook_command(agent);
        let mut settings = load_settings(&path);
        let root = settings.as_object_mut().expect("settings object");
        if agent.needs_schema_version() {
            root.entry("version").or_insert_with(|| json!(1));
        }
        let hooks = root.entry("hooks").or_insert_with(|| json!({}));
        if !hooks.is_object() {
            *hooks = json!({});
        }
        let hooks = hooks.as_object_mut().expect("hooks object");

        for event in agent.events() {
            let list = hooks.entry(*event).or_insert_with(|| json!([]));
            if !list.is_array() {
                *list = json!([]);
            }
            let entries = list.as_array_mut().expect("hooks array");
            entries.retain(|entry| !is_agentsop_entry(entry));
            entries.push(json!({
                "hooks": [{ "type": "command", "command": command, "timeout": 10 }]
            }));
        }

        if dry_run {
            println!("[{}] {}（dry-run）", agent.label(), path.display());
            println!("{}", serde_json::to_string_pretty(&settings).unwrap_or_default());
            continue;
        }
        write_settings(&path, &settings)?;
        println!("已写入 {} hooks → {}", agent.label(), path.display());
        println!("  命令：{}", command);
        let backup = backup_path(&path);
        if backup.exists() {
            println!("  已备份：{}", backup.display());
        }
        if agent == AgentKind::Codex {
            ensure_codex_hooks_flag(dry_run)?;
        }
    }
    Ok(0)
}

/// Codex hooks 需要 `[features] codex_hooks = true` 才生效。
fn ensure_codex_hooks_flag(dry_run: bool) -> Result<()> {
    let path = dirs_home().join(".codex/config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let has_flag = text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("codex_hooks")
            && line
                .split_once('=')
                .map(|(_, v)| v.trim() == "true")
                .unwrap_or(false)
    });
    if has_flag {
        println!("  Codex hooks 特性：已开启");
        return Ok(());
    }

    let mut out = String::new();
    let mut in_features = false;
    let mut inserted = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            if in_features && !inserted {
                out.push_str("codex_hooks = true\n");
                inserted = true;
            }
            in_features = trimmed == "[features]";
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_features && !inserted && trimmed.starts_with("codex_hooks") {
            out.push_str("codex_hooks = true\n");
            inserted = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if in_features && !inserted {
        out.push_str("codex_hooks = true\n");
        inserted = true;
    }
    if !inserted {
        out.push_str("\n[features]\ncodex_hooks = true\n");
    }

    if dry_run {
        println!("  [Codex] 将启用 codex_hooks = true（{}）", path.display());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        let _ = std::fs::copy(&path, backup_path(&path));
    }
    std::fs::write(&path, out)?;
    println!("  Codex hooks 特性：已写入 {}（codex_hooks = true）", path.display());
    Ok(())
}

fn hooks_uninstall(agents: &[AgentKind], dry_run: bool) -> Result<i32> {
    let agents = resolve_agents(agents);
    for agent in agents {
        let path = agent.hooks_path();
        if !path.exists() {
            println!("{} hooks：{} 不存在，无需卸载", agent.label(), path.display());
            continue;
        }
        let mut settings = load_settings(&path);
        let mut removed = 0usize;
        if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            for event in agent.events() {
                if let Some(entries) = hooks.get_mut(*event).and_then(|l| l.as_array_mut()) {
                    let before = entries.len();
                    entries.retain(|entry| !is_agentsop_entry(entry));
                    removed += before - entries.len();
                }
            }
        }
        if dry_run {
            println!("[{}] {}（dry-run）", agent.label(), path.display());
            println!("{}", serde_json::to_string_pretty(&settings).unwrap_or_default());
            continue;
        }
        write_settings(&path, &settings)?;
        println!("已移除 {} 条 {} hooks（{}）", removed, agent.label(), path.display());
    }
    Ok(0)
}

fn hooks_status(agents: &[AgentKind]) -> Result<i32> {
    let agents = resolve_agents(agents);
    if agents.is_empty() {
        return Ok(2);
    }
    let mut all_ok = true;
    for agent in agents {
        let path = agent.hooks_path();
        println!("{} hooks：{}", agent.label(), path.display());
        if !path.exists() {
            println!("  状态：未安装（agentsop hooks install {}）", agent.slug());
            all_ok = false;
            continue;
        }
        let settings = load_settings(&path);
        let events = agent.events();
        let mut configured = 0usize;
        for event in events {
            let ok = settings
                .get("hooks")
                .and_then(|h| h.get(*event))
                .and_then(|v| v.as_array())
                .map(|entries| entries.iter().any(is_agentsop_entry))
                .unwrap_or(false);
            if ok {
                configured += 1;
            }
            println!("  {} {}", if ok { "✓" } else { "·" }, event);
        }
        if configured == events.len() {
            println!("  状态：已配置");
        } else {
            println!(
                "  状态：部分配置（{}/{}），运行 agentsop hooks install {}",
                configured,
                events.len(),
                agent.slug()
            );
            all_ok = false;
        }
        if agent == AgentKind::Codex {
            let flag = std::fs::read_to_string(dirs_home().join(".codex/config.toml"))
                .map(|raw| {
                    raw.lines().any(|line| {
                        let line = line.trim();
                        line.starts_with("codex_hooks")
                            && line
                                .split_once('=')
                                .map(|(_, v)| v.trim() == "true")
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            println!(
                "  特性 codex_hooks：{}",
                if flag { "已开启" } else { "未开启（需 [features] codex_hooks = true）" }
            );
            if !flag {
                all_ok = false;
            }
        }
    }
    Ok(if all_ok { 0 } else { 2 })
}

// ---------------------------------------------------------------- MCP 注册

fn mcp_bin() -> Result<std::path::PathBuf> {
    if let Ok(raw) = std::env::var("AGENTSOP_MCP") {
        let path = std::path::PathBuf::from(raw);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("agentsop-mcp");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(':') {
            if dir.is_empty() {
                continue;
            }
            let candidate = std::path::Path::new(dir).join("agentsop-mcp");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(AppError::new(
        "MCP_NOT_FOUND",
        "找不到 agentsop-mcp：先 cargo build -p agentsop-mcp，或用环境变量 AGENTSOP_MCP 指定路径",
    ))
}

fn trae_mcp_path() -> std::path::PathBuf {
    dirs_home().join("Library/Application Support/Trae CN/User/mcp.json")
}

fn claude_mcp_path() -> std::path::PathBuf {
    dirs_home().join(".claude.json")
}

fn install_json_mcp(path: &std::path::Path, bin: &std::path::Path, label: &str, dry_run: bool) -> Result<()> {
    let mut settings = load_settings(path);
    let root = settings.as_object_mut().expect("settings object");
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    let servers = servers.as_object_mut().expect("mcpServers object");
    servers.insert(
        "agentsop".into(),
        json!({ "command": bin.to_string_lossy() }),
    );
    if dry_run {
        println!("[{}] {}（dry-run）", label, path.display());
        println!("{}", serde_json::to_string_pretty(&settings).unwrap_or_default());
        return Ok(());
    }
    write_settings(path, &settings)?;
    println!("已写入 {} MCP → {}", label, path.display());
    println!("  command：{}", bin.display());
    Ok(())
}

fn install_codex_mcp(bin: &std::path::Path, dry_run: bool) -> Result<()> {
    let config = dirs_home().join(".codex/config.toml");
    let raw = std::fs::read_to_string(&config).unwrap_or_default();
    if raw.contains("[mcp_servers.agentsop]") {
        println!("Codex MCP：已配置（{}）", config.display());
        return Ok(());
    }
    if dry_run {
        println!("[Codex] 将执行：codex mcp add agentsop -- {}", bin.display());
        return Ok(());
    }
    let output = std::process::Command::new("codex")
        .args(["mcp", "add", "agentsop", "--"])
        .arg(bin)
        .output()
        .map_err(|err| {
            AppError::new("MCP_INSTALL_FAILED", format!("无法执行 codex：{}", err))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .find(|line| !line.contains("WARNING") && !line.trim().is_empty())
            .unwrap_or("codex mcp add 失败");
        return Err(AppError::new("MCP_INSTALL_FAILED", message.to_string()));
    }
    println!("已写入 Codex MCP → {}（[mcp_servers.agentsop]）", config.display());
    Ok(())
}

fn remove_json_mcp(path: &std::path::Path, label: &str, dry_run: bool) -> Result<()> {
    if !path.exists() {
        println!("{} MCP：{} 不存在，无需卸载", label, path.display());
        return Ok(());
    }
    let mut settings = load_settings(path);
    let removed = settings
        .get_mut("mcpServers")
        .and_then(|s| s.as_object_mut())
        .and_then(|servers| servers.remove("agentsop"))
        .is_some();
    if !removed {
        println!("{} MCP：未配置 agentsop，无需卸载", label);
        return Ok(());
    }
    if dry_run {
        println!("[{}] {}（dry-run）", label, path.display());
        println!("{}", serde_json::to_string_pretty(&settings).unwrap_or_default());
        return Ok(());
    }
    write_settings(path, &settings)?;
    println!("已移除 {} MCP（{}）", label, path.display());
    Ok(())
}

fn mcp_install(agents: &[AgentKind], dry_run: bool) -> Result<i32> {
    let agents = resolve_agents(agents);
    if agents.is_empty() {
        return Ok(2);
    }
    let bin = mcp_bin()?;
    for agent in agents {
        match agent {
            AgentKind::Codex => install_codex_mcp(&bin, dry_run)?,
            AgentKind::Trae => install_json_mcp(&trae_mcp_path(), &bin, "TRAE", dry_run)?,
            AgentKind::Claude => install_json_mcp(&claude_mcp_path(), &bin, "Claude Code", dry_run)?,
        }
    }
    Ok(0)
}

fn mcp_uninstall(agents: &[AgentKind], dry_run: bool) -> Result<i32> {
    let agents = resolve_agents(agents);
    for agent in agents {
        match agent {
            AgentKind::Codex => {
                let config = dirs_home().join(".codex/config.toml");
                let raw = std::fs::read_to_string(&config).unwrap_or_default();
                if !raw.contains("[mcp_servers.agentsop]") {
                    println!("Codex MCP：未配置 agentsop，无需卸载");
                    continue;
                }
                if dry_run {
                    println!("[Codex] 将执行：codex mcp remove agentsop");
                    continue;
                }
                let output = std::process::Command::new("codex")
                    .args(["mcp", "remove", "agentsop"])
                    .output()
                    .map_err(|err| {
                        AppError::new("MCP_INSTALL_FAILED", format!("无法执行 codex：{}", err))
                    })?;
                if output.status.success() {
                    println!("已移除 Codex MCP（agentsop）");
                } else {
                    return Err(AppError::new(
                        "MCP_INSTALL_FAILED",
                        String::from_utf8_lossy(&output.stderr).trim().to_string(),
                    ));
                }
            }
            AgentKind::Trae => remove_json_mcp(&trae_mcp_path(), "TRAE", dry_run)?,
            AgentKind::Claude => remove_json_mcp(&claude_mcp_path(), "Claude Code", dry_run)?,
        }
    }
    Ok(0)
}

fn mcp_configured(agent: AgentKind) -> bool {
    match agent {
        AgentKind::Codex => std::fs::read_to_string(dirs_home().join(".codex/config.toml"))
            .map(|raw| raw.contains("[mcp_servers.agentsop]"))
            .unwrap_or(false),
        AgentKind::Trae => load_settings(&trae_mcp_path())
            .get("mcpServers")
            .and_then(|s| s.get("agentsop"))
            .is_some(),
        AgentKind::Claude => load_settings(&claude_mcp_path())
            .get("mcpServers")
            .and_then(|s| s.get("agentsop"))
            .is_some(),
    }
}

fn mcp_status(agents: &[AgentKind]) -> Result<i32> {
    let agents = resolve_agents(agents);
    if agents.is_empty() {
        return Ok(2);
    }
    let mut all_ok = true;
    for agent in agents {
        let configured = mcp_configured(agent);
        let path = match agent {
            AgentKind::Codex => dirs_home().join(".codex/config.toml"),
            AgentKind::Trae => trae_mcp_path(),
            AgentKind::Claude => claude_mcp_path(),
        };
        println!(
            "{} MCP：{}（{}）",
            agent.label(),
            if configured { "已配置" } else { "未配置" },
            path.display()
        );
        if !configured {
            all_ok = false;
            println!("  运行 agentsop mcp install {}", agent.slug());
        }
    }
    Ok(if all_ok { 0 } else { 2 })
}


fn health(port: u16) -> Result<Value> {
    let token = Paths::default_handle()
        .token()
        .unwrap_or_else(|_| "missing".into());
    request(port, &token, "GET", "/v1/health", None)
}

fn call(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let port = active_port().ok_or_else(|| {
        AppError::new("APP_NOT_RUNNING", "AgentSOP 未运行：先打开应用或按 ⌥⌘S")
    })?;
    let token = Paths::default_handle().token().unwrap_or_default();
    request(port, &token, method, path, body)
}

/// 端口文件可能因重启残留，实际以能回 health 的端口为准（43117..43127）。
fn active_port() -> Option<u16> {
    let paths = Paths::default_handle();
    let mut candidates = Vec::new();
    if let Some(port) = paths.port() {
        candidates.push(port);
    }
    for offset in 0..agentsop_core::store::PORT_RANGE {
        let port = agentsop_core::store::DEFAULT_PORT.saturating_add(offset);
        if !candidates.contains(&port) {
            candidates.push(port);
        }
    }
    candidates.into_iter().find(|port| health(*port).is_ok())
}

fn request(port: u16, token: &str, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|_| AppError::new("APP_NOT_RUNNING", "AgentSOP 未运行：连接 127.0.0.1 失败"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok();

    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method,
        path,
        token,
        payload.as_bytes().len()
    )
    .into_bytes();
    req.extend_from_slice(payload.as_bytes());
    stream.write_all(&req)?;
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| AppError::new("BAD_RESPONSE", "HTTP 响应不完整"))?;
    let status_code: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);

    if status_code >= 400 {
        let message = value["message"].as_str().unwrap_or("请求失败");
        let code = value["code"].as_str().unwrap_or("HTTP_ERROR");
        return Err(AppError::new("HTTP_ERROR", format!("{} ({})", message, code)));
    }
    Ok(value)
}

fn print_help() {
    println!(
        r#"AgentSOP CLI

  agentsop                      打开面板（等价于 ⌥⌘S）
  agentsop status               当前运行的人读摘要
  agentsop json                 当前运行快照（JSON）
  agentsop next                 当前节点与默认 Prompt（给 Agent 读）
  agentsop context              渲染给 Agent 的上下文
  agentsop runs                 历史运行列表
  agentsop start <sop>          启动运行（--repo PATH，--var k=v）
  agentsop complete [node]      标记节点完成（--artifact FILE）
  agentsop decide <action>      人做决策（approve/reject/retry/allow-once/abort）
                                --node ID --note TEXT
  agentsop pause|resume|abort   运行级控制
  agentsop sop list|show|validate|install
  agentsop hook <source>        供 hook 调用，stdin 传 JSON（fail-open）
  agentsop hooks install|uninstall|status [claude-code|codex|trae|all] [--dry-run]
                                写入/移除 Agent hooks（默认按已安装情况探测）
  agentsop mcp install|uninstall|status [claude-code|codex|trae|all] [--dry-run]
                                注册/移除 AgentSOP MCP server
  agentsop doctor               自检
"#
    );
}
