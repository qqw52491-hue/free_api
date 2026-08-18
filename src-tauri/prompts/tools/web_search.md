<tool_specific_instructions>
<summary>全网实时搜索引擎。核心参数: query(搜索关键词), 可选参数: max_results(最多返回条数, 默认5)。</summary>

当你需要查询最新的事实信息、技术文档、新闻资讯、开源库用法或公开知识时，调用此工具。

### 工具参数 (Params)
- `query` (String, 必填): 你的搜索关键词或精炼查询短语（支持中英文）。
- `max_results` (Number, 可选): 最多返回的搜索结果条数，默认 5，最大 10。

### 执行逻辑
系统将通过内置的原生异步检索聚合引擎并发检索全网，返回结构化的搜索结果列表：
包含：序号、网页标题、网页链接 (URL)、摘要内容及来源。
获取结果后，你可继续针对其中关键的 URL 调用 `web_fetch` 抓取完整 Markdown 正文，或在最终答复中以 `[标题](URL)` 引用来源。

### 使用范例
```json
{
  "tool": "web_search",
  "command": {
    "query": "Rust 2026 最新特性 async trait",
    "max_results": 5
  }
}
```
</tool_specific_instructions>
