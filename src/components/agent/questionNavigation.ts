export interface QuestionNavigationEntry {
    id: string;
    kind: 'question' | 'answer';
    text: string;
    pending?: boolean;
}

export interface QuestionMark {
    id: string;
    question: string;
    answer: string;
    hasAnswer: boolean;
    pending: boolean;
}

/** 只用于悬浮预览；不修改消息原文，不生成或推断回答。 */
export function questionPreview(text: string, limit: number): string {
    const plain = text
        .replace(/<!--[\s\S]*?-->/g, ' ')
        .replace(/📎\s*\[附件:[^\]]*\]/g, ' ')
        .replace(/```[^\n]*\n[\s\S]*?```/g, ' ')
        .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
        .replace(/^\s{0,3}(?:#{1,6}\s+|>\s*|[-*+]\s+|\d+[.)]\s+)/gm, '')
        .replace(/[*`~]/g, '')
        .replace(/\s+/g, ' ')
        .trim();
    const chars = Array.from(plain);
    return chars.length > limit ? chars.slice(0, limit).join('') + '…' : plain;
}

/** 按已展示顺序配对；新问题会结束上一问题的归属，不能借后来的答案补齐旧空白。 */
export function buildQuestionMarks(entries: QuestionNavigationEntry[]): QuestionMark[] {
    const marks: QuestionMark[] = [];
    let current: QuestionMark | undefined;
    for (const entry of entries) {
        if (entry.kind === 'question') {
            current = {
                id: entry.id,
                question: questionPreview(entry.text, 72) || '附件消息',
                answer: '',
                hasAnswer: false,
                pending: Boolean(entry.pending),
            };
            marks.push(current);
        } else if (current && entry.text.trim()) {
            current.answer = questionPreview(entry.text, 96);
            current.hasAnswer = true;
            current.pending = false;
        }
    }
    return marks;
}

/** 摘录为空不代表没回答：代码/附件等内容仍保留在原消息中。 */
export function questionAnswerPreview(mark: QuestionMark): string {
    return mark.answer || (mark.hasAnswer ? '已回答，暂无文字摘录' : mark.pending ? '正在处理，尚未回答' : '尚未回答');
}
