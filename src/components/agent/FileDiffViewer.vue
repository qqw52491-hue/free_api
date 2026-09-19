<template>
    <div class="file-diff-viewer">
        <!-- 顶部标签页与动作栏 (类似 Cursor/VS Code Editor Header) -->
        <header class="diff-header">
            <div class="diff-tabs-bar">
                <!-- 1. 全部文件总览 Tab (当有多个变更文件时置顶展示) -->
                <button
                    v-if="effectiveFiles.length > 1"
                    type="button"
                    class="diff-tab-btn"
                    :class="{ 'is-active': viewMode === 'all' }"
                    title="在右侧平铺展示所有修改过的文件"
                    @click="viewMode = 'all'"
                >
                    <span class="tab-icon">📑</span>
                    <span class="tab-filename">全部变更 ({{ effectiveFiles.length }})</span>
                    <span class="tab-diff-stat">
                        <span v-if="totalAdditions > 0" class="stat-add">+{{ totalAdditions }}</span>
                        <span v-if="totalDeletions > 0" class="stat-del">−{{ totalDeletions }}</span>
                    </span>
                </button>

                <!-- 2. 各个独立文件 Tab -->
                <button
                    v-for="(f, idx) in effectiveFiles"
                    :key="f.filePath || idx"
                    type="button"
                    class="diff-tab-btn"
                    :class="{ 'is-active': viewMode === 'single' && currentFileIndex === idx }"
                    :title="f.filePath"
                    @click="selectSingleFile(idx)"
                >
                    <FileIcon
                        :name="f.fileName || getFileName(f.filePath)"
                        :size="13"
                        class="tab-file-icon"
                    />
                    <span class="tab-filename">{{ f.fileName || getFileName(f.filePath) }}</span>
                    <span class="tab-diff-stat">
                        <span v-if="(f.additions || 0) > 0" class="stat-add">+{{ f.additions }}</span>
                        <span v-if="(f.deletions || 0) > 0" class="stat-del">−{{ f.deletions }}</span>
                    </span>
                </button>
            </div>

            <!-- 右侧全局操作栏 -->
            <div class="diff-header-actions">
                <!-- 切换全部文件平铺 / 单文件 -->
                <button
                    v-if="effectiveFiles.length > 1"
                    type="button"
                    class="btn-diff-action"
                    :title="viewMode === 'all' ? '切换为单文件选项卡模式' : '平铺显示全部变更文件'"
                    @click="viewMode = viewMode === 'all' ? 'single' : 'all'"
                >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 6h18M3 12h18M3 18h18"/></svg>
                    <span>{{ viewMode === 'all' ? '单文件' : `全部平铺 (${effectiveFiles.length})` }}</span>
                </button>

                <!-- 复制所有 Diff (在全部模式下) 或当前 Diff (在单文件模式下) -->
                <button
                    type="button"
                    class="btn-diff-action"
                    :title="copiedCurrent ? '已复制到剪贴板 ✓' : '复制 Diff 内容'"
                    @click="handleCopy"
                >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>
                    <span>{{ copiedCurrent ? '已复制' : '复制' }}</span>
                </button>

                <!-- 切换行内/双栏对比 -->
                <button
                    type="button"
                    class="btn-diff-action"
                    title="切换行内/双栏对比"
                    @click="isSplitView = !isSplitView"
                >
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="3" width="18" height="18" rx="2"/><path d="M12 3v18"/></svg>
                    <span>{{ isSplitView ? '单栏' : '分栏' }}</span>
                </button>

                <!-- 关闭 Diff 视图 -->
                <button
                    type="button"
                    class="btn-diff-action btn-close"
                    title="关闭 Diff 视图"
                    @click="$emit('close')"
                >
                    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>
                </button>
            </div>
        </header>

        <!-- 🌟 模式 1：全部文件平铺视图 (View All) -->
        <div v-if="viewMode === 'all'" class="diff-all-files-scroll">
            <div
                v-for="(file, fIdx) in effectiveFiles"
                :key="file.filePath || fIdx"
                :id="`diff-card-${fIdx}`"
                class="diff-file-card"
            >
                <!-- 卡片顶栏 -->
                <div class="diff-file-card-header" @click="toggleFileCollapsed(fIdx)">
                    <div class="card-header-left">
                        <button
                            type="button"
                            class="btn-chevron-toggle"
                            :title="collapsedIndices.has(fIdx) ? '展开此文件' : '折叠此文件'"
                        >
                            <svg
                                width="11"
                                height="11"
                                viewBox="0 0 24 24"
                                fill="none"
                                stroke="currentColor"
                                stroke-width="2.2"
                                stroke-linecap="round"
                                stroke-linejoin="round"
                                :class="{ 'rotate-minus-90': collapsedIndices.has(fIdx) }"
                            >
                                <polyline points="6 9 12 15 18 9" />
                            </svg>
                        </button>

                        <FileIcon
                            :name="file.fileName || getFileName(file.filePath)"
                            :size="15"
                            class="card-icon"
                        />

                        <span v-if="getFileDir(file.filePath)" class="card-dir">
                            {{ getFileDir(file.filePath) }}
                        </span>
                        <span class="card-filename">{{ file.fileName || getFileName(file.filePath) }}</span>

                        <span class="card-stats">
                            <span v-if="(file.additions || 0) > 0" class="stat-add">+{{ file.additions }}</span>
                            <span v-if="(file.deletions || 0) > 0" class="stat-del">−{{ file.deletions }}</span>
                        </span>

                        <span v-if="file.isIntercepted" class="card-intercepted-tag">已拦截</span>
                    </div>

                    <div class="card-header-right" @click.stop>
                        <button
                            type="button"
                            class="btn-card-action"
                            title="聚焦在单文件选项卡中查看"
                            @click.stop="selectSingleFile(fIdx)"
                        >
                            单文件打开
                        </button>
                        <button
                            type="button"
                            class="btn-card-action"
                            :title="copiedCardIndex === fIdx ? '已复制 ✓' : '复制此文件变更'"
                            @click.stop="handleCopyCardDiff(file, fIdx)"
                        >
                            {{ copiedCardIndex === fIdx ? '已复制' : '复制' }}
                        </button>
                    </div>
                </div>

                <!-- 卡片 Diff 差异区 -->
                <div v-if="!collapsedIndices.has(fIdx)" class="card-diff-content">
                    <!-- Unified View -->
                    <div v-if="!isSplitView" class="diff-viewport unified-view">
                        <div v-if="!filesDiffMap.get(file.filePath || file.fileName || '')?.length" class="diff-empty-state">
                            <p>未检测到代码变动细节或文件内容相同</p>
                        </div>
                        <div v-else class="diff-table">
                            <div
                                v-for="(line, lIdx) in filesDiffMap.get(file.filePath || file.fileName || '')"
                                :key="lIdx"
                                class="diff-line-row"
                                :class="[`diff-line-${line.type}`]"
                            >
                                <span class="line-gutter line-num-old">{{ line.oldLine ?? '' }}</span>
                                <span class="line-gutter line-num-new">{{ line.newLine ?? '' }}</span>
                                <span class="line-marker">{{ line.type === 'add' ? '+' : (line.type === 'delete' ? '−' : ' ') }}</span>
                                <pre class="line-code">{{ line.content }}</pre>
                            </div>
                        </div>
                    </div>

                    <!-- Split View -->
                    <div v-else class="diff-viewport split-view">
                        <div class="split-column left-column">
                            <div class="split-col-header">修改前 (Original)</div>
                            <div class="diff-table">
                                <div
                                    v-for="(pair, pIdx) in getSplitPairsForFile(file)"
                                    :key="pIdx"
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
                                    v-for="(pair, pIdx) in getSplitPairsForFile(file)"
                                    :key="pIdx"
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
            </div>
        </div>

        <!-- 🌟 模式 2：单文件聚焦选项卡视图 -->
        <template v-else>
            <!-- 面包屑导航栏 -->
            <div class="diff-breadcrumb-bar">
                <div class="breadcrumb-path" :title="activeFile.filePath">
                    <span v-for="(seg, idx) in activePathSegments" :key="idx" class="breadcrumb-seg">
                        <span class="seg-text" :class="{ 'is-last': idx === activePathSegments.length - 1 }">{{ seg }}</span>
                        <span v-if="idx < activePathSegments.length - 1" class="seg-divider">/</span>
                    </span>
                </div>
                <div class="breadcrumb-meta">
                    <span class="diff-hunk-count">{{ activeDiffLines.length }} 行变更细节</span>
                </div>
            </div>

            <!-- 门禁拦截提示 -->
            <div v-if="activeFile.isIntercepted" class="diff-intercepted-banner">
                🚨 该写入已被门禁拦截，以下为尝试写入的内容，实际未落盘
            </div>

            <!-- Diff 代码对比视口 (Unified View) -->
            <div v-if="!isSplitView" class="diff-viewport unified-view">
                <div v-if="activeDiffLines.length === 0" class="diff-empty-state">
                    <p>未检测到文件内容差异或文件内容相同</p>
                </div>
                <div v-else class="diff-table">
                    <div
                        v-for="(line, idx) in activeDiffLines"
                        :key="idx"
                        class="diff-line-row"
                        :class="[`diff-line-${line.type}`]"
                    >
                        <span class="line-gutter line-num-old">{{ line.oldLine ?? '' }}</span>
                        <span class="line-gutter line-num-new">{{ line.newLine ?? '' }}</span>
                        <span class="line-marker">{{ line.type === 'add' ? '+' : (line.type === 'delete' ? '−' : ' ') }}</span>
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
                            v-for="(pair, idx) in activeSplitPairs"
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
                            v-for="(pair, idx) in activeSplitPairs"
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
        </template>
    </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import type { FileDiffPayload } from './types';
import FileIcon from './FileIcon.vue';

export interface DiffLine {
    type: 'add' | 'delete' | 'normal';
    oldLine?: number;
    newLine?: number;
    content: string;
}

export interface SplitDiffRow {
    left?: DiffLine;
    right?: DiffLine;
}

const props = withDefaults(
    defineProps<{
        filePath?: string;
        fileName?: string;
        oldContent?: string;
        newContent?: string;
        rawDiff?: string;
        startLine?: number;
        additions?: number;
        deletions?: number;
        intercepted?: boolean;
        allFiles?: FileDiffPayload[];
        viewAll?: boolean;
        activeIndex?: number;
    }>(),
    {
        filePath: 'untitled',
        fileName: '',
        oldContent: '',
        newContent: '',
        rawDiff: '',
        startLine: 1,
        additions: 0,
        deletions: 0,
        intercepted: false,
        allFiles: () => [],
        viewAll: false,
        activeIndex: 0,
    }
);

const emit = defineEmits<{
    (e: 'close'): void;
    (e: 'select-file', file: FileDiffPayload): void;
}>();

const isSplitView = ref(false);
const copiedCurrent = ref(false);
const copiedCardIndex = ref<number | null>(null);

// 所有生效的修改文件列表
const effectiveFiles = computed<FileDiffPayload[]>(() => {
    if (props.allFiles && props.allFiles.length > 0) {
        return props.allFiles;
    }
    return [{
        filePath: props.filePath,
        fileName: props.fileName || getFileName(props.filePath),
        oldContent: props.oldContent,
        newContent: props.newContent,
        rawDiff: props.rawDiff,
        startLine: props.startLine,
        additions: props.additions,
        deletions: props.deletions,
        isIntercepted: props.intercepted,
    }];
});

// 视图模式：'all' 平铺全部变更文件，'single' 单文件选项卡聚焦
const viewMode = ref<'all' | 'single'>((props.viewAll || (props.allFiles && props.allFiles.length > 1 && props.viewAll !== false)) ? 'all' : 'single');
const currentFileIndex = ref<number>(props.activeIndex ?? 0);
const collapsedIndices = ref<Set<number>>(new Set());

watch(() => props.viewAll, (val) => {
    if (val !== undefined) {
        viewMode.value = val ? 'all' : 'single';
    }
}, { immediate: true });

watch(() => props.activeIndex, (idx) => {
    if (typeof idx === 'number' && idx >= 0) {
        currentFileIndex.value = idx;
    }
}, { immediate: true });

watch(
    () => [props.filePath, props.fileName],
    ([p, n]) => {
        if (!p && !n) return;
        const target = (p || n || '').trim();
        if (!target) return;
        const idx = effectiveFiles.value.findIndex(
            (f) => (f.filePath && f.filePath === p) ||
                   (f.fileName && f.fileName === n) ||
                   (f.filePath && f.filePath.endsWith(target))
        );
        if (idx >= 0) {
            currentFileIndex.value = idx;
        }
    },
    { immediate: true }
);

const activeFile = computed<FileDiffPayload>(() => {
    return effectiveFiles.value[currentFileIndex.value] || effectiveFiles.value[0] || {
        filePath: props.filePath,
        fileName: props.fileName || getFileName(props.filePath),
        oldContent: props.oldContent,
        newContent: props.newContent,
        rawDiff: props.rawDiff,
        startLine: props.startLine,
        additions: props.additions,
        deletions: props.deletions,
        isIntercepted: props.intercepted,
    };
});

const totalAdditions = computed(() => {
    return effectiveFiles.value.reduce((acc, f) => acc + (f.additions || 0), 0);
});

const totalDeletions = computed(() => {
    return effectiveFiles.value.reduce((acc, f) => acc + (f.deletions || 0), 0);
});

const activePathSegments = computed(() => {
    const p = (activeFile.value.filePath || '').trim().replace(/^['"]|['"]$/g, '');
    return p.split('/').filter(Boolean);
});

function getFileDir(rawPath?: string): string {
    if (!rawPath) return '';
    const p = rawPath.replace(/\\/g, '/');
    const lastSlash = p.lastIndexOf('/');
    if (lastSlash < 0) return '';
    let dir = p.substring(0, lastSlash + 1);
    dir = dir.replace(/^\/(?:Users|home)\/[^/]+/, '~');
    return dir;
}

function getFileName(rawPath?: string): string {
    if (!rawPath) return 'File';
    const p = rawPath.replace(/\\/g, '/');
    const parts = p.split('/');
    return parts[parts.length - 1] || 'File';
}

function toggleFileCollapsed(idx: number) {
    if (collapsedIndices.value.has(idx)) {
        collapsedIndices.value.delete(idx);
    } else {
        collapsedIndices.value.add(idx);
    }
}

function selectSingleFile(idx: number) {
    currentFileIndex.value = idx;
    viewMode.value = 'single';
    if (effectiveFiles.value[idx]) {
        emit('select-file', effectiveFiles.value[idx]);
    }
}

// ── 🧮 纯前端高保真 LCS 差异计算算法 ──
function computeDiffLines(file: {
    rawDiff?: string;
    oldContent?: string;
    newContent?: string;
    startLine?: number;
}): DiffLine[] {
    // 1. 如果传入了标准的 git diff 字符串，优先按 diff 格式解析
    if (file.rawDiff && file.rawDiff.trim()) {
        const allLines = file.rawDiff.split('\n');
        const hunksStart = allLines.findIndex(l => l.startsWith('@@'));
        const lines = hunksStart >= 0 ? allLines.slice(hunksStart) : allLines;
        const result: DiffLine[] = [];
        let curOld = file.startLine || 1;
        let curNew = file.startLine || 1;

        for (const l of lines) {
            if (l.startsWith('\\')) continue;
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
    const oldLines = file.oldContent ? file.oldContent.split('\n') : [];
    const newLines = file.newContent ? file.newContent.split('\n') : [];

    if (oldLines.length === 0 && newLines.length === 0) return [];

    if (oldLines.length === 0) {
        let cur = file.startLine || 1;
        return newLines.map(content => ({ type: 'add', newLine: cur++, content }));
    }

    if (newLines.length === 0) {
        let cur = file.startLine || 1;
        return oldLines.map(content => ({ type: 'delete', oldLine: cur++, content }));
    }

    const m = oldLines.length;
    const n = newLines.length;

    let prefix = 0;
    while (prefix < m && prefix < n && oldLines[prefix] === newLines[prefix]) {
        prefix++;
    }

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

    for (let i = 0; i < prefix; i++) {
        result.push({ type: 'normal', oldLine: (file.startLine || 1) + i, newLine: (file.startLine || 1) + i, content: oldLines[i] });
    }

    if (midOld.length * midNew.length <= 40000) {
        const dp: number[][] = Array.from({ length: midOld.length + 1 }, () => new Array(midNew.length + 1).fill(0));
        for (let i = 1; i <= midOld.length; i++) {
            for (let j = 1; j <= midNew.length; j++) {
                if (midOld[i - 1] === midNew[j - 1]) {
                    dp[i][j] = dp[i - 1][j - 1] + 1;
                } else {
                    dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
                }
            }
        }
        let i = midOld.length;
        let j = midNew.length;
        const midResult: DiffLine[] = [];
        while (i > 0 || j > 0) {
            if (i > 0 && j > 0 && midOld[i - 1] === midNew[j - 1]) {
                midResult.unshift({ type: 'normal', content: midOld[i - 1] });
                i--;
                j--;
            } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
                midResult.unshift({ type: 'add', content: midNew[j - 1] });
                j--;
            } else if (i > 0) {
                midResult.unshift({ type: 'delete', content: midOld[i - 1] });
                i--;
            }
        }
        let curOld = (file.startLine || 1) + prefix;
        let curNew = (file.startLine || 1) + prefix;
        for (const item of midResult) {
            if (item.type === 'normal') {
                item.oldLine = curOld++;
                item.newLine = curNew++;
            } else if (item.type === 'delete') {
                item.oldLine = curOld++;
            } else {
                item.newLine = curNew++;
            }
            result.push(item);
        }
    } else {
        let curOld = (file.startLine || 1) + prefix;
        for (const l of midOld) {
            result.push({ type: 'delete', oldLine: curOld++, content: l });
        }
        let curNew = (file.startLine || 1) + prefix;
        for (const l of midNew) {
            result.push({ type: 'add', newLine: curNew++, content: l });
        }
    }

    const oldTailStart = m - suffix;
    const newTailStart = n - suffix;
    for (let i = 0; i < suffix; i++) {
        result.push({
            type: 'normal',
            oldLine: (file.startLine || 1) + oldTailStart + i,
            newLine: (file.startLine || 1) + newTailStart + i,
            content: oldLines[oldTailStart + i],
        });
    }

    return result;
}

// 缓存所有文件的 Diff 行
const filesDiffMap = computed(() => {
    const map = new Map<string, DiffLine[]>();
    for (const f of effectiveFiles.value) {
        const key = f.filePath || f.fileName || 'unknown';
        map.set(key, computeDiffLines(f));
    }
    return map;
});

const activeDiffLines = computed<DiffLine[]>(() => {
    const key = activeFile.value.filePath || activeFile.value.fileName || 'unknown';
    return filesDiffMap.value.get(key) || [];
});

function computeSplitPairs(lines: DiffLine[]): SplitDiffRow[] {
    const pairs: SplitDiffRow[] = [];
    let idx = 0;
    while (idx < lines.length) {
        const line = lines[idx];
        if (line.type === 'normal') {
            pairs.push({ left: line, right: line });
            idx++;
        } else if (line.type === 'delete') {
            let delEnd = idx;
            while (delEnd < lines.length && lines[delEnd].type === 'delete') delEnd++;
            let addEnd = delEnd;
            while (addEnd < lines.length && lines[addEnd].type === 'add') addEnd++;
            const dels = lines.slice(idx, delEnd);
            const adds = lines.slice(delEnd, addEnd);
            const maxLen = Math.max(dels.length, adds.length);
            for (let k = 0; k < maxLen; k++) {
                pairs.push({ left: dels[k], right: adds[k] });
            }
            idx = addEnd;
        } else if (line.type === 'add') {
            pairs.push({ right: line });
            idx++;
        }
    }
    return pairs;
}

function getSplitPairsForFile(file: FileDiffPayload): SplitDiffRow[] {
    const lines = filesDiffMap.value.get(file.filePath || file.fileName || '') || [];
    return computeSplitPairs(lines);
}

const activeSplitPairs = computed<SplitDiffRow[]>(() => {
    return computeSplitPairs(activeDiffLines.value);
});

async function handleCopy() {
    let text = '';
    if (viewMode.value === 'all') {
        for (const f of effectiveFiles.value) {
            text += `--- a/${f.filePath || f.fileName}\n+++ b/${f.filePath || f.fileName}\n`;
            const lines = filesDiffMap.value.get(f.filePath || f.fileName || '') || [];
            text += lines.map(l => `${l.type === 'add' ? '+' : (l.type === 'delete' ? '-' : ' ')}${l.content}`).join('\n') + '\n\n';
        }
    } else {
        text = activeDiffLines.value
            .map(l => `${l.type === 'add' ? '+' : (l.type === 'delete' ? '-' : ' ')}${l.content}`)
            .join('\n');
    }
    try {
        await navigator.clipboard.writeText(text.trim());
        copiedCurrent.value = true;
        setTimeout(() => { copiedCurrent.value = false; }, 1800);
    } catch {}
}

async function handleCopyCardDiff(file: FileDiffPayload, idx: number) {
    const lines = filesDiffMap.value.get(file.filePath || file.fileName || '') || [];
    const text = lines
        .map(l => `${l.type === 'add' ? '+' : (l.type === 'delete' ? '-' : ' ')}${l.content}`)
        .join('\n');
    try {
        await navigator.clipboard.writeText(text);
        copiedCardIndex.value = idx;
        setTimeout(() => {
            if (copiedCardIndex.value === idx) copiedCardIndex.value = null;
        }, 1800);
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
    gap: 8px;
}

.diff-tabs-bar {
    display: flex;
    align-items: center;
    height: 100%;
    min-width: 0;
    overflow-x: auto;
    gap: 4px;
    scrollbar-width: none;
}

.diff-tabs-bar::-webkit-scrollbar {
    display: none;
}

.diff-tab-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 27px;
    padding: 0 9px;
    background: transparent;
    border-radius: 5px;
    border: 1px solid transparent;
    font-size: 12px;
    color: #a1a1aa;
    font-weight: 500;
    white-space: nowrap;
    max-width: 220px;
    cursor: pointer;
    transition: all 0.15s ease;
    flex-shrink: 0;
}

.diff-tab-btn:hover {
    background: rgba(255, 255, 255, 0.05);
    color: #e4e4e7;
}

.diff-tab-btn.is-active {
    background: #27272a;
    border-color: rgba(255, 255, 255, 0.12);
    color: #f4f4f5;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.2);
}

.tab-icon {
    font-size: 12px;
    flex-shrink: 0;
}

.tab-file-icon {
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
    gap: 3px;
    font-size: 11px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    margin-left: 2px;
}

.stat-add {
    color: #22c55e !important;
    font-weight: 600;
}

.stat-del {
    color: #ef4444 !important;
    font-weight: 600;
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

.btn-diff-action.btn-close {
    padding: 0 6px;
}

/* 🌟 全部文件平铺滚动区 */
.diff-all-files-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.18) transparent;
}

.diff-all-files-scroll::-webkit-scrollbar {
    width: 6px;
}

.diff-all-files-scroll::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.18);
    border-radius: 4px;
}

/* 单个文件卡片 */
.diff-file-card {
    background: #18181b;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    overflow: hidden;
    box-shadow: 0 2px 10px rgba(0, 0, 0, 0.2);
}

.diff-file-card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 7px 12px;
    background: rgba(255, 255, 255, 0.025);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    user-select: none;
    cursor: pointer;
    transition: background 0.12s ease;
}

.diff-file-card-header:hover {
    background: rgba(255, 255, 255, 0.04);
}

.card-header-left {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
}

.btn-chevron-toggle {
    background: transparent;
    border: none;
    padding: 0;
    color: #71717a;
    display: inline-flex;
    align-items: center;
    cursor: pointer;
    transition: transform 0.15s ease, color 0.15s ease;
}

.btn-chevron-toggle svg {
    transition: transform 0.15s ease;
}

.rotate-minus-90 {
    transform: rotate(-90deg);
}

.card-icon {
    flex-shrink: 0;
}

.card-dir {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    color: #71717a;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
}

.card-filename {
    font-size: 13px;
    font-weight: 500;
    color: #f4f4f5;
    white-space: nowrap;
}

.card-stats {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 11.5px;
    margin-left: 4px;
}

.card-intercepted-tag {
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 3px;
    background: rgba(239, 68, 68, 0.18);
    color: #fca5a5;
    border: 1px solid rgba(239, 68, 68, 0.3);
}

.card-header-right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
}

.btn-card-action {
    background: rgba(255, 255, 255, 0.04);
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 4px;
    padding: 2px 7px;
    font-size: 11px;
    color: #a1a1aa;
    cursor: pointer;
    transition: all 0.12s ease;
}

.btn-card-action:hover {
    background: rgba(255, 255, 255, 0.09);
    color: #f1f5f9;
}

.card-diff-content {
    max-height: 480px;
    overflow-y: auto;
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
    color: #e4e4e7;
    font-weight: 500;
}

.seg-divider {
    color: #3f3f46;
}

.diff-hunk-count {
    font-size: 11px;
    color: #71717a;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

.diff-intercepted-banner {
    padding: 6px 14px;
    background: rgba(239, 68, 68, 0.12);
    border-bottom: 1px solid rgba(239, 68, 68, 0.25);
    color: #fca5a5;
    font-size: 12px;
    font-weight: 500;
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
}

/* 统一视图 / 分栏视图代码展示区域 */
.diff-viewport {
    flex: 1;
    overflow-y: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    line-height: 1.5;
    background: #141416;
}

.diff-empty-state {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 36px 20px;
    color: #71717a;
    font-size: 12.5px;
}

.diff-table {
    display: flex;
    flex-direction: column;
    width: 100%;
}

.diff-line-row {
    display: flex;
    min-height: 20px;
    transition: background 0.05s ease;
}

.diff-line-row:hover {
    filter: brightness(1.15);
}

.line-gutter {
    width: 38px;
    padding: 0 6px;
    text-align: right;
    color: #52525b;
    user-select: none;
    flex-shrink: 0;
    border-right: 1px solid rgba(255, 255, 255, 0.05);
}

.line-marker {
    width: 18px;
    text-align: center;
    user-select: none;
    flex-shrink: 0;
    font-weight: 600;
}

.line-code {
    margin: 0;
    padding: 0 8px;
    white-space: pre-wrap;
    word-break: break-all;
    flex: 1;
    font-family: inherit;
    font-size: inherit;
}

/* 增、删、普通行视觉着色 */
.diff-line-add {
    background: rgba(34, 197, 94, 0.12);
    color: #e4e4e7;
}

.diff-line-add .line-marker {
    color: #22c55e;
}

.diff-line-delete {
    background: rgba(239, 68, 68, 0.12);
    color: #e4e4e7;
}

.diff-line-delete .line-marker {
    color: #ef4444;
}

.diff-line-normal {
    background: transparent;
    color: #d4d4d8;
}

.diff-line-empty {
    background: rgba(255, 255, 255, 0.015);
}

/* 分栏布局 (Split View) */
.split-view {
    display: flex;
    flex-direction: row;
    height: 100%;
}

.split-column {
    flex: 1;
    overflow-x: auto;
    overflow-y: auto;
    border-right: 1px solid rgba(255, 255, 255, 0.06);
}

.split-column:last-child {
    border-right: none;
}

.split-col-header {
    position: sticky;
    top: 0;
    z-index: 10;
    background: #18181b;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    padding: 3px 12px;
    font-size: 11px;
    color: #71717a;
    text-transform: uppercase;
    letter-spacing: 0.5px;
}
</style>
