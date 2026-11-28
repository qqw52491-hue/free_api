# SandwichAgent 的 ACP 桥接

`free-api-chat --acp` 是 ACP v1 的本机入口。客户端启动这个子进程后，子进程通过 stdin/stdout 收发 ACP 消息，再把请求交给正在运行的 Free API Chat 桌面版。

桌面版持有数据库锁，并用现有的 Agent 主循环执行任务。ACP 和桌面端共享项目、模型、历史、审批及任务记录。内部 Worker、Writer 和 Reviewer 仍由 SandwichAgent 自己调度。

## 在 Zed 中使用

1. 构建并启动包含此功能的 Free API Chat 桌面版，确保已配置并启用模型。
2. 将下列条目合并到 Zed 的用户设置。把 `command` 改为本机可执行文件的绝对路径。
3. 在 Zed 打开项目目录，在 Agent Panel 中新建 SandwichAgent 会话并发送任务。

```json
{
  "agent_servers": {
    "SandwichAgent": {
      "type": "custom",
      "command": "/Applications/Free API Chat.app/Contents/MacOS/free-api-chat",
      "args": ["--acp"]
    }
  }
}
```

本仓库的开发构建位于 `src-tauri/target/debug/free-api-chat`，可以用它的绝对路径作 `command`。启动桌面版时使用同一次构建的可执行文件，不带 `--acp`。配置格式见 [Zed External Agents](https://zed.dev/docs/ai/external-agents)。

任务使用桌面版通道路由的模型配置。未设置通道默认模型时，桥接建会话会选择通道 Pro 槽位或数据库里已启用的模型。ACP 面板当前没有模型选择器。

## 这版支持的交互

| 操作 | 桌面版中的行为 |
| --- | --- |
| 新建会话 | 以客户端的绝对工作目录查找或创建项目，再创建项目会话 |
| 发任务 | 通过原有通道入口提交到主循环，并实时回传思考和工具调用，结束时发送桌面端确认的交付正文 |
| 批准或打回方案 | 通过 ACP 的授权选项回传给该张审批卡。选项不携带打回意见 |
| 回答普通问题 | 将问题显示为正文并结束当前回合。下一条消息回答原任务的这条问题 |
| 取消正在执行的回合 | 经桌面端的运行控制入口停止指定运行。若未拿到桌面端的停止确认，客户端会收到说明 |

这版支持文本和作为文本传递的文件引用。客户端提供的图片、音频、嵌入资源和 MCP 服务不进入桌面任务。方案和审查模式沿用桌面通道的配置。客户端暂未提供历史会话恢复、执行中补充目标、子任务面板和对外派发。

## 本机连接

- 桌面端启动独立的回环监听器，发布端口与临时令牌到通道配置所在目录。默认文件名为 `channel-config.acp.json`。在 macOS 和 Linux 上，文件权限限定为当前用户可读写。
- ACP 事件流和按工作目录建项目的入口只在该监听器提供。桥接在派发前订阅目标会话，按运行号跟踪状态，思考只传新增部分。原始模型指令和可被覆盖的正文预览保留在桌面侧。审批和文本回答核对具体问题的标识。
- 桥接在派发前分配运行号，桌面先保存受理记录再启动。回执丢失时，桥接按原号码回查或重试，同一个号码只启动一次。进度连接断开时也按原号码重连，断线期间的个别进度可能缺失，最终正文从对应运行的交付记录读取。
- 若桌面显示旧版本或端点不可达，请打开含 ACP 桥接的桌面版后重试。客户端可独立完成协议握手，创建会话和执行任务需要连接到桌面版。

测试或并行实例可为两个进程设置相同的 `CHANNEL_CONFIG_PATH`。ACP 进程也可以同时使用 `SANDWICH_ACP_CHANNEL_URL` 和 `SANDWICH_ACP_CHANNEL_TOKEN` 指定某个本机测试端点。设置 `SANDWICH_ACP_VERBOSE=1` 可显示更多事件日志。设置 `SANDWICH_ACP_PLAN_MODE` 可为桥接派发指定方案模式，未设置时沿用通道的 `cross_review` 默认值。

## 实现与后续

- Rust SDK 固定为 `agent-client-protocol = 5.6.1`，依赖 schema 1.22.0。两者的版本号与 ACP 协议主版本分开。
- 协议服务、桌面通道客户端和事件映射位于 `src-tauri/src/acp/`。事件来自统一的 `agent/events.rs` 出口，终态以原运行账本核对。
- P0 在 `/tmp/acp_spike` 验证了协议收发。P1 将桥接接到正式桌面入口，继续使用已有数据库锁。
- 客户端可选能力、独立无桌面运行与 SandwichAgent 向其他 ACP agent 派发，属于后续独立工作。

## 本机验证

使用临时数据库和本机模拟模型提供商，真实桌面引擎已跑通普通任务、批准、打回、文本问答、执行中取消和审批等待中取消。在 Zed 中也实际验证了派发、进度显示、审批选项和批准后的正文回传。AWS Quick 与 JetBrains 的界面交互仍待验证。

[ACP 请求、进度和取消](https://agentclientprotocol.com/protocol/v1/prompt-turn) · [ACP 工具调用与授权选项](https://agentclientprotocol.com/protocol/v1/tool-calls)
