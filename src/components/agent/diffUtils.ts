import type { AgentStepItem, FileChangeChip, FileDiffPayload, ReviewerVerdict, RunningPreviewLine1, RunningPreviewLine2, StageActionCapsule, StageActionCapsuleKind, StageRunningPreview } from "./types";
import { parseWorkerArtifacts } from "./workerUtils";

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

function tryParseJson(text: string): any {
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
function scanEmbeddedJsonObjects(text: string): any[] {
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
function cheapLen(v: any): string {
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

function stepFingerprint(step: AgentStepItem): string {
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

/**
 * 基于新旧全文的真实增删行数统计（前缀/后缀裁剪 + LCS，超上限退化为整块替换计数）。
 * 朴素「行数差」会把未变化的行也计成增删，导致徽标与 Diff 视图内容对不上。
 */
export function computeLineDiffStats(oldContent: string | undefined, newContent: string | undefined): { additions: number; deletions: number } {
    const oldLines = oldContent ? oldContent.split("\n") : [];
    const newLines = newContent ? newContent.split("\n") : [];
    if (oldLines.length === 0) return { additions: newLines.length, deletions: 0 };
    if (newLines.length === 0) return { additions: 0, deletions: oldLines.length };

    let prefix = 0;
    while (prefix < oldLines.length && prefix < newLines.length && oldLines[prefix] === newLines[prefix]) prefix++;
    let suffix = 0;
    while (
        suffix < oldLines.length - prefix &&
        suffix < newLines.length - prefix &&
        oldLines[oldLines.length - 1 - suffix] === newLines[newLines.length - 1 - suffix]
    ) {
        suffix++;
    }

    const midOld = oldLines.slice(prefix, oldLines.length - suffix);
    const midNew = newLines.slice(prefix, newLines.length - suffix);
    if (midOld.length === 0) return { additions: midNew.length, deletions: 0 };
    if (midNew.length === 0) return { additions: 0, deletions: midOld.length };
    if (midOld.length * midNew.length > 150000) return { additions: midNew.length, deletions: midOld.length };

    const dp: number[][] = Array.from({ length: midOld.length + 1 }, () => new Array(midNew.length + 1).fill(0));
    for (let i = 1; i <= midOld.length; i++) {
        for (let j = 1; j <= midNew.length; j++) {
            dp[i][j] = midOld[i - 1] === midNew[j - 1] ? dp[i - 1][j - 1] + 1 : Math.max(dp[i - 1][j], dp[i][j - 1]);
        }
    }
    const lcs = dp[midOld.length][midNew.length];
    return { additions: midNew.length - lcs, deletions: midOld.length - lcs };
}

/** 统计 unified diff 文本的真实增删行数（跳过 ---/+++ 头） */
export function statsFromUnifiedDiff(diff: string): { additions: number; deletions: number } {
    let additions = 0;
    let deletions = 0;
    for (const line of diff.split("\n")) {
        if (line.startsWith("+++") || line.startsWith("---")) continue;
        if (line.startsWith("+")) additions++;
        else if (line.startsWith("-")) deletions++;
    }
    return { additions, deletions };
}

/** 该步骤的写入是否被门禁拦截（拦截文案由后端注入 result，均含「拦截」字样） */
export function isInterceptedStep(step: AgentStepItem): boolean {
    const text = `${step.result ?? ""}${step.rawResult ?? ""}`;
    return text.includes("拦截");
}

/**
 * 检查当前步骤是否涉及文件改动或具备可对比的 Diff 数据
 */
export function isStepWithDiff(step: AgentStepItem | null | undefined): boolean {
    if (!step) return false;
    const tool = (step.tool || step.action || "").toLowerCase();
    if (
        tool.includes("edit") ||
        tool.includes("replace") ||
        tool.includes("write") ||
        tool.includes("patch") ||
        tool.includes("diff") ||
        tool === "write_to_file" ||
        tool === "replace_file_content" ||
        tool === "multi_replace_file_content"
    ) {
        return true;
    }
    const params = (step.params || step.rawParams) as any;
    if (params) {
        let p = params;
        if (typeof p === "string") {
            try { p = JSON.parse(p); } catch {}
        }
        if (typeof p === "object" && p !== null) {
            if (p.TargetContent !== undefined || p.ReplacementContent !== undefined || p.ReplacementChunks || p.CodeContent || p.rawDiff) {
                return true;
            }
        }
    }
    return false;
}

/**
 * 从步骤对象中提取完整高精度的 Diff 对比负载
 */
export function extractDiffFromStep(step: AgentStepItem | null | undefined): FileDiffPayload | null {
    if (!step) return null;
    const payload = extractDiffFromStepInner(step);
    if (!payload) return null;
    payload.isIntercepted = isInterceptedStep(step);
    return payload;
}

function extractDiffFromStepInner(step: AgentStepItem): FileDiffPayload | null {
    let params: any = step.params || step.rawParams;
    if (typeof params === "string") {
        try {
            params = JSON.parse(params);
        } catch {
            params = {};
        }
    }
    if (!params || typeof params !== "object") {
        params = {};
    }
    // 解包嵌套的 command / input / arguments
    if (params.command && typeof params.command === "object") {
        params = { ...params, ...params.command };
    }
    if (params.input && typeof params.input === "object") {
        params = { ...params, ...params.input };
    }
    if (params.arguments && typeof params.arguments === "object") {
        params = { ...params, ...params.arguments };
    }

    const tool = (step.tool || step.action || "").toLowerCase();
    let filePath = cleanFilePath(
        params.TargetFile || params.target_file || params.targetFile ||
        params.filePath || params.file_path ||
        params.path || params.file || ""
    );

    if (!filePath && (step.description || step.explanation)) {
        const fileMatch = String(step.description || step.explanation).match(/[\/a-zA-Z0-9_\-.]+\.[a-zA-Z0-9_]+/);
        if (fileMatch) filePath = cleanFilePath(fileMatch[0]);
    }

    const fileName = filePath ? (filePath.split("/").pop() || filePath) : "Modified File";

    // 1. Tool: replace_file_content (单一连续代码块替换)
    if (params.TargetContent !== undefined || params.ReplacementContent !== undefined) {
        const oldContent = typeof params.TargetContent === "string" ? params.TargetContent : "";
        const newContent = typeof params.ReplacementContent === "string" ? params.ReplacementContent : "";
        const stats = computeLineDiffStats(oldContent, newContent);
        const startLine = Number(params.StartLine) || 1;

        return {
            filePath: filePath || "untitled",
            fileName,
            oldContent,
            newContent,
            startLine,
            additions: stats.additions,
            deletions: stats.deletions,
            stepId: step.id,
        };
    }

    // 2. Tool: multi_replace_file_content (多块不连续替换)
    if (Array.isArray(params.ReplacementChunks) && params.ReplacementChunks.length > 0) {
        const oldChunks: string[] = [];
        const newChunks: string[] = [];
        let additions = 0;
        let deletions = 0;
        let firstStartLine = 1;

        params.ReplacementChunks.forEach((chunk: any) => {
            if (oldChunks.length === 0 && chunk.StartLine) firstStartLine = Number(chunk.StartLine) || 1;
            const t = typeof chunk.TargetContent === "string" ? chunk.TargetContent : "";
            const r = typeof chunk.ReplacementContent === "string" ? chunk.ReplacementContent : "";
            const stats = computeLineDiffStats(t, r);
            oldChunks.push(t);
            newChunks.push(r);
            deletions += stats.deletions;
            additions += stats.additions;
        });

        return {
            filePath: filePath || "untitled",
            fileName,
            oldContent: oldChunks.join("\n...\n"),
            newContent: newChunks.join("\n...\n"),
            startLine: firstStartLine,
            additions,
            deletions,
            stepId: step.id,
        };
    }

    // 3. Tool: edit_file (MCP edits 数组)
    if (Array.isArray(params.edits) && params.edits.length > 0) {
        const oldChunks: string[] = [];
        const newChunks: string[] = [];
        let additions = 0;
        let deletions = 0;
        let firstStartLine = Number(params.startLine || params.line_start || 1);

        params.edits.forEach((e: any, idx: number) => {
            if (idx === 0 && (e.startLine || e.line_start)) {
                firstStartLine = Number(e.startLine || e.line_start);
            }
            const o = String(e.oldText ?? e.old_text ?? e.old_str ?? "");
            const n = String(e.newText ?? e.new_text ?? e.new_str ?? "");
            const stats = computeLineDiffStats(o, n);
            oldChunks.push(o);
            newChunks.push(n);
            deletions += stats.deletions;
            additions += stats.additions;
        });

        return {
            filePath: filePath || "untitled",
            fileName,
            oldContent: oldChunks.join("\n...\n"),
            newContent: newChunks.join("\n...\n"),
            startLine: firstStartLine,
            additions,
            deletions,
            stepId: step.id,
        };
    }

    // 4. Tool: old_str / new_str (Cursor/Claude 式精确替换)
    if (params.old_str !== undefined || params.new_str !== undefined || params.oldText !== undefined || params.newText !== undefined) {
        const oldContent = String(params.old_str ?? params.oldText ?? "");
        const newContent = String(params.new_str ?? params.newText ?? "");
        const stats = computeLineDiffStats(oldContent, newContent);
        const startLine = Number(params.startLine || params.line_start || params.StartLine || 1);

        return {
            filePath: filePath || "untitled",
            fileName,
            oldContent,
            newContent,
            startLine,
            additions: stats.additions,
            deletions: stats.deletions,
            stepId: step.id,
        };
    }

    // 5. Tool: write_to_file / write_file (新建或重写整个文件)
    if (params.CodeContent !== undefined || (params.content !== undefined && (tool.includes("write") || tool.includes("create")))) {
        const newContent = String(params.CodeContent ?? params.content ?? "");
        const additions = newContent ? newContent.split("\n").length : 1;

        return {
            filePath: filePath || "untitled",
            fileName,
            oldContent: "",
            newContent,
            startLine: 1,
            additions,
            deletions: 0,
            stepId: step.id,
        };
    }

    // 6. 标准 rawDiff / git diff 格式
    if (params.rawDiff || params.diff || params.patch) {
        const rawDiff = String(params.rawDiff || params.diff || params.patch);
        const stats = statsFromUnifiedDiff(rawDiff);
        return {
            filePath: filePath || "untitled",
            fileName,
            rawDiff,
            additions: stats.additions,
            deletions: stats.deletions,
            stepId: step.id,
        };
    }

    // 7. 参数内显式包含 oldContent 与 newContent
    if (params.oldContent !== undefined || params.newContent !== undefined) {
        const oldContent = String(params.oldContent || "");
        const newContent = String(params.newContent || "");
        const stats = computeLineDiffStats(oldContent, newContent);
        return {
            filePath: filePath || "untitled",
            fileName,
            oldContent,
            newContent,
            startLine: Number(params.startLine || params.line_start || 1),
            additions: stats.additions,
            deletions: stats.deletions,
            stepId: step.id,
        };
    }

    // 8. 如果有文件路径且属于写文件/修改类工具（无内容可用，仅保底展示文件名）
    if (filePath && isStepWithDiff(step)) {
        return {
            filePath,
            fileName,
            oldContent: "",
            newContent: "",
            additions: 0,
            deletions: 0,
            stepId: step.id,
        };
    }

    return null;
}

/**
 * 将文件改动 Chip 转换为 Diff 对比负载
 */
export function diffPayloadFromChip(chip: FileChangeChip): FileDiffPayload {
    return {
        filePath: chip.path,
        fileName: chip.fileName,
        oldContent: chip.oldContent,
        newContent: chip.newContent,
        rawDiff: chip.rawDiff,
        startLine: chip.startLine || 1,
        additions: chip.additions,
        deletions: chip.deletions,
        stepId: chip.stepId,
        isIntercepted: chip.isIntercepted,
    };
}

/**
 * Chip 是否带有可渲染的真实 Diff 内容（没有内容时点击应回落到步骤详情，而不是打开空 Diff）
 */
export function chipHasDiffContent(chip?: FileChangeChip | null): boolean {
    if (!chip) return false;
    return !!(chip.rawDiff?.trim() || chip.oldContent?.trim() || chip.newContent?.trim());
}

export interface StepOperationDisplay {
    badgeIcon: string;
    badgeLabel: string;
    actionText: string;
    filePath?: string;
    fileName?: string;
    lineRange?: string;
    hasDiff: boolean;
    diffStat?: { additions: number; deletions: number };
}

const formatOpMemo = new WeakMap<object, { fp: string; val: StepOperationDisplay }>();

/**
 * 将步骤调用的原始参数提取为结构化、极具可读性的操作意图描述 (例如 "AgentProgressDeck.vue :1-100")
 */
export function formatStepOperation(step: AgentStepItem): StepOperationDisplay {
    if (step && typeof step === "object") {
        const fp = stepFingerprint(step);
        const hit = formatOpMemo.get(step);
        if (hit && hit.fp === fp) return hit.val;
        const val = computeFormatStepOperation(step);
        try { formatOpMemo.set(step, { fp, val }); } catch {}
        return val;
    }
    return computeFormatStepOperation(step);
}

function computeFormatStepOperation(step: AgentStepItem): StepOperationDisplay {
    const rawTool = (step.tool || step.action || "").trim();
    let p: any = step.params || step.rawParams;
    if (typeof p === "string") {
        try { p = JSON.parse(p); } catch { p = {}; }
    }
    if (p && typeof p === "object") {
        if (p.command && typeof p.command === "object") p = { ...p, ...p.command };
        if (p.input && typeof p.input === "object") p = { ...p, ...p.input };
        if (p.arguments && typeof p.arguments === "object") p = { ...p, ...p.arguments };
    } else {
        p = {};
    }

    const rawFilePath = String(
        p.path || p.filePath || p.file_path || p.file || p.TargetFile || p.target_file || p.targetFile || p.fileName || p.filename || ""
    );
    const filePath = cleanFilePath(rawFilePath);

    const fileName = filePath ? (filePath.split("/").pop() || filePath) : "";

    // 提取行号（统一格式化为优雅的 IDE #L 格式，例如 #L1-80, #L1610-1645）
    let lineRange = "";
    if (p.line_start !== undefined && p.line_end !== undefined) {
        lineRange = `#L${p.line_start}-${p.line_end}`;
    } else if (p.start_line !== undefined && p.end_line !== undefined) {
        lineRange = `#L${p.start_line}-${p.end_line}`;
    } else if (p.StartLine !== undefined) {
        lineRange = p.EndLine ? `#L${p.StartLine}-${p.EndLine}` : `#L${p.StartLine}`;
    } else if (p.head !== undefined) {
        lineRange = `#L1-${p.head}`;
    } else if (p.tail !== undefined) {
        lineRange = `tail ${p.tail}`;
    } else if (p.lines !== undefined) {
        lineRange = `#L${p.lines}`;
    } else if (p.offset !== undefined && p.limit !== undefined) {
        lineRange = `#L${p.offset}-${Number(p.offset) + Number(p.limit)}`;
    } else if (Array.isArray(p.edits) && p.edits[0]?.startLine) {
        lineRange = p.edits[0].endLine ? `#L${p.edits[0].startLine}-${p.edits[0].endLine}` : `#L${p.edits[0].startLine}`;
    }

    // 提取增删行数（真实 LCS 统计，与 Diff 视图渲染保持一致）
    let additions = 0;
    let deletions = 0;
    let hasDiff = false;
    if (Array.isArray(p.edits) && p.edits.length > 0) {
        hasDiff = true;
        for (const e of p.edits) {
            const o = String(e.oldText ?? e.old_text ?? e.old_str ?? "");
            const n = String(e.newText ?? e.new_text ?? e.new_str ?? "");
            const stats = computeLineDiffStats(o, n);
            deletions += stats.deletions;
            additions += stats.additions;
        }
    } else if (p.TargetContent !== undefined || p.ReplacementContent !== undefined) {
        hasDiff = true;
        const stats = computeLineDiffStats(
            typeof p.TargetContent === "string" ? p.TargetContent : "",
            typeof p.ReplacementContent === "string" ? p.ReplacementContent : ""
        );
        additions = stats.additions;
        deletions = stats.deletions;
    } else if (p.old_str !== undefined || p.new_str !== undefined || p.oldText !== undefined || p.newText !== undefined) {
        hasDiff = true;
        const stats = computeLineDiffStats(
            String(p.old_str ?? p.oldText ?? ""),
            String(p.new_str ?? p.newText ?? "")
        );
        additions = stats.additions;
        deletions = stats.deletions;
    } else if (p.CodeContent !== undefined || p.content !== undefined) {
        const text = String(p.CodeContent ?? p.content ?? "");
        hasDiff = true;
        additions = text ? text.split("\n").length : 1;
    }

    const t = rawTool.toLowerCase();

    // 1. 读文件
    if (t.includes("read_text") || t.includes("read_file") || t.includes("view_file") || t.includes("cat")) {
        const action = fileName ? `读取 ${fileName}${lineRange ? ` ${lineRange}` : ""}` : "读取文件内容";
        return {
            badgeIcon: "📄",
            badgeLabel: "读取",
            actionText: action,
            filePath,
            fileName,
            lineRange,
            hasDiff: false,
        };
    }

    // 2. 改代码 / 编辑文件
    if (t.includes("edit_file") || t.includes("replace") || t.includes("patch") || t.includes("apply_diff") || t.includes("modify")) {
        let action = fileName ? `修改 ${fileName}${lineRange ? ` ${lineRange}` : ""}` : "修改代码";
        if (additions > 0 || deletions > 0) {
            action += ` (+${additions} -${deletions})`;
        }
        return {
            badgeIcon: "📄",
            badgeLabel: "修改",
            actionText: action,
            filePath,
            fileName,
            lineRange,
            hasDiff: true,
            diffStat: (additions > 0 || deletions > 0) ? { additions, deletions } : undefined,
        };
    }

    // 3. 写文件 / 新建文件
    if (t.includes("write_file") || t.includes("write_to_file") || t.includes("create_file")) {
        const action = fileName ? `写入 ${fileName}${additions > 0 ? ` (+${additions}行)` : ""}` : "写入文件";
        return {
            badgeIcon: "📄",
            badgeLabel: "写入",
            actionText: action,
            filePath,
            fileName,
            lineRange,
            hasDiff: true,
            diffStat: additions > 0 ? { additions, deletions: 0 } : undefined,
        };
    }

    // 4. 搜索代码
    if (t.includes("search") || t.includes("grep") || t.includes("find") || t.includes("ripgrep")) {
        const query = String(p.query || p.pattern || p.regex || p.term || p.Query || "").trim();
        let action = query ? `"${query}"` : "检索代码";
        if (fileName) {
            action += ` in ${fileName}`;
        } else if (p.path || p.dir) {
            const scope = String(p.path || p.dir).split("/").filter(Boolean).slice(-2).join("/");
            if (scope) action += ` (${scope})`;
        }
        return {
            badgeIcon: "🔍",
            badgeLabel: "检索",
            actionText: action,
            filePath,
            fileName,
            hasDiff: false,
        };
    }

    // 5. 终端命令
    if (t.includes("bash") || t.includes("shell") || t.includes("exec") || t.includes("cmd") || t.includes("terminal") || t.includes("run_command")) {
        const cmd = String(p.command || p.cmd || p.CommandLine || "").trim();
        const action = cmd ? `$ ${cmd}` : "执行终端命令";
        return {
            badgeIcon: "⚡",
            badgeLabel: "终端",
            actionText: action,
            hasDiff: false,
        };
    }

    // 6. 查目录
    if (t.includes("directory") || t.includes("tree") || t.includes("list_dir") || t.includes("list_files")) {
        const dir = String(p.dir || p.DirectoryPath || p.path || ".").trim();
        return {
            badgeIcon: "📁",
            badgeLabel: "目录",
            actionText: `查看目录 ${dir}`,
            hasDiff: false,
        };
    }

    // 7. 子智能体 (审查 / 写入 / 协同任务)
    if (t.includes("child_reviewer")) {
        return {
            badgeIcon: "🤖",
            badgeLabel: "子审查",
            actionText: step.action || "执行协同质量审查",
            hasDiff: false,
        };
    }
    if (t.includes("child_writer")) {
        return {
            badgeIcon: "✍️",
            badgeLabel: "子写入",
            actionText: step.action || (fileName ? `子智能体写入 ${fileName}` : "执行沙箱代码写入"),
            filePath,
            fileName,
            hasDiff: hasDiff || !!filePath,
            diffStat: (additions > 0 || deletions > 0) ? { additions, deletions } : undefined,
        };
    }
    if (t.includes("child_worker") || t.includes("sub_agent") || t.includes("invoke_subagent")) {
        return {
            badgeIcon: "🤖",
            badgeLabel: "子代理",
            actionText: step.action || "执行协同子任务",
            hasDiff: false,
        };
    }

    // 8. 挂起等待
    if (t.includes("wait_jobs") || t.includes("wait_job")) {
        return {
            badgeIcon: "⏳",
            badgeLabel: "异步挂起",
            actionText: "等待后台任务完成",
            hasDiff: false,
        };
    }

    // 9. 默认 MCP 工具
    let cleanTool = rawTool;
    if (cleanTool.includes("/")) {
        cleanTool = cleanTool.split("/").pop() || cleanTool;
    }
    const fallbackText = (step.action && step.action !== rawTool) ? step.action : (fileName || "执行操作");
    return {
        badgeIcon: "🧩",
        badgeLabel: cleanTool || "工具",
        actionText: fallbackText,
        filePath,
        fileName,
        hasDiff,
        diffStat: (additions > 0 || deletions > 0) ? { additions, deletions } : undefined,
    };
}

export interface ParsedStepFiles {
    isRead: boolean;
    isSearch: boolean;
    readCount: number;
    searchCount: number;
    chips: FileChangeChip[];
    chip?: FileChangeChip;
    readFiles: string[];
    reviewerVerdict?: ReviewerVerdict;
    subAgentSummary?: string;
}

// worker action_log 的动作分类（后端格式：`action[intent]: {json params}` 或旧版 `action: 裸路径`）
const READ_LOG_ACTIONS = /^(read_file|read_text_file|read_text|view_file|cat_file|cat)$/;
const WRITE_LOG_ACTIONS = /^(edit_file|write_file|write_to_file|replace_file_content|multi_replace_file_content|patch|modify|apply_diff)$/;
const SEARCH_LOG_ACTIONS = /^(grep|ripgrep_search|search|code_search|find)$/;
const RUN_LOG_ACTIONS = /^(run_command|shell_exec|bash|exec)$/;
const READ_COMMAND_RE = /^(?:cat|head|tail|less|nl|bat)\s+/;
const SEARCH_COMMAND_RE = /\b(?:rg|grep|ag|find)\b/;

function parseWorkerActionLogEntry(entry: string): { action: string; params: any; rest: string } | null {
    const match = entry.match(/^([a-zA-Z0-9_/-]+)(?:\[([^\]]*)\])?\s*:\s*([\s\S]+)$/);
    if (!match) return null;
    const rest = match[3].trim();
    return { action: match[1].toLowerCase(), params: tryParseJson(rest) ?? rest, rest };
}

function pathFromLogParams(params: any): string {
    if (params && typeof params === "object" && !Array.isArray(params)) {
        const raw = params.path ?? params.file ?? params.file_path ?? params.target_file ?? params.TargetFile ?? "";
        return typeof raw === "string" ? cleanFilePath(raw) : "";
    }
    return "";
}

/** Reviewer 评分卡 → 放行结论（与 Rust 硬门禁对齐：综合均分 >= 75 且 risk >= 60） */
function reviewerVerdictFromPayload(payload: any, arts: ReturnType<typeof parseWorkerArtifacts>): ReviewerVerdict | null {
    const sc: any = arts.scorecard;
    if (sc && typeof sc === "object") {
        const values = [sc.correctness, sc.risk, sc.style, sc.test_coverage].filter((v: any) => Number.isFinite(v) && v > 0);
        const avg = values.length > 0 ? values.reduce((a: number, b: number) => a + b, 0) / values.length : 0;
        const passes = avg >= 75 && Number(sc.risk) >= 60;
        return { verdict: passes ? "pass" : "fail", score: Math.round(avg) };
    }
    if (payload && typeof payload === "object") {
        if (payload.status === "failed" || payload.error_kind || payload.errorKind) {
            return { verdict: "fail" };
        }
    }
    return null;
}

const parseStepMemo = new WeakMap<object, { fp: string; val: ParsedStepFiles }>();

/**
 * 深度解析步骤中的文件改动，支持主智能体（编辑/写入/替换/阅读）与派发子智能体（审查/写入/侦察）的全部读写操作。
 * 结果按步骤指纹 memo：流式期间同一步骤只在数据变化时重新解析一次。
 */
export function parseStepFileChanges(step: AgentStepItem): ParsedStepFiles {
    if (step && typeof step === "object") {
        const fp = stepFingerprint(step);
        const hit = parseStepMemo.get(step);
        if (hit && hit.fp === fp) return hit.val;
        const val = computeParseStepFileChanges(step);
        try { parseStepMemo.set(step, { fp, val }); } catch {}
        return val;
    }
    return computeParseStepFileChanges(step);
}

function computeParseStepFileChanges(step: AgentStepItem): ParsedStepFiles {
    const res: ParsedStepFiles = {
        isRead: false,
        isSearch: false,
        readCount: 0,
        searchCount: 0,
        chips: [],
        readFiles: [],
    };
    if (!step) return res;

    const pushRead = (path: string) => {
        const cleaned = cleanFilePath(path);
        if (cleaned && !cleaned.includes("{") && !res.readFiles.includes(cleaned)) {
            res.readFiles.push(cleaned);
            res.readCount++;
            res.isRead = true;
        }
    };
    const pushChip = (chip: FileChangeChip) => {
        if (!chip.path && !chip.fileName) return;
        const existing = res.chips.find(c => c.path === chip.path);
        if (!existing) {
            res.chips.push(chip);
        } else {
            // 同文件多次出现：累加行数，内容取有值的一方
            existing.additions += chip.additions || 0;
            existing.deletions += chip.deletions || 0;
            if (!existing.rawDiff && chip.rawDiff) existing.rawDiff = chip.rawDiff;
            if (!existing.newContent && chip.newContent) existing.newContent = chip.newContent;
            if (!existing.oldContent && chip.oldContent) existing.oldContent = chip.oldContent;
            if (existing.actionKind !== "create" && chip.actionKind === "create") existing.actionKind = "create";
            if (chip.isIntercepted) existing.isIntercepted = true;
        }
    };
    const pushWriteChip = (path: string, actionKind: "modify" | "create", params?: any) => {
        const fileName = path.split("/").pop() || path;
        const newContent = params && typeof params === "object"
            ? (typeof params.content === "string" ? params.content : (typeof params.CodeContent === "string" ? params.CodeContent : undefined))
            : undefined;
        pushChip({
            path,
            fileName,
            displayLabel: truncateFileName(fileName),
            actionKind,
            additions: newContent ? newContent.split("\n").length : 0,
            deletions: 0,
            newContent,
            stepId: step.id,
        });
    };

    const rawTool = (step.tool || step.action || "").trim();
    const t = rawTool.toLowerCase();

    // -------------------------------------------------------------
    // 1. 深度解析子智能体与后台挂起执行结果 (child_reviewer, child_writer, child_worker, wait_jobs, sub_agent)
    //    后端真正的结论（changed_files / scorecard / action_log / evidence_refs）只存在于
    //    wait_jobs 的文字报告内嵌 JSON 或子任务回执中，这里统一抽出全部 JSON 载荷再逐个解析。
    // -------------------------------------------------------------
    if (isSubAgentTool(rawTool) || isWaitJobsTool(rawTool)) {
        for (const payload of extractResultPayloads(step.result || step.rawResult)) {
            const arts = parseWorkerArtifacts(payload);

            // A. Reviewer 评分卡结论
            if (!res.reviewerVerdict) {
                const verdict = reviewerVerdictFromPayload(payload, arts);
                if (verdict) res.reviewerVerdict = verdict;
            }

            // B. Evidence Refs (审查员/工作者深入阅读过的证据源码)
            if (Array.isArray(arts.evidenceRefs)) {
                for (const ref of arts.evidenceRefs) {
                    pushRead(ref);
                }
            }

            // C. Action Log (child_worker 执行链: read_file[intent]: {"path":...} / run_command 等)
            if (Array.isArray(arts.actionLog)) {
                for (const logItem of arts.actionLog) {
                    const parsed = parseWorkerActionLogEntry(String(logItem).trim());
                    if (!parsed) continue;
                    if (READ_LOG_ACTIONS.test(parsed.action)) {
                        const path = pathFromLogParams(parsed.params) || (typeof parsed.params === "string" ? cleanFilePath(parsed.params) : "");
                        if (path) pushRead(path);
                    } else if (WRITE_LOG_ACTIONS.test(parsed.action)) {
                        const path = pathFromLogParams(parsed.params) || (typeof parsed.params === "string" ? cleanFilePath(parsed.params) : "");
                        if (path) {
                            pushWriteChip(path, /write|create/.test(parsed.action) ? "create" : "modify", parsed.params);
                        }
                    } else if (SEARCH_LOG_ACTIONS.test(parsed.action)) {
                        res.searchCount++;
                        res.isSearch = true;
                    } else if (RUN_LOG_ACTIONS.test(parsed.action)) {
                        const cmd = parsed.params && typeof parsed.params === "object" ? String(parsed.params.command ?? "") : parsed.rest;
                        if (READ_COMMAND_RE.test(cmd)) {
                            const token = cmd.trim().split(/\s+/)[1];
                            if (token) pushRead(token);
                        } else if (SEARCH_COMMAND_RE.test(cmd)) {
                            res.searchCount++;
                            res.isSearch = true;
                        }
                    }
                }
            }

            // C2. changed_files (writer 在隔离副本上的真实落盘 diff —— 最准确的改动数据)
            const changedFiles = Array.isArray(payload.changed_files)
                ? payload.changed_files
                : (Array.isArray(payload.changedFiles) ? payload.changedFiles : []);
            for (const cf of changedFiles) {
                const path = cleanFilePath(String(cf?.path ?? ""));
                if (!path) continue;
                const rawDiff = typeof cf.diff === "string" ? cf.diff : "";
                const stats = rawDiff ? statsFromUnifiedDiff(rawDiff) : { additions: 0, deletions: 0 };
                const fileName = path.split("/").pop() || path;
                pushChip({
                    path,
                    fileName,
                    displayLabel: truncateFileName(fileName),
                    actionKind: Number(cf.bytes_before) === 0 ? "create" : "modify",
                    additions: stats.additions,
                    deletions: stats.deletions,
                    rawDiff: rawDiff || undefined,
                    stepId: step.id,
                });
            }
        }

        // D. 检查 step.params 中显式声明的目标文件（派发回执阶段尚无结论时的保底展示）
        let p: any = step.params || step.rawParams;
        if (typeof p === "string") {
            try { p = JSON.parse(p); } catch { p = {}; }
        }
        if (p && typeof p === "object") {
            const rawTargets: any[] = [];
            const collectTargets = (v: any) => {
                if (typeof v === "string") rawTargets.push(v);
                else if (Array.isArray(v)) v.forEach(collectTargets);
            };
            collectTargets(p.path);
            collectTargets(p.filePath);
            collectTargets(p.file_path);
            collectTargets(p.target_file);
            collectTargets(p.TargetFile);
            collectTargets(p.file);
            collectTargets(p.focus_paths);
            collectTargets(p.focusPaths);
            const isWriting = t.includes("writer") || t.includes("write");
            for (const rawTarget of rawTargets) {
                const explicitPath = cleanFilePath(rawTarget);
                if (!explicitPath || explicitPath.includes("{")) continue;
                if (isWriting) {
                    pushWriteChip(explicitPath, "modify");
                } else {
                    pushRead(explicitPath);
                }
            }
        }

        if (res.chips.length > 0) {
            res.chip = res.chips[0];
        }
        return res;
    }

    // -------------------------------------------------------------
    // 2. 主 Agent 单步执行检查 (单步工具调用)
    // -------------------------------------------------------------
    const op = formatStepOperation(step);

    // 读文件
    if (op.badgeLabel === "读取" || op.badgeLabel === "读文件" || t.includes("read_text") || t.includes("read_file") || t.includes("view_file") || t.includes("cat")) {
        res.isRead = true;
        res.readCount = 1;
        const p = cleanFilePath(op.filePath || "");
        if (p) res.readFiles.push(p);
        return res;
    }

    // 代码修改与 Diff 解析（先于检索判定，避免 find_and_replace 类工具被误归为搜索）
    const diff = extractDiffFromStep(step);
    if (diff) {
        let actionKind: "modify" | "create" | "delete" = "modify";
        if (t.includes("write") || t.includes("create")) {
            actionKind = "create";
        } else if (t.includes("delete") || t.includes("remove") || t === "rm") {
            actionKind = "delete";
        }
        const cleanPath = cleanFilePath(diff.filePath || "untitled");
        const fileName = diff.fileName ? cleanFilePath(diff.fileName) : "Modified File";
        const chip: FileChangeChip = {
            path: cleanPath,
            fileName,
            displayLabel: truncateFileName(fileName),
            actionKind,
            additions: diff.additions || 0,
            deletions: diff.deletions || 0,
            oldContent: diff.oldContent,
            newContent: diff.newContent,
            rawDiff: diff.rawDiff,
            startLine: diff.startLine,
            stepId: step.id,
            isIntercepted: diff.isIntercepted,
        };
        res.chip = chip;
        res.chips = [chip];
        return res;
    }

    // 搜代码
    if (op.badgeLabel === "检索" || op.badgeLabel === "搜代码" || t.includes("grep") || t.includes("search") || t.includes("find") || t.includes("ripgrep")) {
        res.isSearch = true;
        res.searchCount = 1;
        return res;
    }

    // 兜底：如果属于改写类工具且提取到文件路径，即使 Diff 内容较特殊也保底生成 Chip
    if (isStepWithDiff(step) && op.filePath) {
        const cleanPath = cleanFilePath(op.filePath);
        const fileName = op.fileName || cleanPath.split("/").pop() || cleanPath;
        const chip: FileChangeChip = {
            path: cleanPath,
            fileName,
            displayLabel: truncateFileName(fileName),
            actionKind: (t.includes("write") || t.includes("create")) ? "create" : "modify",
            additions: op.diffStat?.additions || 0,
            deletions: op.diffStat?.deletions || 0,
            stepId: step.id,
            isIntercepted: isInterceptedStep(step),
        };
        res.chip = chip;
        res.chips = [chip];
        return res;
    }

    return res;
}

/**
 * 聚合合并文件 Chips（同一文件的多次增量修改自动累加行数并去重）
 */
export function aggregateFileChips(chips: FileChangeChip[]): FileChangeChip[] {
    const map = new Map<string, FileChangeChip>();
    for (const chip of chips) {
        if (!chip || (!chip.path && !chip.fileName)) continue;
        const key = chip.path || chip.fileName;
        const existing = map.get(key);
        if (!existing) {
            map.set(key, { ...chip });
        } else {
            existing.additions += (chip.additions || 0);
            existing.deletions += (chip.deletions || 0);
            if (existing.actionKind !== "create" && chip.actionKind === "create") {
                existing.actionKind = "create";
            } else if (chip.actionKind === "delete") {
                existing.actionKind = "delete";
            }
            if (chip.newContent) existing.newContent = chip.newContent;
            if (chip.oldContent && !existing.oldContent) existing.oldContent = chip.oldContent;
            if (chip.rawDiff) existing.rawDiff = chip.rawDiff;
            if (chip.stepId) existing.stepId = chip.stepId;
            if (chip.isIntercepted) existing.isIntercepted = true;
        }
    }
    return [...map.values()];
}

/**
 * 格式化阶段标题：统一规范化为 "阶段 N · 标题内容"
 */
export function formatStageTitle(rawTitle: string, stageIndex: number): string {
    if (!rawTitle) return `阶段 ${stageIndex}`;
    // 去除已有的 "阶段 1 · "、"阶段1: "、"步骤 1 · " 等多余前缀，避免双重前缀
    const cleaned = rawTitle
        .replace(/^阶段\s*([0-9一二三四五六七八九十]+|[a-zA-Z]+)\s*[·:：\-\s]\s*/, "")
        .replace(/^步骤\s*([0-9一二三四五六七八九十]+|[a-zA-Z]+)\s*[·:：\-\s]\s*/, "")
        .trim();
    // 防御长文本/多行文本污染：只取首行，折叠多余空白，限制最大长度 40
    const firstLine = (cleaned || rawTitle.trim()).split("\n")[0].trim().replace(/\s+/g, " ");
    const truncated = firstLine.length > 40 ? firstLine.slice(0, 38) + "…" : firstLine;
    const finalTitle = truncated || "执行任务";
    return `阶段 ${stageIndex} · ${finalTitle}`;
}

/**
 * 格式化阶段耗时：14s, 48s, 1m32s, 2m10s
 */
export function formatStageDuration(ms?: number): string {
    if (!ms || ms <= 0) return "";
    const totalSec = Math.round(ms / 1000);
    if (totalSec < 60) return `${Math.max(1, totalSec)}s`;
    const min = Math.floor(totalSec / 60);
    const sec = totalSec % 60;
    return sec > 0 ? `${min}m${sec}s` : `${min}m`;
}

/**
 * 格式化 Token 计数：728 tok, 2.1k tok, 5.4k tok
 */
export function formatTokenCount(n?: number): string {
    if (!n || n <= 0) return "0";
    if (n >= 1000000) return (n / 1000000).toFixed(1) + "M";
    if (n >= 1000) return (n / 1000).toFixed(1) + "k";
    return String(n);
}

export function extractActiveThoughtSentence(raw: string, maxLen = 350): string {
    const fallback = "正在深入分析并规划下一步动作 …";
    if (!raw) return fallback;

    // 去除代码块、XML/HTML 标签、markdown 强调字符等，将换行与连续空白压缩为单空格
    let stripped = raw
        .replace(/```[\s\S]*?```/g, " ")
        .replace(/<[^>]+>/g, " ")
        .replace(/[#*`_>~]/g, "")
        .replace(/\r?\n+/g, " ")
        .replace(/\s+/g, " ")
        .trim();

    if (!stripped) return fallback;

    // 动态 Tail Window：保留足够长（350 字符）的活跃思考流，确保在任何宽屏下都能撑满单行整行并贴紧右侧
    if (stripped.length <= maxLen) {
        return stripped;
    }

    return stripped.slice(-maxLen);
}

/**
 * 将已完成的单步操作格式化为紧凑单行摘要，附带专属图标与耗时（用于前两步记录）
 */
export function formatCompletedStepDigest(step: AgentStepItem): {
    icon: string;
    text: string;
    duration?: string;
    fullThought?: string;
} {
    const duration = step.durationMs ? formatStageDuration(step.durationMs) : undefined;
    const tool = (step.tool || step.action || "").toLowerCase();

    // 0. 门禁拦截
    if (isInterceptedStep(step)) {
        return {
            icon: "🛡️",
            text: "操作已被前置门禁拦截（需审查审批）",
            duration,
            fullThought: step.thought,
        };
    }

    // 1. 文件写/编辑操作
    const op = formatStepOperation(step);
    if (op.fileName) {
        if (tool.includes("write") || tool.includes("edit") || tool.includes("replace") || tool.includes("child_writer")) {
            let diffStat = "";
            const adds = op.diffStat?.additions || 0;
            const dels = op.diffStat?.deletions || 0;
            if (adds > 0 || dels > 0) {
                diffStat = ` (+${adds} -${dels})`;
            }
            return {
                icon: "✏️",
                text: `写入了 ${op.fileName}${diffStat}`,
                duration,
                fullThought: step.thought,
            };
        }
        if (tool.includes("read") || tool.includes("cat") || tool.includes("view")) {
            return {
                icon: "📖",
                text: `读取了 ${op.fileName}${op.lineRange ? ` (${op.lineRange})` : ""}`,
                duration,
                fullThought: step.thought,
            };
        }
    }

    // 2. 搜索操作
    if (tool.includes("search") || tool.includes("grep")) {
        const p = (step.params || step.rawParams) as any;
        let query = "";
        if (p && typeof p === "object") {
            query = String(p.Query || p.query || p.q || "");
        }
        return {
            icon: "🔍",
            text: query ? `检索了: ${query.slice(0, 35)}` : "检索了代码库上下文",
            duration,
            fullThought: step.thought,
        };
    }

    // 3. 审查员 / 子智能体
    if (tool.includes("reviewer") || tool.includes("review") || tool.includes("subagent")) {
        let verdictLabel = "审查完成";
        if (step.result) {
            const rawRes = typeof step.result === "string" ? step.result : JSON.stringify(step.result);
            if (rawRes.includes('"verdict":"passed"') || rawRes.includes("通过") || rawRes.includes("approved")) {
                verdictLabel = "方案审查通过";
            } else if (rawRes.includes("rejected") || rawRes.includes("打回") || rawRes.includes("拦截")) {
                verdictLabel = "审查打回并修正";
            }
        }
        const label = step.description && step.description !== "执行操作"
            ? step.description
            : verdictLabel;
        return {
            icon: "🤖",
            text: label,
            duration,
            fullThought: step.thought,
        };
    }

    // 4. 终端执行
    if (tool.includes("shell") || tool.includes("exec") || tool.includes("command") || tool.includes("bash")) {
        let cmd = "";
        const p = (step.params || step.rawParams) as any;
        if (p && typeof p === "object") {
            cmd = String(p.command || p.cmd || p.CommandLine || "");
            if (Array.isArray(p.args)) {
                cmd += " " + p.args.join(" ");
            }
        }
        let desc = step.description && step.description !== "执行操作" ? step.description : "";
        if (!desc && cmd) {
            desc = cmd.slice(0, 40);
        }
        return {
            icon: "⚡",
            text: desc ? `执行了: ${desc}` : "执行了终端命令",
            duration,
            fullThought: step.thought,
        };
    }

    // 5. 产物管理
    if (tool.includes("artifact")) {
        return {
            icon: "📦",
            text: step.description && step.description !== "执行操作" ? step.description : "生成了交付物文档",
            duration,
            fullThought: step.thought,
        };
    }

    // 6. 有明确 description
    if (step.description && step.description !== "执行操作") {
        return {
            icon: "⚡",
            text: step.description,
            duration,
            fullThought: step.thought,
        };
    }

    // 7. 兜底若有思考
    if (step.thought) {
        return {
            icon: "🧠",
            text: extractActiveThoughtSentence(step.thought),
            duration,
            fullThought: step.thought,
        };
    }

    return {
        icon: "⚡",
        text: "完成了前置操作",
        duration,
        fullThought: step.thought,
    };
}

/**
 * 提取正在执行阶段的三行流光预览 (收拢态直接呈现)
 * Line 1: 前第 2 步已完成记录（清晰掌握前置操作路径）
 * Line 2: 前第 1 步最近完成记录 / 思考结论（紧跟最新进展）
 * Line 3: 正在执行的操作或活跃思考流（带 text-shimmer-flow 流光与快速跳动，极速响应）
 */
export function getRunningStagePreview(
    steps: AgentStepItem[],
    isThinking?: boolean,
    currentThought?: string,
    stageTitle?: string
): StageRunningPreview {
    const succeededSteps = steps.filter((s) => s.status === "succeeded");

    let line1: RunningPreviewLine1;
    let line2: RunningPreviewLine2;

    if (succeededSteps.length >= 2) {
        // 前第 2 步
        const stepA = succeededSteps[succeededSteps.length - 2];
        const digestA = formatCompletedStepDigest(stepA);
        line1 = {
            icon: digestA.icon,
            text: digestA.text,
            duration: digestA.duration,
        };

        // 前第 1 步 (最近完成)
        const stepB = succeededSteps[succeededSteps.length - 1];
        const digestB = formatCompletedStepDigest(stepB);
        line2 = {
            icon: digestB.icon,
            text: digestB.text,
            duration: digestB.duration,
            fullThought: digestB.fullThought,
        };
    } else if (succeededSteps.length === 1) {
        // 只有 1 步完成：Line 1 为环境准备，Line 2 为刚完成的第 1 步
        line1 = {
            icon: "📋",
            text: "准备阶段上下文与工作区环境",
            duration: "0.2s",
        };
        const stepB = succeededSteps[0];
        const digestB = formatCompletedStepDigest(stepB);
        line2 = {
            icon: digestB.icon,
            text: digestB.text,
            duration: digestB.duration,
            fullThought: digestB.fullThought,
        };
    } else {
        // 尚未完成任何步骤：Line 1 为环境准备，Line 2 为任务阶段目标（锚点明确，绝不与 Line 3 的思考内容重复）
        line1 = {
            icon: "📋",
            text: "准备阶段上下文与工作区环境",
            duration: "0.2s",
        };
        const cleanStage = stageTitle
            ? stageTitle.replace(/^阶段\s*\d+\s*[·:：-]\s*/, "").trim()
            : "";
        const targetDesc = cleanStage
            ? (cleanStage.startsWith("目标") || cleanStage.startsWith("规划") ? cleanStage : `目标：${cleanStage}`)
            : "规划：按阶段目标制定实施方案并有序推进";
        line2 = {
            icon: "🎯",
            text: targetDesc,
            fullThought: currentThought,
        };
    }

    // Line 3: 正在执行的当前动作 (刷刷刷的高速流光动画与动态思考)
    const hasLiveStep = steps.some((s) => s.status === "running" || s.status === "pending");
    const runningStep = steps.find((s) => s.status === "running" || s.status === "pending") || steps[steps.length - 1];

    let line3Icon = "⚡";
    let line3Text = "正在执行操作 …";
    let line3Kind: StageRunningPreview["line3"]["actionKind"] = "other";

    const activeThought = currentThought || (runningStep?.thought && (isThinking || runningStep.status === "running") ? runningStep.thought : "");

    if (isThinking || (activeThought && (!hasLiveStep || (runningStep?.status === "running" && !runningStep.tool)))) {
        line3Icon = "💭";
        line3Text = extractActiveThoughtSentence(activeThought);
        line3Kind = "think";
    } else if (runningStep && (runningStep.status === "running" || runningStep.status === "pending")) {
        const tool = (runningStep.tool || runningStep.action || "").toLowerCase();
        let targetFile = "";
        const p = (runningStep.params || runningStep.rawParams) as any;
        if (p && typeof p === "object") {
            targetFile = cleanFilePath(p.TargetFile || p.target_file || p.path || p.file || "");
        }
        if (!targetFile && runningStep.description) {
            const m = runningStep.description.match(/[\/a-zA-Z0-9_\-.]+\.[a-zA-Z0-9_]+/);
            if (m) targetFile = cleanFilePath(m[0]);
        }
        const fileName = targetFile.split("/").pop() || targetFile;

        if (
            tool.includes("write") ||
            tool.includes("edit") ||
            tool.includes("replace") ||
            tool.includes("child_writer")
        ) {
            line3Icon = "✏️";
            line3Text = fileName ? `正在写入 ${fileName} …` : "正在写入代码与文档 …";
            line3Kind = "write";
        } else if (tool.includes("read") || tool.includes("cat") || tool.includes("view")) {
            line3Icon = "📖";
            line3Text = fileName ? `正在读取 ${fileName} …` : "正在读取上下文文件 …";
            line3Kind = "read";
        } else if (tool.includes("search") || tool.includes("grep")) {
            line3Icon = "🔍";
            line3Text = "正在检索相关代码与文档 …";
            line3Kind = "search";
        } else if (tool.includes("child_reviewer") || tool.includes("review")) {
            line3Icon = "🤖";
            line3Text = "子智能体正在审查方案与代码变动 …";
            line3Kind = "review";
        } else if (tool.includes("command") || tool.includes("exec") || tool.includes("shell") || tool.includes("bash")) {
            line3Icon = "⚡";
            line3Text = runningStep.description && runningStep.description !== "执行操作"
                ? `正在执行: ${runningStep.description} …`
                : "正在执行终端命令 …";
            line3Kind = "command";
        } else if (runningStep.description && runningStep.description !== "执行操作") {
            line3Icon = "⚡";
            line3Text = `正在执行: ${runningStep.description} …`;
            line3Kind = "other";
        } else if (runningStep.thought) {
            line3Icon = "💭";
            line3Text = extractActiveThoughtSentence(runningStep.thought);
            line3Kind = "think";
        }
    } else if (isThinking) {
        line3Icon = "💭";
        line3Text = "正在深入分析需求并规划步骤 …";
        line3Kind = "think";
    } else {
        line3Icon = "⚡";
        line3Text = "正在协同推进阶段任务 …";
        line3Kind = "other";
    }

    return {
        line1,
        line2,
        line3: {
            icon: line3Icon,
            text: line3Text,
            actionKind: line3Kind,
        },
    };
}

/**
 * 按执行时间严格正序提取阶段内发生过的所有工具动作胶囊
 * 包含：Excel 表格生成、代码修改/新增(附 Diff)、文档研报、Reviewer 审查结论、方案审查挂起等待、终端执行等
 */
export function extractStageChronologicalCapsules(steps: AgentStepItem[]): StageActionCapsule[] {
    const capsules: StageActionCapsule[] = [];
    const readCapsules: StageActionCapsule[] = [];
    const seenActions = new Set<string>();
    let commandCapsuleCount = 0;

    for (const step of steps) {
        const parsed = parseStepFileChanges(step);

        // 1. 文件改动与产物 (代码修改、Excel 表格、文档研报)
        if (parsed.chips && parsed.chips.length > 0) {
            for (const chip of parsed.chips) {
                const rawName = chip.fileName || chip.path;
                const ext = rawName.split(".").pop()?.toLowerCase() || "";
                const key = (chip.path || chip.fileName).toLowerCase();

                let kind: StageActionCapsuleKind = "code";
                let icon = chip.actionKind === "create" ? "✨" : (chip.actionKind === "delete" ? "🗑️" : "✏️");

                if (["xlsx", "xls", "csv", "ods"].includes(ext) || rawName.toLowerCase().includes("excel")) {
                    kind = "excel";
                    icon = "📊";
                } else if (["md", "markdown", "txt", "pdf", "docx", "doc"].includes(ext)) {
                    kind = "doc";
                    icon = "📝";
                }

                const existing = capsules.find((c) => c.kind === kind && c.id === key);
                if (existing) {
                    existing.additions = (existing.additions || 0) + (chip.additions || 0);
                    existing.deletions = (existing.deletions || 0) + (chip.deletions || 0);
                    if (chip.isIntercepted) existing.isIntercepted = true;
                    if (chip.rawDiff) existing.chip = chip;
                } else {
                    capsules.push({
                        id: key,
                        kind,
                        icon,
                        label: chip.displayLabel || chip.fileName,
                        additions: chip.additions,
                        deletions: chip.deletions,
                        isIntercepted: chip.isIntercepted,
                        stepId: step.id,
                        chip,
                    });
                }
            }
        }

        // 2. 收集读取文件 (带扩展名与文件图标，用于精准识别读取上下文)
        if (parsed.readFiles && parsed.readFiles.length > 0) {
            for (const rf of parsed.readFiles) {
                const rawName = rf.split(/[/\\]/).pop() || rf;
                const readKey = `read-${rawName.toLowerCase()}`;
                if (!seenActions.has(readKey) && !capsules.some((c) => (c.chip?.fileName || c.label).toLowerCase() === rawName.toLowerCase())) {
                    seenActions.add(readKey);
                    readCapsules.push({
                        id: `read-${step.id}-${rawName}`,
                        kind: "read",
                        icon: "📖",
                        label: rawName,
                        stepId: step.id,
                        tooltip: `读取上下文: ${rf}`,
                    });
                }
            }
        }

        // 3. Reviewer 审查结论
        if (parsed.reviewerVerdict) {
            const v = parsed.reviewerVerdict;
            const revKey = `reviewer-${v.verdict}-${v.score || ""}`;
            if (!seenActions.has(revKey)) {
                seenActions.add(revKey);
                capsules.push({
                    id: `reviewer-${step.id}`,
                    kind: "reviewer",
                    icon: "🤖",
                    label: `reviewer ${v.verdict === "pass" ? "✓" : "✕ 拦截"}${v.score ? ` ${v.score}分` : ""}`,
                    verdict: v.verdict,
                    score: v.score,
                    stepId: step.id,
                });
            }
        }

        // 4. 检查未形成标准 diff 的重要产物 (Excel/研报/挂起等待)
        const tool = (step.tool || step.action || "").toLowerCase();
        const desc = (step.description || "").trim();

        const hasExcelMention = /excel|\.xlsx|数据表格/i.test(desc) || /excel|\.xlsx/i.test(tool);
        const hasDocMention = /研报|分析报告|全景分析|\.md/i.test(desc);
        const isWait = isWaitJobsTool(tool) || /等待.*(审查|结论|任务|审批|jobs)/i.test(desc) || desc.includes("等待方案审查");

        if (hasExcelMention && !capsules.some((c) => c.kind === "excel")) {
            let excelName = "Excel 数据表格";
            const m = desc.match(/[\/a-zA-Z0-9_\-\u4e00-\u9fa5]+\.(xlsx|xls|csv)/i);
            if (m) excelName = m[0];
            capsules.push({
                id: `excel-action-${step.id}`,
                kind: "excel",
                icon: "📊",
                label: excelName,
                stepId: step.id,
            });
        }

        if (hasDocMention && !capsules.some((c) => c.kind === "doc") && !capsules.some((c) => c.kind === "code")) {
            let docName = "全景分析研报";
            const m = desc.match(/[\/a-zA-Z0-9_\-\u4e00-\u9fa5]+\.(md|pdf|docx|txt)/i);
            if (m) docName = m[0];
            capsules.push({
                id: `doc-action-${step.id}`,
                kind: "doc",
                icon: "📝",
                label: docName,
                stepId: step.id,
            });
        }

        if (isWait && !capsules.some((c) => c.kind === "wait")) {
            capsules.push({
                id: `wait-${step.id}`,
                kind: "wait",
                icon: "⏳",
                label: desc.includes("结论") ? "等待方案审查结论" : "方案审查挂起等待",
                stepId: step.id,
            });
        }

        // 5. 过滤繁琐 MCP 流水账，仅提取关键的主要命令 (如构建/测试/git)，每个阶段最多展示 1 条
        if (commandCapsuleCount === 0 && ["run_command", "shell_exec", "bash", "exec"].some((t) => tool.includes(t))) {
            const p = (step.params || step.rawParams) as any;
            let cmd = "";
            if (p && typeof p === "object") cmd = String(p.CommandLine || p.command || p.cmd || "");
            const trimmedCmd = cmd.trim();
            // 过滤琐碎内置探活或查询
            const isTrivial = /^(pwd|ls|dir|echo|which|whoami|cat\s+)/i.test(trimmedCmd);
            if (trimmedCmd && !isTrivial) {
                const shortCmd = trimmedCmd.split(/\s+/).slice(0, 3).join(" ");
                const cmdKey = `cmd-${shortCmd}`;
                if (!seenActions.has(cmdKey)) {
                    seenActions.add(cmdKey);
                    commandCapsuleCount++;
                    capsules.push({
                        id: `cmd-${step.id}`,
                        kind: "command",
                        icon: "⚡",
                        label: `$ ${shortCmd}`,
                        stepId: step.id,
                    });
                }
            }
        }
    }

    // 6. 如果写入胶囊较少（< 3个），将核心读取文件胶囊补充展示进来，带专属文件图标
    if (capsules.length < 4 && readCapsules.length > 0) {
        const availableSlots = 4 - capsules.length;
        capsules.push(...readCapsules.slice(0, availableSlots));
    }

    return capsules;
}

