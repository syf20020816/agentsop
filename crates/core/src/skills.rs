//! 本机 skill 发现：扫描各 Agent 自己读的 skills 目录，供 Prompt 里的 `/` 补全使用。
//!
//! 目录约定（与对应 Agent 一致）：
//! - 共享: `~/.agents/skills/<name>/SKILL.md`（Codex 与 Claude Code 都会读）
//! - codex: `~/.codex/skills/<name>/SKILL.md`
//! - codex 内置: `~/.codex/skills/.system/<name>/SKILL.md`
//! - codex 插件: `~/.codex/plugins/cache/<marketplace>/<plugin>/<version>/skills/<name>/SKILL.md`
//! - claude-code: `~/.claude/skills/<name>/SKILL.md`

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

/// 共享目录里的 skill 对任何 Agent 都可用，筛 Agent 时一并保留。
const SHARED: &str = "shared";

#[derive(Debug, Clone, Serialize)]
pub struct SkillMeta {
    /// 目录名，也是 `/` 后面要输入的名字。
    pub name: String,
    pub description: String,
    pub agent: String,
}

/// 按 Agent 列出本机 skill；`agent` 为空时列出所有已支持 Agent 的 skill。
pub fn list(agent: Option<&str>) -> Vec<SkillMeta> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let home = PathBuf::from(home);
    let wanted = agent.map(|a| match a {
        "claude" => "claude-code",
        other => other,
    });

    let mut out = Vec::new();
    for name in ["codex", "claude-code"] {
        if wanted.is_some_and(|w| !w.is_empty() && w != name) {
            continue;
        }
        out.extend(scan_agent(&home, name));
    }
    // 共享目录放最后：同名时优先保留 Agent 自己的那份。
    out.extend(scan_dir(&home.join(".agents/skills"), SHARED));

    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.agent.cmp(&b.agent)));
    out.dedup_by(|a, b| a.name == b.name);
    out
}

fn scan_agent(home: &Path, agent: &str) -> Vec<SkillMeta> {
    let mut out = Vec::new();
    if agent == "codex" {
        let base = home.join(".codex/skills");
        out.extend(scan_dir(&base, agent));
        // `.system` 是隐藏目录，里面是 Codex 自带的 skill。
        out.extend(scan_dir(&base.join(".system"), agent));
        for dir in plugin_skill_dirs(&home.join(".codex/plugins/cache")) {
            out.extend(read_skill(&dir, agent));
        }
    } else {
        out.extend(scan_dir(&home.join(".claude/skills"), agent));
    }
    out
}

fn scan_dir(dir: &Path, agent: &str) -> Vec<SkillMeta> {
    subdirs(dir)
        .iter()
        .filter_map(|d| read_skill(d, agent))
        .collect()
}

/// 插件缓存是 `<marketplace>/<plugin>/<version>/skills/<skill>/SKILL.md` 四层。
fn plugin_skill_dirs(cache: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for marketplace in subdirs(cache) {
        for plugin in subdirs(&marketplace) {
            for version in subdirs(&plugin) {
                for skill in subdirs(&version.join("skills")) {
                    if skill.join("SKILL.md").is_file() {
                        out.push(skill);
                    }
                }
            }
        }
    }
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect()
}

fn read_skill(dir: &Path, agent: &str) -> Option<SkillMeta> {
    let raw = fs::read_to_string(dir.join("SKILL.md")).ok()?;
    let (_, description) = front_matter(&raw);
    Some(SkillMeta {
        // `/` 命令名以目录名为准：front matter 里的 name 可能是展示名（如 `Presentations`）。
        name: dir.file_name()?.to_string_lossy().to_string(),
        description: description.unwrap_or_default(),
        agent: agent.to_string(),
    })
}

/// 极简 YAML front matter：只取 `name` 与 `description`。
/// 同时兼容单行值、`>-` / `|` 块标量（把后续缩进行折成一行）。
fn front_matter(raw: &str) -> (Option<String>, Option<String>) {
    let all: Vec<&str> = raw.lines().collect();
    if all.first().map(|l| l.trim()) != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    let mut i = 1;
    while i < all.len() {
        let line = all[i];
        if line.trim() == "---" {
            break;
        }
        // 只认顶格写的 key，缩进行属于上一项的块标量。
        let is_top = !line.starts_with(' ') && !line.starts_with('\t');
        let Some((key, value)) = line.split_once(':').filter(|_| is_top) else {
            i += 1;
            continue;
        };
        let key = key.trim();
        let mut parts: Vec<String> = Vec::new();
        let head = clean(value);
        if !head.is_empty() {
            parts.push(head);
        }
        let mut j = i + 1;
        while j < all.len() {
            let next = all[j];
            if next.trim() == "---" || (!next.trim().is_empty() && !next.starts_with([' ', '\t'])) {
                break;
            }
            let text = next.trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
            j += 1;
        }
        match key {
            "name" => name = Some(parts.join(" ")),
            "description" => description = Some(parts.join(" ")),
            _ => {}
        }
        i = j.max(i + 1);
    }
    (name, description)
}

fn clean(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('>')
        .trim_start_matches('|')
        .trim_start_matches('-')
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}
