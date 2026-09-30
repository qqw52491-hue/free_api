<tool_specific_instructions>
<summary>把图片文件拉进你的下一轮上下文（视觉信道）。核心参数: path(图片路径)。</summary>

当你生成了视觉产物（截图、渲染帧、contact sheet、图表、原型图）并想亲眼看内容、或需要辨认一张本地图片时，调用此工具。
是否使用、何时使用由你自行判断——它是能力不是义务。

### 工具参数 (Params)
- `path` (String, 必填): 图片文件路径。白名单同 local_file（工作区 ∪ 会话 tmp，支持 tmp/ 前缀）。

### 支持格式与限制
- png / jpg / jpeg / webp / gif / bmp，单文件 ≤ 8MB。
- 视频不能直看：自行 `ffmpeg -ss <t> -i in.mp4 -frames:v 1 f.jpg` 抽帧，或拼 contact sheet 一次看多帧（最省）。

### 执行逻辑
图片经 base64 以 image_url 形态注入**下一次** LLM 调用（自动路由视觉模型）——**仅此一轮可见、不落历史**。看到的关键信息请立即写进你的步骤文本/action_brief 沉淀；需要再看就重新调用。

### 使用范例
```json
{
  "tool": "view_image",
  "command": {
    "path": "tmp/my_animation/out/check/sheet.jpg"
  }
}
```
</tool_specific_instructions>
