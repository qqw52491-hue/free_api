<template>
    <transition name="deck-slide">
        <div v-if="shouldShow" class="agent-progress-deck" @click.stop>
            <!-- 🌟 折叠态 (Image 2: 5 files +42 -12 >) -->
            <div
                v-if="!isExpanded"
                class="deck-collapsed-pill"
                role="button"
                tabindex="0"
                title="点击查看修改文件列表"
                @click.stop="toggleExpanded"
                @keydown.enter="toggleExpanded"
            >
                <span class="collapsed-count">
                    {{ allModifiedFiles.length }} {{ allModifiedFiles.length === 1 ? 'file' : 'files' }}
                </span>
                <span v-if="totalAdditions > 0" class="stat-add">+{{ totalAdditions }}</span>
                <span v-if="totalDeletions > 0" class="stat-del">-{{ totalDeletions }}</span>
                <svg
                    class="chevron-right-icon"
                    viewBox="0 0 24 24"
                    width="12"
                    height="12"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="2.2"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                >
                    <polyline points="9 18 15 12 9 6" />
                </svg>
            </div>

            <!-- 🌟 展开态 (Image 3: 5 files with changes v ... View all) -->
            <div v-else class="deck-expanded-panel">
                <!-- 顶栏标题与 View all -->
                <div class="panel-header">
                    <div
                        class="header-toggle"
                        role="button"
                        tabindex="0"
                        title="点击收起文件列表"
                        @click.stop="toggleExpanded"
                        @keydown.enter="toggleExpanded"
                    >
                        <span class="header-title">
                            {{ allModifiedFiles.length }} {{ allModifiedFiles.length === 1 ? 'file' : 'files' }} with changes
                        </span>
                        <svg
                            class="chevron-down-icon"
                            viewBox="0 0 24 24"
                            width="12"
                            height="12"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="2.2"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                        >
                            <polyline points="6 9 12 15 18 9" />
                        </svg>
                    </div>

                    <button
                        type="button"
                        class="view-all-btn"
                        title="在右侧打开全部代码变更对比"
                        @click.stop="handleViewAll"
                    >
                        View all
                    </button>
                </div>

                <!-- 文件改动清单 -->
                <div class="panel-file-list">
                    <div
                        v-for="file in allModifiedFiles"
                        :key="file.path || file.fileName"
                        class="file-row"
                        :title="`点击查看 ${getFileName(file)} 代码改动`"
                        @click.stop="handleFileClick(file)"
                    >
                        <!-- 1. 目录前缀 (如 ~/Documents/free_api/src-tauri/src/agent/loop_runner/) -->
                        <span v-if="getFileDir(file)" class="file-dir" :title="getFileDir(file)">
                            {{ getFileDir(file) }}
                        </span>

                        <!-- 2. 语言专属矢量图标 (FileIcon) -->
                        <FileIcon
                            :name="getFileName(file)"
                            :size="14"
                            class="file-icon"
                        />

                        <!-- 3. 文件名 (如 dispatch_child_reviewer.rs) -->
                        <span class="file-name" :title="getFileName(file)">
                            {{ getFileName(file) }}
                        </span>

                        <!-- 4. +新增 -删除 统计 -->
                        <span class="file-diff-stats">
                            <span v-if="file.additions && file.additions > 0" class="stat-add">+{{ file.additions }}</span>
                            <span v-if="file.deletions && file.deletions > 0" class="stat-del">-{{ file.deletions }}</span>
                        </span>
                    </div>
                </div>
            </div>
        </div>
    </transition>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import type { AgentStepItem, AgentTokenUsage, FileChangeChip, FileDiffPayload } from './types';
import { diffPayloadFromChip, parseStepFileChanges, aggregateFileChips } from './diffUtils';
import FileIcon from './FileIcon.vue';

const props = withDefaults(
    defineProps<{
        steps?: AgentStepItem[];
        isRunning?: boolean;
        isThinking?: boolean;
        thought?: string;
        actionPhaseText?: string;
        tokenUsage?: AgentTokenUsage | null;
        activeStepNumber?: number;
        sessionTitle?: string;
        summary?: string;
        planGate?: any;
    }>(),
    {
        steps: () => [],
        isRunning: false,
        isThinking: false,
    }
);

const emit = defineEmits<{
    (e: 'select-step', step: AgentStepItem): void;
    (e: 'open-diff', payload: FileDiffPayload): void;
}>();

const isExpanded = ref(false);

function toggleExpanded() {
    isExpanded.value = !isExpanded.value;
}

const allModifiedFiles = computed<FileChangeChip[]>(() => {
    const rawChips: FileChangeChip[] = [];
    for (const step of props.steps || []) {
        const changes = parseStepFileChanges(step);
        // 优先取 changes.chips 列表；若无则回退取 changes.chip，严禁双重 push 导致计数翻倍
        const candidateChips = (changes.chips && changes.chips.length > 0)
            ? changes.chips
            : (changes.chip ? [changes.chip] : []);

        for (const c of candidateChips) {
            // 被拦截/失败的写入未真正落地，不计入"修改过的文件"
            if (!c.isIntercepted && ((c.additions || 0) > 0 || (c.deletions || 0) > 0 || c.actionKind === 'modify' || c.actionKind === 'create' || c.actionKind === 'delete')) {
                rawChips.push(c);
            }
        }
    }
    return aggregateFileChips(rawChips);
});

const totalAdditions = computed(() => {
    return allModifiedFiles.value.reduce((acc, f) => acc + (f.additions || 0), 0);
});

const totalDeletions = computed(() => {
    return allModifiedFiles.value.reduce((acc, f) => acc + (f.deletions || 0), 0);
});

const shouldShow = computed(() => {
    return allModifiedFiles.value.length > 0;
});

function getFileDir(file: FileChangeChip): string {
    const rawPath = (file.path || '').replace(/\\/g, '/');
    if (!rawPath) return '';
    const lastSlash = rawPath.lastIndexOf('/');
    if (lastSlash < 0) return '';
    let dir = rawPath.substring(0, lastSlash + 1);
    // 将系统家目录替换为 ~，对齐 Cursor/Unix 规范并精准复现截图
    dir = dir.replace(/^\/(?:Users|home)\/[^/]+/, '~');
    return dir;
}

function getFileName(file: FileChangeChip): string {
    if (file.fileName) return file.fileName;
    const rawPath = (file.path || '').replace(/\\/g, '/');
    const parts = rawPath.split('/');
    return parts[parts.length - 1] || 'file';
}

function handleFileClick(file: FileChangeChip) {
    emit('open-diff', diffPayloadFromChip(file));
}

function handleViewAll() {
    if (allModifiedFiles.value.length > 0) {
        emit('open-diff', diffPayloadFromChip(allModifiedFiles.value[0]));
    }
}
</script>

<style scoped>
/* 宿主容器：位于 .manus-composer 内部最顶端，底部有纤细分割线与输入区隔离 */
.agent-progress-deck {
    width: 100%;
    box-sizing: border-box;
    background: transparent;
    border: none;
    box-shadow: none;
    padding-bottom: 6px;
    margin-bottom: 6px;
    border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

/* 🌟 折叠态 (Image 2: 5 files +42 -12 >) */
.deck-collapsed-pill {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 1px 2px;
    background: transparent;
    border: none;
    box-shadow: none;
    cursor: pointer;
    user-select: none;
    transition: opacity 0.15s ease;
}

.deck-collapsed-pill:hover {
    opacity: 0.85;
}

.collapsed-count {
    font-size: 13px;
    color: #9ca3af;
    font-weight: 400;
}

.chevron-right-icon {
    color: #71717a;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    margin-left: 1px;
    transition: transform 0.15s ease, color 0.15s ease;
}

.deck-collapsed-pill:hover .chevron-right-icon {
    color: #d4d4d8;
    transform: translateX(1px);
}

/* 🌟 展开态 (Image 3: 5 files with changes v ... View all) */
.deck-expanded-panel {
    width: 100%;
    background: transparent;
    border: none;
    border-radius: 0;
    padding: 0;
    box-shadow: none;
    box-sizing: border-box;
}

.panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 4px;
    padding: 0 2px;
}

.header-toggle {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
    user-select: none;
    color: #9ca3af;
    transition: color 0.15s ease;
}

.header-toggle:hover {
    color: #e4e4e7;
}

.header-title {
    font-size: 12.5px;
    font-weight: 500;
}

.chevron-down-icon {
    color: #71717a;
    transition: transform 0.15s ease, color 0.15s ease;
}

.header-toggle:hover .chevron-down-icon {
    color: #d4d4d8;
}

.view-all-btn {
    background: transparent;
    border: none;
    padding: 1px 4px;
    font-size: 12px;
    color: #9ca3af;
    cursor: pointer;
    transition: color 0.15s ease;
}

.view-all-btn:hover {
    color: #f1f5f9;
}

/* 文件清单滚动容器：紧密排列，极小行间距 */
.panel-file-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: 200px;
    overflow-y: auto;
    scrollbar-width: thin;
    scrollbar-color: rgba(255, 255, 255, 0.18) transparent;
}

.panel-file-list::-webkit-scrollbar {
    width: 4px;
}

.panel-file-list::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.18);
    border-radius: 4px;
}

.panel-file-list::-webkit-scrollbar-thumb:hover {
    background: rgba(255, 255, 255, 0.3);
}

/* 单条文件行：紧凑高度 (1.5px padding)，极致轻巧 */
.file-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 1.5px 4px;
    border-radius: 3px;
    cursor: pointer;
    transition: background 0.1s ease;
    min-width: 0;
    line-height: 1.35;
}

.file-row:hover {
    background: rgba(255, 255, 255, 0.06);
}

.file-row:hover .file-name {
    color: #ffffff;
}

.file-dir {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    color: #71717a;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    flex-shrink: 1;
}

.file-icon {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
}

.file-name {
    font-size: 12.5px;
    font-weight: 500;
    color: #f1f5f9;
    white-space: nowrap;
    flex-shrink: 0;
}

.file-diff-stats {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-left: 6px;
    flex-shrink: 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    font-weight: 500;
}

.stat-add {
    color: #22c55e !important;
}

.stat-del {
    color: #ef4444 !important;
}

/* 进出过渡动画 */
.deck-slide-enter-active,
.deck-slide-leave-active {
    transition: opacity 0.15s ease, transform 0.15s cubic-bezier(0.16, 1, 0.3, 1);
}

.deck-slide-enter-from,
.deck-slide-leave-to {
    opacity: 0;
    transform: translateY(-4px);
}
</style>
