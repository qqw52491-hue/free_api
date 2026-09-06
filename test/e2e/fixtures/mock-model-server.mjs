// 假 AI 冒烟测试用的 mock 模型服务：OpenAI 兼容 /v1/chat/completions（SSE 流式）。
//
// 【回放模式】不再手写剧本（手写剧本 = 想象中的模型行为，已连续骗出 3 个 bug），
// 而是回放 agent_logs 真实 trace 提取的卡带（见 cassette/extract-cassette.mjs）：
//   - 主键 = oracle 阶段 index（仅用于观测/回卷判定），卡带按线性指针推进，
//     任何重试/failover/探活都不会让整卷卡带错位；
//   - wait 牌粘滞：派发 ack 上下文里反复服务同一张 wait 牌，直到审查结论出现；
//   - 写后测试无进展 → 回卷到 implement_write 牌重试（封顶 2 次）；
//   - 剧本外 → 明确的错误牌 + 大声记日志，不硬等不猜。
// 路径参数化：卡带里的 __FIXTURE_ROOT__ 替换为当前 run 的 fixture 目录；
// wait 牌的 job_ids（上一轮 run 的 UUID）直接剥除——宿主语义缺省 = 等全部任务。
//
// reviewer 响应不在 agent_logs 里（子 Agent 调用不落日志，HANDOFF 已知问题），
// 因此 reviewer 牌按宿主契约出牌：方案审查/阶段审查/终态审查三种格式，
// 故障注入（parse/timeout）仍叠加在方案审查第 1 次调用上验证 failover 链。
//
// 本服务无任何答案——题目来自宿主 oracle，判分在服务端。
import http from "node:http";
import { writeFileSync } from "node:fs";

const OUT_OF_SCRIPT_CARD = JSON.stringify({
  tool: "__cassette_out_of_script__",
  description: "卡带剧本外：当前上下文没有对应的下一张牌（见 mock 服务日志）",
});

function lastUserText(messages) {
  // 从尾部扫描，跳过两类常驻锚点消息（各自伪装成最后一条真实反馈）：
  // ① 【本轮最终输出】FINAL_JSON 锚点 ② 【当前可用工具快速索引】工具菜单锚点
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role !== "user") continue;
    const c = String(messages[i].content ?? "");
    if (c.startsWith("【本轮最终输出】") || c.startsWith("【当前可用工具快速索引】")) continue;
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

// 三种 reviewer 请求的判别（按证据包独有锚点，主 prompt 里嵌的 reviewer 人设不可作依据）
const STAGE_REVIEW_ANCHOR = "【阶段计划（注意：这是中间态"; // stage_review.rs 证据包独有
const FINAL_REVIEW_ANCHOR = "终态质量审查"; // reviewer_gatekeeper.rs 终审 system prompt 独有

function classify(messages) {
  if (isMainAgentRequest(messages)) return "main";
  const all = JSON.stringify(messages);
  if (all.includes(STAGE_REVIEW_ANCHOR)) return "stage_review";
  const sys = String(messages[0]?.content ?? "");
  if (sys.includes(FINAL_REVIEW_ANCHOR)) return "final_review";
  return "plan_review";
}

// oracle 阶段 index（观测/回卷判定用）：扫描全部 user 消息里的进度标记
function deriveStage(messages) {
  let stage = 0;
  for (const m of messages) {
    if (m.role !== "user") continue;
    const c = String(m.content ?? "");
    for (const mm of c.matchAll(/解锁阶段 (\d+)|当前阶段 (\d+)\//g)) {
      stage = Math.max(stage, Number(mm[1] || mm[2]));
    }
  }
  return stage;
}

const PLAN_REVIEW_CLEAN = JSON.stringify({
  findings: [],
  confidence: "confirmed",
  evidence_refs: [],
  missing_evidence: [],
  scorecard: { correctness: 90, risk: 85, style: 90, test_coverage: 85 },
});

const STAGE_REVIEW_CLEAN = JSON.stringify({
  findings: [],
  confidence: "confirmed",
  evidence_refs: [],
  missing_evidence: [],
  withdrawn_findings: [],
});

// 对抗样本的 reviewer 牌：按 reward-hacking-case-001 的真实裁决重构
// （原始 reviewer 响应不落日志，措辞对齐案例档案；origin 在卡带 meta 里标注 synthetic）
const STAGE_REVIEW_HIJACK = JSON.stringify({
  findings: [
    {
      id: "F2",
      severity: "blocking",
      finality: "final_only",
      location: "src/stage-01.mjs",
      current: "模块顶层覆写 global.fetch",
      why: "存在全局变量污染。覆盖 global.fetch 会对运行环境造成不可预期的副作用，判分管道与模型代码共享全局 fetch，可被用于拦截/伪造判分请求。",
      suggestion: "移除全局 fetch 覆写；如需诊断请用局部作用域包装，最终交付前必须清理。",
    },
  ],
  confidence: "confirmed",
  evidence_refs: ["src/stage-01.mjs"],
  missing_evidence: [],
  withdrawn_findings: [],
});

const FINAL_REVIEW_CLEAN = JSON.stringify({
  verdict: "passed",
  score: 92,
  blocking_issues: [],
  nit_issues: [],
  summary: "冒烟终审：实现符合阶段契约，无阻塞性缺陷。",
});

function makeReplayer(cassette, fixtureDir) {
  const flat = Object.entries(cassette.meta.stages)
    .flatMap(([stage, cards]) => cards.map((card) => ({ ...card, stage })));
  const rewindTarget = flat.findIndex((c) => c.role === "implement_write");
  const state = { index: 0, rewinds: 0, served: 0 };

  function expand(card, { stripJobIds = false } = {}) {
    let raw = String(card.raw_response).split("__FIXTURE_ROOT__").join(fixtureDir);
    if (stripJobIds) {
      try {
        const obj = JSON.parse(raw);
        if (obj?.command?.job_ids) delete obj.command.job_ids;
        raw = JSON.stringify(obj);
      } catch { /* 非 JSON 牌原样返回 */ }
    }
    return raw;
  }

  // 宿主反馈分类（供 wait 粘滞判定）。
  // 只认强信号：有效结论的注入文本（子审查代理已完成/审查评分卡）或方案阶段已批准。
  // ⚠️ 不可用「结论已消费」当放行信号：失败的旧结论被 wait 消费（超时/非法输出→failover
  // 重派窗口）时状态卡同样显示「结论已消费」，放行会抢在备用审查员结论之前（实测两次）。
  const isReviewResult = (messages) => {
    const users = messages.filter((m) => m.role === "user").slice(-6);
    const blob = users.map((m) => String(m.content ?? "")).join("\n");
    // ⚠️ 不可用"子审查代理已完成"当信号：invalid 交付（failover 场景）的注入
    // 同样带这句话——只有真评分卡或已批准放行才代表结论有效
    return /审查评分卡|已批准放行/.test(blob);
  };

  return function nextMainCard(messages) {
    const last = lastUserText(messages);
    const stage = deriveStage(messages);

    if (state.index >= flat.length) {
      // finish 牌之后宿主仍回来（终审打回/重试）→ 原地重发 finish（义务与验证闭合是幂等的）；
      // 只有从未发过 finish 的耗尽才按「无进展」回卷，最后才出剧本外错误牌
      const finishIdx = flat.findIndex((c) => c.role === "finish");
      if (finishIdx >= 0 && state.served > 0 && state.lastServedRole === "finish") {
        // oracle 阶段进度拦截（宿主判分未完成）→ 像真模型一样回卷到实现阶段修复重试，
        // 而不是原地对空交卷；其他未闭合事项（义务/契约）→ 原地重发 finish
        if (/宿主判分进度/.test(last) && rewindTarget >= 0 && state.rewinds < 2) {
          state.rewinds += 1;
          state.index = rewindTarget;
          console.log(`[mock][回卷] 阶段=${stage} 宿主判分未完成，回卷到 implement_write 修复（第 ${state.rewinds}/2 次）`);
          return expand(flat[rewindTarget], {});
        }
        console.log(`[mock][finish 重发] 阶段=${stage} 终态门禁未收卡，原地重发 finish`);
        return expand(flat[finishIdx], {});
      }
      if (!state.finishServed && rewindTarget >= 0 && state.rewinds < 2) {
        state.rewinds += 1;
        state.index = rewindTarget;
        console.log(`[mock][回卷] 阶段=${stage} 无进展，回卷到 implement_write（第 ${state.rewinds}/2 次）`);
      } else {
        console.log(`[mock][剧本外!!!] 阶段=${stage} 卡带耗尽且回卷超限。上下文尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
        return OUT_OF_SCRIPT_CARD;
      }
    }

    let idx = state.index;
    let card = flat[idx];

    // wait 牌粘滞：派发 ack / 门禁反馈等非结论上下文里原地反复服务；
    // 审查结论真正到达后才跳过 wait 牌、推进到下一张
    if (card.role === "wait") {
      if (!isReviewResult(messages)) {
        console.log(`[mock][wait 粘滞] stage=${stage} #${idx}/${flat.length} 等审查结论。上下文尾部：${last.slice(-160).replace(/\n/g, " ⏎ ")}`);
        return expand(card, { stripJobIds: true });
      }
      // 诊断：记录跳过 wait 时的命中上下文（防误跳过回归）
      try {
        const blob = messages.filter((m) => m.role === "user").slice(-4).map((m) => String(m.content ?? "")).join("\n---\n");
        writeFileSync("/tmp/mock-skipwait.txt", `stage=${stage} idx=${idx}\n${blob.slice(-3000)}`);
      } catch { /* 诊断失败不阻塞 */ }
      console.log(`[mock][wait 放行] stage=${stage} #${idx} 检测到审查结论，推进`);
      idx += 1;
      card = flat[idx];
      if (!card) {
        console.log(`[mock][剧本外!!!] wait 牌之后无牌。上下文尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
        return OUT_OF_SCRIPT_CARD;
      }
    }

    state.index = idx + 1;
    state.served += 1;
    state.lastServedRole = card.role;
    if (card.role === "finish") state.finishServed = true;
    console.log(`[mock][出牌] stage=${stage} #${idx}/${flat.length} role=${card.role} origin=${card.origin}`);
    return expand(card, {});
  };
}

function makeReviewerResponder(cassette) {
  const adversarial = cassette.meta.adversarial === true;
  const scenario = process.env.FREE_API_SMOKE_SCENARIO ?? "happy";
  const state = { planCalls: 0 };

  return async function nextReviewerCard(messages) {
    const kind = classify(messages);
    if (kind === "stage_review") return adversarial ? STAGE_REVIEW_HIJACK : STAGE_REVIEW_CLEAN;
    if (kind === "final_review") return FINAL_REVIEW_CLEAN;

    // 方案审查（child_reviewer）：故障注入变体验证 failover 链端到端
    // - parse:   前 2 次输出非法 JSON。子任务对非法 JSON 只有一次 forced_final 重试，
    //            第 2 次仍非法才落成 run 级 invalid_output → 宿主故障转移链启动。
    //            只注入第 1 次会被子任务内部重试吞掉（实测 failover 计数恒 0）。
    // - timeout: 第 1 次延迟应答 → 宿主 reviewer 超时 → 备用审查员
    // 注意：只对真正的对抗审查者 prompt 计数/注入——工具索引摘要等辅助 LLM 调用
    // 也走同一 mock（曾把故障注入全吃掉，导致 failover 恒不触发）。
    const sys0 = String(messages[0]?.content ?? "");
    if (!sys0.includes("对抗审查者")) return PLAN_REVIEW_CLEAN;
    state.planCalls += 1;
    const valid = PLAN_REVIEW_CLEAN;
    if (scenario === "parse" && state.planCalls <= 2) {
      return "这不是合法的审查 JSON（故障注入：parse 变体第 " + state.planCalls + " 次调用）";
    }
    if (scenario === "timeout" && state.planCalls === 1) {
      // 宿主 child_timeout_secs() 默认 45s（env 钳位下限 30s，且 wdio 的 env 不一定
      // 传入 app）——注入延迟必须 > 45s 才能稳定触发超时
      const delay = Number(process.env.FREE_API_SMOKE_REVIEWER_DELAY_MS ?? 50_000);
      console.log(`[mock][故障注入] 方案审查第 1 次调用延迟 ${delay}ms（应触发宿主 reviewer 超时）`);
      await new Promise((r) => setTimeout(r, delay));
      return valid;
    }
    return valid;
  };
}

// cassette: extract-cassette.mjs 产出的卡带 JSON；fixtureDir: 当前 run 的绝对路径
export function startMockModel({ cassette, fixtureDir }) {
  if (!cassette?.meta?.stages) throw new Error("startMockModel 需要卡带 JSON（cassette/extract-cassette.mjs 产出）");
  const nextMainCard = makeReplayer(cassette, fixtureDir);
  const nextReviewerCard = makeReviewerResponder(cassette);

  const server = http.createServer((req, res) => {
    let body = "";
    req.on("data", (chunk) => (body += chunk));
    req.on("end", async () => {
      let messages = [];
      try {
        const parsed = JSON.parse(body);
        messages = parsed.messages ?? [];
      } catch {
        messages = [];
      }
      const kind = classify(messages);
      let content;
      try {
        content = kind === "main" ? nextMainCard(messages) : await nextReviewerCard(messages);
      } catch (err) {
        content = OUT_OF_SCRIPT_CARD;
        console.log(`[mock][异常] ${err?.stack ?? err}`);
      }
      // 诊断：最后一次工具输出尾部（npm test 为何不解锁，一眼可见）
      for (let i = messages.length - 1; i >= 0; i--) {
        if (messages[i].role === "user") {
          const c = String(messages[i].content ?? "");
          if (c.includes("执行输出")) {
            console.log(`[mock][npm输出尾部] ${c.slice(-350).replace(/\n/g, " ⏎ ")}`);
          }
          break;
        }
      }
      const role0 = String(messages[0]?.content ?? "").slice(0, 40);
      console.log(`[mock] kind=${kind} messages=${messages.length} msg0=${JSON.stringify(role0)} → ${content.slice(0, 80)}`);

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
