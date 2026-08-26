<template>
  <div class="rich-content-stream">
    <template v-for="(block, idx) in parsedBlocks" :key="idx">
      <div
        v-if="block.type === 'markdown'"
        class="markdown-segment markdown-content"
        v-html="renderSegmentMarkdown(block.content)"
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

<script setup lang="ts">
import { computed } from "vue";
import DOMPurify from "dompurify";
import { renderMarkdown } from "../agent/format";
import EChartsBlock from "./EChartsBlock.vue";
import MermaidBlock from "./MermaidBlock.vue";
import HtmlPreviewBlock from "./HtmlPreviewBlock.vue";

interface ContentBlock {
  type: "markdown" | "echarts" | "mermaid" | "html";
  content: string;
}

const props = withDefaults(
  defineProps<{
    content: string;
    compactHtml?: boolean;
  }>(),
  {
    compactHtml: false,
  }
);

defineEmits<{
  "open-preview": [code: string];
}>();

const parsedBlocks = computed<ContentBlock[]>(() => {
  const text = props.content || "";
  if (!text) return [];

  // 1. 优先匹配完整闭合的 ```echarts ... ```, ```chart ... ```, ```mermaid ... ```, ```html ... ```
  const blockRegex = /```(echarts|chart|mermaid|html)[\s\r\n]+([\s\S]*?)```/gi;
  const blocks: ContentBlock[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null = null;

  while ((match = blockRegex.exec(text)) !== null) {
    const matchStart = match.index;
    const matchEnd = blockRegex.lastIndex;

    // 前置 Markdown 文本
    if (matchStart > lastIndex) {
      const mdText = text.slice(lastIndex, matchStart).trim();
      if (mdText) {
        blocks.push({ type: "markdown", content: mdText });
      }
    }

    const lang = match[1].toLowerCase();
    const code = match[2].trim();

    if (lang === "mermaid") {
      blocks.push({ type: "mermaid", content: code });
    } else if (lang === "html") {
      if (/<(?:!doctype|html|body|div|button|canvas|svg|form|input|style|script|section|main|h[1-6])/i.test(code)) {
        blocks.push({ type: "html", content: code });
      } else {
        blocks.push({ type: "markdown", content: `\`\`\`html\n${code}\n\`\`\`` });
      }
    } else {
      blocks.push({ type: "echarts", content: code });
    }

    lastIndex = matchEnd;
  }

  // 2. 尾部剩余文本（容错：截断未闭合的代码块或原生 HTML）
  if (lastIndex < text.length) {
    const trailingText = text.slice(lastIndex).trim();
    if (trailingText) {
      // 容错 A：末尾带有未闭合的 ```html、```echarts 或 ```mermaid
      const unclosedMatch = /```(echarts|chart|mermaid|html)[\s\r\n]+([\s\S]*)$/i.exec(trailingText);
      if (unclosedMatch) {
        const mdBefore = trailingText.slice(0, unclosedMatch.index).trim();
        if (mdBefore) {
          blocks.push({ type: "markdown", content: mdBefore });
        }
        const lang = unclosedMatch[1].toLowerCase();
        const code = unclosedMatch[2].trim();
        if (lang === "html") {
          blocks.push({ type: "html", content: code });
        } else if (lang === "mermaid") {
          blocks.push({ type: "mermaid", content: code });
        } else {
          blocks.push({ type: "echarts", content: code });
        }
      } else if (/^<!DOCTYPE html|<html[\s\S]*<\/html>|<div class="container"/i.test(trailingText)) {
        // 容错 B：模型直接输出了原生 HTML 标签结构而未包裹 ```
        blocks.push({ type: "html", content: trailingText });
      } else {
        blocks.push({ type: "markdown", content: trailingText });
      }
    }
  }

  return blocks.length ? blocks : [{ type: "markdown", content: text }];
});

function getHtmlTitle(htmlCode: string): string {
  const match = htmlCode.match(/<title>([^<]+)<\/title>/i);
  return match ? match[1].trim() : "HTML 交互式页面与应用";
}

function renderSegmentMarkdown(segment: string): string {
  return DOMPurify.sanitize(renderMarkdown(segment), {
    FORBID_TAGS: ["form", "iframe", "object", "embed", "script", "style"],
    USE_PROFILES: { html: true, svg: true, mathMl: true },
  });
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
