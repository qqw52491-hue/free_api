#!/usr/bin/env node
/**
 * eval_runner.mjs
 *
 * 基于真实 Git 提交历史与 Worktree 物理隔离的多 Agent 自动化回归评测执行器。
 *
 * 核心设计：
 * 1. 物理隔离：在 test-artifacts/worktrees/ 下创建临时 git worktree 并切换到指定 base_commit；
 * 2. FAIL_TO_PASS 判分：agent 修代码，考卷提供卷子——判据测试以 test_patch 在判分前注入；
 *    判分只认物理事实（cargo test 退出码 + 至少 1 个测试执行且全部通过），绝不让 LLM 眼睛看 diff；
 * 3. 红基线校准：每道题开考前，先在基线 commit 注入测试补丁并验证判据为红——
 *    若基线即绿（判据空转）或补丁无法编译，判定考题失效，禁止开考（防止假绿）；
 * 4. 黑匣子全量捕获：抓取 Agent 每步轨迹（agent_run_traces.steps_json）、门禁拦截（agent_gate_fires）
 *    与主/子 Agent Token 账单（agent_llm_calls / agent_child_runs）；
 * 5. 零污染保证：无论评测成功或异常，退出时百分之百自动销毁临时 worktree。
 *
 * 用法：
 *   npm run eval:real:dry                                      # 红基线校准 + 沙盒生命周期验证（零 Token）
 *   npm run eval:real -- --case case_02_readonly_diagnosis     # 仅运行指定考题
 *   node scripts/eval_runner.mjs --case <id> --agent-session <sessionId>
 *                                                              # 实弹判分：对已在沙盒工作区作业过的
 *                                                              # 应用会话按 session_id 收取轨迹并判分
 */

import { execSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const rootDir = path.resolve(path.dirname(__filename), '..');
const casesDir = path.join(rootDir, 'test', 'eval_cases');
const artifactsDir = path.join(rootDir, 'test-artifacts');
const worktreeBaseDir = path.join(artifactsDir, 'worktrees');
const evaluationsDir = path.join(rootDir, 'agent_evaluations');
const appDbPath = path.join(
  process.env.HOME || '',
  'Library',
  'Application Support',
  'free-api-chat',
  'data_v2.db'
);

const CALIBRATION_TIMEOUT_MS = 900_000; // 校准含基线冷编译，放宽到 15 分钟
const JUDGE_TIMEOUT_MS = 600_000;

// ---- 参数解析 ----
function parseCliArgs() {
  const args = process.argv.slice(2);
  const options = {
    dryRun: false,
    caseId: null,
    runAll: false,
    verbose: false,
    agentSession: null,
    live: false,
    model: process.env.FREE_API_LIVE_MODEL_ID?.trim() || null,
    reviewerModel: process.env.FREE_API_LIVE_REVIEWER_MODEL_ID?.trim() || null,
    durationMinutes: Number(process.env.FREE_API_LIVE_MAX_DURATION_MINUTES ?? 25),
  };

  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === '--dry-run') {
      options.dryRun = true;
    } else if (a === '--case') {
      options.caseId = args[++i];
    } else if (a === '--all') {
      options.runAll = true;
    } else if (a === '--agent-session') {
      options.agentSession = args[++i];
    } else if (a === '--live') {
      options.live = true;
    } else if (a === '--model') {
      options.model = args[++i];
    } else if (a === '--reviewer-model') {
      options.reviewerModel = args[++i];
    } else if (a === '--duration') {
      options.durationMinutes = Number(args[++i]);
    } else if (a === '-v' || a === '--verbose') {
      options.verbose = true;
    }
  }

  return options;
}

// ---- 加载考题 ----
function loadEvaluationCases(filterId = null) {
  if (!fs.existsSync(casesDir)) {
    console.error(`❌ 考题目录不存在: ${casesDir}`);
    process.exit(1);
  }

  const files = fs.readdirSync(casesDir).filter((f) => f.endsWith('.json'));
  const cases = [];

  for (const file of files) {
    const filePath = path.join(casesDir, file);
    try {
      const content = JSON.parse(fs.readFileSync(filePath, 'utf8'));
      if (!filterId || content.id === filterId || file.replace('.json', '') === filterId) {
        cases.push(content);
      }
    } catch (e) {
      console.warn(`⚠️ 解析考题失败: ${file} - ${e.message}`);
    }
  }

  return cases;
}

// ---- 判据类型 ----
function judgeKind(caseDef) {
  if (caseDef.judge_kind) return caseDef.judge_kind;
  return caseDef.verify_command.trim().startsWith('cargo test') ? 'cargo_test' : 'command';
}

function testPatchAbsPath(caseDef) {
  if (!caseDef.test_patch) return null;
  return path.join(rootDir, caseDef.test_patch);
}

function scaffoldPatchAbsPath(caseDef) {
  if (!caseDef.scaffold_patch) return null;
  return path.join(rootDir, caseDef.scaffold_patch);
}

// ---- Worktree 隔离沙盒生命周期管理 ----
class WorktreeSandbox {
  constructor(caseDef) {
    this.caseDef = caseDef;
    this.timestamp = Date.now();
    this.dirName = `${caseDef.id}-${this.timestamp}`;
    this.sandboxPath = path.join(worktreeBaseDir, this.dirName);
    this.active = false;
  }

  setup() {
    fs.mkdirSync(worktreeBaseDir, { recursive: true });

    try {
      const resolvedCommit = execSync(`git rev-parse --verify "${this.caseDef.base_commit}^{commit}"`, {
        cwd: rootDir,
        encoding: 'utf8',
      }).trim();
      this.resolvedCommit = resolvedCommit;
    } catch {
      throw new Error(`无法解析目标 commit: ${this.caseDef.base_commit}`);
    }

    console.log(`\n📦 [Worktree Sandbox] 正在创建物理隔离副本: ${this.dirName}`);
    console.log(`   目标基线 Commit: ${this.caseDef.base_commit} (${this.resolvedCommit.slice(0, 10)})`);

    execSync(`git worktree add --detach -f "${this.sandboxPath}" "${this.resolvedCommit}"`, {
      cwd: rootDir,
      stdio: 'pipe',
    });

    this.active = true;
    return this.sandboxPath;
  }

  // 考卷脚手架：随环境预置的辅助代码（如判据测试依赖的簿记方法），
  // 属于考题给定条件的一部分；不包含判据测试本身与题眼修复逻辑。
  applyScaffold() {
    const scaffold = scaffoldPatchAbsPath(this.caseDef);
    if (!scaffold || !fs.existsSync(scaffold)) return;
    console.log(`   🧰 [Scaffold] 应用考卷脚手架补丁: ${path.basename(scaffold)}`);
    execSync(`git apply "${scaffold}"`, { cwd: this.sandboxPath, stdio: 'pipe' });
  }

  cleanup() {
    if (!this.active) return;
    try {
      console.log(`🧹 [Worktree Sandbox] 正在清理物理隔离沙盒: ${this.dirName}`);
      execSync(`git worktree remove --force "${this.sandboxPath}"`, {
        cwd: rootDir,
        stdio: 'ignore',
      });
      execSync(`git worktree prune`, {
        cwd: rootDir,
        stdio: 'ignore',
      });
      if (fs.existsSync(this.sandboxPath)) {
        fs.rmSync(this.sandboxPath, { recursive: true, force: true });
      }
    } catch (e) {
      console.warn(`⚠️ 清理 worktree 提示: ${e.message}`);
    } finally {
      this.active = false;
    }
  }
}

// ---- 物理命令执行 ----
function runCommand(sandboxPath, command, timeoutMs) {
  const sharedTargetDir = path.join(rootDir, 'src-tauri', 'target');
  const res = spawnSync(command, {
    cwd: sandboxPath,
    shell: true,
    encoding: 'utf8',
    env: {
      ...process.env,
      CARGO_TARGET_DIR: sharedTargetDir,
    },
    timeout: timeoutMs,
  });

  const timedOut = res.status === null && res.signal != null;
  return {
    exitCode: res.status ?? (res.error ? 1 : 0),
    timedOut,
    stdout: res.stdout || '',
    stderr: res.stderr || '',
  };
}

// ---- 判据解析（cargo_test 必须断言"至少 1 个测试执行且全部通过"，防 0 匹配假绿）----
function evaluateJudge(caseDef, runResult) {
  const kind = judgeKind(caseDef);
  const output = `${runResult.stdout}\n${runResult.stderr}`;

  if (runResult.timedOut) {
    return {
      passed: false,
      reason: `判据执行超时（${kind === 'cargo_test' ? '死锁类缺陷在修复前可能表现为挂起' : '命令未按时返回'}）`,
      exitCode: null,
      testsPassed: null,
      testsFailed: null,
    };
  }

  if (kind === 'command') {
    return {
      passed: runResult.exitCode === caseDef.expected_exit_code,
      reason: runResult.exitCode === caseDef.expected_exit_code ? '' : `退出码 ${runResult.exitCode} ≠ 期望 ${caseDef.expected_exit_code}`,
      exitCode: runResult.exitCode,
      testsPassed: null,
      testsFailed: null,
    };
  }

  // cargo_test：解析 "test result: ok. N passed; M failed; ..." 行
  let testsPassed = 0;
  let testsFailed = 0;
  const resultLines = output.match(/^test result: .+$/gm) || [];
  for (const line of resultLines) {
    const passedMatch = line.match(/(\d+) passed/);
    const failedMatch = line.match(/(\d+) failed/);
    testsPassed += passedMatch ? Number(passedMatch[1]) : 0;
    testsFailed += failedMatch ? Number(failedMatch[1]) : 0;
  }
  const compiled = !/error: could not compile/.test(output) && !/error\[E\d+\]/.test(output);

  if (!compiled) {
    return { passed: false, reason: '判据编译失败（补丁与基线代码不兼容？）', exitCode: runResult.exitCode, testsPassed, testsFailed };
  }
  if (testsPassed === 0 && testsFailed === 0) {
    return { passed: false, reason: '判据空转：0 个测试被执行（测试过滤器未匹配任何测试），禁止计为通过', exitCode: runResult.exitCode, testsPassed, testsFailed };
  }
  if (testsFailed > 0) {
    return { passed: false, reason: `${testsFailed} 个测试失败`, exitCode: runResult.exitCode, testsPassed, testsFailed };
  }
  const passed = runResult.exitCode === caseDef.expected_exit_code && testsPassed >= 1;
  return {
    passed,
    reason: passed ? `${testsPassed} 个测试全部通过` : `退出码 ${runResult.exitCode} ≠ 期望 ${caseDef.expected_exit_code}`,
    exitCode: runResult.exitCode,
    testsPassed,
    testsFailed,
  };
}

// ---- 红基线校准：证明判据在基线上确实为红，否则考题无效 ----
function runCalibration(sandboxPath, caseDef) {
  const kind = judgeKind(caseDef);
  const patchPath = testPatchAbsPath(caseDef);

  if (kind !== 'cargo_test') {
    console.log(`   🟡 [Calibration] 行为类判据 (${kind})，跳过红基线校准（其有效性由题面约束保证）`);
    return { status: 'skipped', kind, reason: '行为类判据无红基线可校准' };
  }

  if (!patchPath || !fs.existsSync(patchPath)) {
    return { status: 'invalid', kind, reason: `cargo_test 判据缺少测试补丁文件: ${caseDef.test_patch}` };
  }

  console.log(`   🧪 [Calibration] 注入测试补丁并验证判据在基线为红: ${path.basename(patchPath)}`);
  execSync(`git apply "${patchPath}"`, { cwd: sandboxPath, stdio: 'pipe' });

  const runResult = runCommand(sandboxPath, caseDef.verify_command, CALIBRATION_TIMEOUT_MS);
  const output = `${runResult.stdout}\n${runResult.stderr}`;

  // 还原基线（agent 作业环境必须不含判据测试，防改卷）
  execSync(`git apply -R "${patchPath}"`, { cwd: sandboxPath, stdio: 'pipe' });

  if (/error: could not compile/.test(output) || /error\[E\d+\]/.test(output)) {
    return { status: 'invalid', kind, reason: '测试补丁在基线编译失败，补丁与考题基线不兼容', exitCode: runResult.exitCode };
  }
  if (runResult.timedOut) {
    return { status: 'calibrated', kind, reason: '基线上判据超时（死锁类缺陷的挂起形态），视为有效红基线', exitCode: null };
  }
  if (runResult.exitCode === caseDef.expected_exit_code) {
    return { status: 'invalid', kind, reason: '判据在基线上为绿——考题无需修复即通过，判定失效（假绿）', exitCode: runResult.exitCode };
  }
  return { status: 'calibrated', kind, reason: `基线判据为红 (exit=${runResult.exitCode})，校准通过`, exitCode: runResult.exitCode };
}

// ---- 检查 Git Diff 边界 ----
function inspectGitDiff(sandboxPath) {
  try {
    const stat = execSync(`git diff --stat`, { cwd: sandboxPath, encoding: 'utf8' }).trim();
    const patch = execSync(`git diff`, { cwd: sandboxPath, encoding: 'utf8' }).trim();
    const changedFiles = execSync(`git diff --name-only`, { cwd: sandboxPath, encoding: 'utf8' })
      .trim()
      .split('\n')
      .filter(Boolean);

    return { stat, patch, changedFiles };
  } catch {
    return { stat: '', patch: '', changedFiles: [] };
  }
}

// ---- 从 SQLite 提取真实 Token 账单与遥测 ----
function querySessionTelemetry(sessionId) {
  const empty = {
    main_prompt: 0,
    main_completion: 0,
    review_prompt: 0,
    review_completion: 0,
    gate_fires: [],
    traces: [],
  };
  if (!sessionId || !fs.existsSync(appDbPath)) return empty;

  try {
    const rawTokens = execSync(
      `sqlite3 "${appDbPath}" "SELECT coalesce(sum(prompt_tokens),0), coalesce(sum(completion_tokens),0) FROM agent_llm_calls WHERE session_id='${sessionId}'"`,
      { encoding: 'utf8' }
    ).trim().split('|');

    const rawReview = execSync(
      `sqlite3 "${appDbPath}" "SELECT coalesce(sum(json_extract(result_json,'$.prompt_tokens')),0), coalesce(sum(json_extract(result_json,'$.completion_tokens')),0), count(*) FROM agent_child_runs WHERE parent_session_id='${sessionId}'"`,
      { encoding: 'utf8' }
    ).trim().split('|');

    // 门禁拦截遥测（run_id 记录的是触发时的运行标识，通常等于会话 id）
    let gateFires = [];
    try {
      const rawGates = execSync(
        `sqlite3 "${appDbPath}" "SELECT gate, route, count(*) FROM agent_gate_fires WHERE session_id='${sessionId}' GROUP BY gate, route"`,
        { encoding: 'utf8' }
      ).trim();
      if (rawGates) {
        gateFires = rawGates.split('\n').map((line) => {
          const [gate, route, count] = line.split('|');
          return { gate, route, count: Number(count) };
        });
      }
    } catch {
      // 兼容尚未建表的轻量环境
    }

    // 轨迹：agent_run_traces.steps_json 为整轮步骤 JSON 数组
    let traces = [];
    try {
      const rawSteps = execSync(
        `sqlite3 "${appDbPath}" "SELECT steps_json FROM agent_run_traces WHERE session_id='${sessionId}' ORDER BY id DESC LIMIT 1"`,
        { encoding: 'utf8' }
      ).trim();
      if (rawSteps) {
        const steps = JSON.parse(rawSteps);
        traces = steps.map((s) => ({
          step: s.id,
          tool: s.action,
          description: s.description,
          instruction: JSON.stringify(s.command || {}),
          output: s.output || '',
          status: 'done',
          metrics: s.metrics || {},
        }));
      }
    } catch {
      // 容错
    }

    return {
      main_prompt: Number(rawTokens[0] || 0),
      main_completion: Number(rawTokens[1] || 0),
      review_prompt: Number(rawReview[0] || 0),
      review_completion: Number(rawReview[1] || 0),
      review_runs: Number(rawReview[2] || 0),
      gate_fires: gateFires,
      traces,
    };
  } catch (e) {
    console.warn(`⚠️ 遥测查询提示: ${e.message}`);
    return empty;
  }
}

// ---- 生成 Markdown 报告 ----
function generateMarkdownReport(caseDef, result, telemetry, diffInfo, mode, calibration) {
  const timestamp = new Date().toISOString();
  const passBadge = result.passed ? '🟢 PASSED' : '🔴 FAILED';
  const totalPrompt = telemetry.main_prompt + telemetry.review_prompt;
  const totalCompletion = telemetry.main_completion + telemetry.review_completion;
  const calBadge =
    calibration.status === 'calibrated' ? '✅ 红基线校准通过' :
    calibration.status === 'skipped' ? '🟡 行为类判据（跳过校准）' : '⛔ 校准失败（考题无效）';

  return `# 🚀 真实 Git 历史自动化评测报告

- **考卷编号**: \`${caseDef.id}\` (${caseDef.tier.toUpperCase()})
- **考题名称**: ${caseDef.title}
- **基线 Commit**: \`${caseDef.base_commit}\`
- **生成时间**: ${timestamp}
- **评测模式**: ${mode.label}
- **综合判定**: **${passBadge}**
- **红基线校准**: ${calBadge} — ${calibration.reason || ''}

---

## 📊 一、核心体检卡 (Scorecard)

| 指标维度 | 实际测定值 | 验收基准 / 期望 | 状态 |
| :--- | :--- | :--- | :--- |
| **判据类型** | \`${judgeKind(caseDef)}\` | ${caseDef.test_patch ? `测试补丁 \`${caseDef.test_patch}\`` : '行为类判据'} | ℹ️ |
| **机械判定** | ${result.passed ? '通过' : `失败 — ${result.reason || ''}`} | FAIL_TO_PASS 转绿 | ${result.passed ? '✅ 达标' : '❌ 失败'} |
| **测试执行数** | \`${result.testsPassed ?? '—'} passed / ${result.testsFailed ?? '—'} failed\` | ≥1 passed 且 0 failed | ${result.testsPassed > 0 ? '✅' : '⚠️'} |
| **测试耗时** | \`${(result.durationMs / 1000).toFixed(2)}s\` | ≤ 校准/判分超时上限 | ✅ 正常 |
| **修改文件数** | \`${diffInfo.changedFiles.length}\` 个 | 预估 ≤ ${caseDef.expected_diff_files?.length || 5} 个 | ${diffInfo.changedFiles.length > (caseDef.expected_diff_files?.length || 5) ? '⚠️ 存在范围扩散' : '✅ 范围可控'} |
| **门禁拦截次数** | \`${telemetry.gate_fires.length}\` 次 | 0 次（或纠偏后收敛） | ${telemetry.gate_fires.length === 0 ? '✅ 完美合规' : '⚠️ 发生过干预'} |
| **Token 总开销** | \`${totalPrompt + totalCompletion}\` tokens | 主模型: ${telemetry.main_prompt}, 审查员: ${telemetry.review_prompt} | ℹ️ 见账单 |

---

## 🛡️ 二、门禁与遥测干预记录 (Gate Interventions)

${
  telemetry.gate_fires.length === 0
    ? '> ✅ **全程零门禁干预**：Agent 行为完全符合工具契约与安全规范，零违规操作。'
    : telemetry.gate_fires
        .map((g) => `- ⚠️ **门禁类型**: \`${g.gate}\` | **路由**: \`${g.route}\` | **拦截次数**: ${g.count} 次`)
        .join('\n')
}

---

## 📝 三、代码修改差异 (Git Diff)

${
  diffInfo.stat
    ? '```text\n' + diffInfo.stat + '\n```'
    : '> 本任务未产生任何代码修改（符合纯只读或未写入场景）。'
}

${
  diffInfo.patch
    ? `<details>\n<summary>🔍 点击展开查看完整 Patch 补丁</summary>\n\n\`\`\`diff\n${diffInfo.patch.slice(0, 15000)}\n\`\`\`\n</details>`
    : ''
}

---

## 🧠 四、黑匣子思考推演轨迹 (Thought Timeline & Actions)

${
  mode.isDryRun
    ? '> 🟡 **DRY-RUN 模式**：本轮执行红基线校准（注入测试补丁 → 验证判据在基线为红 → 还原），未消耗 LLM Token。'
    : telemetry.traces.length > 0
    ? telemetry.traces
        .map(
          (t) =>
            `#### 🔹 Step ${(t.step ?? '?') + 1}: \`${t.tool}\`\n` +
            `> ${t.description || ''}\n\n` +
            `<details>\n<summary>查看指令与输出详情</summary>\n\n\`\`\`json\n${t.instruction}\n\`\`\`\n\n**输出回执摘要**:\n\`\`\`text\n${(t.output || '').slice(0, 500)}\n\`\`\`\n</details>`
        )
        .join('\n\n')
    : '> 暂无轨迹明细记录（需要 --agent-session 指向在沙盒工作区作业过的会话）。'
}

---

## 🏁 五、机械判据输出片段 (Judge Output)

\`\`\`text
${result.stdoutSnippet || result.stderrSnippet || '（无控制台标准输出）'}
\`\`\`
`;
}

// ---- 主运行流水线 ----
async function runEvaluation() {
  const options = parseCliArgs();
  console.log(`\n======================================================`);
  console.log(`  🔬 Free-API 真实 Git 历史多 Agent 自动化评测套具`);
  console.log(`======================================================`);
  console.log(`模式: ${options.dryRun ? '🟡 DRY-RUN (红基线校准 + 沙盒验证)' : '🔵 LIVE (真实 Agent 判分)'}`);

  const cases = loadEvaluationCases(options.caseId);
  if (cases.length === 0) {
    console.log(`未找到匹配的考题。考题目录: ${casesDir}`);
    return;
  }

  console.log(`加载到 ${cases.length} 道考题:`);
  cases.forEach((c, idx) => console.log(`  ${idx + 1}. [${c.tier.toUpperCase()}] ${c.id} - ${c.title}`));

  fs.mkdirSync(evaluationsDir, { recursive: true });

  const summary = [];

  for (const caseDef of cases) {
    console.log(`\n------------------------------------------------------`);
    console.log(`▶ 开始测试: [${caseDef.tier.toUpperCase()}] ${caseDef.title}`);
    console.log(`  题面简述: ${caseDef.problem_statement.slice(0, 80)}...`);

    const sandbox = new WorktreeSandbox(caseDef);
    let sandboxPath = '';

    try {
      sandboxPath = sandbox.setup();
      sandbox.applyScaffold();

      // 第一步：红基线校准（证明判据在基线为红，杜绝假绿考题）
      const calibration = runCalibration(sandboxPath, caseDef);
      if (calibration.status === 'invalid') {
        console.log(`   ⛔ [Calibration] ${calibration.reason}`);
        summary.push({
          id: caseDef.id,
          title: caseDef.title,
          tier: caseDef.tier,
          passed: false,
          error: `考题校准失败: ${calibration.reason}`,
        });
        continue;
      }

      let result;
      let diffInfo;
      let telemetry;
      let mode;

      if (options.dryRun) {
        // DRY-RUN：校准即全部内容，不判分不消耗 Token
        mode = { isDryRun: true, label: '🟡 DRY-RUN 校准模式 (零 Token)' };
        result = {
          passed: calibration.status === 'calibrated' || calibration.status === 'skipped',
          reason: calibration.reason,
          exitCode: calibration.exitCode ?? null,
          testsPassed: null,
          testsFailed: null,
          durationMs: 0,
          stdoutSnippet: '',
          stderrSnippet: '',
        };
        diffInfo = inspectGitDiff(sandboxPath);
        telemetry = { main_prompt: 0, main_completion: 0, review_prompt: 0, review_completion: 0, gate_fires: [], traces: [] };
      } else if (options.live) {
        // LIVE 编排：唤起 wdio 实弹 spec（应用内后台评测跑在沙盒工作区），结束后注入卷子机械判分
        if (!options.model) {
          throw new Error('实弹运行需要 --model <模型UUID>（或环境变量 FREE_API_LIVE_MODEL_ID）');
        }
        const sessionFile = path.join(artifactsDir, `eval-session-${caseDef.id}-${Date.now()}.txt`);
        // 会话文件带时间戳唯一化：支持同一考题并行跑多组对照（A/B 不同模型配置互不覆盖）
        // 清除上一轮会话残留，防止判分指向旧会话
        if (fs.existsSync(sessionFile)) fs.rmSync(sessionFile);
        if (fs.existsSync(`${sessionFile}.status`)) fs.rmSync(`${sessionFile}.status`);
        const planMode = caseDef.plan_mode || '';
        const wdioEnv = {
          ...process.env,
          CARGO_TARGET_DIR: path.join(rootDir, 'src-tauri', 'target'),
          FREE_API_LIVE_MODEL_ID: options.model,
          ...(options.reviewerModel ? { FREE_API_LIVE_REVIEWER_MODEL_ID: options.reviewerModel } : {}),
          FREE_API_LIVE_FIXTURE_DIR: sandboxPath,
          FREE_API_EVALUATION_SCENARIO: 'custom_goal',
          FREE_API_EVAL_CUSTOM_GOAL: caseDef.problem_statement,
          FREE_API_LIVE_MAX_DURATION_MINUTES: String(options.durationMinutes),
          FREE_API_EVALUATION_PLAN_MODE: planMode,
          FREE_API_EVAL_SESSION_FILE: sessionFile,
        };
        console.log(`   🚀 [LIVE] 唤起实弹 Agent (model=${options.model.slice(0, 8)}, plan=${planMode || '默认'}, 上限 ${options.durationMinutes} 分钟)`);
        console.log(`   📂 [LIVE] 沙盒工作区: ${sandboxPath}`);

        // 确保应用二进制与当前代码一致——杜绝"改了代码忘重建，考卷跑在旧门禁上"的流程错误
        console.log(`   🔨 [LIVE] 重建应用二进制 (增量，通常 <1 分钟)...`);
        const buildRes = spawnSync('npm run test:e2e:build', {
          cwd: rootDir, shell: true, encoding: 'utf8',
          timeout: 20 * 60_000,
        });
        if (buildRes.status !== 0) {
          console.log((buildRes.stdout || '').split('\n').slice(-8).join('\n'));
          throw new Error(`应用构建失败 (exit=${buildRes.status})，取消本轮实弹`);
        }
        console.log(`   ✅ [LIVE] 应用已更新至当前代码`);

        const wdioStart = Date.now();
        const wdioRes = spawnSync(
          `npx wdio run wdio.conf.mjs --spec test/e2e/eval-case-agent.e2e.mjs`,
          { cwd: rootDir, shell: true, encoding: 'utf8', env: wdioEnv, timeout: (options.durationMinutes + 15) * 60_000 }
        );
        console.log(`   ⏱️ [LIVE] 实弹阶段耗时 ${((Date.now() - wdioStart) / 1000 / 60).toFixed(1)} 分钟 (wdio exit=${wdioRes.status})`);
        if (wdioRes.status !== 0) {
            console.log(`   ❌ [LIVE] wdio 失败输出尾部（诊断用）:`);
            console.log((wdioRes.stdout || '').split('\n').slice(-25).join('\n'));
            console.log((wdioRes.stderr || '').split('\n').slice(-15).join('\n'));
        }

        if (!fs.existsSync(sessionFile)) {
          throw new Error('实弹运行未产出会话 id（应用启动失败或评测启动被拒），详见上方 wdio 输出');
        }
        const sessionId = fs.readFileSync(sessionFile, 'utf8').trim();
        const traceStatus = fs.existsSync(`${sessionFile}.status`)
          ? fs.readFileSync(`${sessionFile}.status`, 'utf8').trim()
          : 'unknown';
        console.log(`   🧾 [LIVE] 会话 ${sessionId} 终态: ${traceStatus}`);

        mode = { isDryRun: false, label: `🔵 LIVE (model=${options.model.slice(0, 8)}, session=${sessionId}, 终态=${traceStatus})` };

        // agent 作业环境不含卷子：判分前才注入测试补丁（--3way 容忍 agent 编辑导致的上下文漂移，
        // 失败降级 -C1 宽松匹配；agent 若恰好写了同名测试，参考卷已用规格专属名避免撞车）
        const patchPath = testPatchAbsPath(caseDef);
        if (patchPath && fs.existsSync(patchPath)) {
          console.log(`   📥 [Judge] 注入判据测试补丁: ${path.basename(patchPath)}`);
          try {
            execSync(`git apply --3way "${patchPath}"`, { cwd: sandboxPath, stdio: 'pipe' });
          } catch {
            console.log(`   ↩️ --3way 未命中，降级 -C1 宽松匹配重试`);
            execSync(`git apply -C1 "${patchPath}"`, { cwd: sandboxPath, stdio: 'pipe' });
          }
        }

        const runResult = runCommand(sandboxPath, caseDef.verify_command, JUDGE_TIMEOUT_MS);
        result = {
          ...evaluateJudge(caseDef, runResult),
          durationMs: 0,
          stdoutSnippet: runResult.stdout.slice(-2000),
          stderrSnippet: runResult.stderr.slice(-2000),
        };
        diffInfo = inspectGitDiff(sandboxPath);
        telemetry = querySessionTelemetry(sessionId);
      } else {
        // LIVE：必须有 agent 会话；判分前注入测试补丁（agent 作业环境不含卷子，防改卷）
        if (!options.agentSession) {
          throw new Error(
            '实弹判分需要 --agent-session <sessionId>：请先在应用中让 Agent 于沙盒工作区作业（工作区路径见上方沙盒目录），再把会话 id 传入判分。实弹调用接线（自动唤起 Agent）为下一步工作。'
          );
        }
        mode = { isDryRun: false, label: `🔵 LIVE (session: ${options.agentSession})` };

        const patchPath = testPatchAbsPath(caseDef);
        if (patchPath && fs.existsSync(patchPath)) {
          console.log(`   📥 [Judge] 注入判据测试补丁: ${path.basename(patchPath)}`);
          execSync(`git apply "${patchPath}"`, { cwd: sandboxPath, stdio: 'pipe' });
        }

        const runResult = runCommand(sandboxPath, caseDef.verify_command, JUDGE_TIMEOUT_MS);
        result = {
          ...evaluateJudge(caseDef, runResult),
          durationMs: 0,
          stdoutSnippet: runResult.stdout.slice(-2000),
          stderrSnippet: runResult.stderr.slice(-2000),
        };
        diffInfo = inspectGitDiff(sandboxPath);
        telemetry = querySessionTelemetry(options.agentSession);
      }

      // 生成报告
      const reportMd = generateMarkdownReport(caseDef, result, telemetry, diffInfo, mode, calibration);
      const reportJson = {
        schema_version: 2,
        case_id: caseDef.id,
        tier: caseDef.tier,
        base_commit: caseDef.base_commit,
        judge_kind: judgeKind(caseDef),
        test_patch: caseDef.test_patch || null,
        calibration,
        passed: result.passed,
        judge_reason: result.reason || '',
        exit_code: result.exitCode,
        tests_passed: result.testsPassed,
        tests_failed: result.testsFailed,
        duration_ms: result.durationMs,
        diff_changed_files: diffInfo.changedFiles,
        telemetry,
        is_dry_run: options.dryRun,
        agent_session: options.agentSession || null,
        generated_at: new Date().toISOString(),
      };

      const outName = `real-eval-${caseDef.id}-${Date.now()}`;
      const mdPath = path.join(evaluationsDir, `${outName}.md`);
      const jsonPath = path.join(evaluationsDir, `${outName}.json`);

      fs.writeFileSync(mdPath, reportMd, 'utf8');
      fs.writeFileSync(jsonPath, JSON.stringify(reportJson, null, 2), 'utf8');

      console.log(`📄 报告已生成:`);
      console.log(`   - Markdown: ${mdPath}`);
      console.log(`   - JSON:     ${jsonPath}`);

      summary.push({
        id: caseDef.id,
        title: caseDef.title,
        tier: caseDef.tier,
        passed: result.passed,
        calibration: calibration.status,
      });
    } catch (e) {
      console.error(`❌ 考题执行异常: ${caseDef.id} - ${e.message}`);
      summary.push({
        id: caseDef.id,
        title: caseDef.title,
        tier: caseDef.tier,
        passed: false,
        error: e.message,
      });
    } finally {
      sandbox.cleanup();
    }
  }

  console.log(`\n======================================================`);
  console.log(`  🏁 自动化评测汇总看板 (Summary Scorecard)`);
  console.log(`======================================================`);
  for (const s of summary) {
    const mark = s.passed ? '🟢 PASS' : '🔴 FAIL';
    const cal = s.calibration ? `[${s.calibration}]` : '';
    console.log(`  [${s.tier.toUpperCase()}] ${s.id.padEnd(36)}: ${mark} ${cal} ${s.error ? `(${s.error})` : ''}`);
  }
  console.log(`======================================================\n`);
}

runEvaluation().catch((e) => {
  console.error('Fatal Runner Error:', e);
  process.exit(1);
});
