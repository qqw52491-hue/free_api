import { extractActiveThoughtLineInfo, type ActiveThoughtLineInfo } from "./diff/stagePreview";
import type { AgentWorkerItem } from "./types";

/**
 * Consume append-only transport chunks. Completed plain-text lines are normalized once;
 * only the unfinished line/markup region is revisited. Full source stays on the worker.
 * A fence/tag may span chunks and lines, so do not finalize a prefix inside that region.
 */
export function createIncrementalThoughtPreview(
    maxLen = 120,
    extract = extractActiveThoughtLineInfo,
) {
    let pending = "";
    let scan = 0;
    let inFence = false;
    let inTag = false;
    let completedLines = 0;
    let lastCompletedText = "";
    let snapshot: ActiveThoughtLineInfo = extract("", maxLen);
    return {
        append(chunk: string): ActiveThoughtLineInfo {
            if (!chunk) return snapshot;
            pending += chunk;
            let safeEnd = 0;
            while (scan < pending.length) {
                const c = pending[scan];
                if (c === "`") {
                    const tail = pending.slice(scan, scan + 3);
                    if (tail === "```") {
                        inFence = !inFence;
                        scan += 3;
                        continue;
                    }
                    // Keep up to two trailing backticks for a split fence delimiter.
                    if (scan + 3 > pending.length && /^`+$/.test(tail)) break;
                }
                if (!inFence) {
                    if (inTag) {
                        if (c === ">") inTag = false;
                    } else if (c === "<") {
                        let next = scan + 1;
                        if (next === pending.length) break;
                        if (pending[next] === "/") {
                            next++;
                            if (next === pending.length) break;
                        }
                        if (/[a-zA-Z0-9_-]/.test(pending[next])) inTag = true;
                    } else if (c === "\n") {
                        safeEnd = scan + 1;
                    }
                }
                scan++;
            }
            if (safeEnd) {
                const done = extract(pending.slice(0, safeEnd), maxLen);
                completedLines += done.totalLines;
                if (done.totalLines) lastCompletedText = done.text;
                pending = pending.slice(safeEnd);
                scan -= safeEnd;
            }
            const active = extract(pending, maxLen);
            const totalLines = completedLines + active.totalLines;
            const text = active.totalLines ? active.text : lastCompletedText || active.text;
            if (snapshot.text !== text || snapshot.lineKey !== totalLines) {
                snapshot = { text, lineKey: totalLines, totalLines };
            }
            return snapshot;
        },
    };
}

/** Stream text updates in place; only a real action/source switch changes the ticker key. */
export function getWorkerLiveLine(worker: AgentWorkerItem): { key: string; text: string } {
    const label = worker.roleLabel || worker.name;
    const preview = worker.livePreview;
    if (preview && preview.totalLines > 0) {
        return {
            key: `${worker.id}-stream-${preview.streamType}`,
            text: `${label}：${preview.text}`,
        };
    }
    const action = (worker.currentStep || "").trim() || "正在后台执行…";
    return {
        key: `${worker.id}-action-${worker.currentTool || ""}-${action}`,
        text: `${label}：${action}`,
    };
}
