---
name: code_diagnosis
description: 深度故障排查与根因分析（Deep Debug & Root Cause Analysis）。当遭遇运行时报错、空指针异常、内存泄漏、架构层 Bug，或需要顺藤摸瓜定位底层故障时触发。
---

# 深度故障排查与根因分析专家 (Code Diagnosis Expert)

当面临复杂的代码报错或运行异常时，不要盲目通过修改代码来“消除报错”，你的目标是像资深架构师一样，彻底挖出导致偏离的真正原因。

## 1. 核心排错哲学：区分“症状”与“根因”

控制台打印的异常（如 `NullPointerException`, `undefined is not a function`, `500 Internal Error`）通常只是最终的**终端症状 (Terminal Symptom)**，绝不代表真正的发病源。
- **禁止“头痛医头”**：如果某个变量为 null，千万不要直接建议 `if (obj != null)` 绕过，也不要凭空建议“新建一个文件”。
- **必须逆向追溯**：它为什么是 null？它的生命周期是谁管理的？是哪一步上游的拦截器、中间件、依赖注入或配置失效，导致它没有被正确赋值？

## 2. 假设与实证对齐约束 (Exception Alignment)

你提出的任何故障假设，必须在逻辑上与系统抛出的异常类型和语义**严丝合缝**。
- 如果你怀疑是“缺依赖/少打了一个 JAR 包”，程序必然会抛出 `NoClassDefFoundError` 或 `ModuleNotFoundError`。如果实际报错是 `NullPointerException`，那么“缺依赖”的假设就不成立。
- 如果你怀疑是“环境配置连不上数据库”，报错应该是 `Connection Refused` 或超时。如果是 `SQLSyntaxErrorException`，则一定是动态拼接出错，而不是网络问题。
- **验证手段**：如果假设与症状对不上，立刻推翻假设，往更深层思考。

## 3. Web 框架的生命周期防御 (Lifecycle & Proxy Awareness)

在各类后端及依赖注入框架（如 Spring, JFinal, NestJS, FastAPI）中，经常会有大量的“隐形魔法（Magic）”。
当你遇到这类框架的 `Controller` 或 `Service` 莫名其妙发生注入失效、事务未回滚、或内部状态丢失时，优先从以下层面排查：
1. **AOP 与动态代理失效**：目标类是否被代理了？是否用 `private` 访问了代理类的原生字段而导致读取到未初始化的默认值？
2. **拦截器/中间件（Interceptors/Middlewares）吞报错**：上游的数据库报错或参数解析错误，是否被某个全局异常处理（Global Exception Handler）静默捕捉，然后强行转成了导致下游崩溃的假象？
3. **依赖循环与加载时机**：是不是类加载顺序问题导致 bean 还没完成生命周期就被调用？

## 4. 查证步骤规范

1. **查报错上下文**：获取完整的报错堆栈信息，不仅看引发崩溃的最后一行，还要找堆栈里属于当前业务代码的最深层入口（"Caused by"）。
2. **拒绝盲人摸象（Anti-Snippet Blindness）**：用 grep/search 搜出关键词后，**严禁仅凭单行代码下结论**。你必须使用 `read` 工具，至少阅读报错函数及其所属类的完整生命周期声明（前后各20-50行）。
3. **推演前置条件**：找到报错代码行后，问自己：这行代码要成功运行，依赖哪三个前置条件？（比如：网络数据格式正确、缓存已热更新、数据库连接未断开）。然后逐个去排查这三个条件是谁负责保障的，并在哪里脱了节。

## 5. 结论交付标准

向用户汇报排查结论时，必须遵循以下结构：
1. **最终结论（真正根因）**：用一句话说清楚，到底是上游哪一个环节的失效，引发了下游的报错。
2. **证据链还原**：列出你是如何从报错行一步步顺藤摸瓜找到根因的（最好精确到具体文件名和行号）。
3. **副作用评估（反证推演）**：提供修复方案前，说明该方案是否会影响调用方（上游消费端）的正常逻辑。绝不能用破坏调用契约的方式来消除报错。

## 6. 标准执行流黄金范例 (Golden Trajectory)

### 第一步：精准检索报错符号定位触发点
```json
{
  "description": "检索锁巡检服务中 sweepExpiredLocks 方法定义",
  "plan_id": "locate-cause",
  "plan_title": "定位死锁巡检异常触发点",
  "tool": "remote-hub/ripgrep_search",
  "command": { "query": "sweepExpiredLocks", "path": "." }
}
```

### 第二步：带行号安全窗口深入阅读调用方生命周期（前后 30 行）
```json
{
  "description": "阅读 CaseLockService.java 异常点上下文与初始化逻辑",
  "plan_id": "trace-lifecycle",
  "plan_title": "逆向溯源 Db.find 初始化时机",
  "tool": "remote-hub/read_text_file",
  "command": {
    "path": "src/main/java/com/graph/cases/service/CaseLockService.java",
    "start_line": 230,
    "end_line": 280
  }
}
```

### 第三步：交付根因分析与反证修复报告 (finish)
```json
{
  "description": "完成根因溯源与反证推演，交付最终排查报告",
  "tool": "finish",
  "command": {
    "summary": "# 案件锁巡检死锁 NPE 根因分析与修复建议\n\n### 1. 根因分析\n- 触发点位于 `CaseLockService.java:255` 的 `Db.find(...)` 调用。\n- 根因：定时线程由 `ScheduledExecutor` 独立触发，早于 `ActiveRecordPlugin.start()` 完成初始化，导致当前线程未能获取到默认数据源实例。\n\n### 2. 修复建议\n- 在 `CaseLockService.java` 中增加数据源准备就绪守卫（Guard Check），未就绪时跳过本轮巡检并记录 debug 日志。\n\n### 3. 反证与副作用评估\n- 该修复不改变下游锁释放语义，避免了定时任务因未捕获异常而静默终止。",
    "artifacts": ["src/main/java/com/graph/cases/service/CaseLockService.java"]
  }
}
```
