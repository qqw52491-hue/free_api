<tool_specific_instructions>
<summary>网页正文提取与纯净 Markdown 提纯。核心参数: url(目标网页链接)。</summary>

当你需要深入阅读某一个具体的网页文章、API 文档、GitHub 仓库或新闻正文时，调用此工具。

### 工具参数 (Params)
- `url` (String, 必填): 目标网页的完整 HTTP/HTTPS 链接。

### 执行逻辑
系统将自动抓取该网页，去除广告、弹窗、导航栏与样式噪声，并将主体内容提纯为高质量的 Markdown 文本返回给你。

### 使用范例
```json
{
  "tool": "web_fetch",
  "command": {
    "url": "https://doc.rust-lang.org/book/ch01-01-installation.html"
  }
}
```
</tool_specific_instructions>
