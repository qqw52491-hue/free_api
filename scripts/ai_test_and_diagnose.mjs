#!/usr/bin/env node
/**
 * ai_test_and_diagnose.mjs
 * 自动化测试执行、动态输出截断监听与单文件全息胶囊落盘协调器
 *
 * 核心能力：
 * 1. 物理测试不可变参数化执行（禁止 shell 解释，杜绝注入）；
 * 2. 输出动态行截断（<=300 chars）与尾部滑动窗口（<=80 lines），保底防爆上下文；
 * 3. 失败时将全生命周期数据打包为单文件胶囊：test-artifacts/capsules/capsule_latest_failed.json；
 * 4. AI Agent 可通过 read_text_file 单步读取该胶囊，在同一注意力窗口完成归因并提出升级补丁。
 *
 * 设计边界（重要）：
 * - 本脚本是「哑的物理执行器」，永远不内置 LLM 调用。诊断与打补丁由 Agent 会话驱动
 *   （那里有预算熔断/审查门禁/效能观测兜底），在脚本里再造 LLM 循环等于绕过全部门禁。
 * - 支持物理级多轮重试：--max-rounds N（默认 1）。注意：多轮重试只是「确认失败稳定复现」，
 *   不构成自愈——自愈动作必须发生在 Agent 会话里并经过审查门禁。
 *
 * 用法：
 *   node scripts/ai_test_and_diagnose.mjs                    # 单轮
 *   node scripts/ai_test_and_diagnose.mjs --max-rounds 3     # 最多 3 轮，全绿即停
 *   node scripts/ai_test_and_diagnose.mjs --max-rounds 3 --interval-secs 10
 */

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const WORKSPACE_DIR = process.cwd();
const TAURI_DIR = path.resolve(WORKSPACE_DIR, 'src-tauri');
const CAPSULE_DIR = path.join(WORKSPACE_DIR, 'test-artifacts', 'capsules');

// 宿主固定不可变测试命令（参数化直接派发，禁止 shell 注入）
const TEST_EXECUTABLE = 'cargo';
const TEST_ARGS = ['test', '--lib'];
const MAX_LINE_LEN = 300;
const MAX_TAIL_LINES = 80;

// ---- 参数解析（仅接受数字，杜绝注入面） ----
function parseArgs(argv) {
  let maxRounds = 1;
  let intervalSecs = 5;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--max-rounds') {
      const v = Number.parseInt(argv[++i], 10);
      if (Number.isFinite(v) && v >= 1 && v <= 20) maxRounds = v;
    } else if (a === '--interval-secs') {
      const v = Number.parseInt(argv[++i], 10);
      if (Number.isFinite(v) && v >= 0 && v <= 300) intervalSecs = v;
    }
  }
  return { maxRounds, intervalSecs };
}

function clipLine(line, maxLen = MAX_LINE_LEN) {
  if (!line) return '';
  return line.length <= maxLen ? line : line.slice(0, maxLen) + '...[TRUNCATED]';
}

// 从 cargo 输出提取失败测试名（"test xxx ... FAILED" 与 failures: 段）
function extractFailingTests(lines) {
  const names = new Set();
  for (const line of lines) {
    const m = line.match(/^test\s+(\S+)\s+\.\.\..*FAILED/i);
    if (m) names.add(m[1]);
  }
  return [...names];
}

// 启动物理进程执行测试并流式收集输出
async function executePhysicalTest() {
  const runCwd = fs.existsSync(TAURI_DIR) ? TAURI_DIR : WORKSPACE_DIR;
  console.log(`[Flywheel] 🚀 正在启动宿主物理测试: ${TEST_EXECUTABLE} ${TEST_ARGS.join(' ')} (工作目录: ${runCwd})`);

  return new Promise((resolve) => {
    const child = spawn(TEST_EXECUTABLE, TEST_ARGS, {
      cwd: runCwd,
      shell: false, // 严格禁用 shell 解析，防命令注入
    });

    const lines = [];
    let currentLine = '';

    function handleData(chunk) {
      const str = chunk.toString();
      process.stdout.write(str);

      for (const char of str) {
        if (char === '\n') {
          lines.push(clipLine(currentLine));
          if (lines.length > MAX_TAIL_LINES * 2) {
            lines.splice(0, lines.length - MAX_TAIL_LINES);
          }
          currentLine = '';
        } else {
          currentLine += char;
        }
      }
    }

    child.stdout.on('data', handleData);
    child.stderr.on('data', handleData);

    child.on('close', (code) => {
      if (currentLine) {
        lines.push(clipLine(currentLine));
      }
      const tail = lines.slice(-MAX_TAIL_LINES).join('\n');
      resolve({ code: code ?? 1, outputTail: tail, lines });
    });

    child.on('error', (err) => {
      console.error('[Flywheel] ❌ 进程启动异常:', err);
      resolve({ code: 1, outputTail: err.message, lines: [] });
    });
  });
}

const sleep = (secs) => new Promise((r) => setTimeout(r, secs * 1000));

async function main() {
  const { maxRounds, intervalSecs } = parseArgs(process.argv.slice(2));

  for (let round = 1; round <= maxRounds; round++) {
    if (round > 1) {
      console.log(`\n[Flywheel] 🔁 第 ${round}/${maxRounds} 轮物理复测（间隔 ${intervalSecs}s）...`);
      await sleep(intervalSecs);
    }

    const result = await executePhysicalTest();

    if (result.code === 0) {
      console.log(`\n[Flywheel] ✅ 物理实测全部通过！退出码: 0（第 ${round}/${maxRounds} 轮），系统运行正常，无需自愈干预。`);
      process.exit(0);
    }

    console.warn(`\n[Flywheel] ⚠️ 实测出现断言失败或报错 (退出码: ${result.code}, 第 ${round}/${maxRounds} 轮)，正在将数据收敛落盘至单个全息胶囊...`);
    fs.mkdirSync(CAPSULE_DIR, { recursive: true });

    const capsulePath = path.join(CAPSULE_DIR, 'capsule_latest_failed.json');

    const capsuleData = {
      capsule_version: '1.1.0',
      generated_at: new Date().toISOString(),
      status: 'failed',
      round: round,
      max_rounds: maxRounds,
      failing_tests: extractFailingTests(result.lines),
      physical_truth: {
        executable: TEST_EXECUTABLE,
        args: TEST_ARGS,
        exit_code: result.code,
        failure_tail: result.outputTail,
      },
      diagnostic_contract: {
        allowed_mutation_scope: ['agent_role_cards.persona_prompt', 'domain_checklists'],
        forbidden_scopes: ['hard_scope', 'allowed_kinds', 'default_test_command', 'verify_test_command'],
        regression_gate: 'all_golden_tests_must_pass',
        // 防退化三原则：补丁必须过审查门禁；补丁后必须重跑对应评测场景；失败即回滚。
        // 「改角色卡让测试变绿」而不重跑复评 = 用改卷子代替学习，系统将在几轮内报废。
        anti_cheat_rules: [
          'role_card_patch_requires_reviewer_audit（补丁说明必须论证修的是根因而非放宽判据）',
          'patch_must_be_validated_by_rerunning_the_failing_eval_scenario',
          'validation_failure_rolls_back_the_patch',
        ],
      },
    };

    const jsonStr = JSON.stringify(capsuleData, null, 2);
    fs.writeFileSync(capsulePath, jsonStr, 'utf-8');

    console.log(`[Flywheel] 📦 运行全息单文件已就绪: ${capsulePath}`);
    console.log(`[Flywheel] 🤖 AI Agent 现在可以调用 read_text_file('${capsulePath}')，在单次注意力窗口内完成完整归因并提出角色卡提示词升级建议！`);

    if (round < maxRounds) {
      console.log(`[Flywheel] ⏳ 将在 ${intervalSecs}s 后物理复测（仅确认失败是否稳定复现；诊断与修复请由 Agent 会话执行）。`);
    }
  }

  console.error(`\n[Flywheel] ❌ ${maxRounds} 轮物理实测均失败，胶囊已收敛。请由 Agent 会话读取胶囊归因，勿在本脚本内做任何"自动修复"。`);
  process.exit(1);
}

main().catch((err) => {
  console.error('[Flywheel] 致命异常:', err);
  process.exit(1);
});
