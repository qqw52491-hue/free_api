<tool_specific_instructions>
<summary>MCP 外部插件管理工具。支持极速接入远端 HTTP MCP、本地 stdio 命令行插件、批量导入 JSON/清单，并自动调用 LLM 提炼高密度能力卡入库 SQLite。</summary>

当你需要查看、接入、批量导入、切换或删除 MCP 插件时调用此工具。
如果用户要求接入 MCP 但未提供关键信息（如缺少 URL、Token 或未指定模型），请先调用 `ask_human` 附带动态表单（或 `custom_html`）让用户选择/填写，获取数据后再调用本工具完成入库。

### 支持的动作 (action)
1. `list`: 查看当前所有已配置的 MCP 插件列表（包含连接状态与能力卡）。
2. `add_http`: 极速接入远端 HTTP MCP（自动建立 stdio 桥接、测试握手并调用 LLM 生成能力卡保存进数据库）。
   - `url` (String, 必填): 远端 MCP 服务的完整 URL，如 `https://mcp.example.com/mcp`。
   - `bearer_token` 或 `token` (String, 可选): 鉴权 Bearer Token。
   - `name` (String, 可选): 插件标识名，留空将自动从 URL 解析。
   - `transport` (String, 可选): 传输协议，`"streamable-http"`（默认）或 `"sse"`。
   - `model_id` (String, 可选): 提炼能力卡所用大模型 ID，留空使用默认大模型。
3. `add_stdio`: 接入本地命令行 stdio MCP 插件。
   - `name` (String, 必填): 插件标识名称，如 `"excel"`。
   - `command` (String, 必填): 启动命令，如 `"uvx"`, `"npx"`, `"/usr/local/bin/my-mcp"`。
   - `args` (Array of Strings, 可选): 命令行启动参数，如 `["excel-mcp-server", "stdio"]`。
   - `env` (Object, 可选): 环境变量键值对映射。
   - `capability_card` (String, 可选): 自定义能力卡，若留空系统将自动连接工具并由大模型提炼。
4. `batch_import`: 批量导入 MCP 插件清单。
   - `input_text` (String, 必填): Cursor/Claude Desktop `mcpServers` JSON 格式文本，或多行 `https://... [token]` 列表。
   - `model_id` (String, 可选): 提纯能力卡所用大模型 ID。
5. `toggle`: 启用或停用插件。
   - `name` (String, 必填): 插件名称。
   - `enabled` (Boolean, 必填): `true` 为启用，`false` 为停用。
6. `delete`: 从本地数据库中移除插件。
   - `name` (String, 必填): 插件名称。
   - `confirm` (Boolean, 必填): 必须显式传入 `true`。

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
