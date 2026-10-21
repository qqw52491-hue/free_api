import { isReviewTerminal, type ReviewProgressItem } from './reviewProgress';

/** 仅投影宿主枚举；不从任意错误正文、模型输出或父任务结果猜测审查裁决。 */
export function reviewDiagnosis(review: ReviewProgressItem, isRunning: boolean) {
    if (review.phase === 'passed') return { title: '终审通过', detail: '该成员已给出有效通过结论，不代表整个任务已完成交付。', tone: 'success' };
    if (review.phase === 'rejected') return { title: '终审不通过 · 存在阻塞', detail: '该成员提出阻塞问题，具体发现请查看审查回执；不是模型调用失败。', tone: 'risk' };
    if (review.phase === 'skipped') return { title: '本轮未执行模型终审', detail: '不将跳过展示为模型批准。', tone: 'neutral' };
    if (review.phase === 'cancelled') return { title: '审查已停止', detail: '宿主已收到停止信号，本次没有形成有效审查结论；不表示代码不合格。', tone: 'warning' };
    if (review.phase === 'failed') {
        const reasons: Record<string, string> = {
            timeout: '模型响应超时', request: '模型调用失败', format: '模型输出格式无效', configuration: '模型配置加载失败',
        };
        return { title: `审查未完成 · ${reasons[review.failureKind] || '原因未明确'}`, detail: '本次未获得有效审查结论，不是代码被判不通过。', tone: 'warning' };
    }
    if (review.phase === 'incomplete' || !isRunning) return { title: '审查未完成 · 结束原因未确认', detail: '运行已结束或审查调用提前结束，未收到有效终态；不推断为主动停止或代码不通过。', tone: 'warning' };
    return null;
}

/** 宿主阶段的短标签；当前动作和统计默认可见，模型与完整诊断在详情区。 */
export function reviewStatusLabel(review: ReviewProgressItem, isRunning: boolean): string {
    if (review.phase === 'passed') return '通过';
    if (review.phase === 'rejected') return '存在阻塞';
    if (review.phase === 'skipped') return '未执行';
    if (review.phase === 'cancelled') return '已停止';
    if (review.phase === 'failed') return ({ timeout: '响应超时', request: '调用失败', format: '格式无效', configuration: '配置失败' } as Record<string, string>)[review.failureKind] || '未完成';
    if (review.phase === 'incomplete' || !isRunning) return '未完成';
    if (review.phase === 'responding' && review.streamKind === 'thinking') return '正在思考';
    if (review.phase === 'responding' && review.streamKind === 'content') return '正在生成结论';
    return ({ preparing: '准备中', waiting: '等待响应', responding: '接收中', tool: '取证中', verifying: '验证中', retrying: '超时重试', format_retry: '格式补正', converging: '等待判决' } as Record<string, string>)[review.phase] || '审查中';
}

const count = (value: number | null) => value === null ? '未知' : String(value);
/** 白名单导出：故意不复制角色名、模型名、ID、路径、正文、异常原文和流片段。 */
export function buildReviewDiagnostics(reviews: ReviewProgressItem[], isRunning: boolean): string {
    return ['终审诊断（脱敏摘要 v1）', '仅含当前保留的各成员最新尝试；非完整历史，不代表任务最终交付状态。',
        ...reviews.flatMap((review, index) => {
            const diagnosis = reviewDiagnosis(review, isRunning);
            return [`\n成员 ${index + 1}：${diagnosis?.title || '审查进行中'}${!isReviewTerminal(review.phase) && isRunning ? '（观测快照）' : ''}`,
                diagnosis?.detail || '尚未形成审查结论。',
                `观测耗时：${Math.floor(review.elapsedMs / 1000)} 秒`,
                `逻辑请求：${review.round} 次；超时重发：${count(review.timeoutRetries)} 次；格式补正：${count(review.formatCorrections)} 次；已取证：${review.toolSteps}/${review.toolBudget} 步`];
        }), '说明：逻辑请求不等于底层 HTTP 尝试次数；超时重发不含传输层重试；耗时以最后事件为准。',
        '已排除：角色/模型自定义名、会话和任务标识、源码、文件路径、请求地址、密钥、提示词、原始响应及思考内容。'].join('\n');
}

/** 点击后才写剪贴板；失败返回 false，由界面提供可手动选择的摘要。 */
export async function copyReviewDiagnostics(text: string): Promise<boolean> {
    try {
        if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
            await navigator.clipboard.writeText(text);
            return true;
        }
    } catch { /* WebKit/权限限制时使用与现有界面一致的本地复制降级。 */ }
    if (typeof document === 'undefined') return false;
    const field = document.createElement('textarea');
    const previous = document.activeElement;
    field.value = text;
    field.setAttribute('readonly', '');
    field.style.cssText = 'position:fixed;left:-9999px;opacity:0';
    try {
        document.body.appendChild(field);
        field.select();
        return document.execCommand('copy');
    } catch { return false; }
    finally {
        field.remove();
        if (previous instanceof HTMLElement) previous.focus({ preventScroll: true });
    }
}
