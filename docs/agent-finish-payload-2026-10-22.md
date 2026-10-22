# finish 有效参数与交付正文统一

## 事故与证据边界

2026-10-22 11:02:02（Asia/Shanghai），运行
`d4b7d8f8-1a21-4cad-acce-4fbb153d69eb` 的 finish 步骤参数含 235 字符摘要和
2724 字符 Markdown 正文，但步骤 output 与 completion_json 均为「任务已完成」。
这证明正文存在于步骤参数，缺失发生在最终交付保存之前；不是页面单纯漏渲染 Markdown。

步骤记录的 command 和 params 都来自 `get_params()`，并非两份原始模型字段。
没有找到该次原始响应归档，因此不能反推原始 command 的具体值，也不能把下面的合成形态
当作真实响应。已确认的代码缺陷是：记录使用有效参数，两个 finish 执行入口及验收载荷
却直接读取 command，遇到兼容参数形态会得到不同的正文。

## 可复现边界

合成合法指令 `{"tool":"finish","command":"","params":{"summary":"摘要","details":"# 完整正文"}}`
经过现有归一化后，`get_params()` 能取得正文，但旧验收载荷取得空字符串，details 缺失。
旧执行入口还只选择 summary 或 details 的其中一项，空 summary 会遮住非空 details。

新增两项测试在修复前均失败：真实 dispatcher 未返回完整正文，真实验收收据遗漏 details。
不依赖真实日志长期存在，不调用付费模型。

## 局部修改

- 将既有载荷组装迁到 `finalization::delivery_payload`，finish 通过已有 `get_params()`
  读取有效参数；prepare_finish 保持原有显式请求来源与优先级。
- dispatcher 和 loop_runner 的 finish 特殊入口共用 `finish_output`，正文统一由
  既有 `delivery_body` 投影，保留摘要、长正文和 Markdown，重复正文仍只显示一次。
- 验收、指纹、收据和正式提交使用同一份载荷。正式提交仍取已评估收据，不能由未核验的
  新副本覆盖它；没有新增模型字段、填表要求、审查调用或语义门禁。
- command 对象与 params 并存时继续遵循原有效参数优先级，不把两份内容自行拼接。
  未提供任何正文的情况保留原默认行为；本轮不设计新的空正文门禁。

## 回归范围

新增测试覆盖 command 对象、params、null/空/动词 command 配合 params、JSON 字符串
command、顶层参数、冲突参数优先级、空摘要加非空正文及 Markdown/JSON 示例完整保留。
集成测试经过归一化、真实 dispatcher、真实验收、检查点序列化恢复、正式提交，核对事件
载荷、completion_json 和 assistant 消息的完整正文。

已有本机假模型测试改为以 params 提交修订正文：修订须进入新审查，同正文才可复用收据。
假模型仅用于本机测试，不接触付费供应商。

真实结果：新增两项回归修复前 **0 通过、2 失败**，修复后 **2 通过、0 失败**。
包含本轮与工作区既有 JSON 边界修复的完整 Rust 回归 **1284 通过、0 失败、10 项原有忽略、
1 项剪贴板测试过滤**；`cargo check --offline` 通过（保留 3 项既有警告），差异检查通过。

```sh
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib finish_parameter_aliases
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib -- --skip wayland_clipboard_end_to_end
cargo check --offline --manifest-path src-tauri/Cargo.toml
git diff --check
```

## 运行边界

不改写真实历史聊天、收据或日志，不自动续跑、重启应用或提交 Git。
后端重新编译并重启后生效；已保存成默认句的历史回答不会因此自动恢复。
本轮不宣称已确认该次原始响应的具体字段形态，或已覆盖所有不合法参数组合。
