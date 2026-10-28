//! agentsop-mcp：stdio JSON-RPC MCP server。
//!
//! 独立进程运行（Agent 会按需 spawn），只做本地 HTTP 薄客户端：
//! 状态读写全部交给 GUI 里的 Kernel，服务不可达时返回 SERVICE_DOWN 而不崩溃。

use agentsop_core::store::{Paths, DEFAULT_PORT, PORT_RANGE};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SERVER_NAME: &str = "agentsop-mcp";
const PROTOCOL_VERSION: &str = "2024-11-05";
const PROBE_TIMEOUT_MS: u64 = 400;
const CALL_TIMEOUT_SECS: u64 = 10;
const DECISION_TIMEOUT_SECS: u64 = 90;

static PORT_CACHE: Mutex<Option<u16>> = Mutex::new(None);

fn main() {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                eprintln!("[agentsop-mcp] stdin 读取失败：{}", err);
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(id) = msg.get("id").cloned() else {
            continue; // 通知不需要响应
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

        let reply = match method {
            "initialize" => Ok(initialize()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_defs() })),
            "tools/call" => tools_call(&params),
            "resources/list" => Ok(resources_list()),
            "resources/read" => resources_read(&params),
            other => Err(RpcError {
                code: -32601,
                message: format!("未知方法 {}", other),
            }),
        };

        let payload = match reply {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": err.code, "message": err.message }
            }),
        };
        if writeln!(out, "{}", serde_json::to_string(&payload).unwrap_or_default()).is_err() {
            break;
        }
        let _ = out.flush();
    }
}

struct RpcError {
    code: i64,
    message: String,
}

fn initialize() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false }
        },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

// ---------------------------------------------------------------- tools

fn tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "sop_status",
            "description": "查看当前运行与节点状态（含默认 Prompt、待决策问题、拦截原因）。Agent 每轮开始可先调用自检。",
            "inputSchema": {
                "type": "object",
                "properties": { "run_id": { "type": "string", "description": "可选；不传则用当前活跃运行" } }
            }
        }),
        json!({
            "name": "sop_context",
            "description": "拉取渲染给 Agent 的 Markdown 上下文（人的决定、当前节点默认 Prompt、完成条件）。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": { "type": "string" },
                    "max_bytes": { "type": "integer", "description": "默认 1200" }
                }
            }
        }),
        json!({
            "name": "sop_next",
            "description": "取当前节点（含 default_prompt）与下一个节点，直接照着默认 Prompt 继续工作。",
            "inputSchema": {
                "type": "object",
                "properties": { "run_id": { "type": "string" } }
            }
        }),
        json!({
            "name": "sop_list_nodes",
            "description": "列出全部节点及状态，用于规划下一步。",
            "inputSchema": {
                "type": "object",
                "properties": { "run_id": { "type": "string" } }
            }
        }),
        json!({
            "name": "sop_complete_node",
            "description": "在没有 Stop Hook 的 Agent 里显式标记节点完成，并可附带文件证据。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "string" },
                    "run_id": { "type": "string" },
                    "evidence": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "kind": { "type": "string" },
                                "label": { "type": "string" },
                                "path": { "type": "string" }
                            },
                            "required": ["label"]
                        }
                    }
                },
                "required": ["node_id"]
            }
        }),
        json!({
            "name": "sop_request_decision",
            "description": "请人在面板上做决策。blocking=true 时长轮询最多 90 秒，人一旦回答立即返回 approved/rejected；超时返回 pending，可稍后再查。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string" },
                    "options": { "type": "array", "items": { "type": "string" } },
                    "node_id": { "type": "string" },
                    "run_id": { "type": "string" },
                    "blocking": { "type": "boolean", "description": "默认 true" }
                },
                "required": ["question"]
            }
        }),
        json!({
            "name": "sop_list_runs",
            "description": "列出历史运行与当前节点，用于选择目标 run。",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

struct ToolError {
    code: String,
    message: String,
}

impl ToolError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        ToolError {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

fn tools_call(params: &Value) -> Result<Value, RpcError> {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
    let result = match name {
        "sop_status" => tool_status(&args),
        "sop_context" => tool_context(&args),
        "sop_next" => tool_next(&args),
        "sop_list_nodes" => tool_list_nodes(&args),
        "sop_complete_node" => tool_complete(&args),
        "sop_request_decision" => tool_request_decision(&args),
        "sop_list_runs" => tool_list_runs(),
        other => Err(ToolError::new("UNKNOWN_TOOL", format!("未知工具 {}", other))),
    };
    match result {
        Ok(value) => Ok(json!({
            "content": [{ "type": "text", "text": pretty(&value) }],
            "isError": false
        })),
        Err(err) => Ok(json!({
            "content": [{
                "type": "text",
                "text": pretty(&json!({ "ok": false, "code": err.code, "message": err.message }))
            }],
            "isError": true
        })),
    }
}

fn arg_run<'a>(args: &'a Value) -> Option<&'a str> {
    args.get("run_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn tool_status(args: &Value) -> Result<Value, ToolError> {
    let snap = snapshot(arg_run(args))?;
    Ok(json!({
        "run_id": snap["run_id"],
        "sop": snap["sop"],
        "run_status": snap["run_status"],
        "current_node": snap["current_node"],
        "nodes": snap["nodes"],
        "last_decision": snap.get("last_decision").cloned().unwrap_or(Value::Null),
    }))
}

fn tool_context(args: &Value) -> Result<Value, ToolError> {
    let max_bytes = args
        .get("max_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(1200) as usize;
    let mut path = "/v1/context".to_string();
    if let Some(run) = arg_run(args) {
        path.push_str("?run_id=");
        path.push_str(run);
    }
    let value = http("GET", &path, None).map_err(tool_err)?;
    let markdown = value["markdown"].as_str().unwrap_or("").to_string();
    Ok(json!({
        "run_id": value["run_id"],
        "markdown": truncate_utf8(&markdown, max_bytes)
    }))
}

fn tool_next(args: &Value) -> Result<Value, ToolError> {
    let snap = snapshot(arg_run(args))?;
    let current = snap["current_node"].as_str().unwrap_or("");
    let nodes = snap["nodes"].as_array().cloned().unwrap_or_default();
    if current.is_empty() {
        return Ok(json!({ "run_id": snap["run_id"], "current": Value::Null, "next": Value::Null }));
    }
    let index = nodes.iter().position(|n| n["id"] == current);
    let current_node = index.map(|i| &nodes[i]);
    let next_node = index
        .and_then(|i| nodes.get(i + 1))
        .map(|n| json!({ "id": n["id"], "title": n["title"], "status": n["status"] }));
    Ok(json!({
        "run_id": snap["run_id"],
        "current": current_node.map(|n| json!({
            "id": n["id"],
            "idx": n["idx"],
            "title": n["title"],
            "type": n["type"],
            "status": n["status"],
            "default_prompt": n["default_prompt"],
            "prompt": n["prompt"],
            "pending_decision": n["pending_decision"],
        })).unwrap_or(Value::Null),
        "next": next_node,
    }))
}

fn tool_list_nodes(args: &Value) -> Result<Value, ToolError> {
    let snap = snapshot(arg_run(args))?;
    let nodes: Vec<Value> = snap["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|n| {
            json!({
                "id": n["id"],
                "idx": n["idx"],
                "title": n["title"],
                "type": n["type"],
                "status": n["status"],
                "default_prompt": n["default_prompt"],
            })
        })
        .collect();
    Ok(json!({
        "run_id": snap["run_id"],
        "current_node": snap["current_node"],
        "nodes": nodes,
    }))
}

fn tool_complete(args: &Value) -> Result<Value, ToolError> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::new("INVALID_ARGUMENT", "缺少 node_id"))?;
    let evidence: Vec<Value> = args
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    if let Some(s) = item.as_str() {
                        json!({ "kind": "file", "label": s, "path": s })
                    } else {
                        json!({
                            "kind": item.get("kind").and_then(|v| v.as_str()).unwrap_or("file"),
                            "label": item.get("label").and_then(|v| v.as_str()).unwrap_or("evidence"),
                            "path": item.get("path").cloned().unwrap_or(Value::Null),
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let body = json!({ "run_id": arg_run(args), "evidence": evidence });
    let snap = http("POST", &format!("/v1/nodes/{}/complete", node_id), Some(body))
        .map_err(tool_err)?;
    Ok(json!({ "run_id": snap["run_id"], "current_node": snap["current_node"], "run_status": snap["run_status"] }))
}

fn tool_request_decision(args: &Value) -> Result<Value, ToolError> {
    let question = args
        .get("question")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::new("INVALID_ARGUMENT", "缺少 question"))?;
    let blocking = args.get("blocking").and_then(|v| v.as_bool()).unwrap_or(true);
    let body = json!({
        "run_id": arg_run(args),
        "node_id": args.get("node_id").cloned().unwrap_or(Value::Null),
        "question": question,
        "options": args.get("options").cloned().unwrap_or(Value::Null),
    });
    let snap = http("POST", "/v1/decisions/request", Some(body)).map_err(tool_err)?;
    let run_id = snap["run_id"].as_str().unwrap_or("").to_string();
    let node_id = snap["current_node"].as_str().unwrap_or("").to_string();
    let ticket = format!("{}:{}:{}", run_id, node_id, snap["seq"].as_u64().unwrap_or(0));
    if !blocking {
        return Ok(json!({ "ticket": ticket, "status": "pending" }));
    }

    let deadline = Instant::now() + Duration::from_secs(DECISION_TIMEOUT_SECS);
    loop {
        std::thread::sleep(Duration::from_millis(800));
        let latest = snapshot(Some(&run_id))?;
        let still_waiting = latest["nodes"]
            .as_array()
            .and_then(|nodes| nodes.iter().find(|n| n["id"] == node_id.as_str()))
            .map(|n| n["status"] == "waiting_human")
            .unwrap_or(false);
        if !still_waiting {
            let decision = latest.get("last_decision").cloned().unwrap_or(Value::Null);
            let action = decision["action"].as_str().unwrap_or("");
            let status = if action == "reject" { "rejected" } else { "approved" };
            return Ok(json!({
                "ticket": ticket,
                "status": status,
                "note": decision["note"],
                "node_id": node_id,
            }));
        }
        if Instant::now() >= deadline {
            return Ok(json!({
                "ticket": ticket,
                "status": "pending",
                "hint": "人还未决策；可稍后再次调用 sop_request_decision 或 sop_status 轮询"
            }));
        }
    }
}

fn tool_list_runs() -> Result<Value, ToolError> {
    let value = http("GET", "/v1/runs", None).map_err(tool_err)?;
    let runs: Vec<Value> = value["runs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            json!({
                "run_id": r["run_id"],
                "sop": r["sop"]["title"],
                "run_status": r["run_status"],
                "current_node": r["current_node"],
                "updated_at": r["updated_at"],
            })
        })
        .collect();
    Ok(json!({ "runs": runs }))
}

// ---------------------------------------------------------------- resources

fn resources_list() -> Value {
    let mut resources = Vec::new();
    if let Ok(value) = http("GET", "/v1/runs", None) {
        for run in value["runs"].as_array().cloned().unwrap_or_default() {
            let run_id = run["run_id"].as_str().unwrap_or("");
            resources.push(json!({
                "uri": format!("agentsop://run/{}/context", run_id),
                "name": format!("{} 上下文", run_id),
                "mimeType": "text/markdown"
            }));
            resources.push(json!({
                "uri": format!("agentsop://run/{}/snapshot", run_id),
                "name": format!("{} 快照", run_id),
                "mimeType": "application/json"
            }));
        }
    }
    if let Ok(value) = http("GET", "/v1/sops", None) {
        for sop in value["sops"].as_array().cloned().unwrap_or_default() {
            let name = sop["name"].as_str().unwrap_or("");
            resources.push(json!({
                "uri": format!("agentsop://sop/{}", name),
                "name": format!("SOP {}", sop["title"].as_str().unwrap_or(name)),
                "mimeType": "application/yaml"
            }));
        }
    }
    json!({ "resources": resources })
}

fn rpc_err(err: HttpError) -> RpcError {
    RpcError {
        code: -32603,
        message: match err {
            HttpError::Connect => "AgentSOP 未运行（SERVICE_DOWN）".to_string(),
            HttpError::Api { message, .. } => message,
        },
    }
}

fn resources_read(params: &Value) -> Result<Value, RpcError> {
    let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
    let contents = if let Some(rest) = uri.strip_prefix("agentsop://run/") {
        let (run_id, kind) = rest
            .split_once('/')
            .ok_or_else(|| RpcError { code: -32602, message: "资源 URI 非法".into() })?;
        match kind {
            "context" => {
                let value = http("GET", &format!("/v1/context?run_id={}", run_id), None)
                    .map_err(rpc_err)?;
                json!({
                    "uri": uri,
                    "mimeType": "text/markdown",
                    "text": value["markdown"].as_str().unwrap_or("")
                })
            }
            "snapshot" => {
                let value = http("GET", &format!("/v1/snapshot?run_id={}", run_id), None)
                    .map_err(rpc_err)?;
                json!({
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": pretty(&value)
                })
            }
            _ => return Err(RpcError { code: -32602, message: "未知资源类型".into() }),
        }
    } else if let Some(name) = uri.strip_prefix("agentsop://sop/") {
        let value = http("GET", &format!("/v1/sops/{}", name), None)
            .map_err(rpc_err)?;
        json!({
            "uri": uri,
            "mimeType": "application/yaml",
            "text": value["yaml"].as_str().unwrap_or("")
        })
    } else {
        return Err(RpcError { code: -32602, message: "未知资源 URI".into() });
    };
    Ok(json!({ "contents": [contents] }))
}

// ---------------------------------------------------------------- HTTP

fn snapshot(run_id: Option<&str>) -> Result<Value, ToolError> {
    let path = match run_id {
        Some(id) => format!("/v1/snapshot?run_id={}", id),
        None => "/v1/snapshot".to_string(),
    };
    http("GET", &path, None).map_err(tool_err)
}

fn tool_err(err: HttpError) -> ToolError {
    match err {
        HttpError::Connect => ToolError::new(
            "SERVICE_DOWN",
            "AgentSOP 未运行：请先打开应用或按 ⌥⌘S",
        ),
        HttpError::Api { code, message, .. } => ToolError::new(&code, message),
    }
}

enum HttpError {
    Connect,
    Api {
        code: String,
        message: String,
        #[allow(dead_code)]
        status: u16,
    },
}

fn http(method: &str, path: &str, body: Option<Value>) -> Result<Value, HttpError> {
    let ports = candidate_ports();
    for port in ports {
        match request(port, method, path, body.clone()) {
            Ok(value) => {
                *PORT_CACHE.lock().expect("port cache") = Some(port);
                return Ok(value);
            }
            Err(HttpError::Connect) => continue,
            Err(err) => return Err(err),
        }
    }
    Err(HttpError::Connect)
}

fn candidate_ports() -> Vec<u16> {
    let mut ports = Vec::new();
    if let Some(port) = *PORT_CACHE.lock().expect("port cache") {
        ports.push(port);
    }
    if let Some(port) = Paths::default_handle().port() {
        if !ports.contains(&port) {
            ports.push(port);
        }
    }
    for offset in 0..PORT_RANGE {
        let port = DEFAULT_PORT + offset;
        if !ports.contains(&port) {
            ports.push(port);
        }
    }
    ports
}

fn request(port: u16, method: &str, path: &str, body: Option<Value>) -> Result<Value, HttpError> {
    let token = Paths::default_handle()
        .token()
        .map_err(|_| HttpError::Connect)?;
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let timeout = if method == "GET" && path.starts_with("/v1/health") {
        PROBE_TIMEOUT_MS
    } else {
        CALL_TIMEOUT_SECS * 1000
    };

    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|_| HttpError::Connect)?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(timeout)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(timeout)));
    let head = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method,
        path,
        token,
        payload.len()
    );
    let mut req = head.into_bytes();
    req.extend_from_slice(payload.as_bytes());
    stream.write_all(&req).map_err(|_| HttpError::Connect)?;
    let _ = stream.flush();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|_| HttpError::Connect)?;
    let text = String::from_utf8_lossy(&raw).to_string();
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Err(HttpError::Connect);
    };
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if status >= 400 {
        return Err(HttpError::Api {
            code: value["code"].as_str().unwrap_or("HTTP_ERROR").to_string(),
            message: value["message"].as_str().unwrap_or("请求失败").to_string(),
            status,
        });
    }
    Ok(value)
}

// ---------------------------------------------------------------- 工具

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into())
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…（已截断）", &text[..end])
}
