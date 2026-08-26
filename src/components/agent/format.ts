import { marked } from "marked";
import katex from "katex";
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

/**
 * 流式输出平滑预处理器：在编译前虚拟补全未闭合的代码块、数学公式与排版标记，防止打字机流式输出时界面剧烈抖动与闪白
 */
export function normalizeStreamingMarkdown(raw: string): string {
    if (!raw) return "";
    let processed = raw;

    // 1. 预测并补全未闭合的代码块 (```)
    const codeBlockMatches = processed.match(/```/g);
    if (codeBlockMatches && codeBlockMatches.length % 2 !== 0) {
        // 如果有奇数个 ```，说明末尾有一个正在流式输出的代码块，虚拟追加闭合符
        processed += "\n```";
    }

    // 2. 预测并补全未闭合的独立块级数学公式 ($$)
    const blockMathMatches = processed.match(/\$\$/g);
    if (blockMathMatches && blockMathMatches.length % 2 !== 0) {
        processed += "\n$$";
    }

    // 3. 预测并补全未闭合的单美元行内公式 ($ ... $)
    const strippedBlock = processed.replace(/\$\$[\s\S]*?\$\$/g, "");
    const singleDollarMatches = strippedBlock.match(/(?<!\\)\$/g);
    if (singleDollarMatches && singleDollarMatches.length % 2 !== 0) {
        processed += "$";
    }

    // 4. 预测并补全未闭合的加粗标记 (**)
    const boldMatches = processed.match(/\*\*/g);
    if (boldMatches && boldMatches.length % 2 !== 0) {
        processed += "**";
    }

    return processed;
}

export function renderMarkdown(content: string): string {
    if (!content) return "";
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
                        <button class="code-copy-btn" onclick="navigator.clipboard.writeText(decodeURIComponent('${encodeURIComponent(text)}')).then(()=>{this.innerText='已复制';setTimeout(()=>this.innerText='复制',1800)})">复制补丁</button>
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
                    <button class="code-copy-btn" onclick="navigator.clipboard.writeText(decodeURIComponent('${encodeURIComponent(text)}')).then(()=>{this.innerText='已复制';setTimeout(()=>this.innerText='复制',1800)})">复制</button>
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

        return marked.parse(sanitizedContent, {
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

