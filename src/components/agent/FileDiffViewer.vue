<template>
    <div class="file-diff-viewer">
        <!-- 顶部标签页与动作栏 (类似 Cursor/VS Code Editor Header) -->
        <header class="diff-header">
            <div class="diff-tabs-bar">
                <div class="diff-active-tab" :title="filePath">
                    <span class="tab-icon">📄</span>
                    <span class="tab-filename">{{ fileName }}</span>
                    <span class="tab-diff-stat">
                        <span v-if="computedAdditions > 0" class="stat-add">+{{ computedAdditions }}</span>
                        <span v-if="computedDeletions > 0" class="stat-del">−{{ computedDeletions }}</span>
                    </span>
                    <button type="button" class="btn-tab-close" title="关闭 Diff 视图" @click="$emit('close')">
                        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                    </button>
                </div>
            </div>

            <div class="diff-header-actions">
                <button
                    type="button"
                    class="btn-diff-action"
                    :title="copied ? '已复制到剪贴板 ✓' : '复制 Diff 内容'"
                    @click="handleCopyDiff"
                >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                    <span>{{ copied ? '已复制' : '复制' }}</span>
                </button>
                <button
                    type="button"
                    class="btn-diff-action"
                    title="切换行内/双栏对比"
                    @click="isSplitView = !isSplitView"
                >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M12 3v18"/></svg>
                    <span>{{ isSplitView ? '单栏视图' : '分栏视图' }}</span>
                </button>
            </div>
        </header>

        <!-- 面包屑导航栏 -->
        <div class="diff-breadcrumb-bar">
            <div class="breadcrumb-path" :title="filePath">
                <span v-for="(seg, idx) in pathSegments" :key="idx" class="breadcrumb-seg">
                    <span class="seg-text" :class="{ 'is-last': idx === pathSegments.length - 1 }">{{ seg }}</span>
                    <span v-if="idx < pathSegments.length - 1" class="seg-divider">/</span>
                </span>
            </div>
            <div class="breadcrumb-meta">
                <span class="diff-hunk-count">{{ diffLines.length }} 行变更细节</span>
            </div>
        </div>

        <!-- 门禁拦截提示：以下为尝试写入的内容，实际未落盘 -->
        <div v-if="intercepted" class="diff-intercepted-banner">
            🚨 该写入已被门禁拦截，以下为尝试写入的内容，实际未落盘
        </div>

        <!-- Diff 代码对比视口 (Unified View) -->
        <div v-if="!isSplitView" class="diff-viewport unified-view">
            <div v-if="diffLines.length === 0" class="diff-empty-state">
                <p>未检测到文件内容差异或文件内容相同</p>
            </div>
            <div v-else class="diff-table">
                <div
                    v-for="(line, idx) in diffLines"
                    :key="idx"
                    class="diff-line-row"
                    :class="[`diff-line-${line.type}`]"
                >
                    <!-- 旧行号 -->
                    <span class="line-gutter line-num-old">{{ line.oldLine ?? '' }}</span>
                    <!-- 新行号 -->
                    <span class="line-gutter line-num-new">{{ line.newLine ?? '' }}</span>
                    <!-- 增删标记 (+ / - / 空格) -->
                    <span class="line-marker">{{ line.type === 'add' ? '+' : (line.type === 'delete' ? '−' : ' ') }}</span>
                    <!-- 代码正文 -->
                    <pre class="line-code">{{ line.content }}</pre>
                </div>
            </div>
        </div>

        <!-- Diff 代码分栏对比视口 (Split Side-by-Side View) -->
        <div v-else class="diff-viewport split-view">
            <div class="split-column left-column">
                <div class="split-col-header">修改前 (Original)</div>
                <div class="diff-table">
                    <div
                        v-for="(pair, idx) in splitPairs"
                        :key="idx"
                        class="diff-line-row"
                        :class="[`diff-line-${pair.left?.type || 'empty'}`]"
                    >
                        <span class="line-gutter">{{ pair.left?.oldLine ?? '' }}</span>
                        <span class="line-marker">{{ pair.left?.type === 'delete' ? '−' : ' ' }}</span>
                        <pre class="line-code">{{ pair.left?.content ?? '' }}</pre>
                    </div>
                </div>
            </div>
            <div class="split-column right-column">
                <div class="split-col-header">修改后 (Modified)</div>
                <div class="diff-table">
                    <div
                        v-for="(pair, idx) in splitPairs"
                        :key="idx"
                        class="diff-line-row"
                        :class="[`diff-line-${pair.right?.type || 'empty'}`]"
                    >
                        <span class="line-gutter">{{ pair.right?.newLine ?? '' }}</span>
                        <span class="line-marker">{{ pair.right?.type === 'add' ? '+' : ' ' }}</span>
                        <pre class="line-code">{{ pair.right?.content ?? '' }}</pre>
                    </div>
                </div>
            </div>
        </div>
    </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';

export interface DiffLine {
    type: 'add' | 'delete' | 'normal';
    oldLine?: number;
    newLine?: number;
    content: string;
}

const props = withDefaults(defineProps<{
    filePath?: string;
    oldContent?: string;
    newContent?: string;
    rawDiff?: string;
    startLine?: number;
    additions?: number;
    deletions?: number;
    intercepted?: boolean;
}>(), {
    filePath: 'untitled',
    oldContent: '',
    newContent: '',
    rawDiff: '',
    startLine: 1,
    additions: 0,
    deletions: 0,
    intercepted: false,
});

defineEmits<{
    (e: 'close'): void;
}>();

const isSplitView = ref(false);
const copied = ref(false);

const fileName = computed(() => {
    const p = props.filePath.trim().replace(/^['"]|['"]$/g, '');
    return p.split('/').pop() || p || 'File';
});

const pathSegments = computed(() => {
    const p = props.filePath.trim().replace(/^['"]|['"]$/g, '');
    return p.split('/').filter(Boolean);
});

// ── 🧮 纯前端高保真 LCS 差异计算算法 ──
const diffLines = computed<DiffLine[]>(() => {
    // 1. 如果传入了标准的 git diff 字符串，优先按 diff 格式解析
    if (props.rawDiff && props.rawDiff.trim()) {
        const allLines = props.rawDiff.split('\n');
        // 跳过 @@ 之前的文件头（diff --git / index / --- / +++ 等），避免被误渲染为假增删行
        const hunksStart = allLines.findIndex(l => l.startsWith('@@'));
        const lines = hunksStart >= 0 ? allLines.slice(hunksStart) : allLines;
        const result: DiffLine[] = [];
        let curOld = props.startLine;
        let curNew = props.startLine;

        for (const l of lines) {
            if (l.startsWith('\\')) continue; // \ No newline at end of file
            if (l.startsWith('@@')) {
                const match = l.match(/@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
                if (match) {
                    curOld = parseInt(match[1], 10);
                    curNew = parseInt(match[2], 10);
                }
                continue;
            }
            if (l.startsWith('-')) {
                result.push({ type: 'delete', oldLine: curOld++, content: l.slice(1) });
            } else if (l.startsWith('+')) {
                result.push({ type: 'add', newLine: curNew++, content: l.slice(1) });
            } else if (l.startsWith(' ') || l === '') {
                result.push({ type: 'normal', oldLine: curOld++, newLine: curNew++, content: l.startsWith(' ') ? l.slice(1) : l });
            }
        }
        if (result.length > 0) return result;
    }

    // 2. 基于 oldContent 与 newContent 进行 LCS 逐行对比
    const oldLines = props.oldContent ? props.oldContent.split('\n') : [];
    const newLines = props.newContent ? props.newContent.split('\n') : [];

    // 若新旧均为空
    if (oldLines.length === 0 && newLines.length === 0) return [];

    // 若仅有新增
    if (oldLines.length === 0) {
        let cur = props.startLine;
        return newLines.map(content => ({ type: 'add', newLine: cur++, content }));
    }

    // 若仅有删除
    if (newLines.length === 0) {
        let cur = props.startLine;
        return oldLines.map(content => ({ type: 'delete', oldLine: cur++, content }));
    }

    const m = oldLines.length;
    const n = newLines.length;

    // 前缀优化：去除相同的行
    let prefix = 0;
    while (prefix < m && prefix < n && oldLines[prefix] === newLines[prefix]) {
        prefix++;
    }

    // 后缀优化：去除末尾相同的行
    let suffix = 0;
    while (
        suffix < m - prefix &&
        suffix < n - prefix &&
        oldLines[m - 1 - suffix] === newLines[n - 1 - suffix]
    ) {
        suffix++;
    }

    const midOld = oldLines.slice(prefix, m - suffix);
    const midNew = newLines.slice(prefix, n - suffix);

    const result: DiffLine[] = [];
    let curOld = props.startLine;
    let curNew = props.startLine;

    // 添加相同前缀
    for (let i = 0; i < prefix; i++) {
        result.push({ type: 'normal', oldLine: curOld++, newLine: curNew++, content: oldLines[i] });
    }

    // 动态规划计算中间区域 LCS (限制在 50000 单元格以内防性能卡顿)
    if (midOld.length * midNew.length <= 150000) {
        const dp: number[][] = Array.from({ length: midOld.length + 1 }, () =>
            new Array(midNew.length + 1).fill(0)
        );

        for (let i = 1; i <= midOld.length; i++) {
            for (let j = 1; j <= midNew.length; j++) {
                if (midOld[i - 1] === midNew[j - 1]) {
                    dp[i][j] = dp[i - 1][j - 1] + 1;
                } else {
                    dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
                }
            }
        }

        const midDiffs: DiffLine[] = [];
        let i = midOld.length;
        let j = midNew.length;

        while (i > 0 || j > 0) {
            if (i > 0 && j > 0 && midOld[i - 1] === midNew[j - 1]) {
                midDiffs.unshift({ type: 'normal', content: midOld[i - 1] });
                i--;
                j--;
            } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
                midDiffs.unshift({ type: 'add', content: midNew[j - 1] });
                j--;
            } else if (i > 0) {
                midDiffs.unshift({ type: 'delete', content: midOld[i - 1] });
                i--;
            }
        }

        for (const d of midDiffs) {
            if (d.type === 'normal') {
                d.oldLine = curOld++;
                d.newLine = curNew++;
            } else if (d.type === 'delete') {
                d.oldLine = curOld++;
            } else if (d.type === 'add') {
                d.newLine = curNew++;
            }
            result.push(d);
        }
    } else {
        // 超大修改回退策略：旧内容全删，新内容全增
        for (const o of midOld) {
            result.push({ type: 'delete', oldLine: curOld++, content: o });
        }
        for (const n of midNew) {
            result.push({ type: 'add', newLine: curNew++, content: n });
        }
    }

    // 添加相同后缀
    for (let k = m - suffix; k < m; k++) {
        result.push({ type: 'normal', oldLine: curOld++, newLine: curNew++, content: oldLines[k] });
    }

    return result;
});

// 增删徽标：新旧内容齐全（或标准 diff）时以真实 LCS 统计为准，避免与下方渲染的行对不上
const hasRealDiffSource = computed(() => {
    return !!(props.rawDiff?.trim() || (props.oldContent?.trim() && props.newContent?.trim()));
});

const computedAdditions = computed(() => {
    if (hasRealDiffSource.value) return diffLines.value.filter(l => l.type === 'add').length;
    if (props.additions > 0) return props.additions;
    return diffLines.value.filter(l => l.type === 'add').length;
});

const computedDeletions = computed(() => {
    if (hasRealDiffSource.value) return diffLines.value.filter(l => l.type === 'delete').length;
    if (props.deletions > 0) return props.deletions;
    return diffLines.value.filter(l => l.type === 'delete').length;
});

// 分栏左右配对
const splitPairs = computed(() => {
    const pairs: Array<{ left?: DiffLine; right?: DiffLine }> = [];
    const lines = diffLines.value;
    let idx = 0;

    while (idx < lines.length) {
        const line = lines[idx];
        if (line.type === 'normal') {
            pairs.push({ left: line, right: line });
            idx++;
        } else if (line.type === 'delete') {
            const next = lines[idx + 1];
            if (next && next.type === 'add') {
                pairs.push({ left: line, right: next });
                idx += 2;
            } else {
                pairs.push({ left: line });
                idx++;
            }
        } else if (line.type === 'add') {
            pairs.push({ right: line });
            idx++;
        }
    }

    return pairs;
});

async function handleCopyDiff() {
    const text = diffLines.value
        .map(l => `${l.type === 'add' ? '+' : (l.type === 'delete' ? '-' : ' ')}${l.content}`)
        .join('\n');
    try {
        await navigator.clipboard.writeText(text);
        copied.value = true;
        setTimeout(() => { copied.value = false; }, 1800);
    } catch {}
}
</script>

<style scoped>
.file-diff-viewer {
    display: flex;
    flex-direction: column;
    height: 100%;
    width: 100%;
    background: #141416;
    color: #e4e4e7;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    overflow: hidden;
    box-sizing: border-box;
}

/* 顶部标签页栏 */
.diff-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 38px;
    background: #18181b;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
    padding: 0 10px;
    flex-shrink: 0;
    user-select: none;
}

.diff-tabs-bar {
    display: flex;
    align-items: center;
    height: 100%;
    min-width: 0;
}

.diff-active-tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 10px;
    background: #27272a;
    border-radius: 6px;
    border: 1px solid rgba(255, 255, 255, 0.1);
    font-size: 12px;
    color: #f4f4f5;
    font-weight: 500;
    white-space: nowrap;
    max-width: 320px;
}

.tab-icon {
    font-size: 12px;
    flex-shrink: 0;
}

.tab-filename {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}

.tab-diff-stat {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    margin-left: 2px;
}

.stat-add {
    color: #4ade80;
    font-weight: 600;
}

.stat-del {
    color: #f87171;
    font-weight: 600;
}

.btn-tab-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    background: transparent;
    border: none;
    color: #71717a;
    cursor: pointer;
    margin-left: 2px;
    transition: all 0.15s ease;
}

.btn-tab-close:hover {
    background: rgba(255, 255, 255, 0.1);
    color: #e4e4e7;
}

.diff-header-actions {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
}

.btn-diff-action {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 8px;
    border-radius: 5px;
    background: rgba(255, 255, 255, 0.05);
    border: 1px solid rgba(255, 255, 255, 0.08);
    color: #a1a1aa;
    font-size: 11px;
    cursor: pointer;
    transition: all 0.15s ease;
}

.btn-diff-action:hover {
    background: rgba(255, 255, 255, 0.09);
    color: #e4e4e7;
}

/* 面包屑导航栏 */
.diff-breadcrumb-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 28px;
    padding: 0 12px;
    background: #141416;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
    font-size: 11px;
    color: #71717a;
    flex-shrink: 0;
}

.breadcrumb-path {
    display: flex;
    align-items: center;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    gap: 2px;
}

.seg-text {
    color: #71717a;
}

.seg-text.is-last {
    color: #d4d4d8;
    font-weight: 500;
}

.seg-divider {
    color: #3f3f46;
    margin: 0 2px;
}

.diff-hunk-count {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    color: #52525b;
    font-size: 10.5px;
}

/* 门禁拦截提示横幅 */
.diff-intercepted-banner {
    flex-shrink: 0;
    padding: 6px 12px;
    background: rgba(239, 68, 68, 0.12);
    border-bottom: 1px solid rgba(239, 68, 68, 0.3);
    color: #fca5a5;
    font-size: 11.5px;
    font-weight: 550;
}

/* 视口与表格 */
.diff-viewport {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: auto;
    background: #121214;
}

.diff-table {
    display: flex;
    flex-direction: column;
    width: 100%;
    min-width: max-content;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
    font-size: 12px;
    line-height: 20px;
}

.diff-line-row {
    display: flex;
    align-items: stretch;
    width: 100%;
    box-sizing: border-box;
}

.diff-line-row:hover {
    background: rgba(255, 255, 255, 0.025);
}

.line-gutter {
    width: 44px;
    padding: 0 6px;
    text-align: right;
    color: #52525b;
    font-size: 11px;
    user-select: none;
    flex-shrink: 0;
    box-sizing: border-box;
}

.line-marker {
    width: 18px;
    text-align: center;
    font-weight: 600;
    user-select: none;
    flex-shrink: 0;
}

.line-code {
    margin: 0;
    padding: 0 12px 0 4px;
    color: #e4e4e7;
    white-space: pre;
    font-family: inherit;
    font-size: inherit;
    line-height: inherit;
    flex: 1;
}

/* 差异颜色高亮 */
.diff-line-delete {
    background: rgba(239, 68, 68, 0.12) !important;
}

.diff-line-delete .line-gutter {
    background: rgba(239, 68, 68, 0.18);
    color: #fca5a5;
}

.diff-line-delete .line-marker {
    color: #f87171;
}

.diff-line-delete .line-code {
    color: #fca5a5;
}

.diff-line-add {
    background: rgba(34, 197, 94, 0.12) !important;
}

.diff-line-add .line-gutter {
    background: rgba(34, 197, 94, 0.18);
    color: #86efac;
}

.diff-line-add .line-marker {
    color: #4ade80;
}

.diff-line-add .line-code {
    color: #86efac;
}

/* 分栏模式 */
.split-view {
    display: flex;
    width: 100%;
    height: 100%;
}

.split-column {
    flex: 1;
    min-width: 0;
    overflow: auto;
    border-right: 1px solid rgba(255, 255, 255, 0.06);
}

.split-col-header {
    height: 24px;
    line-height: 24px;
    padding: 0 10px;
    font-size: 11px;
    color: #71717a;
    background: #18181b;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
}

.diff-empty-state {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 200px;
    color: #71717a;
    font-size: 13px;
}
</style>
