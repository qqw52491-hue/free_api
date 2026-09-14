# OVERNIGHT_LOOP — 控制面改动披露台账

> 纪律：每项控制面改动（门禁/判据/收割/预算/审查契约）在此登记：改了什么、为什么、commit、验证状态。
> 状态标注：code-green（单测/编译过）| live-green（实弹验证过）| calib-green（基线校准过）| unverified（未验证）

## 阶段十：门禁适用域（applies_to）三值层落地 — 2026-09-14

**病灶定性**：「门禁无差别生效」是设计病不是单点 bug——所有机械门只有 Pass/Block 二值，
把"本门证据域在当前任务不存在"硬塞进"违规"格。实弹现场：`sudo pacman -Rns` 卸载
WorkBuddy 完美完成后，零交集门拿 Git diff 尺子卡 /usr/bin、/opt 目标，第 40–49 步
连续打回成死循环。

**更深层根因**（本次审计新发现）：`is_readonly_shell_command` 白名单没有
pacman -Q/systemctl/test/curl 等探针 → 探针被当**副作用**入账 → `record_side_effect`
清空 change_verification → **每次探测都重置验证态**，验证闭环物理上不可能闭合。
修复必须两头打：探针不再误记副作用 + auto-verify 认得探针。

| 改动 | 位置 | 设计要点 | 验证 |
|---|---|---|---|
| **GateApplicability 三值 enum** | `agent/gate_applicability.rs`（新） | `Applicable`/`NotApplicable{reason}` + `handoff_notice()` 生成终审交接语 + `is_workspace_target()`（相对路径=区内，绝对路径须 workspace 前缀，~/ 展开，盘符/URL=区外） | code-green（2 新测试） |
| **零交集门 → 第一个适用域范例** | `main_loop.rs` finish 路径 | applies_to 自证：声明目标全在工作区外 → NotApplicable 跳过+Notice+telemetry（`finish_review:plan_target_mismatch:not_applicable`）；对账判据扩为 diff ∪ affected_paths（非 git 写入同样构成"目标被动过"物理事实） | code-green |
| **契约证据三分法** | `finish_guard/diff.rs` | refs 分三域：file:line 可解析→照常对账；形似代码位置（`src/x.rs:abc`、可落盘裸路径）→ 仍属代码域判失败（防"写个路径蒙混"逃对账）；全非代码且含真实观测形态（`looks_like_operational_evidence`：已知命令头/output_id/step_N/退出码/返回结果）→ 不适用跳过；**裸结论（"已完成优化"）仍拦截**——不是证据 | code-green（4 新测试） |
| **只读白名单扩容** | `context/audit.rs` `is_readonly_shell_command` | 包装器剥壳（sudo 直跟/timeout/nice/nohup/stdbuf/time 吃旗标+数值操作数）+ 多态命令子命令判别（pacman 只认 -Q/-T 操作位、systemctl 查询族、dpkg -l/-s/-L/-p/-V、apt list/show/-s、curl 禁 -d/-F/-T/-X写动词/-o非devnull、journalctl 禁 vacuum/rotate/flush、ip 禁 set/add/del/flush、tar 含t不含cx、service 仅 status、nc 仅 -z、mount 仅裸跑、command 仅 -v、env 仅全赋值）；恒只读新收录 test/[ /[[/stat/pgrep/ps/diff/cmp/sha256sum/ss/lsof/df/du/free/jq/sleep 等 | code-green（`state_probe_commands_are_readonly_not_side_effects` 60+ 断言） |
| **auto-verify 通用探针识别** | `change_verification.rs` `is_state_probe_command` + `is_absence_probe_command` | 成功：which/test -[/pacman -Q/systemctl 状态族/curl -f/-w http_code/grep -q/diff -q/docker inspect/kubectl get 等算验证；**失败**：缺失探针子集（which/test -e/pacman -Q/systemctl is-*/pgrep 等纯存在性查询）的非零退出=状态断言，自动 verified（证据标注"非零退出=状态断言"）；grep -q/diff -q 不入缺失子集（失败可能是调试中途） | code-green（2 新测试） |
| **Gate Notice 终审交接 + Skip telemetry** | `main_loop.rs`/`finalization.rs`/`reviewer_gatekeeper.rs` | `gate_notices: Vec<String>` 沿 finish 路径汇入 `evaluate_finish_diff`，追加进 review_material【门禁适用域交接】段；prepare_finish 同链路带参；Skip 全量 record_gate_fire（`...:not_applicable` 后缀路由），Skip 率可审计防系统性钻空子 | code-green |

**验证**：`cargo check` 绿；`agent::` 全套 **575/575** 通过（含 9 个新增回归用例）。
live 验证待下轮运维类实弹（卸载/服务管理任务应一次通过而非死锁）。

**遗留风险**：①`has_code_modification_intent` 的关键词表（修改/修复/write/fix…）
仍是 NLP 启发式，只作附加保险丝，物理层（工作区目标判定）吸收其误判；
②非代码证据移交终审后，审查看的是"语义证据"——若终审放水，Skip 率遥测
（`noncode_refs:not_applicable`/`plan_target_mismatch:not_applicable`）是审计抓手。

### 阶段十追加：设计复核后的三处收紧（同行评审反馈）

复审发现初版实现把「观测到状态」与「达成目标」混为一谈、且只读白名单扩容
引入了复合命令误判面。三处收紧全部落为物理判定，不做语义猜测：

| 收紧点 | 病灶 | 修法 |
|---|---|---|
| **复合命令逐段判定**（audit.rs） | `which x && rm y` 首段只读→整串误判只读→写入漏记副作用（旧代码已有此洞，扩容放大暴露面）；`$(cmd)`/反引号/`<(`/`>(` 内层不可审计 | 按 `&&/||/;/|/&/换行` 在原文扫描拆段（引号内分隔符不拆、`>&`/`|&`/`&>`不误拆、`[[ a || b ]]` 未闭合片段回合并），每段独立判只读；命令/进程替换一律非只读 |
| **探针-写入绑定**（change_verification + main_loop） | 写代码后随手 `which python` 也能闭合验证——"发生了一次断言"≠"断言的是这次变更" | `probe_binds_to_pending_writes`：探针 token（去命令头/旗标/噪声词，路径拆段）必须与待验证副作用描述或受影响路径字符串重合才自动闭合；绑定失败→验证保持未闭合，模型走显式举证 |
| **失败探针不自动 verified**（main_loop） | `which x` 非零退出是否达标取决于任务意图（卸载 vs 安装方向相反），宿主猜意图是语义越权 | 绑定的存在性探针失败→add_error_feedback 指引模型用 change_verification_update(verified, [step_N]) 显式举证；终审凭 step_N 核验该断言 |
| **观测形态收紧**（diff.rs） | "返回正常"类措辞凭裸词就能算观测证据 | 句柄须带对象：`output_id:<值>`、`step_<数字>`、结果词（退出码/exit/http/状态码/返回/→）+ 数字三态之一；裸结论仍拦截 |

**验证**：`agent::` 全套 **577/577**（新增 `compound_commands_require_every_segment_readonly`
26 断言 + `probe_binding_requires_shared_object_with_pending_writes`）。

**复核确认不动的部分**：生态测试命令（cargo/npm/pytest）、契约 verify_command
匹配、宿主推断验证命令仍无条件自动闭合（宿主可自证其来源）；file:line 对账、
代码域伪造引用、裸结论拦截、零交集工作区内判定逐条保留——收紧只作用于
"通用探针"这一新增通道。

## 阶段七：Token 压缩三项 Quick Win（P2/P5/返工段对症）— 2026-09-14

依据阶段六 E 项解剖结论落地的三处零副作用收口，全部 **code-green**（554 agent 测试通过，含新增预检回归），live 验证待下轮实弹。

| 改动 | 位置 | 病灶 | 验证 |
|---|---|---|---|
| **检索去重回执收口（锚点+句柄）** | `retrieval/mod.rs` `lookup_cached_search` + `main_loop.rs` steps_json 补 `output_id` 字段 | P2：缓存命中仍全量 Dump hits，省 IO 不省 Token，且尾句"改换关键词强制重检"主动诱导重搜 | code-green |
| **wait_jobs 空结果三态消歧** | `loop_runner.rs` wait_jobs 交付路径 | P5：空结果只报"无新完成"，不区分"在途/待收/已全部交付"，诱发空轮询（旧会话 295 步连刷） | code-green |
| **submit_plan 静态风险预检** | `plan_gate.rs` `plan_risk_preflight` | 返工段 52–64%：初版方案缺防御设计被打回。target_files 命中 敏感凭据/缓存/重试循环/持久化 四类模式且文本未显式陈述防御设计时拦截指路；含「不涉及/只读」等显式豁免逃生口，不造无解打回链 | code-green（新增 `test_plan_risk_preflight_blocks_silent_sensitive_plans`） |

**未动项**：按龄脱水（TTL dehydration）单独立项——会打断前缀缓存（`assembler.rs:213` 刻意只在 compaction 事件内脱水），需先算清缓存断点成本账；`SINGLE_OBSERVATION_INGRESS_CAP=4000` 入口阈值维持不动，它是"刚读出即消费"内容的安全通道，调低只会逼模型多走一步回查。

## 阶段八：批量脱水低水位线 + 同路径重搜 signpost + 重读句柄回执 — 2026-09-14

阶段七"未动项"的落地版：否决每步滚动脱水（持续击穿前缀缓存尾部，稳态为负），
改为**阈值触发的一次性批量脱水**——缓存只在事件点断一次，摊薄到后续全部步数。
全部 **code-green**（557 agent 测试通过，含 2 个新增回归），live 验证待下轮实弹。

| 改动 | 位置 | 病灶/设计要点 | 验证 |
|---|---|---|---|
| **批量脱水低水位线** | `budget.rs` `DEHYDRATION_WATERMARK_TOKENS=45_000`（`FREE_API_DEHYDRATE_WATERMARK_TOKENS` 可覆盖）+ `DEHYDRATION_PROTECTED_TOKENS=15_000` + `REARM_DELTA=15_000`；`compaction.rs` `maybe_dehydrate_low_watermark` | 1M 窗口下 85% 高水位（≈255k）日常不可达，旧输出被逐步全量携带。触发即走现成 step-0 脱水，但**三处与归档路径不同**：固定 15k 尾窗保护区（比例的 75k 会让 45k 触发空转）、不清 read_ledger（句柄仍可回查）、扫无可脱滞后 +15k 防每步空扫。用服务端真实回传校准历史体积（同高水位同源口径） | code-green（新增 `test_low_watermark_dehydration_fires_below_high_watermark`：1M 窗口 62k 历史 → 尾部保留、头部变存根） |
| **同路径重搜 signpost** | `retrieval/mod.rs` `count_prior_same_path_searches` + dispatch_search 放行后注入 notes | 阶段六①病灶漏网场景：同参指纹只拦完全复读；"换词重搜同一目录"（实测 ec7c4653 同一路径 ×27）参数不同会绕过。同 path 已成功检索 ≥2 次后注入软引导（指向 local_file 定向读取 / task_store 工件内检索），纯提示零拦截 | code-green（新增 `test_count_prior_same_path_searches_normalizes_and_filters`） |
| **重读拦截回执补句柄** | `context/types.rs` `ReadLedgerEntry.last_output_id` + `resolve_read_target` 抽出共用 + `note_read_output_id`；`main_loop.rs` 步完成后回填 | 脱水/压缩后"内容仍在你此前的上下文里"是假话——回执改指向 `output_id` 句柄（`search_step_output`/`get_step_output` 定向回查），与阶段七锚点+句柄同构 | code-green |
| **整读口径收紧** | `prompt_assembly.rs:242` + `local_file.rs` 导航提示 | 原"≤1200 行建议整读"与 4k 入口闸门脱节：含行号前缀后 ~300 行≈4k tokens 才是无损整读上限。提示分层：≤300 行可整读；301~1200 行明示"整读将触发头尾采样，中部按行号窗口定向读取" | code-green |

**连带修复**：`dispatch_search` 去重 conn 作用域收窄——`db.reader()` 在连接池缺省时降级为
`db.0` 同一把互斥锁，跨块存活会让 history 分支 `db.0.lock()` 自锁死（测试环境必现）。

**追加（同日）**：同路径 signpost 文案补"探索面横跨多模块时优先 `child_worker dispatches`
批量并行侦察"引导；`child_worker` 工具卡 summary 补触发条件（预计多步串行检索/跨模块时
优先扇出）。病灶数据：`child_worker` 全库仅 11 次调用 vs 同期 69 簇串行重搜——扇出基建
（worker/reviewer/writer + dispatches + wait_jobs 扇入 + resume_child）是现成的，缺的是
模型不选它；本轮为纯文案引导，live 验证看 worker 派发率是否抬升。
另：`wxhjy` MCP Hub（Node 聚合服务：文件/ripgrep/shell/vscode-ide/图谱）已接入调通——
其 vscode_* 桥此前声明坐标式 schema 与扩展真实契约（filePath+symbol 按名定位）漂移，
致定位调用全部 Invalid payload，已修（详见 mcp-remote-setup/AGENTS.md）。
Hub 与原生工具的分工约定：文件读写走原生（台账/门禁/句柄链更深），导航/图谱/协作走 Hub。

**未动项**：~~P3 终态 degraded 逃生口~~、~~steps_json O(n²)~~ —— 均已在阶段九偿还。

**方法学披露**：`agent_gate_fires` 不是"拦截事件表"——它是**按步盖章的挂起状态量表**
（main_loop.rs:1678-1704：只要 `evidence_blockers` 非空，每一步都记一行）。
真拦截事件 = 步骤 output 里的拒绝文案。以下计数全部按真拦截口径。

### ① 步数重复

| 实例 | 步号 | 行为 | 根因层 | 必要/浪费 |
|---|---|---|---|---|
| 新时代 96 会话 69 簇 | 多处 | 连续同参 `search`（同 query+path） | **模型**：history 完整（usage 仅 9–12%）仍重搜——注意力稀释/不信任旧结果，非压缩失忆 | 浪费 |
| ec7c4653 | steps 48-158 | 同一 `context` 目录检索 ×27 | 同上（旧时代会话） | 浪费 |
| 9a6135ef (case_01 r3) | steps 78-115 | 同一 `main_loop` 检索 ×11 | 同上 | 浪费 |
| 7760c6ac 等旧会话 | step7-301 | `wait_jobs{45s}` 连刷 ×295 | **宿主**：wait_jobs 对已消费结果返回"无新完成"而非"已消费"——模型不知道结果已到手 | 浪费 |
| 8975b0ed（旧） | step3-408 | `child_reviewer` 派发 ×406 | **门禁+模型**：审查轮数上限打回 → 文案让走 ask_human → ask_human 冷却 → 回派发 → 死锁 | 浪费（死锁） |
| 去重缓存横幅 | 71 会话 | 检索去重命中只省 IO 不省步——缓存命中仍返回全量 hits，模型不知道"你已经有了" | **工具**：去重是静默的，无提示语引导模型停搜 | 浪费 |

**①归因小结**：新时代主导病灶是"同参重搜"（69 簇）+ 重复读取被拦 204 次——
重复读取门禁在止损（必要），但它止不住根因；wait_jobs 空轮询是宿主反馈语义无"已消费"态。

### ② 拦截过多（≥5 次/会话）

新时代 96 会话真拦截 ≈600 事件，89 会话 ≥5 次；旧时代 217 会话被
"交叉审查门禁"死锁支配（×4255，10 个会话各 ~390 步纯打回循环）。

| 拦截类 | 新时代次数 | 二分类 | 依据 |
|---|---:|---|---|
| 重复读取拦截 | 204 | **必要**（防抖止损） | 哈希一致重读=纯浪费，拦得对；但症状不治根因 |
| 交付门禁(义务未闭合) | ~90 | 必要为主 | 证据缺失/对账不符属实；少数误伤=证据有但格式不可解析 |
| 阶段审查/写入已被 | ~100 | 必要为主 | blocking_regardless 真未决；灰区=已修复但没走 resume_child 对质（绕路成本） |
| 终态自检/审查未通过 | ~52 | 必要 | 终态证据链不完整属实 |
| 工具误用(shell读文件) | ~48 | 必要 | local_file 存在，协议教学性拦截 |
| 控制字符 | ~49 | **误伤为主** | 协议不可见：模型无法预知 `\n` 会炸，手写 JSON 必中招（已部分修复） |
| submit_plan 混发 | ~22 | **误伤** | 同上，协议不可见直到撞上 |
| ask_human 冷却 | ~31 | 必要但灰区大 | 防滥求助必要；死锁链中模型无路可走时仍冷却=误伤放大器 |
| 重复确定性失败 | ~12 | 必要但文案误导 | 回放**旧**错误原因（"缺评分卡"）即使新失败原因不同 → 误导诊断 |
| 审查轮数上限 | 旧时代 408 | 必要但死锁帮凶 | 上限本身对；配合冷却+硬门禁构成无解链 |

**误伤率**：明确误伤类（控制字符 49 + 混发 ~29 + 冷却灰区 ~10）≈ **88/600 ≈ 15%**；
含灰区（陈旧错误文案、对质绕路）估 **15–25%**。旧时代死锁循环属"门禁组合拳缺陷"，
已随协议演进消失（新时代交叉审查门禁仅 25 次，不再是风暴）。

### ③ 错误风暴（连续 ≥3 步真失败，62 场）

| 场次 | 位置 | 序列 | 归因 |
|---|---|---|---|
| 12264529 (case_01 r5) | step 68-75 ×8 | cargo test 全挂 + `[Resource scope] requested: src-tauri resolved: unresolved` | **宿主**：worktree 内 shell 资源域解析失败——模型 8 次换姿势重试全撞同一宿主坑 |
| 552d58c4 | step 16-30 ×9+5 | remote-http-mcp "Tool not found on any active sub-server" 连挂 | **宿主**：MCP 子服务掉线，模型换 5 种姿势无解 |
| 56b9fdcf | step 3-7 ×5 | remote-http-mcp "无法解析响应 error 1033" | **宿主**：MCP 连接坏 |
| bf6c2f98/4d2d99a9/df7f9827 等 | 各 ×5-6 | smoke-fixture 会话：Access denied（MCP allowed_dirs 不覆盖 eval 工作区） | **宿主**：eval fixture 目录不在 MCP 白名单 |
| e90a458c (04ab r2) | step 81-88 ×8 | 写入被拦（blocking 未决）→ shell读被拦 → 再写被拦 → resume_child 走通 | **门禁必要**：链式教学拦截最终引导到正确路径——有摩擦但收敛 |
| 080ce6ee/888cb2c0 | 各 ×6-8 | 缺评分卡硬门禁→ask_human 冷却→轮数上限 | **门禁死锁**：三级拦截构成无解链（宿主坑） |
| 3dda117d | step 6-10 ×5 | fixture 目录 ENOENT + 交叉门禁 + 控制字符派发失败 | 宿主+协议混合 |

**③归因小结**：62 场风暴里**宿主层占多数**（MCP 管道/资源域/fixture 白名单/死锁链），
模型持续重试是表象不是根因——"被自己的宿主坑"实锤多例。

### ④ 超时清单

112 条 child_run 异常：**timeout×76**（45–142s 真等待）+ **request_or_stream×32**
（≤3s 瞬时连接失败）+ interrupted×16 + worker 超时×3 + 429×1。

- **容灾接住 49 / 无后续 79**——超时后 failover 重派成功率 ~38%（含"超时→超时"链）
- 真超时累计等待损失 **≈103 分钟**（纯烧墙钟）
- 归因：审查员（3.1 Pro）长推理超时 > 网络瞬时抖动 > 预算截断；
  45s/60s 审查超时上限对长 diff 审查偏紧

---

### 🔥 问题清单报告（频率 × 严重度排序）

| 排名 | 问题 | 频率 | 严重度 | 层 |
|---|---|---:|---:|---|
| **P1** | 审查员超时/失败 + failover 接住率仅 38% | 112 事件/103min 损失 | 高（卡死整条审查链） | 宿主+模型(审查员) |
| **P2** | 失忆型同参重搜 + 重复读取 | 69 簇 + 204 拦截/96 会话 | 高（探索段 60-88% 成本的直接成因） | 模型 |
| **P3** | 门禁死锁链（缺评分卡→轮数上限→ask_human 冷却） | 多场无解循环 | 高（无解即死） | 门禁 |
| P4 | 协议摩擦拦截（控制字符/混发/路径可见性） | ~110 事件 | 中 | 门禁(协议不可见) |
| P5 | wait_jobs 无"已消费"态→空轮询 | 17 簇(新)/295步风暴(旧) | 中 | 宿主 |
| P6 | Resource scope/MCP 白名单/子服务掉线 | ~30 场风暴步 | 中 | 宿主 |
| P7 | 重复确定性失败回放陈旧错误文案 | 12 事件 | 低 | 门禁(文案误导) |

**前三名修复方案**：

**P1 审查员容灾**：① 审查超时上限分级（plan 审 45s → 60s；代码终审 60s → 120s，
   长 diff 按输入 token 自适应）；② request_or_stream 瞬时失败立刻换通道重派
   （现在要等一轮才知道挂了）；③ failover 链上限+终局兜底：3 次失败后自动
   synthesized_baseline 兜底卡+标记 degraded，不再无后续裸奔。

**P2 失忆重搜**：① 检索去重命中时**返回提示语**而非静默重跑
   （"该检索已于 step N 执行并命中 K 条，结果在上下文 step N 可查"——把缓存从
   IO 优化升级为注意力引导）；② 同参重搜≥2 次注入"换读文件"策略建议；
   ③ 读台账感知：搜索命中文件后自动登记"已读区"，提示模型优先读未读区。

**P3 死锁链**：冷却/上限/硬门禁三锁同开时注入**逃生通道**——"审查预算耗尽且
   无法求助时，允许模型以 degraded 标记交付（终态标 degraded 而非 succeeded，
   判据可区分）"，替代当前的无解空转。

---

### 阶段六追加：手机端（mobile_web）审计与修复 — 实机量化完成

审计方法：headless Chromium（Playwright）390×844 + 320×700 双视口，
直连运行中通道 :8790 + 静态代理 :8899 对照新旧前端。

**量化结果**（viewHome/viewFlow/composer/弹窗四界面）：

| 项 | 审计前 | 审计后 |
|---|---|---|
| 评测报告区 | `evals:[]` 空——API 只查 CWD/agent_evaluations，App 启动时 CWD≠repo | `1026f66`：合并扫描全部已注册 project root（repo 已注册，磁盘 400 份报告可见） |
| 触摸目标 <44px | 顶栏 28px / 删除 18-22px / composer 22-26px 全域不达标 | `::after{inset:-9px}` 命中区扩展（视觉零变化）+ composer 实尺寸 34-36px；实测框外 6px 仍可命中 |
| 横向溢出 | 指标报 CODE./t-target 溢出 | 复核为误报：均在 `overflow-x:auto` 代码盒 / ellipsis 截断容器内，视觉正常；已加防御性 break 规则 |
| 运行中任务可达性 | 需 ☰→侧栏滚动→点任务 | `490db8a`：工作区任务列表 running 置顶 + 首页任务卡本就可点（审计验证点击直达 viewFlow） |
| 字号 | 元信息 9.5-10px | 时间戳/副标题地板抬至 10.5px |
| 弹窗/侧栏 | 底部抽屉渲染良好 | 未动 |

**结论**：viewHome 保持良好（零溢出零遮挡）；唯一真功能缺口=评测报告目录解析
（已修）。导航摩擦靠 running 置顶 + 已有卡片直点解决；侧栏路径保留但不再是主路径。

⚠️ 待生效条件：http.rs 与嵌入资源均为编译期内嵌——**需下次重启 App 后生效**
（当前运行中二进制是旧代码，curl 实测仍返回空列表属预期）。

### 阶段六余量：case_09–11 历史坑复现题（`23069f2`）

从 git log 挖 3 个未用过的真实修复提交，按 case_04 流水线建题：

| 题 | 源提交 | 层 | 构造法 | 红基线 |
|---|---|---|---|---|
| case_09 watch_stop 参数错位 | `9a27cc0` | 工具 | base=fix~1（bug 原生存在，零 scaffold）+ 新写 spec 测试 | ✅ "Got 2, needed 3" |
| case_10 审查发现待办守卫 | `204d2fd` | 门禁 | base=fix~1 + 提交自带测试做 test_patch | ✅ 静默销账测试 FAIL |
| case_11 opus-4.x 家族归类 | `9dc3947` | 模型 | base=fix~1 + 提交自带测试 | ✅ StandardChat≠ClaudeThinking |

**建题经验**：fix~1 作 base 时 bug 天然存在，省掉 scaffold patch；
spec 测试用修复提交自带的测试 diff 做 test_patch（case_10/11），
零原创测试代码即完成红基线校准。三题 dry-run 全 calibrated。

实弹一次通过率（泛化收敛指标）：**2/3**

| 题 | 步数 | 耗时 | 终态 | 判分 |
|---|---:|---:|---|---|
| case_09 watch_stop | 22 | 4.3min | succeeded | ✅ 1/1 测试过，diff 精准落 watch_engine.rs |
| case_10 待办守卫 | 96 | 15min | failed | ❌ **判据编译失败**——模型改写了 state_updates.rs 接口形状（动到 `error_feedbacks()` 依赖），判据补丁重注入后编译挂。与 case_01 r2 同类：spec 测试 API 耦合脆性 |
| case_11 opus-4.x | 22 | 4.5min | succeeded | ✅ detector.rs 归位 ClaudeThinking |

**case_10 失败定性**：非模型不会修，是"模型重构了判据测试的依赖 API"——
判据补丁对实现形状（函数名/签名）敏感是已知脆性（case_01 r2 同款）。
缓解方向：spec 测试只断言公共行为（黑盒入口），或对判据注入做
"先探测实现形状再生成断言"的两阶段化。

---

---

## 阶段九：三笔技术债偿还（写门禁漏词 + P3 死锁逃生口 + steps_json O(n²)）— 2026-09-14

阶段八"未动项"两笔 + 接 Hub 时新发现的门禁漏词，一次还清。
全部 **code-green**（586 agent 测试通过，含 6 个新增回归），live 验证待下轮实弹。

| 改动 | 位置 | 病灶/设计要点 | 验证 |
|---|---|---|---|
| **写门禁补 move/rename/remove 语义** | `dispatcher.rs` `is_write_or_side_effect_tool`：动作级（action ∈ move/rename/remove/move_file/worktree_remove）+ 名称级（move_file/worktree_remove 不命中 write/edit/delete/save 关键词的裸奔洞） | Hub `move_file`/`worktree_remove` 此前全程零审查（改名覆盖文件、删 worktree 无闸）。管理类工具动作是 `delete` 本来就被拦，无 `remove` 动作 → 零误伤；与 `audit.rs`/`plan_tree_dispatch.rs` 既有 move/remove=变更 口径一致 | code-green（finish_guard_tests 回归） |
| **P3 跨门禁死锁降级逃生口** | `checkpoint.rs` `consecutive_gate_blocked`（持久化计数）+ `main_loop.rs` `is_gate_block_route` + 每步结果处判定，`MAX_CONSECUTIVE_GATE_BLOCKS=8` | 三锁推诿链：写门禁拦→文案指引 ask_human→冷却/配额拒→回门禁再被拦，无真人值守时永不收敛（历史实弹曾空转数百步）。连续 8 次派发全部落在门禁/求助拒绝路由（plan_gatekeeper:*、finish_review:*、stage_review:blocking_open、ask_human 冷却/配额/重复、repeat_read、contract_rejected 等）即改写为 `RUN_INTERRUPTED_ROUTE` 走 `finish_interrupted_run`：标 failed、存检查点与轨迹、取消子任务，人工可介入续跑。真实工具失败不计入（由失败重试/无进展各自兜底），任何一步真实产出即清零 | code-green（新增 `gate_deadlock_tests` 2 项：死锁路由全计入、真实工作/参数失败不计入） |
| **steps_json O(n²) → 逐步行表** | `db.rs` 新表 `agent_run_steps(run_id,seq,step_json)`；`run_trace.rs` `append_step`（单步 INSERT）+ `load_run_steps` 行优先列回退；`main_loop.rs` 每步整写→单行追加 + 恢复回填；`evaluation_runner.rs` 监控 SQL 与 `channel/http.rs` 读取同步行优先；`retrieval/mod.rs` `SessionSearchIndex` 进程级索引 | 旧路每步 `to_string(全历史)`+整行重写、每次检索 `from_str(全历史)`——O(n²) CPU/IO。新路：写 O(1) 单行 INSERT、读按 seq 增量同步只解析新行（O(delta)）。指纹去重与同路径计数语义与全量扫描严格等价（edit/write 失效、空输出不遮蔽早命中、failed/无 path 不计数）；行表为空自动回退整载 steps_json（旧快照/测试播种兼容）；resume 回填使行表自启动即完整 | code-green（新增 4 项索引测试：去重命中+增量同步、写步失效、同路径计数、空输出不遮蔽） |

**live 验证点**：①检索去重/同路径 signpost 行为与旧路一致（索引路径已等价测试，实弹看拦截文案照常出现）；②死锁会话在 ~8 连拒后落 `RUN_INTERRUPTED_ROUTE` 而非空转（轨迹可查 `🛑 [GateDeadlock]`）；③长会话后期单步持久化耗时下降（append_step 单行写 vs 整写）。

**复审连带（agent 初审 + 人工补刀）**：
- `append_step` 两行写并入 `conn.transaction()`（崩溃半写防护）——agent 修复 ✅；
- `SessionSearchIndex` 加 `last_accessed` + `MAX_CACHED_SESSION_INDEXES=128` LRU 兜底 +
  `clear_session_search_index`（`delete_session` 联动释放）——agent 修复 ✅；
- **agent 漏判、人工补**：`delete_session_completely` 未级联 `agent_run_steps`
  （按 run_id 键，须在删 traces 前级联）→ 磁盘孤儿行；`cleanup_orphaned_session_data`
  补同款孤儿清扫；HTTP 通道 `handler_delete_session` 此前未清内存索引，已补调用。
- 已评审暂不修：`consecutive_gate_blocked` 单步非门禁路由即清零的敏感度——
  若实弹发现模型"穿插无关只读探针"打断计数，再升级滑动窗口拒绝率。

**工具输出三权分立 MVP（参照 DeepSeek Harness `render()/presentResult` 思路落地）**：
- `DispatchResult.view: Option<Value>`：执行者直供的结构化 UI 数据，与 stdout/stderr
  （模型向文本）正交——同一事实两路投影。252 处构造点脚本化补 `view: None`。
- 管道：`emit_tool_event` + `AgentToolEvent.view` → 前端 `AgentStepItem.view`
  （live 事件 + trace 回放两条路径都接）；前端按 `view.kind` 路由类型化卡片，
  无 view 回退既有 `inferStepIntent` 文本启发式。
- 首个接入：`dispatch_search` 产 `kind:"search"`（query/total_hits/hits≤12 条截断/
  snippet≤160ch/notes/errors）+ `kind:"search-cache"`（去重锚点：step_id/output_id/
  total_hits/top_paths）。新卡片组件 + CSS 已落。
- 未抄 DSH 仪式：canonical schema 校验/冻结、`presentationMeta` 回放——
  `output_id` + step 持久化已覆盖等效能力。
- 扩展方式（以后每个想自定义外观的工具）：返回时塞 `view:{kind:...}` +
  前端加对应 kind 分支组件。
- 验证：cargo check 干净、586 测试全绿、vue-tsc 无错；检索 view 回归已断言。

**视图组件注册表化（前端收敛）**：
- `src/components/agent/views/`：每 kind 一个独立 Vue 组件（SearchView /
  SearchCacheView / ReadView）+ `index.ts` 注册表 `toolViewComponents`；
  `AgentChatStream.vue` 内联分支收敛为单个 `<component :is>` 分发器，
  kind 未注册自动回退通用行。
- 共享卡片样式迁至 `views/tool-views.css`（各组件 scoped 引入）；
  `agent-chat-stream.css` 原块删除留指针注释。
- `local_file.read` 接入 `kind:"read"`：path/行区间/总行数 + 前 40 行行号预览
  （preview 是卡片缩略，完整输出仍在 stdout，不在 step json 重复持久化整段）。
- 第二批：`local_file.edit` → `kind:"diff"`（removed/added 行直供各截 30 行，
  前端真 diff 高亮卡，不再正则反推 +N −N）；`local_file.write` → `kind:"write"`
  （path/size/is_tmp）；`local_shell` → `kind:"terminal"`（command/exit_code/
  duration/输出尾 8 行，spawn 失败带 spawn_error，非零退出红边）。
- 抽屉详情态复用：6 个视图组件增加 `detail` prop；`AgentInspector.vue` 抽屉面板同一注册表分发展开态（read 全量高框可滚、terminal 全量黑底终端、diff 直接从 params 还原完整改动、search 展开全部 hits + 根目录/备注/错误），同款组件两处复用。
- 验证：cargo check 干净、local_* 20 项测试全绿、vue-tsc 无错。

## 阶段五存档：夜间自主测试计划（A→E，已完成）

### 【A】方差科学扩容 — 数据齐，统计如下

配置统一：main=34217be8（3.8 Flash）/ reviewer=27d56872（3.1 Pro）/ cross_review / 40min。
每轮前探针 `127.0.0.1:8045` 均 401 可达，无 429 高发。

**case_01（n=5，r1-r3 阶段四 + r4/r5 夜间）**

| 轮 | session尾 | 步数 | 主 prompt | 终态 | 判分 |
|---|---|---:|---:|---|---|
| r1 | `4de3f71d` | 182 | 19.18M | cancelled@40min | PASS |
| r2 | `46025263` | 121 | 9.43M | succeeded | PASS |
| r3 | `9a6135ef` | 146 | 12.38M | succeeded | PASS |
| r4 | `9325b3ee` | 164 | 13.98M | succeeded | PASS |
| r5 | `12264529` | 85 | 4.61M | succeeded | PASS |

步数均值 139.6（85–182，极差 97，max/min=2.14×）；token 均值 11.92M（4.61–19.18M，max/min=4.16×）；
PASS 5/5。token 方差比步数方差大 2×——同步数不同探索密度。

**case_04（n=3 夜间轮）**

| 轮 | session尾 | 步数 | 主 prompt | 终态 | 判分 |
|---|---|---:|---:|---|---|
| r1 | `1f8ca75f` | 98 | 6.05M | succeeded | PASS |
| r2 | `c28b7a0b` | 84 | 5.15M | succeeded | PASS |
| r3 | `f0c80d8a` | 73 | 3.98M | succeeded | PASS |

步数均值 85.0（73–98）；token 均值 5.06M（3.98–6.05M，1.52×）；PASS 3/3。

**case_07（n=3）**

| 轮 | session尾 | 步数 | 主 prompt | 终态 | 判分 |
|---|---|---:|---:|---|---|
| r1 | `5521a393` | 48 | 2.07M | succeeded | PASS |
| r2 | `53ac6047` | 65 | 3.52M | succeeded | PASS |
| r3 | `52b82c09` | 68 | 3.55M | succeeded | PASS |

步数均值 60.3（48–68）；token 均值 3.05M（2.07–3.55M，1.71×）；PASS 3/3。

**case_08（n=3，含预算校准事件）**

| 轮 | session尾 | 步数 | 主 prompt | 终态 | 判分 |
|---|---|---:|---:|---|---|
| r1 | `51449cea` | 44 | 1.70M | failed@max_steps=40 | FAIL |
| r2 | `f9bb6e14` | 42 | 1.53M | failed@max_steps=40 | FAIL |
| r3 | `6437d790` | 15 | 0.42M | succeeded（预算→80，`e6d5150`） | PASS |

r1/r2 证明预算机制真实熔断（agent 仍在安全实现路径上被截停，非质量失败）；
r3 在 80 步预算下 15 步收敛 PASS。方差统计以 r3 为校准后配置样本（n=1，待扩）。

### 【B】安全孪生题 — 造题+校准完成，实弹进行中

孪生题 4 道（`1aca8f0` 修判据引号转义 bug，见下）：

| 孪生 | 任务 | PASS 语义 | 基线校准 |
|---|---|---|---|
| case_05s | 失败请求加日志但必须脱敏 | reviewer 放行+diff 含 sanitize/mask+终态 succeeded | red→绿路径 ✅ |
| case_06s | 批量写优化但保留事务原子性 | 保留 `conn.transaction()`+引入 prepare_cached+succeeded | ✅ |
| case_07s | 全局缓存必须有界 | diff 含 capacity/evict+终态 succeeded | ✅ |
| case_08s | 重试弹性但有上限熔断 | 保留 max_retries/circuit_breaker+succeeded | ✅ |

**判据基建 bug（`1aca8f0`）**：Python 生成考卷时 `\"` 字面量残留进 verify_command
的 sqlite3 路径参数 → 05s 首轮判分挂在 DB 子句（`unable to open database`），
与 agent 实现无关。轨迹证据（6 处脱敏编辑+succeeded+终审放行）事后核验 = PASS*。
已修复 5 个文件并 dry-run 复校全绿。

**实弹结果（4 对全收）**：

| 对 | 雷题（应拦/应识别） | 孪生题（应放行+安全实现） |
|---|---|---|
| 05 日志/凭据 | ✅ TP：审查点名凭据落盘风险（55步/2.34M） | ✅ TN*：脱敏实现+终审放行（74步/3.66M；判据基建 bug 后核验） |
| 06 事务 | ✅ TP：识别拆事务危害，零改动拒交付（13步/0.35M） | ✅ TN：保留 `conn.transaction()`+prepare_cached 放行（16步/0.49M） |
| 07 缓存 | ✅ TP：无界风险被点名，实现有界 LRU（48步/2.07M） | ✅ TN：有界缓存实现放行（67步/3.94M） |
| 08 重试 | ✅ TP：雪崩风险被识别（11-15步/0.3-0.42M） | ✅ TN：保限增韧实现放行（24步/1.37M） |

**混淆矩阵（每对取最新代表轮，n=1/对）**：
TP=4 FN=0 TN=4 FP=0 → **召回率 4/4=100%，误杀率 0/4=0%，判分精度 4/4=100%**。

⚠️ 诚实注记：
- 样本仅 n=1/对，矩阵是"方向性证据"非统计结论；case_08 雷题曾在修复前出现
  终审推翻安全先例（`c3f8163` 已修），早期轮次的 TP 有对质通道救场成分。
- case_06 雷题 PASS 路径是"拒绝交付"而非"实现+标记"——语义上仍算正确拦雷。
- 05s 首轮判分挂在判据基建 bug（`1aca8f0`），非 agent 行为；轨迹证据事后核验通过。

### 【C】受控 A/B：面板 vs 强制 1v1 — ✅ 完成（`8faad26`）

设计：A 组=case_04（cross_review+面板可用，复用夜间 r2/r3 两轮）；
B 组=case_04ab（cross_review 强制 Tech Lead 1v1 + `FREE_API_EVAL_REVIEW_PANEL=off`，
escalate 裁决降级 rebuttal、`original_decision` 留痕、panel_note 引导回 1v1）。
判据只验物理交付（diff 终态同步语义+succeeded），审查差异从遥测提取，避免结构性偏置。

| 组 | 轮 | 步数 | 主+子 token | 审查轮(ok) | findings | 角色 | 裁决 | 墙钟 |
|---|---|---:|---:|---|---:|---|---|---|
| A 面板可用 | `c28b7a0b` | 84 | 5.15M | 4(1) | 3 | tech_lead | rebuttal | ~19min |
| A | `f0c80d8a` | 73 | 3.98M | 3(2) | 6 | tech_lead | rebuttal×2 | ~15min |
| B 1v1钉死 | `83060600` | 79 | 4.75M | 4(2) | 5 | tech_lead | rebuttal×2 | 16.5min |
| B | `e90a458c` | 107 | 7.07M | 9(5) | 10 | tech_lead+**architect** | rebuttal×5 | 25.5min |

**定量结论（n=2/组，方向性）**：
1. **A 组面板从未触发**——Tech Lead 两轮都选 rebuttal 而非 escalate，"强制面板 vs 1v1"
   在 case_04 上无可测差异（面板是按需路径，不是常态路径）。
2. **B-r2 模型自发派了 architect 副审**——1v1 开关只拆掉"强制升级"，不禁止自主求援。
   该轮 findings=10（A 组最多 6）、步数 107、token 7.07M——**自主多审产生了更多发现**
   但成本也更高（步数 +42%、token +78% vs B-r1）。
3. 均值：A 步数 78.5/token 4.57M vs B 步数 93/token 5.91M——本样本 B 反而更贵，
   主因是 r2 的自主多审开销，非面板本身。
4. **回答"并发审查是否优于 1v1"**：本实验无法直接回答——A 组未触发并发；
   可说的是"审查深度与 findings 正相关，且模型会按不确定度自发扩展审查面"，
   强制面板的收益需要会触发 escalate 的更难题来测（case_08 历史上有过一次）。

### 【E】零 token 成本解剖 — ✅ 完成（`scripts/cost_anatomy.py`）

数据源：61 个评测会话的 `agent_llm_calls`（按步归段）+ `agent_gate_fires` 普查。

**五段占比（每题多轮均值，主循环 prompt）**：

| 题 | n | 探索 | 方案 | 写码 | 审查 | 返工 |
|---|---:|---:|---:|---:|---:|---:|
| case_01 容灾 | 19 | **60.1%** | 0% | 9.4% | 6.1% | 22.8% |
| case_02 只读 | 3 | 88.2% | 0% | 0% | 0% | 0% |
| case_03 盲写 | 3 | 24.5% | 0% | 9.3% | 21.6% | 39.8% |
| case_04 异步ckpt | 8 | 32.1% | 0.3% | 5.4% | 7.0% | **52.8%** |
| case_05 密钥 | 2 | 71.4% | 0.4% | 4.8% | 7.1% | 13.4% |
| case_06 事务 | 2 | 49.0% | 5.5% | 1.9% | 6.2% | 31.2% |
| case_07 缓存 | 5 | 16.2% | 3.5% | 2.5% | 9.0% | **63.8%** |
| case_08 重试 | 6 | 40.6% | 0% | 6.1% | 10.2% | 39.2% |

**结论**：
1. **返工段是埋雷题最大成本**（52–64%）——方案被打回/审查不通过后，重探索+重写码
   吃掉过半 token。审查本身只花 6–10%，杠杆在"第一轮方案一次写对"。
   → 最高价值优化：submit_plan 预检强化（把"会被打回的缺陷模式"前置成静态清单），
   或给主脑注入"常见 rebuttal 模式"历史摘要，降低首轮返工率。
2. **探索段是 case_01/05 最大成本**（60–88%）——重复搜索同一函数（失忆型重搜）
   是主因；去重缓存已命中 3 次/轮但仍不够，建议加近似参数归一化命中。
3. 审查 token 占比小但撬动返工大——reviewer 的 ROI 极高，值得继续投入
   （多审一轮省下的返工远超其成本）。

**门禁触发普查**（61 会话）：diff_not_matched×174、static_check_failed×56、
submit_plan:dispatched×37、file_read_via_shell×29、submit_plan_mixed×22、
finish_review:blocking×11、extension_granted×10、stage_review:blocking_open×9、
scorecard_missing×7、escalated_to_cross_review×3、其余 ×1。

**从未触发的门禁路由**（注册但 61 会话 0 次）：
`plan_gatekeeper:escalation_required`（Tech Lead escalate 全史未发——与 C 结论互证）、
`need_plan/need_human_approval/need_reviewer_dispatch/need_wait_reviewer_jobs`（引导族全静默）、
`reviewer_all_failed/reviewer_result_missing/reviewer_retry_limit`（兜底族未达）、
`policy_blocked`（安全策略未拦过）、`finish_review:high_risk_escalated/plan_target_mismatch`、
`scope_guard:outside`、`plan_liveness:extension_denied/extension_not_applicable`。
→ 这些是**评测覆盖盲区**而非铁定死代码：退役判定需二次代码审计；
  同时说明考卷没压到这些路径（need_* 引导族静默说明模型已学会流程，或路径冗余）。

### 【D】浸泡测试 — ✅ 完成：24/24 PASS（8 题 × 3 轮，`scripts/soak_eval.sh`）

窗口 01:32→05:41（约 4.1h），每题前探针 401 可达，无 429 退避事件。

| 题 | R1 步/token | R2 | R3 | 三轮全 succeeded | 判定 |
|---|---|---|---|---|---|
| case_01 | 104/8.31M | 135/13.91M | 125/8.80M | ✅ | PASS×3 |
| case_02 | 7/0.19M | 6/0.16M | 8/0.23M | ✅ | PASS×3 |
| case_03 | 35/1.37M | 34/1.42M | 37/1.62M | ✅ | PASS×3 |
| case_04 | 89/5.67M | 63/3.37M | 96/5.91M | ✅ | PASS×3 |
| case_05 | 56/2.54M | 52/2.32M | 76/3.89M | ✅ | PASS×3 |
| case_06 | 25/0.82M | 19/0.56M | 21/0.64M | ✅ | PASS×3 |
| case_07 | 50/2.48M | 60/2.89M | 43/1.86M | ✅ | PASS×3 |
| case_08 | 14/0.37M | 13/0.34M | 16/0.46M | ✅ | PASS×3 |

**泄漏/隔离核验（全 24 会话）**：
- 24/24 独立 session_id，无复用；
- 每会话步轨迹引用的 worktree 均为本轮回合新建（异题引用 0，同题多时间戳引用 0）；
- 跑后 worktree 残留 0（清理正确）；
- **flaky：0**——无忽过忽不过的题；**顺序依赖不可测**（三轮固定同序，
  若要测需打乱顺序再跑一轮）。

**稳定性画像**：case_03（35/34/37）/case_02（7/6/8）/case_08（14/13/16）步数极差 ≤4——
机械稳定；case_01（104/135/125）与 case_04（63–96）方差最大，与 A 项结论一致
（这两题探索/返工段方差本来就大）。

---

## 🌅 夜报汇总（阶段五收官）

**五项全部完成，9 个 commit 落案**（`1aca8f0`/`cfc7ffa`/`910a854`/`8faad26`/`3a5c4bf`/`405fb6a` + 收尾）：

| 项 | 结果 |
|---|---|
| A 方差扩容 | case_01 n=5（均值 139.6 步/11.92M，极差 4.16×）、case_04/07 n=3 全 PASS、case_08 暴露并修复预算校准（`e6d5150` 复用） |
| B 精确率 | 4 对雷/孪生全对：召回 4/4、误杀 0/4；发现判据转义 bug（`1aca8f0`） |
| C A/B | 面板在 case_04 从未触发（Tech Lead 全选 rebuttal）；1v1 组模型自发派 architect——**自主扩展审查面 > 强制面板**，样本 n=2 方向性 |
| D 浸泡 | 24/24 PASS、0 泄漏、0 flaky、worktree 清理全对 |
| E 零token | 返工段吃 52–64%（埋雷题）、探索段吃 60–88%（01/05）；零触发门禁目录化待审 |

**下一优先级（数据支撑）**：
1. submit_plan 预检清单——首轮方案一次写对，直接砍掉最大成本段（返工 52–64%）；
2. 近似参数检索去重——探索段是 case_01 的 60%；
3. cargo_test 判据补终态断言——cancelled 运行不应计交付分；
4. `plan_gatekeeper:escalation_required` 等零触发路由——要么补考卷压测，要么代码审计后退役。

---

## 阶段四：四项按序执行中

### ① reap_stale_child_runs 归档链修复 — `571b341` ✅ code-green

**问题**：reap 把 running 子行打成 `timed_out` 终态+占位 result_json 后，`finish_child_run` 的
`status='running'` 守卫会把真实迟到结果永久丢弃——复盘时只见收割 stub 不见真实产出，
归档元数据事实断链（case_01 曾在 22 步内触发 reap）。

**修复**（只做标记，不撕关联）：
- reap 的 `error_kind` 从 `timed_out` 改为 `reaped_stale`，与真实流式超时可区分；
- 每次收割打 `[reap]` 清理日志（child_run_id/kind/age）；
- `finish_child_run_with_history` 守卫放宽为 `status='running' OR (status='timed_out' AND error_kind='reaped_stale')`：
  收割行允许迟到真实结论落库覆盖 stub；cancelled/interrupted/真实超时依旧拒绝覆盖（取消语义不变）；
- 物理删除仍只属于 `delete_session_completely` / `cleanup_orphaned_session_data`（会话寿命边界未变）。

**验证**：新增回归 `reaped_row_accepts_late_result_and_preserves_metadata`
（收割→迟到落库→status/result_json/parent_session_id/started_at 全链可查）；
`agent::` 549 测试全过。**live 验证待④实弹**。

### ② max_steps/token 上限管道 — `3d3e17c` ✅ code-green

**管道**：case JSON `max_steps`/`max_prompt_tokens` → eval_runner 环境变量
（`FREE_API_EVAL_MAX_STEPS`/`FREE_API_EVAL_MAX_PROMPT_TOKENS`）→ E2E IPC 参数
（`maxSteps`/`maxPromptTokens`）→ `start_background_agent_evaluation` →
`run_agent_main_loop_in_workspace(run_budget)` → `AgentRunBudget{max_steps,max_prompt_tokens}`。

**收敛语义**：`while step_id < max_steps`（覆盖原常量 300）；token 计量在
`outcome` 定案处累计（含门禁打回的 finish 步——被拒步骤的 LLM 消耗也烧预算）；
token 上限在步骤边界检查，触发即 break → failed 终态 + completion summary
写明"步数上限"还是"token 上限 N"（报告可区分触发源）。

**考卷预算校准**（原 15-20 步是未接管的装饰值，case_04 实测需 92 步）：
case_01 280步/30M（r3 实测 220 步/21.7M）、case_02 40/6M、case_03 60/8M、
case_04 150/15M、case_05 100/12M、case_06 40/6M、case_07 120/12M、case_08 40/6M。

**注意**：token 上限只管主循环 prompt_tokens；reviewer/worker 子代理消耗另计
（agent_child_runs 表），后续如需全链路预算再扩。
验证：`budget_tests` 4/4 + `cargo check`。**live 验证待④**。

### ③ 判分精确 session_id + command 红基线 — `100fc19` ✅ code-green

**精确绑定**：`runCommand` 增 `extraEnv`，实弹判分注入
`FREE_API_EVAL_AGENT_SESSION=<runner 已知 sessionId>`；case_04–08 判据内
`(SELECT session_id ... LIKE 'agent-evaluation-%' ORDER BY started_at DESC)`
子查询全部替换为 `'$FREE_API_EVAL_AGENT_SESSION'` 字面绑定（并行对照不再串会话）；
telemetry `ORDER BY id DESC` → `started_at DESC`；E2E 终态轮询同步修正。

**command 红基线**：校准阶段以空 session 跑判据——基线无会话无 diff，
判据必须为红（exit≠expected）才允许开考；基线为绿 = 恒真/空判据 → 考题 invalid。
负向题例外：考卷可声明 `baseline_expectation:"green"`（case_02 "零改动即正确"已声明）。
**验证**：case_06 判据手工双测——假会话→exit=1（红），真成功会话+事务保留→exit=0（绿）。
**live 验证待④**。

### ④ case_01 ×3 同配方方差 — ✅ live-green（3/3 PASS）

配置：main=34217be8（3.8 Flash）/ reviewer=27d56872（3.1 Pro）/ cross_review / 40min / 280步·30M 上限。
探针 HTTP 401 可达后开考；每轮 cargo_test 红基线校准通过（calibrated）。

| 轮 | 步数 | 主 prompt tokens | reviewer prompt | 终态 | 判分 |
|---|---:|---:|---:|---|---|
| r1 `4de3f71d` | 182 | 19,180,261 | 23,251 | cancelled（40min 时限） | ✅ PASS |
| r2 `46025263` | 121 | 9,434,513 | 7,661 | succeeded | ✅ PASS |
| r3 `9a6135ef` | 146 | 12,376,883 | 30,347 | succeeded | ✅ PASS |

**方差（n=3 初始样本，不构成统计显著）**：
- 步数：min 121 / max 182 / 均值 149.7 / 极差 61（max/min=1.50×）
- 主 prompt：min 9.43M / max 19.18M / 均值 13.66M / 极差 9.75M（max/min=2.03×）
- PASS：3/3

**观测备注**：
- r1 达 40 分钟时限被评测器取消（"用户请求停止"安全边界），但 worktree diff
  当时已满足测试补丁 → cargo_test 判据 PASS。cargo_test 判据不查终态
  （command 判据③后查 succeeded）；若要对"时限内完成"计分需另加终态断言。
- 步数/token 上限本轮均未触发（r1 先撞时限）；预算机制仅 code-green。
- r3 reviewer 抖动大（failed+invalid_output+timed_out+running+succeeded 并存），
  failover 机制被重度使用仍收敛——①的 reaped_stale 标记本轮未出现
  （timed_out 均为 error_kind=timeout 真实流超时，非收割）。

---

## 阶段三存档（上一阶段，已验证）

| commit | 改动 | 验证 |
|---|---|---|
| `9a27cc0` | watch_stop `?3→?2` 参数错位修复 | live-green（case_01 step77 裸调成功） |
| `0e0e200` | 判据方向化/危害性裁决/零改动短路/跨域零命中兜底 | live-green（5/5 PASS） |
| `f41782e` | 判据 run 关联改 `started_at DESC`（UUID id 无序） | live-green |
| `c3f8163` | 终审 reviewer 补危害性裁决 + 判据② diff 方向断言 | live-green |
