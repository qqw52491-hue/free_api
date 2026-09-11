import type { AgentStepItem } from "../types";

export function cleanFilePath(raw: string): string {
    if (!raw) return "";
    let p = String(raw).trim().replace(/^['"`]|['"`]$/g, "");
    p = p.replace(/^file:\/\//, "");
    // 去除尾部的行号标注如 :L1-100, :10-20, #L1-10, :123
    p = p.replace(/[:#]L?\d+(?:-\d+)?$/i, "");
    return p.trim();
}

export function truncateFileName(fileName: string, maxLen = 24): string {
    if (!fileName || fileName.length <= maxLen) return fileName || "";
    const extIdx = fileName.lastIndexOf(".");
    const ext = extIdx > 0 ? fileName.slice(extIdx) : "";
    const nameWithoutExt = extIdx > 0 ? fileName.slice(0, extIdx) : fileName;
    const keepStart = Math.max(6, Math.floor((maxLen - ext.length - 3) * 0.6));
    const keepEnd = Math.max(3, maxLen - ext.length - 3 - keepStart);
    return `${nameWithoutExt.slice(0, keepStart)}…${nameWithoutExt.slice(-keepEnd)}${ext}`;
}

export function isSubAgentTool(tool?: string): boolean {
    if (!tool) return false;
    const t = tool.toLowerCase();
    return (
        t.includes("child_reviewer") ||
        t.includes("child_worker") ||
        t.includes("child_writer") ||
        t.includes("sub_agent") ||
        t.includes("invoke_subagent")
    );
}

export function isWaitJobsTool(tool?: string): boolean {
    if (!tool) return false;
    const t = tool.toLowerCase();
    return t.includes("wait_jobs") || t.includes("wait_job");
}

// -----------------------------------------------------------------------------
// 🧮 通用解析基础设施
// -----------------------------------------------------------------------------

export function tryParseJson(text: string): any {
    if (!text) return undefined;
    const trimmed = text.trim();
    if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return undefined;
    try {
        return JSON.parse(trimmed);
    } catch {
        return undefined;
    }
}

/**
 * 从文本中扫描顶层平衡大括号的 JSON 对象（wait_jobs 输出为「文字说明 + 输出: {json}」形态，
 * 整体 JSON.parse 会失败，必须逐段抽取）。
 */
export function scanEmbeddedJsonObjects(text: string): any[] {
    const results: any[] = [];
    if (!text || text.length > 1_500_000) return results;
    let depth = 0;
    let start = -1;
    let inString = false;
    let escaped = false;
    for (let i = 0; i < text.length; i++) {
        const ch = text[i];
        if (inString) {
            if (escaped) escaped = false;
            else if (ch === "\\") escaped = true;
            else if (ch === '"') inString = false;
            continue;
        }
        if (ch === '"') {
            inString = true;
            continue;
        }
        if (ch === "{") {
            if (depth === 0) start = i;
            depth++;
        } else if (ch === "}") {
            depth--;
            if (depth === 0 && start >= 0) {
                const parsed = tryParseJson(text.slice(start, i + 1));
                if (parsed) results.push(parsed);
                start = -1;
            }
            if (depth < 0) depth = 0;
        }
    }
    return results;
}

/**
 * 把 step.result / rawResult 还原成结构化载荷列表：
 * - 纯 JSON / JSON 数组：直接解析；
 * - BackgroundJobResult 形态（含 result_json 字段）：解包内层；
 * - wait_jobs 的文字报告：扫描内嵌 JSON 对象（child_writer 的 changed_files、
 *   child_reviewer 的 scorecard/evidence_refs、child_worker 的 action_log 都在其中）。
 */
export function extractResultPayloads(raw: unknown): any[] {
    const payloads: any[] = [];
    const pushUnwrapped = (obj: any) => {
        if (!obj || typeof obj !== "object") return;
        const innerRaw = (obj as any).result_json;
        if (typeof innerRaw === "string") {
            const inner = tryParseJson(innerRaw);
            if (inner && typeof inner === "object") {
                pushUnwrapped(inner);
                return;
            }
        }
        payloads.push(obj);
    };
    if (!raw) return payloads;
    if (typeof raw === "string") {
        const whole = tryParseJson(raw);
        if (whole) {
            if (Array.isArray(whole)) whole.forEach(pushUnwrapped);
            else pushUnwrapped(whole);
            return payloads;
        }
        scanEmbeddedJsonObjects(raw).forEach(pushUnwrapped);
        return payloads;
    }
    if (Array.isArray(raw)) {
        raw.forEach(pushUnwrapped);
        return payloads;
    }
    if (typeof raw === "object") pushUnwrapped(raw);
    return payloads;
}

/**
 * 步骤级 memo 指纹：流式渲染期间同一 step 对象会被原地改写（status/result 到达），
 * 因此按字段廉价长度做指纹，指纹不变直接复用解析结果，避免每次渲染重复 JSON.parse。
 */
export function cheapLen(v: any): string {
    if (v === undefined || v === null) return "-";
    if (typeof v === "string") return `s${v.length}`;
    if (typeof v !== "object") return typeof v;
    try {
        let s = "o";
        for (const k of Object.keys(v)) {
            const val = v[k];
            s += `|${k}:${typeof val === "string" ? `s${val.length}` : typeof val}`;
        }
        return s;
    } catch {
        return "o?";
    }
}

export function stepFingerprint(step: AgentStepItem): string {
    return [
        step.id,
        step.status,
        step.tool,
        step.action,
        cheapLen(step.params),
        cheapLen(step.rawParams),
        cheapLen(step.result),
        cheapLen(step.rawResult),
        cheapLen(step.description),
        cheapLen(step.explanation),
    ].join("~");
}
