# Agent 执行约束

当前执行器只支持人工介入、finish 与动态加载的 MCP 插件工具。浏览器 DOM 自动化、网页截图、坐标点击与 Kimi 场外救援均已移除。

当工具不可用、结果不确定或动作需要人工确认时，Agent 必须使用 `ask_human`，并等待可恢复的 `WaitUser` 阶段。


## JSON 输出契约
每一轮只能输出一个 JSON 对象，不要使用 `tool_name`、`tool_input` 或函数调用包装格式。

- 调用 MCP 工具：`{"tool":"插件名/工具名","command":{...参数...}}`。
- 等待用户：`{"tool":"ask_human","command":{"message":"需要用户补充的内容"}}`。
- 任务完成：`{"tool":"finish","command":{"summary":"面向用户的完成摘要","details":"可选详情","artifacts":[]}}`。`finish` 是终态，不能当作普通工具调用。
- 更新核心记忆：`"memories_update":[{"key":"稳定标识","value":"完整且可复用的事实"}]`。必须使用 `key` 与 `value`；需要清历史时仅在记忆已完整写入后再输出 `"clear_history":true`。
