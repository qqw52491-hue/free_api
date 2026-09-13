# OVERNIGHT_LOOP — 控制面改动披露台账

> 纪律：每项控制面改动（门禁/判据/收割/预算/审查契约）在此登记：改了什么、为什么、commit、验证状态。
> 状态标注：code-green（单测/编译过）| live-green（实弹验证过）| calib-green（基线校准过）| unverified（未验证）

## 阶段四：四项按序执行中

### ① reap_stale_child_runs 归档链修复 — `571b341` ✅ code-green

**问题**：reap 把 running 子行打成 `timed_out` 终态+占位 result_json 后，`finish_child_run` 的
`status='running'` 守卫会把真实迟到结果永久丢弃——复盘时只见收割 stub 不见真实产出，
归档元数据事实断链（case_01 曾在 22 步内触发 reap）。

**修复**（只做标记，不撕关联）：
- reap 的 `error_kind` 从 `timed_out` 改为 `reaped_stale`，与真实流式超时可区分；
- 每次收割打 `[reap]` 清理日志（child_run_id/kind/age）；
- `finish_child_run_with_history` 守卫放宽为 `status='running' OR (status='timed_out' AND error_kind='reaped_stale')`：
  收割行允许迟到真实结论落库覆盖 stub；cancelled/interrupted/真实超时依旧拒绝覆盖（取消语义不变）；
- 物理删除仍只属于 `delete_session_completely` / `cleanup_orphaned_session_data`（会话寿命边界未变）。

**验证**：新增回归 `reaped_row_accepts_late_result_and_preserves_metadata`
（收割→迟到落库→status/result_json/parent_session_id/started_at 全链可查）；
`agent::` 549 测试全过。**live 验证待④实弹**。

### ② max_steps/token 上限管道 — `3d3e17c` ✅ code-green

**管道**：case JSON `max_steps`/`max_prompt_tokens` → eval_runner 环境变量
（`FREE_API_EVAL_MAX_STEPS`/`FREE_API_EVAL_MAX_PROMPT_TOKENS`）→ E2E IPC 参数
（`maxSteps`/`maxPromptTokens`）→ `start_background_agent_evaluation` →
`run_agent_main_loop_in_workspace(run_budget)` → `AgentRunBudget{max_steps,max_prompt_tokens}`。

**收敛语义**：`while step_id < max_steps`（覆盖原常量 300）；token 计量在
`outcome` 定案处累计（含门禁打回的 finish 步——被拒步骤的 LLM 消耗也烧预算）；
token 上限在步骤边界检查，触发即 break → failed 终态 + completion summary
写明"步数上限"还是"token 上限 N"（报告可区分触发源）。

**考卷预算校准**（原 15-20 步是未接管的装饰值，case_04 实测需 92 步）：
case_01 280步/30M（r3 实测 220 步/21.7M）、case_02 40/6M、case_03 60/8M、
case_04 150/15M、case_05 100/12M、case_06 40/6M、case_07 120/12M、case_08 40/6M。

**注意**：token 上限只管主循环 prompt_tokens；reviewer/worker 子代理消耗另计
（agent_child_runs 表），后续如需全链路预算再扩。
验证：`budget_tests` 4/4 + `cargo check`。**live 验证待④**。

### ③ 判分精确 session_id + command 红基线 — `100fc19` ✅ code-green

**精确绑定**：`runCommand` 增 `extraEnv`，实弹判分注入
`FREE_API_EVAL_AGENT_SESSION=<runner 已知 sessionId>`；case_04–08 判据内
`(SELECT session_id ... LIKE 'agent-evaluation-%' ORDER BY started_at DESC)`
子查询全部替换为 `'$FREE_API_EVAL_AGENT_SESSION'` 字面绑定（并行对照不再串会话）；
telemetry `ORDER BY id DESC` → `started_at DESC`；E2E 终态轮询同步修正。

**command 红基线**：校准阶段以空 session 跑判据——基线无会话无 diff，
判据必须为红（exit≠expected）才允许开考；基线为绿 = 恒真/空判据 → 考题 invalid。
负向题例外：考卷可声明 `baseline_expectation:"green"`（case_02 "零改动即正确"已声明）。
**验证**：case_06 判据手工双测——假会话→exit=1（红），真成功会话+事务保留→exit=0（绿）。
**live 验证待④**。

### ④ case_01 ×3 同配方方差 — ✅ live-green（3/3 PASS）

配置：main=34217be8（3.8 Flash）/ reviewer=27d56872（3.1 Pro）/ cross_review / 40min / 280步·30M 上限。
探针 HTTP 401 可达后开考；每轮 cargo_test 红基线校准通过（calibrated）。

| 轮 | 步数 | 主 prompt tokens | reviewer prompt | 终态 | 判分 |
|---|---:|---:|---:|---|---|
| r1 `4de3f71d` | 182 | 19,180,261 | 23,251 | cancelled（40min 时限） | ✅ PASS |
| r2 `46025263` | 121 | 9,434,513 | 7,661 | succeeded | ✅ PASS |
| r3 `9a6135ef` | 146 | 12,376,883 | 30,347 | succeeded | ✅ PASS |

**方差（n=3 初始样本，不构成统计显著）**：
- 步数：min 121 / max 182 / 均值 149.7 / 极差 61（max/min=1.50×）
- 主 prompt：min 9.43M / max 19.18M / 均值 13.66M / 极差 9.75M（max/min=2.03×）
- PASS：3/3

**观测备注**：
- r1 达 40 分钟时限被评测器取消（"用户请求停止"安全边界），但 worktree diff
  当时已满足测试补丁 → cargo_test 判据 PASS。cargo_test 判据不查终态
  （command 判据③后查 succeeded）；若要对"时限内完成"计分需另加终态断言。
- 步数/token 上限本轮均未触发（r1 先撞时限）；预算机制仅 code-green。
- r3 reviewer 抖动大（failed+invalid_output+timed_out+running+succeeded 并存），
  failover 机制被重度使用仍收敛——①的 reaped_stale 标记本轮未出现
  （timed_out 均为 error_kind=timeout 真实流超时，非收割）。

---

## 阶段三存档（上一阶段，已验证）

| commit | 改动 | 验证 |
|---|---|---|
| `9a27cc0` | watch_stop `?3→?2` 参数错位修复 | live-green（case_01 step77 裸调成功） |
| `0e0e200` | 判据方向化/危害性裁决/零改动短路/跨域零命中兜底 | live-green（5/5 PASS） |
| `f41782e` | 判据 run 关联改 `started_at DESC`（UUID id 无序） | live-green |
| `c3f8163` | 终审 reviewer 补危害性裁决 + 判据② diff 方向断言 | live-green |
