import { marked } from "marked";
import { convertFileSrc } from "@tauri-apps/api/core";
import katex from "katex";
import "katex/dist/katex.min.css";
import hljs from "highlight.js/lib/common";

// 注册 KaTeX 数学公式解析扩展 ($行内公式$ 与 $$独立块级公式$$)
marked.use({
    extensions: [
        {
            name: "inlineKatex",
            level: "inline" as const,
            start(src: string) {
                return src.indexOf("$");
            },
            tokenizer(src: string) {
                // 1. 块级公式: $$ ... $$
                const blockMatch = src.match(/^\$\$([\s\S]+?)\$\$/);
                if (blockMatch) {
                    return {
                        type: "inlineKatex",
                        raw: blockMatch[0],
                        text: blockMatch[1].trim(),
                        displayMode: true,
                    };
                }
                // 2. 行内公式: $ ... $ (防止转义 \$ 以及纯货币符号 $50)
                const inlineMatch = src.match(/^\$((?:\\\$|[^\$\n])+?)\$/);
                if (inlineMatch && !/^\s*\d+/.test(inlineMatch[1])) {
                    return {
                        type: "inlineKatex",
                        raw: inlineMatch[0],
                        text: inlineMatch[1].trim(),
                        displayMode: false,
                    };
                }
                return undefined;
            },
            renderer(token: any) {
                try {
                    return katex.renderToString(token.text, {
                        displayMode: !!token.displayMode,
                        throwOnError: false,
                    });
                } catch {
                    return token.raw;
                }
            },
        },
    ],
});

export function asRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function toText(value: unknown): string {
    if (value === null || value === undefined) return "";
    if (typeof value === "string") return value;
    if (typeof value === "number" || typeof value === "boolean") return String(value);
    try {
        return JSON.stringify(value);
    } catch {
        return String(value);
    }
}

export function shortText(value: string, max: number): string {
    const trimmed = value.trim();
    return trimmed.length > max ? `${trimmed.slice(0, max)}\n\n…（内容较长，已截断显示剩余 ${trimmed.length - max} 字符）` : trimmed;
}

export function formatPayload(value: unknown): string {
    let text = "";
    if (typeof value === "string") {
        const trimmed = value.trim();
        if ((trimmed.startsWith("{") && trimmed.endsWith("}")) || (trimmed.startsWith("[") && trimmed.endsWith("]"))) {
            try {
                const parsed = JSON.parse(trimmed);
                text = JSON.stringify(parsed, null, 2);
            } catch {
                text = value;
            }
        } else {
            text = value;
        }
    } else {
        try {
            text = JSON.stringify(value, null, 2);
        } catch {
            text = toText(value);
        }
    }
    return shortText(text, 24000);
}

export function toTimestamp(value: unknown): number {
    if (typeof value === "number" && Number.isFinite(value)) return value > 10_000_000_000 ? value : value * 1000;
    if (typeof value === "string") {
        const parsed = Date.parse(value);
        if (!Number.isNaN(parsed)) return parsed;
    }
    return Date.now();
}

export function numberOrUndefined(value: unknown): number | undefined {
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? number : undefined;
}

export function formatDuration(seconds: number): string {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const remainder = seconds % 60;
    return hours
        ? `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`
        : `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}

export function formatMilliseconds(milliseconds?: number): string {
    if (milliseconds == null || Number.isNaN(milliseconds) || milliseconds < 0) return "";
    if (milliseconds === 0) return "< 1ms";
    if (milliseconds < 1000) return `${milliseconds}ms`;
    return `${(milliseconds / 1000).toFixed(1)}s`;
}

export function formatDurationSeconds(milliseconds?: number): string {
    if (milliseconds == null || Number.isNaN(milliseconds) || milliseconds < 0) return "";
    if (milliseconds < 100) return "< 0.1s";
    return `${(milliseconds / 1000).toFixed(1)}s`;
}

export function formatClock(timestamp: number): string {
    const d = new Date(timestamp);
    const h = String(d.getHours()).padStart(2, "0");
    const m = String(d.getMinutes()).padStart(2, "0");
    const s = String(d.getSeconds()).padStart(2, "0");
    return `${h}:${m}:${s}`;
}

export function cleanStepTitle(desc: string, tool: string, action: string): string {
    let t = (desc || "").replace(/\[(?:🧠|⚡|👁️)?\s*(?:Pro|Flash|Vision)\]/gi, "").trim();
    if (!t) {
        if (tool === "ask_human" || action === "ask_human") return "请求用户确认与输入";
        if (tool === "shell" || tool === "bash") return `执行命令: ${action}`;
        if (tool === "browser") return `浏览器操作: ${action}`;
        if (tool === "task_store") return `数据存储操作: ${action}`;
        return action && action !== tool ? `${tool}: ${action}` : tool || "执行步骤";
    }
    return t;
}

export function extractStepModelBadge(desc: string, source: string): string | null {
    const match = (desc || "").match(/\[((?:🧠|⚡|👁️)?\s*(?:Pro|Flash|Vision))\]/i);
    if (match) return match[1].trim();
    if (source && source !== "内置") return source;
    return null;
}

export function formatNumber(value?: number): string {
    return new Intl.NumberFormat("zh-CN").format(value || 0);
}

function isNpmPackageVersion(str: string): boolean {
    const raw = str.replace(/^mailto:/i, "").trim();
    // 匹配形如 plugin-vue@5.2.4, @vitejs/plugin-vue@5.2.4, vue@3.5.26, vite@6.4.1 等
    if (/^@?[a-zA-Z0-9_\-\.\/]+@[\^~>=<vV]?\d+/i.test(raw)) {
        return true;
    }
    const parts = raw.split("@");
    if (parts.length >= 2) {
        const domain = parts[parts.length - 1];
        // 如果 @ 后面以数字开头，或者末尾是数字（如 .4, .26），是版本号而不是邮箱
        if (/^\d/.test(domain) || /\.\d+$/.test(domain)) return true;
        if (!/\.[a-zA-Z]{2,}$/.test(domain)) return true;
    }
    return false;
}

const KNOWN_FILE_EXTENSIONS = new Set([
    "rs", "vue", "ts", "tsx", "js", "mjs", "cjs", "jsx",
    "css", "scss", "sass", "less",
    "toml", "lock", "json", "yaml", "yml", "ini", "conf", "env",
    "md", "markdown", "mdx",
    "html", "htm", "xml", "svg",
    "sql", "db", "sqlite",
    "py", "ipynb", "go", "java", "c", "cpp", "cc", "cxx", "h", "hpp",
    "sh", "bash", "zsh",
]);

const KNOWN_EXACT_FILENAMES = new Set([
    "cargo.toml", "cargo.lock", "package.json", "package-lock.json",
    "pnpm-lock.yaml", "yarn.lock", "tsconfig.json", "dockerfile",
    "makefile", "cmakelists.txt", "agents.md", "readme.md",
]);

/**
 * 根据文件路径或文件名生成行内 14×14 语言/文件类型专属 SVG 图标
 */
export function getInlineFileIconSvg(rawPath: string): string | null {
    const cleaned = rawPath
        .trim()
        .replace(/^file:\/\//i, "")
        .split(/[?#]/)[0]
        .replace(/:\d+(?::\d+)?(-\d+)?$/, "")
        .trim();
    if (!cleaned) return null;

    const baseName = (cleaned.split(/[\\/]/).pop() || "").toLowerCase();
    if (!baseName) return null;

    const dotIdx = baseName.lastIndexOf(".");
    const ext = dotIdx > 0 ? baseName.slice(dotIdx + 1) : "";

    if (baseName.endsWith(".lock") || baseName === "cargo.lock" || baseName === "package-lock.json" || baseName === "pnpm-lock.yaml" || baseName === "yarn.lock") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><rect x="4.5" y="11" width="15" height="10" rx="2.2" stroke="#7AA2F7" stroke-width="2"/><path d="M8 11V7.5C8 5.29 9.79 3.5 12 3.5C14.21 3.5 16 5.29 16 7.5V11" stroke="#7AA2F7" stroke-width="2" stroke-linecap="round"/></svg>`;
    }

    if (ext === "rs") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><circle cx="12" cy="12" r="10" fill="#E4682A"/><path d="M8.5 16.5V7.5H12.6C14.5 7.5 15.6 8.5 15.6 10.1C15.6 11.4 14.8 12.3 13.5 12.6L16 16.5H13.6L11.4 12.8H10.6V16.5H8.5ZM10.6 9.2V11.3H12.3C13.1 11.3 13.6 10.9 13.6 10.2C13.6 9.6 13.1 9.2 12.3 9.2H10.6Z" fill="#18181B"/></svg>`;
    }

    if (ext === "vue") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M2 3.5H5.8L12 14.2L18.2 3.5H22L12 20.8L2 3.5Z" fill="#41B883"/><path d="M5.8 3.5H9.4L12 8L14.6 3.5H18.2L12 14.2L5.8 3.5Z" fill="#35495E"/></svg>`;
    }

    if (ext === "css" || ext === "scss" || ext === "sass" || ext === "less") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M8.5 4C6.8 4 6 4.9 6 6.5V9.5C6 10.8 5.1 11.6 3.8 12C5.1 12.4 6 13.2 6 14.5V17.5C6 19.1 6.8 20 8.5 20M15.5 4C17.2 4 18 4.9 18 6.5V9.5C18 10.8 18.9 11.6 20.2 12C18.9 12.4 18 13.2 18 14.5V17.5C18 19.1 17.2 20 15.5 20" stroke="#38BDF8" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "toml" || ext === "yaml" || ext === "yml" || ext === "ini" || ext === "conf" || ext === "env" || baseName === "dockerfile" || baseName === "makefile" || baseName === "cmakelists.txt") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M12 15.2A3.2 3.2 0 1 0 12 8.8A3.2 3.2 0 0 0 12 15.2Z" stroke="#8B9BB4" stroke-width="2.1"/><path d="M19.4 15A1.65 1.65 0 0 0 19.73 16.82L19.79 16.88A2 2 0 0 1 16.96 19.71L16.9 19.65A1.65 1.65 0 0 0 15.08 19.32A1.65 1.65 0 0 0 14 20.85V21A2 2 0 0 1 10 21V20.91A1.65 1.65 0 0 0 8.92 19.4A1.65 1.65 0 0 0 7.1 19.73L7.04 19.79A2 2 0 0 1 4.21 16.96L4.27 16.9A1.65 1.65 0 0 0 4.6 15.08A1.65 1.65 0 0 0 3.09 14H3A2 2 0 0 1 3 10H3.09A1.65 1.65 0 0 0 4.6 8.92A1.65 1.65 0 0 0 4.27 7.1L4.21 7.04A2 2 0 0 1 7.04 4.21L7.1 4.27A1.65 1.65 0 0 0 8.92 4.6H9A1.65 1.65 0 0 0 10 3.09V3A2 2 0 0 1 14 3V3.09A1.65 1.65 0 0 0 15 4.6A1.65 1.65 0 0 0 16.82 4.27L16.88 4.21A2 2 0 0 1 19.71 7.04L19.65 7.1A1.65 1.65 0 0 0 19.32 8.92V9A1.65 1.65 0 0 0 20.85 10H21A2 2 0 0 1 21 14H20.91A1.65 1.65 0 0 0 19.4 15Z" stroke="#8B9BB4" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "ts" || ext === "tsx") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><rect x="2.5" y="2.5" width="19" height="19" rx="3.5" fill="#3178C6"/><path d="M6.2 10H11.8M9 10V17.5M17.8 11.2C17.1 10.5 15.8 10.2 14.7 10.9C13.7 11.5 13.9 12.8 15 13.4L16.2 14C17.4 14.6 17.8 15.7 17 16.8C16.2 17.8 14.3 17.8 13.2 16.8" stroke="#FFFFFF" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "js" || ext === "mjs" || ext === "cjs" || ext === "jsx") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><rect x="2.5" y="2.5" width="19" height="19" rx="3.5" fill="#EAB308"/><path d="M10.2 10.2V15.5C10.2 16.8 9.2 17.5 7.8 16.8M17.5 11.2C16.8 10.5 15.5 10.2 14.5 10.9C13.5 11.5 13.8 12.8 14.8 13.4L16 14C17.1 14.6 17.5 15.7 16.7 16.8C15.9 17.8 14.1 17.8 13 16.8" stroke="#18181B" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "json") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M8.5 4C6.8 4 6 4.9 6 6.5V9.5C6 10.8 5.1 11.6 3.8 12C5.1 12.4 6 13.2 6 14.5V17.5C6 19.1 6.8 20 8.5 20M15.5 4C17.2 4 18 4.9 18 6.5V9.5C18 10.8 18.9 11.6 20.2 12C18.9 12.4 18 13.2 18 14.5V17.5C18 19.1 17.2 20 15.5 20" stroke="#FBBF24" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "md" || ext === "markdown" || ext === "mdx") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><rect x="2.5" y="4.5" width="19" height="15" rx="2.5" stroke="#60A5FA" stroke-width="2"/><path d="M6 15V9L8.8 12.2L11.6 9V15M16.5 9V15M16.5 15L14.3 12.8M16.5 15L18.7 12.8" stroke="#60A5FA" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "html" || ext === "htm" || ext === "xml" || ext === "svg") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M8 7L3 12L8 17M16 7L21 12L16 17M13.5 5L10.5 19" stroke="#FB923C" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "sql" || ext === "db" || ext === "sqlite") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><ellipse cx="12" cy="6" rx="7.5" ry="3" stroke="#22D3EE" stroke-width="2"/><path d="M4.5 6V12C4.5 13.66 7.86 15 12 15C16.14 15 19.5 13.66 19.5 12V6M4.5 12V18C4.5 19.66 7.86 21 12 21C16.14 21 19.5 19.66 19.5 18V12" stroke="#22D3EE" stroke-width="2"/></svg>`;
    }

    if (ext === "py" || ext === "ipynb") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M11.8 3C7.5 3 7.8 4.9 7.8 4.9V6.9H12.2V7.5H5.8C3.9 7.5 3 8.8 3 11.2C3 13.6 4.3 13.4 4.3 13.4H5.8V11.5C5.8 9.8 7.2 9.8 7.2 9.8H12.2C13.5 9.8 14.5 8.8 14.5 7.5V4.9C14.5 3.6 13.5 3 11.8 3Z" fill="#38BDF8"/><path d="M12.2 21C16.5 21 16.2 19.1 16.2 19.1V17.1H11.8V16.5H18.2C20.1 16.5 21 15.2 21 12.8C21 10.4 19.7 10.6 19.7 10.6H18.2V12.5C18.2 14.2 16.8 14.2 16.8 14.2H11.8C10.5 14.2 9.5 15.2 9.5 16.5V19.1C9.5 20.4 10.5 21 12.2 21Z" fill="#FACC15"/></svg>`;
    }

    if (ext === "sh" || ext === "bash" || ext === "zsh") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><rect x="3" y="4" width="18" height="16" rx="3" stroke="#34D399" stroke-width="2"/><path d="M7 9L11 12L7 15M13 15H17" stroke="#34D399" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
    }

    if (ext === "go" || ext === "java" || ext === "c" || ext === "cpp" || ext === "cc" || ext === "cxx" || ext === "h" || ext === "hpp") {
        return `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M14 2H6A2 2 0 0 0 4 4V20A2 2 0 0 0 6 22H18A2 2 0 0 0 20 20V8L14 2Z" stroke="#60A5FA" stroke-width="2" stroke-linejoin="round"/><path d="M14 2V8H20" stroke="#60A5FA" stroke-width="2" stroke-linejoin="round"/></svg>`;
    }

    return null;
}

/**
 * 判断行内反引号文本是否为源码/配置文件引用（如 `src-tauri/src/lib.rs`, `src/components/AgentPanel.vue`, `Cargo.toml`）
 */
function isInlineFileReference(rawText: string): boolean {
    const s = rawText.trim();
    if (!s || s.length < 3 || s.length > 180 || /\s/.test(s)) return false;
    if (/^(?:https?|mailto|ftp|ws|wss|data|blob|javascript):/i.test(s)) return false;
    if (s.startsWith("--") || s.startsWith("$") || (s.includes("@") && !s.startsWith("@/"))) return false;
    if (/[()=;{}<>`"'|&*?]/.test(s)) return false;

    const withoutLine = s.split("#")[0].replace(/:\d+(?::\d+)?(-\d+)?$/, "");
    const baseName = (withoutLine.split(/[\\/]/).pop() || "").toLowerCase();
    if (!baseName) return false;

    if (KNOWN_EXACT_FILENAMES.has(baseName)) return true;

    const dotIdx = baseName.lastIndexOf(".");
    if (dotIdx <= 0) return false;
    const ext = baseName.slice(dotIdx + 1);
    if (!KNOWN_FILE_EXTENSIONS.has(ext)) return false;

    // 若包含路径分隔符（如 src/components/AgentPanel.vue），且扩展名命中已知文件后缀，直接判定为文件路径
    if (withoutLine.includes("/") || withoutLine.includes("\\")) {
        return true;
    }

    // 单个文件名（无路径分隔符）：限制多点链式属性访问（仅允许 .test. / .spec. / .min. / .d. / .conf. / .config.）
    const stem = baseName.slice(0, dotIdx);
    if (!stem.includes(".")) return true;
    return /\.(?:test|spec|min|d|conf|config|module|stories)$/i.test(stem);
}

/**
 * 流式输出平滑预处理器：在编译前虚拟补全未闭合的代码块、数学公式与排版标记，防止打字机流式输出时界面剧烈抖动与闪白
 */
export function normalizeStreamingMarkdown(raw: string): string {
    if (!raw) return "";
    let processed = raw;

    // 💡 防御性修复：自动将文本中的字面量 \n 还原为真正的换行符
    if (processed.includes("\\n")) {
        processed = processed.replace(/\\n/g, "\n");
    }
    if (processed.includes("\\t")) {
        processed = processed.replace(/\\t/g, "\t");
    }

    // 1. 预测并补全未闭合的多行代码块 (``` 或 ~~~)
    // 严格遵循 CommonMark 规范逐行追踪围栏状态，杜绝行内反引号、正则字面量导致的奇偶倒错误判
    const lines = processed.split(/\r?\n/);
    let openFence: string | null = null;
    for (const line of lines) {
        // CommonMark 要求代码围栏前导空格必须少于 4 个
        const indentMatch = line.match(/^ {0,3}/);
        if (!indentMatch) continue;
        const trimmed = line.slice(indentMatch[0].length);

        if (trimmed.startsWith("```") || trimmed.startsWith("~~~")) {
            const fenceChar = trimmed[0];
            const match = trimmed.match(/^(`{3,}|~{3,})/);
            if (match) {
                const fence = match[1];
                const rest = trimmed.slice(fence.length);
                if (!openFence) {
                    // CommonMark 规范：开启围栏的代码块 info string 中不能包含相同的围栏符号
                    if (!rest.includes(fenceChar)) {
                        openFence = fence;
                    }
                } else if (openFence[0] === fenceChar && fence.length >= openFence.length && rest.trim() === "") {
                    // 闭合围栏必须使用相同字符且长度不小于开启围栏，且行内无多余内容
                    openFence = null;
                }
            }
        }
    }
    if (openFence) {
        processed += `\n${openFence}`;
    }

    // 2. 预测并补全未闭合的独立块级数学公式 ($$) 与行内公式 ($)
    // 💡 关键防护：必须先排除所有代码块（多行围栏与行内代码），避免将代码/终端命令中的 $ (如 bash 提示符 $、变量 $HOME) 误判为数学公式
    const contentOutsideCode = processed
        .replace(/```[\s\S]*?```/g, "")
        .replace(/~~~[\s\S]*?~~~/g, "")
        .replace(/`[^`\n]+`/g, "");

    const blockMathMatches = contentOutsideCode.match(/\$\$/g);
    if (blockMathMatches && blockMathMatches.length % 2 !== 0) {
        processed += "\n$$";
    }

    // 3. 预测并补全未闭合的单美元行内公式 ($ ... $)
    // 必须仅检测真正的行内公式起始 (如 $E=mc^2，前导非单词非转义、紧随非空字符非数字)，排除货币 $100 与终端提示 $
    const strippedBlock = contentOutsideCode.replace(/\$\$[\s\S]*?\$\$/g, "");
    const unclosedInlineMath = /(?<![\w\$\\])\$(?!\s|\d)(?:[^\$\n]|\\\$)*$/.test(strippedBlock);
    if (unclosedInlineMath) {
        processed += "$";
    }

    // 4. 预测并补全未闭合的加粗标记 (**)
    const boldMatches = contentOutsideCode.match(/\*\*/g);
    if (boldMatches && boldMatches.length % 2 !== 0) {
        processed += "**";
    }

    return processed;
}

const markdownCache = new Map<string, string>();
const MAX_MARKDOWN_CACHE_SIZE = 800;
const MAX_CACHEABLE_LENGTH = 60000;

export function resolveMarkdownMediaSource(href: string): string {
    if (/^file:\/\//i.test(href)) return convertFileSrc(href.replace(/^file:\/\//i, ""));
    if (href.startsWith("/") || /^[A-Za-z]:[\\/]/.test(href)) return convertFileSrc(href);
    return href;
}

export function renderMarkdown(content: string): string {
    if (!content) return "";
    const cached = markdownCache.get(content);
    if (cached !== undefined) {
        markdownCache.delete(content);
        markdownCache.set(content, cached);
        return cached;
    }
    try {
        const sanitizedContent = normalizeStreamingMarkdown(content);
        const renderer = new marked.Renderer();
        renderer.code = function ({ text, lang }: { text: string; lang?: string }) {
            const normalizedLang = (lang || "").trim().toLowerCase();

            // 1. 专业 Diff 代码变更对比代码块
            if (normalizedLang === "diff") {
                const lines = text.split("\n");
                let addCount = 0;
                let delCount = 0;

                const renderedLines = lines.map((line) => {
                    const escaped = line.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
                    if (line.startsWith("+") && !line.startsWith("+++")) {
                        addCount++;
                        return `<div class="diff-line diff-add"><span class="diff-sign">+</span><span class="diff-content">${escaped.slice(1)}</span></div>`;
                    }
                    if (line.startsWith("-") && !line.startsWith("---")) {
                        delCount++;
                        return `<div class="diff-line diff-del"><span class="diff-sign">-</span><span class="diff-content">${escaped.slice(1)}</span></div>`;
                    }
                    if (line.startsWith("@@")) {
                        return `<div class="diff-line diff-hunk"><span class="diff-sign"> </span><span class="diff-content">${escaped}</span></div>`;
                    }
                    if (line.startsWith("+++") || line.startsWith("---")) {
                        return `<div class="diff-line diff-header"><span class="diff-sign"> </span><span class="diff-content">${escaped}</span></div>`;
                    }
                    return `<div class="diff-line diff-ctx"><span class="diff-sign"> </span><span class="diff-content">${escaped}</span></div>`;
                }).join("");

                return `<div class="code-block-wrapper diff-block-wrapper">
                    <div class="code-block-header diff-block-header">
                        <div class="diff-header-left">
                            <span class="code-lang-label diff-label">DIFF 变更对比</span>
                            <span class="diff-stat-badge diff-stat-add">+${addCount}</span>
                            <span class="diff-stat-badge diff-stat-del">-${delCount}</span>
                        </div>
                        <button class="code-copy-btn" data-code="${encodeURIComponent(text)}">复制补丁</button>
                    </div>
                    <div class="diff-code-body">${renderedLines}</div>
                </div>`;
            }

            const validLang = lang && hljs.getLanguage(lang) ? lang : "";
            let highlighted = "";
            try {
                highlighted = validLang
                    ? hljs.highlight(text, { language: validLang, ignoreIllegals: true }).value
                    : hljs.highlightAuto(text).value;
            } catch {
                highlighted = text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
            }
            const langLabel = validLang || "code";
            return `<div class="code-block-wrapper">
                <div class="code-block-header">
                    <span class="code-lang-label">${langLabel}</span>
                    <button class="code-copy-btn" data-code="${encodeURIComponent(text)}">复制</button>
                </div>
                <pre><code class="hljs language-${langLabel}">${highlighted}</code></pre>
            </div>`;
        };

        // 2. GitHub Alerts 规范呼出提示块 (> [!NOTE] / > [!TIP] / > [!IMPORTANT] / > [!WARNING] / > [!CAUTION])
        renderer.blockquote = function (this: any, arg: any) {
            const innerHtml = arg.tokens && this.parser ? this.parser.parse(arg.tokens) : (arg.text || "");
            const alertMatch = innerHtml.match(/^\s*<p>\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*(?:<br\s*\/?>|\n)?([\s\S]*?)<\/p>([\s\S]*)$/i);
            if (alertMatch) {
                const type = alertMatch[1].toUpperCase();
                const firstPara = alertMatch[2].trim();
                const rest = alertMatch[3] || "";
                const contentHtml = (firstPara ? `<p>${firstPara}</p>` : "") + rest;

                const alertConfigs: Record<string, { label: string; cls: string; icon: string }> = {
                    NOTE: {
                        label: "提示 (Note)",
                        cls: "alert-note",
                        icon: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>`,
                    },
                    TIP: {
                        label: "建议 (Tip)",
                        cls: "alert-tip",
                        icon: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z"/></svg>`,
                    },
                    IMPORTANT: {
                        label: "重要 (Important)",
                        cls: "alert-important",
                        icon: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><circle cx="12" cy="12" r="10"/><polygon points="12 8 8 12 12 16 16 12 12 8"/></svg>`,
                    },
                    WARNING: {
                        label: "警告 (Warning)",
                        cls: "alert-warning",
                        icon: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>`,
                    },
                    CAUTION: {
                        label: "危险 (Caution)",
                        cls: "alert-caution",
                        icon: `<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>`,
                    },
                };
                const config = alertConfigs[type] || alertConfigs.NOTE;
                return `<div class="markdown-alert ${config.cls}">
                    <div class="markdown-alert-header">
                        <span class="markdown-alert-icon">${config.icon}</span>
                        <span class="markdown-alert-title">${config.label}</span>
                    </div>
                    <div class="markdown-alert-body">${contentHtml}</div>
                </div>`;
            }
            return `<blockquote>${innerHtml}</blockquote>`;
        };

        renderer.heading = function ({ text, depth }: { text: string; depth: number }) {
            const cleanText = (text || "").replace(/<[^>]+>/g, "").trim();
            const id = cleanText
                .toLowerCase()
                .replace(/[^\w\u4e00-\u9fa5]+/g, "-")
                .replace(/^-+|-+$/g, "");
            return `<h${depth} id="${id}" data-heading-text="${cleanText}">${text}</h${depth}>`;
        };

        renderer.codespan = function ({ text }: { text: string }) {
            const raw = (text || "")
                .replace(/&amp;/g, "&")
                .replace(/&lt;/g, "<")
                .replace(/&gt;/g, ">")
                .replace(/&quot;/g, '"')
                .replace(/&#39;/g, "'");
            const escaped = raw
                .replace(/&/g, "&amp;")
                .replace(/</g, "&lt;")
                .replace(/>/g, "&gt;")
                .replace(/"/g, "&quot;");

            if (isInlineFileReference(raw)) {
                const iconSvg = getInlineFileIconSvg(raw);
                if (iconSvg) {
                    return `<span class="markdown-file-ref">${iconSvg}<span class="markdown-file-ref-name">${escaped}</span></span>`;
                }
            }
            return `<code>${escaped}</code>`;
        };

        renderer.link = function ({ href, title, text }: { href: string; title?: string | null; text: string }) {
            // 过滤 marked 对 npm 依赖包版本号（如 @vitejs/plugin-vue@5.2.4 或 vue@3.5.26）的邮箱误判
            if (href.startsWith("mailto:") && isNpmPackageVersion(href)) {
                return `<code class="pkg-version-tag">${text}</code>`;
            }

            const titleAttr = title ? ` title="${title}"` : "";

            // 1. 文档大纲/目录锚点跳转 (TOC, 如 #1-架构概述)
            if (href.startsWith("#")) {
                return `<a href="${href}"${titleAttr} class="markdown-rich-link markdown-anchor-link">${text}</a>`;
            }

            // 如果链接内部是图片（例如 [![A](img)](/local/report.md)），保留原始内部 HTML
            if (/<img\b|<video\b|<audio\b/i.test(text || "")) {
                return `<a href="${href}"${titleAttr} class="markdown-rich-link">${text}</a>`;
            }

            const plainText = (text || "").replace(/<[^>]+>/g, "").trim() || href;
            const fileIconSvg = getInlineFileIconSvg(href) || getInlineFileIconSvg(plainText);

            // 2. 本地文件与目录 (file:// 或 /Users/... 或 C:\... 或相对源码文件路径)
            const isLocal =
                href.startsWith("file://") ||
                href.startsWith("/") ||
                /^\/?[A-Za-z]:[\\/]/.test(href) ||
                (!/^(?:https?|mailto|ftp|ws|wss|javascript|data|blob):/i.test(href) && Boolean(fileIconSvg));
            if (isLocal) {
                const folderIconSvg = `<svg class="md-file-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#8B9BB4" stroke-width="2" aria-hidden="true"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>`;
                return `<a href="${href}" data-local-path="${href}"${titleAttr} class="markdown-rich-link markdown-local-link markdown-file-ref">${fileIconSvg || folderIconSvg}<span class="markdown-file-ref-name">${plainText}</span></a>`;
            }

            // 3. 外部网络链接
            const isHttp = /^https?:\/\//i.test(href);
            const iconSvg = isHttp
                ? `<svg class="link-arrow-icon" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>`
                : "";
            return `<a href="${href}"${titleAttr} class="markdown-rich-link" target="_blank" rel="noopener noreferrer">${text}${iconSvg}</a>`;
        };

        // 4. 本地媒体：![](/abs/path) / ![](file://…) / 相对工程文件
        //    本地绝对路径走 convertFileSrc（asset:// 协议），远程/内嵌原样透传。
        //    扩展名分流：<video>/<audio>/<img>——回复里嵌生成物的最后一块砖。
        renderer.image = function ({ href, title, text }: { href: string; title?: string | null; text: string }) {
            const escAttr = (s: string) => s.replace(/&/g, "&amp;").replace(/"/g, "&quot;");
            const safeText = escAttr(text || "");
            const titleAttr = title ? ` title="${escAttr(title)}"` : "";

            const src = resolveMarkdownMediaSource(href);
            const safeSrc = escAttr(src);
            const cleanHref = href.split(/[?#]/)[0];
            if (/\.(mp4|webm|mov|mkv|avi)$/i.test(cleanHref)) {
                return `<video controls preload="metadata" class="md-media md-video" src="${safeSrc}"${titleAttr}></video>`;
            }
            if (/\.(mp3|wav|ogg|m4a|flac|aac)$/i.test(cleanHref)) {
                return `<audio controls preload="metadata" class="md-media md-audio" src="${safeSrc}"${titleAttr}></audio>`;
            }
            return `<img src="${safeSrc}" alt="${safeText}"${titleAttr} class="md-img" loading="lazy" />`;
        };

        const html = marked.parse(sanitizedContent, {
            breaks: true,
            gfm: true,
            renderer,
        }) as string;
        if (content.length <= MAX_CACHEABLE_LENGTH) {
            if (markdownCache.size >= MAX_MARKDOWN_CACHE_SIZE) {
                const oldestKeys = Array.from(markdownCache.keys()).slice(0, 100);
                for (const k of oldestKeys) markdownCache.delete(k);
            }
            markdownCache.set(content, html);
        }
        return html;
    } catch {
        const fallback = content.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/\n/g, "<br>");
        if (content.length <= MAX_CACHEABLE_LENGTH) {
            markdownCache.set(content, fallback);
        }
        return fallback;
    }
}

export function getToolIcon(tool?: string, action?: string): { emoji: string; color: string } {
    const t = (tool || "").toLowerCase();
    const a = (action || "").toLowerCase();

    // 1. 网络与网页
    if (t.includes("web_search") || t.includes("bing") || t.includes("google") || t.includes("duckduckgo")) {
        return { emoji: "🌐", color: "#38bdf8" };
    }
    if (t.includes("web_fetch") || t.includes("fetch") || t.includes("http") || t.includes("download") || t.includes("curl") || a.includes("fetch")) {
        return { emoji: "🌍", color: "#60a5fa" };
    }

    // 2. 终端命令与脚本 (包含 local_shell)
    if (t.includes("local_shell") || t.includes("bash") || t.includes("shell") || t.includes("exec") || t.includes("cmd") || t.includes("terminal") || a.includes("exec")) {
        return { emoji: "💻", color: "#38bdf8" };
    }

    // 3. 原生本地文件操作 (local_file 动作分支)
    if (t.includes("local_file")) {
        if (a.includes("edit") || a.includes("modify") || a.includes("replace")) {
            return { emoji: "✏️", color: "#f472b6" };
        }
        if (a.includes("write") || a.includes("create")) {
            return { emoji: "📝", color: "#f472b6" };
        }
        if (a.includes("list") || a.includes("dir") || a.includes("tree")) {
            return { emoji: "📁", color: "#fb923c" };
        }
        return { emoji: "📖", color: "#a78bfa" };
    }

    // 3.1 通用文件读写与代码
    if (t.includes("read_file") || t.includes("read_text") || t.includes("view_file") || t.includes("get_code") || a.includes("read")) {
        return { emoji: "📖", color: "#a78bfa" };
    }
    if (t.includes("edit_file") || a.includes("edit")) {
        return { emoji: "✏️", color: "#f472b6" };
    }
    if (t.includes("write_file") || t.includes("create_file") || t.includes("modify") || a.includes("write")) {
        return { emoji: "📝", color: "#f472b6" };
    }
    if (t.includes("search") || t.includes("ripgrep") || t.includes("grep") || t.includes("find") || a.includes("search")) {
        return { emoji: "🔍", color: "#fbbf24" };
    }
    if (t.includes("directory") || t.includes("tree") || t.includes("list_files") || t.includes("list_dir") || t.includes("filesystem") || a.includes("list")) {
        return { emoji: "📁", color: "#fb923c" };
    }

    // 4. 表格、Excel 与图表
    if (t.includes("excel") || t.includes("workbook") || t.includes("sheet") || t.includes("chart") || a.includes("excel") || a.includes("chart")) {
        return { emoji: "📊", color: "#34d399" };
    }

    // 5. 截断输出取回
    if (t.includes("get_step_output")) {
        return { emoji: "📜", color: "#cbd5e1" };
    }

    // 5.1 数据集与数据库
    if (t.includes("task_store") || t.includes("sqlite") || t.includes("sql") || t.includes("db") || t.includes("database")) {
        return { emoji: "💾", color: "#2dd4bf" };
    }

    // 6. 人工干预与终态
    if (t.includes("ask_human") || t.includes("human") || t.includes("confirm")) {
        return { emoji: "👤", color: "#c084fc" };
    }
    if (t === "finish" || t.includes("finish") || t.includes("done") || t.includes("deliver")) {
        return { emoji: "🎉", color: "#4ade80" };
    }

    // 7. 子智能体与异步挂起
    if (t.includes("child_writer")) {
        return { emoji: "✍️", color: "#f472b6" };
    }
    if (t.includes("child_reviewer") || t.includes("sub_agent") || t.includes("child_worker") || t.includes("investigate")) {
        return { emoji: "🤖", color: "#a855f7" };
    }
    if (t.includes("resume_child")) {
        return { emoji: "🔄", color: "#8b5cf6" };
    }
    if (t.includes("wait_jobs") || t.includes("wait_job") || a.includes("wait_jobs")) {
        return { emoji: "⏳", color: "#f59e0b" };
    }

    // 8. 思考分析
    if (t.includes("thought") || t.includes("think") || t.includes("reason") || t.includes("arch")) {
        return { emoji: "🧠", color: "#e879f9" };
    }

    // 9. 产物交付、系统配置与管理工具
    if (t.includes("artifact")) {
        return { emoji: "📦", color: "#fb923c" };
    }
    if (t.includes("model_manager")) {
        return { emoji: "⚙️", color: "#94a3b8" };
    }
    if (t.includes("skill_manager")) {
        return { emoji: "🧠", color: "#ec4899" };
    }
    if (t.includes("mcp_manager")) {
        return { emoji: "🔌", color: "#6366f1" };
    }
    if (t.includes("role_manager")) {
        return { emoji: "🎭", color: "#f43f5e" };
    }
    if (t.includes("schedule_manager")) {
        return { emoji: "⏰", color: "#eab308" };
    }
    if (t.includes("project_manager")) {
        return { emoji: "🏗️", color: "#0ea5e9" };
    }
    if (t.includes("project_resources") || t.includes("register_project_resource")) {
        return { emoji: "📚", color: "#38bdf8" };
    }
    if (t.includes("watch_start") || t.includes("watch_stop") || t.includes("watch")) {
        return { emoji: "👁️", color: "#06b6d4" };
    }
    if (t.includes("system_info")) {
        return { emoji: "💻", color: "#64748b" };
    }
    if (t.includes("wechat")) {
        return { emoji: "💬", color: "#22c55e" };
    }

    // 10. 默认 MCP 外部插件
    if (t.includes("/")) {
        return { emoji: "🧩", color: "#818cf8" };
    }

    return { emoji: "🔧", color: "#94a3b8" };
}

