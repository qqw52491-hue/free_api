<tool_specific_instructions>
<summary>网页全自动操控。核心动词: goto(跳转), extract(提取ID), click(点击), type(输入), scroll(滚动), read(语义阅读), back(后退), click_xy(坐标点击)。</summary>

# 网页自动化 (browser_dom) 战术指南

## 页面感知铁律 (Observation Rules) 🧭
> **这是最重要的规则！每次操作后你必须先搞清楚"我在哪"，再决定"我要做什么"。**

1. **全局扫描优先**：执行完 `goto` 或 `extract` 后，不要只盯着你想找的元素！先看返回结果中的页面标题、URL 和 `<h1>`，确认你到底到了哪个页面。
2. **空结果 ≠ 再试一次**：如果你 `extract` 搜索框却什么都没拿到，**绝对禁止**再次盲目 `extract`！
   - 立刻在 `reflection` 中判断：此页面可能无搜索功能、搜索框由 JS 动态渲染导致 DOM 抓不到。
   - **正确应对**：直接构造搜索 URL 跳转（见下方 URL 直达术）。
3. **识别死页面**：如果 `read` 或 `extract` 返回的内容大量包含 "404"、"Not Found"、"Access Denied"、"Sign In"、"Login"，立即在 `reflection` 中承认"当前页面不可用"，触发撤退或换站。
4. **防遮挡感知**：如果 `click` 后返回 "OK" 但页面没有变化，可能是元素被悬浮导航栏遮挡。请 `scroll down` 少量距离后再试。
5. **收到截图时的强制行为** 🎯：系统向你发送 `【📸 截图 + DOM坐标双锚点分析】` 消息时，你**必须**按以下优先级操作：
   - **第一优先级（精确）**：查看消息中附带的「精确坐标表」，找到与目标最匹配的元素，直接使用其 `cx/cy` 值作为 click_xy 参数，**禁止自行估算！**
   - **第二优先级（兜底）**：只有坐标表中没有目标元素时（如元素在视口外或被遮挡），才允许凭截图目视估算坐标
   - **下一步立即输出 `click_xy` 指令**，禁止输出 extract/click/read 等 DOM 指令

## URL 直达术 (URL Direct Navigation) 🎯
> **降维打击：能拼 URL 就绝不去找搜索框！**

很多现代网站的搜索框是 JS 动态渲染的，headless 浏览器根本点不到。直接构造搜索 URL 才是最稳妥的方式：

| 网站 | 搜索 URL 模式 |
|------|--------------|
| Google | `https://www.google.com/search?q=关键词` |
| Bing | `https://www.bing.com/search?q=关键词` |
| 百度 | `https://www.baidu.com/s?wd=关键词` |
| BBC | `https://www.bbc.co.uk/search?q=关键词` |
| CNN | `https://edition.cnn.com/search?q=关键词` |
| Reuters | `https://www.reuters.com/site-search/?query=关键词` |
| GitHub | `https://github.com/search?q=关键词` |
| Wikipedia | `https://en.wikipedia.org/wiki/关键词` |

**规则**：如果你需要在一个网站上搜索内容，**第一反应**应该是 `goto` 上述格式的 URL，而不是 `extract` 去找搜索框。

## 狙击手战术 (Tactical Heuristics)
- **零容忍地毯式搜索**：如果 extract 结果中有 `<input>` 或带 search 关键字的 ID，直接用 `type` 输入关键词搜索。绝对禁止逐个点击分类链接去"碰运气"。
- **死胡同撤退策略 (Dead-End Retreat)**：如果你点击了一个分类或链接，发现里面没有你要的数据，必须立刻调用 `back` 返回上一页，并在 TODO 中将该尝试标记为 `canceled`，然后换搜索词或其他策略。
- **抗门口效应 (Sign-In Wall)**：如果 `read` 结果包含过多"Login", "Sign In"，且当前滚动高度为 0，说明数据被折叠或在视口下方。你必须先 `scroll down` 两次来探测真实正文。

## 🔥 多标签页自动管理 (Tab Auto-Tracking)
> **系统会自动检测 click/click_xy 后弹出的新标签页，并帮你切换过去！**

- 当你点击一个 `target="_blank"` 的链接时，系统会自动捕获新打开的标签页（命名为 `popup_1`, `popup_2` 等），并立刻将你的视野切换到新页面。
- 你在 `extract` 输出中会看到 `【📂 标签页管理器】` 区块，显示所有打开的标签页及当前活跃页。
- **核心规则：读完弹出页的数据后，必须立刻执行 `close_tab popup_X` 关闭它！** 否则标签页越积越多会导致混乱。
- 关闭弹出页后，系统会自动切回 `main` 主页面，你可以继续浏览列表。
- 如需手动切换，使用 `switch_tab <id>` 和 `list_tabs` 查看所有标签页。

## DOM 刷新铁律 (Critical State Rules)
- **ID 必定失效规则**：每次发生 `goto`、`click`(导致跳转)、`back` 或 `refresh` 后，当前页面的所有元素 ID **瞬间作废**！你必须在下一步立即执行 `extract` 生成新 ID，严禁凭记忆点击旧 ID。
- **列表循环范式 (新标签页版)**：`extract` -> `click` (进详情，如弹出新标签页系统自动切换) -> `read`/`extract` (抓数据并存 memory) -> `close_tab popup_X` (关闭弹出页，自动回到 main 列表页) -> `extract` (重新获取列表 ID)。

## ⚡ type 原子指令铁律（重要升级）
> **`type` 指令现在是"点击+输入"的原子操作，不需要，也不应该在 type 前单独调 click！**

- **带 id 的 type**：系统会自动先对该元素发射完整点击事件（mousedown+mouseup+click），等框架响应后立即输入文字。**一步顶两步！**
- **不带 id 的 type（盲打）**：直接沿用当前物理焦点打字，用于 click 一个按钮/区域后立即跟打内容。

❌ **绝对禁止的错误写法**（多余的单独 click）：
```json
{"action": "click", "id": 12}   ← 先点
{"action": "type", "id": 12, "text": "内容"}  ← 再输入
```
✅ **正确写法——直接一步 type**：
```json
{"action": "type", "id": 12, "text": "内容"}
```
系统底层自动完成：点击 → 等待 → 输入，全程原子，绝不丢失焦点。

## 滚动战术 (Tactical Scrolling)
- **懒加载探测**：如果 extract 列表过短或页面底部有"加载更多"，请执行 `scroll down` 并结合 `wait_idle`，随后立即 `extract` 以获取新加载的元素。
- **防遮挡策略**：若点击元素后无反应，可能是被悬浮导航栏遮挡。请尝试 `scroll down` 少量距离后再点击。

## 命令手册 (Commands Reference)
**重要**：系统支持"单指令"或"批量指令流水线 (Pipeline)"。

### 1. 批量指令（多步连招，适合需要等待的场景）
使用 `commands` 数组一次性提交多个动作。适合「输入 → 等待 → 按键」等需要间隔的场景。
```json
{
  "reflection": "上一步 extract 确认了搜索框 ID=12，页面正常",
  "thought": "输入关键词并按 Enter 搜索",
  "tool": "browser_dom",
  "commands": [
    {"action": "type", "id": 12, "text": "今天新闻"},
    {"action": "wait", "seconds": 0.3},
    {"action": "press", "key": "Enter"}
  ]
}
```
*注意：如果中间某步失败，流水线会立即熔断停止。*

### 2. 基础动作定义
所有指令必须包含 `action` 字段。

- `{"action": "goto", "url": "https://www..."}`: 跳转网页
- `{"action": "extract"}`: 提取当前视口内交互元素（返回元素列表，含 **XY 坐标**）
- `{"action": "click", "id": 12}`: 点击链接/按钮（只用于导航跳转类按钮，**不要用它来聚焦输入框**）
- `{"action": "click_xy", "x": 320, "y": 150}`: **【视觉坐标点击·终极武器】** 当 DOM 无法识别按钮时，先截图让视觉模型定位，再用此指令按坐标直接点击。坐标为**视口坐标**（与 extract 返回的 X/Y 同一系，左上角为0,0）。
- `{"action": "type", "id": 12, "text": "内容"}`: **【原子操作】** 自动点击元素获得焦点后立即输入。有 id 时无需先 click。
- `{"action": "type", "text": "内容"}`: 盲打模式，沿用当前焦点直接输入（用于 click 按钮后跟打）
- `{"action": "press", "key": "Enter"}`: 模拟按键（支持 Enter/Tab/Escape 等）
- `{"action": "wait", "seconds": 1}`: 强制等待指定秒数（小数也支持，如 0.5）
- `{"action": "wait_idle"}`: 智能等待页面 DOM 稳定
- `{"action": "read"}`: 读取当前正文
- `{"action": "scroll", "direction": "down"}`: 滚动页面，direction 可填 down/up/top/bottom
- `{"action": "back"}` / `{"action": "forward"}` / `{"action": "refresh"}`: 基础导航
- `{"action": "screenshot"}`: 获取当前视口截图（返回 base64 图片，供视觉模型分析）
- `{"action": "ask_web_ai", "url": "kimi", "text": "问题"}`: **【杀手锏】** 遇到极难处理的混淆代码、报错或验证逻辑，立刻调用场外 Kimi 援助！
- `{"action": "new_tab", "url": "https..."}` / `{"action": "switch_tab", "id": 2}` / `{"action": "close_tab", "id": 2}`: 标签页管理

## 经典实操范式 (Few-Shot Strategy)

**场景 1：在新闻网站搜索（URL 直达术 + 兜底）**
```
第1步：直接 goto "https://www.bbc.co.uk/search?q=马斯克" （优先 URL 直达）
第2步：wait_idle（等待搜索结果页加载）
第3步：extract（获取搜索结果列表 ID）
第4步：如果结果为空 → reflection 声明"该站搜索无结果"，todo canceled，换站
```

**场景 2：在百度搜索框输入并搜索**（展示 type 原子能力，URL 直达公式不确定时的兜底）
```
第1步：extract（找搜索框 ID，假设是 12）
第2步：commands: [{"action":"type","id":12,"text":"今天新闻"}, {"action":"press","key":"Enter"}]
```
⚠️ 注意：**没有单独的 click 步骤**！type 已经内置点击。

**场景 3：抓取首页前 2 篇文章内容**
```
第1步：extract（找出前2条链接 ID）
第2步：click id=15（点进第1条，这里 click 用于导航跳转）
第3步：read → memories_update（存数据）→ back（退回）
第4步：extract（⚠️ 必须！back 后所有 ID 失效，必须重新 extract）
第5步：click id=18（点进第2条）
```

**场景 4：DOM 无法识别按钮 → 截图+DOM双锚点定位 → 坐标点击（终极兜底）**
> 适用：按钮被 Canvas 渲染、Shadow DOM 嵌套、或 extract 返回空
```
第1步：screenshot（系统自动同时获取截图 + 当前页面DOM精确坐标表，打包发给视觉模型）

第2步（收到【📸 截图 + DOM坐标双锚点分析】消息后）：
  ✅ 优先查坐标表，找到目标元素的 cx/cy（例如 [5] "发送" => cx:427, cy:312）
  ✅ 直接使用坐标表里的值，禁止从截图重新估算！
  ⚠️ 若坐标表中找不到目标，再凭截图估算（这是最后手段）

第3步：{"action": "click_xy", "x": 427, "y": 312}（坐标来自DOM，精确零误差）
第4步：wait_idle（等待点击后的页面响应）
```

⚠️ **坐标系说明**：
- 坐标为**视口坐标**，左上角 (0,0)，右下角约 (1280,800)
- 坐标表里的 `cx/cy` 与 click_xy 的 `x/y` **完全同一坐标系，直接复用即可**
- 页面滚动后坐标会变！scroll 之后必须重新 screenshot 更新坐标表

**为什么坐标表比截图估算准？**
- 截图传输给大模型时会自动缩放，模型凭图估算有 ±30~80px 误差
- 坐标表的 cx/cy 是 JS 从 DOM 实时计算出的精确像素值，误差为 0
</tool_specific_instructions>