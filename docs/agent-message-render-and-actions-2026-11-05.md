# Agent 回复分隔符与消息操作栏修复

## 事实与原因

- 只读核对本机 `data_v2.db` 的消息 `33892279-1c1d-422a-8713-421f4736fa4e`，会话 `fc72f41c-8e31-4dbf-8566-e5fa1847d801`。原始报告有 126 个真实换行和 7 个字面量 `\n`，包括真实换行之间的 `---\n`。截图分隔符来自回答原文的格式错误，不是正确换行被渲染器改坏；本次未修改数据库或历史正文。
- 用户气泡的操作栏原本绝对定位在底边外 4px；历史消息同时启用 `content-visibility:auto`。真实 Chromium 挂载 `AgentChatStream.vue` 后，按钮中心的命中对象是列表容器，实际点击没有复制。操作栏越出绘制边界，加上悬浮间隙，导致看得到却点不到。
- 原复制降级路径忽略 `document.execCommand('copy')` 返回的 false，会错误显示“已复制”。

## 修改范围

- `format.ts` 仅把代码区外、独占一行的 `---\n`（至少三个连字符）显示为分隔线。没有全局解码转义；普通正文示例、代码、正则、路径保持原样；复制仍使用原始消息。
- 用户正文单独使用 `.user-message-bubble` 绘制气泡，操作栏仍在气泡外侧底部。透明消息容器共同包住正文、间隙与操作栏，避免越出绘制边界并保持悬浮连续；鼠标、键盘焦点和无悬浮设备均有可达入口。任务执行中禁用分叉的原规则保留，复制仍可用。
- `clipboard.ts` 共用复制与降级逻辑，根据真实返回值显示成功或失败，清理临时节点并尽力恢复焦点与选择。
- 没有添加模型提示词、格式门禁或额外模型调用。这是已有错误形状的显示兼容，不能保证模型以后都生成正确 Markdown。

## 验证与边界

- `node scripts/test-agent-message-actions.mjs`：通过。实际 Chromium、实际 Vue 组件/CSS，鼠标命中并点击折叠/展开长消息、短消息、窄屏的复制与分叉；键盘 Enter 可复制；运行中分叉禁用；助手复制保留原始转义；拒绝剪贴板后的失败/成功降级与节点清理。剪贴板接口与分叉事件接收方为测试替身，不会触发真实会话分叉，也不宣称验证了系统剪贴板权限。
- 修复前按钮中心命中 `agent-timeline-list`，修复后命中 `msg-copy-btn`；测试同时断言按钮在气泡下方、鼠标停留气泡与操作栏之间 900ms 后仍可见，避免只验证瞬时状态或把按钮误移入气泡。
- `node scripts/test-markdown-escapes.cjs`、`node scripts/test-markdown-images.cjs`、`npm run build`（含类型检查）、`git diff --check`：通过。
- `scripts/test-agent-question-navigation.cjs` 在 `questionNavigationKey` 断言失败；用 HEAD 原始组件复现相同失败，是已有脚本与当前实现不匹配，本次未调整导航逻辑。
- 未重启桌面应用，未验证原生 WebKit、真实系统剪贴板或真实分叉后端。本次未修改 Rust，未重跑 Rust 全量测试；此前未提交的上下文/审查修改保留。

浏览器回归需要 Node >= 22 和 Chrome，可用 `CHROME_BIN` 指定可执行文件，默认查本机 Puppeteer Chrome 缓存。截图保存到脚本打印的临时目录。
