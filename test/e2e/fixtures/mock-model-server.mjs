// 假 AI 冒烟测试用的 mock 模型服务：OpenAI 兼容 /v1/chat/completions（SSE 流式）。
// 目的：用确定性剧本替代真模型，端到端跑通「取题→写码→跑测试→交卷 + 审查评分」全门禁管道，
// 使管道自身 bug 与模型能力问题彻底解耦。本服务无任何答案——它只是按状态机出牌的客户端，
// 题目仍来自宿主 oracle，判分仍在服务端。
import http from "node:http";

const STAGE1_IMPL = `export function transform(value) {
  return typeof value === 'string' ? value.trim().toUpperCase() : value;
}
`;

// 单会话状态机（冒烟串行运行，一次只有一个会话）
const FIXTURE_DIR = process.env.FREE_API_LIVE_FIXTURE_DIR ?? "";
const state = { phase: "start", finishAttempts: 0 };

function lastUserText(messages) {
  // 从尾部扫描，跳过常驻的 FINAL_JSON 锚点消息（每条上下文末尾都有，会掩盖真实反馈）
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role !== "user") continue;
    const c = String(messages[i].content ?? "");
    if (c.startsWith("【本轮最终输出】")) continue;
    return c;
  }
  return "";
}

function isMainAgentRequest(messages) {
  // 主 Agent 系统提示独有锚点（角色卡菜单里嵌着 reviewer 人设，正向判定会被误判，
  // 两次实测教训：4 次解析错误）——改用反向判定
  const sys = String(messages[0]?.content ?? "");
  return sys.includes("PROMPT_BUILD:");
}
function isReviewerRequest(messages) {
  return !isMainAgentRequest(messages) && /child reviewer|对抗审查/.test(String(messages[0]?.content ?? ""));
}

// 状态机推进：每次请求 = 上一步工具结果已回灌，决定下一步动作
function nextAssistantCall(messages) {
  const last = lastUserText(messages);
  const all = JSON.stringify(messages);

  if (isMainAgentRequest(messages)) {
    return mainAgentScript(last, all, messages);
  }
  if (isReviewerRequest(messages)) {
    // 故障注入变体（验证 failover 链端到端）：
    // - parse:   第 1 次 reviewer 输出非法 JSON → 应触发同参重试
    // - timeout: 第 1 次 reviewer 延迟应答 → 应触发宿主超时 → 跨平台备用审查员
    const scenario = process.env.FREE_API_SMOKE_SCENARIO ?? "happy";
    state.reviewerCalls = (state.reviewerCalls ?? 0) + 1;
    const validReview = JSON.stringify({
      findings: [],
      confidence: "confirmed",
      evidence_refs: [],
      missing_evidence: [],
      scorecard: { correctness: 90, risk: 85, style: 90, test_coverage: 85 },
    });
    if (scenario === "parse" && state.reviewerCalls === 1) {
      return "这不是合法的审查 JSON（故障注入：parse 变体第 1 次调用）";
    }
    if (scenario === "timeout" && state.reviewerCalls === 1) {
      const delay = Number(process.env.FREE_API_SMOKE_REVIEWER_DELAY_MS ?? 40_000);
      const busyUntil = Date.now() + delay;
      while (Date.now() < busyUntil) { /* 故障注入：故意压线超时 */ }
      return validReview;
    }
    return validReview;
  }

  return mainAgentScript(last, all, messages);
}

function mainAgentScript(last, all, messages) {
  if (last.includes("已派发 child_run_id")) {
    state.phase = "waiting";
    return JSON.stringify({ tool: "wait_jobs", description: "挂起等待审查结论" });
  }
  if (state.phase === "waiting" || (all.includes("scorecard") && last.includes("子审查"))) {
    // 审查结论已回收且评分通过 → 写 stage-01
    state.phase = "approved";
    return JSON.stringify({
      tool: "write_file",
      description: "实现 stage-01：去空白并转大写",
      command: { path: `${FIXTURE_DIR}/src/stage-01.mjs`, content: STAGE1_IMPL },
      todo_update: [{ id: 0, status: "done", description: "完成阶段 01 实现并通过宿主判分" }],
    });
  }
  if (state.phase === "approved") {
    state.phase = "testing";
    return JSON.stringify({
      tool: "shell_exec",
      description: "运行 npm test 验证当前阶段",
      command: { command: "npm test", cwd: FIXTURE_DIR, shell_mode: true, timeout_ms: 30000 },
    });
  }
  if (state.phase === "testing") {
    // 注意宿主文案是"解锁阶段 02"（无"已"字），别再写错检测串
    if (last.includes("解锁阶段") || last.includes("已通过宿主判分")) {
      // 全流程管道已验证（至少解锁一阶段）：交卷，附验证证据
      state.phase = "finishing";
      return JSON.stringify({
        tool: "finish",
        description: "冒烟测试：管道验证完成，交卷",
        change_verification_update: {
          status: "verified",
          evidence_refs: ["npm test 通过：阶段 01 宿主判分通过并解锁后续阶段"],
        },
      });
    }
    // 测试失败：回到重写（正常情况下 stage-01 实现是正确的，不应走到这）
    state.phase = "approved";
    return JSON.stringify({
      tool: "write_file",
      description: "修正 stage-01 实现",
      command: { path: `${FIXTURE_DIR}/src/stage-01.mjs`, content: STAGE1_IMPL },
    });
  }
  if (state.phase === "finishing") {
    return JSON.stringify({
      tool: "finish",
      description: "冒烟测试交卷（重试）",
      change_verification_update: {
        status: "verified",
        evidence_refs: ["npm test 通过：阶段 01 宿主判分通过并解锁后续阶段"],
      },
    });
  }

  // 起手：先按门禁要求派发审查（附 README 证据）。
  // 哑脚本无决策能力：派发超过 2 次就改走 wait_jobs（防哑循环空转烧步数）
  state.dispatchCount = (state.dispatchCount ?? 0) + 1;
  if (state.dispatchCount > 2) {
    state.phase = "waiting";
    return JSON.stringify({ tool: "wait_jobs", description: "挂起等待审查结论（派发上限保护）" });
  }
  state.phase = "dispatching";
  return JSON.stringify({
    tool: "child_reviewer",
    description: "对实现方案做对抗审查",
    question: "审查即将进行的 24 阶段实现方案是否有结构性缺陷",
    evidence_refs: ["README.md", "package.json"],
    evidence_excerpt: "24 阶段宿主判分任务：逐阶段实现 transform 并通过 npm test 解锁下一阶段。",
  });
}

export function startMockModel() {
  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      let messages = [];
      try {
        const parsed = JSON.parse(body);
        messages = parsed.messages ?? [];
      } catch {
        messages = [];
      }
      const content = nextAssistantCall(messages);
      const role0 = String(messages[0]?.content ?? "").slice(0, 40);
      console.log(`[mock] messages=${messages.length} msg0=${JSON.stringify(role0)} → ${content.slice(0, 80)}`);

      res.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "close",
      });
      // OpenAI SSE：role → content 分片 → usage → [DONE]
      const chunks = [
        { choices: [{ delta: { role: "assistant", content: "" }, index: 0 }] },
        { choices: [{ delta: { content }, index: 0 }] },
        { choices: [], usage: { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150 } },
      ];
      for (const chunk of chunks) {
        res.write(`data: ${JSON.stringify(chunk)}\n\n`);
      }
      res.write("data: [DONE]\n\n");
      res.end();
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      resolve({
        port,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}
