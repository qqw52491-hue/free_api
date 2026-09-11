/**
 * 🛠️ diffUtils: Agent Diff 计算、操作意图推导与阶段视觉渲染模块出口
 *
 * 为保持高内聚低耦合，已将原 2200+ 行单体文件模块化拆分为：
 * - ./diff/diffTypes: 路径清洗、截断、工具嗅探与 JSON 扫描基础工具
 * - ./diff/diffCore: LCS 差异算法、Unified Diff 统计、Diff 载荷提取与聚合
 * - ./diff/toolOperation: 工具操作意图提取、Badge 徽标与行号格式化
 * - ./diff/stepFileParser: 步骤文件变动（FileChangeChip）与 Worker 工件解析
 * - ./diff/stagePreview: 阶段 3 行流光预览、折叠态摘要、耗时/标题/Token 格式化、时间线操作胶囊
 */

export * from "./diff/diffTypes";
export * from "./diff/diffCore";
export * from "./diff/toolOperation";
export * from "./diff/stepFileParser";
export * from "./diff/stagePreview";
