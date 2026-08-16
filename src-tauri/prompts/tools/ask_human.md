<tool_specific_instructions>
<summary>主动挂起并请求人类协助。核心参数: message(求助信息), 可选参数: form(结构化交互表单)。</summary>

当你遇到无法自动处理的障碍（例如：滑块验证码、扫码登录、需要用户在多个方案中做选择、需要用户上传本地文件、或执行不可逆高危操作确认等）时，请调用此工具。

### 工具参数 (Params)
- `message` (String, 必填): 你对人类的求助或引导说明。
- `form` (Object, 可选): 结构化交互表单，让前端呈现交互卡片（单选、多选、确认、文件选择）：
  - `kind` (String, 可选): `"single_select"`（单选）| `"multi_select"`（多选）| `"confirm"`（确认/取消）| `"file_picker"`（文件上传选择）| `"text"`（问答输入）。
  - `title` (String, 可选): 表单主标题。
  - `description` (String, 可选): 详细指引说明。
  - `options` (Array of Objects, 当 kind 为 single_select 或 multi_select 时必填): 选项列表：
    - `id` (String): 选项唯一ID
    - `label` (String): 选项显示名称
    - `description` (String, 可选): 选项说明/推荐理由
  - `default` (String/Array/Boolean, 可选): 默认预选值。

### 执行逻辑
调用此工具后，你的所有动作将**彻底停摆并进入休眠状态**。前端会弹出交互卡片并等待人类操作。
人类完成选择或输入后，你将被唤醒，人类的回复文本（包含其勾选项、文件路径及补充说明）将作为此工具的 `stdout` 返回给你。

### 使用范例

#### 范例 1：单选让用户做架构决策
```json
{
  "tool": "ask_human",
  "commands": [
    {
      "action": "ask_human",
      "message": "检测到当前项目有多种图表库可选，请选择你的偏好：",
      "form": {
        "kind": "single_select",
        "title": "选择前端图表可视化引擎",
        "description": "我们将根据你的选择初始化图表依赖并生成代码",
        "options": [
          { "id": "echarts", "label": "ECharts (推荐)", "description": "支持折线、柱状、雷达与知识图谱关系拓扑" },
          { "id": "chartjs", "label": "Chart.js", "description": "轻量级 Canvas 图表库" },
          { "id": "d3", "label": "D3.js", "description": "高度定制化数据驱动可视化" }
        ],
        "default": "echarts"
      }
    }
  ]
}
```

#### 范例 2：多选功能模块
```json
{
  "tool": "ask_human",
  "commands": [
    {
      "action": "ask_human",
      "message": "请勾选需要在本轮生成的系统模块：",
      "form": {
        "kind": "multi_select",
        "title": "功能模块定制",
        "options": [
          { "id": "auth", "label": "用户认证与 JWT", "description": "登录注册与令牌守卫" },
          { "id": "db", "label": "SQLite/PostgreSQL ORM", "description": "持久化存储层" },
          { "id": "metrics", "label": "可视化监控仪表盘", "description": "ECharts 实时监控图表" }
        ],
        "default": ["auth", "db"]
      }
    }
  ]
}
```

#### 范例 3：请求用户提供本地文件
```json
{
  "tool": "ask_human",
  "commands": [
    {
      "action": "ask_human",
      "message": "分析数据需要本地 CSV 或 Excel 数据源，请选择你的本地数据文件：",
      "form": {
        "kind": "file_picker",
        "title": "选择待分析的数据集文件",
        "description": "支持 .csv, .xlsx, .json 数据文件"
      }
    }
  ]
}
```

### 注意事项
1. 涉及重大决策、多种技术路线分歧、或缺少本地数据时，积极使用带有 `form` 的 `ask_human` 引导用户做明确选择。
2. 自动化完全受阻（如验证码拦截）时，使用纯 `message` 告知用户手动通过后唤醒你。
</tool_specific_instructions>
