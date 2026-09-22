#!/usr/bin/env node
/**
 * FAIL_TO_PASS 判据：图片上传缩略图破损/无法预览 修复验收（静态契约检查）
 *
 * 基线缺陷（afd3973）：三条图片上传路径（拖拽/粘贴/原生剪贴板回退）
 * 均把 file_path 赋为裸文件名且不落盘；消息徽标不带路径 → 历史重载后
 * convertFileSrc(裸文件名) 必然 onerror → 破损占位图 + 预览失败。
 *
 * 通过条件（全部满足）：
 *  1. 调用了 save_and_parse_uploaded_attachment 且参数名为
 *     sessionId / fileName / base64Data（不得误用 dataUrl）；
 *  2. 消息附件徽标携带路径（"路径:" 标记）；
 *  3. AgentComposer.vue 中不再出现"裸文件名 file_path 且无 upload_failed
 *     标记"的附件入队（失败兜底可以裸名，但必须带 upload_failed 如实标记）。
 */
import fs from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const file = path.join(root, 'src/components/agent/AgentComposer.vue');

if (!fs.existsSync(file)) {
    console.error(`❌ JUDGE FAIL: ${file} 不存在`);
    process.exit(1);
}
const src = fs.readFileSync(file, 'utf8');
const failures = [];

// ── 1. 落盘 invoke 存在且参数契约正确 ─────────────────────────────
const cmd = 'save_and_parse_uploaded_attachment';
if (!src.includes(cmd)) {
    failures.push(`缺少 ${cmd} 调用——图片从未落盘`);
} else {
    // 只认 invoke("...") 真实调用点（跳过注释/函数签名里的同名字符串），
    // 在其后 ~400 字符的参数对象窗口内核对契约
    const re = new RegExp(cmd.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'g');
    let m; let ok = false;
    while ((m = re.exec(src))) {
        const before = src.slice(Math.max(0, m.index - 80), m.index);
        if (!/invoke\b/.test(before)) continue;
        const w = src.slice(m.index, m.index + 400);
        const end = w.indexOf('});');
        const args = end >= 0 ? w.slice(0, end) : w; // 只查 invoke 的参数对象，避免越界到下一函数签名
        if (/\bdataUrl\s*:/.test(args)) continue; // 参数名写错（应为 base64Data），不算
        if (/sessionId/.test(args) && /fileName/.test(args) && /base64Data/.test(args)) { ok = true; break; }
    }
    if (!ok) failures.push(`${cmd} 的 invoke 调用点均未同时携带 sessionId/fileName/base64Data（或误用 dataUrl）`);
}

// ── 2. 消息附件徽标必须携带路径，否则历史重载无法还原 ──────────────
if (!/路径[:：]/.test(src)) {
    failures.push('消息附件徽标未携带 "路径:" 标记——历史消息重载后 file_path 无法还原');
}

// ── 3. 裸文件名 file_path 只允许出现在 upload_failed 兜底里 ────────
const bareRe = /file_path:\s*(file\.name|fileName)\b/g;
let m;
while ((m = bareRe.exec(src))) {
    const w = src.slice(m.index, m.index + 400);
    if (!/upload_failed/.test(w)) {
        const line = src.slice(0, m.index).split('\n').length;
        failures.push(`第 ${line} 行存在裸文件名 file_path 且未带 upload_failed 标记（静默降级）`);
    }
}

if (failures.length) {
    console.error('❌ JUDGE FAIL:');
    for (const f of failures) console.error(`   - ${f}`);
    process.exit(1);
}
console.log('✅ JUDGE PASS: 图片上传落盘参数正确、徽标带路径、无静默裸名降级');
process.exit(0);
