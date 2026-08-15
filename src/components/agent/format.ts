import { marked } from "marked";
import hljs from "highlight.js";

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
    const compact = value.replace(/\s+/g, " ").trim();
    return compact.length > max ? `${compact.slice(0, max)}…（已截断）` : compact;
}

export function formatPayload(value: unknown): string {
    let text = "";
    if (typeof value === "string") text = value;
    else {
        try {
            text = JSON.stringify(value, null, 2);
        } catch {
            text = toText(value);
        }
    }
    return shortText(text, 12000);
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

        return marked.parse(content, {
            breaks: true,
            gfm: true,
            renderer,
        }) as string;
    } catch {
        return content.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/\n/g, "<br>");
    }
}
