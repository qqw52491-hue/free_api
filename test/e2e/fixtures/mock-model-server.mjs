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

function isReviewerRequest(messages) {
  const sys = String(messages[0]?.content ?? "");
  // 精确判定：reviewer 的系统提示以此开场（主 Agent 契约里也含 "child reviewer" 字样，
  // 首版误判导致把审查 JSON 发给了主 Agent——4 次解析错误的教训）
  return sys.includes("你是独立的对抗审查者") || sys.includes("受限的只读 child reviewer");
}

// 状态机推进：每次请求 = 上一步工具结果已回灌，决定下一步动作
function nextAssistantCall(messages) {
  const last = lastUserText(messages);
  const all = JSON.stringify(messages);

  if (isReviewerRequest(messages)) {
    // 固定合法评分卡：均分 87.5 ≥ 75，risk 85 ≥ 60 —— 应通过硬门禁
    return JSON.stringify({
      findings: [],
      confidence: "confirmed",
      evidence_refs: [],
      missing_evidence: [],
      scorecard: { correctness: 90, risk: 85, style: 90, test_coverage: 85 },
    });
  }

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
      command: { path: "src/stage-01.mjs", content: STAGE1_IMPL },
      todo_update: [{ id: 0, status: "done", description: "完成阶段 01 实现并通过宿主判分" }],
    });
  }
  if (state.phase === "approved") {
    state.phase = "testing";
    return JSON.stringify({
      tool: "shell_exec",
      description: "运行 npm test 验证当前阶段",
      command: { command: "npm test", shell_mode: true, timeout_ms: 30000 },
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
      command: { path: "src/stage-01.mjs", content: STAGE1_IMPL },
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
