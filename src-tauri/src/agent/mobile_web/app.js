"use strict";

const S = {
  token: localStorage.getItem("free_api_web_token") || "",
  theme: localStorage.getItem("free_api_web_theme") || "default",
  models: [], modelById: {},
  roleCards: [],
  projects: [],
  sessions: [],
  view: { name: "home" },
  openGroups: new Set(JSON.parse(localStorage.getItem("free_api_open_groups") || "[]")),
  currentSessionId: null,
  flow: null,
  expandedSteps: new Set(),
  expandedThoughts: new Set(),
  taskOpts: { model_id: "", context_budget_mode: "", role_card: "" },
  selectedNewModelId: "",
  composerFiles: [],
  lastTraceStamp: "",
  goalOpen: false,
};

const BUDGETS = [
  { value: "", label: "长程编码 (默认)", sub: "保留完整历史与详细上下文" },
  { value: "deep_coding", label: "长程编码 (Deep Coding)", sub: "保留完整上下文" },
  { value: "cost_saving", label: "省流紧凑 (Cost Saving)", sub: "高压缩比裁剪" },
  { value: "ultra_fast", label: "极速模式 (Ultra Fast)", sub: "最小化上下文极速响应" },
];

function $(id) { return document.getElementById(id); }
function esc(s) {
  return String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/* 格式化 Markdown 与行内高亮 (匹配参考截图 1) */
function renderMarkdownInline(text) {
  if (!text) return "";
  let out = esc(text);
  // `code` / paths
  out = out.replace(/`([^`]+)`/g, '<code class="inline-code">$1</code>');
  // **bold**
  out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  // 文件名胶囊高亮
  out = out.replace(/\b([a-zA-Z0-9_\-\.\/]+\.(?:md|ts|js|rs|json|vue|html|css|py|yaml|yml))\b/g, '<code class="inline-code">$1</code>');
  // ① ② ③ 序号加粗美化
  out = out.replace(/([①②③④⑤⑥⑦⑧⑨⑩])/g, '<strong style="color:var(--accent2); margin-right:2px;">$1</strong>');
  return out;
}

function setConn(ok, msg) {
  const dot = $("connDot");
  dot.className = "dot " + (ok ? "ok" : "bad");
  $("connText").textContent = msg || (ok ? "已连接到当前桌面窗口" : "未连接");
}

function closeSidebarMobile() {
  const sb = $("sidebar");
  const sm = $("sideMask");
  if (sb) sb.classList.remove("show");
  if (sm) sm.classList.remove("show");
}

async function api(path, opts = {}) {
  const headers = Object.assign({ "X-Channel-Token": S.token }, opts.headers || {});
  if (opts.body && typeof opts.body === "string" && !headers["Content-Type"]) {
    headers["Content-Type"] = "application/json";
  }
  let res;
  try {
    res = await fetch(path, Object.assign({}, opts, { headers }));
  } catch (e) {
    setConn(false, "无法连接桌面端");
    throw e;
  }
  if (res.status === 401) { showGate("令牌已失效，请重新输入"); throw new Error("unauthorized"); }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || ("HTTP " + res.status));
  setConn(true);
  return data;
}

function relTime(iso) {
  if (!iso) return "";
  const t = new Date(iso).getTime();
  if (!Number.isFinite(t)) return "";
  const diff = (Date.now() - t) / 1000;
  if (diff < 60) return "刚刚";
  if (diff < 3600) return Math.floor(diff / 60) + " 分钟前";
  const d = new Date(t), now = new Date();
  const hm = d.toTimeString().slice(0, 5);
  if (d.toDateString() === now.toDateString()) return hm;
  return (d.getMonth() + 1) + "月" + d.getDate() + "日 " + hm;
}

function short(s, n) { s = String(s ?? ""); return s.length > n ? s.slice(0, n) + "…" : s; }

function toolIcon(tool) {
  const t = String(tool || "").toLowerCase();
  if (/ask|human/.test(t)) return "❓";
  if (/finish|complete|deliver/.test(t)) return "🏁";
  if (/spawn|child|worker/.test(t)) return "🤖";
  if (/plan|route/.test(t)) return "🧭";
  if (/read|open.*file/.test(t)) return "📄";
  if (/write|edit|apply|patch/.test(t)) return "✍️";
  if (/list|ls|dir|glob|tree/.test(t)) return "📂";
  if (/command|cmd|shell|exec|run/.test(t)) return "💻";
  if (/search|grep|find|query/.test(t)) return "🔍";
  if (/artifact|save|upload/.test(t)) return "📦";
  return "🔍";
}

const STATUS = {
  running:        { cls: "running",  label: "运行中" },
  waiting_human:  { cls: "waiting",  label: "等待回答" },
  succeeded:      { cls: "done",     label: "完成" },
  done:           { cls: "done",     label: "完成" },
  failed:         { cls: "failed",   label: "失败" },
  cancelled:      { cls: "cancelled",label: "已停止" },
};

function chip(status, extra) {
  const meta = STATUS[status] || { cls: "", label: status ? status : "无任务" };
  return `<span class="chip ${meta.cls}">${extra || ""}<span class="cdot"></span>${esc(meta.label)}</span>`;
}

function stepChip(status) {
  const map = { succeeded: ["done", "✓"], failed: ["failed", "✕"], finish_challenged: ["waiting", "待确认"], running: ["running", "●"] };
  const [cls, label] = map[status] || ["", status || ""];
  return `<span class="chip ${cls}">${esc(label)}</span>`;
}

function toast(msg, ms = 2000) {
  const el = document.createElement("div");
  el.className = "toast"; el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), ms);
}

function modelName(id) {
  const m = S.modelById[id];
  return m ? (m.display_name || m.name) : short(id, 12);
}

function fmtSize(n) { return n > 1048576 ? (n / 1048576).toFixed(1) + " MB" : Math.ceil(n / 1024) + " KB"; }

function artIcon(a) {
  const m = a.mime_type || "", f = (a.display_name || "").toLowerCase();
  if (m.startsWith("image/")) return "🖼️";
  if (m.includes("spreadsheet") || /\.(xlsx|csv)$/.test(f)) return "📊";
  if (m.includes("pdf")) return "📕";
  if (m.startsWith("text/") || /\.(md|txt|json)$/.test(f)) return "📄";
  return "📦";
}

/* 渲染单一步骤 (匹配截图 1 的轻量紧致风格) */
function renderStep(st) {
  const open = S.expandedSteps.has(String(st.id));
  const act = (st.action && st.action !== st.tool) ? st.action : (st.description || st.action || "");
  const desc = st.description && st.description !== act
    ? `<div class="assistant-narrative">${renderMarkdownInline(st.description)}</div>`
    : "";

  // 思考块 (🧠 思考 · 持续了几秒)
  let thoughtHtml = "";
  if (st.thought) {
    const thOpen = S.expandedThoughts.has(String(st.id));
    thoughtHtml = `
      <div class="thought-block">
        <div class="thought-header" data-thought-toggle="${esc(st.id)}">
          <span>🧠</span>
          <span>思考 · ${thOpen ? "点击收起 ▲" : "持续了几秒 ▼"}</span>
        </div>
        ${thOpen ? `<div class="thought-content">${esc(st.thought)}</div>` : ""}
      </div>
    `;
  }

  // 详细参数与输出（点击胶囊后展开）
  const params = st.command || st.params;
  const paramsHtml = params ? `<div class="label">⌨️ 参数</div><pre>${esc(typeof params === "string" ? params : JSON.stringify(params, null, 2))}</pre>` : "";
  const output = st.output ? `<div class="label">📄 输出</div><pre>${esc(st.output)}</pre>` : "";

  // 极简单行工具胶囊 (🔍 读取 📄 ...)
  const isSub = /spawn|child|worker/.test(String(st.tool || "").toLowerCase());
  const toolPill = `
    <button class="tool-call-pill" data-step="${esc(st.id)}">
      <span class="t-ic">${toolIcon(st.tool)}</span>
      <div class="t-body">
        <span class="t-name">${esc(st.tool || "工具")}</span>
        ${isSub ? `<span class="tag-badge">🤖 子智能体</span>` : ""}
        <span class="t-args">${esc(act)}</span>
      </div>
      <div class="t-right">
        ${st.timestamp ? `<span>${esc(relTime(new Date(st.timestamp).toISOString()))}</span>` : ""}
        ${stepChip(st.status)}
      </div>
    </button>
  `;

  return `
    <div class="step-container">
      ${desc}
      ${thoughtHtml}
      ${toolPill}
      ${open ? `<div class="step-detail-box">${paramsHtml}${output}</div>` : ""}
    </div>
  `;
}

// ---------------------------------------------------------------- 数据加载与渲染
async function loadHome() {
  const [projRes, sessRes] = await Promise.all([
    api("/channel/projects"),
    api("/channel/sessions"),
  ]);
  S.projects = projRes.projects || [];
  S.sessions = sessRes.sessions || [];
  if (S.openGroups.size === 0) {
    if (S.projects.length) S.openGroups.add(S.projects[0].id);
    S.openGroups.add("__scratch__");
  }
  renderHome();
  renderSidebar();
}

async function loadFlow(sessionId) {
  const [sessRes, runsRes, tracesRes, artsRes] = await Promise.all([
    api("/channel/sessions"),
    api("/channel/runs"),
    api(`/channel/session/${encodeURIComponent(sessionId)}/traces`),
    api(`/channel/session/${encodeURIComponent(sessionId)}/artifacts`),
  ]);
  S.sessions = sessRes.sessions || [];
  const session = S.sessions.find(s => s.id === sessionId) || null;
  const run = (runsRes.runs || []).filter(r => r.session_id === sessionId)
    .sort((a, b) => (b.started_at || "").localeCompare(a.started_at || ""))[0] || null;
  const traces = tracesRes.traces || [];
  const latest = traces.length ? traces[traces.length - 1] : null;
  const stamp = latest ? `${latest.id}:${latest.step_count}:${latest.updated_at}` : "";
  let trace = S.flow && S.flow.sessionId === sessionId ? S.flow.trace : null;
  if (latest && (stamp !== S.lastTraceStamp || !trace)) {
    const detail = await api(`/channel/trace/${encodeURIComponent(latest.id)}`);
    trace = detail.trace;
  }
  S.lastTraceStamp = stamp;
  S.flow = { sessionId, session, run, trace, traces, artifacts: artsRes.artifacts || [] };
  renderFlow();
}

function groupsOf() {
  const groups = [];
  for (const p of S.projects) {
    groups.push({ key: p.id, icon: "📁", name: p.name, sub: p.root_path, tag: "本地",
      sessions: S.sessions.filter(s => s.project_id === p.id) });
  }
  const scratchSessions = S.sessions.filter(s => !s.project_id);
  groups.push({ key: "__scratch__", icon: "💬", name: "任务", sub: "临时会话", tag: "对话", sessions: scratchSessions });
  return groups;
}

function taskCard(s) {
  const title = esc(s.goal || s.title);
  const time = esc(relTime(s.trace_updated_at || s.updated_at));
  return `
    <div class="task-row-card" data-session="${esc(s.id)}">
      <div class="tt">
        <div class="title">${title}</div>
        <div class="sub-line">${time} · ${esc(modelName(s.model_id))}</div>
      </div>
      ${chip(s.status)}
    </div>
  `;
}

function renderHome() {
  $("topTitle").textContent = "free_api 远程控制";
  $("topSub").innerHTML = `<span id="connDot" class="dot ok"></span><span id="connText">已连接到当前桌面窗口</span>`;
  $("btnBackTop").classList.add("hidden");
  $("btnMenu").classList.remove("hidden");
  $("homeMeta").textContent = `${S.projects.length + 1} 个工作区 · ${S.sessions.length} 个任务`;
  
  const groups = groupsOf();
  $("wsList").innerHTML = groups.map(g => {
    const open = S.openGroups.has(g.key);
    const tasks = g.sessions.length
      ? g.sessions.slice(0, 15).map(s => taskCard(s)).join("")
      : `<div class="empty-tip">暂无任务，点击右上 ＋ 发起</div>`;
    return `
      <div class="ws-card ${open ? "open" : ""}" data-group="${esc(g.key)}">
        <div class="ws-head" role="button" data-toggle="${esc(g.key)}">
          <div class="ws-icon-box">${g.icon}</div>
          <div class="ws-info">
            <div class="ws-name-row">
              <span class="ws-name">${esc(g.name)}</span>
              ${g.tag ? `<span class="tag-badge">${g.tag}</span>` : ""}
            </div>
            <div class="ws-path">${esc(g.sub || "")}</div>
          </div>
          <div class="ws-meta-count">${g.sessions.length} 个任务 <span class="ws-chev">⌄</span></div>
          <button class="ws-plus-btn" data-new="${esc(g.key)}" title="在此工作区发起新任务">＋</button>
        </div>
        <div class="ws-tasks">${tasks}</div>
      </div>
    `;
  }).join("");
}

function renderSidebar() {
  const groups = groupsOf();
  $("sidebar").innerHTML = `<div style="padding:4px 2px 10px">
      <button class="btn primary block" data-new="__scratch__">🚀 在临时工作区发任务</button>
    </div>` + groups.map(g => {
    const open = S.openGroups.has(g.key);
    const items = g.sessions.slice(0, 12).map(s => `
      <button class="side-task ${S.currentSessionId === s.id ? "active" : ""}" data-session="${esc(s.id)}">
        <div class="t">${esc(short(s.goal || s.title, 22))}</div>
        <div class="time">${esc(relTime(s.trace_updated_at || s.updated_at))}</div>
      </button>
    `).join("");
    return `
      <div class="side-group ${open ? "open" : ""}" data-sgroup="${esc(g.key)}">
        <button class="side-head" data-toggle="${esc(g.key)}">
          <span>${g.icon}</span><span class="name">${esc(g.name)}</span>
          <span class="count">${g.sessions.length}</span><span class="chev">⌄</span>
        </button>
        <div class="side-sessions">${items || `<div class="empty-tip">无任务</div>`}</div>
      </div>
    `;
  }).join("");
}

function toggleGroup(key) {
  if (S.openGroups.has(key)) S.openGroups.delete(key); else S.openGroups.add(key);
  localStorage.setItem("free_api_open_groups", JSON.stringify([...S.openGroups]));
  renderHome(); renderSidebar();
}

function renderFlow() {
  const f = S.flow;
  if (!f) return;
  const s = f.session || {};
  const active = f.run && ["running", "waiting_human"].includes(f.run.status);
  const status = active ? f.run.status : (f.trace ? f.trace.status : (s.status || null));

  $("topTitle").textContent = "任务会话";
  $("topSub").textContent = short(s.goal || f.trace?.goal || s.title || "任务执行中", 20);
  $("btnBackTop").classList.remove("hidden");
  $("btnMenu").classList.add("hidden");

  // 用户目标气泡 (User Bubble)
  const goalText = s.goal || f.trace?.goal || s.title || "";
  const goalMore = goalText.length > 70 ? `<span class="gmore">${S.goalOpen ? "收起 ▲" : "展开全文 ▼"}</span>` : "";
  $("flowGoal").innerHTML = `<span class="arrow">→</span><span class="gtxt">${esc(goalText)}${goalMore}</span>`;
  $("flowGoal").onclick = () => { S.goalOpen = !S.goalOpen; renderFlow(); };

  // 实时状态行
  const live = $("liveLine");
  if (f.run && f.run.status === "running") {
    live.className = "live-line";
    live.innerHTML = `<span class="livedot"></span><strong>工作中</strong> <span style="color:var(--dim)">· ${esc(f.run.state_detail || f.run.phase || "执行中")}${f.run.event_count ? ` · ${f.run.event_count} 事件` : ""}</span>`;
  } else if (f.run && f.run.status === "waiting_human") {
    live.className = "live-line wait";
    live.innerHTML = `<span class="livedot"></span><span style="color:var(--amber); font-weight:600">等待你的回答</span>`;
  } else {
    live.className = "live-line";
    live.innerHTML = `<span style="color:var(--dim)">${esc(s.project_name || "临时任务")} · ${esc(modelName(s.model_id))}${f.trace ? ` · ${f.trace.step_count ?? (f.trace.steps || []).length} 步` : ""}</span>`;
  }

  // 提问卡片与结果卡片
  const paused = f.run && f.run.status === "waiting_human" ? (f.run.paused_reason || "Agent 请求人工协助。") : null;
  $("askCard").innerHTML = paused
    ? `<div class="ask-card"><div class="rlabel">❓ Agent 提问</div><div class="q">${esc(paused)}</div><div class="hint">在底部输入框直接回复即可</div></div>`
    : "";

  const failed = status === "failed";
  const resultText = (f.run && f.run.error) ? f.run.error
    : (f.run && f.run.result) ? f.run.result
    : (f.trace && f.trace.completion && f.trace.completion.summary) ? f.trace.completion.summary : null;
  $("resultCard").innerHTML = (!active && resultText)
    ? `<div class="result-card ${failed ? "fail" : ""}"><div class="rlabel">${failed ? "✕ 失败原因" : "✓ 执行结果"}</div>${esc(resultText)}</div>`
    : "";

  // 步骤列表渲染
  const steps = (f.trace && f.trace.steps) || [];
  const stepHtmls = steps.map(st => renderStep(st)).join("");
  $("timeline").innerHTML = stepHtmls || `<div class="empty-tip">等待执行记录…</div>`;

  // 产物清单
  const arts = f.artifacts || [];
  $("artBox").innerHTML = arts.length ? `<div style="margin-top:12px;"><div style="font-size:12px; font-weight:700; margin-bottom:6px;">📦 任务产物 (${arts.length})</div>` +
    arts.map(a => `
      <div style="display:flex; align-items:center; gap:6px; background:var(--card); border:1px solid var(--line); border-radius:8px; padding:6px 10px; margin-bottom:4px;">
        <span>${artIcon(a)}</span>
        <span style="flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; font-size:12px;">${esc(a.display_name)}</span>
        <span style="color:var(--dim); font-size:10.5px;">${fmtSize(a.size_bytes || 0)}</span>
        <a class="btn primary" style="padding:2px 7px; font-size:10.5px;" href="/channel/artifact/${encodeURIComponent(a.id)}/raw?token=${encodeURIComponent(S.token)}&dl=1" download>下载</a>
      </div>
    `).join("") + `</div>` : "";

  // Composer 状态
  const input = $("followInput");
  $("btnStop").classList.toggle("hidden", !(f.run && f.run.status === "running"));
  if (f.run && f.run.status === "waiting_human") {
    input.placeholder = "回复 Agent 的提问…";
  } else if (f.run && f.run.status === "running") {
    input.placeholder = "继续输入以排队后续修改 / 接管方向…";
  } else {
    input.placeholder = "在当前会话发起下一个任务…";
  }
}

async function openSession(sessionId) {
  closeSidebarMobile();
  S.currentSessionId = sessionId;
  S.view = { name: "flow" };
  $("viewHome").classList.add("hidden");
  $("viewFlow").classList.remove("hidden");
  S.lastTraceStamp = "";
  $("timeline").innerHTML = `<div class="empty-tip">加载中…</div>`;
  try { await loadFlow(sessionId); } catch (e) { toast("加载失败：" + e.message); }
}

function goHome() {
  closeSidebarMobile();
  S.currentSessionId = null;
  S.view = { name: "home" };
  $("viewFlow").classList.add("hidden");
  $("viewHome").classList.remove("hidden");
  renderHome(); renderSidebar();
  loadHome().catch(() => {});
}

async function sendComposer() {
  const input = $("followInput");
  const text = input.value.trim();
  if (!text || !S.flow) return;
  const f = S.flow;
  try {
    if (f.run && f.run.status === "waiting_human") {
      await api(`/channel/run/${f.run.run_id}/answer`, { method: "POST", body: JSON.stringify({ text }) });
      toast("已回答，Agent 继续执行");
    } else if (f.run && f.run.status === "running") {
      await api(`/channel/run/${f.run.run_id}/goal`, { method: "POST", body: JSON.stringify({ goal: text }) });
      toast("已排队后续修改 (ReplaceGoal)");
    } else {
      for (let i = 0; i < S.composerFiles.length; i++) {
        toast(`上传附件 ${i + 1}/${S.composerFiles.length}…`, 1500);
        await uploadFile(f.sessionId, S.composerFiles[i]);
      }
      S.composerFiles = [];
      const routing = {};
      if (S.taskOpts.model_id) routing.pro = routing.flash = routing.vision = S.taskOpts.model_id;
      if (S.taskOpts.context_budget_mode) routing.context_budget_mode = S.taskOpts.context_budget_mode;
      const body = { session_id: f.sessionId, goal: text };
      if (Object.keys(routing).length) body.model_routing = routing;
      await api("/channel/task", { method: "POST", body: JSON.stringify(body) });
      toast("任务已启动");
      S.lastTraceStamp = "";
    }
    input.value = "";
    await loadFlow(f.sessionId);
  } catch (e) { toast("操作失败：" + e.message); }
}

async function stopRun() {
  const f = S.flow;
  if (!f || !f.run) return;
  try {
    await api(`/channel/run/${f.run.run_id}/cancel`, { method: "POST" });
    toast("已请求停止");
    await loadFlow(f.sessionId);
  } catch (e) { toast("停止失败：" + e.message); }
}

// ---------------------------------------------------------------- 带实时搜索的选择弹层
function openSearchableSheet(title, allItems, currentValue, onPick) {
  $("sheetTitle").textContent = title;
  const searchWrap = $("sheetSearchWrap");
  const searchInput = $("sheetSearchInput");
  searchWrap.style.display = allItems.length > 5 ? "" : "none";
  searchInput.value = "";

  function renderList(query) {
    const q = (query || "").toLowerCase().trim();
    const filtered = allItems.filter(it => {
      if (!q) return true;
      return it.label.toLowerCase().includes(q) || (it.sub && it.sub.toLowerCase().includes(q)) || (it.value && it.value.toLowerCase().includes(q));
    });

    $("sheetList").innerHTML = filtered.length ? filtered.map(it => `
      <button class="sheet-model-row ${it.value === currentValue ? "selected" : ""}" data-v="${esc(it.value)}">
        <div style="flex:1; min-width:0;">
          <div class="m-title">${esc(it.label)}</div>
          ${it.sub ? `<div class="m-sub">${esc(it.sub)}</div>` : ""}
        </div>
        ${it.value === currentValue ? '<span style="font-weight:bold;">✓</span>' : ""}
      </button>
    `).join("") : `<div class="empty-tip">未找到匹配项</div>`;

    $("sheetList").querySelectorAll("button").forEach(btn => {
      btn.onclick = () => {
        onPick(btn.dataset.v);
        $("sheetPick").classList.add("hidden");
      };
    });
  }

  searchInput.oninput = (e) => renderList(e.target.value);
  renderList("");
  $("sheetPick").classList.remove("hidden");
  if (allItems.length > 5) setTimeout(() => searchInput.focus(), 150);
}

function bindOptChips() {
  $("optModel").onclick = () => openSearchableSheet("选择任务模型",
    [{ value: "", label: "跟随会话模型", sub: "使用默认设置" }].concat(S.models.map(m => ({ value: m.id, label: m.display_name || m.name, sub: m.platform_name || "" }))),
    S.taskOpts.model_id,
    (v) => { S.taskOpts.model_id = v; $("lblModel").textContent = v ? short(modelName(v), 4) : "跟随"; $("optModel").classList.toggle("active", !!v); });
  
  $("optBudget").onclick = () => openSearchableSheet("上下文压缩模式", BUDGETS, S.taskOpts.context_budget_mode,
    (v) => {
      S.taskOpts.context_budget_mode = v;
      const bmap = { deep_coding: "长程", cost_saving: "省流", ultra_fast: "极速" };
      $("lblBudget").textContent = v ? (bmap[v] || "长程") : "长程";
      $("optBudget").classList.toggle("active", !!v && v !== "deep_coding");
    });

  $("optRoles").onclick = () => openSearchableSheet("角色卡钉死",
    [{ value: "", label: "跟随默认路由", sub: "不钉死任何角色" }].concat(S.roleCards.map(c => ({ value: c.name, label: c.label || c.name, sub: `作用域 ${c.scope_hints || "-"}` }))),
    S.taskOpts.role_card,
    (v) => { S.taskOpts.role_card = v; $("lblRoles").textContent = v ? short(v, 4) : "默认"; $("optRoles").classList.toggle("active", !!v); });

  $("sheetClose").onclick = () => $("sheetPick").classList.add("hidden");
  $("sheetPick").addEventListener("click", (e) => { if (e.target === $("sheetPick")) $("sheetPick").classList.add("hidden"); });
}

let newTaskType = "scratch";
function openModal(prefillProjectId) {
  closeSidebarMobile();
  newTaskType = prefillProjectId && prefillProjectId !== "__scratch__" ? "project" : "scratch";
  const projects = groupsOf().filter(g => g.key !== "__scratch__");
  $("newProj").innerHTML = projects.map(g =>
    `<option value="${esc(g.key)}" ${g.key === prefillProjectId ? "selected" : ""}>${g.icon} ${esc(g.name)}</option>`).join("")
    || `<option value="">（暂无项目）</option>`;
  $("typeScratch").style.background = newTaskType === "scratch" ? "var(--accent)" : "transparent";
  $("typeProject").style.background = newTaskType === "project" ? "var(--accent)" : "transparent";
  $("projField").style.display = newTaskType === "project" ? "" : "none";
  
  if (!S.selectedNewModelId && S.models.length) {
    S.selectedNewModelId = S.models[0].id;
  }
  $("lblNewModel").textContent = S.selectedNewModelId ? modelName(S.selectedNewModelId) : "选择模型…";
  
  $("newGoal").value = "";
  $("modalNew").classList.remove("hidden");
}

async function submitNew() {
  const projectId = newTaskType === "project" ? $("newProj").value : null;
  const modelId = S.selectedNewModelId || (S.models[0] ? S.models[0].id : "");
  const goal = $("newGoal").value.trim();
  if (!goal) { toast("请填写任务内容"); return; }
  const btn = $("newSubmit"); btn.disabled = true;
  try {
    const sess = await api("/channel/session", {
      method: "POST",
      body: JSON.stringify({ project_id: projectId, title: short(goal.replace(/\s+/g, " "), 28), model_id: modelId }),
    });
    $("modalNew").classList.add("hidden");
    await loadHome();
    await openSession(sess.session_id);
    await api("/channel/task", { method: "POST", body: JSON.stringify({ session_id: sess.session_id, goal }) });
    toast("任务已启动");
    await loadFlow(sess.session_id);
  } catch (e) {
    toast("创建失败: " + e.message);
  } finally {
    btn.disabled = false;
  }
}

function applyTheme(theme) {
  S.theme = theme;
  localStorage.setItem("free_api_web_theme", theme);
  if (theme === "default") document.documentElement.removeAttribute("data-theme");
  else document.documentElement.setAttribute("data-theme", theme);
}

function cycleTheme() {
  const themes = ["default", "oled", "midnight"];
  const next = themes[(themes.indexOf(S.theme) + 1) % themes.length];
  applyTheme(next);
  toast(`主题: ${next === "default" ? "深灰" : next === "oled" ? "纯黑" : "深蓝"}`);
}

function bind() {
  $("gateSave").onclick = async () => {
    const token = $("gateToken").value.trim();
    if (!token) { $("gateErr").textContent = "请粘贴 Token"; return; }
    $("gateSave").disabled = true;
    const ok = await tryConnect(token);
    $("gateSave").disabled = false;
  };
  $("gateToken").addEventListener("keydown", (e) => { if (e.key === "Enter") $("gateSave").click(); });

  $("btnMenu").onclick = () => { $("sidebar").classList.toggle("show"); $("sideMask").classList.toggle("show"); };
  $("sideMask").onclick = closeSidebarMobile;
  $("btnBackTop").onclick = goHome;
  $("btnRefresh").onclick = () => {
    if (S.view.name === "flow" && S.currentSessionId) loadFlow(S.currentSessionId).catch(e => toast(e.message));
    else loadHome().catch(() => {});
  };
  $("btnToggleAll").onclick = () => {
    const allKeys = groupsOf().map(g => g.key);
    if (S.openGroups.size > 0) S.openGroups.clear();
    else allKeys.forEach(k => S.openGroups.add(k));
    renderHome(); renderSidebar();
  };
  $("btnTheme").onclick = cycleTheme;
  $("btnNew").onclick = () => openModal("__scratch__");
  $("btnSend").onclick = sendComposer;
  $("btnStop").onclick = stopRun;
  $("btnClip").onclick = () => $("clipInput").click();
  $("clipInput").onchange = (e) => { S.composerFiles.push(...e.target.files); e.target.value = ""; toast(`已选择 ${S.composerFiles.length} 个附件`); };

  bindOptChips();

  // 新建任务弹窗中的模型搜索选择器
  $("btnPickNewModel").onclick = () => {
    openSearchableSheet("选择执行模型",
      S.models.map(m => ({ value: m.id, label: m.display_name || m.name, sub: m.platform_name || "" })),
      S.selectedNewModelId,
      (v) => {
        S.selectedNewModelId = v;
        $("lblNewModel").textContent = modelName(v);
      }
    );
  };

  $("typeScratch").onclick = () => { newTaskType = "scratch"; $("projField").style.display = "none"; $("typeScratch").style.background = "var(--accent)"; $("typeProject").style.background = "transparent"; };
  $("typeProject").onclick = () => { newTaskType = "project"; $("projField").style.display = ""; $("typeProject").style.background = "var(--accent)"; $("typeScratch").style.background = "transparent"; };
  $("newCancel").onclick = () => $("modalNew").classList.add("hidden");
  $("newSubmit").onclick = submitNew;

  // 快捷回到底部
  window.addEventListener("scroll", () => {
    const show = window.scrollY < (document.documentElement.scrollHeight - window.innerHeight - 250);
    $("btnScrollBottom").classList.toggle("hidden", !show || S.view.name !== "flow");
  });
  $("btnScrollBottom").onclick = () => { window.scrollTo({ top: document.documentElement.scrollHeight, behavior: "smooth" }); };

  // 全局事件代理
  document.addEventListener("click", (e) => {
    const th = e.target.closest("[data-thought-toggle]");
    if (th) {
      const id = th.dataset.thoughtToggle;
      if (S.expandedThoughts.has(id)) S.expandedThoughts.delete(id); else S.expandedThoughts.add(id);
      renderFlow();
      return;
    }
    const st = e.target.closest("[data-step]");
    if (st) {
      const id = st.dataset.step;
      if (S.expandedSteps.has(id)) S.expandedSteps.delete(id); else S.expandedSteps.add(id);
      renderFlow();
      return;
    }
    const plus = e.target.closest("[data-new]");
    if (plus) { e.stopPropagation(); openModal(plus.dataset.new); return; }
    const toggle = e.target.closest("[data-toggle]");
    if (toggle) { toggleGroup(toggle.dataset.toggle); return; }
    const sess = e.target.closest("[data-session]");
    if (sess) { openSession(sess.dataset.session); return; }
  });
}

function showGate(err) {
  $("gateErr").textContent = err || "";
  $("gate").classList.remove("hidden");
  $("app").classList.add("hidden");
}

async function tryConnect(token) {
  S.token = token;
  try {
    await api("/channel/config");
    localStorage.setItem("free_api_web_token", token);
    $("gate").classList.add("hidden");
    $("app").classList.remove("hidden");
    boot();
    return true;
  } catch (e) {
    if (e.message !== "unauthorized") showGate("连接失败: " + e.message);
    return false;
  }
}

async function loadModels() {
  try {
    const data = await api("/channel/models");
    S.models = (data.models || []).filter(m => m.enabled !== 0 && m.enabled !== false);
    S.modelById = Object.fromEntries(S.models.map(m => [m.id, m]));
    if (!S.selectedNewModelId && S.models.length) {
      S.selectedNewModelId = S.models[0].id;
    }
  } catch (e) {}
}

function connectSSE(force) {
  if (!window.EventSource) return;
  if (S.es && S.es.readyState <= 1 && !force) return;
  if (S.es) { try { S.es.close(); } catch (e) {} S.es = null; }
  try {
    const es = new EventSource(`/channel/events?token=${encodeURIComponent(S.token)}`);
    S.es = es;
    es.onopen = () => { setConn(true); };
    es.onmessage = (e) => {
      if (S.view.name === "flow" && S.currentSessionId) loadFlow(S.currentSessionId).catch(() => {});
      else if (S.view.name === "home") loadHome().catch(() => {});
    };
    es.onerror = () => { if (es.readyState === 2) setTimeout(() => connectSSE(true), 3000); };
  } catch (e) {}
}

async function boot() {
  applyTheme(S.theme);
  bind();
  await loadModels().catch(() => {});
  await loadHome().catch(() => {});
  connectSSE();
  setInterval(() => {
    if (document.hidden) return;
    if (S.view.name === "flow" && S.currentSessionId) loadFlow(S.currentSessionId).catch(() => {});
    else if (S.view.name === "home") loadHome().catch(() => {});
  }, 3000);
}

(function init() {
  const url = new URL(location.href);
  const urlToken = url.searchParams.get("token");
  if (urlToken) {
    url.searchParams.delete("token");
    history.replaceState(null, "", url.pathname + (url.searchParams.toString() ? "?" + url.searchParams : ""));
    localStorage.setItem("free_api_web_token", urlToken);
    S.token = urlToken;
  }
  if (S.token) tryConnect(S.token);
  else showGate();
})();
