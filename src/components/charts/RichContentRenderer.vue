<template>
  <div class="rich-content-stream" :class="{ 'is-live-streaming': streaming }">
    <template v-for="(block, idx) in parsedBlocks" :key="idx">
      <div
        v-if="block.type === 'markdown'"
        class="markdown-segment markdown-content"
        v-html="getBlockHtml(block, idx)"
      ></div>
      <EChartsBlock
        v-else-if="block.type === 'echarts'"
        :code="block.content"
      />
      <MermaidBlock
        v-else-if="block.type === 'mermaid'"
        :code="block.content"
      />
      <!-- 紧凑模式下渲染为优雅的单行/胶囊卡片，点击在右侧工作台打开 -->
      <div
        v-else-if="block.type === 'html' && compactHtml"
        class="html-deliverable-card"
        @click="$emit('open-preview', block.content)"
      >
        <div class="card-left">
          <div class="badge-row">
            <span class="deliverable-badge">🌐 交互成果</span>
            <strong class="deliverable-title">{{ getHtmlTitle(block.content) }}</strong>
          </div>
          <p class="deliverable-tip">已在右侧工作台为您实时渲染，支持自由交互、全屏与导出</p>
        </div>
        <button type="button" class="deliverable-open-btn" @click.stop="$emit('open-preview', block.content)">
          <span>右侧预览</span>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M5 12h14M12 5l7 7-7 7"/></svg>
        </button>
      </div>
      <!-- 完整模式下直接渲染可交互沙箱 (用于右侧工作台) -->
      <HtmlPreviewBlock
        v-else-if="block.type === 'html'"
        :code="block.content"
      />
    </template>
  </div>
</template>

<script lang="ts">
import DOMPurify from "dompurify";
import { renderMarkdown } from "../agent/format";

const sanitizeCache = new Map<string, string>();
const MAX_SANITIZE_CACHE = 800;
const MAX_SANITIZE_LENGTH = 60000;

function renderSegmentMarkdown(segment: string): string {
  const cached = sanitizeCache.get(segment);
  if (cached !== undefined) {
    sanitizeCache.delete(segment);
    sanitizeCache.set(segment, cached);
    return cached;
  }
  const sanitized = DOMPurify.sanitize(renderMarkdown(segment), {
    FORBID_TAGS: ["form", "iframe", "object", "embed", "script", "style"],
    USE_PROFILES: { html: true, svg: true, mathMl: true },
  });
  if (segment.length <= MAX_SANITIZE_LENGTH) {
    if (sanitizeCache.size >= MAX_SANITIZE_CACHE) {
      const oldestKeys = Array.from(sanitizeCache.keys()).slice(0, 100);
      for (const k of oldestKeys) sanitizeCache.delete(k);
    }
    sanitizeCache.set(segment, sanitized);
  }
  return sanitized;
}
</script>

<script setup lang="ts">
import { computed, defineAsyncComponent } from "vue";
import { marked } from "marked";
import HtmlPreviewBlock from "./HtmlPreviewBlock.vue";

const EChartsBlock = defineAsyncComponent(() => import("./EChartsBlock.vue"));
const MermaidBlock = defineAsyncComponent(() => import("./MermaidBlock.vue"));

interface ContentBlock {
  type: "markdown" | "echarts" | "mermaid" | "html";
  content: string;
  html?: string;
}

const props = withDefaults(
  defineProps<{
    content: string;
    compactHtml?: boolean;
    streaming?: boolean;
  }>(),
  {
    compactHtml: false,
    streaming: false,
  }
);

defineEmits<{
  "open-preview": [code: string];
}>();

// 常见 Mermaid 图表开头，用于识别未加反引号围栏的残缺段落
const MERMAID_KEYWORDS = /^(?:flowchart|graph|sequenceDiagram|classDiagram|stateDiagram|erDiagram|gantt|mindmap|pie|gitGraph|journey|quadrantChart|xychart-beta|packet-beta|architecture-beta|kanban)/i;

const parsedBlocks = computed<ContentBlock[]>(() => {
  const text = props.content || "";
  if (!text) return [];

  // 使用 marked.lexer 解析顶级 AST Token，完全遵循 Markdown 规范，杜绝代码块穿透
  let tokens: any[] = [];
  try {
    tokens = marked.lexer(text);
  } catch {
    tokens = [];
  }

  if (!tokens || tokens.length === 0) {
    return [{ type: "markdown", content: text, html: renderSegmentMarkdown(text) }];
  }

  const blocks: ContentBlock[] = [];
  let accumulatedMarkdown = "";

  function flushMarkdown() {
    const trimmed = accumulatedMarkdown.trim();
    if (trimmed) {
      blocks.push({
        type: "markdown",
        content: trimmed,
        html: renderSegmentMarkdown(trimmed),
      });
    }
    accumulatedMarkdown = "";
  }

  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];

    // 1. 顶级代码块 token (```lang ... ```)
    if (token.type === "code") {
      const lang = (token.lang || "").trim().toLowerCase();
      const code = (token.text || "").trim();

      if (lang === "mermaid") {
        flushMarkdown();
        blocks.push({ type: "mermaid", content: code });
        continue;
      } else if (lang === "echarts" || lang === "chart") {
        flushMarkdown();
        blocks.push({ type: "echarts", content: code });
        continue;
      } else if (lang === "html") {
        if (/<(?:!doctype|html|body|div|button|canvas|svg|form|input|style|script|section|main|h[1-6])/i.test(code)) {
          flushMarkdown();
          blocks.push({ type: "html", content: code });
          continue;
        }
      }
    }

    // 2. 顶级段落自愈容错：针对模型漏掉头部反引号或结尾转义为 ``n 的破损图表块
    if (token.type === "paragraph" && token.raw) {
      const rawParagraph = token.raw.trim();
      // 检查是否以 mermaid 裸词开头，且紧跟合法图表语法
      const bareMermaidMatch = rawParagraph.match(/^(?:`*\s*)mermaid[\r\n]+([\s\S]+)$/i);
      if (bareMermaidMatch) {
        let innerCode = bareMermaidMatch[1].trim();
        // 剥离尾部破损的 ``n 或 ``` 标记
        innerCode = innerCode.replace(/[\r\n]+`{2,3}n?$/i, "").trim();
        if (MERMAID_KEYWORDS.test(innerCode)) {
          flushMarkdown();
          blocks.push({ type: "mermaid", content: innerCode });
          continue;
        }
      }
    }

    // 3. 顶级原生 HTML 自愈（模型直接输出 HTML 结构而未包裹围栏）
    if (token.type === "html" && token.raw) {
      const trimmedRaw = token.raw.trim();
      if (/^<!DOCTYPE html|<html[\s\S]*<\/html>|<div class="container"/i.test(trimmedRaw)) {
        flushMarkdown();
        blocks.push({ type: "html", content: trimmedRaw });
        continue;
      }
    }

    // 其余普通 token 保持原始文本拼装到 markdown 渲染段中
    accumulatedMarkdown += token.raw || "";
  }

  flushMarkdown();
  return blocks.length ? blocks : [{ type: "markdown", content: text, html: renderSegmentMarkdown(text) }];
});

function getHtmlTitle(htmlCode: string): string {
  const match = htmlCode.match(/<title>([^<]+)<\/title>/i);
  return match ? match[1].trim() : "HTML 交互式页面与应用";
}

function decorateStreamingTail(html: string): string {
  if (!html || typeof window === "undefined" || !window.DOMParser) return html;
  try {
    const parser = new DOMParser();
    const doc = parser.parseFromString(html, "text/html");

    const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT);
    let lastTextNode: Text | null = null;
    while (walker.nextNode()) {
      const node = walker.currentNode as Text;
      if (node.textContent && node.textContent.trim().length > 0) {
        lastTextNode = node;
      }
    }

    if (!lastTextNode || !lastTextNode.textContent) {
      return html;
    }

    const fullText = lastTextNode.textContent;
    const trimmed = fullText.trimEnd();
    const trailingSpaces = fullText.slice(trimmed.length);

    // 末尾字符呈现从 100% 到 0% 的纯透明度阶梯过渡（无颜色渐变，上一句完全不透明）
    const opacities = [0.90, 0.78, 0.64, 0.50, 0.36, 0.22, 0.10];
    const tailCount = Math.min(opacities.length, trimmed.length);
    if (tailCount <= 0) return html;

    const activeOpacities = opacities.slice(opacities.length - tailCount);
    const prefix = trimmed.slice(0, trimmed.length - tailCount);
    const tailChars = Array.from(trimmed.slice(trimmed.length - tailCount));

    const parent = lastTextNode.parentNode;
    if (!parent) return html;

    const frag = doc.createDocumentFragment();
    if (prefix) {
      frag.appendChild(doc.createTextNode(prefix));
    }

    for (let i = 0; i < tailChars.length; i++) {
      const span = doc.createElement("span");
      span.className = "stream-tail-fade";
      span.style.opacity = String(activeOpacities[i]);
      span.textContent = tailChars[i];
      frag.appendChild(span);
    }

    if (trailingSpaces) {
      frag.appendChild(doc.createTextNode(trailingSpaces));
    }

    parent.replaceChild(frag, lastTextNode);
    return doc.body.innerHTML;
  } catch {
    return html;
  }
}

function getBlockHtml(block: ContentBlock, idx: number): string {
  const baseHtml = block.html || renderSegmentMarkdown(block.content);
  if (!props.streaming || idx !== parsedBlocks.value.length - 1) {
    return baseHtml;
  }
  return decorateStreamingTail(baseHtml);
}
</script>

<style scoped>
.rich-content-stream {
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 100%;
}

.markdown-segment {
  width: 100%;
  color: #e4e4e7;
  line-height: 1.65;
  font-size: 13.5px;
}

/* 🌟 类似 ChatGPT 的自然平滑流式渐入微动效（极快但有丝滑渐显质感） */
@keyframes chatgpt-stream-fade-in {
  0% {
    opacity: 0;
    transform: translateY(3px);
  }
  100% {
    opacity: 1;
    transform: translateY(0);
  }
}

.markdown-segment :deep(p),
.markdown-segment :deep(li),
.markdown-segment :deep(pre),
.markdown-segment :deep(blockquote),
.markdown-segment :deep(table),
.markdown-segment :deep(h1),
.markdown-segment :deep(h2),
.markdown-segment :deep(h3),
.markdown-segment :deep(h4) {
  animation: chatgpt-stream-fade-in 0.16s cubic-bezier(0.16, 1, 0.3, 1) forwards;
  will-change: opacity, transform;
}

/* 真流式期间：每个 chunk 都整体重建 innerHTML，若保留逐元素入场动画
   所有元素会反复重放 = 全文闪烁。改为关掉块级动画 +
   给最后一个 markdown 段挂"底部渐显遮罩"——新流入的文字落在
   半透明区、随内容生长逐渐凝实，形成"字从淡到浓浮出来"的流动感 */
.rich-content-stream.is-live-streaming .markdown-segment :deep(p),
.rich-content-stream.is-live-streaming .markdown-segment :deep(li),
.rich-content-stream.is-live-streaming .markdown-segment :deep(pre),
.rich-content-stream.is-live-streaming .markdown-segment :deep(blockquote),
.rich-content-stream.is-live-streaming .markdown-segment :deep(table),
.rich-content-stream.is-live-streaming .markdown-segment :deep(h1),
.rich-content-stream.is-live-streaming .markdown-segment :deep(h2),
.rich-content-stream.is-live-streaming .markdown-segment :deep(h3),
.rich-content-stream.is-live-streaming .markdown-segment :deep(h4) {
  animation: none !important;
  will-change: auto;
}

/* 🌟 新流入文字纯透明度过渡：无颜色渐变，文字颜色天然继承，上一句完全凝实，末端纯透明度 100% -> 0% 阶梯 */
.markdown-segment :deep(.stream-tail-fade) {
  display: inline !important;
  color: inherit !important;
  transition: opacity 0.1s linear;
  will-change: opacity;
}

/* 标题样式 */
.markdown-segment :deep(h1),
.markdown-segment :deep(h2),
.markdown-segment :deep(h3),
.markdown-segment :deep(h4) {
  margin: 12px 0 6px;
  color: #f4f4f5;
  font-weight: 650;
  line-height: 1.4;
}
.markdown-segment :deep(h1) { font-size: 16px; }
.markdown-segment :deep(h2) { font-size: 15px; }
.markdown-segment :deep(h3) { font-size: 14px; }
.markdown-segment :deep(h4) { font-size: 13px; }

/* 分割线 (弱化水平分割线) */
.markdown-segment :deep(hr) {
  border: none;
  height: 1px;
  background: rgba(255, 255, 255, 0.08);
  margin: 14px 0;
}

/* 段落与列表 */
.markdown-segment :deep(p) {
  margin: 4px 0 8px;
}
.markdown-segment :deep(ul),
.markdown-segment :deep(ol) {
  margin: 4px 0 8px;
  padding-left: 20px;
}
.markdown-segment :deep(li) {
  margin: 2px 0;
}

/* 引用块 */
.markdown-segment :deep(blockquote) {
  margin: 8px 0;
  padding: 6px 14px;
  border-left: 3px solid #fbbf24;
  background: rgba(251, 191, 36, 0.05);
  border-radius: 0 6px 6px 0;
  color: #a1a1aa;
}

/* 表格 (Obsidian 曜石暗色网格与悬停高亮) */
.markdown-segment :deep(table) {
  width: 100%;
  margin: 10px 0 14px;
  border-collapse: separate;
  border-spacing: 0;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 8px;
  overflow: hidden;
  font-size: 12.5px;
}
.markdown-segment :deep(th) {
  padding: 8px 12px;
  background: #1f2026;
  color: #f4f4f5;
  font-weight: 600;
  text-align: left;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
  border-right: 1px solid rgba(255, 255, 255, 0.08);
}
.markdown-segment :deep(th:last-child) {
  border-right: none;
}
.markdown-segment :deep(td) {
  padding: 7px 12px;
  background: #161619;
  color: #d4d4d8;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  border-right: 1px solid rgba(255, 255, 255, 0.06);
}
.markdown-segment :deep(td:last-child) {
  border-right: none;
}
.markdown-segment :deep(tr:last-child td) {
  border-bottom: none;
}
.markdown-segment :deep(tr:hover td) {
  background: #1e1e24;
}

/* 行内代码 */
.markdown-segment :deep(code:not(pre code)) {
  padding: 2px 5px;
  border-radius: 4px;
  color: #e5c07b;
  background: #1e1e23;
  font: 12px ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}

/* 优雅紧凑的 HTML 成果胶囊卡片 */
.html-deliverable-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 12px 16px;
  border-radius: 10px;
  border: 1px solid rgba(251, 191, 36, 0.25);
  background: linear-gradient(135deg, rgba(251, 191, 36, 0.08) 0%, rgba(30, 30, 36, 0.6) 100%);
  cursor: pointer;
  transition: all 0.2s ease;
  margin: 6px 0;
}
.html-deliverable-card:hover {
  border-color: rgba(251, 191, 36, 0.45);
  background: linear-gradient(135deg, rgba(251, 191, 36, 0.12) 0%, rgba(35, 35, 42, 0.8) 100%);
  transform: translateY(-1px);
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.25);
}

.card-left {
  display: flex;
  flex-direction: column;
  gap: 3px;
  min-width: 0;
}

.badge-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.deliverable-badge {
  font-size: 10.5px;
  font-weight: 700;
  color: #fbbf24;
  padding: 2px 6px;
  border-radius: 4px;
  background: rgba(251, 191, 36, 0.15);
  letter-spacing: 0.02em;
}

.deliverable-title {
  font-size: 13.5px;
  font-weight: 650;
  color: #f4f4f5;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.deliverable-tip {
  margin: 0;
  font-size: 11.5px;
  color: #a1a1aa;
}

.deliverable-open-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  border: 1px solid rgba(251, 191, 36, 0.35);
  border-radius: 6px;
  background: rgba(251, 191, 36, 0.15);
  color: #fbbf24;
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}
.deliverable-open-btn:hover {
  background: rgba(251, 191, 36, 0.28);
  border-color: rgba(251, 191, 36, 0.6);
  color: #ffffff;
}
</style>
