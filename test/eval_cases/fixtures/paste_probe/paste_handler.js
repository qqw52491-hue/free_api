// 聊天输入框粘贴处理（Tauri 桌面端 / WebKitGTK 环境）
// 依赖注入: ctx = { invoke, attachments, textarea, attachFilePaths }
//   invoke(cmd, args)      -> 调宿主原生命令（read_clipboard_image 等）
//   attachments            -> 附件数组（push 附件对象即上屏）
//   textarea               -> DOM 文本域（value / selectionStart / setSelectionRange）
//   attachFilePaths(paths) -> 既有附件管道：按真实路径解析并入列

export async function handlePaste(e, ctx) {
    const clipboard = e.clipboardData;
    if (!clipboard) return;

    // 1. 提取 DOM 层图片（截图 / 复制图片文件）
    const imageFiles = [];
    const isImage = (f) =>
        f && (f.type?.startsWith("image/") || /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(f.name || ""));
    for (const item of clipboard.items || []) {
        if (item.kind === "file" || (item.type || "").startsWith("image/")) {
            const f = item.getAsFile?.();
            if (isImage(f)) imageFiles.push(f);
        }
    }
    for (const f of clipboard.files || []) {
        if (isImage(f)) imageFiles.push(f);
    }

    // 2. DOM 没有图片时走原生剪贴板回退（第二轮修复加的探针）
    if (imageFiles.length === 0) {
        try {
            const native = await ctx.invoke("read_clipboard_image");
            if (native && native.startsWith("data:image/")) {
                e.preventDefault();
                ctx.attachments.push({
                    file_name: "screenshot.png",
                    image_data_url: native,
                });
                return;
            }
        } catch {
            // 非桌面端或探针不可用，忽略
        }
    }

    // 3. DOM 图片入列
    if (imageFiles.length > 0) {
        e.preventDefault();
        for (const f of imageFiles) {
            ctx.attachments.push({
                file_name: f.name || "image.png",
                image_data_url: "data:" + (f.type || "image/png"),
            });
        }
        return;
    }

    // 4. 长文本折叠为虚拟附件；短文本不拦截，走默认插入
    const text = clipboard.getData?.("text/plain");
    if (text && text.length >= 500) {
        e.preventDefault();
        ctx.attachments.push({
            file_name: "pasted_snippet.txt",
            prompt_text: text,
            is_pasted_snippet: true,
        });
    }
}
