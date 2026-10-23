import hljs from "highlight.js/lib/common";

/** 只投影宿主连续编号的读取输出；未知形态保留原文。复制始终使用原始输出。 */
export function readOutputLines(output: string, start: number, end: number) {
    let expected = start;
    let indent: string | undefined;
    const lines = output.split(/\r?\n/).map(text => {
        const match = text.match(/^\s*(\d+): (.*)$/);
        if (!match) return { number: null, text };
        const number = Number(match[1]);
        if (number !== expected || number > end) return { number: -1, text };
        expected++;
        const content = match[2];
        if (content.trim()) {
            const whitespace = content.match(/^[\t ]*/)?.[0] || "";
            if (indent === undefined) indent = whitespace;
            else {
                let length = 0;
                while (length < indent.length && indent[length] === whitespace[length]) length++;
                indent = indent.slice(0, length);
            }
        }
        return { number, text: content };
    });
    if (expected === start || lines.some(line => line.number === -1)) return null;
    // 将共同缩进移出可视区，保留相对层级，避免深层模板把正文推到窄窗口之外。
    return lines.map(line => ({ ...line, text: line.number !== null && indent && line.text.startsWith(indent)
        ? line.text.slice(indent.length) : line.text }));
}
const fileLanguages: Record<string, string> = {
    vue: "xml", html: "xml", htm: "xml", xml: "xml", svg: "xml", svelte: "xml",
    js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    ts: "typescript", tsx: "typescript", json: "json", css: "css", scss: "scss", less: "less",
    rs: "rust", py: "python", java: "java", kt: "kotlin", go: "go", rb: "ruby", php: "php",
    c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp", cs: "csharp", swift: "swift",
    sh: "bash", bash: "bash", zsh: "bash", sql: "sql", yml: "yaml", yaml: "yaml",
    toml: "ini", ini: "ini", md: "markdown", diff: "diff", patch: "diff",
};

function escapeCode(text: string): string {
    return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;").replace(/'/g, "&#x27;");
}

/** 仅返回高亮器生成的转义文本与 span，文件内容不会作为 HTML 执行。 */
export function highlightReadText(text: string, path: string): string {
    const language = fileLanguages[path.split(".").pop()?.toLowerCase() || ""];
    if (language && hljs.getLanguage(language)) {
        try { return hljs.highlight(text, { language, ignoreIllegals: true }).value; }
        catch { /* 无法高亮时仍可阅读原文。 */ }
    }
    return escapeCode(text);
}

export function highlightReadLines(lines: NonNullable<ReturnType<typeof readOutputLines>>, path: string) {
    const html = highlightReadText(lines.filter(line => line.number !== null).map(line => line.text).join("\n"), path);
    // 整块高亮保留跨行注释/字符串状态；拆行时闭合并重开 span，行号栏不会染色。
    const spans: string[] = [];
    const rendered = [""];
    for (const fragment of html.split(/(<span\b[^>]*>|<\/span>|\n)/g)) {
        if (fragment === "\n") {
            rendered[rendered.length - 1] += "</span>".repeat(spans.length);
            rendered.push(spans.join(""));
        } else {
            if (fragment.startsWith("<span")) spans.push(fragment);
            else if (fragment === "</span>") spans.pop();
            rendered[rendered.length - 1] += fragment;
        }
    }
    let index = 0;
    return lines.map(line => ({ ...line, html: line.number === null ? escapeCode(line.text) : rendered[index++] }));
}
