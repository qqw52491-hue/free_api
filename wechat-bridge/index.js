// SandwichAgent 微信桥 (wechat-bridge)
// - 通过 weixin-agent-sdk 连接微信 ClawBot 通道（个人微信扫码，长轮询，无需公网地址）
// - 将微信消息按路由表转译为本机 SandwichAgent 通道 API（默认 127.0.0.1:8790）：
//     ① 有挂起的 ask_human 提问 → 回复即答案
//     ② 「停止」→ 取消当前任务；「状态」→ 查看在途/队列/绑定；「结果」→ 回放上次结果
//     ③ 「项目」→ 项目列表；「模型」→ 模型列表；纯数字回复 → 作用于最近一次列表
//        （绑定项目 / 切换默认模型，均为隐式操作，无需专门绑定命令）
//     ④ 其余 → 新任务（绑定项目走项目工作区，未绑定走临时 scratch 工作区）；
//        会话忙时自动排队（上限 MAX_QUEUE），完成后自动开始下一个
// - 任务执行期间后台轮询运行状态：ask_human 阻塞即推送提问、每 heartbeat_minutes
//   分钟汇报一次进度、阻塞超 2 小时再次提醒、结束/失败/停止均主动回推；
//   完成时若会话有产物则连文件一起回推
// - 主人锁定：第一条消息的发送者自动成为主人；config.allowlist 可加白名单，其余拒绝
// - 首次联系自动推送帮助文案
// 鉴权：与通道 API 共用 channel-config.json 中的 token（X-Channel-Token）。

import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import QRCode from "qrcode";
import { isLoggedIn, login, logout, start } from "weixin-agent-sdk";

const BRIDGE_VERSION = "0.2.0";
const MAX_QUEUE = 5;
const NUDGE_MS = 2 * 60 * 60 * 1000; // ask_human 挂起 2 小时提醒一次
const MAX_ARTIFACT_BYTES = 25 * 1024 * 1024; // 微信文件回推上限
const MAX_RESULT_CHARS = 1200;

// ---------------------------------------------------------------- 配置与状态

const configPath =
  process.env.CHANNEL_CONFIG_PATH ||
  path.join(os.homedir(), ".config", "free-api-chat", "channel-config.json");

if (!fs.existsSync(configPath)) {
  console.error(
    `[bridge] 未找到通道配置：${configPath}\n` +
      "请先在桌面端「微信绑定」页面保存配置，或用 CHANNEL_CONFIG_PATH 指向配置文件。"
  );
  process.exit(1);
}

const config = JSON.parse(fs.readFileSync(configPath, "utf-8"));
const API_BASE = `http://127.0.0.1:${config.port || 8790}`;
const TOKEN = config.token;
const HEARTBEAT_MS = Math.max(1, config.heartbeat_minutes || 30) * 60 * 1000;
const POLL_INTERVAL_MS = 10_000;

const statePath = path.join(path.dirname(configPath), "wechat-bindings.json");
let state = { owner: null, senders: {}, task_counter: 0 };
if (fs.existsSync(statePath)) {
  try {
    const loaded = JSON.parse(fs.readFileSync(statePath, "utf-8"));
    state = { ...state, ...loaded };
  } catch (error) {
    console.error(`[bridge] 绑定状态文件解析失败，已重置: ${error}`);
  }
}
function saveState() {
  fs.writeFileSync(statePath, JSON.stringify(state, null, 2), "utf-8");
}

function senderOf(conversationId) {
  if (!state.senders[conversationId]) {
    state.senders[conversationId] = {
      seen: false,
      project_id: null,
      project_name: null,
      session_id: null,
    };
  }
  return state.senders[conversationId];
}

/** conversationId → { runId, num, status, lastHeartbeat, waitingSince, lastNudge } */
const activeRuns = new Map();
/** conversationId → { runId, num }：等待用户回复答案的任务 */
const pendingQuestions = new Map();
/** conversationId → { type: "project"|"model", items }：最近一次推送的编号列表 */
const lastSelections = new Map();
/** conversationId → [{ text, media }]：会话忙时的任务队列 */
const taskQueues = new Map();
/** conversationId → { num, text }：最近一次完成的任务结果 */
const lastResults = new Map();

let bot = null;
let accountId = null;
let loginStarted = false;
let loginError = null;
let abortController = null;
/** 登录代际：重新扫码/停止桥后递增，用于忽略过期的 login() 回调 */
let loginGeneration = 0;

// ---------------------------------------------------------------- 通道 API 客户端

async function api(method, apiPath, body) {
  const response = await fetch(`${API_BASE}${apiPath}`, {
    method,
    headers: {
      "x-channel-token": TOKEN,
      "content-type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const value = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(value.error || `通道 API ${response.status}`);
  }
  return value;
}

async function botSend(text) {
  if (!bot) return;
  try {
    await bot.sendMessage(text);
  } catch (error) {
    console.error(`[bridge] 主动推送失败（context_token 可能已过期）: ${error}`);
  }
}

/** 带文件回推的推送；失败降级为纯文本 */
async function botSendWithMedia(text, media) {
  if (!bot) return;
  try {
    await bot.sendMessage({ text, media });
  } catch (error) {
    console.error(`[bridge] 文件推送失败，降级为文本: ${error}`);
    await botSend(text);
  }
}

// ---------------------------------------------------------------- 登录与二维码

// SDK 会把最新二维码链接写进自己的日志（<tmpdir>/openclaw/openclaw-YYYY-MM-DD.log，
// JSON lines，消息字段 "1" 含「二维码链接: <url>」），桥从这里截取二维码内容并渲染为 PNG data URL。
function sdkLogFiles() {
  const dir = path.join(os.tmpdir(), "openclaw");
  const names = [];
  for (const offset of [0, 1, 2]) {
    const day = new Date(Date.now() - offset * 86_400_000);
    const iso = day.toISOString().slice(0, 10);
    names.push(path.join(dir, `openclaw-${iso}.log`));
  }
  return names;
}

function latestQrUrl() {
  for (const file of sdkLogFiles()) {
    if (!fs.existsSync(file)) continue;
    let url = null;
    for (const line of fs.readFileSync(file, "utf-8").split("\n")) {
      if (!line.includes("二维码链接")) continue;
      try {
        const entry = JSON.parse(line);
        const message = entry["1"] || entry.message || entry.msg || "";
        const index = message.indexOf("二维码链接:");
        if (index >= 0) {
          url = message.slice(index + "二维码链接:".length).trim();
        }
      } catch {
        const index = line.indexOf("二维码链接:");
        if (index >= 0) url = line.slice(index + "二维码链接:".length).trim();
      }
    }
    if (url) return url;
  }
  return null;
}

function startBot() {
  if (bot) return;
  abortController = new AbortController();
  bot = start(agent, { abortSignal: abortController.signal });
  bot
    .wait()
    .then(() => console.log("[bridge] 微信监听已停止"))
    .catch((error) => console.error(`[bridge] 微信监听异常退出: ${error?.message || error}`));
}

async function stopMonitor() {
  if (abortController) abortController.abort();
  bot = null;
}

async function beginLogin() {
  const generation = loginGeneration;
  if (bot || isLoggedIn()) {
    startBot();
    return { state: "logged_in" };
  }
  if (loginStarted) return { state: "login_pending" };
  loginStarted = true;
  loginError = null;
  login({ log: (line) => console.log(`[sdk] ${line}`) })
    .then(async (id) => {
      if (generation !== loginGeneration) {
        console.log("[bridge] 已忽略过期的登录回调（期间发生了重新扫码或停止）");
        return;
      }
      accountId = id;
      loginStarted = false;
      console.log(`[bridge] 微信登录成功: ${id}`);
      startBot();
    })
    .catch((error) => {
      if (generation !== loginGeneration) return;
      loginStarted = false;
      loginError = String(error?.message || error);
      console.error(`[bridge] 微信登录失败: ${loginError}`);
    });
  return { state: "login_pending" };
}

/** 一键重新扫码（切换账号）：停监听 → 清凭据 → 直接发起新登录，桥进程不退出 */
async function relogin() {
  await stopMonitor();
  logout({ log: () => {} });
  accountId = null;
  loginStarted = false;
  loginError = null;
  loginGeneration += 1;
  console.log("[bridge] 已清除当前微信绑定，开始新的扫码登录");
  return beginLogin();
}

// ---------------------------------------------------------------- 任务提交与监听

/** 微信媒体文件登记为项目受管资源，返回拼进 goal 的说明文本 */
async function buildGoalWithMedia(sessionId, text, media) {
  if (!media) return text;
  try {
    const value = await api("POST", `/channel/session/${sessionId}/attachment`, {
      path: media.filePath,
      original_name: media.fileName || media.mimeType,
    });
    return `${text}\n\n[用户发来附件] ${media.fileName || media.mimeType} 已登记为受管资源 resource_id=${value.resource_id}，可用 project_resources.read_resource 读取。`;
  } catch (error) {
    return `${text}\n\n[用户发来附件] ${media.fileName || media.mimeType} path=${media.filePath}（登记失败：${error.message}）`;
  }
}

async function submitTask(conversationId, goalText) {
  const sender = senderOf(conversationId);
  let sessionId = sender.session_id;
  if (!sessionId) {
    const session = await api("POST", "/channel/session", { title: "微信 · 临时" });
    sender.session_id = session.session_id;
    saveState();
    sessionId = session.session_id;
  }
  const task = await api("POST", "/channel/task", { session_id: sessionId, goal: goalText });
  state.task_counter = (state.task_counter || 0) + 1;
  saveState();
  const num = state.task_counter;
  watchRun(conversationId, task.run_id, num);
  return num;
}

/** 当前任务结束后取队列中的下一个（如有） */
async function drainQueue(conversationId) {
  if (activeRuns.has(conversationId)) return;
  const queue = taskQueues.get(conversationId);
  if (!queue || !queue.length) return;
  const next = queue.shift();
  if (!queue.length) taskQueues.delete(conversationId);
  try {
    const sender = senderOf(conversationId);
    let sessionId = sender.session_id;
    if (!sessionId) {
      const session = await api("POST", "/channel/session", { title: "微信 · 临时" });
      sender.session_id = session.session_id;
      saveState();
      sessionId = session.session_id;
    }
    const goalText = await buildGoalWithMedia(sessionId, next.text, next.media);
    const num = await submitTask(conversationId, goalText);
    await botSend(`🚀 排队任务 #${num} 已开始。`);
  } catch (error) {
    await botSend(`⚠️ 排队任务启动失败：${error.message}`);
  }
}

function watchRun(conversationId, runId, num) {
  const entry = {
    runId,
    num,
    status: "running",
    lastHeartbeat: Date.now(),
    waitingSince: null,
    lastNudge: 0,
  };
  activeRuns.set(conversationId, entry);
  const timer = setInterval(async () => {
    if (!activeRuns.has(conversationId)) {
      clearInterval(timer);
      return;
    }
    let run;
    try {
      run = await api("GET", `/channel/run/${runId}`);
    } catch {
      return; // 通道 API 暂不可达，下一轮重试
    }
    if (run.status === "waiting_human") {
      if (entry.status !== "waiting_human") {
        entry.status = "waiting_human";
        entry.waitingSince = Date.now();
        entry.lastNudge = Date.now();
        pendingQuestions.set(conversationId, { runId, num });
        await botSend(
          `❓ 任务 #${num} 需要你的决定：\n${run.paused_reason || "Agent 请求协助。"}\n\n直接回复即算回答，发送「停止」可中断。`
        );
      } else if (Date.now() - entry.lastNudge >= NUDGE_MS) {
        entry.lastNudge = Date.now();
        const waitedMinutes = Math.round((Date.now() - entry.waitingSince) / 60000);
        await botSend(
          `⏳ 任务 #${num} 已等你 ${waitedMinutes} 分钟：\n${run.paused_reason || ""}\n直接回复即算回答，或发送「停止」。`
        );
      }
      return;
    }
    if (run.status === "running") {
      entry.status = "running";
      if (Date.now() - entry.lastHeartbeat >= HEARTBEAT_MS) {
        entry.lastHeartbeat = Date.now();
        await botSend(
          `⏳ 任务 #${num} 进行中（已 ${run.event_count} 个动作）。\n${run.last_progress || "正在继续执行……"}`
        );
      }
      return;
    }
    // 终态
    clearInterval(timer);
    activeRuns.delete(conversationId);
    pendingQuestions.delete(conversationId);
    if (run.status === "done") {
      lastResults.set(conversationId, { num, text: run.result || "" });
      const artifact = await latestArtifact(run.session_id);
      const finishText = run.result
        ? `🏁 任务 #${num} 完成：\n\n${run.result}`
        : `🏁 任务 #${num} 完成（无文本产出，详情见桌面端会话）。`;
      if (artifact && artifact.size_bytes > 0 && artifact.size_bytes <= MAX_ARTIFACT_BYTES) {
        const mediaType = String(artifact.mime_type || "").startsWith("image/") ? "image" : "file";
        await botSendWithMedia(finishText, {
          type: mediaType,
          url: artifact.path,
          fileName: artifact.display_name,
        });
      } else {
        await botSend(finishText);
      }
    } else if (run.status === "failed") {
      await botSend(`❌ 任务 #${num} 失败：\n${run.error || "未知错误"}`);
    } else if (run.status === "cancelled") {
      await botSend(`🛑 任务 #${num} 已停止。`);
    }
    await drainQueue(conversationId);
  }, POLL_INTERVAL_MS);
}

async function latestArtifact(sessionId) {
  if (!sessionId) return null;
  try {
    const value = await api("GET", `/channel/session/${sessionId}/artifacts`);
    return value.artifacts && value.artifacts[0];
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------- Agent 路由表

const agent = {
  async chat(request) {
    const conversationId = request.conversationId;
    const text = (request.text || "").trim();
    const sender = senderOf(conversationId);

    // 主人锁定 + 白名单：第一条消息的发送者成为主人，其余（未在 allowlist 中）拒绝
    if (!state.owner) {
      state.owner = conversationId;
      saveState();
      console.log(`[bridge] 主人已锁定: ${conversationId}`);
    }
    const allowlist = Array.isArray(config.allowlist) ? config.allowlist : [];
    if (conversationId !== state.owner && !allowlist.includes(conversationId)) {
      console.log(`[bridge] 已拒绝非白名单发送者: ${conversationId}`);
      return { text: "🔒 该机器人已绑定其他微信账号。" };
    }

    // 首次联系：推送帮助
    if (!sender.seen) {
      sender.seen = true;
      saveState();
      await botSend(config.help_text || "SandwichAgent 微信通道已就绪。");
    }

    // ① 挂起的 ask_human 提问：这条消息就是答案
    const pending = pendingQuestions.get(conversationId);
    if (pending) {
      pendingQuestions.delete(conversationId);
      try {
        await api("POST", `/channel/run/${pending.runId}/answer`, { text });
        return { text: "✅ 已收到，任务继续执行。" };
      } catch (error) {
        return { text: `⚠️ 回答送达失败：${error.message}\n任务可能已结束，请重新派发。` };
      }
    }

    // ② 停止当前任务
    if (/^(停止|取消|stop)$/i.test(text)) {
      const run = activeRuns.get(conversationId);
      if (!run) return { text: "当前没有进行中的任务。" };
      try {
        await api("POST", `/channel/run/${run.runId}/cancel`);
        return { text: "🛑 停止请求已发送，将在最近的安全边界停下。" };
      } catch (error) {
        return { text: `⚠️ 停止失败：${error.message}` };
      }
    }

    // ③ 状态查询
    if (/^(状态|\/status)$/i.test(text)) {
      const lines = [];
      const run = activeRuns.get(conversationId);
      if (run) {
        lines.push(
          run.status === "waiting_human"
            ? `▶ 任务 #${run.num}：等待你回答`
            : `▶ 任务 #${run.num}：执行中`
        );
        if (run.paused_reason) lines.push(`  ❓ ${run.paused_reason}`);
      } else {
        lines.push("▶ 当前无进行中任务");
      }
      const queue = taskQueues.get(conversationId);
      if (queue && queue.length) lines.push(`⏳ 排队中：${queue.length} 个任务`);
      lines.push(`📦 绑定：${sender.project_name || "未绑定（临时工作区）"}`);
      return { text: lines.join("\n") };
    }

    // ④ 上次结果回放
    if (/^(结果|\/result)$/i.test(text)) {
      const last = lastResults.get(conversationId);
      if (!last) return { text: "还没有已完成的任务。" };
      const body = last.text || "（无文本产出）";
      return { text: `📄 任务 #${last.num} 的结果：\n\n${body.slice(0, MAX_RESULT_CHARS)}` };
    }

    // ⑤ 项目列表（编号列表上下文 = project）
    if (/^(项目|项目列表|绑定)$/.test(text)) {
      try {
        const { projects } = await api("GET", "/channel/projects");
        if (!projects.length) {
          return { text: "还没有可用项目。请先在桌面端创建项目，再发送「项目」。" };
        }
        lastSelections.set(conversationId, { type: "project", items: projects });
        return {
          text:
            "回复编号绑定对应项目（绑定后任务将直接在该项目目录执行）：\n" +
            projects.map((p, i) => `${i + 1}. ${p.name}`).join("\n") +
            "\n\n发送「解绑」可回到临时工作区。",
        };
      } catch (error) {
        return { text: `⚠️ 获取项目列表失败：${error.message}` };
      }
    }

    // ⑥ 模型列表（编号列表上下文 = model）
    if (/^(模型|\/model)$/i.test(text)) {
      try {
        const { models } = await api("GET", "/channel/models");
        if (!models || !models.length) {
          return { text: "没有可用模型。请先在桌面端 LLM 管理里添加。" };
        }
        lastSelections.set(conversationId, { type: "model", items: models });
        return {
          text:
            "回复编号切换默认模型（对之后新建的会话生效）：\n" +
            models
              .map((m, i) => `${i + 1}. ${m.display_name || m.name}（${m.platform_name}）`)
              .join("\n"),
        };
      } catch (error) {
        return { text: `⚠️ 获取模型列表失败：${error.message}` };
      }
    }

    // ⑦ 纯数字回复 → 作用于最近一次编号列表
    const pick = text.match(/^(\d{1,3})$/);
    if (pick && lastSelections.has(conversationId)) {
      const selection = lastSelections.get(conversationId);
      const chosen = selection.items[parseInt(pick[1], 10) - 1];
      if (!chosen) return { text: "编号无效，请回复列表中的数字。" };
      try {
        if (selection.type === "project") {
          const session = await api("POST", "/channel/session", {
            project_id: chosen.id,
            title: `微信 · ${chosen.name}`,
          });
          sender.project_id = chosen.id;
          sender.project_name = chosen.name;
          sender.session_id = session.session_id;
          saveState();
          lastSelections.delete(conversationId);
          return {
            text: `✅ 已绑定：${chosen.name}\n后续消息将在该项目内执行；发送「项目」可切换，发送「解绑」回到临时工作区。`,
          };
        }
        await api("PUT", "/channel/config", { model_id: chosen.id });
        lastSelections.delete(conversationId);
        return {
          text: `✅ 默认模型已切换为：${chosen.display_name || chosen.name}\n对之后新建的会话生效。`,
        };
      } catch (error) {
        return { text: `⚠️ 操作失败：${error.message}` };
      }
    }

    // ⑧ 解绑
    if (/^解绑$/.test(text)) {
      sender.project_id = null;
      sender.project_name = null;
      sender.session_id = null;
      saveState();
      return { text: "🔓 已解绑。后续任务将在临时工作区执行；发送「项目」可重新绑定。" };
    }

    // ⑨ 新任务（会话忙则排队）
    if (!text && !request.media) return { text: "请发送任务内容。" };
    if (activeRuns.has(conversationId)) {
      const queue = taskQueues.get(conversationId) || [];
      if (queue.length >= MAX_QUEUE) {
        return { text: `⏳ 排队已满（上限 ${MAX_QUEUE}）。可发送「状态」查看进度，或「停止」当前任务。` };
      }
      queue.push({ text, media: request.media });
      taskQueues.set(conversationId, queue);
      return { text: `⏳ 当前有任务在执行，已加入队列（第 ${queue.length} 位），完成后自动开始。` };
    }
    try {
      const senderState = senderOf(conversationId);
      let sessionId = senderState.session_id;
      if (!sessionId) {
        const session = await api("POST", "/channel/session", { title: "微信 · 临时" });
        senderState.session_id = session.session_id;
        saveState();
        sessionId = session.session_id;
      }
      const goalText = await buildGoalWithMedia(sessionId, text, request.media);
      const num = await submitTask(conversationId, goalText);
      const scope = sender.project_name ? `项目：${sender.project_name}` : "临时工作区";
      const retries = task.llm_retries != null ? `，重试上限 ${task.llm_retries}` : "";
      return {
        text: `🚀 任务 #${num} 已开始（${scope}${retries}）。\n每 ${Math.round(HEARTBEAT_MS / 60000)} 分钟汇报进度；发送「停止」中断；卡住时我会直接提问，回复即算回答。`,
      };
    } catch (error) {
      if (String(error.message).includes("在途任务")) {
        return { text: "⏳ 已有任务在执行中，已自动转入队列逻辑，请稍候。" };
      }
      return { text: `⚠️ 任务派发失败：${error.message}` };
    }
  },
  clearSession(conversationId) {
    const sender = senderOf(conversationId);
    sender.session_id = null;
    saveState();
  },
};

// ---------------------------------------------------------------- 桥 HTTP 服务（供桌面端页面 / 调度器调用）

function authorized(req) {
  return req.headers["x-channel-token"] === TOKEN;
}

function sendJson(res, status, value) {
  const body = JSON.stringify(value);
  res.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(body),
  });
  res.end(body);
}

async function readJson(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  if (!chunks.length) return {};
  return JSON.parse(Buffer.concat(chunks).toString("utf-8"));
}

const server = http.createServer(async (req, res) => {
  if (!authorized(req)) {
    sendJson(res, 401, { error: "缺少或无效的 X-Channel-Token" });
    return;
  }
  try {
    if (req.method === "GET" && req.url === "/bridge/status") {
      sendJson(res, 200, {
        running: true,
        logged_in: Boolean(bot) || isLoggedIn(),
        account_id: accountId,
        active_runs: activeRuns.size,
        pending_questions: pendingQuestions.size,
        version: BRIDGE_VERSION,
      });
      return;
    }
    if (req.method === "POST" && req.url === "/bridge/login/start") {
      sendJson(res, 200, await beginLogin());
      return;
    }
    if (req.method === "POST" && req.url === "/bridge/relogin") {
      sendJson(res, 200, await relogin());
      return;
    }
    if (req.method === "GET" && req.url === "/bridge/qr") {
      const state = bot || isLoggedIn() ? "logged_in" : loginStarted ? "login_pending" : "idle";
      const url = latestQrUrl();
      const qrDataUrl = state !== "logged_in" && url ? await QRCode.toDataURL(url) : null;
      sendJson(res, 200, {
        state,
        qr_data_url: qrDataUrl,
        error: loginError,
        hint: url ? null : "尚未获取到二维码，请稍候或在桌面端重新点击「扫码登录」。",
      });
      return;
    }
    if (req.method === "POST" && req.url === "/bridge/notify") {
      // 主动推送（定时任务结果等）；仅推送给主人
      const body = await readJson(req);
      if (!body.text) {
        sendJson(res, 400, { error: "缺少 text" });
        return;
      }
      if (!bot) {
        sendJson(res, 409, { error: "微信未登录，推送未送达" });
        return;
      }
      await botSend(String(body.text));
      sendJson(res, 200, { ok: true });
      return;
    }
    if (req.method === "POST" && req.url === "/bridge/stop") {
      await stopMonitor();
      loginGeneration += 1; // 使在途登录回调失效
      logout({ log: () => {} });
      sendJson(res, 200, { ok: true });
      return;
    }
    sendJson(res, 404, { error: "not found" });
  } catch (error) {
    sendJson(res, 500, { error: String(error?.message || error) });
  }
});

// 未读取请求体时避免悬挂：忽略所有请求体内容
server.on("clientError", (_err, socket) => socket.end("HTTP/1.1 400 Bad Request\r\n\r\n"));

// 端口被占等监听失败：给出人话并退出，避免甩未捕获堆栈
server.on("error", (error) => {
  if (error && error.code === "EADDRINUSE") {
    console.error(
      `[bridge] 监听 127.0.0.1:${config.bridge_port || 8791} 失败：端口已被占用。\n` +
        "通常说明已有一个桥实例在运行，本实例直接退出即可。"
    );
  } else {
    console.error(`[bridge] 桥服务监听失败: ${error?.message || error}`);
  }
  process.exit(1);
});

server.listen(config.bridge_port || 8791, "127.0.0.1", () => {
  console.log(`[bridge] 微信桥已就绪 http://127.0.0.1:${config.bridge_port || 8791}`);
  if (isLoggedIn()) {
    console.log("[bridge] 检测到已保存的微信凭据，自动恢复监听");
    startBot();
  } else {
    console.log("[bridge] 尚未登录微信，请在桌面端「微信绑定」页面扫码");
  }
});
