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
5. **视觉与 DOM 协同作战原则** 🎯：优先使用 `extract` (DOM) 以追求极致执行速度和 100% 精准度。但遇到 Canvas 渲染、复杂验证码、或者 DOM 无法准确描述的复杂 UI 时，**请不要犹豫，果断使用 `screenshot` 截图**。截图虽然会增加几秒的推理延迟，但能提供所见即所得的视觉理解，是你破局的重要武器。
   - 收到 `【📸 截图 + DOM坐标双锚点分析】` 时，**第一优先级（精确）**：查看消息中附带的「精确坐标表」，直接使用其 `cx/cy` 值作为 `click_xy` 参数，误差为0。
   - **第二优先级（兜底）**：只有坐标表中没有目标元素时，才允许凭截图目视估算坐标。

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

## 🚧 弹窗/遮挡层处理范式 (Overlay Handling)
> **遇到 'element is not clickable' 报错时的标准处置流程。**

### 判断是否被遮挡
- `click` 后返回 `"element is not clickable"` 或 `"ElementClickInterceptedException"`
- `extract` 返回的元素列表顶部存在含 "Cookie"、"同意"、"接受"、"隐私" 等字样的横幅或对话框

### 标准清除流程（3步范式）
```
第1步：extract（重新扫描当前视口，不要依赖任何旧 ID）
第2步：在 extract 结果中，按语义特征定位遮挡物的关闭控件：
       - 文字特征：包含 "接受"、"同意"、"关闭"、"×"、"Accept"、"Agree"、"Dismiss" 的按钮
       - 位置特征：通常是 fixed 定位的横幅（页面顶部/底部）或居中弹窗
第3步：click <新ID>（点击关闭后立即再次 extract，获取遮挡物消失后的干净页面 ID）
```

### 🔑 铁律：永远用语义特征，不传旧 ID
| ❌ 错误做法 | ✅ 正确做法 |
|---|---|
| `reflection` 里写 "关闭 ID:3 的横幅" | `reflection` 里写 "extract 发现含'接受Cookie'文字的横幅区域" |
| 直接 `click id:3`（历史里的旧 ID） | 先 `extract`，再 `click` 新获得的 ID |
| `progress_summary` 里传 `ID:3` | `progress_summary` 里写"寻找含'接受/同意/×'语义的按钮" |

### 兜底方案：DOM 找不到关闭按钮
```
1. scroll down → 检查横幅是否会随滚动消失（部分 Cookie 横幅滚动后自动收起）
2. screenshot → 让视觉模型目视识别关闭按钮位置 → click_xy
3. 如果以上均失败 → press Escape → 再次 extract 确认状态
```

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
- `{"action": "click_xy", "x": 320, "y": 150}`: **【视觉坐标点击】** 当 DOM 无法识别按钮或结构过于复杂时，先截图让视觉模型定位，再用此指令按坐标直接点击。坐标为**视口坐标**（与 extract 返回的 X/Y 同一系，左上角为0,0）。
- `{"action": "type", "id": 12, "text": "内容"}`: **【原子操作】** 自动点击元素获得焦点后立即输入。有 id 时无需先 click。
- `{"action": "type", "text": "内容"}`: 盲打模式，沿用当前焦点直接输入（用于 click 按钮后跟打）
- `{"action": "press", "key": "Enter"}`: 模拟按键（支持 Enter/Tab/Escape 等）
- `{"action": "wait", "seconds": 1}`: 强制等待指定秒数（小数也支持，如 0.5）
- `{"action": "wait_idle"}`: 智能等待页面 DOM 稳定
- `{"action": "read"}`: 读取当前正文
- `{"action": "scroll", "direction": "down"}`: 滚动页面，direction 可填 down/up/top/bottom
- `{"action": "back"}` / `{"action": "forward"}` / `{"action": "refresh"}`: 基础导航
- `{"action": "screenshot"}`: **【视觉升维】** 获取当前视口截图发给视觉模型。遇到 Canvas、复杂图表、验证码或 DOM 结构混乱时，**果断调用此工具**。
- `{"action": "ask_web_ai", "url": "kimi", "text": "问题"}`: **【杀手锏】** 遇到极难处理的混淆代码、报错或验证逻辑，立刻调用场外 Kimi 援助！
- `{"action": "new_tab", "url": "https..."}` / `{"action": "switch_tab", "id": 2}` / `{"action": "close_tab", "id": 2}`: 标签页管理

## 经典实操范式 (JSON Few-Shot Strategy)
以下是针对常见网页交互困境的标准 JSON 响应，请严格照抄思路与格式。

### 1. 目标元素不在视窗内 (Scroll)
```json
{
  "reflection": "extract 返回的 DOM 中没有找到含'提交'文字的按钮，但页面标题已正确显示。按钮大概率在页面底部",
  "thought": "先向下滚动页面，然后重新 extract 检查是否出现含'提交'语义的按钮",
  "description": "向下滚动页面寻找提交按钮",
  "tool": "browser_dom",
  "commands": [
    {"action": "scroll", "direction": "down"},
    {"action": "wait_idle"},
    {"action": "extract"}
  ],
  "todo_update": [],
  "memories_update": [],
  "next_tool_hint": "browser_dom"
}
```

### 2. 页面被弹窗/Cookie横幅遮挡 (Overlay)
⚠️ 关键原则：禁止传递旧 DOM ID，必须用语义特征描述目标元素。
```json
{
  "reflection": "上一步点击搜索按钮失败，报错 'element is not clickable'。发现一个含'接受 Cookie'文字的横幅正在遮挡操作区",
  "thought": "必须先清除遮挡物。寻找语义特征为'接受/关闭/×' 的按钮并点击，清除后再重新 extract 继续原操作",
  "description": "定位并点击 Cookie 横幅的关闭按钮",
  "tool": "browser_dom",
  "commands": [
    {"action": "extract"},
    {"action": "wait", "seconds": 0.3}
  ],
  "todo_update": [],
  "memories_update": [],
  "progress_summary": "页面被 Cookie 横幅遮挡，正在 extract 寻找含'接受/×'文字的关闭按钮",
  "next_tool_hint": "browser_dom"
}
```

### 3. 一步完成多个连续动作 (Pipeline)
```json
{
  "reflection": "页面跳转已触发，需要等待 DOM 稳定后才能提取元素列表",
  "thought": "跳转后需要依次做两件事：先 wait_idle 等待页面稳定，再 extract 获取页面元素。合并为一次流水线调用",
  "description": "等待页面加载完成并提取元素",
  "tool": "browser_dom",
  "commands": [
    {"action": "wait_idle"},
    {"action": "extract"}
  ],
  "todo_update": [],
  "memories_update": [],
  "next_tool_hint": "browser_dom"
}
```
⚠️ 记住：commands（复数）在最外层，绝不能嵌套在 command（单数）里！

### 4. 翻页采集循环 (Pagination)
⚠️ 每次 back 回列表页后，所有 ID 全部作废！必须重新 extract！
```json
{
  "reflection": "已完成第1篇详情页的 read。执行 back 回到列表页后，旧ID全部失效",
  "thought": "必须重新 extract 列表页获取新 ID。下一目标是找第2条文章链接：列表中第二个包含文章标题的 a 元素",
  "description": "back 后重新 extract，定位第2篇文章",
  "tool": "browser_dom",
  "commands": [
    {"action": "back"},
    {"action": "wait_idle"},
    {"action": "extract"}
  ],
  "todo_update": [],
  "memories_update": [],
  "next_tool_hint": "browser_dom"
}
```

### 5. DOM 无法识别复杂元素 (Screenshot/Vision)
```json
{
  "reflection": "连续两次 extract 都只返回极少节点，怀疑页面是 Canvas 渲染",
  "thought": "纯文本 DOM 已失效。必须触发截图，下一轮系统会交给视觉大模型识别位置并使用 click_xy 点击",
  "description": "DOM失效，请求截图升维到视觉模型",
  "tool": "browser_dom",
  "command": {
    "action": "screenshot"
  },
  "todo_update": [],
  "progress_summary": "页面是 Canvas 渲染，已请求视觉模型协助看图",
  "memories_update": [],
  "next_tool_hint": "browser_dom"
}
```
⚠️ 坐标系说明：收到带坐标表的截图后，优先使用坐标表（cx/cy）调用 `click_xy`，误差为0。

### 6. 误点广告/错误链接 (Back)
```json
{
  "reflection": "上一步点击了一个标题含'热门推荐'的链接，实际页面标题显示'广告推广页'。这是一个误导性广告链接",
  "thought": "使用 back 动作立即回退到上一个列表页，重新 extract 获取新 ID，跳过带广告特征的链接",
  "description": "误入广告页，立即回退",
  "tool": "browser_dom",
  "command": {
    "action": "back"
  },
  "todo_update": [],
  "memories_update": [],
  "next_tool_hint": "browser_dom"
}
```

### 7. 遇到登录墙/付费墙 (Paywall)
⚠️ 切记：遇到登录墙不要浪费步骤尝试登录，直接放弃并换站！
```json
{
  "reflection": "goto 进入页面后，read 返回的内容充斥'请登录后查看'，有效正文不足10字",
  "thought": "该站点对目标内容设有付费墙，继续挣扎是死路。立刻将此站标记为黑名单，切换到其他可以免费访问的站点",
  "description": "放弃付费墙站点，换用其他公开来源",
  "tool": "browser_dom",
  "command": {
    "action": "goto",
    "url": "https://www.bing.com/search?q=马斯克+SpaceX+site:reuters.com"
  },
  "todo_update": [],
  "progress_summary": "【排雷黑名单】\n❌ wsj.com: 付费订阅墙 → 永久拉黑，改用 Reuters/BBC",
  "memories_update": [],
  "next_tool_hint": "browser_dom"
}
```
</tool_specific_instructions>