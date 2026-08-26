import type { AgentArtifactInput, ArtifactDescriptor, ArtifactKind } from "../types/artifact";

/**
 * 根据文件扩展名推断 ArtifactKind
 */
export function inferArtifactKind(ext: string): ArtifactKind {
  const e = ext.toLowerCase().replace(/^\./, "");
  switch (e) {
    case "html":
    case "htm":
    case "svg":
      return "web";
    case "md":
    case "markdown":
    case "txt":
    case "pdf":
    case "docx":
    case "doc":
    case "rtf":
      return "document";
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "webp":
    case "ico":
    case "bmp":
      return "image";
    case "xlsx":
    case "xls":
    case "csv":
      return "spreadsheet";
    case "pptx":
    case "ppt":
      return "presentation";
    case "js":
    case "ts":
    case "jsx":
    case "tsx":
    case "vue":
    case "rs":
    case "py":
    case "go":
    case "java":
    case "c":
    case "cpp":
    case "json":
    case "yaml":
    case "yml":
    case "toml":
    case "sql":
    case "sh":
    case "bash":
    case "css":
    case "scss":
    case "less":
      return "code";
    default:
      return "file";
  }
}

/**
 * 获取产物图标 (Emoji / 标识)
 */
export function getArtifactIcon(kind: ArtifactKind, format: string): string {
  const fmt = format.toLowerCase().replace(/^\./, "");
  switch (fmt) {
    case "pdf":
      return "📕";
    case "html":
    case "htm":
      return "🌐";
    case "md":
    case "markdown":
      return "📝";
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "webp":
    case "svg":
      return "🖼️";
    case "xlsx":
    case "xls":
    case "csv":
      return "📊";
    case "docx":
    case "doc":
      return "📄";
    case "pptx":
    case "ppt":
      return "📑";
    case "json":
    case "yaml":
    case "yml":
    case "toml":
      return "⚙️";
    case "js":
    case "ts":
    case "vue":
    case "rs":
    case "py":
    case "go":
      return "💻";
    default:
      switch (kind) {
        case "document":
          return "📄";
        case "web":
          return "🌐";
        case "image":
          return "🖼️";
        case "chart":
          return "📈";
        case "spreadsheet":
          return "📊";
        case "presentation":
          return "📑";
        case "code":
          return "💻";
        default:
          return "📦";
      }
  }
}

/**
 * 获取产物类型中文标签
 */
export function getArtifactKindLabel(kind: ArtifactKind, format: string): string {
  const fmt = format.toUpperCase().replace(/^\./, "");
  switch (kind) {
    case "document":
      return fmt ? `文档 · ${fmt}` : "文档";
    case "web":
      return fmt === "HTML" ? "网页应用 · HTML" : `网页 · ${fmt}`;
    case "image":
      return `图片 · ${fmt || "IMG"}`;
    case "chart":
      return "图表可视化";
    case "spreadsheet":
      return `电子表格 · ${fmt || "XLS"}`;
    case "presentation":
      return `幻灯演示 · ${fmt || "PPT"}`;
    case "code":
      return `源代码 · ${fmt || "CODE"}`;
    default:
      return fmt ? `文件 · ${fmt}` : "产物文件";
  }
}

/**
 * 格式化文件大小
 */
export function formatFileSize(bytes?: number): string {
  if (bytes === undefined || bytes === null || isNaN(bytes) || bytes <= 0) {
    return "";
  }
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/**
 * 判断产物是否支持前端直接预览
 */
export function isArtifactPreviewable(artifact: ArtifactDescriptor): boolean {
  if (artifact.previewUrl) return true;
  if (artifact.content && artifact.content.trim().length > 0) return true;

  const fmt = artifact.format.toLowerCase();
  // 支持预览的格式：html, md, txt, svg, json, js, ts, vue, py, rs, css, 图片等
  const textOrWebFmts = [
    "html", "htm", "md", "markdown", "txt", "svg", "json", "yaml", "yml", "toml",
    "js", "ts", "jsx", "tsx", "vue", "py", "rs", "go", "java", "c", "cpp", "css", "sh"
  ];
  const imageFmts = ["png", "jpg", "jpeg", "gif", "webp", "ico", "bmp"];
  
  if (textOrWebFmts.includes(fmt) || imageFmts.includes(fmt)) {
    return Boolean(artifact.content || artifact.localPath || artifact.previewUrl);
  }

  return false;
}

/**
 * 将旧版路径字符串或任意输入对象统一归一化为 ArtifactDescriptor
 */
export function normalizeArtifact(input: AgentArtifactInput, index = 0): ArtifactDescriptor {
  if (typeof input === "string") {
    const raw = input.trim();
    // 1. 如果是富文本/代码块片段 (如 ```html ... ```)
    const htmlBlock = raw.match(/```html[\s\r\n]+([\s\S]*?)```/i);
    if (htmlBlock) {
      return {
        id: `artifact-rich-html-${index}-${Date.now()}`,
        name: "网页交互产物.html",
        kind: "web",
        format: "html",
        mimeType: "text/html",
        content: htmlBlock[1].trim(),
        description: "由 Agent 编写并渲染的交互式网页应用",
      };
    }

    const echartsBlock = raw.match(/```(?:echarts|chart)[\s\r\n]+([\s\S]*?)```/i);
    if (echartsBlock) {
      return {
        id: `artifact-rich-chart-${index}-${Date.now()}`,
        name: "数据可视化图表.json",
        kind: "chart",
        format: "json",
        mimeType: "application/json",
        content: raw,
        description: "动态交互式 ECharts 图表",
      };
    }

    const mermaidBlock = raw.match(/```mermaid[\s\r\n]+([\s\S]*?)```/i);
    if (mermaidBlock) {
      return {
        id: `artifact-rich-mermaid-${index}-${Date.now()}`,
        name: "架构流程时序图.mermaid",
        kind: "chart",
        format: "mermaid",
        content: raw,
        description: "Mermaid 架构流程图",
      };
    }

    // 2. 如果是文件系统路径或相对路径
    const normalizedPath = raw.replace(/\\/g, "/");
    const segments = normalizedPath.split("/");
    const filename = segments[segments.length - 1] || `artifact-${index + 1}`;
    
    let format = "";
    if (filename.includes(".")) {
      const dotParts = filename.split(".");
      format = (dotParts[dotParts.length - 1] || "").toLowerCase();
    }
    const kind = inferArtifactKind(format);

    return {
      id: `artifact-path-${index}-${filename}`,
      name: filename,
      kind,
      format: format || "file",
      localPath: raw,
      description: normalizedPath.includes("/") ? `本地产物: ${normalizedPath}` : undefined,
    };
  }

  // 3. 如果已经是结构化对象 (未来或当前后端返回)
  const obj = (input || {}) as Record<string, any>;
  const rawName = String(obj.name || obj.filename || obj.title || `artifact-${index + 1}`);
  let format = String(obj.format || obj.ext || "").toLowerCase().replace(/^\./, "");
  if (!format && rawName.includes(".")) {
    const dotParts = rawName.split(".");
    format = (dotParts[dotParts.length - 1] || "").toLowerCase();
  }
  const kind = (obj.kind as ArtifactKind) || inferArtifactKind(format);

  return {
    id: String(obj.id || obj.artifact_id || `artifact-obj-${index}-${Date.now()}`),
    name: rawName,
    kind,
    format: format || "file",
    mimeType: obj.mimeType || obj.mime_type,
    sizeBytes: typeof obj.sizeBytes === "number" ? obj.sizeBytes : (typeof obj.size_bytes === "number" ? obj.size_bytes : undefined),
    createdAt: obj.createdAt || obj.created_at,
    downloadUrl: obj.downloadUrl || obj.download_url,
    previewUrl: obj.previewUrl || obj.preview_url,
    localPath: obj.localPath || obj.local_path || obj.path,
    content: obj.content,
    description: obj.description,
  };
}

/**
 * 批量归一化产物列表并去重
 */
export function normalizeArtifactList(inputs: AgentArtifactInput[] | undefined | null): ArtifactDescriptor[] {
  if (!inputs || !Array.isArray(inputs)) return [];
  const result: ArtifactDescriptor[] = [];
  const seenIds = new Set<string>();

  inputs.forEach((item, index) => {
    if (!item) return;
    const normalized = normalizeArtifact(item, index);
    const key = normalized.localPath || normalized.content || normalized.id;
    if (!seenIds.has(key)) {
      seenIds.add(key);
      result.push(normalized);
    }
  });

  return result;
}
