<tool_specific_instructions>
<summary>工作区 Skill 技能与自定义提示词管理工具。支持添加与删除自定义指令，并自动调用 LLM 提炼高密度触发卡入库 SQLite。</summary>

当你需要查看、新增或删除自定义 Skill / Prompt 指令时调用此工具。
如果用户要求创建 Skill 但未提供详细执行内容或触发场景，可先调用 `ask_human` 附带动态表单（或 `custom_html`）收集必要信息。

### 支持的动作 (action)
1. `list`: 查看当前所有已配置的 Skill 与 Prompt 列表（包含触发卡与启用状态）。
2. `add`: 添加单个 Skill 或自定义提示词（自动调用选定 LLM 提炼高密度技能触发卡）。
   - `name` (String, 必填): 技能或提示词名称，如 `"前端代码审查"` 或 `"回答风格规范"`。
   - `content` (String, 必填): 完整的执行指令、工作流步骤或规范内容。
   - `kind` (String, 可选): `"skill"`（默认）或 `"prompt"`。
   - `description` (String, 可选): 自定义触发卡说明；若留空系统将自动调用大模型提炼。
   - `model_id` (String, 可选): 提炼触发卡所用大模型 ID。
3. `install`: 自助安装技能仓库（Git 地址或本地目录）到应用技能目录，探测 SKILL.md 并自动登记为可路由 Skill（kind=skill 懒加载，不入常驻头部）。
   - `url` (String, 必填): Git 仓库地址（https/git@，depth-1 浅克隆）或本地目录路径（递归复制）。
   - `name` (String, 可选): 单技能仓库时覆盖卡片名；多 SKILL.md 仓库忽略。
   - 返回 JSON：`registered`（登记的技能名+手册路径）、`errors`、`next`（本轮可 next_tool_hint 自取）。
   - 仓库无 SKILL.md 时返回顶层结构清单，不自动登记——由你判断后用 `add` 提炼。
4. `delete`: 从本地数据库中移除 Skill。
   - `id` 或 `name` (String, 必填): 技能 ID 或名称。
   - `confirm` (Boolean, 必填): 必须显式传入 `true`。

注意：本工具当前不支持批量导入（batch_import）与启用切换（toggle）。如需批量添加请逐条调用 `add`；如需更换现有内容，删除后重新 `add`（install 同名会自动更新）。
`install` 的典型用法：调研阶段发现可复用技能仓库（含 SKILL.md 的 Claude/OpenAI skill 包）时，直接 `install` 安装；装完本轮即可 `next_tool_hint` 取回手册正文，下轮起进入 triage 路由索引。

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
