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
import { randomUUID } from "node:crypto";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import QRCode from "qrcode";
import { createMessageDelivery } from "./message-delivery.js";
import { restoreRuntime, saveBindings, readRun } from "./runtime-state.js";
import { isLoggedIn, login, logout, start } from "weixin-agent-sdk";

const BRIDGE_VERSION = "0.3.1";

// 内置指令面板：随桥能力同步更新，首次联系与「帮助」指令时推送
const COMMAND_PANEL = [
  "📋 指令面板",
  "──────────────",
  "🚀 直接发消息 = 派任务",
  "　支持文字/语音/图片/Excel/PDF；执行中再发会自动排队",
  "📊 状态 = 当前进度 + 队列 + 绑定项目",
  "💬 补充：内容 = 补充当前任务，不排队、不打断操作",
  "🗣 回答：内容 = 回答 Agent 的提问（也可直接回复）",
  "▶️ 继续队列 = 恢复暂停的排队任务",
  "⏭ 跳过队首 = 核对桌面记录后，移除未确认的队首任务",
  "📄 结果 = 回放上一次任务的结果",
  "📎 文件 = 重发上一次任务生成的小文件（每个不超过 25MB）",
  "📁 项目 = 项目列表（回复编号绑定工作区）",
  "🤖 模型 = 模型列表（回复编号切换默认模型）",
  "🆕 新会话 = 重开一个干净会话（上下文清零）",
  "🔓 解绑 = 退出项目，回到临时工作区",
  "🛑 停止 = 中断当前任务",
  "🗑️ 清空队列 = 丢弃排队的任务",
  "❓ 帮助 = 随时调出本面板",
  "🎚 详细汇报 / 安静汇报 = 一键切换详略",
  "🎚 动作汇报 N = 每 N 个动作汇报（0 关闭）",
  "──────────────",
  "💬 Agent 提问时，直接回复即算回答；状态、结果等指令仍可用",
  "🔄 桥重启后恢复监听；执行状态不明时暂停队列，不自动重跑",
  "⏱ 进度按配置间隔自动汇报，卡住会主动提问",
  "📁 绑定项目后任务在项目目录执行，未绑定在临时工作区",
].join("\n");

// 夜间静默：该时段内静默心跳与等待提醒（完成任务/失败/提问等关键事件照常推送）
const QUIET_DEFAULT = { start: "23:00", end: "08:00" };
function inQuietHours(now = new Date()) {
  const parse = (value) => {
    const [h, m] = String(value || "").split(":").map(Number);
    return Number.isFinite(h) ? h * 60 + (m || 0) : null;
  };
  const start = parse(config.quiet_start ?? QUIET_DEFAULT.start);
  const end = parse(config.quiet_end ?? QUIET_DEFAULT.end);
  if (start === null || end === null) return false;
  const minutes = now.getHours() * 60 + now.getMinutes();
  return start <= end ? minutes >= start && minutes < end : minutes >= start || minutes < end;
}
const MAX_QUEUE = 5;
const NUDGE_MS = 2 * 60 * 60 * 1000; // ask_human 挂起 2 小时提醒一次
const MAX_ARTIFACT_BYTES = 25 * 1024 * 1024; // 微信文件回推上限

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
  saveBindings(statePath, state, runtime);
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

// 运行快照与绑定一起保存；区分已确认的运行与尚未拿到派发回执的队首。
const runtime = restoreRuntime(state.runtime);
const { activeRuns, pendingQuestions, taskQueues, lastResults, queuePauses } = runtime;
const lastSelections = new Map();
const submitting = new Set();
const answering = new Set();
let runtimeResumed = false;

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
    signal: AbortSignal.timeout(20_000),
    headers: {
      "x-channel-token": TOKEN,
      "content-type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const value = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(value.error || `通道 API ${response.status}`);
    error.status = response.status;
    throw error;
  }
  return value;
}

const deliverMessage = createMessageDelivery(async (message) => {
  if (!bot) throw new Error("微信 bot 未就绪");
  await bot.sendMessage(message);
}, {
  onMediaError: (error) => console.error(`[bridge] 文件推送失败，降级为分段文本: ${error}`),
});

async function botSend(text) {
  if (!bot) {
    console.log("[bridge] botSend 跳过：bot 实例未就绪");
    return false;
  }
  try {
    await deliverMessage(text);
    console.log(`[bridge] 主动推送已完整下发（${text.length} 字符）`);
    return true;
  } catch (error) {
    console.error(`[bridge] 主动推送未完成（未自动重试）: ${error}`);
    return false;
  }
}

/** 带文件回推的推送；附件失败时文本仍按同一规则分段发送。 */
async function botSendWithMedia(text, media) {
  try {
    const delivery = await deliverMessage(text, media);
    if (!delivery.mediaDelivered) {
      await botSend(`⚠️ 文件「${media.fileName || "附件"}」未确认送达。可发送「文件」手动重取，或在桌面端获取。`);
      return false;
    }
    console.log(`[bridge] 主动文件推送已完成: ${media.fileName || media.url}`);
    return true;
  } catch (error) {
    console.error(`[bridge] 主动文件推送未完成（未自动重试）: ${error}`);
    await botSend(`⚠️ 文件「${media.fileName || "附件"}」未确认送达。可发送「文件」手动重取，或在桌面端获取。`);
    return false;
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
  if (!runtimeResumed) {
    runtimeResumed = true;
    for (const [conversationId, entry] of activeRuns) {
      watchRun(conversationId, entry.runId, entry.num, entry);
    }
    for (const conversationId of taskQueues.keys()) {
      void drainQueue(conversationId).catch((error) => console.error(`[bridge] 恢复队列失败: ${error}`));
    }
  }
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
  const name = media.fileName || path.basename(media.filePath || "") || "attachment";
  try {
    const value = await api("POST", `/channel/session/${sessionId}/attachment`, {
      path: media.filePath,
      original_name: name,
    });
    return `${text}\n\n[用户发来附件] ${name} 已登记为受管资源 resource_id=${value.resource_id}，可用 project_resources.read_resource 读取。`;
  } catch (error) {
    throw new Error(`附件「${name}」接收失败：${error.message}`);
  }
}

async function submitTask(conversationId, goalText, queueId) {
  const sender = senderOf(conversationId);
  let sessionId = sender.session_id;
  if (!sessionId) {
    const session = await api("POST", "/channel/session", {
      project_id: sender.project_id || undefined,
      title: sender.project_name ? `微信 · ${sender.project_name}` : "微信 · 临时",
    });
    sender.session_id = session.session_id;
    saveState();
    sessionId = session.session_id;
  }
  let task;
  try {
    task = await api("POST", "/channel/task", { session_id: sessionId, goal: goalText, caller_notifies: true });
  } catch (error) {
    if (error.status !== 409 && (
      String(error.message).includes("会话不存在") ||
      String(error.message).includes("session expired")
    )) {
      console.log(`[bridge] 会话 ${sessionId} 失效/异常，尝试自动重建并重试派发任务...`);
      const session = await api("POST", "/channel/session", {
        project_id: sender.project_id || undefined,
        title: sender.project_name ? `微信 · ${sender.project_name}` : "微信 · 临时",
      });
      sender.session_id = session.session_id;
      saveState();
      sessionId = session.session_id;
      task = await api("POST", "/channel/task", { session_id: sessionId, goal: goalText, caller_notifies: true });
    } else {
      throw error;
    }
  }
  state.task_counter = (state.task_counter || 0) + 1;
  const num = state.task_counter;
  watchRun(conversationId, task.run_id, num, { sessionId, queueId });
  return { num, llmRetries: task.llm_retries ?? null };
}

/** 先保存派发中的队首，再调用 API；缺少回执时不自动重发。 */
async function drainQueue(conversationId, announce = true) {
  if (activeRuns.has(conversationId) || submitting.has(conversationId) || queuePauses.has(conversationId)) return;
  const queue = taskQueues.get(conversationId);
  if (!queue?.length) return;
  const next = queue[0];
  if (next.dispatching) {
    queuePauses.set(conversationId, "队首任务的派发回执未确认，请先在桌面端核对；不会自动重发。确认后可「跳过队首」，再「继续队列」。");
    saveState();
    return;
  }
  submitting.add(conversationId);
  try {
    // 尚未发起任务前的附件登记失败不会冒充任务已经启动。
    const sender = senderOf(conversationId);
    if (!sender.session_id) {
      const session = await api("POST", "/channel/session", {
        project_id: sender.project_id || undefined,
        title: sender.project_name ? `微信 · ${sender.project_name}` : "微信 · 临时",
      });
      sender.session_id = session.session_id;
      saveState();
    }
    const goalText = await buildGoalWithMedia(sender.session_id, next.text, next.media);
    next.dispatching = true;
    saveState();
    const task = await submitTask(conversationId, goalText, next.id);
    if (announce) await botSend(`🚀 排队任务 #${task.num} 已开始。`);
    return task;
  } catch (error) {
    const reason = next.dispatching
      ? `队首派发未确认：${error.message}。请先在桌面端核对，不会自动重发；确认后可「跳过队首」，再「继续队列」。`
      : `队首启动失败：${error.message}。内容已保留，可发送「继续队列」重试。`;
    queuePauses.set(conversationId, reason);
    saveState();
    if (announce) await botSend(`⚠️ ${reason}`);
    else throw new Error(reason);
  } finally {
    submitting.delete(conversationId);
  }
}

function watchRun(conversationId, runId, num, restored = {}) {
  const entry = {
    runId,
    num,
    status: "running",
    lastHeartbeat: Date.now(),
    waitingSince: null,
    lastNudge: 0,
    phase: null,
    stepMilestone: 0,
    reportEvery: null,
    phasePush: null,
    ...restored,
    runId, num,
  };
  activeRuns.set(conversationId, entry);
  // 运行已确认时，清掉同一队首的派发标记；恢复时不会重复执行它。
  const queue = taskQueues.get(conversationId);
  if (entry.queueId && queue?.[0]?.id === entry.queueId) {
    queue.shift();
    if (!queue.length) taskQueues.delete(conversationId);
  }
  saveState();
  let polling = false;
  const timer = setInterval(async () => {
    // 网络/分段发送慢于轮询间隔时，不并发处理同一运行的终态。
    if (polling || !bot) return;
    polling = true;
    try {
      if (activeRuns.get(conversationId) !== entry) {
        clearInterval(timer);
        return;
      }
      let run;
      const controlRevision = entry.controlRevision || 0;
      try {
        run = await readRun(api, entry);
        if (activeRuns.get(conversationId) !== entry) {
          clearInterval(timer);
          return;
        }
        // 查询发出后回答已送达时，忽略旧快照，避免重新挂回刚回答过的问题。
        if ((entry.controlRevision || 0) !== controlRevision) return;
        entry.failCount = 0;
        entry.offlineNotified = false;
      } catch (error) {
        if (activeRuns.get(conversationId) !== entry) {
          clearInterval(timer);
          return;
        }
        entry.failCount = (entry.failCount || 0) + 1;
        if (entry.failCount >= 60) {
          // 通道约 10 分钟不可达：只告警一次，保留监听，避免误报任务已结束
          if (!entry.offlineNotified) {
            entry.offlineNotified = true;
            saveState();
            await botSend(`⚠️ 通道长时间不可达，任务 #${num} 的状态暂未确认；已保留监听和排队任务，连接恢复后继续核对。`);
          }
        }
        return;
      }
      if (run.status === "waiting_human") {
        if (entry.status !== "waiting_human") {
          entry.status = "waiting_human";
          entry.paused_reason = run.paused_reason;
          entry.waitingSince = Date.now();
          entry.lastNudge = Date.now();
          lastSelections.delete(conversationId);
          pendingQuestions.set(conversationId, { runId, num });
          saveState();
          await botSend(
            `❓ 任务 #${num} 需要你的决定：\n${run.paused_reason || "Agent 请求协助。"}\n\n直接回复即算回答，发送「停止」可中断。`
          );
        } else if (Date.now() - entry.lastNudge >= NUDGE_MS) {
          entry.lastNudge = Date.now();
          saveState();
          if (inQuietHours()) return; // 夜间静默：提醒推迟到白天
          const waitedMinutes = Math.round((Date.now() - entry.waitingSince) / 60000);
          await botSend(
            `⏳ 任务 #${num} 已等你 ${waitedMinutes} 分钟：\n${run.paused_reason || ""}\n直接回复即算回答，或发送「停止」。`
          );
        }
        return;
      }
      if (run.status === "running") {
        const notifications = [];
        entry.status = "running";
        entry.paused_reason = null;
        pendingQuestions.delete(conversationId);
        // 阶段里程碑：环境就绪/失败恢复/外部救援/开始交付时即时汇报（可按会话/任务关闭）
        const phasePush = entry.phasePush ?? run.phase_push ?? true;
        if (phasePush && run.phase && run.phase !== entry.phase) {
          entry.phase = run.phase;
          if (!inQuietHours()) {
            notifications.push(`▸ 任务 #${num}：${run.phase}`);
          }
        }
        // 动作里程碑：按任务快照步长（可被会话覆盖）汇报正在干啥
        const every = entry.reportEvery ?? run.report_every_actions ?? 15;
        const stepMilestone = every > 0 ? Math.floor((run.event_count || 0) / every) : 0;
        if (stepMilestone > 0 && stepMilestone > entry.stepMilestone) {
          entry.stepMilestone = stepMilestone;
          if (!inQuietHours() && run.last_progress) {
            notifications.push(
              `▸ 任务 #${num} 已执行 ${run.event_count} 个动作：${run.last_progress}`
            );
          }
        }
        if (Date.now() - entry.lastHeartbeat >= HEARTBEAT_MS) {
          entry.lastHeartbeat = Date.now();
          if (!inQuietHours()) {
            notifications.push(
              `⏳ 任务 #${num} 进行中（已 ${run.event_count} 个动作）。\n${run.last_progress || "正在继续执行……"}`
            );
          }
        }
        saveState();
        for (const notification of notifications) await botSend(notification);
        return;
      }
      if (!["done", "failed", "cancelled", "unknown"].includes(run.status)) return;
      // 先保存终态和完整正文，再发送；发送中断后可用「结果」显式重取，不自动重播。
      if (run.status === "done") lastResults.set(conversationId, { num, text: run.result || "", runId, sessionId: run.session_id || entry.sessionId });
      if (run.status === "unknown") queuePauses.set(conversationId, run.error);
      activeRuns.delete(conversationId);
      const previousQuestion = pendingQuestions.get(conversationId);
      pendingQuestions.delete(conversationId);
      try { saveState(); } catch (error) {
        activeRuns.set(conversationId, entry);
        if (previousQuestion) pendingQuestions.set(conversationId, previousQuestion);
        throw error;
      }
      clearInterval(timer);
      console.log(`[bridge] watchRun 检测到任务 #${num} 终态: ${run.status} (hasResult: ${Boolean(run.result)})`);
      if (run.status === "done") {
        const finishText = run.result
          ? `🏁 任务 #${num} 完成：\n\n${run.result}`
          : `🏁 任务 #${num} 完成（无文本产出，详情见桌面端会话）。`;
        await botSend(finishText);
        await sendRunFiles(run.session_id || entry.sessionId, runId, num);
      } else if (run.status === "failed") {
        await botSend(`❌ 任务 #${num} 失败：\n${run.error || "未知错误"}`);
      } else if (run.status === "cancelled") {
        await botSend(`🛑 任务 #${num} 已停止。`);
      } else if (run.status === "unknown") {
        await botSend(`⚠️ 任务 #${num}：${run.error}\n未自动重跑；排队任务已保留并暂停。请先在桌面端核对，再发送「继续队列」。`);
      }
      await drainQueue(conversationId);
    } catch (error) {
      console.error(`[bridge] 任务 #${num} 状态处理失败（未自动重发）: ${error}`);
    } finally {
      polling = false;
    }
  }, POLL_INTERVAL_MS);
}

async function sendRunFiles(sessionId, runId, num, requested = false) {
  if (!sessionId || !runId) {
    if (requested) await botSend("这条历史结果没有保存文件归属，请在桌面端获取文件。");
    return;
  }
  try {
    const value = await api("GET", `/channel/session/${sessionId}/artifacts?run_id=${encodeURIComponent(runId)}`);
    const artifacts = (value.artifacts || []).filter((file) => file.run_id === runId);
    if (!artifacts.length) {
      if (requested) await botSend(`任务 #${num} 暂无已登记文件，可在桌面端查看。`);
      return;
    }
    for (const file of artifacts) {
      if (!file.path || !Number.isFinite(file.size_bytes) || file.size_bytes <= 0 || file.size_bytes > MAX_ARTIFACT_BYTES) {
        await botSend(`📎「${file.display_name || "附件"}」未发送：${file.size_bytes > MAX_ARTIFACT_BYTES ? "超过 25MB" : "文件路径或大小不可用"}，请在桌面端获取。`);
        continue;
      }
      await botSendWithMedia(`📎 任务 #${num}：${file.display_name}`, {
        type: String(file.mime_type || "").startsWith("image/") ? "image" : "file",
        url: file.path, fileName: file.display_name,
      });
    }
    if (value.has_more) await botSend("本次仅回传前 5 个文件，其余文件请在桌面端获取。");
  } catch (error) {
    await botSend(`⚠️ 任务 #${num} 的文件获取未完成：${error.message}。可发送「文件」重试。`);
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

    // 显式回答可以使用“状态”等指令词作为答案，不受菜单优先级影响。
    if (/^回答[：:\s]/.test(text)) lastSelections.delete(conversationId);

    // 首次联系：推送帮助
    if (!sender.seen) {
      sender.seen = true;
      saveState();
      await botSend(`${config.help_text || "🤖 SandwichAgent 微信通道已就绪。"}\n\n${COMMAND_PANEL}`);
    }

    // ⓪ 元命令：停止 / 新会话（优先于答案路由，保证随时可用）
    if (/^(停止|取消|stop)$/i.test(text)) {
      const run = activeRuns.get(conversationId);
      if (!run) return { text: "当前没有进行中的任务。" };
      try {
        await api("POST", `/channel/run/${run.runId}/cancel`);
        return { text: "🛑 停止请求已发送，将在最近的安全边界停下。排队任务仍会继续；如需一并取消，请发送「清空队列」。" };
      } catch (error) {
        return { text: `⚠️ 停止失败：${error.message}` };
      }
    }
    if (/^(帮助|指令|\/help)$/i.test(text)) {
      return { text: COMMAND_PANEL };
    }
    if (/^(详细汇报|安静汇报)$/.test(text) || /^动作汇报\s*(\d{1,4})$/.test(text)) {
      const detailed = text === "详细汇报";
      const quiet = text === "安静汇报";
      const match = text.match(/^动作汇报\s*(\d{1,4})$/);
      const steps = match ? parseInt(match[1], 10) : detailed ? 15 : 0;
      const phasePush = quiet ? false : detailed ? true : undefined;
      // 立即作用于本会话在途任务
      const entry = activeRuns.get(conversationId);
      if (entry) {
        entry.reportEvery = steps;
        if (phasePush !== undefined) entry.phasePush = phasePush;
      }
      try {
        const payload = { action_milestone_steps: steps };
        if (phasePush !== undefined) payload.phase_pushes = phasePush;
        await api("PUT", "/channel/config", payload);
      } catch (error) {
        return { text: `⚠️ 已对本会话生效，但持久化失败：${error.message}` };
      }
      return {
        text: `🎚 汇报风格已切换：动作里程碑${steps > 0 ? `每 ${steps} 个动作` : "已关闭"}${
          phasePush === undefined ? "" : phasePush ? "，阶段推送开" : "，阶段推送关"
        }。立即生效，对之后的任务也保持。`,
      };
    }
    if (/^(清空队列|清空排队)$/i.test(text)) {
      const queue = taskQueues.get(conversationId);
      const count = queue ? queue.length : 0;
      if (submitting.has(conversationId)) return { text: "正在派发队首任务，请稍后再清空队列。" };
      if (queue?.[0]?.dispatching) return { text: "队首派发结果未确认，请先在桌面端核对；确认后发送「跳过队首」，再清空队列。" };
      taskQueues.delete(conversationId);
      queuePauses.delete(conversationId);
      saveState();
      return { text: count ? `🗑️ 已清空队列中的 ${count} 个任务。` : "队列为空，无需清空。" };
    }
    if (/^(新会话|新对话|\/new)$/i.test(text)) {
      if (activeRuns.has(conversationId) || submitting.has(conversationId) || taskQueues.get(conversationId)?.length) {
        return {
          text: "当前有执行中或排队的任务。请先结束当前任务并清空队列，再新开会话。",
        };
      }
      try {
        const session = sender.project_id
          ? await api("POST", "/channel/session", {
              project_id: sender.project_id,
              title: `微信 · ${sender.project_name}`,
            })
          : await api("POST", "/channel/session", { title: "微信 · 临时" });
        sender.session_id = session.session_id;
        lastResults.delete(conversationId);
        pendingQuestions.delete(conversationId);
        saveState();
        return {
          text: `✅ 已新开会话${sender.project_name ? `（项目：${sender.project_name}）` : "（临时工作区）"}。\n老会话的上下文不再跟随（历史仍在桌面端可查），直接说新需求即可。`,
        };
      } catch (error) {
        return { text: `⚠️ 新开会话失败：${error.message}` };
      }
    }

    // ③ 状态查询（在途任务实时拉取最新阶段明细）
    if (/^(状态|\/status)$/i.test(text)) {
      const lines = [];
      const entry = activeRuns.get(conversationId);
      if (entry) {
        lines.push(
          entry.status === "waiting_human"
            ? `▶ 任务 #${entry.num}：等待你回答`
            : `▶ 任务 #${entry.num}：执行中`
        );
        if (entry.paused_reason) lines.push(`  ❓ ${entry.paused_reason}`);
        try {
          const live = await readRun(api, entry);
          if (live.status === "waiting_human" && live.paused_reason) lines.push(`  ❓ ${live.paused_reason}`);
          if (live.state_detail) lines.push(`  ▸ 当前阶段：${live.state_detail}`);
          else if (live.last_progress) lines.push(`  ▸ ${live.last_progress}`);
          if (live.event_count) lines.push(`  已执行 ${live.event_count} 个动作`);
        } catch {
          /* 通道暂不可达时展示缓存信息即可 */
        }
      } else {
        lines.push("▶ 当前无进行中任务");
      }
      const queue = taskQueues.get(conversationId);
      if (queue && queue.length) {
        lines.push(`⏳ 排队中：${queue.length} 个任务`);
        queue.forEach((item, i) => lines.push(`  ${i + 1}. ${item.text?.slice(0, 60) || item.media?.fileName || "附件任务"}${item.dispatching ? "（派发回执待确认）" : ""}`));
      }
      if (queuePauses.has(conversationId)) lines.push(`⏸ 队列暂停：${queuePauses.get(conversationId)}`);
      lines.push(`📦 绑定：${sender.project_name || "未绑定（临时工作区）"}`);
      return { text: lines.join("\n") };
    }

    // ④ 上次结果回放
    if (/^(结果|\/result)$/i.test(text)) {
      const last = lastResults.get(conversationId);
      if (!last) return { text: "还没有已完成的任务。" };
      const body = last.text || "（无文本产出）";
      const delivered = await botSend(`📄 任务 #${last.num} 的结果：\n\n${body}`);
      return { text: delivered ? "" : "⚠️ 结果发送未完成，完整结果仍已保存；请稍后发送「结果」重试。" };
    }

    if (/^(文件|附件|\/files)$/i.test(text)) {
      const last = lastResults.get(conversationId);
      if (!last) return { text: "还没有已完成任务的文件。" };
      await sendRunFiles(last.sessionId, last.runId, last.num, true);
      return { text: "" };
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
          if (activeRuns.has(conversationId) || submitting.has(conversationId) || taskQueues.get(conversationId)?.length) {
            return { text: "当前有执行中或排队的任务，请先结束并清空队列，再切换项目。" };
          }
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
      if (activeRuns.has(conversationId) || submitting.has(conversationId) || taskQueues.get(conversationId)?.length) {
        return { text: "当前有执行中或排队的任务，请先结束并清空队列，再解绑项目。" };
      }
      sender.project_id = null;
      sender.project_name = null;
      sender.session_id = null;
      saveState();
      return { text: "🔓 已解绑。后续任务将在临时工作区执行；发送「项目」可重新绑定。" };
    }

    if (text === "继续队列") {
      if (activeRuns.has(conversationId) || submitting.has(conversationId)) return { text: "当前任务尚未结束，队列保持原位。" };
      const queue = taskQueues.get(conversationId);
      if (!queue?.length) return { text: "队列为空。" };
      if (queue[0].dispatching) return { text: "队首派发回执未确认，不会自动重发。请先在桌面端核对，确认后发送「跳过队首」。" };
      queuePauses.delete(conversationId);
      saveState();
      await drainQueue(conversationId);
      return { text: "" };
    }
    if (text === "跳过队首") {
      if (activeRuns.has(conversationId) || submitting.has(conversationId)) return { text: "当前任务尚未结束，请稍后操作队列。" };
      const queue = taskQueues.get(conversationId);
      if (!queue?.length) return { text: "队列为空。" };
      queue.shift();
      if (!queue.length) { taskQueues.delete(conversationId); queuePauses.delete(conversationId); }
      saveState();
      return { text: "已移除队首的排队记录（不会停止桌面端已启动的任务）。剩余任务可发送「继续队列」启动。" };
    }

    const supplement = text.match(/^补充[：:\s]([\s\S]*)$/);
    if (supplement || text === "补充") {
      const entry = activeRuns.get(conversationId);
      if (!entry) return { text: "当前没有进行中的任务，请直接发送新任务。" };
      const content = supplement?.[1]?.trim() || "";
      if (!content && !request.media) return { text: "请发送「补充：具体要求」。" };
      try {
        const body = await buildGoalWithMedia(entry.sessionId || sender.session_id, content, request.media);
        await api("POST", `/channel/run/${entry.runId}/supplement`, { text: body });
        return { text: `✅ 补充已送达任务 #${entry.num}，将在后续步骤处理；当前操作继续执行。` };
      } catch (error) {
        return { text: `⚠️ 补充未确认送达：${error.message}。未自动重发。` };
      }
    }

    // 查询/菜单/控制指令优先；只有剩余普通消息或显式「回答」才进入应答。
    const explicitAnswer = text.match(/^回答[：:\s]([\s\S]*)$/);
    const pending = pendingQuestions.get(conversationId);
    if (pending || explicitAnswer || text === "回答") {
      const entry = activeRuns.get(conversationId);
      const target = pending || (entry && { runId: entry.runId, num: entry.num });
      if (!target) return { text: "当前没有等待回答的任务。" };
      const content = explicitAnswer ? explicitAnswer[1].trim() : text === "回答" ? "" : text;
      if (!content && !request.media) return { text: "回答不能为空，请发送「回答：具体内容」。" };
      if (answering.has(conversationId)) return { text: "上一条回答正在送达，请稍候。" };
      answering.add(conversationId);
      try {
        const body = await buildGoalWithMedia(entry?.sessionId || sender.session_id, content, request.media);
        await api("POST", `/channel/run/${target.runId}/answer`, { text: body });
        if (pendingQuestions.get(conversationId) === pending) pendingQuestions.delete(conversationId);
        if (entry?.runId === target.runId) {
          entry.status = "running";
          entry.controlRevision = (entry.controlRevision || 0) + 1;
        }
        saveState();
        return { text: "✅ 已收到，任务继续执行。" };
      } catch (error) {
        // 包括网络回执不明：保留原提问路由，供手动重发；不循环自动投递。
        return { text: `⚠️ 回答未确认送达：${error.message}。等待记录已保留，可查「状态」后重发「回答：内容」。` };
      } finally {
        answering.delete(conversationId);
      }
    }

    // 所有任务先入持久队列，派发前留下标记，重启不会重做回执未知的操作。
    if (!text && !request.media) return { text: "请发送任务内容。" };
    const busy = activeRuns.has(conversationId) || submitting.has(conversationId);
    const queue = taskQueues.get(conversationId) || [];
    if (queue.length >= MAX_QUEUE) return { text: `⏳ 排队已满（上限 ${MAX_QUEUE}）。可发送「状态」查看。` };
    queue.push({ id: randomUUID(), text, media: request.media });
    taskQueues.set(conversationId, queue);
    saveState();
    if (busy || queuePauses.has(conversationId) || queue.length > 1) {
      if (!busy && !queuePauses.has(conversationId)) void drainQueue(conversationId).catch((error) => console.error(`[bridge] 队列启动失败: ${error}`));
      return { text: `⏳ 已加入队列（第 ${queue.length} 位）${queuePauses.has(conversationId) ? "，队列暂停中；请发送「状态」查看原因" : "，轮到后自动开始"}。执行中补充要求请发「补充：内容」。` };
    }
    try {
      const task = await drainQueue(conversationId, false);
      const scope = sender.project_name ? `项目：${sender.project_name}` : "临时工作区";
      return { text: `🚀 任务 #${task.num} 已开始（${scope}）。\n执行中可发「补充：要求」；「状态」查看进度；「停止」中断当前任务。` };
    } catch (error) {
      return { text: `⚠️ ${error.message}` };
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
      const delivered = await botSend(String(body.text));
      sendJson(res, delivered ? 200 : 502, delivered ? { ok: true } : { error: "微信消息未完整送达，请检查桥日志；未自动重试。" });
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
