<tool_specific_instructions>
<summary>工作区 Skill 技能与自定义提示词管理工具。支持添加、批量导入、切换与删除自定义指令，并自动调用 LLM 提炼高密度触发卡入库 SQLite。</summary>

当你需要查看、新增、批量导入、切换或删除自定义 Skill / Prompt 指令时调用此工具。
如果用户要求创建 Skill 但未提供详细执行内容或触发场景，可先调用 `ask_human` 附带动态表单（或 `custom_html`）收集必要信息。

### 支持的动作 (action)
1. `list`: 查看当前所有已配置的 Skill 与 Prompt 列表（包含触发卡与启用状态）。
2. `add`: 添加单个 Skill 或自定义提示词（自动调用选定 LLM 提炼高密度技能触发卡）。
   - `name` (String, 必填): 技能或提示词名称，如 `"前端代码审查"` 或 `"回答风格规范"`。
   - `content` (String, 必填): 完整的执行指令、工作流步骤或规范内容。
   - `kind` (String, 可选): `"skill"`（默认）或 `"prompt"`。
   - `description` (String, 可选): 自定义触发卡说明；若留空系统将自动调用大模型提炼。
   - `model_id` (String, 可选): 提炼触发卡所用大模型 ID。
3. `batch_import`: 批量导入多个 Skill。
   - `skills` (Array of Objects, 可选): `[{"name": "...", "content": "...", "description": "..."}]`。
   - `input_text` (String, 可选): Markdown 格式文本（使用 `### 技能名称` 分隔多段）。
   - `model_id` (String, 可选): 提炼触发卡所用大模型 ID。
4. `toggle`: 启用或停用某条 Skill。
   - `id` 或 `name` (String, 必填): 技能 ID 或名称。
   - `enabled` (Boolean, 必填): `true` 启用，`false` 停用。
5. `delete`: 从本地数据库中移除 Skill。
   - `id` 或 `name` (String, 必填): 技能 ID 或名称。
   - `confirm` (Boolean, 必填): 必须显式传入 `true`。

### 使用范例
```json
{
  "tool": "skill_manager",
  "command": {
    "action": "add",
    "name": "SQL规范检查",
    "content": "检查所有 SQL 查询必须包含 LIMIT，禁止 SELECT *，所有关联查询必须有索引字段。",
    "kind": "skill"
  }
}
```
</tool_specific_instructions>
