<tool_specific_instructions>
<summary>MCP 外部插件管理工具。支持极速接入远端 HTTP MCP、切换与删除插件，并自动调用 LLM 提炼高密度能力卡入库 SQLite。</summary>

当你需要查看、接入、切换或删除 MCP 插件时调用此工具。
如果用户要求接入 MCP 但未提供关键信息（如缺少 URL、Token 或未指定模型），请先调用 `ask_human` 附带动态表单（或 `custom_html`）让用户选择/填写，获取数据后再调用本工具完成入库。

### 支持的动作 (action)
1. `list`: 查看当前所有已配置的 MCP 插件列表（数据库配置与能力卡；环境变量只显示键名，不显示值）。
2. `add_http`: 极速接入远端 HTTP MCP（原生 Rust HTTP 直连，零外部进程依赖；自动测试握手并调用 LLM 生成能力卡保存进数据库）。
   - `url` (String, 必填): 远端 MCP 服务的完整 URL，如 `https://mcp.example.com/mcp`。
   - `bearer_token` 或 `token` (String, 可选): 鉴权 Bearer Token。
   - `name` (String, 可选): 插件标识名，留空将自动从 URL 解析。
   - `transport` (String, 可选): 传输协议；当前仅支持 `"streamable-http"`（默认），传入其它值（如 `"sse"`）会被拒绝。
   - `model_id` (String, 可选): 提炼能力卡所用大模型 ID，留空使用默认大模型。
   别名：`quick_connect` 与 `add_http` 完全等价。
3. `toggle`: 启用或停用插件（在下一次任务会话同步时生效）。
   - `name` (String, 必填): 插件名称。
   - `enabled` (Boolean, 必填): `true` 为启用，`false` 为停用。
4. `delete`: 从本地数据库中移除插件（运行中的插件将在下一次任务会话同步时卸载）。
   - `name` (String, 必填): 插件名称。
   - `confirm` (Boolean, 必填): 必须显式传入 `true`。

注意：本工具当前不支持本地 stdio 插件接入（add_stdio，请改用 plugins 目录下的 YAML 配置文件）与批量导入（batch_import，请逐个调用 `add_http`）。

### 使用范例
```json
{
  "tool": "mcp_manager",
  "command": {
    "action": "add_http",
    "url": "https://mcp.wxhjy.dpdns.org/mcp",
    "token": "sk-example-token-123",
    "name": "remote-http-mcp"
  }
}
```
</tool_specific_instructions>
