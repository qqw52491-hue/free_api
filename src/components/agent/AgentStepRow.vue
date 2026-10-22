<template>
    <div
        class="ambient-text-row reusable-step-row"
        :class="[
            `status-${stepData.status}`,
            {
                'is-compact': compact,
                'is-clickable': isClickable,
                'is-warning': stepData.isWarning,
                'intent-first': !!stepData.explanation,
            }
        ]"
        :title="stepData.tooltip"
        @click="handleRowClick"
    >
        <span class="ambient-text-content">
            <span
                class="ambient-action-lead"
                :class="[
                    `lead-${stepData.actionKind}`,
                    { 'clickable has-diff': stepData.hasDiff }
                ]"
                :title="stepData.hasDiff ? '点击查看代码变更 Diff' : undefined"
                @click.stop="handleLeadClick"
            >
                <!-- 1. 动作类型微标 -->
                <span class="action-tag" :class="`action-tag-${stepData.actionKind}`">
                    {{ stepData.tagLabel }}
                </span>

                <!-- 2. 文件图标 (有文件时展示) -->
                <FileIcon
                    v-if="stepData.fileName && stepData.fileName !== '新文件' && stepData.fileName !== '文件内容'"
                    :name="stepData.fileName"
                    :kind="stepData.iconKind || 'code'"
                    :size="13"
                    class="chip-file-icon"
                />

                <!-- 3. 文件名或命令微丸 -->
                <span
                    v-if="stepData.displayTitle"
                    :class="stepData.actionKind === 'shell' ? 'chip-cmd-pill' : 'chip-file-name'"
                    :title="stepData.displayTitle"
                >
                    {{ stepData.displayTitle }}
                </span>

                <!-- 4. 所在目录简写 (如 iam/web/) -->
                <span v-if="stepData.dirName" class="chip-file-dir">
                    {{ stepData.dirName }}
                </span>

                <!-- 5. 行号或耗时微标 (如 #L35-85, 2.9s) -->
                <span v-if="stepData.capsuleBadge" class="chip-line-range">
                    {{ stepData.capsuleBadge }}
                </span>

                <!-- 6. Diff 增删行微标 (+7 -2) -->
                <span v-if="(stepData.additions || 0) > 0" class="chip-diff-add">+{{ stepData.additions }}</span>
                <span v-if="(stepData.deletions || 0) > 0" class="chip-diff-del">−{{ stepData.deletions }}</span>
                <span v-if="stepData.isIntercepted" class="chip-intercepted">已拦截</span>
            </span>

            <!-- 7. 说明文字 (如: 为审计日志注入做准备) -->
            <span v-if="stepData.explanation" class="ambient-text-body">
                {{ stepData.explanation }}
            </span>
        </span>

        <!-- 8. 时间戳 -->
        <time v-if="stepData.clockTime" class="ambient-text-time">{{ stepData.clockTime }}</time>
    </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { AgentStepItem, AgentWorkerStep } from "./types";
import FileIcon from "./FileIcon.vue";

const props = withDefaults(
    defineProps<{
        step: AgentStepItem | AgentWorkerStep;
        compact?: boolean;
    }>(),
    {
        compact: false,
    }
);

const emit = defineEmits<{
    (e: "select", step: AgentStepItem | AgentWorkerStep): void;
    (e: "open-diff", diffPayload: any): void;
}>();

function getDirName(filePath?: string): string {
    if (!filePath) return "";
    const clean = filePath.replace(/\\/g, "/");
    const lastSlash = clean.lastIndexOf("/");
    if (lastSlash <= 0) return "";
    const dir = clean.substring(0, lastSlash);
    const parts = dir.split("/").filter(Boolean);
    if (parts.length === 0) return "";
    if (parts.length >= 2) {
        return parts.slice(-2).join("/") + "/";
    }
    return parts[parts.length - 1] + "/";
}

function formatClock(timestamp?: number): string {
    if (!timestamp) return "";
    const date = new Date(timestamp);
    const h = String(date.getHours()).padStart(2, "0");
    const m = String(date.getMinutes()).padStart(2, "0");
    const s = String(date.getSeconds()).padStart(2, "0");
    return `${h}:${m}:${s}`;
}

const stepData = computed(() => {
    const s = props.step as any;
    const tool = (s.tool || s.action || "").toLowerCase();
    const isWorker = "stepNumber" in s;

    let actionKind: "read" | "edit" | "write" | "shell" | "search" | "general" = "general";
    let tagLabel = "⚡ 操作";
    let displayTitle = "";
    let fileName: string | undefined = s.fileName;
    let filePath: string | undefined = s.filePath || s.path;
    let capsuleBadge: string | undefined = s.lineRange;
    let additions = s.additions;
    let deletions = s.deletions;
    let isIntercepted = !!s.isIntercepted;
    let explanation = s.explanation || s.description || s.title || "";
    let tooltip = "";

    // 1. 判断动作分类
    if (s.badgeLabel === "隔离") {
        actionKind = "general";
        tagLabel = "🛡️ 隔离";
    } else if (s.badgeLabel === "审查") {
        actionKind = "read";
        tagLabel = "📖 审查";
    } else if (s.badgeLabel === "交付") {
        actionKind = "write";
        tagLabel = "⚡ 交付";
    } else if (tool.includes("read") || tool.includes("view") || s.badgeLabel === "读取") {
        actionKind = "read";
        tagLabel = "📖 读取";
    } else if (tool.includes("shell") || tool.includes("bash") || tool.includes("command") || tool.includes("terminal") || s.badgeLabel === "终端") {
        actionKind = "shell";
        tagLabel = "⚡ 终端";
    } else if (tool.includes("search") || tool.includes("grep") || tool.includes("find") || s.badgeLabel === "检索") {
        actionKind = "search";
        tagLabel = "🔍 检索";
    } else if (tool.includes("write") || tool.includes("create") || s.badgeLabel === "新建") {
        actionKind = "write";
        tagLabel = "📄 新建";
    } else if (tool.includes("edit") || tool.includes("patch") || tool.includes("diff") || (additions && additions > 0) || (deletions && deletions > 0)) {
        actionKind = "edit";
        tagLabel = "⚡ 操作";
    } else if (s.badgeLabel) {
        tagLabel = s.badgeLabel.startsWith("⚡") || s.badgeLabel.startsWith("📖") || s.badgeLabel.startsWith("🛡️") ? s.badgeLabel : `⚡ ${s.badgeLabel}`;
    }

    // 2. 参数结构化提取
    if (s.view) {
        if (s.view.kind === "read") {
            actionKind = "read";
            tagLabel = "📖 读取";
            fileName = s.view.path?.split("/").pop();
            filePath = s.view.path;
            if (s.view.start_line) {
                capsuleBadge = `#L${s.view.start_line}-${s.view.end_line || s.view.start_line + 50}`;
            }
        } else if (s.view.kind === "terminal") {
            actionKind = "shell";
            tagLabel = "⚡ 终端";
            displayTitle = s.view.command || "";
            if (s.view.duration_ms) {
                capsuleBadge = `${(s.view.duration_ms / 1000).toFixed(1)}s`;
            }
        } else if (s.view.kind === "diff") {
            actionKind = "edit";
            tagLabel = "⚡ 操作";
            fileName = s.view.path?.split("/").pop();
            filePath = s.view.path;
            additions = s.view.added_lines?.length || additions;
            deletions = s.view.removed_lines?.length || deletions;
        }
    }

    // 3. Worker step 参数提取
    if (isWorker && s.params) {
        const p = s.params;
        if (p.path) {
            fileName = p.path.split("/").pop();
            filePath = p.path;
        }
        if (p.command) {
            displayTitle = p.command;
        }
        if (p.start_line) {
            capsuleBadge = `#L${p.start_line}-${p.end_line || p.start_line + 50}`;
        }
    }

    // 4. 标题与说明精简
    if (!displayTitle && fileName) {
        displayTitle = fileName;
    }
    if (!displayTitle && s.title) {
        displayTitle = s.title;
    }

    if (s.durationMs) {
        capsuleBadge = `${(s.durationMs / 1000).toFixed(1)}s`;
    }

    // 清理 explanation 中与 displayTitle 重复的词汇
    if (explanation && displayTitle && explanation.includes(displayTitle)) {
        explanation = explanation.replace(displayTitle, "").replace(/^[,，、:：\s]+/, "");
    }
    explanation = explanation.replace(/^(读取|修改|执行|查看|添加|更新)\s*/, "");

    const dirName = getDirName(filePath);
    const clockTime = formatClock(s.timestamp || s.startedAt);

    return {
        status: s.status || "succeeded",
        actionKind,
        tagLabel,
        displayTitle,
        fileName,
        filePath,
        dirName,
        capsuleBadge,
        additions,
        deletions,
        isIntercepted,
        explanation,
        clockTime,
        tooltip: tooltip || explanation || displayTitle,
        isWarning: s.status === "failed" || isIntercepted,
        hasDiff: !!(s.diffPayload || (additions && additions > 0) || (deletions && deletions > 0)),
        diffPayload: s.diffPayload,
        iconKind: s.iconKind || (fileName ? "code" : undefined),
    };
});

const isClickable = computed(() => {
    return stepData.value.hasDiff || !!props.step;
});

function handleRowClick() {
    emit("select", props.step);
}

function handleLeadClick() {
    if (stepData.value.hasDiff) {
        emit("open-diff", stepData.value.diffPayload || {
            path: stepData.value.filePath || stepData.value.fileName,
            rawDiff: "",
        });
        return;
    }
    emit("select", props.step);
}
</script>

<style scoped>
/* 🌟 严格复刻图 2 左侧的高密度环境感知动作行 */
.ambient-text-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    width: 100%;
    margin: 0;
    padding: 2px 6px;
    min-height: 22px;
    border-radius: 4px;
    font-size: 11px;
    line-height: 1.4;
    color: #94a3b8;
    background: transparent;
    transition: background 0.12s ease;
    user-select: none;
    overflow: hidden;
    white-space: nowrap;
    box-sizing: border-box;
}

.ambient-text-row:hover {
    background: rgba(255, 255, 255, 0.04);
}

.ambient-text-row.is-clickable {
    cursor: pointer;
}

.ambient-text-row.is-warning {
    border-left: 2px solid #ef4444;
    background: rgba(239, 68, 68, 0.05);
}

.ambient-text-content {
    display: flex;
    align-items: center;
    flex-wrap: nowrap;
    gap: 6px;
    min-width: 0;
    flex: 1;
    overflow: hidden;
    white-space: nowrap;
}

.ambient-action-lead {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    line-height: 1.35;
    flex-shrink: 0;
    max-width: 65%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: none;
}

.ambient-action-lead.clickable {
    cursor: pointer;
}

/* 动作类型微标 (超低视觉负担，柔和小字号) */
.action-tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10.5px;
    font-weight: 500;
    padding: 0.5px 5px;
    border-radius: 3.5px;
    line-height: 1.35;
    flex-shrink: 0;
    letter-spacing: 0.1px;
}

.action-tag.action-tag-edit {
    color: #38bdf8;
    background: rgba(56, 189, 248, 0.1);
}

.action-tag.action-tag-write {
    color: #34d399;
    background: rgba(52, 211, 153, 0.1);
}

.action-tag.action-tag-read {
    color: #cbd5e1;
    background: rgba(255, 255, 255, 0.06);
}

.action-tag.action-tag-shell {
    color: #fbbf24;
    background: rgba(245, 158, 11, 0.12);
}

.action-tag.action-tag-search {
    color: #cbd5e1;
    background: rgba(255, 255, 255, 0.06);
}

.action-tag.action-tag-general {
    color: #94a3b8;
    background: rgba(255, 255, 255, 0.04);
}

.chip-file-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
}

/* 文件名 (等宽内敛，悬浮高亮) */
.chip-file-name {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11.5px;
    font-weight: 500;
    color: #cbd5e1;
    letter-spacing: -0.2px;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: color 0.12s ease;
}

.ambient-action-lead.has-diff .chip-file-name:hover {
    color: #38bdf8;
    text-decoration: underline;
    cursor: pointer;
}

/* 终端命令微丸 */
.chip-cmd-pill {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11.5px;
    color: #fef08a;
    background: rgba(255, 255, 255, 0.05);
    padding: 0.5px 5px;
    border-radius: 3px;
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

/* 所在目录简写 (如 iam/web/，极度轻柔) */
.chip-file-dir {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11px;
    color: #64748b;
    margin-left: -2px;
    letter-spacing: -0.2px;
}

/* 行号与耗时微标 (#L70-120, 0.5s) */
.chip-line-range {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10.5px;
    font-weight: 500;
    color: #94a3b8;
    background: rgba(255, 255, 255, 0.05);
    padding: 0.5px 4px;
    border-radius: 3px;
    line-height: 1.25;
}

/* Diff 增删行微标 (+22 -163，鲜明优雅) */
.chip-diff-add {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    color: #34d399;
    font-weight: 500;
    font-size: 11px;
}

.chip-diff-del {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    color: #f87171;
    font-weight: 500;
    font-size: 11px;
}

.chip-intercepted {
    font-size: 10px;
    font-weight: 600;
    color: #f87171;
    background: rgba(239, 68, 68, 0.15);
    border-radius: 3px;
    padding: 0 4px;
}

/* 说明文字 (如: 为审计日志注入做准备) */
.ambient-text-body {
    color: #94a3b8;
    font-size: 11.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex: 1;
    margin-left: 4px;
}

/* 右侧时钟时间戳 */
.ambient-text-time {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 10.5px;
    color: #64748b;
    flex-shrink: 0;
    margin-left: auto;
    padding-left: 8px;
}
.intent-first { align-items: flex-start; padding-top: 4px; padding-bottom: 4px; }
.intent-first > .ambient-text-content { flex-direction: column; align-items: stretch; gap: 2px; }
.intent-first > .ambient-text-content > .ambient-text-body {
    order: -1; flex: none; margin: 0; color: #cbd5e1; white-space: normal;
    display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 2; overflow: hidden;
}
.intent-first > .ambient-text-content > .ambient-action-lead { max-width: 100%; width: fit-content; opacity: .8; }
</style>
