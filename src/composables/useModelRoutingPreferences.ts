import { effectScope, ref, watch } from "vue";
import {
    CONTEXT_BUDGET_MODES,
    STATE_CARD_LEVELS,
    type AgentModelOption,
    type ModelRouting,
    type ReasoningEffort,
} from "../components/agent/types";

const STORAGE_KEY = "free-api.agent.model-routing.v1";
const EFFORTS: ReasoningEffort[] = ["auto", "off", "low", "medium", "high"];

function migrateEffort(value: unknown, fallback: ReasoningEffort, legacy: boolean): ReasoningEffort {
    if (value === "none") return "off"; // Older project page option.
    if (value === "max") return "high";
    if (legacy && value === "high") return "medium";
    return EFFORTS.includes(value as ReasoningEffort) ? value as ReasoningEffort : fallback;
}

function loadPreferences(): ModelRouting {
    const defaults: ModelRouting = {
        reasoning_effort_version: 2,
        pro_reasoning_effort: "auto",
        flash_reasoning_effort: "auto",
        vision_reasoning_effort: "auto",
        context_budget_mode: "deep_coding",
        routing_mode: "rules",
        role_models: {},
        stage_review_committee: false,
        reviewer_backup_model: "",
    };
    try {
        const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) || "{}") as Partial<ModelRouting> & { reasoning_effort?: unknown };
        const legacy = stored.reasoning_effort_version !== 2;
        const sharedEffort = migrateEffort(stored.reasoning_effort, "auto", legacy);
        return {
            reasoning_effort_version: 2,
            pro: typeof stored.pro === "string" ? stored.pro : undefined,
            flash: typeof stored.flash === "string" ? stored.flash : undefined,
            vision: typeof stored.vision === "string" ? stored.vision : undefined,
            pro_reasoning_effort: migrateEffort(stored.pro_reasoning_effort, sharedEffort, legacy),
            flash_reasoning_effort: migrateEffort(stored.flash_reasoning_effort, sharedEffort, legacy),
            vision_reasoning_effort: migrateEffort(stored.vision_reasoning_effort, sharedEffort, legacy),
            context_budget_mode: CONTEXT_BUDGET_MODES.find(mode => mode.id === stored.context_budget_mode)?.id ?? defaults.context_budget_mode,
            state_card_level: STATE_CARD_LEVELS.find(level => level.id === stored.state_card_level)?.id,
            routing_mode: stored.routing_mode === "scored" ? "scored" : "rules",
            role_models: stored.role_models && typeof stored.role_models === "object" && !Array.isArray(stored.role_models)
                ? Object.fromEntries(Object.entries(stored.role_models).filter(([, model]) => typeof model === "string"))
                : defaults.role_models,
            stage_reviewer_override: typeof stored.stage_reviewer_override === "string" ? stored.stage_reviewer_override : undefined,
            stage_review_committee: stored.stage_review_committee === true,
            stages: stored.stages && typeof stored.stages === "object" ? Object.fromEntries(
                (["plan", "plan_review", "stage_review", "final_review"] as const)
                    .filter(key => typeof stored.stages?.[key] === "boolean")
                    .map(key => [key, stored.stages?.[key]]),
            ) : undefined,
            reviewer_backup_model: typeof stored.reviewer_backup_model === "string" ? stored.reviewer_backup_model.trim() : "",
        };
    } catch {
        return defaults;
    }
}

const modelRouting = ref(loadPreferences());
// Preferences outlive the first panel that starts using them.
effectScope(true).run(() => {
    watch(modelRouting, (value) => {
        try { localStorage.setItem(STORAGE_KEY, JSON.stringify(value)); }
        catch { /* The open workspace can still share in-memory preferences. */ }
    }, { deep: true, flush: "sync" });
});

export function useModelRoutingPreferences() {
    return modelRouting;
}

export function fillDefaultModelRoutes(models: AgentModelOption[]) {
    const first = models[0]?.id;
    const current = modelRouting.value;
    if (!first || (current.pro !== undefined && current.flash !== undefined && current.vision !== undefined)) return;

    const label = (model: AgentModelOption) => `${model.display_name} ${model.name}`;
    const flash = /flash|mini|haiku|speed|turbo/i;
    const pro = /pro|opus|claude-3-5|gpt-4o|deepseek/i;
    modelRouting.value = {
        ...current,
        pro: current.pro ?? models.find(model => /deepseek.*v4.*pro|v4.*pro.*deepseek/i.test(label(model)))?.id
            ?? models.find(model => !flash.test(label(model)) && pro.test(label(model)))?.id ?? first,
        flash: current.flash ?? models.find(model => /deepseek.*v4.*flash|v4.*flash.*deepseek/i.test(label(model)))?.id
            ?? models.find(model => flash.test(label(model)))?.id ?? first,
        vision: current.vision ?? models.find(model => /vision|vl|multimodal|gpt-4o/i.test(label(model)))?.id ?? first,
    };
}
