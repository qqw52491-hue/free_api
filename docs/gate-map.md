# 拦截点总表(Gate Map)

> 目的:把 Agent 里「会拦住/打回/升级审查」的规则放在一张图里,方便统一治理。只读文档,不影响代码。
> 范围:`src-tauri/src/agent/`。行号为撰写时的位置,会漂移,以函数名为准。标「未核实」的是只看了注释、没读完实现。

## 0. 一步的流程(骨架)

```
用户任务 → 入口分流(triage) → 主循环每一步:
   模型出指令 → [A 派发前检查] → 写操作还要过 [B 写前门] → 执行工具 → [C 运行中保护]
   模型调 finish → [D 收尾链:预检 → 宿主验证 → 机械门 → 终审审查员] → 放行/打回
```

类型说明:**硬拦**=客观事实触发;**启发式**=靠规则猜,有误拦风险;**熔断**=防死循环的安全阀;**提示**=只提醒不拦。

## A. 入口分流与派发前检查

| # | 位置 | 触发条件 | 结果 | 类型 | 备注/已知风险 |
|---|---|---|---|---|---|
| A1 | `triage.rs` | 任务开始 | 判出 route:要不要方案门/阶段审查/终审方式,存入 checkpoint(`triage:fallback/mechanical_only/stage_review_disabled`) | 启发式 | 恢复的旧 checkpoint 无判决,会退回 B3 的机械兜底 |
| A2 | `triage::write_floor_violation` | 动作含 delete/remove/rename/move/unlink/rmdir/truncate/drop;或本轮非 tmp 工作区文件 ≥4 个 | 强制回到方案门,不被判决推翻 | 硬拦 | 「安全地板」,判决豁免不了 |
| A3 | `loop_runner/mod.rs` B0 | 同工具+同路径第 3 次读取且内容没变 | 拒绝(`repeat_read`) | 硬拦 | |
| A4 | `context.repeated_failure_blocker` | 同一调用反复失败 | 拒绝 | 硬拦 | |
| A5 | `context.check_batch_dispatch_allowed` | 连续批发派发解析失败 | 熔断(`batch_circuit_breaker`) | 熔断 | |
| A6 | `utils/llm_client.rs::parse_executable_instruction` + `types::strip_entry_when_status_action` | `submit_plan`/`prepare_finish` 与工具调用混发 | **已改为自动处理**:保留状态动作,丢弃夹带的工具调用(不执行),结果里提示模型"附带的工具调用未执行,需要请下一步单独发"(`loop_runner` 的旧"协议误用"分支留作兜底;`request_retriage` 混发仍打回) | 自动纠正 | 不执行夹带工具=宁可少做不做错 |
| A7 | `path_guard.rs` | `local_file` 写/改工作区外路径 | 拒绝(`path_denied`) | 硬拦 | skills 目录只读放行(be46053) |

## B. 写前门(写操作/有副作用的工具之前)

| # | 位置 | 触发条件 | 结果 | 类型 | 备注/已知风险 |
|---|---|---|---|---|---|
| B1 | `loop_runner/mod.rs` ~430-470 | 写操作:算 `plan_needed` | 有判决:看 route ∨ 地板 ∨ 人工审批模式;无判决:走 B3 | 启发式 | 这里同时有 3 个来源在判「要不要方案门」(A1/A2/B3) |
| B2 | `plan_gate::check_pre_write_gatekeeper` | `plan_needed` 且方案未批准 | 三种模式:**PlanApproval**(要用户批)/**CrossReview**(派审查员盲审→等结论→记分卡→有阻塞问题不放行)/自动。路由 `plan_gatekeeper:*` | 硬拦 | 连续被拦 8 次触发 C3 降级 |
| B3 | `path_guard::is_plan_gate_exempt_write` | 无判决时 | 豁免:tmp/ 产物、磁盘上不存在的新文件、≤12 行的单文件小编辑(已触及≥2 文件后第 3 个不豁免)。非 `local_file` 写入(shell/MCP/child_writer)一律不豁免 | 启发式 | |
| B4 | `plan_gate::stage_blocker_interception` | 阶段审查有未解决 blocking | 拦写入,只有 `resume_child` 对质撤回能解 | 硬拦 | 方案门被豁免时仍执行 |
| B5 | `agent_policy::evaluate_plan_policy` | 方案提交后评估是否升级多 Agent 盲审 | FastPass 白名单放行;高危路径(`src-tauri/`、DB、配置)/危险命令/Tauri IPC/`non_whitelisted_file` 升级审查;可被 `.agent_policy.json` 覆盖 | 启发式 | **已知偏重**:任何非白名单业务文件都先走方案审查,小改动也一样 |
| B6 | `plan_gate::submit_plan_rejection/validate_plan_submission` + `plan_submit.rs` | 提交方案 | 模式/阶段/预算/内容校验 | 硬拦 | 敏感预检已删(f975d32) |
| B7 | `dispatch_ask_human.rs` | 调 `ask_human` | 冷却/重复/配额拒绝;`plan_approval` 说明不足 30 字则自动用 message 补 | 硬拦 | 被门禁引导去 ask_human 的拦截豁免冷却 |
| B8 | `loop_runner/dispatch_local.rs` + `shell_safety::git_destructive_review` | git 破坏性命令 | 目标全在本 session 写入台账/核准集→放行;无路径或含外来目标→ask_human 人工确认。`git checkout <路径>`(无 `--`)按还原处理(117d… 相关提交) | 硬拦 | 事故驱动(2026-09-25) |
| B9 | `loop_runner/dispatch_local.rs` + `shell_safety::sensitive_shell_access_review` | 顶层 local_shell 连数据库(mysql/psql/redis-cli…)或读凭据/密钥文件(.env/id_rsa/.pem…) | 暂停不执行并告知模型"这条路不该自己走",确需则 ask_human 引用完整命令、主人明确同意后原样重发(复用 B8 的指纹核销链路)。不覆盖 parallel 分支与 local_file 读取 | 硬拦(人工放行) | 775b167b 实测:读 DB 凭据 + root 连本地 MySQL |
| B10 | `dispatcher/shell_safety.rs`、`context/audit/shell_readonly.rs` | 终端命令 | 危险命令判定、只读命令判定(影响是否算写、是否进方案审查) | 启发式 | 已知:只读排查命令(`stat`+`echo`)曾被当终端命令拉去方案审查 |

## C. 运行中保护

| # | 位置 | 触发条件 | 结果 | 类型 |
|---|---|---|---|---|
| C1 | `main_loop` 看门狗 | 运行卡死/无进展 | 收敛为 failed 并出报告 | 熔断 |
| C2 | `MAX_STEPS=300`、`AgentRunBudget` | 步数/prompt token 超限 | failed 终态 | 熔断 |
| C3 | `MAX_CONSECUTIVE_GATE_BLOCKS=8` | 连续 8 步只产出门禁拒绝 | 降级收尾,标记失败 | 熔断 |
| C4 | LLM 请求重试 | 瞬时错误 | 默认重试 3 次,上限 100 | 熔断 |

## D. 收尾链（统一实现）

`prepare_finish` 与 `finish` 都只调用 `finalization/evaluate.rs::evaluate_completion`。
二者只有“是否提交终态”与“终审失败计数”不同，不再各写一套放行逻辑。

```text
执行工具 → 副作用记账/自动验证 → 应用契约和验证申报
                  ↓
共同机械检查：待办、契约申报与证据、后台任务、阶段 blocking、方案状态、scope、目标路径
                  ↓
已有当前版本的就绪收据？ → 是：复用验证/终审结论（动态条件仍检查）
                  ↓ 否
冻结义务命令实测 → 尚未解决的通用写入验证 → 终审 → 就绪/阻塞收据
                  ↓
prepare_finish：只返回收据，不完成任务
finish：确认未收到停止、收据仍对应当前版本 → 短事务提交 → 才广播成功
```

| 检查/状态 | 权威来源与行为 | 打回后的出路 |
|---|---|---|
| 契约申报 | `evidenced/waived` 是模型申报，不是验收通过；最终检查完整契约 | 按 id 只补正被打回项，未提交项保持原值；可退回 open，冻结题目/命令保护不变 |
| 写入验证 | 副作用先记账，再处理本步申报；已经执行完的本步探针可作证据，本步写入不能自证 | 新写入使旧验证失效；可重新验证或说明 waived 理由，不锁死旧申报 |
| 冻结命令 | 有本任务新增副作用时执行；失败保留命令、目录、退出码、诊断；全部通过保存宿主验证回执 | 修正实际代码/产物后重试；冻结标准的修改仍走原规则，不靠重复 finish 消除失败 |
| 自适应验证 | 在任务工作区/对应子项目推断并执行，不拿宿主程序目录的项目做测试 | 真实失败打回；无可推断命令/检查工具不适用时沿用 waived 策略，交终审核对并随交付披露 |
| 观察脚本 | 可逐段识别的只读核查/验证脚本不重复制造写入批次；不改变工具权限 | 未识别脚本仍保守记账；实测发现改动即使退出非零也记账 |
| 测试证据 | 不再凭命令中出现 `cargo test` 等子串认账；过滤输出的管道退出 0 不代表前段测试通过 | 使用真实验证退出状态或显式引用执行证据，交终审核对 |
| scope/目标 | 预检和正式交付使用相同适用域；判据不适用时明确移交终审 | 补正改动、显式扩展范围或说明方案修订，不再预检过了却另走一套门 |
| 终审 | 缺陷修复后可重审；`finish_reviewed` 只为旧快照兼容，重试次数不消除阻塞 | 多次失败引导人工仲裁；沿用人工明确授权后带警告交付；服务失败停止为未完成，不伪装通过 |
| 收据保鲜 | 交付内容哈希、资源范围版本、契约内容、写入/验证/方案/阶段状态与交付摘要 | 内容或验收状态改变重新核验；仅显示记账、产物登记不会自动制造新写入 |
| 停止 | 审查返回后和终态提交前都检查停止/替换任务信号 | 已收到停止就不提交；不是只有审查失败分支才尊重停止 |
| 终态提交 | 只有就绪且仍匹配当前验收状态的收据可以提交；候选 checkpoint/目标块在事务成功后才替换内存 | 乐观锁或 DB 失败整体回滚，不能先显示成功或把内存标 Done |

**重试预算耗尽仍是“未完成”，不自动改成 succeeded。** 实在无法完成可沿用失败出口，保留工作供续跑。
本次统一的是交付链；入口分流、方案审批、阶段审查本身的策略不在本次改写范围。

## E. 辅助审查(方案审查/子审查)

- `child_reviewer/*`:方案盲审、子任务审查,含跨模型族检查(`cross_family_check`)、证据范围(`evidence_scope`)。
- 其 `git_diff` 取证已对齐 D8 基线口径(经 `GitBaseline::register_for_session` 按会话登记起点;无 git 基线时回落裸 `git diff HEAD`)。

## F. 观察:哪里重叠、哪里容易再出事故

1. **「要不要方案门」有 4 个来源**:A1 判决、A2 地板、B3 豁免、B5 策略。改一处容易和别处打架。→ 建议收成一个入口函数,返回「要/不要+原因」。
2. **「这次到底改了什么」有多个口径**:终审用基线 diff(D8);子审查已对齐基线 diff(E);finish 义务对账用 `DiffContext`(D2);挂账验证用写入台账(D3)。→ 建议统一成一个「变更材料」入口(未逐一核对实现,属判断)。
3. **规则叠加**:同一件事可能被 B5(方案审查)、D2(义务)、D8(终审)各查一遍,模型被打回的轮次多。
4. **预算只停止未完成任务，不替代验收**:C3 可以结束为失败；D 链不得因达到质询次数而把未解决问题改为通过。
5. **事故→补丁式修复历史**(都有对应提交):grep 无匹配误报(7d9bed2)、path:line 误拦(7fdb901)、终审夹带用户改动(f4d4395)、submit_plan 敏感预检(f975d32)、命令升级审查改默认放行(c3e58cd)、撤回意见占待办(09de0fa)、CRLF 编辑(894664d)。

## G. 建议的规矩(新增拦截前先过这 4 问)

1. 有没有**真实事故**(任务 id/日志)证明需要?没有就只做提示。
2. 触发条件是**客观事实**还是**猜测**?猜测的只能提示,不能硬拦。
3. 证据不存在时是 **NotApplicable**(跳过+移交)还是 Block?默认前者。
4. 有没有**测试**覆盖「误拦」场景,并写进本表?

## H. 待查/待办(未处理)

- `parallel` 里 `local_file` 报「未提供当前活跃的工作区」(疑似宿主并行路径没传工作区)。
- `pkill -f` 匹配到自身 shell 时的提示。
- 只读排查命令不应触发方案审查(B10)。
- B5 `non_whitelisted_file` 对小改动偏重(涉及审查规则,需先定策略)。
- ~~`child_reviewer` 的 `git_diff` 对齐基线口径。~~(已完成)
- 视频类任务契约命令写死产物名与实际不符。
