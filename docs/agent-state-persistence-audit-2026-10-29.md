# Agent 状态与持久化排查（2026-10-29）

> 状态更新：本文件记录修复前的审计结果；现已修复并验证，见 `agent-state-persistence-fix-2026-10-29.md`。附带复现补丁是针对修复前 a4eca06 相关文件的历史测试材料，不应用于当前已修复分支。

## 范围和结论

本轮是排查，不是修复。只检查 free_api Agent 的终态投影、检查点保存/冲突、错误处理与恢复保护；没有修改正式 Rust/Vue 源码、真实数据库、任务记录、模型或运行中的应用。没有调用外部模型或重启进程。

在 Git 忽略的 `src-tauri/target/state-audit-20261029/` 创建源码副本，新增 9 项本地审计测试。检查点测试直接调用未改动的生产保存/读取函数；通道测试为使用 Tauri MockRuntime，仅在副本中将 AppHandle 和 SessionRunGuard 等类型签名泛型化，函数体逻辑保持原样。最初两类测试夹具编译问题（PathBuf 参数类型、固定 Wry 签名）已修正；最终结果为实际运行后的断言结果，不把编译失败当缺陷复现。

新增审计断言结果：2 通过，7 失败；7 条失败归为以下三组缺陷，不是七个独立根因。未证明这些缺陷造成先前 302 业务修复失败。

## P1：已复现——业务失败或桌面取消，被通道收尾投影为 done

位置：`src-tauri/src/agent/channel/http/run_spawn.rs:136-163,176-206`。

- 调用正式 `failure::finish_failed_run`：失败检查点与 trace 已保存，函数返回 Ok（表示失败处理完成），之后按主循环包装器调用 `SessionRunGuard::finish`。
- 实际：`host_result=Ok checkpoint=failed trace=failed channel=done`。
- 调用正式 `control::finish_interrupted_run` 再调用守卫收尾，实际：`machine=Cancelled trace=cancelled channel=done`。
- 状态监听只更新 state_detail/phase，没有把 failed/cancelled 同步到 RunRecord.status；守卫 Ok 分支把非预先标记 cancelled 的记录统一改为 done。`channel-run-done` 的该分支也固定发送 done。
- HTTP run status/list 使用此 RunRecord，数据库会话列表则读 trace，因此不同查询可出现相反终态。网页把 done/succeeded 都显示为完成；本轮未进行桌面/网页 UI 实机点击测试，不宣称所有界面均显示错误。
- HTTP 取消入口会预先设置 cancelled，不能把桌面取消的复现泛化成所有取消入口必然相同。

建议：让收尾消费权威业务终态，不以 Result<(), String> 的 Ok 代替任务成功；持久化、RunRecord 与完成事件同口径。宿主异常和任务主动未完成仍须区分。

## P1：已复现——冲突重试会允许旧快照覆盖新状态

位置：`src-tauri/src/agent/checkpoint/mod.rs:686-725,733-768`。

通过实际 load/save 的两个快照模拟 A 先读、B 更新并保存、A 再保存的确定性交错，无需不稳定线程竞争。

1. B 保存 human_waivers 和 stage_review_batch_id；A 仅更新进度摘要再保存。实际保存成功但 human_waivers=[]、stage_batch=None，B 的状态丢失。
2. B 已写 Done；A 的旧快照仅补进度再保存，实际阶段退回 CallLlm。
3. B 保存新的 history_task_id/目标；A 的旧任务晚保存，实际任务身份与目标变回旧任务。

原因：冲突后取得最新 version，却继续以调用方的旧 data/phase/pending 等覆盖；其他字段也没有把最新值整体回填给调用方。重试成功不等于合并正确。

边界：这些测试确认底层保存接口在此交错下的行为，不证明真实桌面入口必然允许所有上述并发。评测观察器确有独立 load/save，但本轮未完成真实运行调度压力测试，不能据此估计事故频率。

建议：明确会话/任务/运行及字段写入归属；拒绝过期任务的整包回写，独立控制变更走明确增量通道，不使用通用“拿最新版本重交旧整包”替代冲突处理。显式续跑/新任务切换须保留，不可简单禁止所有终态变化。

## P2：已复现——SQL 写失败后，内存 revision 仍提前递增

位置：`src-tauri/src/agent/checkpoint/mod.rs:637-653,656-672`。

用仅作用于临时内存库的 SQLite RAISE(ABORT) 触发器模拟失败：

- 首次 INSERT 失败：数据库没有记录，但内存 version=1。
- UPDATE 失败：数据库 version=1，内存 version=2。

函数确实返回 Err，并非直接向调用者谎称成功。但只在 UPDATE 未命中时恢复 version，真正 SQL 错误路径没有恢复；调用者继续使用这个对象会持有与数据库不一致的版本。后续保存有机会通过重试恢复，不能把它直接等同于永久数据损坏。

建议：仅在提交成功后更新调用方 revision；SQL 错误及事务回滚均恢复内存状态，并测试后续重试。

## 仍是风险，未做端到端故障复现：忽略保存错误

- `main_loop/mod.rs:1863` 阶段审查派发后。
- `loop_runner/dispatch_local.rs:50` 敏感访问等待确认登记。

两处代码直接丢弃 save_checkpoint 的 Result。之后的正常步骤保存可能补上，因此不能直接声称必然丢失。风险主要是中途崩溃/持续存储故障时，提示/副作用与可恢复状态不一致。本轮未向真实存储注入故障，也未完成这两个完整入口的故障注入，不列为已复现端到端缺陷。

## 本轮没有证实为新缺陷的部分

- 普通保存已有专门的模型排除集合合并；新增正向测试确认同任务旧保存不会抹掉新排除项。
- 正常守卫完成的正向测试仍为 done。
- 中断处理内部虽然先尝试保存，外层 main_loop/entry.rs:149-160 对 Err 另有 stop_run_runtime 兜底，不能仅看内部问号就断言取消失败会永远留下后台任务。
- 重新执行既有恢复/回执/失败收停/人工停止审查/通道回归，133 项通过：checkpoint 68、child_receipt 24、failure 8、review_control 8、channel 25。这些通过不覆盖新增失败断言，也不证明没有其他缺陷。

## 可复验材料

- `docs/agent-state-audit-repro-2026-10-29.patch`：只包含本轮审计测试及 MockRuntime 所需的类型泛化，不是修复补丁；请在修复前 a4eca06 的隔离源码副本中用 `git apply --unidiff-zero` 应用，勿误当生产修复。
- 副本和日志：`src-tauri/target/state-audit-20261029/`。
- 顺序执行的清晰证据：`audit-tests-serial.log`；结果 2 passed / 7 failed，Cargo 真实退出码 101。
- 五组正向回归：`checkpoint-controls.log`、`receipt-controls.log`、`failure-controls.log`、`review-controls.log`、`channel-controls.log`。

在隔离副本 src-tauri 下设置独立 XDG_DATA_HOME 后运行：

```sh
cargo test --offline --lib audit_state_ -- --nocapture --test-threads=1
```

该命令在当前未修复逻辑上预期退出 101；不要用后续 echo/tail 的退出码掩盖它。先前完整 1416 项回归通过，与本轮发现不矛盾：这些交错和终态传递断言此前没有覆盖。

优先顺序：终态统一投影 → 检查点冲突与身份隔离 → 写失败内存回滚 → 两处忽略保存错误的故障注入。不要先做大规模状态重写或堆模型提示词。
