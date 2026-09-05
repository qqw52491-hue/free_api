// 通用格式化工具：从 AgentComposer.vue / AttachmentPreviewModal.vue 去重抽出。
export function formatFileSize(bytes?: number): string {
    if (!bytes || bytes === 0) return "0 B";
    if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${bytes} B`;
}

export function formatTokens(count: number): string {
    if (count >= 1000000) return `${(count / 1000000).toFixed(1)}M`;
    if (count >= 1000) return `${(count / 1000).toFixed(1)}k`;
    return String(count);
}

export function formatMs(ms?: number): string {
    if (!ms) return "0ms";
    if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
    return `${ms}ms`;
}
