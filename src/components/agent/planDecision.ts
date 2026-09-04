/**
 * 方案审批结论的结构化传输协议。
 *
 * 背景：后端原先靠回复文本的子串嗅探（如 "打回修改"）判断用户是批准还是打回，
 * 一旦用户在「补充执行要求」里写了这些字样再点批准，就会被误判为打回。
 *
 * 方案：前端在回复末尾附一个结构化标记作为权威来源，后端优先解析它。
 * 采用 HTML 注释形态，既不干扰 Markdown 方案正文的渲染，也不会被模型当作指令文本。
 */

const MARKER_PREFIX = "<!--plan-decision:";
const MARKER_SUFFIX = "-->";

/** 在回复末尾附加方案审批结论标记。approved=true 批准放行，false 打回重新推演。 */
export function withPlanDecision(text: string, approved: boolean): string {
  return `${text}\n${MARKER_PREFIX}${JSON.stringify({ approved })}${MARKER_SUFFIX}`;
}

/** 日志与展示层剥离结构化标记，避免把 HTML 注释原样呈现给用户。 */
export function stripPlanDecisionMarker(text: string): string {
  return text.replace(/\n?<!--plan-decision:\s*\{[^}]*\}\s*-->/g, "");
}
