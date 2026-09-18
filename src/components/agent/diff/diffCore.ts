import type { AgentStepItem, FileChangeChip, FileDiffPayload } from "../types";
import { cleanFilePath } from "./diffTypes";

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
    const tool = (step.tool || "").toLowerCase();
    const action = (step.action || "").toLowerCase();
    if (
        tool.includes("edit") ||
        tool.includes("replace") ||
        tool.includes("write") ||
        tool.includes("patch") ||
        tool.includes("diff") ||
        tool === "write_to_file" ||
        tool === "replace_file_content" ||
        tool === "multi_replace_file_content" ||
        action.includes("edit") ||
        action.includes("write") ||
        action.includes("replace") ||
        (tool.includes("local_file") && (action.startsWith("write") || action.startsWith("edit")))
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
            if (
                p.TargetContent !== undefined ||
                p.ReplacementContent !== undefined ||
                p.target_content !== undefined ||
                p.replacement_content !== undefined ||
                p.targetContent !== undefined ||
                p.replacementContent !== undefined ||
                p.ReplacementChunks ||
                p.CodeContent ||
                p.content !== undefined ||
                p.rawDiff
            ) {
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

    const tool = (step.tool || "").toLowerCase();
    const action = (step.action || params.action || "").toLowerCase();
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

    // 1. Tool: replace_file_content / local_file edit (精准局部替换，兼容驼峰与蛇形)
    const targetContent = params.TargetContent ?? params.target_content ?? params.targetContent;
    const replacementContent = params.ReplacementContent ?? params.replacement_content ?? params.replacementContent;
    if (targetContent !== undefined || replacementContent !== undefined) {
        const oldContent = typeof targetContent === "string" ? targetContent : "";
        const newContent = typeof replacementContent === "string" ? replacementContent : "";
        const stats = computeLineDiffStats(oldContent, newContent);
        const startLine = Number(params.StartLine || params.start_line || params.startLine) || 1;

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

    // 5. Tool: write_to_file / write_file / local_file write (新建或重写整个文件)
    if (
        params.CodeContent !== undefined ||
        (params.content !== undefined && (tool.includes("write") || tool.includes("create") || (tool.includes("local_file") && action.startsWith("write"))))
    ) {
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
            // 拦截标记取"且"语义：后续存在真实落地的写入时，此前的拦截记录不再算数
            existing.isIntercepted = Boolean(existing.isIntercepted) && Boolean(chip.isIntercepted);
        }
    }
    return [...map.values()];
}
