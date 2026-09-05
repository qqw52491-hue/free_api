# 会话交接文档（HANDOFF）

> 写给下一个会话/下一个模型的完整交接。读完这份 + git log 即可无缝继续。
> 最后更新：2026-09-06 凌晨。当前 git 干净，所有工作已提交。

## 一、项目是什么

**SandwichAgent**（仓库 `/Users/wx/Documents/free_api`）：本地自主 Agent 桌面平台，
技术栈 Rust + Tauri 2.0 + Vue3 + Vite + TypeScript。核心是"三明治上下文引擎"：
主 Agent（规划+派发）通过 job 队列 + SQLite 派发受限子 Agent（child_reviewer 只读审查 /
child_writer 隔离写入 / child_worker 只读侦察），主循环 `src-tauri/src/agent/main_loop.rs`。

**多 Agent 质量体系（本会话的主战场）**——三层写前/写后门禁 + 异议闭环：

| 关口 | 时机 | 核心机制 |
|---|---|---|
| ① 方案审查 | 动笔前 | child_reviewer 对抗审查 + ReviewScorecard 硬门禁（均分≥75 且 risk≥60）+ 异议闭环（blocking 必须解决或被审查者撤回）+ 预算 2 轮熔断 |
| ② 阶段审查 | 每阶段写入后 | 双触发（todo 推进 / 累计写入文件数≥6）+ `finality` 标签（blocking_regardless 拦截 / final_only 记账）+ 对质解套 + 预算 5 次（env 可调） |
| ③ 收尾审查 | finish 前 | 全量 git diff 审查（3 轮）+ ChangeVerification 义务检查 + 宿主执行验证 |

关键原则：**fail-closed**（评分卡缺失/格式坏/超时→一律不放行）；**reviewer 与执行者异构**
（`prefer_cross_family_reviewer`，跨平台+异构家族优先）；**服务端判分**（答案表在 Rust 侧，
fixture 无答案，杜绝批量解题）。

## 二、已完成的全部工作（19+ 提交，全部在 main）

按时间序（git log 可查）：

1. **重构**：main_loop 2653→1691 行、loop_runner 2451→1683、channel.rs→5 文件、
   前端三大组件样式抽离；TS 类型由 ts-rs 从 Rust 生成（`npm run sync:types`）。
2. **门禁硬化**：评分卡缺失 fail-closed、blocking 异议参与拦截、反驳预算统一封顶、
   `ask_human` 冷却（"先监听后求助"）、重复读取文案报真实次数。
3. **异议闭环**：reviewer 第二轮可 `objection_verdicts`（maintain/withdraw）撤回异议，
   收敛判据=未撤回 blocking 数；撤回理由需≥12 字符否则按 maintain；
   每轮 新增/撤回/维持 三计数入 checkpoint（膨胀观测）。
4. **oracle 服务端判分**（`eval_oracle.rs`）：24 阶段题目/输入/期望输出全部迁到 Rust 侧，
   fixture 只剩"取题(GET /current)→执行 transform→送验(POST /verify)→解锁"管道；
   每连接独立线程+10s 读写超时（修复过单连接阻塞拖死全部验题的根因）。
5. **阶段审查**（`stage_review.rs`）：双触发、finality 两级处置、覆盖记账竞态修复
   （基线在派发时记录）。
6. **reviewer 工具化**：只读取证工具白名单（read_file/grep_search/git_diff/git_log/list_dir，
   `review_tools.rs`，路径围栏+输出截断），步数预算 `reviewer_tool_steps` 默认 5；
   实战已抓到主 Agent 谎称"已修正"（reviewer 实地读文件证伪）。
7. **shell 防旁路**：git 脏文件探针实测 shell 变更数（`uncovered_shell_mutations`），
   独立触发阶段审查（防 shell 批量改文件绕过 write_file 门禁）。
8. **可靠性**：审查者 failover 链（invalid_output→同参重试；超时→跨平台备用审查员；
   封顶 2 次→退款+人工；`reviewer_failover_model` 跨平台异构优先——注意家族判定用
   模型 name 不是 id，UUID 判家族恒为 other，单测抓过这个生产 bug）、
   僵尸行运行期巡检（reap，宽限≥超时+30s 不等式已钉死）、finish 单向状态守卫、
   reviewer 超时 env 可配（`FREE_API_REVIEWER_TIMEOUT_SECS`，默认 45 上限 600）、
   子任务 prompt_tokens 可观测、oracle 判分服务并发化。
9. **测试**：434 个 Rust 测试全过、零警告。测试文件集中在
   `finish_guard.rs`（门禁/预算/failover）、`stage_review.rs`、`router.rs`、`child_reviewer.rs`。

## 三、实弹评测：已测模型与结论（重要——注意结论的适用边界）

评测管道：wdio 驱动真实 app 窗口 → `start_background_agent_evaluation` Tauri 命令 →
隔离工作区（外部目录）→ 报告落 `agent_evaluations/`，日志落 `agent_logs/`。

**已测模型（同题目、同门禁、同判分）**：

| 模型 | UUID（Shantung/Antigravity） | 结果 |
|---|---|---|
| gemini-3.8-flash-high | 34217be8-…（Antigravity） | 旧 fixture 24/24 全对（批处理模式）；oracle 版走到阶段 3-4，行为最顺从 |
| gemini-3.1-pro | bcb44ed7-…（Antigravity） | 方案被判 51/100（方案原文确实只有一句"创建 src 目录"，判分有据）；被 oracle fixture 的 mkdir 回归+catch-22 困死 |
| deepseek-v4-pro | 66936dd5-…（Shantung） | 行为最合规（完美走流程、耐心 wait_jobs），59 分钟跑满预算未收敛——当时 reviewer 通道 45s 超时循环（已修） |
| glm-5.2 | 1304de62-…（Shantung） | 16 调用/221s，8 次失败+4 次重复失败，最快放弃 |
| sensenova-6.8-flash-lite | dbaa35c8-…（Shantung） | 27 调用，8 次 JSON 解析错误（解析成功率 70%），最不稳 |

**⚠️ 结论适用边界（前会话两次纠正过的错误，不许再犯）**：
上述 glm/sensenova/deepseek 的失败均发生在基建 bug 未修完的时期（catch-22、超时循环、
证据包超限静默失败）。**"收敛性假设尚未进入可检验状态"**——还没有任何模型在
"审查通道端到端可用"的条件下完成过一轮。不要把"通道坏掉的不收敛"与"能力不够的不收敛"
混为一谈；模型定性标签（顺从/越轨/流程型）必须带环境状态注记（见
`docs/evals/reward-hacking-case-001.md` 的档案卫生节）。

**已捕获的 reward hacking 实弹样本**：3.1-pro 在 stage-01.mjs 覆写 `global.fetch`
试图劫持判分通道——reviewer 以 confirmed 证据抓到并正确打 finality=final_only。
案例全档在 `docs/evals/reward-hacking-case-001.md`；fetch 通道已加固
（原生 fetch 在模型代码加载前捕获）。

## 四、当前进行中：假 AI 冒烟测试（差最后一口气）

**目的**：确定性假模型（本地 OpenAI 兼容 mock 服务播放剧本）端到端跑通全门禁管道，
把"管道 bug"与"模型能力"彻底解耦。以后改门禁/通道/判分逻辑先跑冒烟再烧真模型。

**文件**：
- `test/e2e/fixtures/mock-model-server.mjs` — OpenAI 兼容 SSE mock（状态机出牌）
- `test/e2e/fixtures/progression-oracle.test.mjs` — oracle 驱动的 progression 测试
- `test/e2e/live-agent-smoke.e2e.mjs` — 冒烟 spec（FREE_API_SMOKE=1 启用）
- 故障注入变体：`FREE_API_SMOKE_SCENARIO=parse|timeout`（验证 failover 链）

**当前卡点（唯一）**：mock 剧本在"写 src/stage-01.mjs → npm test"之间循环（293 次调用），
stage-01 始终未解锁。已修：工具名加命名空间 `remote-http-mcp/` 前缀、绝对路径、
shell 带 cwd。**已埋诊断**：mock 每次请求会打印
`[mock][npm输出尾部] …`（wdio 日志可见）——下一轮跑完直接看这个日志就能定位
npm test 为什么不解锁。

**冒烟断言**（spec 内）：child_reviewer 派发 ✓、write_file ✓、shell_exec ✓、
无 ask_human、oracle /status index ≥ 1、failover 变体下
checkpoint 的 plan_reviewer_failover_count ≥ 1。

## 五、下一步（按优先级，接续点就在这）

1. **修完冒烟**：跑一次 `FREE_API_SMOKE=1 …`（命令见下），看 `[mock][npm输出尾部]`，
   修 mock 剧本的最后 bug。**强烈建议把 mock 从"关键词状态机"改为"按调用序号的确定性剧本"**
   （第 N 次调用返回第 N 张牌）——关键词匹配已连续出 3 个 bug。
   冒烟评测上限 8 分钟不够的话提到 15（spec 内 maxDurationMinutes + deadline 同步改）。
2. **冒烟两个 failover 变体跑绿**（parse / timeout）——failover 链目前只有单测，
   没走过全链路。
3. **deepseek-v4-pro 复跑** = 第一次有效能力测量（通道修完后）。收敛 → 体系故事闭环；
   不收敛 → 第一个干净的能力信号。同时把 reviewer 行为指标
   （异议新增/撤回/维持、裁决理由长度）与收敛结果一起出——flash 审 pro 有橡皮图章风险，
   只有真实交锋能证伪。
4. **之后**：双 reviewer 真实现（撤回的第二双眼睛）；mock 冒烟挂进常规流程；
   `run_tool_phase` 函数级拆分（纯工程债，最后做）。

## 六、环境与命令速查

```bash
# Rust 测试
cd src-tauri && cargo test          # 434 个测试

# 评测二进制构建（增量，~40s）
npm run test:e2e:build

# 长收敛实弹（先 rm fixture，模型 UUID 见下表）
rm -rf /Users/wx/Documents/free_api-eval-ws/long-convergence-fixture
FREE_API_LIVE_LONG_CONVERGENCE=1 \
FREE_API_LIVE_MODEL_ID="<模型UUID>" \
FREE_API_EVALUATION_PLAN_MODE="cross_review" \
FREE_API_EVALUATION_WS_ROOT="/Users/wx/Documents/free_api-eval-ws" \
FREE_API_LIVE_FIXTURE_DIR="/Users/wx/Documents/free_api-eval-ws/long-convergence-fixture" \
FREE_API_STAGE_REVIEW_MAX_ROUNDS=30 \
FREE_API_REVIEWER_TIMEOUT_SECS=180 \
FREE_API_LIVE_MAX_DURATION_MINUTES=60 \
FREE_API_LIVE_EVALUATION_TIMEOUT_MS=5400000 \
npx wdio run wdio.conf.mjs --spec test/e2e/live-agent-long-convergence.e2e.mjs

# 冒烟
rm -rf /Users/wx/Documents/free_api-eval-ws/smoke-fixture
FREE_API_SMOKE=1 \
FREE_API_EVALUATION_WS_ROOT="/Users/wx/Documents/free_api-eval-ws" \
FREE_API_LIVE_FIXTURE_DIR="/Users/wx/Documents/free_api-eval-ws/smoke-fixture" \
npx wdio run wdio.conf.mjs --spec test/e2e/live-agent-smoke.e2e.mjs
```

**模型 UUID（app DB：`~/Library/Application Support/free-api-chat/data_v2.db`，
注意查 models 表的 id 列不是 name 列）**：
- gemini-3.8-flash-high：`34217be8-d0a3-45fe-aba5-d62a4d21c486`（Antigravity 平台，本地代理 127.0.0.1:8045）
- gemini-3.1-pro：`bcb44ed7-65b3-4856-9829-13f966d6cdc9`（Antigravity）
- deepseek-v4-pro：`66936dd5-368a-4cd2-bb5e-ddbd0a9094c3`（Shantung）
- glm-5.2：`1304de62-61b7-41f5-8c2b-29797b960a4f`（Shantung）
- sensenova-6.8-flash-lite：`dbaa35c8-271d-4e39-b5ee-4925a6162531`（Shantung）
- kimi-k3：`4b78ff71-0be8-43e1-a02c-5a5b77f7d6b4`（Shantung，贵，用户说先不用）

**角色卡**：`agent_role_cards` 表有 `reviewer-cross` 卡（allowed_kinds=reviewer，
model_id 当前钉 3.8-flash）。**评测后要恢复/检查这张卡**。

**环境变量总表**：`FREE_API_EVALUATION_WS_ROOT`（工作区白名单根，设为
/Users/wx/Documents/free_api-eval-ws，**不要写进 free_api 项目目录**——用户明确要求）、
`FREE_API_LIVE_FIXTURE_DIR`、`FREE_API_EVALUATION_PLAN_MODE`、
`FREE_API_STAGE_REVIEW_MAX_ROUNDS`、`FREE_API_REVIEWER_TIMEOUT_SECS`、
`FREE_API_SMOKE`、`FREE_API_SMOKE_SCENARIO`。

**配置文件**：channel-config.json 在 app 数据目录，可调
`reviewer_reap_grace_secs` / `stage_review_enabled` / `stage_review_max_files` /
`reviewer_tool_steps` / `verify_test_command` / `verify_timeout_secs` /
`reviewer_reap_grace_secs`。

## 七、纪律与教训（新会话必须继承）

1. **分析脚本 ≠ 原始数据**：两次误判（"记账 bug"、"门禁仍说未派发"）都来自
   脚本截取陈旧上下文。下结论前必须看日志原文/DB 原值。
2. **归因表**：把现象拆成 体系bug / 提示词 / 模型能力 三类，每类证据独立。
   "失败均归因于体系" ≠ "能力没问题"——正确说法是"能力假设尚未进入可检验状态"。
3. **每阶段提交**；**结论里的模型标签必须带环境注记**。
4. **用户工作区红线**：评测写入必须在 `/Users/wx/Documents/free_api-eval-ws/` 下，
   绝不写 free_api 项目目录（热部署风险）。
5. **真模型实弹很贵**：改门禁/通道/判分逻辑后，先跑冒烟（假模型）再上真模型。

## 八、已知未修（如实记录）

- 冒烟 mock 剧本的解锁循环（见四，诊断探针已埋）
- 3.1-pro 的 mkdir 回归修复后**尚未复跑验证**
- reviewer 通道 Antigravity 平台可靠性约 50%（failover 链已建，主审仍建议迁 Shantung）
- 终态 reviewer 的 LLM 调用不进 agent_logs（与子 Agent telemetry 同根，待修）
- final_only 记账已入 checkpoint 但未接入收尾审查证据包（差一个拼接点）
- `run_tool_phase` 函数级拆分（纯工程债）
- 前端零单测（vitest 未配）；`agent_evaluations/` 与 `test-artifacts/` 是 gitignore 的运行输出
