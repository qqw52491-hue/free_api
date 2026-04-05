<tool_specific_instructions>
<summary>网页全自动操控。核心动词: goto(跳转), extract(提取ID), click(点击), type(输入), scroll(滚动), read(语义阅读), back(后退)。</summary>

# 网页自动化 (browser_dom) 战术指南

## 狙击手战术 (Tactical Heuristics)
- **零容忍地毯式搜索**：如果 extract 结果中有 `<input>` 或带 search 关键字的 ID，第一步永远是输入关键词搜索。绝对禁止逐个点击分类链接去“碰运气”。
- **死胡同撤退策略 (Dead-End Retreat)**：如果你点击了一个分类或链接，发现里面没有你要的数据，必须立刻调用 `back` 返回上一页，并在 TODO 中将该尝试标记为 canceled，然后换搜索词或其他策略。
- **抗门口效应 (Sign-In Wall)**：如果 read 结果包含过多“Login”, “Sign In”，且当前滚动高度为 0，说明数据被折叠或在视口下方。你必须先 `scroll down` 两次来探测真实正文。

## DOM 刷新铁律 (Critical State Rules)
- **ID 必定失效规则**：每次发生 `goto`、`click`(导致跳转)、`back` 或 `refresh` 后，当前页面的所有元素 ID **瞬间作废**！你必须在下一步立即执行 `extract` 生成新 ID，严禁凭记忆点击旧 ID。
- **列表循环范式 (List-Detail-Back)**：`extract` -> 2. `click` (进详情) -> 3. `read`/`extract` (抓数据并存 memory) -> 4. `back` (退回列表) -> 5. `wait_idle` (等 DOM 稳定) -> 6. **必杀技**：重新 `extract` (获取新列表ID)。

## 滚动战术 (Tactical Scrolling)
- **懒加载探测**：如果 extract 列表过短或页面底部有“加载更多”，请执行 `scroll down` 并结合 `wait_idle`，随后立即 `extract` 以获取新加载的元素。
- **防遮挡策略**：若点击元素后无反应，可能是被悬浮导航栏遮挡。请尝试 `scroll down` 少量距离后再点击。

## 命令手册 (Commands Reference - JSON Object 格式)
**重要**：所有指令必须作为 JSON 对象传入 `command` 字段，包含 `action` 和必要的参数。
- `{"action": "goto", "url": "https://www..."}`: 跳转网页
- `{"action": "extract"}`: 提取当前视口内交互元素
- `{"action": "click", "id": 12}`: 点击指定 ID 的元素
- `{"action": "type", "id": 12, "text": "今天天气"}`: 在指定输入框输入文本。每次会清空旧内容。输入后常需结合 `press` 回车。
- `{"action": "press", "text": "Enter"}`: 模拟按键（参数可用 text 传值）
- `{"action": "read"}`: 读取当前正文
- `{"action": "scroll", "text": "down"}`: 滚动页面 (down/up/top/bottom)
- `{"action": "wait", "id": 2}`: 强行死等2秒（秒数传给 id 即可）
- `{"action": "wait_idle"}`: **智能等待页面 DOM 稳定。** 强烈推荐在 click、goto 等之后使用。
- `{"action": "back"}` / `{"action": "forward"}` / `{"action": "refresh"}`: 基础导航 (执行后必重新 extract)
- `{"action": "screenshot"}`: 获取视图快照
- `{"action": "ask_web_ai", "url": "https://kimi.moonshot.cn", "text": "你的具体问题/上下文"}`: **【杀手锏】如果你遇到极难处理的混淆代码、正则提取或验证逻辑，立刻调用此宏 或者遇到无法解决的问题！**系统会自动在后台新建一个 Tab 访问该 AI 网页，自动寻找输入框提问，并死等生成结束后将结果提取返回给你，完全不干扰你当前的网页上下文。
- `{"action": "new_tab", "url": "https..."}`: 新建标签页 
- `{"action": "switch_tab", "id": 2}`: 切换焦点至指定标签页 ID。所有提取、点击等操作都会应用在当前焦点标签页上。
- `{"action": "list_tabs"}`: 列出当前存在的所有标签页（ID、标题、URL及当前焦点所在）。
- `{"action": "close_tab", "id": 2}`: 关闭/移除指定 ID 的标签页。如果不传 id，则关闭当前标签页。

## 经典实操范式 (Few-Shot Strategy)
**场景**： 抓取首页前 2 篇文章内容。
第1步：`{"action": "extract"}` (找出前2条链接 ID)。
第2步：`{"action": "click", "id": 15}` (点进第1条)。
第3步：`{"action": "read"}` -> `memories_update` (存数据) -> **重点动作**：执行 `{"action": "back"}`。
第4步：**必杀技**：执行 `{"action": "extract"}` (因为回退后旧 ID 全失效了，必须重新 extract)。
第5步：`{"action": "click", "id": 18}` (点进第2条)。
</tool_specific_instructions>