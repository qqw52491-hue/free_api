# Pipeline 终审证据摘要与原文定位修复

## 已复现的问题

旧顺序 pipeline 把子步骤回执拼为 `[Step N Result]: …`，终审证据索引却统一走普通工具的「前两行 / 140 字符」摘要。前段文件写入回执较长时，后段 shell 的 HTTP 302、Location、编译输出没有进入默认审查材料。长任务早期补充索引原先只包含 local_shell / shell_exec，较早的 pipeline 也会被后续步骤挤出。

先增加真实旧格式的本地回归，再运行旧实现：测试失败，摘要只有第一段文件写入文字，缺少后面的 302。日志位于本机 `/tmp/free-api-pipeline-review-20261029/red.log`。

## 本次改动

- `reviewer_gatekeeper/evidence.rs`：按宿主持久化 tool=pipeline 选择专用摘要；识别行首旧格式文本标签，首尾各保留两段，段内给命令与 stdout/stderr 分别留有界预算，避免前段写文件或长脚本淹没后段输出。
- 每个展示段附原文 Unicode 字符范围及可直接调用的 read_step_output JSON，输出段定位直接跳过长脚本。未识别格式保留有界首尾及原文读取入口。
- 明示分段只是文本定位，不把工具正文里的标签当作独立子步骤执行状态；省略中间段与段内截取均明确披露。最多展示四段，不保证摘要涵盖任意长输出的所有关键信息。
- 只在摘要投影中移除完整的重复结构化元数据注释，保留注释之后的回执和错误；未闭合注释明确披露。原始保存内容及读取接口不改。
- 早期补充索引包含 pipeline，但仍只列元数据，不解压旧正文、不机械重跑检查。保留任务起点及会话归属边界。
- `reviewer_gatekeeper/finish_diff.rs` 仅增加测试夹具和集成回归；生产终审提示词、模型、审查人数、预算、裁决和执行协议均未改动。

## 验证

使用独立 XDG_DATA_HOME；未调用外部付费模型。新增四项回归覆盖：

1. 长 Unicode 文件回执 + 长脚本 + HTTP 302 / Location + 外层零退出码下的 BUILD FAILURE / stderr；按摘要请求精确读取原文、跨会话拒绝、原文不变、较早 pipeline 元数据保留。
2. 多段长输出有界展示、显式省略和 Unicode 偏移。
3. 无法识别的标签、超长编号、空正文和元数据注释边界。
4. 本机 HTTP 假审查模型经正式终审入口收到 302 及原文入口，可取回持久化回执；模型给出的 rejected 被正常保留。此测试验证材料与裁决传递，不证明真实模型必然识别问题。

结果：

- `cargo test --lib pipeline_ -- --nocapture`：13 通过。
- `cargo test --lib reviewer_gatekeeper:: -- --skip clipboard`：76 通过。
- `cargo check --tests`：通过，保留既有 reviewer_persona 未读赋值警告。
- `cargo test --lib -- --skip clipboard`：1416 通过，0 失败，10 项既有忽略，跳过 1 项桌面剪贴板测试。
- `git diff --check`：通过。

日志在本机 `/tmp/free-api-pipeline-review-20261029/`，未纳入 Git。

## 生效与限制

本轮只改 free_api Agent 源码，没有继续修改 sx_jj_graph 服务，没有改写真实任务、配置或模型选择，也未提交 Git。保留工作区原有 shell 进程状态边界及前端 Markdown 修改。

未重启正在运行的桌面应用、未进行桌面 UI 交互或真实供应商模型测试。需要重新编译并重启 free_api 后端后生效。没有根据 302 / BUILD FAILURE 关键词自动改判业务失败，没有增加新门禁或强制审查步骤；退出码、验证目标、服务方法结果与真实用户请求是否一致，仍需模型依具体任务和原文判断。
