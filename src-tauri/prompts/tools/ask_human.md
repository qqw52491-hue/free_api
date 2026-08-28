<tool_specific_instructions>
<summary>主动挂起并请求人类协助。核心参数: message(求助信息), 可选参数: form(结构化交互表单)。</summary>

当你遇到无法自动处理的障碍（例如：滑块验证码、需要用户在多个架构方案中做抉择、勾选功能模块、输入 API 密钥/配置、上传本地数据文件、或执行不可逆高危操作二次确认等）时，请调用此工具。

### 工具参数 (Params)
- `message` (String, 必填): 你对人类的求助或引导说明。
- `form` (Object, 可选): 结构化交互表单，让前端呈现交互卡片（单选、多选、确认、问答输入、文件选择、自定义HTML）：
  - `kind` (String, 必填): `"single_select"`（单选）| `"multi_select"`（多选）| `"confirm"`（确认/取消）| `"text"`（问答输入）| `"file_picker"`（文件上传选择）| `"custom_html"`（动态交互HTML微应用）。
  - `title` (String, 必填): 表单卡片主标题。
  - `description` (String, 可选): 详细指引、技术背景或安全风险说明。
  - `options` (Array of Objects, 当 kind 为 single_select 或 multi_select 时必填): 选项列表：
    - `id` (String): 选项唯一ID
    - `label` (String): 选项显示名称
    - `description` (String, 可选): 选项说明/推荐理由/优缺点对比
  - `placeholder` (String, 当 kind 为 text 时可选): 输入框占位引导提示词。
  - `default` (String/Array/Boolean, 可选): 默认预选值。
  - `html` (String, 当 kind 为 custom_html 时必填): 包含独立 HTML/CSS/JS 的交互片段，通过 `Agent.submit(data)` 回传数据。

### 执行逻辑
调用此工具后，你的所有动作将**彻底停摆并进入休眠状态**。前端会弹出对应形态的交互卡片并等待人类操作。
人类完成选择或输入后，你将被唤醒，人类的回复文本（包含其勾选项、输入内容、文件路径及补充说明）将作为此工具的 `stdout` 返回给你。

---

### 完整场景与复杂范例库

#### 范例 1：复杂单选（技术选型 / 架构多方案决策）
```json
{
  "description": "请求用户在三种主流 ORM 框架中做出选型决策",
  "plan_id": "choose-orm-framework",
  "plan_title": "确定持久化层技术方案",
  "tool": "ask_human",
  "command": {
    "message": "检测到当前项目为 TypeScript 栈，请根据团队偏好选择适合的持久化 ORM 方案：",
    "form": {
      "kind": "single_select",
      "title": "选择数据库 ORM 框架",
      "description": "我们将根据你的选择初始化依赖、生成 Schema 定义与数据库迁移配置",
      "options": [
        { "id": "prisma", "label": "Prisma ORM (推荐)", "description": "强类型自动生成 Client，开发体验极佳，自带可视化 Studio" },
        { "id": "drizzle", "label": "Drizzle ORM", "description": "零运行时开销，SQL 级精确控制，极致轻量与启动速度" },
        { "id": "typeorm", "label": "TypeORM", "description": "经典装饰器与 Active Record / Data Mapper 模式，适合大型面向对象架构" }
      ],
      "default": "prisma"
    }
  }
}
```

#### 范例 2：复杂多选（模块定制 / 安全策略多项勾选）
```json
{
  "description": "等待用户勾选要注入网关的安全中间件模块",
  "plan_id": "configure-security-middlewares",
  "plan_title": "安全防护策略定制",
  "tool": "ask_human",
  "command": {
    "message": "请勾选需要在生产网关中启用的安全防护策略（支持多选）：",
    "form": {
      "kind": "multi_select",
      "title": "选择安全防护中间件",
      "description": "选中的中间件将自动注入到全局中间件管道中",
      "options": [
        { "id": "rate_limit", "label": "API 速率限制 (Rate Limiting)", "description": "基于 IP + 路由令牌桶算法，防止恶意爆破" },
        { "id": "cors", "label": "严格跨域守卫 (CORS Whitelist)", "description": "仅允许指定的受信前端域名跨域访问" },
        { "id": "jwt_guard", "label": "JWT 认证与双 Token 自动刷新", "description": "无状态会话安全校验与过期拦截" },
        { "id": "audit_log", "label": "敏感操作审计日志 (Audit Ledger)", "description": "对写操作与特权调用进行只追加持久化留痕" }
      ],
      "default": ["rate_limit", "jwt_guard"]
    }
  }
}
```

#### 范例 3：高危破坏性操作二次确认（不可逆动作防护）
```json
{
  "description": "向用户确认是否执行生产数据库表结构删除",
  "plan_id": "confirm-destructive-migration",
  "plan_title": "高危操作确认",
  "tool": "ask_human",
  "command": {
    "message": "即将向生产数据库执行不可逆的破坏性变更，请仔细核对后确认：",
    "form": {
      "kind": "confirm",
      "title": "⚠️ 生产数据库 DROP TABLE 确认",
      "description": "即将执行: DROP TABLE `legacy_orders_2024`。\n影响范围: 将物理清除 12,450 条历史归档记录，该操作无法撤销！",
      "default": false
    }
  }
}
```

#### 范例 4：文本问答与参数收集（收集配置、域名或密钥）
```json
{
  "description": "请求用户提供第三方 Webhook 回调地址",
  "plan_id": "collect-webhook-url",
  "plan_title": "配置回调地址",
  "tool": "ask_human",
  "command": {
    "message": "系统需要公网接收回调，请提供你的 Webhook 接收端点：",
    "form": {
      "kind": "text",
      "title": "配置支付通知 Webhook 地址",
      "description": "请输入可公网访问的 HTTPS 地址，系统将自动发起握手验证与签名秘钥绑定",
      "placeholder": "例如: https://api.my-domain.com/v1/webhooks/payment"
    }
  }
}
```

#### 范例 5：本地文件 / 数据源选择器（上传数据集与分析源文件）
```json
{
  "description": "等待用户选择本地财务流水原始数据文件",
  "plan_id": "pick-financial-dataset",
  "plan_title": "获取数据源",
  "tool": "ask_human",
  "command": {
    "message": "财务报表聚合需要原始数据，请选择你的本地数据文件：",
    "form": {
      "kind": "file_picker",
      "title": "选择财务流水数据集",
      "description": "支持 .csv, .xlsx, .json 或 .parquet 格式数据文件"
    }
  }
}
```

#### 范例 6：高度灵活的自定义 HTML 交互微应用（动态数据核对与表单填报）
```json
{
  "description": "等待用户在动态表格中调整并核对经营数据",
  "plan_id": "audit-financial-metrics",
  "plan_title": "核对经营数据",
  "tool": "ask_human",
  "command": {
    "message": "请在下方动态报表中调整核对数据，完成后点击提交继续：",
    "form": {
      "kind": "custom_html",
      "title": "Q3 经营指标核对与调参",
      "html": "<div style='padding:14px;color:#eee;font-family:sans-serif;'><table style='width:100%;border-collapse:collapse;'><tr style='background:#262838;'><th style='padding:8px;'>月份</th><th>销售额(万元)</th><th>毛利率(%)</th></tr><tr><td style='padding:8px;text-align:center;'>7月</td><td><input id='s7' value='380' style='width:80%;background:#18181b;color:#fff;border:1px solid #444;padding:4px;border-radius:4px;'></td><td><input id='m7' value='28.5' style='width:80%;background:#18181b;color:#fff;border:1px solid #444;padding:4px;border-radius:4px;'></td></tr></table><br><button onclick='doSubmit()' style='background:#7c6cf6;color:#fff;border:none;padding:8px 18px;border-radius:6px;cursor:pointer;font-weight:600;'>确认提交并继续下一步 ➔</button></div><script>function doSubmit(){ var s = document.getElementById('s7').value; var m = document.getElementById('m7').value; Agent.submit({ month: '7月', sales: Number(s), margin: Number(m) }); }</script>"
    }
  }
}
```

* **通信契约说明**：系统已为所有 `custom_html` 自动建立 `MessageChannel` 通信管道并注入全局对象 `Agent.submit(data)` 和 `Agent.cancel(reason)`。用户在页面中点击按钮调用 `Agent.submit(...)` 后，系统会自动将数据回传给你并唤醒执行。

---

### 注意事项与人类意图转向
1. **ask_human 只能作为顶层单步调用，绝对禁止放入 parallel 或 plan_tree 中！**
2. **人类意图转向处理**：当人类在回复中表示“不这么做了/改用其他方案/结束当前改做新目标”时，**绝对禁止将其误当成完成而直接调用 finish**！必须将人类的新诉求作为全局核心新目标，输出新的 `session_title` 与 `plan_title` 并立即开始执行新方案。
3. 涉及重大技术决策、安全敏感配置、缺少本地数据或执行高危操作时，积极使用带有 `form` 的 `ask_human` 引导用户。
4. 自动化完全受阻（如验证码拦截）时，使用纯 `message` 告知用户手动通过后唤醒你。
</tool_specific_instructions>
