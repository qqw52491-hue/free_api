// 假 AI 冒烟测试用的 mock 模型服务：OpenAI 兼容 /v1/chat/completions（SSE 流式）。
//
// 【回放模式 + 宿主事实驱动状态机】不手写剧本（手写剧本 = 想象中的模型行为，
// 已连续骗出 3 个 bug），回放 agent_logs 真实 trace 提取的卡带
// （见 cassette/extract-cassette.mjs）。出牌不再靠整卷线性指针（它无法建模
// 「质询→修复→重试」循环，parse 变体 301 调用循环的根因），而是每次调用从
// 上下文重新推导宿主事实、按事实选相位：
//   plan_approved? —— 「门禁状态: [已批准放行]」（main_loop 方案审查结论注入）
//   oracle_index?  —— deriveStage()（CURRENT_STAGE.md/判分进度标记的最高阶段）
//   判分反馈       —— 阶段失败「实现行为与契约不符」/ 终局拦截「宿主判分进度 X/Y 未完成」
// 相位内保持线性子指针（read→write→verify 次序来自真实 trace）；跨相位推进只由
// 事实翻转驱动——重试/failover/探活消耗的调用不再让卡带错位；wait 牌粘滞与
// 修复回卷（封顶 2 次）保留；剧本外 → 明确的错误牌 + 大声记日志，不硬等不猜。
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

// 宿主事实驱动状态机（替代线性指针回放器，架构见文件头注释）
function makeStateMachine(cassette, fixtureDir) {
  const byStage = cassette.meta.stages;
  const planCards = byStage["0"] ?? [];
  const stageCards = new Map();
  for (const [k, cards] of Object.entries(byStage)) {
    if (k !== "0" && k !== "default") stageCards.set(Number(k), cards);
  }
  const finishCards = byStage.default ?? [];
  const maxStage = Math.max(0, ...stageCards.keys());

  // 进度子指针只记"相位内走到哪"；相位切换由事实翻转驱动（每次调用重推导）
  const state = {
    planIdx: 0,
    postReviewServed: false,
    stageIdx: {},
    stageRepairs: {},
    finishServed: false,
    finishRepairs: 0,
    lastStage: null,
  };

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

  // wait 粘滞判定（强信号）：真评分卡或已批准放行。
  // ⚠️ 不可用「结论已消费」「子审查代理已完成」当信号：invalid 交付（failover
  // 重派窗口）同样带这些字样，放行会抢在备用审查员结论之前（历史实测两次）。
  const isReviewResult = (messages) => {
    const users = messages.filter((m) => m.role === "user").slice(-6);
    const blob = users.map((m) => String(m.content ?? "")).join("\n");
    return /审查评分卡|已批准放行/.test(blob);
  };

  return function nextMainCard(messages) {
    const last = lastUserText(messages);
    const stage = deriveStage(messages);
    const blob = messages
      .filter((m) => m.role === "user")
      .map((m) => String(m.content ?? ""))
      .join("\n");
    // 事实 1：方案关口放行。宽匹配——放行可能经由多种注入路径落地（评分卡行
    // 「门禁状态: [已批准放行]」或门禁状态卡的「方案阶段: 已批准放行」等），
    // 旧线性 mock 即用宽匹配且从未误触发（工具菜单等常驻文本不含该词）。
    const planApproved = /已批准放行/.test(blob);
    // 事实 3a/3b 只看最后一轮反馈（避免历史失败残留误触发）：
    const stageFailedNow = last.includes("实现行为与契约不符"); // eval_oracle 阶段判分失败 hint
    const oracleBlockedNow = last.includes("宿主判分进度"); // finish 门禁判分进度拦截

    // ── 相位一：方案关口（open…gate → wait 粘滞 → post_review）──
    if (!planApproved) {
      const card = planCards[state.planIdx];
      if (!card) {
        console.log(`[mock][剧本外!!!] 方案关口卡带耗尽仍未放行。尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
        return OUT_OF_SCRIPT_CARD;
      }
      if (card.role === "wait" && !isReviewResult(messages)) {
        console.log(`[mock][wait 粘滞] plan #${state.planIdx}/${planCards.length} 等方案审查结论。尾部：${last.slice(-160).replace(/\n/g, " ⏎ ")}`);
        return expand(card, { stripJobIds: true });
      }
      state.planIdx += 1;
      if (card.role === "post_review") state.postReviewServed = true;
      console.log(`[mock][出牌·plan] #${state.planIdx - 1}/${planCards.length} role=${card.role}`);
      return expand(card, {});
    }

    // 方案已放行：post_review 尚未出就补出（方案结论到达后的第一张主牌）
    if (!state.postReviewServed) {
      const i = planCards.findIndex((c) => c.role === "post_review");
      state.postReviewServed = true;
      if (i >= 0) {
        state.planIdx = Math.max(state.planIdx, i + 1);
        console.log(`[mock][出牌·plan] 放行后补出 post_review`);
        return expand(planCards[i], {});
      }
    }

    // ── 相位二：阶段实现（read→write→verify；判分失败原地修复重试）──
    const target = stage < 1 ? 1 : stage;
    if (stageCards.has(target) && !oracleBlockedNow) {
      const cards = stageCards.get(target);
      state.lastStage = target;
      const iw = cards.findIndex((c) => c.role === "implement_write");
      const idx = state.stageIdx[target] ?? 0;

      // 事实 3a：当前阶段判分失败 → 重出 implement_write（封顶 2 次），修复后下一张 verify
      if (stageFailedNow && idx > 0) {
        const repairs = (state.stageRepairs[target] ?? 0) + 1;
        if (iw >= 0 && repairs <= 2) {
          state.stageRepairs[target] = repairs;
          state.stageIdx[target] = iw + 1;
          console.log(`[mock][修复重试] stage=${target} 判分失败，重出 implement_write（第 ${repairs}/2 次）。尾部：${last.slice(-160).replace(/\n/g, " ⏎ ")}`);
          return expand(cards[iw], {});
        }
        console.log(`[mock][剧本外!!!] stage=${target} 判分失败且修复超限。尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
        return OUT_OF_SCRIPT_CARD;
      }

      const card = cards[idx];
      if (card && card.role === "wait" && !isReviewResult(messages)) {
        console.log(`[mock][wait 粘滞] stage=${target} #${idx}/${cards.length} 等审查结论。尾部：${last.slice(-160).replace(/\n/g, " ⏎ ")}`);
        return expand(card, { stripJobIds: true });
      }
      if (!card) {
        // 本阶段牌已出完但解锁事实未进上下文 → 幂等重发 verify 等判分
        const v = cards.findIndex((c) => c.role === "verify");
        if (v >= 0) {
          console.log(`[mock][verify 幂等] stage=${target} 牌尽未解锁，重发 verify 等判分`);
          return expand(cards[v], {});
        }
        console.log(`[mock][剧本外!!!] stage=${target} 牌尽且无 verify 可重发。尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
        return OUT_OF_SCRIPT_CARD;
      }
      state.stageIdx[target] = idx + 1;
      console.log(`[mock][出牌·stage=${target}] #${idx}/${cards.length} role=${card.role}`);
      return expand(card, {});
    }

    // ── 相位三：finish（判分进度拦截 → 回卷修复；否则幂等重发）──
    const finishCard = finishCards[0];
    if (!finishCard) {
      console.log(`[mock][剧本外!!!] 无 finish 牌。尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
      return OUT_OF_SCRIPT_CARD;
    }
    if (state.finishServed && oracleBlockedNow) {
      // 事实 3b：宿主判分进度未完成 → 回卷最后工作阶段的 implement_write 修复重试；
      // 回卷超限仍被拦截 = 卡带内容覆盖不了该进度 → 大声出剧本外牌（不静默循环）
      const rs = state.lastStage ?? maxStage;
      const cards = stageCards.get(rs) ?? [];
      const iw = cards.findIndex((c) => c.role === "implement_write");
      if (state.finishRepairs < 2 && iw >= 0) {
        state.finishRepairs += 1;
        state.stageIdx[rs] = iw + 1;
        console.log(`[mock][回卷] finish 被判分进度拦截，回卷 stage=${rs} 修复（第 ${state.finishRepairs}/2 次）。尾部：${last.slice(-160).replace(/\n/g, " ⏎ ")}`);
        return expand(cards[iw], {});
      }
      console.log(`[mock][剧本外!!!] finish 被判分进度拦截且回卷超限（stage=${rs}）。尾部：${last.slice(-300).replace(/\n/g, " ⏎ ")}`);
      return OUT_OF_SCRIPT_CARD;
    }
    if (state.finishServed) {
      console.log(`[mock][finish 重发] 终态门禁未收卡，幂等重发 finish`);
      return expand(finishCard, {});
    }
    state.finishServed = true;
    console.log(`[mock][出牌·finish] 阶段事实=${stage}/${maxStage}`);
    return expand(finishCard, {});
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
  const nextMainCard = makeStateMachine(cassette, fixtureDir);
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
