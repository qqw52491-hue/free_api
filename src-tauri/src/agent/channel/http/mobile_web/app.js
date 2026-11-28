"use strict";

const S = {
  token: localStorage.getItem("free_api_web_token") || "",
  theme: localStorage.getItem("free_api_web_theme") || "default",
  models: [], modelById: {},
  roleCards: [],
  projects: [],
  expandedTurns: new Map(),
  sessions: [],
  runs: [],
  evals: [],
  view: { name: "home" },
  openGroups: new Set(JSON.parse(localStorage.getItem("free_api_open_groups") || "[]")),
  currentSessionId: null,
  flow: null,
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
  const markup = [];
  // Keep code and link attributes out of the text-formatting passes.
  const plain = String(text).replace(/\0/g, "\uFFFD").replace(
    /`([^`]+)`|\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g,
    (_, code, label, url) => {
      const fragment = code === undefined
        ? `<a class="md-link" href="${esc(url)}" target="_blank" rel="noopener">${formatInline(label)} ↗</a>`
        : `<code class="inline-code">${esc(code)}</code>`;
      return "\0" + (markup.push(fragment) - 1) + "\0";
    },
  );
  let out = esc(plain);
  
  // 2. 加粗 **bold**
  out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  
  // 3. 斜体 *italic*
  out = out.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
  
  // 4. 删除线 ~~del~~
  out = out.replace(/~~([^~]+)~~/g, '<del class="md-del">$1</del>');
  
  // 6. 文件路径与代码文件自动高亮胶囊
  out = out.replace(/\b([a-zA-Z0-9_\-\.\/]+\.(?:md|ts|js|rs|json|vue|html|css|py|yaml|yml|toml|sql|sh))\b/g, '<code class="inline-code">$1</code>');
  
  // 7. 序号加粗美化
  out = out.replace(/([①②③④⑤⑥⑦⑧⑨⑩])/g, '<strong style="color:var(--accent2); margin-right:2px;">$1</strong>');
  
  return out.replace(/\0(\d+)\0/g, (_, index) => markup[Number(index)]);
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
  const dotClass = "dot " + (ok ? "ok" : "bad");
  if (dot && dot.className !== dotClass) dot.className = dotClass;
  const txt = $("connText");
  const label = msg || (ok ? "已连接到当前桌面窗口" : "未连接");
  if (txt) {
    if (txt.textContent !== label) txt.textContent = label;
    txt.classList.toggle("is-error", !ok);
  }
  $("topSub").classList.toggle("has-error", !ok);
}

function renderHeaderSubtitle(label = "") {
  const element = $("topLocation");
  if (element.textContent !== label) {
    element.textContent = label;
    element.title = label;
  }
  element.classList.toggle("hidden", !label);
  $("topSub").classList.toggle("has-location", !!label);
}

function closeSidebarMobile() {
  const sb = $("sidebar");
  const sm = $("sideMask");
  if (sb) sb.classList.remove("show");
  if (sm) sm.classList.remove("show");
}

async function api(path, opts = {}) {
  const token = S.token;
  const headers = Object.assign({ "X-Channel-Token": token }, opts.headers || {});
  if (opts.body && typeof opts.body === "string" && !headers["Content-Type"]) {
    headers["Content-Type"] = "application/json";
  }
  const method = (opts.method || "GET").toUpperCase();
  const retryableAdmission = method === "POST" && path === "/channel/mobile/task";
  // An admission can safely time out because a retry reuses the same request ID.
  const canTimeOut = (method === "GET" || retryableAdmission)
    && !opts.signal && typeof AbortController !== "undefined";
  const controller = canTimeOut ? new AbortController() : null;
  const deadline = controller ? setTimeout(() => controller.abort(), 20000) : null;
  let res;
  let data = {};
  try {
    res = await fetch(path, { ...opts, headers, signal: controller?.signal ?? opts.signal });
    if (res.status !== 401 && res.status !== 204) data = await res.json();
  } catch (error) {
    // An HTTP 200 can arrive before the body is lost. Keep the last rendered snapshot.
    if (!res || res.ok) {
      let message = res ? "响应未收完整，等待重试" : "无法连接桌面端";
      if (controller?.signal.aborted) message = retryableAdmission ? "桌面确认超时" : "读取超时，等待重试";
      if (S.token === token) setConn(false, message);
      throw new Error(message, { cause: error });
    }
  } finally {
    if (deadline !== null) clearTimeout(deadline);
  }
  if (res.status === 401) {
    if (S.token === token) showGate("令牌已失效，请重新输入");
    throw new Error("unauthorized");
  }
  if (!res.ok) {
    throw Object.assign(new Error(data?.error || ("HTTP " + res.status)), {
      status: res.status,
      code: data?.code,
    });
  }
  if (S.token === token) setConn(true);
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
function renderTreeSubStep(st, sIdx, isLast, expansion) {
  const open = expansion?.steps.has(String(st.id));
  const tool = String(st.tool || "工具").trim();
  const act = (st.action && st.action !== st.tool) ? st.action : (st.description || st.action || "");
  
  let shortTool = formatToolBadge(tool);
  let formattedMain = esc(act || tool || `步骤 #${sIdx + 1}`);
  formattedMain = formattedMain.replace(/\b([a-zA-Z0-9_\-\.\/]+\.(?:md|ts|js|rs|json|vue|html|css|py|yaml|yml|toml))\b/g, '<span class="t-target">$1</span>');

  let details = "";
  if (open) {
    const params = st.command || st.params;
    const paramsHtml = params ? `<div class="label">⌨️ 参数</div><pre>${esc(typeof params === "string" ? params : JSON.stringify(params, null, 2))}</pre>` : "";
    const output = st.output ? `<div class="label">📄 输出</div><pre>${esc(st.output)}</pre>` : "";
    details = `<div class="step-detail-box">${paramsHtml}${output}</div>`;
  }

  // 思考块
  let thoughtHtml = "";
  if (st.thought) {
    const thOpen = expansion?.thoughts.has(String(st.id));
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
    <div class="mobile-sub-step-row ${isLastClass} ${esc(st.status || 'pending')}" data-step-id="${esc(st.id)}">
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
        ${details}
      </div>
    </div>
  `;
}

/* 🌟 将物理步骤按阶段上下级聚合成树形折叠清单 */
function renderStepsTree(steps, expansion) {
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
    const isOpen = expansion?.plans.get(g.id) ?? defaultOpen;
    const statusIcon = g.status === "running" 
      ? '<span class="mobile-plan-spinner"></span>' 
      : (g.status === "failed" ? '<span class="mobile-plan-ic fail">✕</span>' : '<span class="mobile-plan-ic ok">✓</span>');

    const subStepsHtml = isOpen
      ? g.steps.map((st, sIdx) => renderTreeSubStep(st, sIdx, sIdx === g.steps.length - 1, expansion)).join("")
      : "";

    return `
      <div class="mobile-plan-tree-group ${g.status} ${isOpen ? 'is-expanded' : ''}">
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
    stage_review_arbitration: "stage_review_arbitration",
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
    requestId: String(data.request_id || ""),
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
  if (!hasCurrentHumanForm(run)) {
    S.humanForm.runId = run.run_id;
    S.humanForm.waitId = run.wait_id;
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
  if (form.kind === "stage_review_arbitration") {
    bodyHtml = `<div class="human-confirm-box">
      <button type="button" class="human-choice-btn confirm-btn" onclick="submitMobilePlanApproval(true)">允许继续修复</button>
      <button type="button" class="human-choice-btn" onclick="submitMobilePlanApproval(false)">不批准</button>
    </div>`;
  } else if (form.kind === "plan_approval") {
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
  if (form.kind !== "text" && form.kind !== "custom_html" && form.kind !== "plan_approval" && form.kind !== "stage_review_arbitration") {
    writeInHtml = `
      <div class="human-write-in-wrap">
        <label class="human-write-in-label" for="humanWriteInInput">补充说明 / 自定义要求（可选）:</label>
        <input id="humanWriteInInput" class="human-write-in-input" type="text" placeholder="如果以上选项不符合，可在此输入补充意见后提交…" value="${esc(S.humanForm.writeInText)}" oninput="S.humanForm.writeInText=this.value" onkeydown="if(event.key==='Enter')submitHumanIntervention()">
      </div>
    `;
  }

  const isSubmitting = S.humanForm.isSubmitting;
  const actionsBarHtml = ["plan_approval", "stage_review_arbitration"].includes(form.kind) ? "" : `
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

function hasCurrentHumanForm(run) {
  return S.humanForm.runId === run.run_id && S.humanForm.waitId === run.wait_id;
}

function isOpenSession(sessionId) {
  return S.view.name === "flow" && S.currentSessionId === sessionId;
}

function currentFlow() {
  return S.flow && isOpenSession(S.flow.sessionId) ? S.flow : null;
}

function isActiveRun(run) {
  return run?.status === "running" || run?.status === "waiting_human";
}

const flowScroll = { following: true, awaitingFirstLayout: true, top: 0, frame: null, anchor: null };
const FLOW_TAIL_THRESHOLD = 48;

function flowScrollBounds() {
  const root = document.scrollingElement || document.documentElement;
  return {
    top: Math.max(0, window.scrollY ?? root.scrollTop ?? 0),
    end: Math.max(0, (root.scrollHeight ?? 0) - (root.clientHeight || window.innerHeight || 0)),
  };
}

function updateLatestButton() {
  $("btnScrollBottom").classList.toggle("hidden", !currentFlow() || flowScroll.following);
}

function resetFlowScroll() {
  if (flowScroll.frame !== null) window.cancelAnimationFrame(flowScroll.frame);
  flowScroll.frame = null;
  flowScroll.anchor = null;
  flowScroll.following = true;
  flowScroll.awaitingFirstLayout = true;
  flowScroll.top = flowScrollBounds().top;
  updateLatestButton();
}

function captureFlowAnchor() {
  if (flowScroll.following || flowScroll.anchor) return null;
  const { top } = flowScrollBounds();
  const timeline = $("timeline");
  const rect = timeline.getBoundingClientRect();
  const x = (rect.left + rect.right) / 2;
  const topbar = document.querySelector(".topbar")?.getBoundingClientRect().bottom ?? 0;
  for (const offset of [8, 28, 56]) {
    const node = document.elementFromPoint(x, topbar + offset)?.closest("[data-step-id], [data-turn-id]");
    if (!node || !timeline.contains(node)) continue;
    const turn = node.closest("[data-turn-id]");
    return { top, node, y: node.getBoundingClientRect().top, turn, turnY: turn?.getBoundingClientRect().top };
  }
  return { top };
}

function restoreFlowAnchor(anchor, top) {
  if (!anchor.node) return anchor.top;
  let node = anchor.node;
  if (!node.isConnected && anchor.turn?.isConnected && node.dataset.stepId !== undefined) {
    node = anchor.turn.querySelector(`[data-step-id="${CSS.escape(node.dataset.stepId)}"]`);
  }
  if (node?.isConnected) return top + node.getBoundingClientRect().top - anchor.y;
  if (anchor.turn?.isConnected) return top + anchor.turn.getBoundingClientRect().top - anchor.turnY;
  return anchor.top;
}

function scheduleFlowScroll(anchor = null) {
  if (!currentFlow()) return;
  flowScroll.anchor ??= anchor;
  if (flowScroll.frame !== null) return;
  flowScroll.frame = window.requestAnimationFrame(() => {
    flowScroll.frame = null;
    const anchor = flowScroll.anchor;
    flowScroll.anchor = null;
    if (!currentFlow()) return;
    const { top, end } = flowScrollBounds();
    const next = flowScroll.following ? end : anchor ? restoreFlowAnchor(anchor, top) : top;
    const target = Math.max(0, Math.min(end, next));
    if (Math.abs(target - top) > 1) window.scrollTo(0, target);
    flowScroll.top = flowScrollBounds().top;
    flowScroll.awaitingFirstLayout = false;
    updateLatestButton();
  });
}

function resumeFlowFollow() {
  flowScroll.following = true;
  flowScroll.anchor = null;
  scheduleFlowScroll();
  updateLatestButton();
}

function markFlowGesture(towardHistory) {
  flowScroll.anchor = null;
  if (towardHistory && flowScrollBounds().top > 0) {
    flowScroll.following = false;
    updateLatestButton();
  }
}

function bindFlowScroll() {
  const isPageGesture = target => !target.closest?.(".modal, .sidebar, .composer-wrap, .thought-content, .step-detail-box pre, .human-plan-desc");
  window.addEventListener("scroll", () => {
    const { top, end } = flowScrollBounds();
    if (currentFlow()) {
      const towardLatest = top > flowScroll.top + 1;
      const nearLatest = end - top <= FLOW_TAIL_THRESHOLD;
      const composerFocused = document.activeElement === $("followInput");
      // Replacing the previous session can clamp its old scroll position before the first layout.
      if (top < flowScroll.top - 1 && !nearLatest && !composerFocused && !flowScroll.awaitingFirstLayout) flowScroll.following = false;
      if (towardLatest && nearLatest) flowScroll.following = true;
      updateLatestButton();
    }
    flowScroll.top = top;
  }, { passive: true });
  window.addEventListener("wheel", event => {
    if (currentFlow() && isPageGesture(event.target) && Math.abs(event.deltaY) > Math.abs(event.deltaX)) {
      markFlowGesture(event.deltaY < 0);
    }
  }, { passive: true });
  let touchStart = null;
  window.addEventListener("touchstart", event => {
    touchStart = currentFlow() && event.touches.length === 1 && isPageGesture(event.target)
      ? { x: event.touches[0].clientX, y: event.touches[0].clientY } : null;
  }, { passive: true });
  window.addEventListener("touchmove", event => {
    if (event.touches.length !== 1) touchStart = null;
    if (!touchStart || !currentFlow()) return;
    const touch = event.touches[0];
    const dy = touch.clientY - touchStart.y;
    if (Math.abs(dy) >= 6 && Math.abs(dy) > Math.abs(touch.clientX - touchStart.x)) {
      markFlowGesture(dy > 0);
      touchStart = { x: touch.clientX, y: touch.clientY };
    }
  }, { passive: true });
  const endTouch = () => { touchStart = null; };
  window.addEventListener("touchend", endTouch, { passive: true });
  window.addEventListener("touchcancel", endTouch, { passive: true });
  window.addEventListener("mobile:composer-geometry", () => scheduleFlowScroll());
  $("btnScrollBottom").onclick = resumeFlowFollow;
}

window.togglePlanRevisionInput = function() {
  const el = $("planRevisionWrap");
  if (!el) return;
  el.style.display = el.style.display === "none" ? "block" : "none";
};

window.submitMobilePlanApproval = async function(approved) {
  const f = currentFlow();
  if (f?.run?.status !== "waiting_human" || !hasCurrentHumanForm(f.run) || S.humanForm.isSubmitting) return;
  let text = "";
  const form = normalizeHumanForm(f.run.paused_form, "");
  const stage = form.kind === "stage_review_arbitration";
  if (stage) {
    if (approved && !form.requestId) return;
    text = approved
      ? `【阶段仲裁】允许继续修复，问题移交终审。\n<!--stage-review-approval:${form.requestId}-->`
      : "【阶段仲裁】不批准继续修复。";
  } else if (approved) {
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
      body: JSON.stringify({ text, wait_id: f.run.wait_id }),
    });
    if (isOpenSession(f.sessionId)) {
      toast(stage ? "阶段仲裁结果已提交" : (approved ? "方案已批准，Agent 开始执行" : "方案已打回，Agent 重新推演"));
      if (hasCurrentHumanForm(f.run)) {
        S.humanForm.runId = "";
        S.humanForm.writeInText = "";
      }
      await loadFlow(f.sessionId);
    }
  } catch (e) {
    if (!isOpenSession(f.sessionId) || !hasCurrentHumanForm(f.run)) return;
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
  const f = currentFlow();
  if (f?.run?.status !== "waiting_human") return;
  const form = normalizeHumanForm(f.run.paused_form, "");
  S.humanForm.multiSelectedIds = selectAll ? form.options.map(o => o.id) : [];
  updateAskCardDom();
};

window.submitHumanIntervention = async function() {
  const f = currentFlow();
  if (f?.sessionUnavailable || f?.run?.status !== "waiting_human" || !hasCurrentHumanForm(f.run) || S.humanForm.isSubmitting) return;
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
      body: JSON.stringify({ text: answerText, wait_id: f.run.wait_id }),
    });
    if (isOpenSession(f.sessionId)) {
      toast("已提交反馈，Agent 继续执行");
      if (hasCurrentHumanForm(f.run)) {
        S.humanForm.runId = "";
        S.humanForm.writeInText = "";
      }
      await loadFlow(f.sessionId);
    }
  } catch (e) {
    if (isOpenSession(f.sessionId) && hasCurrentHumanForm(f.run)) toast("提交失败: " + e.message);
  } finally {
    if (isOpenSession(f.sessionId) && hasCurrentHumanForm(f.run)) {
      S.humanForm.isSubmitting = false;
      updateAskCardDom();
    }
  }
};

function updateAskCardDom() {
  const askCard = $("askCard");
  const f = currentFlow();
  if (!askCard || !f) return;
  const html = renderAskCard(f.run);
  askCard.innerHTML = html;
  askCard.dataset.renderedHtml = html;
}

// ---------------------------------------------------------------- 数据加载与多轮会话流渲染
const traceDetailCache = new WeakMap();
const TRACE_HISTORY_BATCH = 4;
let activeFlowLoad = null;

function traceDetailStamp(trace) {
  return `${trace.step_count ?? ""}:${trace.updated_at || ""}:${trace.status || ""}`;
}

async function fetchTraceDetail(t, cache, isCurrent = () => true) {
  if (t.step_count === 0) return t;
  const stamp = traceDetailStamp(t);
  const previous = cache.get(t.id);
  if (previous?.stamp === stamp) return previous.trace;
  try {
    const detail = await api(`/channel/trace/${encodeURIComponent(t.id)}`);
    if (detail?.trace?.id === t.id) {
      if (isCurrent()) cache.set(t.id, { stamp, trace: detail.trace });
      return detail.trace;
    }
  } catch { /* Retain the last received steps until this trace can be refreshed. */ }
  return previous ? { ...t, steps: previous.trace.steps } : t;
}

async function hydrateTraceBatch(view, details, batch) {
  if (!batch.length || S.view !== view) return;
  await Promise.all(batch.map(async summary => {
    const stamp = traceDetailStamp(summary);
    const isCurrent = () => S.view === view && details.expected.get(summary.id) === stamp;
    if (!isCurrent()) return;
    const key = JSON.stringify([summary.id, stamp]);
    let request = details.inflight.get(key);
    if (!request) {
      request = fetchTraceDetail(summary, details.cache, isCurrent).finally(() => details.inflight.delete(key));
      details.inflight.set(key, request);
    }
    await request;
  }));
  if (S.view !== view) return;
  const loaded = new Map();
  for (const summary of batch) {
    const stamp = traceDetailStamp(summary);
    const saved = details.cache.get(summary.id);
    if (details.expected.get(summary.id) === stamp && saved?.stamp === stamp) loaded.set(summary.id, saved.trace);
  }
  if (!loaded.size) return;
  const flow = currentFlow();
  if (!flow || !flow.traces.some(trace => loaded.has(trace.id) && loaded.get(trace.id) !== trace)) return;
  flow.traces = flow.traces.map(trace => loaded.get(trace.id) ?? trace);
  renderFlow();
}

async function hydrateOlderTraceDetails(view, details) {
  if (details.loadingOlder) return;
  details.loadingOlder = true;
  try {
    while (details.older.length && S.view === view) {
      const batch = details.older.splice(0, TRACE_HISTORY_BATCH);
      await hydrateTraceBatch(view, details, batch);
    }
  } finally {
    details.loadingOlder = false;
  }
}

async function loadHome() {
  const [projRes, sessRes, runsRes, evalsRes] = await Promise.all([
    api("/channel/projects"),
    api("/channel/sessions"),
    // 旧二进制无这两个路由时降级为空列表，不阻塞首页
    api("/channel/runs").catch(() => ({})),
    api("/channel/evals").catch(() => ({})),
  ]);
  S.projects = projRes.projects || [];
  S.sessions = sessRes.sessions || [];
  S.runs = runsRes.runs || [];
  S.evals = evalsRes.evals || [];
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
  if (!isOpenSession(sessionId)) return;
  const view = S.view;
  if (activeFlowLoad?.view === view) {
    activeFlowLoad.refresh = true;
    return;
  }
  const load = { view, refresh: false, refreshingCheckpoint: view.checkpointStale };
  activeFlowLoad = load;
  // Each open view owns its cache and in-flight history so navigation releases both.
  let details = traceDetailCache.get(view);
  if (!details) {
    details = { cache: new Map(), expected: new Map(), inflight: new Map(), older: [], loadingOlder: false };
    traceDetailCache.set(view, details);
  }
  try {
    const promises = [
      api("/channel/runs"),
      api(`/channel/session/${encodeURIComponent(sessionId)}/traces`, {
        headers: { "X-Channel-Trace-Metadata": "compact" },
      }),
      api(`/channel/session/${encodeURIComponent(sessionId)}/artifacts`),
    ];
    const needSessions = !S.sessions.length || !S.sessions.some(s => s.id === sessionId);
    if (needSessions) {
      promises.push(api("/channel/sessions"));
    }
    const [runsRes, tracesRes, artsRes, sessRes] = await Promise.all(promises);
    if (S.view !== view) return;
    if (sessRes && sessRes.sessions) {
      S.sessions = sessRes.sessions;
    }
    const hasSessionScope = Object.hasOwn(tracesRes, "session");
    const session = hasSessionScope ? tracesRes.session : (S.sessions.find(s => s.id === sessionId) || null);
    if (hasSessionScope && !session) view.sessionScopeInvalidated = true;
    if (session) {
      const projectId = session.project_id ?? null;
      if (!Object.hasOwn(view, "sessionProject")) view.sessionProject = projectId;
      else if (view.sessionProject !== projectId) view.sessionScopeInvalidated = true;
    }
    const run = (runsRes.runs || []).filter(r => r.session_id === sessionId)
      .sort((a, b) => (b.started_at || "").localeCompare(a.started_at || ""))[0] || null;
    const traceList = tracesRes.traces || [];
    
    // Distinct runs can have identical user prompts. Reconcile the placeholder by run identity.
    const expected = new Map();
    const pending = [];
    const uniqueTraces = [];
    for (const trace of traceList) {
      if (!trace?.id || expected.has(trace.id)) continue;
      const stamp = traceDetailStamp(trace);
      expected.set(trace.id, stamp);
      const cached = details.cache.get(trace.id);
      if (trace.step_count === 0) uniqueTraces.push(trace);
      else if (cached?.stamp === stamp) uniqueTraces.push(cached.trace);
      else {
        uniqueTraces.push(cached ? { ...trace, steps: cached.trace.steps } : trace);
        pending.push(trace);
      }
    }

    // 仅当没有任何一条已入库 trace 与当前 run 匹配时，才补充临时执行中占位
    if (run?.run_id && run.goal_preview && isActiveRun(run) && !expected.has(run.run_id)) {
      uniqueTraces.push({
        id: run.run_id,
        goal: run.goal_preview,
        status: run.status,
        started_at: run.started_at,
      });
    }

    const checkpoint = tracesRes.task_checkpoint ?? null;
    const previous = currentFlow()?.checkpoint;
    if (isActiveRun(run) || previous?.task_id !== checkpoint?.task_id || previous?.version !== checkpoint?.version) {
      view.composerStartMode = null;
    }
    if (load.refreshingCheckpoint) view.checkpointStale = false;
    S.flow = {
      sessionId,
      session,
      sessionUnavailable: !!view.sessionScopeInvalidated,
      run,
      checkpoint,
      traces: uniqueTraces,
      artifacts: artsRes.artifacts || [],
    };
    details.expected = expected;
    for (const id of details.cache.keys()) {
      if (!expected.has(id)) details.cache.delete(id);
    }
    renderFlow();
    const latest = pending.at(-1);
    const priority = latest && latest.id === uniqueTraces.at(-1)?.id ? [pending.pop()] : [];
    details.older = pending.reverse();
    // Detail requests can survive a disconnect. Release the metadata refresh
    // slot now so a reconnect can fetch a newer snapshot while they settle.
    hydrateTraceBatch(view, details, priority).then(() => {
      if (S.view === view && details.expected === expected) {
        return hydrateOlderTraceDetails(view, details);
      }
    }).catch(() => {});
  } finally {
    if (activeFlowLoad === load) activeFlowLoad = null;
    if (load.refresh && S.view === view) {
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
  renderHeaderSubtitle();
  $("btnBackTop").classList.add("hidden");
  $("btnDelSession").classList.add("hidden");
  $("btnMenu").classList.remove("hidden");
  $("homeMeta").textContent = `${S.projects.length + 1} 个工作区 · ${S.sessions.length} 个任务`;
  renderRecent();
  renderEvals();
  
  const groups = groupsOf();
  $("wsList").innerHTML = groups.map(g => {
    const open = S.openGroups.has(g.key);
    // 运行中任务置顶：移动端列表长、侧栏滚动不可靠，把活跃任务浮到最前直接可点
    const sorted = g.sessions.slice().sort((a, b) =>
      (b.status === "running" ? 1 : 0) - (a.status === "running" ? 1 : 0));
    const tasks = sorted.length
      ? sorted.slice(0, 15).map(s => taskCard(s)).join("")
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

// ===== 最近动态（runs 全局聚合）与评测报告（在线浏览，路由在 http.rs handler_eval_*） =====
function renderRecent() {
  const feed = $("recentFeed");
  if (!feed) return;
  $("recentMeta").textContent = S.runs.length ? `${S.runs.length} 条任务记录` : "";
  const runs = S.runs.slice(0, 8);
  if (!runs.length) {
    feed.innerHTML = `<div class="empty-tip">暂无任务动态</div>`;
    return;
  }
  feed.innerHTML = runs.map(r => {
    const isRunning = r.status === "running" || r.status === "waiting_human";
    const dot = isRunning ? '<span style="color:#38bdf8; font-weight:600;">● 进行中</span> · ' : "";
    return `
      <div class="task-row-card ${isRunning ? "running-card" : ""}" data-recent-run="${esc(r.run_id || "")}">
        <div class="tt">
          <div class="title">${esc(r.goal_preview || "未命名任务")}</div>
          <div class="sub-line">${dot}${esc(relTime(r.started_at))}${r.session_id ? " · 点击查看执行流" : ""}</div>
        </div>
        ${chip(r.status)}
      </div>
    `;
  }).join("");
  feed.querySelectorAll("[data-recent-run]").forEach(el => {
    el.onclick = () => {
      const run = S.runs.find(x => (x.run_id || "") === el.dataset.recentRun);
      if (run?.session_id) openSession(run.session_id);
    };
  });
}

function renderEvals() {
  const list = $("evalList");
  if (!list) return;
  $("evalMeta").textContent = S.evals.length ? `${S.evals.length} 份报告` : "";
  const evals = S.evals.slice(0, 10);
  if (!evals.length) {
    list.innerHTML = `<div class="empty-tip">暂无评测报告（agent_evaluations）</div>`;
    return;
  }
  list.innerHTML = evals.map(ev => `
    <div class="task-row-card" data-eval-id="${esc(ev.id)}">
      <div class="tt">
        <div class="title">${esc(short(ev.id.replace(/^agent-[a-z-]+-/, ""), 46))}</div>
        <div class="sub-line">${esc(relTime(new Date((ev.modified_secs || 0) * 1000).toISOString()))}${ev.has_markdown ? " · 📄 报告" : ""}</div>
      </div>
      <span class="tag-badge">浏览</span>
    </div>
  `).join("");
  list.querySelectorAll("[data-eval-id]").forEach(el => {
    el.onclick = () => openEval(el.dataset.evalId).catch(e => toast(e.message));
  });
}

async function openEval(evalId) {
  const res = await api(`/channel/eval/${encodeURIComponent(evalId)}`);
  let html;
  if (res.kind === "markdown") {
    html = formatMarkdown(res.content || "");
  } else {
    let summary = "";
    try {
      const data = JSON.parse(res.content || "{}");
      const m = data.metrics || {};
      summary = `<div class="eval-summary">
        <b>状态</b> ${esc(data.trace_status || "?")} · <b>结论</b> ${esc(data.verdict || "?")}<br>
        <b>调用</b> ${esc(String(m.llm_call_count ?? "?"))} 次 · <b>解析成功率</b> ${esc(String(m.parse_success_rate_percent ?? "?"))}% ·
        <b>工具失败率</b> ${esc(String(m.tool_failure_rate_percent ?? "?"))}% · <b>缓存命中</b> ${esc(String(m.cache_hit_rate_percent ?? "?"))}%
      </div>`;
    } catch { /* 非结构化 JSON 原样展示 */ }
    html = `${summary}<pre class="mono" style="white-space:pre-wrap; font-size:11px;">${esc(res.content || "")}</pre>`;
  }
  $("apTitle").textContent = `评测报告 · ${short(evalId, 40)}`;
  $("apBody").innerHTML = html;
  $("apOpenRaw").classList.add("hidden");
  $("apDownload").classList.add("hidden");
  $("artPreview").classList.remove("hidden");
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

const renderedTimelines = new WeakMap();

function createTurnNode(id) {
  const element = document.createElement("div");
  element.className = "turn-section";
  element.dataset.turnId = id;
  element.style.marginBottom = "12px";
  const goal = document.createElement("div");
  const steps = document.createElement("div");
  const result = document.createElement("div");
  element.append(goal, steps, result);
  return { element, goal, steps, result };
}

function renderTimeline(flow, active, dirLabel, fullDirPath) {
  const timeline = $("timeline");
  let rendered = renderedTimelines.get(S.view);
  if (!rendered) {
    rendered = { turns: new Map(), empty: null };
    renderedTimelines.set(S.view, rendered);
    timeline.innerHTML = "";
  }
  if (!flow.traces.length) {
    const empty = `<div class="empty-tip">当前对话所在目录：<b>${esc(dirLabel)}</b><br><span style="font-size:11px; color:var(--dim);">${esc(fullDirPath)}</span><br><br>直接在下方输入内容即可开始</div>`;
    if (rendered.empty !== empty) timeline.innerHTML = empty;
    rendered.empty = empty;
    rendered.turns.clear();
    return;
  }
  if (rendered.empty !== null) {
    rendered.empty = null;
    timeline.innerHTML = "";
  }

  const ids = new Set(flow.traces.map(trace => trace.id));
  for (const [id, node] of rendered.turns) {
    if (!ids.has(id)) {
      node.element.remove();
      rendered.turns.delete(id);
    }
  }
  let next = timeline.firstElementChild;
  for (const [index, trace] of flow.traces.entries()) {
    let node = rendered.turns.get(trace.id);
    if (!node) {
      node = createTurnNode(trace.id);
      rendered.turns.set(trace.id, node);
    }
    if (node.element !== next) timeline.insertBefore(node.element, next);
    next = node.element.nextElementSibling;

    if (node.goalText !== trace.goal) {
      node.goalText = trace.goal;
      node.goal.innerHTML = trace.goal
        ? `<div class="goal-bubble"><span class="arrow">→</span><div class="gtxt">${formatMarkdown(trace.goal)}</div></div>`
        : "";
    }
    const expansion = S.expandedTurns.get(trace.id);
    if (node.stepData !== trace.steps || node.stepRevision !== expansion?.revision) {
      node.stepData = trace.steps;
      node.stepRevision = expansion?.revision;
      const steps = renderStepsTree(trace.steps, expansion);
      if (node.stepsHtml !== steps) {
        node.steps.innerHTML = steps;
        node.stepsHtml = steps;
      }
    }

    const isLatest = index === flow.traces.length - 1;
    const failed = trace.status === "failed";
    const result = (isLatest && active) ? "" :
      (trace.completion?.summary || (isLatest && failed ? flow.run?.error : "") || "");
    if (node.resultText !== result || node.failed !== failed) {
      node.resultText = result;
      node.failed = failed;
      node.result.innerHTML = result
        ? `<div class="result-card ${failed ? "fail" : ""}"><div class="rlabel">${failed ? "✕ 失败原因" : "✓ 执行结果"}</div>${formatMarkdown(result)}</div>`
        : "";
    }
  }
}

function renderFlow() {
  const f = currentFlow();
  if (!f) return;
  const scrollAnchor = captureFlowAnchor();
  const s = f.session || {};
  const visibleRun = f.sessionUnavailable ? null : f.run;
  const active = isActiveRun(visibleRun);
  const proj = s.project_id ? S.projects.find(p => p.id === s.project_id) : null;
  const dirLabel = f.sessionUnavailable ? "会话已失效" : s.project_id ? `📁 ${s.project_name || proj?.name || "项目会话"}` : f.session ? "💬 临时工作区" : "会话归属待确认";
  const fullDirPath = s.project_id ? (s.root_path || proj?.root_path || "") : f.session ? "scratch (临时目录)" : "";

  $("topTitle").textContent = s.title || "任务会话";
  renderHeaderSubtitle(s.project_id && fullDirPath ? `${dirLabel} · ${fullDirPath}` : dirLabel);
  $("btnBackTop").classList.remove("hidden");
  $("btnDelSession").classList.remove("hidden");
  $("btnMenu").classList.add("hidden");

  renderTimeline(f, active, dirLabel, fullDirPath);

  // 实时状态行（带 DOM 差异防闪烁）
  const live = $("liveLine");
  let liveHtml = "";
  let liveClass = "live-line";
  if (visibleRun?.status === "running") {
    liveClass = "live-line";
    liveHtml = `<span class="livedot"></span><strong>工作中</strong> <span style="color:var(--dim)">· ${esc(dirLabel)} · ${esc(f.run.state_detail || f.run.phase || "执行中")}${f.run.event_count ? ` · ${f.run.event_count} 事件` : ""}</span>`;
  } else if (visibleRun?.status === "waiting_human") {
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
  const questionChanged = visibleRun?.status === "waiting_human"
    ? !hasCurrentHumanForm(visibleRun)
    : !!askCard.dataset.renderedHtml;
  const askHtml = renderAskCard(visibleRun);
  const activeEl = document.activeElement;
  const isTypingInAsk = activeEl && (activeEl.id === "humanWriteInInput" || activeEl.id === "humanTextInput");
  if (questionChanged || (!isTypingInAsk && askCard.dataset.renderedHtml !== askHtml)) {
    askCard.innerHTML = askHtml;
    askCard.dataset.renderedHtml = askHtml;
  }

  // 产物清单
  const arts = f.artifacts || [];
  const artBox = $("artBox");
  const artsHtml = arts.length ? `<div style="margin-top:12px;"><div style="font-size:12px; font-weight:700; margin-bottom:6px;">📦 任务产物 (${arts.length})</div>` +
    arts.map(a => `
      <div style="display:flex; align-items:center; gap:6px; background:var(--card); border:1px solid var(--line); border-radius:8px; padding:6px 10px; margin-bottom:4px;">
        <span>${artIcon(a)}</span>
        <span style="flex:1; min-width:0; overflow:hidden; text-overflow:ellipsis; font-size:12px;">${esc(a.display_name)}</span>
        <span style="color:var(--dim); font-size:10.5px;">${fmtSize(a.size_bytes || 0)}</span>
        <a class="btn primary" style="padding:2px 7px; font-size:10.5px;" href="/channel/artifact/${encodeURIComponent(a.id)}/raw?token=${encodeURIComponent(S.token)}&dl=1" download>下载</a>
      </div>
    `).join("") + `</div>` : "";
  if (artBox.dataset.renderedHtml !== artsHtml) {
    artBox.innerHTML = artsHtml;
    artBox.dataset.renderedHtml = artsHtml;
  }

  // Composer 状态与占位符
  const composer = $("composer");
  const isWaitingHuman = visibleRun?.status === "waiting_human";
  updateComposerSubmit(f);
  renderComposerOptions(f);
  composer.classList.toggle("waiting-human", !!isWaitingHuman);
  $("btnStop").classList.toggle("hidden", f.sessionUnavailable || !(f.run && f.run.status === "running"));
  scheduleFlowScroll(scrollAnchor);
}

const composerDrafts = new Map();
const composerOptionsBySession = new Map();
const pendingComposerSends = new Set();
const mobileAdmissions = new Map();
const NEW_TASK_SCOPE = "new";
const DEFINITIVE_ADMISSION_ERRORS = new Set([
  "session_unavailable", "session_project_mismatch", "project_unavailable", "request_conflict", "session_in_flight", "invalid_submission",
  "checkpoint_changed",
]);
let attachmentSessionId = null;
let refreshComposerGeometry = () => {};

function sessionAdmissionScope(sessionId) {
  return "session:" + sessionId;
}

function mobileRequestId() {
  if (globalThis.crypto.randomUUID) return globalThis.crypto.randomUUID();
  // A phone can open the console over HTTP, where randomUUID may be unavailable.
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join("");
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join("-");
}

function pendingMobileAdmission(scope) {
  if (!mobileAdmissions.has(scope)) {
    let saved = null;
    try {
      const entry = JSON.parse(sessionStorage.getItem("free_api_admission:" + scope));
      if (entry?.body?.request_id && typeof entry.draftText === "string") {
        saved = { ...entry, posted: true, files: [], uploaded: new Set() };
      }
    } catch (_) {}
    mobileAdmissions.set(scope, saved);
  }
  return mobileAdmissions.get(scope);
}

function rememberMobileAdmission(scope, attempt) {
  mobileAdmissions.set(scope, attempt);
  // Store only the exact submission needed to reconcile a lost reply after reload.
  try {
    const key = "free_api_admission:" + scope;
    if (attempt?.posted) {
      sessionStorage.setItem(key, JSON.stringify({ body: attempt.body, draftText: attempt.draftText }));
    } else {
      sessionStorage.removeItem(key);
    }
  } catch (_) {}
}

async function submitMobileAdmission(scope, attempt) {
  attempt.posted = true;
  rememberMobileAdmission(scope, attempt);
  const receipt = await api("/channel/mobile/task", { method: "POST", body: JSON.stringify(attempt.body) });
  if (!receipt?.run_id || !receipt?.session_id ||
      (attempt.body.session_id && attempt.body.session_id !== receipt.session_id)) {
    throw new Error("桌面未返回完整的任务确认");
  }
  rememberMobileAdmission(scope, null);
  return receipt;
}

function composerOptionsKey(sessionId) {
  return "free_api_task_options:" + sessionId;
}

function composerOptions(sessionId) {
  if (!composerOptionsBySession.has(sessionId)) {
    const options = { model_id: "", context_budget_mode: "", role_card: "" };
    try {
      const saved = JSON.parse(sessionStorage.getItem(composerOptionsKey(sessionId)));
      for (const key of Object.keys(options)) {
        if (typeof saved?.[key] === "string") options[key] = saved[key];
      }
    } catch (_) {}
    composerOptionsBySession.set(sessionId, options);
  }
  return composerOptionsBySession.get(sessionId);
}

function setComposerOption(sessionId, key, value) {
  const options = composerOptions(sessionId);
  options[key] = value;
  try {
    sessionStorage.setItem(composerOptionsKey(sessionId), JSON.stringify(options));
  } catch (_) {}
}

function renderComposerOptions(flow = currentFlow()) {
  const options = flow?.session ? composerOptions(flow.sessionId) : {};
  const model = options.model_id;
  const sessionModel = modelName(flow?.session?.model_id);
  const budget = options.context_budget_mode;
  const budgetName = BUDGETS.find(option => option.value === budget)?.label ?? BUDGETS[0].label;
  const budgetChip = { deep_coding: "长程", cost_saving: "省流", ultra_fast: "极速" }[budget] ?? "长程";
  const role = options.role_card;
  const roleCard = S.roleCards.find(card => card.name === role);
  const roleLabel = roleCard?.label || role;
  const roleTitle = role ? `角色卡 · ${roleLabel}${roleCard?.enabled === false ? "（已停用）" : ""}` : "默认角色";
  const chips = [
    ["Model", model ? short(modelName(model), 6) : "跟随", model ? `任务模型 · ${modelName(model)}` : `跟随会话模型${sessionModel ? ` · ${sessionModel}` : ""}`, model],
    ["Budget", budgetChip, `上下文预算 · ${budgetName}`, budget],
    ["Roles", role ? short(roleLabel, 5) : "默认", roleTitle, role],
  ];
  for (const [key, label, title, value] of chips) {
    const button = $("opt" + key);
    const text = $("lbl" + key);
    if (text.textContent !== label) text.textContent = label;
    if (button.title !== title) {
      button.title = title;
      button.setAttribute("aria-label", title);
    }
    button.classList.toggle("active", !!value);
    button.disabled = !flow?.session || !!flow.sessionUnavailable;
  }
}

function composerStartMode(flow) {
  if (!flow || isActiveRun(flow.run) || S.view.checkpointStale) return null;
  return flow.checkpoint ? S.view.composerStartMode : "new";
}

function chooseComposerStartMode(mode) {
  const flow = currentFlow();
  if (!flow?.checkpoint || flow.sessionUnavailable || pendingComposerSends.has(flow.sessionId) ||
      $("composerStartChoices").classList.contains("hidden")) return;
  S.view.composerStartMode = mode;
  updateComposerSubmit(flow);
}

function updateComposerSubmit(flow = currentFlow()) {
  const btn = $("btnSend");
  const attempt = flow ? pendingMobileAdmission(sessionAdmissionScope(flow.sessionId)) : null;
  const stale = flow?.sessionUnavailable;
  const active = isActiveRun(flow?.run);
  const checking = !!attempt?.posted;
  const refreshing = S.view.checkpointStale && !active;
  const inFlight = !!flow && pendingComposerSends.has(flow.sessionId);
  const choices = !!flow?.checkpoint && !!flow.session && !stale && !active && !checking && !refreshing;
  const startMode = composerStartMode(flow);
  let message = "";
  if (stale) message = "会话已失效或归属已变更。草稿仍保留，请重新打开正确的会话。";
  else if (checking) message = "上次提交尚未确认。再次发送会核对原任务，新的编辑会保留。";
  else if (refreshing) message = "原任务状态已变化，刷新后可重新选择。";
  else if (flow && !flow.session) message = "会话归属尚未确认，请先刷新再发送。";
  const status = $("composerSubmitStatus");
  const statusChanged = status.textContent !== message;
  if (statusChanged) status.textContent = message;
  status.classList.toggle("hidden", !message);
  const choiceGroup = $("composerStartChoices");
  const choicesChanged = choiceGroup.classList.contains("hidden") === choices;
  choiceGroup.classList.toggle("hidden", !choices);
  for (const [id, mode] of [["composerResume", "resume"], ["composerNewTurn", "new"]]) {
    const choice = $(id);
    const pressed = choices && startMode === mode;
    choice.classList.toggle("active", pressed);
    const ariaPressed = String(pressed);
    if (choice.getAttribute("aria-pressed") !== ariaPressed) choice.setAttribute("aria-pressed", ariaPressed);
    choice.disabled = !choices || inFlight;
  }
  const label = checking ? "核对原提交" : startMode === "resume" ? "继续原任务" : startMode === "new" ? "新开一轮" : "发送";
  if (btn.title !== label) {
    btn.title = label;
    btn.setAttribute("aria-label", label);
  }
  btn.disabled = !flow || !!stale || !flow.session || inFlight || (!checking && !active && !startMode);
  $("btnClip").disabled = !flow?.session || !!stale;
  const input = $("followInput");
  const placeholder = stale ? "会话已失效，草稿仍保留"
    : !flow ? "会话加载中…"
    : checking ? "可编辑下一条消息，原提交内容已保留…"
    : flow.run?.status === "waiting_human" ? "回复 Agent 的提问…"
    : active ? "输入新目标会请求停止当前执行…"
    : startMode === "resume" ? "可补充说明，或直接发送以继续…"
    : choices && !startMode ? "选择继续原任务或新开一轮…"
    : "在当前会话输入新一轮的目标…";
  if (input.placeholder !== placeholder) input.placeholder = placeholder;
  if (statusChanged || choicesChanged) refreshComposerGeometry();
}

function invalidateComposerSession(sessionId) {
  const flow = currentFlow();
  if (!flow || flow.sessionId !== sessionId) return;
  S.view.sessionScopeInvalidated = true;
  flow.sessionUnavailable = true;
  renderFlow();
}

function bindComposerViewport() {
  const composer = $("composer");
  const input = $("followInput");
  const rootStyle = document.documentElement.style;
  const viewport = window.visualViewport;
  let frame = null;
  let lastInputWidth = 0;
  let lastInputText = null;
  let lastVisibleHeight = 0;
  let lastInset = -1;
  let lastSpacer = -1;

  refreshComposerGeometry = () => {
    if (frame !== null) return;
    frame = window.requestAnimationFrame(() => {
      frame = null;
      const editable = document.activeElement?.matches("textarea, input:not([type='button'], [type='file'], [type='checkbox'], [type='radio']), [contenteditable='true']");
      const normalScale = viewport && Math.abs(viewport.scale - 1) < 0.01;
      const visibleHeight = Math.round(normalScale ? viewport.height : window.innerHeight);
      const covered = normalScale && editable ? window.innerHeight - viewport.height - viewport.offsetTop : 0;
      const inset = covered > 48 ? Math.round(covered) : 0;
      const viewportChanged = visibleHeight !== lastVisibleHeight || inset !== lastInset;
      if (viewportChanged) {
        rootStyle.setProperty("--visible-viewport-height", `${visibleHeight}px`);
        rootStyle.setProperty("--keyboard-inset", `${inset}px`);
        lastVisibleHeight = visibleHeight;
        lastInset = inset;
      }

      const inputWidth = input.clientWidth;
      if (inputWidth && (inputWidth !== lastInputWidth || input.value !== lastInputText)) {
        input.style.height = "auto";
        input.style.height = `${input.scrollHeight}px`;
        lastInputWidth = inputWidth;
        lastInputText = input.value;
      }

      const composerHeight = composer.offsetHeight;
      const spacer = composerHeight ? composerHeight + inset : 0;
      const spacerChanged = spacer !== lastSpacer;
      if (spacerChanged) {
        rootStyle.setProperty("--composer-spacer", `${spacer}px`);
        lastSpacer = spacer;
      }
      if (spacerChanged || viewportChanged) window.dispatchEvent(new Event("mobile:composer-geometry"));
    });
  };
  input.addEventListener("input", refreshComposerGeometry);
  document.addEventListener("focusin", refreshComposerGeometry);
  document.addEventListener("focusout", refreshComposerGeometry);
  window.addEventListener("resize", refreshComposerGeometry);
  viewport?.addEventListener("resize", refreshComposerGeometry);
  viewport?.addEventListener("scroll", refreshComposerGeometry);
  if (typeof ResizeObserver !== "undefined") new ResizeObserver(refreshComposerGeometry).observe(composer);
  refreshComposerGeometry();
}

function renderComposerFiles() {
  $("composerFiles").innerHTML = S.composerFiles.map((file, index) => {
    const name = esc(file.name ?? file);
    return `<span class="composer-file"><span class="composer-file-name" title="${name}">${name}</span><button type="button" data-remove-composer-file="${index}" aria-label="移除附件 ${name}">×</button></span>`;
  }).join("");
  refreshComposerGeometry();
}

function rememberComposerDraft(sessionId, text, files) {
  if (text || files.length) composerDrafts.set(sessionId, { text, files });
  else composerDrafts.delete(sessionId);
}

function saveComposerDraft() {
  if (S.currentSessionId) rememberComposerDraft(S.currentSessionId, $("followInput").value, S.composerFiles);
}

async function openSession(sessionId) {
  closeSidebarMobile();
  if (!isOpenSession(sessionId) || S.view.sessionScopeInvalidated) {
    closeSearchableSheet();
    saveComposerDraft();
    S.view = { name: "flow" };
    S.flow = null;
    resetFlowScroll();
    const draft = composerDrafts.get(sessionId);
    $("followInput").value = draft?.text ?? pendingMobileAdmission(sessionAdmissionScope(sessionId))?.draftText ?? "";
    S.composerFiles = draft?.files ?? [];
    renderComposerFiles();
    updateComposerSubmit();
    renderComposerOptions();
    $("btnStop").classList.add("hidden");
    $("composer").classList.remove("waiting-human");
    $("topTitle").textContent = S.sessions.find(s => s.id === sessionId)?.title || "任务会话";
    renderHeaderSubtitle();
    for (const id of ["timeline", "liveLine", "askCard", "artBox"]) {
      const element = $(id);
      element.innerHTML = "";
      delete element.dataset.renderedHtml;
    }
    $("timeline").innerHTML = `<div class="empty-tip">加载中…</div>`;
  }
  S.currentSessionId = sessionId;
  renderSidebar();
  $("viewHome").classList.add("hidden");
  $("viewFlow").classList.remove("hidden");
  $("btnDelSession").classList.remove("hidden");
  $("composer").classList.remove("hidden");
  refreshComposerGeometry();
  const view = S.view;
  try { await loadFlow(sessionId); } catch (e) {
    if (S.view === view) toast("加载失败：" + e.message);
  }
}

function goHome() {
  closeSidebarMobile();
  closeSearchableSheet();
  saveComposerDraft();
  S.currentSessionId = null;
  S.flow = null;
  S.view = { name: "home" };
  resetFlowScroll();
  $("viewFlow").classList.add("hidden");
  $("viewHome").classList.remove("hidden");
  $("btnDelSession").classList.add("hidden");
  $("composer").classList.add("hidden");
  refreshComposerGeometry();
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
    composerDrafts.delete(sessionId);
    composerOptionsBySession.delete(sessionId);
    try { sessionStorage.removeItem(composerOptionsKey(sessionId)); } catch (_) {}
    rememberMobileAdmission(sessionAdmissionScope(sessionId), null);
    if (attachmentSessionId === sessionId) attachmentSessionId = null;
  } catch (e) {
    toast("删除失败: " + e.message);
  }
}

async function sendComposer() {
  const f = currentFlow();
  const input = $("followInput");
  if (!f?.session || f.sessionUnavailable || pendingComposerSends.has(f.sessionId)) return;
  const scope = sessionAdmissionScope(f.sessionId);
  let attempt = pendingMobileAdmission(scope);
  let draftText = input.value;
  const text = draftText.trim();
  const runActive = isActiveRun(f.run);
  const startMode = runActive ? attempt?.body.task_start_mode : composerStartMode(f);
  if (!attempt?.posted && ((!runActive && !startMode) || (!text && startMode !== "resume"))) return;
  pendingComposerSends.add(f.sessionId);
  updateComposerSubmit(f);
  resumeFlowFollow();
  let sentFiles = [];
  let keepDraft = false;
  let successMessage;
  try {
    if (!attempt && f.run?.status === "waiting_human") {
      await api(`/channel/run/${f.run.run_id}/answer`, { method: "POST", body: JSON.stringify({ text, wait_id: f.run.wait_id }) });
      successMessage = "已回答，Agent 继续执行";
    } else if (!attempt && f.run?.status === "running") {
      await api(`/channel/run/${f.run.run_id}/goal`, { method: "POST", body: JSON.stringify({ goal: text }) });
      keepDraft = true;
      successMessage = "已请求停止当前执行，目标留在草稿中";
    } else {
      if (!attempt?.posted) {
        const options = composerOptions(f.sessionId);
        const routing = {};
        if (options.model_id) routing.pro = options.model_id;
        if (options.context_budget_mode) routing.context_budget_mode = options.context_budget_mode;
        const body = {
          request_id: attempt?.body.request_id ?? mobileRequestId(),
          session_id: f.sessionId,
          project_id: attempt ? attempt.body.project_id : f.session.project_id ?? null,
          goal: text,
          task_start_mode: startMode,
          expected_checkpoint: runActive ? attempt.body.expected_checkpoint : f.checkpoint ? {
            task_id: f.checkpoint.task_id,
            version: f.checkpoint.version,
          } : null,
        };
        if (Object.keys(routing).length) body.model_routing = routing;
        if (options.role_card) body.role_card = options.role_card;
        attempt = {
          body, draftText, files: [...S.composerFiles], uploaded: attempt?.uploaded ?? new Set(), posted: false,
        };
        rememberMobileAdmission(scope, attempt);
        for (const [i, file] of attempt.files.entries()) {
          if (attempt.uploaded.has(file)) continue;
          if (isOpenSession(f.sessionId)) toast(`上传附件 ${i + 1}/${attempt.files.length}…`, 1500);
          await uploadFile(f.sessionId, file);
          attempt.uploaded.add(file);
        }
      }
      draftText = attempt.draftText;
      sentFiles = attempt.files;
      await submitMobileAdmission(scope, attempt);
      if (isOpenSession(f.sessionId)) S.view.composerStartMode = null;
      successMessage = attempt.body.task_start_mode === "resume" ? "原任务已接续" : "任务已受理";
    }
    const active = isOpenSession(f.sessionId);
    const draft = active ? { text: input.value, files: S.composerFiles } : composerDrafts.get(f.sessionId);
    if (draft) {
      const remainingText = !keepDraft && draft.text === draftText ? "" : draft.text;
      const remainingFiles = draft.files.filter(file => !sentFiles.includes(file));
      if (active) {
        input.value = remainingText;
        S.composerFiles = remainingFiles;
        renderComposerFiles();
      }
      rememberComposerDraft(f.sessionId, remainingText, remainingFiles);
    }
    if (active) {
      toast(successMessage);
      await loadFlow(f.sessionId).catch(() => {});
    }
  } catch (e) {
    if (attempt && DEFINITIVE_ADMISSION_ERRORS.has(e.code)) {
      rememberMobileAdmission(scope, null);
      if (e.code === "session_unavailable" || e.code === "session_project_mismatch") {
        invalidateComposerSession(f.sessionId);
      } else if (isOpenSession(f.sessionId)) {
        if (e.code === "checkpoint_changed") {
          S.view.checkpointStale = true;
          S.view.composerStartMode = null;
          loadFlow(f.sessionId).catch(() => {});
        } else if (e.code === "session_in_flight") {
          loadFlow(f.sessionId).catch(() => {});
        }
      }
    }
    if (isOpenSession(f.sessionId)) {
      const pending = pendingMobileAdmission(scope)?.posted;
      toast(pending ? "任务尚未确认：" + e.message : "操作失败：" + e.message);
    }
  } finally {
    pendingComposerSends.delete(f.sessionId);
    if (currentFlow()?.sessionId === f.sessionId) updateComposerSubmit();
  }
}

async function stopRun() {
  const f = currentFlow();
  if (!f || f.sessionUnavailable || !f.run) return;
  try {
    await api(`/channel/run/${f.run.run_id}/cancel`, { method: "POST" });
    if (isOpenSession(f.sessionId)) {
      toast("已请求停止");
      await loadFlow(f.sessionId);
    }
  } catch (e) { if (isOpenSession(f.sessionId)) toast("停止失败：" + e.message); }
}

// ---------------------------------------------------------------- 带实时搜索的选择弹层
let sheetFilterTimer;

function closeSearchableSheet() {
  if (sheetFilterTimer !== undefined) {
    clearTimeout(sheetFilterTimer);
    sheetFilterTimer = undefined;
  }
  S.sheetOpen = false;
  $("sheetPick").classList.add("hidden");
}

function openSearchableSheet(title, allItems, currentValue, onPick) {
  closeSearchableSheet();
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
        closeSearchableSheet();
        onPick(btn.dataset.v);
      };
    });

    const more = $("sheetLoadMore");
    if (more) more.onclick = () => {
      list.innerHTML = filtered.map(makeRow).join("");
      list.querySelectorAll("button[data-v]").forEach(btn => {
        btn.onclick = () => {
          closeSearchableSheet();
          onPick(btn.dataset.v);
        };
      });
    };
  }

  // 输入时 debounce 300ms 避免每键一次重渲
  searchInput.oninput = (e) => {
    if (sheetFilterTimer !== undefined) clearTimeout(sheetFilterTimer);
    sheetFilterTimer = setTimeout(() => renderList(e.target.value), 300);
  };
  renderList("");
  $("sheetPick").classList.remove("hidden");
  if (allItems.length > 5 && window.matchMedia?.("(pointer: fine)").matches) searchInput.focus({ preventScroll: true });
}

function openComposerOption(key, title, items) {
  const flow = currentFlow();
  if (!flow?.session || flow.sessionUnavailable) return;
  const view = S.view;
  openSearchableSheet(title, items, composerOptions(flow.sessionId)[key], (value) => {
    if (S.view !== view || currentFlow()?.sessionUnavailable) return;
    setComposerOption(flow.sessionId, key, value);
    renderComposerOptions();
  });
}

function bindOptChips() {
  $("optModel").onclick = () => openComposerOption("model_id", "选择下次启动的模型", [
    { value: "", label: "跟随会话模型", sub: modelName(currentFlow()?.session?.model_id) },
    ...S.models.map(m => ({ value: m.id, label: m.display_name || m.name, sub: m.platform_name || "" })),
  ]);
  $("optBudget").onclick = () => openComposerOption("context_budget_mode", "上下文压缩模式", BUDGETS);
  $("optRoles").onclick = () => openComposerOption("role_card", "选择角色卡", [
    { value: "", label: "默认角色" },
    ...S.roleCards.filter(c => c.enabled !== false)
      .map(c => ({ value: c.name, label: c.label || c.name, sub: c.scope_hints || "" })),
  ]);

  $("sheetClose").onclick = closeSearchableSheet;
  $("sheetPick").addEventListener("click", (e) => { if (e.target === $("sheetPick")) closeSearchableSheet(); });
  // 评测报告预览弹层（openEval 打开）
  $("apClose").onclick = () => $("artPreview").classList.add("hidden");
  $("artPreview").addEventListener("click", (e) => { if (e.target === $("artPreview")) $("artPreview").classList.add("hidden"); });
}

let newTaskType = "scratch";
let submittingNewTask = false;

function updateNewSubmit() {
  const attempt = pendingMobileAdmission(NEW_TASK_SCOPE);
  const pending = !!attempt?.posted;
  const locked = submittingNewTask || pending;
  $("newSubmit").disabled = submittingNewTask;
  $("newSubmit").textContent = pending ? "核对原提交" : "启动";
  for (const id of ["newProj", "typeScratch", "typeProject", "btnPickNewModel"]) $(id).disabled = locked;
  $("newGoal").readOnly = locked;
  const status = $("newSubmitStatus");
  const message = pending ? "上次提交尚未确认。再次点击会核对同一任务。" : "";
  if (status.textContent !== message) status.textContent = message;
  status.classList.toggle("hidden", !pending);
}

function openModal(prefillProjectId) {
  closeSidebarMobile();
  const pending = pendingMobileAdmission(NEW_TASK_SCOPE);
  const selectedProjectId = pending ? pending.body.project_id : prefillProjectId;
  newTaskType = selectedProjectId && selectedProjectId !== "__scratch__" ? "project" : "scratch";
  const projects = groupsOf().filter(g => g.key !== "__scratch__");
  $("newProj").innerHTML = projects.map(g =>
    `<option value="${esc(g.key)}" ${g.key === selectedProjectId ? "selected" : ""}>${g.icon} ${esc(g.name)}</option>`).join("")
    || `<option value="">（暂无项目）</option>`;
  $("typeScratch").style.background = newTaskType === "scratch" ? "var(--accent)" : "transparent";
  $("typeProject").style.background = newTaskType === "project" ? "var(--accent)" : "transparent";
  $("projField").style.display = newTaskType === "project" ? "" : "none";
  
  if (pending) {
    S.selectedNewModelId = pending.body.model_id;
  } else if (!S.selectedNewModelId && S.models.length) {
    S.selectedNewModelId = S.models[0].id;
  }
  $("lblNewModel").textContent = S.selectedNewModelId ? modelName(S.selectedNewModelId) : "请选择模型…";
  
  $("newGoal").value = pending?.draftText ?? "";
  updateNewSubmit();
  $("modalNew").classList.remove("hidden");
}

async function submitNew() {
  if (submittingNewTask) return;
  let attempt = pendingMobileAdmission(NEW_TASK_SCOPE);
  if (!attempt) {
    const projectId = newTaskType === "project" ? $("newProj").value : null;
    if (newTaskType === "project" && !projectId) { toast("请先选择项目"); return; }
    const modelId = S.selectedNewModelId || "";
    if (!modelId) { toast("请先选择执行模型"); return; }
    const draftText = $("newGoal").value;
    const goal = draftText.trim();
    if (!goal) { toast("请填写任务内容"); return; }
    attempt = {
      body: {
        request_id: mobileRequestId(),
        project_id: projectId,
        title: short(goal.replace(/\s+/g, " "), 28),
        model_id: modelId,
        goal,
        task_start_mode: "new",
        expected_checkpoint: null,
      },
      draftText,
    };
  }
  const view = S.view;
  submittingNewTask = true;
  let receipt;
  try {
    const submission = submitMobileAdmission(NEW_TASK_SCOPE, attempt);
    updateNewSubmit();
    receipt = await submission;
  } catch (e) {
    if (DEFINITIVE_ADMISSION_ERRORS.has(e.code)) rememberMobileAdmission(NEW_TASK_SCOPE, null);
    const pending = pendingMobileAdmission(NEW_TASK_SCOPE);
    toast(pending?.posted ? "任务尚未确认：" + e.message : "创建失败：" + e.message);
    return;
  } finally {
    submittingNewTask = false;
    updateNewSubmit();
  }
  toast("任务已受理");
  if (S.view === view && !$("modalNew").classList.contains("hidden")) {
    $("modalNew").classList.add("hidden");
    await openSession(receipt.session_id).catch(() => {});
  }
  loadHome().catch(() => {});
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
  $("btnNew").onclick = () => openModal();
  $("btnDelSession").onclick = () => {
    if (S.currentSessionId) deleteSession(S.currentSessionId);
  };
  $("btnSend").onclick = sendComposer;
  $("btnStop").onclick = stopRun;
  $("composerResume").onclick = () => chooseComposerStartMode("resume");
  $("composerNewTurn").onclick = () => chooseComposerStartMode("new");
  bindComposerViewport();
  $("composerFiles").onclick = (event) => {
    const remove = event.target.closest("[data-remove-composer-file]");
    if (!remove) return;
    S.composerFiles.splice(Number(remove.dataset.removeComposerFile), 1);
    saveComposerDraft();
    renderComposerFiles();
  };
  $("btnClip").onclick = () => {
    const flow = currentFlow();
    attachmentSessionId = flow?.session && !flow.sessionUnavailable ? flow.sessionId : null;
    if (attachmentSessionId) $("clipInput").click();
  };
  $("clipInput").onchange = (e) => {
    const sessionId = attachmentSessionId;
    attachmentSessionId = null;
    const files = Array.from(e.target.files);
    e.target.value = "";
    if (!sessionId || !files.length) return;
    if (isOpenSession(sessionId)) {
      S.composerFiles.push(...files);
      renderComposerFiles();
      toast(`已选择 ${S.composerFiles.length} 个附件`);
    } else {
      const draft = composerDrafts.get(sessionId);
      rememberComposerDraft(sessionId, draft?.text ?? "", [...(draft?.files ?? []), ...files]);
    }
  };

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

  bindFlowScroll();

  // 全局事件代理
  document.addEventListener("click", (e) => {
    const del = e.target.closest("[data-del-session]");
    if (del) {
      e.stopPropagation();
      e.preventDefault();
      deleteSession(del.dataset.delSession);
      return;
    }
    const toggle = e.target.closest("[data-plan-toggle], [data-thought-toggle], [data-step]");
    if (toggle) {
      const turnId = toggle.closest("[data-turn-id]")?.dataset.turnId;
      if (!turnId) return;
      flowScroll.following = false;
      let expansion = S.expandedTurns.get(turnId);
      if (!expansion) {
        expansion = { plans: new Map(), thoughts: new Set(), steps: new Set(), revision: 0 };
        S.expandedTurns.set(turnId, expansion);
      }
      const { planToggle, thoughtToggle, step } = toggle.dataset;
      if (planToggle !== undefined) {
        const isOpen = toggle.closest(".mobile-plan-tree-group")?.classList.contains("is-expanded");
        expansion.plans.set(planToggle, !isOpen);
      } else {
        const items = thoughtToggle === undefined ? expansion.steps : expansion.thoughts;
        const id = thoughtToggle ?? step;
        if (items.has(id)) items.delete(id); else items.add(id);
      }
      expansion.revision++;
      renderFlow();
      return;
    }
    const plus = e.target.closest("[data-new]");
    if (plus) { e.stopPropagation(); openModal(plus.dataset.new); return; }
    const group = e.target.closest("[data-toggle]");
    if (group) { toggleGroup(group.dataset.toggle); return; }
    const sess = e.target.closest("[data-session]");
    if (sess) { openSession(sess.dataset.session); return; }
  });
}

function showGate(err) {
  closeChannelEvents();
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
      api("/channel/role-cards").catch(() => ({})),
    ]);
    S.models = (mData.models || []).filter(m => m.enabled !== 0 && m.enabled !== false);
    S.modelById = Object.fromEntries(S.models.map(m => [m.id, m]));
    S.roleCards = rData.role_cards || rData.cards || [];
    if (currentFlow()) renderComposerOptions();
  } catch (e) {}
}

function isAgentActive() {
  return isActiveRun(S.flow?.run);
}

function sseAlive() {
  return !!(S.es && S.es.readyState === 1);
}

const channelRefresh = {
  home: { timer: null, running: false, pending: false, delay: 400 },
  flow: { timer: null, running: false, pending: false, delay: 150 },
};
let channelEventRetry = null;
let mobileEventsBound = false;

function isChannelVisible() {
  return !document.hidden && !$("app").classList.contains("hidden");
}

function queueChannelRefresh(kind) {
  const refresh = channelRefresh[kind];
  if (!isChannelVisible() || refresh.timer !== null) return;
  if (refresh.running) {
    refresh.pending = true;
    return;
  }
  refresh.timer = setTimeout(async () => {
    refresh.timer = null;
    if (!isChannelVisible()) return;
    refresh.running = true;
    try {
      if (kind === "home") await loadHome();
      else if (S.view.name === "flow" && S.currentSessionId) await loadFlow(S.currentSessionId);
    } catch (_) {
      // The next event or the fallback poll can recover transient failures.
    } finally {
      refresh.running = false;
      if (refresh.pending) {
        refresh.pending = false;
        queueChannelRefresh(kind);
      }
    }
  }, refresh.delay);
}

function queueVisibleRefresh() {
  queueChannelRefresh(S.view.name === "flow" ? "flow" : "home");
}

function closeChannelEvents() {
  if (channelEventRetry !== null) clearTimeout(channelEventRetry);
  channelEventRetry = null;
  const es = S.es;
  S.es = null;
  es?.close();
  for (const refresh of Object.values(channelRefresh)) {
    if (refresh.timer !== null) clearTimeout(refresh.timer);
    refresh.timer = null;
    refresh.pending = false;
  }
}

function connectSSE() {
  if (!window.EventSource || !isChannelVisible() || (S.es && S.es.readyState <= 1)) return;
  try {
    const es = new EventSource(`/channel/events?token=${encodeURIComponent(S.token)}`);
    S.es = es;
    es.onopen = () => {
      if (S.es !== es) return;
      setConn(true);
      queueVisibleRefresh();
    };
    es.onmessage = (ev) => {
      if (S.es !== es || !isChannelVisible()) return;
      try {
        const payload = JSON.parse(ev.data || "{}");
        const eventType = (payload.event && payload.event.type) || "";
        if (S.view.name === "flow") {
          if (!payload.session_id || payload.session_id === S.currentSessionId) queueChannelRefresh("flow");
          if (["start", "done", "failed", "cancelled", "created", "deleted"].includes(eventType)) queueChannelRefresh("home");
        } else {
          queueChannelRefresh("home");
        }
      } catch (e) {
        queueVisibleRefresh();
      }
    };
    es.onerror = () => {
      if (S.es === es && es.readyState === 2) {
        S.es = null;
        if (channelEventRetry !== null) clearTimeout(channelEventRetry);
        channelEventRetry = setTimeout(() => {
          channelEventRetry = null;
          connectSSE();
        }, 3000);
      }
    };
  } catch (e) {}
}

async function boot() {
  applyTheme(S.theme);
  if (!mobileEventsBound) {
    mobileEventsBound = true;
    bind();
    document.addEventListener("visibilitychange", () => {
      if (document.hidden) {
        closeChannelEvents();
      } else if (isChannelVisible()) {
        queueVisibleRefresh();
        connectSSE();
      }
    });
    setInterval(pollChannel, 2000);
  }
  await loadModels().catch(() => {});
  await loadHome().catch(() => {});
  if (S.view.name === "flow") queueVisibleRefresh();
  connectSSE();
}

function pollChannel() {
  if (!isChannelVisible() || S.sheetOpen ||
      !$("modalNew").classList.contains("hidden") || !$("artPreview").classList.contains("hidden")) return;
  const interval = sseAlive() ? 10000 : isAgentActive() ? 2000 : 6000;
  const now = Date.now();
  if (!S._lastPoll || now - S._lastPoll >= interval) {
    S._lastPoll = now;
    queueVisibleRefresh();
  }
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
