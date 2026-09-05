"use strict";

const S = {
  token: localStorage.getItem("free_api_web_token") || "",
  theme: localStorage.getItem("free_api_web_theme") || "default",
  models: [], modelById: {},
  roleCards: [],
  projects: [],
  expandedPlans: new Set(),
  collapsedPlans: new Set(),
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
  sheetOpen: false,  // 弹层打开时禁止轮询重渲
  humanForm: {
    runId: "",
    confirmValue: true,
    singleSelectedId: "",
    multiSelectedIds: [],
    writeInText: "",
    isSubmitting: false,
  },
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

/* ---------- 完整超全 Markdown 与富文本解析引擎 ---------- */
function formatInline(text) {
  if (!text) return "";
  let out = esc(text);
  
  // 1. 行内代码 `code`
  out = out.replace(/`([^`]+)`/g, '<code class="inline-code">$1</code>');
  
  // 2. 加粗 **bold**
  out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  
  // 3. 斜体 *italic*
  out = out.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
  
  // 4. 删除线 ~~del~~
  out = out.replace(/~~([^~]+)~~/g, '<del class="md-del">$1</del>');
  
  // 5. 超链接 [title](url)
  out = out.replace(/\[([^\]]+)\]\((https?:\/\/[^\s\)]+)\)/g, '<a class="md-link" href="$2" target="_blank" rel="noopener">$1 ↗</a>');
  
  // 6. 文件路径与代码文件自动高亮胶囊
  out = out.replace(/\b([a-zA-Z0-9_\-\.\/]+\.(?:md|ts|js|rs|json|vue|html|css|py|yaml|yml|toml|sql|sh))\b/g, '<code class="inline-code">$1</code>');
  
  // 7. 序号加粗美化
  out = out.replace(/([①②③④⑤⑥⑦⑧⑨⑩])/g, '<strong style="color:var(--accent2); margin-right:2px;">$1</strong>');
  
  return out;
}

function formatMarkdown(raw) {
  if (!raw) return "";
  let text = String(raw).trim();
  
  // 1. 代码块提取并占位（附带语言标头与复制按钮）
  const codeBlocks = [];
  text = text.replace(/```([a-zA-Z0-9_\-\.]*)\n([\s\S]*?)```/g, (m, lang, code) => {
    const idx = codeBlocks.length;
    codeBlocks.push({ lang: lang || "code", code: code.trim() });
    return `\n\n__CODE_BLOCK_${idx}__\n\n`;
  });

  // 2. 按行逐段处理
  const lines = text.split("\n");
  const processed = [];
  let tableRows = [];
  
  function flushTable() {
    if (!tableRows.length) return;
    let html = '<div class="md-table-wrap"><table class="md-table">';
    let isHeader = true;
    for (let r of tableRows) {
      const cells = r.split("|").map(c => c.trim()).filter((c, i, a) => !(i === 0 && c === "") && !(i === a.length - 1 && c === ""));
      if (cells.length && cells.every(c => /^[-:\s]+$/.test(c))) {
        isHeader = false;
        continue;
      }
      html += '<tr>';
      for (let c of cells) {
        html += isHeader ? `<th>${formatInline(c)}</th>` : `<td>${formatInline(c)}</td>`;
      }
      html += '</tr>';
      isHeader = false;
    }
    html += '</table></div>';
    processed.push(html);
    tableRows = [];
  }

  for (let i = 0; i < lines.length; i++) {
    let l = lines[i].trim();
    
    // 表格识别
    if (l.startsWith("|") && l.endsWith("|")) {
      tableRows.push(l);
      continue;
    } else {
      flushTable();
    }
    
    if (!l) continue;
    
    // 代码块还原
    const cbMatch = l.match(/^__CODE_BLOCK_(\d+)__$/);
    if (cbMatch) {
      const item = codeBlocks[parseInt(cbMatch[1])];
      if (item) {
        processed.push(`
          <div class="md-code-box">
            <div class="md-code-head">
              <span>${esc(item.lang)}</span>
              <button class="md-copy-btn" onclick="copyCode(this)">📋 复制</button>
            </div>
            <pre><code>${esc(item.code)}</code></pre>
          </div>
        `);
      }
      continue;
    }

    // 分割线
    if (/^(---|___|\*\*\*)$/.test(l)) {
      processed.push('<hr class="md-hr">');
      continue;
    }

    // 标题
    if (l.startsWith("#### ")) { processed.push(`<div class="md-h4">${formatInline(l.slice(5))}</div>`); continue; }
    if (l.startsWith("### ")) { processed.push(`<div class="md-h3">${formatInline(l.slice(4))}</div>`); continue; }
    if (l.startsWith("## ")) { processed.push(`<div class="md-h2">${formatInline(l.slice(3))}</div>`); continue; }
    if (l.startsWith("# ")) { processed.push(`<div class="md-h1">${formatInline(l.slice(2))}</div>`); continue; }

    // GitHub 风格 Alert Callout
    if (l.startsWith("> [!NOTE]")) { processed.push(`<div class="md-callout note"><div class="md-callout-title">ℹ️ 说明</div>${formatInline(l.slice(9).trim())}</div>`); continue; }
    if (l.startsWith("> [!TIP]")) { processed.push(`<div class="md-callout tip"><div class="md-callout-title">💡 提示</div>${formatInline(l.slice(8).trim())}</div>`); continue; }
    if (l.startsWith("> [!WARNING]")) { processed.push(`<div class="md-callout warning"><div class="md-callout-title">⚠️ 警告</div>${formatInline(l.slice(12).trim())}</div>`); continue; }
    if (l.startsWith("> [!IMPORTANT]")) { processed.push(`<div class="md-callout important"><div class="md-callout-title">❗ 重要</div>${formatInline(l.slice(14).trim())}</div>`); continue; }

    // 引用块
    if (l.startsWith(">")) {
      processed.push(`<blockquote class="md-blockquote">${formatInline(l.slice(1).trim())}</blockquote>`);
      continue;
    }

    // 任务复选框清单
    if (/^-\s*\[x\]/i.test(l)) {
      processed.push(`<div class="md-task-item"><span class="md-checkbox checked">☑</span><span>${formatInline(l.replace(/^-\s*\[x\]/i, "").trim())}</span></div>`);
      continue;
    }
    if (/^-\s*\[\s*\]/i.test(l)) {
      processed.push(`<div class="md-task-item"><span class="md-checkbox">☐</span><span>${formatInline(l.replace(/^-\s*\[\s*\]/i, "").trim())}</span></div>`);
      continue;
    }

    // 无序列表
    if (/^[-*]\s+/.test(l)) {
      processed.push(`<div class="md-list-item"><span class="md-list-bullet">•</span><span>${formatInline(l.replace(/^[-*]\s+/, ""))}</span></div>`);
      continue;
    }

    // 有序列表 / 序号
    const numMatch = l.match(/^(\d+\.|[①②③④⑤⑥⑦⑧⑨⑩])\s*(.*)$/);
    if (numMatch) {
      processed.push(`<div class="md-list-item"><span class="md-list-bullet">${numMatch[1]}</span><span>${formatInline(numMatch[2])}</span></div>`);
      continue;
    }

    // 普通段落
    processed.push(`<p>${formatInline(l)}</p>`);
  }
  flushTable();
  
  return processed.join("");
}

window.copyCode = function(btn) {
  const box = btn.closest(".md-code-box");
  if (!box) return;
  const code = box.querySelector("pre code")?.textContent || "";
  navigator.clipboard.writeText(code).then(() => {
    btn.textContent = "✓ 已复制";
    setTimeout(() => { btn.textContent = "📋 复制"; }, 1500);
  }).catch(() => {
    toast("复制失败");
  });
};

function setConn(ok, msg) {
  const dot = $("connDot");
  if (dot) dot.className = "dot " + (ok ? "ok" : "bad");
  const txt = $("connText");
  if (txt) txt.textContent = msg || (ok ? "已连接到当前桌面窗口" : "未连接");
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

async function uploadFile(sessionId, file) {
  const buf = await file.arrayBuffer();
  return api(`/channel/session/${encodeURIComponent(sessionId)}/attachment-upload`, {
    method: "POST",
    headers: {
      "Content-Type": "application/octet-stream",
      "X-File-Name": encodeURIComponent(file.name),
    },
    body: buf,
  });
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

const TOOL_NAME_MAP = {
  search: '搜索检索', ripgrep_search: '代码检索', read_text_file: '读取代码',
  edit_file: '精确修改', write_file: '写入文件', shell_exec: '终端执行',
  task_store: '归档状态', finish: '成果交付', ask_human: '用户协同',
  web_search: '全网调研', web_fetch: '抓取网页'
};

/* 格式化工具名称标签 */
function formatToolBadge(tool) {
  if (!tool) return '思考分析';
  const name = tool.includes('/') ? tool.split('/').pop() : tool;
  return TOOL_NAME_MAP[name] || name;
}

/* 渲染二级树形子步骤 */
function renderTreeSubStep(st, sIdx, isLast) {
  const open = S.expandedSteps.has(String(st.id));
  const tool = String(st.tool || "工具").trim();
  const act = (st.action && st.action !== st.tool) ? st.action : (st.description || st.action || "");
  
  let shortTool = formatToolBadge(tool);
  let formattedMain = esc(act || tool || `步骤 #${sIdx + 1}`);
  formattedMain = formattedMain.replace(/\b([a-zA-Z0-9_\-\.\/]+\.(?:md|ts|js|rs|json|vue|html|css|py|yaml|yml|toml))\b/g, '<span class="t-target">$1</span>');

  // 详细参数与输出
  const params = st.command || st.params;
  const paramsHtml = params ? `<div class="label">⌨️ 参数</div><pre>${esc(typeof params === "string" ? params : JSON.stringify(params, null, 2))}</pre>` : "";
  const output = st.output ? `<div class="label">📄 输出</div><pre>${esc(st.output)}</pre>` : "";

  // 思考块
  let thoughtHtml = "";
  if (st.thought) {
    const thOpen = S.expandedThoughts.has(String(st.id));
    thoughtHtml = `
      <div class="thought-block">
        <div class="thought-header" data-thought-toggle="${esc(st.id)}">
          <span>🧠</span>
          <span>思考过程 · ${thOpen ? "收起 ▲" : "展开 ▼"}</span>
        </div>
        ${thOpen ? `<div class="thought-content">${formatMarkdown(st.thought)}</div>` : ""}
      </div>
    `;
  }

  const isLastClass = isLast ? "is-last" : "";

  return `
    <div class="mobile-sub-step-row ${isLastClass} ${esc(st.status || 'pending')}">
      <div class="mobile-tree-node-col">
        <span class="mobile-branch-line"></span>
        <span class="mobile-node-dot ${esc(st.status || 'pending')}"></span>
      </div>
      <div class="mobile-node-main">
        <div class="mobile-node-header" data-step="${esc(st.id)}">
          <span class="mobile-tool-badge">${esc(shortTool)}</span>
          <span class="mobile-step-desc" title="${esc(act)}">${formattedMain}</span>
          <span class="mobile-status-tag">${stepChip(st.status)}</span>
        </div>
        ${thoughtHtml}
        ${open ? `<div class="step-detail-box">${paramsHtml}${output}</div>` : ""}
      </div>
    </div>
  `;
}

/* 🌟 将物理步骤按阶段上下级聚合成树形折叠清单 */
function renderStepsTree(steps) {
  if (!steps || !steps.length) return "";
  
  // 按 planId 或 planTitle 聚合成阶段组
  const groups = [];
  const groupMap = new Map();
  
  for (const st of steps) {
    const planKey = st.planId || st.planTitle || (steps.length > 3 ? "group-main" : `step-${st.id}`);
    let g = groupMap.get(planKey);
    if (!g) {
      g = {
        id: `plan-${planKey}`,
        title: st.planTitle || (st.description && st.description.length <= 40 ? st.description : "阶段推进与执行"),
        status: "succeeded",
        steps: [],
        hasRunning: false,
        hasFailed: false
      };
      groupMap.set(planKey, g);
      groups.push(g);
    }
    g.steps.push(st);
    if (st.status === "running" || st.status === "executing") g.hasRunning = true;
    if (st.status === "failed" || st.status === "error") g.hasFailed = true;
  }

  // 阶段状态汇总与默认展开判定
  for (const g of groups) {
    if (g.hasRunning) g.status = "running";
    else if (g.hasFailed) g.status = "failed";
    else g.status = "succeeded";
  }

  return groups.map((g, gIdx) => {
    // 默认展开正在运行的阶段或最后一个阶段，或者用户显式展开
    const defaultOpen = g.hasRunning || gIdx === groups.length - 1;
    const isOpen = S.expandedPlans.has(g.id) || (defaultOpen && !S.collapsedPlans.has(g.id));
    const statusIcon = g.status === "running" 
      ? '<span class="mobile-plan-spinner"></span>' 
      : (g.status === "failed" ? '<span class="mobile-plan-ic fail">✕</span>' : '<span class="mobile-plan-ic ok">✓</span>');

    const subStepsHtml = g.steps.map((st, sIdx) => renderTreeSubStep(st, sIdx, sIdx === g.steps.length - 1)).join("");

    return `
      <div class="mobile-plan-tree-group ${g.status} ${isOpen ? 'is-expanded' : ''}" id="${esc(g.id)}">
        <div class="mobile-plan-card-header" data-plan-toggle="${esc(g.id)}">
          <div class="mobile-plan-title-row">
            <div class="mobile-plan-icon-wrap">${statusIcon}</div>
            <span class="mobile-plan-title-text">${esc(g.title)}</span>
            <span class="mobile-plan-count-badge">${g.steps.length} 步</span>
          </div>
          <div class="mobile-plan-meta-row">
            <span class="mobile-plan-toggle-arrow ${isOpen ? 'is-up' : ''}">▾</span>
          </div>
        </div>
        <div class="mobile-plan-tree-body ${isOpen ? '' : 'hidden'}">
          <div class="mobile-tree-spine-track"></div>
          <div class="mobile-tree-nodes-list">
            ${subStepsHtml}
          </div>
        </div>
      </div>
    `;
  }).join("");
}

/* 兼容单步渲染 */
function renderStep(st) {
  return renderTreeSubStep(st, 0, true);
}

/* ---------- 结构化人类协助与提问交互 (Human Intervention) ---------- */
function normalizeHumanForm(raw, fallbackTitle) {
  const data = (raw && typeof raw === "object") ? raw : {};
  const rawKind = String(data.kind || "").toLowerCase().trim();
  const aliases = {
    single: "single_select", select: "single_select", single_select: "single_select", radio: "single_select",
    multi: "multi_select", multiple: "multi_select", multi_select: "multi_select", checkbox: "multi_select",
    confirm: "confirm", confirmation: "confirm", text: "text", input: "text",
    file: "file_picker", file_picker: "file_picker", upload: "file_picker", files: "file_picker",
    custom_html: "custom_html", html: "custom_html", custom: "custom_html",
    plan_approval: "plan_approval", plan: "plan_approval", approval: "plan_approval",
  };
  const kind = aliases[rawKind] || (data.options && data.options.length ? "single_select" : "text");
  const options = (Array.isArray(data.options) ? data.options : []).slice(0, 12).map((item, index) => {
    const opt = (item && typeof item === "object") ? item : { label: String(item) };
    const id = String(opt.id || opt.value || index + 1).slice(0, 80);
    const label = String(opt.label || opt.title || opt.name || id).slice(0, 160);
    const description = opt.description || opt.detail ? String(opt.description || opt.detail).slice(0, 300) : "";
    return description ? { id, label, description } : { id, label };
  }).filter(item => item.id && item.label);

  return {
    kind,
    title: String(data.title || data.label || fallbackTitle || "请提供反馈或确认下一步"),
    description: String(data.description || data.help || ""),
    placeholder: String(data.placeholder || ""),
    options,
    defaultValue: data.default ?? data.default_value ?? data.value,
  };
}

function renderAskCard(run) {
  if (!run || run.status !== "waiting_human") {
    return "";
  }
  const rawForm = run.paused_form;
  const reason = run.paused_reason || "Agent 请求人工协助。";
  const form = normalizeHumanForm(rawForm, reason);

  // 初始化该轮任务的交互表单状态
  if (S.humanForm.runId !== run.run_id) {
    S.humanForm.runId = run.run_id;
    S.humanForm.confirmValue = typeof form.defaultValue === "boolean" ? form.defaultValue : true;
    S.humanForm.singleSelectedId = typeof form.defaultValue === "string"
      ? form.defaultValue
      : (form.options && form.options[0]?.id ? form.options[0].id : "");
    S.humanForm.multiSelectedIds = Array.isArray(form.defaultValue) ? [...form.defaultValue] : [];
    S.humanForm.writeInText = (form.kind === "text" && typeof form.defaultValue === "string") ? form.defaultValue : "";
    S.humanForm.isSubmitting = false;
  }

  const kindLabels = {
    plan_approval: "📋 方案审查与审批",
    confirm: "操作确认",
    single_select: "单项选择",
    multi_select: "多项选择",
    file_picker: "文件上传",
    custom_html: "自定义交互",
    text: "问答输入",
  };
  const formTypeLabel = kindLabels[form.kind] || "问答输入";

  let bodyHtml = "";
  if (form.kind === "plan_approval") {
    bodyHtml = `
      <div class="human-plan-approval-box">
        <div class="human-plan-banner">
          <span class="banner-icon">🛡️</span>
          <span>前置技术方案已就绪，请审阅后确认是否批准放行写入。</span>
        </div>
        ${form.description ? `<div class="human-plan-desc md-content">${formatMarkdown(form.description)}</div>` : ''}
        <div class="human-confirm-box" style="margin-top: 12px;">
          <button type="button" class="human-choice-btn confirm-btn active" onclick="submitMobilePlanApproval(true)">
            <span>✅</span><span>批准方案并开始执行</span>
          </button>
          <button type="button" class="human-choice-btn cancel-btn" onclick="togglePlanRevisionInput()">
            <span>🔄</span><span>提出修改 / 打回重推</span>
          </button>
        </div>
        <div id="planRevisionWrap" class="plan-revision-wrap" style="display: none; margin-top: 10px;">
          <textarea id="planRevisionInput" class="human-textarea" placeholder="请输入修改意见，提交后将打回 AI 重新推演方案…" rows="3"></textarea>
          <div style="margin-top: 8px; text-align: right;">
            <button type="button" class="human-submit-btn" style="width: auto; padding: 6px 14px; font-size: 12.5px;" onclick="submitMobilePlanApproval(false)">
              <span>确认打回重推</span>
            </button>
          </div>
        </div>
      </div>
    `;
  } else if (form.kind === "confirm") {
    bodyHtml = `
      <div class="human-confirm-box">
        <button type="button" class="human-choice-btn confirm-btn ${S.humanForm.confirmValue === true ? "active" : ""}" onclick="selectHumanConfirm(true)">
          <span>✓</span><span>确认执行</span>
        </button>
        <button type="button" class="human-choice-btn cancel-btn ${S.humanForm.confirmValue === false ? "active" : ""}" onclick="selectHumanConfirm(false)">
          <span>✕</span><span>取消并终止</span>
        </button>
      </div>
    `;
  } else if (form.kind === "single_select") {
    bodyHtml = `
      <div class="human-options-container">
        ${form.options.map(opt => `
          <div class="human-option-item ${S.humanForm.singleSelectedId === opt.id ? "selected" : ""}" onclick="selectHumanSingle('${esc(opt.id)}')">
            <div class="human-radio-circle">
              ${S.humanForm.singleSelectedId === opt.id ? '<div class="human-radio-inner"></div>' : ''}
            </div>
            <div class="human-option-text">
              <span class="human-option-label">${esc(opt.label)}</span>
              ${opt.description ? `<small class="human-option-sub">${esc(opt.description)}</small>` : ''}
            </div>
          </div>
        `).join("")}
      </div>
    `;
  } else if (form.kind === "multi_select") {
    bodyHtml = `
      <div class="human-multi-toolbar">
        <span>已选 ${S.humanForm.multiSelectedIds.length}/${form.options.length} 项</span>
        <div class="human-toolbar-actions">
          <button type="button" class="mini-btn" onclick="selectHumanMultiAll(true)">全选</button>
          <button type="button" class="mini-btn" onclick="selectHumanMultiAll(false)">清空</button>
        </div>
      </div>
      <div class="human-options-container">
        ${form.options.map(opt => {
          const checked = S.humanForm.multiSelectedIds.includes(opt.id);
          return `
            <div class="human-option-item ${checked ? "selected" : ""}" onclick="toggleHumanMulti('${esc(opt.id)}')">
              <div class="human-checkbox-box ${checked ? "checked" : ""}">
                ${checked ? '<svg viewBox="0 0 24 24"><polyline points="20 6 9 17 4 12"/></svg>' : ''}
              </div>
              <div class="human-option-text">
                <span class="human-option-label">${esc(opt.label)}</span>
                ${opt.description ? `<small class="human-option-sub">${esc(opt.description)}</small>` : ''}
              </div>
            </div>
          `;
        }).join("")}
      </div>
    `;
  } else if (form.kind === "text") {
    bodyHtml = `
      <div class="human-text-wrap">
        <textarea class="human-textarea" id="humanTextInput" placeholder="${esc(form.placeholder || '请输入你的回复或指示…')}" rows="3" oninput="S.humanForm.writeInText=this.value">${esc(S.humanForm.writeInText)}</textarea>
      </div>
    `;
  }

  // 非纯文本问答或方案审批时展示可选补充输入框
  let writeInHtml = "";
  if (form.kind !== "text" && form.kind !== "custom_html" && form.kind !== "plan_approval") {
    writeInHtml = `
      <div class="human-write-in-wrap">
        <label class="human-write-in-label" for="humanWriteInInput">补充说明 / 自定义要求（可选）:</label>
        <input id="humanWriteInInput" class="human-write-in-input" type="text" placeholder="如果以上选项不符合，可在此输入补充意见后提交…" value="${esc(S.humanForm.writeInText)}" oninput="S.humanForm.writeInText=this.value" onkeydown="if(event.key==='Enter')submitHumanIntervention()">
      </div>
    `;
  }

  const isSubmitting = S.humanForm.isSubmitting;
  const actionsBarHtml = form.kind === "plan_approval" ? "" : `
      <div class="human-actions-bar">
        <button type="button" class="human-submit-btn" id="btnSubmitHuman" onclick="submitHumanIntervention()" ${isSubmitting ? "disabled" : ""}>
          <span>${isSubmitting ? "提交中…" : "提交反馈"}</span>
          <svg viewBox="0 0 24 24"><path d="M5 12h14M12 5l7 7-7 7"/></svg>
        </button>
      </div>
  `;

  return `
    <section class="human-intervention-card" aria-label="需要用户协助与确认">
      <div class="human-top-row">
        <div class="human-badge">
          <span class="pulse-dot"></span>
          <span>${form.kind === "plan_approval" ? "📋 方案待审批" : "需要人类协助"}</span>
        </div>
        <span class="human-type-label">${esc(formTypeLabel)}</span>
      </div>
      <h3 class="human-title">${formatMarkdown(form.title)}</h3>
      ${form.kind !== "plan_approval" && form.description ? `<div class="human-desc">${formatMarkdown(form.description)}</div>` : ""}
      ${bodyHtml}
      ${writeInHtml}
      ${actionsBarHtml}
    </section>
  `;
}

window.togglePlanRevisionInput = function() {
  const el = $("planRevisionWrap");
  if (!el) return;
  el.style.display = el.style.display === "none" ? "block" : "none";
};

window.submitMobilePlanApproval = async function(approved) {
  if (!S.flow || !S.flow.run || S.humanForm.isSubmitting) return;
  const f = S.flow;
  let text = "";
  if (approved) {
    text = "【方案已批准】批准方案，请开始执行后续写入与测试步骤。\n<!--plan-decision:{\"approved\":true}-->";
  } else {
    const revEl = $("planRevisionInput");
    const note = revEl ? revEl.value.trim() : "";
    text = `【方案打回修改】请重新审视并按以下意见修改方案后再提交审查：\n${note || "用户打回要求重新推演方案"}\n<!--plan-decision:{\"approved\":false}-->`;
  }

  S.humanForm.isSubmitting = true;
  updateAskCardDom();
  try {
    await api(`/channel/run/${f.run.run_id}/answer`, {
      method: "POST",
      body: JSON.stringify({ text }),
    });
    toast(approved ? "方案已批准，Agent 开始执行" : "方案已打回，Agent 重新推演");
    S.humanForm.runId = "";
    S.humanForm.writeInText = "";
    await loadFlow(f.sessionId);
  } catch (e) {
    toast(`提交失败: ${e.message}`);
    S.humanForm.isSubmitting = false;
    updateAskCardDom();
  }
};

window.selectHumanConfirm = function(val) {
  S.humanForm.confirmValue = val;
  updateAskCardDom();
};

window.selectHumanSingle = function(id) {
  S.humanForm.singleSelectedId = id;
  updateAskCardDom();
};

window.toggleHumanMulti = function(id) {
  const idx = S.humanForm.multiSelectedIds.indexOf(id);
  if (idx >= 0) S.humanForm.multiSelectedIds.splice(idx, 1);
  else S.humanForm.multiSelectedIds.push(id);
  updateAskCardDom();
};

window.selectHumanMultiAll = function(selectAll) {
  if (!S.flow || !S.flow.run) return;
  const form = normalizeHumanForm(S.flow.run.paused_form, "");
  S.humanForm.multiSelectedIds = selectAll ? form.options.map(o => o.id) : [];
  updateAskCardDom();
};

window.submitHumanIntervention = async function() {
  if (!S.flow || !S.flow.run || S.humanForm.isSubmitting) return;
  const f = S.flow;
  const form = normalizeHumanForm(f.run.paused_form, f.run.paused_reason || "");
  const parts = [];

  if (form.kind === "text") {
    const textInput = $("humanTextInput");
    if (textInput) S.humanForm.writeInText = textInput.value;
    const txt = S.humanForm.writeInText.trim();
    if (!txt) {
      toast("请输入回复内容");
      return;
    }
    parts.push(txt);
  } else {
    if (form.kind === "confirm") {
      parts.push(`【确认状态】: ${S.humanForm.confirmValue ? "确认执行" : "取消操作"}`);
    } else if (form.kind === "single_select") {
      const matched = form.options.find(o => o.id === S.humanForm.singleSelectedId);
      parts.push(`【选择项】: ${matched ? matched.label : S.humanForm.singleSelectedId}`);
    } else if (form.kind === "multi_select") {
      const labels = S.humanForm.multiSelectedIds.map(id => {
        const opt = form.options.find(o => o.id === id);
        return opt ? opt.label : id;
      });
      if (!labels.length && !S.humanForm.writeInText.trim()) {
        toast("请至少选择一项或输入补充说明");
        return;
      }
      if (labels.length) parts.push(`【已选项】: ${labels.join(", ")}`);
    }
    const writeInput = $("humanWriteInInput");
    if (writeInput) S.humanForm.writeInText = writeInput.value;
    if (S.humanForm.writeInText.trim()) {
      parts.push(`【补充说明】: ${S.humanForm.writeInText.trim()}`);
    }
  }

  const answerText = parts.join("\n");
  if (!answerText.trim()) return;

  S.humanForm.isSubmitting = true;
  updateAskCardDom();
  try {
    await api(`/channel/run/${f.run.run_id}/answer`, {
      method: "POST",
      body: JSON.stringify({ text: answerText }),
    });
    toast("已提交反馈，Agent 继续执行");
    S.humanForm.runId = "";
    S.humanForm.writeInText = "";
    await loadFlow(f.sessionId);
  } catch (e) {
    toast("提交失败: " + e.message);
  } finally {
    S.humanForm.isSubmitting = false;
    updateAskCardDom();
  }
};

function updateAskCardDom() {
  const askCard = $("askCard");
  if (!askCard || !S.flow) return;
  const html = renderAskCard(S.flow.run);
  askCard.innerHTML = html;
  askCard.dataset.renderedHtml = html;
}

// ---------------------------------------------------------------- 数据加载与多轮会话流渲染
const traceDetailCache = new Map();
let _loadFlowInFlight = false;
let _loadFlowPendingNext = false;

async function fetchTraceDetail(t) {
  const stamp = `${t.id}:${t.step_count || 0}:${t.updated_at || ""}`;
  if (traceDetailCache.has(t.id) && traceDetailCache.get(t.id).stamp === stamp) {
    return traceDetailCache.get(t.id).trace;
  }
  try {
    const detail = await api(`/channel/trace/${encodeURIComponent(t.id)}`);
    if (detail && detail.trace) {
      traceDetailCache.set(t.id, { stamp, trace: detail.trace });
      return detail.trace;
    }
  } catch (e) {}
  return t;
}

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
  if (S.view.name === "home") {
    renderHome();
  }
  renderSidebar();
}

async function loadFlow(sessionId) {
  if (_loadFlowInFlight) {
    _loadFlowPendingNext = true;
    return;
  }
  _loadFlowInFlight = true;
  try {
    const promises = [
      api("/channel/runs"),
      api(`/channel/session/${encodeURIComponent(sessionId)}/traces`),
      api(`/channel/session/${encodeURIComponent(sessionId)}/artifacts`),
    ];
    const needSessions = !S.sessions.length || !S.sessions.some(s => s.id === sessionId);
    if (needSessions) {
      promises.push(api("/channel/sessions"));
    }
    const [runsRes, tracesRes, artsRes, sessRes] = await Promise.all(promises);
    if (sessRes && sessRes.sessions) {
      S.sessions = sessRes.sessions;
    }
    const session = S.sessions.find(s => s.id === sessionId) || null;
    const run = (runsRes.runs || []).filter(r => r.session_id === sessionId)
      .sort((a, b) => (b.started_at || "").localeCompare(a.started_at || ""))[0] || null;
    const traceList = tracesRes.traces || [];
    
    // 并行获取该会话所有历史轮次完整步骤（已完成历史轮次直接命中内存缓存，0 网络请求）
    const fullTraces = await Promise.all(traceList.map(t => fetchTraceDetail(t)));

    // 严格按 ID 与 Goal 去重，彻底杜绝单条消息渲染两次的问题
    const seenIds = new Set();
    const seenGoals = new Set();
    const uniqueTraces = [];
    for (const tr of fullTraces) {
      if (tr && tr.id && !seenIds.has(tr.id)) {
        seenIds.add(tr.id);
        if (tr.goal) seenGoals.add(tr.goal.trim());
        uniqueTraces.push(tr);
      }
    }

    // 仅当没有任何一条已入库 trace 与当前 run 匹配时，才补充临时执行中占位
    if (run && ["running", "waiting_human"].includes(run.status)) {
      const alreadyPresent = uniqueTraces.some(t =>
        t.id === run.run_id ||
        (t.goal && run.goal_preview && (t.goal.trim() === run.goal_preview.trim() || seenGoals.has(run.goal_preview.trim())))
      );
      if (!alreadyPresent && run.goal_preview) {
        uniqueTraces.push({
          id: run.run_id,
          goal: run.goal_preview,
          status: run.status,
          steps: [],
          started_at: run.started_at,
        });
      }
    }

    S.flow = {
      sessionId,
      session,
      run,
      traces: uniqueTraces,
      artifacts: artsRes.artifacts || [],
    };
    renderFlow();
  } finally {
    _loadFlowInFlight = false;
    if (_loadFlowPendingNext) {
      _loadFlowPendingNext = false;
      loadFlow(sessionId).catch(() => {});
    }
  }
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
  // 保持会话标题稳定，副标题展示最新状态与时间
  const title = esc(s.title || s.goal || "未命名会话");
  const time = esc(relTime(s.trace_updated_at || s.updated_at));
  const isRunning = s.status === "running";
  return `
    <div class="task-row-card ${isRunning ? "running-card" : ""}" data-session="${esc(s.id)}">
      <div class="tt">
        <div class="title">${title}</div>
        <div class="sub-line">${isRunning ? '<span style="color:#38bdf8; font-weight:600;">● 工作中…</span> · ' : ''}${time} · ${esc(modelName(s.model_id))}</div>
      </div>
      ${chip(s.status)}
      <button class="task-del-btn" data-del-session="${esc(s.id)}" title="删除此会话">🗑</button>
    </div>
  `;
}

function renderHome() {
  if (S.view.name !== "home") return;
  $("topTitle").textContent = "free_api 远程控制";
  $("topSub").innerHTML = `<span id="connDot" class="dot ok"></span><span id="connText">已连接到当前桌面窗口</span>`;
  $("btnBackTop").classList.add("hidden");
  $("btnDelSession").classList.add("hidden");
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
    const items = g.sessions.slice(0, 15).map(s => `
      <div class="side-task ${S.currentSessionId === s.id ? "active" : ""}" data-session="${esc(s.id)}">
        <div class="side-task-info">
          <div class="t">${esc(short(s.title || s.goal, 20))}</div>
          <div class="time">${esc(relTime(s.trace_updated_at || s.updated_at))}</div>
        </div>
        <button class="side-del-btn" data-del-session="${esc(s.id)}" title="删除此会话">✕</button>
      </div>
    `).join("");
    return `
      <div class="side-group ${open ? "open" : ""}" data-sgroup="${esc(g.key)}">
        <div class="side-head" data-toggle="${esc(g.key)}" role="button">
          <span>${g.icon}</span><span class="name">${esc(g.name)}</span>
          <button class="side-plus-btn" data-new="${esc(g.key)}" title="在此项目下新建对话">＋</button>
          <span class="count">${g.sessions.length}</span><span class="chev">⌄</span>
        </div>
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
  const proj = s.project_id ? S.projects.find(p => p.id === s.project_id) : null;
  const dirLabel = proj ? `📁 ${proj.name}` : `💬 临时工作区`;
  const fullDirPath = proj ? (proj.root_path || "") : "scratch (临时目录)";

  $("topTitle").textContent = s.title || "任务会话";
  $("topSub").innerHTML = `<span id="connDot" class="dot ok"></span><span style="color:var(--dim); font-size:11px; max-width:230px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; display:inline-block; vertical-align:middle;">[${esc(dirLabel)}] ${esc(fullDirPath)}</span>`;
  $("btnBackTop").classList.remove("hidden");
  $("btnDelSession").classList.remove("hidden");
  $("btnMenu").classList.add("hidden");

  // 完整渲染会话中的每一轮历史对话（按时间顺序自上而下完整排列）
  let html = "";
  if (!f.traces.length) {
    html += `<div class="empty-tip">当前对话所在目录：<b>${esc(dirLabel)}</b><br><span style="font-size:11px; color:var(--dim);">${esc(fullDirPath)}</span><br><br>直接在下方输入内容即可开始</div>`;
  } else {
    html = f.traces.map((tr, idx) => {
      const isLatest = idx === f.traces.length - 1;
      const trGoal = tr.goal ? `<div class="goal-bubble"><span class="arrow">→</span><div class="gtxt">${formatMarkdown(tr.goal)}</div></div>` : "";
      const trSteps = renderStepsTree(tr.steps || []);
      
      const trFailed = tr.status === "failed";
      const trResult = (tr.completion && tr.completion.summary)
        ? tr.completion.summary
        : (isLatest && trFailed && f.run && f.run.error ? f.run.error : null);
      const resultHtml = (!active || !isLatest) && trResult
        ? `<div class="result-card ${trFailed ? "fail" : ""}"><div class="rlabel">${trFailed ? "✕ 失败原因" : "✓ 执行结果"}</div>${formatMarkdown(trResult)}</div>`
        : "";

      return `
        <div class="turn-section" style="margin-bottom: 12px;">
          ${trGoal}
          ${trSteps}
          ${resultHtml}
        </div>
      `;
    }).join("");
  }

  // 仅在 html 内容真正变动时才重写 DOM，彻底杜绝轮询导致的整体重绘闪烁
  const timelineEl = $("timeline");
  if (timelineEl.dataset.renderedHtml !== html) {
    const currentScroll = window.scrollY || document.documentElement.scrollTop;
    timelineEl.innerHTML = html;
    timelineEl.dataset.renderedHtml = html;
    window.scrollTo(0, currentScroll);
  }

  // 实时状态行（带 DOM 差异防闪烁）
  const live = $("liveLine");
  let liveHtml = "";
  let liveClass = "live-line";
  if (f.run && f.run.status === "running") {
    liveClass = "live-line";
    liveHtml = `<span class="livedot"></span><strong>工作中</strong> <span style="color:var(--dim)">· ${esc(dirLabel)} · ${esc(f.run.state_detail || f.run.phase || "执行中")}${f.run.event_count ? ` · ${f.run.event_count} 事件` : ""}</span>`;
  } else if (f.run && f.run.status === "waiting_human") {
    liveClass = "live-line wait";
    liveHtml = `<span class="livedot"></span><span style="color:var(--amber); font-weight:600">等待你的回答 · ${esc(dirLabel)}</span>`;
  } else {
    liveClass = "live-line";
    liveHtml = `<span style="color:var(--dim)">${esc(dirLabel)} · ${esc(modelName(s.model_id))}${f.traces.length > 0 ? ` · 共 ${f.traces.length} 轮对话` : ""}</span>`;
  }
  if (live.dataset.renderedHtml !== liveHtml || live.className !== liveClass) {
    live.className = liveClass;
    live.innerHTML = liveHtml;
    live.dataset.renderedHtml = liveHtml;
  }

  // 提问卡片（等待人类输入 / 人工协助表单）
  const askCard = $("askCard");
  const askHtml = renderAskCard(f.run);
  const activeEl = document.activeElement;
  const isTypingInAsk = activeEl && (activeEl.id === "humanWriteInInput" || activeEl.id === "humanTextInput");
  if (!isTypingInAsk && askCard.dataset.renderedHtml !== askHtml) {
    askCard.innerHTML = askHtml;
    askCard.dataset.renderedHtml = askHtml;
  }

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

  // Composer 状态与占位符
  const input = $("followInput");
  const composer = $("composer");
  const isWaitingHuman = f.run && f.run.status === "waiting_human";
  composer.classList.toggle("waiting-human", !!isWaitingHuman);
  $("btnStop").classList.toggle("hidden", !(f.run && f.run.status === "running"));
  if (isWaitingHuman) {
    input.placeholder = "回复 Agent 的提问…";
  } else if (f.run && f.run.status === "running") {
    input.placeholder = "继续输入以排队后续修改 / 接管方向…";
  } else {
    input.placeholder = "在当前会话继续对话…";
  }
}

async function openSession(sessionId) {
  closeSidebarMobile();
  S.currentSessionId = sessionId;
  renderSidebar();
  S.view = { name: "flow" };
  $("viewHome").classList.add("hidden");
  $("viewFlow").classList.remove("hidden");
  $("btnDelSession").classList.remove("hidden");
  $("timeline").innerHTML = `<div class="empty-tip">加载中…</div>`;
  try { await loadFlow(sessionId); } catch (e) { toast("加载失败：" + e.message); }
}

function goHome() {
  closeSidebarMobile();
  S.currentSessionId = null;
  S.view = { name: "home" };
  $("viewFlow").classList.add("hidden");
  $("viewHome").classList.remove("hidden");
  $("btnDelSession").classList.add("hidden");
  renderHome(); renderSidebar();
  loadHome().catch(() => {});
}

async function deleteSession(sessionId) {
  if (!confirm("确定要删除此对话吗？删除后将无法恢复。")) return;
  try {
    await api(`/channel/session/${encodeURIComponent(sessionId)}`, { method: "DELETE" });
    toast("已删除对话");
    if (S.currentSessionId === sessionId) {
      goHome();
    } else {
      await loadHome();
    }
  } catch (e) {
    toast("删除失败: " + e.message);
  }
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
      toast("已发送");
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
  S.sheetOpen = true;
  $("sheetTitle").textContent = title;
  const searchWrap = $("sheetSearchWrap");
  const searchInput = $("sheetSearchInput");
  searchWrap.style.display = allItems.length > 5 ? "" : "none";
  searchInput.value = "";

  function makeRow(it) {
    return `
      <button class="sheet-model-row ${it.value === currentValue ? "selected" : ""}" data-v="${esc(it.value)}">
        <div style="flex:1; min-width:0;">
          <div class="m-title">${esc(it.label)}</div>
          ${it.sub ? `<div class="m-sub">${esc(it.sub)}</div>` : ""}
        </div>
        ${it.value === currentValue ? '<span style="color:var(--accent2); font-weight:bold;">✓</span>' : ""}
      </button>
    `;
  }

  function renderList(query) {
    const q = (query || "").toLowerCase().trim();
    let filtered = allItems.filter(it => {
      if (!q) return true;
      return it.label.toLowerCase().includes(q)
        || (it.sub && it.sub.toLowerCase().includes(q))
        || (it.value && it.value.toLowerCase().includes(q));
    });

    // 移动端首次未搜索时只显示前 30 条，防止 100+ 行同时渲染卡顿
    const cap = !q && filtered.length > 30;
    const visible = cap ? filtered.slice(0, 30) : filtered;

    const list = $("sheetList");
    list.innerHTML = visible.length
      ? visible.map(makeRow).join("")
        + (cap ? `<div class="empty-tip" style="cursor:pointer;color:var(--accent2);" id="sheetLoadMore">显示全部 ${filtered.length} 项（点击或搜索）</div>` : "")
      : `<div class="empty-tip">未找到匹配项</div>`;

    list.querySelectorAll("button[data-v]").forEach(btn => {
      btn.onclick = () => {
        S.sheetOpen = false;
        onPick(btn.dataset.v);
        $("sheetPick").classList.add("hidden");
      };
    });

    const more = $("sheetLoadMore");
    if (more) more.onclick = () => {
      list.innerHTML = filtered.map(makeRow).join("");
      list.querySelectorAll("button[data-v]").forEach(btn => {
        btn.onclick = () => {
          S.sheetOpen = false;
          onPick(btn.dataset.v);
          $("sheetPick").classList.add("hidden");
        };
      });
    };
  }

  // 输入时 debounce 300ms 避免每键一次重渲
  let _debTimer;
  searchInput.oninput = (e) => {
    clearTimeout(_debTimer);
    _debTimer = setTimeout(() => renderList(e.target.value), 300);
  };
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

  $("sheetClose").onclick = () => { S.sheetOpen = false; $("sheetPick").classList.add("hidden"); };
  $("sheetPick").addEventListener("click", (e) => { if (e.target === $("sheetPick")) { S.sheetOpen = false; $("sheetPick").classList.add("hidden"); } });
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
  
  $("lblNewModel").textContent = S.selectedNewModelId ? modelName(S.selectedNewModelId) : "请选择模型…";
  
  $("newGoal").value = "";
  $("modalNew").classList.remove("hidden");
}

async function submitNew() {
  const projectId = newTaskType === "project" ? $("newProj").value : null;
  const modelId = S.selectedNewModelId || "";
  if (!modelId) {
    toast("请先选择执行模型");
    return;
  }
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
    const routing = { pro: modelId, flash: modelId, vision: modelId };
    await api("/channel/task", {
      method: "POST",
      body: JSON.stringify({ session_id: sess.session_id, goal, model_routing: routing }),
    });
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

// 直接在指定项目/目录创建新对话并立即跳转进入聊天页面
async function quickCreateSession(projectId) {
  closeSidebarMobile();
  const pid = (projectId && projectId !== "__scratch__") ? projectId : null;
  const proj = pid ? S.projects.find(p => p.id === pid) : null;
  const defaultTitle = proj ? `${proj.name} 对话` : "新任务对话";
  try {
    const res = await api("/channel/session", {
      method: "POST",
      body: JSON.stringify({
        project_id: pid,
        title: defaultTitle,
      }),
    });
    await loadHome();
    await openSession(res.session_id);
    toast(`已进入 ${proj ? proj.name : '临时工作区'}，直接在下方输入即可`);
    setTimeout(() => {
      const input = $("followInput");
      if (input) {
        input.focus();
        input.scrollIntoView({ behavior: "smooth" });
      }
    }, 200);
  } catch (e) {
    toast("创建失败: " + e.message);
  }
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
  $("btnNew").onclick = () => quickCreateSession("__scratch__");
  $("btnDelSession").onclick = () => {
    if (S.currentSessionId) deleteSession(S.currentSessionId);
  };
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
        $("lblNewModel").textContent = v ? modelName(v) : "请选择模型…";
      }
    );
  };

  $("typeScratch").onclick = () => { newTaskType = "scratch"; $("projField").style.display = "none"; $("typeScratch").style.background = "var(--accent)"; $("typeProject").style.background = "transparent"; };
  $("typeProject").onclick = () => { newTaskType = "project"; $("projField").style.display = ""; $("typeProject").style.background = "var(--accent)"; $("typeScratch").style.background = "transparent"; };
  $("newCancel").onclick = () => $("modalNew").classList.add("hidden");
  $("modalNew").onclick = (e) => { if (e.target === $("modalNew")) $("modalNew").classList.add("hidden"); };
  $("newSubmit").onclick = submitNew;

  // 快捷回到底部
  window.addEventListener("scroll", () => {
    const show = window.scrollY < (document.documentElement.scrollHeight - window.innerHeight - 250);
    $("btnScrollBottom").classList.toggle("hidden", !show || S.view.name !== "flow");
  });
  $("btnScrollBottom").onclick = () => { window.scrollTo({ top: document.documentElement.scrollHeight, behavior: "smooth" }); };

  // 全局事件代理
  document.addEventListener("click", (e) => {
    const del = e.target.closest("[data-del-session]");
    if (del) {
      e.stopPropagation();
      e.preventDefault();
      deleteSession(del.dataset.delSession);
      return;
    }
    const planToggle = e.target.closest("[data-plan-toggle]");
    if (planToggle) {
      const id = planToggle.dataset.planToggle;
      const groupEl = planToggle.closest(".mobile-plan-tree-group");
      const isCurrentlyOpen = groupEl ? groupEl.classList.contains("is-expanded") : false;
      if (isCurrentlyOpen) {
        S.collapsedPlans.add(id);
        S.expandedPlans.delete(id);
      } else {
        S.expandedPlans.add(id);
        S.collapsedPlans.delete(id);
      }
      renderFlow();
      return;
    }
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
    if (plus) { e.stopPropagation(); quickCreateSession(plus.dataset.new); return; }
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
    const [mData, rData] = await Promise.all([
      api("/channel/models"),
      api("/channel/role-cards").catch(() => ({ cards: [] })),
    ]);
    S.models = (mData.models || []).filter(m => m.enabled !== 0 && m.enabled !== false);
    S.modelById = Object.fromEntries(S.models.map(m => [m.id, m]));
    S.roleCards = rData.cards || [];
  } catch (e) {}
}

function isAgentActive() {
  const f = S.flow;
  return f && f.run && ["running", "waiting_human"].includes(f.run.status);
}

function sseAlive() {
  return !!(S.es && S.es.readyState === 1);
}

function connectSSE(force) {
  if (!window.EventSource) return;
  if (S.es && S.es.readyState <= 1 && !force) return;
  if (S.es) { try { S.es.close(); } catch (e) {} S.es = null; }
  try {
    const es = new EventSource(`/channel/events?token=${encodeURIComponent(S.token)}`);
    S.es = es;
    es.onopen = () => { setConn(true); };
    es.onmessage = (ev) => {
      // SSE 实时推送驱动：桌面端 / 微信 / Agent 产生步骤、新任务或完成时立即刷新
      try {
        const payload = JSON.parse(ev.data || "{}");
        const eventType = (payload.event && payload.event.type) || "";
        const sid = payload.session_id;
        if (S.view.name === "flow" && S.currentSessionId) {
          if (!sid || sid === S.currentSessionId) {
            loadFlow(S.currentSessionId).catch(() => {});
          }
          if (["start", "done", "failed", "cancelled", "deleted"].includes(eventType)) {
            loadHome().catch(() => {});
          }
        } else {
          loadHome().catch(() => {});
        }
      } catch (e) {
        if (S.view.name === "flow" && S.currentSessionId) {
          loadFlow(S.currentSessionId).catch(() => {});
        } else {
          loadHome().catch(() => {});
        }
      }
    };
    es.onerror = () => {
      if (es.readyState === 2) {
        S.es = null;
        setTimeout(() => connectSSE(true), 3000);
      }
    };
  } catch (e) {}
}

async function boot() {
  applyTheme(S.theme);
  bind();
  await loadModels().catch(() => {});
  await loadHome().catch(() => {});
  connectSSE();

  // 自适应智能轮询：
  // - SSE 在线 → 全部交由 SSE 实时推送，仅每 10s 兜底心跳一次
  // - SSE 断线 + Agent 运行中 → 每 2s 短轮询
  // - SSE 断线 + Agent 空闲 → 每 6s 慢轮询
  setInterval(() => {
    if (document.hidden) return;
    // ⚠️ 弹层/弹窗打开时跳过轮询重渲，防止 DOM 重建破坏弹层交互或卡顿
    if (S.sheetOpen) return;
    if ($('modalNew') && !$('modalNew').classList.contains('hidden')) return;
    const active = isAgentActive();
    const alive = sseAlive();
    const now = Date.now();

    if (alive) {
      // SSE 存活时，绝大部分状态靠实时推送更新，轮询只做 10s 兜底
      if (!S._lastPoll || now - S._lastPoll >= 10000) {
        S._lastPoll = now;
        if (S.view.name === "home") loadHome().catch(() => {});
        else if (S.view.name === "flow" && S.currentSessionId) loadFlow(S.currentSessionId).catch(() => {});
      }
      return;
    }

    // SSE 断线时，才启用短轮询
    const interval = active ? 2000 : 6000;
    if (!S._lastPoll || now - S._lastPoll >= interval) {
      S._lastPoll = now;
      if (S.view.name === "home") {
        loadHome().catch(() => {});
      } else if (S.view.name === "flow" && S.currentSessionId) {
        loadFlow(S.currentSessionId).catch(() => {});
      }
    }
  }, 2000);
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
