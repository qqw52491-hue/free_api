export function asRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function toText(value: unknown): string {
    if (value === null || value === undefined) return "";
    if (typeof value === "string") return value;
    if (typeof value === "number" || typeof value === "boolean") return String(value);
    try {
        return JSON.stringify(value);
    } catch {
        return String(value);
    }
}

export function shortText(value: string, max: number): string {
    const compact = value.replace(/\s+/g, " ").trim();
    return compact.length > max ? `${compact.slice(0, max)}…（已截断）` : compact;
}

export function formatPayload(value: unknown): string {
    let text = "";
    if (typeof value === "string") text = value;
    else {
        try {
            text = JSON.stringify(value, null, 2);
        } catch {
            text = toText(value);
        }
    }
    return shortText(text, 12000);
}

export function toTimestamp(value: unknown): number {
    if (typeof value === "number" && Number.isFinite(value)) return value > 10_000_000_000 ? value : value * 1000;
    if (typeof value === "string") {
        const parsed = Date.parse(value);
        if (!Number.isNaN(parsed)) return parsed;
    }
    return Date.now();
}

export function numberOrUndefined(value: unknown): number | undefined {
    const number = Number(value);
    return Number.isFinite(number) && number >= 0 ? number : undefined;
}

export function formatDuration(seconds: number): string {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const remainder = seconds % 60;
    return hours
        ? `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`
        : `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}

export function formatMilliseconds(milliseconds?: number): string {
    if (!milliseconds || milliseconds <= 0) return "—";
    if (milliseconds < 1000) return `${Math.round(milliseconds)} ms`;
    return `${(milliseconds / 1000).toFixed(milliseconds >= 10_000 ? 0 : 1)} s`;
}

export function formatClock(timestamp: number): string {
    return new Intl.DateTimeFormat("zh-CN", {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
        hour12: false,
    }).format(new Date(timestamp));
}

export function formatNumber(value?: number): string {
    return new Intl.NumberFormat("zh-CN").format(value || 0);
}
