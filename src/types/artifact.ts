export type ArtifactKind =
  | "document"
  | "web"
  | "image"
  | "chart"
  | "spreadsheet"
  | "presentation"
  | "code"
  | "file";

export interface ArtifactDescriptor {
  id: string;
  name: string;
  kind: ArtifactKind;
  format: string; // "md" | "html" | "pdf" | "docx" | "xlsx" | "png" | "json" ...
  mimeType?: string;
  sizeBytes?: number;
  createdAt?: string;

  // 来源与定位
  artifactId?: string; // 宿主受管账本唯一 ID (如 art_xxxx)
  downloadUrl?: string;
  previewUrl?: string;
  localPath?: string;
  content?: string;
  description?: string;
  status?: "ready" | "deleted" | "missing";
  origin?: "managed_generated" | "workspace_write" | "external_import";
}

export type AgentArtifactInput = string | Partial<ArtifactDescriptor>;
