# 多 Agent 门禁体系设计评审（2026-09-12）

> 评审范围：`src-tauri/src/agent/`（约 3.6 万行、55+ 文件）的多智能体编排与三层门禁体系，
> 以及 2026-09-12 上午"默认会话工作区绑定 + 纯排查任务被 finish 门禁卡死 95 步"的真实事故。

## 一、架构结论

**拓扑**：单主脑 + 深度固定为 1 的星型多代理。主循环（`main_loop.rs`）每步
`call_llm → run_tool`；可派三类子代理：

| 子代理 | 预算 | 能力 | 约束 |
| --- | --- | --- | --- |
| reviewer (L1) | 单次 LLM 调用，45s 超时 | 只评估证据包，输出 JSON findings/scorecard | 禁写文件、禁执行命令、禁再派子代理 |
| worker (L2) | 10 步 / 180s | 只读侦察 | 命令白名单 |
| writer (L3) | 隔离工作区副本全流程 | 写码 + 跑测试 | 每 run 限 1 个，回执带逐文件 diff |

子代理不经 dispatcher，不可递归嵌套。并发上限：全局 6、单 kind 2（角色卡可覆盖）。
子任务经 job 信箱派发，`wait_for_jobs` 实现反应式 join barrier（"问过 ≠ 听过"）。
全部门禁状态持久化于 SQLite `AgentCheckpoint`（乐观锁 + 7 种恢复决策，崩溃可恢复）。

**三层门禁**：

- 写前：`plan_gate.rs::check_pre_write_gatekeeper` —— 阶段审查 blocking 清账 →
  Tech Lead 1v1 方案评审（approved / rebuttal / escalate）→ 按需多专家联合会审 → 齐套屏障；
- 写中：`stage_review.rs` —— todo 阶段完成语义 + 累计写文件数双触发，blocking 可对质撤回，
  对质失败自动降级 final_only；
- 写后：`reviewer_gatekeeper.rs::evaluate_finish_diff`（diff 评估）+ `review_contract.rs`
  （义务清单 + 冻结 SHA-256 验证脚本）+ finish_guard 质询。

## 二、设计优点（应保持）

1. **"固定管线 + 角色卡可配"**：流程写死防状态机漂移，灵活性全部留给角色卡的
   Prompt / 模型 / Schema 配置。这是踩过动态流程图死锁后得到的正确折中。
2. **齐套屏障**："派发了 ≠ 听过了"。join barrier、幂等去重、failover 时 run_id 替换、
   失败记录防死锁——正经分布式系统模式。
3. **成本分层**：L1/L2/L3 预算递增，花得起钱与花不起钱的审查被分开。
4. **跨模型家族异构互审**（`prefer_cross_family_reviewer`）：不同家族盲区不相关，
   对冲"自己审自己必然通过"。
5. **教练式门禁文案**：拦截信息直接指明下一步动作，而非裸报错。
6. **逐 checkpoint 持久化 + 乐观锁 + 恢复决策**：绝大多数同类框架不具备。

## 三、风险（需持续消化）

1. **控制面复杂度**：门禁台账（plan_review_batch、stage_review 计数、human_waivers、
   approved_target_files…）单调增长。单看每个机制都必要（皆源自真实事故），
   但三次死锁事故全部长在**状态组合处**而非单字段上。
2. **审查质量无元检验**：门禁强度最终押在"审查员是否认真"上；
   `is_abnormally_low` 只能抓打分过低，抓不了放水式打高分。
   建议积累指标：审查放行后、后续阶段再次暴露 blocking 缺陷的比率。
3. **深度 1 星型结构的天花板**：复杂任务全压在主脑 300 步内；writer 每 run 限 1 是串行瓶颈。
4. **契约人工多点同步**：同一流程契约散在 core_logic.md、gate_messages.rs、角色卡 Prompt
   三处，靠 commit 纪律保持一致，应收敛为单一来源生成。
5. **交织路径缺故障注入测试**：死锁皆生于超时/崩溃/重试交织处，该区域测试最薄。

## 四、2026-09-12 事故复盘（已修复）

**现象**：默认自由会话中用户随口一问，agent 排查用 `sed -n '265,340p'` 读代码，
被 `audit.rs` 只读白名单（无 sed）误记为副作用；finish 时
`finish_guard/diff.rs::review_contract_evidence_blockers` 因 `side_effect_count > 0`
机械要求证据落入 Git diff；diff 为空 → 三次驳回 → agent 被迫"把排查任务升维成
改代码任务"自证清白，95 步 / 2.2 万 token / 16 分钟，且未经 ask_human 授权
私自修改了 `main_loop.rs`。

**根因**：三处概念混淆叠加——
1. 只读白名单缺 `sed -n`（排查常用形态被误判为写副作用）；
2. `side_effect_count > 0` 被等价为"必然修改了代码，必须有 diff"（副作用 ≠ 代码修改）；
3. 诊断引用（"根因在 main_loop.rs:282"）被拿去 diff 里对账（排查证据 ≠ 改动证据）。

**修复（三个提交）**：

- `f6eb5d7`：仅显式项目会话/评测自动驾驶绑定物理工作区；自由会话提示词明示
  "工作区保持为空，勿臆造绑定目录"（agent 自主修复，人工复核通过）；
- 三分法证据校验（本次提交，`finish_guard/diff.rs`）：以**客观 diff 有无**分流——
  - 有真实 diff → 证据必须落入修改行（防虚假交付，原逻辑不变）；
  - 无真实 diff + 未声称修改 → 诊断通道：引用静态可定位即可（文件存在且行号在范围内，
    保留 affected_paths 回退）；
  - 无真实 diff + 声称"已修复/已修改" → 照常拦截（撒谎 writer 不因无 diff 放行）。
  - 关键词列表刻意从紧（不收"已更新/已调整"等弱信号）；启发式漏判的残余风险
    由义务级 verify_command（宿主真实执行）与人工审批兜底；
- `audit.rs` sed 白名单收敛：`sed -n` 纯读取形态判只读；但逐项排除 `-i`/`--in-place`
  （含 `-in`/`-ni` 合并旗标）、`-f`/`--file`（脚本不可静态审计）、脚本内嵌 `w`/`e`
  写文件/执行指令。`python3`/`sqlite3` **永不进白名单**（可执行任意写入）。

**治理教训**：agent 被门禁逼到墙角后，找到的出口是"改代码满足门禁"而不是 `ask_human`。
门禁拦未证实交付是对的，但系统缺一个第一公民级的"纯诊断交付"通道与强制人工仲裁出口——
堵住排查死结后，此类"为过考试改考题"的行为应从源头消失。

## 五、观测与下一步

- 门禁触发遥测：`agent_gate_fires` 表（pre_write / finish_review /
  finish_obligation_evidence 三族），每个拦截落一行。跑两周后按
  `SELECT route, COUNT(*) FROM agent_gate_fires GROUP BY route` 审视：
  零触发的机制是下一波死锁高发区，应评估退役。
- 待办（按优先级）：
  1. 固定考卷（5~8 题真实任务：纯文案 / 单模块 bug / 跨模块 / 架构重构 / 纯排查 /
     矛盾需求），用 `evaluation_runner.rs` 跑基线——"好不好"用数字回答；
  2. 审查质量反馈指标（放行后复发率）；
  3. job 引擎故障注入测试；
  4. 定期收敛重构：route 字符串 → 枚举、方案审查五个状态载体 → 带类型状态机。
