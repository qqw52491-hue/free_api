import type { Token, Tokens } from 'marked';
import { resolveMarkdownMediaSource } from '../agent/format';

export interface MarkdownImage {
  src: string;
  alt: string;
  title: string;
  sourceHref?: string;
}

function httpLink(href: string): boolean {
  try { return /^https?:$/.test(new URL(href).protocol); } catch { return false; }
}

function imageSource(href: string): string {
  if (!href || /[\u0000-\u001f\u007f]/.test(href)) return '';
  // Vue-bound image sources do not pass through the ordinary HTML sanitizer.
  // Keep local conversion identical to Markdown, and reject executable URI schemes.
  const local = href.startsWith('/') || /^[A-Za-z]:[\\/]/.test(href) || /^file:\/\//i.test(href);
  const data = /^data:image\/(?:png|jpe?g|gif|webp|avif|bmp|svg\+xml);/i.test(href);
  if (!local && !data && !httpLink(href) && !/^asset:/i.test(href) && /^[^/?#]*:/.test(href)) return '';
  try { return resolveMarkdownMediaSource(href); } catch { return ''; }
}

// Only standalone image paragraphs become galleries. Prose, code examples,
// linked text, and audio/video keep their existing Markdown rendering.
export function standaloneImages(token: Token): MarkdownImage[] | null {
  if (token.type !== 'paragraph' || !Array.isArray(token.tokens)) return null;
  const images: MarkdownImage[] = [];
  for (const part of token.tokens as Token[]) {
    if (part.type === 'br' || (part.type === 'text' && !part.text.trim())) continue;
    let image: Tokens.Image;
    let sourceHref: string | undefined;
    if (part.type === 'image') image = part as Tokens.Image;
    else if (part.type === 'link' && part.tokens?.length === 1 && part.tokens[0].type === 'image') {
      image = part.tokens[0] as Tokens.Image;
      // A non-web link keeps its original navigation behavior instead of being swallowed.
      if (!httpLink(part.href)) return null;
      sourceHref = part.href;
    } else return null;
    if (/\.(?:mp4|webm|mov|mkv|avi|mp3|wav|ogg|m4a|flac|aac)$/i.test(image.href.split(/[?#]/)[0])) return null;
    images.push({ src: imageSource(image.href), alt: image.text, title: image.title || '', sourceHref });
  }
  return images.length ? images : null;
}
