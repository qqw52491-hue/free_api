# 角色、权限、流程与模型配置链路审查（2026-11-10）

## 基线与范围

- 起点：`main`，HEAD `d07bf1b3d03c3fff4561312508cc233966bdb902`。
- `git fetch origin` 成功；起点 HEAD 与 `origin/main` 一致，工作区干净。
- 重点对照 `d07bf1b`、`ace7834`、`3f15336`；同时回看 `c24e100` 的任务流程续跑策略及现有模型选择实现。
- 本轮只修改编辑器中的误联动、工具交集计算及已登记执行的回执查询顺序；不改执行器、数据库结构、默认审查策略或人工批准约束。
- 未运行付费模型或重启桌面应用。以下“集成”指临时数据库与生产函数的本地测试，不代表完整桌面或真实模型端到端实测。

## 具体调用链

### 1. 角色编辑、保存与默认配置

```text
RoleManagerView.vue
  form.model_id（可选角色模型，与 aiModelId 孵化模型分开）
  RoleExecutionSettings.vue
    publish → execution_defaults JSON 字符串
      profile
      execution.allowed_tools / workspace / workflow / stages
  saveRoleCard → invoke(upsert_role_card)
commands/roles.rs::upsert_role_card
tools/role_manager/mod.rs::upsert_card
  execution.rs::normalize → RoleExecutionDefaults::parse
    → ExecutionConfig::parse → StageSettings::validate
  → agent_role_cards.execution_defaults / model_id 等列
```

`execution_defaults` 仍以同一 JSON 保存执行方式、权限上限和流程默认值。这是存储上的组合，不能单凭字段放在一起就认定有运行缺陷。后续合并对工具权限取交集，对流程字段做显式覆盖，职责和契约另行使用。

未提交 `execution_defaults` 的旧调用保留原值；显式空字符串才清除。`allowed_tools` 缺省或 `null` 表示继承，`[]` 表示不允许业务工具；控制及交付入口保留。停用“角色默认值”会清除执行默认 JSON，角色自身模型和职责不随之停用。`d07bf1b` 的本地表单重置继续保留。

### 2. 子 Agent 派发

```text
loop_runner/dispatch_child_task.rs::dispatch_inner
  resume 时：child_task/presets.rs::inherit_resume
  presets::requested（task 不自动要求角色；worker/writer 有既有基础角色）
  dispatch::key（原声明 + 步骤 + batch slot；带父会话查询）
  参数预检
  dispatch::prepare
    已结束 / 已挂接 / 可从正式终态回收 → 返回原结果或状态
    新派发 / 需要实际续接 → merge_execution_defaults
  ExecutionConfig::parse / uses_main_executor
  角色启用状态、允许类型、自定义输入、模型选择与可用性检查
  child_reviewer/lifecycle.rs::begin_child_run_extended
  child_task/runtime.rs::spawn → main_loop
```

普通 `task/worker/writer` 沿用通用主循环。显式 `reviewer` 或隔离 writer 使用专用契约/适配路径；普通预设之间无需清掉兼容的流程默认值。切到专用模式时，无法支持普通流程属于真实能力差异，现有确认提示及后端拒绝仍保留。

### 3. 工具权限与宿主授权

```text
声明 execution.allowed_tools
  ∩ role_manager/execution.rs 中的角色工具上限
  ∩ lifecycle::begin_child_run_extended 中的父执行工具上限
  → agent_child_runs.execution_config_json（有效执行配置）
  → execution/runtime.rs::authorize_call
      loop_runner 主入口
      dispatcher/command_exec.rs 的实际执行入口与组合叶子
  → 原有工具、路径与资源授权检查
```

角色缺省不新增工具限制；子任务不能靠声明更多工具突破父执行上限。资源目录沿 `TaskGuard` / `workspace_access` 的父会话授权链处理，目录撤权继续影响受控工具。工具可见菜单是启动快照，实际调用仍核验；发现工具不授予权限。

工具列表不是命令正文的文件访问证明。本轮保留既有 Shell/资源边界，不把“交集正确”表述为所有命令内容均被隔离。

### 4. 模型选择

```text
dispatch_inner
  parent_model = context.last_execution_model
                 （旧上下文缺少事实时兼容 route_for_step）
  locked = 非空角色卡 model_id
           否则 worker/scout 或 writer 的人工槽位
  child_task/models.rs::select
    locked → 显式 dispatch model_id → parent_model
  validate（存在、启用、供应商配置可用）
  pin → 固定本次普通子执行的 planner/executor/pro/flash/vision
  登记 model_id 与 model_selection.source
```

- 无角色 `task` 没有必须填写的角色模型槽，继承父执行实际模型。
- 角色模型为可选的人工绑定；显式派发模型与人工绑定冲突时原规则报错，不静默覆盖。角色卡优先于普通角色槽位，本轮未改优先级。
- 独立审查的角色槽位/备用配置继续保留，`pin` 不把它们改成主执行模型。
- 同一次已登记执行恢复读取原 `model_id`；显式新追问尝试缺省继承前次模型，并重新验证当前可用配置。

### 5. 流程与 Checkpoint

```text
AgentComposer / StageSettingsFields
  → ModelRouting.stages + planMode + stage reviewer 配置
角色默认 JSON / 派发 execution
  → ExecutionConfig.workflow / stages
main_loop/mod.rs
  新任务：stages.for_child → checkpoint.stages
  同任务：restore_task_for_entry，保留 checkpoint.stages
  apply_entry_execution_mode（resuming=true 不覆盖原模式）
  bind_task_review_routing（恢复阶段审查选择与人数）
  → AgentCheckpoint 保存 / load_checkpoint 恢复
```

`agent_child_runs.execution_checkpoint_json` 保存派发输入/执行快照，`execution_config_json` 保存有效权限和流程；普通任务还使用独立 `AgentCheckpoint` 保存任务状态。声明快照与合并后的有效权限不是同一份事实。

合理依赖继续保留：方案审查开启必须有正式方案；关闭正式方案会关闭对应方案审查；人工批准必须有正式方案。它们不会顺带关闭阶段/最终审查。阶段审查字段、审查人数与执行模式分开更新。最终审查显式开启仍保留此前已确认的材料适用免审条件；关闭最终审查不撤销明确验证或既有有效阻塞。

## 问题分类与修复

| 编号 | 分类 | 原有具体行为与证据 | 本轮处理 |
|---|---|---|---|
| D1 | 真实缺陷，编辑模型误写运行模型 | `generateWithAi` 使用 `aiModelId || form.model_id` 回写；用真实处理函数和假 AI 回应复现已有绑定及空绑定被替换 | 删除该回写，孵化选择只用于本次 AI 编辑请求，增加说明；角色绑定保留用户选择 |
| D2 | 真实缺陷，专家模板替换草案 | 对 `d07bf1b` 原函数只读执行：编辑已有草案时不确认就清掉原工具/人工批准流程，同时残留前角色模型 | 活跃编辑器替换模板前明确提示配置替换；取消保留整份草案；确认载入的新角色模型为空。未编辑时直接载入。内部执行快捷组合继续保留兼容流程和工具限制 |
| D3 | 真实缺陷，工具交集误拒合法读取 | 角色仅允许 `local_file.read`，声明 `local_file` 时过滤为空；父执行仅允许 read、子任务声明整体工具也同样失败。两个生产路径回归修前均失败 | 共用 `ExecutionConfig::restrict_tools`，保留交集中更窄的动作名；不更改流程或阶段。读可用，写和 Shell 仍拒绝 |
| D4 | 真实缺陷，旧回执依赖当前角色状态 | 登记并结束任务，停用原角色，同一 key 回查在 `merge_execution_defaults` 处报“角色不存在或已禁用”。提取原生产准备顺序后回归修前失败 | 相同声明 key + 同父会话先回收已登记结果/挂接执行状态，再为新派发合并角色默认值。新请求、其他父会话仍不能借用旧回执 |
| R1 | 合理依赖 | 角色的允许挂载类型、专用适配器契约、模型人工绑定和父权限交集 | 保留，不为表面拆分改变行为 |
| R2 | 合理依赖 | plan / plan_review / plan_approval 的结构约束 | 保留；阶段/最终审查、工具列表和模型不随此联动 |
| U1 | 仅展示语义 | “自动”代表宿主既有策略；启动工具索引代表可见目录，最终审查“开启”不代表所有材料都适用 | 现有说明和行为一致，未发现需另改的纯展示回归；只补孵化模型用途提示 |

## 尚未扩展的边界与证据不足

1. 本轮 D4 覆盖已登记且已结束、仍挂接执行器、或已有正式终态可回收的路径。进程退出后仍标 running、没有终态的实际续接仍走原配置校验；旧 key 兼容也仍须完整输入匹配。没有覆盖“停机后修改/禁用角色，再从未完成子任务重建整树”的桌面实测，不能宣称全树配置完全冻结或可自动恢复。
2. 新建 `child_task.resume` 是新尝试，不是原执行 checkpoint 原地恢复；沿用旧声明并重新核验角色及当前父/宿主权限属于现有行为。角色卡当前限制变化可能影响新尝试，不能一概当作配置丢失。
3. 任务续跑固定原方案模式、阶段设置及审查选择/人数，允许执行模型配置按既有策略变化；没有把全部 `ModelRoutingConfig` 冻结进 checkpoint。旧 checkpoint 缺少审查快照时采用当前值并披露，本轮不猜历史配置。
4. 另见已有保存语义：`upsert_card` 保存自定义卡会设置 `enabled=1`，前端虽传 `enabled`，后端未消费它。可从 SQL 确认其行为，但“保存是否应保持停用”没有本轮明确产品约定；本轮不改变该默认，也不把它归因于上一轮提交。
5. 没有真实供应商、模型自主选择、桌面重启/UI 全流程实测。浏览器回归覆盖 `RoleExecutionSettings`；AI 编辑与顶层模板回归执行真实处理函数并模拟接口回应，不冒充完整 RoleManager 桌面交互。

## 验证记录

- 修前复现：D1 的真实编辑函数断言失败；D3 两项 Rust 生产函数回归均失败；D4 保留原合并顺序时回归因角色停用失败；D2 的起点函数实际输出显示未确认替换且遗留模型。
- 新增 Rust 回归：工具交集两层集成、9 组别名/空集/继承组合、无角色 task 路由、旧回执与新派发/父会话隔离。
- 定向 Rust：`agent::child_task::dispatch::tests` 7 项、`agent::execution::runtime::tests` 14 项、`agent::tools::role_manager::execution::tests` 10 项通过；完整套件另覆盖模型选择、权限撤销、组合执行、人工审批和 checkpoint 恢复。
- 前端：`test-agent-role-isolation.cjs`、`test-agent-policy-lock.cjs`、`test-agent-stage-integration.cjs`、`test-agent-stage-settings.mjs`、`test-agent-model-selectors.cjs`、`test-agent-task-resume.cjs` 均通过。其中阶段设置为 2 项测试，任务续跑脚本覆盖 8 个任务意图和 6 个会话创建场景。
- 真实浏览器：`CHROME_BIN=/opt/google/chrome/chrome node scripts/test-agent-role-settings.mjs` 通过；本机缓存 Chrome 再运行同一回归亦通过。
- 完整 Rust、检查与构建的最终结果见下节。
- 无新增模型调用、认知检测器、门禁或数据库迁移。派发准备增加有归属的登记/回执查询；只处理 dispatch/resume，不增加每轮提示词义务。

## 最终结果

- `cargo test --lib -- --skip clipboard`：1644 通过，0 失败，10 项原有忽略，1 项真实剪贴板测试过滤。
- `cargo check --tests`、`cargo build`、`npm run build`（含 Vue 类型检查）均通过。
- `git diff --check` 通过。
- 构建保留既有 `reviewer_persona` 未读赋值警告及 Vite 分包/混合导入提示；没有为清警告扩大本轮修改范围。
- 本轮差异仅包含上述小范围修复、对应回归与本文；分组本地提交，不自动推送。
