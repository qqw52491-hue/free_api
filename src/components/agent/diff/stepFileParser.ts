import type { AgentStepItem, FileChangeChip, ReviewerVerdict } from "../types";
import { parseWorkerArtifacts } from "../workerUtils";
import { cleanFilePath, truncateFileName, stepFingerprint, isSubAgentTool, isWaitJobsTool, extractResultPayloads, tryParseJson } from "./diffTypes";
import { extractDiffFromStep, statsFromUnifiedDiff, isInterceptedStep, isStepWithDiff } from "./diffCore";
import { formatStepOperation } from "./toolOperation";

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

export function computeParseStepFileChanges(step: AgentStepItem): ParsedStepFiles {
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
    // 1.5 pipeline/parallel 批量命令：params.commands[]|params.parallel[] 内嵌
    //     local_file 等子命令，逐条按嵌套 action 语义解析（edit/write/delete/read），
    //     否则批量写盘在托盘全隐形
    // -------------------------------------------------------------
    if (t === "pipeline" || t === "parallel" || t.includes("pipeline") || t.includes("parallel")) {
        let p: any = step.params ?? step.rawParams;
        if (typeof p === "string") {
            try { p = JSON.parse(p); } catch { p = {}; }
        }
        const commands = p && typeof p === "object"
            ? (Array.isArray(p.commands) ? p.commands : (Array.isArray(p.parallel) ? p.parallel : []))
            : [];
        // pipeline 熔断语义：route=pipeline_fail_at_K / "第 K 步崩溃"——
        // K 之前的命令已真实落地，第 K 步未落地，K 之后从未执行（不算尝试）
        const stepFailed = step.status === "failed" || step.status === "challenged";
        let crashIdx = -1;
        if (stepFailed) {
            const text = `${step.result ?? ""}${step.rawResult ?? ""}`;
            const m = text.match(/第\s*(\d+)\s*步崩溃/) || text.match(/pipeline_fail_at_(\d+)/);
            if (m) crashIdx = parseInt(m[1], 10);
        }
        for (let ci = 0; ci < commands.length; ci++) {
            const entry = commands[ci];
            const ord = ci + 1;
            if (stepFailed && crashIdx > 0 && ord > crashIdx) continue;
            const cmdLanded = !stepFailed || (crashIdx > 0 ? ord < crashIdx : false);
            const cmd = entry && typeof entry === "object" ? entry.command : undefined;
            if (!cmd || typeof cmd !== "object") continue;
            const action = String(cmd.action || "").toLowerCase();
            const path = cleanFilePath(String(cmd.path || cmd.file || cmd.target_file || ""));
            if (!path || path.includes("{")) continue;
            if (action === "read" || action === "view") {
                pushRead(path);
                continue;
            }
            if (action === "edit" || action === "write" || action === "create" || action === "delete" || action === "remove") {
                const newContent = typeof cmd.replacement_content === "string" ? cmd.replacement_content
                    : (typeof cmd.content === "string" ? cmd.content : (typeof cmd.CodeContent === "string" ? cmd.CodeContent : undefined));
                const oldContent = typeof cmd.target_content === "string" ? cmd.target_content : undefined;
                const fileName = path.split("/").pop() || path;
                pushChip({
                    path,
                    fileName,
                    displayLabel: truncateFileName(fileName),
                    actionKind: (action === "delete" || action === "remove") ? "delete"
                        : ((action === "write" || action === "create") ? (oldContent ? "modify" : "create") : "modify"),
                    additions: newContent ? newContent.split("\n").length : 0,
                    deletions: oldContent ? oldContent.split("\n").length : 0,
                    oldContent,
                    newContent,
                    stepId: step.id,
                    isIntercepted: !cmdLanded,
                });
            }
        }
        if (res.chips.length > 0) res.chip = res.chips[0];
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
        // 动作语义判定：local_file 等工具的真实动作在 step.action/command.action
        // （write=全量写、edit=局部改、delete=删除），工具名本身不含语义
        let actionVerb = (step.action || "").toLowerCase();
        const p: any = step.params ?? step.rawParams;
        if (p && typeof p === "object" && typeof p.action === "string") {
            actionVerb = actionVerb || p.action.toLowerCase();
        }
        let actionKind: "modify" | "create" | "delete" = "modify";
        if (t.includes("write") || t.includes("create") || actionVerb === "write" || actionVerb === "create") {
            // 全量写：无旧内容=新建，有旧内容=覆写仍算修改
            actionKind = diff.oldContent ? "modify" : "create";
        } else if (t.includes("delete") || t.includes("remove") || t === "rm" || actionVerb === "delete" || actionVerb === "remove") {
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
            // 写入未落地即视为拦截态：门禁挑战/执行失败的写步不产生真实修改
            isIntercepted: diff.isIntercepted || step.status === "failed" || step.status === "challenged",
        };
        res.chip = chip;
        res.chips = [chip];
        return res;
    }

    // 交付产物文档 (artifact_manager 创建/登记文档)
    if (t.includes("artifact")) {
        let p: any = step.params || step.rawParams;
        if (typeof p === "string") {
            try { p = JSON.parse(p); } catch { p = {}; }
        }
        if (p && typeof p === "object") {
            const rawTitle = String(p.title || p.path || p.name || "").trim();
            if (rawTitle) {
                const cleanPath = cleanFilePath(rawTitle);
                const fileName = cleanPath.split("/").pop() || cleanPath;
                const content = typeof p.content === "string" ? p.content : "";
                const chip: FileChangeChip = {
                    path: cleanPath,
                    fileName,
                    displayLabel: truncateFileName(fileName),
                    actionKind: "create",
                    additions: content ? content.split("\n").length : 1,
                    deletions: 0,
                    newContent: content || undefined,
                    stepId: step.id,
                    isIntercepted: isInterceptedStep(step),
                };
                res.chip = chip;
                res.chips = [chip];
                return res;
            }
        }
    }

    // 搜代码 (排除全网搜索与网页抓取)
    if (
        !t.includes("web_search") &&
        !t.includes("web_fetch") &&
        op.badgeLabel !== "全网搜索" &&
        op.badgeLabel !== "抓取网页" &&
        (op.badgeLabel === "检索" || op.badgeLabel === "搜代码" || op.badgeLabel === "日志检索" || op.badgeLabel === "联邦检索" || t.includes("grep") || t.includes("search") || t.includes("find") || t.includes("ripgrep"))
    ) {
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
