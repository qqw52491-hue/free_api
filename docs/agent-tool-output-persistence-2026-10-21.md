# Agent 工具结果：先保存，再采样

## 事故与根因

运行 `1ccdea85-7aa4-4282-b758-0f88c29f4e54`（太原天气汇总到 Excel）中，并行工具实际抓取了网页，
但 `PlanExecutionReport::to_llm_feedback` 先把 JSON 结果截成前 2800 字符，附上“全文已持久化”提示。
上层保存的已是约 2927–2929 字符的报告；实际 blob 内容及哈希核对一致。
`get_step_output(offset=3000)` 返回空，`offset=2900` 只读到截断提示，后续出现重复抓取。

并行部分失败时，成功分支还会被提前替换为“数据已落盘”，重试记录只保存 stderr，且轨迹没有关联这次失败的 output_id。
这不是 SQLite 随机丢数据，也不能仅归因于模型能力。早期截断代码在 Git 历史中至少可追溯到 2026-08-21。

## 本次边界

- 计划树报告保留完整已返回结果，不在持久化之前截断或删除成功分支。
- 串行计划树继续 fail-fast，失败时保留既有 Scope 中的已执行结果，不运行后续写入。
- 正常收尾和失败重试共用原始输出选择逻辑；失败时同时保留 stdout/stderr。
  失败记录成功落库后才向模型提供真实 output_id，并绑定到轨迹及事件。
- 单独 MCP 调用已有 raw sidecar，仍保留 `_meta`、structuredContent、二进制及供应商扩展字段。
  混合原生工具的 parallel/plan_tree/commands 在此包内追加完整执行回执，避免 MCP 证据包覆盖原生同批结果。
  默认回查投影包含完整执行文本，raw=true 仍取原始 MCP 包；旧记录保持兼容。
- Agent 的 web_fetch 保留清洗后的完整返回正文，退出保存前 6000 字符截断。
  普通聊天 URL 附件仍保留其明确标注的 6000 字符预览，不把两种入口混为一谈。
- 复用已有父步骤 output_id、SQLite/CAS、分页和搜索，没有新增数据库表或分支身份协议。
  完整结果保存后再进入既有 4000-token 观测采样预算；不提高模型上下文或并发上限。

## 对其他工具的检查

不是所有工具都存在同一个缺陷：单独 MCP 的原始保存已存在；原生单步结果也没有计划树的 2800 字符截断。
实际额外修复的是 web_fetch、串行失败、混合 MCP/原生结果和失败重试归档。
local_file 的请求行窗/2MB 拒绝、搜索分页、同步 shell 的 50KB 工具输出上限仍保留。
shell 超限结果本身有 truncated 标记，本次不承诺保存进程产生的无限量原始字节。
第三方 MCP 服务在返回之前自行截断的内容，也无法由宿主恢复。

## 回归与限制

临时目录/内存数据库/隔离 XDG_DATA_HOME 与 FREE_API_OUTPUTS_DIR；不改真实会话、不调用付费模型。

```sh
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib output_persistence_tests -- --test-threads=1
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib agent_web_fetch_preserves_reader_and_html_fallback
XDG_DATA_HOME=$(mktemp -d) FREE_API_OUTPUTS_DIR=$(mktemp -d) \
  cargo test --offline --manifest-path src-tauri/Cargo.toml --lib -- --skip wayland_clipboard_end_to_end
cargo check --offline --manifest-path src-tauri/Cargo.toml
git diff --check
```

前两项测试修复前失败，修复后通过。额外验证完整 Unicode 分页拼接、末尾搜索、失败状态、
原有上下文预算、串行 fail-fast、本机 Python MCP 真实传输的六种成功/失败/混合调用及 raw=true 回查，
以及本机 HTTP Reader/HTML 回退结果超过 6000 字符时末尾仍保留。
另覆盖失败工具只有 stdout、以及 stdout/stderr 同时存在的落库回查，避免失败提示替代实际已返回的数据。

最终结果：完整 Rust 回归 **1277 通过、0 失败、10 项原有忽略、1 项剪贴板测试过滤**；
`cargo check --offline` 通过（仍有 3 项既有警告），`git diff --check` 通过。
没有执行真实模型续跑或桌面端手动验证。

旧记录里已丢失的正文不会自动补回。本次也没有把 next_tool_hint 当成可执行工具，
尚未修复缺工具名节点默认 core 的参数诊断问题。生效需要重新编译并重启后端，不能据本地回归保证真实模型不再重复取证。
