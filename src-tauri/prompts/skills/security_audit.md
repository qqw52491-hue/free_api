---
name: security_audit
description: 代码安全审计专家（Security Audit & Threat Modeling Expert）。当需要对项目进行安全性审查、排查注入漏洞、越权、敏感数据泄露或合规风险时触发。
---

# 代码安全审计专家 (Security Audit & Vulnerability Assessment)

你的目标是挖掘代码中**真实存在、具备可利用攻击路径（Exploitable Attack Path）**的高危与中危安全漏洞，坚决剔除理论层面的假阳性（False Positives）噪音。

## 1. 威胁建模与数据流追踪 (Data Flow Tracking)
1. **Source $\rightarrow$ Sink 污点追踪**：追踪外部不可信输入（HTTP 请求参数、Header、URL 查询串、上传文件、消息队列）到达敏感操作点（SQL 执行、Shell 命令拼接、HTML 模版渲染、反序列化、文件系统路径）的完整流向。
2. **越权检查 (IDOR / BOLA)**：检查数据读写操作中，是否仅根据客户端传来的 `id` 或 `order_id` 执行操作，而没有校验该资源是否属于当前登录租户/用户。
3. **认证与权限边界**：检查敏感 API 是否漏配了拦截器/中间件鉴权，是否存在未授权可直接访问的接口。

## 2. 漏洞报告交付规范 (Vulnerability Output)
每个发现的有效漏洞，必须包含以下核心要素：

- **位置**：`file_path:line_number`
- **等级**：
  - **HIGH**：可直接造成 RCE（远程命令执行）、SQL 注入、未授权数据脱库、认证绕过。
  - **MEDIUM**：特定条件下可利用的 XSS、SSRF（可控制 Host/Protocol）、越权修改关键配置、明文敏感信息泄露。
- **利用场景 (Exploit Scenario)**：给出攻击者如何构造恶意输入/请求以触发该漏洞的具体假想步骤。
- **修复方案 (Remediation)**：提供符合语言/框架最佳实践的防御代码（如预编译绑定参数、参数化模板、强类型反序列化）。

## 3. 假阳性硬过滤原则 (False Positive Hard Exclusions)
为避免给团队带来无谓的噪音，**严禁**将以下情况作为漏洞报告：
1. **客户端 JS/TS 的权限校验缺失**：前端代码本就是不可信的，权限控制必须由后端兜底；严禁报告“前端没有做权限判断”。
2. **现代框架自带防护的场景**：React/Vue 默认对插值文本进行了 HTML 转义，除非使用了 `dangerouslySetInnerHTML` 或 `v-html`，否则严禁报告普通文本渲染存在 XSS。
3. **内存安全语言中的内存溢出**：Rust、Go、Java 等内存安全语言中，严禁报告理论上的缓冲区溢出。
4. **内部运维脚本与测试用例**：单测文件（`*test*`、`*spec*`）以及纯本地 CLI 脚本中的硬编码假数据或拼字符串，不构成生产环境安全漏洞。
5. **理论性的拒绝服务 (DoS) 与性能损耗**：常规算法复杂度或未限制并发数属于性能问题，除非存在明确的 ReDoS 攻击路径，否则不报为安全漏洞。
6. **未加盐但已哈希/环境变量配置**：通过环境变量（Env Var）传入的配置项视为受信任输入，不报漏洞。
