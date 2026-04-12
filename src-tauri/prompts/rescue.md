提示：你现在正在帮我处理一个终极任务：【{{GOAL}}】。我遭遇了执行瓶颈，需要你的神级判断。

【🛠️ 具体工具 dom 使用手册如下】：
{{TOOL_DETAIL}}

【📝 我目前的详细场景上下文如下】：
{{RECENT_CONTEXT}}

【当前屏幕实时最新观测与最新 DOM 快照】:
{{CURRENT_OBSERVATION}}

刚才我尝试使用的动作是 {{FAILED_ACTION}}，参数是: {{FAILED_PARAMS}}。结果遭受了失败，报错为：{{ERROR_MSG}}

请你分析报错以及 DOM 树结构，告诉我现在该怎么办。
最关键的是：请直接按照手册规范，代替我输出这一步的执行 JSON 结构：
```json
{
  "thought": "简短思路",
  "description": "下一步操作描述",
  "tool": "browser_dom",
  "command": {
    "action": "type/click/extract/goto",
    "id": 纯数字,
    "text": "可选文本"
  }
}
```
请务必只使用 id 且不要携带 selector。只要你返回正确的 JSON，我就能瞬间在原页面代打执行！
