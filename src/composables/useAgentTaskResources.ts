import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { AgentTaskRecordPage, AgentTaskResource } from "../components/agent/types";
import { asRecord, numberOrUndefined, shortText, toText } from "../components/agent/format";

const RESOURCE_PAGE_SIZE = 20;

function normalizeResource(data: Record<string, unknown>, index: number): AgentTaskResource | null {
    const resourceId = toText(data.resource_id || data.resourceId || data.id || `context-resource-${index + 1}`);
    if (!resourceId) return null;
    return {
        resourceId,
        kind: toText(data.kind || data.type || "resource"),
        label: toText(data.label || data.name || data.title || resourceId),
        rowCount: (data.row_count as number | string | undefined) ?? (data.rowCount as number | string | undefined),
        status: toText(data.status || "available"),
        summary: shortText(toText(data.summary || data.description || ""), 360),
        cursor: toText(data.cursor || data.next_cursor || data.nextCursor || ""),
    };
}

function normalizeRecordPage(payload: unknown, fallbackResourceId: string): AgentTaskRecordPage {
    const data = asRecord(payload);
    const page = asRecord(data.page);
    const records = (Array.isArray(data.records) ? data.records : []).map((entry, index) => {
        const record = asRecord(entry);
        return {
            key: toText(record.key || `record-${index + 1}`),
            data: record.data ?? record,
            updatedAt: toText(record.updated_at || record.updatedAt || ""),
        };
    });
    return {
        resourceId: toText(data.resource_id || data.resourceId || fallbackResourceId),
        records,
        offset: numberOrUndefined(page.offset) ?? 0,
        limit: numberOrUndefined(page.limit) ?? RESOURCE_PAGE_SIZE,
        total: numberOrUndefined(page.total) ?? records.length,
        hasMore: page.has_more === true || page.hasMore === true,
    };
}

export function useAgentTaskResources(sessionId: string) {
    const resources = ref<AgentTaskResource[]>([]);
    const isResourcesLoading = ref(false);
    const resourceLoadError = ref("");
    const selectedResource = ref<AgentTaskResource | null>(null);
    const isRecordsLoading = ref(false);
    const recordLoadError = ref("");
    const recordPage = ref<AgentTaskRecordPage>({ resourceId: "", records: [], offset: 0, limit: RESOURCE_PAGE_SIZE, total: 0, hasMore: false });
    let refreshTimer: ReturnType<typeof setTimeout> | null = null;

    function mergeResources(nextResources: AgentTaskResource[]) {
        const merged = new Map(resources.value.map((resource) => [resource.resourceId, resource]));
        nextResources.forEach((resource) => merged.set(resource.resourceId, { ...merged.get(resource.resourceId), ...resource }));
        resources.value = [...merged.values()].slice(0, 40);
    }

    function mergeContextResources(rawResources: unknown[]) {
        const nextResources = rawResources
            .map((entry, index) => normalizeResource(asRecord(entry), index))
            .filter((resource): resource is AgentTaskResource => resource !== null);
        if (nextResources.length) mergeResources(nextResources);
        return nextResources;
    }

    async function refreshResources() {
        if (sessionId.startsWith("draft-") || isResourcesLoading.value) return;
        isResourcesLoading.value = true;
        resourceLoadError.value = "";
        try {
            const payload = await invoke<unknown[]>("get_agent_task_resources", { sessionId });
            mergeContextResources(Array.isArray(payload) ? payload : []);
        } catch (error) {
            resourceLoadError.value = toText(error) || "无法读取当前 session 的任务资源。";
        } finally {
            isResourcesLoading.value = false;
        }
    }

    function scheduleResourceRefresh() {
        if (refreshTimer) return;
        refreshTimer = setTimeout(() => {
            refreshTimer = null;
            void refreshResources();
        }, 250);
    }

    async function loadResourceRecords(offset = 0) {
        const resource = selectedResource.value;
        if (!resource || isRecordsLoading.value) return;
        isRecordsLoading.value = true;
        recordLoadError.value = "";
        try {
            const payload = await invoke<unknown>("get_agent_task_records", {
                sessionId,
                resourceId: resource.resourceId,
                offset,
                limit: RESOURCE_PAGE_SIZE,
            });
            recordPage.value = normalizeRecordPage(payload, resource.resourceId);
        } catch (error) {
            recordLoadError.value = toText(error) || "无法读取该资源的记录。";
        } finally {
            isRecordsLoading.value = false;
        }
    }

    function openResourcePreview(resource: AgentTaskResource) {
        selectedResource.value = resource;
        recordPage.value = { resourceId: resource.resourceId, records: [], offset: 0, limit: RESOURCE_PAGE_SIZE, total: 0, hasMore: false };
        recordLoadError.value = "";
        void loadResourceRecords(0);
    }

    function closeResourcePreview() {
        selectedResource.value = null;
        recordLoadError.value = "";
    }

    function loadPreviousRecordPage() {
        void loadResourceRecords(Math.max(0, recordPage.value.offset - recordPage.value.limit));
    }

    function loadNextRecordPage() {
        if (recordPage.value.hasMore) void loadResourceRecords(recordPage.value.offset + recordPage.value.records.length);
    }

    function clearResources() {
        resources.value = [];
        resourceLoadError.value = "";
        selectedResource.value = null;
        recordLoadError.value = "";
        recordPage.value = { resourceId: "", records: [], offset: 0, limit: RESOURCE_PAGE_SIZE, total: 0, hasMore: false };
    }

    function disposeResources() {
        if (refreshTimer) clearTimeout(refreshTimer);
        refreshTimer = null;
    }

    return {
        resources,
        isResourcesLoading,
        resourceLoadError,
        selectedResource,
        isRecordsLoading,
        recordLoadError,
        recordPage,
        mergeContextResources,
        refreshResources,
        scheduleResourceRefresh,
        openResourcePreview,
        closeResourcePreview,
        loadPreviousRecordPage,
        loadNextRecordPage,
        clearResources,
        disposeResources,
    };
}
