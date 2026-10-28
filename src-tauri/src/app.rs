//! Tauri 外壳：无边框悬浮面板、全局快捷键、托盘与 HTTP 服务生命周期。

use crate::server::{self, AppState};
use agentsop_core::{DecisionAction, Evidence, Kernel, Paths, SessionMeta, Snapshot};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tauri::{
    AppHandle, Emitter, Manager, WebviewWindow, WindowEvent,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const PANEL_WIDTH: f64 = 180.0;
const PANEL_HEIGHT: f64 = 560.0;
const RIGHT_MARGIN: f64 = 24.0;
const SNAPSHOT_EVENT: &str = "agentsop://snapshot";

#[derive(Clone)]
struct PanelState {
    kernel: Arc<Mutex<Kernel>>,
    events: tokio::sync::broadcast::Sender<Snapshot>,
}

pub fn run() {
    let paths = Paths::default_handle();
    let kernel = match Kernel::boot(paths.clone()) {
        Ok(k) => Arc::new(Mutex::new(k)),
        Err(err) => {
            eprintln!("AgentSOP 启动失败：{}", err);
            std::process::exit(1);
        }
    };

    let token = kernel
        .lock()
        .expect("kernel")
        .paths()
        .ensure_token()
        .unwrap_or_default();

    let app_state = AppState::new(kernel.clone(), token.clone());
    let events = app_state.events.clone();
    let panel_state = PanelState {
        kernel: kernel.clone(),
        events,
    };

    // HTTP 服务独立线程 + 独立 runtime：与 UI 生命周期解耦，但由本进程托管。
    {
        let state = app_state.clone();
        let kernel_ref = kernel.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                if let Err(err) = server::serve(state, kernel_ref).await {
                    eprintln!("AgentSOP HTTP 服务异常：{}", err);
                }
            });
        });
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_panel(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_panel(app);
                    }
                })
                .build(),
        )
        .manage(panel_state)
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            build_tray(app.handle())?;

            if let Some(window) = app.get_webview_window("panel") {
                style_panel(&window);
                place_panel(&window);
            }

            app.global_shortcut()
                .register("Alt+Super+KeyS")
                .map_err(|e| format!("注册快捷键失败：{}", e))?;

            spawn_snapshot_bridge(app.handle().clone());
            spawn_stale_sweeper(app.handle().clone());

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            snapshot,
            decide,
            complete_node,
            start_run,
            bind_session,
            new_session,
            list_agent_sessions,
            list_skills,
            open_agent_task,
            fill_prompt,
            submit_prompt,
            save_sop,
            save_sop_json,
            get_sop,
            delete_sop,
            list_sops,
            context_markdown,
            hide_panel,
            open_state_dir,
            reveal_terminal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AgentSOP");
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开面板  ⌥⌘S", true, None::<&str>)?;
    let state = MenuItem::with_id(app, "state", "打开状态目录", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 AgentSOP", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &state,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("agentsop")
        .tooltip("AgentSOP")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_panel(app),
            "state" => {
                let _ = std::process::Command::new("open")
                    .arg(Paths::default_root())
                    .spawn();
            }
            "quit" => app.exit(0),
            _ => {}
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn style_panel(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState, apply_vibrancy};
        let _ = apply_vibrancy(
            window,
            NSVisualEffectMaterial::HudWindow,
            Some(NSVisualEffectState::Active),
            Some(12.0),
        );
    }
    let _ = window.set_always_on_top(true);
    let _ = window.set_visible_on_all_workspaces(true);
}

/// right center：右边距 24PX，垂直居中。
fn place_panel(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let scale = monitor.scale_factor();
    let size = monitor.size();
    let pos = monitor.position();
    let width = PANEL_WIDTH * scale;
    let height = PANEL_HEIGHT * scale;
    let x = pos.x as f64 + size.width as f64 - width - RIGHT_MARGIN * scale;
    let y = pos.y as f64 + (size.height as f64 - height) / 2.0;
    let _ = window.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
}

fn toggle_panel(app: &AppHandle) {
    let Some(window) = app.get_webview_window("panel") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        show_panel(app);
    }
}

fn show_panel(app: &AppHandle) {
    let Some(window) = app.get_webview_window("panel") else {
        return;
    };
    place_panel(&window);
    let _ = window.show();
    let _ = window.set_focus();
}

/// 把内核变更（HTTP / IPC 任一来源）推给前端。
fn spawn_snapshot_bridge(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<PanelState>();
        let mut rx = state.events.subscribe();
        while let Ok(snap) = rx.recv().await {
            let _ = app.emit(SNAPSHOT_EVENT, snap);
        }
    });
}

/// 每 60s 把超时节点标记为 stale（默认 30 分钟未更新）。
fn spawn_stale_sweeper(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let state = app.state::<PanelState>();
            let changed = {
                let mut kernel = state.kernel.lock().expect("kernel");
                kernel.sweep_stale(30).unwrap_or_default()
            };
            for snap in changed {
                let _ = app.emit(SNAPSHOT_EVENT, snap);
            }
        }
    });
}

// ---------------------------------------------------------------- IPC

fn err_str(err: agentsop_core::AppError) -> String {
    serde_json::to_string(&err.to_json()).unwrap_or_else(|_| err.message)
}

#[tauri::command]
fn bootstrap(state: tauri::State<'_, PanelState>) -> Result<Value, String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    let runs = kernel.all_snapshots();
    // 被中止的运行不再占用面板：回到空态，方便直接绑定下一条 SOP。
    let current = runs
        .iter()
        .find(|r| r.run_status != agentsop_core::RunStatus::Aborted)
        .cloned();
    let sops: Vec<Value> = kernel
        .paths()
        .list_sops()
        .into_iter()
        .filter_map(|name| kernel.paths().load_sop(&name).ok().map(|sop| (name, sop)))
        .map(|(name, sop)| {
            json!({
                // 以文件名作为 SOP 的唯一标识，避免 YAML 里改名后找不到文件。
                "name": name,
                "title": sop.display_title(),
                "version": sop.version,
                "nodes": sop.nodes.len(),
            })
        })
        .collect();
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "port": kernel.paths().port(),
        "run": current,
        "runs": runs,
        "sops": sops,
    }))
}

#[tauri::command]
fn snapshot(state: tauri::State<'_, PanelState>, run_id: Option<String>) -> Result<Option<Snapshot>, String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    let id = run_id.or_else(|| kernel.active_run_id());
    match id {
        Some(id) => kernel.snapshot(&id).map(Some).map_err(err_str),
        None => Ok(None),
    }
}

#[tauri::command]
fn decide(
    app: AppHandle,
    state: tauri::State<'_, PanelState>,
    run_id: String,
    node_id: String,
    action: DecisionAction,
    note: Option<String>,
) -> Result<Snapshot, String> {
    let snap = {
        let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
        kernel
            .decide(&run_id, &node_id, action, note.as_deref().unwrap_or(""))
            .map_err(err_str)?
    };
    let _ = app.emit(SNAPSHOT_EVENT, &snap);
    Ok(snap)
}

#[tauri::command]
fn complete_node(
    app: AppHandle,
    state: tauri::State<'_, PanelState>,
    run_id: String,
    node_id: String,
    evidence: Option<Vec<Evidence>>,
) -> Result<Snapshot, String> {
    let snap = {
        let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
        kernel
            .complete_node(&run_id, &node_id, evidence.unwrap_or_default())
            .map_err(err_str)?
    };
    let _ = app.emit(SNAPSHOT_EVENT, &snap);
    Ok(snap)
}

#[tauri::command]
fn start_run(
    app: AppHandle,
    state: tauri::State<'_, PanelState>,
    sop: String,
    repo: String,
    vars: Option<std::collections::BTreeMap<String, String>>,
    agent: Option<String>,
    session_id: Option<String>,
) -> Result<Snapshot, String> {
    let snap = {
        let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
        kernel
            .start_run_bound(
                &sop,
                &repo,
                vars.unwrap_or_default(),
                agent.as_deref(),
                session_id.as_deref(),
            )
            .map_err(err_str)?
    };
    let _ = app.emit(SNAPSHOT_EVENT, &snap);
    Ok(snap)
}

/// 空态里选 Agent 后列出可绑定的会话：本地能扫到的（Codex/Claude）+ hook 见过的。
#[tauri::command]
fn list_agent_sessions(
    state: tauri::State<'_, PanelState>,
    agent: String,
) -> Result<Vec<SessionMeta>, String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    Ok(agentsop_core::sessions::list(&agent, &kernel.session_registry()))
}

/// 编辑器里默认 Prompt 的 `/` 补全：列出该 Agent 本机安装的 skill。
#[tauri::command]
fn list_skills(agent: Option<String>) -> Vec<agentsop_core::skills::SkillMeta> {
    agentsop_core::skills::list(agent.as_deref())
}

/// 空态里「新建会话」：只唤起 Agent，让用户在里面开一个新任务。
#[tauri::command]
fn open_agent_task(agent: String) -> Result<(), String> {
    launch_agent(&agent, "~");
    Ok(())
}

#[tauri::command]
fn bind_session(
    app: AppHandle,
    state: tauri::State<'_, PanelState>,
    run_id: String,
    session_id: String,
) -> Result<Snapshot, String> {
    let snap = {
        let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
        kernel
            .bind_session(&run_id, &session_id)
            .map_err(err_str)?
    };
    let _ = app.emit(SNAPSHOT_EVENT, &snap);
    Ok(snap)
}

/// 「+」：让 Agent 新建一个任务。
/// - 还没开始推进：只唤起 Agent，新任务出现在会话下拉里由用户自己选绑定。
/// - 已经开始推进：以同一 SOP/仓库开一次新运行（绑定已锁定），新任务接入后自动绑定。
#[tauri::command]
fn new_session(
    app: AppHandle,
    state: tauri::State<'_, PanelState>,
    run_id: String,
) -> Result<Snapshot, String> {
    let (current, sop_name, repo, agent) = {
        let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
        let snap = kernel.snapshot(&run_id).map_err(err_str)?;
        let agent = snap
            .nodes
            .iter()
            .find(|n| Some(&n.id) == snap.current_node.as_ref())
            .and_then(|n| n.agent.clone())
            .unwrap_or_default();
        let run = kernel.run(&run_id).map_err(err_str)?;
        (snap, run.sop.sop.clone(), run.repo.clone(), agent)
    };

    if !current.session_locked {
        launch_agent(&agent, &repo);
        return Ok(current);
    }

    let snap = {
        let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
        kernel
            .start_run_bound(
                &sop_name,
                &repo,
                std::collections::BTreeMap::new(),
                Some(&agent),
                None,
            )
            .map_err(err_str)?
    };
    let _ = app.emit(SNAPSHOT_EVENT, &snap);
    launch_agent(&agent, &repo);
    Ok(snap)
}

/// 尽力打开对应 Agent 的新会话；做不到就静默失败，由“新运行等接入”兜底。
fn launch_agent(agent: &str, repo: &str) {
    match agent {
        "trae" | "trae-cn" => {
            let _ = std::process::Command::new("open")
                .args(["-a", "Trae CN"])
                .spawn();
        }
        // Codex 用桌面版（Codex 界面）：CLI 只在终端里跑，不符合“在 Codex 里执行”。
        "codex" => {
            if !activate_app(CODEX_BUNDLE) {
                let _ = launch_terminal(repo, "codex");
            }
        }
        "claude-code" | "claude" => {
            if !activate_app(CLAUDE_BUNDLE) {
                let _ = launch_terminal(repo, "claude");
            }
        }
        _ => {}
    }
}

/// 桌面向 Agent 的 Bundle ID；`open -b` 会把已在运行的 App 拉到前台。
const CODEX_BUNDLE: &str = "com.openai.codex";
const CLAUDE_BUNDLE: &str = "com.anthropic.claudefordesktop";

fn activate_app(bundle: &str) -> bool {
    std::process::Command::new("open")
        .args(["-b", bundle])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_osascript(script: &str) -> bool {
    std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() else {
        return false;
    };
    if let Some(stdin) = child.stdin.as_mut() {
        if stdin.write_all(text.as_bytes()).is_err() {
            return false;
        }
    }
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// 把节点 Prompt 填进 Agent 输入框：剪贴板 → 激活 App → ⌘V。
/// 返回 false 表示没粘进去（通常是没给 AgentSOP 开「辅助功能」），内容已留在剪贴板里。
#[tauri::command]
async fn fill_prompt(text: String, agent: Option<String>) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if !copy_to_clipboard(&text) {
            return Err("写剪贴板失败".to_string());
        }
        let bundle = bundle_for(agent.as_deref());
        if !bundle.is_empty() && !activate_app(bundle) {
            return Ok(false);
        }
        // 等 App 真正拿到焦点，再发 ⌘V。
        std::thread::sleep(std::time::Duration::from_millis(260));
        Ok(run_osascript(
            "tell application \"System Events\" to keystroke \"v\" using command down",
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 在 Agent 输入框里按下回车，送出已经填好的 Prompt。
/// 先激活 Agent，避免焦点还在 AgentSOP 时把回车打给自己。
#[tauri::command]
async fn submit_prompt(agent: Option<String>) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let bundle = bundle_for(agent.as_deref());
        if !bundle.is_empty() && !activate_app(bundle) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(180));
        run_osascript("tell application \"System Events\" to key code 36")
    })
    .await
    .map_err(|e| e.to_string())
}

fn bundle_for(agent: Option<&str>) -> &'static str {
    match agent {
        Some("codex") => CODEX_BUNDLE,
        Some("claude-code") | Some("claude") => CLAUDE_BUNDLE,
        _ => "",
    }
}

fn launch_terminal(repo: &str, command: &str) -> bool {
    let escaped = repo.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "tell application \"Terminal\" to do script \"cd {} && {}\"",
        escaped, command
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[tauri::command]
fn save_sop(
    state: tauri::State<'_, PanelState>,
    name: String,
    yaml: String,
) -> Result<Value, String> {
    let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    kernel.paths().save_sop(&name, &yaml).map_err(err_str)?;
    let runs = kernel.reload_sop(&name, &name).map_err(err_str)?;
    Ok(json!({ "ok": true, "runs": runs }))
}

#[tauri::command]
fn get_sop(state: tauri::State<'_, PanelState>, name: String) -> Result<Value, String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    let yaml = kernel.paths().read_sop_yaml(&name).map_err(err_str)?;
    let sop = kernel.paths().load_sop(&name).map_err(err_str)?;
    Ok(json!({ "name": sop.sop, "title": sop.display_title(), "yaml": yaml, "sop": sop }))
}

#[tauri::command]
fn save_sop_json(
    state: tauri::State<'_, PanelState>,
    name: String,
    sop: Value,
    prev: Option<String>,
) -> Result<Value, String> {
    let parsed: agentsop_core::model::Sop =
        serde_json::from_value(sop).map_err(|e| format!("字段不完整：{}", e))?;
    agentsop_core::sop::validate(&parsed).map_err(err_str)?;
    let yaml = serde_yaml::to_string(&parsed).map_err(|e| e.to_string())?;
    let mut kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    kernel.paths().save_sop(&name, &yaml).map_err(err_str)?;
    // 改名时运行里记的还是旧名，两个名字都要匹配。
    let from = prev.filter(|p| !p.is_empty()).unwrap_or_else(|| name.clone());
    let runs = kernel.reload_sop(&from, &name).map_err(err_str)?;
    Ok(json!({ "ok": true, "yaml": yaml, "runs": runs }))
}

/// 编辑器里重命名 SOP 后删掉旧文件，避免留下重复条目。
#[tauri::command]
fn delete_sop(state: tauri::State<'_, PanelState>, name: String) -> Result<(), String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    kernel.paths().delete_sop(&name).map_err(err_str)
}

#[tauri::command]
fn list_sops(state: tauri::State<'_, PanelState>) -> Result<Value, String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    let sops: Vec<String> = kernel.paths().list_sops();
    Ok(json!({ "sops": sops }))
}

#[tauri::command]
fn context_markdown(state: tauri::State<'_, PanelState>, run_id: String) -> Result<String, String> {
    let kernel = state.kernel.lock().map_err(|e| e.to_string())?;
    kernel.context_md(&run_id).map_err(err_str)
}

#[tauri::command]
fn hide_panel(app: AppHandle) {
    if let Some(window) = app.get_webview_window("panel") {
        let _ = window.hide();
    }
}

#[tauri::command]
fn open_state_dir() -> Result<(), String> {
    std::process::Command::new("open")
        .arg(Paths::default_root())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_terminal(repo: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-a")
        .arg("Terminal")
        .arg(repo)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
