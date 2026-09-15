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
 * 产物 URL 只允许交由浏览器/系统打开的 HTTP(S) 地址。
 * artifact 输入来自模型或外部工具，不能把 javascript:、file: 等协议直接传给副作用 API。
 */
export function isSafeArtifactUrl(value?: string): boolean {
  const raw = value?.trim();
  if (!raw) return false;

  try {
    const url = new URL(raw);
    return url.protocol === "https:" || url.protocol === "http:";
  } catch {
    return false;
  }
}

/**
 * 判断产物是否支持前端直接在线预览。
 * 支持：内存内联 content、安全远程 previewUrl、以及本地常见文本/文档/代码文件 (可通过宿主 read_artifact_file 异步拉取)
 */
export function isArtifactPreviewable(artifact: ArtifactDescriptor): boolean {
  const fmt = (artifact.format || "").toLowerCase().replace(/^\./, "");
  
  // 1. 二进制/不可内联预览的文件扩展名黑名单（压缩包/可执行/数据库等）
  const binaryUnpreviewableExts = [
    "zip", "tar", "gz", "7z", "rar",
    "exe", "bin", "dll", "so", "dylib", "db", "sqlite"
  ];
  if (fmt && binaryUnpreviewableExts.includes(fmt)) {
    return false;
  }

  // 1.5 PDF/Office 文档经宿主转换管道（soffice → pdf → pdftoppm → 逐页 PNG）
  // 可渲染预览；宿主缺转换工具时 handlePreviewArtifact 会回退"系统程序打开"
  const convertiblePreviewExts = [
    "pdf", "pptx", "ppt", "docx", "doc", "xlsx", "xls", "odt", "ods", "odp", "rtf"
  ];
  if (fmt && convertiblePreviewExts.includes(fmt)) {
    return true;
  }

  // 2. 如果已经拥有非空内联正文
  if (artifact.content && artifact.content.trim().length > 0) return true;

  // 3. 安全的 Web 预览 URL
  if (isSafeArtifactUrl(artifact.previewUrl)) return true;

  // 4. 白名单可预览格式（文本、代码、轻量图像、SVG、CSV等）
  const previewableExts = [
    "md", "markdown", "txt", "html", "htm", "json", "yaml", "yml", "toml",
    "js", "ts", "jsx", "tsx", "vue", "rs", "py", "go", "java", "c", "cpp",
    "sql", "css", "scss", "sh", "svg", "png", "jpg", "jpeg", "webp", "gif", "csv"
  ];
  if (fmt) {
    return previewableExts.includes(fmt);
  }

  // 5. 没有 format 时，仅当类型为明确的文本、网页、图表或图片时才允许预览
  return ["text", "web", "chart", "image"].includes(artifact.kind);
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

    // 2. 如果是无效的 [object Object] 字符串，直接跳过
    if (raw === "[object Object]" || raw.length === 0) {
      return {
        id: `artifact-invalid-${index}-${Date.now()}`,
        name: `artifact-${index + 1}`,
        kind: "file",
        format: "file",
      };
    }

    // 3. 如果是文件系统路径、相对路径或带有描述说明的文本行
    const parsed = parseArtifactPathAndDesc(raw);
    const cleanPath = parsed.cleanPath;
    const normalizedPath = cleanPath.replace(/\\/g, "/");
    const segments = normalizedPath.split("/");
    let filename = segments[segments.length - 1] || `artifact-${index + 1}`;
    if (filename === "[object Object]") {
      filename = `artifact-${index + 1}`;
    }
    
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
      localPath: cleanPath === "[object Object]" ? undefined : cleanPath,
      description: parsed.description || (normalizedPath.includes("/") ? `本地产物: ${normalizedPath}` : undefined),
    };
  }

  // 4. 如果是结构化对象 (安全深度解构，防止任何 [object Object] 污染)
  const obj = (input || {}) as Record<string, any>;
  
  // 安全提取 path / localPath / canonicalPath
  const safeLocalPath = extractSafeString(obj.localPath) ||
                        extractSafeString(obj.local_path) ||
                        extractSafeString(obj.canonicalPath) ||
                        extractSafeString(obj.canonical_path) ||
                        extractSafeString(obj.path) ||
                        extractSafeString(obj.filePath) ||
                        extractSafeString(obj.file_path) ||
                        extractSafeString(obj.target_file) ||
                        extractSafeString(obj.file);

  // 安全提取 name / filename
  let rawName = extractSafeString(obj.name) ||
                extractSafeString(obj.filename) ||
                extractSafeString(obj.title) ||
                (safeLocalPath ? safeLocalPath.replace(/\\/g, "/").split("/").pop() : undefined) ||
                `artifact-${index + 1}`;

  if (rawName === "[object Object]") {
    rawName = (safeLocalPath ? safeLocalPath.replace(/\\/g, "/").split("/").pop() : undefined) || `artifact-${index + 1}`;
  }

  // 安全提取 format
  let format = (extractSafeString(obj.format) || extractSafeString(obj.ext) || "").toLowerCase().replace(/^\./, "");
  if (!format && rawName.includes(".")) {
    const dotParts = rawName.split(".");
    format = (dotParts[dotParts.length - 1] || "").toLowerCase();
  }
  const kind = (typeof obj.kind === "string" && obj.kind) ? (obj.kind as ArtifactKind) : inferArtifactKind(format);

  const safeDownloadUrl = extractSafeString(obj.downloadUrl) || extractSafeString(obj.download_url);
  const safePreviewUrl = extractSafeString(obj.previewUrl) || extractSafeString(obj.preview_url);
  const safeContent = typeof obj.content === "string" ? obj.content : undefined;
  const safeDesc = typeof obj.description === "string" ? obj.description : (typeof obj.desc === "string" ? obj.desc : undefined);
  const safeArtifactId = extractSafeString(obj.artifactId) || extractSafeString(obj.artifact_id);

  return {
    id: typeof obj.id === "string" && obj.id !== "[object Object]" ? obj.id : (safeArtifactId || `artifact-obj-${index}-${Date.now()}`),
    artifactId: safeArtifactId,
    name: rawName,
    kind,
    format: format || "file",
    mimeType: typeof obj.mimeType === "string" ? obj.mimeType : (typeof obj.mime_type === "string" ? obj.mime_type : undefined),
    sizeBytes: typeof obj.sizeBytes === "number" ? obj.sizeBytes : (typeof obj.size_bytes === "number" ? obj.size_bytes : undefined),
    createdAt: typeof obj.createdAt === "string" ? obj.createdAt : (typeof obj.created_at === "string" ? obj.created_at : undefined),
    downloadUrl: safeDownloadUrl,
    previewUrl: safePreviewUrl,
    localPath: safeLocalPath,
    content: safeContent,
    description: safeDesc || (safeLocalPath ? `本地产物: ${safeLocalPath}` : undefined),
    status: obj.status,
    origin: obj.origin,
  };
}

/**
 * 递归安全提取有效非空非 [object Object] 字符串
 */
function extractSafeString(val: unknown): string | undefined {
  if (typeof val === "string") {
    const trimmed = val.trim();
    if (trimmed.length > 0 && trimmed !== "[object Object]") {
      return trimmed;
    }
  }
  if (val && typeof val === "object" && !Array.isArray(val)) {
    const record = val as Record<string, unknown>;
    for (const key of ["path", "localPath", "local_path", "filePath", "file_path", "file", "target_file", "name", "url"]) {
      if (typeof record[key] === "string") {
        const candidate = (record[key] as string).trim();
        if (candidate.length > 0 && candidate !== "[object Object]") {
          return candidate;
        }
      }
    }
  }
  return undefined;
}

/**
 * 解析带有中文破折号、冒号、括号说明或 Markdown 反引号的路径文本
 * 例如:
 * "src-tauri/docs/frontend-code-review.md — 结构化 Markdown 审查报告（6015 字节...）"
 * "生成文件：/home/w/文档/free_api/src-tauri/hello.md"
 * "`dist/index.html` - 交互看板"
 * "- output/report.pdf: 最终研报"
 */
export function parseArtifactPathAndDesc(raw: string): { cleanPath: string; description?: string } {
  let text = raw.trim();
  // 去除前导列表符号如 `- `, `* `, `1. `
  text = text.replace(/^[-*•]\s+/, "").replace(/^\d+\.\s+/, "").trim();
  // 去除前导文件图标
  text = text.replace(/^[📄📑📝📦📁]\s*/, "").trim();
  // 去除常见前缀标签如 `生成文件：`, `文件路径：`, `交付成果：`, `产物：`, `文件：`
  text = text.replace(/^(?:生成文件|文件路径|交付成果|交付产物|交付物文件|交付物|产物|文件)\s*[:：]\s*/i, "").trim();

  // 如果字符串包含中英文括号内的枚举（如 "结构（package.json、vite.config.ts）"），不是单一交付物路径
  if (/[(（][^()（）]*[、,][^()（）]*[)）]/.test(text)) {
    return { cleanPath: "" };
  }

  // 1. 如果带有反引号包裹 `path` ...
  const backtickMatch = text.match(/^`([^`]+)`\s*(?:[—–\-:：]\s*(.*))?$/);
  if (backtickMatch) {
    return {
      cleanPath: backtickMatch[1].trim(),
      description: backtickMatch[2]?.trim() || undefined,
    };
  }

  // 2. 如果包含标准分隔符: ` — `, ` – `, ` - `, ` : `, `：`
  const separatorMatch = text.match(/^([^\s—–:："'`()[\]{}]+(?:\/[^\s—–:："'`()[\]{}]+)*\.[a-zA-Z0-9]{1,10})\s*(?:[—–\-:：]\s*(.*))?$/u);
  if (separatorMatch) {
    return {
      cleanPath: separatorMatch[1].trim(),
      description: separatorMatch[2]?.trim() || undefined,
    };
  }

  // 3. 通用正则匹配首个像文件路径的 token (支持中文字符与 Unicode 路径)
  const pathMatch = text.match(/([^\s—–:："'`()[\]{}、]+\.[a-zA-Z0-9]{1,10})/u);
  if (pathMatch && pathMatch.index !== undefined) {
    const matchedPath = pathMatch[1].trim();
    const remainder = text.slice(pathMatch.index + matchedPath.length).replace(/^[—–\-:：\s]+/, "").trim();
    return {
      cleanPath: matchedPath,
      description: remainder.length > 0 ? remainder : undefined,
    };
  }

  return { cleanPath: text };
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
    if (typeof item === "string" && (item.trim() === "[object Object]" || !item.trim())) return;
    const normalized = normalizeArtifact(item, index);
    if (!normalized.localPath && !normalized.content && !normalized.previewUrl && !normalized.artifactId) {
      // 如果没有任何真实内容/路径/账本ID，且名称是无意义占位符，不展示无效卡片
      if (normalized.name.startsWith("artifact-") || normalized.name === "[object Object]") {
        return;
      }
    }
    const key = normalized.artifactId || normalized.localPath || normalized.content || normalized.id;
    if (!seenIds.has(key)) {
      seenIds.add(key);
      result.push(normalized);
    }
  });

  return result;
}

/**
 * 原生安全在系统中打开交付物文件 (调用系统关联默认程序如 PDF 查看器/Excel/代码编辑器)
 */
export async function openArtifactInSystem(path: string): Promise<boolean> {
  const safePath = extractSafeString(path);
  if (!safePath) {
    console.warn("未提供有效的本地文件路径");
    return false;
  }
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("open_artifact_in_system", { path: safePath });
    return true;
  } catch (err) {
    console.error("在系统默认程序中打开文件失败:", err);
    return false;
  }
}

/**
 * 在系统文件管理器中高亮定位交付物文件
 */
export async function showArtifactInFolder(path: string): Promise<boolean> {
  const safePath = extractSafeString(path);
  if (!safePath) {
    console.warn("未提供有效的本地文件路径");
    return false;
  }
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("show_artifact_in_folder", { path: safePath });
    return true;
  } catch (err) {
    console.error("在文件管理器中定位失败:", err);
    return false;
  }
}

/**
 * 异步安全读取本地交付物文件内容（支持 Markdown / HTML / 代码 / JSON 等文本格式）
 */
export async function readArtifactFile(path: string): Promise<string | null> {
  const safePath = extractSafeString(path);
  if (!safePath) return null;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const content = await invoke<string>("read_artifact_file", { path: safePath });
    return content;
  } catch (err) {
    console.warn("读取本地产物文件失败:", err);
    return null;
  }
}

/**
 * 将相对路径解析为操作系统绝对路径
 */
export async function resolveArtifactAbsolutePath(path: string): Promise<string> {
  const safePath = extractSafeString(path);
  if (!safePath) return path;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const absPath = await invoke<string>("resolve_artifact_absolute_path", { path: safePath });
    return absPath || safePath;
  } catch {
    return safePath;
  }
}

// ==================== 宿主受管账本 API (基于 artifact_id) ====================

export interface ManagedArtifactRecordRaw {
  id: string;
  session_id: string;
  run_id?: string;
  display_name: string;
  format: string;
  mime_type?: string;
  kind: string;
  canonical_path: string;
  size_bytes: number;
  sha256?: string;
  origin: string;
  status: string;
  created_at: string;
  updated_at: string;
}

/**
 * 转换 Rust 账本记录为前端统一 ArtifactDescriptor
 */
export function normalizeManagedArtifactRecord(rec: ManagedArtifactRecordRaw): ArtifactDescriptor {
  return {
    id: rec.id,
    artifactId: rec.id,
    name: rec.display_name,
    kind: (rec.kind as ArtifactKind) || inferArtifactKind(rec.format),
    format: rec.format,
    mimeType: rec.mime_type,
    sizeBytes: rec.size_bytes,
    createdAt: rec.created_at,
    localPath: rec.canonical_path,
    status: rec.status as "ready" | "deleted" | "missing",
    origin: rec.origin as "managed_generated" | "workspace_write" | "external_import",
    description: `受管交付物 (ID: ${rec.id})`,
  };
}

/**
 * 从宿主 SQLite 账本拉取指定会话的全部受管交付物列表
 */
export async function listSessionArtifacts(sessionId: string): Promise<ArtifactDescriptor[]> {
  if (!sessionId || !sessionId.trim()) return [];
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const records = await invoke<ManagedArtifactRecordRaw[]>("list_session_artifacts", { sessionId });
    return (records || []).map(normalizeManagedArtifactRecord);
  } catch (err) {
    console.warn("查询受管产物账本失败:", err);
    return [];
  }
}

/**
 * 根据 artifact_id 读取产物文件正文（安全、免传裸路径）
 */
export async function readArtifactById(artifactId: string): Promise<string | null> {
  const safeId = extractSafeString(artifactId);
  if (!safeId) return null;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    const content = await invoke<string>("read_artifact_by_id", { artifactId: safeId });
    return content;
  } catch (err) {
    console.warn(`读取受管产物失败 (ID: ${safeId}):`, err);
    return null;
  }
}

/**
 * 根据 artifact_id 在系统中打开受管文件
 */
export async function openArtifactById(artifactId: string): Promise<boolean> {
  const safeId = extractSafeString(artifactId);
  if (!safeId) return false;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("open_artifact_by_id", { artifactId: safeId });
    return true;
  } catch (err) {
    console.error(`在系统中打开受管产物失败 (ID: ${safeId}):`, err);
    return false;
  }
}

/**
 * 根据 artifact_id 在系统文件管理器中高亮定位受管文件
 */
export async function revealArtifactById(artifactId: string): Promise<boolean> {
  const safeId = extractSafeString(artifactId);
  if (!safeId) return false;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("reveal_artifact_by_id", { artifactId: safeId });
    return true;
  } catch (err) {
    console.error(`在文件管理器中定位受管产物失败 (ID: ${safeId}):`, err);
    return false;
  }
}

/**
 * 根据 artifact_id 删除受管产物文件与账本状态
 */
export async function deleteArtifactById(artifactId: string): Promise<boolean> {
  const safeId = extractSafeString(artifactId);
  if (!safeId) return false;
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("delete_artifact_by_id", { artifactId: safeId });
    return true;
  } catch (err) {
    console.error(`删除受管产物失败 (ID: ${safeId}):`, err);
    return false;
  }
}




