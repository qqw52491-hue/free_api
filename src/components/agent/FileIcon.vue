<template>
    <svg
        :width="computedSize"
        :height="computedSize"
        viewBox="0 0 24 24"
        fill="none"
        xmlns="http://www.w3.org/2000/svg"
        class="file-icon-svg"
        :class="[`type-${resolvedType.key}`, customClass]"
        aria-hidden="true"
    >
        <!-- 1. Excel / 电子表格 (.xlsx, .xls, .csv, .ods) -->
        <g v-if="resolvedType.key === 'excel'">
            <path d="M4 4C4 2.89543 4.89543 2 6 2H14L20 8V20C20 21.1046 19.1046 22 18 22H6C4.89543 22 4 21.1046 4 20V4Z" fill="#10B981" fill-opacity="0.18" stroke="#10B981" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M14 2V8H20" stroke="#10B981" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
            <!-- 电子表格网格 -->
            <path d="M8 12H16M8 16H16M11 10V18" stroke="#34D399" stroke-width="1.5" stroke-linecap="round" />
        </g>

        <!-- 2. Word 文档 (.docx, .doc, .wps) -->
        <g v-else-if="resolvedType.key === 'word'">
            <path d="M4 4C4 2.89543 4.89543 2 6 2H14L20 8V20C20 21.1046 19.1046 22 18 22H6C4.89543 22 4 21.1046 4 20V4Z" fill="#2563EB" fill-opacity="0.18" stroke="#3B82F6" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M14 2V8H20" stroke="#3B82F6" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
            <!-- 微软 Word 'W' 徽标 -->
            <path d="M7.5 12L9.2 18L10.8 13.5L12.4 18L14.1 12" stroke="#60A5FA" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 3. PDF 文档 (.pdf) -->
        <g v-else-if="resolvedType.key === 'pdf'">
            <path d="M4 4C4 2.89543 4.89543 2 6 2H14L20 8V20C20 21.1046 19.1046 22 18 22H6C4.89543 22 4 21.1046 4 20V4Z" fill="#EF4444" fill-opacity="0.18" stroke="#EF4444" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M14 2V8H20" stroke="#EF4444" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
            <!-- 经典 PDF 曲线折线 -->
            <path d="M8 17V11H10C11.1 11 11.8 11.7 11.8 12.8C11.8 13.9 11.1 14.6 10 14.6H8M14 17V11H15.8C16.8 11 17.5 11.8 17.5 13C17.5 14.2 16.8 15 15.8 15H14" stroke="#F87171" stroke-width="1.4" stroke-linecap="round" />
        </g>

        <!-- 4. PowerPoint / Keynote (.pptx, .ppt, .key) -->
        <g v-else-if="resolvedType.key === 'ppt'">
            <path d="M4 4C4 2.89543 4.89543 2 6 2H14L20 8V20C20 21.1046 19.1046 22 18 22H6C4.89543 22 4 21.1046 4 20V4Z" fill="#F97316" fill-opacity="0.18" stroke="#F97316" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M14 2V8H20" stroke="#F97316" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
            <path d="M8 17V11H11.5C12.8 11 13.5 11.8 13.5 13C13.5 14.2 12.8 15 11.5 15H8" stroke="#FB923C" stroke-width="1.6" stroke-linecap="round" />
        </g>

        <!-- 5. Markdown (.md, .markdown) -->
        <g v-else-if="resolvedType.key === 'markdown'">
            <rect x="3" y="5" width="18" height="14" rx="2" fill="#0284C7" fill-opacity="0.16" stroke="#38BDF8" stroke-width="1.6" />
            <path d="M6 15V9L8.5 12L11 9V15M17 11.5V15M17 15L15 13M17 15L19 13" stroke="#38BDF8" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 6. TypeScript (.ts, .tsx) -->
        <g v-else-if="resolvedType.key === 'typescript'">
            <rect x="3" y="3" width="18" height="18" rx="3.5" fill="#1E40AF" fill-opacity="0.22" stroke="#3B82F6" stroke-width="1.6" />
            <path d="M6.5 9H11.5M9 9V17" stroke="#60A5FA" stroke-width="1.6" stroke-linecap="round" />
            <path d="M17.5 11C16.8 10.3 15.5 10 14.5 10.8C13.5 11.6 13.8 13 14.8 13.6C15.8 14.2 16.5 14.8 16.2 15.8C15.9 16.8 14.2 17 13.2 16.2" stroke="#60A5FA" stroke-width="1.6" stroke-linecap="round" />
        </g>

        <!-- 7. JavaScript (.js, .jsx, .mjs) -->
        <g v-else-if="resolvedType.key === 'javascript'">
            <rect x="3" y="3" width="18" height="18" rx="3.5" fill="#854D0E" fill-opacity="0.2" stroke="#EAB308" stroke-width="1.6" />
            <path d="M9.5 11V15.5C9.5 16.6 8.5 17 7.5 16.2" stroke="#FDE047" stroke-width="1.6" stroke-linecap="round" />
            <path d="M17 11.5C16.3 10.8 15 10.5 14 11.2C13 12 13.3 13.3 14.3 13.9C15.3 14.5 16 15 15.7 16C15.4 17 13.7 17.2 12.7 16.4" stroke="#FDE047" stroke-width="1.6" stroke-linecap="round" />
        </g>

        <!-- 8. Vue (.vue) -->
        <g v-else-if="resolvedType.key === 'vue'">
            <path d="M2.5 4.5H6.5L12 14.5L17.5 4.5H21.5L12 21L2.5 4.5Z" fill="#10B981" fill-opacity="0.18" stroke="#10B981" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M6.5 4.5L12 14L17.5 4.5H13.8L12 7.6L10.2 4.5H6.5Z" fill="#3B82F6" fill-opacity="0.6" stroke="#3B82F6" stroke-width="1.2" stroke-linejoin="round" />
        </g>

        <!-- 9. Python (.py, .ipynb) -->
        <g v-else-if="resolvedType.key === 'python'">
            <path d="M11.8 3C7.5 3 7.8 4.9 7.8 4.9L7.8 6.9H12.2V7.5H5.8C3.9 7.5 3 8.8 3 11.2C3 13.6 4.3 13.4 4.3 13.4H5.8V11.5C5.8 9.8 7.2 9.8 7.2 9.8H12.2C13.5 9.8 14.5 8.8 14.5 7.5V4.9C14.5 3.6 13.5 3 11.8 3ZM9.5 4.5C10 4.5 10.4 4.9 10.4 5.4C10.4 5.9 10 6.3 9.5 6.3C9 6.3 8.6 5.9 8.6 5.4C8.6 4.9 9 4.5 9.5 4.5Z" fill="#38BDF8" fill-opacity="0.85" />
            <path d="M12.2 21C16.5 21 16.2 19.1 16.2 19.1V17.1H11.8V16.5H18.2C20.1 16.5 21 15.2 21 12.8C21 10.4 19.7 10.6 19.7 10.6H18.2V12.5C18.2 14.2 16.8 14.2 16.8 14.2H11.8C10.5 14.2 9.5 15.2 9.5 16.5V19.1C9.5 20.4 10.5 21 12.2 21ZM14.5 19.5C14 19.5 13.6 19.1 13.6 18.6C13.6 18.1 14 17.7 14.5 17.7C15 17.7 15.4 18.1 15.4 18.6C15.4 19.1 15 19.5 14.5 19.5Z" fill="#FACC15" fill-opacity="0.85" />
        </g>

        <!-- 10. Rust (.rs) -->
        <g v-else-if="resolvedType.key === 'rust'">
            <circle cx="12" cy="12" r="7.5" fill="#EA580C" fill-opacity="0.2" stroke="#F97316" stroke-width="1.6" />
            <path d="M12 2.5V5M12 19V21.5M2.5 12H5M19 12H21.5M5.3 5.3L7 7M17 17L18.7 18.7M5.3 18.7L7 17M17 7L18.7 5.3" stroke="#F97316" stroke-width="1.4" stroke-linecap="round" />
            <!-- R 字母徽标 -->
            <path d="M9.5 15.5V8.5H12.5C13.6 8.5 14.5 9.2 14.5 10.3C14.5 11.4 13.6 12 12.5 12H9.5M12.5 12L14.8 15.5" stroke="#FDBA74" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 11. Go (.go) -->
        <g v-else-if="resolvedType.key === 'go'">
            <rect x="3" y="5" width="18" height="14" rx="7" fill="#0284C7" fill-opacity="0.18" stroke="#00ADD8" stroke-width="1.6" />
            <path d="M10 10C9.5 9.3 8.6 9 7.7 9C6.2 9 5 10.3 5 12C5 13.7 6.2 15 7.7 15C8.8 15 9.7 14.3 10 13.2H7.7M16.5 9C15 9 13.8 10.3 13.8 12C13.8 13.7 15 15 16.5 15C18 15 19.2 13.7 19.2 12C19.2 10.3 18 9 16.5 9Z" stroke="#38BDF8" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 12. C / C++ (.c, .cpp, .h, .hpp) -->
        <g v-else-if="resolvedType.key === 'cpp'">
            <path d="M12 2.5L20 7V17L12 21.5L4 17V7L12 2.5Z" fill="#1D4ED8" fill-opacity="0.18" stroke="#3B82F6" stroke-width="1.5" stroke-linejoin="round" />
            <path d="M11 9.5C10.2 8.8 9 8.8 8.2 9.6C7.4 10.4 7.4 13.6 8.2 14.4C9 15.2 10.2 15.2 11 14.5M14 12H18M16 10V14" stroke="#93C5FD" stroke-width="1.5" stroke-linecap="round" />
        </g>

        <!-- 13. Java (.java, .jar) -->
        <g v-else-if="resolvedType.key === 'java'">
            <path d="M5 10H16C16 10 16.5 17 10.5 17C4.5 17 5 10 5 10Z" fill="#D97706" fill-opacity="0.18" stroke="#F59E0B" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M16 11C18 11 19 12 19 13.5C19 15 18 16 16 16M4 19H17M9 7C9 7 9.5 5 12 5C14.5 5 13 3 13 3" stroke="#FBBF24" stroke-width="1.4" stroke-linecap="round" />
        </g>

        <!-- 14. HTML (.html, .htm) -->
        <g v-else-if="resolvedType.key === 'html'">
            <path d="M4 3L5.5 19.5L12 21.5L18.5 19.5L20 3H4Z" fill="#EA580C" fill-opacity="0.18" stroke="#F97316" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M8 8H16L15.3 15L12 16L8.7 15L8.5 12H13.5" stroke="#FB923C" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 15. CSS (.css, .scss, .less) -->
        <g v-else-if="resolvedType.key === 'css'">
            <path d="M4 3L5.5 19.5L12 21.5L18.5 19.5L20 3H4Z" fill="#0284C7" fill-opacity="0.18" stroke="#0284C7" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M15.5 8H8.5L8.8 11.5H15.2L14.7 15L12 16L9.3 15" stroke="#38BDF8" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 16. JSON / 配置文件 (.json, .yaml, .yml, .toml, .env) -->
        <g v-else-if="resolvedType.key === 'config'">
            <path d="M8 4C6.5 4 6 5.2 6 6.5V9C6 10.3 5.2 11 4 11C5.2 11 6 11.7 6 13V15.5C6 16.8 6.5 18 8 18M16 4C17.5 4 18 5.2 18 6.5V9C18 10.3 18.8 11 20 11C18.8 11 18 11.7 18 13V15.5C18 16.8 17.5 18 16 18" stroke="#A855F7" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" />
            <circle cx="10" cy="11" r="1" fill="#C084FC" />
            <circle cx="14" cy="11" r="1" fill="#C084FC" />
        </g>

        <!-- 17. 数据库 / SQL (.sql, .db, .sqlite) -->
        <g v-else-if="resolvedType.key === 'database'">
            <ellipse cx="12" cy="6" rx="8" ry="3" fill="#0891B2" fill-opacity="0.2" stroke="#06B6D4" stroke-width="1.6" />
            <path d="M4 6V12C4 13.66 7.58 15 12 15C16.42 15 20 13.66 20 12V6M4 12V18C4 19.66 7.58 21 12 21C16.42 21 20 19.66 20 18V12" stroke="#06B6D4" stroke-width="1.6" />
        </g>

        <!-- 18. 终端命令 / Shell 脚本 (.sh, .bash, command) -->
        <g v-else-if="resolvedType.key === 'terminal'">
            <rect x="3" y="4" width="18" height="16" rx="3" fill="#059669" fill-opacity="0.16" stroke="#10B981" stroke-width="1.6" />
            <path d="M7 9L11 12L7 15M13 15H17" stroke="#34D399" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 19. 压缩包 (.zip, .rar, .7z, .tar, .gz) -->
        <g v-else-if="resolvedType.key === 'archive'">
            <path d="M4 4C4 2.89543 4.89543 2 6 2H18C19.1046 2 20 2.89543 20 4V20C20 21.1046 19.1046 22 18 22H6C4.89543 22 4 21.1046 4 20V4Z" fill="#D97706" fill-opacity="0.16" stroke="#F59E0B" stroke-width="1.6" />
            <path d="M10 3V5M14 4V6M10 7V9M14 8V10M10 11V13M14 12V14M11 16H13V19H11V16Z" stroke="#FBBF24" stroke-width="1.4" stroke-linecap="round" />
        </g>

        <!-- 20. 图片 (.png, .jpg, .jpeg, .webp, .svg, .gif) -->
        <g v-else-if="resolvedType.key === 'image'">
            <rect x="3" y="4" width="18" height="16" rx="3" fill="#DB2777" fill-opacity="0.16" stroke="#EC4899" stroke-width="1.6" />
            <circle cx="8.5" cy="8.5" r="1.5" fill="#F472B6" />
            <path d="M21 16L16 11L5 20H19C20.1 20 21 19.1 21 18V16Z" fill="#F472B6" fill-opacity="0.4" stroke="#EC4899" stroke-width="1.4" stroke-linejoin="round" />
        </g>

        <!-- 21. Reviewer 审查员 / 门禁机器徽标 -->
        <g v-else-if="resolvedType.key === 'reviewer'">
            <rect x="4" y="6" width="16" height="14" rx="3" fill="#4F46E5" fill-opacity="0.2" stroke="#6366F1" stroke-width="1.6" />
            <path d="M12 2V6M9 2H15M8 12H10M14 12H16M9 16H15M2 13H4M20 13H22" stroke="#818CF8" stroke-width="1.5" stroke-linecap="round" />
        </g>

        <!-- 22. 挂起等待 / 沙漏时钟 -->
        <g v-else-if="resolvedType.key === 'wait'">
            <path d="M6 3H18M6 21H18M7 3C7 9 12 12 12 12C12 12 17 9 17 3M7 21C7 15 12 12 12 12C12 12 17 15 17 21" stroke="#FBBF24" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
            <path d="M10 17H14M11 15H13" stroke="#FDE68A" stroke-width="1.4" stroke-linecap="round" />
        </g>

        <!-- 23. 代码变动操作：修改 (Modify / Edit) -->
        <g v-else-if="resolvedType.key === 'modify'">
            <path d="M12 20H21M16.5 3.5C17.3 2.7 18.7 2.7 19.5 3.5C20.3 4.3 20.3 5.7 19.5 6.5L7 19L3 20L4 16L16.5 3.5Z" fill="#3B82F6" fill-opacity="0.18" stroke="#60A5FA" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
        </g>

        <!-- 24. 代码变动操作：新建 (Create / Sparkles) -->
        <g v-else-if="resolvedType.key === 'create'">
            <path d="M12 3L13.8 8.2L19 10L13.8 11.8L12 17L10.2 11.8L5 10L10.2 8.2L12 3Z" fill="#10B981" fill-opacity="0.2" stroke="#34D399" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M19 16L19.8 18.2L22 19L19.8 19.8L19 22L18.2 19.8L16 19L18.2 18.2L19 16Z" fill="#34D399" />
        </g>

        <!-- 25. 搜索检索 (Search) -->
        <g v-else-if="resolvedType.key === 'search'">
            <circle cx="11" cy="11" r="7" stroke="#38BDF8" stroke-width="1.6" />
            <path d="M16.5 16.5L21 21" stroke="#38BDF8" stroke-width="1.8" stroke-linecap="round" />
        </g>

        <!-- 26. 纯读取上下文 (Read) -->
        <g v-else-if="resolvedType.key === 'read'">
            <path d="M4 19.5C4 18.1 5.1 17 6.5 17H20M4 19.5C4 20.9 5.1 22 6.5 22H20V4H6.5C5.1 4 4 5.1 4 6.5V19.5Z" fill="#6366F1" fill-opacity="0.16" stroke="#818CF8" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M8 8H16M8 12H13" stroke="#A5B4FC" stroke-width="1.4" stroke-linecap="round" />
        </g>

        <!-- 27. 默认通用文件 (Default Document) -->
        <g v-else>
            <path d="M4 4C4 2.89543 4.89543 2 6 2H14L20 8V20C20 21.1046 19.1046 22 18 22H6C4.89543 22 4 21.1046 4 20V4Z" fill="#64748B" fill-opacity="0.16" stroke="#94A3B8" stroke-width="1.6" stroke-linejoin="round" />
            <path d="M14 2V8H20" stroke="#94A3B8" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
            <path d="M8 13H16M8 17H13" stroke="#CBD5E1" stroke-width="1.4" stroke-linecap="round" />
        </g>
    </svg>
</template>

<script setup lang="ts">
import { computed } from 'vue';

const props = withDefaults(
    defineProps<{
        name?: string;
        ext?: string;
        kind?: string;
        size?: number | string;
        customClass?: string;
    }>(),
    {
        name: '',
        ext: '',
        kind: '',
        size: 14,
        customClass: '',
    }
);

const computedSize = computed(() => {
    if (typeof props.size === 'number') return `${props.size}px`;
    return props.size || '14px';
});

interface ResolvedFileType {
    key: string;
    brandColor: string;
    label: string;
}

const resolvedType = computed<ResolvedFileType>(() => {
    const rawKind = (props.kind || '').toLowerCase().trim();
    const rawName = (props.name || '').toLowerCase().trim();
    let ext = (props.ext || '').toLowerCase().trim();

    if (!ext && rawName.includes('.')) {
        ext = rawName.split('.').pop() || '';
    }

    // 优先匹配显式声明的语义 kind
    if (rawKind === 'excel' || ['xlsx', 'xls', 'csv', 'ods'].includes(ext) || rawName.includes('excel') || rawName.includes('表格')) {
        return { key: 'excel', brandColor: '#10B981', label: 'Excel' };
    }
    if (rawKind === 'word' || ['docx', 'doc', 'wps'].includes(ext) || rawName.includes('word') || rawName.includes('文档')) {
        return { key: 'word', brandColor: '#2563EB', label: 'Word' };
    }
    if (rawKind === 'pdf' || ext === 'pdf') {
        return { key: 'pdf', brandColor: '#EF4444', label: 'PDF' };
    }
    if (rawKind === 'ppt' || ['pptx', 'ppt', 'key'].includes(ext)) {
        return { key: 'ppt', brandColor: '#F97316', label: 'PPT' };
    }
    if (rawKind === 'reviewer') {
        return { key: 'reviewer', brandColor: '#6366F1', label: 'Reviewer' };
    }
    if (rawKind === 'wait') {
        return { key: 'wait', brandColor: '#FBBF24', label: '等待' };
    }
    if (rawKind === 'terminal' || rawKind === 'command' || ['sh', 'bash', 'zsh'].includes(ext)) {
        return { key: 'terminal', brandColor: '#10B981', label: '终端' };
    }
    if (rawKind === 'modify') {
        return { key: 'modify', brandColor: '#3B82F6', label: '修改' };
    }
    if (rawKind === 'create') {
        return { key: 'create', brandColor: '#10B981', label: '新建' };
    }
    if (rawKind === 'search') {
        return { key: 'search', brandColor: '#38BDF8', label: '检索' };
    }
    if (rawKind === 'read') {
        return { key: 'read', brandColor: '#818CF8', label: '读取' };
    }

    // 按文件后缀扩展名匹配现代工程语言与文档
    switch (ext) {
        case 'md':
        case 'markdown':
            return { key: 'markdown', brandColor: '#38BDF8', label: 'Markdown' };
        case 'ts':
        case 'tsx':
            return { key: 'typescript', brandColor: '#3B82F6', label: 'TypeScript' };
        case 'js':
        case 'jsx':
        case 'mjs':
        case 'cjs':
            return { key: 'javascript', brandColor: '#EAB308', label: 'JavaScript' };
        case 'vue':
            return { key: 'vue', brandColor: '#10B981', label: 'Vue' };
        case 'py':
        case 'ipynb':
            return { key: 'python', brandColor: '#38BDF8', label: 'Python' };
        case 'rs':
            return { key: 'rust', brandColor: '#F97316', label: 'Rust' };
        case 'go':
            return { key: 'go', brandColor: '#00ADD8', label: 'Go' };
        case 'c':
        case 'cpp':
        case 'cc':
        case 'cxx':
        case 'h':
        case 'hpp':
            return { key: 'cpp', brandColor: '#3B82F6', label: 'C/C++' };
        case 'java':
        case 'jar':
            return { key: 'java', brandColor: '#F59E0B', label: 'Java' };
        case 'html':
        case 'htm':
            return { key: 'html', brandColor: '#F97316', label: 'HTML' };
        case 'css':
        case 'scss':
        case 'sass':
        case 'less':
            return { key: 'css', brandColor: '#0284C7', label: 'CSS' };
        case 'json':
        case 'yaml':
        case 'yml':
        case 'toml':
        case 'env':
        case 'ini':
        case 'xml':
            return { key: 'config', brandColor: '#A855F7', label: 'Config' };
        case 'sql':
        case 'db':
        case 'sqlite':
        case 'prisma':
            return { key: 'database', brandColor: '#06B6D4', label: 'Database' };
        case 'zip':
        case 'rar':
        case '7z':
        case 'tar':
        case 'gz':
            return { key: 'archive', brandColor: '#F59E0B', label: 'Archive' };
        case 'png':
        case 'jpg':
        case 'jpeg':
        case 'webp':
        case 'svg':
        case 'gif':
        case 'ico':
        case 'bmp':
            return { key: 'image', brandColor: '#EC4899', label: 'Image' };
        default:
            return { key: 'default', brandColor: '#94A3B8', label: 'File' };
    }
});
</script>

<style scoped>
.file-icon-svg {
    display: inline-block;
    vertical-align: middle;
    flex-shrink: 0;
    transition: transform 0.15s ease;
}
</style>
