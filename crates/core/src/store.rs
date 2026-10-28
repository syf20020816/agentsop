//! `~/.agentsop` 目录、token、端口文件、事件与快照的落盘。

use crate::error::{AppError, Result};
use crate::model::{Event, Run, SessionMeta};
use crate::sop;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const DEFAULT_PORT: u16 = 43117;
pub const PORT_RANGE: u16 = 10;

#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Paths { root: root.into() }
    }

    pub fn default_root() -> PathBuf {
        match std::env::var("AGENTSOP_HOME") {
            Ok(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".agentsop")
            }
        }
    }

    pub fn default_handle() -> Self {
        Paths::new(Self::default_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sops_dir(&self) -> PathBuf {
        self.root.join("sops")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn token_path(&self) -> PathBuf {
        self.root.join("token")
    }

    pub fn port_path(&self) -> PathBuf {
        self.root.join("run").join("port")
    }

    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.runs_dir().join(run_id)
    }

    pub fn log_path(&self) -> PathBuf {
        self.root.join("agentsop.log")
    }

    /// 初始化目录与 token；不预置任何 SOP，由用户自己创建。
    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(self.sops_dir())?;
        fs::create_dir_all(self.runs_dir())?;
        fs::create_dir_all(self.root.join("run"))?;
        fs::create_dir_all(self.root.join("logs"))?;
        self.ensure_token()?;
        Ok(())
    }

    pub fn ensure_token(&self) -> Result<String> {
        if let Ok(token) = self.token() {
            return Ok(token);
        }
        let mut buf = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut buf)?;
        let token: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
        atomic_write(&self.token_path(), token.as_bytes())?;
        fs::set_permissions(self.token_path(), fs::Permissions::from_mode(0o600))?;
        Ok(token)
    }

    pub fn token(&self) -> Result<String> {
        let raw = fs::read_to_string(self.token_path())?;
        let token = raw.trim().to_string();
        if token.is_empty() {
            return Err(AppError::new("TOKEN_MISSING", "token 文件为空"));
        }
        Ok(token)
    }

    pub fn port(&self) -> Option<u16> {
        fs::read_to_string(self.port_path())
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    pub fn set_port(&self, port: u16) -> Result<()> {
        atomic_write(&self.port_path(), port.to_string().as_bytes())
    }

    // ------------------------------------------------------------ runs

    pub fn list_runs(&self) -> Vec<String> {
        let mut runs: Vec<String> = fs::read_dir(self.runs_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        runs.sort();
        runs.reverse();
        runs
    }

    pub fn append_event(&self, event: &Event) -> Result<()> {
        let dir = self.run_dir(&event.run_id);
        fs::create_dir_all(&dir)?;
        let path = dir.join("events.jsonl");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        let line = serde_json::to_string(event)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    pub fn read_events(&self, run_id: &str, since: Option<u64>) -> Result<Vec<Event>> {
        let path = self.run_dir(run_id).join("events.jsonl");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(path)?;
        let mut out = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(line) {
                Ok(ev) if since.is_none_or(|s| ev.seq > s) => out.push(ev),
                Ok(_) => {}
                Err(_) => continue,
            }
        }
        Ok(out)
    }

    pub fn write_snapshot(&self, run: &Run) -> Result<()> {
        let dir = self.run_dir(&run.run_id);
        fs::create_dir_all(&dir)?;
        let bytes = serde_json::to_vec_pretty(run)?;
        atomic_write(&dir.join("snapshot.json"), &bytes)
    }

    pub fn read_run(&self, run_id: &str) -> Result<Option<Run>> {
        let path = self.run_dir(run_id).join("snapshot.json");
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path)?;
        let run = serde_json::from_str::<Run>(&raw)?;
        Ok(Some(run))
    }

    // ------------------------------------------------------------ sessions

    /// 全局会话登记表：跨运行记录 hook 上报过的会话（带可读标题）。
    pub fn sessions_path(&self) -> PathBuf {
        self.root.join("sessions.json")
    }

    pub fn read_sessions(&self) -> BTreeMap<String, SessionMeta> {
        let Ok(raw) = fs::read_to_string(self.sessions_path()) else {
            return BTreeMap::new();
        };
        serde_json::from_str::<Vec<SessionMeta>>(&raw)
            .map(|list| list.into_iter().map(|m| (m.id.clone(), m)).collect())
            .unwrap_or_default()
    }

    pub fn write_sessions(&self, sessions: &BTreeMap<String, SessionMeta>) -> Result<()> {
        let list: Vec<&SessionMeta> = sessions.values().collect();
        let bytes = serde_json::to_vec_pretty(&list)?;
        atomic_write(&self.sessions_path(), &bytes)
    }

    pub fn write_context(&self, run_id: &str, markdown: &str) -> Result<()> {
        let dir = self.run_dir(run_id);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir.join("context.md"), markdown.as_bytes())
    }

    // ------------------------------------------------------------ sops

    pub fn list_sops(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.sops_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let path = e.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("yaml") {
                            path.file_stem().map(|s| s.to_string_lossy().to_string())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn load_sop(&self, name: &str) -> Result<crate::model::Sop> {
        let path = self.sops_dir().join(format!("{}.yaml", name));
        if !path.exists() {
            // 允许传绝对路径
            let alt = PathBuf::from(name);
            if alt.exists() {
                let raw = fs::read_to_string(alt)?;
                return sop::parse(&raw);
            }
            return Err(AppError::not_found(format!("SOP「{}」不存在", name)));
        }
        let raw = fs::read_to_string(path)?;
        let mut parsed = sop::parse(&raw)?;
        // 唯一标识统一用文件名，避免 YAML 里的 sop 字段改名后与文件名不一致。
        parsed.sop = name.to_string();
        Ok(parsed)
    }

    pub fn delete_sop(&self, name: &str) -> Result<()> {
        let path = self.sops_dir().join(format!("{}.yaml", name));
        fs::remove_file(path)?;
        Ok(())
    }

    pub fn save_sop(&self, name: &str, yaml: &str) -> Result<()> {
        sop::parse(yaml)?;
        let path = self.sops_dir().join(format!("{}.yaml", name));
        atomic_write(&path, yaml.as_bytes())
    }

    pub fn read_sop_yaml(&self, name: &str) -> Result<String> {
        let path = self.sops_dir().join(format!("{}.yaml", name));
        Ok(fs::read_to_string(path)?)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn vars_from_pairs(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    pairs.iter().cloned().collect()
}
