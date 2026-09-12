# 评测引擎交接清单（2026-09-12 夜）

> 新会话接手实验时先读本文档 + `docs/agent-multi-review-gate-design-review-2026-09.md`。
> 全部状态已入库，无需历史对话。

## 一、系统现状（一句话）

三题套件（gold/silver/bronze）+ 全自动管线（沙盒→红基线校准→实弹→注卷判分→分析）已验证，
冠军配置下 **3/3 PASS**（case_01 连续三轮 PASS），九轮过程数据全在 `agent_evaluations/`。

## 二、冠军配置与常用命令

```bash
# 全套件实弹（主脑 3.8-flash-high + 审查员 3.1-pro-high）
npm run eval:real -- --live --all \
  --model 34217be8-d0a3-45fe-aba5-d62a4d21c486 \
  --reviewer-model 27d56872-9156-4751-bcdd-db9237b2222f --duration 40

# 过程态分析（任意会话）
python3 scripts/eval_analyze.py <session_id>

# 实时监听（5 秒采样，含死循环检测；已兼容 nonce 会话文件名）
python3 /tmp/eval_watch.py        # 若 /tmp 已清，从 git 历史恢复或重写

# 只跑单题
npm run eval:real -- --live --case case_01_subagent_failover --model <uuid> --duration 40
```

模型 UUID（data_v2.db）：3.8-flash-high=`34217be8-...`、3.1-pro-high=`27d56872-...`。
夜间连跑脚本：`/tmp/overnight_eval.sh`（套件 ×3 串行，日志 `/tmp/overnight_eval.log`）。

## 三、已知坑（每条都踩过，勿再踩）

1. **改码后必须重建**：runner 的 `--live` 已内置自动重建（`test:e2e:build`），勿手动绕过；
2. **禁止并行评测**：wdio `embeddedPort: 4445` 写死，双实例抢端口会卡死；
3. **后台任务用 run_in_background**：shell `&` 启动的进程会随父 shell 随机死亡；
4. **pgrep 不可靠**（曾误报 0 进程）：判活用 `ps aux | grep -cE '...'`；
5. **runner 日志在 wdio 阶段静默**（spawnSync 管道缓冲）：失败时已打印输出尾部；
6. **会话文件已 nonce 化**（`eval-session-<case>-<ts>.txt`）：找最新用 glob + mtime；
7. **TaskOutput 长阻塞可能被取消**：取消级联会带走后台任务进程组（曾致一轮被取消）；
8. **prompt 是 include_str! 内嵌 + 运行时双通道**：改提示词后必须重建应用。

## 四、实验路线图（已与用户对齐的顺序）

- **P1 主脑横评（零代码成本）**：同套件换主脑模型 UUID（3.1-pro-high 当主脑、
  claude-opus 系、deepseek），每配置一轮套件，出《主脑横评战报》；
- **P2 混编委员会攒分歧**：先混编（flash+pro 同委员会）攒自然分歧样本；
  若自然分歧不出现，再写"埋雷题"（如：题面诱导用异步批量保存替换同步 checkpoint 持久化，
  好审查员应指出崩溃丢状态与竞态风险）——判据：审查员 findings 是否点名风险类别；
- **P3 长程收敛大题**：最后做（编译预算问题需先解：单测过滤 / sccache）。

## 五、已修复问题清单（防回归，勿 reintroduce）

| 问题 | 修复提交 |
|---|---|
| 证据驳回行号漂移死循环（39 次） | `c7b28b9` diff 地图 + `d59ef3a` 逐文件建议 |
| 判卷补丁上下文撞车 | `2eaf445` --3way + 规格专属测试名 |
| 评测报告门禁统计恒空（键错） | `7fab5f7`/`6234df3` session_id + 双路径守卫 |
| 迁移顺序致应用秒退 | `69b69d1` 索引后置于 ALTER |
| 范围漂移硬拦误伤必要修复 | `7e92f34`/`bf43881`/`cc5d4ae` 降级 + 披露纪律 |
| 原生工具重复读取不拦 | `8e4524f` local_file 纳入重复读取识别 |
| 审查发现被静默丢弃/剔除 | `830d445`+`204d2fd` 转待办 + 监管继承守卫 |
| 联合会审专家卡缺失 | `eeb4761` 预装三专家 |
| 修复类任务拿不到项目地图 | `2b35b2d` Implementation 模式 + 符号地图 |

## 六、关键不变量（改动前自检）

1. 门禁只守物理事实，语义判断交给审查；
2. 控制面改动必须在交付说明中披露（不审批、不静默）；
3. 判分只认物理事实（测试退出码、diff、退出码 ≥1 测试执行），绝不让 LLM 看 diff 打分；
4. `[审查发现]` 待办禁止静默剔除（监管继承守卫）；
5. 每次评测 fresh worktree + 零记忆（考卷不泄题）。
