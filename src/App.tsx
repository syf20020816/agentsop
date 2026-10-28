import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";

type Status =
  | "pending"
  | "ready"
  | "active"
  | "waiting_human"
  | "done"
  | "blocked"
  | "skipped"
  | "stale";

interface Evidence {
  kind: string;
  label: string;
  path?: string;
  ts: string;
}

interface NodeView {
  id: string;
  idx: string;
  title: string;
  type: "human" | "agent" | "auto";
  status: Status;
  agent?: string;
  retries: number;
  entered_at?: string;
  completed_at?: string;
  evidence: Evidence[];
  pending_decision?: { question: string; since: string };
  default_prompt?: string;
  prompt?: string;
  blocking: boolean;
}

interface Snapshot {
  run_id: string;
  sop: { name: string; title: string; version: number };
  run_status: "running" | "paused" | "completed" | "aborted";
  seq: number;
  current_node?: string | null;
  nodes: NodeView[];
  bound_session?: string;
  sessions: { id: string; node_id: string; bound: boolean; title?: string; agent?: string }[];
  known_sessions: KnownSession[];
  session_locked: boolean;
  updated_at: string;
}

interface KnownSession {
  id: string;
  agent: string;
  title: string;
  repo: string;
  updated_at: string;
}

interface SkillMeta {
  name: string;
  description: string;
  agent: string;
}

interface SopNode {
  id: string;
  type: "human" | "agent" | "auto";
  title?: string;
  agent?: string;
  prompt?: string;
  default_prompt?: string;
  review?: boolean;
  blocking?: boolean;
  done?: { events?: string[]; artifacts?: string[]; timeout_sec?: number };
}

interface SopDoc {
  sop: string;
  version: number;
  title?: string;
  entry: string;
  nodes: SopNode[];
  edges: { from: string; to: string; when?: string; kind?: string; max?: number }[];
  vars?: Record<string, string>;
}

interface SopMeta {
  name: string;
  title: string;
  version: number;
  nodes: number;
}

interface Bootstrap {
  version: string;
  port?: number;
  run?: Snapshot | null;
  runs: Snapshot[];
  sops: SopMeta[];
}

const STATUS_TEXT: Record<Status, string> = {
  pending: "待开始",
  ready: "就绪",
  active: "运行中",
  waiting_human: "等你确认",
  done: "已完成",
  blocked: "被拦截",
  skipped: "已跳过",
  stale: "超时",
};

const ICON = {
  pin: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4">
      <path d="M9.5 1.5 14 6l-1.6 1.6-1.1-.3-2.7 2.7.3 2.2L7.3 13.8 2.2 8.7l1.6-1.6 2.2.3 2.7-2.7-.3-1.1z" />
    </svg>
  ),
  more: (
    <svg viewBox="0 0 16 16" fill="currentColor">
      <circle cx="3.2" cy="8" r="1.1" />
      <circle cx="8" cy="8" r="1.1" />
      <circle cx="12.8" cy="8" r="1.1" />
    </svg>
  ),
  chev: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
      <path d="M4 6.5 8 10l4-3.5" />
    </svg>
  ),
  file: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <path d="M4 1.8h5l3 3v9.4H4z" />
      <path d="M9 1.8v3h3" />
    </svg>
  ),
  close: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5">
      <path d="M4 4l8 8M12 4l-8 8" />
    </svg>
  ),
};

function statusChipClass(status: Status): string {
  if (status === "waiting_human") return "chip waiting";
  if (status === "active") return "chip active";
  if (status === "done") return "chip done";
  if (status === "blocked") return "chip blocked";
  return "chip";
}

function railMeta(node: NodeView): string {
  switch (node.status) {
    case "active":
      return node.agent ?? "运行中";
    case "waiting_human":
      return node.pending_decision?.question ? "等你确认" : "等你确认";
    case "blocked":
      return "写操作被拦截";
    case "done":
      return node.type === "human" ? "人工节点" : "已完成";
    case "stale":
      return "超时";
    default:
      return "待开始";
  }
}

function shortTime(ts?: string): string {
  if (!ts) return "";
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
}

function railSide(node: NodeView): string {
  if (node.status === "done" && node.completed_at) {
    return new Date(node.completed_at).toLocaleTimeString("zh-CN", {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
  }
  if (node.retries > 0) return `第${node.retries + 1}次`;
  if (node.status === "waiting_human") return "等你";
  return "";
}

export default function App() {
  const [booted, setBooted] = useState(false);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [runs, setRuns] = useState<Snapshot[]>([]);
  const [sops, setSops] = useState<SopMeta[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [overlay, setOverlay] = useState<"none" | "editor" | "help" | "reject">("none");
  const [pop, setPop] = useState<"none" | "runs" | "more">("none");
  const [toast, setToast] = useState<string | null>(null);
  const [pinned, setPinned] = useState(true);

  const [bindAgent, setBindAgent] = useState("codex");
  const [bindSessionId, setBindSessionId] = useState("");
  const [agentSessions, setAgentSessions] = useState<KnownSession[]>([]);
  const [bindSop, setBindSop] = useState("");

  const [sopDoc, setSopDoc] = useState<SopDoc | null>(null);
  const [sopName, setSopName] = useState<string>("");
  const [creatingSop, setCreatingSop] = useState(false);
  const [editIdx, setEditIdx] = useState(0);
  const [rejectNote, setRejectNote] = useState("");

  const toastTimer = useRef<number | null>(null);
  const say = useCallback((msg: string) => {
    setToast(msg);
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), 2200);
  }, []);

  const refreshBootstrap = useCallback(async () => {
    const data = await invoke<Bootstrap>("bootstrap");
    setRuns(data.runs ?? []);
    setSops(data.sops ?? []);
    if (data.run) {
      setSnap(data.run);
      setSelected((prev) => prev ?? data.run?.current_node ?? null);
    }
    setBooted(true);
  }, []);

  useEffect(() => {
    refreshBootstrap().catch((err) => say(String(err)));
    const un = listen<Snapshot>("agentsop://snapshot", (event) => {
      const next = event.payload;
      setSnap(next);
      setRuns((prev) => {
        const rest = prev.filter((r) => r.run_id !== next.run_id);
        return [next, ...rest];
      });
      setSelected((prev) => prev ?? next.current_node ?? null);
    });
    return () => {
      un.then((f) => f());
    };
  }, [refreshBootstrap, say]);

  // 空态（没有运行）时轮询该 Agent 的会话：新建的任务没有运行可挂，只能靠轮询发现。
  useEffect(() => {
    if (snap) return;
    let alive = true;
    const pull = async () => {
      try {
        const list = await invoke<KnownSession[]>("list_agent_sessions", { agent: bindAgent });
        if (alive) setAgentSessions(list);
      } catch {
        /* 面板/内核还没就绪时忽略 */
      }
    };
    void pull();
    const timer = window.setInterval(() => void pull(), 2500);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [snap, bindAgent]);

  const current = useMemo(() => {
    if (!snap) return null;
    return snap.nodes.find((n) => n.id === snap.current_node) ?? null;
  }, [snap]);

  const shown = useMemo(() => {
    if (!snap) return null;
    return snap.nodes.find((n) => n.id === selected) ?? current ?? snap.nodes[0] ?? null;
  }, [snap, selected, current]);

  const pendingCount = useMemo(
    () =>
      snap?.nodes.filter((n) => n.status === "waiting_human" || n.status === "blocked").length ??
      0,
    [snap],
  );

  const canDecide =
    !!snap &&
    !!shown &&
    shown.id === snap.current_node &&
    (shown.status === "waiting_human" || shown.status === "blocked");

  const decide = useCallback(
    async (action: "approve" | "reject" | "retry" | "allow_once", note = "") => {
      if (!snap || !shown) return;
      try {
        const next = await invoke<Snapshot>("decide", {
          runId: snap.run_id,
          nodeId: shown.id,
          action,
          note,
        });
        setSnap(next);
        setRuns((prev) => [next, ...prev.filter((r) => r.run_id !== next.run_id)]);
        if (action === "approve") {
          const nextNode = next.nodes.find((n) => n.id === next.current_node);
          setSelected(next.current_node ?? null);
          if (nextNode && nextNode.id === shown.id && nextNode.status === "active") {
            say("已确认，Agent 可继续");
          } else {
            say(nextNode ? `已通过 · 进入「${nextNode.title}」` : "流程已完成");
          }
        } else if (action === "reject") {
          setOverlay("none");
          setRejectNote("");
          say("已打回，等待 Agent 重新执行");
        } else {
          say("已放行本次调用");
        }
      } catch (err) {
        say(String(err));
      }
    },
    [snap, shown, say],
  );

  const completeManually = useCallback(async () => {
    if (!snap || !shown) return;
    try {
      const next = await invoke<Snapshot>("complete_node", {
        runId: snap.run_id,
        nodeId: shown.id,
        evidence: [],
      });
      setSnap(next);
      setSelected(next.current_node ?? null);
      say("已标记完成");
    } catch (err) {
      say(String(err));
    }
  }, [snap, shown, say]);

  const startRun = useCallback(
    async (sop: string) => {
      try {
        const next = await invoke<Snapshot>("start_run", {
          sop,
          repo: "~",
          vars: {},
          agent: null,
          sessionId: null,
        });
        setSnap(next);
        setSelected(next.current_node ?? null);
        setOverlay("none");
        say(`已启动「${next.sop.title}」`);
      } catch (err) {
        say(String(err));
      }
    },
    [say],
  );

  const bindSession = useCallback(
    async (sessionId: string) => {
      if (!snap) return;
      try {
        const next = await invoke<Snapshot>("bind_session", {
          runId: snap.run_id,
          sessionId,
        });
        setSnap(next);
        setRuns((prev) => [next, ...prev.filter((r) => r.run_id !== next.run_id)]);
        say(sessionId ? "已绑定这个会话" : "已解除绑定");
      } catch (err) {
        say(String(err));
      }
    },
    [snap, say],
  );

  const newSession = useCallback(async () => {
    if (!snap) return;
    try {
      const next = await invoke<Snapshot>("new_session", { runId: snap.run_id });
      const sameRun = next.run_id === snap.run_id;
      setSnap(next);
      setRuns((prev) => [next, ...prev.filter((r) => r.run_id !== next.run_id)]);
      if (!sameRun) setSelected(next.current_node ?? null);
      say(
        sameRun
          ? "已唤起 Agent，请在它里面新建任务，回来后在下拉里选中它"
          : "已新建运行，新任务发第一条消息后自动绑定",
      );
    } catch (err) {
      say(String(err));
    }
  }, [snap, say]);

  // 空态：「绑定并开始」= 用选中的 Agent + 任务直接创建一次已绑定的运行。
  const bindAndStart = useCallback(async () => {
    const session = agentSessions.find((s) => s.id === bindSessionId);
    const sop = bindSop || sops[0]?.name;
    if (!session) return;
    if (!sop) {
      say("先创建一个 SOP");
      return;
    }
    try {
      const next = await invoke<Snapshot>("start_run", {
        sop,
        repo: session.repo || "~",
        vars: {},
        agent: bindAgent,
        sessionId: session.id,
      });
      setSnap(next);
      setRuns((prev) => [next, ...prev.filter((r) => r.run_id !== next.run_id)]);
      setSelected(next.current_node ?? null);
      // 绑定只准备运行，不自动执行：由用户上下选节点、空格填入、Enter 执行。
      say(`已绑定「${session.title || session.id.slice(0, 8)}」，空格填入第一个节点`);
    } catch (err) {
      say(String(err));
    }
  }, [agentSessions, bindSessionId, bindSop, sops, bindAgent, say]);

  const createTask = useCallback(async () => {
    try {
      await invoke("open_agent_task", { agent: bindAgent });
      say("已唤起 Agent，在里面新建任务即可");
    } catch (err) {
      say(String(err));
    }
  }, [bindAgent, say]);

  // 会话候选 = 全局登记过的会话（跨运行），保证还没开始时也能挑一个绑定。
  const sessionOptions = useMemo(() => {
    const map = new Map<string, { id: string; title: string; agent: string }>();
    for (const s of snap?.known_sessions ?? []) {
      map.set(s.id, { id: s.id, title: s.title, agent: s.agent });
    }
    for (const s of snap?.sessions ?? []) {
      if (!map.has(s.id)) map.set(s.id, { id: s.id, title: s.title ?? "", agent: s.agent ?? "" });
    }
    const bound = snap?.bound_session;
    if (bound && !map.has(bound)) map.set(bound, { id: bound, title: "", agent: "" });
    return [...map.values()];
  }, [snap]);

  // 新建 SOP：先在编辑器里填，保存时才落盘。
  const openNewSop = useCallback(() => {
    let n = sops.length + 1;
    while (sops.some((s) => s.name === `sop-${n}`)) n += 1;
    setSopDoc({
      sop: `sop-${n}`,
      version: 1,
      entry: "step_1",
      nodes: [
        { id: "step_1", type: "agent", title: "新节点", done: { events: ["Stop"], artifacts: [] } },
      ],
      edges: [],
    });
    setSopName("");
    setCreatingSop(true);
    setEditIdx(0);
    setOverlay("editor");
    setPop("none");
  }, [sops]);

  const openEditor = useCallback(async () => {
    const name = sops.find((s) => s.name === snap?.sop.name)?.name ?? sops[0]?.name;
    if (!name) {
      openNewSop();
      return;
    }
    try {
      const doc = await invoke<{ name: string; sop: SopDoc }>("get_sop", { name });
      setSopDoc(structuredClone(doc.sop));
      setSopName(name);
      setCreatingSop(false);
      setEditIdx(0);
      setOverlay("editor");
      setPop("none");
    } catch (err) {
      say(String(err));
    }
  }, [snap, sops, openNewSop, say]);

  const saveEditor = useCallback(async () => {
    if (!sopDoc) return;
    const name = sopDoc.sop.trim().replace(/\s+/g, "-");
    if (!name) {
      say("先给 SOP 起个名字");
      return;
    }
    if (name !== sopName && sops.some((s) => s.name === name)) {
      say(`已有同名 SOP「${name}」`);
      return;
    }
    try {
      await invoke("save_sop_json", {
        name,
        sop: { ...sopDoc, sop: name },
        prev: sopName || null,
      });
      // 改名后删掉旧文件，避免列表里出现两份。
      if (sopName && sopName !== name) {
        await invoke("delete_sop", { name: sopName }).catch(() => undefined);
      }
      await refreshBootstrap();
      setSopName(name);
      setCreatingSop(false);
      setBindSop(name);
      setOverlay("none");
      say("已保存，立即生效");
    } catch (err) {
      say(String(err));
    }
  }, [sopDoc, sopName, sops, refreshBootstrap, say]);

  const loadRun = useCallback(
    async (runId: string) => {
      try {
        const next = await invoke<Snapshot | null>("snapshot", { runId });
        if (next) {
          setSnap(next);
          setSelected(next.current_node ?? null);
        }
        setPop("none");
      } catch (err) {
        say(String(err));
      }
    },
    [say],
  );

  const copyPrompt = useCallback(
    (text: string) => {
      navigator.clipboard?.writeText(text).then(
        () => say("已复制默认 Prompt"),
        () => say("复制失败"),
      );
    },
    [say],
  );

  // 最近一次「填入」过的节点：只有它才允许 Enter 直接发送，避免把空输入框发出去。
  const filledFor = useRef<string | null>(null);

  const fillPrompt = useCallback(async () => {
    if (!snap || !shown) return;
    const text = shown.default_prompt || shown.prompt || "";
    if (!text.trim()) {
      say("这个节点还没有 Prompt");
      return;
    }
    try {
      const pasted = await invoke<boolean>("fill_prompt", {
        text,
        agent: shown.agent ?? null,
      });
      if (pasted) {
        filledFor.current = shown.id;
        say(`已填入「${shown.title}」，改完后按 Enter 执行`);
      } else {
        filledFor.current = null;
        say("已复制，请到 Codex 里 ⌘V 粘贴（需开启辅助功能权限）");
      }
    } catch (err) {
      say(String(err));
    }
  }, [snap, shown, say]);

  const submitPrompt = useCallback(async () => {
    if (!shown || filledFor.current !== shown.id) {
      say("先按空格把 Prompt 填进 Codex");
      return;
    }
    const ok = await invoke<boolean>("submit_prompt", { agent: shown.agent ?? null }).catch(
      () => false,
    );
    say(ok ? "已在 Codex 里执行" : "没发出去，检查 AgentSOP 的辅助功能权限");
  }, [shown, say]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing =
        e.target instanceof HTMLElement &&
        ["INPUT", "TEXTAREA", "SELECT"].includes(e.target.tagName);
      if (e.key === "Escape") {
        if (overlay !== "none") setOverlay("none");
        else if (pop !== "none") setPop("none");
        return;
      }
      if (typing) return;
      // 浮层打开时只处理 Esc，避免编辑 SOP 时误触发填入/发送。
      if (overlay !== "none") return;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        void openEditor();
        return;
      }
      if (e.key === "?") {
        setOverlay("help");
        return;
      }
      if (!snap) return;
      const idx = snap.nodes.findIndex((n) => n.id === shown?.id);
      if (e.key === "j" || e.key === "ArrowDown") {
        e.preventDefault();
        setSelected(snap.nodes[Math.min(idx + 1, snap.nodes.length - 1)].id);
      } else if (e.key === "k" || e.key === "ArrowUp") {
        e.preventDefault();
        setSelected(snap.nodes[Math.max(idx - 1, 0)].id);
      } else if (e.key === " ") {
        // 空格：把选中节点的 Prompt 填进 Codex，人在 Codex 里改完再回车。
        e.preventDefault();
        if (shown && shown.type !== "human") void fillPrompt();
      } else if (e.key === "Enter") {
        e.preventDefault();
        void submitPrompt();
      } else if (e.key === "a" && canDecide && shown?.status === "waiting_human") {
        void decide("approve");
      } else if (e.key === "r" && canDecide && shown?.status === "waiting_human") {
        setOverlay("reject");
      } else if (e.key === "p") {
        setPinned((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [snap, shown, canDecide, decide, overlay, pop, openEditor, fillPrompt, submitPrompt]);

  // 空态（无运行）也要能新建/编辑 SOP，所以浮层在这里统一渲染。
  const editorOverlay =
    overlay === "editor" && sopDoc ? (
      <EditorOverlay
        doc={sopDoc}
        creating={creatingSop}
        agent={bindAgent}
        onChange={setSopDoc}
        index={editIdx}
        onIndex={setEditIdx}
        onSave={saveEditor}
        onClose={() => setOverlay("none")}
      />
    ) : null;

  if (!booted) {
    return <div className="app" />;
  }

  if (!snap) {
    return (
      <div className="app">
        <Titlebar
          runLabel="未选择 SOP"
          pending={0}
          pinned={pinned}
          onPin={() => setPinned((v) => !v)}
          onPop={() => setPop(pop === "more" ? "none" : "more")}
          onRuns={null}
        />
        <div className="empty">
          <div className="empty-title">开始一次运行</div>
          <div className="empty-sub">选 Agent 和任务，绑定后这条 SOP 只在这个任务里跑</div>

          <div className="field efield">
            <label>Agent</label>
            <div className="agent-chips">
              {(
                [
                  ["codex", "Codex"],
                  ["claude-code", "Claude"],
                ] as const
              ).map(([value, label]) => (
                <button
                  key={value}
                  className={`agent-chip ${bindAgent === value ? "is-on" : ""}`}
                  onClick={() => {
                    setBindAgent(value);
                    setBindSessionId("");
                  }}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          <div className="field efield">
            <label>任务</label>
            {agentSessions.length === 0 ? (
              <div className="empty-hint">还没扫到会话；新建一个任务后会自动出现。</div>
            ) : (
              <div className="task-list">
                {agentSessions.map((s) => (
                  <button
                    key={s.id}
                    className={`task-row ${bindSessionId === s.id ? "is-on" : ""}`}
                    title={s.repo || s.id}
                    onClick={() => setBindSessionId(s.id)}
                  >
                    <span className="task-title">{s.title || s.id.slice(0, 10)}</span>
                    <span className="task-time">{shortTime(s.updated_at)}</span>
                  </button>
                ))}
              </div>
            )}
            <button className="task-new" onClick={() => void createTask()}>
              + 新建任务
            </button>
          </div>

          <div className="field efield">
            <label>SOP</label>
            {sops.length === 0 ? (
              <>
                <button className="sop-create" onClick={openNewSop}>
                  + 创建 SOP
                </button>
                <div className="hint">还没有 SOP，先建一条再绑定这个任务。</div>
              </>
            ) : (
              <select
                className="select"
                value={bindSop || sops[0]?.name || ""}
                onChange={(e) => setBindSop(e.target.value)}
              >
                {sops.map((s) => (
                  <option key={s.name} value={s.name}>
                    {s.title} · {s.nodes} 节点
                  </option>
                ))}
              </select>
            )}
          </div>

          <button
            className="bind-start"
            disabled={!bindSessionId || sops.length === 0}
            onClick={() => void bindAndStart()}
          >
            绑定并开始
          </button>
        </div>
        <Statusbar hints={<span>⌥⌘S 呼出 / 收起</span>} connected={false} />
        {pop === "more" && (
          <div className="pop">
            <button className="pop-item" onClick={openNewSop}>
              新建 SOP
            </button>
            {sops.length > 0 && (
              <button className="pop-item" onClick={openEditor}>
                编辑 SOP<span className="meta">⌘K</span>
              </button>
            )}
            <button className="pop-item" onClick={() => setOverlay("help")}>
              快捷键<span className="meta">?</span>
            </button>
          </div>
        )}
        {overlay === "help" && <HelpOverlay onClose={() => setOverlay("none")} />}
        {editorOverlay}
        {toast && <div className="toast">{toast}</div>}
      </div>
    );
  }

  return (
    <div className="app">
      <Titlebar
        runLabel={snap.sop.title}
        pending={pendingCount}
        pinned={pinned}
        onPin={() => setPinned((v) => !v)}
        onPop={() => setPop(pop === "more" ? "none" : "more")}
        onRuns={() => setPop(pop === "runs" ? "none" : "runs")}
      />

      <div className="sessionbar">
        <span className="sb-label">会话</span>
        <select
          className="sb-select"
          value={snap.bound_session ?? ""}
          disabled={snap.session_locked || sessionOptions.length === 0}
          title={
            snap.session_locked
              ? "已开始推进，不能再改绑；需要换任务请用「+」新建一次运行"
              : "把这个 SOP 绑定到某个任务；开始推进后锁定"
          }
          onChange={(e) => void bindSession(e.target.value)}
        >
          <option value="">未绑定</option>
          {sessionOptions.map((s) => (
            <option key={s.id} value={s.id} title={`${s.agent || "agent"} · ${s.id}`}>
              {s.title || `${s.agent || "会话"} · ${s.id.slice(0, 8)}…`}
            </option>
          ))}
        </select>
        <button
          className="sb-plus"
          title="让 Agent 新建一个任务，新任务接入后自动绑定"
          onClick={() => void newSession()}
        >
          +
        </button>
      </div>

      <div className="rail">
        {snap.nodes.map((node) => (
          <button
            key={node.id}
            className="rail-row"
            data-s={node.status}
            data-sel={shown?.id === node.id}
            onClick={() => setSelected(node.id)}
          >
            <span className="dot" data-s={node.status}>
              <i />
            </span>
            <span className="rail-main">
              <span className="rail-title">{node.title}</span>
              <span className="rail-meta">{railMeta(node)}</span>
            </span>
            <span className="rail-side">{railSide(node)}</span>
          </button>
        ))}
      </div>

      {shown && (
        <div className="detail">
          <div className="d-head">
            <span className="d-idx">{shown.idx}</span>
            <span className="d-title">{shown.title}</span>
          </div>
          <div className="d-status">
            <span className={statusChipClass(shown.status)}>{STATUS_TEXT[shown.status]}</span>
            {shown.type === "human" ? (
              <span>人工节点</span>
            ) : (
              <span>{shown.agent ?? "agent"}</span>
            )}
            {shown.retries > 0 && <span>· 第 {shown.retries + 1} 次</span>}
          </div>

          {shown.status === "waiting_human" && shown.pending_decision && (
            <div className="card">
              <div className="card-head">
                <span className="lbl">待你决定</span>
              </div>
              <div className="q-text">{shown.pending_decision.question}</div>
            </div>
          )}

          {shown.status === "blocked" && (
            <div className="card">
              <div className="card-head">
                <span className="lbl">已拦截</span>
              </div>
              <div className="q-text">
                当前节点在等人确认，写操作被 PreToolUse 拦下。放行一次只影响本次调用。
              </div>
            </div>
          )}

          {shown.default_prompt && shown.type !== "human" && (
            <div className="card">
              <div className="card-head">
                <span className="lbl">默认 Prompt</span>
                <span className="card-acts">
                  <button className="copy-btn" onClick={() => void fillPrompt()}>
                    填入
                  </button>
                  <button className="copy-btn" onClick={() => copyPrompt(shown.default_prompt!)}>
                    复制
                  </button>
                </span>
              </div>
              <div className="prompt-text">{shown.default_prompt}</div>
            </div>
          )}

          {shown.evidence.length > 0 && (
            <div className="card">
              <div className="card-head">
                <span className="lbl">产物</span>
                <span className="card-count">{shown.evidence.length}</span>
              </div>
              {shown.evidence.slice(0, 2).map((ev, i) => (
                <div className="ev" key={`${ev.label}-${i}`}>
                  <span style={{ width: 10, height: 10, color: "var(--as-text-3)" }}>
                    {ICON.file}
                  </span>
                  <span className={`ev-name ${ev.kind === "artifact" ? "is-missing" : ""}`}>
                    {ev.label}
                  </span>
                </div>
              ))}
            </div>
          )}

          {shown.status === "pending" && (
            <div className="muted">这一步还没开始。当前节点完成后自动进入。</div>
          )}
        </div>
      )}

      {canDecide && shown && (
        <div className="decision">
          {shown.status === "waiting_human" ? (
            <>
              <button className="btn primary" onClick={() => decide("approve")}>
                通过
              </button>
              <button className="btn danger" onClick={() => setOverlay("reject")}>
                打回
              </button>
            </>
          ) : (
            <>
              <button className="btn primary" onClick={() => decide("allow_once")}>
                允许一次
              </button>
              <button className="btn" onClick={() => decide("retry")}>
                重试
              </button>
            </>
          )}
        </div>
      )}

      {!canDecide && shown?.status === "active" && (
        <div className="decision">
          <button className="btn" onClick={completeManually}>
            标记完成
          </button>
        </div>
      )}

      <Statusbar
        hints={
          shown?.status === "waiting_human" ? (
            <>
              <span>
                <span className="kbd">A</span> 通过
              </span>
              <span>
                <span className="kbd">R</span> 打回
              </span>
            </>
          ) : shown?.status === "blocked" ? (
            <span>允许一次只放行本次调用</span>
          ) : (
            <>
              <span>
                <span className="kbd">空格</span> 填入
              </span>
              <span>
                <span className="kbd">↵</span> 执行
              </span>
            </>
          )
        }
        connected
      />

      {pop === "runs" && (
        <div className="pop">
          <div className="pop-head">我的 SOP</div>
          {sops.map((sop) => {
            const latest = runs
              .filter((r) => r.sop.name === sop.name)
              .sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0];
            const live =
              latest && (latest.run_status === "running" || latest.run_status === "paused");
            return (
              <button
                className="pop-item"
                key={sop.name}
                onClick={() => (live ? void loadRun(latest.run_id) : void startRun(sop.name))}
              >
                <span className={`dot ${live ? "running" : ""}`} />
                <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{sop.title}</span>
                <span className="meta">
                  {live
                    ? `${latest.nodes.filter((n) => n.status === "done").length}/${latest.nodes.length}`
                    : `${sop.nodes} 节点`}
                </span>
              </button>
            );
          })}
        </div>
      )}

      {pop === "more" && (
        <div className="pop">
          <button className="pop-item" onClick={openNewSop}>
            新建 SOP
          </button>
          <button className="pop-item" onClick={openEditor}>
            编辑 SOP<span className="meta">⌘K</span>
          </button>
          <button className="pop-item" onClick={() => invoke("open_state_dir")}>
            状态目录
          </button>
          <button className="pop-item" onClick={() => setOverlay("help")}>
            快捷键<span className="meta">?</span>
          </button>
        </div>
      )}

      {overlay === "help" && <HelpOverlay onClose={() => setOverlay("none")} />}

      {overlay === "reject" && (
        <div className="overlay">
          <div className="overlay-head">
            <span className="overlay-title">打回「{shown?.title}」</span>
            <button className="icon-btn" onClick={() => setOverlay("none")}>
              {ICON.close}
            </button>
          </div>
          <div className="overlay-body">
            <div className="field">
              <label>要改什么</label>
              <textarea
                className="textarea"
                autoFocus
                placeholder="至少 8 个字，写清楚哪里不对、要补什么"
                value={rejectNote}
                onChange={(e) => setRejectNote(e.target.value)}
              />
              <div className="hint">打回原因会写进 run 事件，并通过 MCP / context.md 传给 Agent。</div>
            </div>
          </div>
          <div className="overlay-foot">
            <button className="btn" onClick={() => setOverlay("none")}>
              取消
            </button>
            <button
              className="btn danger"
              disabled={rejectNote.trim().length < 8}
              onClick={() => decide("reject", rejectNote)}
            >
              打回
            </button>
          </div>
        </div>
      )}

      {editorOverlay}

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

function Titlebar(props: {
  runLabel: string;
  pending: number;
  pinned: boolean;
  onPin: () => void;
  onPop: () => void;
  onRuns: (() => void) | null;
}) {
  // 长按 300ms 后拖动窗口：避免点按钮或切节点时误拖。
  const holdTimer = useRef<number | null>(null);
  const origin = useRef<{ x: number; y: number } | null>(null);

  const cancelHold = () => {
    if (holdTimer.current !== null) {
      window.clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
    origin.current = null;
  };

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    origin.current = { x: e.clientX, y: e.clientY };
    holdTimer.current = window.setTimeout(() => {
      holdTimer.current = null;
      origin.current = null;
      void getCurrentWindow().startDragging().catch(() => {});
    }, 300);
  };

  const onPointerMove = (e: React.PointerEvent) => {
    if (!origin.current) return;
    const moved =
      Math.abs(e.clientX - origin.current.x) > 6 ||
      Math.abs(e.clientY - origin.current.y) > 6;
    if (moved) cancelHold();
  };

  return (
    <div
      className="titlebar"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={cancelHold}
      onPointerCancel={cancelHold}
      onPointerLeave={cancelHold}
    >
      <div className="tb-row">
        <span className="logo" />
        <span className="product">AgentSOP</span>
        <span className="tb-spacer" />
        <button className={`icon-btn ${props.pinned ? "is-on" : ""}`} title="固定面板 (P)" onClick={props.onPin}>
          {ICON.pin}
        </button>
        <button className="icon-btn" title="更多" onClick={props.onPop}>
          {ICON.more}
        </button>
      </div>
      <div className="tb-sub">
        {props.onRuns ? (
          <button className="run-btn" onClick={props.onRuns}>
            <span className="run-name">{props.runLabel}</span>
            <span style={{ width: 10, height: 10, display: "inline-flex" }}>{ICON.chev}</span>
          </button>
        ) : (
          <span className="run-btn">
            <span className="run-name">{props.runLabel}</span>
          </span>
        )}
        {props.pending > 0 && (
          <span className="pending">
            <span className="pulse-dot" />
            {props.pending} 待确认
          </span>
        )}
      </div>
    </div>
  );
}

function Statusbar(props: { hints: ReactNode; connected: boolean }) {
  return (
    <div className="statusbar">
      <span className="hints">{props.hints}</span>
      <span className="conn" data-off={!props.connected}>
        <i />
        {props.connected ? "已连接" : "未连接"}
      </span>
    </div>
  );
}

function HelpOverlay(props: { onClose: () => void }) {
  const rows: [string, string][] = [
    ["⌥⌘S", "呼出 / 收起面板"],
    ["J / K", "上下切换节点"],
    ["空格", "把节点 Prompt 填进 Codex"],
    ["Enter", "在 Codex 里发送执行"],
    ["A", "通过当前节点"],
    ["R", "打回并写原因"],
    ["⌘K", "编辑 SOP"],
    ["P", "固定面板"],
    ["长按顶栏", "拖动面板到任意位置"],
    ["Esc", "关闭浮层"],
  ];
  return (
    <div className="overlay">
      <div className="overlay-head">
        <span className="overlay-title">快捷键</span>
        <button className="icon-btn" onClick={props.onClose}>
          {ICON.close}
        </button>
      </div>
      <div className="overlay-body">
        <div className="sc-grid">
          {rows.map(([key, desc]) => (
            <div className="sc-row" key={key}>
              <span className="kbd">{key}</span>
              <span>{desc}</span>
            </div>
          ))}
        </div>
        <div className="hint" style={{ marginTop: 8 }}>
          讨论请直接在 Agent Tool 里进行；AgentSOP 只负责触发与卡点。
        </div>
      </div>
    </div>
  );
}

function EditorOverlay(props: {
  doc: SopDoc;
  creating: boolean;
  agent: string;
  index: number;
  onChange: (doc: SopDoc) => void;
  onIndex: (idx: number) => void;
  onSave: () => void;
  onClose: () => void;
}) {
  const node = props.doc.nodes[props.index];
  const patch = (fields: Partial<SopNode>) => {
    const nodes = props.doc.nodes.map((n, i) => (i === props.index ? { ...n, ...fields } : n));
    props.onChange({ ...props.doc, nodes });
  };
  const patchDone = (fields: Partial<NonNullable<SopNode["done"]>>) => {
    patch({ done: { ...(node.done ?? {}), ...fields } });
  };

  const addNode = () => {
    const nodes = props.doc.nodes;
    const used = new Set(nodes.map((n) => n.id));
    let n = nodes.length + 1;
    while (used.has(`node_${n}`)) n += 1;
    const id = `node_${n}`;
    const prev = nodes[props.index] ?? nodes[nodes.length - 1];
    const edges = [...(props.doc.edges ?? [])];
    if (prev) edges.push({ from: prev.id, to: id, kind: "normal" });
    props.onChange({
      ...props.doc,
      nodes: [
        ...nodes,
        { id, type: "agent", title: "新节点", done: { events: ["Stop"], artifacts: [] } },
      ],
      edges,
    });
    props.onIndex(nodes.length);
  };

  const deleteNode = () => {
    if (props.doc.nodes.length <= 1 || !node) return;
    const nodes = props.doc.nodes.filter((_, i) => i !== props.index);
    const edges = (props.doc.edges ?? []).filter((e) => e.from !== node.id && e.to !== node.id);
    const entry = props.doc.entry === node.id ? nodes[0].id : props.doc.entry;
    props.onChange({ ...props.doc, nodes, edges, entry });
    props.onIndex(Math.min(props.index, nodes.length - 1));
  };

  return (
    <div className="overlay">
      <div className="overlay-head">
        <span className="overlay-title">{props.creating ? "新建 SOP" : "编辑 SOP"}</span>
        <button className="icon-btn" onClick={props.onClose}>
          {ICON.close}
        </button>
      </div>
      <div className="overlay-body">
        <div className="field">
          <label>SOP 名称</label>
          <input
            className="input"
            value={props.doc.sop}
            onChange={(e) => props.onChange({ ...props.doc, sop: e.target.value })}
          />
          <div className="hint">同时作为文件名，列表里显示这个名字。</div>
        </div>

        <div className="field">
          <label>节点</label>
          <div className="node-picker">
            <select
              className="select"
              value={props.index}
              onChange={(e) => props.onIndex(Number(e.target.value))}
            >
              {props.doc.nodes.map((n, i) => (
                <option key={n.id} value={i}>
                  {String(i + 1).padStart(2, "0")} {n.title || n.id}
                </option>
              ))}
            </select>
            <button className="node-btn" title="新建节点" onClick={addNode}>
              +
            </button>
            <button
              className="node-btn danger"
              title="删除当前节点"
              disabled={props.doc.nodes.length <= 1}
              onClick={deleteNode}
            >
              −
            </button>
          </div>
        </div>

        {node && (
          <>
            <div className="row2">
              <div className="field">
                <label>ID</label>
                <input
                  className="input"
                  value={node.id}
                  onChange={(e) => patch({ id: e.target.value })}
                />
              </div>
              <div className="field">
                <label>类型</label>
                <select
                  className="select"
                  value={node.type}
                  onChange={(e) => patch({ type: e.target.value as SopNode["type"] })}
                >
                  <option value="agent">agent</option>
                  <option value="human">human</option>
                  <option value="auto">auto</option>
                </select>
              </div>
            </div>

            <div className="field">
              <label>节点名称</label>
              <input
                className="input"
                value={node.title ?? ""}
                onChange={(e) => patch({ title: e.target.value })}
              />
            </div>

            <div className="field">
              <label>Agent</label>
              <select
                className="select"
                value={node.agent ?? ""}
                onChange={(e) => patch({ agent: e.target.value })}
              >
                <option value="">未指定</option>
                <option value="codex">Codex</option>
                <option value="claude-code">Claude Code</option>
                {node.agent &&
                  !["codex", "claude-code"].includes(node.agent) && (
                    <option value={node.agent}>{node.agent}</option>
                  )}
              </select>
            </div>

            <div className="field">
              <label>默认 Prompt</label>
              <PromptField
                value={node.default_prompt ?? ""}
                agent={node.agent || props.agent}
                placeholder="/analysis 分析 docs/interview.md"
                onChange={(v) => patch({ default_prompt: v })}
              />
              <div className="hint">节点轮转时自动填入；输入 / 可调用本机 skill。</div>
            </div>

            <div className="field">
              <label>完成条件 · 事件</label>
              <input
                className="input"
                placeholder="Stop, SubagentStop"
                value={(node.done?.events ?? []).join(", ")}
                onChange={(e) =>
                  patchDone({
                    events: e.target.value
                      .split(",")
                      .map((s) => s.trim())
                      .filter(Boolean),
                  })
                }
              />
            </div>

            <div className="field">
              <label>完成条件 · 产物</label>
              <input
                className="input"
                placeholder="docs/prd.md"
                value={(node.done?.artifacts ?? []).join(", ")}
                onChange={(e) =>
                  patchDone({
                    artifacts: e.target.value
                      .split(",")
                      .map((s) => s.trim())
                      .filter(Boolean),
                  })
                }
              />
            </div>

            <div className="row2">
              <label className="sc-row">
                <span>需人工复核</span>
                <input
                  type="checkbox"
                  checked={!!node.review}
                  onChange={(e) => patch({ review: e.target.checked })}
                />
              </label>
              <label className="sc-row">
                <span>等待时拦截</span>
                <input
                  type="checkbox"
                  checked={!!node.blocking}
                  onChange={(e) => patch({ blocking: e.target.checked })}
                />
              </label>
            </div>
          </>
        )}
      </div>
      <div className="overlay-foot">
        <span className="hint">
          {props.creating ? "保存后会出现在绑定里的 SOP 列表" : "保存后立即生效，进行中的运行也同步"}
        </span>
        <button className="btn primary" style={{ flex: "none", padding: "0 12px" }} onClick={props.onSave}>
          {props.creating ? "创建" : "保存"}
        </button>
      </div>
    </div>
  );
}

/** 匹配文本框尾部的 `/keyword`（行首或空白之后），用于 skill 补全。 */
const SLASH_RE = /(^|\s)\/([\w-]*)$/;

/** 默认 Prompt 输入框：输入 `/` 弹出本机 skill，选中后插入字面 `/skill-name `。 */
function PromptField(props: {
  value: string;
  agent: string;
  placeholder?: string;
  onChange: (v: string) => void;
}) {
  const ref = useRef<HTMLTextAreaElement | null>(null);
  const [skills, setSkills] = useState<SkillMeta[]>([]);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);

  useEffect(() => {
    let alive = true;
    invoke<SkillMeta[]>("list_skills", { agent: props.agent || null })
      .then((list) => {
        if (alive) setSkills(list);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [props.agent]);

  // 同名 skill 可能在多个 Agent 各装一份，只展示一条。
  const items = useMemo(() => {
    const q = query.toLowerCase();
    const seen = new Set<string>();
    return skills
      .filter((s) => {
        if (q && !s.name.toLowerCase().includes(q)) return false;
        if (seen.has(s.name)) return false;
        seen.add(s.name);
        return true;
      })
      .slice(0, 6);
  }, [skills, query]);

  const insert = (name: string) => {
    const next = props.value.replace(SLASH_RE, (_m, lead: string) => `${lead}/${name} `);
    props.onChange(next);
    setOpen(false);
    ref.current?.focus();
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (!open || items.length === 0) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      setActive((i) => (i + step + items.length) % items.length);
    } else if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      insert(items[Math.min(active, items.length - 1)].name);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
    }
  };

  return (
    <div className="slash-wrap">
      <textarea
        ref={ref}
        className="textarea"
        placeholder={props.placeholder}
        value={props.value}
        onKeyDown={onKeyDown}
        onChange={(e) => {
          const v = e.target.value;
          props.onChange(v);
          const m = v.match(SLASH_RE);
          setQuery(m?.[2] ?? "");
          setActive(0);
          setOpen(!!m);
        }}
      />
      {open && items.length > 0 && (
        <div className="slash-menu">
          {items.map((s, i) => (
            <button
              key={s.name}
              className={`slash-item ${i === active ? "is-on" : ""}`}
              onMouseDown={(e) => {
                e.preventDefault();
                insert(s.name);
              }}
              onMouseEnter={() => setActive(i)}
            >
              <span className="slash-name">/{s.name}</span>
              <span className="slash-desc">{s.description || s.agent}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
