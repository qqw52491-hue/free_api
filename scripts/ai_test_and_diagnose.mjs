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

function clipLine(line, maxLen = MAX_LINE_LEN) {
  if (!line) return '';
  return line.length <= maxLen ? line : line.slice(0, maxLen) + '...[TRUNCATED]';
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
      resolve({ code: code ?? 1, outputTail: tail });
    });

    child.on('error', (err) => {
      console.error('[Flywheel] ❌ 进程启动异常:', err);
      resolve({ code: 1, outputTail: err.message });
    });
  });
}

async function main() {
  const result = await executePhysicalTest();

  if (result.code === 0) {
    console.log('\n[Flywheel] ✅ 物理实测全部通过！退出码: 0，系统运行正常，无需自愈干预。');
    process.exit(0);
  }

  console.warn(`\n[Flywheel] ⚠️ 实测出现断言失败或报错 (退出码: ${result.code})，正在将数据收敛落盘至单个全息胶囊...`);
  fs.mkdirSync(CAPSULE_DIR, { recursive: true });

  const capsulePath = path.join(CAPSULE_DIR, 'capsule_latest_failed.json');
  
  const capsuleData = {
    capsule_version: '1.0.0',
    generated_at: new Date().toISOString(),
    status: 'failed',
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
    },
  };

  const jsonStr = JSON.stringify(capsuleData, null, 2);
  fs.writeFileSync(capsulePath, jsonStr, 'utf-8');

  console.log(`[Flywheel] 📦 运行全息单文件已就绪: ${capsulePath}`);
  console.log(`[Flywheel] 🤖 AI Agent 现在可以调用 read_text_file('${capsulePath}')，在单次注意力窗口内完成完整归因并提出角色卡提示词升级建议！`);
}

main().catch((err) => {
  console.error('[Flywheel] 致命异常:', err);
  process.exit(1);
});
