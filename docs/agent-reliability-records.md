# Agent 可靠性实测记录（持续更新）

> 用途：每轮「修改 → 真实评测 → 总结记录 → 与历史对比」循环的基线账本。
> 新的评测跑完后，在本文件追加一节，并与下方历史记录对比指标变化。
> 评测入口：`FREE_API_LIVE_MODEL_ID=<模型UUID> FREE_API_LIVE_WRITE_FIXTURE=1 npx wdio run wdio.conf.mjs --spec test/e2e/live-agent-model-regression.e2e.mjs`
> 报告落盘：`agent_evaluations/<evaluation_id>.json`（聚合指标）+ `agent_evaluations/monitor/`（逐步快照）。

## 记录 1：2026-08-29 · glm-5.2 @ Shantung · isolated_write_fixture

- **evaluation_id**: `agent-write-fixture-fa5e5292-9b8e-4898-82c6-07ae7b37a145`（run `fa3e944f`）
- **模型**: glm-5.2（UUID `1304de62-61b7-41f5-8c2b-29797b960a4f`，平台 Shantung / token.sensenova.cn）
- **场景**: 隔离写入 fixture（修复 `summarize-readings.mjs`，hidden assertion 验收，受保护文件不可变性校验）
- **结果**: **通过**（E2E ✓，trace `succeeded`，verdict `passes_observed_guardrails`，0 warnings）

| 指标 | 本次 (glm-5.2) | 历史对照 (sensenova-6.8-flash-lite, 2026-08-26) |
|---|---:|---:|
| LLM 调用 | 9 | 8 |
| 工具步骤 | 10 | 9 |
| 工具失败率 | 0% | 0% |
| JSON 解析成功率 | 100% | 100% |
| 重复读取 / 重复失败 | 0 / 0 | 0 / 0 |
| 缓存命中率 | 81.7% | 29.5% |
| 峰值上下文占用 | 16.2% | 13.3% |
| 观测时长 | 247s | 104s |

**行为亮点**（来自逐步轨迹）：
- 自行设计 11 项边界检查脚本（clamp、空 id、非有限值、输入不可变性、top3 排序 tiebreaker 等），超出可见测试要求——复杂交付门控（ReviewContract）在真实运行中生效的直接证据。
- 交付报告完整披露：修改文件、逐条行为对应、验证命令及结果、已覆盖契约、未验证边界。

**结论**：glm-5.2 @ Shantung 在本场景下可靠；缓存效率显著优于历史 sensenova 基线，代价是更长的思考时间（TTFT ~3.5s，末步思考 20s）。

## 记录 2：2026-08-29 · glm-5.2 @ Shantung · 全能力升级后回归（写 fixture + 诊断）

- **触发**：本轮落地 4 项可靠性修复 + watch 持续监听 + L2 侦察 worker + 附件句柄后，重跑验证无回归。
- **evaluation_id**：
  - 写 fixture：`agent-write-fixture-d3c3b11d-e984-4a31-a07b-8f27f59e09f0`（trace `0c256a2b`）
  - 诊断 fake-action：`agent-diag-fake-action-33b6ffd9-25f0-4ad7-9819-cda78c950ef2`（trace `7d289e2f`）

| 指标 | 写 fixture（本次） | 写 fixture（记录1基线） | 诊断 fake-action（本次） |
|---|---:|---:|---:|
| LLM 调用 | 11 | 9 | 7 |
| JSON 解析错误 | 0 | 0 | 0 |
| 工具步骤 | 12 | 10 | 8 |
| 工具失败 | 1（安全拦截） | 0 | 1（MCP 网络抖动） |
| 重复读取 / 重复失败 | 0 / 0 | 0 / 0 | 0 / 0 |
| 缓存命中率 | 82.4% | 81.7% | 88.1% |
| 观测时长 | 231s | 247s | 189s |
| verdict | review_recommended | passes_observed_guardrails | needs_attention |

**失败步骤归因（重要，均非 agent 逻辑回归）**：
1. 写 fixture 第 0 步：模型尝试 `switch_workspace` 被**宿主安全策略正确拦截**，模型随即改用只读工具在边界内完成任务——写入边界实测有效。
2. 诊断第 3 步：MCP 端点 `mcp.wxhjy.dpdns.org` HTTP 请求失败（网络抖动），非代码问题。

### 诊断 4 场景结果（E2E：1 过 1 挂，挂因集中且精确）

| 场景 | 调用/步骤 | 失败 | 重复 | 缓存 | 时长 | verdict |
|---|---|---:|---|---:|---:|---|
| diag-fake-action | 7 / 8 | 1（MCP 网络抖动） | 0/0 | 88.1% | 189s | needs_attention |
| diag-plugin-removal | 9 / 10 | 0 | 0/0 | 89.9% | 148s | needs_attention |
| diag-false-leak | 8 / 9 | 1 | 0/0 | 88.4% | 120s | review_recommended |
| diag-name-collision | 6 / 7 | 0 | 0/0 | 88.3% | 91s | needs_attention |

全部 trace `succeeded`、0 JSON 解析错误、0 重复读取/重复失败。**唯一系统性失分点（E2E 隐藏断言）**：「最终交付必须明确披露已核验范围、未核验范围或关键假设、以及结论强度」——glm-5.2 在诊断类任务上交付纪律不达标（结论本身未被判定错误，是披露结构缺失）。

**下一轮优化动作（未实施，待验证）**：在 `prompts/skills/code_diagnosis.md` 与 core_logic.md 复杂交付要求中，显式加入「诊断交付四件套：已核验范围 / 未核验范围或关键假设 / 结论强度（confirmed|supported|unverified）/ 就地证据引用」，改后重跑 diag-name-collision（最快 91s）验证是否转绿。

## 本轮新发现并修复的迁移缺口（2026-08-29）

- 线上库 `agent_child_runs` 缺 `result_json` 列（首版建表后新增、无滚动迁移）→ 老库升级后子任务终态结果**静默不可消费**。已补 `ALTER TABLE`，并新增 `kind` 列（review/worker 分类记账）。测试用全新建库无法暴露此类问题，**老库迁移是后续每个新列的必检项**。

1. `cancel_running_child_runs` 取消后未广播 `notify_job_state_changed` → 挂起在 `wait_for_jobs` 的协程只能等 180s 超时兜底。已修，并加测试证明取消即时唤醒等待者。
2. `run_child_reviewer`（同步路径）timeout/failed/cancelled 终态不写 `result_json` → 结果永远无法被信箱消费，模型对子审查死亡无感知。已补齐 `child_error_result_json`。
3. `poll_completed_child_runs_filtered` 遇到损坏 result_json 行会永久重试。已改为清除并记日志。
4. 进程崩溃后 `agent_child_runs` 孤儿 running 行永不收敛（job 表有恢复、child 表没有）。新增 `recover_orphaned_child_runs_on_session_start`，挂接到主循环会话启动处。
5. 同步路径 child_reviewer 系统提示词 JSON 示例双重转义（复制粘贴残留）。已修。

## 新增能力：持续监听（watch engine）

- `watch_start(kind: file|command, target, interval_secs 5–600)` / `watch_stop(watch_id?)`，每运行最多 3 个并发监听，单监听 100 事件上限自动收敛。
- 实现锚点：`agent_background_jobs` 表 `job_type='watch'` 的 running 行作 durable 状态锚点；变化事件以 `job_type='watch_event'` 进入既有 durable inbox——`wait_jobs`、主循环自动注入、运行结束级联取消全部复用现有机制，零新增消费路径。
- 文件监听含工作区 containment 校验；command 监听单次执行同时产出哈希与输出摘要（30s 超时、200KB 截断）。
- finish 门控已扩展：存在 running 监听时拦截 finish，提示 `watch_stop` 释放。
- 覆盖原始待办「文件 watch + 结果事件流」的持续监听部分（一次性等待由 `wait_jobs`/job_engine 覆盖）。

## 下一轮对比建议

1. 用 glm-5.2 重跑同一 fixture，确认 watch/child_reviewer 修复无回归（指标应保持 0 失败 0 重复）。
2. 跑 `agent-diag-false-leak`（只读诊断）场景对比 glm-5.2 与 sensenova 的根因定位质量。
3. 待 roadmap §6.2 的「根 + 2 只读 worker 跨模块诊断」评测实现后，补多 agent 协作的首组对比数据（重点看：token 总账、墙钟时间、根因完整度）。

## 记录 3：2026-08-29 · 交付披露修复验证 + L3 落地 + 深度检查结论

### 修复动作（诊断交付披露失分）

1. `code_diagnosis` 技能：交付四件套改为无条件模板（已核验范围/未核验假设/结论强度/主根因就地校准），并要求深度排查默认建立 review_contract。
2. `core_logic.md`：诊断/根因排查类任务默认视为复杂任务，必须建契约。
3. finish 门控泛化新原则：「谁宣称主根因，谁就校准」——未建契约的交付只要 summary 宣称主根因，也必须就地给出直接证据 + 结论强度（复用 assessor 的窗口检测，非新增关键词启发式）。

### 验证结果（diag spec，glm-5.2 @ Shantung）

- 第一轮：1/4 场景过（3 个挂在交付披露合规断言）。
- 第二轮：**3/4 过，且合规断言全部通过**；唯一失败是 0 步基础设施抖动（trace 未产生任何步骤，与 agent 逻辑无关）。
- 第三轮（提示词+门控修复后稳定复跑）：**4/4 全部通过**（2026-08-29，run 时长 9m53s）。结论：交付披露失分已修复且稳定收敛。

### 上下文 / 推理 / 约束深检结论（泛化视角）

- **上下文**：压缩引擎成熟（真实校准水位→先一次性脱水→再归档，append-only 保护前缀缓存）；评测缓存命中 82-90%、峰值上下文 ≤16%，无结构性缺口。本轮补上 worker 回执 findings 单条 600 字符上限，堵住子任务异常输出污染主上下文的口子。
- **推理**：路由分层（Pro 规划/救场、Flash 常规）+ 分层 reasoning effort 运行稳定（六场 0 解析错误）；推理能力的天花板已从单上下文转移到 L2/L3 的独立上下文。
- **约束**：finish 门控现覆盖 未闭合 Todo / 审查契约 / 写入验证 / 复杂交付披露（契约版+无契约主根因版）/ 运行中后台任务与监听；写入边界（switch_workspace 拦截、worker 副本 containment）实测有效。

### 已知边界（记录，不在本轮处理）

1. L3 回执是 diff 建议稿，落实修改仍需 root 手动应用——未来可做宿主端「审阅后一键应用 + 自动重验证」。
2. writer 副本建在工作区内（`.writer-<id>`），评测文件计数会包含它（不影响受保护文件断言）。
3. 门控挑战循环理论上可能消耗步数至 MAX_STEPS——目前唯一一次 trace failed 为基础设施抖动，需持续观察。

## 记录 4：2026-08-29 · 档位对照实验（核心问题：强模型的钱能不能省？）

**实验设置**：同一批 4 个复杂诊断场景（隔离 fixture + 隐藏断言 + 交付披露合规检查），
三种模型配置各跑一遍。当晚 Shantung 端点在劣化（6.8 崩、6.7 高延迟 7-62s TTFT），数据含噪声但方向明确。

| 配置 | E2E 结果 | 披露合规 | 单场景耗时 | 总耗时 |
|---|---|---|---|---|
| **全强 glm-5.2** | **4/4 过** | **4/4** | 91-190s | **9m53s** |
| 全快 6.7-flash-lite | 1/4 过 | 2/4 | 665-1179s | 1h13m54s（**7.5 倍**） |
| 全快 6.8（崩溃端点） | — | 0/1 | 单场景 1036s / 16 次调用 | 已失控终止 |
| 混合（Pro=glm-5.2 + Flash=6.7，规则路由） | 进行中 | 3 场景：2 True / 1 False | 960-1179s | — |

**结论（数据说话，不再纠结）**：
1. **复杂诊断任务的强模型钱不能省**：全快组质量崩（JSON 合规差、披露纪律守不住）+ 墙钟 7.5 倍——"省流"完全被重试和返工吃掉。
2. **混合路由的价值在韧性而非省钱**：常规步骤走 Flash 时 inherits 端点劣化，但规划/救场由强模型兜底，场景没有像纯快组那样彻底崩。
3. **档位映射基线确定**：planner/reviewer → Pro（质量关口绝不降级）；executor/scout → Flash 跑腿可以，但诊断类任务的 executor 建议也挂 Pro 或依赖升维信号。
4. 端点可用性是波动的（6.8 白天 926ms 在线、晚上崩）——这正是"槽位可换 + 角色挂档位"架构的现实理由。

**落地物**：角色→档位映射（role_models，支持档位别名或钉死具体 UUID）、结构化失败信号（替代 ❌ 字符串匹配）、难度打分路由（routing_mode=scored，透明加权表：契约+20/连续失败×15/观测超长+20，≥60 升维）。282 单测全绿。打分模式的实测验证待端点健康后补跑。

## 记录 5：2026-08-29 · 角色档位配置页面落地

- **AgentComposer 模型路由弹窗**新增：①路由模式切换（规则/打分）；②角色档位表——规划、执行、审查、侦察L2、写入L3 五个固定角色，每角色可选「跟随自动 / 强模型槽 / 快模型槽 / 视觉槽」，值经 role_models 透传后端，钉死优先于自动分流。
- **配置面收敛**：用户只维护 3 个槽位（换模型=换槽），角色默认跟随系统映射表，高级钉死入口保留——与 Claude Code 的档位别名模式同构。
- 打分模式（routing_mode=scored）实弹验证已排队后台执行，结果见后续记录。

---

## 附：评测运维手册（2026-08-29 白屏排查战实录，按症状查表）

### 症状 1：评测启动失败「Embedded WebDriver server did not become ready on port 4445」

**根因（当晚真凶）**：跑过普通 `cargo build`（不带 `wdio` feature）之后，`target/debug/free-api-chat`
被覆盖成无驱动插件的版本——app 能起窗口但前端全白、驱动永不注册。
**诊断命令**：`strings src-tauri/target/debug/free-api-chat | grep -c "wdio-webdriver"`（0 = 二进制坏的）。
**修复**：`npm run test:e2e:build`（带 `--features wdio`）再重试。
**铁律**：评测前必跑 test:e2e:build 并校验 strings；写完 Rust 代码后先 cargo test，评测前必须重新 test:e2e:build。

### 症状 2：app 进程活着、窗口白屏、无障碍树读不到

= app 卡死在启动早期（驱动插件注册之前）。当晚链路：二进制无 wdio feature（症状 1）→ 白屏。
**铁律**：`pkill -9 free-api-chat` 强杀后检查 `~/Library/Application Support/free-api-chat/*.db-wal/-shm`
残留（当晚 WAL 0 字节属正常）；重启前确认 `lsof -i :4445` 为空。

### 症状 3：端口 4445 被残留进程占用（连发启动的竞态）

上一轮的 app/driver 还没退干净就起新一轮 → 新 driver 绑不上端口。
**修复**：`pkill -9 -f "wdio"; pkill -9 -f free-api-chat; sleep 5; lsof -i :4445`（必须为空）再启动。
**铁律**：一轮评测结束后再启动下一轮，绝不并行两个 wdio。

### 症状 4：评测规格静默跳过 / executeAsync 脚本超时

- 规格 `describeLive = modelId ? describe : describe.skip`——**只要 FREE_API_LIVE_MODEL_ID 没设，全部场景静默 skip**；
  加分槽变量（PRO/FLASH/VISION）时门控变量仍必须传。
- `browser.executeAsync` 的函数体在浏览器上下文执行，**模块级变量不会带进去**——新增参数必须同步改
  函数签名和调用实参列表，否则 `done` 被顶掉 → 30s 脚本超时（当晚 fake-action 全军覆没的根因）。
- 判断依据：报告 mtime + 报告内 routing id 是否为本轮模型；`agent_llm_calls` 有无新增行。

### 症状 5：评测「卡住」——先分清端点慢还是真死锁

查 `agent_llm_calls` 最近几行：`ttft_ms` 飙到几十秒 = 端点劣化（当晚 6.8 崩、6.7 TTFT 7-62s），
`outcome=parse_error` 反复出现 = 小模型 JSON 合规问题在烧重试。后台评测传输错误最多 20 次有界退避，不会死锁，只会长跑。

### 监控速查（本轮验证有效）

```bash
# 运行状态与步骤数
sqlite3 "$DB" "SELECT id,status,started_at FROM agent_run_traces ORDER BY started_at DESC LIMIT 3;"
sqlite3 "$DB" "SELECT COUNT(*) FROM agent_step_logs WHERE session_id='$SESSION';"
# 逐调用延迟/解析/重试（区分端点慢 vs 死锁）
sqlite3 "$DB" "SELECT session_call_index,outcome,request_try,transport_attempt_count,total_duration_ms,ttft_ms FROM agent_llm_calls WHERE session_id='$SESSION' ORDER BY session_call_index DESC LIMIT 6;"
# 子任务（reviewer/worker/writer 按 kind）
sqlite3 "$DB" "SELECT kind,status,error_kind FROM agent_child_runs WHERE started_at>'$T';"
```
$DB = `~/Library/Application Support/free-api-chat/data_v2.db`；报告自述路由模型（routing.reasoning/execution_model_id），
跨轮核对报告归属以 mtime + routing id 双重确认。
