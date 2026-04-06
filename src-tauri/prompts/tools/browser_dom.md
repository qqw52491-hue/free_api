<tool_specific_instructions>
<summary>网页全自动操控。核心动词: goto(跳转), extract(提取ID), click(点击), type(输入), scroll(滚动), read(语义阅读), back(后退)。</summary>

# 网页自动化 (browser_dom) 战术指南

## 狙击手战术 (Tactical Heuristics)
- **零容忍地毯式搜索**：如果 extract 结果中有 `<input>` 或带 search 关键字的 ID，第一步永远是输入关键词搜索。绝对禁止逐个点击分类链接去"碰运气"。
- **死胡同撤退策略 (Dead-End Retreat)**：如果你点击了一个分类或链接，发现里面没有你要的数据，必须立刻调用 `back` 返回上一页，并在 TODO 中将该尝试标记为 canceled，然后换搜索词或其他策略。
- **抗门口效应 (Sign-In Wall)**：如果 read 结果包含过多"Login", "Sign In"，且当前滚动高度为 0，说明数据被折叠或在视口下方。你必须先 `scroll down` 两次来探测真实正文。

## DOM 刷新铁律 (Critical State Rules)
- **ID 必定失效规则**：每次发生 `goto`、`click`(导致跳转)、`back` 或 `refresh` 后，当前页面的所有元素 ID **瞬间作废**！你必须在下一步立即执行 `extract` 生成新 ID，严禁凭记忆点击旧 ID。
- **列表循环范式 (List-Detail-Back)**：`extract` -> 2. `click` (进详情) -> 3. `read`/`extract` (抓数据并存 memory) -> 4. `back` (退回列表) -> 5. `wait_idle` (等 DOM 稳定) -> 6. **必杀技**：重新 `extract` (获取新列表ID)。

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
- `{"action": "extract"}`: 提取当前视口内交互元素
- `{"action": "click", "id": 12}`: 点击链接/按钮（只用于导航跳转类按钮，**不要用它来聚焦输入框**）
- `{"action": "type", "id": 12, "text": "内容"}`: **【原子操作】** 自动点击元素获得焦点后立即输入。有 id 时无需先 click。
- `{"action": "type", "text": "内容"}`: 盲打模式，沿用当前焦点直接输入（用于 click 按钮后跟打）
- `{"action": "press", "key": "Enter"}`: 模拟按键（支持 Enter/Tab/Escape 等）
- `{"action": "wait", "seconds": 1}`: 强制等待指定秒数（小数也支持，如 0.5）
- `{"action": "wait_idle"}`: 智能等待页面 DOM 稳定
- `{"action": "read"}`: 读取当前正文
- `{"action": "scroll", "direction": "down"}`: 滚动页面，direction 可填 down/up/top/bottom
- `{"action": "back"}` / `{"action": "forward"}` / `{"action": "refresh"}`: 基础导航
- `{"action": "screenshot"}`: 获取视图快照
- `{"action": "ask_web_ai", "url": "kimi", "text": "问题"}`: **【杀手锏】** 遇到极难处理的混淆代码、报错或验证逻辑，立刻调用场外 Kimi 援助！
- `{"action": "new_tab", "url": "https..."}` / `{"action": "switch_tab", "id": 2}` / `{"action": "close_tab", "id": 2}`: 标签页管理

## 经典实操范式 (Few-Shot Strategy)

**场景 1：在百度搜索框输入并搜索**（展示 type 原子能力）
```
第1步：extract（找搜索框 ID，假设是 12）
第2步：commands: [{"action":"type","id":12,"text":"今天新闻"}, {"action":"press","key":"Enter"}]
```
⚠️ 注意：**没有单独的 click 步骤**！type 已经内置点击。

**场景 2：抓取首页前 2 篇文章内容**
```
第1步：extract（找出前2条链接 ID）
第2步：click id=15（点进第1条，这里 click 用于导航跳转）
第3步：read → memories_update（存数据）→ back（退回）
第4步：extract（⚠️ 必须！back 后所有 ID 失效，必须重新 extract）
第5步：click id=18（点进第2条）
```
</tool_specific_instructions>