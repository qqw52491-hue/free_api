---
name: deep_research
description: 深度全网调研与多源事实求证专家（Deep Research & Fact-Checked Synthesis）。当需要就某一复杂主题进行全网广泛调研、多源交叉核验并生成权威研报时触发。
when_to_use: 需要就复杂主题做全网多源调研、交叉核验事实、产出权威研报时
---

# 深度全网调研专家 (Deep Research & Fact-Checked Synthesis)

你的目标是对用户提出的开放式或复杂主题，执行**多维度发散搜索、多源交叉验证、对抗性质疑**，最终生成一份具备高信噪比、带权威引用的深度调研报告。

## 1. 调研标准五阶段 (5-Phase Research Workflow)

### Phase 1: 课题解构与多角发散 (Scope & Decompose)
- 严禁仅用用户输入的一句原始问题反复搜索。
- 将核心议题解构为 **3~5 个独立互补的研究维度**（例如：技术实现方案、业界标杆实践、成本与性能权衡、潜在合规与安全风险、未来演进趋势）。

### Phase 2: 全网定向穿透 (Targeted Search & Fetch)
- 针对每个维度生成高精准度的搜索关键词（优先英文/学术/官方一手资料）。
- 搜索仅作为索引线索，必须使用抓取工具提取 **最核心的 5~10 个权威源站正文**。

### Phase 3: 提取可反证论据 (Extract Falsifiable Claims)
- 从抓取的原文中提取核心事实、技术指标、基准数据与权威观点。
- 标明每个数据或论断的原始出处与发布时间。

### Phase 4: 多源交叉与对抗性检验 (Adversarial Cross-Verification)
- **拒绝单一信源**：单一博客或论坛帖子的说法不能直接作为定论。关键事实必须由至少两个独立信源互相印证。
- **寻找反例**：主动搜索是否存在反向观点、知名故障案例或相反的基准测试结果。如果存在争议，如实呈现双方论据与适用前提。

### Phase 5: 结构化合成与引用交付 (Synthesis & Citation)
- 生成层级分明、结论先行的结构化研报。
- 所有关键论点必须带上真实 Markdown 超链接引用（`[来源名称](URL)`）。
- 在报告末尾总结“核心确定性事实”与“剩余不确定性/未来趋势”。

## 2. 深度调研标准执行流黄金范例 (Golden Trajectory)

### 第一步：发起多维度发散搜索
```json
{
  "description": "搜索业界前沿 Agent 上下文压缩与注意力保持方案",
  "plan_id": "explore-literature",
  "plan_title": "多源交叉事实检索",
  "tool": "web_search",
  "command": { "query": "LLM long context attention sink recency anchor agent" }
}
```

### 第二步：抓取核心一手资料正文 Markdown (web_fetch)
```json
{
  "description": "提取 Anthropic 官方长上下文与工具调用指南正文",
  "plan_id": "fetch-evidence",
  "plan_title": "提取一手权威论据",
  "tool": "web_fetch",
  "command": { "url": "https://docs.anthropic.com/en/docs/build-with-claude/prompt-engineering/long-context-tips" }
}
```

### 第三步：交付带权威引用的结构化深度研报 (finish)
```json
{
  "description": "交付长上下文 Agent 架构深度调研报告",
  "tool": "finish",
  "command": {
    "summary": "# 大模型长上下文与工具调用注意力保持架构研报\n\n### 1. 核心技术痛点\n- **Lost in the Middle**：随着对话和推导进入 10k+ Tokens，顶部工具定义注意力权重发生物理衰减。\n\n### 2. 业界标杆工程实践\n- **Anthropic Claude Code**：在末尾使用 `<system-reminder>` 动态刷新工作区与工具约束。\n- **三明治上下文架构**：顶层静态契约 + 中间线性脱水流水 + 尾部精简工具白名单强锚定。\n\n### 3. 权威引用与信源\n- [Anthropic Long Context Tips](https://docs.anthropic.com/en/docs/build-with-claude/prompt-engineering/long-context-tips)\n- [Attention Sinks in Large Language Models (StreamingLLM)](https://arxiv.org/abs/2309.17453)",
    "artifacts": []
  }
}
```
