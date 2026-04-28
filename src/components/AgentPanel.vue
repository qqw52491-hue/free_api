<template>
    <div class="agent-instance">
        <!-- 左侧：控制面板 -->
        <aside class="agent-sidebar">
            <div class="sidebar-header">
                <div class="sidebar-title">
                    <span class="title-icon">🤖</span>
                    <span>Agent #{{ panelId.slice(-4) }}</span>
                </div>
                <div class="sidebar-subtitle">独立任务执行单元</div>
            </div>

            <!-- 模型配置面板 -->
            <div class="control-section">
                <label class="form-label" style="margin-bottom: 8px;">模型网关配置</label>
                
                <div class="routing-item">
                    <span class="routing-label" style="font-size: 12px; color: #666;">🧠 主力推理模型 (Pro)</span>
                    <select class="form-select" v-model="modelRouting.pro" :disabled="isRunning">
                        <option value="" disabled>选择复杂推理模型…</option>
                        <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                            <option v-for="m in group.models" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
                        </optgroup>
                    </select>
                </div>

                <div class="routing-item" style="margin-top: 8px;">
                    <span class="routing-label" style="font-size: 12px; color: #666;">⚡ 极速执行模型 (Flash)</span>
                    <select class="form-select" v-model="modelRouting.flash" :disabled="isRunning">
                        <option value="" disabled>选择快速执行模型…</option>
                        <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                            <option v-for="m in group.models" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
                        </optgroup>
                    </select>
                </div>

                <div class="routing-item" style="margin-top: 8px;">
                    <span class="routing-label" style="font-size: 12px; color: #666;">👁️ 视觉多模态模型 (Vision)</span>
                    <select class="form-select" v-model="modelRouting.vision" :disabled="isRunning">
                        <option value="" disabled>选择多模态看图模型…</option>
                        <optgroup v-for="group in groupedModels" :key="group.platform" :label="group.platform">
                            <option v-for="m in group.models" :key="m.id" :value="m.id">{{ m.display_name || m.name }}</option>
                        </optgroup>
                    </select>
                </div>
            </div>

            <!-- 浏览器模式选择 -->
            <div class="control-section">
                <label class="form-label">浏览器模式</label>
                <select
                    class="form-select"
                    v-model="selectedBrowserMode"
                    :disabled="isRunning"
                >
                    <option :value="0">🧹 临时模式 (无痕/干净)</option>
                    <option :value="1">💾 持久化模式 (保留登录态)</option>
                    <option :value="2">🔗 连接模式 (接管 Chrome:9222)</option>
                </select>
                <div class="form-tip" v-if="selectedBrowserMode === 1">建议：首次登录后自动保持，无需重复扫码。</div>
                <div class="form-tip connect-tip" v-if="selectedBrowserMode === 2">提示：请先以远程调试模式启动 Chrome。</div>
            </div>

            <!-- 快捷预设 -->
            <div class="control-section">
                <label class="form-label">快捷预设</label>
                <div class="preset-list">
                    <button
                        v-for="preset in presets"
                        :key="preset.label"
                        class="preset-btn"
                        :class="{ active: goalInput === preset.goal }"
                        @click="goalInput = preset.goal"
                        :disabled="isRunning"
                    >
                        <span class="preset-icon">{{ preset.icon }}</span>
                        <span>{{ preset.label }}</span>
                    </button>
                </div>
            </div>

            <!-- 目标输入 -->
            <div
                class="control-section"
                style="flex: 1; display: flex; flex-direction: column; gap: 8px"
            >
                <label class="form-label">任务目标</label>
                <textarea
                    class="form-textarea goal-input"
                    v-model="goalInput"
                    placeholder="描述你想让 AI 完成的任务…&#10;&#10;例如：&#10;• 在桌面创建一个 notes.txt 文件并写入当前时间&#10;• 显示当前目录下所有文件&#10;• 弹窗提醒我去喝水"
                    :disabled="isRunning"
                    rows="6"
                ></textarea>
            </div>

            <!-- 执行按钮 -->
            <div class="control-section">
                <button
                    class="btn btn-primary run-btn"
                    @click="runAgent"
                    :disabled="
                        !modelRouting.pro || !modelRouting.flash || !modelRouting.vision || !goalInput.trim() || isRunning
                    "
                >
                    <span v-if="isRunning" class="spinning">⟳</span>
                    <span v-else>▶</span>
                    {{ isRunning ? "执行中…" : "开始执行" }}
                </button>
                <button
                    v-if="steps.length > 0 && !isRunning"
                    class="btn btn-ghost clear-btn"
                    @click="clearAll"
                >
                    清空记录
                </button>
            </div>
        </aside>

        <!-- 右侧：执行面板 -->
        <main class="agent-main">
            <!-- 空状态 -->
            <div
                v-if="steps.length === 0 && !isRunning && !planningMessage"
                class="empty-state"
            >
                <div class="empty-icon">🤖</div>
                <h2 class="empty-title">AI Agent 执行台</h2>
                <p class="empty-desc">
                    选择一个 AI 模型，输入你的目标，<br />
                    Agent 会自动规划步骤并执行
                </p>
                <div class="capability-grid">
                    <div class="cap-card">
                        <span class="cap-icon">📁</span>
                        <div class="cap-title">文件系统操作</div>
                        <div class="cap-desc">
                            创建、读取、移动、删除文件和文件夹
                        </div>
                    </div>
                    <div class="cap-card">
                        <span class="cap-icon">🌐</span>
                        <div class="cap-title">浏览器自动化</div>
                        <div class="cap-desc">
                            自动控制 Chrome 浏览器操作网页
                        </div>
                    </div>
                    <div class="cap-card">
                        <span class="cap-icon">⚡</span>
                        <div class="cap-title">Shell 命令</div>
                        <div class="cap-desc">
                            执行任意 bash 命令，获取系统信息
                        </div>
                    </div>
                    <div class="cap-card">
                        <span class="cap-icon">🔄</span>
                        <div class="cap-title">循环检测</div>
                        <div class="cap-desc">
                            每步完成后自动检测状态，失败即停止
                        </div>
                    </div>
                </div>
            </div>

            <!-- 规划中状态 -->
            <div
                v-if="planningMessage && steps.length === 0"
                class="planning-state"
            >
                <div class="planning-spinner">
                    <div class="spinner-ring"></div>
                    <span class="spinner-icon">🧠</span>
                </div>
                <div class="planning-text">{{ planningMessage }}</div>
            </div>

            <!-- 任务执行面板 -->
            <div
                v-if="steps.length > 0"
                class="execution-panel"
                ref="execPanel"
            >
                <!-- 进度概览 -->
                <div class="progress-header">
                    <div class="header-left">
                        <div class="progress-goal">
                            <span class="goal-badge">目标</span>
                            <span class="goal-text">{{ currentGoal }}</span>
                        </div>
                        <div class="progress-stats">
                            <span class="stat done">✓ {{ doneCount }}</span>
                            <span class="stat fail" v-if="errorCount > 0"
                                >✕ {{ errorCount }}</span
                            >
                            <span class="stat total">/ {{ steps.length }}</span>
                        </div>
                    </div>

                    <!-- 📊 Token 用量统计栏 -->
                    <div v-if="tokenUsage" class="agent-token-bar">
                        <div class="agent-token-stats">
                            <span class="token-label">Token</span>
                            <span class="token-item">⬆️{{ tokenUsage.prompt_tokens }}</span>
                            <span class="token-item">⬇️{{ tokenUsage.completion_tokens }}</span>
                        </div>
                        <div class="agent-token-ctx">
                            <div class="ctx-bar">
                                <div class="ctx-fill" :style="{ width: Math.min(tokenUsage.usage_percent, 100) + '%' }" :class="{ warning: tokenUsage.usage_percent > 70, danger: tokenUsage.usage_percent > 90 }"></div>
                            </div>
                            <span class="ctx-text">{{ tokenUsage.total_tokens }}/{{ tokenUsage.context_window }} ({{ tokenUsage.usage_percent.toFixed(1) }}%)</span>
                        </div>
                    </div>
                </div>

                <!-- 进度条 -->
                <div class="progress-bar-wrap">
                    <div
                        class="progress-bar-fill"
                        :style="{
                            width: progressPercent + '%',
                            background: hasError
                                ? 'var(--red)'
                                : 'var(--accent)',
                        }"
                    ></div>
                </div>

                <!-- 步骤列表 -->
                <div class="steps-list">
                    <div
                        v-for="step in steps"
                        :key="step.id"
                        class="step-card"
                        :class="[
                            step.status,
                            { expanded: expandedStep === step.id },
                        ]"
                        @click="toggleExpand(step.id)"
                    >
                        <div class="step-icon-wrap">
                            <div class="step-icon" :class="step.status">
                                <span v-if="step.status === 'pending'">{{
                                    step.id + 1
                                }}</span>
                                <span
                                    v-else-if="step.status === 'running'"
                                    class="spinning"
                                    >⟳</span
                                >
                                <span v-else-if="step.status === 'done'"
                                    >✓</span
                                >
                                <span v-else-if="step.status === 'error'"
                                    >✕</span
                                >
                            </div>
                            <div
                                class="step-connector"
                                v-if="step.id < steps.length - 1"
                                :class="step.status === 'done' ? 'filled' : ''"
                            ></div>
                        </div>

                        <div class="step-content">
                            <div class="step-header">
                                <span class="step-desc">{{
                                    step.description
                                }}</span>
                                <div class="step-meta">
                                    <span
                                        class="step-tool-badge"
                                        :class="step.tool"
                                        >{{ step.tool }}</span
                                    >
                                    <span class="step-expand-icon">{{
                                        expandedStep === step.id ? "▲" : "▼"
                                    }}</span>
                                </div>
                            </div>

                            <transition name="expand">
                                <div
                                    v-if="expandedStep === step.id"
                                    class="step-details"
                                >
                                    <div class="detail-block" v-if="step.thought">
                                        <div class="detail-label">💭 思考过程</div>
                                        <div class="detail-thought text-accent-light">{{ step.thought }}</div>
                                    </div>
                                    <div class="detail-block" v-if="step.thinking">
                                        <div class="detail-label">🧠 模型思考</div>
                                        <pre class="detail-thinking">{{ step.thinking }}</pre>
                                    </div>
                                    <div class="detail-block">
                                        <div class="detail-label">🔧 指令详情</div>
                                        <pre class="detail-cmd">{{
                                            step.command
                                        }}</pre>
                                    </div>
                                    <div
                                        class="detail-block"
                                        v-if="step.output"
                                    >
                                        <div class="detail-label">输出</div>
                                        <pre
                                            class="detail-output"
                                            :class="{
                                                error: step.status === 'error',
                                            }"
                                            >{{ step.output }}</pre
                                        >
                                    </div>
                                    <div
                                        v-if="step.status === 'running'"
                                        class="detail-running"
                                    >
                                        <div class="loading-dots">
                                            <span></span><span></span
                                            ><span></span>
                                        </div>
                                        <span>正在执行…</span>
                                    </div>
                                </div>
                            </transition>
                        </div>
                    </div>
                </div>

                <!-- 完成 banner -->
                <transition name="fade">
                    <div
                        v-if="completionMessage"
                        class="completion-banner"
                        :class="{ success: !hasError, error: hasError }"
                    >
                        <span class="completion-icon">{{
                            hasError ? "⚠️" : "🎉"
                        }}</span>
                        <span>{{ completionMessage }}</span>
                    </div>
                </transition>
            </div>

            <!-- 实时日志 -->
            <div v-if="logs.length > 0" class="log-panel">
                <div class="log-header">
                    <span>实时日志</span>
                    <button class="btn-icon btn-xs" @click="logs = []">
                        清空
                    </button>
                </div>
                <div class="log-list" ref="logList">
                    <div
                        v-for="(log, i) in logs"
                        :key="i"
                        class="log-line"
                        :class="log.type"
                    >
                        <span class="log-time">{{ log.time }}</span>
                        <span class="log-msg">{{ log.message }}</span>
                    </div>
                </div>
            </div>
        </main>
    </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---- Props ----
const props = defineProps<{
    panelId: string;
    allModels: { id: string; name: string; display_name: string; platform_id: string; platform_name: string; }[];
}>();

// ---- Types ----
interface AgentStep {
    id: number;
    description: string;
    thought: string;
    thinking: string;
    tool: string;
    command: string;
    status: "pending" | "running" | "done" | "error";
    output: string;
}
interface LogEntry {
    time: string;
    message: string;
    type: "info" | "success" | "error" | "warn";
}
interface TokenUsageInfo {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
    context_window: number;
    usage_percent: number;
}

// ---- 独立状态（每个面板完全隔离） ----
const sessionId = props.panelId;
const tokenUsage = ref<TokenUsageInfo | null>(null);
const modelRouting = ref({ pro: "", flash: "", vision: "" });
const goalInput = ref("");
const currentGoal = ref("");
const isRunning = ref(false);
const selectedBrowserMode = ref(1);
const steps = ref<AgentStep[]>([]);
const planningMessage = ref("");
const runningStep = ref<number | null>(null);
const completionMessage = ref("");
const hasError = ref(false);
const expandedStep = ref<number | null>(null);
const logs = ref<LogEntry[]>([]);
const execPanel = ref<HTMLElement>();
const logList = ref<HTMLElement>();

// ---- 预设 ----
const presets = [
    { icon: "🚀", label: "自动查Github", goal: "去 github.com 搜索关于 tauri 的最新项目，并阅读搜索结果页面的正文内容" },
    { icon: "📄", label: "创建笔记", goal: "在桌面创建一个名为 notes.txt 的文件，写入当前日期和时间，然后显示文件内容" },
    { icon: "📂", label: "查看桌面", goal: "列出桌面上所有文件和文件夹，统计数量" },
    { icon: "💧", label: "喝水提醒", goal: "用 osascript 弹出对话框提醒我喝水，然后再播放一个系统声音" },
    { icon: "📊", label: "磁盘信息", goal: "查看当前磁盘使用情况，以及 CPU 和内存占用信息" },
    { icon: "🗂️", label: "整理下载", goal: "列出下载文件夹中最近修改的 5 个文件，以及其大小" },
    { icon: "🖥️", label: "系统信息", goal: "获取 macOS 版本、主机名、当前用户名和 IP 地址" },
    { icon: "🤖", label: "调戏 Kimi", goal: "使用 browser_kimi 工具，自动去问 Kimi：请用一句话夸夸我的编程技术" },
];

// ---- 计算 ----
const groupedModels = computed(() => {
    const map = new Map<string, typeof props.allModels>();
    for (const m of props.allModels) {
        if (!map.has(m.platform_name)) map.set(m.platform_name, []);
        map.get(m.platform_name)!.push(m);
    }
    const groups: { platform: string; models: typeof props.allModels }[] = [];
    for (const [platform, models] of map) groups.push({ platform, models });
    return groups;
});

const doneCount = computed(() => steps.value.filter((s) => s.status === "done").length);
const errorCount = computed(() => steps.value.filter((s) => s.status === "error").length);
const progressPercent = computed(() => {
    if (steps.value.length === 0) return 0;
    return Math.round(((doneCount.value + errorCount.value) / steps.value.length) * 100);
});

// ---- 事件监听器（挂载时注册，卸载时销毁） ----
let unlistenProgress: UnlistenFn | null = null;

onMounted(async () => {
    // 默认选中第一个模型
    if (props.allModels.length > 0) {
        const defaultId = props.allModels[0].id;
        modelRouting.value.pro = defaultId;
        modelRouting.value.flash = defaultId;
        modelRouting.value.vision = defaultId;
    }

    // 监听属于自己 sessionId 的事件
    unlistenProgress = await listen<any>(`agent-progress-${sessionId}`, (event) => {
        const { type, ...data } = event.payload;

        if (type === "planning") {
            planningMessage.value = data.message;
            addLog("info", data.message);
        } else if (type === "plan") {
            planningMessage.value = "";
            steps.value = data.steps;
            expandedStep.value = null;
            addLog("info", `AI 规划了 ${data.steps.length} 个执行步骤`);
            nextTick(() => execPanel.value?.scrollTo({ top: 0, behavior: "smooth" }));
        } else if (type === "step_new") {
            const idx = steps.value.findIndex(s => s.id === data.step.id);
            if (idx !== -1) {
                steps.value[idx] = { ...steps.value[idx], ...data.step, thinking: steps.value[idx].thinking };
            } else {
                steps.value.push(data.step);
            }
            addLog("info", `🤖 AI 规划了新动作: ${data.step.description}`);
        } else if (type === "step_start") {
            const s = steps.value.find((s) => s.id === data.step_id);
            if (s) s.status = "running";
            runningStep.value = data.step_id;
            expandedStep.value = data.step_id;
            addLog("info", `▶ 步骤 ${data.step_id + 1}: ${data.description}`);
        } else if (type === "step_done") {
            const s = steps.value.find((s) => s.id === data.step_id);
            if (s) { s.status = "done"; s.output = data.output; }
            runningStep.value = null;
            addLog("success", `  ✓ 完成 → ${data.output?.slice(0, 100) || "(无输出)"}`);
        } else if (type === "step_error") {
            const s = steps.value.find((s) => s.id === data.step_id);
            if (s) { s.status = "error"; s.output = data.output; }
            runningStep.value = null;
            addLog("error", `  ✕ 失败 → ${data.output}`);
        } else if (type === "complete") {
            isRunning.value = false;
            completionMessage.value = data.message;
            hasError.value = !data.success;
            addLog(data.success ? "success" : "error", `🏁 ${data.message}`);
        } else if (type === "token_usage") {
            tokenUsage.value = {
                prompt_tokens: data.prompt_tokens,
                completion_tokens: data.completion_tokens,
                total_tokens: data.total_tokens,
                context_window: data.context_window,
                usage_percent: data.usage_percent,
            };
            addLog("info", `📊 Token: ⬆️${data.prompt_tokens} ⬇️${data.completion_tokens} ∑${data.total_tokens} | 上下文: ${data.total_tokens}/${data.context_window} (${data.usage_percent.toFixed(1)}%)`);
        } else if (type === "thinking") {
            let s = steps.value.find((s) => s.id === data.step_id);
            if (!s) {
                s = {
                    id: data.step_id, description: "正在深入思考...", thought: "", thinking: "",
                    tool: "thinking", command: "", status: "running", output: ""
                };
                steps.value.push(s);
                runningStep.value = data.step_id;
                expandedStep.value = data.step_id;
                nextTick(() => { document.querySelector(".agent-instance")?.scrollTo({ top: 99999, behavior: "smooth" }); });
            }
            s.thinking = data.content;
            if (data.done) {
                addLog("info", `🧠 AI思考完毕 (约 ${data.content.length} 字)`);
            }
        } else if (type === "error") {
            isRunning.value = false;
            planningMessage.value = "";
            completionMessage.value = data.message;
            hasError.value = true;
            addLog("error", `❌ ${data.message}`);
        }
    });
});

onUnmounted(() => {
    // 清理事件监听，防止内存泄漏
    if (unlistenProgress) unlistenProgress();
});

// ---- 方法 ----
async function runAgent() {
    if (!modelRouting.value.pro || !modelRouting.value.flash || !modelRouting.value.vision || !goalInput.value.trim() || isRunning.value)
        return;

    isRunning.value = true;

    try {
        const modeDesc = await invoke<string>("set_browser_launch_mode", { 
            mode: selectedBrowserMode.value 
        });

        currentGoal.value = goalInput.value.trim();
        steps.value = [];
        completionMessage.value = "";
        hasError.value = false;
        runningStep.value = null;

        addLog("info", `🚀 开启特工大脑 (${modeDesc}): ${currentGoal.value}`);

        await invoke("run_agent_main_loop", {
            modelRouting: modelRouting.value,
            goal: currentGoal.value,
            autoPilot: true,
            session_id: sessionId,
        });
    } catch (e: any) {
        const msg = typeof e === "string" ? e : e?.message || "任务初始化失败";
        addLog("error", `❌ 启动失败: ${msg}`);
        isRunning.value = false;
    }
}

function clearAll() {
    steps.value = [];
    completionMessage.value = "";
    hasError.value = false;
    planningMessage.value = "";
    currentGoal.value = "";
    runningStep.value = null;
    isRunning.value = false;
}

function toggleExpand(id: number) {
    expandedStep.value = expandedStep.value === id ? null : id;
}

function addLog(type: LogEntry["type"], message: string) {
    const time = new Date().toLocaleTimeString("zh-CN", {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
    });
    logs.value.push({ time, message, type });
    nextTick(() => {
        if (logList.value) logList.value.scrollTop = logList.value.scrollHeight;
    });
}

// 暴露 isRunning 给父组件（用于标签页状态指示）
defineExpose({ isRunning });
</script>

<style scoped>
.agent-instance {
    display: flex;
    height: 100%;
    overflow: hidden;
}

/* ── 左侧控制面板 ── */
.agent-sidebar {
    width: 300px;
    min-width: 300px;
    background: var(--bg-1);
    border-right: 1px solid var(--border-1);
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: 20px;
    gap: 16px;
}
.sidebar-header { padding-bottom: 16px; border-bottom: 1px solid var(--border-1); }
.sidebar-title { display: flex; align-items: center; gap: 10px; font-size: 16px; font-weight: 700; color: var(--text-1); }
.title-icon { font-size: 20px; }
.sidebar-subtitle { font-size: 12px; color: var(--text-4); margin-top: 4px; padding-left: 30px; }
.control-section { display: flex; flex-direction: column; gap: 8px; }
.preset-list { display: flex; flex-direction: column; gap: 4px; }
.preset-btn { display: flex; align-items: center; gap: 8px; padding: 8px 10px; border-radius: var(--radius-sm); background: var(--surface-1); border: 1px solid var(--border-1); color: var(--text-2); font-family: var(--font); font-size: 12px; cursor: pointer; transition: all var(--transition); text-align: left; }
.preset-btn:hover { background: var(--surface-2); color: var(--text-1); border-color: var(--border-2); }
.preset-btn.active { background: rgba(108, 99, 255, 0.1); border-color: rgba(108, 99, 255, 0.3); color: var(--accent-light); }
.preset-btn:disabled { opacity: 0.4; cursor: not-allowed; }
.preset-icon { font-size: 14px; flex-shrink: 0; }
.goal-input { resize: none; flex: 1; font-size: 13px; min-height: 120px; }
.run-btn { width: 100%; justify-content: center; font-size: 14px; padding: 12px; }
.clear-btn { width: 100%; justify-content: center; margin-top: 6px; font-size: 12px; }

/* ── 右侧主区 ── */
.agent-main { flex: 1; display: flex; flex-direction: column; overflow: hidden; background: var(--bg-0); }
.empty-state { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; padding: 40px; }
.empty-icon { font-size: 56px; filter: drop-shadow(0 0 20px rgba(108, 99, 255, 0.4)); animation: pulse 3s ease infinite; }
.empty-title { font-size: 24px; font-weight: 700; background: linear-gradient(135deg, var(--text-1), var(--accent-light)); -webkit-background-clip: text; background-clip: text; -webkit-text-fill-color: transparent; }
.empty-desc { color: var(--text-3); font-size: 14px; text-align: center; line-height: 1.8; }
.capability-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 12px; margin-top: 24px; max-width: 600px; width: 100%; }
.cap-card { background: var(--bg-1); border: 1px solid var(--border-1); border-radius: var(--radius-md); padding: 16px; display: flex; flex-direction: column; gap: 6px; transition: all 0.2s; }
.cap-card:hover { border-color: var(--accent-glow); background: var(--bg-2); }
.cap-icon { font-size: 24px; }
.cap-title { font-size: 13px; font-weight: 600; color: var(--text-1); }
.cap-desc { font-size: 11px; color: var(--text-3); line-height: 1.5; }

/* 规划中 */
.planning-state { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 20px; }
.planning-spinner { position: relative; width: 64px; height: 64px; display: flex; align-items: center; justify-content: center; }
.spinner-ring { position: absolute; inset: 0; border-radius: 50%; border: 3px solid transparent; border-top-color: var(--accent); border-right-color: var(--accent-light); animation: spin 0.8s linear infinite; }
.spinner-icon { font-size: 28px; }
.planning-text { color: var(--text-2); font-size: 14px; }

/* 执行面板 */
.execution-panel { flex: 1; overflow-y: auto; padding: 24px; display: flex; flex-direction: column; gap: 16px; }
.progress-header { display: flex; align-items: center; justify-content: space-between; padding-bottom: 20px; border-bottom: 1px solid var(--border-1); margin-bottom: 20px; }
.header-left { display: flex; align-items: center; gap: 16px; }
.progress-goal { display: flex; align-items: flex-start; gap: 10px; flex: 1; }
.goal-badge { font-size: 10px; font-weight: 700; letter-spacing: 0.06em; padding: 3px 8px; background: rgba(108, 99, 255, 0.15); color: var(--accent-light); border-radius: 6px; border: 1px solid rgba(108, 99, 255, 0.25); flex-shrink: 0; margin-top: 2px; }
.goal-text { font-size: 14px; font-weight: 500; color: var(--text-1); line-height: 1.5; }
.progress-stats { display: flex; align-items: center; gap: 8px; flex-shrink: 0; }
.stat { font-size: 12px; font-weight: 600; padding: 4px 10px; border-radius: 99px; }
.stat.done { background: var(--green-dim); color: var(--green); }
.stat.fail { background: rgba(255, 71, 87, 0.1); color: var(--red-light); }
.stat.total { color: var(--text-3); background: var(--surface-1); }
.progress-bar-wrap { height: 3px; background: var(--bg-3); border-radius: 99px; overflow: hidden; }
.progress-bar-fill { height: 100%; border-radius: 99px; transition: width 0.4s ease, background 0.3s ease; box-shadow: 0 0 8px var(--accent-glow); }

/* 步骤列表 */
.steps-list { display: flex; flex-direction: column; gap: 0; }
.step-card { display: flex; gap: 16px; cursor: pointer; }
.step-icon-wrap { display: flex; flex-direction: column; align-items: center; gap: 0; flex-shrink: 0; }
.step-icon { width: 36px; height: 36px; border-radius: 50%; display: flex; align-items: center; justify-content: center; font-size: 13px; font-weight: 700; flex-shrink: 0; border: 2px solid; transition: all 0.3s ease; }
.step-icon.pending { border-color: var(--border-2); color: var(--text-4); background: var(--bg-2); }
.step-icon.running { border-color: var(--accent); color: var(--accent-light); background: rgba(108, 99, 255, 0.1); box-shadow: 0 0 16px var(--accent-glow); }
.step-icon.done { border-color: var(--green); color: var(--green); background: var(--green-dim); }
.step-icon.error { border-color: var(--red); color: var(--red-light); background: rgba(255, 71, 87, 0.1); }
.step-connector { width: 2px; flex: 1; min-height: 16px; background: var(--border-1); margin: 4px 0; transition: background 0.3s; }
.step-connector.filled { background: var(--green); }
.step-content { flex: 1; padding-bottom: 12px; min-width: 0; }
.step-header { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 8px 0; }
.step-desc { font-size: 13px; font-weight: 500; color: var(--text-1); flex: 1; }
.step-card.pending .step-desc { color: var(--text-3); }
.step-meta { display: flex; align-items: center; gap: 8px; flex-shrink: 0; }
.step-tool-badge { font-size: 10px; font-weight: 600; padding: 2px 8px; border-radius: 99px; letter-spacing: 0.04em; }
.step-tool-badge.shell { background: rgba(0, 212, 170, 0.1); color: var(--teal); border: 1px solid rgba(0, 212, 170, 0.2); }
.step-tool-badge.osascript { background: rgba(255, 164, 61, 0.1); color: var(--orange); border: 1px solid rgba(255, 164, 61, 0.2); }
.step-expand-icon { font-size: 10px; color: var(--text-4); }

/* 展开详情 */
.step-details { background: var(--bg-1); border: 1px solid var(--border-1); border-radius: var(--radius-sm); padding: 14px; margin-bottom: 8px; display: flex; flex-direction: column; gap: 12px; }
.detail-block { display: flex; flex-direction: column; gap: 6px; }
.detail-label { font-size: 10px; font-weight: 700; letter-spacing: 0.08em; text-transform: uppercase; color: var(--text-4); }
.detail-thought { font-size: 13px; line-height: 1.6; color: var(--text-2); background: rgba(108, 99, 255, 0.05); border-left: 3px solid var(--accent); padding: 10px 14px; border-radius: 4px; margin-bottom: 4px; }
.text-accent-light { color: var(--accent-light); }
.detail-thinking { font-family: "JetBrains Mono", "Fira Code", monospace; font-size: 11px; line-height: 1.6; color: var(--text-3); background: rgba(0, 212, 170, 0.04); border: 1px solid rgba(0, 212, 170, 0.12); border-left: 3px solid var(--teal, #00d4aa); padding: 10px 14px; border-radius: 4px; white-space: pre-wrap; word-break: break-word; max-height: 250px; overflow-y: auto; font-style: italic; }
.detail-cmd { font-family: "JetBrains Mono", "Fira Code", monospace; font-size: 12px; color: var(--teal); background: var(--bg-0); border: 1px solid var(--border-2); border-radius: 6px; padding: 10px 14px; white-space: pre-wrap; word-break: break-all; line-height: 1.6; }
.detail-output { font-family: "JetBrains Mono", "Fira Code", monospace; font-size: 12px; color: var(--text-2); background: var(--bg-0); border: 1px solid var(--border-2); border-radius: 6px; padding: 10px 14px; white-space: pre-wrap; word-break: break-all; line-height: 1.6; max-height: 200px; overflow-y: auto; }
.detail-output.error { color: var(--red-light); border-color: rgba(255, 71, 87, 0.2); }
.detail-running { display: flex; align-items: center; gap: 10px; color: var(--accent-light); font-size: 12px; }

/* 完成 Banner */
.completion-banner { display: flex; align-items: center; gap: 12px; padding: 16px 20px; border-radius: var(--radius-md); font-size: 14px; font-weight: 500; }
.completion-banner.success { background: var(--green-dim); color: var(--green); border: 1px solid rgba(46, 213, 115, 0.2); }
.completion-banner.error { background: rgba(255, 71, 87, 0.08); color: var(--red-light); border: 1px solid rgba(255, 71, 87, 0.2); }
.completion-icon { font-size: 20px; }

/* 日志面板 */
.log-panel { border-top: 1px solid var(--border-1); background: var(--bg-1); display: flex; flex-direction: column; max-height: 200px; min-height: 100px; flex-shrink: 0; }
.log-header { display: flex; align-items: center; justify-content: space-between; padding: 8px 16px; border-bottom: 1px solid var(--border-1); font-size: 11px; font-weight: 600; letter-spacing: 0.05em; color: var(--text-4); text-transform: uppercase; }
.log-list { flex: 1; overflow-y: auto; padding: 8px 16px; display: flex; flex-direction: column; gap: 3px; }
.log-line { display: flex; gap: 12px; font-size: 11px; font-family: "JetBrains Mono", "Fira Code", monospace; line-height: 1.6; }
.log-time { font-size: 11px; color: var(--text-4); min-width: 55px; font-family: "JetBrains Mono", "Fira Code", monospace; }
.log-msg { color: var(--text-2); }
.log-line.success .log-msg { color: var(--green); }
.log-line.error .log-msg { color: var(--red-light); }
.log-line.warn .log-msg { color: var(--orange); }
.log-line.info .log-msg { color: var(--text-2); }

/* 过渡动画 */
.expand-enter-active, .expand-leave-active { transition: all 0.25s ease; overflow: hidden; }
.expand-enter-from, .expand-leave-to { opacity: 0; max-height: 0; padding-top: 0; margin-bottom: 0; }
.expand-enter-to, .expand-leave-from { opacity: 1; max-height: 600px; }
.btn-xs { padding: 3px 8px; font-size: 11px; border-radius: 4px; background: var(--surface-1); border: 1px solid var(--border-1); color: var(--text-3); cursor: pointer; }
.btn-xs:hover { background: var(--surface-2); color: var(--text-1); }

@keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
</style>
