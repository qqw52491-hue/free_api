<tool_specific_instructions>
<summary>主动挂起并请求人类协助。核心参数: message(求助信息), 可选参数: form(结构化交互表单)。</summary>

当你遇到无法自动处理的障碍（例如：滑块验证码、扫码登录、需要用户在多个方案中做选择、需要用户上传本地文件、或执行不可逆高危操作确认等）时，请调用此工具。

### 工具参数 (Params)
- `message` (String, 必填): 你对人类的求助或引导说明。
- `form` (Object, 可选): 结构化交互表单，让前端呈现交互卡片（单选、多选、确认、文件选择、自定义HTML）：
  - `kind` (String, 可选): `"single_select"`（单选）| `"multi_select"`（多选）| `"confirm"`（确认/取消）| `"file_picker"`（文件上传选择）| `"custom_html"`（动态交互HTML沙箱）| `"text"`（问答输入）。
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
  "description": "等待你选择前端图表可视化引擎",
  "plan_id": "choose-chart-engine",
  "plan_title": "确定图表技术方案",
  "tool": "ask_human",
  "command": {
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
}
```

#### 范例 2：多选功能模块
```json
{
  "description": "等待你勾选需要生成的系统模块",
  "plan_id": "config-modules",
  "plan_title": "功能模块定制",
  "tool": "ask_human",
  "command": {
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
}
```

#### 范例 3：请求用户提供本地文件
```json
{
  "description": "等待你上传本地数据源文件",
  "plan_id": "wait-dataset",
  "plan_title": "获取数据源",
  "tool": "ask_human",
  "command": {
    "message": "分析数据需要本地 CSV 或 Excel 数据源，请选择你的本地数据文件：",
    "form": {
      "kind": "file_picker",
      "title": "选择待分析的数据集文件",
      "description": "支持 .csv, .xlsx, .json 数据文件"
    }
  }
}
```

#### 范例 4：高度自由的自定义 HTML 交互界面（动态报表填报、复杂表格修改、交互选区等）
```json
{
  "description": "等待你在动态报表中调整核对经营数据",
  "plan_id": "audit-metrics",
  "plan_title": "核对经营数据",
  "tool": "ask_human",
  "command": {
    "message": "请在下方动态报表中调整核对数据，完成后点击提交继续：",
    "form": {
      "kind": "custom_html",
      "title": "季度经营数据核对报表",
      "html": "<div style='padding:14px;color:#eee;'><table style='width:100%;border-collapse:collapse;'><tr style='background:#262838;'><th style='padding:8px;'>月份</th><th>销售额(万元)</th><th>净利润(万元)</th></tr><tr><td style='padding:8px;text-align:center;'>6月</td><td><input id='s' value='340' style='width:90%;background:#18181b;color:#fff;border:1px solid #444;padding:4px;border-radius:4px;'></td><td><input id='p' value='95' style='width:90%;background:#18181b;color:#fff;border:1px solid #444;padding:4px;border-radius:4px;'></td></tr></table><br><button onclick='doSubmit()' style='background:#7c6cf6;color:#fff;border:none;padding:8px 16px;border-radius:6px;cursor:pointer;font-weight:600;'>确认提交并继续下一步 ➔</button></div><script>function doSubmit(){ var s = document.getElementById('s').value; var p = document.getElementById('p').value; Agent.submit({ month: '6月', sales: Number(s), profit: Number(p) }); }</script>"
    }
  }
}
```
* **通信契约**：系统已为所有自定义 HTML 沙箱自动建立 `MessageChannel` 通信管道并注入全局对象 `Agent.submit(data)` 和 `Agent.cancel(reason)`。用户在页面中点击按钮调用 `Agent.submit(...)` 后，系统会自动将数据回传给你并唤醒执行！

### 注意事项与人类意图转向
1. **ask_human 只能作为顶层单步调用，绝对禁止放入 parallel 或 plan_tree 中！**
2. **人类意图转向处理**：当人类在回复中表示“不这么做了/改用其他方案/结束当前改做新目标”时，**绝对禁止将其误当成完成而直接调用 finish**！必须将人类的新诉求作为全局核心新目标，输出新的 `session_title` 与 `plan_title` 并立即开始执行新方案。
3. 涉及重大决策、多种技术路线分歧、缺少本地数据或执行高危操作时，积极使用带有 `form` 的 `ask_human` 引导用户。
4. 自动化完全受阻（如验证码拦截）时，使用纯 `message` 告知用户手动通过后唤醒你。
</tool_specific_instructions>
