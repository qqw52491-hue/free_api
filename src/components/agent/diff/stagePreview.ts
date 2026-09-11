import type { AgentStepItem, RunningPreviewLine1, RunningPreviewLine2, StageActionCapsule, StageActionCapsuleKind, StageRunningPreview } from "../types";
import { cleanFilePath, isWaitJobsTool } from "./diffTypes";
import { isInterceptedStep } from "./diffCore";
import { formatStepOperation } from "./toolOperation";
import { parseStepFileChanges } from "./stepFileParser";

/**
 * 格式化阶段标题：统一规范化为 "阶段 N · 标题内容"
 */
export function formatStageTitle(rawTitle: string, stageIndex: number): string {
    if (!rawTitle) return `阶段 ${stageIndex}`;
    // 去除已有的 "阶段 1 · "、"阶段1: "、"步骤 1 · " 等多余前缀，避免双重前缀
    const cleaned = rawTitle
        .replace(/^阶段\s*([0-9一二三四五六七八九十]+|[a-zA-Z]+)\s*[·:：\-\s]\s*/, "")
        .replace(/^步骤\s*([0-9一二三四五六七八九十]+|[a-zA-Z]+)\s*[·:：\-\s]\s*/, "")
        .trim();
    // 防御长文本/多行文本污染：只取首行，折叠多余空白，限制最大长度 40
    const firstLine = (cleaned || rawTitle.trim()).split("\n")[0].trim().replace(/\s+/g, " ");
    const truncated = firstLine.length > 40 ? firstLine.slice(0, 38) + "…" : firstLine;
    const finalTitle = truncated || "执行任务";
    return `阶段 ${stageIndex} · ${finalTitle}`;
}

/**
 * 格式化阶段耗时：14s, 48s, 1m32s, 2m10s
 */
export function formatStageDuration(ms?: number): string {
    if (!ms || ms <= 0) return "";
    const totalSec = Math.round(ms / 1000);
    if (totalSec < 60) return `${Math.max(1, totalSec)}s`;
    const min = Math.floor(totalSec / 60);
    const sec = totalSec % 60;
    return sec > 0 ? `${min}m${sec}s` : `${min}m`;
}

/**
 * 格式化 Token 计数：728 tok, 2.1k tok, 5.4k tok
 */
export function formatTokenCount(n?: number): string {
    if (!n || n <= 0) return "0";
    if (n >= 1000000) return (n / 1000000).toFixed(1) + "M";
    if (n >= 1000) return (n / 1000).toFixed(1) + "k";
    return String(n);
}

export function extractActiveThoughtSentence(raw: string, maxLen = 350): string {
    const fallback = "正在深入分析并规划下一步动作 …";
    if (!raw) return fallback;

    // 去除代码块、XML/HTML 标签、markdown 强调字符等，将换行与连续空白压缩为单空格
    let stripped = raw
        .replace(/```[\s\S]*?```/g, " ")
        .replace(/<[^>]+>/g, " ")
        .replace(/[#*`_>~]/g, "")
        .replace(/\r?\n+/g, " ")
        .replace(/\s+/g, " ")
        .trim();

    if (!stripped) return fallback;

    // 动态 Tail Window：保留足够长（350 字符）的活跃思考流，确保在任何宽屏下都能撑满单行整行并贴紧右侧
    if (stripped.length <= maxLen) {
        return stripped;
    }

    return stripped.slice(-maxLen);
}

/**
 * 将已完成的单步操作格式化为紧凑单行摘要，附带专属图标与耗时（用于前两步记录）
 */
export function formatCompletedStepDigest(step: AgentStepItem): {
    icon: string;
    text: string;
    duration?: string;
    fullThought?: string;
} {
    const duration = step.durationMs ? formatStageDuration(step.durationMs) : undefined;
    const tool = (step.tool || step.action || "").toLowerCase();

    // 0. 门禁拦截
    if (isInterceptedStep(step)) {
        return {
            icon: "🛡️",
            text: "操作已被前置门禁拦截（需审查审批）",
            duration,
            fullThought: step.thought,
        };
    }

    // 1. 文件写/编辑操作
    const op = formatStepOperation(step);
    if (op.fileName) {
        if (
            op.badgeLabel === "修改" ||
            op.badgeLabel === "写入" ||
            tool.includes("write") ||
            tool.includes("edit") ||
            tool.includes("replace") ||
            tool.includes("child_writer")
        ) {
            let diffStat = "";
            const adds = op.diffStat?.additions || 0;
            const dels = op.diffStat?.deletions || 0;
            if (adds > 0 || dels > 0) {
                diffStat = ` (+${adds} -${dels})`;
            }
            return {
                icon: "✏️",
                text: `${op.badgeLabel === "修改" ? "修改了" : "写入了"} ${op.fileName}${diffStat}`,
                duration,
                fullThought: step.thought,
            };
        }
        if (op.badgeLabel === "读取" || tool.includes("read") || tool.includes("cat") || tool.includes("view")) {
            return {
                icon: "📖",
                text: `读取了 ${op.fileName}${op.lineRange ? ` (${op.lineRange})` : ""}`,
                duration,
                fullThought: step.thought,
            };
        }
    }

    // 1.8 全网搜索与网页提取 (前置判定，避免被下方 search 吞掉)
    if (tool === "web_search" || tool.includes("web_search")) {
        const p = (step.params || step.rawParams) as any;
        let query = "";
        if (p && typeof p === "object") {
            query = String(p.query || p.q || p.Query || "");
        }
        return {
            icon: "🌐",
            text: query ? `全网检索了: ${query.slice(0, 35)}` : "全网检索最新事实",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool === "web_fetch" || tool.includes("web_fetch")) {
        const p = (step.params || step.rawParams) as any;
        const url = p && typeof p === "object" ? String(p.url || p.Url || "") : "";
        return {
            icon: "🌍",
            text: url ? `抓取了网页: ${url.slice(0, 35)}` : "抓取了网页正文",
            duration,
            fullThought: step.thought,
        };
    }

    // 2. 搜索操作 (代码 / 日志)
    if (tool.includes("search") || tool.includes("grep")) {
        const p = (step.params || step.rawParams) as any;
        let query = "";
        let kinds = "";
        if (p && typeof p === "object") {
            query = String(p.Query || p.query || p.q || "");
            kinds = String(p.kinds || "").toLowerCase();
        }
        let text = query ? `检索了: ${query.slice(0, 35)}` : "检索了代码库上下文";
        if (kinds === "history") {
            text = query ? `检索了执行日志: ${query.slice(0, 35)}` : "检索了历史执行日志";
        }
        return {
            icon: "🔍",
            text,
            duration,
            fullThought: step.thought,
        };
    }

    // 3. 审查员 / 子智能体
    if (tool.includes("child_reviewer") || tool.includes("reviewer") || tool.includes("review")) {
        let verdictLabel = "审查完成";
        if (step.result) {
            const rawRes = typeof step.result === "string" ? step.result : JSON.stringify(step.result);
            if (rawRes.includes('"verdict":"passed"') || rawRes.includes("通过") || rawRes.includes("approved")) {
                verdictLabel = "方案审查通过";
            } else if (rawRes.includes("rejected") || rawRes.includes("打回") || rawRes.includes("拦截")) {
                verdictLabel = "审查打回并修正";
            }
        }
        const label = step.description && step.description !== "执行操作"
            ? step.description
            : verdictLabel;
        return {
            icon: "🤖",
            text: label,
            duration,
            fullThought: step.thought,
        };
    }

    // 3.1 追问对质子任务
    if (tool.includes("resume_child")) {
        const p = (step.params || step.rawParams) as any;
        const msg = p && typeof p === "object" ? String(p.message || "") : "";
        return {
            icon: "🔄",
            text: msg ? `追问了子任务: ${msg.slice(0, 35)}` : "追问对质了子任务",
            duration,
            fullThought: step.thought,
        };
    }

    // 3.2 协同子智能体 / worker
    if (tool.includes("child_worker") || tool.includes("child_writer") || tool.includes("subagent") || tool.includes("sub_agent")) {
        const p = (step.params || step.rawParams) as any;
        const task = p && typeof p === "object" ? String(p.task || "") : "";
        return {
            icon: "🤖",
            text: task ? `派发了子任务: ${task.slice(0, 35)}` : (step.description && step.description !== "执行操作" ? step.description : "完成了协同子任务"),
            duration,
            fullThought: step.thought,
        };
    }

    // 4. 终端执行 (包含 local_shell)
    if (tool.includes("shell") || tool.includes("exec") || tool.includes("command") || tool.includes("bash")) {
        let cmd = "";
        const p = (step.params || step.rawParams) as any;
        if (p && typeof p === "object") {
            cmd = String(p.command || p.cmd || p.CommandLine || "");
            if (Array.isArray(p.args)) {
                cmd += " " + p.args.join(" ");
            }
        }
        let desc = step.description && step.description !== "执行操作" ? step.description : "";
        if (!desc && cmd) {
            desc = cmd.slice(0, 40);
        }
        return {
            icon: "⚡",
            text: desc ? `执行了: ${desc}` : "执行了终端命令",
            duration,
            fullThought: step.thought,
        };
    }

    // 5. 目录勘察 (包含 local_file list)
    if (op.badgeLabel === "目录" || tool.includes("directory") || tool.includes("tree") || tool.includes("list_dir") || tool.includes("list_files")) {
        const p = (step.params || step.rawParams) as any;
        const dir = p && typeof p === "object" ? String(p.dir || p.DirectoryPath || p.path || ".") : ".";
        return {
            icon: "📁",
            text: `查看了目录: ${dir}`,
            duration,
            fullThought: step.thought,
        };
    }

    // 5.1 人工交互问询
    if (tool.includes("ask_human") || tool.includes("human")) {
        const p = (step.params || step.rawParams) as any;
        const q = p && typeof p === "object" ? String(p.message || p.question || "") : "";
        return {
            icon: "👤",
            text: q ? `请求人工确认: ${q.slice(0, 35)}` : "呈报了人工问询/方案审批",
            duration,
            fullThought: step.thought,
        };
    }

    // 5.2 终态交付收尾
    if (tool === "finish" || tool.includes("finish")) {
        const p = (step.params || step.rawParams) as any;
        const s = p && typeof p === "object" ? String(p.summary || p.final_summary || "") : "";
        return {
            icon: "🎉",
            text: s ? `达成终态交付: ${s.slice(0, 35)}` : "达成全流程终态交付",
            duration,
            fullThought: step.thought,
        };
    }

    // 5.3 异步挂起等待
    if (tool.includes("wait_jobs") || tool.includes("wait_job")) {
        return {
            icon: "⏳",
            text: "等待后台协同任务完成并回收结论",
            duration,
            fullThought: step.thought,
        };
    }

    // 5.4 产物管理
    if (tool.includes("artifact")) {
        const p = (step.params || step.rawParams) as any;
        const title = p && typeof p === "object" ? String(p.title || p.path || "") : "";
        return {
            icon: "📦",
            text: title ? `生成了交付物: ${title.slice(0, 35)}` : (step.description && step.description !== "执行操作" ? step.description : "生成了交付物文档"),
            duration,
            fullThought: step.thought,
        };
    }

    // 5.5 数据暂存
    if (tool.includes("task_store")) {
        const p = (step.params || step.rawParams) as any;
        const label = p && typeof p === "object" ? String(p.label || p.resource_id || p.key || p.action || "") : "";
        return {
            icon: "💾",
            text: label ? `存取任务数据: ${label.slice(0, 30)}` : "存取了任务结构化数据",
            duration,
            fullThought: step.thought,
        };
    }

    // 5.6 截断输出提取
    if (tool.includes("get_step_output")) {
        return {
            icon: "📜",
            text: "提取了历史执行输出全文",
            duration,
            fullThought: step.thought,
        };
    }

    // 5.7 系统与模型管理
    if (tool.includes("model_manager")) {
        return {
            icon: "⚙️",
            text: "配置了模型平台与连接参数",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("skill_manager")) {
        return {
            icon: "🧠",
            text: "维护了领域专家技能手册",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("mcp_manager")) {
        return {
            icon: "🔌",
            text: "维护了外部 MCP 插件连接",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("role_manager")) {
        return {
            icon: "🎭",
            text: "维护了智能体角色专家卡体系",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("schedule_manager")) {
        return {
            icon: "⏰",
            text: "配置了后台定时调度计划",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("project_manager")) {
        const p = (step.params || step.rawParams) as any;
        const name = p && typeof p === "object" ? String(p.name || "") : "";
        return {
            icon: "🏗️",
            text: name ? `配置了项目工作区: ${name}` : "管理了项目工作区",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("project_resources") || tool.includes("register_project_resource")) {
        const p = (step.params || step.rawParams) as any;
        const path = p && typeof p === "object" ? String(p.path || p.resource_id || "") : "";
        return {
            icon: "📚",
            text: path ? `维护了受管资源: ${path.split("/").pop() || path}` : "管理了项目受管资源",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("watch")) {
        return {
            icon: "👁️",
            text: "设置了工作区文件持续监听",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("system_info")) {
        return {
            icon: "💻",
            text: "探测了宿主系统硬件与环境资源",
            duration,
            fullThought: step.thought,
        };
    }
    if (tool.includes("wechat")) {
        const p = (step.params || step.rawParams) as any;
        const text = p && typeof p === "object" ? String(p.text || p.message || "") : "";
        return {
            icon: "💬",
            text: text ? `发送了微信通知: "${text.slice(0, 30)}"` : "维护了微信服务通道",
            duration,
            fullThought: step.thought,
        };
    }

    // 6. 有明确 description
    if (step.description && step.description !== "执行操作") {
        return {
            icon: "⚡",
            text: step.description,
            duration,
            fullThought: step.thought,
        };
    }

    // 7. 兜底若有思考
    if (step.thought) {
        return {
            icon: "🧠",
            text: extractActiveThoughtSentence(step.thought),
            duration,
            fullThought: step.thought,
        };
    }

    return {
        icon: "⚡",
        text: "完成了前置操作",
        duration,
        fullThought: step.thought,
    };
}

/**
 * 提取正在执行阶段的三行流光预览 (收拢态直接呈现)
 * Line 1: 前第 2 步已完成记录（清晰掌握前置操作路径）
 * Line 2: 前第 1 步最近完成记录 / 思考结论（紧跟最新进展）
 * Line 3: 正在执行的操作或活跃思考流（带 text-shimmer-flow 流光与快速跳动，极速响应）
 */
export function getRunningStagePreview(
    steps: AgentStepItem[],
    isThinking?: boolean,
    currentThought?: string,
    stageTitle?: string
): StageRunningPreview {
    const succeededSteps = steps.filter((s) => s.status === "succeeded");

    let line1: RunningPreviewLine1;
    let line2: RunningPreviewLine2;

    if (succeededSteps.length >= 2) {
        // 前第 2 步
        const stepA = succeededSteps[succeededSteps.length - 2];
        const digestA = formatCompletedStepDigest(stepA);
        line1 = {
            icon: digestA.icon,
            text: digestA.text,
            duration: digestA.duration,
        };

        // 前第 1 步 (最近完成)
        const stepB = succeededSteps[succeededSteps.length - 1];
        const digestB = formatCompletedStepDigest(stepB);
        line2 = {
            icon: digestB.icon,
            text: digestB.text,
            duration: digestB.duration,
            fullThought: digestB.fullThought,
        };
    } else if (succeededSteps.length === 1) {
        // 只有 1 步完成：Line 1 为环境准备，Line 2 为刚完成的第 1 步
        line1 = {
            icon: "📋",
            text: "准备阶段上下文与工作区环境",
            duration: "0.2s",
        };
        const stepB = succeededSteps[0];
        const digestB = formatCompletedStepDigest(stepB);
        line2 = {
            icon: digestB.icon,
            text: digestB.text,
            duration: digestB.duration,
            fullThought: digestB.fullThought,
        };
    } else {
        // 尚未完成任何步骤：Line 1 为环境准备，Line 2 为任务阶段目标（锚点明确，绝不与 Line 3 的思考内容重复）
        line1 = {
            icon: "📋",
            text: "准备阶段上下文与工作区环境",
            duration: "0.2s",
        };
        const cleanStage = stageTitle
            ? stageTitle.replace(/^阶段\s*\d+\s*[·:：-]\s*/, "").trim()
            : "";
        const targetDesc = cleanStage
            ? (cleanStage.startsWith("目标") || cleanStage.startsWith("规划") ? cleanStage : `目标：${cleanStage}`)
            : "规划：按阶段目标制定实施方案并有序推进";
        line2 = {
            icon: "🎯",
            text: targetDesc,
            fullThought: currentThought,
        };
    }

    // Line 3: 正在执行的当前动作 (刷刷刷的高速流光动画与动态思考)
    const hasLiveStep = steps.some((s) => s.status === "running" || s.status === "pending");
    const runningStep = steps.find((s) => s.status === "running" || s.status === "pending") || steps[steps.length - 1];

    let line3Icon = "⚡";
    let line3Text = "正在执行操作 …";
    let line3Kind: StageRunningPreview["line3"]["actionKind"] = "other";

    const activeThought = currentThought || (runningStep?.thought && (isThinking || runningStep.status === "running") ? runningStep.thought : "");

    if (isThinking || (activeThought && (!hasLiveStep || (runningStep?.status === "running" && !runningStep.tool)))) {
        line3Icon = "💭";
        line3Text = extractActiveThoughtSentence(activeThought);
        line3Kind = "think";
    } else if (runningStep && (runningStep.status === "running" || runningStep.status === "pending")) {
        const tool = (runningStep.tool || runningStep.action || "").toLowerCase();
        let targetFile = "";
        const p = (runningStep.params || runningStep.rawParams) as any;
        if (p && typeof p === "object") {
            targetFile = cleanFilePath(p.TargetFile || p.target_file || p.path || p.file || "");
        }
        if (!targetFile && runningStep.description) {
            const m = runningStep.description.match(/[\/a-zA-Z0-9_\-.]+\.[a-zA-Z0-9_]+/);
            if (m) targetFile = cleanFilePath(m[0]);
        }
        const fileName = targetFile.split("/").pop() || targetFile;
        const liveOp = formatStepOperation(runningStep);

        if (
            liveOp.badgeLabel === "修改" ||
            liveOp.badgeLabel === "写入" ||
            tool.includes("write") ||
            tool.includes("edit") ||
            tool.includes("replace") ||
            tool.includes("child_writer")
        ) {
            line3Icon = "✏️";
            line3Text = fileName ? `正在写入 ${fileName} …` : "正在写入代码与文档 …";
            line3Kind = "write";
        } else if (liveOp.badgeLabel === "读取" || tool.includes("read") || tool.includes("cat") || tool.includes("view")) {
            line3Icon = "📖";
            line3Text = fileName ? `正在读取 ${fileName} …` : "正在读取上下文文件 …";
            line3Kind = "read";
        } else if (tool.includes("web_search")) {
            const query = p && typeof p === "object" ? String(p.query || p.q || p.Query || "") : "";
            line3Icon = "🌐";
            line3Text = query ? `正在全网检索: "${query.slice(0, 30)}" …` : "正在全网检索相关资料 …";
            line3Kind = "search";
        } else if (tool.includes("web_fetch")) {
            const url = p && typeof p === "object" ? String(p.url || p.Url || "") : "";
            line3Icon = "🌍";
            line3Text = url ? `正在抓取网页正文: ${url.slice(0, 30)} …` : "正在提取网页正文 Markdown …";
            line3Kind = "read";
        } else if (liveOp.badgeLabel === "目录" || tool.includes("directory") || tool.includes("tree") || tool.includes("list_dir") || tool.includes("list_files")) {
            const dir = p && typeof p === "object" ? String(p.dir || p.DirectoryPath || p.path || ".") : ".";
            line3Icon = "📁";
            line3Text = `正在勘察目录结构: ${dir} …`;
            line3Kind = "read";
        } else if (tool.includes("ask_human") || tool.includes("human")) {
            line3Icon = "👤";
            line3Text = "正在等待人工输入与方案裁决 …";
            line3Kind = "other";
        } else if (tool === "finish" || tool.includes("finish")) {
            line3Icon = "🎉";
            line3Text = "正在完成任务终态交付与产物归档 …";
            line3Kind = "other";
        } else if (tool.includes("resume_child")) {
            line3Icon = "🔄";
            line3Text = "正在追问并对质子任务 …";
            line3Kind = "other";
        } else if (tool.includes("wait_jobs") || tool.includes("wait_job")) {
            line3Icon = "⏳";
            line3Text = "正在零 Token 反应式挂起等待后台任务 …";
            line3Kind = "other";
        } else if (tool.includes("artifact")) {
            line3Icon = "📦";
            line3Text = "正在生成并登记交付产物文档 …";
            line3Kind = "write";
        } else if (tool.includes("task_store")) {
            line3Icon = "💾";
            line3Text = "正在存取任务数据账本 …";
            line3Kind = "other";
        } else if (tool.includes("get_step_output")) {
            line3Icon = "📜";
            line3Text = "正在定向提取历史截断输出原文 …";
            line3Kind = "read";
        } else if (tool.includes("model_manager")) {
            line3Icon = "⚙️";
            line3Text = "正在配置模型提供商与连接渠道 …";
            line3Kind = "other";
        } else if (tool.includes("skill_manager")) {
            line3Icon = "🧠";
            line3Text = "正在装配领域专家技能手册 …";
            line3Kind = "other";
        } else if (tool.includes("mcp_manager")) {
            line3Icon = "🔌";
            line3Text = "正在连接/更新外部 MCP 插件服务 …";
            line3Kind = "other";
        } else if (tool.includes("role_manager")) {
            line3Icon = "🎭";
            line3Text = "正在配置智能体专家角色卡 …";
            line3Kind = "other";
        } else if (tool.includes("schedule_manager")) {
            line3Icon = "⏰";
            line3Text = "正在配置后台定时调度引擎 …";
            line3Kind = "other";
        } else if (tool.includes("project_manager")) {
            line3Icon = "🏗️";
            line3Text = "正在配置项目工作区环境 …";
            line3Kind = "other";
        } else if (tool.includes("project_resources") || tool.includes("register_project_resource")) {
            line3Icon = "📚";
            line3Text = "正在同步项目受管资源 …";
            line3Kind = "other";
        } else if (tool.includes("watch")) {
            line3Icon = "👁️";
            line3Text = "正在配置后台持续监听守候进程 …";
            line3Kind = "other";
        } else if (tool.includes("system_info")) {
            line3Icon = "💻";
            line3Text = "正在探测宿主硬件与系统环境资源 …";
            line3Kind = "other";
        } else if (tool.includes("wechat")) {
            line3Icon = "💬";
            line3Text = "正在调用微信服务通道 …";
            line3Kind = "other";
        } else if (tool.includes("search") || tool.includes("grep")) {
            line3Icon = "🔍";
            line3Text = "正在检索相关代码与文档 …";
            line3Kind = "search";
        } else if (tool.includes("child_reviewer") || tool.includes("review")) {
            line3Icon = "🤖";
            line3Text = "子智能体正在审查方案与代码变动 …";
            line3Kind = "review";
        } else if (tool.includes("command") || tool.includes("exec") || tool.includes("shell") || tool.includes("bash")) {
            line3Icon = "⚡";
            line3Text = runningStep.description && runningStep.description !== "执行操作"
                ? `正在执行: ${runningStep.description} …`
                : "正在执行终端命令 …";
            line3Kind = "command";
        } else if (runningStep.description && runningStep.description !== "执行操作") {
            line3Icon = "⚡";
            line3Text = `正在执行: ${runningStep.description} …`;
            line3Kind = "other";
        } else if (runningStep.thought) {
            line3Icon = "💭";
            line3Text = extractActiveThoughtSentence(runningStep.thought);
            line3Kind = "think";
        }
    } else if (isThinking) {
        line3Icon = "💭";
        line3Text = "正在深入分析需求并规划步骤 …";
        line3Kind = "think";
    } else {
        line3Icon = "⚡";
        line3Text = "正在协同推进阶段任务 …";
        line3Kind = "other";
    }

    return {
        line1,
        line2,
        line3: {
            icon: line3Icon,
            text: line3Text,
            actionKind: line3Kind,
        },
    };
}

/**
 * 按执行时间严格正序提取阶段内发生过的所有工具动作胶囊
 * 包含：Excel 表格生成、代码修改/新增(附 Diff)、文档研报、Reviewer 审查结论、方案审查挂起等待、终端执行等
 */
export function extractStageChronologicalCapsules(steps: AgentStepItem[]): StageActionCapsule[] {
    const capsules: StageActionCapsule[] = [];
    const readCapsules: StageActionCapsule[] = [];
    const seenActions = new Set<string>();
    let commandCapsuleCount = 0;

    for (const step of steps) {
        const parsed = parseStepFileChanges(step);

        // 1. 文件改动与产物 (代码修改、Excel 表格、文档研报)
        if (parsed.chips && parsed.chips.length > 0) {
            for (const chip of parsed.chips) {
                const rawName = chip.fileName || chip.path;
                const ext = rawName.split(".").pop()?.toLowerCase() || "";
                const key = (chip.path || chip.fileName).toLowerCase();

                let kind: StageActionCapsuleKind = "code";
                let icon = chip.actionKind === "create" ? "✨" : (chip.actionKind === "delete" ? "🗑️" : "✏️");

                if (["xlsx", "xls", "csv", "ods"].includes(ext) || rawName.toLowerCase().includes("excel")) {
                    kind = "excel";
                    icon = "📊";
                } else if (["md", "markdown", "txt", "pdf", "docx", "doc"].includes(ext)) {
                    kind = "doc";
                    icon = "📝";
                }

                const existing = capsules.find((c) => c.kind === kind && c.id === key);
                if (existing) {
                    existing.additions = (existing.additions || 0) + (chip.additions || 0);
                    existing.deletions = (existing.deletions || 0) + (chip.deletions || 0);
                    if (chip.isIntercepted) existing.isIntercepted = true;
                    if (chip.rawDiff) existing.chip = chip;
                } else {
                    capsules.push({
                        id: key,
                        kind,
                        icon,
                        label: chip.displayLabel || chip.fileName,
                        additions: chip.additions,
                        deletions: chip.deletions,
                        isIntercepted: chip.isIntercepted,
                        stepId: step.id,
                        chip,
                    });
                }
            }
        }

        // 2. 收集读取文件 (带扩展名与文件图标，用于精准识别读取上下文)
        if (parsed.readFiles && parsed.readFiles.length > 0) {
            for (const rf of parsed.readFiles) {
                const fileName = rf.split("/").pop() || rf;
                const key = rf.toLowerCase();
                const existing = readCapsules.find((c) => c.id === key);
                if (!existing) {
                    readCapsules.push({
                        id: key,
                        kind: "read",
                        icon: "📖",
                        label: fileName,
                        stepId: step.id,
                    });
                }
            }
        }

        // 3. 审查员结论胶囊 (Reviewer Verdict)
        if (parsed.reviewerVerdict) {
            const verdict = parsed.reviewerVerdict;
            const isPass = verdict.verdict === "pass";
            const verdictKey = `verdict-${step.id}`;
            if (!seenActions.has(verdictKey)) {
                seenActions.add(verdictKey);
                capsules.push({
                    id: verdictKey,
                    kind: "reviewer",
                    icon: isPass ? "✅" : "⚠️",
                    label: isPass ? (verdict.score ? `审查通过 (${verdict.score}分)` : "方案审查通过") : "审查发现缺陷（需修正）",
                    stepId: step.id,
                    score: verdict.score,
                    verdict: verdict.verdict,
                });
            }
        }

        // 4. 检查未形成标准 diff 的重要产物 (Excel/研报/挂起等待)
        const tool = (step.tool || step.action || "").toLowerCase();
        const desc = (step.description || "").trim();

        const hasExcelMention = /excel|\.xlsx|数据表格/i.test(desc) || /excel|\.xlsx/i.test(tool);
        const hasDocMention = /研报|分析报告|全景分析|\.md/i.test(desc);
        const isWait = isWaitJobsTool(tool) || /等待.*(审查|结论|任务|审批|jobs)/i.test(desc) || desc.includes("等待方案审查");

        if (hasExcelMention && !capsules.some((c) => c.kind === "excel")) {
            let excelName = "Excel 数据表格";
            const m = desc.match(/[\/a-zA-Z0-9_\-\u4e00-\u9fa5]+\.(xlsx|xls|csv)/i);
            if (m) excelName = m[0];
            capsules.push({
                id: `excel-action-${step.id}`,
                kind: "excel",
                icon: "📊",
                label: excelName,
                stepId: step.id,
            });
        }

        if (hasDocMention && !capsules.some((c) => c.kind === "doc")) {
            let docName = "深度分析研报";
            const m = desc.match(/[\/a-zA-Z0-9_\-\u4e00-\u9fa5]+\.(md|txt|pdf)/i);
            if (m) docName = m[0];
            capsules.push({
                id: `doc-action-${step.id}`,
                kind: "doc",
                icon: "📝",
                label: docName,
                stepId: step.id,
            });
        }

        if (isWait && !seenActions.has(`wait-${step.id}`)) {
            seenActions.add(`wait-${step.id}`);
            capsules.push({
                id: `wait-${step.id}`,
                kind: "wait",
                icon: "⏳",
                label: "方案审查挂起等待",
                stepId: step.id,
            });
        }

        // 5. 关键终端执行命令胶囊（提取命令关键名与动作，最多呈现前 3 条关键命令）
        const isCommand = tool.includes("command") || tool.includes("exec") || tool.includes("shell") || tool.includes("bash");
        if (isCommand && commandCapsuleCount < 3) {
            const p = (step.params || step.rawParams) as any;
            let cmdStr = "";
            if (p && typeof p === "object") {
                cmdStr = String(p.command || p.cmd || p.CommandLine || "");
            }
            if (!cmdStr && desc && desc !== "执行操作") {
                cmdStr = desc;
            }
            if (cmdStr) {
                const cmdKey = `cmd-${step.id}`;
                if (!seenActions.has(cmdKey)) {
                    seenActions.add(cmdKey);
                    commandCapsuleCount++;
                    // 仅提取主命令与第一个关键参数，如 "cargo test", "npm run build"
                    const cmdTokens = cmdStr.trim().split(/\s+/).slice(0, 3).join(" ");
                    capsules.push({
                        id: cmdKey,
                        kind: "command",
                        icon: "⚡",
                        label: `$ ${cmdTokens}`,
                        stepId: step.id,
                    });
                }
            }
        }
    }

    // 将去重后的读取文件胶囊追加在主要产物后面
    if (readCapsules.length > 0) {
        capsules.push(...readCapsules);
    }

    return capsules;
}
