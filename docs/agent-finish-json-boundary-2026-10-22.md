# finish 正文边界与 JSON 兜底

## 已知事故与证据边界

2026-10-22 10:22:55（Asia/Shanghai），运行 `bc59fbfb-147d-4aea-9d63-5118824e5c3b`
的步骤参数、completion_json 和正式 assistant 消息均已将 `plan_title`、`tool:"finish"`
片段保存到 summary 内。问题发生在展示之前，不是用途/指令分层排版产生的。
该次模型原始响应未找到对应归档，不能确认其具体格式错误或唯一触发路径。

使用实际 `repair_json` 源码和合成报文，复现了同形态污染：summary 内含未转义引号时，
旧兜底从 summary 开始截取其余文本，仅用少数状态字段名划边界，再拼成新的 finish。
后面的 plan_title/tool/details 因此可能被吞进正文；其他工具只要含坏掉的 summary，
也可能被错误升级成 finish。合成复现不冒充真实原始响应。

## 局部修复

- 删除 `repair_json` 最后的 summary 抓取、尾部状态字段猜测及新 finish 合成。
- 不能解析的报文返回 None，由实际指令解析入口返回格式错误；调用方继续使用已有原上下文
  补正流程和既有最多两次尝试。不增加模型字段、审查门禁、调用预算或前端正则清理。
- 仅在成功解析为结构化指令后执行原工具与状态更新；不从本次坏报文中提取并执行 Todo/契约更新。
- 保留合法 JSON、command/params 别名，以及原有注释、尾逗号、单引号、换行与 EOF 补全等兼容。
  本轮不重写整个提取器，不宣称所有不完整 JSON 都已被禁止或所有语义错误都能机械识别。
- 合法正文里的 Markdown、引号、JSON 示例、协议字段字面量及结尾符号保持原文。

## 回归

两项新增事故回归先在旧实现运行，结果为 **0 通过、2 失败**：
含协议尾部的坏 summary 被当作可执行 finish，非 finish 工具也被错误改成 finish。
修复后 utils 定向回归 **33 通过、0 失败**，包括新增的合法 Markdown/JSON 示例完整保留检查。
已有针对不明确 summary 的“抢救成功”测试改为要求不能合成指令或提取状态更新。

测试使用临时应用数据目录和合成字符串，不依赖真实日志长期存在，也不调用模型。
最终完整 Rust 回归 **1282 通过、0 失败、10 项原有忽略、1 项剪贴板测试过滤**；
`cargo check --offline` 通过（保留 3 项既有警告），`git diff --check` 通过。

```sh
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib agent::utils::
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib -- --skip wayland_clipboard_end_to_end
cargo check --offline --manifest-path src-tauri/Cargo.toml
git diff --check
```

## 运行边界

没有重写旧聊天、旧收据或真实日志，没有自动续跑或重启应用。
历史上已污染但本身合法的字符串无法靠本次解析修改可靠还原，不能按字段关键词删除正文。
后端重新编译并重启后才生效。格式补正仍可能失败，沿用原失败/恢复出口，不保证真实模型一次补正成功。
