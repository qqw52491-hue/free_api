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
  downloadUrl?: string;
  previewUrl?: string;
  localPath?: string;
  content?: string;
  description?: string;
}

export type AgentArtifactInput = string | Partial<ArtifactDescriptor>;
