# paste_probe fixture

最小复现工程：聊天输入框粘贴处理模块 + 验收测试。

- `paste_handler.js` — 被测实现（当前有缺陷：在 Wayland/WebKitGTK 下
  复制图片文件与截图粘贴均无反应，已经历两轮失败修复，见 FIX_HISTORY.md）
- `test_paste.mjs` — 环境契约模拟与验收断言（勿改）
- 运行：`node test_paste.mjs`（全绿即通过）
