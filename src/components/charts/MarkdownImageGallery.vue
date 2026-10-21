<template>
  <div class="markdown-image-gallery" :class="{ 'is-single': images.length === 1 }">
    <figure v-for="(image, index) in images" :key="`${index}:${image.src}`" class="image-figure">
      <button class="image-thumbnail" type="button" :disabled="failed(image)" :aria-label="`放大图片：${image.alt || image.title || index + 1}`" @click="open(index)">
        <img v-if="!failed(image)" :key="`${image.src}:${retries[image.src] || 0}`" :src="thumbnailSource(image.src)" :alt="image.alt" :title="image.title || undefined" loading="lazy" decoding="async" @error="errors.add(image.src)" />
        <span v-else class="image-error">图片加载失败</span>
      </button>
      <figcaption v-if="image.title || image.alt || image.sourceHref || failed(image)">
        <span class="image-caption">{{ image.title || image.alt }}</span>
        <a v-if="image.sourceHref" :href="image.sourceHref" target="_blank" rel="noopener noreferrer">来源</a>
        <button v-if="failed(image) && image.src" type="button" class="image-retry" @click="retry(image.src)">重试</button>
      </figcaption>
    </figure>
  </div>
  <dialog ref="dialog" class="image-preview-dialog" aria-label="图片预览" @close="activeIndex = null" @click="closeOnBackdrop" @keydown.left.prevent="move(-1)" @keydown.right.prevent="move(1)">
    <template v-if="activeImage">
      <header class="image-preview-toolbar">
        <span>{{ activeIndex! + 1 }} / {{ previewImages.length }}</span>
        <button v-if="previewImages.length > 1" type="button" aria-label="上一张图片" @click="move(-1)">←</button>
        <button v-if="previewImages.length > 1" type="button" aria-label="下一张图片" @click="move(1)">→</button>
        <button type="button" aria-label="关闭图片预览" autofocus @click="close">关闭 ✕</button>
      </header>
      <div class="image-preview-body">
        <img v-if="!previewErrors.has(activeImage.src)" :src="activeImage.src" :alt="activeImage.alt" @error="previewErrors.add(activeImage.src)" />
        <p v-else>图片加载失败，请关闭预览后重试。</p>
      </div>
      <footer>{{ activeImage.title || activeImage.alt }} <a v-if="activeImage.sourceHref" :href="activeImage.sourceHref" target="_blank" rel="noopener noreferrer">来源</a></footer>
    </template>
  </dialog>
</template>

<script setup lang="ts">
import { computed, nextTick, reactive, ref } from 'vue';
import type { MarkdownImage } from './markdownImages';
const props = defineProps<{ images: MarkdownImage[] }>();
const errors = reactive(new Set<string>());
const previewErrors = reactive(new Set<string>());
const retries = reactive<Record<string, number>>({});
const dialog = ref<HTMLDialogElement | null>(null);
const activeIndex = ref<number | null>(null);
// Freeze an opened gallery while later streamed Markdown continues to arrive.
const previewImages = ref<MarkdownImage[]>([]);
const activeImage = computed(() => activeIndex.value == null ? null : previewImages.value[activeIndex.value]);
function failed(image: MarkdownImage) { return !image.src || errors.has(image.src); }
function retry(src: string) { retries[src] = (retries[src] || 0) + 1; errors.delete(src); }
function thumbnailSource(src: string) {
  // Replacing <img> alone can reuse a cached decoding failure. Only add a cache
  // marker to query-free HTTP URLs; existing query parameters may be signed.
  if (!retries[src] || !/^https?:\/\//i.test(src)) return src;
  const url = new URL(src);
  if (url.search) return src;
  url.searchParams.set('_image_retry', String(retries[src]));
  return url.href;
}
async function open(index: number) {
  if (failed(props.images[index])) return;
  previewImages.value = props.images.map(image => ({ ...image, src: thumbnailSource(image.src) }));
  previewErrors.clear();
  activeIndex.value = index;
  await nextTick();
  dialog.value?.showModal();
}
function close() { dialog.value?.close(); }
function closeOnBackdrop(event: MouseEvent) { if (event.target === dialog.value) close(); }
function move(offset: number) {
  if (activeIndex.value != null) activeIndex.value = (activeIndex.value + offset + previewImages.value.length) % previewImages.value.length;
}
</script>

<style scoped>
/* At most three columns, each at least 160px when space permits; follows the
   content width, including narrow inspector panes inside a wide window. */
.markdown-image-gallery { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, max(160px, calc((100% - 20px) / 3))), 1fr)); gap: 10px; margin: 6px 0; width: 100%; }
.image-figure { margin: 0; min-width: 0; }
.image-thumbnail { display: flex; align-items: center; justify-content: center; width: 100%; aspect-ratio: 4 / 3; overflow: hidden; border: 1px solid rgba(255,255,255,.1); border-radius: 10px; padding: 0; background: #1b1b1f; color: #a1a1aa; cursor: zoom-in; }
.image-thumbnail img { display: block; width: 100%; height: 100%; object-fit: contain; }
.is-single { grid-template-columns: minmax(0, 1fr); }
.is-single .image-figure { width: fit-content; max-width: 100%; }
.is-single .image-thumbnail { width: fit-content; max-width: 100%; min-width: 160px; min-height: 100px; aspect-ratio: auto; }
.is-single .image-thumbnail img { width: auto; height: auto; max-width: 100%; max-height: 420px; }
.image-thumbnail:disabled { cursor: default; }
.image-error { padding: 20px; font-size: 12px; }
figcaption { display: flex; gap: 8px; align-items: baseline; padding: 5px 2px 0; color: #a1a1aa; font-size: 11.5px; line-height: 1.5; }
.image-caption { flex: 1; min-width: 0; overflow-wrap: anywhere; }
a { color: #93c5fd; text-decoration: none; flex-shrink: 0; }
button { font: inherit; }
.image-retry { background: transparent; border: 0; padding: 0; color: #93c5fd; cursor: pointer; }
button:focus-visible, a:focus-visible { outline: 2px solid #818cf8; outline-offset: 3px; }
.image-preview-dialog { position: fixed; inset: 0; width: min(1100px, 94vw); max-width: 94vw; max-height: 92vh; margin: auto; padding: 12px; border: 1px solid #3f3f46; border-radius: 12px; color: #e4e4e7; background: #18181b; }
.image-preview-dialog::backdrop { background: rgba(0,0,0,.8); }
.image-preview-toolbar { display: flex; align-items: center; gap: 8px; padding-bottom: 10px; }
.image-preview-toolbar span { flex: 1; font-size: 12px; color: #a1a1aa; }
.image-preview-toolbar button { border: 1px solid #3f3f46; background: #27272a; color: #e4e4e7; padding: 5px 10px; border-radius: 5px; cursor: pointer; }
.image-preview-body { display: flex; align-items: center; justify-content: center; min-height: 100px; }
.image-preview-body img { display: block; max-width: 100%; max-height: 70vh; object-fit: contain; }
footer { padding-top: 8px; font-size: 12px; color: #a1a1aa; overflow-wrap: anywhere; }
</style>
