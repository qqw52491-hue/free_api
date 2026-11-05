# 真实测试复盘：被测对象、审查上下文与角色推荐

## 核对来源

本机数据库只读连接。父会话 `fd87e89f-bdf2-4d46-bb9a-806ea245e83b`，运行 `86f3becf-3f5c-4276-95bf-8579e7791991`，2026-11-05 约 10:09 至 10:23（北京时间）。保留已有未提交修改，未续跑、改写或删除真实记录。

- 真实子执行：`6609bec5` 为方案审查，`92907e95`、`210388ac` 为阶段审查，`63aa7270` 为终审；四项均 succeeded。父任务 operational_verification/milestone，固定开关 `{}`。
- 终审 score=96、duration_ms=37835。终审子执行 step 0 实际调用 get_step_output，读取 `abbac7d7-a4cd-4619-ac52-74b3c0b55530`（A/B 脚本回执）；step 1 交回 passed。其取证描述提及四个步骤，但参数只有一个 output_id，不能据描述推断已读四份原文，也不能说“终审完全没有主动取证”。
- 父步骤 seq 20–25 的原始 Shell 命令包含自建 SQLite 表和 Python 模拟器：D 定义 FileLockSystem，E/F 定义 ReviewGateStateMachine/EvidenceLedger，G 定义 LongChainOrchestrator。未调用这些机制的 Rust 生产入口；PASS 不能支持“生产实现跨模块端到端集成通过”。A/B 首次失败后的脚本修改只证明修正了测试准备，未证实产品 Bug。
- 会话 tmp 的 test_summary.txt 第 47 行确有 `1614 passed; 0 failed; 10 ignored; 0 filtered out`。此前修复者的 1613 是过滤真实剪贴板后的数量，两者并不矛盾。
- 最终 Reviewer 已收到部分脚本节选，还读了 A/B 原文，但仍将替身实现通过接受为生产集成证据。这一部分是证据解释错误，不只是工具不可达。没有通过增加“必须读 N 次”或按 Python/Rust 语言判真假的门禁处理。

## 两个需要纠正的解释

Reviewer 子检查点四个环节均为 false，是 handoff 审查执行器避免再次派方案、阶段审查、终审的内部设置。不是父任务审查被关闭，也不是旧消息覆盖父配置。但原提示未注明作用域，容易误解。

角色数组是推荐候选，不是必派全员名单；会审开关和三人上限另行决定人数。旧代码确有不一致：方案固定 Tech Lead，阶段只自适应挑卡，均未消费对应推荐；终审只解析名单第一项，不会在它失效时继续尝试后面的候选。

此外，现有 Rust lib 测试不是清一色的孤立单元测试。比如 agent_mailbox::coordination 中 worker_send_to_ended_member_replies_directly_and_all_receipts_allow_root_completion、agent_mailbox 中 real_worker_loop_receives_peer_data_and_sends_a_structured_reply，以及正式方案/终审假模型 HTTP 回归，调用了项目实际入口、临时数据库和结果回收。它们已有集成价值；本轮 Python 模拟矩阵没有增加报告声称的相应生产覆盖，也不能据此否定已有 Rust 集成回归。

## 修改

1. `review_tools.rs` 共用测试证据审查参考，进入首次/续接方案、阶段和终审材料：看真正被测的项目函数、命令、API，区分替换外部依赖和重写被测状态机。使用 mock 或 Python 本身不构成缺陷，Rust 本身也不保证调用生产实现。`code_verify.md` 同步给执行 Agent 设计及报告边界参考，不新增必填表格、测试数量或语义检测器。
2. `stage_settings.rs`、主循环区分“当前 Reviewer 子任务内部配置”和普通任务固定配置；审查子任务不再收到允许普通子任务继续委派的通用说明。父任务配置、triage 模式和方案请求单独移交。方案/阶段通过独立消息渲染，不受 5500/6000 字符证据摘要截断；终审经正式交付入口加入父配置材料。来源标为实际 checkpoint 字段，不将来源未知的配置猜成人工决定。
3. `review_team.rs` 共用首个可用主审候选选择。三类审查接入对应的推荐列表，跳过不存在、停用、排除或未授权 reviewer 的候选，再回退各自既有策略。阶段显式角色仍优先且保持单人，固定关闭和旧关闭语义保留。多人名额仍从既有专家池补齐；推荐列表长度不增加人数。
4. get/read_step_output 回执增加 single_output 范围说明，明示单次请求仅覆盖该 output_id，未读的其他步骤和后续分页不能由描述代替。保留原文、来源、分页和读取权限。没有新增阅读次数门禁。
5. 终审规则版本升至 v16，避免新材料语义复用旧规则的审查指纹。

## 验证与限制

新增三项使用生产实现的本地回归：真实角色数据库的候选/权限/人数选择，阶段显式角色及开关优先级，真实检查点与审查准备路径中的父/子配置分离。扩展实际假模型 HTTP 请求回归，检查独立父配置消息、测试证据参考及首项失效后的真实角色人设；扩展真实 get/read_step_output 别名回归，核对单回执范围。假模型只替代远程模型响应，没有替代上述被测逻辑。

未付费模型复测、未重启桌面应用、未改写历史报告或真实会话，不宣称这类模型误判已经消失。本次没有执行覆盖全部 A–G 的新生产端到端测试，也没有运行 Flash/强模型对比。此前讨论的“人工显式开启仍有材料适用免审例外”和完整操作者身份审计，不在本次修复中改写。

最终验证：完整 Rust lib 回归 **1616 passed、0 failed、10 ignored、1 filtered out**（仅过滤真实剪贴板）；`cargo check --tests`、Rust 构建、前端类型检查、前端生产构建及 `git diff --check` 通过。测试数不是生产场景覆盖率，也不是模型识别能力的测量。
