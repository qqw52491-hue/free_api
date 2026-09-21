// paste_probe 验收测试：模拟 WebKitGTK/Wayland 的真实粘贴事件行为。
// 关键环境契约（勿改）：
//  - paste 事件会触发，但 e.clipboardData 可能为 null（沙盒限制）
//  - clipboardData 存在时也可能只有文本项、items/files 为空（文件复制给的是
//    text/uri-list 文本，不是 File）
//  - 默认动作在事件分发结束时执行：未 preventDefault 且剪贴板带文本 →
//    把文本插到光标处；await 之后再 preventDefault 已无法撤销
//  - 宿主命令 read_clipboard_image 返回 {data_urls: string[], file_paths: string[]}
//    （data_urls=剪贴板内图像数据；file_paths=文件管理器复制的真实文件路径）

import { handlePaste } from "./paste_handler.js";

let failures = 0;
function check(name, cond, extra = "") {
    if (cond) console.log(`  ✅ ${name}`);
    else {
        failures++;
        console.log(`  ❌ ${name} ${extra}`);
    }
}

function makeCtx(invokeImpl) {
    const attachments = [];
    const textarea = {
        value: "hello ",
        selectionStart: 6,
        selectionEnd: 6,
        setSelectionRange(s, e) { this.selectionStart = s; this.selectionEnd = e; },
    };
    const calls = { invokeCmds: [], attachFilePaths: [] };
    const ctx = {
        attachments,
        textarea,
        invoke: async (cmd, args) => {
            calls.invokeCmds.push(cmd);
            return typeof invokeImpl === "function" ? invokeImpl(cmd, args) : invokeImpl;
        },
        attachFilePaths: async (paths) => {
            calls.attachFilePaths.push(paths);
            for (const p of paths) {
                attachments.push({ file_name: p.split("/").pop(), file_path: p });
            }
        },
    };
    return { ctx, attachments, textarea, calls };
}

function makeEvent(clipboardData) {
    return {
        clipboardData,
        defaultPrevented: false,
        preventDefault() { this.defaultPrevented = true; },
    };
}

// 浏览器默认动作模拟：事件同步段结束后（我们调用 handler 但不 await 的间隙即
// "事件分发结束"），若未 preventDefault 且剪贴板带文本 → 文本插入光标处。
function simulateDefaultInsert(e, textarea) {
    if (e.defaultPrevented) return;
    const text = e.clipboardData?.getData?.("text/plain");
    if (text) {
        const s = textarea.selectionStart ?? textarea.value.length;
        const epos = textarea.selectionEnd ?? s;
        textarea.value = textarea.value.slice(0, s) + text + textarea.value.slice(epos);
        textarea.selectionStart = textarea.selectionEnd = s + text.length;
    }
}

const URI_TEXT = "file:///home/u/Pictures/cat.png";

// ── T1: WebKitGTK 下 clipboardData=null + 剪贴板是截图（image/png）──
{
    const { ctx, attachments, textarea, calls } = makeCtx({
        data_urls: ["data:image/png;base64,iVBOR"],
        file_paths: [],
    });
    const e = makeEvent(null);
    const p = handlePaste(e, ctx);
    simulateDefaultInsert(e, textarea);
    await p;
    check("T1 null clipboardData 仍调用原生探针", calls.invokeCmds.includes("read_clipboard_image"));
    check("T1 截图数据成为图片附件", attachments.some((a) => a.image_data_url?.startsWith("data:image/")));
    check("T1 输入框文本未被污染", textarea.value === "hello ");
}

// ── T2: clipboardData 只有 uri-list 文本（文件管理器复制图片文件）──
{
    const { ctx, attachments, textarea, calls } = makeCtx({
        data_urls: [],
        file_paths: ["/home/u/Pictures/cat.png"],
    });
    const e = makeEvent({
        types: ["text/plain"],
        items: [],
        files: [],
        getData: (t) => (t === "text/plain" ? URI_TEXT : ""),
    });
    const p = handlePaste(e, ctx);
    simulateDefaultInsert(e, textarea);
    await p;
    check(
        "T2 uri-list 复制的文件经附件管道入列",
        calls.attachFilePaths.flat().includes("/home/u/Pictures/cat.png")
            || attachments.some((a) => a.file_path === "/home/u/Pictures/cat.png")
    );
    check("T2 默认插入的 uri 文本已回滚", textarea.value === "hello ", `实际: ${JSON.stringify(textarea.value)}`);
}

// ── T3: 纯短文本粘贴不回归（不拦截默认行为）──
{
    const { ctx, attachments, textarea } = makeCtx(null);
    const e = makeEvent({
        types: ["text/plain"],
        items: [],
        files: [],
        getData: (t) => (t === "text/plain" ? "short text" : ""),
    });
    const p = handlePaste(e, ctx);
    simulateDefaultInsert(e, textarea);
    await p;
    check("T3 短文本走默认插入", textarea.value.includes("short text"));
    check("T3 未误加附件", attachments.length === 0);
}

// ── T4: DOM 层有图片项时同步拦截（回归保护）──
{
    const { ctx, attachments, textarea } = makeCtx(null);
    const e = makeEvent({
        types: ["Files"],
        items: [{ kind: "file", type: "image/png", getAsFile: () => ({ name: "shot.png", type: "image/png", size: 10 }) }],
        files: [],
        getData: () => "",
    });
    const p = handlePaste(e, ctx);
    check("T4 DOM 图片路径同步 preventDefault", e.defaultPrevented === true);
    simulateDefaultInsert(e, textarea);
    await p;
    check("T4 DOM 图片入列", attachments.some((a) => a.file_name === "shot.png"));
}

if (failures === 0) {
    console.log("ALL PASS");
    process.exit(0);
} else {
    console.log(`${failures} assertion(s) failed`);
    process.exit(1);
}
