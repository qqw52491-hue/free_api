# 多 Agent 编队与自治沙箱系统演进总结（P3 ~ P5 核心架构与微信通道深度剖析）

> 日期：2026-09-01  
> 覆盖范围：P3（角色卡 v2 与物理围栏）、P4（编队规模与批量派发）、P5（Git 快照基线、三态冲突判定、生命周期状态机与脱水追问）以及微信 iLink 通道收发机理诊断。

---

## 目录
1. [架构演进全景图](#一架构演进全景图)
2. [P3 阶段：角色卡 v2 与物理围栏隔离](#二p3-角色卡-v2-与物理围栏隔离)
3. [P4 阶段：编队规模与批量并发派发](#三p4-编队规模与批量并发派发)
4. [P5 阶段：Git 快照、三态冲突指纹与脱水追问](#四p5-git-快照三态冲突指纹与脱水追问)
5. [微信通道收发问题深度剖析与排查建议](#五微信通道收发问题深度剖析与排查建议)
6. [后续优化建议与展望](#六后续优化建议与展望)

---

## 一、架构演进全景图

在本次重构前，多 Agent 体系主要依赖主 Agent 串行派发简单的子任务，存在三大核心痛点：
1. **安全性裸奔**：子 Agent 可以任意读取甚至越权篡改父工作区任意代码（无沙箱边界）。
2. **并发与资源失控**：大模型若发生幻觉极易无限循环派发子 Agent，耗尽系统资源与 API 额度（无全局预算约束）。
3. **协同数据破坏**：子 Agent 修改直接盲目覆盖主工作区，遇到并发修改或主干变动容易引发代码丢失（无冲突检测）。

经过 **P3、P4、P5** 三个阶段的高强度重构，多 Agent 系统完成了从“简单串行脚本”到**“工业级安全自治编队”**的蜕变：

```mermaid
graph TD
    A[主 Agent (Root Coordinator)] -->|1. 任务分解 dispatches 批量| B[调度前瞻看门狗 (Gatekeeper)]
    B -->|全局预算 ≤ 6 / 单类配额| C[子 Agent 容器孵化]
    
    C -->|只读侦察| D[child_worker]
    C -->|代码审查| E[child_reviewer]
    C -->|隔离写入| F[child_writer]
    
    D -->|只读命令白名单 + hard_scope 围栏| W1[主工作区只读]
    E -->|质量评分 + 门禁校验| W2[代码审阅]
    F -->|克隆副本 .writer-* + init_git_baseline| G[独立沙箱工作区]
    
    G -->|TDD 测试通过 + 产出补丁| H[build_changed_files]
    H -->|48KB 预算分级降级| I{三态冲突纯函数判定}
    
    I -->|root_now == spawn| J1[CleanReplay: 安全合入]
    I -->|root_now == result| J2[AlreadyApplied: 无需合入]
    I -->|三者互不相等| J3[Conflict: 真冲突报警拦截]
    
    F -->|脱水落盘 message_history_json| K[(SQLite 状态留存)]
    A -->|追问补充| L[resume_child]
    L -->|步数折半 + 继承副本与脱水历史| F
    
    A -.->|父 run 终态 RAII Guard| M[cleanup_child_workspaces 统一回收]
```

---

## 二、P3: 角色卡 v2 与物理围栏隔离

### 1. 核心目标
解决子 Agent 权限失控问题，确保“专人专事、围栏禁足、身份可控”。

### 2. 关键设计与实现
- **角色卡 v2 数据结构升级** (`src-tauri/src/agent/tools/role_manager.rs`)：
  - `allows_kind`: 限制该角色只能挂载到指定形态的子 Agent（如 reviewer / scout / writer）。
  - `hard_scope`: 设定该角色允许访问的物理工作目录（如仅限 `src/components/`）。
  - `max_concurrency`: 限制该专精角色的最大并行数。
  - `default_test_command`: 为写入型角色预先设定强制准出测试命令。
- **底层物理路径围栏（Path Canonicalization）**：
  - 子 Agent 执行 `read_file`、`list_dir`、`write_file` 时，所有路径强制通过 `canonicalize()` 规范化为物理绝对路径。
  - 严格校验 `path.starts_with(scope_root)`，利用系统文件系统调用物理切断通过 `../` 进行目录逃逸的可能。
- **类型安全拦截**：
  - 调度器在分发子任务时校验 `allows_kind`，防止主模型误将“侦察角色”当成“写入角色”挂载导致非预期行为。

---

## 三、P4: 编队规模与批量并发派发

### 1. 核心目标
解决主 Agent 只能逐个发起子任务的低效问题，同时防止大模型幻觉引发子 Agent 无限裂变（Fork Bomb）。

### 2. 关键设计与实现
- **`dispatches` 结构化批量派发协议** (`src-tauri/src/agent/types.rs`, `loop_runner.rs`)：
  - 主模型在单次工具调用中可传入 `dispatches: [ {task, focus_paths, role}, ... ]` 数组。
  - 支持一步并发启动多达 3 个子任务（如并行侦察前端、后端和测试），极大减少多轮对话往返开销。
- **两道看门狗配额机制（Double Gatekeeper）** (`src-tauri/src/agent/child_reviewer.rs`)：
  - **全局硬顶看门**：每个父运行累计子任务上限 `MAX_TOTAL_CHILDREN_PER_RUN = 6`，防止极端情况下任务递归失控。
  - **角色/单类配额**：默认单类最大并行数为 3；写入类 Agent (`writer`) 硬编码限制为 1 个（保证写入责任单一收敛，杜绝多沙箱写入竞态）。
- **柔性降级与防呆（Fault-Tolerant Execution）**：
  - 批量派发中部分任务参数异常时，系统采取“成功派发的进后台，失败的任务就地记录回执”的策略，避免整个批量操作因单个子项报错而全盘崩溃。

---

## 四、P5: Git 快照、三态冲突指纹与脱水追问

### 1. 核心目标
彻底解决沙箱写回时的“代码覆盖冲突”与子任务完成后的“无记忆追问”难题。

### 2. 关键设计与实现

#### 5a. Git 快照基线与 48KB 预算分级降级
- **自建 Git 基线快照** (`src-tauri/src/agent/child_writer.rs`)：
  - 沙箱副本在跳过 `.git` 目录复制后，初始化内联 Git 环境：
    `git init -q && git add -A && git -c user.email=agent@local -c user.name=writer commit -qm "spawn baseline" --allow-empty`
  - 即使宿主机器未配置全局 Git identity，也能为每个沙箱生成干净的起点 Commit。
- **48KB Unified Diff 分级降级梯队**：
  - **Tier 1 (完整 Hunk)**：总差异 ≤ 48,000 字符时，输出带上下文行号的标准 Unified Diff。
  - **Tier 2 (零上下文压缩)**：超出预算时，自动重算 `git diff --cached --unified=0`，去除未改动上下文行。
  - **Tier 3 & 4 (Artifact 落盘保全)**：极端超长改动时，调用工件管理器生成 `.diff` 文件落盘，回执仅内联各文件前 10 行预览并附带 `patch_ref` 标识。
  - **Tier 5 (兜底)**：无 Git 环境时自动平滑回退至纯文本逐行比对 `line_diff`。

#### 5b. 三态指纹冲突判定纯函数 (`evaluate_file_conflict`)
采样三点哈希指纹：`spawn_hash`（派发前基线）、`result_hash`（沙箱修改结果）、`root_now_hash`（当前主工作区状态）：
| 条件 | 状态 | 行为 |
| :--- | :--- | :--- |
| `root_now == spawn_hash` | **`CleanReplay`** | ✅ 主工作区未变，变更可安全合入 |
| `root_now == result_hash` | **`AlreadyApplied`** | ✅ 主工作区已包含该修改，自动略过 |
| 三者互不相等 | **`Conflict`** | ⚠️ 发生代码真冲突，绝对禁止暴力覆盖 |

#### 5c. 副本生命周期解耦与脱水追问（Resume）
- **生命周期挂载至父 run**：
  - 子 Agent 执行完毕后，沙箱工作区保留不删（保活）。
  - 在 `main_loop.rs` 中使用 Rust 的 RAII 特性封装 `ChildWorkspaceGuard`，仅在父主任务彻底结束（包括正常结束、报错或用户手动取消）时统一批量回收。
- **历史记录智能脱水 (`dehydrate_child_messages`)**：
  - 单条 Observation 超过 4,000 字符时智能截取头尾（保留前 2,500 字符与后 1,500 字符），持久化存入 SQLite `agent_child_runs.message_history_json`。
- **追问工具 `resume_child`**：
  - 允许主模型针对已成功的子任务追加新指令。
  - 步数上限自动减半（防止无休止消耗），继承原沙箱工作区与脱水上下文继续攻坚，实现多轮迭代开发。

---

## 五、微信通道收发问题深度剖析与排查建议

### 1. 现状现象描述
- 微信发送消息：桌面端程序能收到指令并成功触发 Agent 执行（如收到“你好呀”，在 SQLite 里留存了成功的 run 记录）。
- 桌面端主动回复/推送：本地日志显示 `[bridge] 主动推送已下发`，**但手机微信端没有任何新消息弹出**。

### 2. 根本原因定位（底层协议与架构分析）
free_api 使用的 `wechat-bridge` 基于腾讯官方的 **iLink (ClawBot) 个人号协议**（服务端地址 `https://ilinkai.weixin.qq.com`）。该协议存在以下关键机制：

1. **`context_token` 关联会话机制**：
   - iLink 协议为了防止垃圾消息轰炸，**要求下行消息必须携带有效的 `context_token`**。
   - `context_token` 是在用户每次向 Bot 发送上行消息时，由微信服务端动态下发的。
2. **HTTP 200 与静默丢弃（Silent Drop）假象**：
   - 腾讯 iLink 的 `POST /ilink/bot/sendmessage` 接口无论实际投递是否成功，**HTTP 状态码均返回 200 OK**。
   - 在我们的实际测试中，即使故意不传 `context_token`，腾讯服务端依然返回 `{"ret": 0}`！但在微信服务端路由层，因为无法关联活跃会话，**该消息会被腾讯底层静默丢弃**。
3. **微信客户端呈现入口差异**：
   - iLink 机器人使用的是 `bot_type=3`。扫码登录时生成的链接是 `https://liteapp.weixin.qq.com/...`。
   - **重要区别**：它在微信端**并不一定是以普通“微信好友”的形式展示**，而是挂靠在微信官方的“服务通知”、“小程序智能助理/腾讯元宝助理”或者专用的扫码交互页面中。如果在普通联系人列表等待，会产生“没有收到”的错觉。
4. **SDK 内存状态易失（In-Memory Loss）**：
   - `weixin-agent-sdk` 中的 `contextTokenStore` 是一个 **内存 Map**，并没有持久化到本地磁盘。
   - 一旦 `wechat-bridge` 进程发生重启（或断线重连），内存中的 `context_token` 全部丢失。此时如果后台定时任务触发主动推送，会因为拿不到有效 token 被微信网关拒绝。

### 3. 诊断与排查步骤建议
针对上述机理，建议明天按以下顺序进行验证：
1. **核对微信端交互窗口**：
   - 打开手机微信，查看微信登录扫码后打开的页面（通常为小程序或微信小微客服窗口），确认是否在当前会话窗口中有回复。
2. **验证最新上下行即时回路**：
   - 保持程序运行，在手机端主动发一条文字 `状态`。
   - 观察是否能收到菜单回复（因为收到消息的瞬间，`context_token` 绝对新鲜且有效）。
3. **增强 SDK 响应日志打印**：
   - 在 `wechat-bridge/index.js` 或对 `weixin-agent-sdk` 的发送逻辑中，显式解析并打印腾讯接口返回的 `rawText`，以捕获是否有 `ret: -2` 等频控或失效代码。

---

## 六、后续优化建议与展望

1. **持久化微信 `context_token`**：
   - 将接收到的 `context_token` 与时间戳持久化存入 `wechat-bindings.json`，即使桥进程意外重启，24 小时内的主动推送依然具备合法凭据。
2. **多 Agent 冲突自动合入流程（3-Way Auto-Merge）**：
   - 目前 P5 在发生 `Conflict` 时采取“安全告警拦截”。后续可引入类似 `git merge-file` 的自动 3-way 尝试合入，若仅修改不同函数则自动无感合入。
3. **前端 Team Deck 历史回溯看板**：
   - 在右侧 Team Deck 抽屉中增加“已完成子任务列表”，点击可展开查看该子任务的 Unified Diff 与测试输出证据链。
