# Shell 进程状态与验证结果的边界

## 事故与原因

迁移任务的构建/重启组合命令中，Maven 编译失败，但 `&& … || … || true; …` 后续命令最终返回 0，运行步骤因此为 succeeded。工具读到的是外层 sh 的真实退出状态，不是 Maven 的独立退出状态。现有 exit_scope=shell_process 及菜单说明已存在，但绿色回执和 recommended_action=done 仍容易被读成验证通过。

## 本次调整

- 保留脚本及 success/succeeded 的进程状态语义，不根据 ERROR/BUILD FAILURE 字符串改判，不静默添加 set -e 或切换 shell。
- 同步成功正文改为中性进程状态，说明未判定编译、测试或业务目标；建议不再是 done。
- 同步结果、终端视图、显式后台和超时转后台的终态结果均带 verification_status=not_assessed 与状态边界说明。终端卡片显示简短范围提示，旧记录无字段时保持兼容。
- 工具说明建议构建与重启拆为独立调用，或捕获关键退出码后显式 exit。提醒 set -e 并不能覆盖 &&/|| 条件列表。

## 正确执行形态

先独立执行 `mvn clean package -DskipTests`，核对退出状态及构建输出；确认构建成功后再决定是否重启。需要截取日志时：

```sh
mvn clean package -DskipTests > build.log 2>&1
rc=$?
tail -n 40 build.log
exit "$rc"
```

该形态保留 Maven 退出码；不等于测试通过（本例明确跳过测试），也不证明 HTTP 或业务行为正确。不要把编译和重启串进错误容忍兜底，更不要用 killall java 作为端口重启兜底。

## 验证与边界

通过真实 local_shell 分发回归：构建失败被 OR-list/末尾命令吞码、普通 ERROR 文本不误判、保留退出码的真实失败与不执行后续步骤、后台终态披露。未新增模型调用、门禁、脚本解释器或业务成功判定。

未重启桌面应用、未改写真实任务记录、未重跑目标工程或连接业务数据库。源码修改需重新编译并重启 free_api 才能在运行中的桌面后端生效；不能保证模型一定正确理解披露。

本地验证：local_shell 定向测试 22 项通过、工具说明测试 1 项通过、cargo check --tests、vue-tsc --noEmit、Vite 生产构建与 git diff --check 通过。保留既有 reviewer_persona 未读赋值及构建 chunk 大小警告；未执行完整 Rust 测试套件或桌面界面交互测试。
