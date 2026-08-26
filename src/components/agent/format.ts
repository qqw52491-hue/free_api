import { marked } from "marked";
import hljs from "highlight.js/lib/common";

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
    if (!milliseconds || milliseconds <= 0) return "";
    if (milliseconds < 1000) return `${milliseconds}ms`;
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

export function renderMarkdown(content: string): string {
    if (!content) return "";
    try {
        const renderer = new marked.Renderer();
        renderer.code = function ({ text, lang }: { text: string; lang?: string }) {
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
                    <button class="code-copy-btn" onclick="navigator.clipboard.writeText(decodeURIComponent('${encodeURIComponent(text)}')).then(()=>{this.innerText='已复制';setTimeout(()=>this.innerText='复制',1800)})">复制</button>
                </div>
                <pre><code class="hljs language-${langLabel}">${highlighted}</code></pre>
            </div>`;
        };

        renderer.heading = function ({ text, depth }: { text: string; depth: number }) {
            const cleanText = (text || "").replace(/<[^>]+>/g, "").trim();
            const id = cleanText.toLowerCase().replace(/[^\w\u4e00-\u9fa5]+/g, "-");
            return `<h${depth} id="${id}">${text}</h${depth}>`;
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

            // 2. 本地文件与目录 (file:// 或 /Users/... 或 C:\...)
            const isLocal = href.startsWith("file://") || /^\/?[A-Za-z]:[\\/]/.test(href);
            if (isLocal) {
                const folderIconSvg = `<svg class="link-arrow-icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>`;
                return `<a href="${href}" data-local-path="${href}"${titleAttr} class="markdown-rich-link markdown-local-link">${folderIconSvg}${text}</a>`;
            }

            // 3. 外部网络链接
            const isHttp = /^https?:\/\//i.test(href);
            const iconSvg = isHttp
                ? `<svg class="link-arrow-icon" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/><polyline points="15 3 21 3 21 9"/><line x1="10" y1="14" x2="21" y2="3"/></svg>`
                : "";
            return `<a href="${href}"${titleAttr} class="markdown-rich-link" target="_blank" rel="noopener noreferrer">${text}${iconSvg}</a>`;
        };

        return marked.parse(content, {
            breaks: true,
            gfm: true,
            renderer,
        }) as string;
    } catch {
        return content.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/\n/g, "<br>");
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

    // 2. 终端命令与脚本
    if (t.includes("bash") || t.includes("shell") || t.includes("exec") || t.includes("cmd") || t.includes("terminal") || a.includes("exec")) {
        return { emoji: "⚡", color: "#facc15" };
    }

    // 3. 文件读写与代码
    if (t.includes("read_file") || t.includes("read_text") || t.includes("get_code") || a.includes("read")) {
        return { emoji: "📖", color: "#a78bfa" };
    }
    if (t.includes("write_file") || t.includes("edit_file") || t.includes("create_file") || t.includes("modify") || a.includes("write")) {
        return { emoji: "📝", color: "#f472b6" };
    }
    if (t.includes("search") || t.includes("ripgrep") || t.includes("grep") || t.includes("find") || a.includes("search")) {
        return { emoji: "🔍", color: "#fbbf24" };
    }
    if (t.includes("directory") || t.includes("tree") || t.includes("list_files") || t.includes("filesystem") || a.includes("list")) {
        return { emoji: "📁", color: "#fb923c" };
    }

    // 4. 表格、Excel 与图表
    if (t.includes("excel") || t.includes("workbook") || t.includes("sheet") || t.includes("chart") || a.includes("excel") || a.includes("chart")) {
        return { emoji: "📊", color: "#34d399" };
    }

    // 5. 数据集与数据库
    if (t.includes("task_store") || t.includes("sqlite") || t.includes("sql") || t.includes("db") || t.includes("database") || t.includes("get_step_output")) {
        return { emoji: "💾", color: "#2dd4bf" };
    }

    // 6. 人工干预与终态
    if (t.includes("ask_human") || t.includes("human") || t.includes("confirm")) {
        return { emoji: "👤", color: "#c084fc" };
    }
    if (t.includes("finish") || t.includes("done") || t.includes("deliver")) {
        return { emoji: "🎉", color: "#4ade80" };
    }

    // 7. 思考分析
    if (t.includes("thought") || t.includes("think") || t.includes("reason") || t.includes("arch")) {
        return { emoji: "🧠", color: "#e879f9" };
    }

    // 8. 默认 MCP 外部插件
    if (t.includes("/")) {
        return { emoji: "🧩", color: "#818cf8" };
    }

    return { emoji: "🔧", color: "#94a3b8" };
}

