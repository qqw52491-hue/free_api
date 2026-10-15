/** Lossless recovery for a known truncated finish envelope; ordinary report content is opaque. */
export function finishSummaryText(raw: string): string {
    const text = raw.trim();
    if (!text.startsWith("plan-")) return raw;
    try {
        const packet = JSON.parse(`{"plan_id":"${text}}`);
        if (typeof packet.plan_id !== "string" || !/^plan-\d+$/.test(packet.plan_id)
            || typeof packet.plan_title !== "string" || packet.tool !== "finish"
            || typeof packet.command?.summary !== "string") return raw;
        return packet.command.summary;
    } catch {
        return raw;
    }
}
