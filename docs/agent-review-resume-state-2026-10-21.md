# 多成员审查对质的状态归属修复

## 已确认的问题

本轮从当前工作区及 HEAD `5dc6a3f` 的代码确认两条机械路径：

1. `resume_child` 对质在方案未批准时调用 `reserve_plan_review_round`，而预扣预算同时清空整个
   `plan_review_deliveries`。只复审 B 也会丢失 A、C 在当前判断中的结论。
2. 共享 TODO 撤回只匹配描述前缀 `[审查发现] [F1]`，不区分批次和逻辑任务。
   A 撤回 F1 可能误关闭 B 的 F1。

这些不是已在真实会话复现的事故归因。本次使用临时数据库与合成历史验证修复行为，
不从测试通过推导真实模型的判断质量。

## 修复后的语义

- 预扣预算只计数、标记等待。提交新方案时由原有批次入口重置当前批次；历史原件继续留在子任务台账。
- 合法回执按 `batch_id + task_id + attempt_id` 关联。只采用该逻辑任务的最新有效裁决，
  保存被取代的历史回执；晚到的旧回执不再执行旧定稿、旧升级等当前状态副作用。
- 具体发现按同一任务下的 finding ID 更新。旧问题须由本任务明确撤回，或明确更新其发现内容；
  新回复省略旧问题、复审在途、超时、无效输出，都不证明旧问题解除。
- 稳定问题编号继续使用已有 `objection_verdicts` / `withdrawn_findings` 协议；续接材料明确旧问题沿用编号，
  新问题使用新编号。不增加模型必填字段，也不根据自然语言相似度猜测是否为同一个问题。
- TODO 的宿主来源绑定为批次、逻辑任务及 finding ID。两位成员即使都叫 F1、文字完全相同，
  仍分别登记和撤回。公开 TODO 参数不变。
- 来源和 TODO 绑定随现有 checkpoint 保存，新增字段缺省为空，兼容旧快照。
  旧 TODO 只在历史记录能唯一确定来源时自动关联；歧义时保持原状态并披露，不按编号批量关闭。
- 具体阻塞、修订/升级裁决、参考评分和状态卡从相同的任务身份推导。
  失败尝试不进入合法结论缓存，继续走已有的备用或失败出口。

示例：

| 历史与新结果 | 当前状态 |
|---|---|
| A 阻塞、B 阻塞、C 批准；B 开始复审 | A、B 阻塞保留，B 复审仍在途 |
| B 明确撤回 F1 并批准 | 只关闭 B 的 F1；A 的 F1 仍有效 |
| B 新结果先到，旧回执后到 | 历史保存，当前裁决不倒退 |
| B 超时或输出补正失败 | A、B 旧结论保留，披露本次审查未完成 |
| B 撤回 F1，另报 F2 | B 的旧 F1 关闭；A 的 F1 和 B 的 F2 保留 |

## 代码边界

- `checkpoint/plan_ops.rs`：拆分预算与批次重置，阻塞/裁决聚合及旧回执副作用隔离。
- `checkpoint/review_state.rs`、`checkpoint/mod.rs`：来源持久化、当前有效视图和自动 TODO 的来源绑定。
- `child_receipt.rs`：先接收有效回执再同步待办，其他批次不更新当前状态。
- `child_reviewer/resume.rs`、`loop_runner/dispatch_resume_child.rs`：抽出界面无关的实际续接入口，
  共用历史恢复、契约检查、预算预扣、任务创建和派发，以便集成回归覆盖真实路径。
  输入/预算预检失败不会先创建 running 孤儿；任务创建失败退回本次预扣。
- 两项旧测试调整夹具：撤回必须有同一逻辑任务来源；阻塞数量由实际发现支撑，不仅手改计数器。

没有新增数据库表、全局任务状态机、模型调用或审查角色，也没有放宽现有审查预算。
阶段触发条件和终审人数策略不在本次修复中调整。

## 验证

新增 10 项本地回归，覆盖单成员复审、同名/同文 F1 隔离、未声明撤回、新旧回执乱序、
无效输出、真实静默超时（本机服务加虚拟时钟）、新增发现、旧快照恢复/歧义、跨批次撤回及预算耗尽。
测试经过生产续接核心入口、实际 HTTP 请求、落库、定向轮询和统一回执消费者；未启动桌面应用。

```sh
XDG_DATA_HOME=$(mktemp -d /tmp/free-api-review-state-XXXXXX) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib -- \
  review_state_tests agent::child_receipt::tests agent::plan_gate::tests agent::checkpoint::tests

XDG_DATA_HOME=$(mktemp -d /tmp/free-api-review-state-full-XXXXXX) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib -- \
  --skip wayland_clipboard_end_to_end

cargo check --offline --manifest-path src-tauri/Cargo.toml
git diff --check
```

结果：相关 87 项通过；最终完整 Rust 回归 1255 通过、0 失败，10 项原有忽略，
跳过 1 项会覆盖系统剪贴板的测试。`cargo check`、差异空白检查通过。
全量中原有门禁夹具只改计数器而不含发现，已改为具体发现及明确撤回后通过，保留原拦截/放行断言。

## 补充：发现降级与 TODO 同步

审查员明确把同一发现从 `blocking` 改为 `suggestion` / `question` 时，原聚合层已移除阻塞，
但 TODO 同步只读取 blocking 视图，且仅凭明确 withdraw 关闭旧待办，导致两层状态不一致。
两项新增回归在修复前都实际失败于旧 TODO 仍为 pending。

现在先推导包含所有等级的当前发现视图，阻塞聚合和 TODO 同步共用该视图：

- 同一批次、同一逻辑任务、同一 finding ID 的明确降级，更新原 TODO 的参考内容并关闭其阻塞待办；
  不新增参考待办，也不关闭其他成员的同名 F1。
- 后续回复省略这个发现时保留已知等级；再次明确升级为 blocking，恢复原 TODO 为 pending。
- 未声明撤回、失败、旧回执、其他批次和用户编辑后的来源保护保持不变。
- 两项回归覆盖 suggestion/question 降级、交付待办检查、恢复后同步、其他成员隔离和再次升级。
  降级经过实际续接 HTTP 与统一消费者；连续多版本的省略/升级使用合成落库回执检验消费者，
  不放宽生产审查预算。

### 补充修复验证结果

新增 2 项回归；`review_state_tests` 定向 12 项通过。完整 Rust 回归 1257 项通过、0 失败，
10 项原有忽略，跳过 1 项会覆盖系统剪贴板的测试。
`cargo check --offline --manifest-path src-tauri/Cargo.toml`、`vue-tsc --noEmit`、
`git diff --check` 均通过。测试使用临时数据目录，未启动桌面应用。

## 限制与生效

未调用付费模型、未测试真实供应商表现、未自动续跑真实任务；不保证模型总会正确保持问题编号含义。
已发生的历史误关闭/清空不做猜测性回填，也不迁移或改写真实会话。
旧记录来源有歧义时，需要按具体证据核对，系统不会凭 F1 自动认定归属。

后端重新编译并重启后生效。按用户要求，本次补充修复与此前尚未提交的多人审查相关改动
一起纳入本地 Git 提交，不推送远端。
