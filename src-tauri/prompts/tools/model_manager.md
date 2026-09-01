<tool_specific_instructions>
<summary>模型规格、LLM 提供商连接与平台模型绑定管理工具。支持维护公共规格词典、创建提供商连接，并向已有平台绑定模型。</summary>

当你需要查看、新增、修改或删除大模型物理规格（如 context_window、max_tokens、supports_thinking），创建或维护 LLM 提供商连接，或者为已有平台绑定模型时调用此工具。

**新增提供商与模型的固定顺序**：先调用 `list_platforms`；若目标平台不存在，先调用 `add_platform` 保存平台名称、`base_url` 与可选 `api_key`（或多 Key 数组 `api_keys`）；仅在该调用成功后的下一轮，才调用 `add_model`。`add_model` 不创建平台，也不接受或保存 `base_url`、`api_key`。不得把两步合并为一次调用。

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
4. `list_platforms`: 列出当前已配置的 LLM 渠道/提供商（名称、ID 与 Base URL）。新增模型前应先用它确认目标平台是否存在。
5. `add_platform`: 创建一个新的 LLM 提供商连接。
   - `name` (String, 必填): 平台显示名称；后续 `add_model.platform_name` 必须使用这个名称，或使用返回的平台 ID。
   - `base_url` (String, 必填): API 基础地址。
   - `api_key` (String, 可选): 该平台连接密钥，支持填入多个 Key（换行/逗号分隔）；调用模型时若遇限流(429)或欠费(402)将自动无缝切换下一个 Key，模型保持不变。
   - `api_keys` (Array<String>, 可选): 平台多 Key 列表，与 `api_key` 等价。
   - 成功后再执行 `add_model`；若失败则根据错误修复，不能继续添加模型。
6. `update_platform`: 更新已有 LLM 提供商配置（如修改 Base URL 或增补/更换 API Key 列表）。
   - `platform_id` 或 `platform_name` (String, 必填): 目标平台 ID 或名称。
   - `name` (String, 可选): 新平台显示名称。
   - `base_url` (String, 可选): 新 API 基础地址。
   - `api_key` 或 `api_keys` (String 或 Array, 可选): 新的单/多 API Key。
7. `add_model`: **仅向已经存在的平台**绑定添加模型。
   - `platform_id` 或 `platform_name` (String, 必填): 已有目标平台的 ID 或名称。若不确定先调用 `list_platforms`；若不存在先调用 `add_platform`。
   - `model_name` (String, 必填): 平台实际调用的 API 模型标识。
   - `display_name` (String, 可选): 前端显示名称。
   - `spec_name` (String, 可选): 引用的规格名称（将自动继承上下文大小与输出上限）。
   - `context_window`、`max_tokens`、`temperature` (Number, 可选): 仅覆盖模型参数；不得传 `base_url` 或 `api_key`。
8. `delete_model`: 从渠道中移除模型。
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
