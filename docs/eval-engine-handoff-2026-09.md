# 评测引擎完整操作手册与历史档案（2026-09-12）

> 本文是多 Agent 评测引擎的**唯一权威文档**：架构、操作、排查、考卷编写、实测全历史、设计决策记录。
> 新会话/新人接手：**先读第零节（第二期增量）**，再读本文第一、二节即可上手；遇到问题查第四节排查表；要改行为先读第八节不变量。

---

## 零、第二期交接增量（2026-09-13，接手必读）

> 第一期交接（本文原内容）之后，当晚完成了核心目标验证 + 12 个提交（`feb6a34..92c1f8d`）。三份活文档：本文、`agent_evaluations/OVERNIGHT_LOOP.md`（过程台账/战报日志）、`agent_evaluations/主脑横评战报-2026-09-13.md`。

### 0.1 核心目标已验证 ✅（有证据链）

**头部 agent 规划 + 尾部多审查员并发 + 复杂度自适应**实测成立：

- Tech Lead 单审定级后判"单人评审无法兜底，必须升级会审"（escalate 分支）→ 主脑经 `child_reviewer` 的 **dispatches 数组并发派发** architect+performance_expert（**两个子运行相差 2ms 启动、执行时间重叠**=真并发）→ 面板 3/3 识雷（埋雷题 case_04：诱导"checkpoint 异步批量刷盘"，雷=崩溃丢状态+竞态）→ 主脑按意见修订 → 复审 0 findings → 放行交付。完整闭环无死锁。
- 复杂度自适应实测：简单方案走 1v1+对质闭环（不开会审），高风险方案才 escalate——按 TL 判决分流。
- 并发 vs 1v1 的定量结论**还没做**（需要埋雷题库×受控 A/B，见 0.5）。现有定性结论：面板在 TL 类别级发现之上补出具体竞态路径与崩溃一致性视角；1v1 也不弱（今晨同题 1v1 也点到竞态）。

### 0.2 测试成绩（4/4 全绿）

| 考题 | 判定 | 要点 |
|---|---|---|
| case_01 gold | 🟢×2 | 基线 24.5min/838万tok/119 步；二期复跑 105 调用 |
| case_02 bronze | 🟢 | 零 diff（只读合规） |
| case_03 silver | 🟢 | 盲写门禁实弹拦截 3-4 次 |
| case_04 silver（新增埋雷题） | 🟢 | 判据=审查员识雷关键词检索（command 类） |

### 0.3 十二个提交（按主题分组）

**韧性（防"随机停摆/整轮判死"）**：
- `ada06ab` plan+exec 外层重试（3 次+退避+中断豁免）——原注释承诺的循环不存在，单次 call_llm 硬失败曾整轮判死
- `7142538` 压缩摘要请求整体 90s 限时（body 读取曾裸奔挂死整轮）
- `f96d417` **monitor 快照轻量化（重要）**：原每 1s 全量反序列化 trace+checkpoint，长会话后期 22 个并发 tick 打爆 DB 连接池、主循环饿死——所有"随机停摆"的第一主凶。现 SQL 直取状态字段
- `1749cd2` 构建失败输出 spawn error/信号（exit=null 观测盲区）
- `f49b50b` 实弹前 LLM 代理预检（反代宕机快速失败）

**模型适配（接非 gemini 模型必读）**：
- `9dc3947` claude-opus-4.x 归入 ClaudeThinking 家族（top_p<0.95 被 Anthropic 400）
- `d4dd47c` 交替角色自动探测补家族维度：**中转站 base_url 不含 "anthropic" 时按模型家族归并连续同角色消息**——不归并会被转换层注入无签名 thinking 块→400 signature required（平台表 `requires_alternating_roles` 列可显式指定，Antigravity 已置 1）
- `865828a` 审查员超时模型感知：Claude thinking 120s，其余默认 60s（`78f8bec`），env `FREE_API_REVIEWER_TIMEOUT_SECS` 可覆盖
- `452603e` **会话内检索去重**：同参成功 search 物理短路复用结果（edit/write 失效），长会话重复检索的确定性解法
- `92c1f8d` 评测 spec 先等 Tauri IPC 桥就绪再 executeAsync（窗口注入竞态）

**新能力**：
- `08c2c0b` 埋雷考卷 case_04（诱导危险设计 + 审查员识雷率判据，参考 §6.4）

### 0.4 运营与排障新知识（血泪教训）

1. **models 表 status 字段是陈旧缓存**，"offline"不代表不可用——以 curl 探针实测为准；
2. **每次实弹前先探针对应通道**（一次 10-token 请求）：反代账号池夜间会配额枯竭（共享池全线 429，账号锁定 300s~2h），这是所有"随机停摆"的总根源之一；另一主凶见 `f96d417`；
3. **停摆诊断方法论**：应用请求归档在 `agent_logs/<session_id>.json`（含完整 messages_sent_to_model）；冻结时 macOS `sample <pid> 10` 抓线程栈直击阻塞点；反代日志 `docker logs antigravity-manager` 看 429/轮换；
4. OrbStack/Docker 未运行 → Antigravity 反代全停 → 一切 LLM 调用挂死（curl 127.0.0.1:8045 即测）；
5. pkill 安全规则：只动 `target-eval` 路径的进程，用户的 `target/debug/free-api-chat` 实例不能碰；
6. glm-5.2 / kimi-k3 在反代里**没配账号**，sensenova/glm-5.3 与 gemini 共池——目前池里实际只有 gemini 系 + claude（额度用户说了算）。

### 0.5 待办（接手后按价值排序）

1. **埋雷题库**：再造 3-4 道不同风险类别的埋雷题 → 出"识雷率×并发规模×分歧率"矩阵（并发审查深度的完整体检，零外部依赖）；
2. **并发 vs 1v1 受控 A/B**：同题强制 1v1（角色卡钉死）vs 默认（允许 escalate）×N 次，定量回答"并发好多少"；
3. **主脑/审查员横评**：卡在反代账号池扩容（需要 glm 或 sensenova 独立配额），通道探针通过即可跑；
4. **长程大题（P3）**：先上 sccache 压编译预算（edit→test 循环 1-2min 是长程题瓶颈）；
5. 反代侧 bug 待上游修：无签名 thinking 块注入（已在客户端绕过）。

---

## 一、系统架构总览

### 1.1 多 Agent 拓扑

```
主脑（main_loop.rs，每步 call_llm → run_tool，硬上限 300 步）
 ├── child_reviewer（L1）：只读方案/代码审查，单次 LLM 调用，45s 超时
 ├── child_worker（L2）：只读侦察，10 步 / 180s，命令白名单
 └── child_writer（L3）：隔离工作区副本写码 + 跑测试，每 run 限 1 个
子代理不可递归派发；并发上限：全局 6、单类 2（角色卡 max_concurrency 可覆盖）
```

### 1.2 三层门禁（写前 → 写中 → 写后）

| 层 | 实现 | 拦什么 | 路由前缀 |
|---|---|---|---|
| 写前方案门禁 | `plan_gate.rs::check_pre_write_gatekeeper` | 未评审就写核心代码 / 全员审查失败 / 分支顺序错误 | `plan_gatekeeper:*`、`stage_review:blocking_open` |
| 写中阶段审查 | `stage_review.rs` | todo 阶段完成语义 / 累计写文件数触发，blocking 未解决锁写入 | `stage_review:*` |
| 收尾审查 | `reviewer_gatekeeper.rs::evaluate_finish_diff` + `review_contract.rs` + `finish_guard/` | diff 证据三分法、契约义务闭合、范围漂移提示、机械判卷兜底 | `finish_review:*`、`finish_obligation_evidence:*` |

门禁哲学三句话：**物理事实归门禁，语义判断归审查，监管变化归披露。**

### 1.3 评测引擎组件

| 文件 | 职责 |
|---|---|
| `scripts/eval_runner.mjs` | 编排器：沙盒→校准→自动重建应用→实弹→注卷判分→报告。`--live`/`--dry-run`/`--all`/`--case`/`--model`/`--reviewer-model`/`--duration`/`--agent-session` |
| `test/eval_cases/*.json` | 考卷定义（base_commit、problem_statement、verify_command、judge_kind、test_patch、scaffold_patch、expected_diff_files） |
| `test/eval_cases/patches/*.patch` | 脚手架补丁（随环境预置）+ 判据测试补丁（判分前注入） |
| `test/e2e/eval-case-agent.e2e.mjs` | wdio 实弹 spec：唤起应用内后台评测（custom_goal × 沙盒工作区），轮询终态 |
| `scripts/eval_analyze.py` | 过程态分析器：任意会话的轨迹/思维/协作/门禁/收敛全量报告 |
| `/tmp/eval_watch.py` | 实时监听器：5 秒采样落盘，含死循环检测（⚠️ 连续同描述 ≥5 次标记，edit 步骤执行超 5 秒会误报，正常现象） |
| `/tmp/overnight_eval.sh` | 夜间连跑：套件 ×3 串行，日志 `/tmp/overnight_eval.log` |
| `src-tauri/src/agent/finish_guard/state_updates.rs` | `apply_todo_update_guarded`：监管继承守卫（[审查发现] 待办禁止静默剔除） |
| `src-tauri/src/agent/telemetry.rs` | `record_gate_fire` / `GateFireRecord`（run_id + session_id 双键） |
| `data_v2.db` 核心表 | `agent_gate_fires`（门禁触发）、`agent_llm_calls`（逐调用账单）、`agent_child_runs`（子代理运行）、`agent_run_traces`（steps_json 全轨迹）、`agent_checkpoints`（状态机快照） |

---

## 二、快速上手（5 分钟跑通第一轮）

### 2.1 前提

- 应用可构建：`npm run test:e2e:build`（增量 <1 分钟；runner 实弹模式会**自动重建**，无需手动）
- 模型已配置在 `data_v2.db` 的 models 表。常用 UUID：
  - `34217be8-d0a3-45fe-aba5-d62a4d21c486` = gemini-3.8-flash-high（主力主脑）
  - `27d56872-9156-4751-bcdd-db9237b2222f` = gemini-3.1-pro-high（审查员/主力对照）
- 角色卡自动预装（首次 init_db 时）：`tech_lead`（含审查清单 v2）+ `architect` / `security_specialist` / `performance_expert`（联合会审专家）

### 2.2 跑单题（gold 回归基准）

```bash
npm run eval:real -- --live --case case_01_subagent_failover \
  --model 34217be8-d0a3-45fe-aba5-d62a4d21c486 \
  --reviewer-model 27d56872-9156-4751-bcdd-db9237b2222f --duration 40
```

### 2.3 跑全套件

```bash
npm run eval:real -- --live --all \
  --model 34217be8-d0a3-45fe-aba5-d62a4d21c486 \
  --reviewer-model 27d56872-9156-4751-bcdd-db9237b2222f --duration 40
```

### 2.4 只校准不实弹（零 Token，验证考卷有效性）

```bash
npm run eval:real:dry
```

### 2.5 跑完看什么

1. 终端尾部的汇总看板（每题 🟢/🔴 + calibrated 标记）；
2. `agent_evaluations/real-eval-<case>-<ts>.md`：核心体检卡 + 门禁干预记录 + diff + 轨迹；
3. 过程态深分析：

```bash
python3 scripts/eval_analyze.py <session_id>    # session_id 在同名 .json 里
```

---

## 三、评测管线每一步发生什么（理解才能排查）

```
1. WorktreeSandbox.setup()
   git worktree add --detach test-artifacts/worktrees/<case>-<ts> <base_commit>
   → 物理隔离，agent 在基线代码上从零修 bug（考卷不泄题）

2. sandbox.applyScaffold()
   应用脚手架补丁（scaffold_patch）——考卷给定条件的预置代码
   （如 case_01 的 failed_run_ids 簿记方法；不含题眼逻辑）

3. runCalibration() 红基线校准 ★ 有效性核心
   注入判据测试补丁（test_patch）→ 跑 verify_command → 判据必须为红：
   - 红（测试失败/超时）   → 校准通过，还原补丁，开始实弹
   - 绿（基线即通过）      → 考题作废（假绿），禁止开考
   - 编译失败              → 考题作废（补丁与基线不兼容）
   这一步杜绝"什么都不做也能 PASS"的假绿考卷（历史上抓出过）

4. LIVE 实弹：自动重建应用二进制（防旧二进制考旧卷）→ wdio 唤起应用 →
   invoke start_background_agent_evaluation（custom_goal × workspaceOverride=沙盒）→
   agent 在沙盒里读码/修码/自测 → 终态（succeeded/failed/cancelled）

5. Judge 判分：注入判据测试补丁（--3way，失败降级 -C1）→ 跑 verify_command →
   判定标准：退出码 0 且至少 1 个测试执行且 0 失败（防"0 匹配假绿"）

6. 报告：agent_evaluations/real-eval-<case>-<ts>.{md,json}
   JSON 含 judge_reason/tests_passed/diff_changed_files/telemetry(gate_fires 全量)
```

### 3.1 三分法证据校验（`finish_guard/diff.rs`，防撒谎 writer 的核心）

| 场景 | 判定 |
|---|---|
| 有真实 diff | 义务证据行号必须落入修改行（严格对账） |
| 无 diff + 未声称修改 | 诊断通道：引用静态可定位即可（文件存在 + 行号在范围内） |
| 无 diff + 声称"已修复/已修改" | **照常拦截**（撒谎 writer 不因无 diff 放行） |

驳回消息自带：**diff 覆盖地图**（file:[起-止] 区间）+ **逐文件照抄建议**——模型照抄即可修正，把"39 次驳回循环"压成"1 次修正"。

### 3.2 监管继承守卫（`state_updates.rs::apply_todo_update_guarded`）

`[审查发现]` 前缀的待办是审查环节的监管产出：todo_update（全量覆盖语义）中未保留的发现**自动恢复为 pending** 并附恢复告警；销账唯一途径 = 保留条目并显式置 `done`（附处理说明）。两条应用路径（loop_runner 状态更新、main_loop 证据回填）统一走 `apply_todo_update_guarded`，无旁路。

---

## 四、监控与排查手册

### 4.1 监听器（实时实况）

```bash
python3 /tmp/eval_watch.py        # 5 秒采样 → /tmp/eval_watch.log；终态自动退出
tail -f /tmp/eval_watch.log       # 人工实时盯
```

每行格式：`时间 | 状态 | calls=N | fires=N | N步 | 最新: <步号.动作 描述>`。
`⚠️疑似循环` = 同描述连续 ≥5 次——**edit 步骤执行超 5 秒会误报**，真循环的特征是"连续多个已完成步骤同描述"。

> 监听器只允许一个实例：多个实例会往同一日志互相穿插。僵尸实例用 `pkill -f eval_watch` 清理。

### 4.2 进程存活判据（pgrep 不可靠，用 ps）

```bash
ps aux | grep -cE 'free-api-chat$'     # 应用进程数
ps aux | grep -cE 'eval_runner.mjs'    # runner 进程数
ps -o pid,lstart,command -p <pid>      # 带启动时间的精确定位
```

### 4.3 常见故障排查表

| 症状 | 原因 | 处置 |
|---|---|---|
| 应用秒退（spawn 后即死） | init_db 迁移顺序错误（索引引用未补列） | 查启动日志 panic；索引必须后置于 ALTER |
| wdio exit=1、0 实弹 | 应用启动失败 / 端口 4445 被占 | `pkill -f free-api-chat` 清僵尸后重跑；runner 已打印输出尾部 |
| 会话文件不出现在 | 应用在启动 / wdio 连接中 | 等 2 分钟；超时查 wdio 输出尾部 |
| 报告 gate_fires 恒空 | 旧版按 run_id 查询的键错 | 已修（session_id 双键）；确认表结构含 session_id 列 |
| runner 日志长时间静默 | wdio 阶段输出被管道缓冲 | 正常；用 DB 查询（calls 是否增长）判活 |
| 证据驳回 39 次循环 | 行号漂移 + 无修正地图 | 已修：驳回附覆盖地图 + 逐文件照抄建议 |
| 全部失败引导被冷却拦 | 豁免链漏路由 | 已修：`reviewer_all_failed` 已入豁免链；新路由记得同步加入 |

### 4.4 数据库速查（data_v2.db，只读方式打开）

```bash
# 门禁触发统计（哪些机制在真实工作）
sqlite3 "file:$HOME/Library/Application Support/free-api-chat/data_v2.db?mode=ro" \
  "SELECT gate, route, COUNT(*) FROM agent_gate_fires GROUP BY gate, route ORDER BY 3 DESC;"

# 某会话的 Token 账单
sqlite3 "..." "SELECT COUNT(*), SUM(prompt_tokens), SUM(completion_tokens) FROM agent_llm_calls WHERE session_id='<sid>';"

# 子审查员运行情况
sqlite3 "..." "SELECT status, duration_ms FROM agent_child_runs WHERE parent_session_id='<sid>';"

# 全轨迹步骤（JSON 数组，含 action/command/output/metrics）
sqlite3 "..." "SELECT steps_json FROM agent_run_traces WHERE session_id='<sid>';"

# 持久化的门禁状态机（plan_mode/plan_phase/契约）
sqlite3 "..." "SELECT json_extract(snapshot_json,'$.plan_mode'), json_extract(snapshot_json,'$.plan_phase') FROM agent_checkpoints WHERE run_id='<sid>' ORDER BY rowid DESC LIMIT 1;"
```

注意：app 退出后 WAL 残留会导致 `mode=ro` 打开失败——先 `cp db db-wal` 到临时目录再查。

---

## 五、Python 工具详解

### 5.1 `scripts/eval_analyze.py`（过程态分析器）

```bash
python3 scripts/eval_analyze.py <session_id> [<session_id2> ...]
```

输出九段指标及解读：

| 指标 | 健康值（参考） | 异常信号 |
|---|---|---|
| 步数 | gold 90~180 | 反复同一动作无推进 = 打转 |
| prompt tokens | 300 万~2000 万 | 与步数不匹配的暴涨 = 上下文泄漏 |
| 动作画像 | search/read 混合 + edit | 全 read 无 edit = 探索期未收敛 |
| 思考总时长/步均 | 步均 7~10s | 步均 >15s = effort 过高吃预算 |
| 读取 | ≤17 次 / ≤9 文件 | >30 次或单文件 >10 次 = 碎片化复发 |
| 检索重复 | <25% | >40% = 定位策略退化 |
| 交付尝试 | 1~3 次 | >4 次 = 契约闭合困难 |
| 子 agent 派发 | 2~7 全成功 | 超时多 = 审查员模型/超时配置问题 |
| 门禁触发 | 有拦截 + 最终 PASS = 健康 | PASS 且 0 拦截（难题）= 门禁可能形同虚设 |

### 5.2 `/tmp/eval_watch.py`（实时监听器，5 秒采样）

- 自动找最新 `eval-session-*.txt`（nonce 感知）；
- 死循环检测：同描述连续 ≥5 次打 ⚠️（in-flight 步骤会误报，看趋势别看单点）；
- 终态自动退出并写 🏁 行。

> /tmp 会被清理——正式副本建议放 `scripts/eval_watch.py`（内容与 /tmp 版一致）。

### 5.3 `/tmp/overnight_eval.sh`（夜间连跑）

套件 ×3 串行，日志 `/tmp/overnight_eval.log`。清晨看三轮通过率 + 用时分布 = 基准稳定性数据。

---

## 六、考卷编写指南

### 6.1 考卷 JSON 逐字段

```json
{
  "id": "case_XX_名称",                       // 全局唯一
  "title": "一句话题目",
  "tier": "gold | silver | bronze",           // 难度分级
  "base_commit": "<bug修复提交>~1",            // 基线 = 修复的父提交（答案不在仓库里）
  "problem_statement": "【故障复现报障】只给症状，绝不给文件路径/行号/修法",
  "verify_command": "cargo test --manifest-path src-tauri/Cargo.toml --lib <判据测试名>",
  "judge_kind": "cargo_test",                 // cargo_test（至少1测试执行且全绿）| command（纯退出码）
  "test_patch": "test/eval_cases/patches/<case>.test.patch",   // 判据测试（判分前注入）
  "scaffold_patch": "test/eval_cases/patches/<case>.scaffold.patch",  // 可选：考卷脚手架
  "plan_mode": "cross_review",                // 可选：强制评审模式
  "expected_exit_code": 0,
  "max_steps": 15,
  "expected_diff_files": ["src-tauri/src/..."] // 范围扩散守卫参考
}
```

### 6.2 判据测试补丁的制作流程

1. 找到历史上修复该 bug 的提交 `FIX`；
2. `git show FIX -- <测试所在文件>` 提取**纯测试 hunk**（不含生产代码改动！）；
3. 若测试依赖 FIX 新增的生产 API → 把这些 API 作为**脚手架补丁**（scaffold_patch）一并预置，但**题眼逻辑必须留给考生**；
4. 生成后必须过红基线校准：在 base_commit 上应用全部补丁 → 判据必须为红。

### 6.3 规格书写原则（case_01 三轮失败换来的教训）

- 症状驱动：只给报错/现象，不给文件路径；
- **验收标准必须显式**：实现里隐含的行为契约（如"分支判定顺序"、"引导信息须含失败清单"）不写明，考生必然漏——三轮失败都栽在规格不可见上；
- 规格显式化不丢人：考卷测的是写明的契约行为，隐含细节的意外通过才是假信号。

### 6.4 埋雷题（测审查员质量）

题面诱导一个"看起来合理、实际有坑"的方案（例：把 checkpoint 同步持久化改为异步批量保存——雷：崩溃丢状态 + 竞态）。判据：审查员 findings 是否点名风险类别（从 child run 输出检索语义关键词）。用途：flash vs pro 审查员识破率对比 + 意见分歧仲裁链路压测。

---

## 七、九轮实测历史档案（2026-09-12 全天）

| 轮 | 配置变量 | 用时 | 终态 | tokens | 证据驳回 | 交付 | 判定 | 当轮诊断 |
|---|---|---|---|---|---|---|---|---|
| 1 | 无改进基线 | 25' 耗尽 | cancelled | 920万 | — | 0 | FAIL | 读碎片化(51次/6文件)、思考步均15s、零协作 |
| 2 | 读取纪律 v1 | 7.8' | succeeded | 233万 | — | 3 | FAIL | 分支未实现 → 规格缺口① |
| 3 | +遥测会话键 | 14.4' | succeeded | 573万 | 18 | 6 | FAIL | 同点复现 → 确认是规格缺口不是偶然 |
| 4 | +规格 v1（路由顺序） | 40' 耗尽 | cancelled | 1997万 | — | 0 | FAIL(卷子撞车) | 判卷补丁上下文失配 + 编译螺旋(68次shell) |
| 5 | +守卫降级/披露 | ~15' | succeeded | 409万 | 0 | 2 | FAIL | 路由对了，败在信息清单 → 规格缺口② |
| 6 | +规格补全 | 29.8' | succeeded | 1465万 | **39** | 6 | **PASS** | 证据循环磨过；暴露驳回体验问题 |
| 7 | +逐文件建议 | 13.2' | succeeded | 592万 | **0** | 2 | **PASS** | 建议机制生效；基准立住 |
| 8 | 审查员换 3.1-pro | 18.5' | succeeded | 554万 | 0 | **1** | **PASS** | pro 发现冷却豁免真缺口（flash 12 次未发现）；1 次审查员超时触发容灾 |
| 9 | 全量复跑 | 14.6' | succeeded | 579万 | 2 | 3 | **PASS** | 三连 PASS 基准稳固；证据驳回 39→2 |

（另有套件首跑：三题 3/3 全绿，case_01 40'、case_02 1.6'、case_03 11.8'。）

**七轮迭代修复的九类问题**（全部有提交，防回归清单见第八节）：
读取碎片化、重复读取盲区（local_file 不在拦截名单）、遥测键错（报告恒空）、迁移顺序崩溃、判卷撞车（--3way）、规格不可见（两层）、证据行号漂移、范围漂移误伤、shell 读取误用。

**关键行为学发现**：

1. **驳回次数不是质量指标**：第 5 轮 0 驳回但 FAIL（内部门禁落后于判卷），第 6 轮 39 驳回但 PASS（内部门禁严格）。要看"内部标准与外部判卷的对齐度"；
2. **通道置换**：堵住读取碎片化，模型转向疯狂检索（51 次）——行为漂移是常态，每个口子堵住后要观察新口子；
3. **审查员质量 = 审查员模型的函数**：flash 盖章式（12 次 0 发现），pro 结构化（5 次发现真缺口 1 个）。但 pro 更慢且有一次超时；
4. **探索型行为**：模型会无限探索不进入交付（105 步 0 派发）——预算上限是唯一的强制收敛手段；
5. **编译时间吃预算**：Rust 每次 edit→test 循环重编译 1~2 分钟，test-heavy 的迭代 40 分钟大半喂了 rustc。

---

## 八、设计决策记录（为什么这样设计）

1. **红基线校准**：判据在基线必须为红——杜绝"什么都不做也 PASS"的假绿考卷（第三轮曾实际抓出）；
2. **三分法证据校验**：有 diff 严格对账 / 无 diff 诊断通道 / 无 diff 且声称修改 → 拦。驳回是监管核心不能撤，优化方向是"每次驳回的修复成本"（覆盖地图 + 照抄建议）；
3. **监管继承守卫**：`[审查发现]` 待办禁止被 todo_update 静默剔除（全量覆盖语义的洞）；销账 = 保留条目 + 显式 done；
4. **控制面披露纪律**（替代已拆除的升级审批）：控制面改动无需审批但必须在交付说明中列出监管行为变化——单机单人场景下升级审批退化为全民打断；
5. **文件清单硬拦全面拆除**：排查是发现驱动的，人类审批方案时也无法预知全部涉改文件——清单降级为登记簿 + 审查重点提示；
6. **强制重建**：实弹前自动 `test:e2e:build`，根除"改码忘重建考旧卷"；
7. **脚手架与题眼分离**：判据测试依赖的簿记 API 可以随补丁预置（考卷给定条件），但题眼逻辑（状态语义变更）必须考生自己实现。

---

## 九、路线图（已与用户对齐）

- ✅ **P0 题库扩容**：三题套件 3/3 PASS（本文档撰写时）
- ⏭️ **P1 主脑横评**：同套件换主脑模型 UUID（3.1-pro / claude-opus-4.6 系 / deepseek），零代码成本，出《主脑横评战报》
- ⏭️ **P2 混编委员会**：flash+pro 混编攒自然分歧；不足再上埋雷题（诱导缺陷设计见 6.4 思路）
- ⏭️ **P3 长程收敛大题**：编译预算解决后做

---

## 十、关键不变量（任何改动前自检）

1. 门禁只守物理事实（退出码、diff、文件存在性），语义判断交给审查；
2. 判分只认物理退出码 + 测试执行数，绝不让 LLM 看 diff 打分；
3. `[审查发现]` 待办禁止静默剔除（监管继承守卫）；
4. 每次评测 fresh worktree + 零记忆 + 红基线校准（考卷不泄题、不假绿）；
5. 控制面改动必须在交付说明中披露；
6. 观测先于拦截：新门禁路径上线必须同步埋 `record_gate_fire`；
7. 台账要能退役：零触发的门禁路径连续多轮未触发即标记评估退役。
