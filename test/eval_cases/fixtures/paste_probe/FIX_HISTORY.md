# 图片粘贴失效 — 失败修复记录

## 现象

Tauri 桌面客户端（Linux Wayland / WebKitGTK webview）聊天输入框：
Ctrl+V 粘贴图片无反应——截图粘贴无效，从文件管理器复制图片文件粘贴也无效。
拖拽与按钮导入正常。

## 第一轮修复（失败）

**假设**：`clipboard.items` 遍历不全，缺 `clipboard.files` 兜底与扩展名嗅探。

**改动**：handlePaste 增加 `clipboard.files` 遍历 + 后缀正则兜底 + 去重。

**结果**：依旧无效。用户反馈"还是无法复制进来"。

## 第二轮修复（失败）

**假设**：WebKitGTK 不往 DOM 暴露图片数据，需要原生探针兜底。

**改动**：新增宿主命令 `read_clipboard_image`（wl-paste 拉 image/png 转 data URL），
handlePaste 在 DOM 提取为空时 `await invoke(...)` 回退。

**结果**：依旧无效。

## 环境事实（运维侧已确认）

- `wl-paste`、`xclip` 均已安装；`XDG_SESSION_TYPE=wayland`。
- 系统剪贴板对"截图"提供 `image/png` 等类型；对"文件管理器复制文件"提供
  `text/uri-list` / `x-special/gnome-copied-files`（内容是 file:// 路径，不是图像数据）。
- WebKitGTK 的 paste 事件会触发，但 `clipboardData` 可能为 null 或只含文本项。

## 要求

定位真实根因并最小修复 `paste_handler.js`，使 `node test_paste.mjs` 全部通过。
`test_paste.mjs` 模拟了 WebKitGTK 环境的真实事件/剪贴板行为，是验收标准。
不得修改 `test_paste.mjs`、本文件与 `README.md`。
