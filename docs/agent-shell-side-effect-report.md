# Shell 副作用登记

宿主能直接观察到的写入会自动记账。Shell 执行完毕后，如果确实发生了宿主尚未登记的变更，Agent 可以在下一次正常指令的顶层补充 `side_effect_report`。这个字段可选，只读查询和影响尚未确认的操作可以省略。

```json
{
  "side_effect_report": {
    "output_id": "<原 Shell 执行回执 ID>",
    "description": "核对结果显示服务配置已更新",
    "verification_output_id": "<可选的核对回执 ID>"
  }
}
```

- `output_id` 指向当前任务中原始的、可补报的 `local_shell` 回执。
- `description` 写回执能够支持的实际影响。
- 原回执证据不足时，当前 Agent 可以自己读回文件、查服务状态，或先用 `get_step_output` 看原始输出。做了独立核对，就把其回执 ID 放入 `verification_output_id`。这条路径可以全由当前 Agent 完成。已有充分证据时，可省略这个字段。

后端核对回执归属与原回执资格。同一份原回执只入账一次，拒绝补报时会反馈具体原因，当前指令仍按原流程处理。登记项保存到检查点，阶段审查和终审可按回执引用复核。后端验证的是来源与归属，实际影响由 Agent 根据回执判断，登记项标明是模型补报。
