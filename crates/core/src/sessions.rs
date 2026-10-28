//! Agent 会话发现：把本地能扫到的会话（Codex / Claude Code）和 hook 上报过的会话合并成一份候选列表。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::model::SessionMeta;

pub const AGENTS: [&str; 2] = ["codex", "claude-code"];

fn normalize(name: &str) -> &str {
    match name {
        "claude" => "claude-code",
        other => other,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 选中某个 Agent 后的候选会话：本地扫到的 + hook 见过的，按最近活跃排序。
pub fn list(agent: &str, known: &BTreeMap<String, SessionMeta>) -> Vec<SessionMeta> {
    let mut map: BTreeMap<String, SessionMeta> = BTreeMap::new();
    for meta in local(agent) {
        map.insert(meta.id.clone(), meta);
    }
    for meta in known.values() {
        if normalize(&meta.agent) != agent {
            continue;
        }
        match map.get_mut(&meta.id) {
            Some(existing) => {
                if !meta.title.is_empty() {
                    existing.title = meta.title.clone();
                }
                if !meta.repo.is_empty() {
                    existing.repo = meta.repo.clone();
                }
                if meta.updated_at > existing.updated_at {
                    existing.updated_at = meta.updated_at.clone();
                }
            }
            None => {
                map.insert(meta.id.clone(), meta.clone());
            }
        }
    }
    let mut all: Vec<SessionMeta> = map.into_values().collect();
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    all.truncate(30);
    all
}

fn local(agent: &str) -> Vec<SessionMeta> {
    match agent {
        "codex" => codex_sessions(),
        "claude-code" => claude_sessions(),
        _ => Vec::new(),
    }
}

/// Codex 有现成的会话索引 `~/.codex/session_index.jsonl`（id + 线程名 + 更新时间）。
fn codex_sessions() -> Vec<SessionMeta> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let index = home.join(".codex/session_index.jsonl");
    let Ok(raw) = fs::read_to_string(&index) else {
        return Vec::new();
    };
    let repos = codex_repos(&home.join(".codex/sessions"));

    let mut map: BTreeMap<String, SessionMeta> = BTreeMap::new();
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let title = value
            .get("thread_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let updated_at = value
            .get("updated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        map.insert(
            id.to_string(),
            SessionMeta {
                id: id.to_string(),
                agent: "codex".into(),
                title,
                repo: repos.get(id).cloned().unwrap_or_default(),
                created_at: updated_at.clone(),
                updated_at,
            },
        );
    }
    map.into_values().collect()
}

/// 从 rollout 文件第一行拿 session 的 cwd，只扫最近 60 天的文件。
fn codex_repos(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut files = Vec::new();
    collect_recent(root, &mut files, 0);
    for file in files.iter().take(200) {
        let Ok(text) = read_first_line(file) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let payload = value.get("payload").unwrap_or(&value);
        let id = payload.get("id").and_then(|v| v.as_str());
        let cwd = payload.get("cwd").and_then(|v| v.as_str());
        if let (Some(id), Some(cwd)) = (id, cwd) {
            out.insert(id.to_string(), cwd.to_string());
        }
    }
    out
}

fn collect_recent(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_recent(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Claude Code 把每个会话写成 `~/.claude/projects/<项目>/<session-id>.jsonl`。
fn claude_sessions() -> Vec<SessionMeta> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let root = home.join(".claude/projects");
    let Ok(projects) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    for project in projects.flatten() {
        let Ok(entries) = fs::read_dir(project.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((mtime, path));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));

    let mut out = Vec::new();
    for (mtime, path) in files.into_iter().take(30) {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if stem.is_empty() {
            continue;
        }
        let mut meta = SessionMeta {
            id: stem.clone(),
            agent: "claude-code".into(),
            title: String::new(),
            repo: String::new(),
            created_at: stamp(mtime),
            updated_at: stamp(mtime),
        };
        if let Ok(text) = read_first_line(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(id) = value.get("sessionId").and_then(|v| v.as_str()) {
                    meta.id = id.to_string();
                }
                if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()) {
                    meta.repo = cwd.to_string();
                }
            }
        }
        meta.title = claude_title(&path).unwrap_or_else(|| {
            meta.repo
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string()
        });
        out.push(meta);
    }
    out
}

fn claude_title(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines().take(40) {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(|v| v.as_str()) != Some("user") {
            continue;
        }
        let content = value.get("message").and_then(|m| m.get("content"));
        let raw = match content {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|i| i.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        let title = summarize(&raw);
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

fn summarize(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let flat = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 40 {
        flat.chars().take(39).collect::<String>() + "…"
    } else {
        flat
    }
}

fn read_first_line(path: &Path) -> std::io::Result<String> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(path)?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line)?;
    Ok(line)
}

fn stamp(time: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = time.into();
    datetime.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
