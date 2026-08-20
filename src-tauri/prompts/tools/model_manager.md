<tool_specific_instructions>
<summary>模型规格与渠道配置管理工具。支持维护公共模型规格词典（上下文窗口/最大输出等）并绑定到 LLM 平台。</summary>

当你需要查看、新增、修改或删除大模型物理规格（如 context_window, max_tokens, supports_thinking），或者为指定平台绑定新模型时调用此工具。

### 支持的动作 (action)
1. `list_specs`: 查看全局模型物理规格库（包含所有预设及自定义模型规格）。
2. `upsert_spec`: 添加或更新模型物理规格。
   - `name` (String, 必填): 规格名称，如 "DeepSeek-V4", "Claude-3.7-Sonnet"。
   - `context_window` (Number, 可选): 物理上下文窗口大小（Tokens），默认 128000。
   - `max_tokens` (Number, 可选): 最大输出 Tokens，默认 8192。
   - `supports_thinking` (Boolean, 可选): 是否为推理/思考模型，默认 false。
   - `default_temperature` (Number, 可选): 推荐默认温度，默认 0.7。
   - `description` (String, 可选): 规格描述。
3. `delete_spec`: 删除模型规格。
   - `name` 或 `id` (String, 必填): 要删除的规格名称或ID。
   - `confirm` (Boolean, 必填): 必须显式传 `true` 以确认破坏性删除。
4. `list_platforms`: 列出当前已配置的 LLM 渠道/提供商（名称与 Base URL）。
5. `add_model`: 给某个渠道绑定添加模型。
   - `platform_id` 或 `platform_name` (String, 必填): 目标平台名称（如 "硅基流动"、"OpenRouter" 或 ID）。
   - `model_name` (String, 必填): 平台实际调用的 API 模型标识（如 "deepseek-ai/DeepSeek-V3"）。
   - `display_name` (String, 可选): 前端显示名称。
   - `spec_name` (String, 可选): 引用的规格名称（将自动继承上下文大小与输出上限）。
   - `context_window` (Number, 可选): 若不传则优先继承规格库。
   - `max_tokens` (Number, 可选): 若不传则优先继承规格库。
6. `delete_model`: 从渠道中移除模型。
   - `model_id` 或 `model_name` (String, 必填): 模型标识或ID。
   - `confirm` (Boolean, 必填): 必须显式传 `true` 以确认删除。

### 使用范例
```json
{
  "tool": "model_manager",
  "command": {
    "action": "upsert_spec",
    "name": "DeepSeek-V4",
    "context_window": 256000,
    "max_tokens": 16384,
    "supports_thinking": true,
    "description": "DeepSeek 第四代混合思考旗舰模型 (256k 上下文)"
  }
}
```
</tool_specific_instructions>
