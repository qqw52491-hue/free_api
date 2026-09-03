import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

export interface PersistedConversationMessage {
    id: string;
    session_id: string;
    role: "user" | "assistant" | string;
    content: string;
    attachments?: string | null;
    created_at: string;
    model_id?: string | null;
}

export function usePersistedAgentConversation(
    sessionId: string | (() => string),
    onError?: (message: string) => void
) {
    const getSessionId = typeof sessionId === "function" ? sessionId : () => sessionId;
    const conversationMessages = ref<PersistedConversationMessage[]>([]);
    const isConversationLoading = ref(false);

    async function loadConversation() {
        const sid = getSessionId();
        if (!sid || sid.startsWith("draft-")) {
            conversationMessages.value = [];
            return;
        }
        isConversationLoading.value = true;
        try {
            const messages = await invoke<PersistedConversationMessage[]>("get_messages", { sessionId: sid });
            conversationMessages.value = Array.isArray(messages) ? messages : [];
        } catch (error) {
            onError?.(error instanceof Error ? error.message : String(error));
        } finally {
            isConversationLoading.value = false;
        }
    }

    return { conversationMessages, isConversationLoading, loadConversation };
}
