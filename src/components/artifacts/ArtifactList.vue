<template>
  <div v-if="normalizedList.length" class="artifact-list-container">
    <div v-if="showHeader" class="artifact-list-header">
      <div class="header-left">
        <span class="header-icon">📦</span>
        <strong class="header-title">交付物清单</strong>
        <span class="header-count">{{ normalizedList.length }} 项产物</span>
      </div>
      <div class="header-tip">已完成并归档</div>
    </div>

    <div class="artifact-cards-grid">
      <ArtifactCard
        v-for="art in normalizedList"
        :key="art.id"
        :artifact="art"
        @preview="$emit('preview', $event)"
        @download="$emit('download', $event)"
      />
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from "vue";
import type { AgentArtifactInput, ArtifactDescriptor } from "../../types/artifact";
import { normalizeArtifactList } from "../../utils/artifact";
import ArtifactCard from "./ArtifactCard.vue";

const props = withDefaults(
  defineProps<{
    artifacts: AgentArtifactInput[];
    showHeader?: boolean;
  }>(),
  {
    showHeader: true,
  }
);

defineEmits<{
  preview: [artifact: ArtifactDescriptor];
  download: [artifact: ArtifactDescriptor];
}>();

const normalizedList = computed(() => normalizeArtifactList(props.artifacts));
</script>

<style scoped>
.artifact-list-container {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
  margin-top: 8px;
}

.artifact-list-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 4px 2px;
}

.header-left {
  display: flex;
  align-items: center;
  gap: 6px;
}

.header-icon {
  font-size: 14px;
}

.header-title {
  font-size: 12.5px;
  font-weight: 600;
  color: #e4e4e7;
}

.header-count {
  font-size: 11px;
  color: #a5b4fc;
  background: rgba(99, 102, 241, 0.15);
  border: 1px solid rgba(99, 102, 241, 0.25);
  padding: 0 5px;
  border-radius: 99px;
}

.header-tip {
  font-size: 11px;
  color: #71717a;
}

.artifact-cards-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
  gap: 8px;
}

@media (max-width: 640px) {
  .artifact-cards-grid {
    grid-template-columns: 1fr;
  }
}
</style>
