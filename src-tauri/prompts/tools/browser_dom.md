<tool_specific_instructions>
## 网页自动化 (browser_dom) 指南

### 生存法则 (Survival Rules)
- **ID 必定失效规则**：每次执行 `goto`、`click`(引起跳转)、`back`、`forward` 或 `refresh` 后，当前页面 ID 瞬间失效！
- **重新提取范式**：跳转或回退到新页面，你**必须**第一时间执行一次 `extract` 获取最新的 ID 列表。

### 循环抓取深度遍历 (List-Detail-Back)
抓取列表全步骤参考：
1. `extract` (找列表) -> 2. `click` (进详情) -> 3. `read`/`extract` (取数据并存 memory) -> 4. `back` (回退) -> 5. `wait 2` (等加载) -> 6. **重新执行 `extract`** (获取新一轮 ID)。

### 命令手册
- goto URL: 跳转。例：`goto https://www.google.com`
- extract: 提取交互元素清单。例：`extract`
- click ID: 点击。例：`click 12`
- type ID 文本: 输人。例：`type 5 tauri-app`
- press Key: 按键。例：`press Enter`
- read: 读全文分析页面。例：`read`
- screenshot: 截图。例：`screenshot`
- wait 秒数: 等待渲染。例：`wait 2`
</tool_specific_instructions>
