# SandwichAgent 架构与源码导读

[返回 README](../README.md)

本文说明现有实现中的职责边界。具体参数以源码为准；历史设计和评测记录保留在 `docs/` 与 `OVERNIGHT_LOOP.md` 中，不将一次运行结果视为所有模型的保证。

## 按问题找入口

| 想排查什么 | 从哪里开始 | 下一站 |
| --- | --- | --- |
| 项目、会话页签和工作目录 | [`AgentView.vue`](../src/views/AgentView.vue)、[`ProjectWorkspace.vue`](../src/components/ProjectWorkspace.vue) | [`commands/projects/`](../src-tauri/src/commands/projects/) 保存项目与目录配置，[`workspace_access.rs`](../src-tauri/src/agent/workspace_access.rs) 和 [`workspace_lock.rs`](../src-tauri/src/agent/workspace_lock.rs) 处理执行期目录和占用 |
| 提交、回答、停止或恢复了哪次运行 | [`AgentPanel.vue`](../src/components/AgentPanel.vue)、[`useAgentSession.ts`](../src/composables/useAgentSession.ts) | [`main_loop/entry.rs`](../src-tauri/src/agent/main_loop/entry.rs)、[`control.rs`](../src-tauri/src/agent/control.rs)、[`checkpoint/`](../src-tauri/src/agent/checkpoint/) |
| Zed 或手机接进来后任务去了哪里 | [`main.rs`](../src-tauri/src/main.rs)、[`acp/`](../src-tauri/src/acp/)、[`channel/http/`](../src-tauri/src/agent/channel/http/) | [`run_spawn.rs`](../src-tauri/src/agent/channel/http/run_spawn.rs) 受理后连接同一工作区执行入口。微信桥独立在 [`wechat-bridge/`](../wechat-bridge/) |
| 子任务为什么没有出现在图里、现在等谁、结果收到了吗 | [`child_task/team_view.rs`](../src-tauri/src/agent/child_task/team_view.rs)、[`AgentTeamTopology.vue`](../src/components/agent/AgentTeamTopology.vue) | [`agent_mailbox.rs`](../src-tauri/src/agent/agent_mailbox.rs)、[`job_engine.rs`](../src-tauri/src/agent/job_engine.rs)、[`child_receipt.rs`](../src-tauri/src/agent/child_receipt.rs) |
| 模型选了谁、文件工具能做什么、为什么交付被打回 | [`router.rs`](../src-tauri/src/agent/router.rs)、[`child_task/models.rs`](../src-tauri/src/agent/child_task/models.rs)、[`dispatcher/`](../src-tauri/src/agent/dispatcher/)、[`main_loop/finalize.rs`](../src-tauri/src/agent/main_loop/finalize.rs) | 先定位发生在哪一层，再核对模型实际收到的材料、工具返回原文和审查记录 |
| 要验证发布、恢复或模型表现 | [开发与验证](../README.md#开发与验证)、[`test/e2e/`](../test/e2e/)、[`test/eval_cases/`](../test/eval_cases/) | [ACP 本机验证](ACP_INTEGRATION_PLAN.md#本机验证)、[实测记录](agent-reliability-records.md)、[发布指南](releasing.md) |

## 一次任务如何推进

```text
用户任务、工作区与会话历史
    ↓
入口分流 / 恢复已有检查点
    ↓
规划与工具执行 ← 用户干预、子代理结果、按需方案修订
    ↓
执行输出、文件变更、验证与审查
    ↓
准备交付 → 终态事务提交
```

入口分流、方案与调查问题都是可以被后续证据修正的先验。宿主检查参数、路径、执行状态与引用来源；模型和 Reviewer 判断方案与实现是否满足任务。两类职责的设计依据见 [AGENTS.md](../AGENTS.md)。

## 上下文：稳定信息、历史记录、当前状态

“三明治上下文”将信息按变化方式组织：

| 层 | 内容 | 目的 |
| --- | --- | --- |
| 前部 | 系统规则、任务目标等相对稳定的信息 | 尽量保持相邻请求前缀稳定 |
| 中部 | 工具说明事件、执行摘要、输出引用与目标变更 | 保留任务推进过程 |
| 尾部 | 活动计划、待办、最新观察和相关状态 | 让当前需要处理的信息靠近模型输出位置 |

历史记录在压缩或归档边界进行有界整理，长输出保留原文与回查句柄。模型请求组装还会处理供应商的消息格式差异。前缀稳定是工程目标，真实缓存收益需要结合供应商回传的用量与计时观察。

相关入口：[`context/`](../src-tauri/src/agent/context/)、[`checkpoint/`](../src-tauri/src/agent/checkpoint/)、[`utils/llm_client.rs`](../src-tauri/src/agent/utils/llm_client.rs)。

### 图片观察的生命周期

新附件交给视觉槽做一次窄范围观察，结果与原图副本引用进入检查点。成功后，后续发送投影用文字观察替代对应原图块；原始历史不会因成功观察被改写，需要细节时可用 `view_image` 回看。失败保留原图及视觉路径，中断不记为观察完成。

这解决的是“历史图片持续占用视觉路由”的宿主问题，不证明观察资料完整或模型不会再次走偏。详见 [修复记录](agent-attachment-vision-lifecycle-2026-10-23.md)。

## 检索与来源：索引负责找，原文负责核对

当前工作区代码可以实时检索；任务历史和索引存入 SQLite，长输出还可能落盘。`output_id` 用来回查具体观察，步骤号不能替代跨运行的唯一身份。

- 常驻状态帮助模型定位当前任务。
- 搜索返回候选与范围信息，不把截断的命中冒充完整结果。
- `get_step_output` / `search_step_output` 按需读取、搜索已保存的原文。
- 来源与时间归属不明确时披露未知，不根据文本外观猜测。

相关入口：[`retrieval/`](../src-tauri/src/agent/retrieval/)、[`task_store.rs`](../src-tauri/src/agent/task_store.rs)、[`run_trace.rs`](../src-tauri/src/agent/run_trace.rs)、[`evidence_store.rs`](../src-tauri/src/agent/evidence_store.rs)。

## 工具执行与界面展示

工具分发结果区分观察文本、成功状态、执行路由和可选结构化视图。模型接收适合上下文的观察，界面展示文件、命令或协作卡片，原始输出单独保留。缺少结构化视图时仍有文本回退。

`commands` 处理串行操作，`parallel` / `plan_tree` 处理显式执行结构。普通工具单组并行上限统一为 **5**，超过时在执行前拒绝；数量校验不判断语义上的独立性，依赖关系仍需由任务规划处理。异步子代理使用专门的派发与回收契约，不能当成同步执行树叶子混用。

相关入口：[`dispatcher/`](../src-tauri/src/agent/dispatcher/)、[`plan_tree/`](../src-tauri/src/agent/plan_tree/)、[`plan_tree_dispatch.rs`](../src-tauri/src/agent/plan_tree_dispatch.rs)、[`AgentChatStream.vue`](../src/components/agent/AgentChatStream.vue)。

## 子代理协作

| 角色 | 执行方式 | 回收与核对 |
| --- | --- | --- |
| 普通任务、Worker、Writer | 通用子任务可以复用主循环和工作区，拥有各自的上下文、运行身份与检查点。工具权限取父任务和子任务的实际授权，调查角色可显式设为只读 | 按派发配置交接给父任务或正式交付。旧专用 Worker、隔离 Writer 路径按各自的配置和历史记录处理 |
| Reviewer | 审查组合按实际阶段配置派发。正式审查使用审查材料和相应的取证权限 | 保留成员意见、修订要求与裁决，按当前批次收取 |

角色卡提供模型、职责、范围及可选结构化契约。`job_engine`、信箱和等待工具处理后台回执，常用收取路径共享 `child_receipt.rs` 的交付处理。父任务通过检查点事务保存收取结果。子任务的执行状态、结果是否落盘、父任务是否已收取是三个独立事实。协作图以宿主记录的等待对象和回执为准。续跑时按同会话的逻辑任务关联旧尝试，保留原子任务和实际派发者的身份。

普通派发见 [`loop_runner/dispatch_child_task.rs`](../src-tauri/src/agent/loop_runner/dispatch_child_task.rs) 和 [`child_task/runtime.rs`](../src-tauri/src/agent/child_task/runtime.rs)。专用实现见 [`child_worker.rs`](../src-tauri/src/agent/child_worker.rs)、[`child_writer/`](../src-tauri/src/agent/child_writer/)、[`child_reviewer/`](../src-tauri/src/agent/child_reviewer/) 和 [`tools/role_manager/`](../src-tauri/src/agent/tools/role_manager/)。

## 验证、审查与交付

命令成功、证据适用与实现正确是不同的判断。证据存储保留目的、来源、输出关联及文件观察；Reviewer 可以检查它们是否覆盖当前产物。此前真实成功的验证在满足既有复用条件时可以保留，失败结果和具体阻塞不能靠一句新的批准抹掉。

`prepare_finish` 与 `finish` 共用完成度评估路径；准备阶段可能实际执行检查。完成状态、运行轨迹与交付消息在终态提交路径中一起处理。入口分流问题和普通审查建议不自动变成额外验收义务。

相关入口：[`plan_gate/`](../src-tauri/src/agent/plan_gate/)、[`stage_review/`](../src-tauri/src/agent/stage_review/)、[`reviewer_gatekeeper/`](../src-tauri/src/agent/reviewer_gatekeeper/)、[`finalization.rs`](../src-tauri/src/agent/finalization.rs)、[`main_loop/finalize.rs`](../src-tauri/src/agent/main_loop/finalize.rs)。

## 恢复与错误处理

检查点保存任务身份、历史消费位置、计划和协作状态，减少重启后重复导入旧消息或丢失任务进度。外部进程、网络请求和供应商执行状态仍有各自边界，不能仅凭本地检查点宣称外部工作已经完成。

指令生成补正、网络传输重试和工具失败恢复分别处理。指令生成补正耗尽会把耗尽事实传给外层，避免多层重试重新开启同一生成预算；用户中断保持独立处理。详见 [调查与恢复审计](agent-investigation-rule-and-recovery-audit-2026-10-23.md)。

相关入口：[`main_loop/`](../src-tauri/src/agent/main_loop/)、[`call_llm_phase.rs`](../src-tauri/src/agent/call_llm_phase.rs)、[`run_state.rs`](../src-tauri/src/agent/run_state.rs)、[`control.rs`](../src-tauri/src/agent/control.rs)。

[`run_trace.rs`](../src-tauri/src/agent/run_trace.rs) 提供续跑使用的运行归属查询。旧运行须属于同一会话且具有相同的非空任务标识，才能参与后台任务回收、信箱接续和协作图聚合。任务标识缺失或为空的历史只按原运行号处理。

项目页和会话面板通过 [`useModelRoutingPreferences.ts`](../src/composables/useModelRoutingPreferences.ts) 共用桌面模型偏好。打开的面板随设置更新，提交时读取最新值。单个会话休眠的角色卡保存在该会话面板的状态中，任务续跑仍由宿主读取检查点中的流程配置。

## 验证过的范围

| 使用场景 | 当前证据 |
| --- | --- |
| 本机单仓任务、按需派发、人工审批和停止 | README 有实际配置和操作入口，生产执行器有单元与集成回归。交付质量随任务、所选模型和审查配置变化 |
| 模型设置同步 | 本机浏览器用模拟接口打开真实项目与会话页面。项目页换模型后，同一个已打开的会话下次提交带上新模型；还验证了旧推理设置、重新加载、显式清空和会话到项目页的更新 |
| 跨入口续跑 | 2026-11-28 在临时数据库中用本机模拟模型联调。经 ACP 派发，子任务落盘后中断桌面进程，经手机入口恢复。核对同一个子任务只执行一次、父任务只收取一次，其他任务的结果保留原归属。空任务标识的历史通过真实 `wait_jobs` 验证按原运行隔离 |
| 外部客户端 | [ACP 接入说明](ACP_INTEGRATION_PLAN.md) 记录可用流程和限制。Zed 已验证派发、进度、审批和正文，Quick 与 JetBrains 的界面交互待实测。当前实现是入站桥接，依赖桌面引擎；出站调用其他 Agent 尚无生产入口 |
| 使用同一模型时，多 Agent 是否更快或更准 | [`scripts/eval_runner.mjs`](../scripts/eval_runner.mjs) 和 [`test/eval_cases/`](../test/eval_cases/) 已有生产执行器评测入口和夹具。当前证据适合逐案例判断。跨模型、跨工作负载的稳定收益仍需配对比较，记录验收结果、耗时、用量和父任务等待时间 |

## 继续维护时优先看这三处

1. **运行状态。** [`useAgentSession.ts`](../src/composables/useAgentSession.ts) 汇总实时事件、历史读取、人工交互和停止回执。可以先抽出明确的状态更新规则，让聊天、协作图和其他入口共享。
2. **长会话。** 先测首屏、切换和内存，再决定历史分页、增量事件和空闲面板回收的顺序。
3. **角色默认与评测。** 单文件修复、跨模块调查、独立复核适合分成三类实测。已有评测配置和验收用于比较同模型的单 Agent 与按需协作。用真实结果决定哪些角色出现在默认界面、哪些适合留在高级设置。

## 源码导航

| 目录或模块 | 职责 |
| --- | --- |
| [`src/`](../src/) | Vue 界面、执行时间线、会话与模型配置 |
| [`src-tauri/src/agent/`](../src-tauri/src/agent/) | Agent 运行、工具、上下文、协作与审查 |
| [`src-tauri/src/llm/`](../src-tauri/src/llm/) | 模型请求、流式响应、Key 选择和重试 |
| [`src-tauri/src/db/`](../src-tauri/src/db/) | SQLite schema、迁移与数据维护 |
| [`src-tauri/src/commands/`](../src-tauri/src/commands/) | 桌面前端调用的宿主接口 |
| [`src-tauri/src/agent/channel/`](../src-tauri/src/agent/channel/) | HTTP、移动端和桥接通道 |
| [`src-tauri/src/acp/`](../src-tauri/src/acp/) | 本机 ACP 子进程入口，复用桌面任务执行与数据库。参见 [ACP 接入](ACP_INTEGRATION_PLAN.md) |
| [`src-tauri/src/agent/mcp/`](../src-tauri/src/agent/mcp/) | MCP 连接、工具目录与会话引导 |
| [`src-tauri/prompts/`](../src-tauri/prompts/) | 核心提示词、工具手册与内置技能 |
| [`plugins/`](../plugins/) | MCP 配置目录 |
| [`scripts/`](../scripts/) / [`test/`](../test/) | 回归脚本、真实模型评测与测试夹具 |

增加或修改工具时，需要同时核对工具手册、实际参数解析、权限与副作用记录，以及模型观察/原文保存/界面投影。对支持的单步、串行、并行和计划树入口，应保持相同契约。
