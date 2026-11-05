# 方案审查配置传递与执行模型自行免审修复

## 已核对的事故

在只读连接的真实数据库中，会话 `a8d8bea1-4702-4370-ad29-b55588116aa0` 保存了正式方案、单人方案审查及批准，但提交方案和生效分流均为 `final_review_mode=mechanical_only`、`stage_review_mode=none`。最终结果的模型标记为 `triage:mechanical_only`、耗时为零，确为机械通过，而非独立 Reviewer 通过。

旧 `handle_submit_plan` 在结构校验前直接调用 `apply_plan_review_overrides`，后者只验证枚举；方案审查材料的 `custom_inputs` 未包含这两个配置。另有第二条免审路径：即使保留 code_review，复杂只读任务也可能因为未新增写入及会话文件而机械免审。因此只阻止方案降级还不够。

失败的方案审查 `d621f8b3-3a0e-4063-ac1b-f8a4012bc2c3` 保存了 `tool=run_command`、顶层字符串 command；`get_params()` 保留字符串，下游直接赋值 `params["tool"]` 会 Panic。后续重派成功不代表该解析缺陷已消失。

复盘文字还有两点须区别：上一轮会话 `4805917f-b52b-4fa1-bcd5-be21ed137df2` 确有约 144 秒的独立终审，问题是取证不足；入口推荐两个角色不等于已选择双人方案会审。本次未据角色推荐数强制增加审查人数，也未重新核实复盘列出的全部测试统计。

## 修复

- `triage.rs`：方案仍可选择 code_review / operational_verification / artifact_quality，也可加强为 panel；执行模型不能把已有独立终审改为 mechanical_only、将 panel 改为非 panel 类型或将已有 milestone 改为 none。入口为机械免审的简单任务仍可沿用。按仓库既有不对称规则，放宽审查交人工决定，不靠模型自报理由取得授权。
- 复杂方案入口仍保留 milestone 和独立终审，但不再把入口已选 panel、operational_verification、artifact_quality 一律覆盖成 code_review。固定环节人工开关仍独立生效。
- `plan_submit.rs`：结构校验成功后应用配置；传递原值、申请值、实际保留的模式、固定环节开关及入口任务判断，既放入 custom_inputs，也放入证据摘要开头，避免只依赖角色模板渲染自定义字段。申请与实际值不同时反馈事实，不拒绝整个方案或增加审批轮次。方案审查批准不充当人工降审授权。
- `finish_diff.rs`：复杂方案路由及已选 operational_verification/panel 不再仅因只读且无文件而免审；保留简单任务的原有机械免审和实际机械异常处理。审查规则版本升为 v15，旧规则裁决不能按新规则指纹直接复用。
- `main_executor.rs`：非对象参数在授权、计数及执行前返回参数错误和修正形态，不再索引字符串引发 Panic，不消耗取证预算。
- 更新方案/执行/终审指引。终审关于 0 tests、失败命令、未运行真实模型及报告覆盖的对照是审查参考，不新增命令关键词检测器、覆盖率门禁或模型填报字段。

## 回归与边界

新增六项回归：保持入口已有审查方式、模型不能自批免审、实际方案 Reviewer 请求包含完整配置、真实事故字符串 command 与标量 params 不 Panic/不计数、复杂只读免审条件、多人与单人复杂只读任务实际调用独立终审。旧简单免审回归修正为 direct 路由，继续验证人数开关不会自行增加审查。

使用临时数据库、临时工作区和本机假模型 HTTP 回执验证，没有调用付费模型、改写真实记录、重跑已完成任务或重启桌面应用。历史已保存的 mechanical_only 判断不作猜测迁移；完成任务不会被自动补审。证据可读及指引完整不保证模型必然发现报告中的所有失实之处。

最终验证：`cargo test --lib -- --skip wayland_clipboard_end_to_end` 为 **1613 passed、0 failed、10 ignored、1 filtered out**；`cargo check --tests`、Rust 构建、`npx vue-tsc --noEmit`、前端生产构建及 `git diff --check` 通过。
